//! Headless (CLI) mode - GT-10.
//!
//! The same portable exe runs without a GUI when given CLI flags: it scans,
//! reports what it would remove, and (only with `--apply`) removes the profile's
//! auto-selected findings, communicating solely through an exit code and a text
//! report. This is a *second front end over the same worker layer* - it drives
//! [`crate::worker::scan`] and [`crate::worker::delete`] exactly as the GUI does,
//! draining their [`WorkerMsg`] stream on this thread instead of the egui event
//! loop. No scanning/classification/deletion logic is duplicated here.
//!
//! The worker API takes an [`egui::Context`] (to wake the UI while minimized);
//! headless mode hands it a default context whose `request_repaint` is a
//! harmless no-op with no viewer attached.

mod args;
mod report;

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use eframe::egui;
use gametrimmer_core::settings::{self, Settings};

pub use args::{parse_invocation, HeadlessConfig, Invocation, Mode};

use crate::model::{self, FindingItem, FindingRow};
use crate::worker::delete::{space_tally, DeleteItem};
use crate::worker::scan::ScanOptions;
use crate::worker::{self, RemoveOutcome, WorkerMsg};
use report::{ApplyReport, ReportData, EXIT_RUNTIME, EXIT_USAGE};

const USAGE: &str = "\
GameTrimmer - headless mode

USAGE:
    gametrimmer [FLAGS]

Running with no flags launches the graphical app.

FLAGS:
    --scan               Scan libraries and report (deletes nothing).
    --dry-run            Same as --scan: report only, delete nothing (default).
    --apply              Delete the profile's auto-selected findings.
                         Requires --profile; the only mode that removes files.
    --profile <name>     Selection profile: cautious | balanced | aggressive | custom.
                         Optional for a dry run (defaults to the saved setting);
                         mandatory for --apply.
    --report <path>      Also write the full text report to <path>.
    -h, --help           Print this help and exit.
    -V, --version        Print the version and exit.

EXIT CODES:
    0  success            1  bad arguments
    2  runtime error      3  apply completed with some deletion failures
";

/// What [`run_from_env`] decided the process should do.
pub enum Outcome {
    /// No CLI flag was given - the caller should launch the GUI as usual.
    LaunchGui,
    /// A headless run (or help/version/usage error) finished; exit with this code.
    Exit(u8),
}

/// Parses `std::env::args()` and, for anything but the plain GUI launch, runs
/// it to completion. Returns [`Outcome::LaunchGui`] when no CLI flag is present
/// so `main` falls through to `eframe`.
pub fn run_from_env() -> Outcome {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_invocation(&args) {
        Invocation::Gui => Outcome::LaunchGui,
        Invocation::Help => {
            attach_console();
            println!("{USAGE}");
            Outcome::Exit(0)
        }
        Invocation::Version => {
            attach_console();
            println!("GameTrimmer {}", env!("CARGO_PKG_VERSION"));
            Outcome::Exit(0)
        }
        Invocation::Error(msg) => {
            attach_console();
            eprintln!("Помилка аргументів: {msg}\n");
            eprint!("{USAGE}");
            Outcome::Exit(EXIT_USAGE)
        }
        Invocation::Headless(config) => {
            attach_console();
            Outcome::Exit(run_headless(config))
        }
    }
}

