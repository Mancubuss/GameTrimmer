//! The "Delete Selected" job: removes the given files using the method
//! chosen in settings (permanent delete by default, or the Recycle Bin)
//! and journals every attempt via `gametrimmer_core::ops`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;

use eframe::egui;
use gametrimmer_core::db;
use gametrimmer_core::ops::{
    remove_with_log_observed, OpOutcome, PermanentDelete, RecycleBin, Remover,
};
use gametrimmer_core::settings::DeleteMethod;

use crate::i18n::{self, Lang, Verb};

use super::{Notifier, RemoveOutcome, WorkerMsg};

/// One file queued for removal: its `files.id` (to match the outcome back
/// to a [`crate::model::FindingItem`]), its full path on disk, and its on-disk
/// allocated size (GT-05a) so the post-delete summary can report how much space
/// was actually reclaimed versus expected.
pub struct DeleteItem {
    pub file_id: i64,
    pub full_path: PathBuf,
    pub size_on_disk: u64,
}

/// `ctx` is the app's `egui::Context` (see `Notifier`) so per-file delete
/// progress keeps updating even while the main window is minimized.
pub fn spawn_delete(
    db_path: PathBuf,
    items: Vec<DeleteItem>,
    method: DeleteMethod,
    tx: Sender<WorkerMsg>,
    lang: Lang,
    ctx: egui::Context,
) -> JoinHandle<()> {
    let notifier = Notifier::new(tx, ctx);
    std::thread::spawn(move || run_delete(&db_path, items, method, &notifier, lang))
}

