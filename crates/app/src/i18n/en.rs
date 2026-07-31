//! English strings - the default UI language. Translated from the Ukrainian
//! originals in [`super::uk`] (drafted with a local Ollama model, reviewed
//! and finalized by hand for natural, concise UI tone).

use super::Strings;

pub(super) const STRINGS: Strings = Strings {
    btn_scan_libraries: "Scan libraries",
    btn_cancel: "Cancel",
    btn_export: "Export…",
    btn_settings: "Settings…",

    btn_select_all: "Select all",
    btn_deselect_all: "Deselect all",
    btn_delete_selected: "Delete selected",

    settings_section_general: "General",
    settings_section_scanning: "Scanning",
    settings_section_selection: "Selection & deletion",
    settings_section_rules: "Rules",
    settings_section_data: "Data & diagnostics",
    btn_done: "Done",
    btn_restore_defaults: "Restore defaults",
    label_saved: "Saved",
    badge_immediately: "Immediately",
    badge_next_scan: "From the next scan",
    badge_next_delete: "From the next deletion",
    default_profile_label: "Profile a new scan starts from",
    default_profile_hint: "Does not change what is currently checked in the tree - only \
         where the next scan begins.",
    confirm_behavior_label: "Ask before deleting",
    confirm_always_label: "Always",
    confirm_only_1gb_label: "Only above 1 GB",
    confirm_never_label: "Never",
    confirm_behavior_hint: "When to show the confirmation before a deletion runs.",
    selection_independent_switches_hint: "What is scanned (the Scanning section), what is \
         pre-selected (the profile above) and how it is deleted (the method above) are three \
         independent switches - changing one leaves the other two alone.",
    keep_languages_add_placeholder: "Add a language\u{2026}",
    categories_table_header_category: "Category",
    categories_table_header_risk: "Risk",
    categories_table_header_profile_behavior: "Default profile",
    profile_behavior_auto: "Pre-selected",
    profile_behavior_manual: "Never automatic",
    disabled_last_keep_language: "At least one language has to stay on the keep-list.",
    disabled_last_category: "At least one category has to stay enabled.",
    keep_english_warning: "Taking English off the list deletes nothing by itself. It makes the \
                           next scan propose English localization files - interface text among \
                           them, which most games will not start without.",
    keep_english_absent: "English is not on the keep-list: scans propose English localization \
                          files, interface text among them.",
    btn_keep_english_again: "Keep English again",
    rules_pack_category_label: "Category rules (rules.json)",
    rules_pack_lang_label: "Language pack (l10n_rules.json)",
    rules_valid_label: "Valid",
    rules_invalid_label: "Does not parse",
    db_path_label: "Database file:",
    btn_copy: "Copy",
    btn_open_folder: "Open folder",
    danger_zone_label: "DANGER ZONE",

    disabled_busy: "A background operation is running",
    disabled_no_findings: "Scan your libraries first",
    disabled_no_selection: "Nothing is selected",
    disabled_export_running: "An export is already running",

    profile_label: "Profile:",
    profile_cautious: "Cautious",
    profile_balanced: "Balanced",
    profile_aggressive: "Aggressive",
    profile_custom: "Custom",
    profile_hint: "Which findings are pre-checked. Cautious: only what a launcher won't restore \
         (orphaned residue, bonus material, documentation). Balanced: + languages outside your \
         keep-list. Aggressive: + everything at 70% confidence or higher. Custom: the plain 85% \
         threshold — hand-pick the rest. Switching re-checks the current findings.",

    plan_filter_label: "Show:",
    plan_filter_all: "all categories",
    btn_remove_category: "Clean up whole category",
    search_hint: "Search by name\u{2026}",

    elevation_heading: "Speed up scanning?",
    elevation_body: "Fast scanning reads the NTFS file table ($MFT) directly, like the Everything \
         tool, which requires administrator rights. Without them, scanning will \
         be slower (a regular folder walk).",
    btn_continue_without_elevation: "Continue without acceleration",
    btn_relaunch_elevated: "Restart as administrator",
    confirm_delete_heading: "Confirm deletion",
    confirm_label_permanent: "Delete permanently",
    confirm_label_recycle: "Move to Recycle Bin",
    remember_delete_method: "Remember my choice",
    remove_summary_heading: "Deletion result",
    btn_close: "Close",
    confirm_clear_heading: "Clear database?",
    confirm_clear_body: "All scan results and the operations journal will be permanently \
         removed from the database. Files on disk are not touched, and your libraries and \
         settings are kept. This cannot be undone - you will need to scan again to see \
         results.",
    btn_confirm_clear: "Clear database",

    settings_heading: "Settings",
    delete_method_label: "File deletion method:",
    delete_method_permanent_label: "Permanent deletion (fastest)",
    delete_method_permanent_hint:
        "Files are deleted permanently. If something needed gets deleted, \
         the game can always be reinstalled from the store.",
    delete_method_recycle_label: "To the Windows Recycle Bin (slower)",
    delete_method_recycle_hint: "Files can be restored from the Recycle Bin until it's emptied.",
    database_label: "Database:",
    btn_compact_database: "Compact database",
    compact_hint: "Frees space the database no longer uses after deletions. Only runs if \
         at least 25% of the space would be reclaimed.",
    btn_clear_database: "Clear database",
    clear_hint: "Permanently removes all scan results and the operations journal from the \
         database. Files on disk are not touched; libraries and settings are kept.",
    rules_label: "Analysis rules:",
    btn_export_rules: "Export rules",
    btn_import_rules: "Import rules",
    rules_hint: "Export saves rules.json and l10n_rules.json to a chosen folder - a basis \
         for your own or community rules. Import merges the selected files into the \
         current rules (new entries are added, matches are updated) and saves them \
         next to the program; the previous files remain as *.bak. Changes take \
         effect from the next scan.",
    running_ellipsis: "Running...",
    keep_languages_label: "Languages never flagged:",
    keep_languages_hint: "Files identified as belonging to a checked language are never \
         proposed for deletion. At least one language must stay checked. \
         Changes take effect on the next scan.",
    scan_routing_label: "Scan file-enumeration method:",
    scan_routing_auto_label: "Auto (recommended)",
    scan_routing_auto_hint: "Uses the fast MFT index on hard drives and a regular folder walk \
         on SSDs/NVMe, whichever is faster for that drive type.",
    // Not "Always": elevation, volume-letter and canonical-path gates all
    // still send a game to the folder walk under this mode (audit §6.8).
    scan_routing_force_mft_label: "Prefer the MFT index where available",
    scan_routing_force_mft_hint: "Uses the MFT index even on SSDs/NVMe, where a folder walk is \
         normally faster. Requires administrator rights; volumes that can't be read this way \
         (not elevated, network drives, junctions/symlinks) still fall back to a folder walk. \
         Takes effect on the next scan.",
    scan_routing_force_walkdir_label: "Always walk folders",
    scan_routing_force_walkdir_hint: "Skips the MFT index entirely and always scans by walking \
         folders, even on hard drives where the MFT index is normally faster. Takes effect on \
         the next scan.",
    app_language_label: "App language:",
    lang_name_en: "English",
    lang_name_uk: "Ukrainian",
    theme_label: "Theme:",
    theme_system_label: "System (follow Windows)",
    theme_light_label: "Light",
    theme_dark_label: "Dark",
    categories_label: "Scanned artifact categories:",
    categories_hint: "Unchecked categories are skipped entirely during scanning - their files \
         are never analyzed, listed, or saved. At least one category must stay \
         checked. Changes take effect on the next scan.",
    logging_label: "Diagnostic log",
    logging_checkbox: "Write diagnostic log (gametrimmer.log next to the app)",
    logging_hint: "Only for troubleshooting: records errors and scan events. Off by default.",

    libraries_header: "Libraries",
    btn_add_folder: "Add folder...",
    picking_folder: "Selecting folder...",
    no_libraries_registered: "No libraries registered yet.",
    btn_remove: "Remove",

    scanning_in_progress: "Scanning...",
    no_findings_hint: "No findings. Click \u{201c}Scan libraries\u{201d} to begin.",
    col_language: "Language",
    col_files: "Files",
    col_size: "Size",
    col_confidence: "Confidence",
    col_name: "Name",

    ctx_reveal_in_explorer: "Reveal in Explorer",
    ctx_open_with: "Open with\u{2026}",
    ctx_copy_path: "Copy path",

    add_library_dialog_title: "Choose a library folder",
    export_dialog_title: "Export analysis results",
    text_file_filter_label: "Text file",
    rules_export_dialog_title: "Choose a folder to export rules into",
    rules_import_dialog_title: "Choose rule files to import",
    rules_import_filter_label: "GameTrimmer rules (JSON)",

    no_db_path: "No database path.",
    db_path_error: "Failed to determine the database path.",
    detecting_libraries: "Detecting game libraries...",
    preparing_database: "Preparing the database...",
    loading_previous_scan: "Loading previous scan results...",
    deleting_selected_files: "Deleting selected files...",
    compacting_database: "Compacting the database...",
    clearing_database: "Clearing the database...",
    scan_cancelled: "Scan cancelled.",
    deletion_completed: "Deletion completed.",
    database_compacted: "Database compacted.",
    database_cleared: "Database cleared.",
    settings_not_saved_no_db: "Settings not saved: no database path.",

    verb_scan: "Scanning",
    verb_analyze: "Analyzing",
    verb_delete: "Deleting",
    verb_compact: "Compacting database",
    verb_clear: "Clearing database",

    category_redist: "Redistributables",
    category_docs: "Documentation and reference material",
    category_bonus: "Bonus content",
    category_loc: "Localization files",
    category_other: "Other",
    category_orphan: "Orphaned",

    orphan_branch_label: "Orphaned residue",

    unit_gb: "GB",
    unit_mb: "MB",
    unit_kb: "KB",
    unit_b: "B",

    csv_yes: "yes",
    csv_no: "no",
};
