//! Diagnostic log file (`gametrimmer.log`, next to the executable).
//!
//! Every existing diagnostic in this crate goes to `eprintln!`, which is
//! invisible in a release build (`windows_subsystem = "windows"` - see
//! `main.rs` - has no console attached). This module gives the user a way to
//! retain those diagnostics when something goes wrong, without
//! changing anything about the dev-console experience: [`log`] always
//! `eprintln!`s its message first, then *additionally* appends it to the log
//! file when logging is enabled.
//!
//! State is a single global `Mutex<Option<OpenLog>>` - `Some` means enabled
//! (and holds the open file plus the path it was opened at), `None` means
//! disabled. There is deliberately no separate "enabled" flag to keep in sync
//! with the file handle: the two could never usefully disagree.
//!
//! Failure handling is entirely non-fatal: a file that can't be opened, or a
//! write that fails partway through a session (disk full, the log file
//! deleted out from under the process, ...), disables logging (after one
//! `eprintln!` explaining why) rather than panicking or retrying in a loop.
//! Diagnostics are a nice-to-have; they must never be able to take the app
//! down.
//!
//! [`install_panic_hook`] routes panics here too, which is the one diagnostic
//! that has nowhere else to go: with `windows_subsystem = "windows"` a
//! release build has no console for the default hook's message to land in.
//!
//! Timestamps carry their UTC offset - `2026-08-14 10:44:02+03:00`. The
//! database stores Unix seconds, so a bare local wall clock could only be
//! lined up against it by someone who knew what time zone the machine was
//! in; on a log attached to a bug report, nobody does.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};

/// `GetTimeZoneInformation`'s return values. The `windows` crate binds only
/// `TIME_ZONE_ID_INVALID` of the four - the other three are plain SDK
/// `#define`s that its metadata does not carry - so they are spelled out
/// here rather than reached for from a module that does not have them.
const TIME_ZONE_ID_STANDARD: u32 = 1;
const TIME_ZONE_ID_DAYLIGHT: u32 = 2;

/// The open log file, and the path it was opened at. The path is kept so
/// [`set_enabled`] can tell "enable the file already being written to" (a
/// no-op) from "enable a different file" (a real re-open) - see there for
/// why that distinction has to exist.
struct OpenLog {
    path: PathBuf,
    file: File,
}

/// `Some(..)` when logging is enabled and the file is open; `None` when
/// disabled at process start. `main` applies the saved setting before the
/// window is built, and `GameTrimmerApp::new` re-applies it; that setting is
/// enabled by default but remains user-controlled.
static STATE: Mutex<Option<OpenLog>> = Mutex::new(None);

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
/// Enabling the file that is *already* open is a no-op, header included.
/// `main` opens the log as early as the saved preference can be read (so a
/// panic during startup still leaves a trace), and `GameTrimmerApp::new_with`
/// applies the same preference again once settings are properly loaded -
/// without this, one run would write two session headers and read as two.
/// A *different* path always re-opens, which is what keeps parallel tests
/// (each with its own temp log) from inheriting each other's file.
///
/// Disabling simply drops the open file handle.
pub fn set_enabled(enabled: bool, elevated: bool, log_path: &Path) {
    let mut state = lock_state();

    if !enabled {
        *state = None;
        return;
    }

    if state.as_ref().is_some_and(|open| open.path == log_path) {
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
            *state = Some(OpenLog {
                path: log_path.to_path_buf(),
                file,
            });
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
    let Some(open) = state.as_mut() else {
        return;
    };

    let line = format!("[{}] {msg}\n", local_timestamp());

    if let Err(err) = open.file.write_all(line.as_bytes()) {
        eprintln!("Diagnostic log write failed, disabling logging: {err}");
        *state = None;
    }
}

/// Routes every panic, on every thread, into the diagnostic log.
///
/// Without this the app's worst failures are its least documented ones. A
/// release build sets `windows_subsystem = "windows"` (`main.rs:1`), so the
/// default hook's message goes to a stderr nothing is attached to; and the
/// three `catch_unwind` call sites (`worker/scan/discovery.rs`,
/// `worker/scan.rs`, `core/mftscan/mod.rs`) deliberately *swallow* the panics
/// they contain, converting them into a degraded scan rather than a crash. So
/// a panic in the UI thread, the writer thread, the delete path or startup
/// used to leave no artifact at all, and a contained one left only "provider
/// X failed".
///
/// Call once, as early in `main` as possible: this replaces the default hook
/// rather than chaining onto it, because the report below already carries
/// everything the default hook prints and [`log`] `eprintln!`s it, so a dev
/// console sees the same information exactly once.
///
/// Note on the backtrace in release builds: `strip = true` and no PDB mean
/// the frames symbolize to module-relative addresses rather than function
/// names. The `location()` line - a `&'static str` compiled into the binary -
/// is the part that stays readable, and it is the part that names the bug.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        // Captured unconditionally rather than through `Backtrace::capture`,
        // which would honour `RUST_BACKTRACE` - an env var no user reporting
        // a crash will have set.
        let backtrace = std::backtrace::Backtrace::force_capture();
        let thread = std::thread::current();
        log(&format_panic_report(
            info.payload_as_str(),
            info.location()
                .map(|location| location.to_string())
                .as_deref(),
            thread.name(),
            &format!("{:?}", thread.id()),
            &backtrace.to_string(),
        ));
    }));
}