fn run_delete(
    db_path: &Path,
    items: Vec<DeleteItem>,
    method: DeleteMethod,
    notifier: &Notifier,
    lang: Lang,
) {
    let mut conn = match db::open(db_path) {
        Ok(conn) => conn,
        Err(err) => {
            notifier.send(WorkerMsg::Error {
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
            notifier.send(WorkerMsg::Progress {
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
                notifier.send(WorkerMsg::FileRemoved {
                    file_id: items[index].file_id,
                });
            }
        },
    ) {
        Ok(outcomes) => outcomes,
        Err(err) => {
            notifier.send(WorkerMsg::Error {
                msg: i18n::delete_failed(lang, err),
            });
            return;
        }
    };

    // For a Recycle Bin batch, list the bin once and cross-reference: a
    // reported-success path that is not actually in the bin was permanently
    // deleted by Windows because it did not fit the volume's bin quota
    // (verified in `gametrimmer_core`'s `tests/recycle_bin_quota.rs`).
    // `None` means the bin could not be listed - then `nuked_flags` claims no
    // such delete, since we must never assert one we cannot prove. Skipped
    // entirely for a permanent delete: nothing to reclassify.
    let recycled_paths: Option<HashSet<PathBuf>> = match method {
        DeleteMethod::RecycleBin => match gametrimmer_core::ops::recycled_original_paths() {
            Ok(paths) => Some(paths.into_iter().collect()),
            Err(err) => {
                notifier.send(WorkerMsg::Warning {
                    msg: i18n::recycle_bin_list_failed(lang, err),
                });
                None
            }
        },
        DeleteMethod::Permanent => None,
    };
    let nuked = nuked_flags(method, &outcomes, recycled_paths.as_ref());

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
        .zip(nuked)
        .map(|((item, outcome), nuked)| {
            let purged =
                outcome.error.is_none() || std::fs::symlink_metadata(&outcome.path).is_err();
            RemoveOutcome {
                file_id: item.file_id,
                path: outcome.path,
                error: outcome.error,
                purged,
                nuked,
                size_on_disk: item.size_on_disk,
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
        notifier.send(WorkerMsg::Warning {
            msg: i18n::db_update_after_delete_failed(lang, err),
        });
    }

    // Recompute the occupied-space snapshot now that the deleted files'
    // rows have been purged, so the UI's occupied/percent readout reflects
    // the just-freed space rather than the pre-delete total. A failed purge
    // above only means this still shows the stale rows until the next
    // scan/load - the same self-healing the purge warning already notes.
    let occupancy = super::occupancy_or_default(&conn);

    notifier.send(WorkerMsg::RemoveDone {
        outcomes: mapped,
        occupancy,
        method,
    });
}

/// Decides, per outcome, whether a reported-success removal was actually a
/// permanent delete (see [`RemoveOutcome::nuked`]). Pure so the classification
/// is unit-testable without touching the real Recycle Bin:
///
/// - Permanent-delete batches never flag - the user already chose a permanent
///   delete, so there is nothing to reclassify.
/// - `recycled_paths == None` (the bin could not be listed) yields all
///   `false`: we never assert a permanent delete we cannot prove.
/// - Otherwise an outcome is flagged when it succeeded (`error.is_none()`) yet
///   its path is absent from the bin.
fn nuked_flags(
    method: DeleteMethod,
    outcomes: &[OpOutcome],
    recycled_paths: Option<&HashSet<PathBuf>>,
) -> Vec<bool> {
    outcomes
        .iter()
        .map(|outcome| {
            matches!(method, DeleteMethod::RecycleBin)
                && outcome.error.is_none()
                && recycled_paths.is_some_and(|paths| !paths.contains(&outcome.path))
        })
        .collect()
}

/// On-disk space (GT-05a) a delete batch reclaimed, split by *how*: what was
/// expected, what was freed immediately, and what only frees once the Recycle
/// Bin is emptied. Pure so the accounting is unit-testable without a running
/// app (which has no test constructor).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SpaceTally {
    /// Sum of every queued item's on-disk size - the figure the confirm dialog
    /// promised.
    pub expected: u64,
    /// Reclaimed immediately: successful permanent deletes, plus over-quota
    /// recycles Windows turned into permanent deletes (`nuked`).
    pub freed: u64,
    /// Moved to the Recycle Bin on the same volume - frees only when the bin is
    /// emptied. Always 0 for a permanent delete.
    pub recycled_pending: u64,
}

/// Tallies [`SpaceTally`] over a batch's outcomes. A failed removal frees
/// nothing (its bytes count only towards `expected`); a success frees its
/// on-disk size now unless it is a genuine (non-`nuked`) recycle, whose space
/// is merely bin-bound until emptied.
pub(crate) fn space_tally(method: DeleteMethod, outcomes: &[RemoveOutcome]) -> SpaceTally {
    let mut tally = SpaceTally::default();
    for outcome in outcomes {
        tally.expected += outcome.size_on_disk;
        if outcome.error.is_some() {
            continue;
        }
        let recycled_not_nuked = matches!(method, DeleteMethod::RecycleBin) && !outcome.nuked;
        if recycled_not_nuked {
            tally.recycled_pending += outcome.size_on_disk;
        } else {
            tally.freed += outcome.size_on_disk;
        }
    }
    tally
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(path: &str) -> OpOutcome {
        OpOutcome {
            path: PathBuf::from(path),
            error: None,
        }
    }

    fn failed(path: &str) -> OpOutcome {
        OpOutcome {
            path: PathBuf::from(path),
            error: Some("boom".to_string()),
        }
    }

    #[test]
    fn permanent_delete_never_marks_anything_nuked() {
        let outcomes = [ok("C:\\a"), failed("C:\\b")];
        // Even with an empty bin listing, a permanent delete has nothing to
        // reclassify - the user asked for a permanent delete.
        assert_eq!(
            nuked_flags(DeleteMethod::Permanent, &outcomes, Some(&HashSet::new())),
            vec![false, false]
        );
    }

    #[test]
    fn recycle_success_absent_from_bin_is_nuked() {
        let outcomes = [ok("C:\\in_bin"), ok("C:\\too_big")];
        let mut bin = HashSet::new();
        bin.insert(PathBuf::from("C:\\in_bin"));
        assert_eq!(
            nuked_flags(DeleteMethod::RecycleBin, &outcomes, Some(&bin)),
            vec![false, true],
            "a recycle success not present in the bin must be flagged nuked"
        );
    }

    #[test]
    fn recycle_failure_is_not_nuked_even_when_absent_from_bin() {
        // A failed removal is already reported as a per-file error; it is not a
        // permanent delete of the over-quota kind, so it must never be flagged.
        let outcomes = [failed("C:\\gone")];
        assert_eq!(
            nuked_flags(DeleteMethod::RecycleBin, &outcomes, Some(&HashSet::new())),
            vec![false]
        );
    }

    #[test]
    fn unlistable_bin_claims_no_nuke() {
        let outcomes = [ok("C:\\a"), ok("C:\\b")];
        assert_eq!(
            nuked_flags(DeleteMethod::RecycleBin, &outcomes, None),
            vec![false, false],
            "without a bin listing we cannot prove a nuke, so claim none"
        );
    }

    fn removed(size_on_disk: u64, error: Option<&str>, nuked: bool) -> RemoveOutcome {
        RemoveOutcome {
            file_id: 0,
            path: PathBuf::from("C:\\x"),
            error: error.map(|e| e.to_string()),
            purged: error.is_none(),
            nuked,
            size_on_disk,
        }
    }

    #[test]
    fn space_tally_permanent_delete_frees_every_success_and_counts_failures_only_as_expected() {
        let outcomes = [
            removed(4096, None, false),
            removed(8192, None, false),
            removed(1024, Some("permission denied"), false),
        ];
        let tally = space_tally(DeleteMethod::Permanent, &outcomes);
        assert_eq!(
            tally,
            SpaceTally {
                expected: 4096 + 8192 + 1024,
                freed: 4096 + 8192,
                recycled_pending: 0,
            },
            "a permanent delete frees every success; a failure adds only to expected"
        );
    }

    #[test]
    fn space_tally_recycle_defers_recoverable_space_but_counts_nuked_as_freed_now() {
        let outcomes = [
            // Recoverable recycle: space only frees on emptying.
            removed(4096, None, false),
            // Over-quota, permanently deleted by Windows: freed immediately.
            removed(1_000_000, None, true),
            // Failed: neither freed nor pending, but still expected.
            removed(2048, Some("locked"), false),
        ];
        let tally = space_tally(DeleteMethod::RecycleBin, &outcomes);
        assert_eq!(
            tally,
            SpaceTally {
                expected: 4096 + 1_000_000 + 2048,
                freed: 1_000_000,
                recycled_pending: 4096,
            }
        );
    }

    #[test]
    fn space_tally_empty_batch_is_all_zero() {
        assert_eq!(
            space_tally(DeleteMethod::Permanent, &[]),
            SpaceTally::default()
        );
    }
}
