//! Safe file removal with an operations journal.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::Connection;

use crate::error::Result;
use crate::safety::{
    current_identity, normalize_relative_path, validate_delete_plan, DeleteBlockReason, DeletePlan,
    FileIdentity, SafetySnapshot, TargetKind,
};
use crate::settings::DeleteMethod;

/// Abstraction over the actual removal mechanism so tests never touch the
/// real Recycle Bin or filesystem.
pub(crate) trait Remover {
    fn remove(&self, path: &Path) -> Result<()>;
    /// Stable action name journaled into the `operations` table.
    fn action(&self) -> &'static str;

    /// Removes a target whose identity has just been proven, while it is still
    /// held open - see [`crate::safety::VerifiedTarget`].
    ///
    /// The default releases the handle and falls back to removing by name,
    /// which is all a remover that only speaks paths can do. Overriding it is
    /// how a remover opts into having no window between the proof and the act.
    fn remove_target(&self, target: crate::safety::VerifiedTarget) -> Result<()> {
        // The handle must be gone before the path-based removal runs: it was
        // opened without `FILE_SHARE_DELETE`, so leaving it open would make
        // the very deletion it exists to protect fail with a sharing
        // violation.
        self.remove(&target.into_path())
    }
}

/// Recoverable remover: sends paths to the Windows Recycle Bin via the
/// `trash` crate. Slower than [`PermanentDelete`] (each file goes through
/// the shell), but recoverable.
pub(crate) struct RecycleBin;

impl Remover for RecycleBin {
    fn remove(&self, path: &Path) -> Result<()> {
        trash::delete(path)?;
        Ok(())
    }

    fn action(&self) -> &'static str {
        "recycle"
    }

    // Deliberately not overridden. `trash::delete` goes through the shell's
    // `IFileOperation`, which takes a path and gives no way to hand it a
    // handle, so recycling cannot be made handle-based at all. It keeps the
    // original resolve-twice window knowingly - and it is much the better of
    // the two modes to keep it in: if it ever did race onto a different
    // object, that object is in the Recycle Bin, not destroyed.
}

/// Fast remover: deletes files/directories permanently via `std::fs`, with
/// no way to recover. The default for game libraries - anything removed by
/// mistake can always be re-downloaded from the store.
pub(crate) struct PermanentDelete;

impl Remover for PermanentDelete {
    fn remove(&self, path: &Path) -> Result<()> {
        // `symlink_metadata` (not `metadata`) so a symlink/junction inside a
        // game folder is removed as the link itself, never by following it
        // into whatever it points at.
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.is_dir() {
            std::fs::remove_dir_all(path)?;
        } else {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    fn action(&self) -> &'static str {
        "delete"
    }

    /// The irreversible mode, so it is the one that must not race: this acts
    /// on the proven handle itself and never resolves the name a second time.
    fn remove_target(&self, target: crate::safety::VerifiedTarget) -> Result<()> {
        target.delete()?;
        Ok(())
    }
}

/// Outcome of removing one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsOutcome {
    Removed,
    AlreadyAbsent,
    Blocked,
    Failed,
}

impl FsOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Removed => "removed",
            Self::AlreadyAbsent => "already_absent",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug)]
pub struct OpOutcome {
    pub path: PathBuf,
    /// None = success; Some(message) = failure reason.
    pub error: Option<String>,
    pub status: FsOutcome,
    /// A failed final journal update never hides the observed filesystem
    /// outcome; startup reconciliation will repair the pending intent.
    pub journal_error: Option<String>,
    /// Identity and hard-link count read from the live file *immediately
    /// before* it was removed, so callers can tell whether removing this path
    /// actually freed its allocation or merely dropped one of several names
    /// for it. `None` when nothing was removed, or when the query failed -
    /// see [`crate::hardlink::reclaimable_bytes`], which treats `None` as
    /// "assume unshared".
    pub share: Option<crate::hardlink::FileShare>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciledOperation {
    pub operation_id: i64,
    pub outcome: String,
    pub error: Option<String>,
}

/// Classifies durable intents left pending by a crash. This function never
/// performs a filesystem mutation and never retries an operation.
pub fn reconcile_pending_operations(conn: &mut Connection) -> Result<Vec<ReconciledOperation>> {
    let pending = {
        let mut stmt = conn.prepare(
            "SELECT id, trusted_root, rel_path, expected_identity,
                    expected_tree_fingerprint
             FROM operations WHERE status = 'pending' ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let mut reconciled = Vec::with_capacity(pending.len());
    for (operation_id, root, rel_path, expected_identity, expected_fingerprint) in pending {
        let classification = (|| {
            let root = root.ok_or_else(|| "missing trusted root".to_string())?;
            let rel_path = rel_path.ok_or_else(|| "missing relative path".to_string())?;
            let expected_identity = expected_identity
                .ok_or_else(|| "missing expected identity".to_string())
                .and_then(|encoded| FileIdentity::decode(&encoded).map_err(|e| e.to_string()))?;
            let relative = normalize_relative_path(&rel_path).map_err(|e| e.to_string())?;
            let root = PathBuf::from(root);
            // An unreachable root - a drive that is unplugged, unmounted or has
            // lost its letter since the crash - is not evidence that the delete
            // happened. Settling the intent here would record a removal that
            // may never have occurred, so leave it pending for a later start.
            if matches!(current_identity(&root), Err(DeleteBlockReason::Missing)) {
                return Ok(None);
            }
            let target = root.join(relative);
            match current_identity(&target) {
                Err(DeleteBlockReason::Missing) => Ok(Some(("reconciled_removed", None))),
                Ok(current) if current.same_object(&expected_identity) => {
                    // The object is still there and is still the same object -
                    // but for a directory that proves nothing about its
                    // contents. `remove_dir_all` interrupted halfway leaves the
                    // folder in place, with the same file index, holding an
                    // unknown fraction of what it had. Calling that
                    // `not_applied` would be the one verdict that is certainly
                    // wrong: it invites the same delete to be proposed again
                    // over a tree that is already half gone, and reports the
                    // space as still occupied.
                    if current.kind == TargetKind::Directory {
                        match (
                            &expected_fingerprint,
                            crate::safety::tree_fingerprint(&target),
                        ) {
                            (Some(expected), Ok(actual)) if *expected == actual => {
                                Ok(Some(("not_applied", None)))
                            }
                            (Some(_), Ok(_)) => Ok(Some((
                                "partially_applied",
                                Some(
                                    "the directory still exists but its contents changed \
                                     since the scan - the delete may have been interrupted \
                                     partway"
                                        .to_string(),
                                ),
                            ))),
                            // No fingerprint was captured (a row written before
                            // schema v5), or the tree cannot be walked now.
                            // Either way the question is unanswerable, and an
                            // unanswerable question is not evidence that
                            // nothing happened.
                            (None, _) => Ok(Some((
                                "unknown",
                                Some(
                                    "no directory fingerprint was recorded for this intent"
                                        .to_string(),
                                ),
                            ))),
                            (Some(_), Err(error)) => Ok(Some(("unknown", Some(error.to_string())))),
                        }
                    } else {
                        Ok(Some(("not_applied", None)))
                    }
                }
                Ok(_) => Ok(Some((
                    "conflict",
                    Some("a different filesystem object now occupies the target".to_string()),
                ))),
                Err(error) => Ok(Some(("unknown", Some(error.to_string())))),
            }
        })();
        let (outcome, error) = match classification {
            Ok(Some(value)) => value,
            Ok(None) => continue,
            Err(error) => ("unknown", Some(error)),
        };
        let completed_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        conn.execute(
            "UPDATE operations
             SET status = 'final', completed_at = ?1, outcome = ?2, error = ?3
             WHERE id = ?4 AND status = 'pending'",
            rusqlite::params![completed_at, outcome, error.as_deref(), operation_id],
        )?;
        reconciled.push(ReconciledOperation {
            operation_id,
            outcome: outcome.to_string(),
            error,
        });
    }
    Ok(reconciled)
}

/// One file the preflight left out of the batch without failing it: a
/// monolithic archive candidate, which has to be trimmed in place rather than
/// removed whole. Every other refusal is a safety error and still aborts the
/// whole call - see [`prepare_delete_plans_with_skips`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteSkip {
    pub file_id: i64,
    pub path: PathBuf,
    pub reason: String,
}

/// Whether a human is present to consent to this batch, or the preflight is
/// running unattended - see the automatic re-trim engine in
/// [`crate::retrim`]. Not a bare `bool`: `prepare_delete_plans(.., true)` at
/// a call site tells a reader nothing, and the two situations get a
/// genuinely different anti-cheat verdict (below), not just a different
/// label for the same behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteAttendance {
    /// A person selected this file and pressed delete. That click is the
    /// anti-cheat consent; the preflight does not second-guess it.
    Interactive,
    /// Nobody is watching. An anti-cheat-protected game is always skipped,
    /// never silently re-deleted after an unattended update.
    Unattended,
}

/// Builds immutable delete plans only from the active, non-legacy generation.
/// Monolithic archive candidates are dropped from the batch; every other
/// refusal fails the whole call. Callers that need to tell the user which
/// files were dropped use [`prepare_delete_plans_with_skips`] instead.
pub fn prepare_delete_plans(
    conn: &Connection,
    file_ids: &[i64],
    method: DeleteMethod,
    attendance: DeleteAttendance,
) -> Result<Vec<DeletePlan>> {
    Ok(prepare_delete_plans_with_skips(conn, file_ids, method, attendance)?.0)
}

/// [`prepare_delete_plans`] plus the containers it left out, so an
/// interactive caller can report them instead of silently deleting fewer
/// files than the user selected.
pub fn prepare_delete_plans_with_skips(
    conn: &Connection,
    file_ids: &[i64],
    method: DeleteMethod,
    attendance: DeleteAttendance,
) -> Result<(Vec<DeletePlan>, Vec<DeleteSkip>)> {
    let action = match method {
        DeleteMethod::Permanent => "delete",
        DeleteMethod::RecycleBin => "recycle",
    };
    prepare_delete_plans_for_action(conn, file_ids, action, false, attendance)
}

fn prepare_delete_plans_for_action(
    conn: &Connection,
    file_ids: &[i64],
    action: &str,
    allow_missing: bool,
    attendance: DeleteAttendance,
) -> Result<(Vec<DeletePlan>, Vec<DeleteSkip>)> {
    if !matches!(action, "delete" | "recycle") {
        return Err(crate::error::CoreError::Other(format!(
            "unsupported destructive action: {action}"
        )));
    }
    let mut unique_ids = HashSet::with_capacity(file_ids.len());
    if file_ids.iter().any(|file_id| !unique_ids.insert(*file_id)) {
        return Err(crate::error::CoreError::Other(
            "delete preflight blocked a duplicate file id".into(),
        ));
    }
    let mut plans = Vec::with_capacity(file_ids.len());
    let mut skips = Vec::new();
    // The anti-cheat verdict costs a full directory walk - see
    // `unattended_skip_reason`. Memoized per game for the duration of this
    // call, so a batch touching many files in the same game pays for it once,
    // and only ever populated on the unattended path.
    let mut anti_cheat_cache: HashMap<i64, Option<String>> = HashMap::new();
    for file_id in file_ids {
        let row = conn.query_row(
            "SELECT f.scan_id, f.game_id, fs.trusted_root, fs.rel_path,
                    fs.root_identity, fs.target_identity, fs.target_kind,
                    fs.tree_fingerprint, fs.block_reason, sle.status, g.install_dir,
                    g.anti_cheat_protected
             FROM files f
             LEFT JOIN games g ON g.id = f.game_id
             LEFT JOIN game_libraries gl ON gl.id = g.library_id
             LEFT JOIN file_safety fs ON fs.file_id = f.id
             LEFT JOIN scan_library_evidence sle
                    ON sle.scan_id = f.scan_id
                   AND sle.library_path = COALESCE(gl.path, fs.evidence_library_path)
             WHERE f.id = ?1",
            [file_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<bool>>(11)?,
                ))
            },
        );
        let (
            scan_id,
            game_id,
            trusted_root,
            rel_path,
            root_identity,
            target_identity,
            target_kind,
            tree_fingerprint,
            block_reason,
            evidence_status,
            install_dir,
            stored_anti_cheat,
        ) = row.map_err(|_| {
            crate::error::CoreError::Other(format!(
                "delete preflight blocked file_id {file_id}: {}",
                DeleteBlockReason::MissingSafetyEvidence
            ))
        })?;

