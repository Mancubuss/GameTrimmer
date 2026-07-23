//! Opt-in diagnostic log file (`gametrimmer.log`, next to the executable).
//!
//! Every existing diagnostic in this crate goes to `eprintln!`, which is
//! invisible in a release build (`windows_subsystem = "windows"` - see
//! `main.rs` - has no console attached). This module gives the user a way to
//! capture those diagnostics to a file when something goes wrong, without
//! changing anything about the dev-console experience: [`log`] always
//! `eprintln!`s its message first, then *additionally* appends it to the log
//! file when logging is enabled.
//!
//! State is a single global `Mutex<Option<File>>` - `Some` means enabled
//! (and holds the open file), `None` means disabled. There is deliberately
//! no separate "enabled" flag to keep in sync with the file handle: the two
//! could never usefully disagree.
//!
//! Failure handling is entirely non-fatal: a file that can't be opened, or a
//! write that fails partway through a session (disk full, the log file
//! deleted out from under the process, ...), disables logging (after one
//! `eprintln!` explaining why) rather than panicking or retrying in a loop.
//! Diagnostics are a nice-to-have; they must never be able to take the app
//! down.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use windows::Win32::System::SystemInformation::GetLocalTime;

/// `Some(file)` when logging is enabled and the file is open; `None` when
/// disabled (the default at process start - nothing is written until
/// `set_enabled(true, ..)` is called, see `app::GameTrimmerApp::new`).
static STATE: Mutex<Option<File>> = Mutex::new(None);

/// Enables or disables the diagnostic log file. `elevated` and the app
/// version are recorded in a one-line header written at the top of every
/// session, so a shared log file makes it obvious where one run ended and
/// the next began.
///
/// Enabling opens `log_path` in append+create mode - an existing file (from
/// an earlier session) is kept, never truncated, so the log accumulates
/// history across restarts until the user clears it themselves. If opening
/// fails (permissions, a locked file, ...) this logs the error once via
/// `eprintln!` and leaves logging disabled - never panics, never blocks the
/// caller.
///
/// Disabling simply drops the open file handle.
pub fn set_enabled(enabled: bool, elevated: bool, log_path: &Path) {
    let mut state = lock_state();

    if !enabled {
        *state = None;
        return;
    }

    match File::options().create(true).append(true).open(log_path) {
        Ok(mut file) => {
            let header = format!(
                "=== GameTrimmer {} session start (elevated: {elevated}) ===\n",
                env!("CARGO_PKG_VERSION")
            );
            // A header write failure is not worth disabling logging over -
            // the file is open and later `log()` calls can still append to
            // it; only report it once here.
            if let Err(err) = file.write_all(header.as_bytes()) {
                eprintln!("Failed to write diagnostic log header: {err}");
            }
            *state = Some(file);
        }
        Err(err) => {
            eprintln!(
                "Failed to open diagnostic log file {}: {err}",
                log_path.display()
            );
            *state = None;
        }
    }
}

/// Logs a diagnostic message. Always `eprintln!`s it first (preserving
/// today's dev-console behavior exactly, unconditionally), then - only when
/// logging is enabled - appends `"[{timestamp}] {msg}"` to the log file. A
/// write failure disables logging (after one `eprintln!`) rather than
/// retrying it on every subsequent call, which would otherwise turn one bad
/// write into an endless error loop.
pub fn log(msg: &str) {
    eprintln!("{msg}");

    let mut state = lock_state();
    let Some(file) = state.as_mut() else {
        return;
    };

    // SAFETY: `GetLocalTime` takes no arguments and simply fills in and
    // returns a `SYSTEMTIME` value on the stack - there is no buffer or
    // pointer for the caller to get wrong.
    let now = unsafe { GetLocalTime() };
    let line = format!("[{}] {msg}\n", format_timestamp(&now));

    if let Err(err) = file.write_all(line.as_bytes()) {
        eprintln!("Diagnostic log write failed, disabling logging: {err}");
        *state = None;
    }
}

/// Locks [`STATE`], recovering from a poisoned mutex rather than panicking.
/// A panic on some other thread while this lock was held must not also take
/// down logging for the rest of the process - the recovered guard's data is
/// still perfectly usable (a `File` handle can't be left in a half-written
/// state by a panic elsewhere).
fn lock_state() -> std::sync::MutexGuard<'static, Option<File>> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Formats a `SYSTEMTIME` (local wall-clock time, from `GetLocalTime`) as
/// `YYYY-MM-DD HH:MM:SS`. A pure, unit-testable wrapper around the otherwise
/// untestable Win32 call in [`log`].
fn format_timestamp(st: &windows::Win32::Foundation::SYSTEMTIME) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes every test in this module: they all share the same global
    /// `STATE`, so running them concurrently (the default with `cargo test`)
    /// would have one test's `set_enabled` clobber another's in-flight
    /// assertions. Each test also unconditionally disables logging again
    /// before returning (even on assertion failure, via a guard-less direct
    /// call at the end), so no test leaks enabled state to whichever test
    /// happens to run next.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock_tests() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn disabled_logging_writes_nothing() {
        let _guard = lock_tests();
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("gametrimmer.log");

        set_enabled(false, false, &path);
        log("hello, this must not reach disk");

        assert!(
            !path.exists(),
            "no file should be created while logging is disabled"
        );
    }

    #[test]
    fn enabled_logging_writes_header_and_message() {
        let _guard = lock_tests();
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("gametrimmer.log");

        set_enabled(true, true, &path);
        log("a diagnostic message");
        // Restore the global disabled state for whichever test runs next.
        set_enabled(false, false, &path);

        let contents = std::fs::read_to_string(&path).expect("read log file");
        let mut lines = contents.lines();

        let header = lines.next().expect("header line");
        assert!(header.contains("GameTrimmer"));
        assert!(header.contains("session start"));
        assert!(header.contains("elevated: true"));

        let message_line = lines.next().expect("logged message line");
        assert!(
            message_line.starts_with('['),
            "logged line should start with a [timestamp]: {message_line}"
        );
        assert!(message_line.contains("a diagnostic message"));
    }

    #[test]
    fn format_timestamp_zero_pads_every_field() {
        let st = windows::Win32::Foundation::SYSTEMTIME {
            wYear: 2026,
            wMonth: 1,
            wDayOfWeek: 0,
            wDay: 5,
            wHour: 9,
            wMinute: 3,
            wSecond: 7,
            wMilliseconds: 0,
        };

        assert_eq!(format_timestamp(&st), "2026-01-05 09:03:07");
    }
}
