//! Cleanup Execution Engine.
//!
//! Handles execution of cleanup actions:
//! - Direct file deletion (permanent or via Windows Recycle Bin)
//! - Fail-closed blocking of archive mutations until full-file rollback exists

use std::path::PathBuf;

use rusqlite::{params, Connection};

use crate::error::Result;
use crate::models::FindingAction;
use crate::ops::{prepare_delete_plans, PermanentDelete, RecycleBin, Remover};
use crate::safety::DeletePlan;
use crate::settings::DeleteMethod;

/// Target for cleanup execution.
#[derive(Debug, Clone)]
pub struct CleanupTarget {
    pub file_id: i64,
    pub path: PathBuf,
    pub game_dir: PathBuf,
    pub action: FindingAction,
}

/// Options controlling cleanup execution.
#[derive(Debug, Clone)]
pub struct CleanupOptions {
    pub delete_method: DeleteMethod,
    pub force_unsafe: bool,
    pub create_snapshots: bool,
    pub custom_backup_dir: Option<PathBuf>,
}

impl Default for CleanupOptions {
    fn default() -> Self {
        Self {
            delete_method: DeleteMethod::Permanent,
            force_unsafe: false,
            create_snapshots: true,
            custom_backup_dir: None,
        }
    }
}

/// Result of executing a cleanup action on a single target.
#[derive(Debug, Clone)]
pub struct CleanupResult {
    pub file_id: i64,
    pub path: PathBuf,
    pub success: bool,
    pub bytes_freed: u64,
    pub action_taken: String,
    pub snapshot_path: Option<PathBuf>,
    pub error: Option<String>,
}

/// Summary report of a batch cleanup operation.
#[derive(Debug, Default, Clone)]
pub struct BatchCleanupReport {
    pub results: Vec<CleanupResult>,
    pub total_bytes_freed: u64,
    pub files_processed: usize,
    pub files_succeeded: usize,
    pub files_failed: usize,
}

/// Executes cleanup on a list of targets.
pub fn execute_cleanup(
    conn: Option<&Connection>,
    targets: &[CleanupTarget],
    options: &CleanupOptions,
) -> BatchCleanupReport {
    if targets.is_empty() {
        return BatchCleanupReport::default();
    }
    let (conn, plans) = match preflight_direct_cleanup_batch(conn, targets, options) {
        Ok(preflight) => preflight,
        Err(error) => {
            return BatchCleanupReport {
                results: targets
                    .iter()
                    .map(|target| blocked_target(target, options, &error))
                    .collect(),
                total_bytes_freed: 0,
                files_processed: targets.len(),
                files_succeeded: 0,
                files_failed: targets.len(),
            };
        }
    };

    let mut report = BatchCleanupReport::default();
    report.results.reserve(targets.len());
    for (target, plan) in targets.iter().zip(&plans) {
        report.files_processed += 1;
        let res = execute_preflighted_direct_delete(conn, target, plan, options);
        if res.success {
            report.files_succeeded += 1;
            report.total_bytes_freed += res.bytes_freed;
        } else {
            report.files_failed += 1;
        }
        report.results.push(res);
    }

    report
}

/// Executes cleanup for one target.
pub fn execute_single(
    conn: Option<&Connection>,
    target: &CleanupTarget,
    options: &CleanupOptions,
) -> CleanupResult {
    match &target.action {
        FindingAction::DirectDelete => {
            match preflight_direct_cleanup_batch(conn, std::slice::from_ref(target), options) {
                Ok((conn, plans)) => {
                    execute_preflighted_direct_delete(conn, target, &plans[0], options)
                }
                Err(error) => blocked_direct_delete(target, options, &error),
            }
        }
        FindingAction::SparseZero { format, .. } => blocked_archive_action(
            target,
            &format!("sparse_zero_{format}"),
            "Sparse zeroing is disabled: a header snapshot cannot restore modified payload ranges",
        ),
        FindingAction::Repack { format, .. } => blocked_archive_action(
            target,
            &format!("repack_{format}"),
            "Repacking is disabled until a verified atomic replacement and full rollback path is available",
        ),
    }
}

