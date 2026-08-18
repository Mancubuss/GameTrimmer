//! Crash dump and diagnostic runaway log detection (GT-183).
//!
//! Safely cleans non-essential diagnostic dumps:
//! - Windows WER minidumps (`%LOCALAPPDATA%\CrashDumps\*.dmp`)
//! - Unreal Engine 4/5 crash folders (`Saved/Crashes`) and logs (`Saved/Logs`)
//! - Unity engine runtime logs (`AppData/LocalLow/*/*/Player.log`)

use crate::janitor::JanitorArtifact;
use crate::rules::Category;
use std::path::{Path, PathBuf};

/// Recursively computes directory size.
fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                if let Ok(meta) = entry.metadata() {
                    total += meta.len();
                }
            } else if p.is_dir() {
                total += dir_size(&p);
            }
        }
    }
    total
}

/// Scans the system Windows Error Reporting crash dumps folder (`%LOCALAPPDATA%\CrashDumps`).
pub fn scan_windows_wer_crash_dumps() -> Vec<JanitorArtifact> {
    let mut artifacts = Vec::new();
    let Ok(local_appdata) = std::env::var("LOCALAPPDATA") else {
        return artifacts;
    };

    let crash_dumps_dir = PathBuf::from(local_appdata).join("CrashDumps");
    if !crash_dumps_dir.is_dir() {
        return artifacts;
    }

    let Ok(entries) = std::fs::read_dir(&crash_dumps_dir) else {
        return artifacts;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default();
            if ext.eq_ignore_ascii_case("dmp") {
                if let Ok(meta) = entry.metadata() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    artifacts.push(JanitorArtifact {
                        path,
                        category: Category::CrashDump,
                        size_bytes: meta.len(),
                        description: format!("Windows WER Minidump ({name})"),
                        is_safe_default: true,
                        requires_backup: false,
                        app_id: None,
                        game_title: None,
                    });
                }
            }
        }
    }

    artifacts
}

/// Scans a game install directory for Unreal Engine crash folders and logs.
pub fn scan_game_engine_crashes(
    game_root: &Path,
    game_title: Option<&str>,
) -> Vec<JanitorArtifact> {
    let mut artifacts = Vec::new();

    // Check for Saved/Crashes and Saved/Logs at root and 1-level subdirectories
    let candidates = vec![game_root.join("Saved")];

    let mut check_dirs = candidates;
    if let Ok(entries) = std::fs::read_dir(game_root) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                check_dirs.push(p.join("Saved"));
            }
        }
    }

    for saved_dir in check_dirs {
        if !saved_dir.is_dir() {
            continue;
        }

        // 1. Saved/Crashes
        let crashes_dir = saved_dir.join("Crashes");
        if crashes_dir.is_dir() {
            let size = dir_size(&crashes_dir);
            if size > 0 {
                artifacts.push(JanitorArtifact {
                    path: crashes_dir,
                    category: Category::CrashDump,
                    size_bytes: size,
                    description: format!(
                        "Unreal Engine crash reports ({})",
                        game_title.unwrap_or("Game")
                    ),
                    is_safe_default: true,
                    requires_backup: false,
                    app_id: None,
                    game_title: game_title.map(|s| s.to_string()),
                });
            }
        }

        // 2. Saved/Logs
        let logs_dir = saved_dir.join("Logs");
        if logs_dir.is_dir() {
            let size = dir_size(&logs_dir);
            if size > 0 {
                artifacts.push(JanitorArtifact {
                    path: logs_dir,
                    category: Category::DiagnosticLogs,
                    size_bytes: size,
                    description: format!(
                        "Unreal Engine diagnostic logs ({})",
                        game_title.unwrap_or("Game")
                    ),
                    is_safe_default: true,
                    requires_backup: false,
                    app_id: None,
                    game_title: game_title.map(|s| s.to_string()),
                });
            }
        }
    }

    artifacts
}

/// Scans Unity logs under `%USERPROFILE%\AppData\LocalLow`.
pub fn scan_unity_logs() -> Vec<JanitorArtifact> {
    let mut artifacts = Vec::new();
    let Ok(user_profile) = std::env::var("USERPROFILE") else {
        return artifacts;
    };

    let locallow = PathBuf::from(user_profile).join("AppData").join("LocalLow");
    if !locallow.is_dir() {
        return artifacts;
    }

    let Ok(company_entries) = std::fs::read_dir(&locallow) else {
        return artifacts;
    };

    for company in company_entries.flatten() {
        let comp_path = company.path();
        if !comp_path.is_dir() {
            continue;
        }

        if let Ok(game_entries) = std::fs::read_dir(&comp_path) {
            for game in game_entries.flatten() {
                let game_path = game.path();
                if !game_path.is_dir() {
                    continue;
                }

                for log_name in ["Player.log", "Player-prev.log"] {
                    let log_file = game_path.join(log_name);
                    if log_file.is_file() {
                        if let Ok(meta) = log_file.metadata() {
                            if meta.len() > 1024 * 1024 {
                                // Only logs larger than 1 MB to avoid noise
                                let g_name = game_path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("Game");
                                artifacts.push(JanitorArtifact {
                                    path: log_file,
                                    category: Category::DiagnosticLogs,
                                    size_bytes: meta.len(),
                                    description: format!(
                                        "Unity Player log for {g_name} ({log_name})"
                                    ),
                                    is_safe_default: true,
                                    requires_backup: false,
                                    app_id: None,
                                    game_title: Some(g_name.to_string()),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    artifacts
}
