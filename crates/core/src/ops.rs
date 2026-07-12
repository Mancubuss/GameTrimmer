//! Safe file removal with an operations journal.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::Connection;

use crate::error::Result;

/// Abstraction over the actual removal mechanism so tests never touch the
/// real Recycle Bin.
pub trait Remover {
    fn remove(&self, path: &Path) -> Result<()>;
}

/// Production remover: sends paths to the Windows Recycle Bin via the `trash` crate.
pub struct RecycleBin;

impl Remover for RecycleBin {
    fn remove(&self, path: &Path) -> Result<()> {
        trash::delete(path)?;
        Ok(())
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
            rusqlite::params![ts, "recycle", &src_path, None::<String>, "pending"],
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
}
