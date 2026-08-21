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
    let lower = ext.to_ascii_lowercase();
    CANDIDATE_ARCHIVE_EXTENSIONS.contains(&lower.as_str())
}

/// Identifies whether a file is a candidate for monolithic archive deep inspection.
pub fn is_candidate_archive_path(rel_path: &str) -> bool {
    // If it's already an external single-language file (e.g., sound_fre.pck, locales/es.pak),
    // it is a candidate for whole-file deletion in Phase 2, not Phase 3 sparse zeroing.
    if archive_trimmer::formats::is_external_single_language_file(rel_path) {
        return false;
    }

    let clean = rel_path.replace('\\', "/");
    let lower = clean.to_lowercase();
    let filename = lower.rsplit('/').next().unwrap_or(&lower);

    if filename.starts_with("re_chunk") && filename.ends_with(".pak") {
        return true;
    }

    if let Some((_, ext)) = filename.rsplit_once('.') {
        is_candidate_archive_extension(ext)
    } else {
        false
    }
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
