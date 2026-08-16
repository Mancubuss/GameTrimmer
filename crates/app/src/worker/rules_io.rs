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

fn pack_target_path(kind: PackKind) -> std::io::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .ok_or_else(|| std::io::Error::other("executable has no parent directory"))?;
    Ok(directory.join(match kind {
        PackKind::CategoryRules => RULES_FILE_NAME,
        PackKind::LangPack => L10N_RULES_FILE_NAME,
    }))
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
/// than the user thinks. A pack that cannot even be read counts
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
    gametrimmer_core::atomic_file::atomic_write_with_backup(
        target,
        text.as_bytes(),
        |_path, bytes| {
            let text = std::str::from_utf8(bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            validate_pack_text(kind, text)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        },
    )
    .map_err(|err| i18n::write_failed(lang, target.display(), err))?;
    Ok(i18n::rules_restored(lang, target.display()))
}

/// Unwraps an `ensure_*` result and reads the materialized file.
fn read_ensured(lang: Lang, ensured: std::io::Result<PathBuf>) -> Result<String, String> {
    let path = ensured.map_err(|err| i18n::prepare_rules_file_failed(lang, err))?;
    std::fs::read_to_string(&path).map_err(|err| i18n::read_file_failed(lang, path.display(), err))
}

/// Imports every picked pack file as an all-or-nothing batch. Every selected
/// file is size-checked, read and validated before either effective pack is
/// changed. The two output files share the same rollback-capable replacement
/// batch, so a failure cannot leave a half-imported selection.
pub struct PreparedRuleImport {
    outputs: Vec<(PathBuf, String)>,
    summary: String,
    pub preview: String,
}

pub fn prepare_pack_import(lang: Lang, files: &[PathBuf]) -> Result<PreparedRuleImport, String> {
    let mut incoming = Vec::with_capacity(files.len());
    for file in files {
        let metadata =
            std::fs::metadata(file).map_err(|err| format!("{}: {err}", file.display()))?;
        if metadata.len() > gametrimmer_core::rules::MAX_RULE_PACK_BYTES as u64 {
            return Err(format!(
                "{}: file exceeds the {} byte limit",
                file.display(),
                gametrimmer_core::rules::MAX_RULE_PACK_BYTES
            ));
        }
        let text = std::fs::read_to_string(file).map_err(|err| {
            format!(
                "{}: {}",
                file.display(),
                i18n::read_picked_file_failed(lang, err)
            )
        })?;
        let kind =
            packs::detect_pack_kind(&text).map_err(|err| format!("{}: {err}", file.display()))?;
        validate_pack_text(kind, &text).map_err(|err| format!("{}: {err}", file.display()))?;
        incoming.push((kind, text));
    }

    let needs_rules = incoming
        .iter()
        .any(|(kind, _)| *kind == PackKind::CategoryRules);
    let needs_lang = incoming.iter().any(|(kind, _)| *kind == PackKind::LangPack);
    let rules_target = needs_rules
        .then(|| {
            pack_target_path(PackKind::CategoryRules)
                .map_err(|err| i18n::prepare_rules_json_failed(lang, err))
        })
        .transpose()?;
    let lang_target = needs_lang
        .then(|| {
            pack_target_path(PackKind::LangPack)
                .map_err(|err| i18n::prepare_l10n_rules_failed(lang, err))
        })
        .transpose()?;
    let mut rules_text = rules_target
        .as_ref()
        .map(|path| {
            if path.is_file() {
                std::fs::read_to_string(path)
                    .map_err(|err| i18n::read_file_failed(lang, path.display(), err))
            } else {
                builtin_text(PackKind::CategoryRules)
            }
        })
        .transpose()?;
    let mut lang_text = lang_target
        .as_ref()
        .map(|path| {
            if path.is_file() {
                std::fs::read_to_string(path)
                    .map_err(|err| i18n::read_file_failed(lang, path.display(), err))
            } else {
                builtin_text(PackKind::LangPack)
            }
        })
        .transpose()?;
    let mut rules_stats: Option<MergeStats> = None;
    let mut lang_stats: Option<MergeStats> = None;

    for (kind, text) in incoming {
        let (merged, stats) = match kind {
            PackKind::CategoryRules => packs::merge_category_rules(
                rules_text.as_deref().expect("rules base was loaded"),
                &text,
            )
            .map_err(|err| err.to_string())?,
            PackKind::LangPack => packs::merge_lang_packs(
                lang_text.as_deref().expect("language base was loaded"),
                &text,
            )
            .map_err(|err| err.to_string())?,
        };
        match kind {
            PackKind::CategoryRules => {
                rules_text = Some(merged);
                rules_stats = Some(accumulate(rules_stats, stats));
            }
            PackKind::LangPack => {
                lang_text = Some(merged);
                lang_stats = Some(accumulate(lang_stats, stats));
            }
        }
    }

    let summary = build_summary(lang, rules_stats, lang_stats);
    let preview = format_preview(lang, &summary);
    let mut outputs = Vec::new();
    if let (Some(path), Some(text)) = (&rules_target, &rules_text) {
        outputs.push((path.clone(), text.clone()));
    }
    if let (Some(path), Some(text)) = (&lang_target, &lang_text) {
        outputs.push((path.clone(), text.clone()));
    }

    Ok(PreparedRuleImport {
        outputs,
        summary,
        preview,
    })
}

