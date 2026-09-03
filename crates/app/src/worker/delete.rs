//! The "Delete Selected" job: removes the given files using the method
//! chosen in settings (permanent delete by default, or the Recycle Bin)
//! and journals every attempt via `gametrimmer_core::ops`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;

use gametrimmer_core::db;
use gametrimmer_core::ops::{
    execute_delete_plans_observed, prepare_delete_plans_with_skips, DeleteAttendance, FsOutcome,
    OpOutcome,
};
use gametrimmer_core::settings::DeleteMethod;

use crate::i18n::{self, Lang, Verb};

use super::{Notifier, RemoveOutcome, Wake, WorkerMsg};

/// One database row queued for removal. The path is deliberately not accepted
/// from the UI: the core delete preflight reconstructs it from the active
/// generation's immutable safety snapshot.
pub struct DeleteItem {
    pub file_id: i64,
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
    wake: Wake,
) -> JoinHandle<()> {
    let notifier = Notifier::new(tx, wake);
    std::thread::spawn(move || run_delete(&db_path, items, method, &notifier, lang))
}

fn run_delete(
    db_path: &Path,
    mut items: Vec<DeleteItem>,
    method: DeleteMethod,
    notifier: &Notifier,
    lang: Lang,
) {
    let mut conn = match db::open(db_path) {
        Ok(conn) => conn,
        Err(err) => {
            notifier.report_error(i18n::Reported::new(lang, |l| {
                i18n::db_open_error_short(l, &err)
            }));
            return;
        }
    };

    let file_ids: Vec<i64> = items.iter().map(|item| item.file_id).collect();
    let intro_file_ids: HashSet<i64> = {
        let mut set = HashSet::new();
        match conn.prepare("SELECT file_id FROM findings WHERE category = 'intro'") {
            Ok(mut stmt) => match stmt.query_map([], |row| row.get::<_, i64>(0)) {
                Ok(rows) => {
                    let batch_set: HashSet<i64> = file_ids.iter().copied().collect();
                    for id in rows.flatten() {
                        if batch_set.contains(&id) {
                            set.insert(id);
                        }
                    }
                }
                Err(err) => {
                    notifier.report_error(i18n::Reported::new(lang, |l| {
                        i18n::delete_failed(l, format!("the intro findings lookup failed: {err}"))
                    }));
                    return;
                }
            },
            Err(err) => {
                notifier.report_error(i18n::Reported::new(lang, |l| {
                    i18n::delete_failed(
                        l,
                        format!("the intro findings lookup could not be prepared: {err}"),
                    )
                }));
                return;
            }
        }
        set
    };

    let save_file_ids: HashSet<i64> = {
        let mut set = HashSet::new();
        match conn.prepare("SELECT file_id FROM findings WHERE category = 'save_bloat'") {
            Ok(mut stmt) => match stmt.query_map([], |row| row.get::<_, i64>(0)) {
                Ok(rows) => {
                    let batch_set: HashSet<i64> = file_ids.iter().copied().collect();
                    for id in rows.flatten() {
                        if batch_set.contains(&id) {
                            set.insert(id);
                        }
                    }
                }
                Err(err) => {
                    notifier.report_error(i18n::Reported::new(lang, |l| {
                        i18n::delete_failed(l, format!("the save findings lookup failed: {err}"))
                    }));
                    return;
                }
            },
            Err(err) => {
                notifier.report_error(i18n::Reported::new(lang, |l| {
                    i18n::delete_failed(
                        l,
                        format!("the save findings lookup could not be prepared: {err}"),
                    )
                }));
                return;
            }
        }
        set
    };

    // A multi-asset container the preflight held back is dropped from the
    // batch rather than failing it - one such file used to mean nothing at all
    // got deleted. It is still reported: `error` keeps it out of the freed
    // figure (see `space_tally`) and puts it, by name and reason, in the
    // summary the window shows when the batch finishes.
    // The user ticked the box and pressed delete: that click is the
    // anti-cheat consent - see `DeleteAttendance`.
    let (plans, blocked) = match prepare_delete_plans_with_skips(
        &conn,
        &file_ids,
        method,
        DeleteAttendance::Interactive,
    ) {
        Ok(prepared) => prepared,
        Err(err) => {
            notifier.report_error(i18n::Reported::new(lang, |l| i18n::delete_failed(l, &err)));
            return;
        }
    };
    let blocked_outcomes: Vec<RemoveOutcome> = blocked
        .iter()
        .filter_map(|skip| {
            let item = items.iter().find(|item| item.file_id == skip.file_id)?;
            Some(RemoveOutcome {
                file_id: skip.file_id,
                path: skip.path.clone(),
                error: Some(skip.reason.clone()),
                purged: false,
                nuked: false,
                size_on_disk: item.size_on_disk,
                share: None,
            })
        })
        .collect();
    // Keeps `items[i]` paired with `plans[i]` for the whole job below.
    items.retain(|item| !blocked.iter().any(|skip| skip.file_id == item.file_id));
    if plans.len() != items.len() {
        notifier.report_error(i18n::Reported::new(lang, |l| {
            i18n::delete_failed(l, "delete preflight returned an incomplete batch")
        }));
        return;
    }

    // Zero-Data-Loss Shield: If any save_bloat files are queued, backup them before deleting
    if !save_file_ids.is_empty() {
        let save_paths: Vec<PathBuf> = plans
            .iter()
            .enumerate()
            .filter(|(idx, _)| save_file_ids.contains(&items[*idx].file_id))
            .map(|(_, plan)| plan.target_path())
            .collect();

        if !save_paths.is_empty() {
            let backup_base = std::env::var("LOCALAPPDATA")
                .map(|p| {
                    PathBuf::from(p)
                        .join("GameTrimmer")
                        .join("backups")
                        .join("saves")
                })
                .unwrap_or_else(|_| PathBuf::from("backups").join("saves"));

            if let Err(err) = gametrimmer_core::janitor::saves::create_save_backup_zip(
                &save_paths,
                &backup_base,
                "Saves_AutoBackup",
            ) {
                notifier.report_error(i18n::Reported::new(lang, move |l| {
                    i18n::save_backup_zip_failed(l, &err)
                }));
                return;
            }
        }
    }

    // Identify each intro file's real container while it still exists on disk.
    // `remover.remove` (inside `execute_delete_plans_observed`) leaves nothing
    // left to open, so this has to happen before that call, not from within
    // its `on_outcome` callback - see `gametrimmer_core::stub::detect_stub_bytes`.
    // A container this build has no stub for is excluded from the batch
    // entirely rather than deleted and left stub-less: a video replaced by
    // nothing is the exact boot crash this feature exists to prevent.
    //
    // `execute_plans`/`execute_origin`/`execute_stub` stay parallel: index i in
    // all three describes the same queued deletion, and `execute_origin[i]` is
    // that deletion's position in `items`/`plans`, so the callbacks below
    // (which only see a position within this filtered slice) can report back
    // against the right `DeleteItem`.
    let mut execute_plans = Vec::with_capacity(plans.len());
    let mut execute_origin = Vec::with_capacity(plans.len());
    let mut execute_stub: Vec<Option<Vec<u8>>> = Vec::with_capacity(plans.len());
    let mut skipped_outcomes: Vec<(usize, OpOutcome)> = Vec::new();

    for (origin, plan) in plans.iter().enumerate() {
        if !intro_file_ids.contains(&items[origin].file_id) {
            execute_plans.push(plan.clone());
            execute_origin.push(origin);
            execute_stub.push(None);
            continue;
        }
        let nominal_path = plan.target_path();
        match gametrimmer_core::stub::detect_stub_bytes(&nominal_path) {
            Some(bytes) => {
                execute_plans.push(plan.clone());
                execute_origin.push(origin);
                execute_stub.push(Some(bytes));
            }
            None => {
                notifier.report_warning(i18n::Reported::new(lang, |l| {
                    i18n::intro_stub_unsupported_skip(l, nominal_path.display())
                }));
                skipped_outcomes.push((
                    origin,
                    OpOutcome {
                        path: nominal_path,
                        error: Some(
                            "unrecognized video container; kept to avoid an unbootable game"
                                .to_string(),
                        ),
                        status: FsOutcome::Blocked,
                        journal_error: None,
                        share: None,
                    },
                ));
            }
        }
    }

    // A micro-stub that fails to land leaves the game with no file at that
    // path - the exact outcome the stub exists to prevent - so it is collected
    // here and folded into the batch's outcomes below. `WorkerMsg::Warning`
    // alone would not do: the window drops it, and only the log would know.
    let mut stub_failures: Vec<(usize, String)> = Vec::new();
    let filtered_outcomes = match execute_delete_plans_observed(
        &mut conn,
        method,
        &execute_plans,
        |current, total, path| {
            // Slow removers (the Recycle Bin goes through the shell per file) can
            // take a noticeable moment per item, so the file currently being
            // worked on is reported before the attempt, not after - see
            // `execute_delete_plans_observed`'s contract.
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
            if outcome.status == FsOutcome::Removed {
                if let Some(bytes) = &execute_stub[index] {
                    if let Some(err) =
                        report_stub_write_failure_if_any(notifier, lang, &outcome.path, bytes)
                    {
                        stub_failures.push((index, err));
                    }
                }
            }
            // Stream success back to the UI immediately so the findings tree
            // can drop the file mid-batch instead of waiting for the whole
            // delete to finish - failures are only reported once at the end
            // (via `RemoveDone`) since they still need the summary dialog.
            if matches!(
                outcome.status,
                FsOutcome::Removed | FsOutcome::AlreadyAbsent
            ) {
                notifier.send(WorkerMsg::FileRemoved {
                    file_id: items[execute_origin[index]].file_id,
                });
            }
            if let Some(journal_error) = &outcome.journal_error {
                notifier.report_warning(i18n::Reported::new(lang, |l| {
                    i18n::db_update_after_delete_failed(l, journal_error)
                }));
            }
        },
    ) {
        Ok(outcomes) => outcomes,
        Err(err) => {
            notifier.report_error(i18n::Reported::new(lang, |l| i18n::delete_failed(l, &err)));
            return;
        }
    };
    let mut filtered_outcomes = filtered_outcomes;
    for (index, err) in stub_failures {
        if let Some(outcome) = filtered_outcomes.get_mut(index) {
            outcome.error = Some(err);
        }
    }

    // Reassemble `plans`' original order out of the two disjoint,
    // origin-sorted sources above: every plan index is either in
    // `execute_origin` (attempted, possibly failed) or in `skipped_outcomes`
    // (never attempted at all), and both lists stay in increasing origin order
    // because the loop that built them visited origins 0..plans.len() in
    // order. `Peekable::next_if` walks that merge without ever indexing past
    // what a `peek` already confirmed.
    let mut outcomes = Vec::with_capacity(plans.len());
    let mut executed = execute_origin.into_iter().zip(filtered_outcomes).peekable();
    let mut skipped = skipped_outcomes.into_iter().peekable();
    for origin in 0..plans.len() {
        if let Some((_, outcome)) = executed.next_if(|(o, _)| *o == origin) {
            outcomes.push(outcome);
        } else if let Some((_, outcome)) = skipped.next_if(|(o, _)| *o == origin) {
            outcomes.push(outcome);
        }
    }
    // Every index below pairs `items[i]` with `outcomes[i]`. A short merge
    // would not fail loudly, it would shift every later pairing and attribute
    // one file's result to the next file's row - including what gets purged.
    debug_assert_eq!(outcomes.len(), plans.len());
    if outcomes.len() != plans.len() {
        notifier.report_error(i18n::Reported::new(lang, |l| {
            i18n::delete_failed(l, "delete outcomes could not be matched to their files")
        }));
        return;
    }

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
                notifier.report_warning(i18n::Reported::new(lang, |l| {
                    i18n::recycle_bin_list_failed(l, &err)
                }));
                None
            }
        },
        DeleteMethod::Permanent => None,
    };
    let nuked = nuked_flags(method, &outcomes, recycled_paths.as_ref());

    // Files actually removed from disk must also disappear from `files`/
    // `findings`, or the next app start (worker::load) would resurrect them
    // as selectable despite being gone - re-deleting would then just fail.
    // Only an observed removal or an explicit NotFound result may purge the
    // database row. Permission, sharing and other I/O failures remain visible.
    let mut mapped: Vec<RemoveOutcome> = items
        .iter()
        .zip(outcomes)
        .zip(nuked)
        .map(|((item, outcome), nuked)| {
            let purged = matches!(
                outcome.status,
                FsOutcome::Removed | FsOutcome::AlreadyAbsent
            );
            RemoveOutcome {
                file_id: item.file_id,
                path: outcome.path,
                error: outcome.error,
                purged,
                nuked,
                size_on_disk: item.size_on_disk,
                share: outcome.share,
            }
        })
        .collect();
    mapped.extend(blocked_outcomes);

    record_space_tally(&conn, method, &mapped);

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
        notifier.report_warning(i18n::Reported::new(lang, |l| {
            i18n::db_update_after_delete_failed(l, &err)
        }));
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

