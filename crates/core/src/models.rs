//! Data models for findings, classification actions, and archive trimming.

use serde::{Deserialize, Serialize};

use crate::rules::{Category, RuleProvenance};

/// Information about an embedded or localized stream in a monolithic container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonolithicStreamInfo {
    pub name: String,
    pub language: String,
    pub size: u64,
}

/// The execution action to be taken for a classified finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "action_type", content = "data")]
pub enum FindingAction {
    /// Direct whole-file deletion to recycle bin / permanent delete / backup.
    #[default]
    DirectDelete,
    /// In-place NTFS sparse zeroing of localized streams inside a monolithic container.
    SparseZero {
        format: String,
        languages: Vec<String>,
        stream_count: usize,
        offsets: Vec<(u64, u64)>,
        #[serde(default)]
        streams: Vec<MonolithicStreamInfo>,
        #[serde(default)]
        estimated_savings: u64,
    },
    /// Container repacking to strip trimmable localized chunks.
    Repack {
        format: String,
        languages: Vec<String>,
        #[serde(default)]
        streams: Vec<MonolithicStreamInfo>,
        estimated_savings: u64,
    },
}

impl FindingAction {
    /// Serializes this action to JSON for SQLite storage. Returns None for DirectDelete.
    pub fn to_json(&self) -> Option<String> {
        match self {
            FindingAction::DirectDelete => None,
            _ => serde_json::to_string(self).ok(),
        }
    }

    /// Deserializes a persisted action without ever downgrading corrupt JSON
    /// to a whole-file deletion.
    ///
    /// `NULL` and blank values are the legacy representation of
    /// [`FindingAction::DirectDelete`]. A non-empty value, however, is an
    /// explicit action contract: callers must handle a parse error as a
    /// blocked finding, never as permission to delete the container.
    pub fn from_json(raw: Option<&str>) -> Result<Self, serde_json::Error> {
        match raw {
            Some(s) if !s.trim().is_empty() => serde_json::from_str(s),
            _ => Ok(FindingAction::DirectDelete),
        }
    }

    /// Parses and validates the persisted `findings.category + action`
    /// contract used by destructive callers.
    ///
    /// Ordinary categories written by this build may use the legacy blank
    /// action representation, which means direct deletion. A monolithic row
    /// must carry an explicit archive action. Unknown categories and any
    /// category/action disagreement fail closed.
    pub fn from_persisted_contract(category: &str, raw: Option<&str>) -> Result<Self, String> {
        let legacy_blank = raw.map(str::trim).is_none_or(str::is_empty);
        let action = Self::from_json(raw)
            .map_err(|err| format!("persisted action JSON is invalid: {err}"))?;
        if category == "monolithic_archive" {
            if legacy_blank || !action.is_monolithic_archive() {
                return Err(
                    "monolithic_archive requires an explicit SparseZero or Repack action"
                        .to_string(),
                );
            }
            return Ok(action);
        }
        if !is_known_direct_delete_category(category) {
            return Err(format!("unknown finding category {category:?}"));
        }
        if action != FindingAction::DirectDelete {
            return Err(format!(
                "non-monolithic category {category:?} cannot carry an archive action"
            ));
        }
        Ok(action)
    }

    /// Whether this action modifies a monolithic container (sparse zeroing or repacking).
    pub fn is_monolithic_archive(&self) -> bool {
        matches!(
            self,
            FindingAction::SparseZero { .. } | FindingAction::Repack { .. }
        )
    }

    /// Estimated trimmable savings in bytes.
    pub fn estimated_savings(&self) -> u64 {
        match self {
            FindingAction::DirectDelete => 0,
            FindingAction::SparseZero {
                estimated_savings,
                offsets,
                ..
            } => {
                if *estimated_savings > 0 {
                    *estimated_savings
                } else {
                    offsets
                        .iter()
                        .fold(0, |total, (_, length)| total.saturating_add(*length))
                }
            }
            FindingAction::Repack {
                estimated_savings, ..
            } => *estimated_savings,
        }
    }

    /// Internal localized stream infos if available.
    pub fn streams(&self) -> &[MonolithicStreamInfo] {
        match self {
            FindingAction::DirectDelete => &[],
            FindingAction::SparseZero { streams, .. } => streams,
            FindingAction::Repack { streams, .. } => streams,
        }
    }
}

fn is_known_direct_delete_category(category: &str) -> bool {
    matches!(
        category,
        "redist_folder"
            | "redist_file"
            | "docs_folder"
            | "docs_file"
            | "bonus"
            | "dev_leftovers"
            | "intro"
            | "workshop_orphan"
            | "downloading_staging"
            | "shader_cache"
            | "crash_dump"
            | "diagnostic_logs"
            | "save_bloat"
            | "launcher_web_cache"
            | "mod_manager_downloads"
            | "loc_audio"
            | "loc_text"
            | "loc_video"
            | "loc_font"
            | "loc_graphic"
            | "loc_unknown"
            | "localization"
            | "orphan_folder"
            | "orphan_service"
            | "orphan_unreferenced_file"
    )
}

/// A classification produced by the engine for one file.
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
    ///
    /// `#[serde(default)]`, like `action`: a finding serialized before this
    /// field existed describes a screen, which is what `false` says.
    #[serde(default)]
    pub localized_content: bool,
    #[serde(default)]
    pub action: FindingAction,
}

