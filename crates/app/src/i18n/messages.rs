//! Messages that interpolate a count, a path, or an error into otherwise
//! static text. Kept as plain functions (rather than more `Strings` fields
//! with `{0}`-style placeholders) so the interpolation is ordinary,
//! type-checked `format!` - no runtime template parsing, no way for a
//! placeholder count to silently drift from its call site.

use gametrimmer_core::langdetect::{LangEvidence, LangReason};

use super::{strings, Lang};
use crate::model::RiskLevel;

/// Why one root was walked instead of read from the MFT index, in the
/// settings dialog's routing diagnostics (see
/// `worker::scan_route::format_walkdir_breakdown`). Phrased as a cause, not
/// as an error: most of these are correct decisions, not failures.
pub fn walkdir_reason_label(
    lang: Lang,
    reason: crate::worker::scan_route::WalkdirReason,
) -> &'static str {
    use crate::worker::scan_route::WalkdirReason as R;
    match (lang, reason) {
        (Lang::En, R::NotElevated) => "not running as administrator",
        (Lang::En, R::NoVolumeLetter) => "not on a lettered local drive",
        (Lang::En, R::VolumeUnavailable) => "volume could not be opened",
        (Lang::En, R::SsdVolume) => "SSD, where walking is faster",
        (Lang::En, R::CanonicalMismatch) => "junction, symlink or subst drive",
        (Lang::En, R::MftFailed) => "the MFT read failed",
        (Lang::En, R::MftEmptyOnNonEmptyDisk) => "the MFT returned nothing for a non-empty folder",
        (Lang::En, R::ForcedBySetting) => "the directory walk is forced in Settings",
        (Lang::Uk, R::NotElevated) => "запущено не від імені адміністратора",
        (Lang::Uk, R::NoVolumeLetter) => "не на локальному диску з літерою",
        (Lang::Uk, R::VolumeUnavailable) => "не вдалося відкрити том",
        (Lang::Uk, R::SsdVolume) => "SSD, де обхід тек швидший",
        (Lang::Uk, R::CanonicalMismatch) => "з'єднання, символьне посилання або subst-диск",
        (Lang::Uk, R::MftFailed) => "читання MFT не вдалося",
        (Lang::Uk, R::MftEmptyOnNonEmptyDisk) => "MFT нічого не повернув для непорожньої теки",
        (Lang::Uk, R::ForcedBySetting) => "обхід тек примусово увімкнено в налаштуваннях",
    }
}

pub fn walkdir_breakdown(lang: Lang, walked: usize, total: usize, parts: &str) -> String {
    match lang {
        Lang::En => format!(
            "Last scan: {walked} of {total} roots walked instead of read from the MFT - {parts}."
        ),
        Lang::Uk => format!(
            "Останнє сканування: {walked} з {total} коренів обійдено теками замість MFT — {parts}."
        ),
    }
}

/// The risk word on its own, for a table that already has a "Risk" column
/// heading. [`plan_risk_label`] prefixes it, which reads as a stutter once
/// the column says so too.
pub fn risk_level_bare_label(lang: Lang, risk: RiskLevel) -> &'static str {
    match (lang, risk) {
        (Lang::En, RiskLevel::None) => "none",
        (Lang::En, RiskLevel::Low) => "low",
        (Lang::En, RiskLevel::Medium) => "medium",
        (Lang::Uk, RiskLevel::None) => "нульовий",
        (Lang::Uk, RiskLevel::Low) => "низький",
        (Lang::Uk, RiskLevel::Medium) => "середній",
    }
}

/// Localized risk badge for a plan card (plan-action filtering): "Risk: none/low/medium".
pub fn plan_risk_label(lang: Lang, risk: RiskLevel) -> &'static str {
    match (lang, risk) {
        (Lang::En, RiskLevel::None) => "Risk: none",
        (Lang::En, RiskLevel::Low) => "Risk: low",
        (Lang::En, RiskLevel::Medium) => "Risk: medium",
        (Lang::Uk, RiskLevel::None) => "Ризик: нульовий",
        (Lang::Uk, RiskLevel::Low) => "Ризик: низький",
        (Lang::Uk, RiskLevel::Medium) => "Ризик: середній",
    }
}

