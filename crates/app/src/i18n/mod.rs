//! Hand-rolled i18n: no runtime string keys to typo, no heavy crate (fluent
//! etc.) for two languages and a few hundred short strings. Plain text with
//! no interpolation lives in [`Strings`] (one `&'static str` field per
//! string, exhaustively filled in for both languages at compile time);
//! anything with a count, a path, or an error message interpolated into it
//! is a small function in [`messages`] instead, taking [`Lang`] plus its
//! arguments and returning an owned `String`.
//!
//! [`Lang`] itself lives in `gametrimmer_core::settings` (not duplicated
//! here) since the same enum is also the persisted `app_language` setting -
//! one type, one source of truth, both for "which language is active" and
//! "which language is saved".

mod en;
mod messages;
mod system;
mod uk;

pub use gametrimmer_core::settings::Lang;
pub use messages::*;
pub use system::detect as detect_system_language;

/// One `&'static str` per plain (non-interpolated) UI string, mirrored for
/// every [`Lang`] variant. Compile-time exhaustive: adding a field here
/// without filling it in for both `en` and `uk` fails the build, so a
/// language can never silently fall back to empty text.
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
    pub settings_section_scanning: &'static str,
    pub settings_section_selection: &'static str,
    pub settings_section_rules: &'static str,
    pub settings_section_data: &'static str,
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

    // -- tree_view.rs: row right-click context menu --
    pub ctx_reveal_in_explorer: &'static str,
    pub ctx_open_with: &'static str,
    pub ctx_copy_path: &'static str,

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
    pub category_docs: &'static str,
    pub category_bonus: &'static str,
    pub category_loc: &'static str,
    pub category_other: &'static str,
    pub category_orphan: &'static str,

    // -- tree_view.rs: the synthetic orphan-branch pseudo-game label (orphan-residue safety) --
    pub orphan_branch_label: &'static str,

    // -- model.rs: size units --
    pub unit_gb: &'static str,
    pub unit_mb: &'static str,
    pub unit_kb: &'static str,
    pub unit_b: &'static str,

    // -- export.rs --
    pub csv_yes: &'static str,
    pub csv_no: &'static str,
}

/// Returns the static string table for `lang`. Cheap - callers can call this
/// once per frame/message without worrying about cost.
pub fn strings(lang: Lang) -> &'static Strings {
    match lang {
        Lang::En => &en::STRINGS,
        Lang::Uk => &uk::STRINGS,
    }
}

/// The progress verb shown before the `current/total` counter in the top bar
/// (e.g. "Scanning 3/10: ..."). Kept as an enum on [`crate::worker::WorkerMsg`]
/// rather than a pre-localized `&'static str` so the label always reflects
/// the *current* UI language, even if it changes mid-operation.
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
            ("settings_section_scanning", self.settings_section_scanning),
            (
                "settings_section_selection",
                self.settings_section_selection,
            ),
            ("settings_section_rules", self.settings_section_rules),
            ("settings_section_data", self.settings_section_data),
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
            ("ctx_reveal_in_explorer", self.ctx_reveal_in_explorer),
            ("ctx_open_with", self.ctx_open_with),
            ("ctx_copy_path", self.ctx_copy_path),
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
            ("category_docs", self.category_docs),
            ("category_bonus", self.category_bonus),
            ("category_loc", self.category_loc),
            ("category_other", self.category_other),
            ("category_orphan", self.category_orphan),
            ("orphan_branch_label", self.orphan_branch_label),
            ("unit_gb", self.unit_gb),
            ("unit_mb", self.unit_mb),
            ("unit_kb", self.unit_kb),
            ("unit_b", self.unit_b),
            ("csv_yes", self.csv_yes),
            ("csv_no", self.csv_no),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_string_table_field_is_empty_in_either_language() {
        for lang in [Lang::En, Lang::Uk] {
            for (field, value) in strings(lang).all_fields() {
                assert!(!value.is_empty(), "{lang:?}::{field} must not be empty");
            }
        }
    }

    #[test]
    fn every_verb_has_a_label_in_both_languages() {
        for lang in [Lang::En, Lang::Uk] {
            for verb in [Verb::Analyze, Verb::Delete, Verb::Compact, Verb::Clear] {
                assert!(!verb_label(lang, verb).is_empty());
            }
        }
    }
}
