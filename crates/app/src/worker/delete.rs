//! The "Видалити вибране" job: removes the given files using the method
//! chosen in settings (permanent delete by default, or the Recycle Bin)
//! and journals every attempt via `gametrimmer_core::ops`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;

use gametrimmer_core::db;
use gametrimmer_core::ops::{remove_with_log, PermanentDelete, RecycleBin, Remover};
use gametrimmer_core::settings::DeleteMethod;

use super::{RemoveOutcome, WorkerMsg};

/// One file queued for removal: its `files.id` (to match the outcome back
/// to a [`crate::model::FindingItem`]) and its full path on disk.
pub struct DeleteItem {
    pub file_id: i64,
    pub full_path: PathBuf,
}

pub fn spawn_delete(
    db_path: PathBuf,
    items: Vec<DeleteItem>,
    method: DeleteMethod,
    tx: Sender<WorkerMsg>,
) -> JoinHandle<()> {
    std::thread::spawn(move || run_delete(&db_path, items, method, &tx))
}

fn run_delete(
    db_path: &Path,
    items: Vec<DeleteItem>,
    method: DeleteMethod,
    tx: &Sender<WorkerMsg>,
) {
    let mut conn = match db::open(db_path) {
        Ok(conn) => conn,
        Err(err) => {
            let _ = tx.send(WorkerMsg::Error {
                msg: format!("Помилка відкриття бази даних: {err}"),
            });
            return;
        }
    };

    let remover: &dyn Remover = match method {
        DeleteMethod::Permanent => &PermanentDelete,
        DeleteMethod::RecycleBin => &RecycleBin,
    };

    let paths: Vec<PathBuf> = items.iter().map(|item| item.full_path.clone()).collect();

    let outcomes = match remove_with_log(&mut conn, remover, &paths) {
        Ok(outcomes) => outcomes,
        Err(err) => {
            let _ = tx.send(WorkerMsg::Error {
                msg: format!("Помилка видалення: {err}"),
            });
            return;
        }
    };

    let mapped = items
        .iter()
        .zip(outcomes)
        .map(|(item, outcome)| RemoveOutcome {
            file_id: item.file_id,
            path: outcome.path,
            error: outcome.error,
        })
        .collect();

    let _ = tx.send(WorkerMsg::RemoveDone { outcomes: mapped });
}