/// The summary that opens the row above the tree (plan summary): "Found N item(s)
/// in M game(s)". `game_count` counts distinct games across every category,
/// so it is never the sum of the per-category figures - see
/// [`crate::model::plan_totals`].
pub fn plan_totals_summary(lang: Lang, finding_count: usize, game_count: usize) -> String {
    match lang {
        Lang::En => format!("Found {finding_count} item(s) in {game_count} game(s)"),
        Lang::Uk => format!("Знайдено {finding_count} об’єктів у {game_count} іграх"),
    }
}

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

pub fn scan_incomplete(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!(
            "The scan was not activated because it was incomplete: {err}. The previous snapshot was preserved."
        ),
        Lang::Uk => format!(
            "Сканування не активовано, бо воно було неповним: {err}. Попередній знімок збережено."
        ),
    }
}

/// The classification reason (persisted into `findings.rule_id`, shown in the
/// row tooltip) for an orphaned-residue finding - see `worker::scan`'s orphan
/// pass and `gametrimmer_core::orphans::OrphanKind`.
pub fn orphan_reason(lang: Lang, kind: gametrimmer_core::orphans::OrphanKind) -> String {
    use gametrimmer_core::orphans::OrphanKind;
    match (lang, kind) {
        (Lang::En, OrphanKind::UnmanagedFolder) => {
            "Folder in the library with no matching launcher manifest (orphaned install)"
                .to_string()
        }
        (Lang::En, OrphanKind::ServiceFolder) => {
            "Launcher download/cache scratch folder (aborted or partial downloads)".to_string()
        }
        (Lang::Uk, OrphanKind::UnmanagedFolder) => {
            "Тека в бібліотеці без відповідного маніфесту лаунчера (осиротіла інсталяція)"
                .to_string()
        }
        (Lang::Uk, OrphanKind::ServiceFolder) => {
            "Службова тека завантажень лаунчера (незавершені або часткові завантаження)".to_string()
        }
    }
}

/// Non-fatal warning: the game scan succeeded but persisting the orphaned-
/// residue findings (orphan-residue safety) failed. The rest of the results are intact; only
/// the orphan branch is missing until the next scan.
pub fn orphans_persist_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Failed to record orphaned residue: {err}"),
        Lang::Uk => format!("Не вдалося зберегти осиротілі рештки: {err}"),
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

pub fn deletion_block_reason(lang: Lang, reason: &str) -> String {
    if lang == Lang::En {
        return reason.to_string();
    }
    if reason.starts_with("invalid relative path:") {
        return "Недійсний відносний шлях у знімку сканування".to_string();
    }
    if reason.starts_with("reparse point is not deletable:") {
        return "Шлях містить symlink, junction, mount point або іншу точку повторної обробки"
            .to_string();
    }
    if reason.starts_with("filesystem state could not be verified:") {
        return "Не вдалося перевірити поточний стан файлової системи".to_string();
    }
    match reason {
        "target is outside its trusted root" => "Ціль перебуває поза довіреним коренем".to_string(),
        "target no longer exists" => "Ціль більше не існує".to_string(),
        "trusted root identity changed since the scan" => {
            "Ідентичність довіреного кореня змінилася після сканування".to_string()
        }
        "target identity changed since the scan" => {
            "Ідентичність цілі змінилася після сканування".to_string()
        }
        "directory contents changed since the scan" => {
            "Вміст каталогу змінився після сканування".to_string()
        }
        "a fresh safety scan is required" | "legacy snapshot is read-only" => {
            "Потрібне нове перевірене сканування; цей знімок доступний лише для перегляду"
                .to_string()
        }
        "launcher discovery was incomplete" | "library discovery was degraded" => {
            "Дані лаунчера для цієї бібліотеки неповні".to_string()
        }
        "scan-time filesystem identity is missing"
        | "missing filesystem safety evidence"
        | "missing filesystem identity" => {
            "Відсутні scan-time дані ідентичності файлової системи".to_string()
        }
        "missing library discovery evidence" => {
            "Відсутні підтверджені дані discovery для бібліотеки".to_string()
        }
        "orphan inventory is not authoritative" => {
            "Inventory лаунчера не є авторитетним для визначення orphan-залишків".to_string()
        }
        "the selected database row is no longer active"
        | "the selected row is no longer active"
        | "the selected row no longer exists" => {
            "Вибраний рядок більше не належить активному знімку".to_string()
        }
        "the delete batch contains a duplicate row" => {
            "Пакет видалення містить дубльований рядок".to_string()
        }
        _ => format!("Перевірка безпеки заблокувала видалення: {reason}"),
    }
}

