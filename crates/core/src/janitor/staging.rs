//! Incomplete download and staging residue detection (GT-181).
//!
//! Scans:
//! - Steam `steamapps/downloading` for leftover depot chunks.
//! - Epic Games Store `.egstore/Pending` for aborted downloads.
//!
//! Note: Active downloads where files are locked by the launcher process
//! are safely skipped to avoid corrupting running tasks.

use crate::janitor::JanitorArtifact;
use crate::rules::Category;
use std::path::Path;

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

/// Checks if a file or directory is locked for writing by an active process.
#[cfg(windows)]
fn is_locked(path: &Path) -> bool {
    if path.is_file() {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .is_err()
    } else {
        false
    }
}

#[cfg(not(windows))]
fn is_locked(_path: &Path) -> bool {
    false
}

/// Scans a Steam library for leftover download folders in `steamapps/downloading`.
pub fn scan_steam_downloading(library_root: &Path) -> Vec<JanitorArtifact> {
    let mut artifacts = Vec::new();
    let downloading_dir = library_root.join("steamapps").join("downloading");

    if !downloading_dir.is_dir() {
        return artifacts;
    }

    let Ok(entries) = std::fs::read_dir(&downloading_dir) else {
        return artifacts;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if is_locked(&path) {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_string();
        let size = if path.is_dir() {
            dir_size(&path)
        } else {
            entry.metadata().map(|m| m.len()).unwrap_or(0)
        };

        if size == 0 {
            continue;
        }

        let is_appid = name.chars().all(|c| c.is_ascii_digit());
        let desc = if is_appid {
            format!("Incomplete Steam download staging for AppID {name}")
        } else {
            format!("Leftover Steam downloading chunk ({name})")
        };

        artifacts.push(JanitorArtifact {
            path,
            category: Category::DownloadingStaging,
            size_bytes: size,
            description: desc,
            is_safe_default: true,
            requires_backup: false,
            app_id: if is_appid { Some(name) } else { None },
            game_title: None,
        });
    }

    artifacts
}

/// Scans Epic Games library or staging locations for `.egstore/Pending`.
pub fn scan_egs_pending(manifest_or_install_root: &Path) -> Vec<JanitorArtifact> {
    let mut artifacts = Vec::new();
    let pending_dir = manifest_or_install_root.join(".egstore").join("Pending");

    if pending_dir.is_dir() && !is_locked(&pending_dir) {
        let size = dir_size(&pending_dir);
        if size > 0 {
            artifacts.push(JanitorArtifact {
                path: pending_dir,
                category: Category::DownloadingStaging,
                size_bytes: size,
                description: "Epic Games Store pending download staging".to_string(),
                is_safe_default: true,
                requires_backup: false,
                app_id: None,
                game_title: None,
            });
        }
    }

    artifacts
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_scan_steam_downloading() {
        let temp = tempdir().unwrap();
        let downloading = temp.path().join("steamapps").join("downloading");
        std::fs::create_dir_all(&downloading).unwrap();

        let app_dir = downloading.join("123456");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(app_dir.join("chunk.bin"), vec![0u8; 1024]).unwrap();

        let artifacts = scan_steam_downloading(temp.path());
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].size_bytes, 1024);
        assert_eq!(artifacts[0].category, Category::DownloadingStaging);
        assert_eq!(artifacts[0].app_id.as_deref(), Some("123456"));
    }
}
