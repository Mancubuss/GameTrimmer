//! Background worker: everything that touches the database or the
//! filesystem runs on a spawned `std::thread`, communicating back to the
//! UI thread through an `mpsc` channel of [`WorkerMsg`].

pub mod clear;
pub mod compact;
pub mod delete;
pub mod load;
pub mod manual;
pub mod rules_io;
pub mod scan;
pub(crate) mod scan_route;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use eframe::egui;

use crate::i18n::Verb;
use crate::model::FindingRow;

const DB_FILE_NAME: &str = "gametrimmer.db";
const LOG_FILE_NAME: &str = "gametrimmer.log";
pub(crate) const RULES_FILE_NAME: &str = "rules.json";
/// Localization-detector data pack (community rules).
pub const L10N_RULES_FILE_NAME: &str = "l10n_rules.json";

/// Messages sent from a worker thread back to the UI thread.
#[derive(Debug)]
pub enum WorkerMsg {
    /// A plain, already-localized status line for a phase that has no
    /// granular `current/total` progress yet (e.g. the scan's library-
    /// discovery and database-preparation phases, which run before the first
    /// per-game `Progress`). Setting it clears any active progress bar, so
    /// the UI shows the spinner + this text rather than a frozen-looking gap.
    Status { text: String },
    /// Libraries discovered and persisted; scanning of individual games is
    /// about to start.
    LibrariesFound { libraries: usize, games: usize },
    /// Granular progress for a long-running operation (scanning games,
    /// deleting files, compacting the database, ...). `verb` names the
    /// operation, rendered before the `current/total` counter via
    /// `i18n::verb_label` (kept as an enum rather than a pre-localized
    /// `&'static str` so the label always reflects the *current* UI
    /// language); `detail` names the item currently being worked on (a game
    /// name for scanning, a file name for deletion). Compaction has no
    /// per-item detail - it leaves `detail` empty and reports an estimated
    /// `current`/100 percent instead (see
    /// `gametrimmer_core::db::compact_observed`); `ui::top_bar` renders that
    /// case as `"{verb} {percent}%"`.
    Progress {
        verb: Verb,
        current: usize,
        total: usize,
        detail: String,
    },
    /// The scan finished successfully with the given findings.
    Done {
        findings: Vec<FindingRow>,
        /// Human-readable summary (in the language active when the scan was
        /// started) of how the scan was carried out (MFT index vs. walkdir
        /// counts, elapsed time) - see `worker::scan_route::format_scan_summary`.
        scan_summary: String,
        /// Live disk-usage snapshot (see `gametrimmer_core::db::occupied_by_library`),
        /// aggregated straight from the `files` table rather than carried
        /// over from `findings` - the flagged-only findings list can't
        /// derive total occupied space on its own.
        occupancy: crate::model::Occupancy,
        /// How long the scan's phases took (see `crate::model::ScanTiming`
        /// and `worker::scan::run_scan`). `Some` for a fresh scan; `None`
        /// when these results were loaded from a previous scan instead (see
        /// `worker::load`) - no scan happened, so there is nothing to time.
        timing: Option<crate::model::ScanTiming>,
    },
    /// A delete operation finished (possibly with some per-file failures).
    /// `occupancy` is recomputed after the deleted files' rows are purged
    /// from the database, so the UI's occupied-space/percent readout reflects
    /// the just-freed space instead of the pre-delete snapshot.
    ///
    /// `method` is the removal method this specific batch actually ran with
    /// (the per-operation choice from the confirmation modal, not the
    /// persisted default), so the post-delete summary can word itself
    /// honestly - "moved to the Recycle Bin" only when it really was.
    RemoveDone {
        outcomes: Vec<RemoveOutcome>,
        occupancy: crate::model::Occupancy,
        method: gametrimmer_core::settings::DeleteMethod,
    },
    /// One file finished being removed successfully mid-batch, so the UI can
    /// drop it from the tree immediately.
    FileRemoved { file_id: i64 },
    /// The scan was cancelled by the user before completion.
    Cancelled,
    /// Something went wrong; `msg` is an already-localized user-facing
    /// description.
    Error { msg: String },
    /// A non-fatal issue during scanning (one provider failed, or a manual
    /// library's folder is currently missing) - the scan continues.
    Warning { msg: String },
    /// The background "Додати теку..." folder picker finished. `None` means
    /// the user cancelled the dialog.
    FolderPicked { path: Option<PathBuf> },
    /// The background "Експортувати..." export finished. `path` is `None`
    /// when the user cancelled the save dialog (in which case `error` is
    /// also `None`); `error` is set if the save dialog returned a path but
    /// writing the CSV failed.
    ExportDone {
        path: Option<PathBuf>,
        error: Option<String>,
    },
    /// The background «Експортувати правила» job finished. `path` is the
    /// folder the two pack files were written into; `path` and `error` both
    /// `None` means the user cancelled the folder picker.
    RulesExportDone {
        path: Option<PathBuf>,
        error: Option<String>,
    },
    /// The background «Імпортувати правила» job finished. `summary` is the
    /// ready-to-show Ukrainian merge summary; `summary` and `error` both
    /// `None` means the user cancelled the file picker.
    RulesImportDone {
        summary: Option<String>,
        error: Option<String>,
    },
    /// The background "Стиснути базу даних" job finished.
    CompactDone {
        error: Option<String>,
        /// The reclaimable share was below the worthwhile threshold, so
        /// `VACUUM` was not run (a cheap WAL checkpoint still happened).
        skipped: bool,
    },
    /// The background "Clear database" job finished (see `worker::clear`).
    /// `error` is `None` on success, in which case the UI resets to the
    /// empty startup state (no findings, nothing scanned yet).
    ClearDone { error: Option<String> },
}

