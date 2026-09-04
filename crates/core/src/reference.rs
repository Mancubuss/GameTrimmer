//! What an external catalogue knows about one *named* game, as a table.
//!
//! # Why this is not a rule pack
//!
//! A rule is a pattern: `^(.*[_. -])?logos?.*\.bik$` is a guess someone wrote
//! because startup videos tend to be named that way, and for a game nobody
//! has catalogued it is the only answer available. A catalogue entry is not a
//! guess - PCGamingWiki names this game's intro videos one by one, so there
//! is nothing left to generalize over.
//!
//! The entries used to be carried as rules anyway, one regex per game
//! (`"pattern": "^(intro_ea\\.bik|legal\\.bik)$"`, `"origin": "reference"`).
//! That shape was measurably the wrong one, and the measurement is the reason
//! this module exists: **all 935 of those patterns were literal alternations**,
//! not one of them containing a single regex metacharacter.
//!
//! The engine paid for the costume twice:
//!
//! * **Time.** Compiling them cost the scan 156 ms of its 176.8 ms rule build,
//!   once per scan, to produce 935 automata that never matched anything a
//!   string comparison could not.
//! * **Space.** 327 KB of the 407 KB `rules.json`, most of it 935 copies of
//!   the same sentence in two languages - text this module generates from the
//!   game's title instead ([`intro_desc`]).
//!
//! Kept in a table, a game costs one hash lookup and a handful of
//! case-insensitive comparisons, and `rules.json` goes back to being the 51
//! hand-written rules a person can open and edit.
//!
//! # What did *not* move
//!
//! The precedence is unchanged and still lives in the rule engine: within a
//! category, an entry that *looked the answer up* beats a pattern that
//! *guessed* it (`rules::RuleOrigin`), and a higher-ranked category still
//! wins over both. [`Rule::origin`](crate::rules::Rule::origin) also stays -
//! it is how an imported or hand-written pack declares the same thing, and
//! this table is simply the shipped catalogue's own storage.
//!
//! # One deliberate narrowing
//!
//! A rule in the `intro` category is tested against every folder segment as
//! well as the file name, because that category mixes folder rules (a `Logos`
//! folder) with file rules (a bare `nvidia_logo.bik`). A catalogue entry is
//! only ever a file name, so this table tests only the file name. The
//! difference is a folder named byte for byte like one of the wiki's video
//! files - `Movies\intro_ea.bik\...` - whose contents the old shape would
//! have flagged as intro. That was a false positive, not a feature.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::localized::DEFAULT_LANG;

/// Supported major version of `game_reference.json`. A file with a greater
/// version was produced by a newer GameTrimmer - refuse it rather than
/// silently misread it, exactly as [`crate::rules::RULE_PACK_VERSION`] does.
pub const GAME_REFERENCE_VERSION: u32 = 1;

/// The repo's `game_reference.json`, embedded at build time.
///
/// Embedded rather than shipped beside the executable on purpose: the
/// catalogue is read-only data that changes only when a new build ships, and
/// a copy on disk next to the exe is a copy that goes stale silently - the
/// failure a stale language pack in `dist/` already produced once, where a
/// fresh binary quietly analysed with a month-old table.
pub const BUILTIN_GAME_REFERENCE_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../game_reference.json"
));

/// How deep into a game's tree a catalogue entry may match.
///
/// The built-in intro *patterns* cap at 2-4 segments, which is right for a
/// broad regex that must not reach into an asset tree. An exact file name in
/// one named game is not that pattern, and the wiki's own paths go deeper:
/// `Whiplash\GameSDK\Videos\LegalScreens.bk2` is already 4, and Unreal titles
/// bury movies under `Game\Content\Movies\Startup\`.
pub const REFERENCE_MAX_DEPTH: usize = 8;

/// The confidence a catalogue entry reports.
///
/// Above `app::model::REVIEW_CONFIDENCE_THRESHOLD` (85), so these rows do not
/// carry the review mark: a catalogue naming this game's intro videos one by
/// one is the strongest evidence the app has. Nothing is pre-ticked (GT-89),
/// so this decides how the row is *marked*, never whether it is selected.
pub const REFERENCE_CONFIDENCE: u8 = 96;

/// The description a catalogue entry shows, built from the game's title.
///
/// Generated rather than stored because it is the same sentence 649 times
/// over, and storing it was most of the old shape's bulk. English is the
/// language the scan stores findings in; the window translates at display
/// time through the same function (see the app's `descriptions` module),
/// which is why the mapping has to be reproducible from the title alone.
pub fn intro_desc(title: &str, lang: &str) -> String {
    match lang {
        "uk" => format!("Вступне відео, яке PCGamingWiki називає для гри {title}"),
        _ => format!("Intro video PCGamingWiki names for {title}"),
    }
}

/// The on-disk shape of `game_reference.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceFile {
    version: u32,
    games: Vec<ReferenceEntry>,
}

