//! Hand-rolled i18n with dynamic community locale support.
//! Plain text lives in [`Strings`]; anything with a count, a path, or an error
//! is a function in [`messages`].
//!
//! External JSON locales in `locales/*.json` (portable root or AppData)
//! are discovered dynamically and merged over the English baseline.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

mod en;
mod messages;
mod system;

pub use gametrimmer_core::settings::Lang;
pub use messages::*;
pub use system::detect as detect_system_language;

#[allow(dead_code)]
pub const EMBEDDED_LOCALE_EN: &str = include_str!("../../../../locales/en.json");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocaleInfo {
    pub id: String,
    pub name: String,
    pub native_name: String,
    pub is_builtin: bool,
}

#[derive(serde::Deserialize)]
struct LocaleHeader {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    native_name: String,
    #[serde(default)]
    strings: HashMap<String, String>,
}

static LOCALE_CACHE: OnceLock<RwLock<HashMap<String, &'static Strings>>> = OnceLock::new();

fn get_locale_cache() -> &'static RwLock<HashMap<String, &'static Strings>> {
    LOCALE_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn scan_dir_locales(dir: &std::path::Path, map: &mut HashMap<String, LocaleInfo>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("json") {
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if file_name.ends_with(".template.json") || file_name.ends_with(".schema.json") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(header) = serde_json::from_str::<LocaleHeader>(&content) {
                    if !header.id.is_empty() {
                        let id = header.id.to_lowercase();
                        let is_builtin = id == "en";
                        let name = if header.name.is_empty() {
                            id.clone()
                        } else {
                            header.name
                        };
                        let native_name = if header.native_name.is_empty() {
                            name.clone()
                        } else {
                            header.native_name
                        };
                        map.insert(
                            id.clone(),
                            LocaleInfo {
                                id,
                                name,
                                native_name,
                                is_builtin,
                            },
                        );
                    }
                }
            }
        }
    }
}

pub fn available_locales() -> Vec<LocaleInfo> {
    let mut map = HashMap::new();

    // Built-in
    map.insert(
        "en".to_string(),
        LocaleInfo {
            id: "en".to_string(),
            name: "English".to_string(),
            native_name: "English".to_string(),
            is_builtin: true,
        },
    );

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            scan_dir_locales(&exe_dir.join("locales"), &mut map);
        }
    }
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        let p = std::path::PathBuf::from(local_app_data)
            .join("GameTrimmer")
            .join("locales");
        scan_dir_locales(&p, &mut map);
    }
    scan_dir_locales(std::path::Path::new("locales"), &mut map);

    let mut list: Vec<LocaleInfo> = map.into_values().collect();
    list.sort_by(|a, b| match (a.id.as_str(), b.id.as_str()) {
        ("en", _) => std::cmp::Ordering::Less,
        (_, "en") => std::cmp::Ordering::Greater,
        _ => a.id.cmp(&b.id),
    });
    list
}

fn load_external_strings(id: &str) -> Option<Strings> {
    let file_name = format!("{id}.json");

    let candidate_paths = [
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("locales").join(&file_name))),
        std::env::var_os("LOCALAPPDATA").map(|l| {
            std::path::PathBuf::from(l)
                .join("GameTrimmer")
                .join("locales")
                .join(&file_name)
        }),
        Some(std::path::PathBuf::from("locales").join(&file_name)),
    ];

    for path_opt in candidate_paths.into_iter().flatten() {
        if path_opt.exists() {
            if let Ok(content) = std::fs::read_to_string(&path_opt) {
                if let Ok(header) = serde_json::from_str::<LocaleHeader>(&content) {
                    return Some(en::STRINGS.apply_overrides(&header.strings));
                }
            }
        }
    }
    None
}

/// Returns the string table for `lang`.
/// Falls back to embedded English for missing keys/locales.
pub fn strings(lang: Lang) -> &'static Strings {
    let tag = lang.as_str();

    let cache = get_locale_cache();
    {
        let r = cache.read().unwrap();
        if let Some(&ptr) = r.get(tag) {
            return ptr;
        }
    }

    if let Some(loaded) = load_external_strings(tag) {
        let leaked: &'static Strings = Box::leak(Box::new(loaded));
        let mut w = cache.write().unwrap();
        w.insert(tag.to_string(), leaked);
        return leaked;
    }

    &en::STRINGS
}

#[derive(Debug, Clone, Copy)]
pub struct Strings {
    // -- top_bar --
    pub btn_scan_libraries: &'static str,
    pub btn_cancel: &'static str,
    pub btn_export: &'static str,
    pub btn_settings: &'static str,

    // -- bottom_bar --
    pub btn_select_all: &'static str,
    pub btn_deselect_all: &'static str,
    pub btn_delete_selected: &'static str,

    // Left-hand navigation of the rebuilt settings dialog, in listed order,
    // plus its footer.
    pub settings_section_general: &'static str,
    pub settings_section_monitoring: &'static str,
    pub settings_section_scanning: &'static str,
    pub settings_section_selection: &'static str,
    pub settings_section_rules: &'static str,
    pub settings_section_data: &'static str,
    pub watch_enabled_label: &'static str,
    pub watch_enabled_hint: &'static str,
    pub watch_autostart_label: &'static str,
    pub watch_autostart_hint: &'static str,
    pub watch_mode_label: &'static str,
    pub watch_mode_interactive: &'static str,
    pub watch_mode_interactive_hint: &'static str,
    pub watch_mode_autotrim: &'static str,
    pub watch_mode_autotrim_hint: &'static str,
    pub watch_mode_passive: &'static str,
    pub watch_mode_passive_hint: &'static str,
    pub watch_daemon_status_running: &'static str,
    pub watch_daemon_status_stopped: &'static str,
    pub btn_watch_rescan_now: &'static str,
    pub watch_tray_tooltip_active: &'static str,
    pub watch_tray_tooltip_paused: &'static str,
    pub watch_tray_menu_open: &'static str,
    pub watch_tray_menu_check_now: &'static str,
    pub watch_tray_menu_pause: &'static str,
    pub watch_tray_menu_resume: &'static str,
    pub watch_tray_menu_exit: &'static str,
    pub watch_toast_updated_transition: &'static str,
    pub watch_toast_updated_build: &'static str,
    pub watch_toast_files_changed: &'static str,
    pub watch_toast_daemon_title: &'static str,
    /// "Done" rather than "Close": every setting is already applied and
    /// persisted, so there is nothing left to discard.
    pub btn_done: &'static str,
    pub btn_restore_defaults: &'static str,
    /// Confirmation that a change reached the database. Transient, unlike
    /// the failure case: a save that worked needs acknowledging once, a
    /// save that did not has to stay on screen.
    pub label_saved: &'static str,

    /// When a setting takes effect, shown beside the control it belongs to.
    /// The old dialog left this implicit, so users could not tell whether
    /// could not tell an immediate switch from one that only applies to the
    /// next scan. Sections that persist on change tag their rows with this.
    pub badge_immediately: &'static str,

    /// When a setting takes effect only on the next run of something. The
    /// counterpart to [`Self::badge_immediately`]; this distinction prevents
    /// precisely that the old dialog mixed the two without saying so.
    pub badge_next_scan: &'static str,
    pub badge_next_delete: &'static str,

