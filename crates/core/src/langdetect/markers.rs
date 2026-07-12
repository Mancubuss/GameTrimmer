//! Context markers: positive (audio/text/video/font) and negative segment
//! tokens that steer confidence and `LangKind` (see
//! docs/04_implementation_plan.md §5.3, extended with Video/Font per the
//! 2026-07-12 requirement change).
//!
//! Markers match as **whole segment tokens**, never substrings, and a match
//! anywhere along the path affects the whole file (not just the adjacent
//! segment) — except for "closest wins" when several marker categories are
//! present, which picks the category found in the segment nearest the file.

use crate::langdetect::tokens::Segment;

pub const NEGATIVE: &[&str] = &[
    "art",
    "arts",
    "models",
    "meshes",
    "textures",
    "materials",
    "units",
    "structures",
    "buildings",
    "history",
    "decisions",
    "events",
    "missions",
    "campaigns",
    "maps",
    "terrain",
    "scripts",
    "music",
    "shaders",
    "animations",
];

/// The one negative marker that a confirmed language family is allowed to
/// override (localized texture packs are a real, if rare, case). All other
/// negative markers block flagging unconditionally.
pub const OVERRIDABLE_NEGATIVE: &str = "textures";

pub const POSITIVE_AUDIO: &[&str] = &[
    "sound",
    "sounds",
    "soundbanks",
    "audio",
    "voice",
    "voices",
    "vo",
    "speech",
    "dub",
    "dubbing",
    "wwise",
    "fmod",
];

pub const POSITIVE_TEXT: &[&str] = &[
    "text",
    "texts",
    "strings",
    "subtitles",
    "subs",
    "caption",
    "captions",
    "closecaption",
    "translations",
    "fonts",
];

/// Generic "this is localized" indicators: they confirm a positive marker
/// is present (same scoring boost as `POSITIVE_TEXT`) but, unlike
/// `sound`/`voice`/`subtitles`/`video`/`font`, they do not themselves
/// describe the asset's *kind* — a folder named `Localization` or
/// `Languages` says nothing about whether the files inside are audio,
/// text, or video. Keeping these separate from `POSITIVE_TEXT` fixes a
/// real bug found via the corpus regression (`tests/corpus/corpus.rs`):
/// a file such as `...\Localization\DEU\dialog_deu.upk` inside an `Audio\`
/// tree was mis-typed `Text` (from the closer "localization" word) instead
/// of `Audio`, and `lang_fr_voice.archive` was mis-typed `Text` (from
/// "lang") instead of `Audio` (from "voice") when both markers landed in
/// the same path segment. A generic marker only decides `kind` when NO
/// content-type-specific marker (audio/text/video/font) exists anywhere
/// on the path — see `MarkerContext::closest()`.
pub const POSITIVE_LOC_GENERIC: &[&str] = &[
    "loc",
    "localization",
    "localized",
    "locale",
    "lang",
    "language",
    "languages",
    "l10n",
    "i18n",
];

pub const POSITIVE_VIDEO: &[&str] = &[
    "movies",
    "movie",
    "videos",
    "video",
    "cinematics",
    "cutscenes",
    "fmv",
];

// Note: "fonts" is deliberately in POSITIVE_TEXT too (a `fonts/` folder is a
// legitimate text-localization signal on its own); POSITIVE_FONT additionally
// covers the singular "font" token so `font_schinese.ttf` matches.
pub const POSITIVE_FONT: &[&str] = &["font", "fonts"];

