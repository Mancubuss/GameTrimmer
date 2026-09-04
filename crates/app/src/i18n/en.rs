//! English strings - the default and, while localizations are frozen for
//! development (Vikunja #443 tracks unfreezing them), the only UI language
//! this app compiles in. Originally translated from the Ukrainian strings
//! (drafted with a local Ollama model, reviewed and finalized by hand for
//! natural, concise UI tone) that used to live beside this file.

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
    settings_section_monitoring: "Background monitoring",
    settings_section_scanning: "Scanning",
    settings_section_selection: "Selection & deletion",
    settings_section_rules: "Rules",
    settings_section_data: "Data & diagnostics",
    watch_enabled_label: "Enable background update monitoring",
    watch_enabled_hint:
        "Monitors game manifests for launcher updates and re-scans or trims automatically.",
    watch_autostart_label: "Start monitoring automatically with Windows",
    watch_autostart_hint: "Launches gametrimmer-watch in background on user login.",
    watch_mode_label: "Update action mode",
    watch_mode_interactive: "Interactive notification",
    watch_mode_interactive_hint:
        "Shows a Windows Toast notification with a button to trim re-downloaded files.",
    watch_mode_autotrim: "Silent auto-trim",
    watch_mode_autotrim_hint:
        "Silently re-trims newly updated games without prompting. Not active yet.",
    watch_mode_passive: "Passive badge only",
    watch_mode_passive_hint:
        "Updates status badges in GameTrimmer without automatic deletion or toasts.",
    watch_daemon_status_running: "Active (IPC connected)",
    watch_daemon_status_stopped: "Stopped (daemon not running)",
    btn_watch_rescan_now: "Check / Rescan now",
    watch_tray_tooltip_active: "GameTrimmer Watcher (Active)",
    watch_tray_tooltip_paused: "GameTrimmer Watcher (Paused)",
    watch_tray_menu_open: "Open GameTrimmer",
    watch_tray_menu_check_now: "Check now",
    watch_tray_menu_pause: "Pause monitoring",
    watch_tray_menu_resume: "Resume monitoring",
    watch_tray_menu_exit: "Exit",
    watch_toast_updated_transition:
        "{name} was updated ({old} → {new}). Click to re-trim and reclaim space.",
    watch_toast_updated_build:
        "{name} was updated (build {new}). Click to re-trim and reclaim space.",
    watch_toast_files_changed: "{name} files changed. Click to re-trim and reclaim space.",
    watch_toast_daemon_title: "GameTrimmer Watcher",
    btn_done: "Done",
    btn_restore_defaults: "Restore defaults",
    label_saved: "Saved",
    badge_immediately: "Immediately",
    badge_next_scan: "From the next scan",
    badge_next_delete: "From the next deletion",
    confirm_behavior_label: "Ask before deleting",
    confirm_yes_label: "Yes",
    confirm_no_label: "No",
    confirm_behavior_hint: "With this off, the delete starts the moment you press the button \
         - there is no second chance to look at what was ticked.",
    selection_independent_switches_hint: "What is scanned (the Scanning section) and how \
         a file is disposed of (the method above) are independent switches - changing one \
         leaves the other alone. Neither decides what gets ticked: a scan ticks nothing, \
         and what goes is only ever what you ticked yourself.",
    keep_languages_add_placeholder: "Add a language\u{2026}",
    categories_table_header_category: "Category",
    categories_table_header_risk: "Risk",
    disabled_last_keep_language: "At least one language has to stay on the keep-list.",
    disabled_last_category: "At least one category has to stay enabled.",
    disabled_last_library: "At least one library has to stay included in the scan.",
    keep_english_warning: "Taking English off the list deletes nothing by itself. It makes the \
                           next scan propose English localization files - interface text among \
                           them, which most games will not start without.",
    keep_english_absent: "English is not on the keep-list: scans propose English localization \
                          files, interface text among them.",
    btn_keep_english_again: "Keep English again",
    rules_found_label: "Your own rule files found here:",
    rules_valid_label: "in use",
    rules_invalid_label: "does not parse - ignored",
    db_path_label: "Database file:",
    btn_copy: "Copy",
    btn_open_folder: "Open folder",
    danger_zone_label: "DANGER ZONE",

    disabled_busy: "A background operation is running",
    disabled_no_findings: "Scan your libraries first",
    disabled_no_selection: "Nothing is selected",
    disabled_export_running: "An export is already running",
    disabled_disclaimer: "Read and accept the disclaimer on the start screen first",
    disabled_database: "The database could not be opened",

    plan_filter_label: "Show:",
    plan_filter_all: "all categories",
    plan_group_label: "Group by:",
    group_axis_disk: "disk",
    group_axis_launcher: "launcher",
    group_axis_library: "library",
    group_axis_category: "category",
    group_axis_flat: "nothing",
    group_unattributed: "Unattributed",
    btn_remove_category: "Clean up whole category",
    search_hint: "Search by name\u{2026}",
    btn_clear_search: "Clear the search",
    search_no_matches: "Nothing matches the current search or category filter. Clear them to \
         see the findings again.",

    elevation_heading: "Speed up scanning?",
    elevation_body: "At least one of your game libraries is on a hard drive. There GameTrimmer \
         can read the NTFS file table ($MFT) directly instead of walking every folder - the \
         technique the Everything search tool uses, and much faster on a spinning disk. \
         Reading a volume that way is something Windows permits only to an administrator, so \
         this is the one thing the program cannot do for you unelevated.",
    elevation_when_asked: "Declining costs time, not results: the scan walks folders instead and \
         finds exactly the same files. You will not be asked on a machine whose libraries are \
         all on SSD or NVMe - there walking is the faster route anyway, so administrator rights \
         would buy nothing.",
    btn_continue_without_elevation: "Continue without acceleration",
    btn_relaunch_elevated: "Restart as administrator",
    elevation_never_ask: "Don't ask again",
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
    clear_hint: "Removes all scan results from the database. Files on disk are not touched; \
         libraries and settings are kept. The only cost is scanning again.",
    rules_hint: "GameTrimmer analyses with the rules built into it, so updating the program updates them - nothing is stored next to it and nothing goes stale. To extend them, put a rules.json or an l10n_rules.json of your own in this folder: it is applied on top of the built-in rules from the next scan, and can only add to them. The format is described in rules-packs.md beside the program, with a ready example of each in the templates folder.",
    running_ellipsis: "Running...",
    keep_languages_label: "Languages never flagged:",
    keep_languages_hint: "Files identified as belonging to a checked language are never \
         proposed for deletion. At least one language must stay checked. \
         Changes take effect on the next scan.",
    scan_method_label: "How the last scan read files:",
    scan_method_hint: "Chosen per drive: the NTFS index on hard drives, a folder walk on SSDs, \
         where walking is faster.",
    app_language_label: "App language:",
    lang_name_system: "Follow Windows",
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
    logging_hint: "Records errors and scan events locally. On by default; you can turn it off.",
    log_path_label: "Log file:",
    bundle_label: "Diagnostic bundle",
    bundle_hint: "Collects a .zip you can attach to a bug report. It is written to \
         a folder you choose and sent by nobody - GameTrimmer has no network \
         code. Read the preview below: that text is in the file.",
    btn_generate_bundle: "Save diagnostic bundle...",
    bundle_titles_checkbox: "Include real game names (otherwise Game 1, Game 2, ...)",
    bundle_operations_checkbox: "Include the deletion journal row by row (otherwise counts only)",
    bundle_preview_label: "What the bundle will say about this machine:",
    bundle_save_title: "Save diagnostic bundle",

    libraries_header: "Libraries",
    btn_add_folder: "Add folder...",
    picking_folder: "Selecting folder...",
    no_libraries_registered: "No libraries registered yet.",
    btn_remove: "Remove",
    library_include_checkbox: "Include in scan",

    onboarding_heading: "Free up space without uninstalling games",
    onboarding_step_scan: "1. Scan. GameTrimmer lists what can go: localizations you do not \
         use, leftovers of deleted games, redistributables, bonus material.",
    onboarding_step_review: "2. Look through it, grouped by disk and game. Every file is \
         there to inspect, with its full path.",
    onboarding_step_remove: "3. Remove what you ticked - permanently or to the Recycle Bin, \
         as you choose.",
    onboarding_how_heading: "How it decides",
    onboarding_how_body: "Never by a file's size or age. A rule pack matches known file and \
         folder names, a language detector identifies localization files, and an orphan check \
         compares each launcher's manifests against what is on disk.",
    onboarding_filters_body: "Settings \u{2192} Scanning narrows that: a switched-off category \
         is never analyzed, and a language on the keep-list is never proposed for deletion. \
         The README beside the program describes both detectors and the rule files in full.",
    onboarding_selection: "A finished scan arrives with nothing ticked. The app proposes \
         nothing for deletion on your behalf - what goes is what you tick.",
    onboarding_review_mark: "A \u{26a0} beside a file means the app is less sure of it - \
         worth a longer look before you tick it.",
    onboarding_safety: "Scanning changes nothing on disk. Nothing is deleted until you tick \
         it and confirm.",
    onboarding_logging_body:
        "On by default. Writes gametrimmer.log next to the program: errors, and what \
         the scan did on which volume. Nothing leaves your machine - there is no telemetry \
         here. If you ever hit a wrong finding, that file is the difference between a fixable \
         report and guesswork. Settings \u{2192} Data & diagnostics switches it off again.",

    disclaimer_heading: "USE AT YOUR OWN RISK",
    disclaimer_body: "GameTrimmer deletes the files you tick. Detection is heuristic: a file \
         can be misidentified, and removing the wrong one can leave a game unable to start. \
         The program is provided \u{201c}as is\u{201d}, without warranty of any kind, and its \
         authors are not liable for lost data, broken installations, or anything else arising \
         from its use (see LICENSE, beside the program). What you delete is your decision and \
         your responsibility - keep backups of anything you cannot re-download. Game files \
         themselves can normally be restored by verifying or reinstalling from the store.",
    disclaimer_accept_checkbox: "I have read the above. I accept the risk and take \
         responsibility for what I delete.",
    disclaimer_already_accepted: "Already accepted - this one cannot be taken back. The \
         program keeps no other record of it.",

    credits_heading: "Thanks",
    credits_anthropic: "Anthropic - for Claude Code, with which this program was written.",
    credits_karpathy: "Andrej Karpathy - for the inspiration.",
    credits_tikione: "The author of TikiOne Steam Cleaner - for the idea, and for the first \
         set of filters it started from.",

    scanning_in_progress: "Scanning...",
    no_findings_hint: "No findings. Click \u{201c}Scan libraries\u{201d} to begin.",
    col_language: "Language",
    col_files: "Files",
    col_size: "Size",
    col_name: "Name",
    col_sort_hint: "Click to sort by this column, again to reverse it, \
                    a third time for the original order",
    review_mark_hint: "The app is less sure about this file, so it was not ticked for you. \
         Check what it is - the tooltip on its name gives the path and the reason - before \
         you delete it.",
    hover_stub_note:
        "Replaced with a format-aware micro-stub on removal for crash-free instant game launch.",

    ctx_open: "Open",
    ctx_reveal_in_explorer: "Reveal in Explorer",
    ctx_open_with: "Open with\u{2026}",
    ctx_copy_path: "Copy path",
    ctx_never_touch: "Never touch this file in this game",
    ctx_never_touch_needs_app_id: "Only for games a launcher identifies. This one was found by \
         folder scan, so there is no id to pin the exception to.",
    game_without_launcher_id: "No launcher lists this game - it was found by scanning folders, \
         or you added it by hand. It has no launcher id, so anything read from a launcher \
         manifest is unavailable for it, including personal exceptions.",
    btn_find_standalone: "Find games installed outside launchers",
    find_standalone_hint: "Reads the Windows uninstall list for programs that no launcher \
         manages. It cannot tell a game from any other program, so it offers folders rather \
         than adding them.",
    standalone_candidates_header: "Installed outside any launcher - add the ones that are games:",
    no_standalone_candidates: "Nothing found outside your launchers.",

    add_library_dialog_title: "Choose a library folder",
    export_dialog_title: "Export analysis results",
    text_file_filter_label: "Text file",

    no_db_path: "No database path.",
    db_path_error: "Failed to determine the database path.",
    detecting_libraries: "Detecting game libraries...",
    preparing_database: "Preparing the database...",
    finishing_scan: "Finishing...",
    loading_previous_scan: "Loading previous scan results...",
    deleting_selected_files: "Deleting selected files...",
    compacting_database: "Compacting the database...",
    clearing_database: "Clearing the database...",
    scan_cancelled: "Scan cancelled.",
    deletion_completed: "Deletion completed.",
    database_compacted: "Database compacted.",
    database_cleared: "Database cleared.",
    settings_not_saved_no_path: "Settings not saved: no path to gametrimmer.ini.",

    verb_analyze: "Analyzing",
    verb_delete: "Deleting",
    verb_compact: "Compacting database",
    verb_clear: "Clearing database",
    verb_bundle: "Collecting diagnostics",

    category_redist: "Redistributables",
    category_intro: "Intro and startup videos",
    category_docs: "Documentation and reference material",
    category_bonus: "Bonus content",
    category_loc: "Localization files",
    category_dev_leftovers: "Development leftovers",
    category_orphan: "Orphaned",
    category_workshop: "Workshop mods",
    category_shader_cache: "Shader caches",
    category_crashes: "Crash dumps & logs",
    category_saves: "Save games & autosaves",
    category_launcher_cache: "Launcher web caches",
    category_mod_downloads: "Mod manager archives",
    badge_safe: "Safe to clean",
    badge_review: "Review recommended",
    badge_backup_shield: "Protected by backup",
    saves_pruner_title: "Smart Save Pruner",
    saves_retention_slider: "Keep latest quicksaves per game",
    saves_auto_backup_label: "Automatic ZIP backup before deletion",
    saves_backup_success: "Save backup created successfully",
    saves_total_prunable: "Prunable autosaves found",

    orphan_branch_label: "Orphaned residue",

    system_branch_label: "System and launcher files",

    unit_gb: "GB",
    unit_mb: "MB",
    unit_kb: "KB",
    unit_b: "B",

    csv_yes: "yes",
    csv_no: "no",

    already_running_title: "GameTrimmer is already running",

    badge_anticheat_shield: "🛡️ Anti-Cheat Protected",
    anticheat_shield_tooltip: "This game uses anti-cheat. This only blocks an unattended \
                                re-trim of it; everything else behaves normally.",
    group_checkbox_disabled_hint: "Nothing here is selected, and nothing qualifies for Select all.",
};
