//! File-level export/import of the two rule packs (`rules.json`,
//! `l10n_rules.json`) behind the settings dialog's «Export rules» /
//! «Import rules» buttons. The merge semantics live in
//! `gametrimmer_core::packs`; this module only decides which files to read
//! and write. Errors are user-facing, already-localized strings - they end
//! up directly in the warnings list.

use std::path::{Path, PathBuf};

use gametrimmer_core::packs::{self, MergeStats, PackKind};

use crate::i18n::{self, Lang};

use super::{ensure_l10n_rules_path, ensure_rules_path, L10N_RULES_FILE_NAME, RULES_FILE_NAME};

/// Writes both packs into `dir`. The source files always exist by the time
/// they are read - `ensure_*` materializes the embedded defaults next to
/// the executable on first use - so the export is always a verbatim copy of
/// exactly what the scanner runs with.
pub fn export_packs_to(lang: Lang, dir: &Path) -> Result<(), String> {
    let rules_text = read_ensured(lang, ensure_rules_path())?;
    let l10n_text = read_ensured(lang, ensure_l10n_rules_path())?;

    write_text(lang, &dir.join(RULES_FILE_NAME), &rules_text)?;
    write_text(lang, &dir.join(L10N_RULES_FILE_NAME), &l10n_text)
}

/// The path the scanner actually loads a pack of `kind` from, materializing
/// the embedded default next to the executable if it is not there yet.
///
/// Public because the settings dialog shows it: when a pack reads "does not
/// parse", the next question is where the file is.
pub fn pack_path(kind: PackKind) -> std::io::Result<PathBuf> {
    match kind {
        PackKind::CategoryRules => ensure_rules_path(),
        PackKind::LangPack => ensure_l10n_rules_path(),
    }
}

/// The embedded default text for a pack of `kind`.
fn builtin_text(kind: PackKind) -> Result<String, String> {
    match kind {
        PackKind::CategoryRules => Ok(gametrimmer_core::rules::BUILTIN_RULES_JSON.to_string()),
        PackKind::LangPack => gametrimmer_core::langdetect::LangPack::builtin()
            .to_json_pretty()
            .map_err(|err| err.to_string()),
    }
}

/// Whether the pack file on disk still parses.
///
/// Shown live in the settings dialog rather than only as a toast after an
/// import: a hand-edited `rules.json` that no longer compiles otherwise
/// fails silently at scan time, with the app quietly running on fewer rules
/// than the user thinks (audit §6.6). A pack that cannot even be read counts
/// as invalid - from the user's side the effect is the same.
pub fn pack_is_valid(kind: PackKind) -> bool {
    let Ok(path) = pack_path(kind) else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    pack_text_is_valid(kind, &text)
}

/// The parse half of [`pack_is_valid`], split out so it can be tested
/// against text instead of against whatever happens to sit next to the test
/// binary.
fn pack_text_is_valid(kind: PackKind, text: &str) -> bool {
    match kind {
        PackKind::CategoryRules => gametrimmer_core::rules::RuleEngine::from_json(text).is_ok(),
        PackKind::LangPack => gametrimmer_core::langdetect::LangPack::from_json(text).is_ok(),
    }
}

/// Overwrites a pack with the embedded default, keeping the previous file as
/// `*.bak`.
///
/// The way out of an invalid pack: without it a broken hand edit can only be
/// fixed by finding the file and deleting it, which is exactly the knowledge
/// a user in that position does not have. Purely a filesystem operation, so
/// it runs synchronously rather than on a worker thread like export/import.
pub fn restore_builtin(lang: Lang, kind: PackKind) -> Result<String, String> {
    let target = pack_path(kind).map_err(|err| i18n::prepare_rules_file_failed(lang, err))?;
    restore_builtin_at(lang, kind, &target)
}

/// The path-taking half of [`restore_builtin`], so a test can restore into a
/// temp directory instead of over the packs next to the test binary.
fn restore_builtin_at(lang: Lang, kind: PackKind, target: &Path) -> Result<String, String> {
    let text = builtin_text(kind)?;
    backup(lang, target)?;
    write_text(lang, target, &text)?;
    Ok(i18n::rules_restored(lang, target.display()))
}

/// Unwraps an `ensure_*` result and reads the materialized file.
fn read_ensured(lang: Lang, ensured: std::io::Result<PathBuf>) -> Result<String, String> {
    let path = ensured.map_err(|err| i18n::prepare_rules_file_failed(lang, err))?;
    std::fs::read_to_string(&path).map_err(|err| i18n::read_file_failed(lang, path.display(), err))
}

