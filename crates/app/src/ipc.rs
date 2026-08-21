//! IPC Client for GameTrimmer Watch companion daemon (`\\.\pipe\gametrimmer-ipc`).

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

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

/// Upper bound on how long the caller waits for the daemon to answer.
///
/// The daemon has its own 2000ms responder timeout (see
/// `gametrimmer_watch::ipc::handle_client_connection`), so anything longer
/// than that on the client side would only ever be waiting on the daemon's
/// own safety net; the margin above it covers the round-trip itself.
const CLIENT_TIMEOUT: Duration = Duration::from_millis(2500);

/// Sends an IPC request to the named pipe and awaits the response, without
/// blocking the calling thread past [`CLIENT_TIMEOUT`].
///
/// This is called synchronously from the UI thread on every settings save
/// (`GameTrimmerApp::persist_settings`), not only for watch-related
/// settings - a daemon that accepts the connection and then never answers
/// used to hang that thread forever, since the underlying pipe I/O
/// (`perform_roundtrip`) has no notion of a deadline of its own. The actual
/// I/O runs on a detached worker thread instead; this function only ever
/// waits up to `timeout` on the channel that carries its result back.
///
/// A daemon that is merely slow, rather than truly stuck, still finishes
/// its write eventually - the worker thread outlives this call in that
/// case and exits on its own once the read returns; nothing here attempts
/// to cancel it, since Rust threads cannot be cancelled from the outside.
pub fn send_ipc_request(req: &IpcRequest, pipe_name: Option<&str>) -> Result<IpcResponse, String> {
    let name = pipe_name.unwrap_or(DEFAULT_PIPE_NAME).to_string();
    let payload = serde_json::to_string(req).map_err(|e| e.to_string())?;
    let data = format!("{payload}\n");
    send_with_timeout(name, data, CLIENT_TIMEOUT, perform_roundtrip)
}

/// The blocking, unbounded part of an IPC round trip: open the pipe, write
/// the request, and read back a reply. Split out from [`send_ipc_request`]
/// so [`send_with_timeout`] can run it on a worker thread, and so a test can
/// substitute a roundtrip that behaves like a stalled daemon without a real
/// named pipe.
fn perform_roundtrip(name: &str, data: &str) -> Result<IpcResponse, String> {
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

/// Runs `roundtrip(&name, &data)` on a detached worker thread and waits at
/// most `timeout` for it to answer over a channel, rather than on the
/// roundtrip call itself.
///
/// Generic over `roundtrip` (a plain `fn`, not a closure capturing state) so
/// tests can swap in a stand-in that behaves like a stuck daemon - sleeps
/// past `timeout` before returning - and assert that the *caller* still
/// returns on time, which is the property this function exists to
/// guarantee. The real transport (`perform_roundtrip`) never blocks the
/// caller directly for exactly the same reason: this is the one place that
/// property is enforced, so `send_ipc_request` does not have to reason about
/// it itself.
fn send_with_timeout(
    name: String,
    data: String,
    timeout: Duration,
    roundtrip: fn(&str, &str) -> Result<IpcResponse, String>,
) -> Result<IpcResponse, String> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let result = roundtrip(&name, &data);
        // The receiver may already be gone if `recv_timeout` below expired
        // first - that is a race this side lost, not a bug, so the send
        // error is deliberately ignored.
        let _ = tx.send(result);
    });

    rx.recv_timeout(timeout).unwrap_or_else(|_| {
        Err(format!(
            "IPC request timed out after {timeout:?} waiting for the daemon to respond"
        ))
    })
}

/// Pings the daemon to check whether it is running.
pub fn ping_daemon(pipe_name: Option<&str>) -> bool {
    matches!(
        send_ipc_request(&IpcRequest::Ping, pipe_name),
        Ok(IpcResponse::Pong { .. } | IpcResponse::Ok { .. })
    )
}

/// Notifies the daemon to reload its configuration from `gametrimmer.ini`.
///
/// Returns the outcome rather than discarding it - the caller
/// (`GameTrimmerApp::persist_settings`) decides what a failure is worth
/// (logged, not shown to the user: the daemon simply not running is the
/// ordinary case, not an error).
pub fn reload_daemon_settings(pipe_name: Option<&str>) -> Result<(), String> {
    send_ipc_request(&IpcRequest::ReloadSettings, pipe_name).map(|_| ())
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

    /// A roundtrip standing in for a daemon that accepted the request and
    /// then never answered - the exact scenario `send_with_timeout` exists
    /// to bound. Sleeps well past every timeout this test uses, so if the
    /// caller ever waited on this function directly instead of the channel,
    /// the test itself would hang instead of failing fast.
    fn stalled_roundtrip(_name: &str, _data: &str) -> Result<IpcResponse, String> {
        thread::sleep(Duration::from_secs(30));
        Ok(IpcResponse::Ok {
            message: "too late".to_string(),
        })
    }

    #[test]
    fn a_stalled_daemon_does_not_block_the_caller_past_the_timeout() {
        let start = std::time::Instant::now();
        let timeout = Duration::from_millis(100);

        let result = send_with_timeout(
            "irrelevant".to_string(),
            "irrelevant".to_string(),
            timeout,
            stalled_roundtrip,
        );

        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "the caller waited {elapsed:?} for a roundtrip that never answers within the timeout",
        );
        assert!(
            result.is_err(),
            "a timed-out request must not read as success"
        );
    }

    /// The other half of the contract: a roundtrip that answers quickly
    /// still has to reach the caller, so the timeout machinery must not
    /// swallow or delay an ordinary successful reply.
    #[test]
    fn a_prompt_roundtrip_still_reaches_the_caller() {
        fn instant_roundtrip(_name: &str, _data: &str) -> Result<IpcResponse, String> {
            Ok(IpcResponse::Pong {
                version: "1.0.0".to_string(),
                is_paused: false,
                watching_count: 0,
            })
        }

        let result = send_with_timeout(
            "irrelevant".to_string(),
            "irrelevant".to_string(),
            Duration::from_secs(2),
            instant_roundtrip,
        );

        assert_eq!(
            result,
            Ok(IpcResponse::Pong {
                version: "1.0.0".to_string(),
                is_paused: false,
                watching_count: 0,
            })
        );
    }
}
