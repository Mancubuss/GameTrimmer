//! Recognized-language-token occurrences within a tokenized path.

use std::collections::HashSet;

use crate::langdetect::data::LangData;
use crate::langdetect::dict::Level;
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
pub fn collect_occurrences(data: &LangData, segments: &[Segment]) -> Vec<Occurrence> {
    let mut seen: HashSet<(usize, usize, usize, &'static str)> = HashSet::new();
    let mut out = Vec::new();

    for seg in segments {
        let mut covered: Vec<(usize, usize)> = Vec::new();
        let seg_stem_end = seg.lower.find('.').unwrap_or(seg.lower.len());

        for piece in &seg.weak_pieces {
            let direct = data.lookup(&piece.text);
            let via_tag = direct
                .is_none()
                .then(|| data.lookup_locale_tag(&piece.text))
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
        // Locale tags that neither pass above can see, because the tag sits
        // *inside* a longer name rather than being the whole piece:
        //
        // - Camel-joined (`deDE.json`, `frFR`): the CamelCase split yields
        //   two atoms with NO delimiter between them, so the weak-piece pass
        //   (which needs `-`/`_`) never forms the tag.
        // - Delimiter-separated but prefixed (`lic_da-DK.html`,
        //   `visconfig_bg_BG.qm`, `actions_ro_ro.json`): the weak-piece pass
        //   splits only on `. ( ) [ ]`, so the whole `lic_da-dk` stays glued
        //   and never matches; the strong split then scatters it into `lic`,
        //   `da`, `dk`, leaving only a family-gated bare code behind.
        //
        // Both are found the same way: a language atom immediately followed
        // by a 2-letter region atom, either byte-adjacent or with exactly one
        // `-`/`_` between them.
        //
        // The two cases are NOT trusted equally. `da-DK` written with its
        // delimiter is the canonical BCP-47 spelling and is only accepted
        // when the dictionary lists that exact tag as a Level A alias — so
        // `pl_us` (Polish language, American region: not a real pairing)
        // stays out. The glued form is the older, looser rule and keeps its
        // original reach.
        for window in seg.atoms.windows(2) {
            let (a, b) = (&window[0], &window[1]);
            if b.text.len() != 2 || !b.text.chars().all(|c| c.is_ascii_alphabetic()) {
                continue;
            }
            let delim = match b.start.checked_sub(a.end) {
                Some(0) => None,
                Some(1) => match seg.lower.as_bytes()[a.end] {
                    sep @ (b'-' | b'_') => Some(sep as char),
                    _ => continue,
                },
                _ => continue,
            };
            let (canonical, matched) = match delim {
                Some(sep) => {
                    let tag = format!("{}{sep}{}", a.text, b.text);
                    match data.lookup(&tag) {
                        Some((canonical, Level::A)) => (canonical, tag),
                        _ => continue,
                    }
                }
                None => match data.lookup(&a.text) {
                    Some((canonical, Level::C)) => (canonical, format!("{}{}", a.text, b.text)),
                    _ => continue,
                },
            };
            covered.push((a.start, b.end));
            let key = (seg.index, a.start, b.end, canonical);
            if seen.insert(key) {
                out.push(Occurrence {
                    canonical,
                    level: Level::A,
                    matched,
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

        // A script or region qualifier that is not a two-letter region code,
        // so the pass above cannot see it: `ChineseTraditional` (glued in
        // CamelCase), `Portuguese(Brazil)` (parenthesized), `es_MX` (the
        // dictionary spells that tag with a hyphen, the file with an
        // underscore).
        //
        // All three are the same failure. The strong/CamelCase split scatters
        // the name into atoms, only the *base* language is a dictionary word
        // on its own, and the qualifier is dropped - so a Traditional file is
        // labelled Simplified and a Mexican one plain Spanish. The label is
        // what the keep-language list is applied to, so a wrong label is not
        // a cosmetic problem: it walks a file the player meant to keep past
        // an intact guard.
        //
        // The span between the two atoms is read back out of the segment and
        // looked up as written, then with `_` normalized to the `-` the
        // dictionary uses. Only a curated Level A alias is accepted, which is
        // what keeps the rule from inventing variants: `de` + `1` in
        // `voice_de_1.pck` spells no alias and stays plain German.
        for window in seg.atoms.windows(2) {
            let (a, b) = (&window[0], &window[1]);
            // A closing parenthesis belongs to the qualifier that opened it;
            // the atom itself ends before it (`portuguese(brazil` + `)`).
            let end =
                if seg.lower[b.end..].starts_with(')') && seg.lower[a.start..b.end].contains('(') {
                    b.end + 1
                } else {
                    b.end
                };
            let span = &seg.lower[a.start..end];
            let as_written = data.lookup(span);
            let hyphenated = as_written
                .is_none()
                .then(|| data.lookup(&span.replace('_', "-")))
                .flatten();
            let Some((canonical, Level::A)) = as_written.or(hyphenated) else {
                continue;
            };
            covered.push((a.start, end));
            let key = (seg.index, a.start, end, canonical);
            if seen.insert(key) {
                out.push(Occurrence {
                    canonical,
                    level: Level::A,
                    matched: span.to_string(),
                    is_filename: seg.is_filename,
                    seg_index: seg.index,
                    start: a.start,
                    end,
                    whole_segment: a.start == 0 && end == seg.lower.len(),
                    whole_stem: a.start == 0 && end == seg_stem_end,
                    loc_adjacent: false,
                    loc_adjacent_generic: false,
                    asset_adjacent: false,
                });
            }
        }

        // Locale tags written solid: `presentations_ptbr.arch06`,
        // `presentations_eses`, `presentations_esla`. The strong split leaves
        // one opaque atom, so neither pass above can see the pair inside it -
        // the weak-piece pass needs a `-`/`_` and the adjacency pass above
        // needs two atoms to sit next to each other.
        //
        // Only a pair the dictionary already curates as a Level A alias is
        // accepted, and only when the atom is not a word in its own right:
        // `pt-br` is such a tag, `pt-xx` is not, so a four-letter word cannot
        // wander in unless it happens to spell a real locale.
        //
        // But it is trusted one notch *lower* than the CamelCase form it
        // mirrors, not equally. `deDE` carries a case boundary that says
        // "these are two fields"; all-lowercase `ptbr` says nothing at all,
        // and the glued spelling of a real tag can be an ordinary word -
        // `fa-ir` is "fair", `he-il` is "heil". Level C hands the decision to
        // the sibling family, which is what recognizes a per-language archive
        // set anyway, and leaves a lone `fair.pak` alone.
        for piece in &seg.atoms {
            let nested = covered
                .iter()
                .any(|&(cs, ce)| piece.start >= cs && piece.end <= ce);
            if nested
                || !(4..=8).contains(&piece.text.len())
                || !piece.text.chars().all(|c| c.is_ascii_alphabetic())
                || data.lookup(&piece.text).is_some()
            {
                continue;
            }
            let Some(canonical) = (2..=piece.text.len() - 2).find_map(|split| {
                let tag = format!("{}-{}", &piece.text[..split], &piece.text[split..]);
                match data.lookup(&tag) {
                    Some((canonical, Level::A)) => Some(canonical),
                    _ => None,
                }
            }) else {
                continue;
            };
            covered.push((piece.start, piece.end));
            let key = (seg.index, piece.start, piece.end, canonical);
            if seen.insert(key) {
                out.push(Occurrence {
                    canonical,
                    level: Level::C,
                    matched: piece.text.clone(),
                    is_filename: seg.is_filename,
                    seg_index: seg.index,
                    start: piece.start,
                    end: piece.end,
                    whole_segment: piece.start == 0 && piece.end == seg.lower.len(),
                    whole_stem: piece.start == 0 && piece.end == seg_stem_end,
                    loc_adjacent: false,
                    loc_adjacent_generic: false,
                    asset_adjacent: false,
                });
            }
        }

        for (atom_idx, piece) in seg.atoms.iter().enumerate() {
            let nested = covered
                .iter()
                .any(|&(cs, ce)| piece.start >= cs && piece.end <= ce);
            if nested {
                continue;
            }
            if let Some((canonical, level)) = data.lookup(&piece.text) {
                let stem_end = seg_stem_end;
                let neighbor_atoms = || {
                    [atom_idx.checked_sub(1), atom_idx.checked_add(1)]
                        .into_iter()
                        .flatten()
                        .filter_map(|i| seg.atoms.get(i))
                };
                let neighbors_generic_loc_atom =
                    neighbor_atoms().any(|a| data.loc_generic.contains(a.text.as_str()));
                let neighbors_loc_atom = neighbors_generic_loc_atom
                    || neighbor_atoms().any(|a| data.loc_specific.contains(a.text.as_str()));
                // LOC_SPECIFIC words are handled by the (level-gated)
                // loc-pair check above — they must not double as asset
                // adjacency, or `intro_no_subtitles.bik` would sneak a
                // bare "no" through the weaker asset-pair path.
                let neighbors_asset_atom = piece.end <= stem_end
                    && neighbor_atoms().any(|a| {
                        let word = a.text.as_str();
                        !data.loc_specific.contains(word)
                            && (data.audio.contains(word)
                                || data.text.contains(word)
                                || data.video.contains(word)
                                || data.font.contains(word))
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

    fn occs_for(path: &str) -> Vec<Occurrence> {
        let segs = tokenize_path(path);
        collect_occurrences(&LangData::builtin(), &segs)
    }

    #[test]
    fn finds_full_name_token() {
        let occs = occs_for("base\\sound\\soundbanks\\hhpc\\Spanish(Spain)_patch_1.snd");
        assert!(occs.iter().any(|o| o.canonical == "es"));
    }

    #[test]
    fn finds_locale_tag_as_compound_piece() {
        let occs = occs_for("loc\\pt-br\\strings.json");
        assert!(occs
            .iter()
            .any(|o| o.canonical == "pt-br" && o.whole_segment));
    }

    #[test]
    fn finds_iso3_code() {
        let occs = occs_for("localization\\pol\\quests.json");
        assert!(occs
            .iter()
            .any(|o| o.canonical == "pl" && o.level == Level::B && o.whole_segment));
    }

    #[test]
    fn marks_loc_adjacent_language_atom() {
        let occs = occs_for("Game\\CookedPCConsole\\Opening_LOC_JPN.upk");
        assert!(occs.iter().any(|o| o.canonical == "ja" && o.loc_adjacent));
    }

    #[test]
    fn finds_delimited_locale_tag_inside_a_longer_name() {
        // `lic_da-dk` is one weak piece (the weak split does not break on
        // `-`/`_`) and three atoms (`lic`, `da`, `dk`), so before the pair
        // rule covered delimiters the canonical tag was invisible and only a
        // family-gated bare "da" remained.
        let occs = occs_for("Support\\License\\lic_da-DK.html");
        let tag = occs
            .iter()
            .find(|o| o.canonical == "da")
            .expect("da-DK should be recognized as a locale tag");
        assert_eq!(tag.level, Level::A);
        assert_eq!(tag.matched, "da-dk");
    }

    #[test]
    fn unlisted_language_region_pair_is_not_a_locale_tag() {
        // "pl" is Polish and "us" is a region, but `pl-US` is not a pairing
        // the dictionary lists — accepting it would turn any two adjacent
        // two-letter atoms into self-sufficient evidence.
        let occs = occs_for("data\\map_pl-US.dat");
        assert!(
            !occs
                .iter()
                .any(|o| o.level == Level::A && o.matched.contains('-')),
            "an unlisted language-region pair must not become a Level A tag"
        );
    }

    #[test]
    fn embedded_token_is_not_whole_segment() {
        let occs = occs_for("entities\\french_colonization\\lamp01.ent");
        let fr = occs
            .iter()
            .find(|o| o.canonical == "fr")
            .expect("french token should be recognized");
        assert!(!fr.whole_segment);
        assert!(!fr.loc_adjacent);
    }
}
