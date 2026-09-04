//! Messages that interpolate a count, a path, or an error into otherwise
//! static text. Kept as plain functions (rather than more `Strings` fields
//! with `{0}`-style placeholders) so the interpolation is ordinary,
//! type-checked `format!` - no runtime template parsing, no way for a
//! placeholder count to silently drift from its call site.

use gametrimmer_core::langdetect::{LangEvidence, LangReason};

use super::{strings, Lang};
use crate::model::RiskLevel;

/// One report rendered twice: English for the diagnostic log, the interface
/// language for the window.
///
/// The two audiences are genuinely different. Whoever reads a log is
/// diagnosing a problem, often on someone else's machine, and needs text
/// they can search for and compare against the source; the person at the
/// window needs their own language. Before this, the worker rendered once -
/// in the interface language - and the log inherited whatever that was, so
/// a Ukrainian install produced a log that could not be grepped for
/// "Failed to write libraries" and a mixed log whenever a call site had
/// hardcoded `Lang::En` instead.
///
/// Rendering happens here, at the point of production, because that is the
/// only place both the message and the arguments are still in hand: further
/// up all that survives is a finished string, and a finished string cannot
/// be translated back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reported {
    /// English, for the log.
    pub log: String,
    /// The interface language, for the window.
    pub shown: String,
}

impl Reported {
    /// Renders `message` once per audience.
    ///
    /// `message` is called twice, so it must borrow rather than consume its
    /// arguments - every `i18n` function here takes `impl Display`, which
    /// `&T` satisfies.
    pub fn new(lang: Lang, message: impl Fn(Lang) -> String) -> Self {
        Self {
            log: message(Lang::En),
            shown: message(lang),
        }
    }
}

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
        (Lang::En | Lang::Custom(_), R::NotElevated) => "not running as administrator",
        (Lang::En | Lang::Custom(_), R::NoVolumeLetter) => "not on a lettered local drive",
        (Lang::En | Lang::Custom(_), R::VolumeUnavailable) => "volume could not be opened",
        (Lang::En | Lang::Custom(_), R::SsdVolume) => "SSD, where walking is faster",
        (Lang::En | Lang::Custom(_), R::CanonicalMismatch) => "junction, symlink or subst drive",
        (Lang::En | Lang::Custom(_), R::MftFailed) => "the MFT read failed",
        (Lang::En | Lang::Custom(_), R::MftEmptyOnNonEmptyDisk) => {
            "the MFT returned nothing for a non-empty folder"
        }
    }
}

pub fn walkdir_breakdown(lang: Lang, walked: usize, total: usize, parts: &str) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!(
            "Last scan: {walked} of {total} roots walked instead of read from the MFT - {parts}."
        ),
    }
}

/// Body of the native message box `single_instance::notify_already_running`
/// shows when a second launch from the same portable directory finds the
/// first one still running (GT-75). Two variants rather than one generic
/// line: `raised` tells the reader whether this dialog already brought the
/// running window forward for them (so "switch to it" would be redundant
/// and slightly confusing - it is already in front) or could not find it
/// (so they need to be told to look for it themselves, e.g. on another
/// virtual desktop or minimized to the taskbar).
pub fn already_running_body(lang: Lang, raised: bool) -> String {
    match (lang, raised) {
        (Lang::En | Lang::Custom(_), true) => "GameTrimmer is already running from this folder. \
            Its window has been brought to the front."
            .to_string(),
        (Lang::En | Lang::Custom(_), false) => "GameTrimmer is already running from this folder. \
            Switch to its window - it may be minimized or on another desktop."
            .to_string(),
    }
}

/// The `desc` written into a personal exception rule.
///
/// Written for someone reading the pack file by hand months later, because
/// that is currently the only way to undo one: it has to say which game and
/// which file, not just "kept". Both languages are stored at once (see the
/// call site in `ui::tree_view`) - the pack outlives the interface language it
/// was created under.
pub fn exception_desc(
    lang: Lang,
    game_name: impl std::fmt::Display,
    rel_path: impl std::fmt::Display,
) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Kept by hand: {rel_path} in {game_name}"),
    }
}