        if !crate::db::scan_allows_deletion(conn, scan_id)? {
            return Err(crate::error::CoreError::Other(format!(
                "delete preflight blocked file_id {file_id}: {}",
                DeleteBlockReason::LegacySnapshot
            )));
        }
        let Some(evidence_status) = evidence_status.as_deref() else {
            return Err(crate::error::CoreError::Other(format!(
                "delete preflight blocked file_id {file_id}: {}",
                DeleteBlockReason::MissingSafetyEvidence
            )));
        };
        // One shared policy with the scan's persistence path and the load
        // query - see `safety::discovery_block_reason`. The reason it returns
        // is more specific than `DegradedDiscovery`, which this used to report
        // for every kind of insufficient evidence including a wholly missing
        // row, so it is passed through instead of being flattened.
        if let Some(reason) =
            crate::safety::discovery_block_reason(game_id.is_some(), Some(evidence_status))
        {
            return Err(crate::error::CoreError::Other(format!(
                "delete preflight blocked file_id {file_id}: {reason}"
            )));
        }
        if let Some(reason) = block_reason {
            return Err(crate::error::CoreError::Other(format!(
                "delete preflight blocked file_id {file_id}: {reason}"
            )));
        }
        let Some(trusted_root) = trusted_root else {
            return Err(crate::error::CoreError::Other(format!(
                "delete preflight blocked file_id {file_id}: {}",
                DeleteBlockReason::MissingSafetyEvidence
            )));
        };
        let Some(rel_path) = rel_path else {
            return Err(crate::error::CoreError::Other(format!(
                "delete preflight blocked file_id {file_id}: {}",
                DeleteBlockReason::MissingSafetyEvidence
            )));
        };
        if crate::worker::is_candidate_archive_path(&rel_path) {
            skips.push(DeleteSkip {
                file_id: *file_id,
                path: Path::new(&trusted_root).join(&rel_path),
                reason: "a monolithic archive candidate is trimmed in place, not deleted whole"
                    .to_string(),
            });
            continue;
        }
        // Variant B (owner's decision): ticking the checkbox by hand IS the
        // anti-cheat consent, so the interactive path never consults this.
        // Nobody ticks anything for an unattended re-trim, so there the
        // verdict is authoritative and refusal is unconditional - the file
        // is left alone and reported, not silently re-deleted after a game
        // update. Monolithic archives keep their own hard block above,
        // unrelated to this one.
        //
        // This is the ONE place the anti-cheat decision is made. The executor
        // deliberately does not repeat it: by the time plans exist every
        // protected file has already left the batch, and re-asking there only
        // gave a directory that went briefly unreadable the power to abort a
        // whole unattended run - see `execute_delete_plans_with_remover_observed`.
        if attendance == DeleteAttendance::Unattended {
            // Fails closed on absent data, like every other missing-evidence
            // case in this preflight. A file with no game row (an orphan, a
            // janitor artifact) has no install directory to clear, and "no
            // directory to check" is not the same claim as "checked, clear".
            //
            // Two detectors, and the union of them, because they do not agree.
            // The stored verdict comes from `is_safe_from_relative_paths` over
            // the scan's whole inventory and matches substrings, so it sees a
            // `Vanguard\` directory; the live walk below matches exact file
            // names and does not. Neither is a superset of the other, and both
            // fail closed, so asking only one leaves that one's blind spot as a
            // hole in the gate. The stored answer also cannot see a game that
            // acquired anti-cheat since the scan - which is precisely the case
            // an after-an-update re-trim runs in - so the live walk cannot be
            // dropped for it either.
            let reason = match (game_id, install_dir.as_deref()) {
                (Some(game_id), Some(install_dir)) => {
                    if stored_anti_cheat.unwrap_or(true) {
                        Some(
                            "no one is present to consent, and the last scan found this game \
                             anti-cheat protected"
                                .to_string(),
                        )
                    } else {
                        anti_cheat_cache
                            .entry(game_id)
                            .or_insert_with(|| unattended_skip_reason(Path::new(install_dir)))
                            .clone()
                    }
                }
                _ => Some(
                    "no one is present to consent, and this file has no game install \
                     directory to clear of anti-cheat first"
                        .to_string(),
                ),
            };
            if let Some(reason) = reason {
                skips.push(DeleteSkip {
                    file_id: *file_id,
                    path: Path::new(&trusted_root).join(&rel_path),
                    reason,
                });
                continue;
            }
        }
        let root_identity = FileIdentity::decode(root_identity.as_deref().ok_or_else(|| {
            crate::error::CoreError::Other(format!(
                "delete preflight blocked file_id {file_id}: missing root identity"
            ))
        })?)?;
        let target_identity =
            FileIdentity::decode(target_identity.as_deref().ok_or_else(|| {
                crate::error::CoreError::Other(format!(
                    "delete preflight blocked file_id {file_id}: missing target identity"
                ))
            })?)?;
        let persisted_kind = target_kind
            .as_deref()
            .and_then(TargetKind::parse)
            .ok_or_else(|| {
                crate::error::CoreError::Other(format!(
                    "delete preflight blocked file_id {file_id}: invalid target kind"
                ))
            })?;
        if persisted_kind != target_identity.kind {
            return Err(crate::error::CoreError::Other(format!(
                "delete preflight blocked file_id {file_id}: target kind mismatch"
            )));
        }
        let plan = DeletePlan {
            file_id: *file_id,
            scan_id,
            action: action.to_string(),
            snapshot: SafetySnapshot {
                trusted_root: PathBuf::from(trusted_root),
                rel_path: PathBuf::from(rel_path),
                root_identity,
                target_identity,
                tree_fingerprint,
            },
        };
        if plan.snapshot.target_identity.kind == TargetKind::File {
            // Identity, and nothing else. This used to also read the file's
            // first bytes and hold back anything shaped like a multi-asset
            // container, on the principle that the preflight was the final
            // authority for every selected file. It was never the first
            // authority: `should_probe_archive_contents` asks the same
            // question while classifying - for every imported rule and every
            // archive-looking extension - and a container caught there is
            // blocked as read-only and never becomes selectable at all. What
            // arrives here has already passed that gate or was deliberately
            // kept off it.
            //
            // The one case classification cannot see, bytes that changed
            // between the scan and the delete, is not a question for a format
            // detector either. `FileIdentity` carries size and last-write time
            // and `validate_delete_plan` compares the whole struct, so a
            // rewritten file is `TargetChanged` long before any magic is read.
            //
            // Deliberately given up: a *built-in* rule pointing at a
            // non-archive extension whose bytes are really a container - the
            // single combination classification skips for speed. That is a bug
            // in the built-in rule and belongs in the rule, not in a guard at
            // the exit. A file the user chose is a file the user chose; the
            // program does not reopen it to argue the point.
            match crate::safety::validate_delete_plan(&plan) {
                Ok(_) => {}
                Err(DeleteBlockReason::Missing) if allow_missing => {}
                Err(error) => {
                    return Err(crate::error::CoreError::Other(format!(
                        "delete preflight blocked file_id {file_id}: {error}"
                    )));
                }
            }
        }
        plans.push(plan);
    }
    Ok((plans, skips))
}

/// Why an unattended batch must leave the game installed at `install_dir`
/// alone, or `None` when a complete walk found nothing to protect.
///
/// Both refusals are the same fail-closed answer and they are worded apart on
/// purpose. `AntiCheatShield::is_safe` collapses "a complete scan found
/// EasyAntiCheat" into the same `false` as "the directory could not be walked
/// at all", which is safe but reports a game as anti-cheat protected when what
/// actually happened is that a launcher update moved its folder - in the one
/// line an operator reads to find out what the re-trim did.
fn unattended_skip_reason(install_dir: &Path) -> Option<String> {
    match crate::anti_cheat::AntiCheatShield::check_directory(install_dir, false) {
        Ok(report) if report.is_safe => None,
        Ok(report) => {
            let engine = report
                .findings
                .first()
                .map(|finding| finding.engine.to_string())
                .unwrap_or_else(|| "an unnamed engine".to_string());
            Some(format!(
                "no one is present to consent - an unattended re-trim never deletes in an \
                 anti-cheat-protected game ({engine})"
            ))
        }
        Err(error) => Some(format!(
            "no one is present to consent, and the anti-cheat check over {} could not \
             complete ({error}) - an unattended re-trim never deletes on an unproven verdict",
            install_dir.display()
        )),
    }
}

