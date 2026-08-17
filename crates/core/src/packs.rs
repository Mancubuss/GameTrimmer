//! Text-level merge of community rule packs for the settings dialog's
//! "Import rules" flow.
//!
//! The app layer only shuttles file contents around; everything that needs
//! JSON - telling the two pack kinds apart, validating an incoming file,
//! merging it into the current effective pack - lives here so it is
//! unit-testable and the app crate needs no serde dependency.

use crate::error::{CoreError, Result};
use crate::langdetect::LangPack;
use crate::rules::{parse_rule_list, serialize_rule_list, Rule, RuleEngine, RuleProvenance};

/// Which of the two pack files a picked JSON file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackKind {
    /// `rules.json` - `{"version", "rules"}`.
    CategoryRules,
    /// `l10n_rules.json` - `{"version", "languages", ...}`.
    LangPack,
}

/// What an import merge did, for the user-facing summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeStats {
    /// Entries the incoming pack added (rules / languages / words).
    pub added: usize,
    /// Entries the incoming pack replaced or extended.
    pub updated: usize,
}

/// Tells the two pack formats apart by the payload key each one carries, so
/// a community file keeps working whatever it is named.
///
/// This used to be the cheaper structural test - array meant category rules,
/// object meant a language pack. Versioning `rules.json` made both objects,
/// so the distinguishing key is what is left. It is also the more honest
/// test: it names what the file *contains* rather than what shape it happens
/// to have.
pub fn detect_pack_kind(json: &str) -> Result<PackKind> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    if value.get("rules").is_some() {
        return Ok(PackKind::CategoryRules);
    }
    if value.get("languages").is_some() {
        return Ok(PackKind::LangPack);
    }
    Err(CoreError::Other(
        "unrecognized rules file: expected a \"rules\" key (category rules) \
         or a \"languages\" key (localization pack)"
            .to_string(),
    ))
}

/// Whether two rules are the same rule for merge purposes.
///
/// Identity is `(polarity, category, scope, pattern)`. The first three joined
/// the key when exceptions did: a keep rule and a deleting rule over the same
/// pattern are opposites, not versions of each other, and the same pattern
/// scoped to two different games is two different rules - collapsing either
/// pair would silently drop one of them on the next import.
fn same_rule(a: &Rule, b: &Rule) -> bool {
    a.polarity == b.polarity
        && a.category == b.category
        && a.app_id == b.app_id
        && a.pattern == b.pattern
}

/// Merges an incoming rules.json on top of the current one. Rules are keyed
/// by [`same_rule`]: a match replaces the existing rule (so a pack can retune
/// confidence or depth), anything else is appended. The incoming file is fully
/// validated first - including regex compilation - so a broken pack is
/// rejected before it can reach disk.
pub fn merge_category_rules(base_json: &str, incoming_json: &str) -> Result<(String, MergeStats)> {
    RuleEngine::from_json(incoming_json)?;
    let base = parse_rule_list(base_json)?;
    let incoming = parse_rule_list(incoming_json)?;

    let mut merged: Vec<Rule> = base;
    let mut added = 0;
    let mut updated = 0;
    for mut rule in incoming {
        rule.provenance = RuleProvenance::ImportedUntrusted;
        match merged
            .iter_mut()
            .find(|existing| same_rule(existing, &rule))
        {
            Some(existing) => {
                *existing = rule;
                updated += 1;
            }
            None => {
                merged.push(rule);
                added += 1;
            }
        }
    }

    let json = serialize_rule_list(&merged)?;
    RuleEngine::from_json(&json)?;
    Ok((json, MergeStats { added, updated }))
}

/// Adds one rule to a pack, unless the pack already holds it.
///
/// This is how the personal exception pack grows: the app reads the file,
/// calls this, and writes the result back. `false` means the rule was already
/// there - the caller says "already kept" rather than writing an identical
/// second copy, which is what right-clicking the same file twice would
/// otherwise produce.
///
/// The result is validated by compiling it, exactly as an import is: a
/// personal pack that no longer parses would take the *scan's* whole rule set
/// down with it (see `worker::scan::run_scan`), so it must never be written
/// broken in the first place.
pub fn add_rule(base_json: &str, rule: Rule) -> Result<(String, bool)> {
    let mut rules = parse_rule_list(base_json)?;
    if rules.iter().any(|existing| same_rule(existing, &rule)) {
        return Ok((serialize_rule_list(&rules)?, false));
    }
    rules.push(rule);
    let json = serialize_rule_list(&rules)?;
    RuleEngine::from_json(&json)?;
    Ok((json, true))
}