/// Runs one headless job end to end and returns its exit code.
fn run_headless(config: HeadlessConfig) -> u8 {
    let db_path = match worker::db_path() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("Не вдалося визначити шлях до бази даних: {err}");
            return EXIT_RUNTIME;
        }
    };

    let elevated = crate::elevation::is_elevated();

    let settings = match load_settings(&db_path) {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!(
                "Не вдалося відкрити базу даних {}: {err}",
                db_path.display()
            );
            return EXIT_RUNTIME;
        }
    };

    // Diagnostics: always keep a full log for an --apply run (the only mode that
    // removes files), and honor the user's setting otherwise. Best-effort - a
    // log that can't be opened never blocks the run.
    let want_log = matches!(config.mode, Mode::Apply) || settings.logging_enabled;
    if want_log {
        if let Ok(log_path) = worker::log_path() {
            crate::logger::set_enabled(true, elevated, &log_path);
        }
    }

    // An explicit --profile wins; otherwise the persisted profile decides, just
    // as it does for the GUI's post-scan selection.
    let profile = config.profile.unwrap_or(settings.selection_profile);
    let lang = settings.app_language;

    crate::logger::log(&format!(
        "CLI run: mode={:?}, profile={}, elevated={elevated}",
        config.mode,
        profile.as_str()
    ));

    let options = ScanOptions {
        lang,
        keep_languages: settings.keep_languages.clone(),
        scan_routing: settings.scan_routing,
        enabled_categories: settings.enabled_categories.clone(),
    };

    let (findings, scan_summary) = match run_scan_headless(&db_path, elevated, options) {
        Ok(result) => result,
        Err(err) => {
            eprintln!("Помилка сканування: {err}");
            return EXIT_RUNTIME;
        }
    };

    // Apply the selection profile exactly like the GUI does on a fresh scan.
    let items: Vec<FindingItem> = findings
        .into_iter()
        .map(|row| {
            let selected =
                model::profile_auto_selects(profile, row.display_category(), row.confidence);
            FindingItem {
                row,
                selected,
                removed: false,
            }
        })
        .collect();

    let cards = model::plan_cards(&items);
    let total_findings = items.len();
    let selected: Vec<&FindingItem> = items.iter().filter(|item| item.selected).collect();
    let selected_count = selected.len();
    let selected_size_on_disk: u64 = selected.iter().map(|item| item.row.size_on_disk).sum();

    // For --apply, delete the selected set through the same worker the GUI uses.
    let apply = match config.mode {
        Mode::DryRun => None,
        Mode::Apply => {
            let delete_items: Vec<DeleteItem> = selected
                .iter()
                .map(|item| DeleteItem {
                    file_id: item.row.file_id,
                    full_path: item.row.install_dir.join(&item.row.rel_path),
                    size_on_disk: item.row.size_on_disk,
                })
                .collect();

            if delete_items.is_empty() {
                // Nothing selected: report an empty apply rather than spawning a
                // no-op delete job.
                Some(ApplyReport {
                    method: settings.delete_method,
                    expected: 0,
                    freed: 0,
                    recycled_pending: 0,
                    succeeded: 0,
                    failed: 0,
                })
            } else {
                match run_delete_headless(&db_path, delete_items, settings.delete_method, lang) {
                    Ok(outcomes) => Some(apply_report(settings.delete_method, &outcomes)),
                    Err(err) => {
                        eprintln!("Помилка видалення: {err}");
                        return EXIT_RUNTIME;
                    }
                }
            }
        }
    };

    let data = ReportData {
        mode: config.mode,
        profile,
        elevated,
        scan_summary,
        cards,
        total_findings,
        selected_count,
        selected_size_on_disk,
        apply,
    };

    let text = report::format_report(&data, lang);
    print!("{text}");

    if let Some(path) = &config.report {
        if let Err(err) = std::fs::write(path, &text) {
            eprintln!("Не вдалося записати звіт у {}: {err}", path.display());
            return EXIT_RUNTIME;
        }
        eprintln!("Звіт записано: {}", path.display());
    }

    data.exit_code()
}

/// Opens the database (creating/migrating the schema on first use, same as the
/// GUI's own first connection) and reads the persisted settings, dropping the
/// connection before the scan opens its own.
fn load_settings(db_path: &Path) -> Result<Settings, String> {
    let conn = gametrimmer_core::db::open(db_path).map_err(|err| err.to_string())?;
    settings::load(&conn).map_err(|err| err.to_string())
}