/// The risk word on its own, for a table that already has a "Risk" column
/// heading. [`plan_risk_label`] prefixes it, which reads as a stutter once
/// the column says so too.
pub fn risk_level_bare_label(lang: Lang, risk: RiskLevel) -> &'static str {
    match (lang, risk) {
        (Lang::En | Lang::Custom(_), RiskLevel::None) => "none",
        (Lang::En | Lang::Custom(_), RiskLevel::Low) => "low",
        (Lang::En | Lang::Custom(_), RiskLevel::Medium) => "medium",
    }
}

/// Localized risk badge for a plan card (plan-action filtering): "Risk: none/low/medium".
pub fn plan_risk_label(lang: Lang, risk: RiskLevel) -> &'static str {
    match (lang, risk) {
        (Lang::En | Lang::Custom(_), RiskLevel::None) => "Risk: none",
        (Lang::En | Lang::Custom(_), RiskLevel::Low) => "Risk: low",
        (Lang::En | Lang::Custom(_), RiskLevel::Medium) => "Risk: medium",
    }
}

/// The summary that opens the row above the tree (plan summary): "Found N item(s)
/// in M game(s)". `game_count` counts distinct games across every category,
/// so it is never the sum of the per-category figures - see
/// [`crate::model::plan_totals`].
pub fn plan_totals_summary(lang: Lang, finding_count: usize, game_count: usize) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => {
            format!("Found {finding_count} item(s) in {game_count} game(s)")
        }
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
        Lang::En | Lang::Custom(_) => format!(
            "Scanned {total} game(s) (MFT: {mft}, walkdir: {walkdir}) in {elapsed_secs:.1} sec."
        ),
    }
}

pub fn db_open_error_long(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!(
            "Failed to open the database: {err}. Move the program to a folder with write \
             access (not Program Files without administrator rights)."
        ),
    }
}

pub fn db_open_error_short(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Failed to open the database: {err}"),
    }
}

pub fn add_library_failed(
    lang: Lang,
    path: impl std::fmt::Display,
    err: impl std::fmt::Display,
) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Failed to add folder {path}: {err}"),
    }
}

pub fn remove_library_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Failed to remove library: {err}"),
    }
}

pub fn libraries_found(lang: Lang, libraries: usize, games: usize) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => {
            format!("Found {libraries} librar(y/ies), {games} game(s). Scanning files...")
        }
    }
}

pub fn scan_done_status(lang: Lang, scan_summary: &str, count: usize) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("{scan_summary} Found {count} file(s) to review."),
    }
}

pub fn remove_done_status(lang: Lang, succeeded: usize, failed: usize) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => {
            format!("Deletion completed: {succeeded} succeeded, {failed} failed.")
        }
    }
}

pub fn error_prefixed(lang: Lang, msg: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Error: {msg}"),
    }
}

pub fn export_save_failed(lang: Lang, error: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Failed to save the export: {error}"),
    }
}

pub fn exported_to(lang: Lang, path: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Exported to: {path}"),
    }
}

pub fn settings_save_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Failed to save settings: {err}"),
    }
}

// -- worker::scan --

pub fn rules_json_load_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => {
            format!("Failed to load rules.json: {err} - using the built-in category rules.")
        }
    }
}

pub fn builtin_rules_corrupted(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Built-in rules are corrupted: {err}"),
    }
}

/// An installed per-game catalogue that could not be read or parsed.
///
/// Says what was lost, not just what failed: without this line, "the games I
/// installed a catalogue for are not being found" looks exactly like the app
/// having ignored the file, and the user has no way to tell which.
pub fn installed_reference_load_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!(
            "Failed to load game_reference.json: {err} - scanning without the installed \
             catalogue, so games only it knows about will not have their startup videos found."
        ),
    }
}

/// A personal exception pack that could not be read or compiled.
///
/// Says what was lost, not just what failed: the visible symptom is files the
/// user kept being proposed again, and without this line that is
/// indistinguishable from the app having ignored them all along.
pub fn personal_rules_load_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!(
            "Failed to load personal_rules.json: {err} - scanning without your personal \
             exceptions, so files you kept may be proposed again."
        ),
    }
}

/// The clause appended to the scan summary when exceptions actually vetoed
/// something - the answer to "why is that finding gone?".
pub fn kept_by_exceptions(lang: Lang, kept: usize) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => {
            format!("Your exceptions kept {kept} file(s) out of the results.")
        }
    }
}