/// Builds the multi-line panic report [`install_panic_hook`] writes. Split
/// out as a pure function because everything else in the hook (the live
/// backtrace, the thread identity, the global log file) is untestable.
///
/// The thread id accompanies the name because the threads that matter here
/// are unnamed: `dispatch_scans`' rayon workers and the writer thread are
/// plain `thread::spawn`s, so "unnamed" alone could not tell two concurrent
/// panics apart.
fn format_panic_report(
    payload: Option<&str>,
    location: Option<&str>,
    thread_name: Option<&str>,
    thread_id: &str,
    backtrace: &str,
) -> String {
    format!(
        "PANIC in thread {} ({}) at {}: {}\n{}",
        thread_name.unwrap_or("<unnamed>"),
        thread_id,
        location.unwrap_or("an unknown location"),
        // A payload that is neither `&str` nor `String` has no readable form
        // to print; naming that explicitly beats an empty message that reads
        // like a panic with no reason.
        payload.unwrap_or("<non-string panic payload>"),
        backtrace,
    )
}

/// Locks [`STATE`], recovering from a poisoned mutex rather than panicking.
/// A panic on some other thread while this lock was held must not also take
/// down logging for the rest of the process - the recovered guard's data is
/// still perfectly usable (a `File` handle can't be left in a half-written
/// state by a panic elsewhere).
fn lock_state() -> std::sync::MutexGuard<'static, Option<OpenLog>> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Serializes every test in the crate that enables the process-global
/// [`STATE`], not only the ones in this module: `app`'s tests assert that a
/// fatal message reaches a real log file, which means enabling logging for
/// the duration. Running that concurrently with a `logger` test would have
/// one replace the other's open file handle mid-assertion.
///
/// Lives beside `STATE` rather than in the test module below because the
/// thing it protects lives here - a second lock elsewhere would guard
/// nothing.
#[cfg(test)]
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Takes the crate-wide logging test lock, recovering from poisoning for the
/// same reason [`lock_state`] does: one failed assertion must not turn every
/// later logging test into a panic about the lock rather than about the code.
///
/// A test that takes this lock is responsible for calling
/// `set_enabled(false, ..)` before it returns, so it does not leak enabled
/// state to whichever test runs next.
#[cfg(test)]
pub(crate) fn lock_for_test() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Reads the wall clock and the zone it is in, and formats the pair.
///
/// The two Win32 calls are together here so [`format_timestamp`] stays pure
/// and testable - the interesting part is the formatting, not the reading.
fn local_timestamp() -> String {
    // SAFETY: `GetLocalTime` takes no arguments and simply fills in and
    // returns a `SYSTEMTIME` value on the stack - there is no buffer or
    // pointer for the caller to get wrong.
    let now = unsafe { GetLocalTime() };
    format_timestamp(&now, local_offset_minutes())
}