/// Imports every picked pack file: detects its kind, merges it into the
/// current effective pack of that kind, backs the previous file up as
/// `*.bak` and writes the merged result where the scanner will load it
/// from. Returns the ready-to-show, already-localized summary. Stops at the
/// first broken file - files before it are already merged and stay merged,
/// which the error message says explicitly.
pub fn import_pack_files(lang: Lang, files: &[PathBuf]) -> Result<String, String> {
    let mut rules_stats: Option<MergeStats> = None;
    let mut lang_stats: Option<MergeStats> = None;

    for (index, file) in files.iter().enumerate() {
        let (kind, stats) = import_one_file(lang, file).map_err(|err| {
            let done_note = if index > 0 {
                i18n::previous_files_already_imported(lang, index)
            } else {
                String::new()
            };
            format!("{}: {err}{done_note}", file.display())
        })?;
        match kind {
            PackKind::CategoryRules => rules_stats = Some(accumulate(rules_stats, stats)),
            PackKind::LangPack => lang_stats = Some(accumulate(lang_stats, stats)),
        }
    }

    Ok(build_summary(lang, rules_stats, lang_stats))
}

/// Reads one picked file, detects which pack it is and merges it into the
/// matching current pack on disk.
fn import_one_file(lang: Lang, file: &Path) -> Result<(PackKind, MergeStats), String> {
    let text =
        std::fs::read_to_string(file).map_err(|err| i18n::read_picked_file_failed(lang, err))?;
    let kind = packs::detect_pack_kind(&text).map_err(|err| err.to_string())?;
    let stats = match kind {
        PackKind::CategoryRules => import_category_rules(lang, &text)?,
        PackKind::LangPack => import_lang_pack(lang, &text)?,
    };
    Ok((kind, stats))
}

/// Merges an incoming rules.json into the current effective one next to the
/// executable (materialized from the embedded defaults if this is the first
/// touch) and writes the merged result back.
fn import_category_rules(lang: Lang, incoming: &str) -> Result<MergeStats, String> {
    let target = ensure_rules_path().map_err(|err| i18n::prepare_rules_json_failed(lang, err))?;
    let base = std::fs::read_to_string(&target)
        .map_err(|err| i18n::read_file_failed(lang, target.display(), err))?;

    let (merged, stats) =
        packs::merge_category_rules(&base, incoming).map_err(|err| err.to_string())?;
    backup(lang, &target)?;
    write_text(lang, &target, &merged)?;
    Ok(stats)
}

/// Merges an incoming l10n_rules.json into the current effective pack next
/// to the executable - same materialize-first contract as
/// [`import_category_rules`], so the first-ever import starts from exactly
/// what the scanner uses today.
fn import_lang_pack(lang: Lang, incoming: &str) -> Result<MergeStats, String> {
    let target =
        ensure_l10n_rules_path().map_err(|err| i18n::prepare_l10n_rules_failed(lang, err))?;
    let base = std::fs::read_to_string(&target)
        .map_err(|err| i18n::read_file_failed(lang, target.display(), err))?;

    let (merged, stats) =
        packs::merge_lang_packs(&base, incoming).map_err(|err| err.to_string())?;
    backup(lang, &target)?;
    write_text(lang, &target, &merged)?;
    Ok(stats)
}

/// Copies an existing `target` aside as `<name>.bak` before it gets
/// overwritten by a merge, so one step of rolling back the import is always
/// a manual rename away. A `target` that does not exist yet needs no backup.
fn backup(lang: Lang, target: &Path) -> Result<(), String> {
    if !target.is_file() {
        return Ok(());
    }
    let mut name = target
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(".bak");
    let bak = target.with_file_name(name);
    std::fs::copy(target, &bak)
        .map(|_| ())
        .map_err(|err| i18n::backup_failed(lang, bak.display(), err))
}

fn write_text(lang: Lang, path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|err| i18n::write_failed(lang, path.display(), err))
}

fn accumulate(current: Option<MergeStats>, stats: MergeStats) -> MergeStats {
    match current {
        Some(prev) => MergeStats {
            added: prev.added + stats.added,
            updated: prev.updated + stats.updated,
        },
        None => stats,
    }
}

