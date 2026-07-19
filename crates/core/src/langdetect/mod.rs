//! Localization detection engine: finds files that belong to a specific
//! language's localization (voice-over, subtitles, UI text, cinematics,
//! fonts) while never flagging ordinary game resources whose names merely
//! resemble language identifiers. See docs/04_implementation_plan.md §5.
//!
//! Zero false positives on ordinary game resources matters more than
//! completeness: a bare two-letter code (`it`, `no`, `de`, `es`, ...) never
//! flags on its own, three-letter ISO codes need a positive context marker
//! nearby, and only full names / Steam folder names / explicit locale tags
//! are trusted alone — see `dict.rs` for the trust-level rationale.

mod dict;
mod family;
mod markers;
mod occurrences;
mod tokens;

#[cfg(test)]
mod tests;

use std::collections::HashSet;

use crate::scanner::FileEntry;

use dict::normalize_lang_key;
use family::FamilyHit;
use markers::{scan_markers, MarkerContext, MarkerKind};
use occurrences::{collect_occurrences, Occurrence};
use tokens::{file_extension, tokenize_path};

/// What kind of localization asset a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LangKind {
    /// Voice-over / audio banks.
    Audio,
    /// Subtitles, captions, UI strings, localized documents.
    Text,
    /// Cinematics / cutscenes / full-motion video.
    Video,
    /// Localized font files.
    Font,
    /// Clearly a localization file, but the asset kind is unclear.
    Unknown,
}

/// One localization finding for a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LangFinding {
    /// Normalized language key, lowercase (e.g. "es", "pt-br", "de").
    pub lang_tag: String,
    pub kind: LangKind,
    /// 0-100.
    pub confidence: u8,
    /// Short human-readable explanation of why this file was flagged,
    /// e.g. "токен 'Spanish(Spain)' + маркер 'soundbanks'".
    pub reason: String,
}

/// The detection engine. Holds the language dictionary and the keep-list.
pub struct LangDetector {
    keep: HashSet<String>,
}

impl LangDetector {
    /// Detector with the default keep-list (Ukrainian and English — never flagged).
    pub fn new() -> Self {
        let keep: Vec<String> = dict::KEEP_DEFAULT.iter().map(|s| s.to_string()).collect();
        Self::with_keep_list(&keep)
    }

    /// Detector with a custom keep-list of normalized language keys
    /// (e.g. ["uk", "en"]). Files of kept languages are never returned.
    pub fn with_keep_list(keep: &[String]) -> Self {
        let keep = keep.iter().map(|s| normalize_lang_key(s)).collect();
        Self { keep }
    }

    /// Analyzes all files of ONE game together (sibling context is required
    /// for the language-family heuristic). Returns `(index into files,
    /// finding)` pairs for files identified as removable localizations.
    pub fn analyze_game(&self, files: &[FileEntry]) -> Vec<(usize, LangFinding)> {
        let seg_lists: Vec<_> = files.iter().map(|f| tokenize_path(&f.rel_path)).collect();
        let occ_lists: Vec<Vec<Occurrence>> =
            seg_lists.iter().map(|s| collect_occurrences(s)).collect();
        let family_hits = family::compute_family(&seg_lists, &occ_lists, &self.keep);

        let mut out = Vec::with_capacity(files.len());
        for (i, (file, segs)) in files.iter().zip(seg_lists.iter()).enumerate() {
            let ext = file_extension(&file.rel_path);

            // Executable code is never a removable localization, whatever
            // its name says: NVIDIA Streamline ships `sl.dlss.dll` ("sl" is
            // not Slovenian) and installers routinely embed helper DLLs.
            // The one exception: .NET satellite assemblies
            // (`de\Foo.resources.dll`) exist *only* as per-language
            // resource containers.
            let is_satellite_assembly = file.rel_path.to_lowercase().ends_with(".resources.dll");
            if matches!(ext.as_deref(), Some("dll") | Some("exe")) && !is_satellite_assembly {
                continue;
            }
            // A file living under a keep-language folder belongs to that
            // language, no matter what its own name resembles: GRID keeps
            // Japanese-named engine cues (`collision_drift_jpn_01.raw`)
            // inside `audio\speech\en\` — the `en\` folder wins. The same
            // goes for a file whose own name carries an unambiguous (A/B)
            // keep-language token: `ai_voice_sk_vo_english.spk` is English
            // VO ("sk" is a mission code), `*_LOC_INT.upk` is the English
            // master locale.
            let named_keep_language = occ_lists[i].iter().any(|occ| {
                occ.is_filename
                    && !matches!(occ.level, dict::Level::C)
                    && self.keep.contains(occ.canonical)
            });
            if named_keep_language || self.under_keep_language_folder(segs) {
                continue;
            }

            let has_filename_lang_token = occ_lists[i].iter().any(|o| o.is_filename);
            let ctx = scan_markers(segs, has_filename_lang_token, ext.as_deref());

            if let Some(finding) =
                decide_finding(&occ_lists[i], &ctx, family_hits.get(&i), &self.keep)
            {
                out.push((i, finding));
            }
        }
        out
    }