/// One catalogue entry: everything the wiki knows about one game, keyed by
/// the store id the Steam and GOG providers put in `games.app_id`.
///
/// `intro_files` is the only column today. The next ones the harvest already
/// holds - launch options for 271 games, config edits for 199 - are added
/// here as further fields when something reads them, which is what makes a
/// table the right shape: they do not become more regexes in `rules.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceEntry {
    app_id: String,
    title: String,
    intro_files: Vec<String>,
}

/// One game's row, parsed: its title, the description resolved once for the
/// interface language, and its intro file names lowercased for matching.
#[derive(Debug, Clone)]
struct ReferenceGame {
    title: String,
    desc: String,
    /// ASCII-lowercased. See [`GameReference::intro_desc_for`] for why a
    /// `Vec` beats a `HashSet` here.
    intro_files: Vec<String>,
}

/// The catalogue, indexed by store id.
///
/// Empty by default, and an empty catalogue is a valid one: an engine built
/// to validate an incoming rule pack has no business consulting the shipped
/// reference, and every test that does not name a game gets the same
/// behaviour the engine had before this table existed.
#[derive(Debug, Default)]
pub struct GameReference {
    games: HashMap<String, ReferenceGame>,
}

impl GameReference {
    /// Parses a catalogue, describing its entries in English - the language
    /// the scan stores findings in.
    pub fn from_json(json: &str) -> Result<Self> {
        Self::from_json_in(json, DEFAULT_LANG)
    }

    /// Parses a catalogue, resolving every description into `lang` once,
    /// here, rather than per matched file.
    pub fn from_json_in(json: &str, lang: &str) -> Result<Self> {
        let file: ReferenceFile = serde_json::from_str(json)?;
        if file.version > GAME_REFERENCE_VERSION {
            return Err(CoreError::Other(format!(
                "game_reference.json version {} is newer than supported \
                 {GAME_REFERENCE_VERSION} - update GameTrimmer",
                file.version,
            )));
        }

        let mut games = HashMap::with_capacity(file.games.len());
        for (index, entry) in file.games.into_iter().enumerate() {
            if entry.app_id.is_empty() || entry.title.is_empty() {
                return Err(CoreError::Other(format!(
                    "game_reference.json: entry #{index} has no app_id or no title; \
                     a catalogue entry is about one named, identified game"
                )));
            }
            // Every name the harvest has ever produced is ASCII, and the
            // matcher folds case the ASCII way because that is what makes it
            // allocation-free. A non-ASCII name is refused rather than
            // matched case-sensitively behind everyone's back - it means the
            // wiki started naming files this matcher cannot fold, and that is
            // a decision to take in daylight.
            if let Some(name) = entry.intro_files.iter().find(|name| !name.is_ascii()) {
                return Err(CoreError::Other(format!(
                    "game_reference.json: entry #{index} ({}) names a non-ASCII file \
                     `{name}`; the matcher folds case as ASCII, so this would only ever \
                     match byte for byte",
                    entry.title
                )));
            }
            let game = ReferenceGame {
                desc: intro_desc(&entry.title, lang),
                title: entry.title,
                intro_files: entry
                    .intro_files
                    .into_iter()
                    .map(|name| name.to_ascii_lowercase())
                    .collect(),
            };
            // Two entries claiming one store id would mean one of the two
            // silently losing its file list. Refused rather than merged: the
            // generator deduplicates, so this can only fire on a data bug,
            // and a data bug that announces itself is worth more than a
            // catalogue that half-works.
            if let Some(previous) = games.insert(entry.app_id.clone(), game) {
                return Err(CoreError::Other(format!(
                    "game_reference.json: app_id {} is claimed twice ({} and entry #{index})",
                    entry.app_id, previous.title
                )));
            }
        }
        Ok(Self { games })
    }

    /// The catalogue this build ships, in English.
    pub fn builtin() -> Result<Self> {
        Self::from_json(BUILTIN_GAME_REFERENCE_JSON)
    }

    /// How many games the catalogue knows.
    pub fn len(&self) -> usize {
        self.games.len()
    }

    pub fn is_empty(&self) -> bool {
        self.games.is_empty()
    }

    /// Every catalogued game's title, in no particular order.
    ///
    /// The window uses this to rebuild the English-to-interface-language
    /// description mapping without a second copy of the text - see
    /// [`intro_desc`].
    pub fn titles(&self) -> impl Iterator<Item = &str> {
        self.games.values().map(|game| game.title.as_str())
    }

