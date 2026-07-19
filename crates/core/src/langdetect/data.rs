//! Runtime-loadable data tables of the localization detector.
//!
//! The detection *algorithms* (families, positional analysis, scoring) live
//! in code; every word list they consult — the language dictionary, marker
//! words, veto lists — lives here as a [`LangData`] value. There is exactly
//! one source of truth for those word lists: `l10n_rules.json` at the repo
//! root, embedded into the binary via `include_str!` (mirroring how
//! `rules.json` seeds `gametrimmer_core::rules::BUILTIN_RULES_JSON`). A
//! `l10n_rules.json` file next to the *executable* overrides the embedded
//! copy at runtime, which is what makes community rule packs possible
//! (docs/05_rules_pack_plan.md). `dict.rs` and `markers.rs` now hold only
//! the algorithm-facing types (`Level`, `MarkerKind`, ...) and the design
//! rationale behind the word lists — the words themselves live in the JSON
//! file so editing them never means touching Rust source.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

use super::dict::Level;

/// Supported major version of `l10n_rules.json`. A file with a greater
/// version was produced by a newer GameTrimmer — refuse it rather than
/// silently misread it.
pub const LANG_PACK_VERSION: u32 = 1;

/// The canonical `l10n_rules.json`, embedded at build time from the repo
/// root — same layout as `gametrimmer_core::rules::BUILTIN_RULES_JSON`.
/// This is the single source of truth for the detector's word lists; a
/// broken file here is a broken build, not a runtime surprise (see
/// [`LangPack::builtin`]). `tests::embedded_file_matches_canonical_serialization`
/// guards against the file drifting out of sync with its own serde shape.
const EMBEDDED_L10N_RULES_JSON: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../l10n_rules.json"));

/// Canonical language keys must be `&'static str` throughout the engine
/// (`Occurrence`, `FamilyHit`, keep-lists compare against them). Every key —
/// built-in or introduced by a loaded community pack — is interned here on
/// first sight; each unique string is leaked exactly once for the process
/// lifetime, which for the built-in dictionary is a few dozen short strings.
static INTERNED_KEYS: LazyLock<Mutex<HashSet<&'static str>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn intern_key(key: &str) -> &'static str {
    let mut interned = INTERNED_KEYS.lock().expect("interner poisoned");
    if let Some(existing) = interned.get(key) {
        return existing;
    }
    let leaked: &'static str = Box::leak(key.to_string().into_boxed_str());
    interned.insert(leaked);
    leaked
}

// --- Serialized schema (l10n_rules.json) ---------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LangPackEntry {
    /// Canonical key, e.g. "fr", "pt-br".
    pub key: String,
    /// Self-sufficient aliases (full names, Steam folder names, locale tags).
    #[serde(default)]
    pub level_a: Vec<String>,
    /// Three-letter ISO codes — context-gated.
    #[serde(default)]
    pub level_b: Vec<String>,
    /// Two-letter ISO codes — family-gated.
    #[serde(default)]
    pub level_c: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkerTables {
    pub negative: Vec<String>,
    pub overridable_negative: Vec<String>,
    pub audio: Vec<String>,
    pub text: Vec<String>,
    pub video: Vec<String>,
    pub font: Vec<String>,
    pub loc_generic: Vec<String>,
    pub loc_specific: Vec<String>,
    pub video_extensions: Vec<String>,
    pub font_extensions: Vec<String>,
    pub text_extensions: Vec<String>,
}

/// The on-disk shape of `l10n_rules.json` — also what "export rules"
/// writes, so a user's starting point for edits is the exact built-in
/// state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LangPack {
    pub version: u32,
    pub languages: Vec<LangPackEntry>,
    pub industry_words: Vec<String>,
    pub keep_default: Vec<String>,
    pub markers: MarkerTables,
}

impl LangPack {
    /// The built-in tables, parsed from the JSON embedded at build time
    /// ([`EMBEDDED_L10N_RULES_JSON`]). `l10n_rules.json` at the repo root is
    /// part of the build the same way `rules.json` is — a file that fails to
    /// parse or carries a version this binary does not support is a build
    /// defect, not something to recover from at runtime, hence the `expect`.
    pub fn builtin() -> Self {
        Self::from_json(EMBEDDED_L10N_RULES_JSON)
            .expect("l10n_rules.json is embedded at build time from the repo root and must always parse as a valid, supported-version LangPack — see crates/core/src/langdetect/data.rs")
    }

