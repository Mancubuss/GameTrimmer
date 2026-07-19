//! Application state and the `eframe::App` entry point. Rendering itself is
//! delegated to the `ui` module; this file owns the worker channel and the
//! state transitions driven by [`WorkerMsg`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use eframe::egui;

use gametrimmer_core::settings::{DeleteMethod, Settings};

use crate::elevation;
use crate::export;
use crate::model::{self, DiskGroup, FindingItem};
use crate::ui;
use crate::worker::delete::DeleteItem;
use crate::worker::manual::{self, LibraryRow};
use crate::worker::{self, WorkerMsg};

pub const APP_TITLE: &str = "GameTrimmer";

/// Summary shown to the user after a delete operation completes.
pub struct RemoveSummary {
    pub succeeded: usize,
    pub failed: Vec<(PathBuf, String)>,
}

/// Progress of the currently running background operation (scan, delete, or
/// compaction), rendered by `ui::top_bar` as `"{verb} {current}/{total}:
/// {detail}"` for scan/delete, or `"{verb} {percent}%"` when `detail` is
/// empty (compaction, which has no per-item detail to show). See
/// `WorkerMsg::Progress` for the field meanings.
#[derive(Clone)]
pub struct ProgressState {
    pub verb: &'static str,
    pub current: usize,
    pub total: usize,
    pub detail: String,
}

pub struct GameTrimmerApp {
    db_path: Option<PathBuf>,
    /// Set only when the database could not be located or opened at startup;
    /// the path itself is not shown in the UI (the database always lives
    /// next to the executable).
    pub db_error: Option<String>,

    /// Persisted user settings (deletion method, ...), loaded from the
    /// database at startup and saved on every change in the settings dialog.
    pub settings: Settings,
    /// Whether the settings dialog is currently open.
    pub show_settings: bool,

    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
    cancel: Arc<AtomicBool>,
    /// Kept only so the thread is joined on drop rather than detached;
    /// never awaited from the UI thread.
    _worker: Option<JoinHandle<()>>,

    pub busy: bool,
    pub progress: Option<ProgressState>,
    pub status_message: String,
    /// Non-fatal issues surfaced during the last scan (a provider failed, a
    /// manual library's folder is currently missing, ...). Cleared at the
    /// start of every scan.
    pub warnings: Vec<String>,

    pub findings: Vec<FindingItem>,
    pub tree: Vec<DiskGroup>,
    /// Set by mid-batch `FileRemoved` messages during a delete. The tree is
    /// rebuilt at most once per frame in `drain_messages`, not once per
    /// message - running `build_tree` over thousands of findings for every
    /// single removed file would burn CPU for nothing.
    tree_dirty: bool,
    /// Explicit user expand/collapse choices for the virtualized tree view,
    /// keyed by a stable node key (see `ui::tree_view`). Absent key = the
    /// node's default (disks open, games/folders closed, categories open).
    /// Cleared whenever a fresh tree is built (`WorkerMsg::Done`) but kept
    /// across `WorkerMsg::RemoveDone` so the user's expanded state survives
    /// deletions.
    pub tree_toggles: std::collections::HashMap<String, bool>,
    /// Keyboard cursor: index into the tree view's current list of visible
    /// rows (see `ui::tree_view::build_visible_rows`). `None` until the user
    /// starts navigating with the keyboard or clicks a row. Reset whenever a
    /// fresh tree is built.
    pub tree_cursor: Option<usize>,
    /// The findings tree's scroll offset as of the last rendered frame, used
    /// to compute keyboard-driven scrolling (PgUp/PgDn, keeping the cursor
    /// in view) before the current frame's scroll area is laid out.
    pub tree_scroll_offset: f32,
    /// The findings tree viewport height as of the last rendered frame - the
    /// "page" for PgUp/PgDn.
    pub tree_viewport_height: f32,

    /// Every registered library (all vendors), for the library management
    /// list. Refreshed after every add/remove and on startup.
    pub libraries: Vec<LibraryRow>,
    /// True while the background "Додати теку..." folder-picker thread is
    /// running, so the button can't be clicked twice concurrently.
    pub folder_picker_active: bool,
    /// True while the background "Експортувати..." save-dialog thread is
    /// running, so the button can't be clicked twice concurrently.
    pub export_active: bool,