fn blocked_target(
    target: &CleanupTarget,
    options: &CleanupOptions,
    batch_error: &str,
) -> CleanupResult {
    match &target.action {
        FindingAction::DirectDelete => blocked_direct_delete(target, options, batch_error),
        FindingAction::SparseZero { format, .. } => {
            blocked_archive_action(target, &format!("sparse_zero_{format}"), batch_error)
        }
        FindingAction::Repack { format, .. } => {
            blocked_archive_action(target, &format!("repack_{format}"), batch_error)
        }
    }
}

fn blocked_archive_action(target: &CleanupTarget, action: &str, reason: &str) -> CleanupResult {
    CleanupResult {
        file_id: target.file_id,
        path: target.path.clone(),
        success: false,
        bytes_freed: 0,
        action_taken: action.to_string(),
        snapshot_path: None,
        error: Some(reason.to_string()),
    }
}

fn preflight_direct_cleanup_batch<'a>(
    conn: Option<&'a Connection>,
    targets: &[CleanupTarget],
    options: &CleanupOptions,
) -> std::result::Result<(&'a Connection, Vec<DeletePlan>), String> {
    if let Some(target) = targets
        .iter()
        .find(|target| target.action != FindingAction::DirectDelete)
    {
        return Err(format!(
            "batch contains non-delete archive action for file_id {}; no files were changed",
            target.file_id
        ));
    }
    let conn = conn.ok_or_else(|| {
        "Direct deletion requires active scan-time path and identity evidence".to_string()
    })?;
    let file_ids: Vec<i64> = targets.iter().map(|target| target.file_id).collect();
    let plans = prepare_delete_plans(conn, &file_ids, options.delete_method)
        .map_err(|error| error.to_string())?;
    if plans.len() != targets.len() {
        return Err("delete preflight returned an incomplete batch".to_string());
    }
    for (target, plan) in targets.iter().zip(&plans) {
        crate::safety::validate_delete_plan(plan).map_err(|error| error.to_string())?;
        let requested_path = std::fs::canonicalize(&target.path)
            .map_err(|error| format!("target path could not be verified: {error}"))?;
        let planned_path = plan.snapshot.trusted_root.join(&plan.snapshot.rel_path);
        let planned_path = std::fs::canonicalize(&planned_path)
            .map_err(|error| format!("planned path could not be verified: {error}"))?;
        if requested_path != planned_path {
            return Err("requested path does not match the scan-time safety path".to_string());
        }
    }
    Ok((conn, plans))
}

fn execute_preflighted_direct_delete(
    conn: &Connection,
    target: &CleanupTarget,
    plan: &DeletePlan,
    options: &CleanupOptions,
) -> CleanupResult {
    let remover: &dyn Remover = match options.delete_method {
        DeleteMethod::Permanent => &PermanentDelete,
        DeleteMethod::RecycleBin => &RecycleBin,
    };
    let initial_size = plan.snapshot.target_identity.size;

    // The same identity snapshot that carries the expected size is verified
    // again through a held handle immediately before removal. Permanent
    // deletion acts on that handle; recycle-bin removal has to fall back to
    // the shell's path API, but only after the same fail-closed preflight.
    let res: Result<()> = match crate::safety::open_verified_for_delete(plan) {
        Ok(verified) => remover.remove_target(verified),
        Err(err) => Err(crate::error::CoreError::Other(err.to_string())),
    };

    match res {
        Ok(()) => {
            let _ = conn.execute(
                "DELETE FROM findings WHERE file_id = ?1",
                params![target.file_id],
            );
            CleanupResult {
                file_id: target.file_id,
                path: target.path.clone(),
                success: true,
                bytes_freed: initial_size,
                action_taken: options.delete_method.as_str().to_string(),
                snapshot_path: None,
                error: None,
            }
        }
        Err(e) => CleanupResult {
            file_id: target.file_id,
            path: target.path.clone(),
            success: false,
            bytes_freed: 0,
            action_taken: options.delete_method.as_str().to_string(),
            snapshot_path: None,
            error: Some(e.to_string()),
        },
    }
}

