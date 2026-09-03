//! Data models for findings.

use serde::{Deserialize, Serialize};

use crate::rules::{Category, RuleProvenance};

/// A classification produced by the engine for one file. Always a whole-file
/// deletion candidate - GameTrimmer no longer carries an in-place archive
/// trimmer, so there is nothing left for a finding to distinguish itself
/// from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub category: Category,
    pub rule_desc: String,
    pub confidence: u8,
    pub provenance: RuleProvenance,
    /// Whether the rule that produced this finding says its subject is
    /// content in the player's language rather than a screen the game plays
    /// on the way in - see [`crate::rules::Rule::localized_content`] and
    /// [`crate::worker::keep_language_vetoes_rule`], which is the only thing
    /// that reads it.
    #[serde(default)]
    pub localized_content: bool,
}

impl Finding {
    /// Constructs a whole-file deletion finding.
    pub fn new(
        category: Category,
        rule_desc: impl Into<String>,
        confidence: u8,
        provenance: RuleProvenance,
    ) -> Self {
        Self {
            category,
            rule_desc: rule_desc.into(),
            confidence,
            provenance,
            // A caller building a finding by hand is not the rule pack, and
            // only the pack can say a rule names content - see
            // `rules::Rule::localized_content`.
            localized_content: false,
        }
    }
}
