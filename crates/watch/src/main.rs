//! GameTrimmer Watch (`gametrimmer-watch.exe`)
//!
//! Ultra-lightweight background companion daemon monitoring game library manifests
//! (Steam, Epic, GOG, custom) via Windows IOCP and ReadDirectoryChangesW.
//!
//! Epic GT-EP12 / Task GT-139

#![windows_subsystem = "windows"]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, FindWindowW, PeekMessageW, SetForegroundWindow, TranslateMessage, MSG,
    PM_REMOVE, WM_QUIT,
};

pub mod fsm;
pub mod i18n;
pub mod ipc;
pub mod toast;
pub mod tray;
pub mod watcher;

use fsm::FsmDebouncer;
use i18n::WatchStrings;
use ipc::{IpcRequest, IpcResponse, IpcServer};
use toast::format_game_updated_toast;
use tray::{TrayCommand, TrayIcon};
use watcher::{discover_watch_directories, ManifestWatcher};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const APP_TITLE: &str = "GameTrimmer";
const EXE_NAME: &str = "gametrimmer.exe";

struct SingleInstanceGuard(HANDLE);

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn acquire_single_instance(exe_dir: &Path) -> Option<SingleInstanceGuard> {
    let mut hasher = DefaultHasher::new();
    exe_dir.to_string_lossy().to_lowercase().hash(&mut hasher);
    let mutex_name = format!("Local\\GameTrimmerWatchMutex-{:016x}", hasher.finish());
    let wide_name = to_wide(&mutex_name);

    let handle = unsafe { CreateMutexW(None, false, PCWSTR::from_raw(wide_name.as_ptr())) }.ok()?;
    let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;

    if already_exists {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return None;
    }

    Some(SingleInstanceGuard(handle))
}

