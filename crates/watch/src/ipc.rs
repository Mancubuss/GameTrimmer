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
    CloseHandle, GetLastError, LocalFree, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL,
    INVALID_HANDLE_VALUE,
};
use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows::Win32::Security::{
    EqualSid, GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
    TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    PIPE_READMODE_MESSAGE, PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
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
        pub fn recv_timeout(
            self,
            timeout: std::time::Duration,
        ) -> Result<T, std::sync::mpsc::RecvTimeoutError> {
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
        let _ = send_ipc_raw(
            &self.pipe_name,
            b"{\"type\":\"Ping\"}\n",
            Duration::from_millis(50),
        );
        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.join();
        }
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A `SECURITY_ATTRIBUTES` restricting a named pipe to its creating user.
///
/// `CreateNamedPipeW` with no security attributes uses the process token's
/// default DACL, which on this pipe's default configuration grants access
/// to any local process regardless of which account it runs under - the
/// pipe carries library paths (`GetStatus`) and accepts `Pause`/`Resume`/
/// `Exit`, so that default is a real information-disclosure and
/// denial-of-service surface. `"D:P(A;;GA;;;OW)"` grants full access to the
/// pipe's owner (the account that created it - by default the account this
/// process runs as) and nothing to anyone else; `P` marks the DACL
/// protected so nothing can merge inherited ACEs into it later.
struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

impl PipeSecurity {
    fn current_user_only() -> windows::core::Result<Self> {
        let sddl = to_wide("D:P(A;;GA;;;OW)");
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` is a valid, NUL-terminated wide string that outlives
        // this call. `descriptor` receives a pointer to memory this API
        // allocates with `LocalAlloc`; `Drop` below releases it with
        // `LocalFree`, the release its documentation calls for.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR::from_raw(sddl.as_ptr()),
                1, // SDDL_REVISION_1
                &mut descriptor,
                None,
            )?;
        }

        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: windows::core::BOOL(0),
        };

        Ok(Self {
            descriptor,
            attributes,
        })
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        if self.descriptor.0.is_null() {
            return;
        }
        // SAFETY: `self.descriptor` was allocated by
        // `ConvertStringSecurityDescriptorToSecurityDescriptorW` in
        // `current_user_only`, which documents `LocalFree` as the correct
        // release for it; this runs exactly once, from `Drop`, after every
        // `CreateNamedPipeW` call that borrowed `self.attributes` has
        // already returned.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.descriptor.0)));
        }
    }
}

fn run_server_loop(
    pipe_name: &str,
    shutdown: Arc<AtomicBool>,
    command_tx: Sender<IpcServerCommand>,
) {
    let wide_name = to_wide(pipe_name);

    // Built once and reused for every instance this loop creates: the SDDL
    // string is fixed, so re-deriving the descriptor per connection would
    // only be work spent to arrive back at the same thing.
    let security = match PipeSecurity::current_user_only() {
        Ok(sec) => sec,
        Err(_) => {
            // Without a security descriptor we would fall back to
            // `CreateNamedPipeW`'s default DACL, which is far too open for
            // this pipe (see `PipeSecurity`) - refuse to open it at all
            // rather than open it insecurely.
            return;
        }
    };

    while !shutdown.load(Ordering::SeqCst) {
        // SAFETY: `wide_name` is a NUL-terminated wide string that outlives
        // this call; `security.attributes` borrows `security`, which
        // outlives the whole loop. The returned handle is closed on every
        // path out of this iteration, below.
        let pipe_handle = unsafe {
            CreateNamedPipeW(
                PCWSTR::from_raw(wide_name.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                1000,
                Some(&security.attributes),
            )
        };

        if pipe_handle == INVALID_HANDLE_VALUE || pipe_handle.is_invalid() {
            thread::sleep(Duration::from_millis(200));
            continue;
        }

        // SAFETY: `pipe_handle` was just created above and is valid for the
        // duration of this call.
        let connected = unsafe { ConnectNamedPipe(pipe_handle, None) };
        // SAFETY: only called to interpret the error left by
        // `ConnectNamedPipe` immediately above; nothing here outlives this
        // expression.
        let connect_success =
            connected.is_ok() || (unsafe { GetLastError() } == ERROR_PIPE_CONNECTED);

        if connect_success && !shutdown.load(Ordering::SeqCst) {
            handle_client_connection(pipe_handle, &command_tx);
        }

        // SAFETY: `pipe_handle` is only used within this iteration; both
        // calls are unconditional cleanup performed before the handle goes
        // out of scope.
        unsafe {
            let _ = DisconnectNamedPipe(pipe_handle);
            let _ = CloseHandle(pipe_handle);
        }
    }
}

fn handle_client_connection(pipe_handle: HANDLE, command_tx: &Sender<IpcServerCommand>) {
    if !client_is_current_user(pipe_handle) {
        // The pipe's DACL (see `PipeSecurity`) already keeps a process
        // running as a different user from ever obtaining a handle to
        // connect with, so reaching this point with a mismatched SID is not
        // expected in practice. Check anyway, and drop the connection
        // without reading from it: defense in depth against that assumption
        // ever quietly breaking (a future change to the DACL, a Windows
        // configuration with different defaults, ...).
        return;
    }

    let mut buffer = [0u8; 4096];
    let mut bytes_read: u32 = 0;

    // SAFETY: `pipe_handle` is a connected server-side pipe instance owned
    // by the caller for the duration of this call; `buffer` and
    // `bytes_read` are valid, correctly sized, and not aliased elsewhere.
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

/// Whether the process on the other end of `pipe_handle` is running as the
/// same user as this process.
///
/// The pipe's DACL is the real control (see [`PipeSecurity`]); this is a
/// second, independent check using a different Win32 mechanism
/// (`GetNamedPipeClientProcessId` plus a token SID comparison) so the two do
/// not share a single point of failure. Any failure along the way -
/// `GetNamedPipeClientProcessId` itself, opening the client process, reading
/// either token's SID - is treated as "not the same user": this gate exists
/// to keep untrusted connections out, so an inconclusive answer fails closed
/// exactly like `retrim::is_game_running` does for the same reason.
fn client_is_current_user(pipe_handle: HANDLE) -> bool {
    let mut client_pid = 0u32;
    // SAFETY: `pipe_handle` is a connected server-side pipe instance owned
    // by the caller for the duration of this call; `client_pid` is a plain
    // `u32` on the stack.
    if unsafe { GetNamedPipeClientProcessId(pipe_handle, &mut client_pid) }.is_err() {
        return false;
    }

    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no
    // closing and is valid for the lifetime of this process.
    let self_sid = match process_user_sid(unsafe { GetCurrentProcess() }) {
        Some(sid) => sid,
        None => return false,
    };

    // SAFETY: `client_pid` came from `GetNamedPipeClientProcessId` above;
    // `PROCESS_QUERY_LIMITED_INFORMATION` is sufficient to open its token for
    // the `TokenUser` query below, and the resulting handle is closed before
    // this function returns.
    let client_handle =
        match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, client_pid) } {
            Ok(h) if !h.is_invalid() => h,
            _ => return false,
        };
    let client_sid = process_user_sid(client_handle);
    // SAFETY: `client_handle` was opened immediately above and is not used
    // again after this point.
    let _ = unsafe { CloseHandle(client_handle) };
    let Some(client_sid) = client_sid else {
        return false;
    };

    // A buffer too short to hold the header means the API broke its contract,
    // and an unidentifiable client is not this user's - refuse it.
    let (Some(self_ptr), Some(client_ptr)) = (sid_ptr(&self_sid), sid_ptr(&client_sid)) else {
        return false;
    };
    // SAFETY: both `self_sid` and `client_sid` are `Vec<u8>` buffers that
    // outlive this call, and the pointers above only ever point inside them;
    // both buffers were produced by `GetTokenInformation(.., TokenUser, ..)`,
    // which guarantees a well-formed SID at that offset.
    unsafe { EqualSid(self_ptr, client_ptr) }.is_ok()
}

/// Reads the `SID` of the user owning `process_handle`'s primary token, as
/// the raw bytes `GetTokenInformation` fills for `TokenUser`.
///
/// Returned as owned bytes rather than the `PSID` embedded in the decoded
/// `TOKEN_USER`, because that pointer only stays valid as long as this
/// buffer does - handing back the struct without the buffer behind it would
/// leave the caller holding a dangling pointer disguised as an ordinary
/// value.
fn process_user_sid(process_handle: HANDLE) -> Option<Vec<u8>> {
    let mut token = HANDLE::default();
    // SAFETY: `process_handle` is a valid, open (or pseudo-) process handle
    // supplied by the caller; `OpenProcessToken` only reads it and writes
    // the resulting token handle into `token`, which is closed below before
    // this function returns.
    if unsafe { OpenProcessToken(process_handle, TOKEN_QUERY, &mut token) }.is_err() {
        return None;
    }

    let mut needed = 0u32;
    // SAFETY: passing `None` for the output buffer with a
    // `tokeninformationlength` of 0 is the documented way to ask
    // `GetTokenInformation` for the required buffer size; it writes that
    // size into `needed` and returns an error (`ERROR_INSUFFICIENT_BUFFER`)
    // that is expected and ignored here.
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
    if needed == 0 {
        // SAFETY: `token` was opened above and is only used within this function.
        let _ = unsafe { CloseHandle(token) };
        return None;
    }

    let mut buf = vec![0u8; needed as usize];
    // SAFETY: `buf` is sized exactly to `needed`, the size just reported for
    // this same token by the call above; `GetTokenInformation` writes a
    // `TOKEN_USER` followed by its variable-length `SID` into it, and does
    // not write past `needed` bytes.
    let filled = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr() as *mut _),
            needed,
            &mut needed,
        )
    };
    // SAFETY: `token` was opened above and is only used within this function.
    let _ = unsafe { CloseHandle(token) };

    if filled.is_err() {
        return None;
    }
    Some(buf)
}

/// Extracts the `PSID` embedded by `GetTokenInformation(.., TokenUser, ..)`
/// in a buffer filled by [`process_user_sid`], or `None` if the buffer is too
/// short to hold the header - which would mean the API broke its contract.
fn sid_ptr(buf: &[u8]) -> Option<PSID> {
    if buf.len() < std::mem::size_of::<TOKEN_USER>() {
        return None;
    }
    // SAFETY: the length check above covers the read, and the buffer is a
    // `Vec<u8>` - alignment 1, while `TOKEN_USER` wants 8 - so this reads the
    // header out unaligned rather than taking a reference to it, which would
    // be undefined behaviour no matter what the allocator happened to return.
    // The `Sid` it carries points into the SID bytes packed later in that same
    // buffer, valid for as long as `buf` is: every caller keeps the `Vec`
    // alive across the `EqualSid` call that uses this pointer.
    let token_user = unsafe { std::ptr::read_unaligned(buf.as_ptr().cast::<TOKEN_USER>()) };
    Some(token_user.User.Sid)
}

/// Client helper: Sends an IPC request to the named pipe and awaits the response.
pub fn send_ipc_request(pipe_name: &str, req: &IpcRequest) -> std::io::Result<IpcResponse> {
    let req_json = serde_json::to_string(req)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let payload = format!("{req_json}\n");
    let resp_bytes = send_ipc_raw(pipe_name, payload.as_bytes(), Duration::from_millis(3000))?;
    let resp_str = String::from_utf8_lossy(&resp_bytes);
    let resp: IpcResponse = serde_json::from_str(resp_str.trim()).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{e}: {resp_str}"))
    })?;
    Ok(resp)
}

fn send_ipc_raw(pipe_name: &str, data: &[u8], _timeout: Duration) -> std::io::Result<Vec<u8>> {
    let mut file = OpenOptions::new().read(true).write(true).open(pipe_name)?;

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

    #[test]
    fn pipe_security_current_user_only_builds_a_usable_descriptor() {
        let security = PipeSecurity::current_user_only().expect("build security descriptor");
        assert!(!security.descriptor.0.is_null());
        assert_eq!(
            security.attributes.nLength as usize,
            std::mem::size_of::<SECURITY_ATTRIBUTES>()
        );
        assert!(!security.attributes.bInheritHandle.as_bool());
    }

    #[test]
    fn process_user_sid_agrees_with_itself() {
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle valid for the
        // life of the process and needs no closing.
        let handle = unsafe { GetCurrentProcess() };
        let first = process_user_sid(handle).expect("read this process's own SID");
        let second = process_user_sid(handle).expect("read it again");

        let first_ptr = sid_ptr(&first).expect("the header must fit the buffer");
        let second_ptr = sid_ptr(&second).expect("the header must fit the buffer");
        // SAFETY: both buffers are held alive as local variables across this
        // call, and both were produced by `GetTokenInformation(.., TokenUser, ..)`.
        assert!(unsafe { EqualSid(first_ptr, second_ptr) }.is_ok());
    }
}