/// Writes the intro micro-stub `bytes` at `path` - already vacated by the
/// delete this runs after - and returns the failure text when it does not
/// land, so the caller can put it on the file's own outcome. A warning alone
/// would be dropped by the window, and the batch would report a clean success
/// over a game left with no file at that path at all.
fn report_stub_write_failure_if_any(
    notifier: &Notifier,
    lang: Lang,
    path: &Path,
    bytes: &[u8],
) -> Option<String> {
    let err = gametrimmer_core::stub::write_stub(path, bytes).err()?;
    let detail = format!(
        "the intro micro-stub could not be written after the delete, so the game may not start: {err}"
    );
    notifier.report_warning(i18n::Reported::new(lang, |l| {
        i18n::intro_stub_write_failed(l, path.display(), &err)
    }));
    Some(detail)
}

/// Writes the batch's space accounting where it outlives the dialog that
/// shows it.
///
/// "It said 40 GB and my disk didn't change" has exactly three possible
/// answers - the Recycle Bin still holds it, the files were hard-linked, or
/// a removal failed - and [`SpaceTally`]'s three numbers separate them.
/// They were computed on the UI thread *after* this worker returned, so
/// they existed only for as long as the dialog was open; computing them
/// here instead costs one pure function call and keeps them.
///
/// One row rather than a column per file: the three totals and the two
/// counts are what the question needs, and a per-file breakdown would add a
/// row for every deleted file to answer a question about the batch.
/// Deliberately not fatal, and deliberately silent on failure beyond the
/// log - the files are already gone, and a bookkeeping note must not look
/// like a failed delete.
fn record_space_tally(
    conn: &rusqlite::Connection,
    method: DeleteMethod,
    outcomes: &[RemoveOutcome],
) {
    let scan_id = match db::active_scan_id(conn) {
        // No active generation means nothing to attach the note to. That
        // combination should not occur (these rows were deleted out of a
        // scan) but it is not worth an error path.
        Ok(None) | Err(_) => return,
        Ok(Some(scan_id)) => scan_id,
    };
    let tally = space_tally(method, outcomes);
    let nuked = outcomes.iter().filter(|outcome| outcome.nuked).count();
    let shared = outcomes
        .iter()
        .filter(|outcome| outcome.share.is_some_and(|share| share.link_count > 1))
        .count();
    let message = format!(
        "method={method:?} files={} expected={} freed={} recycled_pending={} \
         nuked={nuked} hardlinked={shared}",
        outcomes.len(),
        tally.expected,
        tally.freed,
        tally.recycled_pending,
    );
    if let Err(err) =
        db::record_scan_diagnostic(conn, scan_id, "delete", "space-tally", None, &message)
    {
        crate::logger::error(&format!("Failed to record the delete space tally: {err}"));
    }
}