    /// "Selection & deletion": three switches the old dialog conflated.
    pub default_profile_label: &'static str,
    pub default_profile_hint: &'static str,
    pub confirm_behavior_label: &'static str,
    pub confirm_yes_label: &'static str,
    pub confirm_no_label: &'static str,
    pub confirm_behavior_hint: &'static str,
    /// Spells out that scanning, auto-selection and deletion are three
    /// separate decisions - the section is arranged around that, but the
    /// users from reading the three independent groups as one pipeline.
    pub selection_independent_switches_hint: &'static str,

    /// "Scanning": the keep-list search box and the category table's
    /// headings. The table replaces a wall of 36 bare checkboxes with rows
    /// that say what each category costs to remove and whether the default
    /// profile would pick it up.
    pub keep_languages_add_placeholder: &'static str,
    pub categories_table_header_category: &'static str,
    pub categories_table_header_risk: &'static str,
    pub categories_table_header_profile_behavior: &'static str,
    pub profile_behavior_auto: &'static str,
    pub profile_behavior_manual: &'static str,
    /// Why the last remaining keep-language, category, or included library
    /// cannot be switched off. The old dialog silently reverted the click,
    /// which reads as a broken checkbox rather than as a floor.
    pub disabled_last_keep_language: &'static str,
    pub disabled_last_category: &'static str,
    pub disabled_last_library: &'static str,
    /// The keep-list's one dangerous edit (protected-language editing), which is why these live
    /// inside a red frame rather than beside the other chips.
    ///
    /// The danger is *deferred* and the wording has to say so: taking English
    /// off the list deletes nothing by itself, it changes what the next scan
    /// proposes - including interface text, because findings are not yet
    /// split by resource type (the spike "resource-type localization split - splitting localization by
    /// resource type"). "This will delete files" would be false, and a
    /// warning the user can catch lying is worth less than none.
    pub keep_english_warning: &'static str,
    pub keep_english_absent: &'static str,
    pub btn_keep_english_again: &'static str,

    /// "Rules": the two analysis packs, each with a live validity readout.
    pub rules_pack_category_label: &'static str,
    pub rules_pack_lang_label: &'static str,
    pub rules_valid_label: &'static str,
    pub rules_invalid_label: &'static str,

    /// "Data & diagnostics": the database file and the irreversible wipe.
    pub db_path_label: &'static str,
    pub btn_copy: &'static str,
    pub btn_open_folder: &'static str,
    /// Heading of the red-framed block around "Clear database". The label
    /// (§6.5) found it sitting inline beside "Compact", one tab-stop away and
    /// visually identical to a recoverable action.
    pub danger_zone_label: &'static str,

    /// Why a greyed-out action is unavailable, shown on hover. A disabled
    /// control that gives no reason reads as broken rather than as gated -
    /// see `ui::gated_button`.
    pub disabled_busy: &'static str,
    pub disabled_no_findings: &'static str,
    pub disabled_no_selection: &'static str,
    pub disabled_export_running: &'static str,
    /// Why scanning and deleting are unavailable before the first-run
    /// disclaimer is accepted - see `GameTrimmerApp::blocked_by_disclaimer`.
    pub disabled_disclaimer: &'static str,
    /// Why scanning is unavailable when the database never opened - see
    /// `GameTrimmerApp::blocked_by_database`. Short on purpose: it is a
    /// disabled-button tooltip, and the long explanation of *why* the
    /// database failed already sits above it in the panel (`db_open_error_long`).
    pub disabled_database: &'static str,

    /// Label preceding the selection-profile picker (selection profiles).
    pub profile_label: &'static str,
    pub profile_cautious: &'static str,
    pub profile_balanced: &'static str,
    pub profile_aggressive: &'static str,
    pub profile_custom: &'static str,
    /// Tooltip explaining what the selection profile changes. Deliberately
    /// short: it names the switch and points at the settings section that
    /// describes each profile, rather than trying to fit four definitions
    /// into a tooltip on the busiest row of the app.
    pub profile_hint: &'static str,
    /// One line per profile, shown under its radio button in "Selection &
    /// deletion". The picker used to offer four bare names - "Cautious",
    /// "Balanced", "Aggressive", "Custom" - with nothing on the screen saying
    /// what any of them ticks, in a dialog whose whole subject is what gets
    /// deleted.
    pub profile_cautious_hint: &'static str,
    pub profile_balanced_hint: &'static str,
    pub profile_aggressive_hint: &'static str,
    pub profile_custom_hint: &'static str,

    // -- plan_panel (plan summary) --
    /// Label in front of the category selector on the summary row above the
    /// tree.
    pub plan_filter_label: &'static str,
    /// The selector's "no filter" entry - the tree shows every category.
    pub plan_filter_all: &'static str,
    /// Label in front of the grouping-axis selector on the same row - what the
    /// tree's top level is cut by (see `model::GroupAxis`).
    pub plan_group_label: &'static str,
    /// The axis entries, in `model::GROUP_AXIS_ORDER`.
    pub group_axis_disk: &'static str,
    pub group_axis_launcher: &'static str,
    pub group_axis_library: &'static str,
    pub group_axis_category: &'static str,
    pub group_axis_flat: &'static str,
    /// Heading of the branch holding rows that carry no value on the active
    /// axis - residue whose library root no longer resolves, or rows from a
    /// database written before the attribution existed. Named rather than
    /// hidden: a tree that dropped them would show fewer findings after a
    /// switch than before it.
    pub group_unattributed: &'static str,
    /// Deletes every finding of the currently selected category. Only offered
    /// while a category is selected, so it can never mean "delete everything".
    pub btn_remove_category: &'static str,
    /// Placeholder inside the empty name-search field (name search).
    pub search_hint: &'static str,
    /// Tooltip on the button that empties the search field. Offered only while
    /// the field has something in it (MT-F05).
    pub btn_clear_search: &'static str,
    /// Shown in place of the tree when a search or a category filter has hidden
    /// every finding. Distinct from `no_findings_hint`, which means the scan
    /// found nothing at all - telling someone to press "Scan libraries" when
    /// their own query is what emptied the list sends them the wrong way
    /// (MT-F05).
    pub search_no_matches: &'static str,

    // -- dialogs --
    pub elevation_heading: &'static str,
    pub elevation_body: &'static str,
    /// Why declining is safe, and why this modal appears on some machines and
    /// never on others - the question the old two-sentence body left open.
    pub elevation_when_asked: &'static str,
    pub btn_continue_without_elevation: &'static str,
    pub btn_relaunch_elevated: &'static str,
    /// Checkbox label inside the elevation modal: the persistent way to stop
    /// the UAC prompt, now that "always walk folders" no longer exists.
    pub elevation_never_ask: &'static str,
    pub confirm_delete_heading: &'static str,
    pub confirm_label_permanent: &'static str,
    pub confirm_label_recycle: &'static str,
    pub remember_delete_method: &'static str,
    pub remove_summary_heading: &'static str,
    pub btn_close: &'static str,

