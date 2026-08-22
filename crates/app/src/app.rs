//! Application state and the `eframe::App` entry point. Rendering itself is
//! delegated to the `ui` module; this file owns the worker channel and the
//! state transitions driven by [`WorkerMsg`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use eframe::egui;

use gametrimmer_core::mftscan;
use gametrimmer_core::settings::{
    ConfirmBehavior, DeleteMethod, Lang, LanguagePreference, SelectionProfile, Settings, Theme,
};

use crate::elevation;
use crate::export;
use crate::i18n;
use crate::logger;
use crate::model::{self, DisplayCategory, FindingItem, TopGroup};
use crate::search;
use crate::ui;
use crate::worker::delete::DeleteItem;
use crate::worker::manual::{self, LibraryRow};
use crate::worker::scan_route;
use crate::worker::{self, WorkerMsg};

pub const APP_TITLE: &str = "GameTrimmer";

/// Summary shown to the user after a delete operation completes.
pub struct RemoveSummary {
    /// Files removed without error. For a Recycle Bin batch this still
    /// includes any `nuked` items (Windows reported them as success); the
    /// summary splits them back out so the recoverable count is honest.
    pub succeeded: usize,
    /// Of `succeeded`, how many were permanently deleted rather than recycled
    /// because they exceeded the volume's Recycle Bin quota (see
    /// [`crate::worker::RemoveOutcome::nuked`]). Always 0 for a permanent
    /// delete.
    pub nuked: usize,
    pub failed: Vec<(PathBuf, String)>,
    /// The method this batch actually ran with, so the summary words itself
    /// per-operation rather than off the persisted default (which may differ
    /// when the user picked a one-off method without "remember").
    pub method: gametrimmer_core::settings::DeleteMethod,
    /// On-disk bytes the user expected to free (sum over everything queued) -
    /// the same figure the confirm dialog promised (allocated-size accounting).
    pub expected_bytes: u64,
    /// On-disk bytes actually reclaimed *now*: successfully removed files for a
    /// permanent delete, or only the over-quota `nuked` files for a recycle
    /// (recycled files still occupy the same volume until the bin is emptied).
    pub freed_bytes: u64,
    /// On-disk bytes moved to the Recycle Bin that will free once it is emptied
    /// (recycle batches only; 0 for a permanent delete).
    pub recycled_pending_bytes: u64,
}

/// State of the delete confirmation modal (`ui::dialogs::show_confirm_delete`).
///
/// The removal method lives here rather than being read straight from
/// `Settings` at delete time so the modal can offer it as a per-operation
/// choice: `method` starts at the persisted setting and is only written back
/// to it when the user ticks `remember`. Leaving `remember` off keeps the
/// choice scoped to this one delete, which is the point of offering it at the
/// moment of the decision instead of only in the settings dialog.
#[derive(Clone)]
pub struct ConfirmDelete {
    /// Indices into `findings` awaiting confirmation.
    pub indices: Vec<usize>,
    /// Removal method to use for this operation.
    pub method: DeleteMethod,
    /// Whether confirming should also persist `method` as the new default.
    pub remember: bool,
}

/// Progress of the currently running background operation (scan, delete, or
/// compaction), rendered by `ui::top_bar` as `"{verb} {current}/{total}:
/// {detail}"` for scan/delete, or `"{verb} {percent}%"` when `detail` is
/// empty (compaction, which has no per-item detail to show). See
/// `WorkerMsg::Progress` for the field meanings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgressState {
    pub verb: i18n::Verb,
    pub current: usize,
    pub total: usize,
    pub detail: String,
}

/// Progress state of one phase in the 3-phase scanning architecture.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PhaseProgress {
    pub current: usize,
    pub total: usize,
    pub detail: String,
    pub extra_count: usize,
}

/// Aggregate 3-phase scanning progress state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScanPhaseState {
    pub phase1: Option<PhaseProgress>,
    pub phase2: Option<PhaseProgress>,
    pub phase3: Option<PhaseProgress>,
    pub overall_fraction: f32,
    pub overall_message: String,
}

pub struct GameTrimmerApp {
    db_path: Option<PathBuf>,
    settings_path: Option<PathBuf>,
    /// Where this instance writes diagnostics. Production resolves the file
    /// beside the executable; tests pass `None` so their process-global
    /// logger cannot leak state or files between parallel UI cases.
    log_path: Option<PathBuf>,
    /// Set only when the database could not be located or opened at startup;
    /// the path itself is not shown in the UI (the database always lives
    /// next to the executable).
    pub db_error: Option<String>,

    /// Persisted user settings (deletion method, ...), loaded from
    /// `gametrimmer.ini` at startup and saved there on every change.
    pub settings: Settings,
    /// What Windows reported as the user's preferred UI language at startup,
    /// already narrowed to one this app speaks. Read once and kept, not
    /// re-detected - see [`Self::lang`], which resolves the persisted
    /// preference against it.
    system_lang: Lang,
    /// Whether the settings dialog is currently open.
    pub show_settings: bool,
    /// Which section of the rebuilt settings dialog is showing. UI-only and
    /// never persisted - reopening the dialog starts at General.
    pub settings_section: ui::settings::SettingsSection,
    /// Filter text for the "Scanning" section's keep-language search box.
    /// UI-only, never persisted - and deliberately app state rather than
    /// egui memory, so the section stays a plain `show(app, ui)` function
    /// the harness can drive.
    pub keep_language_query: String,
    /// Why the last scan walked the roots it did not read from the MFT
    /// index, already localized (see
    /// `worker::scan_route::format_walkdir_breakdown`). Empty before any
    /// scan this session, and whenever every root took the MFT path.
    /// Shown in the "Scanning" section, because "prefer the MFT index"
    /// cannot promise the MFT will be used.
    pub last_routing_breakdown: String,

    /// Why the last settings write failed, already localized, or `None` if
    /// it succeeded. Shown in the dialog's footer, where the change was
    /// made - the warnings list it also lands in is on the main window,
    /// behind the modal.
    pub settings_save_error: Option<String>,
    /// Whether the most recent settings write succeeded. Drives the
    /// dialog's "Saved" indicator; cleared when the dialog closes, so it
    /// always describes a change made in the session the user can see.
    pub settings_saved: bool,
    /// Snapshot of [`Self::settings`] as of the last time the watch daemon
    /// was notified over IPC (or as loaded at startup, before any notify has
    /// happened this session) - see `persist_settings` and
    /// `watch_relevant_settings_changed`. Compared against the current
    /// settings on every save so a change to a setting the daemon has no use
    /// for (theme, delete method, ...) does not pay for a pipe round trip at
    /// all. Updated optimistically right before the notify is spawned, not
    /// after it succeeds - the notify has no way back to this struct from its
    /// background thread, and the settings themselves are already durably
    /// saved to `gametrimmer.ini` by the time this is read, so a dropped
    /// notify only means the daemon finds out later than the file did, not
    /// that this snapshot lies about what was saved.
    watch_synced_settings: Settings,

    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
    /// Cloned into every worker spawn (see `worker::Notifier`) so a
    /// background thread can call `request_repaint()` right after every
    /// `WorkerMsg` send - this is what keeps progress moving while the main
    /// window is minimized, when winit stops calling `eframe::App::ui()` (and
    /// so `drain_messages()` never runs) on its own.
    egui_ctx: egui::Context,
    cancel: Arc<AtomicBool>,
    /// Kept only so the thread is joined on drop rather than detached;
    /// never awaited from the UI thread.
    _worker: Option<JoinHandle<()>>,

    /// Whether any background job is running. Read all over the UI to
    /// disable controls; **write only** through [`GameTrimmerApp::begin_job`]
    /// and [`GameTrimmerApp::end_job`], which keep it in step with
    /// `cancellable_job`.
    pub busy: bool,
    /// Whether the running job can actually be stopped - true only for a
    /// scan, the sole worker that reads the cancel token (`worker::scan`).
    ///
    /// Recorded when the job starts rather than derived from
    /// `progress.verb`, because `start_scan` clears `progress` and the scan's
    /// first phase can run 15-20s before the first `Progress` message. A verb
    /// gate would hide "Cancel" for exactly that stretch - the part of a scan
    /// a user is most likely to want to abort.
    cancellable_job: bool,
    pub progress: Option<ProgressState>,
    /// Granular 3-phase scan progress.
    pub scan_phase_state: Option<ScanPhaseState>,
    pub status_message: String,
    /// UI-only animation state for the progress line: the `progress.detail`
    /// shown last frame and the animation-clock time (`egui`'s input `time`)
    /// at which it last changed. `ui::top_bar` uses these to tell when a
    /// single item (a large game being analyzed) has held the line unchanged
    /// long enough to look frozen, and animate a running-dots suffix after
    /// its name so the app clearly still looks alive.
    pub last_progress_detail: String,
    pub last_progress_detail_at: f64,
    /// How long the most recently completed scan's phases took (see
    /// `model::ScanTiming`), shown persistently in the bottom bar until the
    /// next scan starts. `None` before any scan has completed this session,
    /// after "Clear database", or when the current results were loaded from
    /// a previous scan rather than freshly produced (see `WorkerMsg::Done`).
    pub last_scan_timing: Option<model::ScanTiming>,

    /// Whether the background monitoring companion daemon is running.
    pub watch_daemon_running: bool,
    /// Games that have been recently updated via background monitoring.
    pub updated_games: std::collections::HashMap<String, String>,
    /// Last time we checked IPC daemon status.
    #[allow(dead_code)]
    pub last_ipc_poll: Option<std::time::Instant>,

    pub findings: Vec<FindingItem>,
    /// Turns each finding's stored (English) description into the one the
    /// window shows - see `worker::descriptions`.
    ///
    /// Held here rather than applied to the rows once, because the answer
    /// depends on the *current* interface language: overwriting
    /// `FindingRow::rule_desc` with a translation destroys the English key
    /// the next translation would need, which is exactly how a language
    /// switch used to leave the previous language's text on screen.
    /// Rebuilt by [`Self::refresh_descriptions`].
    pub descriptions: worker::descriptions::Descriptions,
    /// Live disk-usage snapshot (total + per-library), refreshed by every
    /// `WorkerMsg::Done` (scan or load) - see
    /// `gametrimmer_core::db::occupied_by_library`. Never persisted.
    pub occupancy: model::Occupancy,
    pub tree: Vec<TopGroup>,
    /// Set by mid-batch `FileRemoved` messages during a delete. The tree is
    /// rebuilt at most once per frame in `drain_messages`, not once per
    /// message - running `build_tree` over thousands of findings for every
    /// single removed file would burn CPU for nothing.
    /// Programs found installed outside every launcher, offered as folders to
    /// register by hand. `None` until the user asks - the sweep reads the
    /// whole uninstall registry. See `ui::settings::scanning`.
    pub(crate) standalone_candidates:
        Option<Vec<gametrimmer_core::standalone::StandaloneCandidate>>,
    /// `pub(crate)` for `ui::tree_view::apply_keep_request`, which drops a
    /// kept row from the plan and needs the next frame to rebuild the tree
    /// without it.
    pub(crate) tree_dirty: bool,
    /// Explicit user expand/collapse choices for the virtualized tree view,
    /// keyed by a stable node key (see `ui::tree_view`). Absent key = the
    /// node's default (top-level branches open, games/folders closed,
    /// categories open). Keyed per grouping axis (see `model::TopKey`), so one
    /// map holds every axis's state without them reading each other's.
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
    /// Active "plan of action" card filter (plan-action filtering): when `Some`, the findings
    /// tree shows only that display category (the user clicked a card's
    /// "View" button). `None` = the full tree. UI-only; never persisted, and
    /// cleared whenever a fresh tree is built.
    pub tree_category_filter: Option<model::DisplayCategory>,
    /// The column and direction the user ordered the tree by, or `None` for
    /// the tree's own designed order (see `model::sort_tree`). UI-only and
    /// never persisted - but, unlike the filter and the search above,
    /// deliberately *kept* across a fresh scan: it is a preference about how to
    /// read a tree, not a selection keyed to the result set that is being
    /// replaced.
    pub tree_sort: Option<model::TreeSort>,
    /// What the tree's top level is grouped by (see `model::GroupAxis`).
    /// UI-only and never persisted, and kept across a fresh scan for the same
    /// reason `tree_sort` is: it says how the user wants to read a tree, not
    /// which rows of one particular result set they picked.
    pub tree_axis: model::GroupAxis,
    /// Name search text (name search), as typed. UI-only, never persisted, and
    /// cleared whenever a fresh tree is built - a query from the previous
    /// result set would silently hide most of the new one.
    pub tree_search: String,
    /// Which findings `tree_search` matches. Rebuilt only when the query or
    /// the findings change, never per frame - see `search::SearchIndex`.
    pub tree_search_index: search::SearchIndex,
    /// The lowercase text `tree_search_index` is built from. Folded once per
    /// findings list rather than once per keystroke, which is what keeps the
    /// search field responsive on a large scan - see `search::Corpus`.
    tree_search_corpus: search::Corpus,