/// Decides, per outcome, whether a reported-success removal was actually a
/// permanent delete (see [`RemoveOutcome::nuked`]). Pure so the classification
/// is unit-testable without touching the real Recycle Bin:
///
/// - Permanent-delete batches never flag - the user already chose a permanent
///   delete, so there is nothing to reclassify.
/// - `recycled_paths == None` (the bin could not be listed) yields all
///   `false`: we never assert a permanent delete we cannot prove.
/// - Otherwise an outcome is flagged when it was removed yet
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
                && outcome.status == FsOutcome::Removed
                && recycled_paths.is_some_and(|paths| !paths.contains(&outcome.path))
        })
        .collect()
}

/// On-disk space (allocated-size accounting) a delete batch reclaimed, split by *how*: what was
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
    let mut freed_items = Vec::new();
    let mut pending_items = Vec::new();

    for outcome in outcomes {
        tally.expected = tally.expected.saturating_add(outcome.size_on_disk);
        if outcome.error.is_some() {
            continue;
        }
        let recycled_not_nuked = matches!(method, DeleteMethod::RecycleBin) && !outcome.nuked;
        if recycled_not_nuked {
            pending_items.push((outcome.share, outcome.size_on_disk));
        } else {
            freed_items.push((outcome.share, outcome.size_on_disk));
        }
    }

    // Both figures go through the hard-link arithmetic rather than a plain
    // sum: a file named by several links keeps its allocation until the last
    // name is removed, so removing one of them frees nothing, and removing all
    // of them frees the file's size once - not once per link. `expected` stays
    // the plain sum on purpose: it is the figure the confirm dialog promised
    // from stored row sizes, and the gap to `freed` is exactly the honest
    // signal that some of those bytes were shared.
    tally.freed = gametrimmer_core::hardlink::reclaimable_bytes(&freed_items);
    tally.recycled_pending = gametrimmer_core::hardlink::reclaimable_bytes(&pending_items);
    tally
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(path: &str) -> OpOutcome {
        OpOutcome {
            path: PathBuf::from(path),
            error: None,
            status: FsOutcome::Removed,
            journal_error: None,
            share: None,
        }
    }

    fn failed(path: &str) -> OpOutcome {
        OpOutcome {
            path: PathBuf::from(path),
            error: Some("boom".to_string()),
            status: FsOutcome::Failed,
            journal_error: None,
            share: None,
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
            share: None,
        }
    }

    /// A removed file that is one of `links` hard links to the same allocation.
    fn removed_link(size_on_disk: u64, file_index: u64, links: u32) -> RemoveOutcome {
        RemoveOutcome {
            file_id: 0,
            path: PathBuf::from("C:\\x"),
            error: None,
            purged: true,
            nuked: false,
            size_on_disk,
            share: Some(gametrimmer_core::hardlink::FileShare {
                volume_serial: 1,
                file_index,
                link_count: links,
            }),
        }
    }

    #[test]
    fn space_tally_does_not_claim_space_for_a_surviving_hard_link() {
        // One of two names removed: the allocation stays behind the other
        // name, so nothing was freed - but the batch still expected its bytes.
        let outcomes = [removed_link(8 * 1024 * 1024, 42, 2)];
        let tally = space_tally(DeleteMethod::Permanent, &outcomes);
        assert_eq!(
            tally,
            SpaceTally {
                expected: 8 * 1024 * 1024,
                freed: 0,
                recycled_pending: 0,
            },
            "deleting one of two links frees nothing"
        );
    }

    #[test]
    fn space_tally_counts_a_fully_removed_link_set_once() {
        let outcomes = [
            removed_link(8 * 1024 * 1024, 42, 2),
            removed_link(8 * 1024 * 1024, 42, 2),
        ];
        let tally = space_tally(DeleteMethod::Permanent, &outcomes);
        assert_eq!(
            tally.freed,
            8 * 1024 * 1024,
            "both links gone frees the file's size once, not twice"
        );
        assert_eq!(
            tally.expected,
            16 * 1024 * 1024,
            "expected still reflects what the dialog promised from row sizes"
        );
    }

    #[test]
    fn space_tally_recycle_of_a_shared_file_pends_nothing() {
        // Recycling renames the file into the bin; the other link still names
        // the same allocation, so not even bin-bound space was gained.
        let outcomes = [removed_link(4096, 7, 3)];
        let tally = space_tally(DeleteMethod::RecycleBin, &outcomes);
        assert_eq!(tally.recycled_pending, 0);
        assert_eq!(tally.freed, 0);
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

    #[test]
    fn space_tally_saturates_corrupt_persisted_sizes() {
        let outcomes = [
            removed(u64::MAX, Some("blocked"), false),
            removed(1, Some("blocked"), false),
        ];

        assert_eq!(
            space_tally(DeleteMethod::Permanent, &outcomes).expected,
            u64::MAX
        );
    }

    fn insert_test_finding(
        conn: &rusqlite::Connection,
        scan_id: i64,
        root: &Path,
        rel_path: &str,
        category: &str,
    ) -> i64 {
        conn.execute(
            "INSERT OR IGNORE INTO game_libraries (vendor, path) VALUES ('steam', ?1)",
            [root.to_string_lossy()],
        )
        .unwrap();
        let lib_id: i64 = conn
            .query_row(
                "SELECT id FROM game_libraries WHERE path = ?1",
                [root.to_string_lossy()],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO games (scan_id, library_id, app_id, name, install_dir) \
             VALUES (?1, ?2, 'app', 'Test Game', ?3)",
            rusqlite::params![scan_id, lib_id, root.to_string_lossy()],
        )
        .unwrap();
        let game_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO files (scan_id, game_id, rel_path, size) VALUES (?1, ?2, ?3, 1000)",
            rusqlite::params![scan_id, game_id, rel_path],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO findings (file_id, category, confidence) VALUES (?1, ?2, 90)",
            rusqlite::params![file_id, category],
        )
        .unwrap();
        let snapshot = gametrimmer_core::safety::capture_safety_snapshot(root, rel_path).unwrap();
        conn.execute(
            "INSERT INTO file_safety \
             (file_id, scan_id, trusted_root, rel_path, root_identity, \
              target_identity, target_kind, tree_fingerprint) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                file_id,
                scan_id,
                snapshot.trusted_root.to_string_lossy(),
                snapshot.rel_path.to_string_lossy(),
                snapshot.root_identity.encode(),
                snapshot.target_identity.encode(),
                snapshot.target_identity.kind.as_str(),
                snapshot.tree_fingerprint,
            ],
        )
        .unwrap();
        gametrimmer_core::db::record_scan_library_evidence(conn, scan_id, root, "test", "complete")
            .unwrap();
        file_id
    }

    #[test]
    fn deleting_intro_finding_creates_micro_stub_while_docs_finding_does_not() {
        let temp = tempfile::tempdir().unwrap();
        let intro_path = temp.path().join("intro.mp4");
        let docs_path = temp.path().join("manual.pdf");

        std::fs::write(
            &intro_path,
            b"ORIGINAL MP4 VIDEO WITH LOTS OF BYTES 1234567890",
        )
        .unwrap();
        std::fs::write(&docs_path, b"ORIGINAL PDF MANUAL DOCUMENTATION 1234567890").unwrap();

        let db_path = temp.path().join("test.db");
        let mut conn = gametrimmer_core::db::open(&db_path).unwrap();
        let scan_id = gametrimmer_core::db::begin_scan(&conn, "complete").unwrap();
        let intro_file_id = insert_test_finding(&conn, scan_id, temp.path(), "intro.mp4", "intro");
        let docs_file_id =
            insert_test_finding(&conn, scan_id, temp.path(), "manual.pdf", "docs_file");
        gametrimmer_core::db::activate_scan(&mut conn, scan_id).unwrap();
        drop(conn);

        let (tx, rx) = std::sync::mpsc::channel();
        let notifier = Notifier::silent(tx);

        let items = vec![
            DeleteItem {
                file_id: intro_file_id,
                size_on_disk: 1000,
            },
            DeleteItem {
                file_id: docs_file_id,
                size_on_disk: 1000,
            },
        ];

        run_delete(
            &db_path,
            items,
            DeleteMethod::Permanent,
            &notifier,
            Lang::En,
        );

        let mut done = false;
        for msg in rx {
            if let WorkerMsg::RemoveDone { outcomes, .. } = msg {
                assert_eq!(outcomes.len(), 2);
                assert!(outcomes[0].purged);
                assert!(outcomes[1].purged);
                done = true;
                break;
            }
        }
        assert!(done, "run_delete must emit RemoveDone");

        // The intro file should exist and contain the MP4 micro-stub
        assert!(
            intro_path.exists(),
            "intro file should be replaced with micro-stub"
        );
        let intro_content = std::fs::read(&intro_path).unwrap();
        assert_eq!(
            &intro_content[4..8],
            b"ftyp",
            "intro stub should have MP4 ftyp bytes"
        );
        assert_ne!(
            intro_content,
            b"ORIGINAL MP4 VIDEO WITH LOTS OF BYTES 1234567890"
        );

        // The docs file should be completely removed (not stubbed)
        assert!(
            !docs_path.exists(),
            "docs file should be deleted without stub"
        );
    }

    #[test]
    fn a_blocked_container_is_reported_while_the_rest_of_the_batch_is_deleted() {
        let temp = tempfile::tempdir().unwrap();
        let intro_path = temp.path().join("intro.bik");
        let container_path = temp.path().join("re_chunk_000.pak");
        let docs_path = temp.path().join("manual.pdf");
        // A real Bink 1 video, the format that used to fail the whole batch.
        let mut intro_bytes = gametrimmer_core::stub::BIK1_STUB.to_vec();
        intro_bytes.extend_from_slice(&[0xAB; 4096]);
        std::fs::write(&intro_path, &intro_bytes).unwrap();
        let mut kpka = b"KPKA".to_vec();
        kpka.extend_from_slice(&[0u8; 60]);
        // Held back by its name. The preflight no longer reads a selected
        // file's bytes to decide - see
        // `the_preflight_holds_files_back_by_name_and_never_by_reading_them`.
        std::fs::write(&container_path, &kpka).unwrap();
        std::fs::write(&docs_path, b"ORIGINAL PDF MANUAL DOCUMENTATION").unwrap();

        let db_path = temp.path().join("test.db");
        let mut conn = gametrimmer_core::db::open(&db_path).unwrap();
        let scan_id = gametrimmer_core::db::begin_scan(&conn, "complete").unwrap();
        let intro_id = insert_test_finding(&conn, scan_id, temp.path(), "intro.bik", "intro");
        let container_id =
            insert_test_finding(&conn, scan_id, temp.path(), "re_chunk_000.pak", "docs_file");
        let docs_id = insert_test_finding(&conn, scan_id, temp.path(), "manual.pdf", "docs_file");
        gametrimmer_core::db::activate_scan(&mut conn, scan_id).unwrap();
        drop(conn);

        let (tx, rx) = std::sync::mpsc::channel();
        let notifier = Notifier::silent(tx);
        let items = vec![
            DeleteItem {
                file_id: intro_id,
                size_on_disk: 1000,
            },
            DeleteItem {
                file_id: container_id,
                size_on_disk: 4000,
            },
            DeleteItem {
                file_id: docs_id,
                size_on_disk: 2000,
            },
        ];

        run_delete(
            &db_path,
            items,
            DeleteMethod::Permanent,
            &notifier,
            Lang::En,
        );

        let mut done = false;
        for msg in rx {
            if let WorkerMsg::RemoveDone {
                outcomes, method, ..
            } = msg
            {
                let find = |file_id: i64| {
                    outcomes
                        .iter()
                        .find(|outcome| outcome.file_id == file_id)
                        .unwrap_or_else(|| panic!("file_id {file_id} must be reported"))
                };
                assert_eq!(
                    outcomes.len(),
                    3,
                    "every selected file must be accounted for"
                );
                assert!(find(intro_id).purged, "a Bink intro must be deletable");
                assert!(find(docs_id).purged);
                let blocked = find(container_id);
                assert!(!blocked.purged);
                assert!(
                    blocked
                        .error
                        .as_deref()
                        .is_some_and(|err| err.contains("multi-asset container")),
                    "the user has to be told which file was skipped and why"
                );
                assert_eq!(
                    space_tally(method, &outcomes).freed,
                    1000 + 2000,
                    "the skipped file's bytes must not be reported as freed"
                );
                done = true;
                break;
            }
        }
        assert!(done, "run_delete must emit RemoveDone");

        assert!(container_path.is_file(), "the container must survive");
        assert_eq!(std::fs::read(&container_path).unwrap(), kpka);
        assert!(!docs_path.exists());
        assert_eq!(
            std::fs::read(&intro_path).unwrap(),
            gametrimmer_core::stub::BIK1_STUB,
            "the intro must be replaced by the micro-stub"
        );
    }

    #[test]
    fn intro_with_unrecognized_container_is_not_deleted() {
        let temp = tempfile::tempdir().unwrap();
        // Neither the bytes nor the extension match a container this build
        // has a stub for.
        let mystery_path = temp.path().join("intro.smk");
        std::fs::write(&mystery_path, b"NOT A KNOWN CONTAINER AT ALL").unwrap();

        let db_path = temp.path().join("test.db");
        let mut conn = gametrimmer_core::db::open(&db_path).unwrap();
        let scan_id = gametrimmer_core::db::begin_scan(&conn, "complete").unwrap();
        let file_id = insert_test_finding(&conn, scan_id, temp.path(), "intro.smk", "intro");
        gametrimmer_core::db::activate_scan(&mut conn, scan_id).unwrap();
        drop(conn);

        let (tx, rx) = std::sync::mpsc::channel();
        let notifier = Notifier::silent(tx);
        let items = vec![DeleteItem {
            file_id,
            size_on_disk: 1000,
        }];

        run_delete(
            &db_path,
            items,
            DeleteMethod::Permanent,
            &notifier,
            Lang::En,
        );

        let mut done = false;
        for msg in rx {
            if let WorkerMsg::RemoveDone { outcomes, .. } = msg {
                assert_eq!(outcomes.len(), 1);
                assert!(
                    !outcomes[0].purged,
                    "an unidentifiable intro container must not be deleted"
                );
                done = true;
                break;
            }
        }
        assert!(done, "run_delete must emit RemoveDone");

        assert!(
            mystery_path.exists(),
            "the original file must survive when its container cannot be identified"
        );
        assert_eq!(
            std::fs::read(&mystery_path).unwrap(),
            b"NOT A KNOWN CONTAINER AT ALL"
        );
    }

    #[test]
    fn intro_stub_uses_the_real_container_even_when_the_extension_lies() {
        let temp = tempfile::tempdir().unwrap();
        // Named like an MP4, but the bytes on disk are really a WebM header -
        // the engine's own loader reads bytes, not the file name, so the stub
        // written after deletion must match what was actually there.
        let intro_path = temp.path().join("intro.mp4");
        std::fs::write(&intro_path, gametrimmer_core::stub::WEBM_STUB).unwrap();

        let db_path = temp.path().join("test.db");
        let mut conn = gametrimmer_core::db::open(&db_path).unwrap();
        let scan_id = gametrimmer_core::db::begin_scan(&conn, "complete").unwrap();
        let file_id = insert_test_finding(&conn, scan_id, temp.path(), "intro.mp4", "intro");
        gametrimmer_core::db::activate_scan(&mut conn, scan_id).unwrap();
        drop(conn);

        let (tx, rx) = std::sync::mpsc::channel();
        let notifier = Notifier::silent(tx);
        let items = vec![DeleteItem {
            file_id,
            size_on_disk: 1000,
        }];

        run_delete(
            &db_path,
            items,
            DeleteMethod::Permanent,
            &notifier,
            Lang::En,
        );

        let mut done = false;
        for msg in rx {
            if let WorkerMsg::RemoveDone { outcomes, .. } = msg {
                assert!(outcomes[0].purged);
                done = true;
                break;
            }
        }
        assert!(done, "run_delete must emit RemoveDone");

        let contents = std::fs::read(&intro_path).unwrap();
        assert_eq!(
            contents,
            gametrimmer_core::stub::WEBM_STUB,
            "must stub with the sniffed container, not the misleading .mp4 extension"
        );
    }

    #[test]
    fn stub_write_failure_is_reported_as_a_warning_not_only_logged() {
        let dir = tempfile::tempdir().unwrap();
        // A plain file where the stub's directory would need to exist blocks
        // the write, forcing the failure path deterministically.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let bogus_path = blocker.join("intro.mp4");

        let (tx, rx) = std::sync::mpsc::channel();
        let notifier = Notifier::silent(tx);

        let detail = report_stub_write_failure_if_any(
            &notifier,
            Lang::En,
            &bogus_path,
            gametrimmer_core::stub::MP4_STUB,
        );

        let msg = rx
            .try_recv()
            .expect("a failed stub write must be reported, not silently dropped");
        assert!(
            matches!(msg, WorkerMsg::Warning { .. }),
            "must surface as a WorkerMsg the UI can show, not just a log line"
        );
        assert!(
            detail.is_some(),
            "the failure text must come back for the file's own outcome - the              window drops a bare Warning, so that is the only channel the              summary dialog ever sees"
        );
    }

    #[test]
    fn a_skipped_file_does_not_shift_the_next_file_s_outcome() {
        let temp = tempfile::tempdir().unwrap();
        // Index 0 is skipped (no stub for its container), index 1 is deleted.
        // The two travel through separate lists and are merged back by
        // origin, so this is the batch shape that would expose a merge that
        // pairs `items[i]` with the wrong outcome.
        let skipped_path = temp.path().join("intro.smk");
        std::fs::write(&skipped_path, b"NOT A KNOWN CONTAINER AT ALL").unwrap();
        let deleted_path = temp.path().join("manual.pdf");
        std::fs::write(&deleted_path, b"a document with no stub involved").unwrap();

        let db_path = temp.path().join("test.db");
        let mut conn = gametrimmer_core::db::open(&db_path).unwrap();
        let scan_id = gametrimmer_core::db::begin_scan(&conn, "complete").unwrap();
        let skipped_id = insert_test_finding(&conn, scan_id, temp.path(), "intro.smk", "intro");
        let deleted_id =
            insert_test_finding(&conn, scan_id, temp.path(), "manual.pdf", "docs_file");
        gametrimmer_core::db::activate_scan(&mut conn, scan_id).unwrap();
        drop(conn);

        let (tx, rx) = std::sync::mpsc::channel();
        let notifier = Notifier::silent(tx);
        let items = vec![
            DeleteItem {
                file_id: skipped_id,
                size_on_disk: 1000,
            },
            DeleteItem {
                file_id: deleted_id,
                size_on_disk: 2000,
            },
        ];

        run_delete(
            &db_path,
            items,
            DeleteMethod::Permanent,
            &notifier,
            Lang::En,
        );

        let mut done = false;
        for msg in rx {
            if let WorkerMsg::RemoveDone { outcomes, .. } = msg {
                assert_eq!(outcomes.len(), 2, "every file needs its own outcome");
                let skipped = outcomes
                    .iter()
                    .find(|outcome| outcome.file_id == skipped_id)
                    .expect("the skipped file must still be in the batch");
                let deleted = outcomes
                    .iter()
                    .find(|outcome| outcome.file_id == deleted_id)
                    .expect("the deleted file must still be in the batch");
                assert!(
                    !skipped.purged && skipped.error.is_some(),
                    "the skipped intro must carry its own refusal, not the other file's result"
                );
                assert!(
                    deleted.purged && deleted.error.is_none(),
                    "the deleted document must carry its own success"
                );
                assert!(skipped.path.ends_with("intro.smk"));
                assert!(deleted.path.ends_with("manual.pdf"));
                done = true;
                break;
            }
        }
        assert!(done, "run_delete must emit RemoveDone");

        assert!(skipped_path.exists(), "the skipped file must survive");
        assert!(!deleted_path.exists(), "the deletable file must be gone");
    }
}
