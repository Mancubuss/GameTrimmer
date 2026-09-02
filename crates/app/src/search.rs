//! Name search over the findings tree (name search): the user types part of a game,
//! folder, or file name and the tree shows only the branches that contain a
//! match.
//!
//! # Why an index and not a per-frame filter
//!
//! `ui::tree_view::build_visible_rows` normally runs once per frame (and once
//! more only after keyboard expansion changes structural visibility), while a
//! real scan holds hundreds of thousands of findings.
//! Testing every path against the query inside that walk would put a full
//! string scan of the whole result set into the frame budget. Instead the match
//! decision is made once per query change and stored here: a `Vec<bool>` keyed
//! by finding index, plus the set of game ids that have at least one match, so
//! the per-frame walk only does O(1) lookups.
//!
//! # Why matching a game name marks its whole subtree
//!
//! Visibility then has a single rule at every level - "does this node contain a
//! matched finding" - instead of one rule for game rows and another for
//! everything below them. Typing a game's name therefore opens up its entire
//! contents to browse, which is the point of searching for a game.
//!
//! # Why the case folding is precomputed
//!
//! Once per query change is still once per keystroke, and the first version of
//! this module folded case *during* the comparison: for every byte offset of
//! every path it re-ran `char::to_lowercase` character by character. That is
//! what made the field feel like it was lagging a character behind. Measured on
//! this machine over a synthetic 200,000-finding result set, one keystroke cost
//! 75-112 ms (500,000 findings: 192-278 ms), all of it on the UI thread, with
//! the worst case being a query that matches nothing - every position of every
//! string is tried before it gives up.
//!
//! An ASCII fast path was the cheaper fix and was measured too: 20-27 ms, but
//! it does nothing for a Cyrillic query (110 ms), and game and file names here
//! are routinely non-ASCII. So the fold happens exactly once, when the findings
//! change, into a [`Corpus`] of lowercase haystacks; a keystroke then does
//! plain `str::contains` over them - 4-8 ms at 200,000 findings, and the same
//! for a Cyrillic query as for an ASCII one.
//!
//! The cost is one lowercase copy of each finding's game name and relative path
//! (~15 MB at 200,000 findings) plus ~80 ms to build it - paid once, at the end
//! of a scan that took seconds or minutes.
//!
//! On top of that, [`SearchIndex::build`] narrows: when the new query extends
//! the one the previous index was built for, only findings that already matched
//! can still match, so every keystroke after the first scans the previous hits
//! rather than the whole corpus.
//!
//! # Wildcards
//!
//! `*` matches a run of characters, including none, so `loc_*.pak` reads as
//! "starts with `loc_`, ends with `.pak`" and `*.ttf` as "ends with `.ttf`".
//! Several `*` may appear in one query. A query with none is exactly the
//! substring match described above - [`glob_segments`] splits on `*`, and a
//! query without one produces a single segment, so the loops below run the
//! same `find` they always did.
//!
//! `?` is not special - it matches only itself, like every other character.
//! GT-226 asked for `*`; a single-character wildcard was not requested and
//! regex is explicitly out of scope, so this stays the smallest thing that
//! covers the ask.
//!
//! No pattern is compiled per row: [`SearchIndex::build`] splits the query on
//! `*` once, and the two match loops below reuse that `Vec<&str>` for every
//! haystack.

use std::collections::HashSet;

use crate::model::FindingItem;

/// Separates a finding's game name from its relative path inside one haystack.
/// `NUL` because NTFS forbids it in a file name, so no query can match across
/// the join and produce a hit that exists in neither field.
const FIELD_SEPARATOR: char = '\0';

/// The searchable text of the current findings, folded to lowercase once.
///
/// Rebuilt whenever `findings` is replaced (see `GameTrimmerApp::clear_search`,
/// which is called at every site that swaps the findings list), and indexed by
/// position in that list exactly like [`SearchIndex::matched`].
#[derive(Debug, Default, Clone)]
pub struct Corpus {
    /// `"<game name>\0<relative path>"`, lowercased, one per finding.
    haystacks: Vec<String>,
    /// The game id of each finding, in the same order.
    game_ids: Vec<i64>,
}