/// Minutes east of UTC for the machine's current zone, daylight saving
/// included.
///
/// Win32 states the bias the other way round - `Bias` is the number of
/// minutes to *add* to local time to get UTC - so both terms are negated
/// here to produce the sign an ISO-8601 offset uses. The seasonal component
/// comes from whichever bias the API says is in effect, rather than from
/// comparing dates against the transition rules ourselves.
fn local_offset_minutes() -> i32 {
    let mut info = TIME_ZONE_INFORMATION::default();
    // SAFETY: `GetTimeZoneInformation` fills the caller-allocated
    // `TIME_ZONE_INFORMATION` above; the pointer is valid, correctly
    // aligned, and lives for the whole call.
    let id = unsafe { GetTimeZoneInformation(&mut info) };
    let seasonal = match id {
        TIME_ZONE_ID_DAYLIGHT => info.DaylightBias,
        TIME_ZONE_ID_STANDARD => info.StandardBias,
        // `TIME_ZONE_ID_UNKNOWN` (a zone with no DST rules) and the error
        // return both mean there is no seasonal component to apply. An
        // error here is not worth reporting: it would report once per line,
        // and the base bias is still the best answer available.
        _ => 0,
    };
    -(info.Bias + seasonal)
}

/// Formats a `SYSTEMTIME` (local wall-clock time, from `GetLocalTime`) with
/// its UTC offset as `YYYY-MM-DD HH:MM:SS+HH:MM`.
///
/// The offset is what makes the line reconcilable with the database, which
/// stores Unix seconds: without it, a timestamp read on a different machine
/// is only interpretable by someone who knows which zone produced it - and
/// on a log attached to a bug report, nobody does.
fn format_timestamp(st: &windows::Win32::Foundation::SYSTEMTIME, offset_minutes: i32) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}{}",
        st.wYear,
        st.wMonth,
        st.wDay,
        st.wHour,
        st.wMinute,
        st.wSecond,
        format_offset(offset_minutes),
    )
}

