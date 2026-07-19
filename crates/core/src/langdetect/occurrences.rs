//! Recognized-language-token occurrences within a tokenized path.

use std::collections::HashSet;

use crate::langdetect::dict::{self, Level};
use crate::langdetect::markers::{
    LOC_SPECIFIC, POSITIVE_AUDIO, POSITIVE_FONT, POSITIVE_LOC_GENERIC, POSITIVE_TEXT,
    POSITIVE_VIDEO,
};
use crate::langdetect::tokens::Segment;

#[derive(Debug, Clone)]
pub struct Occurrence {
    pub canonical: &'static str,
    pub level: Level,
    /// The exact text that matched (for building `reason` strings).
    pub matched: String,
    pub is_filename: bool,
    /// Index of the path segment this occurrence was found in.
    pub seg_index: usize,
    pub start: usize,
    pub end: usize,
    /// The match spans the entire path segment (`spanish\`, `pol\`) — the
    /// segment *is* the language token, as opposed to a token embedded in a
    /// longer name (`french_colonization\`, `victory_german.webm`).
    pub whole_segment: bool,
    /// The match spans the entire file-name stem (`fi.pak`, `Czech.DCL`) —
    /// the name minus extension *is* the language token.
    pub whole_stem: bool,
    /// The matched atom directly neighbors a localization word in the same
    /// segment (`..._LOC_JPN.upk`, `lang_de_...`): an explicit
    /// "<loc-word>_<lang>" pairing, trusted like a Level A occurrence.
    pub loc_adjacent: bool,
    /// The neighboring localization word is a *generic* one
    /// (`loc`/`lang`/`locale`...), not merely loc-specific vocabulary like
    /// `subtitles`. Generic words never carry content meaning, so even a
    /// bare two-letter code next to one is a deliberate pick
    /// (`lang_es-es_text.archive`); `subtitles` can neighbor ordinary
    /// words (`intro_no_subtitles.bik` — "no" is English).
    pub loc_adjacent_generic: bool,
    /// The matched atom sits in the file-name *stem* directly next to an
    /// asset-kind marker word (`sounds_spa.pck`, `boot_sound_ita_patch`):
    /// an explicit "<asset-word>_<lang>" pairing. Weaker than
    /// `loc_adjacent` (asset words are not localization-specific), but
    /// still a deliberate naming pattern — unlike the same words merely
    /// appearing somewhere on the path. Never set for extension atoms
    /// (`Sounds.nob` — "nob" is a format extension, not a language pick).
    pub asset_adjacent: bool,
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
        let seg_stem_end = seg.lower.find('.').unwrap_or(seg.lower.len());

        for piece in &seg.weak_pieces {
            let direct = dict::lookup(&piece.text);
            let via_tag = direct
                .is_none()
                .then(|| dict::lookup_locale_tag(&piece.text))
                .flatten();
            if let Some((canonical, level)) = direct.or(via_tag) {
                // A generic locale-tag match tolerates trailing parts
                // (`ja_JP_TRADITIONAL`), but then the piece as a whole is
                // NOT a clean language token — `pl_ri_s##depth` must not
                // count as "this folder IS the language" the way `pl_ri`
                // or a direct dictionary hit would.
                let clean_tag = direct.is_some() || piece.text.split(['-', '_']).count() == 2;
                // `Chinese(Traditional)\` — a leading language name whose
                // only companion is a parenthesized qualifier IS the whole
                // segment for trust purposes.
                let paren_qualified = piece.start == 0
                    && seg.lower[piece.end..].starts_with('(')
                    && seg.lower.ends_with(')');
                covered.push((piece.start, piece.end));
                let key = (seg.index, piece.start, piece.end, canonical);
                if seen.insert(key) {
                    out.push(Occurrence {
                        canonical,
                        level,
                        matched: piece.text.clone(),
                        is_filename: seg.is_filename,
                        seg_index: seg.index,
                        start: piece.start,
                        end: piece.end,
                        whole_segment: paren_qualified
                            || (piece.start == 0 && piece.end == seg.lower.len() && clean_tag),
                        whole_stem: piece.start == 0 && piece.end == seg_stem_end && clean_tag,
                        loc_adjacent: false,
                        loc_adjacent_generic: false,
                        asset_adjacent: false,
                    });
                }
            }
        }
        // Camel-joined locale tags (`deDE.json`, `frFR`): the CamelCase
        // split yields two adjacent atoms with NO delimiter between them
        // (`de`+`de`), so neither the weak-piece pass (needs `-`/`_`) nor
        // the plain atom pass (bare 2-letter, family-gated) recognizes the
        // pair as the self-sufficient locale tag it is. Detect the pair
        // explicitly: a 2-letter language atom immediately followed
        // (byte-adjacent) by a 2-letter alphabetic region atom.
        for window in seg.atoms.windows(2) {
            let (a, b) = (&window[0], &window[1]);
            if b.start != a.end || b.text.len() != 2 {
                continue;
            }
            if !b.text.chars().all(|c| c.is_ascii_alphabetic()) {
                continue;
            }
            if let Some((canonical, Level::C)) = dict::lookup(&a.text) {
                covered.push((a.start, b.end));
                let key = (seg.index, a.start, b.end, canonical);
                if seen.insert(key) {
                    out.push(Occurrence {
                        canonical,
                        level: Level::A,
                        matched: format!("{}{}", a.text, b.text),
                        is_filename: seg.is_filename,
                        seg_index: seg.index,
                        start: a.start,
                        end: b.end,
                        whole_segment: a.start == 0 && b.end == seg.lower.len(),
                        whole_stem: a.start == 0 && b.end == seg_stem_end,
                        loc_adjacent: false,
                        loc_adjacent_generic: false,
                        asset_adjacent: false,
                    });
                }
            }
        }