    pub fn from_json(json: &str) -> Result<Self> {
        let pack: LangPack = serde_json::from_str(json)?;
        if pack.version > LANG_PACK_VERSION {
            return Err(CoreError::Other(format!(
                "l10n_rules.json version {} is newer than supported {} — \
                 оновіть GameTrimmer або використайте старіший пакет правил",
                pack.version, LANG_PACK_VERSION
            )));
        }
        Ok(pack)
    }

    pub fn to_json_pretty(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(CoreError::from)
    }

    /// Merges `incoming` on top of `base` for the import flow: languages
    /// are matched by canonical key (per-level aliases are unioned, new
    /// languages appended), every word list is set-unioned. Nothing is ever
    /// removed by an import - a community pack can only extend the tables,
    /// so a bad pack cannot silently disable built-in detection. The result
    /// is stamped with the current [`LANG_PACK_VERSION`].
    pub fn merge(base: LangPack, incoming: LangPack) -> LangPack {
        let mut languages = base.languages;
        for entry in incoming.languages {
            match languages
                .iter_mut()
                .find(|existing| existing.key.to_lowercase() == entry.key.to_lowercase())
            {
                Some(existing) => {
                    existing.level_a = union(std::mem::take(&mut existing.level_a), entry.level_a);
                    existing.level_b = union(std::mem::take(&mut existing.level_b), entry.level_b);
                    existing.level_c = union(std::mem::take(&mut existing.level_c), entry.level_c);
                }
                None => languages.push(entry),
            }
        }

        LangPack {
            version: LANG_PACK_VERSION,
            languages,
            industry_words: union(base.industry_words, incoming.industry_words),
            keep_default: union(base.keep_default, incoming.keep_default),
            markers: MarkerTables {
                negative: union(base.markers.negative, incoming.markers.negative),
                overridable_negative: union(
                    base.markers.overridable_negative,
                    incoming.markers.overridable_negative,
                ),
                audio: union(base.markers.audio, incoming.markers.audio),
                text: union(base.markers.text, incoming.markers.text),
                video: union(base.markers.video, incoming.markers.video),
                font: union(base.markers.font, incoming.markers.font),
                loc_generic: union(base.markers.loc_generic, incoming.markers.loc_generic),
                loc_specific: union(base.markers.loc_specific, incoming.markers.loc_specific),
                video_extensions: union(
                    base.markers.video_extensions,
                    incoming.markers.video_extensions,
                ),
                font_extensions: union(
                    base.markers.font_extensions,
                    incoming.markers.font_extensions,
                ),
                text_extensions: union(
                    base.markers.text_extensions,
                    incoming.markers.text_extensions,
                ),
            },
        }
    }

    /// How many words a pack contributes across every list - used by the
    /// import summary to report "нових слів N" as the size delta between
    /// the merged and the base pack.
    pub fn word_count(&self) -> usize {
        self.languages
            .iter()
            .map(|entry| entry.level_a.len() + entry.level_b.len() + entry.level_c.len())
            .sum::<usize>()
            + self.industry_words.len()
            + self.keep_default.len()
            + [
                &self.markers.negative,
                &self.markers.overridable_negative,
                &self.markers.audio,
                &self.markers.text,
                &self.markers.video,
                &self.markers.font,
                &self.markers.loc_generic,
                &self.markers.loc_specific,
                &self.markers.video_extensions,
                &self.markers.font_extensions,
                &self.markers.text_extensions,
            ]
            .iter()
            .map(|list| list.len())
            .sum::<usize>()
    }
}

/// Case-insensitive ordered set-union: keeps `base` as-is and appends the
/// items of `incoming` it does not already contain. Comparison mirrors the
/// lowercasing that [`LangData::compile`] applies, so entries that would
/// compile identically never get duplicated.
fn union(base: Vec<String>, incoming: Vec<String>) -> Vec<String> {
    let mut merged = base;
    for item in incoming {
        let lower = item.to_lowercase();
        if !merged
            .iter()
            .any(|existing| existing.to_lowercase() == lower)
        {
            merged.push(item);
        }
    }
    merged
}

