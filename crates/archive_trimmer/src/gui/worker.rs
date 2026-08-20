//! Background Worker Thread Communication & Async Task Orchestrator.
//!
//! Offloads heavy SQLite querying, file-system archive analysis, header parsing,
//! read-only archive inspection and guarded trim attempts to background threads,
//! streaming live status updates and progress events back to the `egui` UI loop via `mpsc::channel`.

use eframe::egui;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;

use crate::anti_cheat::{self, SafetyReport};
use crate::db_reader::GameArchiveCandidates;
use crate::formats::{
    is_external_single_language_file, ArchiveAnalysis, ArchiveType, FormatDetector,
};
use crate::logger;

/// Detailed analysis of an individual archive in a game.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedArchive {
    pub file_id: i64,
    pub rel_path: String,
    pub full_path: PathBuf,
    pub size: u64,
    pub on_disk_size: u64,
    pub archive_type: Option<ArchiveType>,
    pub analysis: Option<ArchiveAnalysis>,
    pub error: Option<String>,
}

/// Aggregated analysis results for a game.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameScanResult {
    pub game_id: i64,
    pub game_name: String,
    pub install_dir: PathBuf,
    pub library_path: PathBuf,
    pub game_root: PathBuf,
    pub is_safe: bool,
    pub safety_report: SafetyReport,
    pub candidate_files_count: usize,
    pub archives: Vec<ScannedArchive>,
    pub total_logical_size: u64,
    pub total_on_disk_size: u64,
    pub total_trimmable_bytes: u64,
    pub detected_languages: Vec<String>,
}

/// Messages emitted by background worker threads to the UI.
#[derive(Debug)]
pub enum WorkerMsg {
    /// Incremental progress when scanning games from the database.
    ScanProgress {
        current_game: usize,
        total_games: usize,
        game_name: String,
    },
    /// A single game has been fully analyzed.
    GameScanned { game_result: Box<GameScanResult> },
    /// Scanning all games in the batch is complete.
    ScanComplete {
        total_savings: u64,
        total_archives: usize,
        total_games_with_monoliths: usize,
    },
    /// Informational log message.
    Log { message: String },
    /// Error notification.
    Error { message: String },
}

/// Creates a new worker communication channel.
pub fn create_worker_channel() -> (Sender<WorkerMsg>, Receiver<WorkerMsg>) {
    channel()
}