fn blocked_direct_delete(
    target: &CleanupTarget,
    options: &CleanupOptions,
    reason: &str,
) -> CleanupResult {
    CleanupResult {
        file_id: target.file_id,
        path: target.path.clone(),
        success: false,
        bytes_freed: 0,
        action_taken: options.delete_method.as_str().to_string(),
        snapshot_path: None,
        error: Some(reason.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn database_with_direct_finding(root: &std::path::Path, rel_path: &str) -> (Connection, i64) {
        let mut conn = crate::db::open_in_memory().expect("open database");
        let scan_id = crate::db::begin_scan(&conn, "complete").expect("begin scan");
        conn.execute(
            "INSERT INTO game_libraries (vendor, path) VALUES ('steam', ?1)",
            [root.to_string_lossy()],
        )
        .expect("insert library");
        let library_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO games (scan_id, library_id, app_id, name, install_dir) \
             VALUES (?1, ?2, 'app', 'Test Game', ?3)",
            rusqlite::params![scan_id, library_id, root.to_string_lossy()],
        )
        .expect("insert game");
        let game_id = conn.last_insert_rowid();
        let file_path = root.join(rel_path);
        let size = fs::metadata(&file_path).expect("file metadata").len();
        conn.execute(
            "INSERT INTO files (scan_id, game_id, rel_path, size) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![scan_id, game_id, rel_path, size as i64],
        )
        .expect("insert file");
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO findings (file_id, category, confidence) VALUES (?1, 'docs_file', 90)",
            [file_id],
        )
        .expect("insert finding");
        let snapshot = crate::safety::capture_safety_snapshot(root, rel_path).expect("snapshot");
        conn.execute(
            "INSERT INTO file_safety \
             (file_id, scan_id, trusted_root, rel_path, root_identity, target_identity, \
              target_kind, tree_fingerprint) \
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
        .expect("insert safety evidence");
        crate::db::record_scan_library_evidence(&conn, scan_id, root, "test", "complete")
            .expect("record library evidence");
        crate::db::activate_scan(&mut conn, scan_id).expect("activate scan");
        (conn, file_id)
    }

    #[test]
    fn direct_delete_without_scan_identity_is_blocked() {
        let dir = tempdir().expect("tempdir");
        let file_path = dir.path().join("redist.exe");
        fs::write(&file_path, b"test content").expect("write");

        let target = CleanupTarget {
            file_id: 1,
            path: file_path.clone(),
            game_dir: dir.path().to_path_buf(),
            action: FindingAction::DirectDelete,
        };

        let options = CleanupOptions {
            delete_method: DeleteMethod::Permanent,
            ..Default::default()
        };

        let res = execute_single(None, &target, &options);
        assert!(!res.success);
        assert!(file_path.exists(), "unverified target must not be deleted");
        assert_eq!(res.bytes_freed, 0);
    }

    #[test]
    fn verified_direct_delete_preserves_the_existing_direct_delete_semantics() {
        let dir = tempdir().expect("tempdir");
        let file_path = dir.path().join("redist.exe");
        fs::write(&file_path, b"test content").expect("write");
        let (conn, file_id) = database_with_direct_finding(dir.path(), "redist.exe");
        let target = CleanupTarget {
            file_id,
            path: file_path.clone(),
            game_dir: dir.path().to_path_buf(),
            action: FindingAction::DirectDelete,
        };

        let result = execute_single(Some(&conn), &target, &CleanupOptions::default());
        assert!(result.success, "{:?}", result.error);
        assert!(!file_path.exists());
        assert_eq!(result.bytes_freed, 12);
    }

    #[test]
    fn mixed_cleanup_batch_is_fully_preflighted_before_any_delete() {
        let dir = tempdir().expect("tempdir");
        let direct_path = dir.path().join("redist.exe");
        let archive_path = dir.path().join("voices.pck");
        fs::write(&direct_path, b"test content").expect("write direct");
        fs::write(&archive_path, b"archive content").expect("write archive");
        let (conn, direct_id) = database_with_direct_finding(dir.path(), "redist.exe");
        let targets = [
            CleanupTarget {
                file_id: direct_id,
                path: direct_path.clone(),
                game_dir: dir.path().to_path_buf(),
                action: FindingAction::DirectDelete,
            },
            CleanupTarget {
                file_id: direct_id + 1,
                path: archive_path.clone(),
                game_dir: dir.path().to_path_buf(),
                action: FindingAction::SparseZero {
                    format: "Wwise".to_string(),
                    languages: vec!["fr".to_string()],
                    stream_count: 1,
                    offsets: vec![(0, 4096)],
                    streams: vec![],
                    estimated_savings: 4096,
                },
            },
        ];

        let report = execute_cleanup(Some(&conn), &targets, &CleanupOptions::default());
        assert_eq!(report.files_succeeded, 0);
        assert_eq!(report.files_failed, 2);
        assert!(direct_path.exists(), "first target must not be deleted");
        assert!(archive_path.exists(), "archive target must remain intact");
    }

    #[test]
    fn cleanup_cannot_downgrade_monolithic_null_contract_to_direct_delete() {
        let dir = tempdir().expect("tempdir");
        let archive_path = dir.path().join("legacy.pck");
        fs::write(&archive_path, b"archive content").expect("write archive");
        let (conn, file_id) = database_with_direct_finding(dir.path(), "legacy.pck");
        conn.execute(
            "UPDATE findings SET category = 'monolithic_archive', action = NULL \
             WHERE file_id = ?1",
            [file_id],
        )
        .expect("corrupt legacy contract");
        let target = CleanupTarget {
            file_id,
            path: archive_path.clone(),
            game_dir: dir.path().to_path_buf(),
            action: FindingAction::DirectDelete,
        };

        let result = execute_single(Some(&conn), &target, &CleanupOptions::default());
        assert!(!result.success);
        assert!(
            archive_path.exists(),
            "monolithic container must not be deleted"
        );
    }

    #[test]
    fn sparse_zero_is_blocked_even_when_force_unsafe_is_requested() {
        let dir = tempdir().expect("tempdir");
        let eac_dir = dir.path().join("EasyAntiCheat");
        fs::create_dir_all(&eac_dir).expect("mkdir");
        fs::write(eac_dir.join("easyanticheat_x64.dll"), b"fake dll").expect("write dll");

        let archive_path = dir.path().join("voices.pck");
        fs::write(&archive_path, vec![0x41; 65536]).expect("write archive");

        let target = CleanupTarget {
            file_id: 2,
            path: archive_path.clone(),
            game_dir: dir.path().to_path_buf(),
            action: FindingAction::SparseZero {
                format: "Wwise".to_string(),
                languages: vec!["french".to_string()],
                stream_count: 1,
                offsets: vec![(4096, 8192)],
                streams: vec![],
                estimated_savings: 8192,
            },
        };

        let options = CleanupOptions {
            force_unsafe: true,
            ..Default::default()
        };

        let res = execute_single(None, &target, &options);
        assert!(!res.success);
        assert!(res.error.unwrap().contains("disabled"));
        assert!(archive_path.exists());
    }

    #[test]
    fn sparse_zero_never_modifies_the_container() {
        let dir = tempdir().expect("tempdir");
        let archive_path = dir.path().join("game.pak");
        // Create 64 KiB buffer
        fs::write(&archive_path, vec![0xAB; 65536]).expect("write");

        let target = CleanupTarget {
            file_id: 3,
            path: archive_path.clone(),
            game_dir: dir.path().to_path_buf(),
            action: FindingAction::SparseZero {
                format: "Unreal".to_string(),
                languages: vec!["german".to_string()],
                stream_count: 1,
                offsets: vec![(4096, 4096)],
                streams: vec![],
                estimated_savings: 4096,
            },
        };

        let options = CleanupOptions {
            force_unsafe: true,
            create_snapshots: true,
            ..Default::default()
        };

        let before = fs::read(&archive_path).expect("read before");
        let res = execute_single(None, &target, &options);
        assert!(!res.success);
        assert_eq!(res.bytes_freed, 0);
        assert!(res.snapshot_path.is_none());
        assert_eq!(fs::read(&archive_path).expect("read after"), before);
    }
}
