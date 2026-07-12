//! Application state and the `eframe::App` entry point. Rendering itself is
//! delegated to the `ui` module; this file owns the worker channel and the
//! state transitions driven by [`WorkerMsg`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use eframe::egui;

use crate::model::{self, CategoryGroup, FindingItem};
use crate::ui;
use crate::worker::delete::DeleteItem;
use crate::worker::{self, WorkerMsg};

pub const APP_TITLE: &str = "GameTrimmer";

/// Summary shown to the user after a delete operation completes.
pub struct RemoveSummary {
    pub succeeded: usize,
    pub failed: Vec<(PathBuf, String)>,
}

pub struct GameTrimmerApp {
    db_path: Option<PathBuf>,
    pub db_status: String,

    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
    cancel: Arc<AtomicBool>,
    /// Kept only so the thread is joined on drop rather than detached;
    /// never awaited from the UI thread.
    _worker: Option<JoinHandle<()>>,

    pub busy: bool,
    pub progress: Option<(usize, usize, String)>,
    pub status_message: String,

    pub findings: Vec<FindingItem>,
    pub tree: Vec<CategoryGroup>,

    /// Indices into `findings` awaiting the user's confirmation in the
    /// delete modal.
    pub confirm_delete: Option<Vec<usize>>,
    pub remove_summary: Option<RemoveSummary>,
}

impl GameTrimmerApp {
    pub fn new() -> Self {
        let db_path = worker::db_path().ok();
        let db_status = match &db_path {
            Some(path) => match gametrimmer_core::db::open(path) {
                Ok(_) => path.display().to_string(),
                Err(err) => format!("помилка відкриття {}: {err}", path.display()),
            },
            None => "помилка визначення шляху до бази даних".to_string(),
        };

        let (tx, rx) = mpsc::channel();

        Self {
            db_path,
            db_status,
            tx,
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            _worker: None,
            busy: false,
            progress: None,
            status_message: String::new(),
            findings: Vec::new(),
            tree: Vec::new(),
            confirm_delete: None,
            remove_summary: None,
        }
    }

    pub fn start_scan(&mut self) {
        if self.busy {
            return;
        }
        let Some(db_path) = self.db_path.clone() else {
            self.status_message = "Немає шляху до бази даних.".to_string();
            return;
        };

        self.cancel.store(false, Ordering::Relaxed);
        self.busy = true;
        self.progress = None;
        self.status_message.clear();
        self.remove_summary = None;

        let handle = worker::scan::spawn_scan(db_path, Arc::clone(&self.cancel), self.tx.clone());
        self._worker = Some(handle);
    }

    pub fn cancel_scan(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Selects the currently-checked, non-removed findings and opens the
    /// confirmation modal. No-op if nothing is selected.
    pub fn request_delete_confirmation(&mut self) {
        let indices: Vec<usize> = self
            .findings
            .iter()
            .enumerate()
            .filter(|(_, item)| item.selected && !item.removed)
            .map(|(index, _)| index)
            .collect();

        if indices.is_empty() {
            return;
        }
        self.confirm_delete = Some(indices);
    }

    pub fn cancel_delete_confirmation(&mut self) {
        self.confirm_delete = None;
    }

    /// Runs after the user confirms the modal: builds full paths and hands
    /// the job to the delete worker.
    pub fn confirm_delete_now(&mut self) {
        let Some(indices) = self.confirm_delete.take() else {
            return;
        };
        let Some(db_path) = self.db_path.clone() else {
            self.status_message = "Немає шляху до бази даних.".to_string();
            return;
        };

        let items: Vec<DeleteItem> = indices
            .iter()
            .map(|&index| {
                let row = &self.findings[index].row;
                DeleteItem {
                    file_id: row.file_id,
                    full_path: row.install_dir.join(&row.rel_path),
                }
            })
            .collect();

        self.busy = true;
        self.status_message = "Видалення вибраних файлів...".to_string();
        let handle = worker::delete::spawn_delete(db_path, items, self.tx.clone());
        self._worker = Some(handle);
    }

    /// Drains every pending [`WorkerMsg`] and applies it to app state.
    /// Requests a repaint whenever something changed or a scan is still
    /// running, so progress keeps animating without user input.
    fn drain_messages(&mut self, ctx: &egui::Context) {
        let mut received_any = false;

        while let Ok(msg) = self.rx.try_recv() {
            received_any = true;
            self.apply_message(msg);
        }

        if received_any || self.busy {
            ctx.request_repaint();
        }
    }

    fn apply_message(&mut self, msg: WorkerMsg) {
        match msg {
            WorkerMsg::LibrariesFound { libraries, games } => {
                self.status_message =
                    format!("Знайдено бібліотек: {libraries}, ігор: {games}. Сканування файлів...");
            }
            WorkerMsg::Progress {
                current,
                total,
                game_name,
            } => {
                self.progress = Some((current, total, game_name));
            }
            WorkerMsg::Done { findings } => {
                self.busy = false;
                self.progress = None;
                self._worker = None;
                let count = findings.len();
                self.findings = findings
                    .into_iter()
                    .map(|row| {
                        let selected = model::default_selected(row.confidence);
                        FindingItem {
                            row,
                            selected,
                            removed: false,
                        }
                    })
                    .collect();
                self.tree = model::build_tree(&self.findings);
                self.status_message = format!("Готово. Знайдено {count} файл(ів) для перевірки.");
            }
            WorkerMsg::RemoveDone { outcomes } => {
                self.busy = false;
                self._worker = None;

                let mut succeeded = 0usize;
                let mut failed = Vec::new();
                for outcome in outcomes {
                    if let Some(item) = self
                        .findings
                        .iter_mut()
                        .find(|item| item.row.file_id == outcome.file_id)
                    {
                        match outcome.error {
                            None => {
                                item.removed = true;
                                succeeded += 1;
                            }
                            Some(err) => failed.push((outcome.path, err)),
                        }
                    }
                }

                self.tree = model::build_tree(&self.findings);
                self.status_message = format!(
                    "Видалення завершено: успішно {succeeded}, помилок {}.",
                    failed.len()
                );
                self.remove_summary = Some(RemoveSummary { succeeded, failed });
            }
            WorkerMsg::Cancelled => {
                self.busy = false;
                self.progress = None;
                self._worker = None;
                self.status_message = "Сканування скасовано.".to_string();
            }
            WorkerMsg::Error { msg } => {
                self.busy = false;
                self.progress = None;
                self._worker = None;
                self.status_message = format!("Помилка: {msg}");
            }
        }
    }
}

impl eframe::App for GameTrimmerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_messages(&ctx);

        ui::top_bar::show(self, ui);
        ui::bottom_bar::show(self, ui);
        ui::tree_view::show(self, ui);
        ui::dialogs::show(self, ui);
    }
}
