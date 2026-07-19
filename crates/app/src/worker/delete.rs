//! The "Видалити вибране" job: removes the given files using the method
//! chosen in settings (permanent delete by default, or the Recycle Bin)
//! and journals every attempt via `gametrimmer_core::ops`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;

use gametrimmer_core::db;
use gametrimmer_core::ops::{remove_with_log_observed, PermanentDelete, RecycleBin, Remover};
use gametrimmer_core::settings::DeleteMethod;

use crate::i18n::{self, Lang, Verb};

use super::{RemoveOutcome, WorkerMsg};

/// One file queued for removal: its `files.id` (to match the outcome back
/// to a [`crate::model::FindingItem`]) and its full path on disk.
pub struct DeleteItem {
    pub file_id: i64,
    pub full_path: PathBuf,
}

pub fn spawn_delete(
    db_path: PathBuf,
    items: Vec<DeleteItem>,
    method: DeleteMethod,
    tx: Sender<WorkerMsg>,
    lang: Lang,
) -> JoinHandle<()> {
    std::thread::spawn(move || run_delete(&db_path, items, method, &tx, lang))
}

fn run_delete(
    db_path: &Path,
    items: Vec<DeleteItem>,
    method: DeleteMethod,
    tx: &Sender<WorkerMsg>,
    lang: Lang,
) {
    let mut conn = match db::open(db_path) {
        Ok(conn) => conn,
        Err(err) => {
            let _ = tx.send(WorkerMsg::Error {
                msg: i18n::db_open_error_short(lang, err),
            });
            return;
        }
    };

    let remover: &dyn Remover = match method {
        DeleteMethod::Permanent => &PermanentDelete,
        DeleteMethod::RecycleBin => &RecycleBin,
    };

    let paths: Vec<PathBuf> = items.iter().map(|item| item.full_path.clone()).collect();

    let outcomes = match remove_with_log_observed(
        &mut conn,
        remover,
        &paths,
        |current, total, path| {
            // Slow removers (the Recycle Bin goes through the shell per file) can
            // take a noticeable moment per item, so the file currently being
            // worked on is reported before the attempt, not after - see
            // `remove_with_log_observed`'s doc comment.
            let detail = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let _ = tx.send(WorkerMsg::Progress {
                verb: Verb::Delete,
                current,
                total,
                detail,
            });
        },
        |index, outcome| {
            // Stream success back to the UI immediately so the findings tree
            // can drop the file mid-batch instead of waiting for the whole
            // delete to finish - failures are only reported once at the end
            // (via `RemoveDone`) since they still need the summary dialog.
            if outcome.error.is_none() {
                let _ = tx.send(WorkerMsg::FileRemoved {
                    file_id: items[index].file_id,
                });
            }
        },
    ) {
        Ok(outcomes) => outcomes,
        Err(err) => {
            let _ = tx.send(WorkerMsg::Error {
                msg: i18n::delete_failed(lang, err),
            });
            return;
        }
    };

    // Files actually removed from disk must also disappear from `files`/
    // `findings`, or the next app start (worker::load) would resurrect them
    // as selectable despite being gone - re-deleting would then just fail.
    // A failed attempt whose path is no longer on disk (stale row from an
    // older session, someone deleted the file manually, ...) is purged too:
    // the end state the user asked for - "file gone" - already holds, and
    // keeping the row would resurrect the same error on every next attempt.
    // `symlink_metadata` (not `exists`) so a dangling link still counts as
    // present - the link itself is a removable entry. Computed once per
    // outcome here and carried on `RemoveOutcome::purged` so both this
    // filter and the UI's `RemoveDone` handling reuse the same flag instead
    // of re-checking the filesystem.
    let mapped: Vec<RemoveOutcome> = items
        .iter()
        .zip(outcomes)
        .map(|(item, outcome)| {
            let purged =
                outcome.error.is_none() || std::fs::symlink_metadata(&outcome.path).is_err();
            RemoveOutcome {
                file_id: item.file_id,
                path: outcome.path,
                error: outcome.error,
                purged,
            }
        })
        .collect();

    let removed_ids: Vec<i64> = mapped
        .iter()
        .filter(|outcome| outcome.purged)
        .map(|outcome| outcome.file_id)
        .collect();

    if let Err(err) = gametrimmer_core::ops::purge_removed_files(&mut conn, &removed_ids) {
        // Non-fatal: the files are already gone from disk, and this
        // session's in-memory state is still correct - only a later load of
        // saved results would show stale rows until the next successful
        // purge or rescan.
        let _ = tx.send(WorkerMsg::Warning {
            msg: i18n::db_update_after_delete_failed(lang, err),
        });
    }

    let _ = tx.send(WorkerMsg::RemoveDone { outcomes: mapped });
}