/// Confirmation that a "never touch this" click landed, naming the file so
/// the sentence is checkable against what was right-clicked.
pub fn exception_kept(lang: Lang, rel_path: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => {
            format!(
                "Never touching {rel_path} in this game again - it is off the list from now on."
            )
        }
    }
}

/// The same click on a file that already has an exception. Says so plainly
/// rather than reporting a second success for a file nothing happened to.
pub fn exception_already_kept(lang: Lang, rel_path: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("{rel_path} was already on your never-touch list."),
    }
}

pub fn l10n_rules_load_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => {
            format!("Failed to load l10n_rules.json: {err} - using the built-in language rules.")
        }
    }
}

/// Attributes one line to the provider it came from. Named for what it does
/// rather than for a failure: most of these lines are failures, but a
/// `GAME_ABSENT` note is an ordinary state travelling the same route (see
/// `scan::discovery`), and a function called `provider_failed` producing an
/// INFO line would be the wrong label at the call site.
pub fn provider_message(
    lang: Lang,
    name: impl std::fmt::Display,
    detail: impl std::fmt::Display,
) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Provider \"{name}\": {detail}"),
    }
}

pub fn manual_libraries_read_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Failed to read manual libraries: {err}"),
    }
}

pub fn no_libraries_found(lang: Lang) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => "No libraries found.".to_string(),
    }
}

pub fn libraries_write_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Failed to write libraries to the database: {err}"),
    }
}

pub fn write_thread_crashed(lang: Lang) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => "The scan results writer thread crashed.".to_string(),
    }
}

pub fn scan_incomplete(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!(
            "The scan was not activated because it was incomplete: {err}. The previous snapshot was preserved."
        ),
    }
}

/// The classification reason (persisted into `findings.rule_id`, shown in the
/// row tooltip) for an orphaned-residue finding - see `worker::scan`'s orphan
/// pass and `gametrimmer_core::orphans::OrphanKind`.
pub fn orphan_reason(lang: Lang, kind: gametrimmer_core::orphans::OrphanKind) -> String {
    use gametrimmer_core::orphans::OrphanKind;
    match (lang, kind) {
        (Lang::En | Lang::Custom(_), OrphanKind::UnmanagedFolder) => {
            "Folder in the library with no matching launcher manifest (orphaned install)"
                .to_string()
        }
        (Lang::En | Lang::Custom(_), OrphanKind::ServiceFolder) => {
            "Launcher download/cache scratch folder (aborted or partial downloads)".to_string()
        }
        (Lang::En | Lang::Custom(_), OrphanKind::UnreferencedFile) => {
            "Cached file no installed app still references (e.g. an old Steam depot manifest)"
                .to_string()
        }
        (Lang::En | Lang::Custom(_), OrphanKind::WorkshopItem) => {
            "Workshop mod this game's own subscription state no longer lists as installed"
                .to_string()
        }
    }
}

/// Non-fatal warning: the game scan succeeded but persisting the orphaned-
/// residue findings (orphan-residue safety) failed. The rest of the results are intact; only
/// the orphan branch is missing until the next scan.
pub fn orphans_persist_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Failed to record orphaned residue: {err}"),
    }
}

pub fn reading_mft_detail(lang: Lang, volume: char, percent: u64) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Reading file table {volume}: — {percent}%"),
    }
}

// -- worker::manual --

pub fn manual_library_unavailable(lang: Lang, path: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => {
            format!("Manual library \"{path}\" is unavailable (disk disconnected or folder moved).")
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
    // The English side names the exact nested path that made the folder
    // undeletable (`DeleteBlockReason::ReparsePoint` carries it). This used to
    // replace the whole thing with a generic sentence, and since this string
    // is what the tree's tooltip shows, the Ukrainian user was told their
    // folder is permanently blocked without being told which link inside it is
    // responsible - i.e. with no way to act on it. Keep the path.
    if let Some(path) = reason.strip_prefix("reparse point is not deletable:") {
        return format!(
            "Шлях містить symlink, junction, mount point або іншу точку \
             повторної обробки:{path}"
        );
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
        Lang::En | Lang::Custom(_) => format!("Deletion blocked: {reason}"),
    }
}

pub fn delete_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Deletion failed: {err}"),
    }
}

