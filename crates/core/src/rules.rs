//! Regex rule engine for non-essential file categories (redist, docs, bonus, ...).
//! Rules are loaded from an external `rules.json` next to the executable.

use std::collections::HashSet;
use std::path::Path;

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::localized::{LocalizedText, DEFAULT_LANG};

/// Redist and bonus rules only apply when the match occurs within this many
/// path segments from the game root (redist installers and bonus-material
/// folders live at the root or in a first/second-level folder, not deep
/// inside asset or engine trees such as `Launcher\QtQuick\Extras`).
const MAX_SHALLOW_DEPTH: usize = 2;

/// Supported major version of `rules.json`. A file with a greater version was
/// produced by a newer GameTrimmer - refuse it rather than silently misread
/// it, exactly as [`crate::langdetect::LANG_PACK_VERSION`] already does for
/// the localization pack.
///
/// The two packs materialize from the built-ins on first use and are then
/// never overwritten (`worker::ensure_rules_path`), so a hand edit or an
/// imported community pack wins permanently. Without a version in the file
/// there is no way, after the fact, to tell which rule set actually produced
/// a finding - only a diff of the whole file.
pub const RULE_PACK_VERSION: u32 = 1;

pub const MAX_RULE_PACK_BYTES: usize = 1024 * 1024;
pub const MAX_RULES: usize = 2_000;
pub const MAX_REGEX_BYTES: usize = 512;
pub const MAX_RULE_DEPTH: usize = 32;
pub const MAX_EXTENSIONS: usize = 32;
pub const MAX_EXTENSION_BYTES: usize = 16;

/// The repo's rules.json embedded at build time - the seed for the external
/// file the app materializes next to the executable on first use, so users
/// always have the full effective rule set on disk to audit and edit. The
/// scanner never reads this constant directly once the file exists.
pub const BUILTIN_RULES_JSON: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../rules.json"));

/// Category of a non-essential file/folder. Serialized snake_case in rules.json
/// and in the `findings.category` DB column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    RedistFolder,
    RedistFile,
    DocsFolder,
    DocsFile,
    Bonus,
    DevLeftovers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleProvenance {
    #[default]
    Builtin,
    ImportedUntrusted,
}

fn is_builtin_provenance(provenance: &RuleProvenance) -> bool {
    *provenance == RuleProvenance::Builtin
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::RedistFolder => "redist_folder",
            Category::RedistFile => "redist_file",
            Category::DocsFolder => "docs_folder",
            Category::DocsFile => "docs_file",
            Category::Bonus => "bonus",
            Category::DevLeftovers => "dev_leftovers",
        }
    }

    /// Whether rules of this category match against directory segments
    /// (as opposed to the final file name segment).
    fn matches_folder_segments(self) -> bool {
        matches!(
            self,
            Category::RedistFolder | Category::DocsFolder | Category::Bonus
        )
    }

    /// Whether rules of this category are restricted to shallow matches
    /// (see [`MAX_SHALLOW_DEPTH`]).
    fn is_depth_limited(self) -> bool {
        matches!(
            self,
            Category::RedistFolder | Category::RedistFile | Category::Bonus
        )
    }

    /// Precedence when several rules match one file: the lowest rank wins
    /// regardless of confidence, and confidence only breaks ties within one
    /// rank. Ordered by how reliably the category is identified: redists are
    /// exact installer/folder names, dev leftovers are exact file names,
    /// bonus rules need both a telling folder name and a media-typed file
    /// (an artbook PDF inside `Extras\` is bonus material, not standalone
    /// documentation), and docs rules are the most generic (any PDF/RTF
    /// anywhere). Localization is checked after all rule categories - see
    /// `combine_finding` in the app's scan worker.
    fn priority_rank(self) -> u8 {
        match self {
            Category::RedistFolder | Category::RedistFile => 0,
            Category::DevLeftovers => 1,
            Category::Bonus => 2,
            Category::DocsFolder | Category::DocsFile => 3,
        }
    }
}

