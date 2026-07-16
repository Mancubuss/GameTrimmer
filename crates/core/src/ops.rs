//! Safe file removal with an operations journal.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::Connection;

use crate::error::Result;

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
#[derive(Debug)]
pub struct OpOutcome {
    pub path: PathBuf,
    /// None = success; Some(message) = failure reason.
    pub error: Option<String>,
}

/// Removes `paths` one by one via `remover`, journaling every attempt into the
/// `operations` table (row written as `pending` before the attempt, updated to
/// `done`/`failed` after). Continues past individual failures.
pub fn remove_with_log(
    conn: &mut Connection,
    remover: &dyn Remover,
    paths: &[PathBuf],
) -> Result<Vec<OpOutcome>> {
    let mut outcomes = Vec::new();

    for path in paths {
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

        // Attempt removal
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
        outcomes.push(OpOutcome {
            path: path.clone(),
            error: remove_result.err().map(|e| e.to_string()),
        });
    }

    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashSet;

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
}
