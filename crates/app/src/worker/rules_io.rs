//! The two optional overlay packs (`rules.json`, `l10n_rules.json`) and the
//! personal exception pack (`personal_rules.json`).
//!
//! The app writes neither overlay: the rules a scan runs on are compiled
//! into the binary, and an overlay is in effect because someone put a file
//! of that name next to the executable - the way `winapp2.ini` is in effect
//! for CCleaner. See `docs/rules-packs.md`. All this module does is say
//! where such a file would be, whether the one that is there still parses,
//! and write the personal pack, which *is* the app's to write.
//!
//! Errors are user-facing, already-localized strings - they end up directly
//! in the warnings list.

use std::path::{Path, PathBuf};

use gametrimmer_core::packs::{self, PackKind};

use crate::i18n::{self, Lang};

/// Where an overlay pack of `kind` would live: next to the executable.
///
/// The path is returned whether or not a file is there, because that is the
/// question the settings dialog asks - "where do I put one?" is as useful an
/// answer as "here is the one you have".
pub fn pack_path(kind: PackKind) -> std::io::Result<PathBuf> {
    match kind {
        PackKind::CategoryRules => super::rules_path(),
        PackKind::LangPack => super::l10n_rules_path(),
    }
}

/// Whether an overlay pack of `kind` is present at all.
///
/// Absent is the normal state and not a fault: the built-in rules are the
/// full rule set on their own.
pub fn pack_is_present(kind: PackKind) -> bool {
    super::overlay_pack_path(kind).is_some()
}