    /// Every registered library (all vendors), for the library management
    /// list. Refreshed after every add/remove and on startup.
    pub libraries: Vec<LibraryRow>,
    /// True while the background "Add Folder..." folder-picker thread is
    /// running, so the button can't be clicked twice concurrently.
    pub folder_picker_active: bool,
    /// True while the background "Export..." save-dialog thread is
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
    /// True while a user-triggered database maintenance job (compact or
    /// clear, started from the settings dialog) runs. Distinct from `busy`
    /// (which every job sets) so the dialog can show a spinner only for its
    /// own jobs, never for an unrelated scan the user left running. The
    /// automatic post-delete compaction deliberately does not set it - the
    /// dialog isn't open then and never needs to reflect it.
    /// Opt-ins for the diagnostic bundle. Not persisted to the ini on
    /// purpose: a privacy choice that survives restarts is one the user
    /// stops re-reading, and both of these are decisions worth taking
    /// again each time a bundle is generated.
    pub bundle_options: gametrimmer_core::bundle::BundleOptions,
    /// The rendered `summary.txt` shown before anything is written, and the
    /// options it was rendered for - so a toggle re-renders it and nothing
    /// else does. `None` until the Data section is first opened.
    pub bundle_preview: Option<(gametrimmer_core::bundle::BundleOptions, String)>,
    pub bundle_active: bool,
    pub bundle_result: Option<Result<String, String>>,
    pub db_maint_active: bool,
    /// Outcome of the last database maintenance job, shown inside the
    /// settings dialog next to its buttons for the same reason as
    /// [`Self::rules_io_result`] (the top-bar status is hidden behind the
    /// modal): `Ok` carries the success line, `Err` the failure text.
    pub db_maint_result: Option<Result<String, String>>,

    /// State of the delete confirmation modal, `None` while it is closed.
    pub confirm_delete: Option<ConfirmDelete>,
    pub remove_summary: Option<RemoveSummary>,
    /// Whether the "Clear database" confirmation modal (see `ui::dialogs`)
    /// is currently shown. Set by the settings dialog's "Clear database"
    /// button; the wipe itself only starts once the user confirms - this is
    /// a destructive, unconfirmed-click-proof action, mirroring
    /// `confirm_delete` above.
    pub confirm_clear_database: bool,
    /// Set when the compaction job about to run was chained automatically
    /// after a delete (see `RemoveDone`), rather than triggered manually via
    /// the settings dialog (`start_compact`). Read (and reset) by the
    /// `CompactDone` arm to decide whether its status message should be
    /// prefixed with a "Deletion complete" note - a manual compaction
    /// never gets that prefix.
    compact_after_delete: bool,

    /// Whether this process currently holds Administrator rights - gates
    /// the MFT index scan path (see `crate::elevation`, `worker::scan_route`).
    /// Checked once at startup; a relaunch-elevated always restarts the
    /// process, so this never needs to change while running.
    pub elevated: bool,
    /// Whether the startup modal offering a UAC relaunch (for faster MFT
    /// scanning) is currently shown. Only ever `true` at startup, and only
    /// when `!elevated`, the user has not permanently refused
    /// (`settings.never_ask_elevation`), *and* elevating would actually help:
    /// at least one game library's volume must not be a confirmed SSD, on
    /// which the MFT path would lose to a directory walk anyway. See
    /// `scan_route::should_offer_elevation` and `compute_show_elevation_prompt`.
    pub show_elevation_prompt: bool,
    /// The elevation modal's "don't ask again" tick, in flight.
    ///
    /// Lives here rather than in the modal body for the same reason
    /// [`ConfirmDelete::remember`] does: the modal is rebuilt every frame, so
    /// a tick held in a local would be re-read from the persisted setting on
    /// the next frame and be gone by the time "Continue" is clicked - which is
    /// never the frame the box was ticked in. Only written through to
    /// `settings.never_ask_elevation` on the way out, and deliberately not at
    /// all when the answer is a relaunch (see `ui::dialogs`).
    pub elevation_never_ask: bool,
}

impl GameTrimmerApp {
    /// `ctx` is `cc.egui_ctx` from `eframe::run_native`'s creation callback -
    /// see `worker::Notifier` for why every background worker needs it.
    pub fn new(ctx: egui::Context) -> Self {
        Self::new_with(
            ctx,
            worker::db_path().ok(),
            worker::settings_path().ok(),
            worker::log_path().ok(),
            true,
        )
    }

    /// The real constructor, with the ambient state that makes
    /// [`Self::new`] unusable from a test taken as parameters instead:
    ///
    /// * `db_path`: where the database lives. Production passes
    ///   `worker::db_path().ok()`, i.e. `gametrimmer.db` next to the
    ///   executable. Under `cargo test` that would resolve to
    ///   `target/debug/deps/`, so every test in the binary would open, create
    ///   and mutate one shared file - tests pass a path inside their own
    ///   `TempDir` instead.
    /// * `settings_path`: the `gametrimmer.ini` beside the executable. Tests
    ///   pass a sibling of their throwaway database so preferences cannot
    ///   leak between tests.
    /// * `log_path`: the `gametrimmer.log` beside the executable. Tests pass
    ///   `None` because the logger is process-global and parallel UI tests
    ///   must neither replace each other's file handles nor create a shared
    ///   log under `target/debug/deps/`.
    /// * `autoload`: whether to spawn the background worker that reloads the
    ///   previous scan's findings. A test wants a deterministic widget tree,
    ///   not a thread racing its assertions, so it passes `false`.
    ///
    /// Everything else (`elevation::is_elevated`, the per-volume media probe
    /// behind `show_elevation_prompt`) is left alone deliberately: with a
    /// fresh database `load_libraries` returns an empty list, and
    /// `compute_show_elevation_prompt` over no libraries probes no volumes
    /// and answers `false`. So a temp-database app is already deterministic
    /// without stubbing those out.
    fn new_with(
        ctx: egui::Context,
        db_path: Option<PathBuf>,
        settings_path: Option<PathBuf>,
        log_path: Option<PathBuf>,
        autoload: bool,
    ) -> Self {
        // One Win32 call, before anything can need a string: the default
        // preference is "follow Windows", so even the pre-database errors
        // below are worded in the machine's own language rather than in
        // English on a Ukrainian desktop.
        let system_lang = i18n::detect_system_language();

        // Settings (and thus the UI language) are not known until the ini is
        // read. Startup errors before that point use the default preference's
        // text, same as any other place with no settings yet.
        let startup_lang = Settings::default().app_language.resolve(system_lang);
        // Two renderings of the same failure, on purpose: one for the window
        // in the user's language, one for the log in English. The log is read
        // by whoever receives a bug report, and a Ukrainian error line in a
        // report from a Ukrainian desktop is the same problem the forced
        // `Lang::En` at the scan worker's logging sites already solves.
        let (db_error, db_error_for_log, legacy_conn) = match &db_path {
            Some(path) => match gametrimmer_core::db::open(path) {
                Ok(conn) => (None, None, Some(conn)),
                Err(err) => (
                    Some(i18n::db_open_error_long(startup_lang, &err)),
                    Some(i18n::db_open_error_long(Lang::En, &err)),
                    None,
                ),
            },
            None => (
                Some(i18n::strings(startup_lang).db_path_error.to_string()),
                Some(i18n::strings(Lang::En).db_path_error.to_string()),
                None,
            ),
        };
        // A damaged or unreadable ini must not block startup. When it is
        // absent, `load_file_or_migrate` reads the legacy table once and
        // atomically creates the new single source of truth.
        let settings = match settings_path.as_deref() {
            Some(path) => {
                gametrimmer_core::settings::load_file_or_migrate(path, legacy_conn.as_ref())
                    .unwrap_or_default()
            }
            None => Settings::default(),
        };
        drop(legacy_conn);
        let libraries = Self::load_libraries(db_path.as_deref());
        // Short-circuits when `autoload` is off, so a test does not even pay
        // the database open for a question whose answer it would ignore.
        let has_saved_findings = autoload && Self::has_saved_findings(db_path.as_deref());
        let elevated = elevation::is_elevated();
        let never_ask_elevation = settings.never_ask_elevation;
        // Only worth computing when not already elevated - the modal is
        // never shown otherwise, so there is nothing this decision could
        // change.
        let show_elevation_prompt =
            !elevated && compute_show_elevation_prompt(settings.never_ask_elevation, &libraries);

        // Logging is enabled by default (see
        // `settings::Settings::logging_enabled`), while an explicit saved
        // `false` still keeps it off. Failure to resolve the exe directory
        // leaves `log_path` as `None`, so logging simply stays unavailable
        // for this session; there is no UI up yet to report it through.
        if settings.logging_enabled {
            if let Some(path) = log_path.as_deref() {
                logger::set_enabled(true, elevated, path);
            }
        }

        // Deliberately *after* `set_enabled`: the database is opened before
        // the ini is read (the ini is the only place the logging preference
        // lives), so logging the failure at the point it happens would write
        // it to a file that is not open yet. Without this, a user whose
        // database never opened hands over a log containing nothing but the
        // session header - the one failure where the log is the only artifact
        // they have, since there is no scan to leave a trail either.
        if let Some(message) = &db_error_for_log {
            logger::error(message);
        }

        let (tx, rx) = mpsc::channel();

        let mut app = Self {
            db_path: db_path.clone(),
            settings_path,
            log_path,
            db_error,
            watch_synced_settings: settings.clone(),
            settings,
            system_lang,
            show_settings: false,
            settings_section: ui::settings::SettingsSection::General,
            keep_language_query: String::new(),
            last_routing_breakdown: String::new(),
            settings_save_error: None,
            settings_saved: false,
            tx: tx.clone(),
            rx,
            egui_ctx: ctx,
            cancel: Arc::new(AtomicBool::new(false)),
            _worker: None,
            busy: false,
            cancellable_job: false,
            progress: None,
            scan_phase_state: None,
            status_message: String::new(),
            last_progress_detail: String::new(),
            last_progress_detail_at: 0.0,
            last_scan_timing: None,
            findings: Vec::new(),
            descriptions: worker::descriptions::Descriptions::load(Lang::En),
            occupancy: model::Occupancy::default(),
            tree: Vec::new(),
            standalone_candidates: None,
            tree_dirty: false,
            tree_toggles: std::collections::HashMap::new(),
            tree_cursor: None,
            tree_scroll_offset: 0.0,
            tree_viewport_height: 0.0,
            tree_category_filter: None,
            tree_sort: None,
            tree_axis: model::GroupAxis::default(),
            tree_search: String::new(),
            tree_search_index: search::SearchIndex::default(),
            tree_search_corpus: search::Corpus::default(),
            libraries,
            folder_picker_active: false,
            export_active: false,
            rules_io_active: false,
            rules_io_result: None,
            bundle_options: gametrimmer_core::bundle::BundleOptions::default(),
            bundle_preview: None,
            bundle_active: false,
            bundle_result: None,
            db_maint_active: false,
            db_maint_result: None,
            confirm_delete: None,
            remove_summary: None,
            confirm_clear_database: false,
            compact_after_delete: false,
            elevated,
            show_elevation_prompt,
            elevation_never_ask: never_ask_elevation,
            watch_daemon_running: false,
            updated_games: std::collections::HashMap::new(),
            last_ipc_poll: None,
        };

        // Show the previous scan's results immediately rather than an empty
        // screen: if the database already holds at least one `findings` row
        // (from an earlier "Scan Libraries" run), load and display it
        // right away. A missing db_path, a database that fails to open, or
        // one with no saved findings yet all fall through unchanged - the
        // ordinary empty startup screen, waiting for the user to scan. So
        // does `autoload = false`, which is how a test keeps this thread
        // from racing its own assertions.
        if has_saved_findings {
            if let Some(db_path) = db_path {
                app.begin_job(false);
                app.status_message = i18n::strings(app.lang()).loading_previous_scan.to_string();
                app._worker = Some(worker::load::spawn_load(
                    db_path,
                    tx,
                    app.lang(),
                    app.egui_ctx.clone(),
                ));
            }
        }

        app
    }

