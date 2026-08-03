//! Why a file was flagged, as data rather than as a finished sentence.
//!
//! The detector lives in `gametrimmer-core`, which has no notion of a user
//! interface language. Building the explanation as a string here would bake
//! the language of whoever wrote the code into every finding - which is
//! exactly the bug this type replaced: an English interface used to show
//! Ukrainian sentences in its row tooltips and CSV export.
//!
//! So the engine reports the *evidence*, and whoever displays it writes the
//! sentence. [`Display`](std::fmt::Display) renders English, the language the
//! rest of this crate speaks, so CLI reports, benchmarks and debug output stay
//! readable without reaching into the app's i18n layer.

use std::fmt;

/// The evidence that made the detector flag a file.
///
/// `dir` fields hold a display form of the directory, already trimmed for
/// reading rather than for path manipulation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LangEvidence {
    /// A recognized language token inside an explicit localization pair, such
    /// as `loc\de\` - the folder itself declares that it splits by language.
    LocPair { token: String },
    /// A recognized language token with an asset-kind marker word nearby
    /// (`soundbanks`, `subtitles`, ...) confirming what kind of file it is.
    TokenWithMarker { token: String, marker: String },
    /// A recognized language token in a language folder, with no marker word
    /// to corroborate it. The weakest evidence the engine acts on.
    BareToken { token: String },
    /// Sibling files in one directory whose names differ only by the language
    /// token (`Voice_english.pak` / `Voice_french.pak` / ...).
    Family { languages: usize, dir: String },
    /// A family found by the language token sitting at the same position in
    /// each sibling's name, rather than by the names being otherwise equal.
    FamilyAtSharedPosition { languages: usize, dir: String },
    /// Sibling subdirectories whose entire names are language tokens
    /// (`en/ de/ fr/ es/`).
    SubfolderFamily { languages: usize, dir: String },
    /// The same, where the language-named subdirectories also share a common
    /// prefix.
    SubfolderFamilyWithPrefix { languages: usize, dir: String },
}

impl LangEvidence {
    /// Whether this is one of the four language-family shapes - the strongest
    /// evidence the engine has. Corpus tooling scores family and non-family
    /// hits separately, and asks through this rather than by matching on the
    /// rendered text.
    pub fn is_family(&self) -> bool {
        matches!(
            self,
            Self::Family { .. }
                | Self::FamilyAtSharedPosition { .. }
                | Self::SubfolderFamily { .. }
                | Self::SubfolderFamilyWithPrefix { .. }
        )
    }
}

/// Why a file was flagged: the evidence, plus the marker word that
/// corroborated it when the evidence itself does not already name one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LangReason {
    pub evidence: LangEvidence,
    /// Set only for family evidence, which establishes *that* a file is
    /// localized without saying what kind of asset it is; the other shapes
    /// either carry their marker inside the evidence or have none.
    pub marker: Option<String>,
}

impl LangReason {
    pub fn new(evidence: LangEvidence) -> Self {
        Self {
            evidence,
            marker: None,
        }
    }

    /// Same reason with `marker` attached, or unchanged when there is none.
    pub fn with_marker(self, marker: Option<&str>) -> Self {
        Self {
            marker: marker.map(str::to_string),
            ..self
        }
    }

    /// See [`LangEvidence::is_family`].
    pub fn is_family(&self) -> bool {
        self.evidence.is_family()
    }
}

impl fmt::Display for LangReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.evidence {
            LangEvidence::LocPair { token } => {
                write!(f, "token '{token}' in an explicit loc pair")?
            }
            LangEvidence::TokenWithMarker { token, marker } => {
                write!(f, "token '{token}' + marker '{marker}'")?
            }
            LangEvidence::BareToken { token } => {
                write!(f, "token '{token}' (language folder with no explicit context)")?
            }
            LangEvidence::Family { languages, dir } => {
                write!(f, "language family of {languages} languages in folder '{dir}'")?
            }
            LangEvidence::FamilyAtSharedPosition { languages, dir } => write!(
                f,
                "language family of {languages} languages in folder '{dir}' (shared token position)"
            )?,
            LangEvidence::SubfolderFamily { languages, dir } => write!(
                f,
                "subfolder language family of {languages} languages in folder '{dir}'"
            )?,
            LangEvidence::SubfolderFamilyWithPrefix { languages, dir } => write!(
                f,
                "subfolder language family with a shared prefix ({languages} languages) in folder '{dir}'"
            )?,
        }
        match &self.marker {
            Some(marker) => write!(f, "; marker '{marker}'"),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_is_appended_to_the_evidence_it_corroborates() {
        let reason = LangReason::new(LangEvidence::Family {
            languages: 4,
            dir: "Voices".to_string(),
        })
        .with_marker(Some("soundbanks"));

        assert_eq!(
            reason.to_string(),
            "language family of 4 languages in folder 'Voices'; marker 'soundbanks'"
        );
    }

    #[test]
    fn without_a_marker_the_sentence_ends_at_the_evidence() {
        let reason = LangReason::new(LangEvidence::BareToken {
            token: "de".to_string(),
        });

        assert_eq!(
            reason.to_string(),
            "token 'de' (language folder with no explicit context)"
        );
    }

    /// The four family shapes are the ones corpus tooling scores separately;
    /// the others must never be counted as family evidence.
    #[test]
    fn only_the_family_shapes_report_themselves_as_family() {
        let dir = || "Data".to_string();
        let family = [
            LangEvidence::Family {
                languages: 3,
                dir: dir(),
            },
            LangEvidence::FamilyAtSharedPosition {
                languages: 3,
                dir: dir(),
            },
            LangEvidence::SubfolderFamily {
                languages: 3,
                dir: dir(),
            },
            LangEvidence::SubfolderFamilyWithPrefix {
                languages: 3,
                dir: dir(),
            },
        ];
        for evidence in family {
            assert!(evidence.is_family(), "{evidence:?} should be family");
        }

        let not_family = [
            LangEvidence::LocPair {
                token: "de".to_string(),
            },
            LangEvidence::TokenWithMarker {
                token: "de".to_string(),
                marker: "voice".to_string(),
            },
            LangEvidence::BareToken {
                token: "de".to_string(),
            },
        ];
        for evidence in not_family {
            assert!(!evidence.is_family(), "{evidence:?} should not be family");
        }
    }
}