/// Whether the overlay pack on disk still parses.
///
/// Shown live in the settings dialog rather than only when a scan runs: an
/// overlay that no longer compiles otherwise fails silently at scan time,
/// with the app quietly running on the built-ins alone while the user
/// believes their file is in effect. A pack that cannot even be read counts
/// as invalid - from the user's side the effect is the same. A pack that is
/// not there is neither valid nor invalid; ask [`pack_is_present`] first.
pub fn pack_is_valid(kind: PackKind) -> bool {
    let Some(path) = super::overlay_pack_path(kind) else {
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
    validate_pack_text(kind, text).is_ok()
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

/// Adds "never touch `rel_path` in the game whose vendor id is `app_id`" to
/// the personal exception pack, and returns the already-localized line to show
/// for it.
///
/// Synchronous: it is one read and one atomic write of a file measured in
/// kilobytes, and pushing it onto a worker thread would only add a round trip
/// between the click and the row leaving the tree.
///
/// The write goes through a validating atomic writer, so a pack that would no
/// longer compile is never left on disk - the scan folds this file into the
/// rule engine and a broken one costs the run its exceptions.
pub fn add_personal_exception(
    lang: Lang,
    app_id: &str,
    rel_path: &str,
    desc: gametrimmer_core::localized::LocalizedText,
) -> Result<String, String> {
    let path = super::ensure_personal_rules_path()
        .map_err(|err| i18n::prepare_rules_file_failed(lang, err))?;
    add_personal_exception_at(lang, &path, app_id, rel_path, desc)
}

/// The path-taking half of [`add_personal_exception`], so a test can write
/// into a temp directory instead of over the pack next to the test binary.
fn add_personal_exception_at(
    lang: Lang,
    path: &Path,
    app_id: &str,
    rel_path: &str,
    desc: gametrimmer_core::localized::LocalizedText,
) -> Result<String, String> {
    let current = std::fs::read_to_string(path)
        .map_err(|err| i18n::read_file_failed(lang, path.display(), err))?;

    let rule = gametrimmer_core::rules::Rule::keep_file(app_id, rel_path, desc);
    let (json, added) =
        packs::add_rule(&current, rule).map_err(|err| format!("{}: {err}", path.display()))?;
    if !added {
        return Ok(i18n::exception_already_kept(lang, rel_path));
    }

    gametrimmer_core::atomic_file::atomic_write_with_backup(
        path,
        json.as_bytes(),
        |_path, bytes| {
            let text = std::str::from_utf8(bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            // The personal pack *is* a category-rules pack - same envelope, same
            // parser, same compile check. Only its polarity and its ownership
            // differ, and neither is something this validation can or should see.
            validate_pack_text(PackKind::CategoryRules, text)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        },
    )
    .map_err(|err| i18n::write_failed(lang, path.display(), err))?;

    Ok(i18n::exception_kept(lang, rel_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-in text of a pack of `kind` - the reference an overlay is
    /// written against, and the only pack text a test can be sure of.
    fn builtin_text(kind: PackKind) -> String {
        match kind {
            PackKind::CategoryRules => gametrimmer_core::rules::BUILTIN_RULES_JSON.to_string(),
            PackKind::LangPack => gametrimmer_core::langdetect::LangPack::builtin()
                .to_json_pretty()
                .expect("the built-in lang pack serializes"),
        }
    }

    /// Both directions matter: the built-in packs must pass the check an
    /// overlay is held to, or the documented format would be one the app
    /// itself fails; and text that does not parse must fail it, or the check
    /// reports nothing.
    #[test]
    fn validity_accepts_the_builtin_packs_and_rejects_broken_text() {
        for kind in [PackKind::CategoryRules, PackKind::LangPack] {
            assert!(
                pack_text_is_valid(kind, &builtin_text(kind)),
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
        let rules = builtin_text(PackKind::CategoryRules);
        let lang_pack = builtin_text(PackKind::LangPack);

        assert!(!pack_text_is_valid(PackKind::LangPack, &rules));
        assert!(!pack_text_is_valid(PackKind::CategoryRules, &lang_pack));
    }

    /// The templates shipped in `docs/templates/` are the starting point the
    /// documentation hands out. One that does not load would send every
    /// first-time overlay author straight into "your pack does not parse".
    #[test]
    fn the_documented_templates_are_packs_the_app_would_accept() {
        for (kind, name) in [
            (PackKind::CategoryRules, "rules.template.json"),
            (PackKind::LangPack, "l10n_rules.template.json"),
        ] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../docs/templates")
                .join(name);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            if let Err(err) = validate_pack_text(kind, &text) {
                panic!("{name} is not a pack the app would load: {err}");
            }
        }
    }

    /// A fresh personal pack, as `ensure_personal_rules_path` would seed it.
    fn empty_personal_pack(dir: &Path) -> PathBuf {
        let path = dir.join(super::super::PERSONAL_RULES_FILE_NAME);
        std::fs::write(
            &path,
            gametrimmer_core::rules::serialize_rule_list(&[]).expect("serialize the empty pack"),
        )
        .expect("seed the personal pack");
        path
    }

    /// The acceptance criterion, at the file level: what "never touch this"
    /// writes is still there, and still a veto, after the file is read back -
    /// which is all a re-scan does with it.
    #[test]
    fn a_personal_exception_survives_being_written_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = empty_personal_pack(dir.path());

        add_personal_exception_at(
            Lang::En,
            &path,
            "620",
            r"Support\ru\voices.pak",
            "Kept by me".into(),
        )
        .expect("the exception is written");

        let engine = gametrimmer_core::rules::RuleEngine::load(&path)
            .expect("the written pack is the pack a scan loads");
        assert_eq!(
            engine.classify(r"Support\ru\voices.pak", Some("620")),
            gametrimmer_core::rules::Verdict::Kept,
        );
        assert_eq!(
            engine.classify(r"Support\ru\voices.pak", Some("730")),
            gametrimmer_core::rules::Verdict::Unmatched,
            "the exception is bound to the game it was written in",
        );
    }

    /// Right-clicking the same file twice is an ordinary thing to do, and it
    /// has to say what happened rather than quietly growing the file.
    #[test]
    fn keeping_the_same_file_twice_reports_it_as_already_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = empty_personal_pack(dir.path());
        let keep = || {
            add_personal_exception_at(Lang::En, &path, "620", r"data\loc_de.pak", "Kept".into())
                .expect("the exception is written")
        };

        let first = keep();
        let second = keep();

        assert_ne!(first, second, "the second click must say something else");
        let rules = gametrimmer_core::rules::parse_rule_list(
            &std::fs::read_to_string(&path).expect("read the pack"),
        )
        .expect("the pack parses");
        assert_eq!(rules.len(), 1, "the pack grew a duplicate");
    }
}