/// Spawns a background thread to inspect and analyze candidate archives across all games.
pub fn spawn_scan_candidates(
    candidates: Vec<GameArchiveCandidates>,
    tx: Sender<WorkerMsg>,
    ctx: egui::Context,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let panic_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let total_games = candidates.len();
            let mut total_savings = 0u64;
            let mut total_archives = 0usize;
            let mut total_games_with_monoliths = 0usize;

            for (i, game) in candidates.into_iter().enumerate() {
                let game_root = game.game_root();
                let game_name = game.game_name.clone();

                let _ = tx.send(WorkerMsg::ScanProgress {
                    current_game: i + 1,
                    total_games,
                    game_name: game_name.clone(),
                });
                ctx.request_repaint();

                // 1. Check Anti-Cheat safety for this game
                let safety_report =
                    anti_cheat::check_game_safety(&game_root, false).unwrap_or_default();
                let is_safe = safety_report.is_safe;

                let mut archives = Vec::new();
                let mut total_game_logical_size = 0u64;
                let mut total_game_on_disk_size = 0u64;
                let mut total_game_trimmable_bytes = 0u64;
                let mut detected_languages = Vec::new();

                // 2. Inspect candidate files
                for file in &game.candidate_files {
                    let full_path = &file.full_path;
                    let rel_path = file.rel_path.clone();
                    let file_id = file.file_id;

                    if !full_path.exists() {
                        continue;
                    }

                    // Exclude standalone whole external language files
                    if is_external_single_language_file(&rel_path) {
                        continue;
                    }

                    match FormatDetector::detect_file(full_path) {
                        Ok(Some(archive_type)) => {
                            let handler = FormatDetector::get_handler(archive_type);
                            match handler.analyze(full_path) {
                                Ok(analysis) => {
                                    // ONLY consider valid monolithic archives that have internal trimmable language streams
                                    if analysis.total_trimmable_bytes > 0
                                        && analysis.trimmable_chunks.iter().any(|c| c.is_language)
                                    {
                                        total_game_logical_size = total_game_logical_size
                                            .saturating_add(analysis.total_size);
                                        total_game_on_disk_size = total_game_on_disk_size
                                            .saturating_add(analysis.on_disk_size);
                                        total_game_trimmable_bytes = total_game_trimmable_bytes
                                            .saturating_add(analysis.total_trimmable_bytes);

                                        for lang in &analysis.detected_languages {
                                            if !detected_languages.contains(lang) {
                                                detected_languages.push(lang.clone());
                                            }
                                        }

                                        archives.push(ScannedArchive {
                                            file_id,
                                            rel_path,
                                            full_path: full_path.clone(),
                                            size: analysis.total_size,
                                            on_disk_size: analysis.on_disk_size,
                                            archive_type: Some(archive_type),
                                            analysis: Some(analysis),
                                            error: None,
                                        });
                                    }
                                }
                                Err(err) => {
                                    let err_str = err.to_string();
                                    logger::log_entry(
                                        "WARN",
                                        &format!("Failed to analyze {:?}: {}", full_path, err_str),
                                    );
                                    let _ = tx.send(WorkerMsg::Log {
                                        message: format!(
                                            "[WARN] Failed to analyze {:?}: {}",
                                            full_path, err_str
                                        ),
                                    });
                                }
                            }
                        }
                        Ok(None) => {
                            // Not a recognized monolith archive or skipped
                        }
                        Err(err) => {
                            let err_str = err.to_string();
                            logger::log_entry(
                                "WARN",
                                &format!("Error detecting format for {:?}: {}", full_path, err_str),
                            );
                            let _ = tx.send(WorkerMsg::Log {
                                message: format!(
                                    "[WARN] Error detecting format for {:?}: {}",
                                    full_path, err_str
                                ),
                            });
                        }
                    }
                }

                detected_languages.sort();

                // ONLY emit and count game if it contains at least one monolithic archive with internal trimmable localizations
                if !archives.is_empty() {
                    total_games_with_monoliths += 1;
                    total_archives += archives.len();
                    total_savings = total_savings.saturating_add(total_game_trimmable_bytes);

                    let game_result = GameScanResult {
                        game_id: game.game_id,
                        game_name,
                        install_dir: game.install_dir,
                        library_path: game.library_path,
                        game_root,
                        is_safe,
                        safety_report,
                        candidate_files_count: game.candidate_files.len(),
                        archives,
                        total_logical_size: total_game_logical_size,
                        total_on_disk_size: total_game_on_disk_size,
                        total_trimmable_bytes: total_game_trimmable_bytes,
                        detected_languages,
                    };

                    let _ = tx.send(WorkerMsg::GameScanned {
                        game_result: Box::new(game_result),
                    });
                    ctx.request_repaint();
                }
            }

            let _ = tx.send(WorkerMsg::ScanComplete {
                total_savings,
                total_archives,
                total_games_with_monoliths,
            });
            ctx.request_repaint();
        }));

        if let Err(panic_err) = panic_res {
            let msg = if let Some(s) = panic_err.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = panic_err.downcast_ref::<String>() {
                s.clone()
            } else {
                "Unknown panic in scan worker thread".to_string()
            };
            let _ = tx.send(WorkerMsg::Error {
                message: format!("Scanner worker panic: {msg}"),
            });
            let _ = tx.send(WorkerMsg::ScanComplete {
                total_savings: 0,
                total_archives: 0,
                total_games_with_monoliths: 0,
            });
            ctx.request_repaint();
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn standalone_gui_worker_remains_read_only() {
        let source = include_str!("worker.rs");
        let direct_trim_call = [".tr", "im("].concat();
        let batch_trim_call = ["batch_", "trim_game("].concat();
        let single_spawn = ["spawn_", "trim_archive"].concat();
        let batch_spawn = ["spawn_batch_", "trim_game"].concat();

        assert!(!source.contains(&direct_trim_call));
        assert!(!source.contains(&batch_trim_call));
        assert!(!source.contains(&single_spawn));
        assert!(!source.contains(&batch_spawn));
    }
}
