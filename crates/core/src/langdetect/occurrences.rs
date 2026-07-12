//! Recognized-language-token occurrences within a tokenized path.

use std::collections::HashSet;

use crate::langdetect::dict::{self, Level};
use crate::langdetect::tokens::Segment;

#[derive(Debug, Clone)]
pub struct Occurrence {
    pub canonical: &'static str,
    pub level: Level,
    /// The exact text that matched (for building `reason` strings).
    pub matched: String,
    pub is_filename: bool,
    pub start: usize,
    pub end: usize,
}

/// Finds every dictionary match within `segments`: both weak-split compound
/// pieces (so literal locale tags like `es-mx` match as a whole, and generic
/// `<lang>[-_]<REGION>` locale tags like `es_AR` via `dict::lookup_locale_tag`)
/// and fully split atomic tokens (so `SpanishNation` -> `spanish` matches).
///
/// An atom nested entirely inside an already-matched weak piece is
/// suppressed: e.g. `en_SL.res` is a single weak piece that resolves (via
/// `lookup_locale_tag`) to English-with-a-region-suffix ("en", keep-listed),
/// but the strong-delimiter split also produces a standalone `sl` atom
/// (`Segment` splits on `_`) that would otherwise independently match
/// Slovenian — the region suffix of a validated locale tag must not be
/// re-interpreted as a free-standing two-letter language token in its own
/// right, or a keep-listed file could end up flagged under a different,
/// wrong language.
pub fn collect_occurrences(segments: &[Segment]) -> Vec<Occurrence> {
    let mut seen: HashSet<(usize, usize, usize, &'static str)> = HashSet::new();
    let mut out = Vec::new();

    for seg in segments {
        let mut covered: Vec<(usize, usize)> = Vec::new();

        for piece in &seg.weak_pieces {
            if let Some((canonical, level)) =
                dict::lookup(&piece.text).or_else(|| dict::lookup_locale_tag(&piece.text))
            {
                covered.push((piece.start, piece.end));
                let key = (seg.index, piece.start, piece.end, canonical);
                if seen.insert(key) {
                    out.push(Occurrence {
                        canonical,
                        level,
                        matched: piece.text.clone(),
                        is_filename: seg.is_filename,
                        start: piece.start,
                        end: piece.end,
                    });
                }
            }
        }
        for piece in &seg.atoms {
            let nested = covered
                .iter()
                .any(|&(cs, ce)| piece.start >= cs && piece.end <= ce);
            if nested {
                continue;
            }
            if let Some((canonical, level)) = dict::lookup(&piece.text) {
                let key = (seg.index, piece.start, piece.end, canonical);
                if seen.insert(key) {
                    out.push(Occurrence {
                        canonical,
                        level,
                        matched: piece.text.clone(),
                        is_filename: seg.is_filename,
                        start: piece.start,
                        end: piece.end,
                    });
                }
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::langdetect::tokens::tokenize_path;

    #[test]
    fn finds_full_name_token() {
        let segs = tokenize_path("base\\sound\\soundbanks\\hhpc\\Spanish(Spain)_patch_1.snd");
        let occs = collect_occurrences(&segs);
        assert!(occs.iter().any(|o| o.canonical == "es"));
    }

    #[test]
    fn finds_locale_tag_as_compound_piece() {
        let segs = tokenize_path("loc\\pt-br\\strings.json");
        let occs = collect_occurrences(&segs);
        assert!(occs.iter().any(|o| o.canonical == "pt-br"));
    }

    #[test]
    fn finds_iso3_code() {
        let segs = tokenize_path("localization\\pol\\quests.json");
        let occs = collect_occurrences(&segs);
        assert!(occs
            .iter()
            .any(|o| o.canonical == "pl" && o.level == Level::B));
    }
}