pub fn db_update_after_delete_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => {
            format!("Failed to update the database after deletion: {err}")
        }
    }
}

pub fn pending_delete_reconciled(lang: Lang, count: usize) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => {
            format!("Reconciled {count} interrupted deletion intent(s); no deletion was retried.")
        }
    }
}

/// Zero-Data-Loss Shield: the pre-delete save backup ZIP could not be
/// created, so the whole batch was aborted rather than risking a save file
/// with no backup at all.
pub fn save_backup_zip_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!(
            "Zero-Data-Loss Shield: Failed to create save backup ZIP archive ({err}). \
             Deletion aborted to protect save files."
        ),
    }
}

/// An intro file was left in place rather than deleted, because its video
/// container has no micro-stub in this build - deleting it outright would
/// leave the game with no file at that path at all. See
/// `gametrimmer_core::stub::detect_stub_bytes`.
pub fn intro_stub_unsupported_skip(lang: Lang, path: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!(
            "Skipped deleting intro file {path} - its video container is not one this \
             build has a micro-stub for, and deleting it would leave the game with no \
             file there at all"
        ),
    }
}

/// The intro file was deleted, but writing its replacement micro-stub failed
/// afterward - the exact "no file left at all" outcome the stub exists to
/// prevent, so this needs to reach the user, not just the log.
pub fn intro_stub_write_failed(
    lang: Lang,
    path: impl std::fmt::Display,
    err: impl std::fmt::Display,
) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!(
            "Failed to write the micro-stub for intro file {path} after deleting it; \
             the game may fail to start until this is fixed: {err}"
        ),
    }
}

// -- worker::compact --

// -- worker::bundle --

pub fn bundle_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Failed to write the diagnostic bundle: {err}"),
    }
}

pub fn bundle_saved_to(lang: Lang, path: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Diagnostic bundle saved to {path}"),
    }
}

pub fn compact_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Failed to compact the database: {err}"),
    }
}

// -- worker::clear --

pub fn clear_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Failed to clear the database: {err}"),
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
        Lang::En | Lang::Custom(_) => {
            "The database was corrupted and has been rebuilt (registered libraries and \
             settings were kept)."
                .to_string()
        }
    }
}

// -- worker::load --

pub fn loaded_saved_results(lang: Lang) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => "Showing saved results from a previous scan.".to_string(),
    }
}

pub fn load_previous_results_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Failed to load previous scan results: {err}"),
    }
}

// -- worker::rules_io --

pub fn prepare_rules_file_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("failed to prepare the rules file: {err}"),
    }
}

pub fn read_file_failed(
    lang: Lang,
    path: impl std::fmt::Display,
    err: impl std::fmt::Display,
) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("failed to read {path}: {err}"),
    }
}

pub fn write_failed(
    lang: Lang,
    path: impl std::fmt::Display,
    err: impl std::fmt::Display,
) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("failed to write {path}: {err}"),
    }
}

pub fn confirm_permanent_question(lang: Lang, count: usize, size: &str) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!(
            "Permanently delete {count} file(s) ({size})? This cannot be undone \
             (the game can be reinstalled from the store)."
        ),
    }
}

pub fn confirm_recycle_question(lang: Lang, count: usize, size: &str) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Move {count} file(s) ({size}) to the Recycle Bin?"),
    }
}

pub fn success_line_permanent(lang: Lang, succeeded: usize) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Successfully deleted: {succeeded}"),
    }
}

pub fn success_line_recycle(lang: Lang, succeeded: usize) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!(
            "Moved to the Recycle Bin: {succeeded} \
             (disk space frees up after you empty it)"
        ),
    }
}

/// Shown after a Recycle Bin delete when Windows permanently deleted some
/// items because they exceeded the volume's Recycle Bin quota - see
/// `worker::RemoveOutcome::nuked`. These are gone for good, so the wording
/// must never imply they are recoverable.
pub fn success_line_nuked(lang: Lang, nuked: usize) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!(
            "Permanently deleted (too large for the Recycle Bin, cannot be \
             recovered): {nuked}"
        ),
    }
}

