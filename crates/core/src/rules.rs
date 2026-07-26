//! Regex rule engine for non-essential file categories (redist, docs, bonus, ...).
//! Rules are loaded from an external `rules.json` next to the executable.

use std::collections::HashSet;
use std::path::Path;

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// Redist and bonus rules only apply when the match occurs within this many
/// path segments from the game root (redist installers and bonus-material
/// folders live at the root or in a first/second-level folder, not deep
/// inside asset or engine trees such as `Launcher\QtQuick\Extras`).
const MAX_SHALLOW_DEPTH: usize = 2;

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

impl Category {
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
/// the "Експортувати правила"/"Імпортувати правила" flow (see
/// `crate::packs`), which rewrites the merged list back to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub category: Category,
    /// Case-insensitive regex. Folder rules match one path segment,
    /// file rules match the file name.
    pub pattern: String,
    /// Human-readable description, e.g. "MS Visual C++ Redist".
    pub desc: String,
    /// 0-100.
    pub confidence: u8,
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

/// Parses the raw rule list of a rules.json without compiling the regexes -
/// the parse used by the import merge (`crate::packs`), where validation
/// happens separately through [`RuleEngine::from_json`].
pub fn parse_rule_list(json: &str) -> Result<Vec<Rule>> {
    serde_json::from_str(json).map_err(CoreError::from)
}

/// A classification produced by the engine for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub category: Category,
    pub rule_desc: String,
    pub confidence: u8,
}

/// A rule with its pattern already compiled to a case-insensitive [`Regex`].
#[derive(Debug)]
struct CompiledRule {
    category: Category,
    regex: Regex,
    desc: String,
    confidence: u8,
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
    /// Builds the engine from rules.json text.
    pub fn from_json(json: &str) -> Result<Self> {
        let raw_rules = parse_rule_list(json)?;

        let mut rules = Vec::with_capacity(raw_rules.len());
        for (index, rule) in raw_rules.into_iter().enumerate() {
            let regex = RegexBuilder::new(&rule.pattern)
                .case_insensitive(true)
                .build()
                .map_err(|err| {
                    CoreError::Other(format!(
                        "rules.json: invalid regex in rule #{index} (category {:?}, desc \"{}\", pattern `{}`): {err}",
                        rule.category, rule.desc, rule.pattern
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
                desc: rule.desc,
                confidence: rule.confidence,
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

    /// Loads and builds the engine from a rules.json file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::from_json(&text)
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
                });
            }
        }

        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_json() -> &'static str {
        r#"[
            {"category": "redist_folder", "pattern": "^_?commonredist(s)?$", "desc": "Common redist folder", "confidence": 90},
            {"category": "redist_file", "pattern": "^vcredist.*\\.exe$", "desc": "MS Visual C++ Redist", "confidence": 95}
        ]"#
    }

    #[test]
    fn from_json_parses_minimal_rule_set() {
        let engine = RuleEngine::from_json(sample_json()).expect("valid rules should parse");
        assert_eq!(engine.rules.len(), 2);
        assert_eq!(engine.rules[0].category, Category::RedistFolder);
        assert_eq!(engine.rules[1].confidence, 95);
    }

    #[test]
    fn classify_finds_common_redist_folder_and_file_with_highest_confidence() {
        let engine = RuleEngine::from_json(sample_json()).unwrap();

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
        let engine = RuleEngine::from_json(json).unwrap();

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
        // the docs category ("Документація і довідкові матеріали" in the
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
        let engine = RuleEngine::from_json(json).unwrap();

        assert!(engine.classify(r"Extras\Artbook.PDF").is_some());
        assert_eq!(engine.classify(r"Extras\readme.txt"), None);
        assert_eq!(
            engine.classify(r"Extras\noextension"),
            None,
            "a file without an extension never passes a whitelist"
        );
    }

    #[test]
    fn from_json_reports_which_rule_has_the_invalid_regex() {
        let bad_json = r#"[
            {"category": "bonus", "pattern": "^(unterminated", "desc": "Broken rule", "confidence": 80}
        ]"#;

        let err =
            RuleEngine::from_json(bad_json).expect_err("unterminated group should fail to compile");
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