    // -- settings --
    pub settings_heading: &'static str,
    /// Collapsed-by-default section heading (collapsed technical-details section) gathering the technical
    /// knobs (scan routing, database maintenance, rule packs, logging) that
    /// aren't decisions a user makes on every visit.
    pub delete_method_label: &'static str,
    pub delete_method_permanent_label: &'static str,
    pub delete_method_permanent_hint: &'static str,
    pub delete_method_recycle_label: &'static str,
    pub delete_method_recycle_hint: &'static str,
    pub database_label: &'static str,
    pub btn_compact_database: &'static str,
    pub compact_hint: &'static str,
    pub btn_clear_database: &'static str,
    pub clear_hint: &'static str,
    pub confirm_clear_heading: &'static str,
    pub confirm_clear_body: &'static str,
    pub btn_confirm_clear: &'static str,
    pub rules_label: &'static str,
    pub btn_export_rules: &'static str,
    pub btn_import_rules: &'static str,
    pub rules_hint: &'static str,
    pub running_ellipsis: &'static str,
    pub keep_languages_label: &'static str,
    pub keep_languages_hint: &'static str,
    /// Heading over the read-only diagnostic line reporting what the last
    /// scan actually did (`app.last_routing_breakdown`). Routing itself has
    /// no user-facing control any more - see [`Self::scan_method_hint`].
    pub scan_method_label: &'static str,
    /// One line under [`Self::scan_method_label`] explaining that the
    /// choice is automatic. Must not promise the MFT index is always used
    /// (it falls back per volume) and must not read as a setting - there is
    /// no control left to change.
    pub scan_method_hint: &'static str,
    pub app_language_label: &'static str,
    /// The "follow Windows" language option.
    ///
    /// Deliberately *not* worded like the theme row's system option, which it
    /// shares a dialog with: the two started out as the same "System (follow
    /// Windows)" string, which put two identically-labelled radio buttons on
    /// one screen - ambiguous to read, and genuinely unresolvable for a
    /// screen reader or anything else addressing a control by its name.
    pub lang_name_system: &'static str,
    pub lang_name_en: &'static str,
    pub lang_name_uk: &'static str,
    pub theme_label: &'static str,
    pub theme_system_label: &'static str,
    pub theme_light_label: &'static str,
    pub theme_dark_label: &'static str,
    pub categories_label: &'static str,
    pub categories_hint: &'static str,
    pub logging_label: &'static str,
    pub logging_checkbox: &'static str,
    pub logging_hint: &'static str,
    pub log_path_label: &'static str,
    pub bundle_label: &'static str,
    pub bundle_hint: &'static str,
    pub btn_generate_bundle: &'static str,
    pub bundle_titles_checkbox: &'static str,
    pub bundle_operations_checkbox: &'static str,
    pub bundle_preview_label: &'static str,
    pub bundle_save_title: &'static str,

    // -- libraries_panel --
    pub libraries_header: &'static str,
    pub btn_add_folder: &'static str,
    pub picking_folder: &'static str,
    pub no_libraries_registered: &'static str,
    pub btn_remove: &'static str,
    /// Per-library toggle. Unlike Remove, unchecking this leaves the row on
    /// screen and the `game_libraries` row untouched - see
    /// `gametrimmer_core::settings::Settings::excluded_libraries`.
    pub library_include_checkbox: &'static str,

    // -- onboarding (first-run onboarding) --
    /// The first-run explanation, shown in the empty tree area until the user
    /// has started a scan once. Covers the order of operations, the one word
    /// the main screen otherwise uses without defining ("profile") and the
    /// one mark it draws without explaining (\u{26a0}), plus the promise that
    /// scanning is not deleting.
    pub onboarding_heading: &'static str,
    pub onboarding_step_scan: &'static str,
    pub onboarding_step_review: &'static str,
    pub onboarding_step_remove: &'static str,
    /// How a finding is arrived at, and what narrows the search. Both were
    /// reachable before only by reading the README shipped beside the exe -
    /// which is not where someone deciding whether to trust a delete button
    /// looks.
    pub onboarding_how_heading: &'static str,
    pub onboarding_how_body: &'static str,
    pub onboarding_filters_body: &'static str,
    pub onboarding_profile: &'static str,
    pub onboarding_review_mark: &'static str,
    pub onboarding_safety: &'static str,
    /// Why keeping the diagnostic log is useful, explained where the default
    /// can be reviewed. The same setting as the one in "Data & diagnostics",
    /// never a second flag.
    pub onboarding_logging_body: &'static str,

    // -- onboarding: the liability disclaimer and its gate --
    /// Heading of the red-framed block. The frame is the app's one danger
    /// treatment - see `ui::danger_frame`.
    pub disclaimer_heading: &'static str,
    pub disclaimer_body: &'static str,
    pub disclaimer_accept_checkbox: &'static str,
    /// Hover text on the checkbox once it is ticked. Accepting is one-way (see
    /// `GameTrimmerApp::accept_disclaimer`), so the tick is disabled afterwards;
    /// a disabled control owes the user the reason, or clicking it looks like
    /// the app ignoring them (MT-A02).
    pub disclaimer_already_accepted: &'static str,

    // -- onboarding: acknowledgements --
    pub credits_heading: &'static str,
    pub credits_anthropic: &'static str,
    pub credits_karpathy: &'static str,
    pub credits_tikione: &'static str,

    // -- tree_view --
    pub scanning_in_progress: &'static str,
    pub no_findings_hint: &'static str,
    pub col_language: &'static str,
    pub col_files: &'static str,
    pub col_size: &'static str,
    pub col_name: &'static str,
    /// Tooltip on every column header, spelling out what clicking it does.
    /// Without it the headers are four words that happen to be clickable, and
    /// the third click - the one that gives the tree's own order back - is
    /// undiscoverable.
    pub col_sort_hint: &'static str,
    /// Tooltip on the \u{26a0} that marks a file the detector is less sure
    /// about. The mark replaced a per-row "Confidence" percentage: the number
    /// was the app's internal scale and told the user nothing actionable,
    /// while the one thing it decided - "this was not ticked for you, look at
    /// it" - is exactly what the mark now says.
    pub review_mark_hint: &'static str,
    pub hover_stub_note: &'static str,

    // -- tree_view.rs: row right-click context menu --
    pub ctx_open: &'static str,
    pub ctx_reveal_in_explorer: &'static str,
    pub ctx_open_with: &'static str,
    pub ctx_copy_path: &'static str,
    /// "Never touch this" - writes a personal exception for this one file in
    /// this one game (see `worker::rules_io::add_personal_exception`).
    pub ctx_never_touch: &'static str,
    /// Why the entry above is greyed out: an exception is bound to a game by
    /// its launcher id, and a folder-scan or manually added game has none, so
    /// there is nothing to bind it to. Said rather than left to be guessed at
    /// from a disabled control.
    pub ctx_never_touch_needs_app_id: &'static str,
    /// Hover text on a game row no launcher claims (marked in the tree with a
    /// small diamond). Names what is unavailable for such a game rather than
    /// only that it is different - the difference is only worth showing
    /// because of what it costs.
    pub game_without_launcher_id: &'static str,
    /// Starts the sweep for programs installed outside every launcher.
    pub btn_find_standalone: &'static str,
    pub find_standalone_hint: &'static str,
    /// Heading over the offered folders. Phrased as "installed outside a
    /// launcher", never as "games found" - the sweep cannot tell the two
    /// apart, and claiming otherwise is what this design refuses to do.
    pub standalone_candidates_header: &'static str,
    /// The sweep ran and found nothing. Said out loud, because silence here
    /// reads as broken detection rather than as an empty answer.
    pub no_standalone_candidates: &'static str,