/// Outcome of removing one file, matched back to its `files.id` row.
#[derive(Debug, Clone)]
pub struct RemoveOutcome {
    pub file_id: i64,
    pub path: PathBuf,
    pub error: Option<String>,
    /// True when the row was (or is about to be) purged from the DB even
    /// though the removal attempt failed - the path is already gone from
    /// disk, so the UI must treat it as removed.
    pub purged: bool,
    /// True only for a Recycle Bin removal that reported success yet did not
    /// land in the bin: Windows permanently deletes (never recycles) an item
    /// too large for the target volume's Recycle Bin quota, and `trash::delete`
    /// returns `Ok(())` for it regardless - verified by `gametrimmer_core`'s
    /// `tests/recycle_bin_quota.rs`. (Windows may well warn the user about such
    /// a delete; the app does not depend on that either way.) The UI counts
    /// these as permanent deletions so it never tells the user a gone-for-good
    /// file is "recoverable in the Recycle Bin". Always `false` for a permanent
    /// delete (nothing to reclassify) and when the bin could not be listed
    /// (we never assert a permanent delete we cannot prove).
    pub nuked: bool,
}

/// Pairs a [`WorkerMsg`] sender with the app's `egui::Context` so every
/// background-thread send can also wake the UI event loop via
/// `Context::request_repaint()`. This is the fix for progress appearing to
/// freeze while the main window is minimized: winit stops calling
/// `eframe::App::ui()` (and so `drain_messages()` never runs to drain the
/// channel) while minimized, but a repaint request forces a frame regardless
/// of window visibility. `Clone` (both fields are `Clone`) so it can be
/// handed to as many worker threads/closures as `Sender<WorkerMsg>` used to
/// be, e.g. once per rayon task in `scan::dispatch_scans`.
#[derive(Clone)]
pub(crate) struct Notifier {
    tx: Sender<WorkerMsg>,
    ctx: egui::Context,
}

impl Notifier {
    pub(crate) fn new(tx: Sender<WorkerMsg>, ctx: egui::Context) -> Self {
        Self { tx, ctx }
    }

    /// Sends `msg` and immediately requests a repaint. A closed receiver (the
    /// UI already dropped, e.g. during shutdown) is not an error the worker
    /// thread can act on - same as the plain `Sender::send` calls this
    /// replaces, the result is discarded.
    pub(crate) fn send(&self, msg: WorkerMsg) {
        let _ = self.tx.send(msg);
        self.ctx.request_repaint();
    }
}