// --- Compiled runtime form ------------------------------------------------

/// The compiled, lookup-ready form of a [`LangPack`] that the detection
/// engine consults on every path. Word sets are lowercase.
#[derive(Debug)]
pub struct LangData {
    /// alias (lowercase) -> (canonical key, trust level).
    alias_map: HashMap<String, (&'static str, Level)>,
    industry_words: HashSet<String>,
    keep_default: Vec<String>,
    pub negative: HashSet<String>,
    pub overridable_negative: HashSet<String>,
    pub audio: HashSet<String>,
    pub text: HashSet<String>,
    pub video: HashSet<String>,
    pub font: HashSet<String>,
    pub loc_generic: HashSet<String>,
    pub loc_specific: HashSet<String>,
    pub video_extensions: HashSet<String>,
    pub font_extensions: HashSet<String>,
    pub text_extensions: HashSet<String>,
}

static BUILTIN: LazyLock<Arc<LangData>> =
    LazyLock::new(|| Arc::new(LangData::compile(&LangPack::builtin())));

impl LangData {
    /// The compiled built-in tables (shared, cheap to clone the Arc).
    pub fn builtin() -> Arc<LangData> {
        Arc::clone(&BUILTIN)
    }

    /// Compiles a pack into lookup-ready form.
    pub fn compile(pack: &LangPack) -> LangData {
        let mut alias_map: HashMap<String, (&'static str, Level)> = HashMap::new();
        for entry in &pack.languages {
            let canonical = intern_key(&entry.key.to_lowercase());
            // Every canonical key resolves to itself at Level A, so callers
            // can pass either an alias or a bare canonical key.
            alias_map
                .entry(canonical.to_string())
                .or_insert((canonical, Level::A));
            for (aliases, level) in [
                (&entry.level_a, Level::A),
                (&entry.level_b, Level::B),
                (&entry.level_c, Level::C),
            ] {
                for alias in aliases {
                    alias_map.insert(alias.to_lowercase(), (canonical, level));
                }
            }
        }
        let to_set = |list: &[String]| {
            list.iter()
                .map(|s| s.to_lowercase())
                .collect::<HashSet<_>>()
        };
        LangData {
            alias_map,
            industry_words: to_set(&pack.industry_words),
            keep_default: pack.keep_default.iter().map(|s| s.to_lowercase()).collect(),
            negative: to_set(&pack.markers.negative),
            overridable_negative: to_set(&pack.markers.overridable_negative),
            audio: to_set(&pack.markers.audio),
            text: to_set(&pack.markers.text),
            video: to_set(&pack.markers.video),
            font: to_set(&pack.markers.font),
            loc_generic: to_set(&pack.markers.loc_generic),
            loc_specific: to_set(&pack.markers.loc_specific),
            video_extensions: to_set(&pack.markers.video_extensions),
            font_extensions: to_set(&pack.markers.font_extensions),
            text_extensions: to_set(&pack.markers.text_extensions),
        }
    }

    /// Loads and compiles `l10n_rules.json` text.
    pub fn from_json(json: &str) -> Result<Arc<LangData>> {
        Ok(Arc::new(Self::compile(&LangPack::from_json(json)?)))
    }

    /// The pack's default keep-list (normalized).
    pub fn keep_default(&self) -> &[String] {
        &self.keep_default
    }

    /// Looks up a lowercase token/segment-piece in the dictionary.
    pub fn lookup(&self, text: &str) -> Option<(&'static str, Level)> {
        self.alias_map.get(text).copied()
    }

    /// Generic `<lang>[-_]<REGION>` locale-tag pattern for tags not covered
    /// by a curated literal alias (`es_AR`, `fr_MG`, `zh-TWA`). Accepted
    /// only if the leading part is itself a dictionary-known language; the
    /// trust level returned is the prefix's own level, so this never grants
    /// extra trust — see the original rationale in git history of dict.rs.
    pub fn lookup_locale_tag(&self, text: &str) -> Option<(&'static str, Level)> {
        let (prefix, rest) = text.split_once(['-', '_'])?;
        if !(2..=3).contains(&prefix.len()) || !prefix.chars().all(|c| c.is_ascii_alphabetic()) {
            return None;
        }
        // Region: 2 ASCII letters, or 3 for a 2-letter language prefix
        // (`zh-TWA`) — but not 3+3 (`KOR_INT` is a mission folder, not
        // Korean). Trailing parts (`ja_JP_TRADITIONAL`) are ignored.
        let region = rest.split(['-', '_']).next().unwrap_or(rest);
        let max_region_len = if prefix.len() == 2 { 3 } else { 2 };
        if !(2..=max_region_len).contains(&region.len())
            || !region.chars().all(|c| c.is_ascii_alphabetic())
        {
            return None;
        }
        self.lookup(prefix)
    }

    /// True if a matched Level A alias is localization-industry vocabulary
    /// (region-qualified forms, Steam folder names) rather than a plain
    /// natural-language word — see the 2026-07-16 recalibration.
    pub fn is_industry_alias(&self, matched: &str) -> bool {
        matched.contains('(') || matched.contains('-') || self.industry_words.contains(matched)
    }

    /// Normalizes an arbitrary user-supplied language key (e.g. from a
    /// keep-list) to its canonical form; falls back to the lowercased
    /// input so forward-compatible keys still work as exact-match entries.
    pub fn normalize_lang_key(&self, input: &str) -> String {
        let lower = input.trim().to_lowercase();
        match self.lookup(&lower) {
            Some((canonical, _)) => canonical.to_string(),
            None => lower,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_pack_round_trips_through_json() {
        let pack = LangPack::builtin();
        let json = pack.to_json_pretty().expect("builtin serializes");
        let reparsed = LangPack::from_json(&json).expect("builtin JSON parses");
        assert_eq!(reparsed.version, LANG_PACK_VERSION);
        assert_eq!(reparsed.languages.len(), pack.languages.len());
        assert_eq!(reparsed.markers.negative, pack.markers.negative);
    }

    /// `l10n_rules.json` at the repo root is the canonical file people
    /// download from GitHub and the file `LangPack::builtin()` is embedded
    /// from; this test guards against the two ever silently diverging. If a
    /// future edit hand-formats the file differently (different key order,
    /// stray whitespace, ...) this fails and says exactly how to fix it:
    /// regenerate the file from `LangPack::builtin().to_json_pretty()`.
    #[test]
    fn embedded_file_matches_canonical_serialization() {
        let regenerated = LangPack::builtin()
            .to_json_pretty()
            .expect("builtin serializes");
        assert_eq!(
            regenerated.trim_end(),
            EMBEDDED_L10N_RULES_JSON.trim_end(),
            "l10n_rules.json at the repo root has drifted from LangPack's canonical \
             pretty-printed form — regenerate it with `LangPack::builtin().to_json_pretty()` \
             (or `serde_json::to_string_pretty`) after editing it by hand"
        );
    }

    #[test]
    fn compiled_builtin_matches_dictionary_expectations() {
        let data = LangData::builtin();
        assert_eq!(data.lookup("french"), Some(("fr", Level::A)));
        assert_eq!(data.lookup("fra"), Some(("fr", Level::B)));
        assert_eq!(data.lookup("fr"), Some(("fr", Level::C)));
        assert!(data.lookup("zzz").is_none());
        assert!(data.is_industry_alias("schinese"));
        assert!(!data.is_industry_alias("german"));
        assert_eq!(data.normalize_lang_key("Deutsch"), "de");
        assert!(data.negative.contains("gfx"));
        assert!(data.loc_generic.contains("localization"));
    }

    #[test]
    fn custom_pack_extends_languages_and_markers() {
        let json = r#"{
            "version": 1,
            "languages": [
                {"key": "fr", "level_a": ["french"], "level_c": ["fr"]},
                {"key": "tlh", "level_a": ["klingon"], "level_b": ["tlh"]}
            ],
            "industry_words": [],
            "keep_default": ["uk", "en"],
            "markers": {
                "negative": ["province_names"],
                "overridable_negative": [],
                "audio": ["sound"], "text": ["text"], "video": ["movies"], "font": ["font"],
                "loc_generic": ["loc"], "loc_specific": [],
                "video_extensions": [], "font_extensions": [], "text_extensions": []
            }
        }"#;
        let data = LangData::from_json(json).expect("custom pack compiles");
        assert_eq!(data.lookup("klingon").map(|(k, _)| k), Some("tlh"));
        assert!(data.negative.contains("province_names"));
        assert!(
            data.lookup("german").is_none(),
            "a pack fully replaces the tables it defines"
        );
    }

    #[test]
    fn newer_version_is_refused() {
        let json = r#"{"version": 99, "languages": [], "industry_words": [],
            "keep_default": [], "markers": {"negative": [], "overridable_negative": [],
            "audio": [], "text": [], "video": [], "font": [], "loc_generic": [],
            "loc_specific": [], "video_extensions": [], "font_extensions": [],
            "text_extensions": []}}"#;
        assert!(LangPack::from_json(json).is_err());
    }

    #[test]
    fn merge_unions_aliases_and_appends_new_languages() {
        let base = LangPack::builtin();
        let base_langs = base.languages.len();
        let incoming = LangPack {
            version: 1,
            languages: vec![
                LangPackEntry {
                    // Existing language, new alias + duplicate differing only
                    // in case - only the genuinely new alias may be added.
                    key: "FR".to_string(),
                    level_a: vec!["Français".to_string(), "quebecois".to_string()],
                    level_b: vec![],
                    level_c: vec![],
                },
                LangPackEntry {
                    key: "tlh".to_string(),
                    level_a: vec!["klingon".to_string()],
                    level_b: vec![],
                    level_c: vec![],
                },
            ],
            industry_words: vec!["schinese".to_string()],
            keep_default: vec![],
            markers: MarkerTables {
                negative: vec!["province_names".to_string(), "gfx".to_string()],
                overridable_negative: vec![],
                audio: vec![],
                text: vec![],
                video: vec![],
                font: vec![],
                loc_generic: vec![],
                loc_specific: vec![],
                video_extensions: vec![],
                font_extensions: vec![],
                text_extensions: vec![],
            },
        };

        let merged = LangPack::merge(base.clone(), incoming);

        assert_eq!(merged.version, LANG_PACK_VERSION);
        assert_eq!(merged.languages.len(), base_langs + 1, "only tlh is new");
        let fr = merged
            .languages
            .iter()
            .find(|entry| entry.key.eq_ignore_ascii_case("fr"))
            .expect("fr survives the merge");
        assert!(fr.level_a.iter().any(|alias| alias == "quebecois"));
        assert_eq!(
            fr.level_a
                .iter()
                .filter(|alias| alias.to_lowercase() == "français")
                .count(),
            1,
            "a case-variant duplicate must not be added twice"
        );
        assert!(merged
            .markers
            .negative
            .iter()
            .any(|w| w == "province_names"));
        assert_eq!(
            merged
                .markers
                .negative
                .iter()
                .filter(|w| w.as_str() == "gfx")
                .count(),
            1
        );
        assert_eq!(
            merged.industry_words.len(),
            base.industry_words.len(),
            "schinese is already built in"
        );
        // The merged pack still compiles.
        let data = LangData::compile(&merged);
        assert_eq!(data.lookup("klingon").map(|(k, _)| k), Some("tlh"));
        assert_eq!(data.lookup("quebecois").map(|(k, _)| k), Some("fr"));
    }

    #[test]
    fn merge_over_builtin_changes_nothing_for_the_builtin_pack_itself() {
        let merged = LangPack::merge(LangPack::builtin(), LangPack::builtin());
        assert_eq!(merged.word_count(), LangPack::builtin().word_count());
        assert_eq!(merged.languages.len(), LangPack::builtin().languages.len());
    }

    #[test]
    fn interned_custom_key_is_stable_across_loads() {
        let entry = LangPackEntry {
            key: "xx-custom".to_string(),
            level_a: vec!["customlang".to_string()],
            level_b: vec![],
            level_c: vec![],
        };
        let mut pack = LangPack::builtin();
        pack.languages.push(entry);
        let a = LangData::compile(&pack);
        let b = LangData::compile(&pack);
        let (ka, _) = a.lookup("customlang").unwrap();
        let (kb, _) = b.lookup("customlang").unwrap();
        assert!(std::ptr::eq(ka, kb), "same interned pointer expected");
    }
}
