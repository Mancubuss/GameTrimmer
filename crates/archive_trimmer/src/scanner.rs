//! High-Performance Directory Scanner & Monolithic Archive Aggregator.
//!
//! Recursively scans game install directories, discovers supported archives
//! (.pck, .bnk, .pak, .asar, .bik, .bk2, .bundle, .assets), evaluates anti-cheat
//! safety, and computes potential storage savings across all languages and categories.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;
use walkdir::WalkDir;

use crate::anti_cheat::{self, SafetyError, SafetyReport};
use crate::formats::{
    is_external_single_language_file, ArchiveAnalysis, ArchiveError, FormatDetector, TrimOptions,
    TrimResult,
};

#[derive(Error, Debug)]
pub enum ScanError {
    #[error("I/O error during scan: {0}")]
    Io(#[from] std::io::Error),
    #[error("Safety violation: {0}")]
    Safety(#[from] SafetyError),
    #[error("Archive error in file {0}: {1}")]
    Archive(PathBuf, #[source] ArchiveError),
}

/// Comprehensive scan report for a game directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameScanReport {
    pub game_root: PathBuf,
    pub is_safe: bool,
    pub safety_report: SafetyReport,
    pub archives_scanned: usize,
    pub detected_archives: Vec<ArchiveAnalysis>,
    pub total_logical_size: u64,
    pub total_on_disk_size: u64,
    pub total_potential_savings: u64,
    pub all_detected_languages: Vec<String>,
}

/// Aggregated report of a batch trimming operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchTrimReport {
    pub game_root: PathBuf,
    pub results: Vec<TrimResult>,
    pub total_chunks_trimmed: usize,
    pub total_logical_bytes_trimmed: u64,
    pub total_physical_bytes_freed: u64,
    pub snapshots_created: Vec<PathBuf>,
}

/// Scans a game directory, discovers archives, and produces a `GameScanReport`.
pub fn scan_game_directory(
    root: &Path,
    max_depth: Option<usize>,
) -> Result<GameScanReport, ScanError> {
    // 1. Anti-cheat check
    let safety_report = anti_cheat::check_game_safety(root, false)?;

    // 2. Discover archives
    let mut detected_archives = Vec::new();
    let mut total_logical_size = 0u64;
    let mut total_on_disk_size = 0u64;
    let mut total_potential_savings = 0u64;
    let mut all_detected_languages = Vec::new();

    let mut walker = WalkDir::new(root).follow_links(false);
    if let Some(depth) = max_depth {
        walker = walker.max_depth(depth);
    }

    for entry in walker.into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();

        // Exclude standalone whole-file localizations handled by GameTrimmer core
        if is_external_single_language_file(&path.to_string_lossy()) {
            continue;
        }

        if let Ok(Some(archive_type)) = FormatDetector::detect_file(path) {
            let handler = FormatDetector::get_handler(archive_type);
            match handler.analyze(path) {
                Ok(analysis) => {
                    // Only consider valid monolithic candidates with trimmable internal language streams
                    if analysis.total_trimmable_bytes > 0
                        && analysis.trimmable_chunks.iter().any(|c| c.is_language)
                    {
                        total_logical_size = total_logical_size.saturating_add(analysis.total_size);
                        total_on_disk_size =
                            total_on_disk_size.saturating_add(analysis.on_disk_size);
                        total_potential_savings =
                            total_potential_savings.saturating_add(analysis.total_trimmable_bytes);

                        for lang in &analysis.detected_languages {
                            if !all_detected_languages.contains(lang) {
                                all_detected_languages.push(lang.clone());
                            }
                        }

                        detected_archives.push(analysis);
                    }
                }
                Err(err) => {
                    eprintln!("Warning: failed to analyze archive {:?}: {}", path, err);
                }
            }
        }
    }

    all_detected_languages.sort();

    Ok(GameScanReport {
        game_root: root.to_path_buf(),
        is_safe: safety_report.is_safe,
        safety_report,
        archives_scanned: detected_archives.len(),
        detected_archives,
        total_logical_size,
        total_on_disk_size,
        total_potential_savings,
        all_detected_languages,
    })
}

