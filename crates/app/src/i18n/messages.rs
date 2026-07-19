//! Messages that interpolate a count, a path, or an error into otherwise
//! static text. Kept as plain functions (rather than more `Strings` fields
//! with `{0}`-style placeholders) so the interpolation is ordinary,
//! type-checked `format!` - no runtime template parsing, no way for a
//! placeholder count to silently drift from its call site.

use super::Lang;

/// "Scanned N games (MFT: x, walkdir: y) in s.s sec." - the scan-method
/// breakdown shown in the final status line after a scan completes. Pure
/// formatting, unit-tested via `worker::scan_route::format_scan_summary`
/// (which calls straight through to this).
pub fn format_scan_summary(
    lang: Lang,
    total: usize,
    mft: usize,
    walkdir: usize,
    elapsed_secs: f64,
) -> String {
    match lang {
        Lang::En => format!(
            "Scanned {total} game(s) (MFT: {mft}, walkdir: {walkdir}) in {elapsed_secs:.1} sec."
        ),
        Lang::Uk => format!(
            "Проскановано {total} ігор (MFT: {mft}, обхід тек: {walkdir}) за {elapsed_secs:.1} с."
        ),
    }
}

pub fn db_open_error_long(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!(
            "Failed to open the database: {err}. Move the program to a folder with write \
             access (not Program Files without administrator rights)."
        ),
        Lang::Uk => format!(
            "Помилка відкриття бази даних: {err}. Перемістіть програму в теку з \
             правами на запис (не Program Files без прав адміністратора)."
        ),
    }
}

pub fn db_open_error_short(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Failed to open the database: {err}"),
        Lang::Uk => format!("Помилка відкриття бази даних: {err}"),
    }
}

pub fn add_library_failed(
    lang: Lang,
    path: impl std::fmt::Display,
    err: impl std::fmt::Display,
) -> String {
    match lang {
        Lang::En => format!("Failed to add folder {path}: {err}"),
        Lang::Uk => format!("Не вдалося додати теку {path}: {err}"),
    }
}

pub fn remove_library_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Failed to remove library: {err}"),
        Lang::Uk => format!("Не вдалося прибрати бібліотеку: {err}"),
    }
}

pub fn libraries_found(lang: Lang, libraries: usize, games: usize) -> String {
    match lang {
        Lang::En => format!("Found {libraries} librar(y/ies), {games} game(s). Scanning files..."),
        Lang::Uk => format!("Знайдено бібліотек: {libraries}, ігор: {games}. Сканування файлів..."),
    }
}

pub fn scan_done_status(lang: Lang, scan_summary: &str, count: usize) -> String {
    match lang {
        Lang::En => format!("{scan_summary} Found {count} file(s) to review."),
        Lang::Uk => format!("{scan_summary} Знайдено {count} файл(ів) для перевірки."),
    }
}

pub fn remove_done_status(lang: Lang, succeeded: usize, failed: usize) -> String {
    match lang {
        Lang::En => format!("Deletion completed: {succeeded} succeeded, {failed} failed."),
        Lang::Uk => format!("Видалення завершено: успішно {succeeded}, помилок {failed}."),
    }
}

pub fn error_prefixed(lang: Lang, msg: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Error: {msg}"),
        Lang::Uk => format!("Помилка: {msg}"),
    }
}

pub fn export_save_failed(lang: Lang, error: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Failed to save the export: {error}"),
        Lang::Uk => format!("Не вдалося зберегти експорт: {error}"),
    }
}

pub fn exported_to(lang: Lang, path: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Exported to: {path}"),
        Lang::Uk => format!("Експортовано: {path}"),
    }
}

pub fn rules_export_failed(lang: Lang, error: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Failed to export rules: {error}"),
        Lang::Uk => format!("Не вдалося експортувати правила: {error}"),
    }
}

pub fn rules_exported_to(lang: Lang, path: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Rules exported to {path}"),
        Lang::Uk => format!("Правила експортовано до {path}"),
    }
}

pub fn rules_import_failed(lang: Lang, error: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Failed to import rules: {error}"),
        Lang::Uk => format!("Не вдалося імпортувати правила: {error}"),
    }
}

pub fn settings_save_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Failed to save settings: {err}"),
        Lang::Uk => format!("Не вдалося зберегти налаштування: {err}"),
    }
}

// -- worker::scan --

pub fn rules_json_load_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => {
            format!("Failed to load rules.json: {err} - using the built-in category rules.")
        }
        Lang::Uk => format!(
            "Помилка завантаження rules.json: {err} - використовую вбудовані правила категорій."
        ),
    }
}

pub fn builtin_rules_corrupted(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Built-in rules are corrupted: {err}"),
        Lang::Uk => format!("Вбудовані правила пошкоджено: {err}"),
    }
}