/// Computes the live disk-usage snapshot (see
/// [`crate::model::Occupancy`]) from the `files` table, or an empty snapshot
/// on any query error. The single place the three producers (scan, load,
/// delete) build their `Done`/`RemoveDone` occupancy, so the non-fatal
/// fallback and the log message stay identical across all of them. An
/// aggregation failure must never hide otherwise-good results, so it degrades
/// to "0 bytes / 0%" rather than propagating.
pub(crate) fn occupancy_or_default(conn: &rusqlite::Connection) -> crate::model::Occupancy {
    match gametrimmer_core::db::occupied_by_library(conn) {
        Ok(by_library) => crate::model::Occupancy::from_by_library(by_library),
        Err(err) => {
            crate::logger::log(&format!(
                "Не вдалося порахувати зайнятий обсяг за бібліотеками: {err}"
            ));
            crate::model::Occupancy::default()
        }
    }
}

/// The directory every data file lives in: next to the executable.
fn exe_dir() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| io::Error::other("не вдалося визначити директорію виконуваного файлу"))?;
    Ok(dir.to_path_buf())
}

/// Resolves the database path: `gametrimmer.db` next to the executable.
pub fn db_path() -> io::Result<PathBuf> {
    Ok(exe_dir()?.join(DB_FILE_NAME))
}

/// Resolves the diagnostic log path: `gametrimmer.log` next to the
/// executable - see `crate::logger`.
pub fn log_path() -> io::Result<PathBuf> {
    Ok(exe_dir()?.join(LOG_FILE_NAME))
}

/// Ensures `dir/file_name` exists, seeding it with `builtin` on first use.
/// An existing file is never touched - user edits and imported community
/// packs always win over the embedded defaults.
fn ensure_data_file_in(dir: &Path, file_name: &str, builtin: &str) -> io::Result<PathBuf> {
    let path = dir.join(file_name);
    if !path.is_file() {
        std::fs::write(&path, builtin)?;
    }
    Ok(path)
}

/// Ensures `rules.json` (category rules) exists next to the executable and
/// returns its path, materializing the embedded defaults on first run. The
/// scanner reads rules exclusively from this file - never from an invisible
/// built-in - so users always have the full effective rule set on disk to
/// audit and edit.
pub fn ensure_rules_path() -> io::Result<PathBuf> {
    ensure_data_file_in(
        &exe_dir()?,
        RULES_FILE_NAME,
        gametrimmer_core::rules::BUILTIN_RULES_JSON,
    )
}

/// Ensures `l10n_rules.json` (the localization detector's data pack) exists
/// next to the executable and returns its path, materializing the built-in
/// tables on first run - same transparency contract as [`ensure_rules_path`].
pub fn ensure_l10n_rules_path() -> io::Result<PathBuf> {
    let builtin = gametrimmer_core::langdetect::LangPack::builtin()
        .to_json_pretty()
        .map_err(io::Error::other)?;
    ensure_data_file_in(&exe_dir()?, L10N_RULES_FILE_NAME, &builtin)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `db_path()` (and, by the same `exe_dir()` construction, `ensure_rules_path()`/
    /// `ensure_l10n_rules_path()`) must resolve identically no matter what the
    /// process's current working directory happens to be at launch - this is
    /// P0 item 1 of `docs/portability-audit.md`: double-clicking the exe from
    /// Explorer and running it from `cmd.exe` in an unrelated folder must give
    /// the same data paths.
    ///
    /// This changes the process-global CWD, which would be unsafe if any
    /// other code path read it - the audit's grep sweep (finding #23)
    /// confirmed nothing in the workspace calls `std::env::current_dir()`,
    /// so no other test can observe or be perturbed by this change. The
    /// original CWD is restored before returning either way.
    #[test]
    fn db_path_is_independent_of_current_working_directory() {
        let original_cwd = std::env::current_dir().expect("read original cwd");
        let path_before = db_path().expect("db_path with original cwd");

        // `std::env::temp_dir()` is guaranteed to differ from the test
        // binary's own directory (the only thing `exe_dir()` should track).
        let alt_dir = std::env::temp_dir();
        let cwd_change = std::env::set_current_dir(&alt_dir);

        let result = cwd_change.map(|()| db_path());

        // Always restore, even if the assertion below panics.
        let restore = std::env::set_current_dir(&original_cwd);

        let path_after = result
            .expect("change cwd for test")
            .expect("db_path with alternate cwd");
        restore.expect("restore original cwd");

        assert_eq!(
            path_before, path_after,
            "db_path() must not depend on the process's current working directory"
        );
    }
}
