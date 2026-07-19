//! The "Стиснути базу даних" job: cheaply folds the WAL back into the main
//! file, then estimates the reclaimable share via
//! `gametrimmer_core::db::free_page_fraction`. `VACUUM` (via
//! `gametrimmer_core::db::compact_observed`) only runs when that share
//! clears `MIN_FREE_FRACTION` - otherwise it is skipped since rewriting the
//! whole file to reclaim a sliver of space costs more time than it's worth.
//! While `VACUUM` runs, progress is reported back to the UI as a
//! `WorkerMsg::Progress` percentage (see `gametrimmer_core::db::compact_observed`
//! for how that percentage is estimated).

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;

use gametrimmer_core::db;

use super::WorkerMsg;

/// Minimum reclaimable share (free pages / total pages) required before a
/// full `VACUUM` runs. `VACUUM` rewrites the whole file - a full read+write
/// of the database - so small reclaims cost more time than the space is
/// worth; the user-visible rule is "compact only when at least a quarter
/// comes back".
const MIN_FREE_FRACTION: f64 = 0.25;

/// Ukrainian verb shown in the progress bar while `VACUUM` runs.
const COMPACT_VERB: &str = "Стискання бази даних";

/// Spawns the compact job on a new thread.
pub fn spawn_compact(db_path: PathBuf, tx: Sender<WorkerMsg>) -> JoinHandle<()> {
    std::thread::spawn(move || run_compact(&db_path, &tx))
}

fn run_compact(db_path: &Path, tx: &Sender<WorkerMsg>) {
    let result = (|| -> gametrimmer_core::error::Result<bool> {
        let conn = db::open(db_path)?;
        // Cheap regardless of whether VACUUM ends up running: folds the WAL
        // back into the main file so `free_page_fraction` sees an accurate
        // freelist.
        db::checkpoint_truncate(&conn)?;
        let fraction = db::free_page_fraction(&conn)?;
        let skipped = fraction < MIN_FREE_FRACTION;
        if !skipped {
            let progress_tx = tx.clone();
            db::compact_observed(&conn, move |fraction| {
                let percent = (fraction * 100.0) as usize;
                let _ = progress_tx.send(WorkerMsg::Progress {
                    verb: COMPACT_VERB,
                    current: percent,
                    total: 100,
                    detail: String::new(),
                });
            })?;
        }
        Ok(skipped)
    })();

    let skipped = match result {
        Ok(skipped) => skipped,
        Err(err) => {
            let _ = tx.send(WorkerMsg::CompactDone {
                error: Some(format!("Не вдалося стиснути базу даних: {err}")),
                skipped: false,
            });
            return;
        }
    };

    let _ = tx.send(WorkerMsg::CompactDone {
        error: None,
        skipped,
    });
}
