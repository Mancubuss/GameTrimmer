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

use std::collections::HashSet;
use std::sync::LazyLock;

use crate::models::Finding;

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

/// Language tags as a filename spells them, indexed once from the canonical
/// dictionary (`l10n_rules.json`, via [`crate::langdetect::LangData`])
/// instead of being spelled out a second time here.
///
/// The hand-written copy this replaces knew 25 of the dictionary's languages
/// and silently dropped the rest, so a `qtwebengine_locales` folder was 33
/// languages to the detector and 25 to this gate: `bg`, `he`, `hi`, `hr`,
/// `ro`, `sk`, `sl` and `sr` were thrown away as protected containers before
/// they could ever be offered.
///
/// Built from the *built-in* pack, never from a runtime-loaded one: this is
/// the safety veto's escape hatch, and a community rule pack must not be
/// able to talk GameTrimmer into deleting a container whole.
struct LangFilenameTags {
    /// Every dictionary spelling, plus its separator-free form (`es-419`
    /// also as `es419`), so a stem written either way matches.
    tags: HashSet<String>,
    /// The spellings that read as a *name* rather than a code - the only
    /// ones allowed to match without a separator in front of them
    /// (`voGerman.pck`) and the only ones a directory may carry without
    /// further corroboration. A bare two-letter code never gets either
    /// liberty: that is how a rifle called "ar 2" turns Arabic (8b03b91).
    names: Vec<String>,
}

/// A spelling long enough, and wordy enough, to be a language *name*:
/// "german", "francais", "schinese". Two- and three-letter codes and
/// region-qualified forms (`fr-be`, `french(france)`) are excluded - they
/// have to be their own atom of a name, or to sit under a folder that
/// already announces per-language layout, before they mean anything.
fn is_language_name(spelling: &str) -> bool {
    spelling.chars().count() >= 4 && spelling.chars().all(char::is_alphabetic)
}

static LANG_FILENAME_TAGS: LazyLock<LangFilenameTags> = LazyLock::new(|| {
    let data = crate::langdetect::LangData::builtin();
    let mut tags = HashSet::new();
    let mut names = Vec::new();
    for alias in data.aliases() {
        if is_language_name(alias) {
            names.push(alias.to_string());
        }
        tags.insert(alias.chars().filter(|c| !matches!(c, '-' | '_')).collect());
        tags.insert(alias.to_string());
    }
    LangFilenameTags { tags, names }
});

/// Folder words that say "what is under here is separated by language".
/// They are what lets a bare two-letter directory (`localization/ru/audio/`)
/// count: on its own `ru` is as likely to be a studio prefix as a language.
const LOCALIZED_FOLDER_WORDS: &[&str] = &[
    "localization",
    "localisation",
    "localizations",
    "localized",
    "localised",
    "translations",
    "languages",
    "language",
    "lang",
    "audio",
    "sound",
    "sounds",
    "speech",
    "dialogue",
    "dialogues",
    "vo",
    "voice",
    "voices",
];

/// Characters that break a file-name stem into its atoms.
///
/// CamelCase is deliberately *not* a boundary. Splitting it would read
/// `SoundbanksNoMedia.pck` as "soundbanks | no | media" and hand Albion
/// Online's monolithic bank to the deleter as Norwegian - the same class of
/// mistake 8b03b91 closed inside the detector.
const STEM_ATOM_DELIMS: [char; 8] = ['_', '-', '.', '(', ')', '[', ']', ' '];

/// Whether any atom of `stem` is a known language spelling.
///
/// "Atom", not "substring": `sounds_ita_patch_1` is Italian because `ita`
/// stands alone between two separators, while `capital.pak` is not Italian
/// and `SoundbanksNoMedia.pck` is not Norwegian.
fn stem_atom_is_language(stem: &str, tags: &HashSet<String>) -> bool {
    stem.split(STEM_ATOM_DELIMS)
        .any(|atom| !atom.is_empty() && tags.contains(atom))
}

