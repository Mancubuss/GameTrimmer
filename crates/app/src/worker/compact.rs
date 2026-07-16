//! The "Стиснути базу даних" job: runs `gametrimmer_core::db::compact`
//! (WAL checkpoint + `VACUUM`) on a background thread and reports the
//! on-disk size before and after, so the settings dialog can show the user
//! how much space was reclaimed.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;

use gametrimmer_core::db;

use super::WorkerMsg;

/// Spawns the compact job on a new thread.
pub fn spawn_compact(db_path: PathBuf, tx: Sender<WorkerMsg>) -> JoinHandle<()> {
    std::thread::spawn(move || run_compact(&db_path, &tx))
}

fn run_compact(db_path: &Path, tx: &Sender<WorkerMsg>) {
    let before_bytes = total_db_size(db_path);

    let result = (|| -> gametrimmer_core::error::Result<()> {
        let conn = db::open(db_path)?;
        db::compact(&conn)?;
        // WAL truncation is only fully visible on disk once the connection
        // (and its WAL/SHM handles) are closed - drop it explicitly before
        // measuring the "after" size rather than relying on scope exit.
        drop(conn);
        Ok(())
    })();

    if let Err(err) = result {
        let _ = tx.send(WorkerMsg::CompactDone {
            before_bytes,
            after_bytes: before_bytes,
            error: Some(format!("Не вдалося стиснути базу даних: {err}")),
        });
        return;
    }

    let after_bytes = total_db_size(db_path);
    let _ = tx.send(WorkerMsg::CompactDone {
        before_bytes,
        after_bytes,
        error: None,
    });
}

/// Total on-disk size of the database file plus its WAL/SHM sidecar files.
/// A missing sidecar (already checkpointed away, say) counts as 0 rather
/// than failing the measurement.
fn total_db_size(db_path: &Path) -> u64 {
    file_size(db_path)
        + file_size(&sidecar_path(db_path, "-wal"))
        + file_size(&sidecar_path(db_path, "-shm"))
}

fn sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut file_name = db_path.file_name().unwrap_or_default().to_os_string();
    file_name.push(suffix);
    db_path.with_file_name(file_name)
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}
