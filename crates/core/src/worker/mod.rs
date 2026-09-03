//! Classification policy every removal path has to obey.
//!
//! Orchestrates nothing - the scan job (`app::worker::scan`) and the
//! unattended re-trim ([`crate::retrim`]) drive their own loops. What lives
//! here is the part those loops must not each decide for themselves: which
//! files are containers no whole-file delete may touch
//! ([`is_protected_container`]), when the user's keep-language list overrules
//! a rule ([`keep_language_vetoes_rule`]), and the progress vocabulary they
//! report in.
//!
//! The module used to call itself a "2-Phase Scanning and Analysis Pipeline",
//! which described the deep archive inspector that was removed rather than
//! anything it does: it named a pipeline it never had and left the guards
//! reading like leftovers of a deleted feature.

mod classify;

pub use classify::{
    assign_group_dirs, category_enabled, category_ui_key, classify_game, display_category,
    id_names_category, parse_source_key, source_key, ClassifyPolicy, CombinedFinding,
    DisplayCategory, FindingSource, GameIdentity, ImportedRules, PreparedFinding, PreparedGame,
};

use crate::models::Finding;

/// Progress a scan reports as it walks a library.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerProgress {
    /// Phase 1: Discovering and indexing filesystem entries.
    ScanPhase1 {
        current: usize,
        total: usize,
        game_name: String,
    },
    /// Phase 2: Rule classification and whole-file detection.
    ScanPhase2 {
        current: usize,
        total: usize,
        file_name: String,
        findings_count: usize,
    },
    /// Overall scan progress across phases and games.
    OverallProgress { fraction: f32, message: String },
}

/// Extensions of files GameTrimmer refuses to delete whole, because deleting
/// one throws away assets the user never asked to lose: a `.pak`/`.pck`/
/// `.bnk`/... holds many assets in one file, and a rule - especially an
/// imported one - that matched it by a keyword in its path would otherwise
/// take an entire 40 GB archive with it.
///
/// Named for what it protects, not for what once read it. The list was
/// originally the input list of a deep archive inspector that could open a
/// container and remove single streams from it; that inspector is gone (it
/// never shipped a working mutation - every `trim()` returned `Unsupported`),
/// and while it existed these files were "candidates" for that trimmer. They
/// are candidates for nothing now. They are simply off limits.
///
/// One list, read by the classifier's veto ([`RuleEngine::classify`]) and by
/// the delete preflight ([`crate::ops`]). It used to be written out twice, in
/// two crates, with no constant between them.
///
/// [`RuleEngine::classify`]: crate::rules::RuleEngine::classify
pub const PROTECTED_CONTAINER_EXTENSIONS: &[&str] =
    &["pck", "bnk", "pak", "asar", "bundle", "unity3d", "assets"];

/// Whether `ext` (without its dot, any case) belongs to the protected
/// container list. See [`PROTECTED_CONTAINER_EXTENSIONS`].
pub fn is_protected_container_extension(ext: &str) -> bool {
    PROTECTED_CONTAINER_EXTENSIONS
        .iter()
        .any(|known| ext.eq_ignore_ascii_case(known))
}

