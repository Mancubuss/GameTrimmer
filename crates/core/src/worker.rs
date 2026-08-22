//! 3-Phase Scanning and Analysis Pipeline.
//!
//! Orchestrates:
//! - Phase 1: Disk & Library discovery / file indexing
//! - Phase 2: Regex rules & whole-file localization detections
//! - Phase 3: Monolithic archive deep inspection & internal stream discovery

use std::collections::HashSet;
use std::path::Path;

use crate::error::{CoreError, Result};
use crate::models::{Finding, FindingAction, MonolithicStreamInfo};
use crate::rules::{Category, RuleProvenance};

/// Progress reported across the 3-phase scanning architecture.
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
    /// Phase 3: Monolithic archive deep inspection and internal stream discovery.
    ScanPhase3 {
        current: usize,
        total: usize,
        archive_name: String,
        monoliths_count: usize,
    },
    /// Overall scan progress across phases and games.
    OverallProgress { fraction: f32, message: String },
}

/// Extensions the deep archive inspector claims, to the exclusion of every
/// other path in the application.
///
/// A file whose extension is on this list is refused by [`crate::rules::RuleEngine::classify`]
/// and blocked at delete preflight, so that a container holding data the user
/// wants can never be deleted whole because a rule matched its name. That
/// guard is worth its bluntness for a 40 GB archive; it was not worth it for
/// `bik`, which used to be here.
///
/// Bink 1 and Bink 2 are videos, not containers of separable language
/// streams: the archive handler reports zero trimmable bytes for both and
/// refuses to trim them, so listing either here bought nothing and cost the
/// intro rules - seven of the eight match `.bik`/`.bk2` - every file they
/// exist for. `bk2` stayed until a header-derived stub for it was verified
/// live in a real game (Scars Above, variant B); see GT-204.
///
/// One list, read by both the classifier and the content prober. It used to be
/// written out twice, in two crates, with no constant between them.
pub const CANDIDATE_ARCHIVE_EXTENSIONS: &[&str] =
    &["pck", "bnk", "pak", "asar", "bundle", "unity3d", "assets"];