impl Corpus {
    /// Folds `findings` into lowercase haystacks. O(total text length), and the
    /// only place in the search path that allocates per finding.
    pub fn build(findings: &[FindingItem]) -> Self {
        let mut haystacks = Vec::with_capacity(findings.len());
        let mut game_ids = Vec::with_capacity(findings.len());

        for item in findings {
            let mut haystack = item.row.game_name.to_lowercase();
            haystack.push(FIELD_SEPARATOR);
            haystack.push_str(&item.row.rel_path.to_lowercase());
            haystacks.push(haystack);
            game_ids.push(item.row.game_id);
        }

        Self {
            haystacks,
            game_ids,
        }
    }

    /// How many findings this corpus covers. Private: nothing outside this
    /// module has any use for the count, and the only reason it exists is
    /// [`SearchIndex::narrows_into`]'s check that an index and a corpus are
    /// talking about the same findings list.
    fn len(&self) -> usize {
        self.haystacks.len()
    }
}

/// Splits a folded query on `*` into the literal runs a match must contain in
/// order. A query with no `*` comes back as one segment - the same string
/// `str::contains` was always called with - so plain queries are unaffected.
fn glob_segments(query: &str) -> Vec<&str> {
    query.split('*').collect()
}

/// Whether `haystack` contains `segments` in order, each found no earlier
/// than the end of the previous one - `*` becomes "any run of characters,
/// including none" between them. A single segment (no `*` in the query)
/// reduces to `haystack.contains(segments[0])`, today's behaviour.
///
/// ponytail: a segment search is free to walk past the `FIELD_SEPARATOR`
/// between a haystack's game name and its path, so a deliberately crafted
/// multi-segment query (e.g. `"me*eternal"` against `"Some Game\0eternal.pak"`)
/// could otherwise claim a match that starts in one field and ends in the
/// other - exactly what [`FIELD_SEPARATOR`] exists to prevent for a plain
/// query. The check below refuses any match whose span crosses it. Upgrade
/// path if that is ever too coarse: search each field independently instead
/// of the joined haystack.
fn matches_segments(haystack: &str, segments: &[&str]) -> bool {
    let mut pos = 0;
    let mut span_start = None;
    let mut span_end = 0;

    for segment in segments {
        if segment.is_empty() {
            continue; // `*` matching zero characters: no position to advance.
        }
        match haystack[pos..].find(segment) {
            Some(offset) => {
                let start = pos + offset;
                span_start.get_or_insert(start);
                span_end = start + segment.len();
                pos = span_end;
            }
            None => return false,
        }
    }

    match span_start {
        Some(start) => !haystack[start..span_end].contains(FIELD_SEPARATOR),
        // Every segment was empty: the query is made entirely of `*` (e.g.
        // "*", "**"), which matches everything.
        None => true,
    }
}

/// Which findings match the current search query. An empty query is the
/// inactive state: [`is_active`](Self::is_active) is false and the tree view
/// skips filtering entirely.
#[derive(Debug, Default, Clone)]
pub struct SearchIndex {
    /// The trimmed, lowercased query this index was built for, `""` when
    /// inactive.
    query: String,
    /// Indexed by position in `findings`: does this finding match?
    matched: Vec<bool>,
    /// Game ids with at least one matching finding.
    games_with_match: HashSet<i64>,
}

impl SearchIndex {
    /// Builds the index for `query` over `corpus`. A blank query (or one that
    /// is only whitespace) yields the inactive index.
    ///
    /// `previous` is the index this one replaces, if any. When the new query
    /// extends the previous one, only findings that already matched are
    /// re-tested: a longer needle can only ever match a subset of what a prefix
    /// of it matched. Passing `None` (or an index from a different findings
    /// list) is always correct, merely slower - it just scans the whole corpus.
    ///
    /// The two queries are compared *after* trimming and folding, i.e. as the
    /// strings actually tested against the corpus, so the narrowing holds
    /// regardless of how either was spelled.
    pub fn build(query: &str, corpus: &Corpus, previous: Option<&SearchIndex>) -> Self {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Self::default();
        }