/// Executes a preflighted batch. The whole batch is validated before the first
/// mutation, then each row and live identity is checked again immediately
/// before its own operation.
///
/// Takes no [`DeleteAttendance`]: the anti-cheat decision belongs to
/// [`prepare_delete_plans_for_action`] and is made exactly once, there. See
/// [`execute_delete_plans_with_remover_observed`] for why repeating it here
/// was worse than useless.
pub fn execute_delete_plans_observed(
    conn: &mut Connection,
    method: DeleteMethod,
    plans: &[DeletePlan],
    mut on_progress: impl FnMut(usize, usize, &Path),
    mut on_outcome: impl FnMut(usize, &OpOutcome),
) -> Result<Vec<OpOutcome>> {
    let remover: &dyn Remover = match method {
        DeleteMethod::Permanent => &PermanentDelete,
        DeleteMethod::RecycleBin => &RecycleBin,
    };
    execute_delete_plans_with_remover_observed(
        conn,
        remover,
        plans,
        &mut on_progress,
        &mut on_outcome,
    )
}

/// The rechecks below re-derive each plan and compare it against the one the
/// caller preflighted, which is what catches a database row that changed
/// underneath a batch. They deliberately run as
/// [`DeleteAttendance::Interactive`] regardless of who asked for the batch.
///
/// An unattended preflight has already dropped every file in an anti-cheat
/// protected game, so the ids that reach here belong to unprotected games
/// only, and re-deriving them as interactive yields exactly the same plans -
/// the comparison stays honest. Threading the attendance through instead
/// bought one thing, catching a game that becomes protected in the
/// milliseconds between prepare and execute, and charged two for it: a
/// directory that is momentarily unreadable (a launcher update, which is
/// precisely when a re-trim runs) failed the walk closed, emptied the batch
/// and aborted the whole run under `StaleDatabaseRow` - a name that does not
/// even describe what happened - and every unprotected game paid for one
/// complete extra traversal per file on top of two per batch.
fn execute_delete_plans_with_remover_observed(
    conn: &mut Connection,
    remover: &dyn Remover,
    plans: &[DeletePlan],
    mut on_progress: impl FnMut(usize, usize, &Path),
    mut on_outcome: impl FnMut(usize, &OpOutcome),
) -> Result<Vec<OpOutcome>> {
    conn.pragma_update(None, "synchronous", "FULL")?;

    let ids: Vec<i64> = plans.iter().map(|plan| plan.file_id).collect();
    let (current, _) = prepare_delete_plans_for_action(
        conn,
        &ids,
        remover.action(),
        true,
        DeleteAttendance::Interactive,
    )?;
    if current != plans {
        return Err(crate::error::CoreError::Other(
            DeleteBlockReason::StaleDatabaseRow.to_string(),
        ));
    }
    for plan in plans {
        if let Err(reason) = validate_delete_plan(plan) {
            if reason != DeleteBlockReason::Missing {
                return Err(crate::error::CoreError::Other(format!(
                    "delete preflight blocked file_id {}: {reason}",
                    plan.file_id
                )));
            }
        }
    }

    let mut outcomes = Vec::with_capacity(plans.len());
    for (index, plan) in plans.iter().enumerate() {
        let nominal_path = plan.snapshot.trusted_root.join(&plan.snapshot.rel_path);
        on_progress(index + 1, plans.len(), &nominal_path);

        let refreshed = prepare_delete_plans_for_action(
            conn,
            &[plan.file_id],
            remover.action(),
            true,
            DeleteAttendance::Interactive,
        );
        if refreshed.as_ref().ok().and_then(|(rows, _)| rows.first()) != Some(plan) {
            let outcome = OpOutcome {
                path: nominal_path,
                error: Some(DeleteBlockReason::StaleDatabaseRow.to_string()),
                status: FsOutcome::Blocked,
                journal_error: None,
                share: None,
            };
            on_outcome(index, &outcome);
            outcomes.push(outcome);
            continue;
        }

        let ts = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        conn.execute(
            "INSERT INTO operations
             (ts, action, src_path, dst_path, status, scan_id, file_id,
              trusted_root, rel_path, expected_identity, expected_tree_fingerprint)
             VALUES (?1, ?2, ?3, NULL, 'pending', ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                ts,
                remover.action(),
                nominal_path.to_string_lossy(),
                plan.scan_id,
                plan.file_id,
                plan.snapshot.trusted_root.to_string_lossy(),
                plan.snapshot.rel_path.to_string_lossy(),
                plan.snapshot.target_identity.encode(),
                // `None` for a file. For a directory this is the only thing
                // that can tell a half-finished `remove_dir_all` from one that
                // never started - the folder's identity survives both.
                plan.snapshot.tree_fingerprint.as_deref(),
            ],
        )?;
        let operation_id = conn.last_insert_rowid();

        // Read the file's hard-link count while it still exists: afterwards
        // there is no way to tell whether this path was the last name for its
        // allocation or one of several. Costs one open per file, on a bounded,
        // user-initiated batch - never on a scan.
        let mut share = None;
        // `open_verified_for_delete`, not `validate_delete_plan`: the latter
        // proves identity and then closes the handle, leaving the remover to
        // resolve the name again. This keeps the proven handle open and hands
        // it to the remover, so there is no second resolution to race.
        let (status, error) = match crate::safety::open_verified_for_delete(plan) {
            Ok(target) => {
                let path = target.path().to_path_buf();
                // Read while it still exists: afterwards there is no way to
                // tell whether this path was the last name for its allocation
                // or one of several. Safe to do with the verification handle
                // still open - this asks only for FILE_READ_ATTRIBUTES, which
                // Windows exempts from share-mode checks.
                share = crate::hardlink::file_share(&path);
                match remover.remove_target(target) {
                    Ok(()) => (FsOutcome::Removed, None),
                    Err(remove_error) => match std::fs::symlink_metadata(&path) {
                        Err(metadata_error)
                            if metadata_error.kind() == std::io::ErrorKind::NotFound =>
                        {
                            (FsOutcome::AlreadyAbsent, None)
                        }
                        _ => (FsOutcome::Failed, Some(remove_error.to_string())),
                    },
                }
            }
            Err(DeleteBlockReason::Missing) => (FsOutcome::AlreadyAbsent, None),
            Err(reason) => (FsOutcome::Blocked, Some(reason.to_string())),
        };

        let completed_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        let journal_error = conn
            .execute(
                "UPDATE operations
                 SET status = 'final', completed_at = ?1, outcome = ?2, error = ?3
                 WHERE id = ?4",
                rusqlite::params![
                    completed_at,
                    status.as_str(),
                    error.as_deref(),
                    operation_id
                ],
            )
            .err()
            .map(|failure| failure.to_string());

        let outcome = OpOutcome {
            path: nominal_path,
            error,
            status,
            journal_error,
            share,
        };
        on_outcome(index, &outcome);
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

/// Lists the original on-disk paths of everything currently in the Windows
/// Recycle Bin.
///
/// Used to tell, after a [`RecycleBin`] removal, which items really landed in
/// the bin versus were permanently deleted by Windows because they exceeded the
/// target volume's bin quota - `trash::delete` returns `Ok(())` either way (see
/// the app's `worker::delete::nuked_flags` and this crate's
/// `tests/recycle_bin_quota.rs`). Kept here so the `trash` dependency stays
/// contained in `core` rather than leaking into the app crate.
pub fn recycled_original_paths() -> Result<Vec<PathBuf>> {
    let items = trash::os_limited::list()?;
    Ok(items.iter().map(|item| item.original_path()).collect())
}

/// Removes `paths` one by one via `remover`, journaling every attempt into the
/// `operations` table (row written as `pending` before the attempt, updated to
/// `done`/`failed` after). Continues past individual failures.
///
/// Thin wrapper over [`remove_with_log_observed`] for callers that don't need
/// per-file progress.
#[cfg(test)]
fn remove_with_log(
    conn: &mut Connection,
    remover: &dyn Remover,
    paths: &[PathBuf],
) -> Result<Vec<OpOutcome>> {
    remove_with_log_observed(conn, remover, paths, |_, _, _| {}, |_, _| {})
}

/// Same as [`remove_with_log`], but calls `on_progress(index, total, path)`
/// (1-based `index`) right before each path is attempted - not after - so a
/// caller reporting progress to a UI shows the file currently being worked
/// on, which matters for slow removers (e.g. the Windows Recycle Bin, where
/// each file goes through the shell and can take a noticeable moment).
///
/// Also calls `on_outcome(index, &outcome)` (0-based `index` into `paths`)
/// right after each path's outcome is journaled, so a caller can stream
/// per-file results back to a UI immediately instead of waiting for the
/// whole batch to finish.
#[cfg(test)]
fn remove_with_log_observed(
    conn: &mut Connection,
    remover: &dyn Remover,
    paths: &[PathBuf],
    mut on_progress: impl FnMut(usize, usize, &Path),
    mut on_outcome: impl FnMut(usize, &OpOutcome),
) -> Result<Vec<OpOutcome>> {
    let mut outcomes = Vec::new();

    for (index, path) in paths.iter().enumerate() {
        on_progress(index + 1, paths.len(), path);

        // Get current Unix timestamp
        let ts = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Convert path to string for database
        let src_path = path.to_string_lossy().to_string();

        // INSERT as "pending"
        conn.execute(
            "INSERT INTO operations (ts, action, src_path, dst_path, status) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![ts, remover.action(), &src_path, None::<String>, "pending"],
        )?;

        let last_id = conn.last_insert_rowid();

        // Attempt removal, capturing the link count first (see the observed
        // path above - after removal the sharing is unknowable).
        let share = crate::hardlink::file_share(path);
        let remove_result = remover.remove(path);

        // UPDATE status based on outcome
        let status = match &remove_result {
            Ok(()) => "done",
            Err(_) => "failed",
        };

        conn.execute(
            "UPDATE operations SET status = ?1 WHERE id = ?2",
            rusqlite::params![status, last_id],
        )?;

        // Add outcome (continue on individual failures)
        let filesystem_status = if remove_result.is_ok() {
            FsOutcome::Removed
        } else {
            FsOutcome::Failed
        };
        let outcome = OpOutcome {
            path: path.clone(),
            error: remove_result.err().map(|e| e.to_string()),
            status: filesystem_status,
            journal_error: None,
            share,
        };
        on_outcome(index, &outcome);
        outcomes.push(outcome);
    }

    Ok(outcomes)
}

/// Deletes the `findings` and `files` rows for files that were actually
/// removed from disk, so a later load of saved results doesn't resurrect
/// them. One transaction: either all rows go or none.
pub fn purge_removed_files(conn: &mut Connection, file_ids: &[i64]) -> Result<()> {
    if file_ids.is_empty() {
        return Ok(());
    }

    let tx = conn.transaction()?;
    {
        // Both child tables are cleared before their `files` parent.
        let mut delete_safety = tx.prepare("DELETE FROM file_safety WHERE file_id = ?1")?;
        let mut delete_findings = tx.prepare("DELETE FROM findings WHERE file_id = ?1")?;
        let mut delete_files = tx.prepare("DELETE FROM files WHERE id = ?1")?;
        for file_id in file_ids {
            delete_safety.execute([file_id])?;
            delete_findings.execute([file_id])?;
            delete_files.execute([file_id])?;
        }
    }
    tx.commit()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashSet;

    fn insert_safe_finding(
        conn: &Connection,
        scan_id: i64,
        root: &Path,
        rel_path: &str,
        app_id: &str,
    ) -> i64 {
        conn.execute(
            "INSERT OR IGNORE INTO game_libraries (vendor, path) VALUES ('test', ?1)",
            [root.to_string_lossy()],
        )
        .unwrap();
        let library_id: i64 = conn
            .query_row(
                "SELECT id FROM game_libraries WHERE path = ?1",
                [root.to_string_lossy()],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            // A scanned game always carries a verdict; NULL means "never
            // assessed" and reads as protected, which would make every
            // unattended fixture here skip instead of exercising its case.
            // Tests that want protection plant an anti-cheat marker on disk
            // and let the live half of the gate find it.
            "INSERT INTO games (scan_id, library_id, name, install_dir, app_id,
                                anti_cheat_protected)
             VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            rusqlite::params![scan_id, library_id, app_id, root.to_string_lossy(), app_id],
        )
        .unwrap();
        let game_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO files (scan_id, game_id, rel_path, size) VALUES (?1, ?2, ?3, 1)",
            rusqlite::params![scan_id, game_id, rel_path],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO findings (file_id, category, confidence) VALUES (?1, 'bonus', 90)",
            [file_id],
        )
        .unwrap();
        let snapshot = crate::safety::capture_safety_snapshot(root, rel_path).unwrap();
        conn.execute(
            "INSERT INTO file_safety
             (file_id, scan_id, trusted_root, rel_path, root_identity,
              target_identity, target_kind, tree_fingerprint)
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
        crate::db::record_scan_library_evidence(conn, scan_id, root, "test", "complete").unwrap();
        file_id
    }

    #[test]
    fn already_absent_is_distinct_and_purgeable() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("gone.bin"), b"x").unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let file_id = insert_safe_finding(&conn, scan_id, temp.path(), "gone.bin", "one");
        crate::db::activate_scan(&mut conn, scan_id).unwrap();
        let plans = prepare_delete_plans(
            &conn,
            &[file_id],
            DeleteMethod::Permanent,
            DeleteAttendance::Interactive,
        )
        .unwrap();
        std::fs::remove_file(temp.path().join("gone.bin")).unwrap();
        let outcomes = execute_delete_plans_observed(
            &mut conn,
            DeleteMethod::Permanent,
            &plans,
            |_, _, _| {},
            |_, _| {},
        )
        .unwrap();
        assert_eq!(outcomes[0].status, FsOutcome::AlreadyAbsent);
    }

    /// A removal that fails while the target is *still there* must be
    /// `Failed`, never `AlreadyAbsent`.
    ///
    /// The two are settled by re-reading the target's metadata after a failed
    /// remove: `NotFound` means it really did go, anything else means it did
    /// not. `AlreadyAbsent` is the outcome that lets the caller purge the row,
    /// so getting this backwards would drop a finding for a file that is still
    /// on disk - the user is told it is gone, it is not, and nothing ever
    /// proposes it again. Only the `NotFound` side of that branch was covered.
    #[test]
    fn a_failed_removal_with_the_target_still_present_is_failed_not_already_absent() {
        /// Fails every removal, the way a permission denial or a sharing
        /// violation does - without needing to manufacture either.
        struct RefusingRemover;

        impl Remover for RefusingRemover {
            fn remove(&self, _path: &Path) -> Result<()> {
                Err(crate::error::CoreError::Other("access is denied".into()))
            }

            fn action(&self) -> &'static str {
                "delete"
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("locked.bin");
        std::fs::write(&target, b"x").unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let file_id = insert_safe_finding(&conn, scan_id, temp.path(), "locked.bin", "one");
        crate::db::activate_scan(&mut conn, scan_id).unwrap();
        let plans = prepare_delete_plans(
            &conn,
            &[file_id],
            DeleteMethod::Permanent,
            DeleteAttendance::Interactive,
        )
        .unwrap();

        let outcomes = execute_delete_plans_with_remover_observed(
            &mut conn,
            &RefusingRemover,
            &plans,
            |_, _, _| {},
            |_, _| {},
        )
        .unwrap();

        assert_eq!(
            outcomes[0].status,
            FsOutcome::Failed,
            "a refused removal must not be reported as already absent"
        );
        assert!(
            outcomes[0].error.is_some(),
            "a failure must carry its reason"
        );
        assert!(
            target.exists(),
            "the file is still there - that is the whole point"
        );
    }

    /// The whole library root going away must not be read as "every file in it
    /// was already deleted". `AlreadyAbsent` is what lets the caller purge the
    /// rows, so this is the difference between a disconnected drive and a
    /// library silently vanishing from the database.
    #[test]
    fn an_unreachable_library_root_blocks_instead_of_purging_the_rows() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("library");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("leftover.bin"), b"x").unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let file_id = insert_safe_finding(&conn, scan_id, &root, "leftover.bin", "one");
        crate::db::activate_scan(&mut conn, scan_id).unwrap();
        let plans = prepare_delete_plans(
            &conn,
            &[file_id],
            DeleteMethod::Permanent,
            DeleteAttendance::Interactive,
        )
        .unwrap();

        // The volume goes away between planning and execution.
        std::fs::remove_dir_all(&root).unwrap();

        let result = execute_delete_plans_observed(
            &mut conn,
            DeleteMethod::Permanent,
            &plans,
            |_, _, _| {},
            |_, _| {},
        );

        // The batch preflight refuses the whole set before touching anything,
        // so there is no outcome to misread as "already gone".
        let error = result.expect_err("an unreachable root must refuse the batch");
        assert!(
            error.to_string().contains("root is currently unreachable"),
            "unexpected refusal: {error}"
        );
        let findings: i64 = conn
            .query_row("SELECT COUNT(*) FROM findings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(findings, 1, "the finding must survive an unreachable root");
    }

    /// A pending intent whose root is unreachable cannot be settled either way,
    /// so it must stay pending for a later start rather than be recorded as
    /// applied.
    #[test]
    fn reconciliation_leaves_an_operation_pending_while_its_root_is_unreachable() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("library");
        std::fs::create_dir(&root).unwrap();
        let target = root.join("leftover.bin");
        std::fs::write(&target, b"x").unwrap();
        let identity = crate::safety::current_identity(&target).unwrap().encode();

        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO operations
                 (ts, action, src_path, status, trusted_root, rel_path, expected_identity)
             VALUES (0, 'delete', ?1, 'pending', ?2, 'leftover.bin', ?3)",
            rusqlite::params![target.to_string_lossy(), root.to_string_lossy(), identity],
        )
        .unwrap();

        std::fs::remove_dir_all(&root).unwrap();

        let reconciled = reconcile_pending_operations(&mut conn).unwrap();
        assert!(
            reconciled.is_empty(),
            "nothing may be settled while the root is unreachable: {reconciled:?}"
        );
        let status: String = conn
            .query_row("SELECT status FROM operations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(status, "pending");
    }

    /// A directory keeps its volume serial and file index through an
    /// interrupted `remove_dir_all`, so `same_object` says "untouched" about a
    /// folder that has already lost half its contents. That verdict is worse
    /// than no verdict: it invites the same delete to be proposed again over a
    /// tree that is already partly destroyed, and reports the space as still
    /// occupied. The scan-time fingerprint is the only thing that separates
    /// the two cases.
    #[test]
    fn a_half_deleted_directory_is_not_reported_as_untouched() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("library");
        let target = root.join("bonus");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("keep.bin"), b"a").unwrap();
        std::fs::write(target.join("gone.bin"), b"b").unwrap();

        let snapshot = crate::safety::capture_safety_snapshot(&root, "bonus").unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO operations
                 (ts, action, src_path, status, trusted_root, rel_path,
                  expected_identity, expected_tree_fingerprint)
             VALUES (0, 'delete', ?1, 'pending', ?2, 'bonus', ?3, ?4)",
            rusqlite::params![
                target.to_string_lossy(),
                root.to_string_lossy(),
                snapshot.target_identity.encode(),
                snapshot.tree_fingerprint,
            ],
        )
        .unwrap();

        // The delete got partway: one child gone, the directory itself still
        // there and still the same object.
        std::fs::remove_file(target.join("gone.bin")).unwrap();

        let reconciled = reconcile_pending_operations(&mut conn).unwrap();
        assert_eq!(reconciled.len(), 1);
        assert_eq!(
            reconciled[0].outcome, "partially_applied",
            "a directory that lost contents must not settle as not_applied"
        );
    }

    /// The other half of the same comparison: an untouched directory must
    /// still settle as `not_applied`, or the fingerprint check would just be a
    /// way of never trusting anything.
    #[test]
    fn an_untouched_directory_still_reconciles_as_not_applied() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("library");
        let target = root.join("bonus");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("keep.bin"), b"a").unwrap();

        let snapshot = crate::safety::capture_safety_snapshot(&root, "bonus").unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO operations
                 (ts, action, src_path, status, trusted_root, rel_path,
                  expected_identity, expected_tree_fingerprint)
             VALUES (0, 'delete', ?1, 'pending', ?2, 'bonus', ?3, ?4)",
            rusqlite::params![
                target.to_string_lossy(),
                root.to_string_lossy(),
                snapshot.target_identity.encode(),
                snapshot.tree_fingerprint,
            ],
        )
        .unwrap();

        let reconciled = reconcile_pending_operations(&mut conn).unwrap();
        assert_eq!(reconciled.len(), 1);
        assert_eq!(reconciled[0].outcome, "not_applied");
    }

    /// A row written before schema v5 has no fingerprint. That is an
    /// unanswerable question, and an unanswerable question must not be
    /// answered with the confident "nothing happened".
    #[test]
    fn a_directory_intent_without_a_fingerprint_settles_as_unknown() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("library");
        let target = root.join("bonus");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("keep.bin"), b"a").unwrap();

        let identity = crate::safety::current_identity(&target).unwrap().encode();
        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO operations
                 (ts, action, src_path, status, trusted_root, rel_path, expected_identity)
             VALUES (0, 'delete', ?1, 'pending', ?2, 'bonus', ?3)",
            rusqlite::params![target.to_string_lossy(), root.to_string_lossy(), identity],
        )
        .unwrap();

        let reconciled = reconcile_pending_operations(&mut conn).unwrap();
        assert_eq!(reconciled.len(), 1);
        assert_eq!(reconciled[0].outcome, "unknown");
    }

    /// The regression this card exists for: reconciliation used to be
    /// reachable only from `worker::load::run_load`, which the app starts only
    /// when the database already holds findings. Crash midway through a delete
    /// with an empty `findings` table and the `pending` row was stranded
    /// forever - the only code that could settle it was gated on unrelated
    /// state. `db::open` now reconciles unconditionally, so the empty-findings
    /// case is exactly what this asserts: no scan, no findings, just an open.
    #[test]
    fn opening_a_database_with_no_findings_still_settles_a_crash_left_intent() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("library");
        std::fs::create_dir(&root).unwrap();
        let target = root.join("leftover.bin");
        std::fs::write(&target, b"x").unwrap();
        let identity = crate::safety::current_identity(&target).unwrap().encode();

        let db_path = parent.path().join("gametrimmer.db");
        {
            let conn = crate::db::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO operations
                     (ts, action, src_path, status, trusted_root, rel_path, expected_identity)
                 VALUES (0, 'delete', ?1, 'pending', ?2, 'leftover.bin', ?3)",
                rusqlite::params![target.to_string_lossy(), root.to_string_lossy(), identity],
            )
            .unwrap();
            let findings: i64 = conn
                .query_row("SELECT COUNT(*) FROM findings", [], |row| row.get(0))
                .unwrap();
            assert_eq!(findings, 0, "the whole point is an empty findings table");
        }

        // The delete did happen; the process died before the row was settled.
        std::fs::remove_file(&target).unwrap();

        let (conn, reconciliation) = crate::db::open_reconciling(&db_path).unwrap();
        assert!(reconciliation.error.is_none());
        assert_eq!(
            reconciliation.reconciled.len(),
            1,
            "the open must settle the stranded intent"
        );
        let (status, outcome): (String, String) = conn
            .query_row("SELECT status, outcome FROM operations", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(status, "final");
        assert_eq!(outcome, "reconciled_removed");
    }

    /// The plain `open` must reconcile too - it is the entry point nineteen of
    /// twenty callers use, so if it could skip reconciliation the guarantee
    /// would be worth nothing.
    #[test]
    fn the_plain_open_reconciles_as_well_as_the_reporting_one() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("library");
        std::fs::create_dir(&root).unwrap();
        let target = root.join("leftover.bin");
        std::fs::write(&target, b"x").unwrap();
        let identity = crate::safety::current_identity(&target).unwrap().encode();

        let db_path = parent.path().join("gametrimmer.db");
        {
            let conn = crate::db::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO operations
                     (ts, action, src_path, status, trusted_root, rel_path, expected_identity)
                 VALUES (0, 'delete', ?1, 'pending', ?2, 'leftover.bin', ?3)",
                rusqlite::params![target.to_string_lossy(), root.to_string_lossy(), identity],
            )
            .unwrap();
        }
        std::fs::remove_file(&target).unwrap();

        let conn = crate::db::open(&db_path).unwrap();
        let (status, outcome): (String, String) = conn
            .query_row("SELECT status, outcome FROM operations", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(status, "final");
        assert_eq!(outcome, "reconciled_removed");
    }

    /// The counterpart: with the root present and the target gone, the intent
    /// really was applied and must be settled.
    #[test]
    fn reconciliation_settles_an_operation_whose_target_is_gone_under_a_live_root() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("library");
        std::fs::create_dir(&root).unwrap();
        let target = root.join("leftover.bin");
        std::fs::write(&target, b"x").unwrap();
        let identity = crate::safety::current_identity(&target).unwrap().encode();

        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO operations
                 (ts, action, src_path, status, trusted_root, rel_path, expected_identity)
             VALUES (0, 'delete', ?1, 'pending', ?2, 'leftover.bin', ?3)",
            rusqlite::params![target.to_string_lossy(), root.to_string_lossy(), identity],
        )
        .unwrap();

        std::fs::remove_file(&target).unwrap();

        let reconciled = reconcile_pending_operations(&mut conn).unwrap();
        assert_eq!(reconciled.len(), 1);
        assert_eq!(reconciled[0].outcome, "reconciled_removed");
    }

    #[test]
    fn whole_batch_preflight_runs_before_the_first_mutation() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("safe.bin"), b"safe").unwrap();
        std::fs::write(temp.path().join("swapped.bin"), b"old").unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let safe = insert_safe_finding(&conn, scan_id, temp.path(), "safe.bin", "one");
        let swapped = insert_safe_finding(&conn, scan_id, temp.path(), "swapped.bin", "two");
        crate::db::activate_scan(&mut conn, scan_id).unwrap();
        let plans = prepare_delete_plans(
            &conn,
            &[safe, swapped],
            DeleteMethod::Permanent,
            DeleteAttendance::Interactive,
        )
        .unwrap();
        std::fs::remove_file(temp.path().join("swapped.bin")).unwrap();
        std::fs::write(temp.path().join("swapped.bin"), b"replacement").unwrap();
        assert!(execute_delete_plans_observed(
            &mut conn,
            DeleteMethod::Permanent,
            &plans,
            |_, _, _| {},
            |_, _| {},
        )
        .is_err());
        assert!(temp.path().join("safe.bin").is_file());
    }

    #[test]
    fn missing_library_evidence_blocks_an_active_finding() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("target.bin"), b"safe").unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let file_id = insert_safe_finding(&conn, scan_id, temp.path(), "target.bin", "one");
        crate::db::activate_scan(&mut conn, scan_id).unwrap();
        conn.execute(
            "DELETE FROM scan_library_evidence WHERE scan_id = ?1",
            [scan_id],
        )
        .unwrap();

        let error = prepare_delete_plans(
            &conn,
            &[file_id],
            DeleteMethod::Permanent,
            DeleteAttendance::Interactive,
        )
        .unwrap_err();
        assert!(error.to_string().contains("filesystem identity is missing"));
        assert!(temp.path().join("target.bin").is_file());
    }

    #[test]
    fn duplicate_file_ids_are_rejected_before_any_mutation() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("target.bin"), b"safe").unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let file_id = insert_safe_finding(&conn, scan_id, temp.path(), "target.bin", "one");
        crate::db::activate_scan(&mut conn, scan_id).unwrap();

        let error = prepare_delete_plans(
            &conn,
            &[file_id, file_id],
            DeleteMethod::Permanent,
            DeleteAttendance::Interactive,
        )
        .unwrap_err();
        assert!(error.to_string().contains("duplicate file id"));
        assert!(temp.path().join("target.bin").is_file());
    }

    #[test]
    /// A `findings.category = 'monolithic_archive'` row is residue from a
    /// build that had an in-place archive trimmer; that feature is gone and
    /// no code writes this category any more, but an old database can still
    /// carry the row. It must never turn into a whole-file delete plan - the
    /// path-based block in [`prepare_delete_plans_with_skips`] is what
    /// guarantees that, entirely by the file's extension, before the
    /// category or the (also legacy) `action` column is even read.
    fn monolithic_category_row_is_skipped_by_the_path_based_block() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("archive.pck"), b"archive").unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let file_id = insert_safe_finding(&conn, scan_id, temp.path(), "archive.pck", "archive");
        conn.execute(
            "UPDATE findings SET category = 'monolithic_archive', action = NULL \
             WHERE file_id = ?1",
            [file_id],
        )
        .unwrap();
        crate::db::activate_scan(&mut conn, scan_id).unwrap();

        let (plans, skips) = prepare_delete_plans_with_skips(
            &conn,
            &[file_id],
            DeleteMethod::Permanent,
            DeleteAttendance::Interactive,
        )
        .unwrap();
        assert!(
            plans.is_empty(),
            "a monolithic archive candidate must never get a delete plan"
        );
        assert_eq!(skips.len(), 1);
        assert!(skips[0].reason.contains("monolithic archive candidate"));
        assert!(temp.path().join("archive.pck").is_file());
    }

    #[test]
    fn ordinary_category_cannot_smuggle_archive_candidate_into_direct_delete() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("EasyAntiCheat")).unwrap();
        std::fs::write(temp.path().join("voices.pck"), b"container").unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let file_id = insert_safe_finding(&conn, scan_id, temp.path(), "voices.pck", "archive");
        conn.execute(
            "UPDATE findings SET category = 'docs_file', provenance = 'imported_untrusted', \
             action = NULL WHERE file_id = ?1",
            [file_id],
        )
        .unwrap();
        crate::db::activate_scan(&mut conn, scan_id).unwrap();

        let (plans, skips) = prepare_delete_plans_with_skips(
            &conn,
            &[file_id],
            DeleteMethod::Permanent,
            DeleteAttendance::Interactive,
        )
        .unwrap();
        assert!(plans.is_empty(), "a container must never get a delete plan");
        assert_eq!(skips.len(), 1);
        assert!(skips[0].reason.contains("monolithic archive candidate"));
        assert!(temp.path().join("voices.pck").is_file());
    }

    #[test]
    /// A misleading name is classification's problem, not the preflight's.
    /// The preflight never reads a file's bytes to decide whether it may be
    /// deleted whole - only its path, against
    /// [`crate::worker::is_candidate_archive_path`].
    fn a_misleading_name_reaches_a_plan_because_the_preflight_stopped_reading() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = b"AKPK-shaped bytes do not matter to the preflight".to_vec();
        std::fs::write(temp.path().join("sounds_fra.pck"), &bytes).unwrap();
        std::fs::write(temp.path().join("manual.txt"), &bytes).unwrap();
        // Neither name is a monolithic archive candidate, so the name check
        // that remains has no opinion about either.
        assert!(!crate::worker::is_candidate_archive_path("sounds_fra.pck"));
        assert!(!crate::worker::is_candidate_archive_path("manual.txt"));

        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let language_named = insert_safe_finding(
            &conn,
            scan_id,
            temp.path(),
            "sounds_fra.pck",
            "language-named",
        );
        let text_named =
            insert_safe_finding(&conn, scan_id, temp.path(), "manual.txt", "text-named");
        crate::db::activate_scan(&mut conn, scan_id).unwrap();

        for (file_id, name) in [
            (language_named, "sounds_fra.pck"),
            (text_named, "manual.txt"),
        ] {
            let (plans, skips) = prepare_delete_plans_with_skips(
                &conn,
                &[file_id],
                DeleteMethod::Permanent,
                DeleteAttendance::Interactive,
            )
            .unwrap();
            assert!(
                skips.is_empty(),
                "{name} must not be held back by a probe the preflight no longer runs"
            );
            assert_eq!(
                plans.iter().map(|plan| plan.file_id).collect::<Vec<_>>(),
                vec![file_id],
                "{name} was selected, so it gets a plan"
            );
        }
    }

    #[test]
    fn the_preflight_holds_files_back_by_name_and_never_by_reading_them() {
        // Real files, not stubs of their magic bytes: `BIK1_STUB` is the very
        // file this app writes over an intro, and the PCK comes out of the
        // Wwise handler's own fixture builder.
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("intro.bik"), crate::stub::BIK1_STUB).unwrap();
        std::fs::write(temp.path().join("logo.bk2"), crate::stub::BINK2_STUB).unwrap();
        let container = b"AKPK-shaped bytes do not matter to the preflight".to_vec();
        // Same bytes, two names. `voices.pck` is a monolithic archive
        // candidate by name alone; `manual.txt` is not, and nothing but its
        // contents could say otherwise.
        std::fs::write(temp.path().join("voices.pck"), &container).unwrap();
        std::fs::write(temp.path().join("manual.txt"), &container).unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let bink1 = insert_safe_finding(&conn, scan_id, temp.path(), "intro.bik", "bink1");
        let bink2 = insert_safe_finding(&conn, scan_id, temp.path(), "logo.bk2", "bink2");
        let named = insert_safe_finding(&conn, scan_id, temp.path(), "voices.pck", "pck");
        let disguised = insert_safe_finding(&conn, scan_id, temp.path(), "manual.txt", "pck");
        crate::db::activate_scan(&mut conn, scan_id).unwrap();

        let (plans, skips) = prepare_delete_plans_with_skips(
            &conn,
            &[bink1, bink2],
            DeleteMethod::Permanent,
            DeleteAttendance::Interactive,
        )
        .unwrap();
        assert!(
            skips.is_empty(),
            "a Bink video is one video, not a container of separable assets"
        );
        assert_eq!(
            plans.iter().map(|plan| plan.file_id).collect::<Vec<_>>(),
            vec![bink1, bink2],
            "both Bink generations must reach a delete plan"
        );

        // The name check stays: it costs no I/O and it is what reserves a
        // container for in-place trimming.
        let (plans, skips) = prepare_delete_plans_with_skips(
            &conn,
            &[named],
            DeleteMethod::Permanent,
            DeleteAttendance::Interactive,
        )
        .unwrap();
        assert!(plans.is_empty());
        assert_eq!(skips.len(), 1);
        assert!(skips[0].reason.contains("monolithic archive candidate"));

        // The same container under a name the preflight has no opinion about
        // reaches a delete plan: the preflight never reads a file's bytes,
        // only its path.
        let (plans, skips) = prepare_delete_plans_with_skips(
            &conn,
            &[disguised],
            DeleteMethod::Permanent,
            DeleteAttendance::Interactive,
        )
        .unwrap();
        assert!(
            skips.is_empty(),
            "the preflight must not read a selected file's bytes to hold it back"
        );
        assert_eq!(
            plans.iter().map(|plan| plan.file_id).collect::<Vec<_>>(),
            vec![disguised]
        );
    }

    #[test]
    fn one_blocked_container_does_not_cost_the_rest_of_the_batch_its_plans() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("intro.bik"), crate::stub::BIK1_STUB).unwrap();
        std::fs::write(temp.path().join("readme.txt"), b"a plain leftover file").unwrap();
        std::fs::write(temp.path().join("changelog.txt"), b"another one").unwrap();
        let mut kpka = b"KPKA".to_vec();
        kpka.extend_from_slice(&[0u8; 60]);
        // Held back by its name, not its bytes - `re_chunk_000.pak` is a
        // monolithic archive candidate before anything is read.
        std::fs::write(temp.path().join("re_chunk_000.pak"), &kpka).unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let bink = insert_safe_finding(&conn, scan_id, temp.path(), "intro.bik", "bink");
        let readme = insert_safe_finding(&conn, scan_id, temp.path(), "readme.txt", "readme");
        let container = insert_safe_finding(&conn, scan_id, temp.path(), "re_chunk_000.pak", "pak");
        let changelog =
            insert_safe_finding(&conn, scan_id, temp.path(), "changelog.txt", "changelog");
        crate::db::activate_scan(&mut conn, scan_id).unwrap();

        let (plans, skips) = prepare_delete_plans_with_skips(
            &conn,
            &[bink, readme, container, changelog],
            DeleteMethod::Permanent,
            DeleteAttendance::Interactive,
        )
        .unwrap();

        assert_eq!(
            plans.iter().map(|plan| plan.file_id).collect::<Vec<_>>(),
            vec![bink, readme, changelog],
            "the ordinary files keep their plans when one container is held back"
        );
        assert_eq!(skips.len(), 1);
        assert_eq!(skips[0].file_id, container);
        assert_eq!(skips[0].path, temp.path().join("re_chunk_000.pak"));
        assert!(
            skips[0].reason.contains("monolithic archive candidate"),
            "the skip has to say why, not just name a file id"
        );
    }

    #[test]
    fn execution_rechecks_all_contracts_before_mutating_a_stale_batch() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("first.bin"), b"first").unwrap();
        std::fs::write(temp.path().join("second.bin"), b"second").unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let first_id = insert_safe_finding(&conn, scan_id, temp.path(), "first.bin", "first");
        let second_id = insert_safe_finding(&conn, scan_id, temp.path(), "second.bin", "second");
        crate::db::activate_scan(&mut conn, scan_id).unwrap();
        let plans = prepare_delete_plans(
            &conn,
            &[first_id, second_id],
            DeleteMethod::Permanent,
            DeleteAttendance::Interactive,
        )
        .unwrap();

        // The batch went stale between planning and execution: the library
        // this scan trusted no longer verifies. Any one of the preflight's
        // contracts would serve here - what is pinned is that execution
        // re-derives all of them, and does so before touching the first
        // file rather than one plan at a time.
        crate::db::record_scan_library_evidence(&conn, scan_id, temp.path(), "test", "failed")
            .unwrap();
        assert!(execute_delete_plans_observed(
            &mut conn,
            DeleteMethod::Permanent,
            &plans,
            |_, _, _| {},
            |_, _| {},
        )
        .is_err());
        assert!(
            temp.path().join("first.bin").is_file(),
            "batch contract recheck must happen before the first mutation"
        );
        assert!(temp.path().join("second.bin").is_file());
    }

    #[test]
    fn unknown_or_failed_evidence_status_is_never_deletable() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("target.bin"), b"safe").unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let file_id = insert_safe_finding(&conn, scan_id, temp.path(), "target.bin", "one");
        crate::db::activate_scan(&mut conn, scan_id).unwrap();

        for status in ["failed", "unexpected"] {
            crate::db::record_scan_library_evidence(&conn, scan_id, temp.path(), "test", status)
                .unwrap();
            assert!(prepare_delete_plans(
                &conn,
                &[file_id],
                DeleteMethod::Permanent,
                DeleteAttendance::Interactive
            )
            .is_err());
        }
        assert!(temp.path().join("target.bin").is_file());
    }

    #[test]
    fn orphan_finding_requires_authoritative_library_evidence() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("orphan.bin"), b"safe").unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        conn.execute(
            "INSERT INTO files (scan_id, game_id, rel_path, size)
             VALUES (?1, NULL, 'orphan.bin', 4)",
            [scan_id],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO findings (file_id, category, confidence)
             VALUES (?1, 'orphan_folder', 90)",
            [file_id],
        )
        .unwrap();
        let snapshot = crate::safety::capture_safety_snapshot(temp.path(), "orphan.bin").unwrap();
        conn.execute(
            "INSERT INTO file_safety
             (file_id, scan_id, evidence_library_path, trusted_root, rel_path,
              root_identity, target_identity, target_kind, tree_fingerprint)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                file_id,
                scan_id,
                temp.path().to_string_lossy(),
                snapshot.trusted_root.to_string_lossy(),
                snapshot.rel_path.to_string_lossy(),
                snapshot.root_identity.encode(),
                snapshot.target_identity.encode(),
                snapshot.target_identity.kind.as_str(),
                snapshot.tree_fingerprint,
            ],
        )
        .unwrap();
        crate::db::record_scan_library_evidence(&conn, scan_id, temp.path(), "test", "heuristic")
            .unwrap();
        crate::db::activate_scan(&mut conn, scan_id).unwrap();

        assert!(prepare_delete_plans(
            &conn,
            &[file_id],
            DeleteMethod::Permanent,
            DeleteAttendance::Interactive
        )
        .is_err());
        crate::db::record_scan_library_evidence(&conn, scan_id, temp.path(), "test", "complete")
            .unwrap();
        assert!(prepare_delete_plans(
            &conn,
            &[file_id],
            DeleteMethod::Permanent,
            DeleteAttendance::Interactive
        )
        .is_ok());
    }

    /// Mock remover for testing that never touches the real Recycle Bin.
    struct MockRemover {
        /// Paths that should be removed successfully.
        success_paths: RefCell<HashSet<PathBuf>>,
        /// Paths that should fail with this error message.
        fail_paths: RefCell<std::collections::HashMap<PathBuf, String>>,
    }

    impl MockRemover {
        fn new() -> Self {
            Self {
                success_paths: RefCell::new(HashSet::new()),
                fail_paths: RefCell::new(std::collections::HashMap::new()),
            }
        }

        fn add_success(&self, path: PathBuf) {
            self.success_paths.borrow_mut().insert(path);
        }

        fn add_failure(&self, path: PathBuf, error: String) {
            self.fail_paths.borrow_mut().insert(path, error);
        }
    }

    impl Remover for MockRemover {
        fn remove(&self, path: &Path) -> Result<()> {
            let path_buf = path.to_path_buf();
            if let Some(error_msg) = self.fail_paths.borrow().get(&path_buf) {
                return Err(crate::error::CoreError::Other(error_msg.clone()));
            }
            if self.success_paths.borrow().contains(&path_buf) {
                return Ok(());
            }
            // Default to failure if not in either set
            Err(crate::error::CoreError::Other(
                "path not registered in mock".to_string(),
            ))
        }

        fn action(&self) -> &'static str {
            "recycle"
        }
    }

    #[test]
    fn two_paths_removed_successfully() {
        let mut conn = crate::db::open_in_memory().expect("open in-memory db");
        let mock = MockRemover::new();

        let path1 = PathBuf::from("C:\\Games\\Game1");
        let path2 = PathBuf::from("C:\\Games\\Game2");

        mock.add_success(path1.clone());
        mock.add_success(path2.clone());

        let outcomes = remove_with_log(&mut conn, &mock, &[path1.clone(), path2.clone()])
            .expect("remove_with_log should succeed");

        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes[0].path, path1);
        assert!(outcomes[0].error.is_none());
        assert_eq!(outcomes[1].path, path2);
        assert!(outcomes[1].error.is_none());

        // Verify database entries
        let mut stmt = conn
            .prepare("SELECT src_path, status FROM operations ORDER BY id")
            .expect("prepare query");
        let records: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query_map")
            .collect::<std::result::Result<_, _>>()
            .expect("collect");

        assert_eq!(records.len(), 2);
        assert_eq!(
            records[0],
            ("C:\\Games\\Game1".to_string(), "done".to_string())
        );
        assert_eq!(
            records[1],
            ("C:\\Games\\Game2".to_string(), "done".to_string())
        );
    }

    #[test]
    fn first_path_fails_second_succeeds() {
        let mut conn = crate::db::open_in_memory().expect("open in-memory db");
        let mock = MockRemover::new();

        let path1 = PathBuf::from("C:\\Games\\Corrupted");
        let path2 = PathBuf::from("C:\\Games\\Good");

        mock.add_failure(path1.clone(), "permission denied".to_string());
        mock.add_success(path2.clone());

        let outcomes = remove_with_log(&mut conn, &mock, &[path1.clone(), path2.clone()])
            .expect("remove_with_log should succeed despite individual failure");

        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes[0].path, path1);
        assert!(outcomes[0].error.is_some());
        assert_eq!(outcomes[1].path, path2);
        assert!(outcomes[1].error.is_none());

        // Verify database entries
        let mut stmt = conn
            .prepare("SELECT src_path, status FROM operations ORDER BY id")
            .expect("prepare query");
        let records: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query_map")
            .collect::<std::result::Result<_, _>>()
            .expect("collect");

        assert_eq!(records.len(), 2);
        assert_eq!(
            records[0],
            ("C:\\Games\\Corrupted".to_string(), "failed".to_string())
        );
        assert_eq!(
            records[1],
            ("C:\\Games\\Good".to_string(), "done".to_string())
        );
    }

    #[test]
    fn records_have_correct_action_and_src_path() {
        let mut conn = crate::db::open_in_memory().expect("open in-memory db");
        let mock = MockRemover::new();

        let path = PathBuf::from("C:\\Games\\TestGame");
        mock.add_success(path.clone());

        remove_with_log(&mut conn, &mock, std::slice::from_ref(&path))
            .expect("remove_with_log should succeed");

        let (action, src_path): (String, String) = conn
            .query_row(
                "SELECT action, src_path FROM operations WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("query_row");

        assert_eq!(action, "recycle");
        assert_eq!(src_path, "C:\\Games\\TestGame");
    }

    #[test]
    fn permanent_delete_removes_a_file() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("junk.txt");
        std::fs::write(&file_path, b"payload").expect("write test file");

        PermanentDelete
            .remove(&file_path)
            .expect("permanent delete should succeed");

        assert!(!file_path.exists(), "file must be gone after removal");
    }

    #[test]
    fn permanent_delete_removes_a_directory_recursively() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let sub_dir = dir.path().join("redist");
        std::fs::create_dir(&sub_dir).expect("create sub dir");
        std::fs::write(sub_dir.join("setup.exe"), b"payload").expect("write nested file");

        PermanentDelete
            .remove(&sub_dir)
            .expect("permanent delete should succeed");

        assert!(!sub_dir.exists(), "directory must be gone after removal");
    }

    #[test]
    fn permanent_delete_reports_a_missing_path_as_an_error() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let missing = dir.path().join("never_existed.bin");

        assert!(
            PermanentDelete.remove(&missing).is_err(),
            "a missing path must surface as a per-file error, not silent success"
        );
    }

    #[test]
    fn remove_with_log_observed_reports_progress_in_order_before_each_removal() {
        let mut conn = crate::db::open_in_memory().expect("open in-memory db");
        let mock = MockRemover::new();

        let path1 = PathBuf::from("C:\\Games\\Game1");
        let path2 = PathBuf::from("C:\\Games\\Game2");
        let path3 = PathBuf::from("C:\\Games\\Game3");
        mock.add_success(path1.clone());
        mock.add_success(path2.clone());
        mock.add_success(path3.clone());

        let paths = [path1.clone(), path2.clone(), path3.clone()];
        let mut calls: Vec<(usize, usize, PathBuf)> = Vec::new();

        let outcomes = remove_with_log_observed(
            &mut conn,
            &mock,
            &paths,
            |current, total, path| {
                calls.push((current, total, path.to_path_buf()));
            },
            |_, _| {},
        )
        .expect("remove_with_log_observed should succeed");

        assert_eq!(outcomes.len(), 3);
        assert_eq!(
            calls,
            vec![(1, 3, path1), (2, 3, path2), (3, 3, path3),],
            "callback must receive (1..=len) in order with the matching path"
        );
    }

    #[test]
    fn remove_with_log_observed_reports_each_outcome_once_in_order() {
        let mut conn = crate::db::open_in_memory().expect("open in-memory db");
        let mock = MockRemover::new();

        let path1 = PathBuf::from("C:\\Games\\Game1");
        let path2 = PathBuf::from("C:\\Games\\Game2");
        let path3 = PathBuf::from("C:\\Games\\Game3");
        mock.add_success(path1.clone());
        mock.add_success(path2.clone());
        mock.add_success(path3.clone());

        let paths = [path1.clone(), path2.clone(), path3.clone()];
        let mut outcome_calls: Vec<(usize, PathBuf, Option<String>)> = Vec::new();

        let outcomes = remove_with_log_observed(
            &mut conn,
            &mock,
            &paths,
            |_, _, _| {},
            |index, outcome| {
                outcome_calls.push((index, outcome.path.clone(), outcome.error.clone()));
            },
        )
        .expect("remove_with_log_observed should succeed");

        assert_eq!(outcomes.len(), 3);
        assert_eq!(
            outcome_calls,
            vec![(0, path1, None), (1, path2, None), (2, path3, None),],
            "on_outcome must fire once per path, in order, with the outcome's error"
        );
    }

    #[test]
    fn permanent_delete_journals_the_delete_action() {
        let mut conn = crate::db::open_in_memory().expect("open in-memory db");
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("junk.txt");
        std::fs::write(&file_path, b"payload").expect("write test file");

        remove_with_log(
            &mut conn,
            &PermanentDelete,
            std::slice::from_ref(&file_path),
        )
        .expect("remove_with_log should succeed");

        let (action, status): (String, String) = conn
            .query_row(
                "SELECT action, status FROM operations WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("query_row");

        assert_eq!(action, "delete");
        assert_eq!(status, "done");
    }

    #[test]
    fn purge_removed_files_deletes_findings_and_files_for_given_ids_only() {
        let mut conn = crate::db::open_in_memory().expect("open in-memory db");

        // game_id may be NULL - files aren't required to belong to a game.
        conn.execute(
            "INSERT INTO files (id, game_id, rel_path, size, mtime) VALUES (1, NULL, 'a.txt', 10, NULL)",
            [],
        )
        .expect("insert file 1");
        conn.execute(
            "INSERT INTO files (id, game_id, rel_path, size, mtime) VALUES (2, NULL, 'b.txt', 20, NULL)",
            [],
        )
        .expect("insert file 2");
        conn.execute(
            "INSERT INTO findings (file_id, category, rule_id, confidence, lang_tag) VALUES (1, 'redist', NULL, 90, NULL)",
            [],
        )
        .expect("insert finding 1");
        conn.execute(
            "INSERT INTO findings (file_id, category, rule_id, confidence, lang_tag) VALUES (2, 'redist', NULL, 90, NULL)",
            [],
        )
        .expect("insert finding 2");

        purge_removed_files(&mut conn, &[1]).expect("purge should succeed");

        let remaining_files: Vec<i64> = conn
            .prepare("SELECT id FROM files ORDER BY id")
            .expect("prepare files query")
            .query_map([], |row| row.get(0))
            .expect("query_map files")
            .collect::<std::result::Result<_, _>>()
            .expect("collect files");
        assert_eq!(remaining_files, vec![2], "only file 1 should be purged");

        let remaining_findings: Vec<i64> = conn
            .prepare("SELECT file_id FROM findings ORDER BY file_id")
            .expect("prepare findings query")
            .query_map([], |row| row.get(0))
            .expect("query_map findings")
            .collect::<std::result::Result<_, _>>()
            .expect("collect findings");
        assert_eq!(
            remaining_findings,
            vec![2],
            "only file 1's finding should be purged"
        );
    }

    #[test]
    fn purge_removed_files_with_empty_slice_is_a_no_op() {
        let mut conn = crate::db::open_in_memory().expect("open in-memory db");

        conn.execute(
            "INSERT INTO files (id, game_id, rel_path, size, mtime) VALUES (1, NULL, 'a.txt', 10, NULL)",
            [],
        )
        .expect("insert file 1");
        conn.execute(
            "INSERT INTO findings (file_id, category, rule_id, confidence, lang_tag) VALUES (1, 'redist', NULL, 90, NULL)",
            [],
        )
        .expect("insert finding 1");

        purge_removed_files(&mut conn, &[]).expect("empty slice should succeed as a no-op");

        let file_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
            .expect("count files");
        let finding_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM findings", [], |row| row.get(0))
            .expect("count findings");
        assert_eq!(file_count, 1, "no-op must leave files untouched");
        assert_eq!(finding_count, 1, "no-op must leave findings untouched");
    }

    /// Both refusals are fail-closed, and an operator has to be able to tell
    /// which one happened: the shield collapses them into one `false`, and
    /// the skip line used to claim anti-cheat for a folder that had simply
    /// gone missing.
    #[test]
    fn an_unwalkable_directory_is_refused_without_claiming_anti_cheat() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("ordinary.bin"), b"x").unwrap();
        assert_eq!(
            unattended_skip_reason(temp.path()),
            None,
            "a directory that walks clean is not a refusal"
        );

        std::fs::create_dir_all(temp.path().join("EasyAntiCheat")).unwrap();
        std::fs::write(
            temp.path().join("EasyAntiCheat").join("EasyAntiCheat.exe"),
            b"MZ",
        )
        .unwrap();
        let detected = unattended_skip_reason(temp.path()).expect("a detected engine refuses");
        assert!(detected.contains("anti-cheat"), "{detected}");
        assert!(
            detected.contains("Easy Anti-Cheat"),
            "the refusal must name what was found: {detected}"
        );

        let missing = temp.path().join("moved-by-a-launcher-update");
        let unwalkable = unattended_skip_reason(&missing).expect("an unproven verdict refuses");
        assert!(
            unwalkable.contains("could not complete"),
            "a failed walk must not be reported as anti-cheat: {unwalkable}"
        );
    }

    #[test]
    fn unattended_preflight_skips_every_file_in_an_anti_cheat_protected_game() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("EasyAntiCheat")).unwrap();
        std::fs::write(
            temp.path().join("EasyAntiCheat").join("EasyAntiCheat.exe"),
            b"MZ",
        )
        .unwrap();
        std::fs::write(temp.path().join("one.bin"), b"one").unwrap();
        std::fs::write(temp.path().join("two.bin"), b"two").unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let file_one = insert_safe_finding(&conn, scan_id, temp.path(), "one.bin", "eac-game");
        // A second file in the *same* game: `insert_safe_finding` inserts a
        // fresh game row on every call, so the second file is attached
        // directly to the first file's own game instead of calling it again
        // - this is what proves the batch shares one verdict, not two.
        let game_id: i64 = conn
            .query_row(
                "SELECT game_id FROM files WHERE id = ?1",
                [file_one],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO files (scan_id, game_id, rel_path, size) VALUES (?1, ?2, 'two.bin', 1)",
            rusqlite::params![scan_id, game_id],
        )
        .unwrap();
        let file_two = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO findings (file_id, category, confidence) VALUES (?1, 'bonus', 90)",
            [file_two],
        )
        .unwrap();
        let snapshot = crate::safety::capture_safety_snapshot(temp.path(), "two.bin").unwrap();
        conn.execute(
            "INSERT INTO file_safety
             (file_id, scan_id, trusted_root, rel_path, root_identity,
              target_identity, target_kind, tree_fingerprint)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                file_two,
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
        crate::db::activate_scan(&mut conn, scan_id).unwrap();

        let (plans, skips) = prepare_delete_plans_with_skips(
            &conn,
            &[file_one, file_two],
            DeleteMethod::Permanent,
            DeleteAttendance::Unattended,
        )
        .unwrap();

        assert!(
            plans.is_empty(),
            "an anti-cheat-protected game blocks every file in the batch"
        );
        assert_eq!(skips.len(), 2);
        for skip in &skips {
            assert!(
                skip.reason.contains("anti-cheat"),
                "unexpected reason: {}",
                skip.reason
            );
        }
        assert!(temp.path().join("one.bin").is_file());
        assert!(temp.path().join("two.bin").is_file());
    }

    #[test]
    fn unattended_preflight_deletes_normally_outside_an_anti_cheat_game() {
        // Guards against over-blocking: an ordinary game must not pay any
        // price for the new gate.
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("leftover.bin"), b"x").unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let file_id =
            insert_safe_finding(&conn, scan_id, temp.path(), "leftover.bin", "ordinary-game");
        crate::db::activate_scan(&mut conn, scan_id).unwrap();

        let plans = prepare_delete_plans(
            &conn,
            &[file_id],
            DeleteMethod::Permanent,
            DeleteAttendance::Unattended,
        )
        .unwrap();
        assert_eq!(plans.len(), 1, "an ordinary game must not be over-blocked");

        let outcomes = execute_delete_plans_observed(
            &mut conn,
            DeleteMethod::Permanent,
            &plans,
            |_, _, _| {},
            |_, _| {},
        )
        .unwrap();
        assert_eq!(outcomes[0].status, FsOutcome::Removed);
        assert!(!temp.path().join("leftover.bin").is_file());
    }

    /// The stored verdict has to be able to refuse on its own, because the
    /// two detectors do not see the same things. The scan's verdict matches
    /// substrings over the whole inventory and catches a `Vanguard\`
    /// directory; the live walk matches exact file names and does not. This
    /// game is clean on disk as far as that walk is concerned - no marker
    /// anywhere - and must still be refused on the strength of what the scan
    /// recorded, or the walk's blind spot becomes a hole in the gate.
    #[test]
    fn a_stored_verdict_refuses_an_unattended_delete_with_no_marker_on_disk() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("loc_fr.pak"), b"x").unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let file_id = insert_safe_finding(&conn, scan_id, temp.path(), "loc_fr.pak", "riot-game");
        crate::db::activate_scan(&mut conn, scan_id).unwrap();
        conn.execute("UPDATE games SET anti_cheat_protected = 1", [])
            .unwrap();

        let (plans, skips) = prepare_delete_plans_with_skips(
            &conn,
            &[file_id],
            DeleteMethod::Permanent,
            DeleteAttendance::Unattended,
        )
        .unwrap();

        assert!(plans.is_empty(), "the stored verdict alone must refuse");
        assert_eq!(skips.len(), 1, "and the refusal must be reported");
        assert!(
            skips[0].reason.contains("last scan"),
            "the reason must name the scan as its source rather than claim a live finding: {}",
            skips[0].reason
        );
        assert!(
            temp.path().join("loc_fr.pak").is_file(),
            "the file must survive"
        );
    }

    /// The executor used to re-ask the anti-cheat question, and `is_safe`
    /// fails closed on a walk that cannot complete. An install directory that
    /// went momentarily unreadable between prepare and execute (a launcher
    /// update, which is exactly when an unattended re-trim runs) therefore
    /// emptied the recheck batch and aborted the whole run under
    /// `StaleDatabaseRow`, a verdict that is both wrong and misnamed.
    #[test]
    fn an_install_directory_lost_between_prepare_and_execute_no_longer_aborts_the_batch() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("leftover.bin"), b"x").unwrap();
        // The game's install directory is kept in its own root so it can
        // disappear mid-batch without disturbing the identity of the tree the
        // delete target lives in - the walk has to be the only thing that
        // breaks, or the test proves something else.
        let install = tempfile::tempdir().unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let file_id =
            insert_safe_finding(&conn, scan_id, temp.path(), "leftover.bin", "ordinary-game");
        conn.execute(
            "UPDATE games SET install_dir = ?1",
            [install.path().to_string_lossy()],
        )
        .unwrap();
        crate::db::activate_scan(&mut conn, scan_id).unwrap();

        let plans = prepare_delete_plans(
            &conn,
            &[file_id],
            DeleteMethod::Permanent,
            DeleteAttendance::Unattended,
        )
        .unwrap();
        assert_eq!(plans.len(), 1, "an ordinary game plans normally");

        install.close().unwrap();

        let outcomes = execute_delete_plans_observed(
            &mut conn,
            DeleteMethod::Permanent,
            &plans,
            |_, _, _| {},
            |_, _| {},
        )
        .expect("a directory that cannot be walked must not fail the batch");
        assert_eq!(outcomes[0].status, FsOutcome::Removed);
        assert!(!temp.path().join("leftover.bin").is_file());
    }

    /// Finding 5: the unattended gate used to be a `if let (Some, Some)`, so a
    /// file with no game row - an orphan, a janitor artifact - fell past the
    /// anti-cheat check entirely and was deleted unattended. Every other
    /// missing-evidence case in this preflight fails closed; so does this one.
    #[test]
    fn an_unattended_file_with_no_game_row_is_skipped_rather_than_deleted() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("leftover.bin"), b"x").unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let file_id =
            insert_safe_finding(&conn, scan_id, temp.path(), "leftover.bin", "ordinary-game");
        // What an orphan-residue or janitor row looks like: safety evidence
        // and library evidence, but no game and so no directory to clear.
        conn.execute(
            "UPDATE file_safety SET evidence_library_path = ?1 WHERE file_id = ?2",
            rusqlite::params![temp.path().to_string_lossy(), file_id],
        )
        .unwrap();
        conn.execute("UPDATE files SET game_id = NULL", []).unwrap();
        crate::db::activate_scan(&mut conn, scan_id).unwrap();

        let (plans, skips) = prepare_delete_plans_with_skips(
            &conn,
            &[file_id],
            DeleteMethod::Permanent,
            DeleteAttendance::Unattended,
        )
        .unwrap();
        assert!(plans.is_empty(), "no directory to clear means no delete");
        assert_eq!(skips.len(), 1);
        assert!(
            skips[0].reason.contains("no game install"),
            "unexpected reason: {}",
            skips[0].reason
        );
        assert!(temp.path().join("leftover.bin").is_file());
    }

    #[test]
    fn interactive_preflight_still_plans_a_delete_in_an_anti_cheat_protected_game() {
        // Variant B, the owner's decision: a person ticking the box by hand
        // IS the anti-cheat consent. Proves the unattended guard did not leak
        // onto the attended path.
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("EasyAntiCheat")).unwrap();
        std::fs::write(
            temp.path().join("EasyAntiCheat").join("EasyAntiCheat.exe"),
            b"MZ",
        )
        .unwrap();
        std::fs::write(temp.path().join("one.bin"), b"one").unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        let scan_id = crate::db::begin_scan(&conn, "complete").unwrap();
        let file_id = insert_safe_finding(&conn, scan_id, temp.path(), "one.bin", "eac-game");
        crate::db::activate_scan(&mut conn, scan_id).unwrap();

        let (plans, skips) = prepare_delete_plans_with_skips(
            &conn,
            &[file_id],
            DeleteMethod::Permanent,
            DeleteAttendance::Interactive,
        )
        .unwrap();
        assert!(
            skips.is_empty(),
            "ticking the box by hand is the anti-cheat consent"
        );
        assert_eq!(plans.len(), 1);

        let outcomes = execute_delete_plans_observed(
            &mut conn,
            DeleteMethod::Permanent,
            &plans,
            |_, _, _| {},
            |_, _| {},
        )
        .unwrap();
        assert_eq!(outcomes[0].status, FsOutcome::Removed);
        assert!(!temp.path().join("one.bin").is_file());
    }
}
