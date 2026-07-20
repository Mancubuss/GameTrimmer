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

use crate::i18n::{self, Lang, Verb};

use super::WorkerMsg;

/// Minimum reclaimable share (free pages / total pages) required before a
/// full `VACUUM` runs. `VACUUM` rewrites the whole file - a full read+write
/// of the database - so small reclaims cost more time than the space is
/// worth; the user-visible rule is "compact only when at least a quarter
/// comes back". `pub(super)` rather than private: `worker::clear` reuses it
/// after a full wipe, where the same "is it worth a VACUUM" question applies.
pub(super) const MIN_FREE_FRACTION: f64 = 0.25;

/// Spawns the compact job on a new thread.
pub fn spawn_compact(db_path: PathBuf, tx: Sender<WorkerMsg>, lang: Lang) -> JoinHandle<()> {
    std::thread::spawn(move || run_compact(&db_path, &tx, lang))
}

fn run_compact(db_path: &Path, tx: &Sender<WorkerMsg>, lang: Lang) {
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
                    verb: Verb::Compact,
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
                error: Some(i18n::compact_failed(lang, err)),
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