pub fn l10n_rules_load_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => {
            format!("Failed to load l10n_rules.json: {err} - using the built-in language rules.")
        }
        Lang::Uk => format!(
            "Помилка завантаження l10n_rules.json: {err} - використовую вбудовані мовні правила."
        ),
    }
}

pub fn provider_failed(
    lang: Lang,
    name: impl std::fmt::Display,
    err: impl std::fmt::Display,
) -> String {
    match lang {
        Lang::En => format!("Provider \"{name}\": {err}"),
        Lang::Uk => format!("Провайдер \"{name}\": {err}"),
    }
}

pub fn manual_libraries_read_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Failed to read manual libraries: {err}"),
        Lang::Uk => format!("Помилка читання ручних бібліотек: {err}"),
    }
}

pub fn no_libraries_found(lang: Lang) -> String {
    match lang {
        Lang::En => "No libraries found.".to_string(),
        Lang::Uk => "Бібліотек не знайдено.".to_string(),
    }
}

pub fn libraries_write_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Failed to write libraries to the database: {err}"),
        Lang::Uk => format!("Помилка запису бібліотек у базу даних: {err}"),
    }
}

pub fn write_thread_crashed(lang: Lang) -> String {
    match lang {
        Lang::En => "The scan results writer thread crashed.".to_string(),
        Lang::Uk => "Потік запису результатів сканування завершився аварійно.".to_string(),
    }
}

pub fn reading_mft_detail(lang: Lang, volume: char, percent: u64) -> String {
    match lang {
        Lang::En => format!("Reading file table {volume}: — {percent}%"),
        Lang::Uk => format!("Читання файлової таблиці {volume}: — {percent}%"),
    }
}

// -- worker::manual --

pub fn manual_library_unavailable(lang: Lang, path: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => {
            format!("Manual library \"{path}\" is unavailable (disk disconnected or folder moved).")
        }
        Lang::Uk => {
            format!("Ручна бібліотека \"{path}\" недоступна (диск від'єднано або теку переміщено).")
        }
    }
}

// -- worker::delete --

pub fn delete_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Deletion failed: {err}"),
        Lang::Uk => format!("Помилка видалення: {err}"),
    }
}

pub fn db_update_after_delete_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Failed to update the database after deletion: {err}"),
        Lang::Uk => format!("Не вдалося оновити базу даних після видалення: {err}"),
    }
}

// -- worker::compact --

pub fn compact_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Failed to compact the database: {err}"),
        Lang::Uk => format!("Не вдалося стиснути базу даних: {err}"),
    }
}

// -- worker::load --

pub fn loaded_saved_results(lang: Lang) -> String {
    match lang {
        Lang::En => "Showing saved results from a previous scan.".to_string(),
        Lang::Uk => "Показано збережені результати попереднього сканування.".to_string(),
    }
}

pub fn load_previous_results_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Failed to load previous scan results: {err}"),
        Lang::Uk => format!("Помилка завантаження результатів попереднього сканування: {err}"),
    }
}

// -- worker::rules_io --

pub fn prepare_rules_file_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("failed to prepare the rules file: {err}"),
        Lang::Uk => format!("не вдалося підготувати файл правил: {err}"),
    }
}

pub fn read_file_failed(
    lang: Lang,
    path: impl std::fmt::Display,
    err: impl std::fmt::Display,
) -> String {
    match lang {
        Lang::En => format!("failed to read {path}: {err}"),
        Lang::Uk => format!("не вдалося прочитати {path}: {err}"),
    }
}

pub fn previous_files_already_imported(lang: Lang, index: usize) -> String {
    match lang {
        Lang::En => format!(" ({index} previous file(s) already imported)"),
        Lang::Uk => format!(" (попередні {index} файл(и) вже імпортовано)"),
    }
}

pub fn read_picked_file_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("failed to read the file: {err}"),
        Lang::Uk => format!("не вдалося прочитати файл: {err}"),
    }
}

pub fn prepare_rules_json_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("failed to prepare rules.json: {err}"),
        Lang::Uk => format!("не вдалося підготувати rules.json: {err}"),
    }
}

pub fn prepare_l10n_rules_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("failed to prepare l10n_rules.json: {err}"),
        Lang::Uk => format!("не вдалося підготувати l10n_rules.json: {err}"),
    }
}

pub fn backup_failed(
    lang: Lang,
    path: impl std::fmt::Display,
    err: impl std::fmt::Display,
) -> String {
    match lang {
        Lang::En => format!("failed to create a backup copy {path}: {err}"),
        Lang::Uk => format!("не вдалося створити резервну копію {path}: {err}"),
    }
}

