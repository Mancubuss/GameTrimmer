//! Name search over the findings tree (GT-18): the user types part of a game,
//! folder, or file name and the tree shows only the branches that contain a
//! match.
//!
//! # Why an index and not a per-frame filter
//!
//! `ui::tree_view::build_visible_rows` runs on every frame (twice, around
//! keyboard handling), and a real scan holds hundreds of thousands of findings.
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
//! Matching is case-insensitive without allocating: the index is rebuilt on
//! every keystroke, and `to_lowercase()` on every finding's path would mean one
//! allocation per finding per keystroke. Comparison folds case per character,
//! so Cyrillic file names match too, not just ASCII ones.

use std::collections::HashSet;

use crate::model::FindingItem;

/// Which findings match the current search query. An empty query is the
/// inactive state: [`is_active`](Self::is_active) is false and the tree view
/// skips filtering entirely.
#[derive(Debug, Default, Clone)]
pub struct SearchIndex {
    /// The lowercase query this index was built for, `""` when inactive.
    query: String,
    /// Indexed by position in `findings`: does this finding match?
    matched: Vec<bool>,
    /// Game ids with at least one matching finding.
    games_with_match: HashSet<i64>,
}

impl SearchIndex {
    /// Builds the index for `query` over `findings`. A blank query (or one
    /// that is only whitespace) yields the inactive index.
    pub fn build(query: &str, findings: &[FindingItem]) -> Self {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Self::default();
        }

        let mut matched = Vec::with_capacity(findings.len());
        let mut games_with_match = HashSet::new();

        for item in findings {
            // A game-name hit marks every finding under that game, so the
            // whole subtree stays browsable (see the module docs).
            let hit = contains_ignore_case(&item.row.game_name, &query)
                || contains_ignore_case(&item.row.rel_path, &query);
            matched.push(hit);
            if hit {
                games_with_match.insert(item.row.game_id);
            }
        }

        Self {
            query,
            matched,
            games_with_match,
        }
    }

    /// Whether a search is in effect. When false the tree view must show
    /// every row - callers should not consult the other methods.
    pub fn is_active(&self) -> bool {
        !self.query.is_empty()
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

/// Case-insensitive `haystack.contains(needle)` without allocating. `needle`
/// must already be lowercase (the caller lowercases the query once).
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }

    haystack
        .char_indices()
        .any(|(offset, _)| starts_with_ignore_case(&haystack[offset..], needle))
}

/// Case-insensitive `haystack.starts_with(needle)`, folding `haystack`'s case
/// as it goes. `needle` must already be lowercase.
fn starts_with_ignore_case(haystack: &str, needle: &str) -> bool {
    let mut folded = haystack.chars().flat_map(char::to_lowercase);
    let mut wanted = needle.chars();

    loop {
        match (wanted.next(), folded.next()) {
            // Needle exhausted: every char matched.
            (None, _) => return true,
            // Haystack ran out first, or the chars differ.
            (Some(_), None) => return false,
            (Some(want), Some(have)) => {
                if want != have {
                    return false;
                }
            }
        }
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
                install_dir: PathBuf::from(r"F:\Games\Game"),
                rel_path: rel_path.to_string(),
                size: 1,
                size_on_disk: 1,
                source: FindingSource::Rule(Category::RedistFolder),
                rule_desc: String::new(),
                confidence: 100,
                lang_tag: None,
                group_dir: None,
            },
            selected: false,
            removed: false,
        }
    }

    #[test]
    fn build_matches_findings_by_relative_path() {
        let findings = vec![
            finding(1, "Some Game", r"_CommonRedist\vcredist\vc.exe"),
            finding(1, "Some Game", r"Docs\manual.pdf"),
        ];

        let index = SearchIndex::build("manual", &findings);

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

        let index = SearchIndex::build("witcher", &findings);

        assert!(index.item_matches(0));
        assert!(index.item_matches(1));
        assert!(!index.item_matches(2));
        assert!(index.game_matches(7));
        assert!(!index.game_matches(8));
    }

    #[test]
    fn build_ignores_surrounding_whitespace_in_the_query() {
        let findings = vec![finding(1, "Some Game", r"Docs\manual.pdf")];
        assert!(SearchIndex::build("  manual  ", &findings).item_matches(0));
    }

    #[test]
    fn any_matches_reports_whether_a_group_still_has_content() {
        let findings = vec![
            finding(1, "Some Game", r"Docs\manual.pdf"),
            finding(1, "Some Game", r"bin\game.exe"),
        ];

        let index = SearchIndex::build("manual", &findings);

        assert!(index.any_matches(&[0, 1]));
        assert!(!index.any_matches(&[1]));
    }

    #[test]
    fn contains_ignore_case_matches_regardless_of_case() {
        assert!(contains_ignore_case(
            r"Data\Localization\FR.pak",
            "localization"
        ));
        assert!(contains_ignore_case("VALORANT", "valor"));
        assert!(!contains_ignore_case("Battlefield", "portal"));
    }

    /// Game and file names are routinely non-ASCII, so folding has to cover
    /// more than `a-z` - an ASCII-only fold would silently miss these.
    #[test]
    fn contains_ignore_case_folds_cyrillic() {
        assert!(contains_ignore_case(
            r"Озвучення\Українська.pak",
            "українська"
        ));
        assert!(contains_ignore_case("ВІДЬМАК", "відьмак"));
    }

    #[test]
    fn contains_ignore_case_handles_the_end_of_the_haystack() {
        assert!(contains_ignore_case("game.pak", "pak"));
        // Needle longer than what remains must not match (and must not panic).
        assert!(!contains_ignore_case("pak", "package"));
    }

    #[test]
    fn empty_needle_matches_anything() {
        assert!(contains_ignore_case("whatever", ""));
    }

    #[test]
    fn blank_query_builds_an_inactive_index() {
        assert!(!SearchIndex::build("", &[]).is_active());
        assert!(!SearchIndex::build("   ", &[]).is_active());
    }

    #[test]
    fn non_blank_query_builds_an_active_index() {
        assert!(SearchIndex::build("witcher", &[]).is_active());
    }

    #[test]
    fn item_matches_is_false_for_an_out_of_range_index() {
        let index = SearchIndex::build("witcher", &[]);
        assert!(!index.item_matches(0));
        assert!(!index.item_matches(9_999));
    }

    #[test]
    fn any_matches_is_false_for_no_indices() {
        let index = SearchIndex::build("witcher", &[]);
        assert!(!index.any_matches(&[]));
    }
}