        let mut matched = vec![false; corpus.len()];
        let mut games_with_match = HashSet::new();

        // A game-name hit marks every finding under that game, because the
        // game's name is part of every one of its haystacks (see the module
        // docs) - so the whole subtree stays browsable.
        let mut record = |index: usize| {
            matched[index] = true;
            games_with_match.insert(corpus.game_ids[index]);
        };

        // Split once per query change, not once per haystack - see the
        // module docs on wildcards.
        let segments = glob_segments(&query);

        match previous.filter(|prev| prev.narrows_into(&query, corpus)) {
            Some(prev) => {
                for index in prev
                    .matched
                    .iter()
                    .enumerate()
                    .filter(|(_, hit)| **hit)
                    .map(|(index, _)| index)
                {
                    if matches_segments(&corpus.haystacks[index], &segments) {
                        record(index);
                    }
                }
            }
            None => {
                for (index, haystack) in corpus.haystacks.iter().enumerate() {
                    if matches_segments(haystack, &segments) {
                        record(index);
                    }
                }
            }
        }

        Self {
            query,
            matched,
            games_with_match,
        }
    }

    /// Whether this index's hits are a superset of what `query` can match over
    /// `corpus` - the precondition for reusing it instead of rescanning.
    ///
    /// The length check is what ties it to `corpus`: an index built against a
    /// different findings list has no meaningful positions. It cannot be
    /// reached in practice (every site that replaces the findings goes through
    /// `GameTrimmerApp::clear_search`, which resets this index to the inactive
    /// one), but "in practice" is not a guarantee worth silently wrong results.
    fn narrows_into(&self, query: &str, corpus: &Corpus) -> bool {
        self.is_active() && self.matched.len() == corpus.len() && query.starts_with(&self.query)
    }

    /// Whether a search is in effect. When false the tree view must show
    /// every row - callers should not consult the other methods.
    pub fn is_active(&self) -> bool {
        !self.query.is_empty()
    }

    /// The query as this index tested it - trimmed and folded to lowercase -
    /// or `""` when inactive.
    ///
    /// Exposed so that `ui::highlight` can tint the matched characters using
    /// the very string the filtering used. Handing out the raw text the user
    /// typed instead would let the two disagree about case or whitespace, and
    /// a row would appear with nothing on it explaining why.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Whether the finding at `index` matches. Out-of-range indices (an index
    /// built against an older findings list) count as non-matching rather than
    /// panicking - the index is rebuilt the next time the query changes.
    pub fn item_matches(&self, index: usize) -> bool {
        self.matched.get(index).copied().unwrap_or(false)
    }

    /// Whether any finding of `game_id` matches - the O(1) test the tree walk
    /// uses to decide whether a game (and the disk above it) is visible.
    pub fn game_matches(&self, game_id: i64) -> bool {
        self.games_with_match.contains(&game_id)
    }

    /// Whether any of `indices` matches - how category and folder rows decide
    /// whether they still have visible content under the current query.
    pub fn any_matches(&self, indices: &[usize]) -> bool {
        indices.iter().any(|&index| self.item_matches(index))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use gametrimmer_core::rules::Category;

    use super::*;
    use crate::model::{FindingRow, FindingSource};

    fn finding(game_id: i64, game_name: &str, rel_path: &str) -> FindingItem {
        FindingItem {
            row: FindingRow {
                file_id: 1,
                game_id,
                game_name: game_name.to_string(),
                app_id: None,
                install_dir: PathBuf::from(r"F:\Games\Game"),
                rel_path: rel_path.to_string(),
                size: 1,
                size_on_disk: 1,
                source: FindingSource::Rule(Category::RedistFolder),
                rule_desc: String::new(),
                confidence: 100,
                lang_tag: None,
                group_dir: None,
                deletion_block_reason: None,
                imported_untrusted: false,
                library: None,
                action: gametrimmer_core::models::FindingAction::DirectDelete,
                anti_cheat_protected: false,
                monolith_badge: None,
            },
            selected: false,
            removed: false,
        }
    }

    /// The whole path most callers take: fold the findings, then query them.
    fn index(query: &str, findings: &[FindingItem]) -> SearchIndex {
        SearchIndex::build(query, &Corpus::build(findings), None)
    }

    #[test]
    fn build_matches_findings_by_relative_path() {
        let findings = vec![
            finding(1, "Some Game", r"_CommonRedist\vcredist\vc.exe"),
            finding(1, "Some Game", r"Docs\manual.pdf"),
        ];

        let index = index("manual", &findings);

        assert!(!index.item_matches(0));
        assert!(index.item_matches(1));
        assert!(index.game_matches(1));
    }

    /// A game-name hit has to mark the game's whole subtree, otherwise typing
    /// the game's name would show the game row with nothing under it.
    #[test]
    fn build_matches_every_finding_of_a_game_whose_name_matches() {
        let findings = vec![
            finding(7, "The Witcher 3", r"content\audio.pak"),
            finding(7, "The Witcher 3", r"bin\x64\game.exe"),
            finding(8, "Other Game", r"content\audio.pak"),
        ];

        let index = index("witcher", &findings);

        assert!(index.item_matches(0));
        assert!(index.item_matches(1));
        assert!(!index.item_matches(2));
        assert!(index.game_matches(7));
        assert!(!index.game_matches(8));
    }

    #[test]
    fn build_ignores_surrounding_whitespace_in_the_query() {
        let findings = vec![finding(1, "Some Game", r"Docs\manual.pdf")];
        assert!(index("  manual  ", &findings).item_matches(0));
    }

    #[test]
    fn any_matches_reports_whether_a_group_still_has_content() {
        let findings = vec![
            finding(1, "Some Game", r"Docs\manual.pdf"),
            finding(1, "Some Game", r"bin\game.exe"),
        ];

        let index = index("manual", &findings);

        assert!(index.any_matches(&[0, 1]));
        assert!(!index.any_matches(&[1]));
    }

    #[test]
    fn matching_is_case_insensitive_in_both_directions() {
        let findings = vec![
            finding(1, "VALORANT", r"Data\Localization\FR.pak"),
            finding(2, "Battlefield", r"bin\game.exe"),
        ];

        assert!(index("localization", &findings).item_matches(0));
        assert!(index("VALOR", &findings).item_matches(0));
        assert!(!index("portal", &findings).item_matches(1));
    }

    /// Game and file names are routinely non-ASCII, so folding has to cover
    /// more than `a-z` - an ASCII-only fold would silently miss these, and an
    /// ASCII-only *fast path* was rejected for exactly this reason (see the
    /// module docs).
    #[test]
    fn matching_folds_cyrillic() {
        let findings = vec![finding(
            1,
            // "ВІДЬМАК", uppercase
            "\u{412}\u{406}\u{414}\u{42c}\u{41c}\u{410}\u{41a}",
            // "Озвучення\Українська.pak"
            "\u{41e}\u{437}\u{432}\u{443}\u{447}\u{435}\u{43d}\u{43d}\u{44f}\\\
             \u{423}\u{43a}\u{440}\u{430}\u{457}\u{43d}\u{441}\u{44c}\u{43a}\u{430}.pak",
        )];

        // "відьмак" and "українська", both lowercase.
        assert!(index(
            "\u{432}\u{456}\u{434}\u{44c}\u{43c}\u{430}\u{43a}",
            &findings
        )
        .item_matches(0));
        assert!(index(
            "\u{443}\u{43a}\u{440}\u{430}\u{457}\u{43d}\u{441}\u{44c}\u{43a}\u{430}",
            &findings
        )
        .item_matches(0));
    }

    #[test]
    fn a_needle_longer_than_the_haystack_does_not_match() {
        let findings = vec![finding(1, "G", "pak")];
        assert!(index("pak", &findings).item_matches(0));
        assert!(!index("package", &findings).item_matches(0));
    }

    /// The separator between the two fields must not be matchable across, or a
    /// query would find a "hit" that exists in neither the name nor the path.
    #[test]
    fn a_query_cannot_match_across_the_name_and_the_path() {
        let findings = vec![finding(1, "Doom", "eternal.pak")];

        assert!(index("doom", &findings).item_matches(0));
        assert!(index("eternal", &findings).item_matches(0));
        assert!(!index("doometernal", &findings).item_matches(0));
    }

    #[test]
    fn blank_query_builds_an_inactive_index() {
        assert!(!index("", &[]).is_active());
        assert!(!index("   ", &[]).is_active());
    }

    #[test]
    fn non_blank_query_builds_an_active_index() {
        assert!(index("witcher", &[]).is_active());
    }

    #[test]
    fn item_matches_is_false_for_an_out_of_range_index() {
        let index = index("witcher", &[]);
        assert!(!index.item_matches(0));
        assert!(!index.item_matches(9_999));
    }

    #[test]
    fn any_matches_is_false_for_no_indices() {
        let index = index("witcher", &[]);
        assert!(!index.any_matches(&[]));
    }

    /// The narrowing optimisation must be invisible in the result: typing a
    /// query character by character has to land on exactly what building it in
    /// one go produces. This is the assertion that keeps the fast path honest.
    #[test]
    fn narrowing_keystroke_by_keystroke_gives_the_same_answer_as_one_shot() {
        let findings = vec![
            finding(1, "The Witcher 3", r"content\voice\french.pak"),
            finding(1, "The Witcher 3", r"content\voice\polish.pak"),
            finding(2, "Voidtrain", r"bin\game.exe"),
            finding(3, "Other Game", r"Docs\manual.pdf"),
        ];
        let corpus = Corpus::build(&findings);

        let mut typed = SearchIndex::default();
        for query in ["v", "vo", "voi", "voic", "voice"] {
            typed = SearchIndex::build(query, &corpus, Some(&typed));
        }
        let one_shot = SearchIndex::build("voice", &corpus, None);

        assert_eq!(typed.matched, one_shot.matched);
        assert_eq!(typed.games_with_match, one_shot.games_with_match);
        assert!(typed.item_matches(0) && typed.item_matches(1));
        assert!(!typed.item_matches(2), "\"Voidtrain\" is not \"voice\"");
        assert!(!typed.item_matches(3));
    }

    /// Deleting a character widens the query, so the previous index is *not* a
    /// superset any more and the whole corpus has to be rescanned. Getting this
    /// wrong would make backspace silently lose rows.
    #[test]
    fn shortening_the_query_rescans_instead_of_narrowing() {
        let findings = vec![
            finding(1, "Some Game", r"Docs\manual.pdf"),
            finding(2, "Other Game", r"bin\man.exe"),
        ];
        let corpus = Corpus::build(&findings);

        let narrow = SearchIndex::build("manual", &corpus, None);
        assert!(!narrow.item_matches(1));

        let widened = SearchIndex::build("man", &corpus, Some(&narrow));

        assert!(widened.item_matches(0));
        assert!(
            widened.item_matches(1),
            "backspacing lost a row the shorter query matches",
        );
    }

    /// An unrelated query is not an extension of the previous one either.
    #[test]
    fn an_unrelated_query_rescans_instead_of_narrowing() {
        let findings = vec![
            finding(1, "Some Game", r"Docs\manual.pdf"),
            finding(2, "Other Game", r"bin\game.exe"),
        ];
        let corpus = Corpus::build(&findings);

        let first = SearchIndex::build("manual", &corpus, None);
        let second = SearchIndex::build("game.exe", &corpus, Some(&first));

        assert!(second.item_matches(1));
        assert!(!second.item_matches(0));
    }

    /// The guard against reusing an index whose positions mean something else.
    /// It cannot happen through the app (see `narrows_into`), which is exactly
    /// why it needs a test of its own.
    #[test]
    fn an_index_from_another_findings_list_is_not_reused() {
        let old = vec![finding(1, "Some Game", r"Docs\manual.pdf")];
        let new = vec![
            finding(2, "Other Game", r"bin\game.exe"),
            finding(2, "Other Game", r"Docs\manual.pdf"),
        ];
        let stale = SearchIndex::build("manual", &Corpus::build(&old), None);

        let rebuilt = SearchIndex::build("manual", &Corpus::build(&new), Some(&stale));

        assert!(!rebuilt.item_matches(0));
        assert!(
            rebuilt.item_matches(1),
            "the stale index narrowed away a real match",
        );
    }

    #[test]
    fn an_empty_corpus_matches_nothing_without_panicking() {
        let corpus = Corpus::build(&[]);
        assert_eq!(corpus.len(), 0);

        let index = SearchIndex::build("anything", &corpus, None);

        assert!(index.is_active());
        assert!(!index.item_matches(0));
    }

    // GT-226: `*` wildcards.

    #[test]
    fn a_wildcard_at_the_end_matches_a_prefix() {
        let findings = vec![
            finding(1, "Game", r"Data\Localization\loc_fr.pak"),
            finding(1, "Game", r"Data\Localization\FR.ttf"),
        ];
        assert!(index("loc_*", &findings).item_matches(0));
        assert!(!index("loc_*", &findings).item_matches(1));
    }

    #[test]
    fn a_wildcard_at_the_start_matches_a_suffix() {
        let findings = vec![
            finding(1, "Game", r"Fonts\FR.ttf"),
            finding(1, "Game", r"Data\loc_fr.pak"),
        ];
        assert!(index("*.ttf", &findings).item_matches(0));
        assert!(!index("*.ttf", &findings).item_matches(1));
    }

    #[test]
    fn a_wildcard_in_the_middle_matches_both_sides() {
        let findings = vec![
            finding(1, "Game", r"Data\Localization\loc_fr.pak"),
            finding(1, "Game", r"Data\Localization\loc_fr.ttf"),
        ];
        assert!(index("loc_*.pak", &findings).item_matches(0));
        assert!(!index("loc_*.pak", &findings).item_matches(1));
    }

    /// Several `*` in one query: each literal segment has to be found in
    /// order, not just the first and last.
    #[test]
    fn several_wildcards_in_one_query_all_have_to_match_in_order() {
        let findings = vec![
            finding(1, "Game", r"Data\Localization\loc_fr_voice.pak"),
            finding(1, "Game", r"Data\Localization\loc_fr.pak"),
        ];
        assert!(index("loc_*fr*voice*.pak", &findings).item_matches(0));
        assert!(!index("loc_*fr*voice*.pak", &findings).item_matches(1));
    }

    /// A query that is only `*` (or several) has no literal text at all, so
    /// it matches every finding - the degenerate case of "any run of
    /// characters, including none".
    #[test]
    fn a_query_of_only_wildcards_matches_everything() {
        let findings = vec![
            finding(1, "Game", r"Data\loc_fr.pak"),
            finding(2, "Other", r"bin\game.exe"),
        ];
        assert!(index("*", &findings).item_matches(0));
        assert!(index("*", &findings).item_matches(1));
        assert!(index("**", &findings).item_matches(0));
    }

    /// A glob that finds no finding behaves like any other non-matching
    /// query - `*` does not make the field somehow always match.
    #[test]
    fn a_wildcard_query_that_matches_nothing_matches_nothing() {
        let findings = vec![finding(1, "Game", r"Data\loc_fr.pak")];
        assert!(!index("xyz*never", &findings).item_matches(0));
    }

    /// A literal query with no `*` must behave exactly as it did before
    /// wildcards existed - plain, unanchored, case-insensitive substring.
    #[test]
    fn a_query_without_a_wildcard_is_still_plain_substring_matching() {
        let findings = vec![finding(1, "Game", r"Data\Localization\loc_fr.pak")];
        assert!(index("pak", &findings).item_matches(0));
        assert!(index("loc_fr", &findings).item_matches(0));
    }

    /// A wildcard query must not be able to bridge the game-name/path
    /// separator any more than a literal one can (see
    /// `a_query_cannot_match_across_the_name_and_the_path`) - `*` matching
    /// "any run of characters" must not include matching across fields that
    /// were never meant to be searched as one string.
    #[test]
    fn a_wildcard_cannot_bridge_the_name_and_the_path() {
        let findings = vec![finding(1, "Doom", "eternal.pak")];
        assert!(!index("m*eternal", &findings).item_matches(0));
    }
}
