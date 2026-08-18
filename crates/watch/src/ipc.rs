//! Windows Named Pipe IPC communication for GameTrimmer Watch companion daemon.
//!
//! Exposes a named pipe at `\\.\pipe\gametrimmer-ipc` allowing the main
//! GameTrimmer GUI or CLI to communicate with the background monitoring daemon.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
    PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

pub const DEFAULT_PIPE_NAME: &str = r"\\.\pipe\gametrimmer-ipc";

/// IPC Request commands sent to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "payload")]
pub enum IpcRequest {
    /// Ping the daemon for liveness.
    Ping,
    /// Notification that a game's state was updated.
    GameUpdated {
        app_id: String,
        name: String,
        new_build_id: Option<String>,
        launcher: String,
    },
    /// Request the daemon to reload settings from config/db.
    ReloadSettings,
    /// Request an immediate rescan/check of all watched libraries.
    TriggerRescan,
    /// Request re-trimming of a specific game.
    RetrimGame {
        app_id: String,
        path: Option<String>,
    },
    /// Request daemon status info.
    GetStatus,
    /// Pause file monitoring.
    Pause,
    /// Resume file monitoring.
    Resume,
    /// Request the daemon to shut down cleanly.
    Exit,
}

/// IPC Response sent back from the daemon.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", content = "data")]
pub enum IpcResponse {
    Ok {
        message: String,
    },
    Pong {
        version: String,
        is_paused: bool,
        watching_count: usize,
    },
    Status {
        is_paused: bool,
        watching_paths: Vec<String>,
        games_tracked: usize,
    },
    Error {
        message: String,
    },
}

/// Message passed from IPC thread to main event loop.
#[derive(Debug)]
pub struct IpcServerCommand {
    pub request: IpcRequest,
    pub responder: oneshot_channel::Sender<IpcResponse>,
}

pub mod oneshot_channel {
    use std::sync::mpsc::{channel, Receiver, Sender as StdSender};

    #[derive(Debug)]
    pub struct Sender<T>(StdSender<T>);

    #[derive(Debug)]
    pub struct ReceiverWrapper<T>(Receiver<T>);

    pub fn oneshot<T>() -> (Sender<T>, ReceiverWrapper<T>) {
        let (tx, rx) = channel();
        (Sender(tx), ReceiverWrapper(rx))
    }

    impl<T> Sender<T> {
        pub fn send(self, value: T) -> Result<(), T> {
            self.0.send(value).map_err(|e| e.0)
        }
    }

    impl<T> ReceiverWrapper<T> {
        pub fn recv_timeout(self, timeout: std::time::Duration) -> Result<T, std::sync::mpsc::RecvTimeoutError> {
            self.0.recv_timeout(timeout)
        }
    }
}

/// Named pipe server running in a dedicated background thread.
pub struct IpcServer {
    pipe_name: String,
    shutdown: Arc<AtomicBool>,
    worker_handle: Option<JoinHandle<()>>,
    command_rx: Receiver<IpcServerCommand>,
}

impl IpcServer {
    /// Starts the IPC server on the default or specified pipe name.
    pub fn start(pipe_name: Option<&str>) -> std::io::Result<Self> {
        let pipe_name = pipe_name.unwrap_or(DEFAULT_PIPE_NAME).to_string();
        let shutdown = Arc::new(AtomicBool::new(false));
        let (command_tx, command_rx) = mpsc::channel::<IpcServerCommand>();

        let pipe_name_clone = pipe_name.clone();
        let shutdown_clone = Arc::clone(&shutdown);

        let worker_handle = thread::Builder::new()
            .name("gametrimmer-ipc-server".to_string())
            .stack_size(64 * 1024)
            .spawn(move || {
                run_server_loop(&pipe_name_clone, shutdown_clone, command_tx);
            })?;

        Ok(Self {
            pipe_name,
            shutdown,
            worker_handle: Some(worker_handle),
            command_rx,
        })
    }

