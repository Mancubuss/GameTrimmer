//! Tinting the part of a tree row's name that the search actually matched.
//!
//! The search already hides every row that does not match, but on a row that
//! survives - a long relative path, a game with a long title - the user still
//! has to find their query inside the name by eye. This module draws the name
//! as an [`egui::text::LayoutJob`] with a background behind the matched
//! characters, so the answer to "why is this row here" is visible rather than
//! inferred.
//!
//! # Why the name is assembled from pieces
//!
//! Rows do not display a raw field. A game row shows the title in quotation
//! marks; a flat row shows the game and the relative path joined by a dash.
//! Those characters were never in [`crate::search::Corpus`], so a query that
//! happens to contain one must not light them up - that would claim a match
//! the filtering never made. [`Part`] is how each row says which of its pieces
//! the search could have seen.
//!
//! # Why an unmatched name is still drawn as `RichText`
//!
//! A `LayoutJob` has to name a concrete colour and font for every section,
//! while `RichText` leaves both to the widget. Building one unconditionally
//! would therefore freeze the colour of every name in the tree, including the
//! hover shade an interactive [`egui::Label`] gives it. So a name with nothing
//! to tint is returned exactly as it was built before this module existed, and
//! only a name with a real match pays the price - and even then it keeps
//! [`egui::Color32::PLACEHOLDER`] wherever it can, which is the same "the
//! widget decides" marker `RichText` emits.

use std::borrow::Cow;
use std::ops::Range;

use eframe::egui;
use egui::text::LayoutJob;

