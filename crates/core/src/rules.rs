//! Regex rule engine for non-essential file categories (redist, docs, bonus, ...).
//! Rules are loaded from an external `rules.json` next to the executable.

use std::path::Path;

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// Redist rules only apply when the match occurs within this many path
/// segments from the game root (redist installers live at the root or in a
/// first/second-level folder, not deep inside asset trees).
const MAX_REDIST_DEPTH: usize = 2;

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
    /// (see [`MAX_REDIST_DEPTH`]).
    fn is_depth_limited(self) -> bool {
        matches!(self, Category::RedistFolder | Category::RedistFile)
    }
}

/// One rule from rules.json.
#[derive(Debug, Clone, Deserialize)]
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
    /// ([`MAX_REDIST_DEPTH`] for redist rules, unlimited otherwise). Lets a
    /// highly specific pattern (e.g. `vc_redist.*.exe`) match inside nested
    /// vendor folders like `Support\Software\VCRedist\` without loosening
    /// the shallow default that keeps generic patterns away from deep asset
    /// trees.
    #[serde(default)]
    pub max_depth: Option<usize>,
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
}

#[derive(Debug)]
pub struct RuleEngine {
    rules: Vec<CompiledRule>,
}

impl RuleEngine {
    /// Builds the engine from rules.json text.
    pub fn from_json(json: &str) -> Result<Self> {
        let raw_rules: Vec<Rule> = serde_json::from_str(json)?;

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
                MAX_REDIST_DEPTH
            } else {
                usize::MAX
            };
            rules.push(CompiledRule {
                category: rule.category,
                regex,
                desc: rule.desc,
                confidence: rule.confidence,
                max_depth: rule.max_depth.unwrap_or(default_depth),
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
    /// (`\`-separated, as produced by the scanner). Returns the
    /// highest-confidence finding, if any rule matches the file name or
    /// any directory segment of the path.
    pub fn classify(&self, rel_path: &str) -> Option<Finding> {
        let segments: Vec<&str> = rel_path
            .split(['\\', '/'])
            .filter(|segment| !segment.is_empty())
            .collect();

        let (file_name, folder_segments) = match segments.split_last() {
            Some((file_name, folders)) => (*file_name, folders),
            None => return None,
        };

        let mut best: Option<Finding> = None;

        for rule in &self.rules {
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
                Some(current) => rule.confidence > current.confidence,
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

        assert_eq!(finding.confidence, 85);
        assert!(matches!(
            finding.category,
            Category::DocsFolder | Category::DocsFile
        ));
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
    fn repo_rules_classify_nested_vc_redist_as_redist_not_bonus() {
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

        // Real-world layout (Assassin's Creed Mirage): the redist installer
        // lives 3 folders deep under "Support", which alone matches a
        // low-confidence bonus rule - the specific redist file rule must win.
        let finding = engine
            .classify(r"Support\Software\VCRedist\vc_redist.x64.exe")
            .expect("vc_redist installer should be classified");

        assert_eq!(finding.category, Category::RedistFile);
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