    /// The active UI language: the persisted preference, resolved against
    /// what Windows was reporting at startup. Render code and worker spawns
    /// call this each frame/action rather than caching it, so a language
    /// switch (see `set_language`) takes effect immediately.
    ///
    /// The *system* half is deliberately not re-read here. It is one Win32
    /// call, but this runs several times per frame, and a user changing their
    /// Windows UI language mid-session is a case Windows itself resolves by
    /// sign-out - so a value that could change under a running app would only
    /// buy inconsistency between one widget and the next.
    pub fn lang(&self) -> Lang {
        self.settings.app_language.resolve(self.system_lang)
    }

    /// The detected OS UI language, captured once at startup.
    pub fn system_lang(&self) -> Lang {
        self.system_lang
    }

    /// Read access to the otherwise-private database path: the settings
    /// dialog shows it (and offers to open its folder) so a user filing a
    /// bug can find the file, and the harness tests assert that each test
    /// app has its own.
    pub fn db_path(&self) -> Option<&std::path::Path> {
        self.db_path.as_deref()
    }

    /// The `gametrimmer.log` beside the executable, when one is in play.
    ///
    /// Shown in the settings dialog for the same reason the database path is
    /// (see `ui::settings::data`): a file the user is asked to attach to a
    /// bug report has to be findable without knowing where the exe lives.
    pub fn log_path(&self) -> Option<&std::path::Path> {
        self.log_path.as_deref()
    }

    /// Test-only read access to the portable ini path.
    #[cfg(test)]
    pub fn settings_path(&self) -> Option<&std::path::Path> {
        self.settings_path.as_deref()
    }

    /// Marks a background job as started. `cancellable` is true only when the
    /// spawned worker is handed the cancel token and actually polls it.
    ///
    /// The only way to set `busy`, so the "is it cancellable" answer cannot
    /// drift away from "is something running" the way two independently
    /// assigned flags would.
    pub(crate) fn begin_job(&mut self, cancellable: bool) {
        self.busy = true;
        self.cancellable_job = cancellable;
    }

    /// Marks the running job as finished. The counterpart to [`Self::begin_job`]
    /// and the only way to clear `busy`.
    pub(crate) fn end_job(&mut self) {
        self.busy = false;
        self.cancellable_job = false;
    }

    /// Whether to offer "Cancel" right now.
    ///
    /// The button used to appear for any `busy`, but `cancel_scan` only sets
    /// the scan's cancel token - during a delete, compaction, database clear,
    /// rules import or export it looked actionable but could not cancel that
    /// operation.
    pub fn can_cancel(&self) -> bool {
        self.busy && self.cancellable_job
    }

    /// Whether any modal dialog is on screen.
    ///
    /// One source of truth for "the user is inside a dialog", so background
    /// keyboard handling can be switched off in a single place (see
    /// `ui::tree_view`). Previously each caller listed the modals it happened
    /// to know about, and the tree's list was missing `show_settings` and
    /// `confirm_clear_database` - so arrow keys and Space still moved the
    /// cursor and toggled selection behind an open settings dialog.
    ///
    /// Every arm corresponds to one `egui::Modal` in `ui::dialogs` or
    /// `ui::settings`; adding a modal without adding it here is the
    /// mistake this method exists to make harder.
    pub fn any_modal_open(&self) -> bool {
        self.confirm_delete.is_some()
            || self.remove_summary.is_some()
            || self.show_elevation_prompt
            || self.show_settings
            || self.confirm_clear_database
    }

    /// Applies a new UI language preference and persists it immediately,
    /// mirroring `set_theme`. Called from the settings dialog's language
    /// selector; takes effect the same frame since every render call reads
    /// `self.lang()` fresh rather than caching it.
    ///
    /// Takes the *preference*, not a [`Lang`]: "follow Windows" is one of the
    /// three things the picker offers, and it is not a language.
    pub fn set_language(&mut self, preference: LanguagePreference) {
        if self.settings.app_language == preference {
            return;
        }
        self.settings = Settings {
            app_language: preference,
            ..self.settings.clone()
        };
        // Findings already on screen describe themselves through
        // `descriptions`, so the index has to follow the language here -
        // otherwise the tree keeps speaking the language it was loaded in.
        self.refresh_descriptions();
        self.persist_settings();
    }