    /// The description to report when `file_name` is one of the intro videos
    /// the catalogue names for the game whose store id is `app_id`.
    ///
    /// A `Vec` scanned with [`str::eq_ignore_ascii_case`] rather than a
    /// `HashSet`: a game names 2.3 intro files on average, so hashing a
    /// lowercased copy of every file name in the library would cost an
    /// allocation per file to save two string comparisons per catalogued
    /// game. This way a file in a catalogued game pays one hash lookup and a
    /// handful of comparisons, and a file in every other game pays the
    /// lookup alone.
    pub(crate) fn intro_desc_for(&self, app_id: &str, file_name: &str) -> Option<&str> {
        let game = self.games.get(app_id)?;
        game.intro_files
            .iter()
            .any(|name| name.eq_ignore_ascii_case(file_name))
            .then_some(game.desc.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalogue(games: &str) -> String {
        format!(r#"{{"version":{GAME_REFERENCE_VERSION},"games":{games}}}"#)
    }

    fn prey() -> String {
        catalogue(
            r#"[{"app_id":"480490","title":"Prey (2017)",
                 "intro_files":["ArkaneLogoAnim_Redux.bk2","legalscreens.bk2"]}]"#,
        )
    }

    /// The lookup, and its counter-example in the same test: the same file
    /// name in a game the catalogue does not know must answer nothing, or a
    /// green result here would only prove that the name matched *something*.
    #[test]
    fn an_entry_answers_for_its_own_game_and_no_other() {
        let reference = GameReference::from_json(&prey()).expect("the catalogue parses");

        assert_eq!(
            reference.intro_desc_for("480490", "legalscreens.bk2"),
            Some("Intro video PCGamingWiki names for Prey (2017)"),
        );
        assert_eq!(reference.intro_desc_for("730", "legalscreens.bk2"), None);
        assert_eq!(reference.intro_desc_for("480490", "gameplay.bk2"), None);
    }

    /// The wiki writes file names as they appear on the page, Windows does
    /// not care, and the old shape compiled its regexes case-insensitively.
    #[test]
    fn matching_ignores_case_in_both_directions() {
        let reference = GameReference::from_json(&prey()).expect("the catalogue parses");

        for name in [
            "arkanelogoanim_redux.bk2",
            "ArkaneLogoAnim_Redux.bk2",
            "ARKANELOGOANIM_REDUX.BK2",
        ] {
            assert!(
                reference.intro_desc_for("480490", name).is_some(),
                "{name} was not recognised",
            );
        }
    }

    /// Descriptions are generated, so the window has to be able to rebuild
    /// the English form from the title alone to translate it.
    #[test]
    fn a_title_reproduces_the_description_the_scan_stored() {
        let reference = GameReference::from_json(&prey()).expect("the catalogue parses");
        let title = reference.titles().next().expect("one game");

        assert_eq!(
            reference.intro_desc_for("480490", "legalscreens.bk2"),
            Some(intro_desc(title, DEFAULT_LANG).as_str()),
        );
        assert_ne!(intro_desc(title, "uk"), intro_desc(title, DEFAULT_LANG));
    }

    #[test]
    fn a_catalogue_from_a_newer_build_is_refused() {
        let json = format!(r#"{{"version":{},"games":[]}}"#, GAME_REFERENCE_VERSION + 1);

        let err = GameReference::from_json(&json).expect_err("a newer version must be refused");
        assert!(err.to_string().contains("newer than supported"), "{err}");
    }

    #[test]
    fn one_store_id_claimed_twice_is_refused() {
        let json = catalogue(
            r#"[{"app_id":"480490","title":"Prey (2017)","intro_files":["a.bk2"]},
                {"app_id":"480490","title":"Prey (2017) again","intro_files":["b.bk2"]}]"#,
        );

        let err = GameReference::from_json(&json).expect_err("a duplicate id must be refused");
        assert!(err.to_string().contains("claimed twice"), "{err}");
    }

    /// The matcher folds case the ASCII way; a name it cannot fold must not
    /// slip in and match byte for byte instead.
    #[test]
    fn a_non_ascii_file_name_is_refused() {
        let json = catalogue(r#"[{"app_id":"1","title":"Some game","intro_files":["intrõ.bk2"]}]"#);

        let err = GameReference::from_json(&json).expect_err("a non-ASCII name must be refused");
        assert!(err.to_string().contains("non-ASCII"), "{err}");
    }

    #[test]
    fn an_entry_without_an_identity_is_refused() {
        for games in [
            r#"[{"app_id":"","title":"Nameless id","intro_files":[]}]"#,
            r#"[{"app_id":"1","title":"","intro_files":[]}]"#,
        ] {
            let err = GameReference::from_json(&catalogue(games))
                .expect_err("an entry with no identity must be refused");
            assert!(err.to_string().contains("no app_id or no title"), "{err}");
        }
    }

    /// The shipped catalogue is data, and data can be broken by a harvest as
    /// easily as by a hand edit. Every invariant above is checked against it
    /// here so a bad regeneration fails the build rather than a scan.
    #[test]
    fn the_shipped_catalogue_parses_and_names_real_games() {
        let reference = GameReference::builtin().expect("the built-in catalogue parses");

        assert!(
            reference.len() > 500,
            "the catalogue shrank to {} games - a harvest or generator regression",
            reference.len(),
        );
        // Prey is the precedence case (a studio heuristic also matches this
        // file at confidence 95) and Alice is the reach case (no built-in
        // rule sees this name at all). Both are named in GT-223 and both must
        // survive the move out of rules.json.
        assert!(
            reference
                .intro_desc_for("480490", "arkanelogoanim_redux_1080p2997_st-16lufs.bk2")
                .is_some(),
            "Prey (2017) lost its catalogue entry",
        );
        assert!(
            reference.intro_desc_for("19680", "intro_ea.bik").is_some(),
            "Alice: Madness Returns lost its catalogue entry",
        );
    }
}