    /// Indices into `findings` awaiting the user's confirmation in the
    /// delete modal.
    pub confirm_delete: Option<Vec<usize>>,
    pub remove_summary: Option<RemoveSummary>,
    /// Set when the compaction job about to run was chained automatically
    /// after a delete (see `RemoveDone`), rather than triggered manually via
    /// the settings dialog (`start_compact`). Read (and reset) by the
    /// `CompactDone` arm to decide whether its status message should be
    /// prefixed with a "Видалення завершено." note - a manual compaction
    /// never gets that prefix.
    compact_after_delete: bool,

    /// Whether this process currently holds Administrator rights - gates
    /// the MFT index scan path (see `crate::elevation`, `worker::scan_route`).
    /// Checked once at startup; a relaunch-elevated always restarts the
    /// process, so this never needs to change while running.
    pub elevated: bool,
    /// Whether the startup modal offering a UAC relaunch (for faster MFT
    /// scanning) is currently shown. Only ever `true` at startup, and only
    /// when `!elevated`.
    pub show_elevation_prompt: bool,
}

impl GameTrimmerApp {
    pub fn new() -> Self {
        let db_path = worker::db_path().ok();
        let (db_error, settings) = match &db_path {
            Some(path) => match gametrimmer_core::db::open(path) {
                Ok(conn) => {
                    // Unreadable settings are not fatal - fall back to the
                    // defaults rather than blocking startup.
                    let settings = gametrimmer_core::settings::load(&conn).unwrap_or_default();
                    (None, settings)
                }
                Err(err) => (
                    Some(format!("Помилка відкриття бази даних: {err}")),
                    Settings::default(),
                ),
            },
            None => (
                Some("Помилка визначення шляху до бази даних.".to_string()),
                Settings::default(),
            ),
        };
        let libraries = Self::load_libraries(db_path.as_deref());
        let has_saved_findings = Self::has_saved_findings(db_path.as_deref());
        let elevated = elevation::is_elevated();

        let (tx, rx) = mpsc::channel();

        let mut app = Self {
            db_path: db_path.clone(),
            db_error,
            settings,
            show_settings: false,
            tx: tx.clone(),
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            _worker: None,
            busy: false,
            progress: None,
            status_message: String::new(),
            warnings: Vec::new(),
            findings: Vec::new(),
            tree: Vec::new(),
            tree_dirty: false,
            tree_toggles: std::collections::HashMap::new(),
            tree_cursor: None,
            tree_scroll_offset: 0.0,
            tree_viewport_height: 0.0,
            libraries,
            folder_picker_active: false,
            export_active: false,
            confirm_delete: None,
            remove_summary: None,
            compact_after_delete: false,
            elevated,
            show_elevation_prompt: !elevated,
        };

        // Show the previous scan's results immediately rather than an empty
        // screen: if the database already holds at least one `findings` row
        // (from an earlier "Сканувати бібліотеки" run), load and display it
        // right away. A missing db_path, a database that fails to open, or
        // one with no saved findings yet all fall through unchanged - the
        // ordinary empty startup screen, waiting for the user to scan.
        if has_saved_findings {
            if let Some(db_path) = db_path {
                app.busy = true;
                app.status_message =
                    "Завантаження результатів попереднього сканування...".to_string();
                app._worker = Some(worker::load::spawn_load(db_path, tx));
            }
        }

        app
    }

    /// Reads every `game_libraries` row for the library management list.
    /// Returns an empty list (rather than propagating the error into the
    /// UI) if the database can't be opened - the DB status line already
    /// reports that problem.
    fn load_libraries(db_path: Option<&std::path::Path>) -> Vec<LibraryRow> {
        let Some(db_path) = db_path else {
            return Vec::new();
        };
        let Ok(conn) = gametrimmer_core::db::open(db_path) else {
            return Vec::new();
        };
        manual::list_libraries(&conn).unwrap_or_default()
    }

    fn refresh_libraries(&mut self) {
        self.libraries = Self::load_libraries(self.db_path.as_deref());
    }