fn minimize_working_set() {
    unsafe {
        let process = GetCurrentProcess();
        let _ = K32EmptyWorkingSet(process);
        let _ = windows::Win32::System::Threading::SetProcessWorkingSetSize(
            process,
            usize::MAX,
            usize::MAX,
        );
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Appends one timestamped line to `gametrimmer-watch.log`, next to the
/// executable.
///
/// The daemon has no logging framework of its own (unlike the GUI's
/// `logger` module - too much for a background process this small) and no
/// console (`windows_subsystem = "windows"`), so this is the only way a
/// lost `ReadDirectoryChangesW` buffer, today the sole caller, ever becomes
/// visible to anyone debugging a report of "the tray icon stopped noticing
/// updates". Best-effort and silent on failure: a diagnostic that can panic
/// or wedge the daemon over a locked log file would be worse than no
/// diagnostic at all.
fn log_line(exe_dir: &Path, message: &str) {
    use std::io::Write;

    let log_path = exe_dir.join("gametrimmer-watch.log");
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    else {
        return;
    };

    let seconds_since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = writeln!(file, "[{seconds_since_epoch}] {message}");
}

fn raise_or_launch_gui(exe_dir: &Path) {
    let title_wide = to_wide(APP_TITLE);
    let existing_window = unsafe { FindWindowW(None, PCWSTR::from_raw(title_wide.as_ptr())) };

    if let Ok(hwnd) = existing_window {
        if !hwnd.0.is_null() {
            unsafe {
                let _ = SetForegroundWindow(hwnd);
            }
            return;
        }
    }

    let gui_exe = exe_dir.join(EXE_NAME);
    if gui_exe.exists() {
        let _ = std::process::Command::new(gui_exe).spawn();
    }
}

fn open_local_db(db_path: &Path) -> Option<Connection> {
    if db_path.exists() {
        let conn = Connection::open_with_flags(
            db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )
        .ok()?;
        // Keep page cache ultra-minimal (64 KB) to avoid retaining MBs of memory in idle
        let _ = conn.pragma_update(None, "cache_size", "-64");
        let _ = conn.pragma_update(None, "temp_store", "MEMORY");
        Some(conn)
    } else {
        None
    }
}

fn main() {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    // 1. Single-instance guard
    let _instance_guard = match acquire_single_instance(&exe_dir) {
        Some(guard) => guard,
        None => {
            // Already running; ping existing daemon or exit gracefully
            let _ = ipc::send_ipc_request(ipc::DEFAULT_PIPE_NAME, &IpcRequest::Ping);
            return;
        }
    };

    // 2. Resolve database path
    let db_path = exe_dir.join("gametrimmer.db");

    // 3. Initialize components with localization
    let mut strings = WatchStrings::load(&exe_dir);
    let mut tray = match TrayIcon::create(strings.clone()) {
        Ok(t) => t,
        Err(_) => return,
    };

    let ipc_server = IpcServer::start(None).ok();

    let initial_dirs = discover_watch_directories(Some(&db_path));
    let mut watcher = match ManifestWatcher::new(&initial_dirs) {
        Ok(w) => w,
        Err(_) => return,
    };

    let mut fsm = FsmDebouncer::new();

    // 4. Initial baseline sweep (cold-start handling)
    fsm.set_baseline_established(true);

    // Minimize working set immediately upon startup
    minimize_working_set();

    let mut running = true;
    let mut last_trim_time = Instant::now();
    let mut msg = MSG::default();

    // 5. Main event loop
    while running {
        // A. Win32 message pump
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_QUIT {
                running = false;
                break;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        // B. Handle tray menu actions
        while let Some(tray_cmd) = tray.try_recv_command() {
            match tray_cmd {
                TrayCommand::OpenUi => {
                    raise_or_launch_gui(&exe_dir);
                }
                TrayCommand::CheckNow => {
                    let db_conn = open_local_db(&db_path);
                    let _ = fsm.check_settled(db_conn.as_ref());
                    drop(db_conn);
                    minimize_working_set();
                }
                TrayCommand::TogglePause => {
                    let paused = !watcher.is_paused();
                    if paused {
                        watcher.pause();
                        tray.set_paused(true);
                    } else {
                        watcher.resume();
                        tray.set_paused(false);
                    }
                }
                TrayCommand::Exit => {
                    running = false;
                }
            }
        }

        // C. Handle IPC requests from GUI / CLI
        if let Some(ref server) = ipc_server {
            while let Some(cmd) = server.try_recv() {
                match cmd.request {
                    IpcRequest::Ping => {
                        let _ = cmd.responder.send(IpcResponse::Pong {
                            version: VERSION.to_string(),
                            is_paused: watcher.is_paused(),
                            watching_count: watcher.watched_paths().len(),
                        });
                    }
                    IpcRequest::GetStatus => {
                        let watching_paths = watcher
                            .watched_paths()
                            .into_iter()
                            .map(|p| p.to_string_lossy().to_string())
                            .collect();
                        let _ = cmd.responder.send(IpcResponse::Status {
                            is_paused: watcher.is_paused(),
                            watching_paths,
                            games_tracked: fsm.tracked_count(),
                        });
                    }
                    IpcRequest::Pause => {
                        watcher.pause();
                        tray.set_paused(true);
                        let _ = cmd.responder.send(IpcResponse::Ok {
                            message: "Paused".to_string(),
                        });
                    }
                    IpcRequest::Resume => {
                        watcher.resume();
                        tray.set_paused(false);
                        let _ = cmd.responder.send(IpcResponse::Ok {
                            message: "Resumed".to_string(),
                        });
                    }
                    IpcRequest::ReloadSettings => {
                        let refreshed_dirs = discover_watch_directories(Some(&db_path));
                        for (dir, launcher) in refreshed_dirs {
                            let _ = watcher.add_directory(dir, launcher);
                        }
                        let refreshed_strings = WatchStrings::load(&exe_dir);
                        tray.update_i18n(refreshed_strings.clone());
                        strings = refreshed_strings;

                        let _ = cmd.responder.send(IpcResponse::Ok {
                            message: "Settings reloaded".to_string(),
                        });
                        minimize_working_set();
                    }
                    IpcRequest::TriggerRescan => {
                        let db_conn = open_local_db(&db_path);
                        let _ = fsm.check_settled(db_conn.as_ref());
                        drop(db_conn);
                        let _ = cmd.responder.send(IpcResponse::Ok {
                            message: "Rescan triggered".to_string(),
                        });
                        minimize_working_set();
                    }
                    IpcRequest::RetrimGame { ref app_id, .. } => {
                        raise_or_launch_gui(&exe_dir);
                        let _ = cmd.responder.send(IpcResponse::Ok {
                            message: format!("Retrim requested for {app_id}"),
                        });
                    }
                    IpcRequest::GameUpdated { .. } => {
                        let _ = cmd.responder.send(IpcResponse::Ok {
                            message: "Event recorded".to_string(),
                        });
                    }
                    IpcRequest::Exit => {
                        let _ = cmd.responder.send(IpcResponse::Ok {
                            message: "Shutting down".to_string(),
                        });
                        running = false;
                    }
                }
            }
        }

        // D. Poll IOCP manifest change events with adaptive sleep
        let wait_timeout = if fsm.has_pending() { 100 } else { 250 };
        let poll_result = watcher.poll_events(wait_timeout);
        for event in poll_result.events {
            fsm.record_event(event);
        }
        if !poll_result.overflowed_dirs.is_empty() {
            // Events were lost for these directories - see
            // `watcher::PollResult` - so the FSM's incremental picture of
            // them can no longer be trusted. Log it (the daemon has no
            // console and no other way to surface this) and force a
            // settle-check sweep now, rather than waiting for the FSM's own
            // debounce window, which only fires on events it actually
            // received.
            for dir in &poll_result.overflowed_dirs {
                log_line(
                    &exe_dir,
                    &format!(
                        "watch buffer overflow on {} - directory changes may have been missed, forcing a rescan",
                        dir.display()
                    ),
                );
            }
            let db_conn = open_local_db(&db_path);
            let _ = fsm.check_settled(db_conn.as_ref());
            drop(db_conn);
        }

        // E. Check FSM settling window and verify states (only opens DB if events are pending)
        if fsm.has_pending() {
            let db_conn = open_local_db(&db_path);
            let updates = fsm.check_settled(db_conn.as_ref());
            drop(db_conn);
            for update in updates {
                let (title, body) = format_game_updated_toast(
                    &strings,
                    &update.name,
                    &update.launcher,
                    update.old_build_id.as_deref(),
                    update.new_build_id.as_deref(),
                );
                tray.show_notification(&title, &body);
            }
            if !fsm.has_pending() {
                minimize_working_set();
            }
        }

        // F. Periodic working set minimization (every 30s)
        if last_trim_time.elapsed() >= Duration::from_secs(30) {
            minimize_working_set();
            last_trim_time = Instant::now();
        }
    }
}
