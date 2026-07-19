//! Background worker: everything that touches the database or the
//! filesystem runs on a spawned `std::thread`, communicating back to the
//! UI thread through an `mpsc` channel of [`WorkerMsg`].

pub mod compact;
pub mod delete;
pub mod load;
pub mod manual;
pub mod scan;
pub(crate) mod scan_route;

use std::io;
use std::path::{Path, PathBuf};

use crate::model::FindingRow;

const DB_FILE_NAME: &str = "gametrimmer.db";
const RULES_FILE_NAME: &str = "rules.json";

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

/// Resolves the database path: `gametrimmer.db` next to the executable.
pub fn db_path() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| io::Error::other("не вдалося визначити директорію виконуваного файлу"))?;
    Ok(dir.join(DB_FILE_NAME))
}

/// Resolves `rules.json`: first next to the executable (portable build),
/// then falling back to the repo root (`cargo run` during development).
/// Returns `None` if neither location has the file - callers must report
/// this to the user rather than panicking.
pub fn resolve_rules_path() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(RULES_FILE_NAME);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    let fallback = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(RULES_FILE_NAME);
    fallback.is_file().then_some(fallback)
}