        for (atom_idx, piece) in seg.atoms.iter().enumerate() {
            let nested = covered
                .iter()
                .any(|&(cs, ce)| piece.start >= cs && piece.end <= ce);
            if nested {
                continue;
            }
            if let Some((canonical, level)) = dict::lookup(&piece.text) {
                let stem_end = seg_stem_end;
                let neighbor_atoms = || {
                    [atom_idx.checked_sub(1), atom_idx.checked_add(1)]
                        .into_iter()
                        .flatten()
                        .filter_map(|i| seg.atoms.get(i))
                };
                let neighbors_generic_loc_atom =
                    neighbor_atoms().any(|a| POSITIVE_LOC_GENERIC.contains(&a.text.as_str()));
                let neighbors_loc_atom = neighbors_generic_loc_atom
                    || neighbor_atoms().any(|a| LOC_SPECIFIC.contains(&a.text.as_str()));
                // LOC_SPECIFIC words are handled by the (level-gated)
                // loc-pair check above — they must not double as asset
                // adjacency, or `intro_no_subtitles.bik` would sneak a
                // bare "no" through the weaker asset-pair path.
                let neighbors_asset_atom = piece.end <= stem_end
                    && neighbor_atoms().any(|a| {
                        let word = a.text.as_str();
                        !LOC_SPECIFIC.contains(&word)
                            && (POSITIVE_AUDIO.contains(&word)
                                || POSITIVE_TEXT.contains(&word)
                                || POSITIVE_VIDEO.contains(&word)
                                || POSITIVE_FONT.contains(&word))
                    });

                let key = (seg.index, piece.start, piece.end, canonical);
                if seen.insert(key) {
                    out.push(Occurrence {
                        canonical,
                        level,
                        matched: piece.text.clone(),
                        is_filename: seg.is_filename,
                        seg_index: seg.index,
                        start: piece.start,
                        end: piece.end,
                        whole_segment: piece.start == 0 && piece.end == seg.lower.len(),
                        whole_stem: piece.start == 0 && piece.end == stem_end,
                        loc_adjacent: neighbors_loc_atom,
                        loc_adjacent_generic: neighbors_generic_loc_atom,
                        asset_adjacent: neighbors_asset_atom,
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
        assert!(occs
            .iter()
            .any(|o| o.canonical == "pt-br" && o.whole_segment));
    }

    #[test]
    fn finds_iso3_code() {
        let segs = tokenize_path("localization\\pol\\quests.json");
        let occs = collect_occurrences(&segs);
        assert!(occs
            .iter()
            .any(|o| o.canonical == "pl" && o.level == Level::B && o.whole_segment));
    }

    #[test]
    fn marks_loc_adjacent_language_atom() {
        let segs = tokenize_path("Game\\CookedPCConsole\\Opening_LOC_JPN.upk");
        let occs = collect_occurrences(&segs);
        assert!(occs.iter().any(|o| o.canonical == "ja" && o.loc_adjacent));
    }

    #[test]
    fn embedded_token_is_not_whole_segment() {
        let segs = tokenize_path("entities\\french_colonization\\lamp01.ent");
        let occs = collect_occurrences(&segs);
        let fr = occs
            .iter()
            .find(|o| o.canonical == "fr")
            .expect("french token should be recognized");
        assert!(!fr.whole_segment);
        assert!(!fr.loc_adjacent);
    }
}