/// Whether a directory segment *is* a language: `Sound/Italian/`,
/// `audio/packages/german/`, `DATA/SOUND/FRENCH(FRANCE)/`,
/// `Sound/win/Chinese(PRC)/`, `localization/zh_sg/`.
///
/// Three spellings of the same segment are tried: as written, with a
/// parenthesized region qualifier dropped, and with the separator of a
/// locale tag removed (`zh_sg` -> `zhsg`, which is how the dictionary keeps
/// `zh-sg`).
///
/// A full name qualifies on its own - a folder called `German` is German.
/// Anything shorter (`ru`, `br`, `int`) qualifies only under a folder that
/// already announces per-language layout, which is what `loc_context`
/// carries.
fn directory_is_language(segment: &str, table: &LangFilenameTags, loc_context: bool) -> bool {
    let head = segment.split(['(', '[']).next().unwrap_or(segment);
    let glued = segment.replace(['-', '_'], "");
    [segment, head, glued.as_str()].iter().any(|candidate| {
        table.tags.contains(*candidate) && (loc_context || is_language_name(candidate))
    })
}

/// Determines if a file path points to a standalone external single-language
/// file: a whole-file localization the rest of the app already deletes
/// safely, as opposed to a container whose internal streams a whole-file
/// delete could not separate. Kept as the exception to
/// [`is_protected_container`]'s protection, not for an in-place trimmer -
/// GameTrimmer no longer has one.
///
/// Three shapes count, and all three insist the language is a *whole naming
/// unit* rather than a substring:
///
/// 1. The file sits in a `locales/` or `locale/` folder.
/// 2. An atom of the file-name stem is a language spelling - anywhere in the
///    stem, not only at its end: `italian_dialog.pak`,
///    `sounds_ita_female_install_1.pck`, `global.ru.bundle`, `de.pak`,
///    `voGerman.pck`.
/// 3. A directory on the path is a language: `Sound/Italian/External.pck`,
///    `audio/packages/german/subtitles.pck`, `localization/ru/audio/vo.bnk`.
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

    let table = &*LANG_FILENAME_TAGS;

    let mut segments = lower.split('/').filter(|s| !s.is_empty());
    let filename = segments.next_back().unwrap_or(&lower);
    let directories: Vec<&str> = segments.collect();

    // 1. Locales directory (3DMark/bin/x64/locales/ar.pak, locale/fr.json).
    if directories
        .iter()
        .any(|dir| *dir == "locales" || *dir == "locale")
    {
        return true;
    }

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
        // 2. The whole stem is a language, however it spells the separator
        // (`en-us.pak`, `es419.pak`, `pt_br.pak`).
        if table.tags.contains(s) || table.tags.contains(&s.replace(['-', '_'], "")) {
            return true;
        }

        // 3. A language atom anywhere in the stem (`sounds_fra`,
        // `italian_dialog`, `global.ru`, `japanese-part0`).
        if stem_atom_is_language(s, &table.tags) {
            return true;
        }

        // 4. A full name glued to the end of a longer word (`voGerman.pck`,
        // `*French.pck`). Names only - see `LangFilenameTags::names`.
        if table.names.iter().any(|name| s.ends_with(name)) {
            return true;
        }
    }

    // 5. The folder is the language and the file name says nothing
    // (`game/sound/Italian/External.pck`). By far the largest shape in the
    // real library: 7961 files / 46.9 GB stayed protected because the rule
    // insisted the *stem* carry the language too.
    let loc_context = directories
        .iter()
        .any(|dir| LOCALIZED_FOLDER_WORDS.contains(dir));
    directories
        .iter()
        .any(|dir| directory_is_language(dir, table, loc_context))
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

    /// GT-454. The gate used to carry its own 25-language list while the
    /// detector read 36 from `l10n_rules.json`; the eight languages only the
    /// dictionary knew were found and then silently dropped as containers.
    ///
    /// `qtwebengine_locales` is the folder that exposed it: the segment ends
    /// in "locales" but is not named `locales`, so rule 1 never fires and the
    /// verdict comes down to whether the stem is a known language.
    #[test]
    fn every_language_the_dictionary_knows_is_a_language_to_the_container_gate() {
        for code in ["bg", "he", "hi", "hr", "ro", "sk", "sl", "sr"] {
            let path = format!("PySide6/translations/qtwebengine_locales/{code}.pak");
            assert!(
                is_external_single_language_file(&path),
                "{path} is a language file to the detector and must be one here too"
            );
        }
        // Canonical keys the old list spelled only in their glued form.
        assert!(is_external_single_language_file("Paks/zh-hans.pak"));
        assert!(is_external_single_language_file("Paks/zh-hant.pak"));
        assert!(is_external_single_language_file("Paks/es-419.pak"));
        assert!(is_external_single_language_file("Paks/es419.pak"));
    }

    /// GT-454, shape B - the folder is the language and the file name says
    /// nothing. The largest single loss in the real library (7961 files /
    /// 46.9 GB), because the rule insisted the *stem* carry the language.
    #[test]
    fn a_file_under_a_language_folder_is_a_single_language_file() {
        for path in [
            r"game\sound\Italian\External.pck",
            r"edit\audio\packages\german\subtitles.pck",
            r"edit\audio\packages\czech\subtitles_scotch.pck",
            r"DATA\SOUND\FRENCH(FRANCE)\DIALOGUE_GENERIC.BNK",
            r"Sound\win\Chinese(PRC)\sb_001_vo_ai_un_lines.pck",
            r"ExampleGame\WwiseAudio\PC\German\VO_E03_3.STM.pck",
            // A bare two-letter folder needs a per-language layout around it.
            r"res\localization\ru\audio\vo_coordinator.bnk",
            // ... and a locale tag spelled with an underscore is the same
            // folder written the long way.
            r"res\localization\zh_sg\audio\vo_coordinator.bnk",
            r"res\localization\pt_br\audio\vo_coordinator.bnk",
            r"res\localization\es_ar\audio\vo_coordinator.bnk",
        ] {
            assert!(
                is_external_single_language_file(path),
                "{path} sits in a language folder"
            );
        }

        // Without that corroboration a two-letter folder proves nothing: a
        // studio or project prefix is spelled exactly the same way.
        assert!(!is_external_single_language_file(
            r"Content\Paks\it\chunk0.pak"
        ));
    }

    /// GT-454, shapes C and D - the language tag is an atom of the stem, but
    /// not the last one. `ends_with_separated_tag` only ever looked at the
    /// tail, and a dot was not a separator to it at all.
    #[test]
    fn a_language_atom_anywhere_in_the_stem_counts() {
        for path in [
            r"localization\italian_dialog.pak",
            r"Localization\Japanese-part0.pak",
            r"sounddata\pc\soundspc_ita_patch_1_.pck",
            r"sounddata\PC\sounds_ita_female_install_1.pck",
            r"Madness\Content\Paks\pakchunk12_s00_ru-WindowsNoEditor_0_P.pak",
            r"DATA\SOUND\GERMAN_DIALOGUE.PCK",
            // Shape D: the tag is at the tail, but behind a dot.
            "global.ru.bundle",
        ] {
            assert!(
                is_external_single_language_file(path),
                "{path} names a language as its own atom"
            );
        }
    }

    /// The single real false positive the widening could have let through,
    /// kept as a named guard.
    ///
    /// Albion Online's soundbank is one file of 5.67 MB, but it is the exact
    /// shape the boundary exists for: the detector reads the English word
    /// "No" inside "NoMedia" as Norwegian (confidence 70), and only the
    /// "a language tag is a whole atom of the name" rule keeps a monolithic
    /// bank from being offered for deletion on the strength of a syllable.
    /// Measured over all 9409 files the widened gate releases, this was the
    /// only genuine false positive in the set - so if this test ever passes
    /// its assertion in reverse, the boundary is gone, not merely dented.
    #[test]
    fn albions_soundbanks_no_media_stays_a_protected_container() {
        let path = r"game\Albion-Online_Data\StreamingAssets\Audio\Windows\SoundbanksNoMedia.pck";
        assert!(!is_external_single_language_file(path));
        assert!(is_protected_container(path));
    }

    /// The boundary the widening must not cross: a language spelling that is
    /// a *syllable* of a longer word, not an atom of the name. Splitting
    /// CamelCase here would read `WindowsNoEditor` as Norwegian too - the
    /// same class of mistake 8b03b91 closed inside the detector, where the
    /// detector still makes it.
    #[test]
    fn a_language_spelled_inside_a_longer_word_is_still_a_container() {
        for path in [
            r"Content\Paks\pakchunk0-WindowsNoEditor.pak",
            "re_chunk_000.pak",
            "voices.pck",
            "resources/app.asar",
            "soundbanks.pck",
            "audio.pck",
            "VO_AMICIA_MEDIA.PC.PCK",
        ] {
            assert!(
                is_protected_container(path),
                "{path} must stay a protected container"
            );
        }
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
