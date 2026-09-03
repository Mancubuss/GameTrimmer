//! Turning a stored finding description into the one the window shows.
//!
//! Everything the database holds is English - see [`crate::logger`] for the
//! same rule applied to the log. That is not a style preference: a bug
//! report carries the database's text verbatim into the diagnostic bundle
//! (`core::bundle::sections::findings` selects `findings.rule_id` straight
//! into `findings.json`), so a Ukrainian install used to produce a report
//! that could only be read by someone who reads Ukrainian.
//!
//! Storing English costs nothing to write and everything to *display*,
//! because the tree still has to speak the user's language. This module is
//! the one place that translation happens, and it treats the three kinds of
//! description differently on purpose:
//!
//! - **Rule descriptions are curated.** 32 of the 33 built-in rules carry a
//!   hand-written Ukrainian variant, and `rules.json` documents per-language
//!   `desc` as a feature. Those are looked back up and shown translated.
//! - **Orphan reasons are curated** too, and cheaper still: the finding
//!   stores its `OrphanKind`, so the sentence is re-derived from the enum
//!   rather than looked up at all.
//! - **Localization reasons are generated.** `LangReason`'s `Display` builds
//!   a sentence out of the evidence it found ("token 'de' in an explicit
//!   loc pair"); there is no curated text to lose, and reversing a generated
//!   sentence back into its structure would mean persisting the structure.
//!   Those stay English in the window as well as in the file.

use std::collections::HashMap;

use crate::i18n::{self, Lang};
use crate::model::FindingSource;

/// The lookup a scan or a load needs to render stored descriptions.
///
/// Built once per job rather than per row: a scan produces hundreds of
/// thousands of findings, and re-reading the rule pack for each would turn
/// a display concern into a disk one.
#[derive(Debug)]
pub(crate) struct Descriptions {
    lang: Lang,
    /// English description -> the same rule's description in `lang`.
    ///
    /// Keyed by the English text rather than by a rule id because rules do
    /// not have ids - they are identified by `(category, pattern)`, neither
    /// of which the finding row keeps. The English description is what the
    /// row *does* keep, and it is stable for as long as the rule pack is.
    ///
    /// ponytail: two rules sharing one English description collapse to one
    /// entry, so the second one's translation is lost. Give rules an `id`
    /// field (the pack is versioned now, so adding one is cheap) if that
    /// ever stops being hypothetical.
    rules: HashMap<String, String>,
}

