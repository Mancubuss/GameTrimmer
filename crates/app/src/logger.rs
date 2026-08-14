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
//! # The shape of a line
//!
//! ```text
//! [2026-08-14 10:44:02+03:00] [ERROR] [scan 42] Failed to record scan timing: ...
//! [2026-08-14 10:44:03+03:00] [INFO] Scan cancelled
//! ```
//!
//! Each of the three leading fields answers a question the log could not
//! answer before:
//!
//! - **The offset** makes the timestamp reconcilable with the database,
//!   which stores Unix seconds. A bare local wall clock is only
//!   interpretable by someone who knows what time zone the machine was in.
//! - **The level** is what lets the diagnostic bundle's `errors.txt` show
//!   the failures instead of the whole tail.
//! - **The scan generation** ties a line to its `scan_runs` row. Two scans
//!   in one session were previously separated by nothing at all.
//!
//! Messages themselves are always English, whatever the interface language
//! is set to (see [`log`]).

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};

/// `GetTimeZoneInformation`'s return values. The `windows` crate binds only
/// `TIME_ZONE_ID_INVALID` of the four - the other three are plain SDK
/// `#define`s that its metadata does not carry - so they are spelled out
/// here rather than reached for from a module that does not have them.
const TIME_ZONE_ID_STANDARD: u32 = 1;
const TIME_ZONE_ID_DAYLIGHT: u32 = 2;

/// Upper bound on the live log file. Past this the current file is renamed
/// aside (see [`rolled_path`]) and a fresh one started, so the pair on disk
/// can reach twice this and no more.
///
/// 5 MB is roughly a hundred thousand lines - months of ordinary use, and
/// still small enough to attach to a bug report. The bundle only ever
/// carries the last 256 KB anyway (`core::bundle::LOG_TAIL_BYTES`); this
/// bound is about the file the user has to live with on disk, not about
/// what gets reported.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Severity of one logged line.
///
/// Deliberately two variants and not the usual five. The only consumer that
/// needs to tell lines apart is the diagnostic bundle's `errors.txt`, and
/// the only question it asks is "did something fail". A `WARN` tier would
/// have to be assigned at 37 call sites by judgement, and every judgement
/// call is a chance for two similar failures to land in different tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Info => "INFO",
            Level::Error => "ERROR",
        }
    }
}

/// The `scan_runs.id` currently being written, or [`NO_SCAN`] outside a scan.
///
/// A global rather than a parameter because the call sites that most need
/// the association are the deepest ones - a persistence failure four calls
/// below `run_scan`, a provider panic inside `catch_unwind` - and threading
/// an id through all of them to reach `logger::log` would be a wide change
/// for a diagnostic field. Set through [`ScanScope`], which restores it on
/// every exit path including a panic.
static SCAN_GENERATION: AtomicI64 = AtomicI64::new(NO_SCAN);

/// Sentinel for "no scan is running". Real ids are SQLite rowids, which
/// start at 1, so zero can never collide with one.
const NO_SCAN: i64 = 0;

/// The open log file, and what is needed to reopen it.
///
/// The path is kept so [`set_enabled`] can tell "enable the file already
/// being written to" (a no-op) from "enable a different file" (a real
/// re-open) - see there for why that distinction has to exist. `elevated`
/// is kept for the same reason: rolling the file mid-session starts a fresh
/// one, and its header has to say the same thing the session's first header
/// did.
///
/// `bytes` is tracked rather than re-`stat`ed per line: the size only
/// changes because this process wrote to it, so a counter is exact and
/// costs nothing, while a `metadata()` call per line would put a syscall in
/// front of every diagnostic.
struct OpenLog {
    path: PathBuf,
    file: File,
    elevated: bool,
    bytes: u64,
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

    open_at(&mut state, elevated, log_path);
}