/// Post-delete "freed X of the expected Y" line (allocated-size accounting): closes the loop on
/// the confirm dialog's "will free {size}" promise with the on-disk space that
/// was actually reclaimed. When nothing failed (`freed == expected`), the
/// caller passes `show_expected = false` for the shorter "Freed X". `freed`
/// and `expected` are pre-formatted size strings.
pub fn freed_summary_line(lang: Lang, freed: &str, expected: &str, show_expected: bool) -> String {
    match (lang, show_expected) {
        (Lang::En | Lang::Custom(_), true) => format!("Freed {freed} of the expected {expected}"),
        (Lang::En | Lang::Custom(_), false) => format!("Freed {freed}"),
    }
}

/// Recycle summary line (allocated-size accounting): on-disk bytes that will free only once the
/// Recycle Bin is emptied - they still sit on the same volume until then.
/// `size` is a pre-formatted size string.
pub fn recycle_pending_size_line(lang: Lang, size: &str) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Will free after emptying the Recycle Bin: {size}"),
    }
}

/// Recycle summary line (allocated-size accounting): on-disk bytes freed immediately because
/// Windows permanently deleted over-quota items (the `nuked` ones). `size` is a
/// pre-formatted size string.
pub fn freed_now_size_line(lang: Lang, size: &str) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Freed immediately: {size}"),
    }
}

/// Non-fatal warning: the Recycle Bin could not be enumerated after a recycle
/// delete, so the app could not tell whether any item was permanently deleted
/// (see `worker::delete::nuked_flags`). The delete itself already
/// happened; this only means the summary may over-report recoverability.
pub fn recycle_bin_list_failed(lang: Lang, err: impl std::fmt::Display) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!(
            "Could not read the Recycle Bin to confirm which files are \
             recoverable: {err}"
        ),
    }
}

pub fn errors_count_line(lang: Lang, failed_count: usize) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Errors: {failed_count}"),
    }
}

pub fn more_errors_line(lang: Lang, remaining: usize) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("... and {remaining} more error(s)"),
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
        Lang::En | Lang::Custom(_) => {
            "Files the app is not sure enough about are marked \u{26a0} and are never \
             ticked for you after a scan - look at them before deleting.\n\n\
             A game protected by anti-cheat has its own row marked \
             \u{1f6e1}\u{fe0f} Anti-Cheat Protected: its files still delete normally, the \
             shield only stops an unattended re-trim from touching that game.\n\n\
             Checkboxes on tree rows select a whole disk, game, category, or folder. \
             Right-clicking a disk, game, or category opens bulk selection actions \
             (including a category across the whole disk).\n\n\
             Keyboard: \u{2191}\u{2193} - cursor, PgUp/PgDn - page, \
             \u{2192}/\u{2190} - expand/collapse, Space - select."
                .to_string()
        }
    }
}

pub fn selected_summary(lang: Lang, count: usize, size: &str) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Selected {count} file(s), will free up {size}"),
    }
}

/// Live disk-usage line: total bytes occupied by every scanned game, plus
/// the percentage of that total the current selection would free (see
/// `crate::model::freed_percent`). `total` is already formatted via
/// `crate::model::format_size`.
pub fn occupancy_summary(lang: Lang, total: &str, pct: f64) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => {
            format!("Games occupy {total} \u{b7} selection frees {pct:.1}%")
        }
    }
}

/// Persistent scan-timing summary shown in the bottom bar after a scan
/// completes (see `crate::model::ScanTiming`, `crate::app::GameTrimmerApp::last_scan_timing`).
/// `scan`/`analyze`/`total` are already formatted via `crate::model::format_duration`.
///
/// Worded as containment - "within" - rather than as a list of three
/// independent figures. The file table is read underneath the classification
/// now, so the first two spans overlap and do not sum to the total; printed
/// side by side with separators, they read as arithmetic that does not work
/// ("32s, 1:03, total 1:04") and the first thing anyone does with them is try
/// to add them.
pub fn scan_timing_summary(lang: Lang, scan: &str, analyze: &str, total: &str) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => {
            format!("Scan {scan} within analysis {analyze} \u{b7} Total {total}")
        }
    }
}

// -- ui::top_bar --

