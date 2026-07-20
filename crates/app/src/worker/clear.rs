//! The "Clear database" job: wipes every scan-produced row
//! (`gametrimmer_core::db::clear_scan_data` - `findings`, `files`, `games`,
//! `operations`; `game_libraries` and `settings` are left untouched), then
//! runs the same cheap-checkpoint + conditional-`VACUUM` flow as
//! `worker::compact`. After a full wipe the free-page fraction will normally
//! clear `compact::MIN_FREE_FRACTION` on any database that had a meaningful
//! amount of scan data in it, so the file actually shrinks. `VACUUM`
//! progress is reported back to the UI the same way `worker::compact` does
//! (see `gametrimmer_core::db::compact_observed`).

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;

use gametrimmer_core::db;

use crate::i18n::{self, Lang, Verb};

use super::compact::MIN_FREE_FRACTION;
use super::WorkerMsg;

/// Spawns the clear job on a new thread.
pub fn spawn_clear(db_path: PathBuf, tx: Sender<WorkerMsg>, lang: Lang) -> JoinHandle<()> {
    std::thread::spawn(move || run_clear(&db_path, &tx, lang))
}

fn run_clear(db_path: &Path, tx: &Sender<WorkerMsg>, lang: Lang) {
    let result = (|| -> gametrimmer_core::error::Result<()> {
        let conn = db::open(db_path)?;
        db::clear_scan_data(&conn)?;

        // Same reasoning as `worker::compact`: fold the WAL back into the
        // main file first (cheap, no rewrite) so `free_page_fraction` sees
        // an accurate freelist, then only pay for a full `VACUUM` if it's
        // actually worth it.
        db::checkpoint_truncate(&conn)?;
        let fraction = db::free_page_fraction(&conn)?;
        if fraction >= MIN_FREE_FRACTION {
            let progress_tx = tx.clone();
            db::compact_observed(&conn, move |fraction| {
                let percent = (fraction * 100.0) as usize;
                let _ = progress_tx.send(WorkerMsg::Progress {
                    verb: Verb::Clear,
                    current: percent,
                    total: 100,
                    detail: String::new(),
                });
            })?;
        }
        Ok(())
    })();

    let error = result.err().map(|err| i18n::clear_failed(lang, err));
    let _ = tx.send(WorkerMsg::ClearDone { error });
}
