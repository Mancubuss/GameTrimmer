//! Real-Time Log Streaming Engine & File Logger for Archive Trimmer.
//!
//! Streams real-time scanner updates, anti-cheat warnings, NTFS sparse zeroing operations,
//! and background worker diagnostics to `archive-trimmer.log` next to the executable
//! or current working directory.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::SystemTime;

static LOG_FILE_PATH: RwLock<Option<PathBuf>> = RwLock::new(None);
static LOG_MUTEX: Mutex<()> = Mutex::new(());

/// Initializes or updates the log file location.
pub fn init_log_path(custom_path: Option<PathBuf>) -> PathBuf {
    let path = custom_path.unwrap_or_else(get_default_log_path);
    if let Ok(mut lock) = LOG_FILE_PATH.write() {
        *lock = Some(path.clone());
    }
    path
}

/// Returns the current log file path.
pub fn get_log_path() -> PathBuf {
    if let Ok(lock) = LOG_FILE_PATH.read() {
        if let Some(ref path) = *lock {
            return path.clone();
        }
    }
    get_default_log_path()
}

/// Computes the default log file path (`archive-trimmer.log` next to exe or in current dir).
pub fn get_default_log_path() -> PathBuf {
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(parent) = exe_path.parent() {
            let candidate = parent.join("archive-trimmer.log");
            if parent.is_dir() {
                return candidate;
            }
        }
    }
    PathBuf::from("archive-trimmer.log")
}

/// Formats a SystemTime timestamp as `YYYY-MM-DD HH:MM:SS`.
pub fn format_timestamp(time: SystemTime) -> String {
    let secs = match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(_) => 0,
    };
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = (secs / 86400) as i64;

    // Howard Hinnant's civil date algorithm
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m_adj = if mp < 10 { mp + 3 } else { mp - 9 };
    let y_adj = if m_adj <= 2 { y + 1 } else { y };

    format!("{y_adj:04}-{m_adj:02}-{d:02} {h:02}:{m:02}:{s:02}")
}

/// Formats a log line: `[YYYY-MM-DD HH:MM:SS] [LEVEL] message`.
pub fn format_log_line(level: &str, message: &str) -> String {
    let now = SystemTime::now();
    let ts = format_timestamp(now);
    let clean_level = level.trim().trim_start_matches('[').trim_end_matches(']');
    format!("[{ts}] [{clean_level}] {message}")
}

/// Writes a formatted log entry to a specific log file path in real-time.
pub fn write_to_custom_log_file(path: &std::path::Path, entry: &str) {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let _lock = LOG_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let line = if entry.ends_with('\n') {
            entry.to_string()
        } else {
            format!("{entry}\n")
        };
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }
}

/// Writes a formatted log entry to the default/global log file in real-time.
pub fn write_to_log_file(entry: &str) {
    let path = get_log_path();
    write_to_custom_log_file(&path, entry);
}

/// Logs a message at a given level, writing to disk and returning the formatted line.
pub fn log_entry(level: &str, message: &str) -> String {
    let formatted = format_log_line(level, message);
    write_to_log_file(&formatted);
    formatted
}

/// Clears the log file on disk.
pub fn clear_log_file() -> Result<(), std::io::Error> {
    let path = get_log_path();
    let _lock = LOG_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    std::fs::write(&path, "")?;
    Ok(())
}

/// Opens the log file using the OS default handler or fallback text editor.
///
/// Avoids shell wrappers (`cmd.exe /C start`) to prevent shell metacharacter injection
/// and hanging processes on paths containing spaces or special characters.
pub fn open_log_file() -> Result<(), String> {
    let path = get_log_path();
    if !path.exists() {
        let _ = log_entry("INFO", "Log file initialized.");
    }

    #[cfg(target_os = "windows")]
    {
        // Try opening with explorer.exe (opens associated application without shell injection)
        let opened = std::process::Command::new("explorer.exe")
            .arg(&path)
            .spawn()
            .is_ok();

        if !opened {
            std::process::Command::new("notepad.exe")
                .arg(&path)
                .spawn()
                .map_err(|e| format!("Failed to open log file: {e}"))?;
        }
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        std::process::Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open log file: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::thread;
    use tempfile::tempdir;

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn test_format_timestamp_known_date() {
        // Unix timestamp 1700000000 = 2023-11-14 22:13:20 UTC
        let st = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1700000000);
        let formatted = format_timestamp(st);
        assert_eq!(formatted, "2023-11-14 22:13:20");
    }

    #[test]
    fn test_log_entry_and_file_writing() {
        let _guard = TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().expect("tempdir");
        let log_file = dir.path().join("test-archive-trimmer.log");

        init_log_path(Some(log_file.clone()));

        let line = log_entry("INFO", "Scanner initialized");
        assert!(line.contains("[INFO] Scanner initialized"));

        let content = std::fs::read_to_string(&log_file).expect("read log");
        assert!(content.contains("[INFO] Scanner initialized"));

        clear_log_file().expect("clear log");
        let content_after = std::fs::read_to_string(&log_file).expect("read log after clear");
        assert_eq!(content_after, "");
    }

    #[test]
    fn test_concurrent_log_writes_thread_safety() {
        let _guard = TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().expect("tempdir");
        let log_file = dir.path().join("concurrent-archive-trimmer.log");

        init_log_path(Some(log_file.clone()));

        let mut handles = Vec::new();
        for thread_idx in 0..8 {
            let handle = thread::spawn(move || {
                for msg_idx in 0..20 {
                    log_entry("INFO", &format!("Thread {thread_idx} message {msg_idx}"));
                }
            });
            handles.push(handle);
        }

        for h in handles {
            h.join().expect("thread join");
        }

        let content = std::fs::read_to_string(&log_file).expect("read concurrent log");
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 160);
    }
}