/// One rule from rules.json. `Serialize` keeps the round trip lossless for
/// the "Export rules"/"Import rules" flow (see
/// `crate::packs`), which rewrites the merged list back to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub category: Category,
    /// Case-insensitive regex. Folder rules match one path segment,
    /// file rules match the file name.
    pub pattern: String,
    /// Human-readable description, e.g. "MS Visual C++ Redist". Either one
    /// string or one per language - see [`LocalizedText`].
    pub desc: LocalizedText,
    /// 0-100.
    pub confidence: u8,
    #[serde(default, skip_serializing_if = "is_builtin_provenance")]
    pub provenance: RuleProvenance,
    /// Optional per-rule override of the category's default depth limit
    /// ([`MAX_SHALLOW_DEPTH`] for redist/bonus rules, unlimited otherwise).
    /// Lets a highly specific pattern (e.g. `vc_redist.*.exe`) match inside
    /// nested vendor folders like `Support\Software\VCRedist\` without
    /// loosening the shallow default that keeps generic patterns away from
    /// deep asset trees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<usize>,
    /// Optional whitelist of file extensions (lowercase, without the dot).
    /// When set, the rule only matches files whose extension is listed -
    /// used by generic folder-name rules (e.g. the bonus "extras" pattern)
    /// to demand content-type evidence (artbooks, music, video) instead of
    /// trusting the folder name alone: `Extras\artbook.pdf` is bonus
    /// material, `QtQuick\Extras\Extras.qml` is program code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
}

/// A whole `rules.json`: the version marker and the rules it carries.
///
/// The version is the only reason this wraps the list instead of being one -
/// see [`RULE_PACK_VERSION`]. It is deliberately not an `Option`: a file
/// without the field is not "version 0", it is a file this build has no way
/// to interpret, and saying so at the parse is better than guessing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulePack {
    pub version: u32,
    pub rules: Vec<Rule>,
}

/// Parses a rules.json without compiling the regexes - the parse used by the
/// import merge (`crate::packs`), where validation happens separately through
/// [`RuleEngine::from_json`]. A pack from a newer GameTrimmer is refused
/// here, before any rule of it is looked at.
pub fn parse_rule_list(json: &str) -> Result<Vec<Rule>> {
    let pack: RulePack = serde_json::from_str(json)?;
    if pack.version > RULE_PACK_VERSION {
        return Err(CoreError::Other(format!(
            "rules.json version {} is newer than supported {RULE_PACK_VERSION} - \
             update GameTrimmer, or use an older rules pack",
            pack.version,
        )));
    }
    Ok(pack.rules)
}

/// Serializes a rule list as a complete pack, stamped with the version this
/// build writes. Every path that produces a rules.json goes through here, so
/// a merged or restored file can never come out unversioned.
pub fn serialize_rule_list(rules: &[Rule]) -> Result<String> {
    serde_json::to_string_pretty(&RulePack {
        version: RULE_PACK_VERSION,
        rules: rules.to_vec(),
    })
    .map_err(CoreError::from)
}

/// A classification produced by the engine for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub category: Category,
    pub rule_desc: String,
    pub confidence: u8,
    pub provenance: RuleProvenance,
}

/// A rule with its pattern already compiled to a case-insensitive [`Regex`].
#[derive(Debug)]
struct CompiledRule {
    category: Category,
    regex: Regex,
    desc: String,
    confidence: u8,
    provenance: RuleProvenance,
    /// The effective depth limit for this rule: the rule's own `max_depth`
    /// if given, otherwise the category default (see [`Rule::max_depth`]).
    max_depth: usize,
    /// Lowercased extension whitelist, if the rule declares one
    /// (see [`Rule::extensions`]).
    extensions: Option<HashSet<String>>,
}

#[derive(Debug)]
pub struct RuleEngine {
    rules: Vec<CompiledRule>,
}