/// One status line covering whichever pack kinds the import touched, e.g.
/// "Rules imported: categories - 2 new, 1 updated; localization - 1 new
/// language, 12 new words. Changes take effect from the next scan."
fn build_summary(lang: Lang, rules: Option<MergeStats>, lang_stats: Option<MergeStats>) -> String {
    let mut parts = Vec::new();
    if let Some(stats) = rules {
        parts.push(i18n::summary_categories_part(
            lang,
            stats.added,
            stats.updated,
        ));
    }
    if let Some(stats) = lang_stats {
        parts.push(i18n::summary_lang_part(lang, stats.added, stats.updated));
    }
    i18n::summary_final(lang, &parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_mentions_only_the_imported_kinds() {
        let rules_only = build_summary(
            Lang::Uk,
            Some(MergeStats {
                added: 2,
                updated: 1,
            }),
            None,
        );
        assert!(rules_only.contains("категорії — нових 2, оновлено 1"));
        assert!(!rules_only.contains("локалізація"));

        let both = build_summary(
            Lang::Uk,
            Some(MergeStats {
                added: 1,
                updated: 0,
            }),
            Some(MergeStats {
                added: 1,
                updated: 12,
            }),
        );
        assert!(both.contains("категорії"));
        assert!(both.contains("локалізація — нових мов 1, нових слів 12"));
    }

    #[test]
    fn summary_mentions_only_the_imported_kinds_english() {
        let rules_only = build_summary(
            Lang::En,
            Some(MergeStats {
                added: 2,
                updated: 1,
            }),
            None,
        );
        assert!(rules_only.contains("categories - 2 new, 1 updated"));
        assert!(!rules_only.contains("localization"));
    }

    #[test]
    fn backup_copies_an_existing_file_and_skips_a_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("rules.json");

        backup(Lang::En, &target).expect("missing target needs no backup");
        assert!(!dir.path().join("rules.json.bak").exists());

        std::fs::write(&target, "[]").unwrap();
        backup(Lang::En, &target).expect("existing target is backed up");
        let bak = dir.path().join("rules.json.bak");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "[]");
    }

    /// Both directions matter: the built-in packs must pass their own check,
    /// or the dialog would show "Syntax error" on a fresh install; and text
    /// that does not parse must fail it, or the check reports nothing.
    #[test]
    fn validity_accepts_the_builtin_packs_and_rejects_broken_text() {
        for kind in [PackKind::CategoryRules, PackKind::LangPack] {
            let builtin = builtin_text(kind).expect("builtin pack serializes");
            assert!(
                pack_text_is_valid(kind, &builtin),
                "{kind:?}: the built-in pack fails its own validity check",
            );
            assert!(!pack_text_is_valid(kind, "{ not json"), "{kind:?}");
            assert!(!pack_text_is_valid(kind, ""), "{kind:?}: empty");
        }
    }

    /// A pack of the wrong shape is not merely unparseable JSON - the two
    /// kinds must not accept each other's file.
    #[test]
    fn validity_does_not_accept_the_other_kind_of_pack() {
        let rules = builtin_text(PackKind::CategoryRules).expect("builtin rules");
        let lang_pack = builtin_text(PackKind::LangPack).expect("builtin lang pack");

        assert!(!pack_text_is_valid(PackKind::LangPack, &rules));
        assert!(!pack_text_is_valid(PackKind::CategoryRules, &lang_pack));
    }

    /// The way out of a broken hand edit: restoring must produce a pack that
    /// passes the check, and must not throw the broken file away - the user
    /// may want whatever they were trying to write back.
    #[test]
    fn restoring_replaces_a_broken_pack_and_keeps_it_as_a_backup() {
        for (kind, name) in [
            (PackKind::CategoryRules, RULES_FILE_NAME),
            (PackKind::LangPack, L10N_RULES_FILE_NAME),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join(name);
            std::fs::write(&target, "{ broken").unwrap();

            restore_builtin_at(Lang::En, kind, &target).expect("restore succeeds");

            let restored = std::fs::read_to_string(&target).unwrap();
            assert!(
                pack_text_is_valid(kind, &restored),
                "{kind:?}: the restored pack is not valid",
            );
            let backup = std::fs::read_to_string(target.with_extension("json.bak")).unwrap();
            assert_eq!(backup, "{ broken", "{kind:?}: the broken file was lost");
        }
    }

    /// Restoring over a pack that is not there yet is the first-run case; it
    /// must still produce a valid file rather than failing on the missing
    /// backup source.
    #[test]
    fn restoring_works_when_there_is_no_pack_file_yet() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(RULES_FILE_NAME);

        restore_builtin_at(Lang::En, PackKind::CategoryRules, &target).expect("restore succeeds");

        let restored = std::fs::read_to_string(&target).unwrap();
        assert!(pack_text_is_valid(PackKind::CategoryRules, &restored));
        assert!(!target.with_extension("json.bak").exists());
    }

    #[test]
    fn export_writes_both_pack_files() {
        // `ensure_*` materializes both files next to the test binary on
        // first use (from the embedded defaults), so the export always has
        // something real to copy.
        let dir = tempfile::tempdir().unwrap();

        export_packs_to(Lang::En, dir.path())
            .expect("export succeeds from the materialized defaults");

        let rules = std::fs::read_to_string(dir.path().join(RULES_FILE_NAME)).unwrap();
        gametrimmer_core::rules::RuleEngine::from_json(&rules)
            .expect("exported rules.json compiles");
        let l10n = std::fs::read_to_string(dir.path().join(L10N_RULES_FILE_NAME)).unwrap();
        gametrimmer_core::langdetect::LangPack::from_json(&l10n)
            .expect("exported l10n_rules.json parses");
    }
}