    /// Whether the database already has at least one saved `findings` row
    /// from a previous scan - decides whether `new()` auto-loads results on
    /// startup. `false` (rather than propagating an error) if the database
    /// can't be opened, matching `load_libraries`'s "DB status line already
    /// reports that problem" approach.
    fn has_saved_findings(db_path: Option<&std::path::Path>) -> bool {
        let Some(db_path) = db_path else {
            return false;
        };
        let Ok(conn) = gametrimmer_core::db::open(db_path) else {
            return false;
        };
        conn.query_row("SELECT EXISTS(SELECT 1 FROM findings LIMIT 1)", [], |row| {
            row.get::<_, bool>(0)
        })
        .unwrap_or(false)
    }

    /// Spawns the (blocking) folder-picker dialog on a background thread so
    /// the UI thread never blocks on it; the result comes back through the
    /// existing worker channel as [`WorkerMsg::FolderPicked`].
    pub fn start_add_library(&mut self) {
        if self.folder_picker_active {
            return;
        }
        self.folder_picker_active = true;

        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let path = rfd::FileDialog::new()
                .set_title("Виберіть теку бібліотеки")
                .pick_folder();
            let _ = tx.send(WorkerMsg::FolderPicked { path });
        });
    }

    /// Registers a user-picked folder as a manual library and refreshes the
    /// library list. Errors are surfaced as a warning rather than a full
    /// scan-blocking error, since the folder picker can run at any time.
    fn add_manual_library(&mut self, path: PathBuf) {
        let Some(db_path) = self.db_path.clone() else {
            self.warnings.push("Немає шляху до бази даних.".to_string());
            return;
        };

        match gametrimmer_core::db::open(&db_path) {
            Ok(conn) => {
                if let Err(err) = manual::add_manual_library(&conn, &path) {
                    self.warnings
                        .push(format!("Не вдалося додати теку {}: {err}", path.display()));
                    return;
                }
                self.refresh_libraries();
            }
            Err(err) => self
                .warnings
                .push(format!("Помилка відкриття бази даних: {err}")),
        }
    }

    /// Removes a manual library (and, cascading, its games/files/findings)
    /// and refreshes the library list.
    pub fn remove_manual_library(&mut self, library_id: i64) {
        let Some(db_path) = self.db_path.clone() else {
            self.warnings.push("Немає шляху до бази даних.".to_string());
            return;
        };

        match gametrimmer_core::db::open(&db_path) {
            Ok(mut conn) => {
                if let Err(err) = manual::remove_library(&mut conn, library_id) {
                    self.warnings
                        .push(format!("Не вдалося прибрати бібліотеку: {err}"));
                    return;
                }
                self.refresh_libraries();
            }
            Err(err) => self
                .warnings
                .push(format!("Помилка відкриття бази даних: {err}")),
        }
    }

    /// Spawns the (blocking) export save-dialog on a background thread. The
    /// CSV text is built on the UI thread first (cheap - `findings`/`tree`
    /// are already in memory, and this avoids sharing app state with the
    /// background thread); the thread then only shows the dialog and writes
    /// the prebuilt string. No-op if an export is already running or there
    /// are no findings to export.
    pub fn start_export(&mut self) {
        if self.export_active || self.findings.is_empty() {
            return;
        }
        self.export_active = true;

        let csv = export::export_csv(&self.findings, &self.tree);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title("Експорт результатів аналізу")
                .set_file_name("gametrimmer_analysis.csv")
                .add_filter("CSV", &["csv"])
                .add_filter("Текстовий файл", &["txt"])
                .save_file();

            let (path, error) = match picked {
                Some(path) => match export::write_export(&path, &csv) {
                    Ok(()) => (Some(path), None),
                    Err(err) => (None, Some(err.to_string())),
                },
                None => (None, None),
            };
            let _ = tx.send(WorkerMsg::ExportDone { path, error });
        });
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
        self.warnings.clear();
        self.remove_summary = None;

        let handle = worker::scan::spawn_scan(
            db_path,
            Arc::clone(&self.cancel),
            self.tx.clone(),
            self.elevated,
        );
        self._worker = Some(handle);
    }

    /// Attempts to relaunch the app elevated (triggers a UAC prompt). On
    /// success the current process exits immediately, handing off to the
    /// new elevated instance; on failure (user declined UAC, or the
    /// relaunch could not start) this just dismisses the modal - the
    /// session continues unelevated, never a hard failure.
    pub fn relaunch_elevated(&mut self) {
        if elevation::relaunch_elevated() {
            std::process::exit(0);
        }
        self.show_elevation_prompt = false;
    }

    pub fn continue_without_elevation(&mut self) {
        self.show_elevation_prompt = false;
    }

    pub fn cancel_scan(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Selects every non-removed finding (the "Вибрати все" action).
    pub fn select_all(&mut self) {
        for item in &mut self.findings {
            if !item.removed {
                item.selected = true;
            }
        }
    }

    /// Deselects every finding (the "Зняти вибір" action).
    pub fn deselect_all(&mut self) {
        for item in &mut self.findings {
            item.selected = false;
        }
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
        let handle = worker::delete::spawn_delete(
            db_path,
            items,
            self.settings.delete_method,
            self.tx.clone(),
        );
        self._worker = Some(handle);
    }

    /// Spawns the "Стиснути базу даних" job (WAL checkpoint + `VACUUM`).
    /// No-op while another job is running.
    pub fn start_compact(&mut self) {
        if self.busy {
            return;
        }
        let Some(db_path) = self.db_path.clone() else {
            self.status_message = "Немає шляху до бази даних.".to_string();
            return;
        };
        self.spawn_compact_job(db_path, "Стискання бази даних...".to_string());
    }

    /// Shared by `start_compact` (user-triggered) and the automatic
    /// post-delete chain in `apply_message`. Does not check `self.busy` -
    /// callers are responsible for that; the post-delete chain deliberately
    /// keeps `busy` set from the delete job straight through to compaction.
    fn spawn_compact_job(&mut self, db_path: PathBuf, status_message: String) {
        self.busy = true;
        self.status_message = status_message;
        let handle = worker::compact::spawn_compact(db_path, self.tx.clone());
        self._worker = Some(handle);
    }

    /// Applies a new deletion method and persists it immediately, so the
    /// choice survives a restart even if the dialog is closed by killing
    /// the app. A save failure keeps the in-memory choice for this session
    /// and surfaces as a warning.
    pub fn set_delete_method(&mut self, method: DeleteMethod) {
        if self.settings.delete_method == method {
            return;
        }
        self.settings = Settings {
            delete_method: method,
        };
        self.persist_settings();
    }

    fn persist_settings(&mut self) {
        let Some(db_path) = self.db_path.clone() else {
            self.warnings
                .push("Налаштування не збережено: немає шляху до бази даних.".to_string());
            return;
        };
        let result = gametrimmer_core::db::open(&db_path)
            .and_then(|conn| gametrimmer_core::settings::save(&conn, &self.settings));
        if let Err(err) = result {
            self.warnings
                .push(format!("Не вдалося зберегти налаштування: {err}"));
        }
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

        if self.tree_dirty {
            self.tree = model::build_tree(&self.findings);
            self.tree_dirty = false;
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
                verb,
                current,
                total,
                detail,
            } => {
                self.progress = Some(ProgressState {
                    verb,
                    current,
                    total,
                    detail,
                });
            }
            WorkerMsg::Done {
                findings,
                scan_summary,
            } => {
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
                // A fresh scan means a fresh tree shape - stale toggle keys
                // (folders/categories that no longer exist, or now mean
                // something else) must not leak into it, and the keyboard
                // cursor's row index no longer points at the same row.
                self.tree_toggles.clear();
                self.tree_cursor = None;
                self.status_message =
                    format!("{scan_summary} Знайдено {count} файл(ів) для перевірки.");
            }
            WorkerMsg::FileRemoved { file_id } => {
                if let Some(item) = self
                    .findings
                    .iter_mut()
                    .find(|item| item.row.file_id == file_id)
                {
                    item.removed = true;
                    self.tree_dirty = true;
                }
            }
            WorkerMsg::RemoveDone { outcomes } => {
                self._worker = None;
                // A finished delete must not leave the last file's progress
                // bar stuck on screen while the auto-compaction below runs
                // (which reports its own status via `busy` + a spinner, not
                // `progress`) - mirrors the scan `Done` arm.
                self.progress = None;

                let mut succeeded = 0usize;
                let mut failed = Vec::new();
                for outcome in outcomes {
                    // Summary counts stay keyed off `error` alone - a purged-
                    // but-failed file (path already gone from disk) still
                    // counts as a failure in the report - but it's still
                    // marked `removed` below so it disappears from the tree
                    // like any other successfully-handled file.
                    let succeeded_this_file = outcome.error.is_none();
                    if succeeded_this_file {
                        succeeded += 1;
                    }

                    if succeeded_this_file || outcome.purged {
                        if let Some(item) = self
                            .findings
                            .iter_mut()
                            .find(|item| item.row.file_id == outcome.file_id)
                        {
                            item.removed = true;
                        }
                    }

                    if !succeeded_this_file {
                        failed.push((outcome.path, outcome.error.unwrap()));
                    }
                }

                // This arm always rebuilds the full tree unconditionally, so
                // any pending mid-batch `FileRemoved` dirtiness is already
                // subsumed - clear it to avoid a redundant rebuild next frame.
                self.tree = model::build_tree(&self.findings);
                self.tree_dirty = false;
                self.status_message = format!(
                    "Видалення завершено: успішно {succeeded}, помилок {}.",
                    failed.len()
                );
                self.remove_summary = Some(RemoveSummary { succeeded, failed });

                // Rows deleted from `files`/`findings` leave free pages behind
                // in the database file; chain straight into compaction after a
                // successful delete rather than leaving that space stranded
                // until the user remembers to run it manually. `busy` stays
                // true across the chain (not routed through `start_compact`,
                // whose `busy` guard would otherwise block it here).
                if succeeded > 0 {
                    if let Some(db_path) = self.db_path.clone() {
                        self.compact_after_delete = true;
                        self.spawn_compact_job(
                            db_path,
                            "Видалення завершено. Стискання бази даних...".to_string(),
                        );
                    } else {
                        self.busy = false;
                    }
                } else {
                    self.busy = false;
                }
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
            WorkerMsg::Warning { msg } => {
                self.warnings.push(msg);
            }
            WorkerMsg::FolderPicked { path } => {
                self.folder_picker_active = false;
                if let Some(path) = path {
                    self.add_manual_library(path);
                }
            }
            WorkerMsg::ExportDone { path, error } => {
                self.export_active = false;
                if let Some(error) = error {
                    self.warnings
                        .push(format!("Не вдалося зберегти експорт: {error}"));
                } else if let Some(path) = path {
                    self.status_message = format!("Експортовано: {}", path.display());
                }
                // `path` and `error` both `None` means the user cancelled
                // the save dialog - nothing to report.
            }
            WorkerMsg::CompactDone { error, skipped } => {
                self.busy = false;
                self._worker = None;
                // Compaction now drives the progress bar (see
                // `worker::compact`) - it must not linger once the job is
                // done, whether it ran, was skipped, or failed.
                self.progress = None;
                // Only ever set by the RemoveDone arm right before chaining
                // into this compaction job - reset here unconditionally so it
                // can never leak into a later, manually-triggered compaction.
                let after_delete = std::mem::take(&mut self.compact_after_delete);
                match error {
                    Some(err) => self.status_message = err,
                    None if skipped && after_delete => {
                        self.status_message = "Видалення завершено.".to_string();
                    }
                    // Manually-triggered compaction with nothing worth doing:
                    // the hint under the settings button already explains the
                    // 25% rule, so no status message is needed here.
                    None if skipped => {
                        self.status_message.clear();
                    }
                    None => {
                        let prefix = if after_delete {
                            "Видалення завершено. "
                        } else {
                            ""
                        };
                        self.status_message = format!("{prefix}Базу даних стиснуто.");
                    }
                }
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
        ui::settings_dialog::show(self, ui);
    }
}