impl RuleEngine {
    /// Builds the engine from rules.json text, describing its findings in
    /// English. This is the form used to *validate* an incoming rule pack,
    /// where no interface language is in play.
    pub fn from_json(json: &str) -> Result<Self> {
        Self::from_json_in(json, DEFAULT_LANG)
    }

    /// Builds the engine from rules.json text, resolving every description
    /// into `lang` once, here, rather than per matched file: `classify` runs
    /// over every file of every game, and the engine is rebuilt whenever the
    /// interface language changes anyway.
    pub fn from_json_in(json: &str, lang: &str) -> Result<Self> {
        if json.len() > MAX_RULE_PACK_BYTES {
            return Err(CoreError::Other(format!(
                "rules.json exceeds the {} byte limit",
                MAX_RULE_PACK_BYTES
            )));
        }
        let raw_rules = parse_rule_list(json)?;
        if raw_rules.len() > MAX_RULES {
            return Err(CoreError::Other(format!(
                "rules.json contains {} rules; the limit is {MAX_RULES}",
                raw_rules.len()
            )));
        }

        let mut rules = Vec::with_capacity(raw_rules.len());
        for (index, rule) in raw_rules.into_iter().enumerate() {
            if rule.pattern.len() > MAX_REGEX_BYTES {
                return Err(CoreError::Other(format!(
                    "rules.json: rule #{index} regex exceeds {MAX_REGEX_BYTES} bytes"
                )));
            }
            if rule.confidence > 100 {
                return Err(CoreError::Other(format!(
                    "rules.json: rule #{index} confidence must be in 0..=100"
                )));
            }
            if rule.max_depth.is_some_and(|depth| depth > MAX_RULE_DEPTH) {
                return Err(CoreError::Other(format!(
                    "rules.json: rule #{index} max_depth exceeds {MAX_RULE_DEPTH}"
                )));
            }
            if let Some(extensions) = &rule.extensions {
                if extensions.len() > MAX_EXTENSIONS {
                    return Err(CoreError::Other(format!(
                        "rules.json: rule #{index} has more than {MAX_EXTENSIONS} extensions"
                    )));
                }
                for extension in extensions {
                    if extension.is_empty()
                        || extension.len() > MAX_EXTENSION_BYTES
                        || !extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
                    {
                        return Err(CoreError::Other(format!(
                            "rules.json: rule #{index} has invalid extension `{extension}`"
                        )));
                    }
                }
            }
            if rule.desc.is_empty() {
                return Err(CoreError::Other(format!(
                    "rules.json: rule #{index} (category {:?}, pattern `{}`) has no description; \
                     a finding the user cannot read is worse than no rule at all",
                    rule.category, rule.pattern
                )));
            }
            let desc = rule.desc.get(lang).to_string();
            let regex = RegexBuilder::new(&rule.pattern)
                .case_insensitive(true)
                .size_limit(MAX_RULE_PACK_BYTES)
                .build()
                .map_err(|err| {
                    CoreError::Other(format!(
                        "rules.json: invalid regex in rule #{index} (category {:?}, desc \"{desc}\", pattern `{}`): {err}",
                        rule.category, rule.pattern
                    ))
                })?;

            let default_depth = if rule.category.is_depth_limited() {
                MAX_SHALLOW_DEPTH
            } else {
                usize::MAX
            };
            rules.push(CompiledRule {
                category: rule.category,
                regex,
                desc,
                confidence: rule.confidence,
                provenance: rule.provenance,
                max_depth: rule.max_depth.unwrap_or(default_depth),
                extensions: rule.extensions.map(|list| {
                    list.into_iter()
                        .map(|ext| ext.to_ascii_lowercase())
                        .collect()
                }),
            });
        }

        Ok(Self { rules })
    }

    /// Loads and builds the engine from a rules.json file, describing its
    /// findings in English.
    pub fn load(path: &Path) -> Result<Self> {
        Self::load_in(path, DEFAULT_LANG)
    }