/// Drives one scan to completion on this thread, draining the worker's message
/// stream. Returns the findings and the scanner's own summary line, or an
/// already-user-facing error string if the scan reported one.
fn run_scan_headless(
    db_path: &Path,
    elevated: bool,
    options: ScanOptions,
) -> Result<(Vec<FindingRow>, String), String> {
    let (tx, rx) = std::sync::mpsc::channel::<WorkerMsg>();
    // No viewer is attached, so the context's repaint requests are no-ops; it
    // exists only because the shared worker API pairs a sender with one.
    let ctx = egui::Context::default();
    // Headless runs never cancel (no Stop button); the flag is required by the
    // worker API and simply stays false.
    let cancel = Arc::new(AtomicBool::new(false));

    let handle =
        worker::scan::spawn_scan(db_path.to_path_buf(), cancel, tx, ctx, elevated, options);

    let mut result: Option<(Vec<FindingRow>, String)> = None;
    let mut error: Option<String> = None;
    for msg in rx {
        match msg {
            WorkerMsg::Status { text } => eprintln!("{text}"),
            WorkerMsg::LibrariesFound { libraries, games } => {
                eprintln!("Бібліотек: {libraries}, ігор: {games}");
            }
            WorkerMsg::Warning { msg } => eprintln!("Попередження: {msg}"),
            WorkerMsg::Done {
                findings,
                scan_summary,
                ..
            } => result = Some((findings, scan_summary)),
            WorkerMsg::Error { msg } => error = Some(msg),
            WorkerMsg::Cancelled => error = Some("сканування скасовано".to_string()),
            // Per-game progress would spam the console; the summary line covers it.
            _ => {}
        }
    }
    let _ = handle.join();

    match (result, error) {
        (Some(result), _) => Ok(result),
        (None, Some(err)) => Err(err),
        (None, None) => Err("сканування завершилося без результату".to_string()),
    }
}

/// Drives one delete batch to completion on this thread, returning the per-file
/// outcomes once the worker reports `RemoveDone`.
fn run_delete_headless(
    db_path: &Path,
    items: Vec<DeleteItem>,
    method: settings::DeleteMethod,
    lang: settings::Lang,
) -> Result<Vec<RemoveOutcome>, String> {
    let (tx, rx) = std::sync::mpsc::channel::<WorkerMsg>();
    let ctx = egui::Context::default();

    let handle = worker::delete::spawn_delete(db_path.to_path_buf(), items, method, tx, lang, ctx);

    let mut outcomes: Option<Vec<RemoveOutcome>> = None;
    let mut error: Option<String> = None;
    for msg in rx {
        match msg {
            WorkerMsg::Warning { msg } => eprintln!("Попередження: {msg}"),
            WorkerMsg::RemoveDone { outcomes: o, .. } => outcomes = Some(o),
            WorkerMsg::Error { msg } => error = Some(msg),
            _ => {}
        }
    }
    let _ = handle.join();

    match (outcomes, error) {
        (Some(outcomes), _) => Ok(outcomes),
        (None, Some(err)) => Err(err),
        (None, None) => Err("видалення завершилося без результату".to_string()),
    }
}

/// Folds a delete batch's outcomes into an [`ApplyReport`]: the on-disk space
/// tally (reusing [`space_tally`], the same accounting the GUI's summary shows)
/// plus per-file success/failure counts.
fn apply_report(method: settings::DeleteMethod, outcomes: &[RemoveOutcome]) -> ApplyReport {
    let tally = space_tally(method, outcomes);
    let failed = outcomes.iter().filter(|o| o.error.is_some()).count();
    ApplyReport {
        method,
        expected: tally.expected,
        freed: tally.freed,
        recycled_pending: tally.recycled_pending,
        succeeded: outcomes.len() - failed,
        failed,
    }
}

/// Best-effort attach to the parent process's console so `println!`/`eprintln!`
/// reach the terminal a user launched the (GUI-subsystem) exe from. A failure -
/// no parent console, or a console already allocated in a debug build - is
/// ignored: console output is a convenience, the `--report` file is the
/// authoritative output.
#[cfg(windows)]
fn attach_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: `AttachConsole` takes a process id; `ATTACH_PARENT_PROCESS` is the
    // documented sentinel for "the parent process's console". It has no output
    // parameters and no buffers to misuse; a failure is expected and ignored.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_console() {}