    // -- app.rs: dialog titles / filter labels --
    pub add_library_dialog_title: &'static str,
    pub export_dialog_title: &'static str,
    pub text_file_filter_label: &'static str,
    pub rules_export_dialog_title: &'static str,
    pub rules_import_dialog_title: &'static str,
    pub rules_import_filter_label: &'static str,

    // -- app.rs: plain status/warning text --
    pub no_db_path: &'static str,
    pub db_path_error: &'static str,
    pub detecting_libraries: &'static str,
    pub preparing_database: &'static str,
    pub loading_previous_scan: &'static str,
    pub deleting_selected_files: &'static str,
    pub compacting_database: &'static str,
    pub clearing_database: &'static str,
    pub scan_cancelled: &'static str,
    pub deletion_completed: &'static str,
    pub database_compacted: &'static str,
    pub database_cleared: &'static str,
    pub settings_not_saved_no_path: &'static str,

    // -- worker progress verbs --
    pub verb_analyze: &'static str,
    pub verb_delete: &'static str,
    pub verb_compact: &'static str,
    pub verb_clear: &'static str,
    pub verb_bundle: &'static str,

    // -- model.rs: category display names --
    pub category_redist: &'static str,
    pub category_intro: &'static str,
    pub category_docs: &'static str,
    pub category_bonus: &'static str,
    pub category_loc: &'static str,
    pub category_dev_leftovers: &'static str,
    pub category_orphan: &'static str,
    pub category_workshop: &'static str,
    pub category_shader_cache: &'static str,
    pub category_crashes: &'static str,
    pub category_saves: &'static str,
    pub category_launcher_cache: &'static str,
    pub category_mod_downloads: &'static str,
    pub badge_safe: &'static str,
    pub badge_review: &'static str,
    pub badge_backup_shield: &'static str,
    pub saves_pruner_title: &'static str,
    pub saves_retention_slider: &'static str,
    pub saves_auto_backup_label: &'static str,
    pub saves_backup_success: &'static str,
    pub saves_total_prunable: &'static str,

    // -- tree_view.rs: the synthetic orphan-branch pseudo-game label (orphan-residue safety) --
    pub orphan_branch_label: &'static str,

    // -- tree_view.rs: the synthetic system-branch pseudo-game label - crash
    // dumps, shader caches, launcher caches and save bloat, none of which sit
    // inside a game --
    pub system_branch_label: &'static str,

    // -- model.rs: size units --
    pub unit_gb: &'static str,
    pub unit_mb: &'static str,
    pub unit_kb: &'static str,
    pub unit_b: &'static str,

    // -- export.rs --
    pub csv_yes: &'static str,
    pub csv_no: &'static str,

    // -- single_instance.rs --
    /// Caption of the native message box shown when a second launch from the
    /// same portable directory finds the first one already running (GT-75).
    /// The body text is interpolated (whether the window could be raised),
    /// so it lives in `messages::already_running_body` instead.
    pub already_running_title: &'static str,

    // -- 2-phase scanning --
    pub scan_phase_1_title: &'static str,
    pub scan_phase_2_title: &'static str,
    pub scan_overall_title: &'static str,
    /// Badge drawn once on a game row's name (see
    /// `ui::tree_view::show_game_row`, next to `[🔄 Updated]`) for a game
    /// whose findings carry `anti_cheat_protected`. The verdict is a
    /// per-*game* fact, not a per-row one, so it is marked on the game
    /// instead of repeated on every one of its rows.
    pub badge_anticheat_shield: &'static str,
    /// Hover text for [`Self::badge_anticheat_shield`]. States what the
    /// anti-cheat verdict actually does: it blocks an unattended re-trim of
    /// that game, nothing else - every whole-file delete (redist, docs,
    /// unused language packs, intro videos, ...) still behaves normally.
    /// Must never promise a wider carve-out than that.
    pub anticheat_shield_tooltip: &'static str,
    /// Hover text on a category/game/folder header checkbox when it is
    /// disabled: nothing in the group is selected, and nothing in it is
    /// bulk-selectable either, so a click could never do anything (see
    /// `model::group_selection_state`). Without this the control just looks
    /// clickable and silently does nothing.
    pub group_checkbox_disabled_hint: &'static str,
}

