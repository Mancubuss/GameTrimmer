//! Application state and the `eframe::App` entry point. Rendering itself is
//! delegated to the `ui` module; this file owns the worker channel and the
//! state transitions driven by [`WorkerMsg`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use eframe::egui;

use gametrimmer_core::settings::{DeleteMethod, Lang, ScanRouting, Settings, Theme};

use crate::elevation;
use crate::export;
use crate::i18n;
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
    pub verb: i18n::Verb,
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
    /// True while a background rules export/import thread (its file dialog
    /// plus the file work) is running - guards both settings-dialog rule
    /// buttons at once, since they write/read the same pack files.
    pub rules_io_active: bool,
    /// Outcome of the last rules export/import, shown inside the settings
    /// dialog (the top-bar status line is hidden behind the modal, so the
    /// result must be visible right where the buttons are): `Ok` carries
    /// the success summary, `Err` the failure text. Cleared when a new
    /// rules operation starts and when the dialog closes.
    pub rules_io_result: Option<Result<String, String>>,

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
        // Settings (and thus the UI language) aren't known until the
        // database opens - startup errors before that point use the default
        // language's text, same as any other place with no settings yet.
        let startup_lang = Settings::default().app_language;
        let (db_error, settings) = match &db_path {
            Some(path) => match gametrimmer_core::db::open(path) {
                Ok(conn) => {
                    // Unreadable settings are not fatal - fall back to the
                    // defaults rather than blocking startup.
                    let settings = gametrimmer_core::settings::load(&conn).unwrap_or_default();
                    (None, settings)
                }
                Err(err) => (
                    Some(i18n::db_open_error_long(startup_lang, err)),
                    Settings::default(),
                ),
            },
            None => (
                Some(i18n::strings(startup_lang).db_path_error.to_string()),
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
            rules_io_active: false,
            rules_io_result: None,
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
                app.status_message = i18n::strings(app.lang()).loading_previous_scan.to_string();
                app._worker = Some(worker::load::spawn_load(db_path, tx, app.lang()));
            }
        }

        app
    }

    /// The active UI language, read from persisted settings. Render code and
    /// worker spawns call this each frame/action rather than caching it, so
    /// a language switch (see `set_language`) takes effect immediately.
    pub fn lang(&self) -> Lang {
        self.settings.app_language
    }

    /// Applies a new UI language and persists it immediately, mirroring
    /// `set_delete_method`. Called from the settings dialog's language
    /// selector; takes effect the same frame since every render call reads
    /// `self.lang()` fresh rather than caching it.
    pub fn set_language(&mut self, lang: Lang) {
        if self.settings.app_language == lang {
            return;
        }
        self.settings = Settings {
            app_language: lang,
            ..self.settings.clone()
        };
        self.persist_settings();
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

        let title = i18n::strings(self.lang()).add_library_dialog_title;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let path = rfd::FileDialog::new().set_title(title).pick_folder();
            let _ = tx.send(WorkerMsg::FolderPicked { path });
        });
    }

    /// Registers a user-picked folder as a manual library and refreshes the
    /// library list. Errors are surfaced as a warning rather than a full
    /// scan-blocking error, since the folder picker can run at any time.
    fn add_manual_library(&mut self, path: PathBuf) {
        let lang = self.lang();
        let Some(db_path) = self.db_path.clone() else {
            self.warnings
                .push(i18n::strings(lang).no_db_path.to_string());
            return;
        };

        match gametrimmer_core::db::open(&db_path) {
            Ok(conn) => {
                if let Err(err) = manual::add_manual_library(&conn, &path) {
                    self.warnings
                        .push(i18n::add_library_failed(lang, path.display(), err));
                    return;
                }
                self.refresh_libraries();
            }
            Err(err) => self.warnings.push(i18n::db_open_error_short(lang, err)),
        }
    }

    /// Removes a manual library (and, cascading, its games/files/findings)
    /// and refreshes the library list.
    pub fn remove_manual_library(&mut self, library_id: i64) {
        let lang = self.lang();
        let Some(db_path) = self.db_path.clone() else {
            self.warnings
                .push(i18n::strings(lang).no_db_path.to_string());
            return;
        };

        match gametrimmer_core::db::open(&db_path) {
            Ok(mut conn) => {
                if let Err(err) = manual::remove_library(&mut conn, library_id) {
                    self.warnings.push(i18n::remove_library_failed(lang, err));
                    return;
                }
                self.refresh_libraries();
            }
            Err(err) => self.warnings.push(i18n::db_open_error_short(lang, err)),
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

        let lang = self.lang();
        let s = i18n::strings(lang);
        let csv = export::export_csv(lang, &self.findings, &self.tree);
        let (title, text_filter_label) = (s.export_dialog_title, s.text_file_filter_label);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title(title)
                .set_file_name("gametrimmer_analysis.csv")
                .add_filter("CSV", &["csv"])
                .add_filter(text_filter_label, &["txt"])
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

    /// Spawns the «Export rules» flow: a blocking folder picker on
    /// a background thread, then writing both pack files there (see
    /// `worker::rules_io::export_packs_to`). Result comes back as
    /// [`WorkerMsg::RulesExportDone`].
    pub fn start_rules_export(&mut self) {
        if self.rules_io_active {
            return;
        }
        self.rules_io_active = true;
        self.rules_io_result = None;

        let lang = self.lang();
        let title = i18n::strings(lang).rules_export_dialog_title;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new().set_title(title).pick_folder();

            let (path, error) = match picked {
                Some(dir) => match worker::rules_io::export_packs_to(lang, &dir) {
                    Ok(()) => (Some(dir), None),
                    Err(err) => (None, Some(err)),
                },
                None => (None, None),
            };
            let _ = tx.send(WorkerMsg::RulesExportDone { path, error });
        });
    }

    /// Spawns the «Import rules» flow: a blocking multi-file picker
    /// on a background thread, then merging each picked pack into the
    /// current files (see `worker::rules_io::import_pack_files`). Result
    /// comes back as [`WorkerMsg::RulesImportDone`]. The settings dialog
    /// additionally disables the button while a scan runs, since the import
    /// rewrites the files a scan reads at startup.
    pub fn start_rules_import(&mut self) {
        if self.rules_io_active {
            return;
        }
        self.rules_io_active = true;
        self.rules_io_result = None;

        let lang = self.lang();
        let s = i18n::strings(lang);
        let (title, filter_label) = (s.rules_import_dialog_title, s.rules_import_filter_label);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title(title)
                .add_filter(filter_label, &["json"])
                .pick_files();

            let (summary, error) = match picked {
                Some(files) => match worker::rules_io::import_pack_files(lang, &files) {
                    Ok(summary) => (Some(summary), None),
                    Err(err) => (None, Some(err)),
                },
                None => (None, None),
            };
            let _ = tx.send(WorkerMsg::RulesImportDone { summary, error });
        });
    }

    pub fn start_scan(&mut self) {
        if self.busy {
            return;
        }
        let Some(db_path) = self.db_path.clone() else {
            self.status_message = i18n::strings(self.lang()).no_db_path.to_string();
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
            worker::scan::ScanOptions {
                lang: self.lang(),
                keep_languages: self.settings.keep_languages.clone(),
                scan_routing: self.settings.scan_routing,
                enabled_categories: self.settings.enabled_categories.clone(),
            },
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
            self.status_message = i18n::strings(self.lang()).no_db_path.to_string();
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
        self.status_message = i18n::strings(self.lang())
            .deleting_selected_files
            .to_string();
        let handle = worker::delete::spawn_delete(
            db_path,
            items,
            self.settings.delete_method,
            self.tx.clone(),
            self.lang(),
        );
        self._worker = Some(handle);
    }

    /// Spawns the "Compact database" job (WAL checkpoint + `VACUUM`).
    /// No-op while another job is running.
    pub fn start_compact(&mut self) {
        if self.busy {
            return;
        }
        let Some(db_path) = self.db_path.clone() else {
            self.status_message = i18n::strings(self.lang()).no_db_path.to_string();
            return;
        };
        let status = i18n::strings(self.lang()).compacting_database.to_string();
        self.spawn_compact_job(db_path, status);
    }

    /// Shared by `start_compact` (user-triggered) and the automatic
    /// post-delete chain in `apply_message`. Does not check `self.busy` -
    /// callers are responsible for that; the post-delete chain deliberately
    /// keeps `busy` set from the delete job straight through to compaction.
    fn spawn_compact_job(&mut self, db_path: PathBuf, status_message: String) {
        self.busy = true;
        self.status_message = status_message;
        let handle = worker::compact::spawn_compact(db_path, self.tx.clone(), self.lang());
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
            ..self.settings.clone()
        };
        self.persist_settings();
    }

    /// Applies a new keep-list and persists it immediately, mirroring
    /// `set_delete_method`. Takes effect on the *next* scan - the currently
    /// displayed findings (if any) are left untouched. Callers (the settings
    /// dialog) are responsible for never producing an empty list - see
    /// `ui::settings_dialog`'s "at least one language stays checked" rule.
    pub fn set_keep_languages(&mut self, keep_languages: Vec<String>) {
        if self.settings.keep_languages == keep_languages {
            return;
        }
        self.settings = Settings {
            keep_languages,
            ..self.settings.clone()
        };
        self.persist_settings();
    }

    /// Applies a new scan-routing mode and persists it immediately,
    /// mirroring `set_delete_method`. Takes effect on the *next* scan - a
    /// scan already in progress keeps using the mode it was spawned with.
    pub fn set_scan_routing(&mut self, scan_routing: ScanRouting) {
        if self.settings.scan_routing == scan_routing {
            return;
        }
        self.settings = Settings {
            scan_routing,
            ..self.settings.clone()
        };
        self.persist_settings();
    }

    /// Applies a new theme and persists it immediately, mirroring
    /// `set_delete_method`. Takes effect the same frame: `eframe::App::ui`
    /// calls `ctx.set_theme` every frame from `self.settings.theme`, so
    /// there is no separate "apply" step to forget.
    pub fn set_theme(&mut self, theme: Theme) {
        if self.settings.theme == theme {
            return;
        }
        self.settings = Settings {
            theme,
            ..self.settings.clone()
        };
        self.persist_settings();
    }

    /// Applies a new set of enabled scan categories and persists it
    /// immediately, mirroring `set_keep_languages`. Takes effect on the
    /// *next* scan - the currently displayed findings (if any) are left
    /// untouched. Callers (the settings dialog) are responsible for never
    /// letting the *last* checked category be unchecked - see
    /// `ui::settings_dialog`. An empty list is otherwise a perfectly valid
    /// value here (it means "every category enabled" - see
    /// `gametrimmer_core::settings::Settings::enabled_categories`).
    pub fn set_enabled_categories(&mut self, enabled_categories: Vec<String>) {
        if self.settings.enabled_categories == enabled_categories {
            return;
        }
        self.settings = Settings {
            enabled_categories,
            ..self.settings.clone()
        };
        self.persist_settings();
    }

    fn persist_settings(&mut self) {
        let lang = self.lang();
        let Some(db_path) = self.db_path.clone() else {
            self.warnings
                .push(i18n::strings(lang).settings_not_saved_no_db.to_string());
            return;
        };
        let result = gametrimmer_core::db::open(&db_path)
            .and_then(|conn| gametrimmer_core::settings::save(&conn, &self.settings));
        if let Err(err) = result {
            self.warnings.push(i18n::settings_save_failed(lang, err));
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

        // `rules_io_active` keeps frames coming while a rules export/import
        // runs behind its native file dialog, so the result label appears
        // the moment the background thread reports back - not on the next
        // mouse move.
        if received_any || self.busy || self.rules_io_active {
            ctx.request_repaint();
        }
    }

    fn apply_message(&mut self, msg: WorkerMsg) {
        let lang = self.lang();
        match msg {
            WorkerMsg::LibrariesFound { libraries, games } => {
                self.status_message = i18n::libraries_found(lang, libraries, games);
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
                self.status_message = i18n::scan_done_status(lang, &scan_summary, count);
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
                self.status_message = i18n::remove_done_status(lang, succeeded, failed.len());
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
                        let s = i18n::strings(lang);
                        let status = format!("{} {}", s.deletion_completed, s.compacting_database);
                        self.spawn_compact_job(db_path, status);
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
                self.status_message = i18n::strings(lang).scan_cancelled.to_string();
            }
            WorkerMsg::Error { msg } => {
                self.busy = false;
                self.progress = None;
                self._worker = None;
                self.status_message = i18n::error_prefixed(lang, msg);
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
                    self.warnings.push(i18n::export_save_failed(lang, error));
                } else if let Some(path) = path {
                    self.status_message = i18n::exported_to(lang, path.display());
                }
                // `path` and `error` both `None` means the user cancelled
                // the save dialog - nothing to report.
            }
            WorkerMsg::RulesExportDone { path, error } => {
                self.rules_io_active = false;
                if let Some(error) = error {
                    let msg = i18n::rules_export_failed(lang, error);
                    self.rules_io_result = Some(Err(msg.clone()));
                    self.warnings.push(msg);
                } else if let Some(path) = path {
                    let msg = i18n::rules_exported_to(lang, path.display());
                    self.rules_io_result = Some(Ok(msg.clone()));
                    self.status_message = msg;
                }
                // Both `None`: the folder picker was cancelled.
            }
            WorkerMsg::RulesImportDone { summary, error } => {
                self.rules_io_active = false;
                if let Some(error) = error {
                    let msg = i18n::rules_import_failed(lang, error);
                    self.rules_io_result = Some(Err(msg.clone()));
                    self.warnings.push(msg);
                } else if let Some(summary) = summary {
                    self.rules_io_result = Some(Ok(summary.clone()));
                    self.status_message = summary;
                }
                // Both `None`: the file picker was cancelled.
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
                let s = i18n::strings(lang);
                match error {
                    Some(err) => self.status_message = err,
                    None if skipped && after_delete => {
                        self.status_message = s.deletion_completed.to_string();
                    }
                    // Manually-triggered compaction with nothing worth doing:
                    // the hint under the settings button already explains the
                    // 25% rule, so no status message is needed here.
                    None if skipped => {
                        self.status_message.clear();
                    }
                    None => {
                        self.status_message = if after_delete {
                            format!("{} {}", s.deletion_completed, s.database_compacted)
                        } else {
                            s.database_compacted.to_string()
                        };
                    }
                }
            }
        }
    }
}

/// Converts the persisted theme setting into the egui type that actually
/// drives rendering. `System` maps to egui's own `ThemePreference::System`,
/// which resolves against the OS preference itself (via the raw system
/// theme reported by the windowing backend) - this app never needs to poll
/// the OS directly for that.
fn theme_preference(theme: Theme) -> egui::ThemePreference {
    match theme {
        Theme::System => egui::ThemePreference::System,
        Theme::Light => egui::ThemePreference::Light,
        Theme::Dark => egui::ThemePreference::Dark,
    }
}

impl eframe::App for GameTrimmerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // eframe's built-in persistence is deliberately disabled (see
        // docs/portability-audit.md) so this app owns "did the theme
        // change" itself: applying the current setting every frame is
        // cheap (it just writes an enum into egui's in-memory options) and
        // means a change from the settings dialog takes effect the same
        // frame with no separate "apply" step to forget.
        ctx.set_theme(theme_preference(self.settings.theme));
        self.drain_messages(&ctx);

        ui::top_bar::show(self, ui);
        ui::bottom_bar::show(self, ui);
        ui::tree_view::show(self, ui);
        ui::dialogs::show(self, ui);
        ui::settings_dialog::show(self, ui);
    }
}
