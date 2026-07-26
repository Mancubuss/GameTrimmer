//! Context markers: positive (audio/text/video/font) and negative segment
//! tokens that steer confidence and `LangKind` (see
//! docs/04_implementation_plan.md §5.3, extended with Video/Font per the
//! 2026-07-12 requirement change).
//!
//! Markers match as **whole segment tokens**, never substrings, and a match
//! anywhere along the path affects the whole file (not just the adjacent
//! segment) — except for "closest wins" when several marker categories are
//! present, which picks the category found in the segment nearest the file.
//!
//! The marker *word lists* themselves (`markers.negative`, `markers.audio`,
//! ...) live in `l10n_rules.json` at the repo root, not here — this module
//! only holds the scanning logic that consults them via [`LangData`]. Design
//! notes worth knowing before editing that file:
//! - `negative` includes asset-tree words such as `gfx`, `sprites`, `icons`,
//!   `fragments`, `navmesh`, `particles`, `flag(s)` — 2D-asset and geometry
//!   trees where short language-code lookalikes are endemic
//!   (`gfx\anemone-bg.png`, `sprites\...\DoodleKor.png`, `icons\tr\...`) —
//!   plus `internationalization`, since engine-internal ICU locale data
//!   (Unreal `Engine\Content\Internationalization\icudt53l\bg.res`) is
//!   required by the engine itself, not a removable game localization.
//! - `overridable_negative` (`textures`, `gui`) lists negative markers a
//!   confirmed language family is allowed to override: localized texture
//!   packs and per-language UI packs inside a confirmed language folder
//!   (`sds_latam\gui\`) are real, if rare, cases. Every other negative
//!   marker blocks flagging unconditionally.
//! - `loc_generic` (`loc`, `localization`, `lang`, `l10n`, ...) confirms a
//!   positive marker is present (same scoring boost as `text`) but, unlike
//!   `sound`/`voice`/`subtitles`/`video`/`font`, does not itself describe
//!   the asset's *kind* — a folder named `Localization` says nothing about
//!   whether the files inside are audio, text, or video. Keeping it
//!   separate from `text` fixes a real bug found via the corpus regression
//!   (`tests/corpus/corpus.rs`): `...\Localization\DEU\dialog_deu.upk`
//!   inside an `Audio\` tree was mis-typed `Text` (from the closer
//!   "localization" word) instead of `Audio`. A generic marker only decides
//!   `kind` when no content-type-specific marker exists anywhere on the
//!   path — see [`MarkerContext::closest`].
//! - `loc_specific` (`closecaption`, `subtitles`, `dub`, ...) are words
//!   that, while describing an asset kind (they also appear in `text`/
//!   `audio`), are *localization-specific* vocabulary rather than generic
//!   asset words: a `subtitles\` folder is about translated content by
//!   definition, unlike `sound\` or `movies\` which mostly hold ordinary
//!   game assets. These count as strong localization context for the
//!   `loc_adjacent` pairing check and `generic_loc_in_folder` where
//!   `sound`/`movies`/`textures` do not (2026-07-16 report:
//!   `victory_german.webm` under `Movies\` was a false positive; nothing
//!   named `subtitles\*` was).
//! - `font` deliberately also lists `"fonts"`, already present in `text` (a
//!   `fonts/` folder is a legitimate text-localization signal on its own);
//!   `font` additionally covers the singular `"font"` token so
//!   `font_schinese.ttf` matches.
//! - The four `*_extensions` lists name extensions that state a content type
//!   outright. They serve two distinct purposes, and the difference matters:
//!
//!   1. **Deciding `kind`** via `MarkerContext::ext_kind`, for every file.
//!      The extension outranks every marker word, because words describe the
//!      neighbourhood while the extension describes the file itself:
//!      `Movies\Subtitles\..._pt-BR.vtt` is text, `lang\de_DE\sounds\*.wav`
//!      is audio, and `Support\EA Help\Es\Sound_card.htm` is a help page
//!      about a sound card, not a sound. The 2026-07-26 corpus pass found
//!      ~230 such rows where a folder word was overriding the plain fact of
//!      the extension.
//!   2. **Reinforcing context** via `consider`, only when the file's own
//!      name carries a language token. This form feeds `has_any()` and
//!      therefore scoring, so it stays gated: a `.bik` in the same name as a
//!      language token is a deliberate pairing, a `.bik` under some distant
//!      folder is not.
//!
//!   `ext_kind` is deliberately absent from `has_any()` — see its field doc.
//!   An extension says what a file contains, never that it is localized.

