//! Crash dump and diagnostic runaway log detection (GT-183).
//!
//! Safely cleans non-essential diagnostic dumps:
//! - Windows WER minidumps (`%LOCALAPPDATA%\CrashDumps\*.dmp`)
//! - Unreal Engine 4/5 crash folders (`Saved/Crashes`) and logs (`Saved/Logs`)
//! - Unity engine runtime logs (`AppData/LocalLow/*/*/Player.log`)

use crate::janitor::JanitorArtifact;
use crate::rules::Category;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

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

/// Extracts the crashing executable's name from a WER dump file name.
///
/// WER names a dump `<exe>.<pid>.dmp`, e.g. `picard.exe.47628.dmp` yields
/// `Some("picard.exe")` (lowercased, for case-insensitive lookup). A name
/// that does not fit that shape - no numeric pid segment, or nothing ending
/// in `.exe` ahead of it - yields `None` rather than a guess.
fn wer_dump_exe_name(file_name: &str) -> Option<String> {
    let parts: Vec<&str> = file_name.split('.').collect();
    // Minimum shape is `<name>.exe.<pid>.dmp`: 4 dot-separated segments.
    if parts.len() < 4 {
        return None;
    }
    if !parts[parts.len() - 1].eq_ignore_ascii_case("dmp") {
        return None;
    }
    let pid = parts[parts.len() - 2];
    if pid.is_empty() || !pid.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let exe_name = parts[..parts.len() - 2].join(".");
    if !exe_name.to_ascii_lowercase().ends_with(".exe") {
        return None;
    }
    Some(exe_name.to_ascii_lowercase())
}

/// Resolves the WER crash dumps directory (`%LOCALAPPDATA%\CrashDumps`), or
/// `None` if `LOCALAPPDATA` is unset.
fn wer_crash_dumps_dir() -> Option<PathBuf> {
    std::env::var("LOCALAPPDATA")
        .ok()
        .map(|dir| PathBuf::from(dir).join("CrashDumps"))
}

/// The distinct crashing executables (lowercased, e.g. `portal2.exe`) that
/// have a dump waiting in the WER folder.
///
/// Deliberately cheap - one `read_dir`, no metadata reads - so a caller can
/// check it before doing anything expensive: attributing a dump to a game
/// (GT-230) means walking every game's install directory, which is wasted
/// work when the WER folder holds nothing worth attributing.
pub fn wer_crash_dump_candidate_exes() -> HashSet<String> {
    let Some(dir) = wer_crash_dumps_dir() else {
        return HashSet::new();
    };
    candidate_exes_in(&dir)
}

fn candidate_exes_in(dir: &Path) -> HashSet<String> {
    let mut names = HashSet::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return names;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().to_string();
        if let Some(exe) = wer_dump_exe_name(&file_name) {
            names.insert(exe);
        }
    }
    names
}

/// Scans the system Windows Error Reporting crash dumps folder
/// (`%LOCALAPPDATA%\CrashDumps`).
///
/// This folder catches every WER dump on the machine, not just games'
/// (GT-230: a sample from the owner's library turned up a linker, a music
/// tagger, a KDE Connect daemon and Ubisoft Connect itself). A dump is only
/// offered when its crashing executable resolves to an entry in
/// `known_games` (lowercased exe file name -> display title, built by the
/// caller from the discovered libraries); anything that does not match is
/// dropped rather than offered.
pub fn scan_windows_wer_crash_dumps(
    known_games: &HashMap<String, String>,
    cancel: &AtomicBool,
) -> Vec<JanitorArtifact> {
    if known_games.is_empty() {
        return Vec::new();
    }
    let Some(dir) = wer_crash_dumps_dir() else {
        return Vec::new();
    };
    scan_wer_dir(&dir, known_games, cancel)
}

/// The directory walk behind [`scan_windows_wer_crash_dumps`], factored out
/// so it is unit-testable against a tempdir instead of mutating the
/// `LOCALAPPDATA` process env (other tests run in parallel and would race a
/// shared env var).
fn scan_wer_dir(
    crash_dumps_dir: &Path,
    known_games: &HashMap<String, String>,
    cancel: &AtomicBool,
) -> Vec<JanitorArtifact> {
    let mut artifacts = Vec::new();
    if !crash_dumps_dir.is_dir() {
        return artifacts;
    }

    let Ok(entries) = std::fs::read_dir(crash_dumps_dir) else {
        return artifacts;
    };

    for entry in entries.flatten() {
        if cancel.load(Ordering::Relaxed) {
            return artifacts;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().to_string();
        let Some(exe_name) = wer_dump_exe_name(&file_name) else {
            continue;
        };
        let Some(game_title) = known_games.get(&exe_name) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        artifacts.push(JanitorArtifact {
            path,
            category: Category::CrashDump,
            size_bytes: meta.len(),
            description: format!("Windows WER Minidump for {game_title} ({file_name})"),
            is_safe_default: true,
            requires_backup: false,
            app_id: None,
            game_title: Some(game_title.clone()),
            group_dir: None,
        });
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
                    group_dir: None,
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
                    group_dir: None,
                });
            }
        }
    }

    artifacts
}