// -- ui::tree_view --

pub fn disk_label(lang: Lang, disk: &str) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Disk {disk}"),
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
        "paradox" => "Paradox",
        "itch" => "itch.io",
        "xbox" => "Xbox",
        // The one vendor with no launcher behind it: a folder the user pointed
        // the scanner at by hand (`worker::manual::MANUAL_VENDOR`), so it is
        // the only tag that needs translating rather than branding.
        "manual" => {
            return match lang {
                Lang::En | Lang::Custom(_) => "Added by hand".to_string(),
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
        Lang::En | Lang::Custom(_) => ("\u{201c}", "\u{201d}"),
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
        (Lang::En | Lang::Custom(_), TopKey::Disk(disk)) => format!("Select all on disk {disk}"),
        (Lang::En | Lang::Custom(_), TopKey::Launcher(vendor)) => {
            format!("Select all in {}", launcher_label(lang, vendor))
        }
        (Lang::En | Lang::Custom(_), TopKey::Library(root)) => {
            format!("Select all in library {}", root.to_string_lossy())
        }
        (Lang::En | Lang::Custom(_), TopKey::Category(category)) => format!(
            "Select every {}",
            quoted(lang, crate::model::category_display(lang, *category))
        ),
        (Lang::En | Lang::Custom(_), TopKey::Flat) => "Select everything".to_string(),
        (Lang::En | Lang::Custom(_), TopKey::Unattributed(_)) => {
            "Select all unattributed".to_string()
        }
    }
}

/// The other half of [`select_all_in_group`].
pub fn deselect_all_in_group(lang: Lang, key: &crate::model::TopKey) -> String {
    use crate::model::TopKey;
    match (lang, key) {
        (Lang::En | Lang::Custom(_), TopKey::Disk(disk)) => format!("Deselect all on disk {disk}"),
        (Lang::En | Lang::Custom(_), TopKey::Launcher(vendor)) => {
            format!("Deselect all in {}", launcher_label(lang, vendor))
        }
        (Lang::En | Lang::Custom(_), TopKey::Library(root)) => {
            format!("Deselect all in library {}", root.to_string_lossy())
        }
        (Lang::En | Lang::Custom(_), TopKey::Category(category)) => format!(
            "Deselect every {}",
            quoted(lang, crate::model::category_display(lang, *category))
        ),
        (Lang::En | Lang::Custom(_), TopKey::Flat) => "Deselect everything".to_string(),
        (Lang::En | Lang::Custom(_), TopKey::Unattributed(_)) => {
            "Deselect all unattributed".to_string()
        }
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
        (Lang::En | Lang::Custom(_), TopKey::Disk(disk)) => format!("on the whole disk {disk}"),
        (Lang::En | Lang::Custom(_), TopKey::Launcher(vendor)) => {
            format!("across all of {}", launcher_label(lang, vendor))
        }
        (Lang::En | Lang::Custom(_), TopKey::Library(root)) => {
            format!("in the whole library {}", root.to_string_lossy())
        }
        // The category and flat axes fold the category row away, so these two
        // never reach a call site - answered anyway rather than left to a
        // catch-all arm that a sixth axis could quietly fall into.
        (Lang::En | Lang::Custom(_), TopKey::Category(_) | TopKey::Flat) => {
            "everywhere".to_string()
        }
        (Lang::En | Lang::Custom(_), TopKey::Unattributed(_)) => {
            "across everything unattributed".to_string()
        }
    }
}

pub fn select_all_in_game(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Select all in {}", quoted(lang, name)),
    }
}

pub fn deselect_all_in_game(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("Deselect all in {}", quoted(lang, name)),
    }
}

/// "Select this category across the whole branch", for whatever the branch is
/// under the active axis.
pub fn select_category_in_group(lang: Lang, label: &str, key: &crate::model::TopKey) -> String {
    let scope = whole_group_scope(lang, key);
    match lang {
        Lang::En | Lang::Custom(_) => format!("Select {} {scope}", quoted(lang, label)),
    }
}

