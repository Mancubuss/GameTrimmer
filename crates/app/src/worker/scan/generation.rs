//! Staging-generation lifecycle and rollback guard.

use std::path::{Path, PathBuf};

use gametrimmer_core::db;
use gametrimmer_core::error::Result as CoreResult;
use rusqlite::Connection;

/// Rollback guard for a staging generation. Any early return or panic after
/// generation creation removes only that generation and preserves the active
/// snapshot.
pub(super) struct ScanGenerationGuard {
    db_path: PathBuf,
    scan_id: i64,
    terminal: bool,
}

impl ScanGenerationGuard {
    pub(super) fn new(db_path: &Path, scan_id: i64) -> Self {
        Self {
            db_path: db_path.to_path_buf(),
            scan_id,
            terminal: false,
        }
    }

    pub(super) fn abort(&mut self, conn: &mut Connection, state: &str) {
        if !self.terminal {
            if let Err(err) = db::abort_scan(conn, self.scan_id, state) {
                crate::logger::error(&format!(
                    "Failed to abort scan generation {}: {err}",
                    self.scan_id
                ));
            }
            self.terminal = true;
        }
    }

    pub(super) fn activate(mut self, conn: &mut Connection) -> CoreResult<()> {
        db::activate_scan(conn, self.scan_id)?;
        self.terminal = true;
        Ok(())
    }
}

impl Drop for ScanGenerationGuard {
    fn drop(&mut self) {
        if self.terminal {
            return;
        }
        match db::open(&self.db_path) {
            Ok(mut conn) => {
                let _ = db::abort_scan(&mut conn, self.scan_id, "failed");
            }
            Err(err) => crate::logger::error(&format!(
                "Could not reopen the database to abort scan generation {}: {err}",
                self.scan_id
            )),
        }
    }
}