pub fn deletion_blocked(lang: Lang, reason: &str) -> String {
    let reason = deletion_block_reason(lang, reason);
    match lang {
        Lang::En => format!("Deletion blocked: {reason}"),
        Lang::Uk => format!("Видалення заблоковано: {reason}"),
    }
}

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

pub fn pending_delete_reconciled(lang: Lang, count: usize) -> String {
    match lang {
        Lang::En => {
            format!("Reconciled {count} interrupted deletion intent(s); no deletion was retried.")
        }
        Lang::Uk => format!(
            "Узгоджено {count} перерваних намірів видалення; повторне видалення не запускалося."
        ),
    }
}

// -- worker::compact --

pub fn compact_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Failed to compact the database: {err}"),
        Lang::Uk => format!("Не вдалося стиснути базу даних: {err}"),
    }
}

// -- worker::clear --

pub fn clear_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!("Failed to clear the database: {err}"),
        Lang::Uk => format!("Не вдалося очистити базу даних: {err}"),
    }
}

/// Shown when "Clear database" hit a genuine corruption error (see
/// `gametrimmer_core::db::is_corruption_error`) and recovered by rebuilding
/// the database file from scratch (see
/// `gametrimmer_core::db::rebuild_database`). The user's registered
/// libraries and settings survived, but the scan data the clear was going to
/// discard anyway is gone along with the unusable file.
pub fn clear_rebuilt_after_corruption(lang: Lang) -> String {
    match lang {
        Lang::En => "The database was corrupted and has been rebuilt (registered libraries and \
             settings were kept)."
            .to_string(),
        Lang::Uk => "Базу було пошкоджено, її перебудовано (зареєстровані бібліотеки й \
             налаштування збережено)."
            .to_string(),
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

#[cfg(test)]
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

pub fn rules_restored(lang: Lang, path: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!(
            "Built-in rules restored ({path}); the previous file was kept as *.bak. \
             Changes take effect from the next scan."
        ),
        Lang::Uk => format!(
            "Вбудовані правила відновлено ({path}); попередній файл збережено як *.bak. \
             Зміни діятимуть з наступного сканування."
        ),
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
        Lang::En => format!(
            "Moved to the Recycle Bin: {succeeded} \
             (disk space frees up after you empty it)"
        ),
        Lang::Uk => format!(
            "Переміщено в Кошик: {succeeded} \
             (місце на диску звільниться після його очищення)"
        ),
    }
}

/// Shown after a Recycle Bin delete when Windows permanently deleted some
/// items because they exceeded the volume's Recycle Bin quota - see
/// `worker::RemoveOutcome::nuked`. These are gone for good, so the wording
/// must never imply they are recoverable.
pub fn success_line_nuked(lang: Lang, nuked: usize) -> String {
    match lang {
        Lang::En => format!(
            "Permanently deleted (too large for the Recycle Bin, cannot be \
             recovered): {nuked}"
        ),
        Lang::Uk => {
            format!("Видалено безповоротно (завеликі для Кошика, відновити не можна): {nuked}")
        }
    }
}

/// Post-delete "freed X of the expected Y" line (allocated-size accounting): closes the loop on
/// the confirm dialog's "will free {size}" promise with the on-disk space that
/// was actually reclaimed. When nothing failed (`freed == expected`), the
/// caller passes `show_expected = false` for the shorter "Freed X". `freed`
/// and `expected` are pre-formatted size strings.
pub fn freed_summary_line(lang: Lang, freed: &str, expected: &str, show_expected: bool) -> String {
    match (lang, show_expected) {
        (Lang::En, true) => format!("Freed {freed} of the expected {expected}"),
        (Lang::En, false) => format!("Freed {freed}"),
        (Lang::Uk, true) => format!("Звільнено {freed} з очікуваних {expected}"),
        (Lang::Uk, false) => format!("Звільнено {freed}"),
    }
}

/// Recycle summary line (allocated-size accounting): on-disk bytes that will free only once the
/// Recycle Bin is emptied - they still sit on the same volume until then.
/// `size` is a pre-formatted size string.
pub fn recycle_pending_size_line(lang: Lang, size: &str) -> String {
    match lang {
        Lang::En => format!("Will free after emptying the Recycle Bin: {size}"),
        Lang::Uk => format!("Звільниться після очищення Кошика: {size}"),
    }
}

/// Recycle summary line (allocated-size accounting): on-disk bytes freed immediately because
/// Windows permanently deleted over-quota items (the `nuked` ones). `size` is a
/// pre-formatted size string.
pub fn freed_now_size_line(lang: Lang, size: &str) -> String {
    match lang {
        Lang::En => format!("Freed immediately: {size}"),
        Lang::Uk => format!("Звільнено відразу: {size}"),
    }
}

/// Non-fatal warning: the Recycle Bin could not be enumerated after a recycle
/// delete, so the app could not tell whether any item was permanently deleted
/// (see `worker::delete::nuked_flags`). The delete itself already
/// happened; this only means the summary may over-report recoverability.
pub fn recycle_bin_list_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En => format!(
            "Could not read the Recycle Bin to confirm which files are \
             recoverable: {err}"
        ),
        Lang::Uk => format!(
            "Не вдалося прочитати Кошик, щоб підтвердити, які файли можна \
             відновити: {err}"
        ),
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

/// The \u{2139} tooltip beside the selection summary.
///
/// It used to open by defining the auto-selection confidence threshold in
/// percent, which is how the tree used to report it too. The number is gone
/// from both: what the user can act on is that a file the app is unsure about
/// carries a \u{26a0} and was left unticked on purpose.
pub fn selection_hint(lang: Lang) -> String {
    match lang {
        Lang::En => "Files the app is not sure enough about are marked \u{26a0} and are never \
             ticked for you after a scan - look at them before deleting.\n\n\
             Checkboxes on tree rows select a whole disk, game, category, or folder. \
             Right-clicking a disk, game, or category opens bulk selection actions \
             (including a category across the whole disk).\n\n\
             Keyboard: \u{2191}\u{2193} - cursor, PgUp/PgDn - page, \
             \u{2192}/\u{2190} - expand/collapse, Space - select."
            .to_string(),
        Lang::Uk => "Файли, щодо яких застосунок недостатньо впевнений, позначені \u{26a0} і \
             ніколи не обираються за вас після сканування — гляньте на них перед \
             видаленням.\n\n\
             Прапорці на рядках дерева вибирають цілий диск, гру, категорію чи теку. \
             Права кнопка миші на диску, грі або категорії відкриває дії масового вибору \
             (зокрема категорію на всьому диску).\n\n\
             Клавіатура: \u{2191}\u{2193} — курсор, PgUp/PgDn — сторінка, \
             \u{2192}/\u{2190} — розгорнути/згорнути, Space — вибрати."
            .to_string(),
    }
}

pub fn selected_summary(lang: Lang, count: usize, size: &str) -> String {
    match lang {
        Lang::En => format!("Selected {count} file(s), will free up {size}"),
        Lang::Uk => format!("Вибрано {count} файл(ів), буде звільнено {size}"),
    }
}

/// Live disk-usage line: total bytes occupied by every scanned game, plus
/// the percentage of that total the current selection would free (see
/// `crate::model::freed_percent`). `total` is already formatted via
/// `crate::model::format_size`.
pub fn occupancy_summary(lang: Lang, total: &str, pct: f64) -> String {
    match lang {
        Lang::En => format!("Games occupy {total} \u{b7} selection frees {pct:.1}%"),
        Lang::Uk => format!("Ігри займають {total} \u{b7} вибране звільнить {pct:.1}%"),
    }
}

/// Persistent scan-timing summary shown in the bottom bar after a scan
/// completes (see `crate::model::ScanTiming`, `crate::app::GameTrimmerApp::last_scan_timing`).
/// `scan`/`analyze`/`total` are already formatted via `crate::model::format_duration`.
pub fn scan_timing_summary(lang: Lang, scan: &str, analyze: &str, total: &str) -> String {
    match lang {
        Lang::En => format!("Scan {scan} \u{b7} Analysis {analyze} \u{b7} Total {total}"),
        Lang::Uk => format!("Сканування {scan} \u{b7} Аналіз {analyze} \u{b7} Разом {total}"),
    }
}

// -- ui::top_bar --

// -- ui::tree_view --

pub fn disk_label(lang: Lang, disk: &str) -> String {
    match lang {
        Lang::En => format!("Disk {disk}"),
        Lang::Uk => format!("Диск {disk}"),
    }
}

/// Display name for a `game_libraries.vendor` tag.
///
/// Unknown tags pass through verbatim rather than being prettified or replaced
/// with a placeholder: a vendor this table has not heard of is still a real
/// launcher whose games are on screen, and its raw tag is the only true thing
/// there is to say about it.
pub fn launcher_label(lang: Lang, vendor: &str) -> String {
    let known = match vendor {
        "steam" => "Steam",
        "epic" => "Epic Games",
        "gog" => "GOG",
        "ea" => "EA",
        "ubisoft" => "Ubisoft",
        "battlenet" => "Battle.net",
        "rockstar" => "Rockstar Games",
        "riot" => "Riot Games",
        "amazon" => "Amazon Games",
        "humble" => "Humble",
        "itch" => "itch.io",
        "xbox" => "Xbox",
        // The one vendor with no launcher behind it: a folder the user pointed
        // the scanner at by hand (`worker::manual::MANUAL_VENDOR`), so it is
        // the only tag that needs translating rather than branding.
        "manual" => {
            return match lang {
                Lang::En => "Added by hand".to_string(),
                Lang::Uk => "Додані вручну".to_string(),
            }
        }
        other => return other.to_string(),
    };
    known.to_string()
}

/// Heading of one top-level branch of the tree, in the terms of the axis it
/// was cut along (see `model::TopKey`).
pub fn top_group_label(lang: Lang, key: &crate::model::TopKey) -> String {
    use crate::model::TopKey;
    match key {
        TopKey::Disk(disk) => disk_label(lang, disk),
        TopKey::Launcher(vendor) => launcher_label(lang, vendor),
        TopKey::Library(root) => root.to_string_lossy().into_owned(),
        TopKey::Category(category) => crate::model::category_display(lang, *category).to_string(),
        // Never drawn - the flat axis folds this level away - but named rather
        // than left blank, so a stray render says what it is instead of
        // nothing.
        TopKey::Flat => strings(lang).group_axis_flat.to_string(),
        TopKey::Unattributed(_) => strings(lang).group_unattributed.to_string(),
    }
}

/// The opening and closing quotation marks `lang` puts around a name.
///
/// Split out of [`quoted`] because the findings tree needs the two halves
/// separately: a search match is tinted inside the name, and the marks around
/// it are punctuation the row added, which the search never saw (see
/// `ui::highlight::Part`).
pub fn quote_marks(lang: Lang) -> (&'static str, &'static str) {
    match lang {
        Lang::En => ("\u{201c}", "\u{201d}"),
        Lang::Uk => ("\u{ab}", "\u{bb}"),
    }
}

pub fn quoted(lang: Lang, name: &str) -> String {
    let (open, close) = quote_marks(lang);
    format!("{open}{name}{close}")
}

/// "Select everything in this whole top-level branch", phrased for the axis
/// the branch was cut along.
///
/// One sentence per axis rather than one generic sentence with the heading
/// pasted in: "on disk E:", "in Steam" and "in library E:\SteamLibrary" each
/// need their own preposition and case, and Ukrainian will not survive a
/// template that ignores that.
pub fn select_all_in_group(lang: Lang, key: &crate::model::TopKey) -> String {
    use crate::model::TopKey;
    match (lang, key) {
        (Lang::En, TopKey::Disk(disk)) => format!("Select all on disk {disk}"),
        (Lang::Uk, TopKey::Disk(disk)) => format!("Вибрати все на диску {disk}"),
        (Lang::En, TopKey::Launcher(vendor)) => {
            format!("Select all in {}", launcher_label(lang, vendor))
        }
        (Lang::Uk, TopKey::Launcher(vendor)) => {
            format!("Вибрати все в {}", launcher_label(lang, vendor))
        }
        (Lang::En, TopKey::Library(root)) => {
            format!("Select all in library {}", root.to_string_lossy())
        }
        (Lang::Uk, TopKey::Library(root)) => {
            format!("Вибрати все в бібліотеці {}", root.to_string_lossy())
        }
        (Lang::En, TopKey::Category(category)) => format!(
            "Select every {}",
            quoted(lang, crate::model::category_display(lang, *category))
        ),
        (Lang::Uk, TopKey::Category(category)) => format!(
            "Вибрати всі {}",
            quoted(lang, crate::model::category_display(lang, *category))
        ),
        (Lang::En, TopKey::Flat) => "Select everything".to_string(),
        (Lang::Uk, TopKey::Flat) => "Вибрати все".to_string(),
        (Lang::En, TopKey::Unattributed(_)) => "Select all unattributed".to_string(),
        (Lang::Uk, TopKey::Unattributed(_)) => "Вибрати все без прив'язки".to_string(),
    }
}

/// The other half of [`select_all_in_group`].
pub fn deselect_all_in_group(lang: Lang, key: &crate::model::TopKey) -> String {
    use crate::model::TopKey;
    match (lang, key) {
        (Lang::En, TopKey::Disk(disk)) => format!("Deselect all on disk {disk}"),
        (Lang::Uk, TopKey::Disk(disk)) => format!("Зняти вибір на диску {disk}"),
        (Lang::En, TopKey::Launcher(vendor)) => {
            format!("Deselect all in {}", launcher_label(lang, vendor))
        }
        (Lang::Uk, TopKey::Launcher(vendor)) => {
            format!("Зняти вибір у {}", launcher_label(lang, vendor))
        }
        (Lang::En, TopKey::Library(root)) => {
            format!("Deselect all in library {}", root.to_string_lossy())
        }
        (Lang::Uk, TopKey::Library(root)) => {
            format!("Зняти вибір у бібліотеці {}", root.to_string_lossy())
        }
        (Lang::En, TopKey::Category(category)) => format!(
            "Deselect every {}",
            quoted(lang, crate::model::category_display(lang, *category))
        ),
        (Lang::Uk, TopKey::Category(category)) => format!(
            "Зняти вибір з усіх {}",
            quoted(lang, crate::model::category_display(lang, *category))
        ),
        (Lang::En, TopKey::Flat) => "Deselect everything".to_string(),
        (Lang::Uk, TopKey::Flat) => "Зняти вибір з усього".to_string(),
        (Lang::En, TopKey::Unattributed(_)) => "Deselect all unattributed".to_string(),
        (Lang::Uk, TopKey::Unattributed(_)) => "Зняти вибір без прив'язки".to_string(),
    }
}

/// "the whole branch", as the phrase a category-wide bulk action ends with.
///
/// Each axis gets its own preposition and case rather than one template with
/// the heading dropped in: "on the whole disk E:", "across all of Steam" and
/// "in the whole library E:\SteamLibrary" are three different sentences in
/// English and three different ones again in Ukrainian.
fn whole_group_scope(lang: Lang, key: &crate::model::TopKey) -> String {
    use crate::model::TopKey;
    match (lang, key) {
        (Lang::En, TopKey::Disk(disk)) => format!("on the whole disk {disk}"),
        (Lang::Uk, TopKey::Disk(disk)) => format!("на всьому диску {disk}"),
        (Lang::En, TopKey::Launcher(vendor)) => {
            format!("across all of {}", launcher_label(lang, vendor))
        }
        (Lang::Uk, TopKey::Launcher(vendor)) => {
            format!("в усьому {}", launcher_label(lang, vendor))
        }
        (Lang::En, TopKey::Library(root)) => {
            format!("in the whole library {}", root.to_string_lossy())
        }
        (Lang::Uk, TopKey::Library(root)) => {
            format!("в усій бібліотеці {}", root.to_string_lossy())
        }
        // The category and flat axes fold the category row away, so these two
        // never reach a call site - answered anyway rather than left to a
        // catch-all arm that a sixth axis could quietly fall into.
        (Lang::En, TopKey::Category(_) | TopKey::Flat) => "everywhere".to_string(),
        (Lang::Uk, TopKey::Category(_) | TopKey::Flat) => "усюди".to_string(),
        (Lang::En, TopKey::Unattributed(_)) => "across everything unattributed".to_string(),
        (Lang::Uk, TopKey::Unattributed(_)) => "серед усього без прив'язки".to_string(),
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

/// "Select this category across the whole branch", for whatever the branch is
/// under the active axis.
pub fn select_category_in_group(lang: Lang, label: &str, key: &crate::model::TopKey) -> String {
    let scope = whole_group_scope(lang, key);
    match lang {
        Lang::En => format!("Select {} {scope}", quoted(lang, label)),
        Lang::Uk => format!("Вибрати {} {scope}", quoted(lang, label)),
    }
}

/// The other half of [`select_category_in_group`].
pub fn deselect_category_in_group(lang: Lang, label: &str, key: &crate::model::TopKey) -> String {
    let scope = whole_group_scope(lang, key);
    match lang {
        Lang::En => format!("Deselect {} {scope}", quoted(lang, label)),
        Lang::Uk => format!("Зняти вибір {} {scope}", quoted(lang, label)),
    }
}

/// The switcher entry, and the tooltip text, for one grouping axis.
pub fn group_axis_label(lang: Lang, axis: crate::model::GroupAxis) -> &'static str {
    use crate::model::GroupAxis;
    let s = strings(lang);
    match axis {
        GroupAxis::Disk => s.group_axis_disk,
        GroupAxis::Launcher => s.group_axis_launcher,
        GroupAxis::Library => s.group_axis_library,
        GroupAxis::Category => s.group_axis_category,
        GroupAxis::Flat => s.group_axis_flat,
    }
}

/// A game row's bulk action under [`GroupAxis::Category`], where the branch is
/// the category and the row therefore covers only that slice of the game.
///
/// The plain "Select all in {game}" would be a lie there: it reads as the
/// game's whole contribution to the tree, and it is one category of it.
pub fn select_category_in_game(lang: Lang, category: &str, game: &str) -> String {
    match lang {
        Lang::En => format!(
            "Select {} in {}",
            quoted(lang, category),
            quoted(lang, game)
        ),
        Lang::Uk => format!(
            "Вибрати {} у {}",
            quoted(lang, category),
            quoted(lang, game)
        ),
    }
}

/// The other half of [`select_category_in_game`].
pub fn deselect_category_in_game(lang: Lang, category: &str, game: &str) -> String {
    match lang {
        Lang::En => format!(
            "Deselect {} in {}",
            quoted(lang, category),
            quoted(lang, game)
        ),
        Lang::Uk => format!(
            "Зняти вибір {} у {}",
            quoted(lang, category),
            quoted(lang, game)
        ),
    }
}

/// What joins the game to its relative path on a file row under
/// [`GroupAxis::Flat`], where there are no headings above the row to say which
/// game it came from.
///
/// The row shows the relative path and not the absolute one: the Name column
/// truncates on the right, and an absolute path truncated on the right hides
/// the filename - the one part of it the row exists to show.
///
/// A constant rather than a `flat_row_name(lang, game, rel_path)` that returns
/// the finished string, because the row is assembled from pieces: the game and
/// the path are fields the search can match and this dash is not (see
/// `ui::highlight::Part`).
pub const FLAT_ROW_SEPARATOR: &str = " \u{2014} ";

/// Tooltip for a file row: the full path on the first line (details on
/// demand - the inline row only shows the leaf name), then the classification
/// reason. `path` is the file's absolute path, not the relative one.
pub fn hover_reason(lang: Lang, path: &str, rule_desc: &str, confidence: u8) -> String {
    match lang {
        Lang::En => format!("{path}\nReason: {rule_desc} (confidence {confidence}%)"),
        Lang::Uk => format!("{path}\nПричина: {rule_desc} (упевненість {confidence}%)"),
    }
}

/// Renders the localization detector's evidence into `lang`.
///
/// The engine reports why it flagged a file as data rather than prose
/// (`gametrimmer_core::langdetect::LangReason`) precisely so this function can
/// exist: the sentence is written where the interface language is known, not
/// where the detection happens.
///
/// English is the engine's own `Display`, so the two can never drift apart -
/// adding a variant there is a compile error here, not a silently untranslated
/// string.
pub fn lang_reason(lang: Lang, reason: &LangReason) -> String {
    if lang == Lang::En {
        return reason.to_string();
    }

    // "у теці 'Voices'" or "у корені гри" - the engine reports the directory
    // as data and leaves this wording to whoever writes the sentence.
    let location = |dir: &Option<String>| match dir {
        Some(dir) => format!("у теці '{dir}'"),
        None => "у корені гри".to_string(),
    };

    let mut text = match &reason.evidence {
        LangEvidence::LocPair { token } => format!("токен '{token}' у явній loc-парі"),
        LangEvidence::TokenWithMarker { token, marker } => {
            format!("токен '{token}' + маркер '{marker}'")
        }
        LangEvidence::BareToken { token } => {
            format!("токен '{token}' (мовна тека без явного контексту)")
        }
        LangEvidence::Family { languages, dir } => {
            format!("мовна сім'я з {languages} мов {}", location(dir))
        }
        LangEvidence::FamilyAtSharedPosition { languages, dir } => format!(
            "мовна сім'я з {languages} мов {} (спільна позиція токена)",
            location(dir)
        ),
        LangEvidence::SubfolderFamily { languages, dir } => {
            format!("мовна сім'я підтек з {languages} мов {}", location(dir))
        }
        LangEvidence::SubfolderFamilyWithPrefix { languages, dir } => format!(
            "мовна сім'я підтек зі спільним префіксом ({languages} мов) {}",
            location(dir)
        ),
    };
    if let Some(marker) = &reason.marker {
        text.push_str(&format!("; маркер '{marker}'"));
    }
    text
}

pub fn hover_lang_suffix(lang: Lang, lang_tag: &str) -> String {
    match lang {
        Lang::En => format!("\nLanguage: {lang_tag}"),
        Lang::Uk => format!("\nМова: {lang_tag}"),
    }
}

/// Tooltip line spelling out the logical size (allocated-size accounting): the row and totals show
/// the on-disk allocated size as primary - the honest "space freed" figure -
/// so this adds the logical size for context. Only shown when the two differ
/// (`logical` is a pre-formatted size string).
pub fn hover_logical_size_suffix(lang: Lang, logical: &str) -> String {
    match lang {
        Lang::En => format!("\nLogical size: {logical}"),
        Lang::Uk => format!("\nЛогічний розмір: {logical}"),
    }
}

// -- ui::settings --

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