/// The other half of [`select_category_in_group`].
pub fn deselect_category_in_group(lang: Lang, label: &str, key: &crate::model::TopKey) -> String {
    let scope = whole_group_scope(lang, key);
    match lang {
        Lang::En | Lang::Custom(_) => format!("Deselect {} {scope}", quoted(lang, label)),
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
        Lang::En | Lang::Custom(_) => format!(
            "Select {} in {}",
            quoted(lang, category),
            quoted(lang, game)
        ),
    }
}

/// The other half of [`select_category_in_game`].
pub fn deselect_category_in_game(lang: Lang, category: &str, game: &str) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!(
            "Deselect {} in {}",
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
        Lang::En | Lang::Custom(_) => {
            format!("{path}\nReason: {rule_desc} (confidence {confidence}%)")
        }
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
/// Unused since the classification cycle moved into `core`, where the
/// interface language does not reach: its one caller rendered every stored
/// reason with `Lang::En`, which is `LangReason`'s own `Display`. Kept
/// rather than deleted because the Ukrainian wording below is translation
/// work, and GT-391 (the localization thaw) is where it is decided whether
/// a stored reason is ever shown in another language - see
/// `worker::descriptions`, which translates on the way to the screen.
#[allow(dead_code)]
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
        Lang::En | Lang::Custom(_) => format!("\nLanguage: {lang_tag}"),
    }
}

/// Tooltip line for a localization finding whose language could not be named
/// (GT-464). It says three things, because a blank label alone says none of
/// them: the file does stand in a set of translations, the detector could not
/// read which language it is, and that is why "select all" leaves it alone.
pub fn hover_lang_unnamed_suffix(lang: Lang) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => {
            "\nLanguage: undetermined - this file fills a slot in a confirmed set of \
             languages, but its name is not one this build's dictionary knows. \
             Bulk selection leaves it for you to judge."
                .to_string()
        }
    }
}

/// Tooltip line spelling out the logical size (allocated-size accounting): the row and totals show
/// the on-disk allocated size as primary - the honest "space freed" figure -
/// so this adds the logical size for context. Only shown when the two differ
/// (`logical` is a pre-formatted size string).
pub fn hover_logical_size_suffix(lang: Lang, logical: &str) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("\nLogical size: {logical}"),
    }
}

/// Tooltip line spelling out the on-disk size when the row itself has fallen
/// back to showing the logical size instead (a resident NTFS file: content
/// small enough to live inside the MFT record, so it occupies no cluster and
/// `size_on_disk` is 0 - see `model::display_size_bytes`). Keeps the on-disk
/// figure - the honest "this frees nothing" answer - reachable even though
/// the row no longer shows it directly.
pub fn hover_on_disk_size_suffix(lang: Lang, on_disk: &str) -> String {
    match lang {
        Lang::En | Lang::Custom(_) => format!("\nOn-disk size: {on_disk}"),
    }
}

/// Tooltip suffix explaining that intro videos are replaced with micro-stubs on removal.
pub fn hover_stub_suffix(lang: Lang) -> String {
    format!("\n{}", strings(lang).hover_stub_note)
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
        Lang::En | Lang::Custom(_) => "Disk;Category;Game;Group folder;Path;Size (bytes);Size;Confidence;Source;Rule/reason;Language;Selected",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A translation may reword a reason; it may not delete the evidence in
    /// it. The reparse-point block is permanent and fail-closed, so the only
    /// thing the user can act on is *which* nested link caused it - and the
    /// non-English string (hardcoded here, not routed through the frozen
    /// `Strings` table - see `deletion_block_reason`) is what the tree's
    /// tooltip shows for any language but English, `uk` included.
    #[test]
    fn the_reparse_point_translation_keeps_the_offending_path() {
        let reason = gametrimmer_core::safety::DeleteBlockReason::ReparsePoint(
            std::path::PathBuf::from(r"D:\Games\Some Game\mods\link"),
        )
        .to_string();

        let english = deletion_block_reason(Lang::En, &reason);
        let ukrainian = deletion_block_reason(Lang::parse("uk").expect("parse uk"), &reason);

        assert!(english.contains(r"D:\Games\Some Game\mods\link"));
        assert!(
            ukrainian.contains(r"D:\Games\Some Game\mods\link"),
            "the Ukrainian reason dropped the path: {ukrainian}"
        );
        assert_ne!(
            ukrainian, english,
            "it should still be translated, not passed through"
        );
    }
}