/// Whether the user's keep-language list forbids `finding`'s rule from
/// claiming `rel_path`, and therefore whether the verdict has to be dropped.
///
/// The keep-list is a promise that files of the languages the user keeps are
/// not touched. It was only half kept: the check lived inside the
/// localization detector's analysis loop
/// ([`crate::langdetect::LangDetector::carries_kept_language`] is the same
/// predicate), so it protected a file from the localization stage and from
/// nothing else. The rule engine has no language tables and never consulted
/// it.
///
/// The line is drawn per rule, by [`crate::rules::Rule::localized_content`],
/// and not per category - because the categories do not draw it. Read the
/// flag off the rule to see why any given file is exempt; a rule that names
/// content in the player's language opts in by setting it, and needs no
/// change here.
///
/// - A **screen** the game plays on the way in - a logo, a legal or rating
///   screen, a health warning, a splash - is removed whatever language it
///   carries. Protecting the one legal screen the player can actually read
///   while removing the eighteen they cannot is not keeping a promise, it is
///   keeping the wrong copy.
/// - **Content** in a language the user keeps is off limits. No rule the repo
///   ships claims this any more - the attract reel did, and gave it up: which
///   startup videos to remove is the player's decision, and a reel offered
///   under the auto-select threshold is already a decision they make with
///   their own hand. The flag stays for a personal or imported rule that does
///   name content in the player's language.
///
/// Deliberately *not* keyed on the rule's description: `Rule::desc` is
/// resolved to the interface language when the pack is compiled
/// (`rules.rs`), so a list of English descriptions would stop matching the
/// moment someone ran the app in Ukrainian - a guard that silently switches
/// off for most of the world.
///
/// Both classification paths call this, and only this: the interactive scan
/// (`app::worker::scan::classify_game`) and unattended re-trim
/// ([`crate::retrim::retrim_game_with_new_build`]). They used to be free to
/// disagree about one file, which is the failure GT-206 exists to fix; the
/// policy lives here so there is one answer to disagree with.
///
/// Costs nothing on the overwhelming majority of findings: the flag is a
/// bool test, and the language tokenization behind
/// `carries_kept_language` only runs for a rule that declared itself
/// content.
pub fn keep_language_vetoes_rule(
    detector: &crate::langdetect::LangDetector,
    finding: &Finding,
    rel_path: &str,
) -> bool {
    finding.localized_content && detector.carries_kept_language(rel_path)
}

/// Identifies whether a file is a multi-asset container that must never be
/// offered as a whole-file deletion. See
/// [`PROTECTED_CONTAINER_EXTENSIONS`].
///
/// The extension is tested first, and it decides almost every call: seven
/// extensions against a `.exe`, a `.uasset` or a texture is a handful of byte
/// comparisons, while [`is_external_single_language_file`] walks a hundred
/// language tags. The order used to be the other way round, which charged
/// *every file in every game* for a test only an archive can pass - and this
/// function is called once per file by `RuleEngine::classify`, by the scan's
/// candidate-archive filter and by the writer. Measured on the real library
/// (1637 games, 874 k findings) with the old order: 646 s of worker CPU in the
/// rules stage and 223 s of single-threaded row building in the writer, on a
/// scan that took 281 s wall.
pub fn is_protected_container(rel_path: &str) -> bool {
    let filename = rel_path.rsplit(['\\', '/']).next().unwrap_or(rel_path);
    let Some((_, ext)) = filename.rsplit_once('.') else {
        return false;
    };
    if !is_protected_container_extension(ext) {
        return false;
    }

    // An external single-language file (`sound_fre.pck`, `locales/es.pak`) is
    // a whole-file deletion candidate for Phase 2, not a protected container
    // - even when it carries one of the extensions above.
    !is_external_single_language_file(rel_path)
}

/// Language tags as a filename spells them, shared by
/// [`is_external_single_language_file`] and its corpus test. Module-level so
/// both see one list: two copies of a table this long drift the moment a tag
/// is added.
const LANG_CODES: &[&str] = &[
    "en", "eng", "us", "gb", "fra", "fre", "fr", "ger", "deu", "de", "spa", "esn", "es", "es419",
    "ita", "it", "rus", "ru", "jpn", "ja", "jap", "zho", "chi", "chn", "zh", "zhcn", "zhtw", "kor",
    "ko", "pol", "pl", "por", "pt", "ptbr", "bra", "ukr", "uk", "tur", "tr", "cze", "cs", "cz",
    "hun", "hu", "nld", "nl", "ara", "ar", "dan", "da", "fin", "fi", "nor", "no", "swe", "sv",
    "ell", "el", "gre", "tha", "th", "vie", "vi", "ind", "id",
];

const LANG_NAMES: &[&str] = &[
    "english",
    "french",
    "german",
    "spanish",
    "italian",
    "russian",
    "japanese",
    "chinese",
    "korean",
    "polish",
    "portuguese",
    "ukrainian",
    "turkish",
    "czech",
    "hungarian",
    "dutch",
    "arabic",
    "danish",
    "finnish",
    "norwegian",
    "swedish",
    "greek",
    "thai",
    "vietnamese",
    "indonesian",
    "francais",
    "deutsch",
    "espanol",
    "italiano",
    "brazilian",
];

