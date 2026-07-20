//! The "Clear database" job: wipes every scan-produced row
//! (`gametrimmer_core::db::clear_scan_data` - `findings`, `files`, `games`,
//! `operations`; `game_libraries` and `settings` are left untouched), then
//! runs the same cheap-checkpoint + conditional-`VACUUM` flow as
//! `worker::compact`. After a full wipe the free-page fraction will normally
//! clear `compact::MIN_FREE_FRACTION` on any database that had a meaningful
//! amount of scan data in it, so the file actually shrinks. `VACUUM`
//! progress is reported back to the UI the same way `worker::compact` does
//! (see `gametrimmer_core::db::compact_observed`).
//!
//! If the operation fails with a genuine SQLite corruption error (see
//! `gametrimmer_core::db::is_corruption_error`) - e.g. a WAL left
//! uncheckpointed after a prior scan put the database into an unreadable
//! state - there is no SQL fix left to try, so this falls back to
//! `gametrimmer_core::db::rebuild_database`: a destructive rebuild that
//! keeps `game_libraries`/`settings` and drops the (already corrupt, and
//! already being thrown away by "Clear database") scan data. This is the
//! safety net so the user is never dead-ended by "malformed" errors on
//! what's supposed to be the "reset and start clean" button.

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

    // A non-corruption error keeps the original behavior unchanged (below).
    // A corruption error gets one recovery attempt: rebuilding the database
    // still achieves "Clear database"'s goal (empty scan data, libraries and
    // settings kept), so that counts as success from the user's point of
    // view - just with a warning explaining what happened. If the rebuild
    // itself fails there is nothing left to try, so it falls through to the
    // same `clear_failed` message the original error would have produced.
    let error = match result {
        Ok(()) => None,
        Err(err) if db::is_corruption_error(&err) => match db::rebuild_database(db_path) {
            Ok(_conn) => {
                let _ = tx.send(WorkerMsg::Warning {
                    msg: i18n::clear_rebuilt_after_corruption(lang),
                });
                None
            }
            Err(rebuild_err) => {
                // The rebuild itself failed (disk full, permission, ...): the
                // temp-then-rename in `rebuild_database` leaves the original
                // file untouched on failure, so nothing was lost - but surface
                // the real cause in the log rather than discarding it silently.
                eprintln!("Не вдалося перебудувати пошкоджену базу даних: {rebuild_err}");
                Some(i18n::clear_failed(lang, err))
            }
        },
        Err(err) => Some(i18n::clear_failed(lang, err)),
    };
    let _ = tx.send(WorkerMsg::ClearDone { error });
}