use crate::langdetect::data::LangData;
use crate::langdetect::tokens::Segment;

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
    /// A generic localization marker found in a *directory* segment (not
    /// the file name). This is the strong form of context: a `loc\`/
    /// `Localization\`/`LANGUAGE\` folder describes everything inside it,
    /// while the same word inside one file's name (`..._LOC_INT.upk`) only
    /// describes that file. Filename language tokens are trusted against
    /// folder-level localization context only — see
    /// `best_non_family_candidate` in `mod.rs`.
    pub generic_loc_in_folder: bool,
    /// Kind implied by the file's own extension, when the extension names a
    /// content type outright (`.wav`, `.srt`, `.bik`, `.ttf`).
    ///
    /// Deliberately kept out of [`MarkerContext::has_any`]: an extension says
    /// what a file *contains*, never that it is a localization. Letting it
    /// count as context would mean every `.wav` on the disk lends weight to a
    /// weak language token — exactly the false-positive direction the whole
    /// design guards against. It only settles `kind` for a file something
    /// else already decided to flag, and there it is the most reliable
    /// evidence available: a `.wav` is audio however many `Movies\` folders
    /// it sits under, and a `.srt` is text however cinematic its neighbours.
    pub ext_kind: Option<MarkerKind>,
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
        // The extension outranks every word marker: words describe the
        // neighbourhood, the extension describes the file. `Movies\
        // Subtitles\..._pt-BR.vtt` is text, not video, and `lang\de_DE\
        // sounds\Male5a.wav` is audio however the folders read.
        self.ext_kind
            .or_else(|| self.closest_hit().map(|h| h.kind))
            .or(self.generic_loc.as_ref().map(|_| MarkerKind::Text))
    }

    pub fn has_any(&self) -> bool {
        self.audio.is_some()
            || self.text.is_some()
            || self.video.is_some()
            || self.font.is_some()
            || self.generic_loc.is_some()
    }

    /// True only if every negative marker seen is an overridable one
    /// (i.e. no stricter negative marker is present — `blocked` is set
    /// the moment a non-overridable one appears).
    pub fn only_overridable_negative(&self) -> bool {
        !self.negative_words.is_empty() && !self.blocked
    }
}

