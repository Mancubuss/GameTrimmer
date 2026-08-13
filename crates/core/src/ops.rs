//! Safe file removal with an operations journal.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::Connection;

use crate::error::Result;
use crate::safety::{
    current_identity, normalize_relative_path, validate_delete_plan, DeleteBlockReason, DeletePlan,
    FileIdentity, SafetySnapshot, TargetKind,
};

/// Abstraction over the actual removal mechanism so tests never touch the
/// real Recycle Bin or filesystem.
pub trait Remover {
    fn remove(&self, path: &Path) -> Result<()>;
    /// Stable action name journaled into the `operations` table.
    fn action(&self) -> &'static str;
}

/// Recoverable remover: sends paths to the Windows Recycle Bin via the
/// `trash` crate. Slower than [`PermanentDelete`] (each file goes through
/// the shell), but recoverable.
pub struct RecycleBin;

impl Remover for RecycleBin {
    fn remove(&self, path: &Path) -> Result<()> {
        trash::delete(path)?;
        Ok(())
    }

    fn action(&self) -> &'static str {
        "recycle"
    }
}

/// Fast remover: deletes files/directories permanently via `std::fs`, with
/// no way to recover. The default for game libraries - anything removed by
/// mistake can always be re-downloaded from the store.
pub struct PermanentDelete;

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
            "SELECT id, trusted_root, rel_path, expected_identity
             FROM operations WHERE status = 'pending' ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let mut reconciled = Vec::with_capacity(pending.len());
    for (operation_id, root, rel_path, expected_identity) in pending {
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
                    Ok(Some(("not_applied", None)))
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

/// Builds immutable delete plans only from the active, non-legacy generation.
/// Every selected row must pass; callers never receive a partial batch.
pub fn prepare_delete_plans(
    conn: &Connection,
    file_ids: &[i64],
    action: &str,
) -> Result<Vec<DeletePlan>> {
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
    for file_id in file_ids {
        let row = conn.query_row(
            "SELECT f.scan_id, f.game_id, fs.trusted_root, fs.rel_path,
                    fs.root_identity, fs.target_identity, fs.target_kind,
                    fs.tree_fingerprint, fs.block_reason, sle.status
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
        let evidence_allows_deletion = matches!(
            (game_id.is_some(), evidence_status),
            (_, "complete") | (true, "heuristic")
        );
        if !evidence_allows_deletion {
            return Err(crate::error::CoreError::Other(format!(
                "delete preflight blocked file_id {file_id}: {}",
                DeleteBlockReason::DegradedDiscovery
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
        plans.push(DeletePlan {
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
        });
    }
    Ok(plans)
}

/// Executes a preflighted batch. The whole batch is validated before the first
/// mutation, then each row and live identity is checked again immediately
/// before its own operation.
pub fn execute_delete_plans_observed(
    conn: &mut Connection,
    remover: &dyn Remover,
    plans: &[DeletePlan],
    mut on_progress: impl FnMut(usize, usize, &Path),
    mut on_outcome: impl FnMut(usize, &OpOutcome),
) -> Result<Vec<OpOutcome>> {
    conn.pragma_update(None, "synchronous", "FULL")?;

    let ids: Vec<i64> = plans.iter().map(|plan| plan.file_id).collect();
    let current = prepare_delete_plans(conn, &ids, remover.action())?;
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

        let refreshed = prepare_delete_plans(conn, &[plan.file_id], remover.action());
        if refreshed.as_ref().ok().and_then(|rows| rows.first()) != Some(plan) {
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
              trusted_root, rel_path, expected_identity)
             VALUES (?1, ?2, ?3, NULL, 'pending', ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                ts,
                remover.action(),
                nominal_path.to_string_lossy(),
                plan.scan_id,
                plan.file_id,
                plan.snapshot.trusted_root.to_string_lossy(),
                plan.snapshot.rel_path.to_string_lossy(),
                plan.snapshot.target_identity.encode(),
            ],
        )?;
        let operation_id = conn.last_insert_rowid();

        // Read the file's hard-link count while it still exists: afterwards
        // there is no way to tell whether this path was the last name for its
        // allocation or one of several. Costs one open per file, on a bounded,
        // user-initiated batch - never on a scan.
        let mut share = None;
        let (status, error) = match validate_delete_plan(plan) {
            Ok(path) => {
                share = crate::hardlink::file_share(&path);
                match remover.remove(&path) {
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
            "INSERT INTO games (scan_id, library_id, name, install_dir, app_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
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
        let plans = prepare_delete_plans(&conn, &[file_id], "delete").unwrap();
        std::fs::remove_file(temp.path().join("gone.bin")).unwrap();
        let outcomes = execute_delete_plans_observed(
            &mut conn,
            &PermanentDelete,
            &plans,
            |_, _, _| {},
            |_, _| {},
        )
        .unwrap();
        assert_eq!(outcomes[0].status, FsOutcome::AlreadyAbsent);
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
        let plans = prepare_delete_plans(&conn, &[file_id], "delete").unwrap();

        // The volume goes away between planning and execution.
        std::fs::remove_dir_all(&root).unwrap();

        let result = execute_delete_plans_observed(
            &mut conn,
            &PermanentDelete,
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
        let plans = prepare_delete_plans(&conn, &[safe, swapped], "delete").unwrap();
        std::fs::remove_file(temp.path().join("swapped.bin")).unwrap();
        std::fs::write(temp.path().join("swapped.bin"), b"replacement").unwrap();
        assert!(execute_delete_plans_observed(
            &mut conn,
            &PermanentDelete,
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

        let error = prepare_delete_plans(&conn, &[file_id], "delete").unwrap_err();
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

        let error = prepare_delete_plans(&conn, &[file_id, file_id], "delete").unwrap_err();
        assert!(error.to_string().contains("duplicate file id"));
        assert!(temp.path().join("target.bin").is_file());
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
            assert!(prepare_delete_plans(&conn, &[file_id], "delete").is_err());
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
             VALUES (?1, 'orphan', 90)",
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

        assert!(prepare_delete_plans(&conn, &[file_id], "delete").is_err());
        crate::db::record_scan_library_evidence(&conn, scan_id, temp.path(), "test", "complete")
            .unwrap();
        assert!(prepare_delete_plans(&conn, &[file_id], "delete").is_ok());
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
}