impl Strings {
    pub fn apply_overrides(&self, map: &std::collections::HashMap<String, String>) -> Strings {
        let mut s = *self;
        if let Some(val) = map.get("btn_scan_libraries") {
            s.btn_scan_libraries = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_cancel") {
            s.btn_cancel = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_export") {
            s.btn_export = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_settings") {
            s.btn_settings = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_select_all") {
            s.btn_select_all = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_deselect_all") {
            s.btn_deselect_all = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_delete_selected") {
            s.btn_delete_selected = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("settings_section_general") {
            s.settings_section_general = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("settings_section_monitoring") {
            s.settings_section_monitoring = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("settings_section_scanning") {
            s.settings_section_scanning = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("settings_section_selection") {
            s.settings_section_selection = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("settings_section_rules") {
            s.settings_section_rules = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("settings_section_data") {
            s.settings_section_data = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_enabled_label") {
            s.watch_enabled_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_enabled_hint") {
            s.watch_enabled_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_autostart_label") {
            s.watch_autostart_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_autostart_hint") {
            s.watch_autostart_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_mode_label") {
            s.watch_mode_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_mode_interactive") {
            s.watch_mode_interactive = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_mode_interactive_hint") {
            s.watch_mode_interactive_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_mode_autotrim") {
            s.watch_mode_autotrim = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_mode_autotrim_hint") {
            s.watch_mode_autotrim_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_mode_passive") {
            s.watch_mode_passive = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_mode_passive_hint") {
            s.watch_mode_passive_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_daemon_status_running") {
            s.watch_daemon_status_running = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_daemon_status_stopped") {
            s.watch_daemon_status_stopped = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_watch_rescan_now") {
            s.btn_watch_rescan_now = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_tray_tooltip_active") {
            s.watch_tray_tooltip_active = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_tray_tooltip_paused") {
            s.watch_tray_tooltip_paused = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_tray_menu_open") {
            s.watch_tray_menu_open = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_tray_menu_check_now") {
            s.watch_tray_menu_check_now = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_tray_menu_pause") {
            s.watch_tray_menu_pause = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_tray_menu_resume") {
            s.watch_tray_menu_resume = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_tray_menu_exit") {
            s.watch_tray_menu_exit = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_toast_updated_transition") {
            s.watch_toast_updated_transition = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_toast_updated_build") {
            s.watch_toast_updated_build = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_toast_files_changed") {
            s.watch_toast_files_changed = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("watch_toast_daemon_title") {
            s.watch_toast_daemon_title = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_done") {
            s.btn_done = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_restore_defaults") {
            s.btn_restore_defaults = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("label_saved") {
            s.label_saved = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("badge_immediately") {
            s.badge_immediately = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("badge_next_scan") {
            s.badge_next_scan = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("badge_next_delete") {
            s.badge_next_delete = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("default_profile_label") {
            s.default_profile_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("default_profile_hint") {
            s.default_profile_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("confirm_behavior_label") {
            s.confirm_behavior_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("confirm_yes_label") {
            s.confirm_yes_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("confirm_no_label") {
            s.confirm_no_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("confirm_behavior_hint") {
            s.confirm_behavior_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("selection_independent_switches_hint") {
            s.selection_independent_switches_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("keep_languages_add_placeholder") {
            s.keep_languages_add_placeholder = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("categories_table_header_category") {
            s.categories_table_header_category = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("categories_table_header_risk") {
            s.categories_table_header_risk = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("categories_table_header_profile_behavior") {
            s.categories_table_header_profile_behavior = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("profile_behavior_auto") {
            s.profile_behavior_auto = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("profile_behavior_manual") {
            s.profile_behavior_manual = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("disabled_last_keep_language") {
            s.disabled_last_keep_language = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("disabled_last_category") {
            s.disabled_last_category = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("disabled_last_library") {
            s.disabled_last_library = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("keep_english_warning") {
            s.keep_english_warning = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("keep_english_absent") {
            s.keep_english_absent = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_keep_english_again") {
            s.btn_keep_english_again = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("rules_pack_category_label") {
            s.rules_pack_category_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("rules_pack_lang_label") {
            s.rules_pack_lang_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("rules_valid_label") {
            s.rules_valid_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("rules_invalid_label") {
            s.rules_invalid_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("db_path_label") {
            s.db_path_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_copy") {
            s.btn_copy = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_open_folder") {
            s.btn_open_folder = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("danger_zone_label") {
            s.danger_zone_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("disabled_busy") {
            s.disabled_busy = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("disabled_no_findings") {
            s.disabled_no_findings = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("disabled_no_selection") {
            s.disabled_no_selection = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("disabled_export_running") {
            s.disabled_export_running = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("disabled_disclaimer") {
            s.disabled_disclaimer = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("disabled_database") {
            s.disabled_database = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("profile_label") {
            s.profile_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("profile_cautious") {
            s.profile_cautious = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("profile_balanced") {
            s.profile_balanced = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("profile_aggressive") {
            s.profile_aggressive = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("profile_custom") {
            s.profile_custom = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("profile_hint") {
            s.profile_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("profile_cautious_hint") {
            s.profile_cautious_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("profile_balanced_hint") {
            s.profile_balanced_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("profile_aggressive_hint") {
            s.profile_aggressive_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("profile_custom_hint") {
            s.profile_custom_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("plan_filter_label") {
            s.plan_filter_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("plan_filter_all") {
            s.plan_filter_all = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("plan_group_label") {
            s.plan_group_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("group_axis_disk") {
            s.group_axis_disk = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("group_axis_launcher") {
            s.group_axis_launcher = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("group_axis_library") {
            s.group_axis_library = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("group_axis_category") {
            s.group_axis_category = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("group_axis_flat") {
            s.group_axis_flat = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("group_unattributed") {
            s.group_unattributed = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_remove_category") {
            s.btn_remove_category = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("search_hint") {
            s.search_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_clear_search") {
            s.btn_clear_search = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("search_no_matches") {
            s.search_no_matches = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("elevation_heading") {
            s.elevation_heading = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("elevation_body") {
            s.elevation_body = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("elevation_when_asked") {
            s.elevation_when_asked = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_continue_without_elevation") {
            s.btn_continue_without_elevation = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_relaunch_elevated") {
            s.btn_relaunch_elevated = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("elevation_never_ask") {
            s.elevation_never_ask = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("confirm_delete_heading") {
            s.confirm_delete_heading = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("confirm_label_permanent") {
            s.confirm_label_permanent = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("confirm_label_recycle") {
            s.confirm_label_recycle = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("remember_delete_method") {
            s.remember_delete_method = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("remove_summary_heading") {
            s.remove_summary_heading = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_close") {
            s.btn_close = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("settings_heading") {
            s.settings_heading = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("delete_method_label") {
            s.delete_method_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("delete_method_permanent_label") {
            s.delete_method_permanent_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("delete_method_permanent_hint") {
            s.delete_method_permanent_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("delete_method_recycle_label") {
            s.delete_method_recycle_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("delete_method_recycle_hint") {
            s.delete_method_recycle_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("database_label") {
            s.database_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_compact_database") {
            s.btn_compact_database = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("compact_hint") {
            s.compact_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_clear_database") {
            s.btn_clear_database = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("clear_hint") {
            s.clear_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("confirm_clear_heading") {
            s.confirm_clear_heading = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("confirm_clear_body") {
            s.confirm_clear_body = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_confirm_clear") {
            s.btn_confirm_clear = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("rules_label") {
            s.rules_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_export_rules") {
            s.btn_export_rules = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_import_rules") {
            s.btn_import_rules = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("rules_hint") {
            s.rules_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("running_ellipsis") {
            s.running_ellipsis = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("keep_languages_label") {
            s.keep_languages_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("keep_languages_hint") {
            s.keep_languages_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("scan_method_label") {
            s.scan_method_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("scan_method_hint") {
            s.scan_method_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("app_language_label") {
            s.app_language_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("lang_name_system") {
            s.lang_name_system = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("lang_name_en") {
            s.lang_name_en = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("lang_name_uk") {
            s.lang_name_uk = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("theme_label") {
            s.theme_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("theme_system_label") {
            s.theme_system_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("theme_light_label") {
            s.theme_light_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("theme_dark_label") {
            s.theme_dark_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("categories_label") {
            s.categories_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("categories_hint") {
            s.categories_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("logging_label") {
            s.logging_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("logging_checkbox") {
            s.logging_checkbox = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("logging_hint") {
            s.logging_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("log_path_label") {
            s.log_path_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("bundle_label") {
            s.bundle_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("bundle_hint") {
            s.bundle_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_generate_bundle") {
            s.btn_generate_bundle = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("bundle_titles_checkbox") {
            s.bundle_titles_checkbox = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("bundle_operations_checkbox") {
            s.bundle_operations_checkbox = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("bundle_preview_label") {
            s.bundle_preview_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("bundle_save_title") {
            s.bundle_save_title = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("libraries_header") {
            s.libraries_header = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_add_folder") {
            s.btn_add_folder = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("picking_folder") {
            s.picking_folder = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("no_libraries_registered") {
            s.no_libraries_registered = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_remove") {
            s.btn_remove = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("library_include_checkbox") {
            s.library_include_checkbox = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("onboarding_heading") {
            s.onboarding_heading = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("onboarding_step_scan") {
            s.onboarding_step_scan = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("onboarding_step_review") {
            s.onboarding_step_review = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("onboarding_step_remove") {
            s.onboarding_step_remove = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("onboarding_how_heading") {
            s.onboarding_how_heading = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("onboarding_how_body") {
            s.onboarding_how_body = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("onboarding_filters_body") {
            s.onboarding_filters_body = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("onboarding_profile") {
            s.onboarding_profile = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("onboarding_review_mark") {
            s.onboarding_review_mark = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("onboarding_safety") {
            s.onboarding_safety = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("onboarding_logging_body") {
            s.onboarding_logging_body = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("disclaimer_heading") {
            s.disclaimer_heading = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("disclaimer_body") {
            s.disclaimer_body = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("disclaimer_accept_checkbox") {
            s.disclaimer_accept_checkbox = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("disclaimer_already_accepted") {
            s.disclaimer_already_accepted = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("credits_heading") {
            s.credits_heading = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("credits_anthropic") {
            s.credits_anthropic = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("credits_karpathy") {
            s.credits_karpathy = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("credits_tikione") {
            s.credits_tikione = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("scanning_in_progress") {
            s.scanning_in_progress = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("no_findings_hint") {
            s.no_findings_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("col_language") {
            s.col_language = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("col_files") {
            s.col_files = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("col_size") {
            s.col_size = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("col_name") {
            s.col_name = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("col_sort_hint") {
            s.col_sort_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("review_mark_hint") {
            s.review_mark_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("hover_stub_note") {
            s.hover_stub_note = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("ctx_open") {
            s.ctx_open = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("ctx_reveal_in_explorer") {
            s.ctx_reveal_in_explorer = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("ctx_open_with") {
            s.ctx_open_with = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("ctx_copy_path") {
            s.ctx_copy_path = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("ctx_never_touch") {
            s.ctx_never_touch = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("ctx_never_touch_needs_app_id") {
            s.ctx_never_touch_needs_app_id = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("game_without_launcher_id") {
            s.game_without_launcher_id = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("btn_find_standalone") {
            s.btn_find_standalone = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("find_standalone_hint") {
            s.find_standalone_hint = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("standalone_candidates_header") {
            s.standalone_candidates_header = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("no_standalone_candidates") {
            s.no_standalone_candidates = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("add_library_dialog_title") {
            s.add_library_dialog_title = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("export_dialog_title") {
            s.export_dialog_title = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("text_file_filter_label") {
            s.text_file_filter_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("rules_export_dialog_title") {
            s.rules_export_dialog_title = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("rules_import_dialog_title") {
            s.rules_import_dialog_title = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("rules_import_filter_label") {
            s.rules_import_filter_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("no_db_path") {
            s.no_db_path = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("db_path_error") {
            s.db_path_error = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("detecting_libraries") {
            s.detecting_libraries = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("preparing_database") {
            s.preparing_database = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("loading_previous_scan") {
            s.loading_previous_scan = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("deleting_selected_files") {
            s.deleting_selected_files = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("compacting_database") {
            s.compacting_database = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("clearing_database") {
            s.clearing_database = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("scan_cancelled") {
            s.scan_cancelled = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("deletion_completed") {
            s.deletion_completed = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("database_compacted") {
            s.database_compacted = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("database_cleared") {
            s.database_cleared = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("settings_not_saved_no_path") {
            s.settings_not_saved_no_path = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("verb_analyze") {
            s.verb_analyze = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("verb_delete") {
            s.verb_delete = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("verb_compact") {
            s.verb_compact = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("verb_clear") {
            s.verb_clear = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("verb_bundle") {
            s.verb_bundle = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("category_redist") {
            s.category_redist = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("category_intro") {
            s.category_intro = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("category_docs") {
            s.category_docs = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("category_bonus") {
            s.category_bonus = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("category_loc") {
            s.category_loc = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("category_dev_leftovers") {
            s.category_dev_leftovers = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("category_orphan") {
            s.category_orphan = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("category_workshop") {
            s.category_workshop = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("category_shader_cache") {
            s.category_shader_cache = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("category_crashes") {
            s.category_crashes = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("category_saves") {
            s.category_saves = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("category_launcher_cache") {
            s.category_launcher_cache = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("category_mod_downloads") {
            s.category_mod_downloads = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("badge_safe") {
            s.badge_safe = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("badge_review") {
            s.badge_review = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("badge_backup_shield") {
            s.badge_backup_shield = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("saves_pruner_title") {
            s.saves_pruner_title = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("saves_retention_slider") {
            s.saves_retention_slider = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("saves_auto_backup_label") {
            s.saves_auto_backup_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("saves_backup_success") {
            s.saves_backup_success = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("saves_total_prunable") {
            s.saves_total_prunable = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("orphan_branch_label") {
            s.orphan_branch_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("system_branch_label") {
            s.system_branch_label = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("unit_gb") {
            s.unit_gb = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("unit_mb") {
            s.unit_mb = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("unit_kb") {
            s.unit_kb = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("unit_b") {
            s.unit_b = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("csv_yes") {
            s.csv_yes = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("csv_no") {
            s.csv_no = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("already_running_title") {
            s.already_running_title = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("scan_phase_1_title") {
            s.scan_phase_1_title = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("scan_phase_2_title") {
            s.scan_phase_2_title = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("scan_overall_title") {
            s.scan_overall_title = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("badge_anticheat_shield") {
            s.badge_anticheat_shield = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("anticheat_shield_tooltip") {
            s.anticheat_shield_tooltip = Box::leak(val.clone().into_boxed_str());
        }
        if let Some(val) = map.get("group_checkbox_disabled_hint") {
            s.group_checkbox_disabled_hint = Box::leak(val.clone().into_boxed_str());
        }
        s
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// A scan, all of it. There used to be a second verb here for the MFT
    /// pre-pass, because it was a phase of its own that ran to completion
    /// before anything was classified. It no longer is: the file table is
    /// read underneath the classification (see `worker::scan`), so both
    /// would now be live at once and would fight over one bar with two
    /// different totals. The bar counts games and the file-table read
    /// appears in the detail line instead.
    Analyze,
    Delete,
    Compact,
    Clear,
    /// Assembling the diagnostic bundle, one section at a time.
    Bundle,
}

pub fn verb_label(lang: Lang, verb: Verb) -> &'static str {
    let s = strings(lang);
    match verb {
        Verb::Analyze => s.verb_analyze,
        Verb::Delete => s.verb_delete,
        Verb::Compact => s.verb_compact,
        Verb::Clear => s.verb_clear,
        Verb::Bundle => s.verb_bundle,
    }
}

#[cfg(test)]
impl Strings {
    /// Every field paired with its name, for the "no empty strings" test
    /// below. Listed explicitly (rather than via a derive/macro) so a new
    /// field left out here fails loudly instead of silently escaping
    /// coverage.
    fn all_fields(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("btn_scan_libraries", self.btn_scan_libraries),
            ("btn_cancel", self.btn_cancel),
            ("btn_export", self.btn_export),
            ("btn_settings", self.btn_settings),
            ("btn_select_all", self.btn_select_all),
            ("btn_deselect_all", self.btn_deselect_all),
            ("btn_delete_selected", self.btn_delete_selected),
            ("settings_section_general", self.settings_section_general),
            (
                "settings_section_monitoring",
                self.settings_section_monitoring,
            ),
            ("settings_section_scanning", self.settings_section_scanning),
            (
                "settings_section_selection",
                self.settings_section_selection,
            ),
            ("settings_section_rules", self.settings_section_rules),
            ("settings_section_data", self.settings_section_data),
            ("watch_enabled_label", self.watch_enabled_label),
            ("watch_enabled_hint", self.watch_enabled_hint),
            ("watch_autostart_label", self.watch_autostart_label),
            ("watch_autostart_hint", self.watch_autostart_hint),
            ("watch_mode_label", self.watch_mode_label),
            ("watch_mode_interactive", self.watch_mode_interactive),
            (
                "watch_mode_interactive_hint",
                self.watch_mode_interactive_hint,
            ),
            ("watch_mode_autotrim", self.watch_mode_autotrim),
            ("watch_mode_autotrim_hint", self.watch_mode_autotrim_hint),
            ("watch_mode_passive", self.watch_mode_passive),
            ("watch_mode_passive_hint", self.watch_mode_passive_hint),
            (
                "watch_daemon_status_running",
                self.watch_daemon_status_running,
            ),
            (
                "watch_daemon_status_stopped",
                self.watch_daemon_status_stopped,
            ),
            ("btn_watch_rescan_now", self.btn_watch_rescan_now),
            ("watch_tray_tooltip_active", self.watch_tray_tooltip_active),
            ("watch_tray_tooltip_paused", self.watch_tray_tooltip_paused),
            ("watch_tray_menu_open", self.watch_tray_menu_open),
            ("watch_tray_menu_check_now", self.watch_tray_menu_check_now),
            ("watch_tray_menu_pause", self.watch_tray_menu_pause),
            ("watch_tray_menu_resume", self.watch_tray_menu_resume),
            ("watch_tray_menu_exit", self.watch_tray_menu_exit),
            (
                "watch_toast_updated_transition",
                self.watch_toast_updated_transition,
            ),
            ("watch_toast_updated_build", self.watch_toast_updated_build),
            ("watch_toast_files_changed", self.watch_toast_files_changed),
            ("watch_toast_daemon_title", self.watch_toast_daemon_title),
            ("btn_done", self.btn_done),
            ("btn_restore_defaults", self.btn_restore_defaults),
            ("label_saved", self.label_saved),
            ("badge_immediately", self.badge_immediately),
            ("badge_next_scan", self.badge_next_scan),
            ("badge_next_delete", self.badge_next_delete),
            ("default_profile_label", self.default_profile_label),
            ("default_profile_hint", self.default_profile_hint),
            ("profile_cautious_hint", self.profile_cautious_hint),
            ("profile_balanced_hint", self.profile_balanced_hint),
            ("profile_aggressive_hint", self.profile_aggressive_hint),
            ("profile_custom_hint", self.profile_custom_hint),
            ("confirm_behavior_label", self.confirm_behavior_label),
            ("confirm_yes_label", self.confirm_yes_label),
            ("confirm_no_label", self.confirm_no_label),
            ("confirm_behavior_hint", self.confirm_behavior_hint),
            (
                "selection_independent_switches_hint",
                self.selection_independent_switches_hint,
            ),
            (
                "keep_languages_add_placeholder",
                self.keep_languages_add_placeholder,
            ),
            (
                "categories_table_header_category",
                self.categories_table_header_category,
            ),
            (
                "categories_table_header_risk",
                self.categories_table_header_risk,
            ),
            (
                "categories_table_header_profile_behavior",
                self.categories_table_header_profile_behavior,
            ),
            ("profile_behavior_auto", self.profile_behavior_auto),
            ("profile_behavior_manual", self.profile_behavior_manual),
            (
                "disabled_last_keep_language",
                self.disabled_last_keep_language,
            ),
            ("disabled_last_category", self.disabled_last_category),
            ("disabled_last_library", self.disabled_last_library),
            ("keep_english_warning", self.keep_english_warning),
            ("keep_english_absent", self.keep_english_absent),
            ("btn_keep_english_again", self.btn_keep_english_again),
            ("rules_pack_category_label", self.rules_pack_category_label),
            ("rules_pack_lang_label", self.rules_pack_lang_label),
            ("rules_valid_label", self.rules_valid_label),
            ("rules_invalid_label", self.rules_invalid_label),
            ("db_path_label", self.db_path_label),
            ("btn_copy", self.btn_copy),
            ("btn_open_folder", self.btn_open_folder),
            ("danger_zone_label", self.danger_zone_label),
            ("disabled_busy", self.disabled_busy),
            ("disabled_no_findings", self.disabled_no_findings),
            ("disabled_no_selection", self.disabled_no_selection),
            ("disabled_export_running", self.disabled_export_running),
            ("disabled_disclaimer", self.disabled_disclaimer),
            ("disabled_database", self.disabled_database),
            ("elevation_heading", self.elevation_heading),
            ("elevation_body", self.elevation_body),
            ("elevation_when_asked", self.elevation_when_asked),
            (
                "btn_continue_without_elevation",
                self.btn_continue_without_elevation,
            ),
            ("btn_relaunch_elevated", self.btn_relaunch_elevated),
            ("elevation_never_ask", self.elevation_never_ask),
            ("confirm_delete_heading", self.confirm_delete_heading),
            ("confirm_label_permanent", self.confirm_label_permanent),
            ("confirm_label_recycle", self.confirm_label_recycle),
            ("remember_delete_method", self.remember_delete_method),
            ("remove_summary_heading", self.remove_summary_heading),
            ("btn_close", self.btn_close),
            ("settings_heading", self.settings_heading),
            ("delete_method_label", self.delete_method_label),
            (
                "delete_method_permanent_label",
                self.delete_method_permanent_label,
            ),
            (
                "delete_method_permanent_hint",
                self.delete_method_permanent_hint,
            ),
            (
                "delete_method_recycle_label",
                self.delete_method_recycle_label,
            ),
            (
                "delete_method_recycle_hint",
                self.delete_method_recycle_hint,
            ),
            ("database_label", self.database_label),
            ("btn_compact_database", self.btn_compact_database),
            ("compact_hint", self.compact_hint),
            ("btn_clear_database", self.btn_clear_database),
            ("clear_hint", self.clear_hint),
            ("confirm_clear_heading", self.confirm_clear_heading),
            ("confirm_clear_body", self.confirm_clear_body),
            ("btn_confirm_clear", self.btn_confirm_clear),
            ("rules_label", self.rules_label),
            ("btn_export_rules", self.btn_export_rules),
            ("btn_import_rules", self.btn_import_rules),
            ("rules_hint", self.rules_hint),
            ("running_ellipsis", self.running_ellipsis),
            ("keep_languages_label", self.keep_languages_label),
            ("keep_languages_hint", self.keep_languages_hint),
            ("scan_method_label", self.scan_method_label),
            ("scan_method_hint", self.scan_method_hint),
            ("app_language_label", self.app_language_label),
            ("lang_name_system", self.lang_name_system),
            ("lang_name_en", self.lang_name_en),
            ("lang_name_uk", self.lang_name_uk),
            ("theme_label", self.theme_label),
            ("theme_system_label", self.theme_system_label),
            ("theme_light_label", self.theme_light_label),
            ("theme_dark_label", self.theme_dark_label),
            ("categories_label", self.categories_label),
            ("categories_hint", self.categories_hint),
            ("logging_label", self.logging_label),
            ("logging_checkbox", self.logging_checkbox),
            ("logging_hint", self.logging_hint),
            ("log_path_label", self.log_path_label),
            ("bundle_label", self.bundle_label),
            ("bundle_hint", self.bundle_hint),
            ("btn_generate_bundle", self.btn_generate_bundle),
            ("bundle_titles_checkbox", self.bundle_titles_checkbox),
            (
                "bundle_operations_checkbox",
                self.bundle_operations_checkbox,
            ),
            ("bundle_preview_label", self.bundle_preview_label),
            ("bundle_save_title", self.bundle_save_title),
            ("libraries_header", self.libraries_header),
            ("btn_add_folder", self.btn_add_folder),
            ("picking_folder", self.picking_folder),
            ("no_libraries_registered", self.no_libraries_registered),
            ("btn_remove", self.btn_remove),
            ("library_include_checkbox", self.library_include_checkbox),
            ("onboarding_heading", self.onboarding_heading),
            ("onboarding_step_scan", self.onboarding_step_scan),
            ("onboarding_step_review", self.onboarding_step_review),
            ("onboarding_step_remove", self.onboarding_step_remove),
            ("onboarding_how_heading", self.onboarding_how_heading),
            ("onboarding_how_body", self.onboarding_how_body),
            ("onboarding_filters_body", self.onboarding_filters_body),
            ("onboarding_profile", self.onboarding_profile),
            ("onboarding_review_mark", self.onboarding_review_mark),
            ("onboarding_safety", self.onboarding_safety),
            ("onboarding_logging_body", self.onboarding_logging_body),
            ("disclaimer_heading", self.disclaimer_heading),
            ("disclaimer_body", self.disclaimer_body),
            (
                "disclaimer_accept_checkbox",
                self.disclaimer_accept_checkbox,
            ),
            (
                "disclaimer_already_accepted",
                self.disclaimer_already_accepted,
            ),
            ("credits_heading", self.credits_heading),
            ("credits_anthropic", self.credits_anthropic),
            ("credits_karpathy", self.credits_karpathy),
            ("credits_tikione", self.credits_tikione),
            ("scanning_in_progress", self.scanning_in_progress),
            ("no_findings_hint", self.no_findings_hint),
            ("btn_clear_search", self.btn_clear_search),
            ("search_no_matches", self.search_no_matches),
            ("col_language", self.col_language),
            ("col_files", self.col_files),
            ("col_size", self.col_size),
            ("col_name", self.col_name),
            ("col_sort_hint", self.col_sort_hint),
            ("review_mark_hint", self.review_mark_hint),
            ("hover_stub_note", self.hover_stub_note),
            ("ctx_open", self.ctx_open),
            ("ctx_reveal_in_explorer", self.ctx_reveal_in_explorer),
            ("ctx_open_with", self.ctx_open_with),
            ("ctx_copy_path", self.ctx_copy_path),
            ("ctx_never_touch", self.ctx_never_touch),
            (
                "ctx_never_touch_needs_app_id",
                self.ctx_never_touch_needs_app_id,
            ),
            ("game_without_launcher_id", self.game_without_launcher_id),
            ("btn_find_standalone", self.btn_find_standalone),
            ("find_standalone_hint", self.find_standalone_hint),
            (
                "standalone_candidates_header",
                self.standalone_candidates_header,
            ),
            ("no_standalone_candidates", self.no_standalone_candidates),
            ("add_library_dialog_title", self.add_library_dialog_title),
            ("export_dialog_title", self.export_dialog_title),
            ("text_file_filter_label", self.text_file_filter_label),
            ("rules_export_dialog_title", self.rules_export_dialog_title),
            ("rules_import_dialog_title", self.rules_import_dialog_title),
            ("rules_import_filter_label", self.rules_import_filter_label),
            ("no_db_path", self.no_db_path),
            ("db_path_error", self.db_path_error),
            ("detecting_libraries", self.detecting_libraries),
            ("preparing_database", self.preparing_database),
            ("loading_previous_scan", self.loading_previous_scan),
            ("deleting_selected_files", self.deleting_selected_files),
            ("compacting_database", self.compacting_database),
            ("clearing_database", self.clearing_database),
            ("scan_cancelled", self.scan_cancelled),
            ("deletion_completed", self.deletion_completed),
            ("database_compacted", self.database_compacted),
            ("database_cleared", self.database_cleared),
            (
                "settings_not_saved_no_path",
                self.settings_not_saved_no_path,
            ),
            ("verb_analyze", self.verb_analyze),
            ("verb_delete", self.verb_delete),
            ("verb_compact", self.verb_compact),
            ("verb_clear", self.verb_clear),
            ("verb_bundle", self.verb_bundle),
            ("category_redist", self.category_redist),
            ("category_intro", self.category_intro),
            ("category_docs", self.category_docs),
            ("category_bonus", self.category_bonus),
            ("category_loc", self.category_loc),
            ("category_dev_leftovers", self.category_dev_leftovers),
            ("category_orphan", self.category_orphan),
            ("category_workshop", self.category_workshop),
            ("category_shader_cache", self.category_shader_cache),
            ("category_crashes", self.category_crashes),
            ("category_saves", self.category_saves),
            ("category_launcher_cache", self.category_launcher_cache),
            ("category_mod_downloads", self.category_mod_downloads),
            ("badge_safe", self.badge_safe),
            ("badge_review", self.badge_review),
            ("badge_backup_shield", self.badge_backup_shield),
            ("saves_pruner_title", self.saves_pruner_title),
            ("saves_retention_slider", self.saves_retention_slider),
            ("saves_auto_backup_label", self.saves_auto_backup_label),
            ("saves_backup_success", self.saves_backup_success),
            ("saves_total_prunable", self.saves_total_prunable),
            ("orphan_branch_label", self.orphan_branch_label),
            ("system_branch_label", self.system_branch_label),
            ("unit_gb", self.unit_gb),
            ("unit_mb", self.unit_mb),
            ("unit_kb", self.unit_kb),
            ("unit_b", self.unit_b),
            ("csv_yes", self.csv_yes),
            ("csv_no", self.csv_no),
            ("already_running_title", self.already_running_title),
            ("scan_phase_1_title", self.scan_phase_1_title),
            ("scan_phase_2_title", self.scan_phase_2_title),
            ("scan_overall_title", self.scan_overall_title),
            ("badge_anticheat_shield", self.badge_anticheat_shield),
            ("anticheat_shield_tooltip", self.anticheat_shield_tooltip),
            (
                "group_checkbox_disabled_hint",
                self.group_checkbox_disabled_hint,
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_string_table_field_is_empty() {
        let lang = Lang::En;
        for (field, value) in strings(lang).all_fields() {
            assert!(!value.is_empty(), "{lang:?}::{field} must not be empty");
        }
    }

    #[test]
    fn every_verb_has_a_label() {
        let lang = Lang::En;
        for verb in [Verb::Analyze, Verb::Delete, Verb::Compact, Verb::Clear] {
            assert!(!verb_label(lang, verb).is_empty());
        }
    }
}