/// Whether `ext` (without its dot, any case) belongs to the deep archive
/// inspector. See [`CANDIDATE_ARCHIVE_EXTENSIONS`].
pub fn is_candidate_archive_extension(ext: &str) -> bool {
    CANDIDATE_ARCHIVE_EXTENSIONS
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

/// Identifies whether a file is a candidate for monolithic archive deep inspection.
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
///
/// [`is_external_single_language_file`]: archive_trimmer::formats::is_external_single_language_file
pub fn is_candidate_archive_path(rel_path: &str) -> bool {
    let filename = rel_path.rsplit(['\\', '/']).next().unwrap_or(rel_path);
    let Some((_, ext)) = filename.rsplit_once('.') else {
        return false;
    };
    if !is_candidate_archive_extension(ext) {
        return false;
    }

    // An external single-language file (`sound_fre.pck`, `locales/es.pak`) is
    // a whole-file deletion candidate for Phase 2, never a container for the
    // deep inspector - even when it carries one of the extensions above.
    !archive_trimmer::formats::is_external_single_language_file(rel_path)
}

/// Deeply inspects one monolithic archive for trimmable language streams.
pub fn inspect_monolithic_archive(
    archive_path: &Path,
    keep_languages: &[String],
) -> Result<Option<Finding>> {
    let detector_res = archive_trimmer::formats::FormatDetector::detect_file(archive_path)
        .map_err(|e| {
            CoreError::Other(format!(
                "failed to detect format for {}: {e}",
                archive_path.display()
            ))
        })?;

    let Some(archive_type) = detector_res else {
        return Ok(None);
    };

    let handler = archive_trimmer::formats::FormatDetector::get_handler(archive_type);
    let analysis = match handler.analyze(archive_path) {
        Ok(a) => a,
        Err(e) => {
            // Unparseable, corrupted or encrypted archive - skip gracefully
            log_debug_archive(&format!("Skipping archive {}: {e}", archive_path.display()));
            return Ok(None);
        }
    };

    let mut trimmable_offsets = Vec::new();
    let mut trimmable_languages = Vec::new();
    let mut trimmable_streams = Vec::new();
    let mut seen_langs = HashSet::new();

    let mut selected_chunks: Vec<_> = analysis
        .trimmable_chunks
        .iter()
        .filter(|chunk| chunk.is_language && chunk.can_zero_in_place)
        .filter(|chunk| {
            chunk.language.as_deref().is_some_and(|language| {
                archive_trimmer::formats::is_known_language(language)
                    && !archive_trimmer::formats::is_language_kept(language, keep_languages)
            })
        })
        .filter_map(|chunk| {
            let end = chunk.offset.checked_add(chunk.length)?;
            (chunk.length > 0 && end <= analysis.total_size).then_some((chunk, end))
        })
        .collect();
    selected_chunks.sort_by_key(|(chunk, _)| chunk.offset);

    let mut previous_end = 0u64;
    for (chunk, end) in selected_chunks {
        // Overlapping parser output is ambiguous. Keep the first valid range and
        // conservatively skip subsequent overlaps so savings are never double-counted.
        if chunk.offset < previous_end {
            continue;
        }
        let lang_str = chunk.language.as_deref().expect("filtered language");
        let canon = archive_trimmer::formats::canonical_language(lang_str);
        trimmable_offsets.push((chunk.offset, chunk.length));
        if seen_langs.insert(canon.to_string()) {
            trimmable_languages.push(canon.to_string());
        }
        trimmable_streams.push(MonolithicStreamInfo {
            name: chunk.name.clone(),
            language: canon.to_string(),
            size: chunk.length,
        });
        previous_end = end;
    }

    if !trimmable_offsets.is_empty() {
        // Savings must describe the finalized, user-selected, non-overlapping
        // ranges above. Handler-wide estimates may use a different keep-list.
        let total_savings: u64 = trimmable_offsets.iter().map(|(_, length)| *length).sum();
        let finding = Finding {
            category: Category::MonolithicArchive,
            rule_desc: format!(
                "{}: Monolithic archive localized streams",
                analysis.archive_type
            ),
            confidence: 90,
            provenance: RuleProvenance::Builtin,
            // Archive stream trimming carries its own per-language keep-list
            // (`trimmable_offsets` is already filtered by it), so the
            // whole-file guard has nothing left to decide here.
            localized_content: false,
            action: FindingAction::SparseZero {
                format: analysis.archive_type.to_string(),
                languages: trimmable_languages,
                stream_count: trimmable_offsets.len(),
                offsets: trimmable_offsets,
                streams: trimmable_streams,
                estimated_savings: total_savings,
            },
        };
        Ok(Some(finding))
    } else {
        Ok(None)
    }
}

fn log_debug_archive(_msg: &str) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_candidate_archive_path_detection() {
        assert!(is_candidate_archive_path("Data/Audio/Voices.pck"));
        assert!(is_candidate_archive_path(
            "Content/Paks/pakchunk0-WindowsNoEditor.pak"
        ));
        assert!(is_candidate_archive_path("resources/app.asar"));
        assert!(is_candidate_archive_path("re_chunk_000.pak"));

        // Standalone external single-language files must NOT be treated as monolithic archives
        assert!(!is_candidate_archive_path("locales/es.pak"));
        assert!(!is_candidate_archive_path("Audio/sounds_fre.pck"));
        assert!(!is_candidate_archive_path("sound_rus.pck"));

        // Non-archives
        assert!(!is_candidate_archive_path("bin/game.exe"));
        assert!(!is_candidate_archive_path("readme.txt"));

        // Bink 1 and Bink 2 are videos, not archives of separable language
        // streams - both now belong to the intro rules, not the archive
        // inspector. See GT-204.
        assert!(!is_candidate_archive_path("movies/intro.bik"));
        assert!(!is_candidate_archive_path("movies/intro.bk2"));
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
        let p3 = WorkerProgress::ScanPhase3 {
            current: 1,
            total: 2,
            archive_name: "voices.pck".to_string(),
            monoliths_count: 1,
        };
        let p4 = WorkerProgress::OverallProgress {
            fraction: 0.5,
            message: "Analyzing...".to_string(),
        };

        assert!(matches!(p1, WorkerProgress::ScanPhase1 { .. }));
        assert!(matches!(p2, WorkerProgress::ScanPhase2 { .. }));
        assert!(matches!(p3, WorkerProgress::ScanPhase3 { .. }));
        assert!(matches!(p4, WorkerProgress::OverallProgress { .. }));
    }

    #[test]
    fn secondary_analysis_and_persistence_api_stays_removed() {
        let source = include_str!("worker.rs");
        let analyze_export = ["pub fn ", "analyze("].concat();
        let persist_export = ["pub fn ", "persist_game_findings("].concat();

        assert!(!source.contains(&analyze_export));
        assert!(!source.contains(&persist_export));
    }

    #[test]
    fn monolithic_finding_uses_only_safe_selected_ranges_for_savings() {
        let dir = tempdir().expect("tempdir");
        let archive = dir.path().join("voices.pck");
        let bytes = archive_trimmer::formats::wwise::create_synthetic_wwise_pck(
            &[(1, "English(US)"), (2, "German")],
            &[(100, 1, 4096), (200, 2, 8192), (300, 999, 16384)],
        );
        fs::write(&archive, bytes).expect("write pck");

        let finding = inspect_monolithic_archive(&archive, &["english".to_string()])
            .expect("inspect")
            .expect("finding");
        let FindingAction::SparseZero {
            offsets,
            streams,
            estimated_savings,
            ..
        } = finding.action
        else {
            panic!("expected sparse-zero finding");
        };

        assert_eq!(offsets.len(), 1);
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].language, "german");
        assert_eq!(estimated_savings, 8192);
    }
}

#[cfg(test)]
mod keep_language_veto_tests {
    use super::*;
    use crate::langdetect::LangDetector;
    use crate::rules::RuleProvenance;

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
            action: FindingAction::DirectDelete,
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