/// One piece of a row's displayed name.
#[derive(Debug, Clone)]
pub enum Part<'a> {
    /// Text taken from a field the search index reads - a game name, a
    /// relative path. A match here is a match the user really made.
    Searched(Cow<'a, str>),
    /// Punctuation the row adds around those fields: quotation marks, the flat
    /// row's dash. Never tinted, because the search never saw it.
    Decoration(Cow<'a, str>),
}

impl<'a> Part<'a> {
    pub fn searched(text: impl Into<Cow<'a, str>>) -> Self {
        Self::Searched(text.into())
    }

    pub fn decoration(text: impl Into<Cow<'a, str>>) -> Self {
        Self::Decoration(text.into())
    }

    fn text(&self) -> &str {
        match self {
            Self::Searched(text) | Self::Decoration(text) => text,
        }
    }
}

/// A row name in the tree's ordinary weight, with the search match tinted.
///
/// `query` must be the query as [`crate::search::SearchIndex`] folded it -
/// trimmed and lowercased - so that what is tinted and what was filtered agree
/// by construction. `""` means no search is in effect.
pub fn name(ui: &egui::Ui, parts: &[Part<'_>], query: &str) -> egui::WidgetText {
    draw(ui, parts, query, false)
}

/// The same for a heading row, which is drawn in the theme's strong text
/// colour. The tint sits *behind* the characters, so the heading keeps that
/// colour whether or not it matched.
pub fn strong_name(ui: &egui::Ui, parts: &[Part<'_>], query: &str) -> egui::WidgetText {
    draw(ui, parts, query, true)
}

fn draw(ui: &egui::Ui, parts: &[Part<'_>], query: &str, strong: bool) -> egui::WidgetText {
    let color = if strong {
        ui.visuals().strong_text_color()
    } else {
        egui::Color32::PLACEHOLDER
    };
    let font_id = egui::FontSelection::Default.resolve(ui.style());
    // The theme's text-selection fill: the one background colour egui
    // guarantees ordinary text stays readable on, in either theme. The
    // keyboard cursor paints its row with the same colour at a third of the
    // alpha, so a match reads as the brighter of the two rather than as a
    // second, unexplained kind of highlight.
    let tint = ui.visuals().selection.bg_fill;

    match tinted_job(parts, query, &font_id, color, tint) {
        Some(job) => job.into(),
        None => {
            let text = egui::RichText::new(joined(parts));
            if strong { text.strong() } else { text }.into()
        }
    }
}

/// The whole name as one string - what the row displayed before there was
/// anything to tint, and what it still displays when nothing matches.
fn joined(parts: &[Part<'_>]) -> String {
    match parts {
        [only] => only.text().to_owned(),
        _ => parts.iter().map(Part::text).collect(),
    }
}

/// Builds the two-tone job, or `None` when no [`Part::Searched`] piece
/// matches.
///
/// In that case the caller keeps drawing the plain `RichText` it always drew
/// (see the module docs).
fn tinted_job(
    parts: &[Part<'_>],
    query: &str,
    font_id: &egui::FontId,
    color: egui::Color32,
    tint: egui::Color32,
) -> Option<LayoutJob> {
    if query.is_empty() {
        return None;
    }

    let mut job = LayoutJob::default();
    let mut matched = false;
    for part in parts {
        let Part::Searched(text) = part else {
            append(&mut job, part.text(), font_id, color, None);
            continue;
        };
        let mut at = 0;
        for range in match_ranges(text, query) {
            matched = true;
            append(&mut job, &text[at..range.start], font_id, color, None);
            append(&mut job, &text[range.clone()], font_id, color, Some(tint));
            at = range.end;
        }
        append(&mut job, &text[at..], font_id, color, None);
    }

    matched.then_some(job)
}

/// Appends one section, skipping the empty ones a match at the very start or
/// end of a piece would otherwise produce.
fn append(
    job: &mut LayoutJob,
    text: &str,
    font_id: &egui::FontId,
    color: egui::Color32,
    background: Option<egui::Color32>,
) {
    if text.is_empty() {
        return;
    }
    job.append(
        text,
        0.0,
        egui::TextFormat {
            font_id: font_id.clone(),
            color,
            background: background.unwrap_or(egui::Color32::TRANSPARENT),
            ..Default::default()
        },
    );
}

/// Byte ranges of `text` that `query` matches, in ascending order and never
/// overlapping. `query` is expected already folded (see [`name`]).
///
/// # Why the folded text needs a map back
///
/// Lowercasing can change a string's length - `'\u{130}'` folds into two
/// characters - so an offset found in the folded text cannot be used to slice
/// the original. Each folded byte therefore remembers the whole original
/// character it came from, and a match is widened to character boundaries.
/// Tinting half of a character is not a thing a layout can do anyway.
fn match_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    debug_assert_eq!(
        query,
        query.trim().to_lowercase(),
        "the query must arrive folded exactly as the search index folded it",
    );
    if query.is_empty() || text.is_empty() {
        return Vec::new();
    }

    let mut folded = String::with_capacity(text.len());
    let mut starts: Vec<usize> = Vec::with_capacity(text.len());
    let mut ends: Vec<usize> = Vec::with_capacity(text.len());
    for (offset, ch) in text.char_indices() {
        for lower in ch.to_lowercase() {
            folded.push(lower);
        }
        starts.resize(folded.len(), offset);
        ends.resize(folded.len(), offset + ch.len_utf8());
    }

    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut from = 0;
    while let Some(offset) = folded[from..].find(query) {
        let start = from + offset;
        let end = start + query.len();
        let range = starts[start]..ends[end - 1];
        match ranges.last_mut() {
            // Two hits can land inside one original character (folding can
            // turn one character into several), and touching ranges would
            // paint the same gap twice. Either way the layout wants one
            // section, so they are merged instead of pushed as a pair.
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => ranges.push(range),
        }
        // A non-empty query always advances, so this cannot spin.
        from = end;
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLOR: egui::Color32 = egui::Color32::WHITE;
    const TINT: egui::Color32 = egui::Color32::BLUE;

    /// Builds the job and reduces it to its whole observable result: every
    /// section as `(text, is it tinted)`, in the order it will be painted.
    fn job(parts: &[Part<'_>], query: &str) -> Option<Vec<(String, bool)>> {
        let font_id = egui::FontId::proportional(12.0);
        tinted_job(parts, query, &font_id, COLOR, TINT).map(|job| {
            job.sections
                .iter()
                .map(|section| {
                    let range = section.byte_range.start.0..section.byte_range.end.0;
                    (
                        job.text[range].to_owned(),
                        section.format.background == TINT,
                    )
                })
                .collect()
        })
    }

    #[test]
    fn a_match_splits_the_name_into_a_tinted_middle() {
        assert_eq!(
            job(&[Part::searched("data\\loc_fr.pak")], "loc"),
            Some(vec![
                ("data\\".to_owned(), false),
                ("loc".to_owned(), true),
                ("_fr.pak".to_owned(), false),
            ]),
        );
    }

    #[test]
    fn a_match_at_either_end_produces_no_empty_section() {
        assert_eq!(
            job(&[Part::searched("loc")], "loc"),
            Some(vec![("loc".to_owned(), true)]),
        );
        assert_eq!(
            job(&[Part::searched("locale")], "loc"),
            Some(vec![("loc".to_owned(), true), ("ale".to_owned(), false)]),
        );
        assert_eq!(
            job(&[Part::searched("myloc")], "loc"),
            Some(vec![("my".to_owned(), false), ("loc".to_owned(), true)]),
        );
    }

    #[test]
    fn every_occurrence_is_tinted_not_just_the_first() {
        assert_eq!(
            job(&[Part::searched("loc\\loc.pak")], "loc"),
            Some(vec![
                ("loc".to_owned(), true),
                ("\\".to_owned(), false),
                ("loc".to_owned(), true),
                (".pak".to_owned(), false),
            ]),
        );
    }

    /// Back-to-back hits are one run of characters on screen, so they are one
    /// section - not two that happen to touch.
    #[test]
    fn touching_occurrences_merge_into_one_section() {
        assert_eq!(
            job(&[Part::searched("abab.pak")], "ab"),
            Some(vec![("abab".to_owned(), true), (".pak".to_owned(), false)]),
        );
    }

    /// The tint has to follow the same case folding the filtering used, or a
    /// row would be shown with nothing on it explaining why.
    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(
            job(&[Part::searched("Data\\LOC.pak")], "loc"),
            Some(vec![
                ("Data\\".to_owned(), false),
                ("LOC".to_owned(), true),
                (".pak".to_owned(), false),
            ]),
        );
    }

    #[test]
    fn matching_folds_cyrillic() {
        // "Українська.pak" against the lowercase "українська".
        let text = "\u{423}\u{43a}\u{440}\u{430}\u{457}\u{43d}\u{441}\u{44c}\u{43a}\u{430}.pak";
        let query = "\u{443}\u{43a}\u{440}\u{430}\u{457}\u{43d}\u{441}\u{44c}\u{43a}\u{430}";
        assert_eq!(
            job(&[Part::searched(text)], query),
            Some(vec![
                (text[..text.len() - 4].to_owned(), true),
                (".pak".to_owned(), false),
            ]),
        );
    }

    /// A character whose lowercase form is longer than itself: the match is
    /// found in the folded text at offsets that do not exist in the original,
    /// and must still come back as a whole character.
    #[test]
    fn a_match_inside_a_character_that_folds_longer_covers_the_whole_character() {
        // 'I' with a dot above folds into "i" plus a combining dot.
        let text = "\u{130}stanbul";
        assert_eq!(
            job(&[Part::searched(text)], "i"),
            Some(vec![
                ("\u{130}".to_owned(), true),
                ("stanbul".to_owned(), false),
            ]),
        );
    }

    #[test]
    fn decoration_is_never_tinted() {
        assert_eq!(
            job(
                &[
                    Part::decoration("\u{ab}"),
                    Part::searched("Portal"),
                    Part::decoration("\u{bb}"),
                ],
                "portal",
            ),
            Some(vec![
                ("\u{ab}".to_owned(), false),
                ("Portal".to_owned(), true),
                ("\u{bb}".to_owned(), false),
            ]),
        );
    }

    /// A query that only appears in the punctuation the row added is not a
    /// match at all - the row is on screen for some other reason, and tinting
    /// the quotation mark would point at the wrong thing.
    #[test]
    fn a_query_that_only_hits_decoration_leaves_the_name_alone() {
        assert!(job(
            &[
                Part::decoration("\u{ab}"),
                Part::searched("Portal"),
                Part::decoration("\u{bb}"),
            ],
            "\u{ab}",
        )
        .is_none());
    }

    /// The pieces are matched one at a time, so a query cannot bridge the
    /// separator between two of them - the same rule the search index enforces
    /// with its `NUL` between a game name and a path.
    #[test]
    fn a_query_cannot_match_across_two_pieces() {
        assert!(job(
            &[
                Part::searched("Doom"),
                Part::decoration(" \u{2014} "),
                Part::searched("eternal.pak"),
            ],
            "doom \u{2014} eternal",
        )
        .is_none());
    }

    #[test]
    fn no_match_and_no_query_both_leave_the_row_as_it_was() {
        assert!(job(&[Part::searched("data\\loc.pak")], "witcher").is_none());
        assert!(job(&[Part::searched("data\\loc.pak")], "").is_none());
    }

    /// What [`draw`] adds on top of [`tinted_job`]: the style it resolves and
    /// the shape it returns. Reduced to `(is it a job, the colour of every
    /// section)` - the two things a row depends on.
    fn drawn(parts: &[Part<'_>], query: &str, strong: bool) -> (bool, Vec<egui::Color32>) {
        let mut result = None;
        egui::__run_test_ui(|ui| {
            let text = draw(ui, parts, query, strong);
            result = Some(match text {
                egui::WidgetText::LayoutJob(job) => (
                    true,
                    job.sections
                        .iter()
                        .map(|section| section.format.color)
                        .collect(),
                ),
                _ => (false, vec![]),
            });
        });
        result.expect("__run_test_ui always runs its contents")
    }

    /// A name with nothing to tint has to come back as the plain `RichText`
    /// the tree drew before this module existed - baking a job would freeze
    /// its colour, hover shade and all (see the module docs).
    #[test]
    fn a_name_with_no_match_is_left_as_plain_text() {
        assert!(!drawn(&[Part::searched("loc.pak")], "", false).0);
        assert!(!drawn(&[Part::searched("loc.pak")], "witcher", true).0);
    }

    /// The heading's colour is the *base* of every section, tinted or not - so
    /// a heading that matches is still drawn as a heading, which is exactly
    /// what tinting the background rather than the glyphs buys.
    #[test]
    fn a_matched_heading_keeps_its_strong_colour_across_every_section() {
        let (is_job, colors) = drawn(&[Part::searched("data\\loc.pak")], "loc", true);
        assert!(is_job, "a match has to produce a job to tint");
        assert!(
            colors.len() > 1,
            "the match must split the name: {colors:?}"
        );
        let strong = egui::Visuals::dark().strong_text_color();
        let light = egui::Visuals::light().strong_text_color();
        assert!(
            colors.iter().all(|c| *c == strong || *c == light),
            "a heading's sections must all be the theme's strong colour: {colors:?}",
        );
    }

    /// An ordinary row leaves its colour to the widget, the same way
    /// `RichText` does, so an interactive label still lights up on hover.
    #[test]
    fn a_matched_file_row_leaves_its_colour_to_the_widget() {
        let (is_job, colors) = drawn(&[Part::searched("data\\loc.pak")], "loc", false);
        assert!(is_job);
        assert!(
            colors.iter().all(|c| *c == egui::Color32::PLACEHOLDER),
            "{colors:?}",
        );
    }

    #[test]
    fn joined_reassembles_the_displayed_name() {
        assert_eq!(
            joined(&[
                Part::decoration("\u{ab}"),
                Part::searched("Portal"),
                Part::decoration("\u{bb}"),
            ]),
            "\u{ab}Portal\u{bb}",
        );
        assert_eq!(joined(&[Part::searched("only")]), "only");
        assert_eq!(joined(&[]), "");
    }
}