    /// True if any directory segment of the path *is* (entirely) a
    /// keep-language token: `en\`, `english\`, `int\` (the Unreal-style
    /// English master locale), `uk\`, ...
    fn under_keep_language_folder(&self, segs: &[tokens::Segment]) -> bool {
        segs.iter()
            .filter(|seg| !seg.is_filename)
            .any(|seg| match dict::lookup(&seg.lower) {
                Some((canonical, _)) => self.keep.contains(canonical),
                None => false,
            })
    }
}

impl Default for LangDetector {
    fn default() -> Self {
        Self::new()
    }
}

fn marker_kind_to_lang_kind(kind: MarkerKind) -> LangKind {
    match kind {
        MarkerKind::Audio => LangKind::Audio,
        MarkerKind::Text => LangKind::Text,
        MarkerKind::Video => LangKind::Video,
        MarkerKind::Font => LangKind::Font,
    }
}

fn append_marker_note(reason: &str, ctx: &MarkerContext) -> String {
    match ctx.closest_any_hit() {
        Some(hit) => format!("{reason}; маркер '{}'", hit.word),
        None => reason.to_string(),
    }
}

/// Best non-family candidate among a file's recognized occurrences.
///
/// Recalibrated after the 2026-07-16 screenshot report: the report showed
/// that a language token in a *file name*, backed only by an asset-kind
/// marker, is wrong far more often than right (`Sounds.nob`, `Dan.bank`,
/// `victory_german.webm`, `M20_Russian_Standoff10.sub`, `9mm_czech.bnk`,
/// `BKG_ItalianCoast.bank` — all game content, all had markers). What
/// separates real localizations is *structural* context, so:
///
/// - A token directly paired with a generic loc word in the same name
///   (`..._LOC_JPN.upk`) is self-sufficient (90).
/// - A filename token otherwise needs a generic localization *folder*
///   (`loc\`, `Localization\`, `LANGUAGE\`) on its path: A→90, B→80, C→70.
///   Asset-kind markers (`sound`, `movies`, `fonts`) alone no longer
///   qualify a filename token — those cases must earn a language family
///   instead (see `family.rs`).
/// - A directory segment that *is* the language token (`spanish\`, `pol\`)
///   keeps close to the old scoring: A→90 with any marker / 75 bare,
///   B→80 with any marker, C→75 with a generic localization folder.
/// - A token embedded in a longer directory name (`french_colonization\`,
///   `ItalianCypress\`) needs a generic localization folder as well.
fn best_non_family_candidate(
    occs: &[Occurrence],
    ctx: &MarkerContext,
    keep: &HashSet<String>,
) -> Option<(&'static str, u8, String)> {
    let mut best: Option<(u8, &'static str, String)> = None;

    for occ in occs {
        if keep.contains(occ.canonical) {
            continue;
        }
        // The explicit loc-pair upgrade covers `_LOC_JPN`-style naming.
        // For bare two-letter codes only a *generic* loc word qualifies
        // (`lang_es-es_text.archive`); loc-specific vocabulary can
        // neighbor ordinary words (`intro_no_subtitles.bik` — "no" is the
        // English word, not Norwegian).
        let loc_paired =
            occ.loc_adjacent && (!matches!(occ.level, dict::Level::C) || occ.loc_adjacent_generic);
        let score = if loc_paired {
            Some(if matches!(occ.level, dict::Level::C) {
                75
            } else {
                90
            })
        } else if occ.is_filename {
            match occ.level {
                // Region-qualified and Steam-style aliases
                // (`Spanish(Spain)`, `pt-br`, `schinese`) are
                // localization-industry vocabulary — trustworthy in a
                // filename even without a loc folder, unlike plain full
                // names. See `dict::is_industry_alias`.
                dict::Level::A if dict::is_industry_alias(&occ.matched) && ctx.has_any() => {
                    Some(90)
                }
                dict::Level::A if dict::is_industry_alias(&occ.matched) => Some(75),
                dict::Level::A if ctx.generic_loc_in_folder => Some(90),
                dict::Level::B if ctx.generic_loc_in_folder => Some(80),
                // An explicit "<asset-word>_<lang>" pairing in the stem:
                // `sounds_spa.pck`, `boot_sound_ita_patch_01.forge`,
                // `VO_Cutscenes_FR.bank`.
                dict::Level::A if occ.asset_adjacent => Some(90),
                dict::Level::B if occ.asset_adjacent => Some(80),
                dict::Level::C if occ.asset_adjacent => Some(70),
                // A file whose entire stem IS a bare code, inside a
                // localization folder: `webview2\Locales\fi.pak`. Embedded
                // codes (`locale\...\id_xpbls.xnb`) stay family-gated.
                dict::Level::C if occ.whole_stem && ctx.generic_loc_in_folder => Some(70),
                // Level C filename tokens stay family-gated even inside a
                // loc folder: `locale\creatures\pl_f\id_xpbls.xnb` — "id"
                // is an id, not Indonesian (corpus calibration).
                _ => None,
            }
        } else if occ.whole_segment {
            match occ.level {
                dict::Level::A if ctx.has_any() => Some(90),
                dict::Level::A => Some(75),
                dict::Level::B if ctx.has_any() => Some(80),
                dict::Level::C if ctx.generic_loc_in_folder => Some(75),
                _ => None,
            }
        } else {
            match occ.level {
                dict::Level::A if ctx.generic_loc_in_folder => Some(90),
                dict::Level::B if ctx.generic_loc_in_folder => Some(80),
                _ => None,
            }
        };
        let Some(score) = score else { continue };

        let reason = if loc_paired {
            format!("токен '{}' у явній loc-парі", occ.matched)
        } else if ctx.has_any() {
            let word = ctx
                .closest_any_hit()
                .map(|h| h.word.as_str())
                .unwrap_or_default();
            format!("токен '{}' + маркер '{}'", occ.matched, word)
        } else {
            format!("токен '{}' (мовна тека без явного контексту)", occ.matched)
        };

        let is_better = match &best {
            Some((existing_score, _, _)) => score > *existing_score,
            None => true,
        };
        if is_better {
            best = Some((score, occ.canonical, reason));
        }
    }

    best.map(|(score, canonical, reason)| (canonical, score, reason))
}

/// Combines marker context, plain-occurrence scoring, and family evidence
/// into a final decision for one file.
fn decide_finding(
    occs: &[Occurrence],
    ctx: &MarkerContext,
    family: Option<&FamilyHit>,
    keep: &HashSet<String>,
) -> Option<LangFinding> {
    // A strict negative marker (anything but the overridable "textures")
    // blocks flagging unconditionally, regardless of family evidence.
    if ctx.blocked {
        return None;
    }

    let kind = ctx
        .closest()
        .map(marker_kind_to_lang_kind)
        .unwrap_or(LangKind::Unknown);

    // "textures" is negative but a confirmed language family may still
    // override it — that is the ONLY path through which such a file can be
    // flagged (plain marker/bare scoring stays blocked).
    if ctx.only_overridable_negative() {
        return family.map(|fh| LangFinding {
            lang_tag: fh.canonical.to_string(),
            kind,
            confidence: fh.confidence,
            reason: append_marker_note(&fh.reason, ctx),
        });
    }

    let bare = best_non_family_candidate(occs, ctx, keep);

    let (canonical, confidence, reason) = match (family, bare) {
        (Some(f), Some((b_canonical, b_score, b_reason))) => {
            if f.confidence >= b_score {
                (
                    f.canonical,
                    f.confidence,
                    append_marker_note(&f.reason, ctx),
                )
            } else {
                (b_canonical, b_score, b_reason)
            }
        }
        (Some(f), None) => (
            f.canonical,
            f.confidence,
            append_marker_note(&f.reason, ctx),
        ),
        (None, Some((b_canonical, b_score, b_reason))) => (b_canonical, b_score, b_reason),
        (None, None) => return None,
    };

    Some(LangFinding {
        lang_tag: canonical.to_string(),
        kind,
        confidence,
        reason,
    })
}