/// Trims all discovered archives in a game directory.
pub fn batch_trim_game(root: &Path, options: &TrimOptions) -> Result<BatchTrimReport, ScanError> {
    if options.force_unsafe {
        return Err(ScanError::Archive(
            root.to_path_buf(),
            ArchiveError::Unsupported(
                "force_unsafe is rejected; anti-cheat protection is a hard block".to_string(),
            ),
        ));
    }
    // Anti-cheat protection is a hard block. `force_unsafe` is retained only
    // for serialization compatibility and must never bypass this gate.
    let _ = anti_cheat::check_game_safety(root, true)?;

    let scan_report = scan_game_directory(root, None)?;

    // Preflight before any mutation. Header-only snapshots cannot roll back
    // payload zeroing, so standalone batch trim is disabled for every candidate.
    if let Some(analysis) = scan_report.detected_archives.first() {
        return Err(ScanError::Archive(
            analysis.path.clone(),
            ArchiveError::Unsupported(
                "Standalone batch mutation is disabled until full payload rollback is available"
                    .to_string(),
            ),
        ));
    }

    let mut results = Vec::new();
    let mut total_chunks_trimmed = 0usize;
    let mut total_logical_bytes_trimmed = 0u64;
    let mut total_physical_bytes_freed = 0u64;
    let mut snapshots_created = Vec::new();

    for analysis in &scan_report.detected_archives {
        let handler = FormatDetector::get_handler(analysis.archive_type);
        match handler.trim(&analysis.path, options) {
            Ok(result) => {
                total_chunks_trimmed += result.chunks_trimmed;
                total_logical_bytes_trimmed =
                    total_logical_bytes_trimmed.saturating_add(result.logical_bytes_trimmed);
                total_physical_bytes_freed =
                    total_physical_bytes_freed.saturating_add(result.physical_bytes_freed);

                if let Some(ref snap) = result.snapshot_path {
                    snapshots_created.push(snap.clone());
                }

                results.push(result);
            }
            Err(err) => {
                eprintln!("Error trimming {:?}: {}", analysis.path, err);
            }
        }
    }

    Ok(BatchTrimReport {
        game_root: root.to_path_buf(),
        results,
        total_chunks_trimmed,
        total_logical_bytes_trimmed,
        total_physical_bytes_freed,
        snapshots_created,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::asar::create_synthetic_asar;
    use crate::formats::wwise::create_synthetic_wwise_pck;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_scan_and_batch_trim_synthetic_game() {
        let dir = tempdir().expect("tempdir");
        let game_root = dir.path();

        // 1. Create a Wwise PCK in Audio/
        let audio_dir = game_root.join("Game_Data").join("StreamingAssets");
        fs::create_dir_all(&audio_dir).expect("create audio dir");

        let languages = vec![(0, "SFX"), (1, "English(US)"), (2, "Spanish(Spain)")];
        let streams = vec![(1, 0, 8192), (2, 1, 8192), (3, 2, 16384)];
        let pck_bytes = create_synthetic_wwise_pck(&languages, &streams);
        fs::write(audio_dir.join("voices.pck"), &pck_bytes).expect("write pck");

        // 2. Create an Electron ASAR in Resources/
        let res_dir = game_root.join("resources");
        fs::create_dir_all(&res_dir).expect("create res dir");

        let en_pak = vec![0x11u8; 4096];
        let de_pak = vec![0x22u8; 16384];
        let asar_entries = vec![
            ("package.json", b"{}".as_slice()),
            ("locales/en-US.pak", en_pak.as_slice()),
            ("locales/de.pak", de_pak.as_slice()),
        ];
        let asar_bytes = create_synthetic_asar(&asar_entries);
        fs::write(res_dir.join("app.asar"), &asar_bytes).expect("write asar");

        // 3. Run Scan
        let scan_report = scan_game_directory(game_root, None).expect("scan directory");
        // ASAR remains detectable/analyzable but is not advertised as a
        // destructive candidate until validated unpack support exists.
        assert_eq!(scan_report.archives_scanned, 1);
        assert!(scan_report.is_safe);
        assert!(scan_report
            .all_detected_languages
            .contains(&"Spanish(Spain)".to_string()));
        assert!(!scan_report
            .all_detected_languages
            .contains(&"German".to_string()));
        assert_eq!(scan_report.total_potential_savings, 16384);

        // 4. Run Batch Trim (keep English + SFX)
        let options = TrimOptions {
            keep_languages: vec!["english".to_string(), "sfx".to_string()],
            dry_run: false,
            create_snapshot: true,
            force_unsafe: false,
            custom_backup_dir: None,
        };

        assert!(matches!(
            batch_trim_game(game_root, &options),
            Err(ScanError::Archive(_, ArchiveError::Unsupported(_)))
        ));

        let mut unsafe_options = options;
        unsafe_options.force_unsafe = true;
        let error = batch_trim_game(game_root, &unsafe_options).expect_err("hard block");
        assert!(error.to_string().contains("force_unsafe is rejected"));
    }
}