/// Scans all segments for marker words, then folds in extension-based
/// reinforcement for video/font when the file name itself carries a
/// recognized language token (`has_filename_lang_token`).
pub fn scan_markers(
    data: &LangData,
    segments: &[Segment],
    has_filename_lang_token: bool,
    extension: Option<&str>,
) -> MarkerContext {
    let mut ctx = MarkerContext::default();

    for seg in segments {
        for atom in &seg.atoms {
            let word = atom.text.as_str();
            if data.negative.contains(word) {
                if !data.overridable_negative.contains(word) {
                    ctx.blocked = true;
                }
                ctx.negative_words.push(word.to_string());
            }
            if data.audio.contains(word) {
                ctx.consider(MarkerKind::Audio, seg.index, word);
            }
            if data.text.contains(word) {
                ctx.consider(MarkerKind::Text, seg.index, word);
            }
            if data.loc_generic.contains(word) {
                ctx.consider_generic(seg.index, word);
                if !seg.is_filename {
                    ctx.generic_loc_in_folder = true;
                }
            }
            if data.loc_specific.contains(word) && !seg.is_filename {
                ctx.generic_loc_in_folder = true;
            }
            if data.video.contains(word) {
                ctx.consider(MarkerKind::Video, seg.index, word);
            }
            if data.font.contains(word) {
                ctx.consider(MarkerKind::Font, seg.index, word);
            }
        }
    }

    if let Some(ext) = extension {
        // `ext_kind` settles the kind but never counts as context (see the
        // field doc), so unlike the marker words above it is safe to derive
        // for every file — including one whose language token lives in a
        // folder rather than in its own name.
        ctx.ext_kind = if data.audio_extensions.contains(ext) {
            Some(MarkerKind::Audio)
        } else if data.text_extensions.contains(ext) {
            Some(MarkerKind::Text)
        } else if data.video_extensions.contains(ext) {
            Some(MarkerKind::Video)
        } else if data.font_extensions.contains(ext) {
            Some(MarkerKind::Font)
        } else {
            None
        };

        // The filename-token gate stays on the *marker* form of the same
        // evidence, which does feed `has_any()` and therefore scoring: a
        // `.bik` next to a language token in the same name is a deliberate
        // pairing, while a `.bik` under some distant folder is not.
        if has_filename_lang_token {
            if let Some(last) = segments.last() {
                if data.video_extensions.contains(ext) {
                    ctx.consider(MarkerKind::Video, last.index, &format!(".{ext}"));
                }
                if data.font_extensions.contains(ext) {
                    ctx.consider(MarkerKind::Font, last.index, &format!(".{ext}"));
                }
                if data.text_extensions.contains(ext) {
                    ctx.consider(MarkerKind::Text, last.index, &format!(".{ext}"));
                }
                if data.audio_extensions.contains(ext) {
                    ctx.consider(MarkerKind::Audio, last.index, &format!(".{ext}"));
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

    fn data() -> std::sync::Arc<LangData> {
        LangData::builtin()
    }

    #[test]
    fn detects_negative_marker() {
        let segs = tokenize_path("Art\\Units\\King Spanish\\unit.flc");
        let ctx = scan_markers(&data(), &segs, false, None);
        assert!(ctx.blocked);
    }

    #[test]
    fn textures_negative_is_overridable_only() {
        let segs = tokenize_path("textures\\ui\\de\\panel.dds");
        let ctx = scan_markers(&data(), &segs, false, None);
        assert!(!ctx.blocked, "textures alone must not hard-block");
        assert!(ctx.only_overridable_negative());
    }

    #[test]
    fn video_extension_reinforces_only_with_filename_lang_token() {
        // No word marker anywhere here — only the extension can supply one.
        let segs = tokenize_path("data\\intro_german.bik");

        let with_token = scan_markers(&data(), &segs, true, Some("bik"));
        assert!(
            with_token.video.is_some(),
            "extension + lang token in filename should reinforce"
        );

        let without_token = scan_markers(&data(), &segs, false, Some("bik"));
        assert!(
            without_token.video.is_none(),
            "extension alone (no lang token in filename) must not reinforce"
        );
    }

    #[test]
    fn closest_marker_wins() {
        let segs = tokenize_path("sound\\text\\voice_line.wav");
        let ctx = scan_markers(&data(), &segs, false, None);
        assert_eq!(ctx.closest(), Some(MarkerKind::Audio));
    }

    #[test]
    fn extension_settles_kind_against_contradicting_folder_words() {
        // A subtitle inside a Movies folder is text; a voice line inside a
        // `lang\` folder is audio. Both had the folder word winning before
        // the extension became the deciding evidence for `kind`.
        let subtitle = tokenize_path("LIS\\Content\\Movies\\Subtitles\\Act_pt-BR.vtt");
        assert_eq!(
            scan_markers(&data(), &subtitle, true, Some("vtt")).closest(),
            Some(MarkerKind::Text)
        );

        let voice = tokenize_path("lang\\de_DE\\sounds\\Male5a.wav");
        assert_eq!(
            scan_markers(&data(), &voice, false, Some("wav")).closest(),
            Some(MarkerKind::Audio)
        );

        // ...and it applies even when the language token is in a folder, so
        // the reinforcement gate cannot reach it.
        let help_page = tokenize_path("Support\\EA Help\\Es\\Sound_card.htm");
        assert_eq!(
            scan_markers(&data(), &help_page, false, Some("htm")).closest(),
            Some(MarkerKind::Text)
        );
    }

    #[test]
    fn extension_kind_never_counts_as_context() {
        // An ordinary game sound with no marker word and no language token:
        // knowing it is a `.wav` must not make it look like localization
        // context, or every audio file on the disk would start lending
        // weight to weak language tokens.
        let segs = tokenize_path("data\\pak01\\chunk.wav");
        let ctx = scan_markers(&data(), &segs, false, Some("wav"));
        assert_eq!(ctx.ext_kind, Some(MarkerKind::Audio));
        assert!(!ctx.has_any(), "extension alone must not count as context");
    }
}