pub fn write_failed(
    lang: Lang,
    path: impl std::fmt::Display,
    err: impl std::fmt::Display,
) -> String {
    match lang {
        Lang::En => format!("failed to write {path}: {err}"),
        Lang::Uk => format!("не вдалося записати {path}: {err}"),
    }
}

pub fn summary_categories_part(lang: Lang, added: usize, updated: usize) -> String {
    match lang {
        Lang::En => format!("categories - {added} new, {updated} updated"),
        Lang::Uk => format!("категорії — нових {added}, оновлено {updated}"),
    }
}

pub fn summary_lang_part(lang: Lang, added: usize, updated: usize) -> String {
    match lang {
        Lang::En => format!("localization - {added} new language(s), {updated} new word(s)"),
        Lang::Uk => format!("локалізація — нових мов {added}, нових слів {updated}"),
    }
}

pub fn summary_final(lang: Lang, parts: &str) -> String {
    match lang {
        Lang::En => format!("Rules imported: {parts}. Changes take effect from the next scan."),
        Lang::Uk => {
            format!("Правила імпортовано: {parts}. Зміни діятимуть з наступного сканування.")
        }
    }
}

// -- ui::dialogs --

pub fn confirm_permanent_question(lang: Lang, count: usize, size: &str) -> String {
    match lang {
        Lang::En => format!(
            "Permanently delete {count} file(s) ({size})? This cannot be undone \
             (the game can be reinstalled from the store)."
        ),
        Lang::Uk => format!(
            "Безповоротно видалити {count} файл(ів) ({size})? Відновлення буде \
             неможливе (гру можна перевстановити з магазину)."
        ),
    }
}

pub fn confirm_recycle_question(lang: Lang, count: usize, size: &str) -> String {
    match lang {
        Lang::En => format!("Move {count} file(s) ({size}) to the Recycle Bin?"),
        Lang::Uk => format!("Перемістити {count} файл(ів) ({size}) у Кошик?"),
    }
}

pub fn success_line_permanent(lang: Lang, succeeded: usize) -> String {
    match lang {
        Lang::En => format!("Successfully deleted: {succeeded}"),
        Lang::Uk => format!("Успішно видалено: {succeeded}"),
    }
}

pub fn success_line_recycle(lang: Lang, succeeded: usize) -> String {
    match lang {
        Lang::En => format!("Successfully moved to Recycle Bin: {succeeded}"),
        Lang::Uk => format!("Успішно переміщено в Кошик: {succeeded}"),
    }
}

pub fn errors_count_line(lang: Lang, failed_count: usize) -> String {
    match lang {
        Lang::En => format!("Errors: {failed_count}"),
        Lang::Uk => format!("Помилок: {failed_count}"),
    }
}

pub fn more_errors_line(lang: Lang, remaining: usize) -> String {
    match lang {
        Lang::En => format!("... and {remaining} more error(s)"),
        Lang::Uk => format!("... і ще {remaining} помилка(ок)"),
    }
}

// -- ui::bottom_bar --

pub fn selection_hint(lang: Lang, threshold: u8) -> String {
    match lang {
        Lang::En => format!(
            "Only findings with confidence of at least {threshold}% are automatically \
             selected after a scan. Findings with lower confidence are marked \u{26a0} and left \
             unselected - review them manually.\n\n\
             Checkboxes on tree rows select a whole disk, game, category, or folder. \
             Right-clicking a disk, game, or category opens bulk selection actions \
             (including a category across the whole disk).\n\n\
             Keyboard: \u{2191}\u{2193} - cursor, PgUp/PgDn - page, \
             \u{2192}/\u{2190} - expand/collapse, Space - select."
        ),
        Lang::Uk => format!(
            "Після сканування автоматично вибираються лише знахідки з упевненістю \
             не нижче {threshold}%. Знахідки з нижчою упевненістю \
             позначені \u{26a0} і залишаються невибраними — перегляньте їх вручну.\n\n\
             Прапорці на рядках дерева вибирають цілий диск, гру, категорію чи теку. \
             Права кнопка миші на диску, грі або категорії відкриває дії масового вибору \
             (зокрема категорію на всьому диску).\n\n\
             Клавіатура: \u{2191}\u{2193} — курсор, PgUp/PgDn — сторінка, \
             \u{2192}/\u{2190} — розгорнути/згорнути, Space — вибрати."
        ),
    }
}

pub fn selected_summary(lang: Lang, count: usize, size: &str) -> String {
    match lang {
        Lang::En => format!("Selected {count} file(s), will free up {size}"),
        Lang::Uk => format!("Вибрано {count} файл(ів), буде звільнено {size}"),
    }
}

// -- ui::top_bar --

pub fn warnings_header(lang: Lang, count: usize) -> String {
    match lang {
        Lang::En => format!("Warnings ({count})"),
        Lang::Uk => format!("Попередження ({count})"),
    }
}