impl Finding {
    /// Constructs a direct deletion finding with default DirectDelete action.
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
            action: FindingAction::DirectDelete,
        }
    }

    /// Attaches a specific execution action.
    pub fn with_action(mut self, action: FindingAction) -> Self {
        self.action = action;
        self
    }

    /// Whether this finding represents a monolithic container targeted for sparse zeroing or repacking.
    pub fn is_monolithic_archive(&self) -> bool {
        matches!(
            self.action,
            FindingAction::SparseZero { .. } | FindingAction::Repack { .. }
        )
    }

    /// Short human-readable summary of the archive and localized streams if monolithic.
    pub fn archive_summary(&self) -> Option<String> {
        match &self.action {
            FindingAction::DirectDelete => None,
            FindingAction::SparseZero {
                format,
                languages,
                stream_count,
                ..
            } => {
                let langs = if languages.is_empty() {
                    "none".to_string()
                } else {
                    languages.join(", ")
                };
                Some(format!("{format} ({stream_count} streams, langs: {langs})"))
            }
            FindingAction::Repack {
                format,
                languages,
                estimated_savings,
                ..
            } => {
                let langs = if languages.is_empty() {
                    "none".to_string()
                } else {
                    languages.join(", ")
                };
                Some(format!(
                    "{format} (repack, langs: {langs}, savings: {estimated_savings} B)"
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_finding_action_default_is_direct_delete() {
        assert_eq!(FindingAction::default(), FindingAction::DirectDelete);
    }

    #[test]
    fn test_finding_helper_methods() {
        let direct = Finding::new(
            Category::RedistFile,
            "Direct redist",
            90,
            RuleProvenance::Builtin,
        );
        assert!(!direct.is_monolithic_archive());
        assert_eq!(direct.archive_summary(), None);

        let sparse = direct.with_action(FindingAction::SparseZero {
            format: "Audiokinetic Wwise PCK".to_string(),
            languages: vec!["french".to_string(), "german".to_string()],
            stream_count: 42,
            offsets: vec![(1024, 2048)],
            streams: vec![MonolithicStreamInfo {
                name: "voice_fr.wem".to_string(),
                language: "french".to_string(),
                size: 2048,
            }],
            estimated_savings: 2048,
        });
        assert!(sparse.is_monolithic_archive());
        assert_eq!(
            sparse.archive_summary(),
            Some("Audiokinetic Wwise PCK (42 streams, langs: french, german)".to_string())
        );
        assert_eq!(sparse.action.estimated_savings(), 2048);
        assert_eq!(sparse.action.streams().len(), 1);

        let repack = Finding::new(
            Category::DevLeftovers,
            "Repack archive",
            85,
            RuleProvenance::Builtin,
        )
        .with_action(FindingAction::Repack {
            format: "Unreal Engine PAK".to_string(),
            languages: vec!["japanese".to_string()],
            streams: vec![],
            estimated_savings: 1048576,
        });
        assert!(repack.is_monolithic_archive());
        assert_eq!(
            repack.archive_summary(),
            Some("Unreal Engine PAK (repack, langs: japanese, savings: 1048576 B)".to_string())
        );
        assert_eq!(repack.action.estimated_savings(), 1048576);
    }

    #[test]
    fn test_finding_action_json_roundtrip() {
        let action = FindingAction::SparseZero {
            format: "Bink".to_string(),
            languages: vec!["es".to_string()],
            stream_count: 2,
            offsets: vec![(100, 200), (300, 400)],
            streams: vec![MonolithicStreamInfo {
                name: "track1.bk2".to_string(),
                language: "es".to_string(),
                size: 600,
            }],
            estimated_savings: 600,
        };
        let json = action.to_json().expect("serialize");
        let restored = FindingAction::from_json(Some(&json)).expect("parse serialized action");
        assert_eq!(action, restored);

        let direct = FindingAction::DirectDelete;
        assert_eq!(direct.to_json(), None);
        assert_eq!(
            FindingAction::from_json(None).expect("legacy null action"),
            FindingAction::DirectDelete
        );
        assert_eq!(
            FindingAction::from_json(Some("")).expect("legacy blank action"),
            FindingAction::DirectDelete
        );
    }

    #[test]
    fn malformed_action_json_never_becomes_direct_delete() {
        assert!(FindingAction::from_json(Some("{not valid json")).is_err());
        assert!(
            FindingAction::from_json(Some(r#"{"action_type":"UnknownFutureAction"}"#)).is_err()
        );
    }

    #[test]
    fn sparse_estimated_savings_saturates_for_corrupt_persisted_ranges() {
        let action = FindingAction::SparseZero {
            format: "Wwise PCK".to_string(),
            languages: vec![],
            stream_count: 2,
            offsets: vec![(0, u64::MAX), (u64::MAX, 1)],
            streams: vec![],
            estimated_savings: 0,
        };

        assert_eq!(action.estimated_savings(), u64::MAX);
    }

    #[test]
    fn persisted_contract_rejects_monolithic_blank_and_unknown_categories() {
        assert!(FindingAction::from_persisted_contract("monolithic_archive", None).is_err());
        assert!(FindingAction::from_persisted_contract("monolithic_archive", Some(" ")).is_err());
        assert!(FindingAction::from_persisted_contract("future_category", None).is_err());
        assert_eq!(
            FindingAction::from_persisted_contract("docs_file", None)
                .expect("legacy ordinary direct delete"),
            FindingAction::DirectDelete
        );
    }
}