/// Formats a UTC offset in minutes as `+HH:MM` / `-HH:MM`.
///
/// Minutes and not just hours: India is +05:30, Nepal +05:45, Chatham
/// +12:45. An hours-only offset would be silently wrong for those and
/// correct everywhere the developer is likely to test.
fn format_offset(minutes: i32) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let magnitude = minutes.unsigned_abs();
    format!("{sign}{:02}:{:02}", magnitude / 60, magnitude % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes every test that shares the global `STATE` - see
    /// [`lock_for_test`], which is where the lock itself lives now that
    /// `app`'s tests need it too. Each test here also unconditionally
    /// disables logging again before returning, so no test leaks enabled
    /// state to whichever test happens to run next.
    use super::lock_for_test as lock_tests;

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

    /// GT-114. The end of the chain that matters: a real panic, caught so the
    /// test survives it, has to leave its message and its `file:line` in the
    /// log file. Before the hook existed this produced nothing anywhere in a
    /// release build.
    #[test]
    fn a_panic_reaches_the_log_file() {
        let _guard = lock_tests();
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("gametrimmer.log");

        // Restored before returning: the hook is process-global, and leaving
        // ours installed would have every later test's failure report through
        // a log file inside a deleted temp dir.
        let previous = std::panic::take_hook();
        set_enabled(true, false, &path);
        install_panic_hook();

        let result = std::panic::catch_unwind(|| panic!("gt_probe_panic_payload"));

        std::panic::set_hook(previous);
        set_enabled(false, false, &path);

        assert!(result.is_err(), "the probe was supposed to panic");
        let contents = std::fs::read_to_string(&path).expect("read log file");
        assert!(
            contents.contains("gt_probe_panic_payload"),
            "the panic message should be in the log: {contents}",
        );
        assert!(
            contents.contains("logger.rs:"),
            "the panic location should be in the log: {contents}",
        );
    }

    /// A caught panic still reports. The three `catch_unwind` sites exist to
    /// degrade one provider or volume instead of killing the scan, which used
    /// to mean the panic itself was never named anywhere - only its
    /// consequence was.
    #[test]
    fn the_report_names_thread_location_and_payload() {
        let report = format_panic_report(
            Some("index out of bounds"),
            Some("crates/app/src/worker/scan.rs:908:5"),
            None,
            "ThreadId(7)",
            "   0: some_frame\n",
        );

        assert!(report.starts_with("PANIC in thread <unnamed> (ThreadId(7)) at "));
        assert!(report.contains("crates/app/src/worker/scan.rs:908:5"));
        assert!(report.contains("index out of bounds"));
        assert!(report.contains("some_frame"));
    }

    /// A payload that is neither `&str` nor `String` must still produce a
    /// report - `panic_any` is rare but the hook has no say in what reaches
    /// it, and an empty message would read as a panic with no reason.
    #[test]
    fn the_report_survives_a_payload_it_cannot_read() {
        let report = format_panic_report(None, None, Some("main"), "ThreadId(1)", "");

        assert!(report.contains("<non-string panic payload>"));
        assert!(report.contains("an unknown location"));
        assert!(report.contains("thread main"));
    }

    /// Re-enabling the file that is already open must not write a second
    /// session header: `main` opens the log from the saved preference and
    /// `GameTrimmerApp::new_with` applies that preference again, so without
    /// this one run would read as two in every log.
    #[test]
    fn enabling_the_already_open_file_does_not_add_a_second_header() {
        let _guard = lock_tests();
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("gametrimmer.log");

        set_enabled(true, false, &path);
        set_enabled(true, false, &path);
        log("one message");
        set_enabled(false, false, &path);

        let contents = std::fs::read_to_string(&path).expect("read log file");
        assert_eq!(
            contents.matches("session start").count(),
            1,
            "one run must write one header: {contents}",
        );
    }

    /// The other half of the same rule: a *different* path always re-opens.
    /// Parallel tests each own a temp log, and inheriting the previous test's
    /// file would have them assert against each other's output.
    #[test]
    fn enabling_a_different_file_switches_to_it() {
        let _guard = lock_tests();
        let dir = tempfile::tempdir().expect("create temp dir");
        let first = dir.path().join("first.log");
        let second = dir.path().join("second.log");

        set_enabled(true, false, &first);
        set_enabled(true, false, &second);
        log("goes to the second file");
        set_enabled(false, false, &second);

        assert!(!std::fs::read_to_string(&first)
            .expect("read the first log")
            .contains("goes to the second file"));
        assert!(std::fs::read_to_string(&second)
            .expect("read the second log")
            .contains("goes to the second file"));
    }

    /// GT-126. Half-hour and quarter-hour zones are real (India +05:30,
    /// Nepal +05:45, Chatham +12:45), and an hours-only offset would be
    /// correct everywhere this is likely to be tested and wrong there.
    #[test]
    fn the_offset_carries_minutes_and_a_sign() {
        assert_eq!(format_offset(180), "+03:00");
        assert_eq!(format_offset(330), "+05:30");
        assert_eq!(format_offset(345), "+05:45");
        assert_eq!(format_offset(0), "+00:00");
        assert_eq!(format_offset(-300), "-05:00");
        assert_eq!(format_offset(-210), "-03:30");
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

        assert_eq!(format_timestamp(&st, 180), "2026-01-05 09:03:07+03:00");
    }
}