pub const VIDEO_EXTENSIONS: &[&str] = &["bik", "bk2", "usm", "wmv", "webm", "ogv"];
pub const FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "fnt"];
/// Subtitle-file extensions: unambiguously text even when they sit inside a
/// `Movies`/`Cutscenes` folder — found via the corpus regression
/// (`tests/corpus/corpus.rs`): `rerelease\baseq2\video\eou6__ru.srt` has no
/// "subtitles"/"loc" word anywhere on its path, only the enclosing `video`
/// folder marker, so without this extension reinforcement it resolves to
/// `Video` even though a `.srt` file plainly cannot contain video.
pub const TEXT_EXTENSIONS: &[&str] = &["srt", "vtt"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerKind {
    Audio,
    Text,
    Video,
    Font,
}

#[derive(Debug, Clone)]
pub struct MarkerHit {
    pub kind: MarkerKind,
    /// Segment index where the marker was found (higher = closer to file).
    pub seg_index: usize,
    pub word: String,
}

#[derive(Debug, Clone, Default)]
pub struct MarkerContext {
    /// Non-overridable negative marker found somewhere on the path.
    pub blocked: bool,
    /// The specific negative words found (used to detect the
    /// textures-only-with-family exception).
    pub negative_words: Vec<String>,
    pub audio: Option<MarkerHit>,
    pub text: Option<MarkerHit>,
    pub video: Option<MarkerHit>,
    pub font: Option<MarkerHit>,
    /// Closest generic "this is localized" marker (`POSITIVE_LOC_GENERIC`).
    /// Contributes to `has_any()`/scoring but only decides `kind` as a
    /// last-resort fallback — see `closest()`.
    pub generic_loc: Option<MarkerHit>,
}

impl MarkerContext {
    fn consider(&mut self, kind: MarkerKind, seg_index: usize, word: &str) {
        let hit = MarkerHit {
            kind,
            seg_index,
            word: word.to_string(),
        };
        let slot = match kind {
            MarkerKind::Audio => &mut self.audio,
            MarkerKind::Text => &mut self.text,
            MarkerKind::Video => &mut self.video,
            MarkerKind::Font => &mut self.font,
        };
        // Strict `>` (not `>=`) so that a tie at the same segment index keeps
        // whichever hit was recorded first — word markers are scanned before
        // extension-based reinforcement, so a real word like "font" wins
        // over the less descriptive ".ttf" for the `reason` text.
        let better = match slot {
            None => true,
            Some(existing) => seg_index > existing.seg_index,
        };
        if better {
            *slot = Some(hit);
        }
    }

    fn consider_generic(&mut self, seg_index: usize, word: &str) {
        // Tagged `Text` because that is the fallback kind if this ends up
        // being the only marker present at all — see `closest()`.
        let hit = MarkerHit {
            kind: MarkerKind::Text,
            seg_index,
            word: word.to_string(),
        };
        let better = match &self.generic_loc {
            None => true,
            Some(existing) => seg_index > existing.seg_index,
        };
        if better {
            self.generic_loc = Some(hit);
        }
    }

    /// The closest content-type-specific marker hit (audio/text/video/font
    /// word — NOT a generic "this is localized" word), used to pick
    /// `LangKind`. See `closest()` for the generic-marker fallback.
    pub fn closest_hit(&self) -> Option<&MarkerHit> {
        [&self.audio, &self.text, &self.video, &self.font]
            .into_iter()
            .flatten()
            .max_by_key(|h| h.seg_index)
    }

    /// Best marker word to mention in a human-readable `reason` string:
    /// the closest specific hit if any, otherwise the closest generic
    /// localization marker (only for messaging — it never overrides the
    /// specific hit for `kind`).
    pub fn closest_any_hit(&self) -> Option<&MarkerHit> {
        self.closest_hit().or(self.generic_loc.as_ref())
    }

    /// The `kind` decision: a content-type-specific marker (audio/text/
    /// video/font) always wins when present. Only when none exists
    /// anywhere on the path does a generic "this is localized" marker
    /// (`loc`/`lang`/`language`/...) fall back to `Text` — such words
    /// confirm localization but say nothing about audio/video/font, so
    /// they must never outrank a real content-type marker even if they
    /// happen to sit in a segment closer to the file.
    pub fn closest(&self) -> Option<MarkerKind> {
        self.closest_hit()
            .map(|h| h.kind)
            .or(self.generic_loc.as_ref().map(|_| MarkerKind::Text))
    }

    pub fn has_any(&self) -> bool {
        self.audio.is_some()
            || self.text.is_some()
            || self.video.is_some()
            || self.font.is_some()
            || self.generic_loc.is_some()
    }

    /// True only if the sole negative marker seen is the overridable
    /// "textures" one (i.e. no other, stricter negative marker is present).
    pub fn only_overridable_negative(&self) -> bool {
        !self.negative_words.is_empty()
            && self
                .negative_words
                .iter()
                .all(|w| w == OVERRIDABLE_NEGATIVE)
    }
}

/// Scans all segments for marker words, then folds in extension-based
/// reinforcement for video/font when the file name itself carries a
/// recognized language token (`has_filename_lang_token`).
pub fn scan_markers(
    segments: &[Segment],
    has_filename_lang_token: bool,
    extension: Option<&str>,
) -> MarkerContext {
    let mut ctx = MarkerContext::default();

    for seg in segments {
        for atom in &seg.atoms {
            let word = atom.text.as_str();
            if NEGATIVE.contains(&word) {
                if word != OVERRIDABLE_NEGATIVE {
                    ctx.blocked = true;
                }
                ctx.negative_words.push(word.to_string());
            }
            if POSITIVE_AUDIO.contains(&word) {
                ctx.consider(MarkerKind::Audio, seg.index, word);
            }
            if POSITIVE_TEXT.contains(&word) {
                ctx.consider(MarkerKind::Text, seg.index, word);
            }
            if POSITIVE_LOC_GENERIC.contains(&word) {
                ctx.consider_generic(seg.index, word);
            }
            if POSITIVE_VIDEO.contains(&word) {
                ctx.consider(MarkerKind::Video, seg.index, word);
            }
            if POSITIVE_FONT.contains(&word) {
                ctx.consider(MarkerKind::Font, seg.index, word);
            }
        }
    }

    if has_filename_lang_token {
        if let Some(last) = segments.last() {
            if let Some(ext) = extension {
                if VIDEO_EXTENSIONS.contains(&ext) {
                    ctx.consider(MarkerKind::Video, last.index, &format!(".{ext}"));
                }
                if FONT_EXTENSIONS.contains(&ext) {
                    ctx.consider(MarkerKind::Font, last.index, &format!(".{ext}"));
                }
                if TEXT_EXTENSIONS.contains(&ext) {
                    ctx.consider(MarkerKind::Text, last.index, &format!(".{ext}"));
                }
            }
        }
    }

    ctx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::langdetect::tokens::tokenize_path;

    #[test]
    fn detects_negative_marker() {
        let segs = tokenize_path("Art\\Units\\King Spanish\\unit.flc");
        let ctx = scan_markers(&segs, false, None);
        assert!(ctx.blocked);
    }

    #[test]
    fn textures_negative_is_overridable_only() {
        let segs = tokenize_path("textures\\ui\\de\\panel.dds");
        let ctx = scan_markers(&segs, false, None);
        assert!(!ctx.blocked, "textures alone must not hard-block");
        assert!(ctx.only_overridable_negative());
    }

    #[test]
    fn video_extension_reinforces_only_with_filename_lang_token() {
        // No word marker anywhere here — only the extension can supply one.
        let segs = tokenize_path("data\\intro_german.bik");

        let with_token = scan_markers(&segs, true, Some("bik"));
        assert!(
            with_token.video.is_some(),
            "extension + lang token in filename should reinforce"
        );

        let without_token = scan_markers(&segs, false, Some("bik"));
        assert!(
            without_token.video.is_none(),
            "extension alone (no lang token in filename) must not reinforce"
        );
    }

    #[test]
    fn closest_marker_wins() {
        let segs = tokenize_path("sound\\text\\voice_line.wav");
        let ctx = scan_markers(&segs, false, None);
        assert_eq!(ctx.closest(), Some(MarkerKind::Audio));
    }
}