    /// Try to receive a pending command from the IPC client.
    pub fn try_recv(&self) -> Option<IpcServerCommand> {
        self.command_rx.try_recv().ok()
    }

    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Poke the pipe with a dummy connection to unblock ConnectNamedPipe if waiting
        let _ = send_ipc_raw(&self.pipe_name, b"{\"type\":\"Ping\"}\n", Duration::from_millis(50));
        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.join();
        }
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn run_server_loop(pipe_name: &str, shutdown: Arc<AtomicBool>, command_tx: Sender<IpcServerCommand>) {
    let wide_name = to_wide(pipe_name);

    while !shutdown.load(Ordering::SeqCst) {
        let pipe_handle = unsafe {
            CreateNamedPipeW(
                PCWSTR::from_raw(wide_name.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                1000,
                None,
            )
        };

        if pipe_handle == INVALID_HANDLE_VALUE || pipe_handle.is_invalid() {
            thread::sleep(Duration::from_millis(200));
            continue;
        }

        let connected = unsafe { ConnectNamedPipe(pipe_handle, None) };
        let connect_success = connected.is_ok() || (unsafe { GetLastError() } == ERROR_PIPE_CONNECTED);

        if connect_success && !shutdown.load(Ordering::SeqCst) {
            handle_client_connection(pipe_handle, &command_tx);
        }

        unsafe {
            let _ = DisconnectNamedPipe(pipe_handle);
            let _ = CloseHandle(pipe_handle);
        }
    }
}

fn handle_client_connection(pipe_handle: HANDLE, command_tx: &Sender<IpcServerCommand>) {
    let mut buffer = [0u8; 4096];
    let mut bytes_read: u32 = 0;

    let read_ok = unsafe {
        windows::Win32::Storage::FileSystem::ReadFile(
            pipe_handle,
            Some(&mut buffer),
            Some(&mut bytes_read),
            None,
        )
    };

    if read_ok.is_err() || bytes_read == 0 {
        return;
    }

    let raw_str = String::from_utf8_lossy(&buffer[..bytes_read as usize]);
    let req_res: Result<IpcRequest, _> = serde_json::from_str(raw_str.trim());

    let response = match req_res {
        Ok(request) => {
            let (resp_tx, resp_rx) = oneshot_channel::oneshot();
            let cmd = IpcServerCommand {
                request,
                responder: resp_tx,
            };

            if command_tx.send(cmd).is_ok() {
                resp_rx
                    .recv_timeout(Duration::from_millis(2000))
                    .unwrap_or_else(|_| IpcResponse::Error {
                        message: "Handler timed out".to_string(),
                    })
            } else {
                IpcResponse::Error {
                    message: "Server stopping".to_string(),
                }
            }
        }
        Err(err) => IpcResponse::Error {
            message: format!("Invalid JSON request: {err}"),
        },
    };

    if let Ok(resp_json) = serde_json::to_string(&response) {
        let resp_bytes = format!("{resp_json}\n");
        let mut bytes_written: u32 = 0;
        let _ = unsafe {
            windows::Win32::Storage::FileSystem::WriteFile(
                pipe_handle,
                Some(resp_bytes.as_bytes()),
                Some(&mut bytes_written),
                None,
            )
        };
        let _ = unsafe { windows::Win32::Storage::FileSystem::FlushFileBuffers(pipe_handle) };
    }
}

/// Client helper: Sends an IPC request to the named pipe and awaits the response.
pub fn send_ipc_request(pipe_name: &str, req: &IpcRequest) -> std::io::Result<IpcResponse> {
    let req_json = serde_json::to_string(req).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let payload = format!("{req_json}\n");
    let resp_bytes = send_ipc_raw(pipe_name, payload.as_bytes(), Duration::from_millis(3000))?;
    let resp_str = String::from_utf8_lossy(&resp_bytes);
    let resp: IpcResponse = serde_json::from_str(resp_str.trim())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{e}: {resp_str}")))?;
    Ok(resp)
}

fn send_ipc_raw(pipe_name: &str, data: &[u8], _timeout: Duration) -> std::io::Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(pipe_name)?;

    file.write_all(data)?;
    file.flush()?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.ends_with(b"\n") || buf.ends_with(b"}") {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_request_serialization_roundtrip() {
        let requests = vec![
            IpcRequest::Ping,
            IpcRequest::GameUpdated {
                app_id: "123456".to_string(),
                name: "DOOM Eternal".to_string(),
                new_build_id: Some("987654".to_string()),
                launcher: "steam".to_string(),
            },
            IpcRequest::ReloadSettings,
            IpcRequest::TriggerRescan,
            IpcRequest::RetrimGame {
                app_id: "Fortnite".to_string(),
                path: Some(r"C:\Games\Fortnite".to_string()),
            },
            IpcRequest::GetStatus,
            IpcRequest::Pause,
            IpcRequest::Resume,
            IpcRequest::Exit,
        ];

        for req in requests {
            let json = serde_json::to_string(&req).expect("serialize");
            let deserialized: IpcRequest = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(req, deserialized);
        }
    }

    #[test]
    fn ipc_response_serialization_roundtrip() {
        let responses = vec![
            IpcResponse::Ok {
                message: "Done".to_string(),
            },
            IpcResponse::Pong {
                version: "1.0.0".to_string(),
                is_paused: false,
                watching_count: 4,
            },
            IpcResponse::Status {
                is_paused: true,
                watching_paths: vec![r"C:\Steam\steamapps".to_string()],
                games_tracked: 42,
            },
            IpcResponse::Error {
                message: "Failed".to_string(),
            },
        ];

        for resp in responses {
            let json = serde_json::to_string(&resp).expect("serialize");
            let deserialized: IpcResponse = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(resp, deserialized);
        }
    }

    #[test]
    fn ipc_server_client_communication() {
        let pipe_name = format!(r"\\.\pipe\gametrimmer-ipc-test-{}", std::process::id());
        let server = IpcServer::start(Some(&pipe_name)).expect("start server");

        // Spawn a thread to handle server command
        let handle = thread::spawn(move || {
            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_secs(3) {
                if let Some(cmd) = server.try_recv() {
                    match cmd.request {
                        IpcRequest::Ping => {
                            let _ = cmd.responder.send(IpcResponse::Pong {
                                version: "1.0.0".to_string(),
                                is_paused: false,
                                watching_count: 3,
                            });
                        }
                        _ => {
                            let _ = cmd.responder.send(IpcResponse::Ok {
                                message: "ACK".to_string(),
                            });
                        }
                    }
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            panic!("timed out waiting for IPC command");
        });

        // Client sends Ping
        thread::sleep(Duration::from_millis(50));
        let resp = send_ipc_request(&pipe_name, &IpcRequest::Ping).expect("send request");
        assert_eq!(
            resp,
            IpcResponse::Pong {
                version: "1.0.0".to_string(),
                is_paused: false,
                watching_count: 3,
            }
        );

        handle.join().expect("join");
    }
}
