//! Text-level merge of community rule packs for the settings dialog's
//! «Імпортувати правила» flow (docs/05_rules_pack_plan.md, крок 5).
//!
//! The app layer only shuttles file contents around; everything that needs
//! JSON - telling the two pack kinds apart, validating an incoming file,
//! merging it into the current effective pack - lives here so it is
//! unit-testable and the app crate needs no serde dependency.

use crate::error::{CoreError, Result};
use crate::langdetect::LangPack;
use crate::rules::{parse_rule_list, Rule, RuleEngine};

/// Which of the two pack files a picked JSON file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackKind {
    /// `rules.json` - a JSON array of category rules.
    CategoryRules,
    /// `l10n_rules.json` - a JSON object (the localization data pack).
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

/// Tells the two pack formats apart by their JSON top level. The array vs.
/// object distinction is structural, so a community file keeps working
/// whatever it is named.
pub fn detect_pack_kind(json: &str) -> Result<PackKind> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    match value {
        serde_json::Value::Array(_) => Ok(PackKind::CategoryRules),
        serde_json::Value::Object(_) => Ok(PackKind::LangPack),
        _ => Err(CoreError::Other(
            "невідомий формат файлу правил: очікується масив правил категорій \
             або об'єкт мовного пакета"
                .to_string(),
        )),
    }
}

/// Merges an incoming rules.json on top of the current one. Rules are keyed
/// by `(category, pattern)`: a match replaces the existing rule (so a pack
/// can retune confidence or depth), anything else is appended. The incoming
/// file is fully validated first - including regex compilation - so a broken
/// pack is rejected before it can reach disk.
pub fn merge_category_rules(base_json: &str, incoming_json: &str) -> Result<(String, MergeStats)> {
    RuleEngine::from_json(incoming_json)?;
    let base = parse_rule_list(base_json)?;
    let incoming = parse_rule_list(incoming_json)?;

    let mut merged: Vec<Rule> = base;
    let mut added = 0;
    let mut updated = 0;
    for rule in incoming {
        match merged
            .iter_mut()
            .find(|existing| existing.category == rule.category && existing.pattern == rule.pattern)
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

    let json = serde_json::to_string_pretty(&merged)?;
    Ok((json, MergeStats { added, updated }))
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

    #[test]
    fn detect_pack_kind_tells_the_formats_apart() {
        assert_eq!(detect_pack_kind("[]").unwrap(), PackKind::CategoryRules);
        assert_eq!(
            detect_pack_kind(r#"{"version": 1}"#).unwrap(),
            PackKind::LangPack
        );
        assert!(detect_pack_kind("42").is_err());
        assert!(detect_pack_kind("not json").is_err());
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

        let (json, stats) = merge_category_rules(base, incoming).expect("valid packs merge");

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
        assert_eq!(merged[1].desc, "Bonus retuned");
        assert_eq!(merged[1].confidence, 85);
        assert_eq!(merged[2].desc, "Manuals");
        // The merged output still compiles into an engine.
        RuleEngine::from_json(&json).expect("merged rules compile");
    }

    #[test]
    fn merge_category_rules_rejects_an_invalid_incoming_pack() {
        let base = "[]";
        let bad_regex = r#"[
            {"category": "bonus", "pattern": "^(broken", "desc": "Bad", "confidence": 80}
        ]"#;
        assert!(merge_category_rules(base, bad_regex).is_err());

        let bad_category = r#"[
            {"category": "no_such_category", "pattern": "^x$", "desc": "Bad", "confidence": 80}
        ]"#;
        assert!(merge_category_rules(base, bad_category).is_err());
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

        let (json, _) = merge_category_rules(base, incoming).unwrap();
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
