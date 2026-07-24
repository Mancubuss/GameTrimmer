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
mod uk;

pub use gametrimmer_core::settings::Lang;
pub use messages::*;

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

    // -- dialogs --
    pub elevation_heading: &'static str,
    pub elevation_body: &'static str,
    pub btn_continue_without_elevation: &'static str,
    pub btn_relaunch_elevated: &'static str,
    pub confirm_delete_heading: &'static str,
    pub confirm_label_permanent: &'static str,
    pub confirm_label_recycle: &'static str,
    pub remember_delete_method: &'static str,
    pub remove_summary_heading: &'static str,
    pub btn_close: &'static str,

    // -- settings_dialog --
    pub settings_heading: &'static str,
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
    pub scan_routing_label: &'static str,
    pub scan_routing_auto_label: &'static str,
    pub scan_routing_auto_hint: &'static str,
    pub scan_routing_force_mft_label: &'static str,
    pub scan_routing_force_mft_hint: &'static str,
    pub scan_routing_force_walkdir_label: &'static str,
    pub scan_routing_force_walkdir_hint: &'static str,
    pub app_language_label: &'static str,
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

    // -- libraries_panel --
    pub libraries_header: &'static str,
    pub btn_add_folder: &'static str,
    pub picking_folder: &'static str,
    pub no_libraries_registered: &'static str,
    pub btn_remove: &'static str,

    // -- tree_view --
    pub scanning_in_progress: &'static str,
    pub no_findings_hint: &'static str,
    pub col_language: &'static str,
    pub col_files: &'static str,
    pub col_size: &'static str,
    pub col_confidence: &'static str,
    pub col_name: &'static str,

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
    pub settings_not_saved_no_db: &'static str,

    // -- worker progress verbs --
    pub verb_scan: &'static str,
    pub verb_analyze: &'static str,
    pub verb_delete: &'static str,
    pub verb_compact: &'static str,
    pub verb_clear: &'static str,

    // -- model.rs: category display names --
    pub category_redist: &'static str,
    pub category_docs: &'static str,
    pub category_bonus: &'static str,
    pub category_loc: &'static str,
    pub category_other: &'static str,
    pub category_orphan: &'static str,

    // -- tree_view.rs: the synthetic orphan-branch pseudo-game label (GT-02) --
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
    /// Reading a volume's file table (the MFT pre-pass) - the "disk" phase.
    Scan,
    /// Classifying each game's files - the second, per-game phase. Labelled
    /// distinctly from [`Verb::Scan`] so the user doesn't read the two scan
    /// phases as "scanning twice"; see `worker::scan`.
    Analyze,
    Delete,
    Compact,
    Clear,
}

pub fn verb_label(lang: Lang, verb: Verb) -> &'static str {
    let s = strings(lang);
    match verb {
        Verb::Scan => s.verb_scan,
        Verb::Analyze => s.verb_analyze,
        Verb::Delete => s.verb_delete,
        Verb::Compact => s.verb_compact,
        Verb::Clear => s.verb_clear,
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
            ("elevation_heading", self.elevation_heading),
            ("elevation_body", self.elevation_body),
            (
                "btn_continue_without_elevation",
                self.btn_continue_without_elevation,
            ),
            ("btn_relaunch_elevated", self.btn_relaunch_elevated),
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
            ("scan_routing_label", self.scan_routing_label),
            ("scan_routing_auto_label", self.scan_routing_auto_label),
            ("scan_routing_auto_hint", self.scan_routing_auto_hint),
            (
                "scan_routing_force_mft_label",
                self.scan_routing_force_mft_label,
            ),
            (
                "scan_routing_force_mft_hint",
                self.scan_routing_force_mft_hint,
            ),
            (
                "scan_routing_force_walkdir_label",
                self.scan_routing_force_walkdir_label,
            ),
            (
                "scan_routing_force_walkdir_hint",
                self.scan_routing_force_walkdir_hint,
            ),
            ("app_language_label", self.app_language_label),
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
            ("libraries_header", self.libraries_header),
            ("btn_add_folder", self.btn_add_folder),
            ("picking_folder", self.picking_folder),
            ("no_libraries_registered", self.no_libraries_registered),
            ("btn_remove", self.btn_remove),
            ("scanning_in_progress", self.scanning_in_progress),
            ("no_findings_hint", self.no_findings_hint),
            ("col_language", self.col_language),
            ("col_files", self.col_files),
            ("col_size", self.col_size),
            ("col_confidence", self.col_confidence),
            ("col_name", self.col_name),
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
            ("settings_not_saved_no_db", self.settings_not_saved_no_db),
            ("verb_scan", self.verb_scan),
            ("verb_analyze", self.verb_analyze),
            ("verb_delete", self.verb_delete),
            ("verb_compact", self.verb_compact),
            ("verb_clear", self.verb_clear),
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
            for verb in [
                Verb::Scan,
                Verb::Analyze,
                Verb::Delete,
                Verb::Compact,
                Verb::Clear,
            ] {
                assert!(!verb_label(lang, verb).is_empty());
            }
        }
    }
}