/// Merges an incoming l10n_rules.json on top of the current one (see
/// [`LangPack::merge`] for the semantics). `added` counts new languages,
/// `updated` counts new words across every list.
pub fn merge_lang_packs(base_json: &str, incoming_json: &str) -> Result<(String, MergeStats)> {
    let base = LangPack::from_json(base_json)?;
    let incoming = LangPack::from_json(incoming_json)?;

    let base_languages = base.languages.len();
    let base_words = base.word_count();
    let merged = LangPack::merge(base, incoming);

    let stats = MergeStats {
        added: merged.languages.len() - base_languages,
        updated: merged.word_count() - base_words,
    };
    Ok((merged.to_json_pretty()?, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wraps a bare rule array in the pack envelope, so a test about merge
    /// *semantics* does not restate the wrapper each time.
    fn pack(rules: &str) -> String {
        format!(r#"{{"version":1,"rules":{rules}}}"#)
    }

    #[test]
    fn detect_pack_kind_tells_the_formats_apart() {
        assert_eq!(
            detect_pack_kind(&pack("[]")).unwrap(),
            PackKind::CategoryRules
        );
        assert_eq!(
            detect_pack_kind(r#"{"version": 1, "languages": []}"#).unwrap(),
            PackKind::LangPack
        );
        assert!(detect_pack_kind("42").is_err());
        assert!(detect_pack_kind("not json").is_err());
    }

    /// Both packs are versioned objects now, so the shape alone no longer
    /// tells them apart - an object carrying neither payload key has to be
    /// refused rather than guessed at.
    #[test]
    fn detect_pack_kind_refuses_an_object_that_is_neither_pack() {
        let err = detect_pack_kind(r#"{"version": 1}"#).expect_err("neither key is present");
        let message = err.to_string();
        assert!(message.contains("rules"), "{message}");
        assert!(message.contains("languages"), "{message}");
    }

    #[test]
    fn merge_category_rules_replaces_matches_and_appends_new() {
        let base = r#"[
            {"category": "redist_folder", "pattern": "^redist$", "desc": "Redist", "confidence": 90},
            {"category": "bonus", "pattern": "^extras$", "desc": "Bonus", "confidence": 80}
        ]"#;
        let incoming = r#"[
            {"category": "bonus", "pattern": "^extras$", "desc": "Bonus retuned", "confidence": 85},
            {"category": "docs_folder", "pattern": "^manuals$", "desc": "Manuals", "confidence": 88}
        ]"#;

        let (json, stats) =
            merge_category_rules(&pack(base), &pack(incoming)).expect("valid packs merge");

        assert_eq!(
            stats,
            MergeStats {
                added: 1,
                updated: 1
            }
        );
        let merged = parse_rule_list(&json).expect("merged output reparses");
        assert_eq!(merged.len(), 3);
        // The replaced rule keeps its position and takes the new tuning.
        assert_eq!(merged[1].desc.get("en"), "Bonus retuned");
        assert_eq!(merged[1].confidence, Some(85));
        assert_eq!(merged[1].provenance, RuleProvenance::ImportedUntrusted);
        assert_eq!(merged[2].desc.get("en"), "Manuals");
        assert_eq!(merged[2].provenance, RuleProvenance::ImportedUntrusted);
        // The merged output still compiles into an engine.
        RuleEngine::from_json(&json).expect("merged rules compile");
    }

    #[test]
    fn merge_category_rules_rejects_an_invalid_incoming_pack() {
        let base = "[]";
        let bad_regex = r#"[
            {"category": "bonus", "pattern": "^(broken", "desc": "Bad", "confidence": 80}
        ]"#;
        assert!(merge_category_rules(&pack(base), &pack(bad_regex)).is_err());

        let bad_category = r#"[
            {"category": "no_such_category", "pattern": "^x$", "desc": "Bad", "confidence": 80}
        ]"#;
        assert!(merge_category_rules(&pack(base), &pack(bad_category)).is_err());
    }

    #[test]
    fn merge_category_rules_preserves_optional_fields_of_kept_rules() {
        let base = r#"[
            {"category": "bonus", "pattern": "^extras$", "desc": "Bonus", "confidence": 80,
             "max_depth": 3, "extensions": ["pdf", "mp3"]}
        ]"#;
        let incoming = r#"[
            {"category": "docs_file", "pattern": "^readme", "desc": "Readme", "confidence": 88}
        ]"#;

        let (json, _) = merge_category_rules(&pack(base), &pack(incoming)).unwrap();
        let merged = parse_rule_list(&json).unwrap();
        assert_eq!(merged[0].max_depth, Some(3));
        assert_eq!(
            merged[0].extensions.as_deref(),
            Some(["pdf".to_string(), "mp3".to_string()].as_slice())
        );
        // Absent optional fields stay absent in the rewritten JSON rather
        // than serializing as nulls.
        assert!(!json.contains("null"));
    }

    /// Right-clicking the same file twice must not grow the pack twice, and
    /// the caller has to be able to tell the user which of the two happened.
    #[test]
    fn adding_the_same_exception_twice_changes_nothing_the_second_time() {
        let rule = keep_rule("620", r"Support\ru\voices.pak", "Kept by me");

        let (once, added) = add_rule(&pack("[]"), rule.clone()).expect("the first add succeeds");
        assert!(added);
        assert_eq!(parse_rule_list(&once).unwrap().len(), 1);

        let (twice, added) = add_rule(&once, rule).expect("the second add succeeds");
        assert!(!added, "an identical exception must not be appended again");
        assert_eq!(parse_rule_list(&twice).unwrap().len(), 1);
    }

    /// The scope is part of a rule's identity: the same path kept in two games
    /// is two exceptions, and merging them into one would silently unprotect
    /// one of the games.
    #[test]
    fn the_same_path_kept_in_two_games_stays_two_rules() {
        let path = r"Support\ru\voices.pak";
        let (first, _) = add_rule(&pack("[]"), keep_rule("620", path, "Kept in one")).unwrap();
        let (both, added) = add_rule(&first, keep_rule("730", path, "Kept in the other")).unwrap();

        assert!(added);
        assert_eq!(parse_rule_list(&both).unwrap().len(), 2);
    }

    /// Spelled out rather than imported under a short name so the test reads
    /// as what it is: the constructor the UI's "never touch this" writes with.
    fn keep_rule(app_id: &str, rel_path: &str, desc: &str) -> Rule {
        Rule::keep_file(app_id, rel_path, desc.into())
    }

    #[test]
    fn merge_lang_packs_counts_new_languages_and_words() {
        let base = LangPack::builtin().to_json_pretty().unwrap();
        let incoming = r#"{
            "version": 1,
            "languages": [
                {"key": "fr", "level_a": ["quebecois"]},
                {"key": "tlh", "level_a": ["klingon"]}
            ],
            "industry_words": [],
            "keep_default": [],
            "markers": {
                "negative": ["province_names"], "overridable_negative": [],
                "audio": [], "text": [], "video": [], "font": [],
                "loc_generic": [], "loc_specific": [],
                "video_extensions": [], "font_extensions": [], "text_extensions": []
            }
        }"#;

        let (json, stats) = merge_lang_packs(&base, incoming).expect("valid packs merge");

        assert_eq!(stats.added, 1, "only tlh is a new language");
        // quebecois + klingon + province_names.
        assert_eq!(stats.updated, 3);
        let merged = LangPack::from_json(&json).expect("merged output reparses");
        assert!(merged.languages.iter().any(|entry| entry.key == "tlh"));
    }

    #[test]
    fn merge_lang_packs_rejects_a_newer_version() {
        let base = LangPack::builtin().to_json_pretty().unwrap();
        let incoming = r#"{"version": 99, "languages": [], "industry_words": [],
            "keep_default": [], "markers": {"negative": [], "overridable_negative": [],
            "audio": [], "text": [], "video": [], "font": [], "loc_generic": [],
            "loc_specific": [], "video_extensions": [], "font_extensions": [],
            "text_extensions": []}}"#;
        assert!(merge_lang_packs(&base, incoming).is_err());
    }
}
