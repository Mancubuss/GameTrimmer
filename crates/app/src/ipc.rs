//! IPC Client for GameTrimmer Watch companion daemon (`\\.\pipe\gametrimmer-ipc`).

use std::fs::OpenOptions;
use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

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

/// Sends an IPC request to the named pipe and awaits the response.
pub fn send_ipc_request(req: &IpcRequest, pipe_name: Option<&str>) -> Result<IpcResponse, String> {
    let name = pipe_name.unwrap_or(DEFAULT_PIPE_NAME);
    let payload = serde_json::to_string(req).map_err(|e| e.to_string())?;
    let data = format!("{payload}\n");

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(name)
        .map_err(|e| e.to_string())?;

    file.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())?;

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
            Err(e) => return Err(e.to_string()),
        }
    }

    let resp_str = String::from_utf8_lossy(&buf);
    let resp: IpcResponse =
        serde_json::from_str(resp_str.trim()).map_err(|e| format!("{e}: {resp_str}"))?;
    Ok(resp)
}

/// Pings the daemon to check whether it is running.
pub fn ping_daemon(pipe_name: Option<&str>) -> bool {
    matches!(
        send_ipc_request(&IpcRequest::Ping, pipe_name),
        Ok(IpcResponse::Pong { .. } | IpcResponse::Ok { .. })
    )
}

/// Notifies the daemon to reload its configuration from `gametrimmer.ini`.
pub fn reload_daemon_settings(pipe_name: Option<&str>) {
    let _ = send_ipc_request(&IpcRequest::ReloadSettings, pipe_name);
}

/// Requests an immediate rescan/check of all watched libraries from the daemon.
pub fn trigger_daemon_rescan(pipe_name: Option<&str>) -> Result<IpcResponse, String> {
    send_ipc_request(&IpcRequest::TriggerRescan, pipe_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_request_response_serde_roundtrip() {
        let req = IpcRequest::GameUpdated {
            app_id: "730".to_string(),
            name: "Counter-Strike 2".to_string(),
            new_build_id: Some("12345".to_string()),
            launcher: "Steam".to_string(),
        };
        let json = serde_json::to_string(&req).expect("serialize req");
        let parsed: IpcRequest = serde_json::from_str(&json).expect("deserialize req");
        assert_eq!(req, parsed);

        let resp = IpcResponse::Pong {
            version: "1.0.0".to_string(),
            is_paused: false,
            watching_count: 3,
        };
        let resp_json = serde_json::to_string(&resp).expect("serialize resp");
        let resp_parsed: IpcResponse = serde_json::from_str(&resp_json).expect("deserialize resp");
        assert_eq!(resp, resp_parsed);
    }

    #[test]
    fn ping_nonexistent_pipe_returns_false() {
        let dummy_pipe = r"\\.\pipe\gametrimmer-nonexistent-test-pipe-xyz";
        assert!(!ping_daemon(Some(dummy_pipe)));
    }
}
