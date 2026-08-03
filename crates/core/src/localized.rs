//! Human-facing text that a data file may supply in more than one language.
//!
//! Rule descriptions are prose written by people - the project's own authors
//! and, through rule packs, the community - so unlike the detector's reasons
//! they cannot be modelled as an enum the program renders. They travel with
//! the data instead, and a pack may give one string or one per language:
//!
//! ```json
//! "desc": "MS Visual C++ Redist"
//! "desc": { "en": "MS Visual C++ Redist", "uk": "Тека MS Visual C++ Runtime" }
//! ```
//!
//! The plain form stays valid forever. A pack written before this existed
//! keeps working untouched, and its single string is shown whatever the
//! interface language is: the file does not say which language it is in, and
//! guessing would be worse than showing it exactly as its author wrote it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The language every localized value falls back to, and the one a rule pack
/// should always carry.
pub const DEFAULT_LANG: &str = "en";

/// One human-readable string, optionally per language.
///
/// `untagged` is what keeps both JSON shapes readable by one type, and it
/// round-trips: a plain string is written back as a plain string, so exporting
/// a pack that was imported unchanged produces the same file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LocalizedText {
    /// One string, language unstated - shown as-is in every interface language.
    Plain(String),
    /// One string per language tag (`"en"`, `"uk"`, ...).
    PerLanguage(BTreeMap<String, String>),
}

impl LocalizedText {
    /// The text for `lang`, falling back to [`DEFAULT_LANG`] and then to
    /// whichever tag sorts first.
    ///
    /// The last fallback matters for a community pack that ships, say, only
    /// German: showing German is worse than showing English but far better
    /// than showing an empty tooltip, and it tells the reader immediately
    /// which pack needs a translation.
    pub fn get(&self, lang: &str) -> &str {
        match self {
            Self::Plain(text) => text,
            Self::PerLanguage(by_lang) => by_lang
                .get(lang)
                .or_else(|| by_lang.get(DEFAULT_LANG))
                .or_else(|| by_lang.values().next())
                .map(String::as_str)
                .unwrap_or_default(),
        }
    }

    /// Whether this carries no usable text at all - an empty string or an
    /// empty object. Callers that validate incoming packs reject those rather
    /// than letting a blank description reach a tooltip.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Plain(text) => text.trim().is_empty(),
            Self::PerLanguage(by_lang) => {
                by_lang.is_empty() || by_lang.values().all(|text| text.trim().is_empty())
            }
        }
    }
}

impl From<&str> for LocalizedText {
    fn from(text: &str) -> Self {
        Self::Plain(text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> LocalizedText {
        serde_json::from_str(json).expect("parse localized text")
    }

    /// The shape every rule pack written before this type existed uses.
    #[test]
    fn a_plain_string_is_shown_in_every_language() {
        let text = parse(r#""MS Visual C++ Redist""#);

        assert_eq!(text.get("en"), "MS Visual C++ Redist");
        assert_eq!(text.get("uk"), "MS Visual C++ Redist");
    }

    #[test]
    fn a_per_language_object_answers_in_the_language_asked_for() {
        let text = parse(r#"{"en": "Redist folder", "uk": "Тека редистрибутивів"}"#);

        assert_eq!(text.get("en"), "Redist folder");
        assert_eq!(text.get("uk"), "Тека редистрибутивів");
    }

    #[test]
    fn an_unknown_language_falls_back_to_english() {
        let text = parse(r#"{"en": "Redist folder", "uk": "Тека редистрибутивів"}"#);

        assert_eq!(text.get("de"), "Redist folder");
    }

    /// A pack with no English at all still says something rather than
    /// rendering an empty tooltip.
    #[test]
    fn without_english_the_first_tag_answers() {
        let text = parse(r#"{"fr": "Dossier", "de": "Ordner"}"#);

        assert_eq!(
            text.get("en"),
            "Ordner",
            "BTreeMap orders tags, so 'de' wins"
        );
        assert_eq!(text.get("fr"), "Dossier");
    }

    #[test]
    fn blank_text_is_recognized_in_both_shapes() {
        assert!(parse(r#""""#).is_empty());
        assert!(parse(r#""   ""#).is_empty());
        assert!(parse("{}").is_empty());
        assert!(parse(r#"{"en": " "}"#).is_empty());
        assert!(!parse(r#"{"en": "Redist folder"}"#).is_empty());
    }

    /// Export writes back what import read, so a pack round-trips through the
    /// app without its descriptions changing shape.
    #[test]
    fn both_shapes_survive_a_round_trip() {
        for json in [
            r#""MS Visual C++ Redist""#,
            r#"{"en":"Redist folder","uk":"Тека редистрибутивів"}"#,
        ] {
            let written = serde_json::to_string(&parse(json)).expect("serialize");
            assert_eq!(written, json);
        }
    }
}