/// Whether `stem` ends with `tag` preceded by `_` or `-` (`sounds_fra` for
/// `fra`, `vo-german` for `german`).
///
/// Written as a byte check rather than `stem.ends_with(&format!("_{tag}"))`:
/// the caller runs it against ~100 language tags, twice, for every file in
/// every game, and the formatted version allocated a `String` per tag per
/// file - some 400 allocations to answer "is this an ordinary .exe". That was
/// the single largest cost in a full scan.
fn ends_with_separated_tag(stem: &str, tag: &str) -> bool {
    let Some(head) = stem.strip_suffix(tag) else {
        return false;
    };
    matches!(head.as_bytes().last(), Some(b'_') | Some(b'-'))
}

/// Determines if a file path points to a standalone external single-language
/// file: a whole-file localization the rest of the app already deletes
/// safely, as opposed to a container whose internal streams a whole-file
/// delete could not separate. Kept as the exception to
/// [`is_protected_container`]'s protection, not for an in-place trimmer -
/// GameTrimmer no longer has one.
///
/// Matches paths such as:
/// - `*/locales/*.pak`, `*/locales/*.json`, `locales/*`, `*/locale/*`
/// - `*_fra.pck`, `*_ger.pck`, `*_rus.pck`, `*_spa.pck`, `*_deu.pck`, `*_ita.pck`
/// - `*German.pck`, `*French.pck`, `*Spanish.pck`, `*Russian.pck`
/// - `*/Localization/Spanish.pak`, `*/Sound/Russian.pck`, `*/Audio/de.pck`, `*/Audio/German.pck`
///
/// Returns `false` for monolithic archives containing internal multi-language data:
/// - `VO_AMICIA_MEDIA.PC.PCK`, `VO_D1_MEDIA.PC.PCK`
/// - `re_chunk_000.pak`, `app.asar`, `pakchunk0.pak`, `voices.pck`, `soundbanks.pck`, `audio.pck`
pub fn is_external_single_language_file(path: &str) -> bool {
    // Separator normalization and ASCII-only lowering in one pass, one
    // allocation instead of two. Every tag compared below is ASCII, so a
    // non-ASCII character left as it is cannot change any answer - while
    // `str::to_lowercase` walks the Unicode tables for every character of
    // every path in the library.
    let lower: String = path
        .chars()
        .map(|c| {
            if c == '\\' {
                '/'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();

    // 1. Locales directory match (e.g. 3DMark/bin/x64/locales/ar.pak, locales/en-US.pak, locales/fr.json)
    if lower.starts_with("locales/")
        || lower.starts_with("locale/")
        || lower.contains("/locales/")
        || lower.contains("/locale/")
    {
        return true;
    }

    // Extract filename and stem
    let filename = lower.rsplit('/').next().unwrap_or(&lower);
    let stem = match filename.rsplit_once('.') {
        Some((s, _)) => s,
        None => filename,
    };

    // Strip common secondary platform tags (e.g., sounds_fra.pc.pck -> sounds_fra)
    let effective_stem = stem
        .strip_suffix(".pc")
        .or_else(|| stem.strip_suffix(".win"))
        .or_else(|| stem.strip_suffix(".windows"))
        .or_else(|| stem.strip_suffix(".ps4"))
        .or_else(|| stem.strip_suffix(".xbox"))
        .unwrap_or(stem);

    for s in [stem, effective_stem] {
        // 2. Exact match on stem (e.g. ar.pak, de.pak, spanish.pak, russian.pck, en-us.pak, zh-cn.pak)
        let clean_s = s.replace(['-', '_'], "");
        if LANG_CODES.contains(&s)
            || LANG_CODES.contains(&clean_s.as_str())
            || LANG_NAMES.contains(&s)
            || LANG_NAMES.contains(&clean_s.as_str())
        {
            return true;
        }

        // 3. Suffix with underscore / hyphen (e.g. sounds_fra.pck, vo_german.pak, speech_rus.pck, audio_de.pck)
        if LANG_CODES
            .iter()
            .chain(LANG_NAMES.iter())
            .any(|tag| ends_with_separated_tag(s, tag))
        {
            return true;
        }

        // 4. Suffix without separator for language names (e.g. *German.pck, *French.pck, *Spanish.pck, *Russian.pck)
        for name in LANG_NAMES {
            if s.ends_with(name) {
                return true;
            }
        }
    }

    // 5. Parent directory is a dedicated localization/audio/language folder and stem is a language code or name
    // e.g. Localization/Spanish.pak, Sound/Russian.pck, Audio/de.pck, Audio/German.pck
    //
    // Outside the stem loop, and last: the folder test is fourteen substring
    // searches over the whole path and does not depend on the stem, so running
    // it per stem scanned every path twice for the same answer. The checks
    // above are pure "does anything match" tests, so the order between them
    // and this one cannot change the result.
    if [stem, effective_stem]
        .iter()
        .any(|s| LANG_CODES.contains(s) || LANG_NAMES.contains(s))
    {
        let is_loc_folder = lower.contains("/localization/")
            || lower.contains("/localisation/")
            || lower.contains("/languages/")
            || lower.contains("/language/")
            || lower.contains("/lang/")
            || lower.contains("/audio/")
            || lower.contains("/sound/")
            || lower.contains("/sounds/")
            || lower.contains("/speech/")
            || lower.contains("/dialogue/")
            || lower.contains("/dialogues/")
            || lower.contains("/vo/")
            || lower.contains("/voice/")
            || lower.contains("/voices/");
        if is_loc_folder {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_candidate_archive_path_detection() {
        assert!(is_protected_container("Data/Audio/Voices.pck"));
        assert!(is_protected_container(
            "Content/Paks/pakchunk0-WindowsNoEditor.pak"
        ));
        assert!(is_protected_container("resources/app.asar"));
        assert!(is_protected_container("re_chunk_000.pak"));

        // Standalone external single-language files must NOT be treated as monolithic archives
        assert!(!is_protected_container("locales/es.pak"));
        assert!(!is_protected_container("Audio/sounds_fre.pck"));
        assert!(!is_protected_container("sound_rus.pck"));

        // Non-archives
        assert!(!is_protected_container("bin/game.exe"));
        assert!(!is_protected_container("readme.txt"));

        // Bink 1 and Bink 2 are videos, not archives of separable language
        // streams - both belong to the intro rules, not the archive
        // extension list. See GT-204.
        assert!(!is_protected_container("movies/intro.bik"));
        assert!(!is_protected_container("movies/intro.bk2"));
    }

    #[test]
    fn test_worker_progress_variants() {
        let p1 = WorkerProgress::ScanPhase1 {
            current: 1,
            total: 10,
            game_name: "Doom".to_string(),
        };
        let p2 = WorkerProgress::ScanPhase2 {
            current: 5,
            total: 100,
            file_name: "file.txt".to_string(),
            findings_count: 3,
        };
        let p3 = WorkerProgress::OverallProgress {
            fraction: 0.5,
            message: "Analyzing...".to_string(),
        };

        assert!(matches!(p1, WorkerProgress::ScanPhase1 { .. }));
        assert!(matches!(p2, WorkerProgress::ScanPhase2 { .. }));
        assert!(matches!(p3, WorkerProgress::OverallProgress { .. }));
    }

    #[test]
    fn secondary_analysis_and_persistence_api_stays_removed() {
        let source = include_str!("mod.rs");
        let analyze_export = ["pub fn ", "analyze("].concat();
        let persist_export = ["pub fn ", "persist_game_findings("].concat();

        assert!(!source.contains(&analyze_export));
        assert!(!source.contains(&persist_export));
    }

    #[test]
    fn external_single_language_files_are_recognized() {
        assert!(is_external_single_language_file("locales/es.pak"));
        assert!(is_external_single_language_file("Audio/sounds_fre.pck"));
        assert!(is_external_single_language_file("sound_rus.pck"));
        assert!(is_external_single_language_file("Localization/Spanish.pak"));

        // Monolithic multi-language containers are not single-language files.
        assert!(!is_external_single_language_file("voices.pck"));
        assert!(!is_external_single_language_file("re_chunk_000.pak"));
        assert!(!is_external_single_language_file("app.asar"));
    }
}

#[cfg(test)]
mod keep_language_veto_tests {
    use super::*;
    use crate::langdetect::LangDetector;
    use crate::rules::{Category, RuleProvenance};

    fn detector(keep: &[&str]) -> LangDetector {
        LangDetector::with_keep_list(&keep.iter().map(|k| k.to_string()).collect::<Vec<_>>())
    }

    fn finding(localized_content: bool) -> Finding {
        Finding {
            category: Category::Intro,
            rule_desc: "test".to_string(),
            confidence: 80,
            provenance: RuleProvenance::Builtin,
            localized_content,
        }
    }

    /// The line the decision drew. A startup screen is removed whatever
    /// language it carries - removing the eighteen legal screens the player
    /// cannot read while protecting the one that actually plays is not
    /// keeping the keep-list's promise, it is keeping the wrong copy.
    #[test]
    fn a_startup_screen_is_removed_even_in_a_language_the_user_keeps() {
        let german = detector(&["de"]);

        assert!(!keep_language_vetoes_rule(
            &german,
            &finding(false),
            r"XComGame\Movies\1080_LogoLegal_PCConsole_DEU.bik"
        ));
        assert!(!keep_language_vetoes_rule(
            &german,
            &finding(false),
            r"videos\de\warning_disclaimer.bik"
        ));
    }

    /// The other side: a rule that says its subject is content yields, and
    /// the file stays. No built-in rule says it any more (see
    /// [`no_builtin_rule_marks_itself_as_localized_content`]), so the
    /// predicate is exercised here with a synthetic finding - which is what
    /// a personal or imported pack setting the flag would produce.
    #[test]
    fn localized_content_in_a_kept_language_is_off_limits() {
        assert!(keep_language_vetoes_rule(
            &detector(&["de"]),
            &finding(true),
            r"movies\german\attract.bik"
        ));
        // The folder half and the file-name half of the same predicate.
        assert!(keep_language_vetoes_rule(
            &detector(&["de"]),
            &finding(true),
            r"movies\attract_german.bik"
        ));
    }

    /// A language the user does not keep is removable whichever side of the
    /// line the rule sits on.
    #[test]
    fn content_in_a_language_the_user_does_not_keep_is_still_removable() {
        assert!(!keep_language_vetoes_rule(
            &detector(&["en"]),
            &finding(true),
            r"movies\german\attract.bik"
        ));
    }

    /// A file with no language marker at all is unaffected, whatever the
    /// keep-list says - the overwhelmingly common case.
    #[test]
    fn a_file_without_a_language_marker_is_never_vetoed() {
        assert!(!keep_language_vetoes_rule(
            &detector(&["de", "en", "fr"]),
            &finding(true),
            r"Movies\UE4_Logo.mp4"
        ));
    }

    /// The classification is data, not code: every rule the repo ships says
    /// which side of the line it is on, and none of them says `content`.
    ///
    /// The attract reel used to, and was the only one. It stopped because
    /// which startup videos a player wants is the player's call, not the
    /// pack's: the reel is offered at confidence 80 - under
    /// `app::model::AUTO_SELECT_CONFIDENCE_THRESHOLD`, so never ticked on the
    /// user's behalf - and a player who wants to keep the one in their own
    /// language keeps it by leaving the box alone, or permanently by the
    /// "never touch this" exception. A keep-language veto took that decision
    /// away from them instead, and did it invisibly.
    ///
    /// The mechanism stays - [`keep_language_vetoes_rule`] and
    /// [`crate::rules::Rule::localized_content`] are part of the pack format,
    /// available to a personal or imported rule that does name content in the
    /// player's language. This pins the *built-in* pack's answer, so it
    /// cannot drift back without someone noticing.
    #[test]
    fn no_builtin_rule_marks_itself_as_localized_content() {
        let rules = crate::rules::parse_rule_list(crate::rules::BUILTIN_RULES_JSON)
            .expect("the built-in pack parses");
        let content: Vec<&str> = rules
            .iter()
            .filter(|rule| rule.localized_content)
            .map(|rule| rule.pattern.as_str())
            .collect();

        assert!(
            content.is_empty(),
            "no shipped rule may take a startup video off the table by language: {content:?}"
        );
    }
}