    /// Rebuilds the finding-description index for the language now in
    /// effect. Cheap enough to call on a language switch (it reads the rule
    /// pack once), and skipped entirely for an English interface, where the
    /// stored text is already what gets shown.
    fn refresh_descriptions(&mut self) {
        self.descriptions = worker::descriptions::Descriptions::load(self.lang());
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
        conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM findings fi
                JOIN files f ON f.id = fi.file_id
                WHERE f.scan_id = (
                    SELECT active_scan_id FROM scan_state WHERE singleton = 1
                )
                LIMIT 1
            )",
            [],
            |row| row.get::<_, bool>(0),
        )
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
    /// Registers `path` as a manual library. Public so the standalone-game
    /// suggestions in the settings dialog can accept an offer directly, with
    /// the path they already know, instead of sending the user through a
    /// folder picker to find a folder the app just named.
    pub fn add_library_path(&mut self, path: PathBuf) {
        self.add_manual_library(path);
    }

    fn add_manual_library(&mut self, path: PathBuf) {
        let lang = self.lang();
        let Some(db_path) = self.db_path.clone() else {
            self.report_action_failure(i18n::strings(lang).no_db_path.to_string());
            return;
        };

        match gametrimmer_core::db::open(&db_path) {
            Ok(conn) => {
                if let Err(err) = manual::add_manual_library(&conn, &path) {
                    self.report_action_failure(i18n::add_library_failed(lang, path.display(), err));
                    return;
                }
                self.refresh_libraries();
            }
            Err(err) => self.report_action_failure(i18n::db_open_error_short(lang, err)),
        }
    }

    /// Removes a manual library (and, cascading, its games/files/findings)
    /// and refreshes the library list.
    pub fn remove_manual_library(&mut self, library_id: i64) {
        let lang = self.lang();
        let Some(db_path) = self.db_path.clone() else {
            self.report_action_failure(i18n::strings(lang).no_db_path.to_string());
            return;
        };

        match gametrimmer_core::db::open(&db_path) {
            Ok(mut conn) => {
                if let Err(err) = manual::remove_library(&mut conn, library_id) {
                    self.report_action_failure(i18n::remove_library_failed(lang, err));
                    return;
                }
                self.refresh_libraries();
            }
            Err(err) => self.report_action_failure(i18n::db_open_error_short(lang, err)),
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
        let csv = export::export_csv(lang, &self.descriptions, &self.findings, &self.tree);
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

    /// Overwrites one rule pack with the embedded defaults, keeping the
    /// previous file as `*.bak` (see `worker::rules_io::restore_builtin`).
    ///
    /// Runs synchronously: unlike export/import it opens no file picker and
    /// touches no database - it writes one small file, so a worker thread
    /// and a round trip through [`WorkerMsg`] would buy nothing.
    pub fn restore_rules_builtin(&mut self, kind: gametrimmer_core::packs::PackKind) {
        if self.busy || self.rules_io_active {
            return;
        }
        self.rules_io_result = Some(worker::rules_io::restore_builtin(self.lang(), kind));
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

    /// Spawns the «Import rules» flow: a blocking multi-file picker, a
    /// read-only active-snapshot impact preview, explicit confirmation, then
    /// an atomic batch replacement. Result comes back as
    /// [`WorkerMsg::RulesImportDone`]. The settings dialog
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
                Some(files) => match worker::rules_io::prepare_pack_import(lang, &files) {
                    Ok(prepared) => {
                        let confirmed = rfd::MessageDialog::new()
                            .set_title(title)
                            .set_description(&prepared.preview)
                            .set_level(rfd::MessageLevel::Warning)
                            .set_buttons(rfd::MessageButtons::OkCancel)
                            .show()
                            == rfd::MessageDialogResult::Ok;
                        if confirmed {
                            match worker::rules_io::apply_prepared_import(lang, prepared) {
                                Ok(summary) => (Some(summary), None),
                                Err(err) => (None, Some(err)),
                            }
                        } else {
                            (None, None)
                        }
                    }
                    Err(err) => (None, Some(err)),
                },
                None => (None, None),
            };
            let _ = tx.send(WorkerMsg::RulesImportDone { summary, error });
        });
    }

    /// Records that the user has started a scan at least once, which is what
    /// retires the first-run explanation (first-run onboarding, `ui::onboarding`).
    ///
    /// Its own method rather than three lines inside [`Self::start_scan`]:
    /// that one spawns a worker that walks the machine's real libraries, so a
    /// test cannot call it, and the bookkeeping would otherwise be reachable
    /// only by scanning for real.
    pub fn mark_scan_started(&mut self) {
        if self.settings.has_scanned {
            return;
        }
        self.settings = Settings {
            has_scanned: true,
            ..self.settings.clone()
        };
        self.persist_settings();
    }

    /// Records that the user accepted the liability disclaimer, which is what
    /// unblocks scanning and deletion (see [`Self::blocked_by_disclaimer`]).
    ///
    /// One-way on purpose. The checkbox that drives it renders only on the
    /// first-run screen, and that screen is gone the moment it is ticked -
    /// so there is no second state to return to, and un-accepting would mean
    /// an agreement the app remembers the user withdrawing while it keeps
    /// their scan results.
    pub fn accept_disclaimer(&mut self) {
        if self.settings.disclaimer_accepted {
            return;
        }
        self.settings = Settings {
            disclaimer_accepted: true,
            ..self.settings.clone()
        };
        self.persist_settings();
    }

    /// The reason scanning and deleting are unavailable before the disclaimer
    /// is accepted, or `None` once it is.
    ///
    /// Returned as the `gated_button` reason rather than checked at each call
    /// site, so the button says why it is grey instead of merely being grey -
    /// the app's standing convention for a gated action.
    pub fn blocked_by_disclaimer(&self) -> Option<&'static str> {
        (!self.settings.disclaimer_accepted)
            .then_some(i18n::strings(self.lang()).disabled_disclaimer)
    }

    /// The reason scanning is unavailable when the database never opened, or
    /// `None` once it did. Same shape as [`Self::blocked_by_disclaimer`]: the
    /// button reads this to say why it is grey instead of merely being grey.
    pub fn blocked_by_database(&self) -> Option<&'static str> {
        self.db_error
            .is_some()
            .then_some(i18n::strings(self.lang()).disabled_database)
    }

    pub fn start_scan(&mut self) {
        if self.busy {
            return;
        }
        // Belt to the button's braces: the gate has to hold at the action,
        // not only on the one control that happens to be greyed out. Both
        // entry points (top bar and the first-run screen) route here.
        if self.blocked_by_disclaimer().is_some() {
            return;
        }
        // Same belt-and-braces reasoning for a database that never opened:
        // `db_path` below is still `Some` in that case (see its assignment
        // in `new_with`), so without this a scan would reach the worker and
        // fail there too - the exact duplicate error this gate exists to
        // prevent (GT-74).
        if self.blocked_by_database().is_some() {
            return;
        }
        let Some(db_path) = self.db_path.clone() else {
            self.status_message = i18n::strings(self.lang()).no_db_path.to_string();
            return;
        };

        self.mark_scan_started();

        self.cancel.store(false, Ordering::Relaxed);
        self.begin_job(true);
        self.progress = None;
        self.scan_phase_state = None;
        self.status_message.clear();
        self.remove_summary = None;
        self.last_scan_timing = None;

        let handle = worker::scan::spawn_scan(
            db_path,
            Arc::clone(&self.cancel),
            self.tx.clone(),
            self.egui_ctx.clone(),
            self.elevated,
            worker::scan::ScanOptions {
                lang: self.lang(),
                keep_languages: self.settings.keep_languages.clone(),
                enabled_categories: self.settings.enabled_categories.clone(),
                excluded_libraries: self.settings.excluded_libraries.clone(),
                scan_monolithic_archives: self.settings.scan_monolithic_archives,
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

    /// Dismisses the elevation offer for this session, and - when the modal's
    /// checkbox was ticked - for every future one too.
    ///
    /// `never_ask` is persisted before the modal closes rather than after,
    /// so a crash between the click and the next launch cannot lose an answer
    /// the user has already given.
    pub fn continue_without_elevation(&mut self, never_ask: bool) {
        self.set_never_ask_elevation(never_ask);
        self.show_elevation_prompt = false;
    }

    pub fn cancel_scan(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Selects every non-removed finding (the "Select All" action).
    pub fn select_all(&mut self) {
        crate::deletion_controller::select_all(&mut self.findings);
        self.mark_selection_custom();
    }

    /// Deselects every finding (the "Deselect All" action).
    pub fn deselect_all(&mut self) {
        crate::deletion_controller::deselect_all(&mut self.findings);
        self.mark_selection_custom();
    }

    /// Records that the user hand-edited the selection, so the profile stops
    /// claiming a policy the checkboxes no longer follow.
    ///
    /// `SelectionProfile::Custom`'s own doc comment already described it as
    /// the state "entered when the user hand-edits the selection", but
    /// nothing ever set it: the picker could read "Balanced" while the actual
    /// selection had been edited row by row into something else.
    ///
    /// Cheap to call on every edit - it returns immediately once the profile
    /// is already `Custom`, so only the first hand-edit after a profile
    /// switch rewrites the ini.
    ///
    /// Only the live profile moves. What a *fresh scan* pre-checks lives in
    /// `default_selection_profile` and is untouched here, so hand-editing the
    /// tree does not quietly rewrite the setting for the next scan.
    pub fn mark_selection_custom(&mut self) {
        if self.settings.selection_profile == SelectionProfile::Custom {
            return;
        }
        self.settings = Settings {
            selection_profile: SelectionProfile::Custom,
            ..self.settings.clone()
        };
        self.persist_settings();
    }

    /// Selects the currently-checked, non-removed findings and opens the
    /// confirmation modal. No-op if nothing is selected.
    pub fn request_delete_confirmation(&mut self) {
        let indices = crate::deletion_controller::selected_indices(&self.findings);

        self.open_delete_confirmation(indices);
    }

    /// Whether a batch has to be confirmed before it is deleted, under the
    /// current [`ConfirmBehavior`].
    ///
    /// Split out from the two request paths so the policy can be exercised
    /// as plain logic - the alternative is a test that has to let a real
    /// delete run in order to observe the decision not to ask.
    pub(crate) fn needs_delete_confirmation(&self) -> bool {
        self.settings.confirm_behavior.should_confirm()
    }

    /// Shared tail of both delete request paths: build the modal state, then
    /// either show it or - when the policy says this batch does not need
    /// asking about - confirm it on the spot.
    ///
    /// Going through the modal state either way keeps this the only place
    /// that decides *whether* to ask, and `confirm_delete_now` the only place
    /// that starts a delete. No-op on an empty batch.
    fn open_delete_confirmation(&mut self, indices: Vec<usize>) {
        if indices.is_empty() {
            return;
        }
        let indices = match crate::deletion_controller::validate_batch(&self.findings, &indices) {
            Ok(indices) => indices,
            Err(block) => {
                self.status_message = i18n::deletion_blocked(self.lang(), &block.reason);
                return;
            }
        };
        // The single funnel both delete paths reach, so the disclaimer gate
        // is stated once here rather than at each button. It matters for the
        // upgrade case specifically: a database from before the disclaimer
        // already holds findings, so the tree is populated and its checkboxes
        // are live before anything has been agreed to.
        if self.blocked_by_disclaimer().is_some() {
            return;
        }
        let skip = !self.needs_delete_confirmation();
        // The persisted setting is the starting point, not a lock: the modal
        // lets the user pick a different method for this delete alone.
        self.confirm_delete = Some(ConfirmDelete {
            indices,
            method: self.settings.delete_method,
            remember: false,
        });
        if skip {
            self.confirm_delete_now();
        }
    }

    pub fn cancel_delete_confirmation(&mut self) {
        self.confirm_delete = None;
    }

    /// Focuses the findings tree on a single display category (plan-action filtering: a plan
    /// card's "View" button), or clears the filter with `None`. Resets the
    /// keyboard cursor, since the set of visible rows changes.
    pub fn set_category_filter(&mut self, filter: Option<DisplayCategory>) {
        self.tree_category_filter = filter;
        self.tree_cursor = None;
    }

    /// Rebuilds the findings tree and puts it back into the user's chosen
    /// order.
    ///
    /// The two steps belong together at every call site: `build_tree` only ever
    /// produces the default order, so a plain rebuild after a delete would
    /// silently throw away an active sort. `pub(crate)` so the test harness can
    /// seed findings through the same path.
    pub(crate) fn rebuild_tree(&mut self) {
        self.tree = model::build_tree(&self.findings, self.tree_axis);
        model::sort_tree(&mut self.tree, &self.findings, self.tree_sort);
    }

    /// Regroups the findings tree along a different axis (see
    /// `model::GroupAxis`).
    ///
    /// A pure re-cut of the findings already in memory: no rescan, and no row
    /// is added or dropped. The expand/collapse state is deliberately *not*
    /// cleared - the keys are namespaced per axis (see `model::TopKey`), so
    /// switching away and back returns the branches the user had opened
    /// instead of a tree folded shut again.
    pub fn set_tree_axis(&mut self, axis: model::GroupAxis) {
        if axis == self.tree_axis {
            return;
        }
        self.tree_axis = axis;
        self.rebuild_tree();
        // Every row index the cursor could be holding now names a different
        // row - the same reason [`Self::set_tree_sort`] resets it.
        self.tree_cursor = None;
    }

    /// Orders the findings tree by a column of the user's choosing, or restores
    /// the tree's own order with `None` (see `model::sort_tree`). Resets the
    /// keyboard cursor for the same reason [`Self::set_category_filter`] does:
    /// every row index it could be holding now names a different row.
    pub fn set_tree_sort(&mut self, sort: Option<model::TreeSort>) {
        if sort == self.tree_sort {
            return;
        }
        self.tree_sort = sort;
        self.rebuild_tree();
        self.tree_cursor = None;
    }

    /// Applies a new name-search query (name search), rebuilding the match index.
    /// No-op when the text is unchanged, so the index is not rebuilt on frames
    /// where the user merely clicked into the field. Resets the keyboard
    /// cursor for the same reason [`Self::set_category_filter`] does: the set
    /// of visible rows changes under it.
    pub fn set_search_query(&mut self, query: String) {
        if query == self.tree_search {
            return;
        }
        // The outgoing index is handed to the new one: typing one more
        // character can only narrow the previous hits, so the rebuild scans
        // those rather than the whole corpus (see `search::SearchIndex::build`).
        self.tree_search_index = search::SearchIndex::build(
            &query,
            &self.tree_search_corpus,
            Some(&self.tree_search_index),
        );
        self.tree_search = query;
        self.tree_cursor = None;
    }

    /// Drops any active search and refolds the corpus - used when a fresh
    /// result set replaces the one the query was built against.
    ///
    /// The two belong together: the corpus and the index are both keyed by
    /// position in `findings`, so this is called from every site that replaces
    /// that list, and is the reason no live index can ever outlive its corpus.
    ///
    /// `pub(crate)` so the test harness can restore the same invariant after
    /// seeding `findings` directly - a seeded list with a stale (empty) corpus
    /// would make every search assertion pass or fail for the wrong reason.
    pub(crate) fn clear_search(&mut self) {
        self.tree_search.clear();
        self.tree_search_index = search::SearchIndex::default();
        self.tree_search_corpus = search::Corpus::build(&self.findings);
    }

    /// Opens the delete confirmation for every non-removed finding in one
    /// display category (plan-action filtering: a plan card's "Remove" action). Unlike
    /// [`Self::request_delete_confirmation`], it acts on the whole category
    /// regardless of the current checkbox selection, so the card is a
    /// self-contained action. No-op if the category is empty.
    pub fn request_delete_for_category(&mut self, category: DisplayCategory) {
        let indices = crate::deletion_controller::category_indices(&self.findings, category);

        self.open_delete_confirmation(indices);
    }

    /// Runs after the user confirms the modal: builds full paths and hands
    /// the job to the delete worker.
    pub fn confirm_delete_now(&mut self) {
        let Some(ConfirmDelete {
            indices,
            method,
            remember,
        }) = self.confirm_delete.take()
        else {
            return;
        };
        let Some(db_path) = self.db_path.clone() else {
            self.status_message = i18n::strings(self.lang()).no_db_path.to_string();
            return;
        };

        // "Remember my choice" writes through the same setter the settings
        // dialog uses, so the two never drift into separate sources of truth.
        // A no-op when the method already matches (see `set_delete_method`).
        if remember {
            self.set_delete_method(method);
        }

        let items: Vec<DeleteItem> = indices
            .iter()
            .map(|&index| {
                let row = &self.findings[index].row;
                DeleteItem {
                    file_id: row.file_id,
                    size_on_disk: row.size_on_disk,
                    action: row.action.clone(),
                }
            })
            .collect();

        self.begin_job(false);
        self.status_message = i18n::strings(self.lang())
            .deleting_selected_files
            .to_string();
        let handle = worker::delete::spawn_delete(
            db_path,
            items,
            method,
            self.tx.clone(),
            self.lang(),
            self.egui_ctx.clone(),
        );
        self._worker = Some(handle);
    }

    /// Spawns the "Compact database" job (WAL checkpoint + `VACUUM`).
    /// No-op while another job is running.
    /// Renders `summary.txt` for the current opt-ins, reusing the last
    /// render when nothing changed.
    ///
    /// Cheap enough to call from the render pass: it is counts and identity
    /// only, not the findings projection. That is what lets the preview be
    /// the actual file rather than a description of it - the same function
    /// produces the archive's own `summary.txt`.
    pub fn refresh_bundle_preview(&mut self) {
        if self
            .bundle_preview
            .as_ref()
            .is_some_and(|(options, _)| *options == self.bundle_options)
        {
            return;
        }
        let Some(db_path) = self.db_path.clone() else {
            return;
        };
        let text = worker::bundle::input_from_paths(db_path, self.bundle_options, self.elevated)
            .map_err(|err| err.to_string())
            .and_then(|input| {
                gametrimmer_core::bundle::summary(&input).map_err(|err| err.to_string())
            });
        self.bundle_preview = Some(match text {
            Ok(summary) => (self.bundle_options, summary),
            // A preview that cannot be rendered is itself worth showing:
            // the same failure would meet the real generation.
            Err(err) => (self.bundle_options, i18n::bundle_failed(self.lang(), err)),
        });
    }

    pub fn start_bundle(&mut self) {
        if self.busy {
            return;
        }
        let Some(db_path) = self.db_path.clone() else {
            self.status_message = i18n::strings(self.lang()).no_db_path.to_string();
            return;
        };
        let input =
            match worker::bundle::input_from_paths(db_path, self.bundle_options, self.elevated) {
                Ok(input) => input,
                Err(err) => {
                    self.bundle_result = Some(Err(i18n::bundle_failed(self.lang(), err)));
                    return;
                }
            };

        self.bundle_active = true;
        self.bundle_result = None;
        self.begin_job(true);
        self.status_message = i18n::strings(self.lang()).bundle_label.to_string();
        let handle = worker::bundle::spawn_bundle(
            input,
            self.cancel.clone(),
            self.tx.clone(),
            self.lang(),
            self.egui_ctx.clone(),
        );
        self._worker = Some(handle);
    }

    pub fn start_compact(&mut self) {
        if self.busy {
            return;
        }
        let Some(db_path) = self.db_path.clone() else {
            self.status_message = i18n::strings(self.lang()).no_db_path.to_string();
            return;
        };
        // User-triggered from the settings dialog, so it drives the dialog's
        // own spinner/result (see `db_maint_active`). The automatic
        // post-delete compaction goes straight through `spawn_compact_job`
        // and deliberately leaves these untouched.
        self.db_maint_active = true;
        self.db_maint_result = None;
        let status = i18n::strings(self.lang()).compacting_database.to_string();
        self.spawn_compact_job(db_path, status);
    }

    /// Shared by `start_compact` (user-triggered) and the automatic
    /// post-delete chain in `apply_message`. Does not check `self.busy` -
    /// callers are responsible for that; the post-delete chain deliberately
    /// keeps `busy` set from the delete job straight through to compaction.
    fn spawn_compact_job(&mut self, db_path: PathBuf, status_message: String) {
        self.begin_job(false);
        self.status_message = status_message;
        let handle = worker::compact::spawn_compact(
            db_path,
            self.tx.clone(),
            self.lang(),
            self.egui_ctx.clone(),
        );
        self._worker = Some(handle);
    }

    /// Opens the "Clear database" confirmation modal (see
    /// `ui::dialogs::show_confirm_clear_database`). No-op while another job
    /// is running - the settings-dialog button is already disabled in that
    /// case, this is just the defensive mirror of `start_compact`.
    pub fn request_clear_database_confirmation(&mut self) {
        if self.busy {
            return;
        }
        self.confirm_clear_database = true;
    }

    pub fn cancel_clear_database_confirmation(&mut self) {
        self.confirm_clear_database = false;
    }

    /// Runs after the user confirms the "Clear database" modal: spawns the
    /// background wipe (see `worker::clear`). Destructive, hence the
    /// confirmation gate - unlike `start_compact`, this is never reachable
    /// without going through the modal first.
    pub fn confirm_clear_database_now(&mut self) {
        self.confirm_clear_database = false;
        if self.busy {
            return;
        }
        let Some(db_path) = self.db_path.clone() else {
            self.status_message = i18n::strings(self.lang()).no_db_path.to_string();
            return;
        };
        self.begin_job(false);
        self.progress = None;
        self.db_maint_active = true;
        self.db_maint_result = None;
        self.status_message = i18n::strings(self.lang()).clearing_database.to_string();
        let handle = worker::clear::spawn_clear(
            db_path,
            self.tx.clone(),
            self.lang(),
            self.egui_ctx.clone(),
        );
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
    /// the "at least one language stays checked" rule in `ui::settings`.
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

    /// Records whether the UAC-relaunch offer should stay suppressed across
    /// restarts, and persists it immediately, mirroring `set_delete_method`.
    ///
    /// Only ever set from the modal itself: the answer belongs where the
    /// question is asked, not in a settings screen the user would have to go
    /// looking for while the modal is in the way.
    pub fn set_never_ask_elevation(&mut self, never_ask: bool) {
        if self.settings.never_ask_elevation == never_ask {
            return;
        }
        self.settings = Settings {
            never_ask_elevation: never_ask,
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
    /// `ui::settings::scanning`. An empty list is otherwise a perfectly valid
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

    /// Applies a new excluded-library set and persists it immediately,
    /// mirroring `set_enabled_categories`. Takes effect on the *next* scan -
    /// the currently displayed findings (if any) are left untouched.
    /// Callers (the settings dialog) are responsible for never letting the
    /// last *included* library be excluded - see
    /// `ui::settings::scanning::show_libraries`.
    ///
    /// This is not `remove_manual_library`'s opposite number: it never
    /// touches the `game_libraries` row. An excluded library has to stay
    /// visible in Settings with its toggle off and survive a re-scan without
    /// re-entering the scanned set - vanishing from the list on exclude
    /// would just be Remove wearing a different label.
    pub fn set_excluded_libraries(&mut self, excluded_libraries: Vec<String>) {
        if self.settings.excluded_libraries == excluded_libraries {
            return;
        }
        self.settings = Settings {
            excluded_libraries,
            ..self.settings.clone()
        };
        self.persist_settings();
    }

    /// Sets whether to inspect and trim monolithic archives on the next scan.
    pub fn set_scan_monolithic_archives(&mut self, scan_monolithic_archives: bool) {
        if self.settings.scan_monolithic_archives == scan_monolithic_archives {
            return;
        }
        self.settings = Settings {
            scan_monolithic_archives,
            ..self.settings.clone()
        };
        self.persist_settings();
    }

    /// Switches the selection profile (selection profiles), persists it, and **re-applies**
    /// it to the currently displayed findings without re-scanning: every
    /// non-removed finding's checkbox is recomputed from the new profile,
    /// overwriting any manual tweaks (the point of a profile is to be a
    /// one-click policy). Removed items are left alone - they are already gone.
    /// The tree is not rebuilt: its shape is independent of selection, which is
    /// per-item state the tree reads live.
    ///
    /// Also becomes the scan default. Picking a profile from the main screen
    /// is a deliberate policy choice, and before the two fields were split it
    /// was the *only* thing a scan read - carrying it forward is what keeps
    /// that behaviour. Hand-editing a checkbox does not
    /// ([`Self::mark_selection_custom`] touches the live field alone): that
    /// says something about these findings, not about the next scan.
    pub fn set_selection_profile(&mut self, profile: SelectionProfile) {
        if self.settings.selection_profile == profile
            && self.settings.default_selection_profile == profile
        {
            return;
        }
        self.settings = Settings {
            selection_profile: profile,
            default_selection_profile: profile,
            ..self.settings.clone()
        };
        self.persist_settings();
        for item in &mut self.findings {
            if item.removed {
                continue;
            }
            item.selected = model::profile_auto_selects(
                profile,
                item.row.display_category(),
                item.row.confidence,
            ) && item.row.bulk_selectable();
        }
    }

    /// Records the profile the freshly arrived findings already follow.
    ///
    /// Separate from [`Self::set_selection_profile`] because that one also
    /// re-applies the profile to `self.findings`; here the caller has just
    /// built them from this very profile, so re-applying would be a wasted
    /// pass over the whole result set.
    fn set_live_selection_profile_silently(&mut self, profile: SelectionProfile) {
        if self.settings.selection_profile == profile {
            return;
        }
        self.settings = Settings {
            selection_profile: profile,
            ..self.settings.clone()
        };
        self.persist_settings();
    }

    /// Sets the profile a **future** scan pre-selects with.
    ///
    /// Deliberately persist-only: unlike [`Self::set_selection_profile`] it
    /// never touches `self.findings`, so changing it in Settings cannot look
    /// as though it silently rewrote the checkboxes the user is looking at.
    /// See [`Settings::default_selection_profile`].
    pub fn set_default_selection_profile(&mut self, profile: SelectionProfile) {
        if self.settings.default_selection_profile == profile {
            return;
        }
        self.settings = Settings {
            default_selection_profile: profile,
            ..self.settings.clone()
        };
        self.persist_settings();
    }

    /// Sets when the delete confirmation is shown - see [`ConfirmBehavior`]
    /// and [`Self::needs_delete_confirmation`].
    pub fn set_confirm_behavior(&mut self, behavior: ConfirmBehavior) {
        if self.settings.confirm_behavior == behavior {
            return;
        }
        self.settings = Settings {
            confirm_behavior: behavior,
            ..self.settings.clone()
        };
        self.persist_settings();
    }

    /// Applies the diagnostic-logging toggle and persists it immediately,
    /// mirroring `set_theme`. Unlike the other setters, this also has an
    /// immediate side effect beyond persistence: the logger itself is
    /// enabled/disabled right away (see `crate::logger::set_enabled`), so a
    /// toggle in the settings dialog starts (or stops) writing to
    /// `gametrimmer.log` the same frame rather than waiting for a restart.
    pub fn set_logging_enabled(&mut self, enabled: bool) {
        if self.settings.logging_enabled == enabled {
            return;
        }
        self.settings = Settings {
            logging_enabled: enabled,
            ..self.settings.clone()
        };
        self.persist_settings();

        if let Some(path) = self.log_path.as_deref() {
            logger::set_enabled(enabled, self.elevated, path);
        }
    }

    /// Sets whether background update monitoring is enabled.
    #[allow(dead_code)]
    pub fn set_watch_enabled(&mut self, enabled: bool) {
        if self.settings.watch_enabled == enabled {
            return;
        }
        self.settings = Settings {
            watch_enabled: enabled,
            ..self.settings.clone()
        };
        self.persist_settings();
    }

    /// Sets whether background update monitoring starts with Windows.
    #[allow(dead_code)]
    pub fn set_watch_autostart(&mut self, autostart: bool) {
        if self.settings.watch_autostart == autostart {
            return;
        }
        self.settings = Settings {
            watch_autostart: autostart,
            ..self.settings.clone()
        };
        self.persist_settings();
    }

    /// Sets the background monitoring action mode.
    #[allow(dead_code)]
    pub fn set_watch_mode(&mut self, mode: gametrimmer_core::settings::WatchMode) {
        if self.settings.watch_mode == mode {
            return;
        }
        self.settings = Settings {
            watch_mode: mode,
            ..self.settings.clone()
        };
        self.persist_settings();
    }

    /// Applies a whole new settings structure, persists it, and syncs autostart.
    #[allow(dead_code)]
    pub fn set_settings(&mut self, settings: Settings) {
        if self.settings == settings {
            return;
        }
        self.settings = settings;
        self.persist_settings();
    }

    /// Pings the watch companion daemon and caches its liveness state.
    #[allow(dead_code)]
    pub fn check_watch_daemon(&mut self) -> bool {
        let running = crate::ipc::ping_daemon(None);
        self.watch_daemon_running = running;
        self.last_ipc_poll = Some(std::time::Instant::now());
        running
    }

    /// Triggers an immediate rescan on the watch daemon.
    pub fn trigger_watch_rescan(&mut self) -> Result<String, String> {
        match crate::ipc::trigger_daemon_rescan(None) {
            Ok(crate::ipc::IpcResponse::Ok { message }) => {
                self.watch_daemon_running = true;
                Ok(message)
            }
            Ok(resp) => {
                self.watch_daemon_running = true;
                Ok(format!("{resp:?}"))
            }
            Err(e) => {
                self.watch_daemon_running = false;
                Err(e)
            }
        }
    }

    fn persist_settings(&mut self) {
        let lang = self.lang();
        let Some(settings_path) = self.settings_path.clone() else {
            let message = i18n::strings(lang).settings_not_saved_no_path.to_string();
            crate::logger::error(&message);
            self.record_settings_save(Err(message));
            return;
        };
        let result = gametrimmer_core::settings::save_file(&settings_path, &self.settings);

        // Synchronize autostart in the Windows registry. Kept synchronous,
        // unlike the daemon notify below: `RegSetValueEx` is a local kernel
        // call with no counterpart that can leave it hanging the way an
        // unresponsive daemon can leave a named-pipe read hanging - there is
        // no timeout to design around here, only an error to stop
        // discarding. Best-effort and logged rather than fed into
        // `record_settings_save` - a failure here is not a failure to save
        // the settings the user is looking at, and must not make the dialog
        // claim otherwise.
        if let Err(err) =
            gametrimmer_core::autostart::set_autostart(self.settings.watch_autostart, None)
        {
            crate::logger::error(&format!(
                "failed to sync watch_autostart to the Windows registry: {err}"
            ));
        }

        // Notify the watch daemon over IPC, off the UI thread and only when
        // a setting it actually cares about changed - see
        // `watch_relevant_settings_changed`. Every other settings save
        // (theme, delete method, confirm behavior, ...) used to pay for a
        // pipe round trip capped at `ipc::CLIENT_TIMEOUT` (2500ms) for a
        // daemon that had nothing to do with it; now the pipe is only
        // touched when there is something for the daemon to reload, and
        // never on the thread the window is painted from.
        if watch_relevant_settings_changed(&self.watch_synced_settings, &self.settings) {
            self.watch_synced_settings = self.settings.clone();
            // Nothing is captured: `reload_daemon_settings` takes no
            // settings payload of its own, it only tells the daemon to go
            // re-read `gametrimmer.ini` for itself, so there is nothing here
            // that needs to outlive `self`.
            std::thread::spawn(|| {
                if let Err(err) = crate::ipc::reload_daemon_settings(None) {
                    // Not shown to the user: the daemon simply not running is
                    // the ordinary state for anyone who has not turned on
                    // background monitoring, and this notify is best-effort
                    // by design (the daemon re-reads `gametrimmer.ini` itself
                    // on its own schedule regardless). Logged so a daemon
                    // that *is* running but wedged is still visible to
                    // whoever reads `gametrimmer.log`, rather than failing
                    // silently.
                    crate::logger::log(&format!(
                        "watch daemon settings-reload notify failed (daemon likely not running): {err}"
                    ));
                }
            });
        }

        match result {
            Ok(()) => self.record_settings_save(Ok(())),
            Err(err) => {
                let message = i18n::settings_save_failed(lang, err);
                crate::logger::error(&message);
                self.record_settings_save(Err(message));
            }
        }
    }

    /// Reports a failed user action on the status line, where the result of
    /// that action already appears when it succeeds.
    ///
    /// Adding a folder, removing a library and exporting are all things the
    /// user just asked for and is waiting on. Their failures used to go to a
    /// shared warnings list; with that list gone they need to land where the
    /// success message does, or a click that did nothing would look exactly
    /// like a click that worked.
    fn report_action_failure(&mut self, message: String) {
        crate::logger::error(&message);
        self.status_message = i18n::error_prefixed(self.lang(), message);
    }

    /// Records how the last settings write went, for the dialog to report.
    ///
    /// The failure is reported inside the dialog, where the change was made -
    /// before this, a setting that failed to save looked exactly like one
    /// that saved fine.
    fn record_settings_save(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.settings_save_error = None;
                self.settings_saved = true;
            }
            Err(message) => {
                self.settings_save_error = Some(message);
                self.settings_saved = false;
            }
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
            self.rebuild_tree();
            self.tree_dirty = false;
        }

        // `rules_io_active` keeps frames coming while a rules export/import
        // runs behind its native file dialog, so the result label appears
        // the moment the background thread reports back - not on the next
        // mouse move.
        if received_any || self.busy || self.rules_io_active || self.db_maint_active {
            ctx.request_repaint();
        }
    }

    /// `pub(crate)` so UI tests can drive a worker outcome straight into the
    /// app and assert what the window does with it, without a real worker.
    pub(crate) fn apply_message(&mut self, msg: WorkerMsg) {
        let lang = self.lang();
        match msg {
            WorkerMsg::GameUpdatedIpc {
                app_id,
                name,
                new_build_id,
                launcher,
            } => {
                crate::logger::log(&format!(
                    "Daemon reported game updated: {name} (app_id: {app_id}, launcher: {launcher}, build: {new_build_id:?})"
                ));
                self.status_message = format!("🔄 {} updated ({})", name, launcher);
                self.updated_games
                    .insert(app_id, new_build_id.unwrap_or_default());
                self.tree_dirty = true;
            }
            WorkerMsg::Status { text } => {
                // A phase without granular progress: drop any stale bar so the
                // spinner + this text is what shows.
                self.progress = None;
                self.status_message = text;
            }
            WorkerMsg::LibrariesFound { libraries, games } => {
                self.status_message = i18n::libraries_found(lang, libraries, games);
                // The scan has just committed the discovered libraries to
                // `game_libraries` (this message is sent right after
                // `persist_libraries`). Reload the in-memory list the panel
                // renders from, so newly found libraries appear immediately
                // rather than only after the next app restart. Safe to read
                // here while the scan's writer thread runs: under WAL a reader
                // takes the last committed snapshot without blocking the
                // writer.
                self.refresh_libraries();
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
            WorkerMsg::ScanPhaseProgress(phase_progress) => {
                let state = self
                    .scan_phase_state
                    .get_or_insert_with(ScanPhaseState::default);
                match phase_progress {
                    gametrimmer_core::worker::WorkerProgress::ScanPhase1 {
                        current,
                        total,
                        game_name,
                    } => {
                        state.phase1 = Some(PhaseProgress {
                            current,
                            total,
                            detail: game_name,
                            extra_count: 0,
                        });
                        let frac = if total > 0 {
                            current as f32 / total as f32
                        } else {
                            0.0
                        };
                        state.overall_fraction = (frac * 0.33).clamp(0.0, 1.0);
                        state.overall_message = format!("{}/{}", current, total);
                    }
                    gametrimmer_core::worker::WorkerProgress::ScanPhase2 {
                        current,
                        total,
                        file_name,
                        findings_count,
                    } => {
                        state.phase2 = Some(PhaseProgress {
                            current,
                            total,
                            detail: file_name,
                            extra_count: findings_count,
                        });
                        let frac = if total > 0 {
                            current as f32 / total as f32
                        } else {
                            0.0
                        };
                        state.overall_fraction = (0.33 + frac * 0.34).clamp(0.0, 1.0);
                        state.overall_message = format!("{}/{}", current, total);
                    }
                    gametrimmer_core::worker::WorkerProgress::ScanPhase3 {
                        current,
                        total,
                        archive_name,
                        monoliths_count,
                    } => {
                        state.phase3 = Some(PhaseProgress {
                            current,
                            total,
                            detail: archive_name,
                            extra_count: monoliths_count,
                        });
                        let frac = if total > 0 {
                            current as f32 / total as f32
                        } else {
                            0.0
                        };
                        state.overall_fraction = (0.67 + frac * 0.33).clamp(0.0, 1.0);
                        state.overall_message = format!("{}/{}", current, total);
                    }
                    gametrimmer_core::worker::WorkerProgress::OverallProgress {
                        fraction,
                        message,
                    } => {
                        state.overall_fraction = fraction.clamp(0.0, 1.0);
                        state.overall_message = message;
                    }
                }
            }
            WorkerMsg::Done {
                findings,
                scan_summary,
                occupancy,
                timing,
                routing_breakdown,
            } => {
                // A rules import between one scan and the next changes what
                // the descriptions resolve to, so the index is rebuilt with
                // the results rather than only when the language changes.
                self.refresh_descriptions();
                self.end_job();
                self.progress = None;
                self.scan_phase_state = None;
                self._worker = None;
                self.last_scan_timing = timing;
                self.last_routing_breakdown = routing_breakdown;
                let count = findings.len();
                // selection profiles: a persisted profile decides which findings arrive
                // pre-checked (see `model::profile_auto_selects`), not a bare
                // confidence threshold. The *default* profile is the one that
                // applies here - `selection_profile` describes the tree being
                // replaced, and may well have drifted to `Custom` through
                // hand-edits that say nothing about the new results.
                let profile = self.settings.default_selection_profile;
                // ... so the live profile is reset to match what was just
                // applied, rather than left claiming the previous scan's.
                self.set_live_selection_profile_silently(profile);
                self.findings = findings
                    .into_iter()
                    .map(|row| {
                        let selected = model::profile_auto_selects(
                            profile,
                            row.display_category(),
                            row.confidence,
                        ) && row.bulk_selectable();
                        FindingItem {
                            row,
                            selected,
                            removed: false,
                        }
                    })
                    .collect();
                self.occupancy = occupancy;
                self.rebuild_tree();
                // A fresh scan means a fresh tree shape - stale toggle keys
                // (folders/categories that no longer exist, or now mean
                // something else) must not leak into it, and the keyboard
                // cursor's row index no longer points at the same row.
                self.tree_toggles.clear();
                self.tree_cursor = None;
                // A fresh result set means the previous plan-card filter (if
                // any) may no longer have matching findings - start unfiltered.
                self.tree_category_filter = None;
                // Same for the name search: its match index is keyed by
                // position in the old findings list, so it is meaningless now.
                self.clear_search();
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
            WorkerMsg::RemoveDone {
                outcomes,
                occupancy,
                method,
            } => {
                self._worker = None;
                // The deleted files' rows were purged, so the footprint
                // shrank - adopt the freshly recomputed snapshot.
                self.occupancy = occupancy;
                // A finished delete must not leave the last file's progress
                // bar stuck on screen while the auto-compaction below runs
                // (which reports its own status via `busy` + a spinner, not
                // `progress`) - mirrors the scan `Done` arm.
                self.progress = None;

                // On-disk space accounting for the honest "freed X of expected
                // Y" summary (allocated-size accounting) - a pure tally so it stays unit-testable
                // outside the app; see `worker::delete::space_tally`.
                let space = worker::delete::space_tally(method, &outcomes);

                let mut succeeded = 0usize;
                let mut nuked = 0usize;
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
                        // A recycle that Windows turned into a permanent delete
                        // (over-quota) is still a "success" for tree/DB
                        // purposes, but the summary reports it as permanently
                        // deleted, not recoverable - see `RemoveOutcome::nuked`.
                        if outcome.nuked {
                            nuked += 1;
                        }
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
                self.rebuild_tree();
                self.tree_dirty = false;
                self.status_message = i18n::remove_done_status(lang, succeeded, failed.len());
                self.remove_summary = Some(RemoveSummary {
                    succeeded,
                    nuked,
                    failed,
                    method,
                    expected_bytes: space.expected,
                    freed_bytes: space.freed,
                    recycled_pending_bytes: space.recycled_pending,
                });

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
                        self.end_job();
                    }
                } else {
                    self.end_job();
                }
            }
            WorkerMsg::Cancelled => {
                // A cancelled bundle wrote nothing, so the dialog's own
                // result stays empty rather than claiming a failure.
                self.bundle_active = false;
                self.end_job();
                self.progress = None;
                self.scan_phase_state = None;
                self._worker = None;
                self.status_message = i18n::strings(lang).scan_cancelled.to_string();
            }
            WorkerMsg::BundleDone { path, error } => {
                self.bundle_active = false;
                self.end_job();
                self.progress = None;
                self.scan_phase_state = None;
                self._worker = None;
                self.bundle_result = match (&path, error) {
                    (_, Some(error)) => {
                        crate::logger::error(&error);
                        Some(Err(error))
                    }
                    (Some(path), None) => {
                        let message = i18n::bundle_saved_to(lang, path.display());
                        self.status_message = message.clone();
                        Some(Ok(message))
                    }
                    // Both `None`: the user closed the save dialog.
                    (None, None) => None,
                };
            }
            WorkerMsg::Error { msg } => {
                // Not logged here. `msg` arrives already rendered in the
                // interface language, so logging it would put Ukrainian in
                // the log on a Ukrainian install; the English rendering is
                // written by `worker::scan::send_error`, which is the last
                // place both languages are still available.
                self.end_job();
                self.progress = None;
                self.scan_phase_state = None;
                self._worker = None;
                self.status_message = i18n::error_prefixed(lang, msg);
            }
            // `Warning` carries a scan-time diagnostic that is for whoever
            // reads a bug report - an app id or a manifest field name tells
            // the person at the window nothing they can act on. It is
            // logged by `worker::scan::send_warning`, for the same reason
            // the `Error` arm above no longer logs.
            WorkerMsg::Warning { msg: _ } => {}
            WorkerMsg::FolderPicked { path } => {
                self.folder_picker_active = false;
                if let Some(path) = path {
                    self.add_manual_library(path);
                }
            }
            WorkerMsg::ExportDone { path, error } => {
                self.export_active = false;
                if let Some(error) = error {
                    self.report_action_failure(i18n::export_save_failed(lang, error));
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
                    crate::logger::error(&msg);
                    self.rules_io_result = Some(Err(msg));
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
                    crate::logger::error(&msg);
                    self.rules_io_result = Some(Err(msg));
                } else if let Some(summary) = summary {
                    self.rules_io_result = Some(Ok(summary.clone()));
                    self.status_message = summary;
                }
                // Both `None`: the file picker was cancelled.
            }
            WorkerMsg::CompactDone { error, skipped } => {
                self.end_job();
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
                // Mirror the outcome into the settings dialog when the user
                // started this compaction there (the top-bar status is hidden
                // behind the modal). A skipped compaction still counts as a
                // successful "done, nothing worth reclaiming".
                if self.db_maint_active {
                    self.db_maint_active = false;
                    self.db_maint_result = Some(match &error {
                        Some(err) => Err(err.clone()),
                        None => Ok(s.database_compacted.to_string()),
                    });
                }
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
            WorkerMsg::ClearDone { error } => {
                self.end_job();
                self._worker = None;
                self.progress = None;
                // Show the outcome inside the settings dialog (where the
                // "Clear database" button lives and the top-bar status is
                // hidden) - without this the wipe finishes invisibly and the
                // user, seeing nothing, clicks the button again.
                if self.db_maint_active {
                    self.db_maint_active = false;
                    self.db_maint_result = Some(match &error {
                        Some(err) => Err(err.clone()),
                        None => Ok(i18n::strings(lang).database_cleared.to_string()),
                    });
                }
                match error {
                    Some(err) => self.status_message = err,
                    None => {
                        // Reset to the same empty state a fresh install
                        // shows before the first scan - the database no
                        // longer has any findings to display, so neither
                        // should the UI.
                        self.findings = Vec::new();
                        self.occupancy = model::Occupancy::default();
                        self.tree = Vec::new();
                        self.tree_dirty = false;
                        self.tree_toggles.clear();
                        self.tree_cursor = None;
                        self.clear_search();
                        self.remove_summary = None;
                        self.last_scan_timing = None;
                        self.status_message = i18n::strings(lang).database_cleared.to_string();
                    }
                }
            }
        }
    }
}

/// Whether `old` and `new` differ on a setting the watch daemon's
/// `ReloadSettings` handler has any use for, deciding whether
/// `persist_settings` bothers notifying it at all.
///
/// `watch_enabled` and `watch_mode` describe whether and how the daemon
/// should act; `excluded_libraries` describes which of the registered
/// libraries it should leave alone. Deliberately conservative (compares
/// three fields, not "did anything change") - the alternative was notifying
/// on every save regardless of content, which is the exact waste this
/// exists to avoid. Everything else in [`Settings`] (theme, delete method,
/// selection profile, logging, ...) is either purely a UI/scan concern the
/// daemon never reads, or - like `watch_autostart` - governs the Windows
/// registry entry that decides whether the daemon gets *launched* at boot,
/// not anything the already-running daemon needs to hear about over IPC.
///
/// Note: as of this writing the daemon's `ReloadSettings` handler
/// (`gametrimmer_watch::main`) does not actually consult `excluded_libraries`
/// when re-enumerating directories to watch (see
/// `gametrimmer_watch::watcher::discover_watch_directories`, which reads the
/// `game_libraries` table directly and applies no exclusion filter) - it is
/// included here on the strength of what the field means, not what the
/// daemon currently does with it, so this stays correct if that gap is
/// closed without anyone having to remember to widen this comparison too.
fn watch_relevant_settings_changed(old: &Settings, new: &Settings) -> bool {
    old.watch_enabled != new.watch_enabled
        || old.watch_mode != new.watch_mode
        || old.excluded_libraries != new.excluded_libraries
}

/// Gathers the per-volume media-kind data `scan_route::should_offer_elevation`
/// needs, then asks it whether the startup UAC-relaunch modal is worth
/// showing. The impure half of that decision: resolving each library's drive
/// letter and probing it with `mftscan::media_kind` costs one
/// `DeviceIoControl` call per *distinct* volume - cheap, and (unlike the raw
/// MFT read the elevated scan itself performs) requires no Administrator
/// rights, so this can run synchronously in `new()` for every startup that
/// isn't already elevated.
///
/// Only called when `!elevated` - an elevated startup never shows the modal
/// at all (see `show_elevation_prompt`'s doc comment), so there is nothing to
/// compute in that case.
fn compute_show_elevation_prompt(never_ask: bool, libraries: &[LibraryRow]) -> bool {
    // A standing refusal is checked before anything is probed: the answer
    // cannot change, so the `DeviceIoControl` calls would be pure cost.
    if never_ask {
        return false;
    }

    let mut letters: Vec<char> = libraries
        .iter()
        .filter_map(|library| mftscan::volume_letter(&library.path))
        .collect();
    letters.sort_unstable();
    letters.dedup();

    let volume_media: Vec<(char, mftscan::MediaKind)> = letters
        .into_iter()
        .map(|letter| (letter, mftscan::media_kind(letter)))
        .collect();

    scan_route::should_offer_elevation(&volume_media)
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
        // eframe's built-in persistence is deliberately disabled (it would
        // write window state to %APPDATA%; guarded by
        // `cargo_lock_does_not_pull_in_eframe_persistence_deps` in
        // `gametrimmer_core`), so this app owns "did the theme change"
        // itself: applying the current setting every frame is
        // cheap (it just writes an enum into egui's in-memory options) and
        // means a change from the settings dialog takes effect the same
        // frame with no separate "apply" step to forget.
        ctx.set_theme(theme_preference(self.settings.theme));
        self.drain_messages(&ctx);

        ui::top_bar::show(self, ui);
        ui::bottom_bar::show(self, ui);
        // The plan-card strip is rendered inside the tree region (at the top of
        // its central panel), so the tree is always visible directly below the
        // cards - see `ui::tree_view::show`. It is deliberately not a separate
        // top panel: a second top panel starved the central tree panel of
        // height, hiding the tree entirely (the plan-action filtering regression this fixes).
        ui::tree_view::show(self, ui);
        ui::dialogs::show(self, ui);
        ui::settings::show(self, ui);
    }
}

#[cfg(test)]
impl GameTrimmerApp {
    /// Builds an app for a test: its own throwaway database and ini inside
    /// `dir`, and no previous-scan autoload thread. See [`Self::new_with`] for
    /// why these matter. The caller keeps the `TempDir` alive for as long as
    /// the app is used - dropping it deletes both files out from under it.
    pub fn new_for_test(dir: &std::path::Path) -> Self {
        let mut app = Self::new_with(
            egui::Context::default(),
            Some(dir.join("gametrimmer.db")),
            Some(dir.join("gametrimmer.ini")),
            None,
            false,
        );
        // Pin the machine's answer. The default preference is "follow
        // Windows", so without this every assertion about UI text would
        // depend on the developer's own Windows language - green here, red on
        // a Ukrainian desktop, for no reason to do with the code. Tests that
        // are *about* the system language set this themselves.
        app.system_lang = Lang::En;
        app
    }

    /// Overrides what "the system language" is, for the tests that are about
    /// exactly that. Everything else keeps the pinned default from
    /// [`Self::new_for_test`].
    pub fn set_system_language_for_test(&mut self, lang: Lang) {
        self.system_lang = lang;
    }

    /// Gives the test app a log path *without* opening the file.
    ///
    /// `new_for_test` deliberately passes `None`, because `new_with` opens
    /// whatever path it is given - and the logger's open file is process
    /// global, so every UI test would be fighting the others for it. The
    /// settings dialog only needs the path to render, never the handle, so
    /// this sets the one and not the other.
    pub fn set_log_path_for_test(&mut self, path: std::path::PathBuf) {
        self.log_path = Some(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the `new_with` seam: a test app must not touch the
    /// database and ini `new()` would resolve to. Under `cargo test` those
    /// paths live in `target/debug/deps/`, shared by every test and surviving
    /// between runs.
    #[test]
    fn test_app_uses_its_own_files_and_leaves_production_paths_alone() {
        let production_db = worker::db_path().expect("resolve production db path");
        let production_ini = worker::settings_path().expect("resolve production ini path");
        let production_log = worker::log_path().expect("resolve production log path");
        let existed_before = (
            production_db.exists(),
            production_ini.exists(),
            production_log.exists(),
        );

        let dir = tempfile::tempdir().expect("create temp dir");
        let app = GameTrimmerApp::new_for_test(dir.path());

        assert_eq!(
            app.db_path.as_deref(),
            Some(dir.path().join("gametrimmer.db").as_path()),
            "the test app database should live inside its temp dir",
        );
        assert_eq!(
            app.settings_path.as_deref(),
            Some(dir.path().join("gametrimmer.ini").as_path()),
            "the test app ini should live inside its temp dir",
        );
        assert_eq!(app.log_path, None, "a test app must not own a global log");
        assert_eq!(
            (
                production_db.exists(),
                production_ini.exists(),
                production_log.exists(),
            ),
            existed_before,
            "building a test app must not create production database, ini or log files",
        );
    }

    /// Two apps built at the same time must not share a file. If they did,
    /// tests would pass or fail depending on which one won the race.
    #[test]
    fn concurrently_built_test_apps_do_not_share_a_database() {
        let dirs: Vec<tempfile::TempDir> = (0..4)
            .map(|_| tempfile::tempdir().expect("create temp dir"))
            .collect();

        let paths: Vec<Option<PathBuf>> = std::thread::scope(|scope| {
            let handles: Vec<_> = dirs
                .iter()
                .map(|dir| scope.spawn(|| GameTrimmerApp::new_for_test(dir.path()).db_path))
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("build app on worker thread"))
                .collect()
        });

        let unique: std::collections::HashSet<&PathBuf> = paths.iter().flatten().collect();
        assert_eq!(unique.len(), dirs.len(), "each app needs its own database");
    }

    /// settings/cache separation's user-facing contract: the scan cache is disposable, while the
    /// preferences in the sibling ini are not. Reopening after deleting the
    /// database must therefore restore every setting from the ini.
    #[test]
    fn deleting_the_database_does_not_reset_settings() {
        let dir = tempfile::tempdir().expect("create temp dir");
        {
            let mut app = GameTrimmerApp::new_for_test(dir.path());
            app.set_language(LanguagePreference::Fixed(Lang::Uk));
            app.set_theme(Theme::Dark);
            app.set_delete_method(DeleteMethod::RecycleBin);
            // Persist an explicit opt-out without touching the process-global
            // logger; reopening must not overwrite it with the new default.
            app.settings.logging_enabled = false;
            app.persist_settings();
            assert!(dir.path().join("gametrimmer.ini").exists());
        }

        std::fs::remove_file(dir.path().join("gametrimmer.db"))
            .expect("delete disposable scan database");
        let reopened = GameTrimmerApp::new_for_test(dir.path());

        assert_eq!(
            reopened.settings.app_language,
            LanguagePreference::Fixed(Lang::Uk)
        );
        assert_eq!(reopened.settings.theme, Theme::Dark);
        assert_eq!(reopened.settings.delete_method, DeleteMethod::RecycleBin);
        assert!(!reopened.settings.logging_enabled);
    }

    /// Runs `body` with the process-global logger pointed at a throwaway file
    /// and hands back what was written, then unconditionally disables logging
    /// again - including when `body` panics, since the guard drops either way
    /// and a leaked open handle would follow the next test into a temp dir
    /// that no longer exists.
    ///
    /// Takes [`logger::lock_for_test`] because `STATE` is one global: without
    /// it, a `logger` unit test running in parallel would swap the file out
    /// from under these assertions.
    fn captured_log(body: impl FnOnce(&std::path::Path)) -> String {
        logger::captured_for_test(body)
    }

    /// GT-115, now split with GT-127. A fatal worker error still has to
    /// reach the window - the log half moved to `worker::scan::send_error`,
    /// which is the last place the message exists in both languages, and is
    /// asserted there.
    #[test]
    fn a_fatal_worker_error_reaches_the_window() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mut app = GameTrimmerApp::new_for_test(dir.path());

        app.apply_message(WorkerMsg::Error {
            msg: "gt_probe_scan_blew_up".to_string(),
        });

        assert!(
            app.status_message.contains("gt_probe_scan_blew_up"),
            "{}",
            app.status_message,
        );
    }

    /// GT-115, the other half. A database that never opened leaves no scan
    /// trail either, so the log is the only artifact that user has - and it
    /// used to hold nothing but the session header.
    #[test]
    fn a_failed_database_open_reaches_the_log() {
        let contents = captured_log(|dir| {
            // A database inside a directory that does not exist: SQLite
            // cannot create the file, which is a plain `CANTOPEN` rather
            // than anything platform-specific.
            let db_path = dir.join("no_such_directory").join("gametrimmer.db");
            let app = GameTrimmerApp::new_with(
                egui::Context::default(),
                Some(db_path),
                Some(dir.join("gametrimmer.ini")),
                Some(dir.join("gametrimmer.log")),
                false,
            );
            assert!(
                app.db_error.is_some(),
                "the window must still report it too",
            );
        });

        assert!(
            contents.contains("Failed to open the database"),
            "the startup database error should be in the log: {contents}",
        );
    }

    /// GT-74, half B. `db_path` is assigned unconditionally in `new_with`
    /// regardless of whether `db::open` succeeded, so without the
    /// `blocked_by_database` gate `start_scan` would happily hand a dead path
    /// to the worker - reaching the worker's own `db::open` failure and
    /// producing the exact duplicate the ticket is about.
    #[test]
    fn start_scan_does_nothing_when_the_database_never_opened() {
        let dir = tempfile::tempdir().expect("create temp dir");
        // Same fixture as `a_failed_database_open_reaches_the_log`: a
        // directory that does not exist, so SQLite cannot create the file.
        let db_path = dir.path().join("no_such_directory").join("gametrimmer.db");
        let mut app = GameTrimmerApp::new_with(
            egui::Context::default(),
            Some(db_path),
            Some(dir.path().join("gametrimmer.ini")),
            None,
            false,
        );
        assert!(app.db_error.is_some(), "fixture must actually fail to open");
        // Isolate the database gate from the disclaimer gate, which has its
        // own test above.
        app.accept_disclaimer();

        app.start_scan();

        assert!(
            !app.busy,
            "a scan started against a database that never opened",
        );
        assert!(
            app._worker.is_none(),
            "no worker should have been spawned for a dead database",
        );
    }

    /// `autoload = false` is what keeps the widget tree deterministic: with
    /// the loader thread running, an assertion can land before or after
    /// `WorkerMsg::Done` swaps the findings in.
    #[test]
    fn test_app_does_not_spawn_the_previous_scan_loader() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let app = GameTrimmerApp::new_for_test(dir.path());

        assert!(
            app._worker.is_none(),
            "no background worker should be spawned"
        );
        assert!(!app.busy, "a freshly built test app should be idle");
        assert!(app.findings.is_empty(), "a fresh database has no findings");
    }

    /// A fresh temp database must open cleanly - otherwise every harness
    /// test would silently be asserting against a dialog showing a database
    /// error banner instead of the real thing.
    #[test]
    fn test_app_opens_its_database_without_error() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let app = GameTrimmerApp::new_for_test(dir.path());

        assert_eq!(app.db_error, None, "temp database should open cleanly");
        assert!(
            app.libraries.is_empty(),
            "a fresh database has no libraries, so no volume probe runs",
        );
        assert!(
            !app.show_elevation_prompt,
            "with no libraries there is nothing to offer elevation for",
        );
    }

    /// One finding of `size` bytes, selected and not yet removed.
    fn app_with_one_selected_finding(dir: &std::path::Path, size: u64) -> GameTrimmerApp {
        let mut app = GameTrimmerApp::new_for_test(dir);
        // A user who already has findings is one who got past the first-run
        // screen; without this every delete path here would be testing the
        // disclaimer gate instead of the policy it is about. The gate has its
        // own test below.
        app.accept_disclaimer();
        app.findings = vec![FindingItem {
            row: model::FindingRow {
                file_id: 1,
                game_id: 1,
                game_name: "Test Game".to_string(),
                app_id: None,
                install_dir: PathBuf::from("C:\\Games\\Test"),
                rel_path: "data/loc.pak".to_string(),
                size,
                size_on_disk: size,
                source: model::FindingSource::Loc(gametrimmer_core::langdetect::LangKind::Text),
                rule_desc: "test rule".to_string(),
                confidence: 90,
                lang_tag: Some("de".to_string()),
                group_dir: None,
                deletion_block_reason: None,
                imported_untrusted: false,
                library: None,
                action: gametrimmer_core::models::FindingAction::DirectDelete,
                anti_cheat_protected: false,
                monolith_badge: None,
            },
            selected: true,
            removed: false,
        }];
        // Keeps the search corpus in step with the findings, exactly as
        // `WorkerMsg::Done` does for a real result set.
        app.clear_search();
        app
    }

    /// The confirmation policy, exercised without letting a delete run - the
    /// decision and the consequence are separate on purpose (see
    /// `needs_delete_confirmation`).
    ///
    /// Batch sizes are varied deliberately even though the setting no longer
    /// looks at them: the retired "only above 1 GB" option keyed off the batch
    /// total (which is not what its label said), and this is what states that
    /// size has stopped being an input.
    #[test]
    fn the_confirmation_policy_ignores_the_size_of_the_batch() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let one_gb: u64 = 1024 * 1024 * 1024;

        for (behavior, size, expected) in [
            (ConfirmBehavior::Always, 1_u64, true),
            (ConfirmBehavior::Always, one_gb, true),
            (ConfirmBehavior::Never, 1, false),
            (ConfirmBehavior::Never, one_gb, false),
        ] {
            let mut app = app_with_one_selected_finding(dir.path(), size);
            app.set_confirm_behavior(behavior);

            assert_eq!(
                app.needs_delete_confirmation(),
                expected,
                "{behavior:?} on a {size}-byte batch",
            );
        }
    }

    /// The default policy must still be the one that asks - a delete is not
    /// something to leave one accidental click away from silent.
    #[test]
    fn a_fresh_app_asks_before_deleting() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mut app = app_with_one_selected_finding(dir.path(), 1);

        app.request_delete_confirmation();

        assert!(
            app.confirm_delete.is_some(),
            "the default policy skipped the confirmation",
        );
    }

    /// The disclaimer gate, asserted at the action rather than at the button.
    /// A greyed-out control is the polite half of this; the half that has to
    /// hold is the one a keyboard shortcut or a future call site cannot walk
    /// around.
    #[test]
    fn nothing_is_scanned_or_deleted_before_the_disclaimer_is_accepted() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mut app = app_with_one_selected_finding(dir.path(), 1);
        // Back to a user who has not agreed to anything - the fixture accepts
        // for the benefit of every other test in this module.
        app.settings = Settings {
            disclaimer_accepted: false,
            ..app.settings.clone()
        };

        app.request_delete_confirmation();
        assert!(
            app.confirm_delete.is_none(),
            "a delete was offered before the disclaimer was accepted",
        );

        app.start_scan();
        assert!(
            !app.busy,
            "a scan started before the disclaimer was accepted"
        );
        assert!(
            !app.settings.has_scanned,
            "the refused scan still counted as a first run",
        );

        app.accept_disclaimer();
        app.request_delete_confirmation();
        assert!(
            app.confirm_delete.is_some(),
            "accepting the disclaimer did not unblock the delete",
        );
    }

    /// The other direction of the split. Before the two fields existed, the
    /// main-screen picker was the only thing a scan read; that has to keep
    /// working, or picking a profile silently stops affecting the next scan.
    #[test]
    fn picking_a_profile_on_the_main_screen_becomes_the_scan_default() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mut app = app_with_one_selected_finding(dir.path(), 1);

        app.set_selection_profile(SelectionProfile::Aggressive);

        assert_eq!(
            app.settings.default_selection_profile,
            SelectionProfile::Aggressive,
        );
    }

    /// But a hand-edited checkbox says nothing about the next scan, so it
    /// must not drag the default to `Custom` with it.
    #[test]
    fn hand_editing_the_tree_leaves_the_scan_default_alone() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mut app = app_with_one_selected_finding(dir.path(), 1);
        app.set_selection_profile(SelectionProfile::Balanced);

        app.mark_selection_custom();

        assert_eq!(app.settings.selection_profile, SelectionProfile::Custom);
        assert_eq!(
            app.settings.default_selection_profile,
            SelectionProfile::Balanced,
        );
    }

    /// Editing the scan default must not disturb the tree on screen: that
    /// silent re-check is what the split into two fields exists to prevent.
    #[test]
    fn changing_the_scan_default_leaves_the_current_selection_alone() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mut app = app_with_one_selected_finding(dir.path(), 1);
        app.mark_selection_custom();

        app.set_default_selection_profile(SelectionProfile::Aggressive);

        assert!(app.findings[0].selected, "the tree's checkbox moved");
        assert_eq!(
            app.settings.selection_profile,
            SelectionProfile::Custom,
            "the live profile followed the scan default",
        );
        assert_eq!(
            app.settings.default_selection_profile,
            SelectionProfile::Aggressive,
        );
    }

    /// A save that touches nothing the daemon reads (theme, in this case)
    /// must not be reported as worth notifying it over - this is the
    /// property that keeps `persist_settings` from paying for a pipe round
    /// trip on every settings change.
    #[test]
    fn watch_relevant_settings_changed_ignores_unrelated_fields() {
        let old = Settings::default();
        let new = Settings {
            theme: Theme::Dark,
            delete_method: DeleteMethod::RecycleBin,
            logging_enabled: !old.logging_enabled,
            ..old.clone()
        };
        assert_ne!(old.delete_method, new.delete_method);

        assert!(!watch_relevant_settings_changed(&old, &new));
    }

    /// `watch_enabled` flips whether the daemon should be doing anything at
    /// all - the daemon must be told.
    #[test]
    fn watch_relevant_settings_changed_true_for_watch_enabled() {
        let old = Settings::default();
        let new = Settings {
            watch_enabled: !old.watch_enabled,
            ..old.clone()
        };

        assert!(watch_relevant_settings_changed(&old, &new));
    }

    /// `watch_mode` decides how the daemon reacts to an update - also worth
    /// telling it about.
    #[test]
    fn watch_relevant_settings_changed_true_for_watch_mode() {
        let old = Settings::default();
        let new = Settings {
            watch_mode: gametrimmer_core::settings::WatchMode::AutoTrim,
            ..old.clone()
        };
        assert_ne!(old.watch_mode, new.watch_mode);

        assert!(watch_relevant_settings_changed(&old, &new));
    }

    /// `excluded_libraries` names which registered libraries the daemon
    /// should leave alone - also worth telling it about, even though (see
    /// `watch_relevant_settings_changed`'s doc comment) the daemon does not
    /// currently act on it.
    #[test]
    fn watch_relevant_settings_changed_true_for_excluded_libraries() {
        let old = Settings::default();
        let new = Settings {
            excluded_libraries: vec!["c:\\games\\somelib".to_string()],
            ..old.clone()
        };

        assert!(watch_relevant_settings_changed(&old, &new));
    }

    /// Identical settings (the degenerate case of a save that changed
    /// nothing at all) must never be reported as worth a notify.
    #[test]
    fn watch_relevant_settings_changed_false_for_identical_settings() {
        let settings = Settings::default();

        assert!(!watch_relevant_settings_changed(&settings, &settings));
    }
}