impl Descriptions {
    /// Reads the effective rule pack and indexes it for `lang`.
    ///
    /// An English interface needs no index at all - the stored text is
    /// already what it would render - so that case skips the file entirely.
    /// A pack that cannot be read or parsed also yields an empty index
    /// rather than an error: an untranslated description is a far better
    /// outcome than a scan that refuses to show its results, and the scan
    /// worker already reports a broken pack through its own path.
    pub(crate) fn load(lang: Lang) -> Self {
        if lang == Lang::En {
            return Self {
                lang,
                rules: HashMap::new(),
            };
        }

        let rules = super::ensure_rules_path()
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| gametrimmer_core::rules::parse_rule_list(&text).ok())
            .map(|rules| {
                rules
                    .iter()
                    .map(|rule| {
                        (
                            rule.desc
                                .get(gametrimmer_core::localized::DEFAULT_LANG)
                                .to_string(),
                            rule.desc.get(lang.as_str()).to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        Self { lang, rules }
    }

    /// The description to show for a finding stored as `stored`.
    ///
    /// Falls back to `stored` whenever there is nothing better - an
    /// unreadable pack, a rule deleted since the scan that found it, a
    /// generated reason. English in the window is a small loss; an empty
    /// description is a real one.
    pub(crate) fn display(&self, source: FindingSource, stored: &str) -> String {
        match source {
            FindingSource::Rule(_) => self
                .rules
                .get(stored)
                .cloned()
                .unwrap_or_else(|| stored.to_string()),
            FindingSource::Orphan(kind) => i18n::orphan_reason(self.lang, kind),
            FindingSource::Loc(_) => stored.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gametrimmer_core::orphans::OrphanKind;
    use gametrimmer_core::rules::Category;

    fn indexed(pairs: &[(&str, &str)], lang: Lang) -> Descriptions {
        Descriptions {
            lang,
            rules: pairs
                .iter()
                .map(|(en, uk)| (en.to_string(), uk.to_string()))
                .collect(),
        }
    }

    /// The curated half: a rule's hand-written translation has to survive
    /// the trip through a database that only speaks English.
    #[test]
    fn a_rule_description_is_translated_back_for_the_window() {
        let descriptions = indexed(
            &[("Common redist folder", "Common redistributables folder")],
            Lang::En,
        );

        assert_eq!(
            descriptions.display(
                FindingSource::Rule(Category::RedistFolder),
                "Common redist folder"
            ),
            "Common redistributables folder",
        );
    }

    /// Orphan reasons are re-derived from the kind the row already carries,
    /// so they never depend on the stored text matching anything.
    #[test]
    fn an_orphan_reason_is_rebuilt_from_its_kind() {
        let descriptions = indexed(&[], Lang::En);

        let shown = descriptions.display(
            FindingSource::Orphan(OrphanKind::ServiceFolder),
            "aborted or partial downloads",
        );

        assert_eq!(
            shown,
            i18n::orphan_reason(Lang::En, OrphanKind::ServiceFolder)
        );
        assert!(shown.contains("Launcher download/cache"), "{shown}");
    }

    /// The generated half stays as stored - there is no curated translation
    /// for it to be restored to.
    #[test]
    fn a_localization_reason_stays_as_it_was_stored() {
        let descriptions = indexed(&[("anything", "something else")], Lang::En);
        let stored = "token 'de' in an explicit loc pair";

        assert_eq!(
            descriptions.display(
                FindingSource::Loc(gametrimmer_core::langdetect::LangKind::Audio),
                stored
            ),
            stored,
        );
    }

    /// A rule dropped from the pack since the scan that found it, or a pack
    /// that will not parse: the window shows English rather than nothing.
    #[test]
    fn an_unknown_description_falls_back_to_what_was_stored() {
        let descriptions = indexed(&[], Lang::En);

        assert_eq!(
            descriptions.display(FindingSource::Rule(Category::Bonus), "Retired rule"),
            "Retired rule",
        );
    }

    /// An English interface reads the stored text directly, so building the
    /// index would be pure cost - assert the skip rather than trusting the
    /// comment above it.
    #[test]
    fn an_english_interface_builds_no_index() {
        assert!(Descriptions::load(Lang::En).rules.is_empty());
    }

    /// The bug this design exists to prevent, and the one the first
    /// implementation shipped: descriptions were resolved once and written
    /// back over `FindingRow::rule_desc`, which destroyed the English key -
    /// switching the interface language afterwards left the first answer on
    /// screen with no way back.
    ///
    /// Resolving from the stored text every time is what makes the switch
    /// work, so the property to hold is that resolving is *repeatable* and
    /// *derived*: an orphan reason is rebuilt from `kind`, not read back from
    /// `stored`, and asking twice must not inherit a stale first answer.
    #[test]
    fn the_same_stored_row_answers_in_whichever_language_asks() {
        let stored = "this exact text must not survive the round trip";
        let source = FindingSource::Orphan(OrphanKind::ServiceFolder);

        let first = indexed(&[], Lang::En).display(source, stored);
        let second = indexed(&[], Lang::En).display(source, stored);

        assert_eq!(
            first,
            i18n::orphan_reason(Lang::En, OrphanKind::ServiceFolder),
            "the reason must be rebuilt from the kind, not read back from `stored`",
        );
        assert_eq!(
            first, second,
            "the same stored row must answer consistently"
        );
    }

    /// GT-129's actual guarantee. Fixing the three known sites is worth
    /// little on its own - the next description added to a worker would
    /// reach the database in whatever language the interface happened to be
    /// in, and nothing would fail.
    ///
    /// So the rule is checked at the source: inside `worker/`, the two
    /// description renderers may only ever be called with `Lang::En`, and
    /// the rule engine may only be built in its default (English) language.
    /// Everything a worker produces goes to the database or to the log, and
    /// both of those are English-only.
    #[test]
    fn no_worker_renders_a_stored_description_in_the_interface_language() {
        // `(needle, what to say when it is found)`. Matched on the call
        // itself, so a mention in a comment or a doc link does not trip it.
        const FORBIDDEN: &[(&str, &str)] = &[
            (
                "orphan_reason(lang",
                "orphan reasons are stored, so they must be rendered with Lang::En \
                 and translated by Descriptions::display",
            ),
            (
                "lang_reason(ui_lang",
                "localization reasons are stored, so they must be rendered with Lang::En",
            ),
            (
                "lang_reason(lang",
                "localization reasons are stored, so they must be rendered with Lang::En",
            ),
            (
                "RuleEngine::load_in(",
                "the scan engine resolves the descriptions that get stored, so it must \
                 be built with RuleEngine::load (English)",
            ),
            (
                "RuleEngine::from_json_in(",
                "the scan engine resolves the descriptions that get stored, so it must \
                 be built with RuleEngine::from_json (English)",
            ),
        ];

        let worker_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/worker");
        let mut offenders = Vec::new();
        let mut pending = vec![worker_dir];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).expect("read the worker source directory") {
                let path = entry.expect("read a worker source entry").path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if !path.extension().is_some_and(|ext| ext == "rs")
                    // This file states the needles; it cannot also be a
                    // haystack for them.
                    || path.file_name().is_some_and(|name| name == "descriptions.rs")
                {
                    continue;
                }
                let source = std::fs::read_to_string(&path).expect("read a worker source file");
                for (number, line) in source.lines().enumerate() {
                    let code = line.trim_start();
                    if code.starts_with("//") {
                        continue;
                    }
                    for (needle, why) in FORBIDDEN {
                        if code.contains(needle) {
                            offenders.push(format!(
                                "{}:{}: {needle} - {why}",
                                path.display(),
                                number + 1,
                            ));
                        }
                    }
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "the database and the log are English-only:\n{}",
            offenders.join("\n"),
        );
    }
}