/// Opens `log_path` for appending, writes the session header and records the
/// resulting size, replacing whatever `state` held.
///
/// Shared by [`set_enabled`] and the mid-session roll in [`write`] so the
/// two cannot drift: a rolled file has to be indistinguishable from one
/// opened at startup, header included, or the second half of a long session
/// reads as a log with no provenance.
fn open_at(state: &mut Option<OpenLog>, elevated: bool, log_path: &Path) {
    roll_if_oversized(log_path);

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
            // From the file, not from the header length: opening appends to
            // whatever an earlier session left behind, and starting the
            // counter at zero would let an already-large file grow by a
            // further `MAX_LOG_BYTES` before the bound noticed.
            let bytes = file.metadata().map(|meta| meta.len()).unwrap_or(0);
            *state = Some(OpenLog {
                path: log_path.to_path_buf(),
                file,
                elevated,
                bytes,
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

/// Where the previous generation goes: `gametrimmer.log` -> `gametrimmer.log.old`.
///
/// One generation, not a numbered series. The reason to keep any history at
/// all is that a user reporting a problem may roll past it while collecting
/// the report; the reason not to keep six is that nobody reads the sixth,
/// and each one is another file to explain in the folder next to the exe.
fn rolled_path(log_path: &Path) -> PathBuf {
    let mut name = log_path.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    log_path.with_file_name(name)
}

/// Renames an oversized log aside, so the caller can open a fresh one.
///
/// Entirely best-effort: if the rename fails (the previous generation is
/// open in an editor, the directory is read-only) the log simply keeps
/// growing, which is the behaviour that existed before the bound and is
/// still better than losing the diagnostic. Called with the log file
/// *closed* - Windows refuses to rename a file this process holds open.
fn roll_if_oversized(log_path: &Path) {
    let Ok(metadata) = std::fs::metadata(log_path) else {
        return;
    };
    if metadata.len() <= MAX_LOG_BYTES {
        return;
    }
    if let Err(err) = std::fs::rename(log_path, rolled_path(log_path)) {
        eprintln!("Could not roll the diagnostic log aside, it will keep growing: {err}");
    }
}

/// Binds every line logged for its lifetime to `scan_id`, and unbinds on
/// drop.
///
/// A guard rather than a matching pair of calls because `run_scan` leaves
/// through cancellation, several early returns and three `catch_unwind`
/// sites; a manual reset would have to be repeated at each of them, and the
/// one that got missed would stamp the next scan's id on lines belonging to
/// no scan at all.
pub struct ScanScope;

impl ScanScope {
    pub fn new(scan_id: i64) -> Self {
        SCAN_GENERATION.store(scan_id, Ordering::Relaxed);
        Self
    }
}

impl Drop for ScanScope {
    fn drop(&mut self) {
        SCAN_GENERATION.store(NO_SCAN, Ordering::Relaxed);
    }
}

/// Logs an informational diagnostic - the default level.
///
/// `msg` is expected to be English regardless of the interface language.
/// The log is read by whoever is diagnosing the problem, not by the user
/// running the app: the user already gets the same failure on the status
/// line in their own language, and a file that switches language depending
/// on a setting is harder to search than one written in either language
/// consistently.
pub fn log(msg: &str) {
    write(Level::Info, msg);
}

/// Logs a failure. Same contract as [`log`]; the only difference is the
/// level stamped on the line, which is what lets a reader - and the
/// diagnostic bundle - find the failures without reading everything.
pub fn error(msg: &str) {
    write(Level::Error, msg);
}

/// Always `eprintln!`s the message first (preserving today's dev-console
/// behavior exactly, unconditionally), then - only when logging is enabled -
/// appends the formatted line to the log file, rolling the file aside first
/// if this line would push it past [`MAX_LOG_BYTES`].
///
/// A write failure disables logging (after one `eprintln!`) rather than
/// retrying it on every subsequent call, which would otherwise turn one bad
/// write into an endless error loop.
fn write(level: Level, msg: &str) {
    eprintln!("{msg}");

    let mut state = lock_state();
    let Some(open) = state.as_ref() else {
        return;
    };

    let line = format_line(level, &local_timestamp(), scan_generation(), msg);
    let addition = line.len() as u64;

    if open.bytes.saturating_add(addition) > MAX_LOG_BYTES {
        // Take the handle down before renaming: Windows refuses to rename a
        // file the calling process still holds open, so a roll attempted
        // around a live handle would fail every time and the bound would
        // never take effect.
        let path = open.path.clone();
        let elevated = open.elevated;
        *state = None;
        open_at(&mut state, elevated, &path);
    }

    let Some(open) = state.as_mut() else {
        return;
    };
    if let Err(err) = open.file.write_all(line.as_bytes()) {
        eprintln!("Diagnostic log write failed, disabling logging: {err}");
        *state = None;
        return;
    }
    open.bytes = open.bytes.saturating_add(addition);
}

/// The `scan_runs.id` currently in scope, if any - see [`ScanScope`].
fn scan_generation() -> Option<i64> {
    match SCAN_GENERATION.load(Ordering::Relaxed) {
        NO_SCAN => None,
        id => Some(id),
    }
}

/// Builds one log line. Pure, so the format every reader and the diagnostic
/// bundle depend on is testable without a clock, a scan or a file.
fn format_line(level: Level, timestamp: &str, scan: Option<i64>, msg: &str) -> String {
    match scan {
        Some(id) => format!("[{timestamp}] [{}] [scan {id}] {msg}\n", level.as_str()),
        None => format!("[{timestamp}] [{}] {msg}\n", level.as_str()),
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

    /// GT-124 and GT-125. The line format is what the diagnostic bundle and
    /// every human reader parse by eye, so it is pinned here rather than
    /// only implied by the file-writing tests.
    #[test]
    fn a_line_carries_its_level_and_its_scan() {
        assert_eq!(
            format_line(Level::Error, "2026-01-05 09:03:07+03:00", Some(42), "boom"),
            "[2026-01-05 09:03:07+03:00] [ERROR] [scan 42] boom\n",
        );
        assert_eq!(
            format_line(Level::Info, "2026-01-05 09:03:07+03:00", None, "idle"),
            "[2026-01-05 09:03:07+03:00] [INFO] idle\n",
        );
    }

    /// The end of the chain for GT-124: a failure has to be findable in the
    /// file without reading every line, which is what `errors.txt` in the
    /// diagnostic bundle depends on.
    #[test]
    fn an_error_is_marked_in_the_file_and_an_info_is_not() {
        let _guard = lock_tests();
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("gametrimmer.log");

        set_enabled(true, false, &path);
        log("an ordinary line");
        error("a failure");
        set_enabled(false, false, &path);

        let contents = std::fs::read_to_string(&path).expect("read log file");
        let error_lines: Vec<_> = contents
            .lines()
            .filter(|line| line.contains("[ERROR]"))
            .collect();
        assert_eq!(error_lines.len(), 1, "{contents}");
        assert!(error_lines[0].contains("a failure"), "{contents}");
        assert!(
            contents.contains("[INFO] an ordinary line"),
            "the default level is INFO: {contents}",
        );
    }

    /// GT-125. Outside a scan there is no generation to name, and stamping
    /// one anyway would tie unrelated lines to whichever scan ran last.
    #[test]
    fn the_scan_generation_appears_only_inside_a_scan() {
        let _guard = lock_tests();
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("gametrimmer.log");

        set_enabled(true, false, &path);
        log("before the scan");
        {
            let _scope = ScanScope::new(42);
            log("during the scan");
        }
        log("after the scan");
        set_enabled(false, false, &path);

        let contents = std::fs::read_to_string(&path).expect("read log file");
        assert!(contents.contains("[scan 42] during the scan"), "{contents}");
        assert!(
            !contents.contains("[scan 42] before the scan")
                && !contents.contains("[scan 42] after the scan"),
            "the generation must not leak outside its scope: {contents}",
        );
    }

    /// GT-125's failure mode: `run_scan` has several early returns and three
    /// `catch_unwind` sites, so the reset has to survive an unwind rather
    /// than depend on reaching a matching call.
    #[test]
    fn a_panic_inside_a_scan_still_clears_the_generation() {
        let _guard = lock_tests();

        let result = std::panic::catch_unwind(|| {
            let _scope = ScanScope::new(7);
            panic!("gt_probe_scan_scope");
        });

        assert!(result.is_err(), "the probe was supposed to panic");
        assert_eq!(scan_generation(), None);
    }

    /// GT-123. The file that already exceeds the bound when a session opens
    /// it is the common case - the growth happened over previous runs - so
    /// the roll has to happen at open, not only after this session writes
    /// enough.
    #[test]
    fn an_oversized_log_is_rolled_aside_when_it_is_opened() {
        let _guard = lock_tests();
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("gametrimmer.log");
        std::fs::write(&path, vec![b'x'; (MAX_LOG_BYTES + 1) as usize]).expect("write a big log");

        set_enabled(true, false, &path);
        log("the first line of the fresh file");
        set_enabled(false, false, &path);

        let fresh = std::fs::read_to_string(&path).expect("read the fresh log");
        assert!(fresh.len() < MAX_LOG_BYTES as usize, "{}", fresh.len());
        assert!(fresh.contains("the first line of the fresh file"));
        let rolled = std::fs::metadata(rolled_path(&path)).expect("the previous log was kept");
        assert_eq!(rolled.len(), MAX_LOG_BYTES + 1);
    }

    /// The other half of the bound: a single session that keeps writing must
    /// roll too, or a long-running scan could grow the file without limit
    /// between restarts.
    #[test]
    fn a_session_that_outgrows_the_bound_rolls_mid_run() {
        let _guard = lock_tests();
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("gametrimmer.log");
        // Just under the bound, so a couple of ordinary lines cross it.
        std::fs::write(&path, vec![b'x'; (MAX_LOG_BYTES - 64) as usize]).expect("write a big log");

        set_enabled(true, false, &path);
        log("this line crosses the bound");
        log("and this one lands in the fresh file");
        set_enabled(false, false, &path);

        let fresh = std::fs::read_to_string(&path).expect("read the fresh log");
        assert!(fresh.len() < MAX_LOG_BYTES as usize, "{}", fresh.len());
        assert!(
            fresh.contains("and this one lands in the fresh file"),
            "{fresh}",
        );
        assert!(
            fresh.contains("session start"),
            "a rolled file still identifies the build that wrote it: {fresh}",
        );
        assert!(rolled_path(&path).is_file(), "the previous log was kept");
    }

    /// Only one generation is kept: rolling twice must replace the previous
    /// `.old`, not accumulate files next to the executable.
    #[test]
    fn rolling_twice_keeps_one_previous_generation() {
        let _guard = lock_tests();
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("gametrimmer.log");

        for _ in 0..2 {
            std::fs::write(&path, vec![b'x'; (MAX_LOG_BYTES + 1) as usize]).expect("write a log");
            set_enabled(true, false, &path);
            set_enabled(false, false, &path);
        }

        let siblings: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read the log directory")
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
            .collect();
        assert_eq!(siblings.len(), 2, "{siblings:?}");
    }
}