/// Scans Unity logs under `%USERPROFILE%\AppData\LocalLow`.
pub fn scan_unity_logs(cancel: &AtomicBool) -> Vec<JanitorArtifact> {
    let Ok(user_profile) = std::env::var("USERPROFILE") else {
        return Vec::new();
    };
    let locallow = PathBuf::from(user_profile).join("AppData").join("LocalLow");
    scan_locallow_dir(&locallow, cancel)
}

/// The directory walk behind [`scan_unity_logs`], factored out so it is
/// unit-testable against a tempdir instead of mutating the `USERPROFILE`
/// process env (other tests run in parallel and would race a shared env var).
fn scan_locallow_dir(locallow: &Path, cancel: &AtomicBool) -> Vec<JanitorArtifact> {
    let mut artifacts = Vec::new();
    if !locallow.is_dir() {
        return artifacts;
    }

    let Ok(company_entries) = std::fs::read_dir(locallow) else {
        return artifacts;
    };

    for company in company_entries.flatten() {
        if cancel.load(Ordering::Relaxed) {
            return artifacts;
        }
        let comp_path = company.path();
        if !comp_path.is_dir() {
            continue;
        }

        if let Ok(game_entries) = std::fs::read_dir(&comp_path) {
            for game in game_entries.flatten() {
                if cancel.load(Ordering::Relaxed) {
                    return artifacts;
                }
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
                                    group_dir: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn wer_dump_exe_name_reads_the_executable_out_of_a_wer_file_name() {
        assert_eq!(
            wer_dump_exe_name("picard.exe.47628.dmp"),
            Some("picard.exe".to_string())
        );
        assert_eq!(
            wer_dump_exe_name("upc.exe.123.dmp"),
            Some("upc.exe".to_string())
        );
    }

    #[test]
    fn wer_dump_exe_name_rejects_a_file_name_that_does_not_fit_the_wer_shape() {
        assert_eq!(wer_dump_exe_name("not-a-dump.txt"), None);
        assert_eq!(wer_dump_exe_name("readme.dmp"), None);
        assert_eq!(wer_dump_exe_name("picard.exe.notapid.dmp"), None);
    }

    #[test]
    fn scan_wer_dir_offers_only_the_dump_whose_executable_is_a_known_game() {
        let temp = tempdir().expect("tempdir");
        std::fs::write(temp.path().join("portal2.exe.1111.dmp"), b"dump").expect("write dump");
        std::fs::write(temp.path().join("link.exe.2222.dmp"), b"dump").expect("write dump");

        let mut known_games = HashMap::new();
        known_games.insert("portal2.exe".to_string(), "Portal 2".to_string());

        let cancel = AtomicBool::new(false);
        let artifacts = scan_wer_dir(temp.path(), &known_games, &cancel);

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].game_title, Some("Portal 2".to_string()));
        assert_eq!(
            artifacts[0].path.file_name().and_then(|n| n.to_str()),
            Some("portal2.exe.1111.dmp")
        );
    }

    #[test]
    fn scan_wer_dir_returns_immediately_when_cancelled() {
        let temp = tempdir().expect("tempdir");
        std::fs::write(temp.path().join("portal2.exe.1111.dmp"), b"dump").expect("write dump");

        let mut known_games = HashMap::new();
        known_games.insert("portal2.exe".to_string(), "Portal 2".to_string());

        let cancel = AtomicBool::new(true);
        let artifacts = scan_wer_dir(temp.path(), &known_games, &cancel);

        assert!(artifacts.is_empty());
    }

    #[test]
    fn scan_locallow_dir_returns_immediately_when_cancelled() {
        let temp = tempdir().expect("tempdir");
        let game_dir = temp.path().join("Some Studio").join("Some Game");
        std::fs::create_dir_all(&game_dir).expect("create game dir");
        std::fs::write(game_dir.join("Player.log"), vec![0u8; 2 * 1024 * 1024]).expect("write log");

        let cancel = AtomicBool::new(true);
        let artifacts = scan_locallow_dir(temp.path(), &cancel);

        assert!(artifacts.is_empty());
    }
}