    /// Loads and builds the engine from a rules.json file, describing its
    /// findings in `lang` - see [`from_json_in`](Self::from_json_in).
    pub fn load_in(path: &Path, lang: &str) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::from_json_in(&text, lang)
    }

    /// Classifies one file by its path relative to the game root
    /// (`\`-separated, as produced by the scanner). When several rules
    /// match the file name or a directory segment, the winner is the one
    /// whose category has the highest precedence (see
    /// [`Category::priority_rank`]); confidence breaks ties within one
    /// category rank.
    pub fn classify(&self, rel_path: &str) -> Option<Finding> {
        let segments: Vec<&str> = rel_path
            .split(['\\', '/'])
            .filter(|segment| !segment.is_empty())
            .collect();

        let (file_name, folder_segments) = match segments.split_last() {
            Some((file_name, folders)) => (*file_name, folders),
            None => return None,
        };
        let file_ext = file_name
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase());

        let mut best: Option<Finding> = None;

        for rule in &self.rules {
            if let Some(allowed) = &rule.extensions {
                let ext_listed = file_ext.as_deref().is_some_and(|ext| allowed.contains(ext));
                if !ext_listed {
                    continue;
                }
            }

            let is_match = if rule.category.matches_folder_segments() {
                folder_segments.iter().enumerate().any(|(i, segment)| {
                    let depth = i + 1;
                    depth <= rule.max_depth && rule.regex.is_match(segment)
                })
            } else {
                let file_depth = folder_segments.len() + 1;
                file_depth <= rule.max_depth && rule.regex.is_match(file_name)
            };

            if !is_match {
                continue;
            }

            let is_better = match &best {
                Some(current) => {
                    let current_rank = current.category.priority_rank();
                    let rank = rule.category.priority_rank();
                    rank < current_rank
                        || (rank == current_rank && rule.confidence > current.confidence)
                }
                None => true,
            };
            if is_better {
                best = Some(Finding {
                    category: rule.category,
                    rule_desc: rule.desc.clone(),
                    confidence: rule.confidence,
                    provenance: rule.provenance,
                });
            }
        }

        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wraps a bare rule array in the pack envelope the format requires, so
    /// a test about rule *semantics* does not restate the wrapper each time.
    /// The envelope itself is covered directly by the tests further down.
    fn pack(rules: &str) -> String {
        format!(r#"{{"version":{RULE_PACK_VERSION},"rules":{rules}}}"#)
    }

    fn sample_json() -> String {
        pack(
            r#"[
            {"category": "redist_folder", "pattern": "^_?commonredist(s)?$", "desc": "Common redist folder", "confidence": 90},
            {"category": "redist_file", "pattern": "^vcredist.*\\.exe$", "desc": "MS Visual C++ Redist", "confidence": 95}
        ]"#,
        )
    }

    #[test]
    fn from_json_parses_minimal_rule_set() {
        let engine = RuleEngine::from_json(&sample_json()).expect("valid rules should parse");
        assert_eq!(engine.rules.len(), 2);
        assert_eq!(engine.rules[0].category, Category::RedistFolder);
        assert_eq!(engine.rules[1].confidence, 95);
    }

    #[test]
    fn classify_finds_common_redist_folder_and_file_with_highest_confidence() {
        let engine = RuleEngine::from_json(&sample_json()).unwrap();

        let finding = engine
            .classify("_CommonRedist\\vcredist_x64.exe")
            .expect("should match both the folder and the file rule");

        // The file rule (95) beats the folder rule (90).
        assert_eq!(finding.confidence, 95);
        assert_eq!(finding.category, Category::RedistFile);
    }

    #[test]
    fn classify_matches_docs_folder_and_file() {
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

        let finding = engine
            .classify("manual\\game_manual.pdf")
            .expect("manual folder + pdf file should be classified as docs");

        // The dedicated readme/eula/manual rule (88) outranks the generic
        // PDF/RTF rule (85) within the same docs category.
        assert_eq!(finding.confidence, 88);
        assert!(matches!(
            finding.category,
            Category::DocsFolder | Category::DocsFile
        ));
    }

    #[test]
    fn classify_matches_legal_text_folders_without_an_extension() {
        // Found by the 2026-07-26 corpus review: these were the only four
        // rows it tagged as documentation that no rule reached. The files
        // themselves are named after the language and carry no extension
        // (`TermsOfService\de`), so only a folder rule with no extension
        // filter can see them.
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

        for path in [
            "Siren\\Content\\Data\\TermsOfService\\de",
            "Siren\\Content\\Data\\PrivacyPolicy\\pt",
        ] {
            let finding = engine
                .classify(path)
                .unwrap_or_else(|| panic!("{path} should be classified as documentation"));
            assert_eq!(finding.category, Category::DocsFolder);
        }
    }

    #[test]
    fn classify_returns_none_for_unremarkable_game_content() {
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

        let finding = engine.classify("base\\sound\\music\\track01.ogg");

        assert_eq!(finding, None);
    }

    #[test]
    fn classify_ignores_redist_pattern_match_beyond_max_depth() {
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

        // "installations" contains the substring "install" but sits four
        // segments deep, well past MAX_REDIST_DEPTH, and is real game content.
        let finding = engine.classify("data\\assets\\textures\\installations\\wall.dds");

        assert_eq!(finding, None);
    }

    #[test]
    fn classify_respects_a_per_rule_max_depth_override() {
        let json = r#"[
            {"category": "redist_file", "pattern": "^vc_?redist.*\\.exe$", "desc": "MS Visual C++ Redist", "confidence": 95, "max_depth": 4}
        ]"#;
        let engine = RuleEngine::from_json(&pack(json)).unwrap();

        // Depth 4 (3 folders + file) is beyond the category default of 2,
        // but within this rule's own override.
        let finding = engine
            .classify(r"Support\Software\VCRedist\vc_redist.x64.exe")
            .expect("max_depth override should allow the deeper match");
        assert_eq!(finding.category, Category::RedistFile);

        // Depth 5 is beyond even the override.
        assert_eq!(
            engine.classify(r"a\b\c\d\vc_redist.x64.exe"),
            None,
            "the override is still a limit, not an unlimited pass"
        );
    }

    #[test]
    fn repo_rules_classify_nested_vc_redist_as_redist_not_docs() {
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

        // Real-world layout (Assassin's Creed Mirage): the redist installer
        // lives 3 folders deep under "Support", which alone matches a
        // low-confidence support/docs folder rule - the specific redist
        // file rule must win by category precedence.
        let finding = engine
            .classify(r"Support\Software\VCRedist\vc_redist.x64.exe")
            .expect("vc_redist installer should be classified");

        assert_eq!(finding.category, Category::RedistFile);
    }

    #[test]
    fn classify_prefers_higher_priority_category_over_higher_confidence() {
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

        // The docs rule for *.pdf (85) is more confident than the bonus
        // folder rule (80), but an artbook inside an extras folder is bonus
        // material - category precedence must beat raw confidence.
        let finding = engine
            .classify(r"Extras\artbook.pdf")
            .expect("an artbook inside Extras should be classified");

        assert_eq!(finding.category, Category::Bonus);
    }

    #[test]
    fn classify_puts_support_folder_content_into_docs_not_bonus() {
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

        // Support/help folders are reference material, so they belong to
        // the docs category ("Documentation and reference material" in the
        // UI), not to bonus - and the folder claims its content wholesale,
        // whatever the file type or per-language subfolder split inside.
        let finding = engine
            .classify(r"Support\ru\voices.pak")
            .expect("support folder content should be classified");

        assert_eq!(finding.category, Category::DocsFolder);
    }

    #[test]
    fn classify_ignores_bonus_folder_deep_inside_program_trees() {
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

        // Real-world layout (XCOM 2 launcher): `Extras` here is a Qt QML
        // module three segments deep, not a bonus-content folder. The
        // shallow depth limit must keep the bonus rule away from it even
        // for media-typed files.
        assert_eq!(engine.classify(r"Launcher\QtQuick\Extras\poster.png"), None);
    }

    #[test]
    fn classify_bonus_folder_requires_media_or_document_content() {
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

        let media = engine
            .classify(r"Extras\track01.mp3")
            .expect("music inside Extras should be bonus material");
        assert_eq!(media.category, Category::Bonus);

        assert_eq!(
            engine.classify(r"Extras\plugin.dll"),
            None,
            "a program file is not bonus material even inside an extras folder"
        );
    }

    #[test]
    fn from_json_parses_extension_whitelist_case_insensitively() {
        let json = r#"[
            {"category": "bonus", "pattern": "^extras$", "desc": "Bonus", "confidence": 80, "extensions": ["PDF"]}
        ]"#;
        let engine = RuleEngine::from_json(&pack(json)).unwrap();

        assert!(engine.classify(r"Extras\Artbook.PDF").is_some());
        assert_eq!(engine.classify(r"Extras\readme.txt"), None);
        assert_eq!(
            engine.classify(r"Extras\noextension"),
            None,
            "a file without an extension never passes a whitelist"
        );
    }

    /// The shipped rules were written in Ukrainian, and an English
    /// interface showed them untranslated in its row tooltips and CSV export.
    /// A new rule added Ukrainian-only would bring the bug straight back, so
    /// the data file is checked rather than only the machinery that reads it.
    #[test]
    fn every_builtin_rule_describes_itself_in_english() {
        let rules = parse_rule_list(BUILTIN_RULES_JSON).expect("builtin rules parse");
        assert!(!rules.is_empty(), "the builtin rule list must not be empty");

        for rule in &rules {
            let english = rule.desc.get(DEFAULT_LANG);
            assert!(
                !english.trim().is_empty(),
                "rule `{}` has no English description",
                rule.pattern
            );
            assert!(
                !english
                    .chars()
                    .any(|ch| ('\u{0400}'..='\u{04FF}').contains(&ch)),
                "rule `{}` describes itself in Cyrillic where English is expected: {english:?}",
                rule.pattern
            );
        }
    }

    /// The Ukrainian side must survive the translation, or the fix would have
    /// been "delete the Ukrainian" rather than "add the English".
    #[test]
    fn the_builtin_rules_keep_their_ukrainian_descriptions() {
        let rules = parse_rule_list(BUILTIN_RULES_JSON).expect("builtin rules parse");
        let translated = rules
            .iter()
            .filter(|rule| rule.desc.get("uk") != rule.desc.get(DEFAULT_LANG))
            .count();

        // Every rule but the one whose description is a bare product name
        // ("AMD Dual-Core Optimizer"), which is deliberately language-neutral
        // and stored as a single string.
        assert_eq!(
            translated,
            rules.len() - 1,
            "expected all but the language-neutral rule to carry a Ukrainian variant"
        );
    }

    #[test]
    fn from_json_reports_which_rule_has_the_invalid_regex() {
        let bad_json = r#"[
            {"category": "bonus", "pattern": "^(unterminated", "desc": "Broken rule", "confidence": 80}
        ]"#;

        let err = RuleEngine::from_json(&pack(bad_json))
            .expect_err("unterminated group should fail to compile");
        let message = err.to_string();
        assert!(
            message.contains("rule #0"),
            "error should name the offending rule: {message}"
        );
        assert!(
            message.contains("Broken rule"),
            "error should include the rule's desc: {message}"
        );
    }

    #[test]
    fn builtin_rules_json_parses_and_compiles() {
        let engine = RuleEngine::from_json(BUILTIN_RULES_JSON)
            .expect("embedded builtin rules must always compile");
        assert!(!engine.rules.is_empty());
    }

    /// The point of the version: a pack written by a newer build is refused
    /// with a message that says so, instead of being read as far as this
    /// build happens to understand it.
    #[test]
    fn a_pack_from_a_newer_build_is_refused() {
        let newer = format!(
            r#"{{"version":{},"rules":[]}}"#,
            RULE_PACK_VERSION.saturating_add(1)
        );

        let err = parse_rule_list(&newer).expect_err("a newer pack must not be read");

        let message = err.to_string();
        assert!(message.contains("newer than supported"), "{message}");
        assert!(
            message.contains(&RULE_PACK_VERSION.to_string()),
            "the message should name the supported version: {message}",
        );
    }

    /// The other half: a file with no version is not "version 0" to be read
    /// leniently - nothing about it says which rules format it holds.
    #[test]
    fn a_pack_without_a_version_is_refused_rather_than_assumed() {
        let bare_array = r#"[{"category":"bonus","pattern":"x","desc":"x","confidence":80}]"#;

        assert!(parse_rule_list(bare_array).is_err());
        assert!(parse_rule_list(r#"{"rules":[]}"#).is_err());
    }

    /// Every path that writes a rules.json goes through `serialize_rule_list`
    /// (the import merge, the restore-to-builtin), so an unversioned file can
    /// never be produced by the app itself.
    #[test]
    fn a_serialized_pack_carries_the_version_and_reparses() {
        let rules = parse_rule_list(BUILTIN_RULES_JSON).expect("builtin rules parse");

        let json = serialize_rule_list(&rules).expect("rules serialize");

        assert!(
            json.contains(&format!("\"version\": {RULE_PACK_VERSION}")),
            "the written pack must declare its version: {}",
            &json[..json.len().min(120)],
        );
        assert_eq!(
            parse_rule_list(&json).expect("written pack reparses").len(),
            rules.len(),
        );
    }

    /// The embedded pack is the seed for every user's file; if it fell behind
    /// the constant, every fresh install would materialize a file this build
    /// then refuses or misreads.
    #[test]
    fn the_builtin_pack_declares_the_supported_version() {
        let pack: RulePack =
            serde_json::from_str(BUILTIN_RULES_JSON).expect("builtin rules parse as a pack");

        assert_eq!(pack.version, RULE_PACK_VERSION);
    }

    #[test]
    fn strict_schema_and_numeric_bounds_are_enforced() {
        let unknown =
            r#"[{"category":"bonus","pattern":"x","desc":"x","confidence":80,"surprise":true}]"#;
        assert!(RuleEngine::from_json(&pack(unknown)).is_err());

        let confidence = r#"[{"category":"bonus","pattern":"x","desc":"x","confidence":101}]"#;
        assert!(RuleEngine::from_json(&pack(confidence)).is_err());

        let depth =
            r#"[{"category":"bonus","pattern":"x","desc":"x","confidence":80,"max_depth":33}]"#;
        assert!(RuleEngine::from_json(&pack(depth)).is_err());

        let extension = r#"[{"category":"bonus","pattern":"x","desc":"x","confidence":80,"extensions":["thisextensionistoolong"]}]"#;
        assert!(RuleEngine::from_json(&pack(extension)).is_err());
    }

    #[test]
    fn regex_rule_and_file_count_limits_are_enforced() {
        let pattern = "x".repeat(MAX_REGEX_BYTES + 1);
        let oversized_regex =
            format!(r#"[{{"category":"bonus","pattern":"{pattern}","desc":"x","confidence":80}}]"#);
        assert!(RuleEngine::from_json(&pack(&oversized_regex)).is_err());

        let rule = r#"{"category":"bonus","pattern":"x","desc":"x","confidence":80}"#;
        let too_many = format!(
            "[{}]",
            std::iter::repeat_n(rule, MAX_RULES + 1)
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(RuleEngine::from_json(&pack(&too_many)).is_err());

        let oversized_file = " ".repeat(MAX_RULE_PACK_BYTES + 1);
        assert!(RuleEngine::from_json(&oversized_file).is_err());
    }

    #[test]
    fn load_reads_and_compiles_the_real_repo_rules_json() {
        let engine = RuleEngine::load(&default_rules_path())
            .expect("repo rules.json should parse and compile");
        assert!(!engine.rules.is_empty());
    }

    fn default_rules_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("rules.json")
    }
}