pub fn apply_prepared_import(lang: Lang, prepared: PreparedRuleImport) -> Result<String, String> {
    let outputs: Vec<(&Path, &[u8])> = prepared
        .outputs
        .iter()
        .map(|(path, text)| (path.as_path(), text.as_bytes()))
        .collect();
    gametrimmer_core::atomic_file::atomic_write_batch_with_backup(&outputs, |_path, bytes| {
        let text = std::str::from_utf8(bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let kind = packs::detect_pack_kind(text)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        validate_pack_text(kind, text)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    })
    .map_err(|err| i18n::write_failed(lang, "rule pack batch", err))?;

    Ok(prepared.summary)
}

/// The dialog shown before an import is written.
///
/// It reports what the *rules* gain and lose, and says plainly that the
/// effect on the library is only visible after a rescan.
///
/// It used to answer the stronger question - "how many files in the current
/// snapshot would change verdict?" - by re-classifying the whole active
/// inventory. Answering it without a rescan required `files` to hold a row
/// for every file of every game: 4.9 million rows against 720 thousand
/// findings on a real library. That inventory was this preview's only reader,
/// and it cost roughly 8 s to write and 10 s to delete on every scan, plus
/// some 700 MB of database - a permanent price on every run for a dialog
/// shown when someone imports a rule pack. The owner's call was that the
/// comparison was interesting but not worth the storage; importing now asks
/// for a rescan instead.
fn format_preview(lang: Lang, summary: &str) -> String {
    match lang {
        Lang::Uk => format!(
            "Попередній перегляд імпорту

{summary}

Щоб побачити, що це змінює у вашій бібліотеці, запустіть сканування після імпорту.

Продовжити?"
        ),
        Lang::En => format!(
            "Import preview

{summary}

Run a scan after importing to see what this changes in your library.

Continue?"
        ),
    }
}

fn validate_pack_text(kind: PackKind, text: &str) -> Result<(), String> {
    match kind {
        PackKind::CategoryRules => gametrimmer_core::rules::RuleEngine::from_json(text)
            .map(|_| ())
            .map_err(|error| error.to_string()),
        PackKind::LangPack => gametrimmer_core::langdetect::LangPack::from_json(text)
            .map(|_| ())
            .map_err(|error| error.to_string()),
    }
}

/// Copies an existing `target` aside as `<name>.bak` before it gets
/// overwritten by a merge, so one step of rolling back the import is always
/// a manual rename away. A `target` that does not exist yet needs no backup.
#[cfg(test)]
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
    gametrimmer_core::atomic_file::atomic_write_with_backup(
        path,
        text.as_bytes(),
        |_path, bytes| {
            std::str::from_utf8(bytes)
                .map(|_| ())
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        },
    )
    .map_err(|err| i18n::write_failed(lang, path.display(), err))
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