// -- ui::tree_view --

pub fn disk_label(lang: Lang, disk: &str) -> String {
    match lang {
        Lang::En => format!("Disk {disk}"),
        Lang::Uk => format!("Диск {disk}"),
    }
}

pub fn quoted(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("\u{201c}{name}\u{201d}"),
        Lang::Uk => format!("\u{ab}{name}\u{bb}"),
    }
}

pub fn select_all_on_disk(lang: Lang, disk: &str) -> String {
    match lang {
        Lang::En => format!("Select all on disk {disk}"),
        Lang::Uk => format!("Вибрати все на диску {disk}"),
    }
}

pub fn deselect_all_on_disk(lang: Lang, disk: &str) -> String {
    match lang {
        Lang::En => format!("Deselect all on disk {disk}"),
        Lang::Uk => format!("Зняти вибір на диску {disk}"),
    }
}

pub fn select_all_in_game(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("Select all in {}", quoted(lang, name)),
        Lang::Uk => format!("Вибрати все у {}", quoted(lang, name)),
    }
}

pub fn deselect_all_in_game(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("Deselect all in {}", quoted(lang, name)),
        Lang::Uk => format!("Зняти вибір у {}", quoted(lang, name)),
    }
}

pub fn select_category_on_disk(lang: Lang, label: &str, disk: &str) -> String {
    match lang {
        Lang::En => format!("Select {} on disk {disk}", quoted(lang, label)),
        Lang::Uk => format!("Вибрати {} на всьому диску {disk}", quoted(lang, label)),
    }
}

pub fn deselect_category_on_disk(lang: Lang, label: &str, disk: &str) -> String {
    match lang {
        Lang::En => format!("Deselect {} on disk {disk}", quoted(lang, label)),
        Lang::Uk => format!("Зняти вибір {} на всьому диску {disk}", quoted(lang, label)),
    }
}

pub fn hover_reason(lang: Lang, rel_path: &str, rule_desc: &str, confidence: u8) -> String {
    match lang {
        Lang::En => format!("{rel_path}\nReason: {rule_desc} (confidence {confidence}%)"),
        Lang::Uk => format!("{rel_path}\nПричина: {rule_desc} (упевненість {confidence}%)"),
    }
}

pub fn hover_lang_suffix(lang: Lang, lang_tag: &str) -> String {
    match lang {
        Lang::En => format!("\nLanguage: {lang_tag}"),
        Lang::Uk => format!("\nМова: {lang_tag}"),
    }
}

// -- ui::settings_dialog --

/// Human-readable display name for a language code shown in the keep-list
/// checkboxes, e.g. `"uk"` -> `"Українська (uk)"`, `"en"` -> `"English
/// (en)"`. The name itself is the language's own native name - it does not
/// depend on the active UI language, matching how language pickers
/// conventionally show each entry in its own language. Covers every code
/// currently in `l10n_rules.json`; a code from a future community pack that
/// isn't in this table falls back to showing the bare code, which is still
/// usable (if less polished) than an empty label.
pub fn lang_display_name(code: &str) -> String {
    let native = match code {
        "en" => "English",
        "uk" => "Українська",
        "fr" => "Français",
        "de" => "Deutsch",
        "es" => "Español",
        "es-419" => "Español (Latinoamérica)",
        "pt" => "Português",
        "pt-br" => "Português (Brasil)",
        "it" => "Italiano",
        "ru" => "Русский",
        "pl" => "Polski",
        "cs" => "Čeština",
        "ja" => "日本語",
        "ko" => "한국어",
        "zh-hans" => "简体中文",
        "zh-hant" => "繁體中文",
        "tr" => "Türkçe",
        "ar" => "العربية",
        "th" => "ไทย",
        "vi" => "Tiếng Việt",
        "hu" => "Magyar",
        "nl" => "Nederlands",
        "da" => "Dansk",
        "no" => "Norsk",
        "sv" => "Svenska",
        "fi" => "Suomi",
        "el" => "Ελληνικά",
        "ro" => "Română",
        "bg" => "Български",
        "hr" => "Hrvatski",
        "sr" => "Српски",
        "sk" => "Slovenčina",
        "sl" => "Slovenščina",
        "id" => "Bahasa Indonesia",
        "hi" => "हिन्दी",
        "he" => "עברית",
        _ => return code.to_string(),
    };
    format!("{native} ({code})")
}

// -- export.rs --

pub fn csv_header(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Disk;Category;Game;Group folder;Path;Size (bytes);Size;Confidence;Source;Rule/reason;Language;Selected",
        Lang::Uk => "Диск;Категорія;Гра;Тека групи;Шлях;Розмір (байт);Розмір;Впевненість;Джерело;Правило/причина;Мова;Вибрано",
    }
}
