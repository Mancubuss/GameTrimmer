//! Background worker: everything that touches the database or the
//! filesystem runs on a spawned `std::thread`, communicating back to the
//! UI thread through an `mpsc` channel of [`WorkerMsg`].

pub mod compact;
pub mod delete;
pub mod load;
pub mod manual;
pub mod rules_io;
pub mod scan;
pub(crate) mod scan_route;

use std::io;
use std::path::{Path, PathBuf};

use crate::model::FindingRow;

const DB_FILE_NAME: &str = "gametrimmer.db";
pub(crate) const RULES_FILE_NAME: &str = "rules.json";
/// Localization-detector data pack (community rules).
pub const L10N_RULES_FILE_NAME: &str = "l10n_rules.json";

/// Messages sent from a worker thread back to the UI thread.
#[derive(Debug)]
pub enum WorkerMsg {
    /// Libraries discovered and persisted; scanning of individual games is
    /// about to start.
    LibrariesFound { libraries: usize, games: usize },
    /// Granular progress for a long-running operation (scanning games,
    /// deleting files, compacting the database, ...). `verb` is the
    /// Ukrainian operation name rendered before the `current/total` counter
    /// (e.g. "Сканування", "Видалення", "Стискання бази даних"); `detail`
    /// names the item currently being worked on (a game name for scanning, a
    /// file name for deletion). Compaction has no per-item detail - it
    /// leaves `detail` empty and reports an estimated `current`/100 percent
    /// instead (see `gametrimmer_core::db::compact_observed`); `ui::top_bar`
    /// renders that case as `"{verb} {percent}%"`.
    Progress {
        verb: &'static str,
        current: usize,
        total: usize,
        detail: String,
    },
    /// The scan finished successfully with the given findings.
    Done {
        findings: Vec<FindingRow>,
        /// Human-readable Ukrainian summary of how the scan was carried out
        /// (MFT index vs. walkdir counts, elapsed time) - see
        /// `worker::scan_route::format_scan_summary`.
        scan_summary: String,
    },
    /// A delete operation finished (possibly with some per-file failures).
    RemoveDone { outcomes: Vec<RemoveOutcome> },
    /// One file finished being removed successfully mid-batch, so the UI can
    /// drop it from the tree immediately.
    FileRemoved { file_id: i64 },
    /// The scan was cancelled by the user before completion.
    Cancelled,
    /// Something went wrong; `msg` is a user-facing Ukrainian description.
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
