//! GPU and Steam shader cache analyzer (GT-182).
//!
//! Safely detects stale GPU shader caches and uninstalled Steam pipeline caches:
//! - NVIDIA DXCache / GLCache (`%LOCALAPPDATA%\NVIDIA\DXCache`, `GLCache`)
//! - AMD DxCache (`%LOCALAPPDATA%\AMD\DxCache`)
//! - DirectX D3DSCache (`%LOCALAPPDATA%\D3DSCache`)
//! - Steam `steamapps/shadercache/<appid>`
//!
//! Employs an age-based (mtime) filter (default: 30 days) to ensure active games
//! do not suffer shader recompilation stutter.

use crate::janitor::JanitorArtifact;
use crate::rules::Category;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Reports every file in `dir_path` last modified more than `stale_days` ago
/// as its own artifact.
///
/// One artifact per file, not one per directory: a GPU cache directory holds
/// stale and fresh entries side by side, and a directory-shaped artifact whose
/// size counted only the stale half would hand the deletion path a target
/// bigger than the thing that was measured - the fresh shaders of a game
/// played yesterday included. What is measured here is exactly what a later
/// deletion removes.
pub fn scan_stale_cache_files(
    dir_path: &Path,
    stale_days: u32,
    desc_prefix: &str,
) -> Vec<JanitorArtifact> {
    let mut artifacts = Vec::new();
    if !dir_path.is_dir() {
        return artifacts;
    }

    let cutoff_duration = Duration::from_secs(stale_days as u64 * 86400);
    let now = SystemTime::now();

    let Ok(entries) = std::fs::read_dir(dir_path) else {
        return artifacts;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let is_stale = meta
            .modified()
            .ok()
            .and_then(|mtime| now.duration_since(mtime).ok())
            .map(|elapsed| elapsed >= cutoff_duration)
            .unwrap_or(false);
        if !is_stale || meta.len() == 0 {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        artifacts.push(JanitorArtifact {
            path,
            category: Category::ShaderCache,
            size_bytes: meta.len(),
            description: format!(
                "{desc_prefix} - stale cache file {name} (> {stale_days} days old)"
            ),
            is_safe_default: true,
            requires_backup: false,
            app_id: None,
            game_title: None,
            group_dir: None,
        });
    }

    artifacts
}

/// Discovers standard GPU driver shader cache locations on Windows.
pub fn get_system_gpu_cache_dirs() -> Vec<(PathBuf, &'static str)> {
    let mut dirs = Vec::new();

    if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
        let base = PathBuf::from(local_appdata);
        // NVIDIA
        dirs.push((
            base.join("NVIDIA").join("DXCache"),
            "NVIDIA DirectX Shader Cache (DXCache)",
        ));
        dirs.push((
            base.join("NVIDIA").join("GLCache"),
            "NVIDIA OpenGL/Vulkan Cache (GLCache)",
        ));
        dirs.push((
            base.join("NVIDIA Corporation").join("NV_Cache"),
            "NVIDIA Legacy NV_Cache",
        ));
        // AMD
        dirs.push((
            base.join("AMD").join("DxCache"),
            "AMD Radeon DirectX Shader Cache (DxCache)",
        ));
        dirs.push((
            base.join("AMD").join("DxcCache"),
            "AMD Radeon DXC Shader Cache",
        ));
        // Microsoft DirectX
        dirs.push((base.join("D3DSCache"), "Direct3D Shader Cache (D3DSCache)"));
    }

    if let Ok(appdata) = std::env::var("APPDATA") {
        let base = PathBuf::from(appdata);
        dirs.push((
            base.join("NVIDIA").join("ComputeCache"),
            "NVIDIA Compute / CUDA Shader Cache",
        ));
    }

    dirs
}

/// Scans standard system GPU shader cache directories for stale files.
pub fn scan_system_shader_caches(stale_days: u32) -> Vec<JanitorArtifact> {
    let mut artifacts = Vec::new();
    for (dir, desc) in get_system_gpu_cache_dirs() {
        artifacts.extend(scan_stale_cache_files(&dir, stale_days, desc));
    }
    artifacts
}

/// Scans a Steam library `steamapps/shadercache` directory.
/// Identifies shader cache folders for uninstalled AppIDs.
pub fn scan_steam_shader_cache(
    library_root: &Path,
    installed_app_ids: &HashSet<String>,
) -> Vec<JanitorArtifact> {
    let mut artifacts = Vec::new();
    let shadercache_dir = library_root.join("steamapps").join("shadercache");

    if !shadercache_dir.is_dir() {
        return artifacts;
    }

    let Ok(entries) = std::fs::read_dir(&shadercache_dir) else {
        return artifacts;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let app_id = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) if name.chars().all(|c| c.is_ascii_digit()) => name.to_string(),
            _ => continue,
        };

        if !installed_app_ids.contains(&app_id) {
            let size = dir_size(&path);
            if size > 0 {
                artifacts.push(JanitorArtifact {
                    path,
                    category: Category::ShaderCache,
                    size_bytes: size,
                    description: format!(
                        "Steam Fossilize/Vulkan shader cache for uninstalled game (AppID {app_id})"
                    ),
                    is_safe_default: true,
                    requires_backup: false,
                    app_id: Some(app_id),
                    game_title: None,
                    group_dir: None,
                });
            }
        }
    }

    artifacts
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_scan_steam_shader_cache() {
        let temp = tempdir().unwrap();
        let sc_dir = temp
            .path()
            .join("steamapps")
            .join("shadercache")
            .join("999999");
        std::fs::create_dir_all(&sc_dir).unwrap();
        std::fs::write(sc_dir.join("foz_pipelines.bin"), vec![0u8; 2048]).unwrap();

        let mut installed = HashSet::new();
        installed.insert("111111".to_string());

        let artifacts = scan_steam_shader_cache(temp.path(), &installed);
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].size_bytes, 2048);
        assert_eq!(artifacts[0].app_id.as_deref(), Some("999999"));
    }

    #[test]
    fn stale_cache_files_are_reported_one_per_file_at_their_own_size() {
        // A cache directory is never deleted whole: each stale file is its own
        // artifact, so what a later deletion removes is what was measured.
        let temp = tempdir().unwrap();
        std::fs::write(temp.path().join("a.bin"), vec![0u8; 1024]).unwrap();
        std::fs::write(temp.path().join("b.bin"), vec![0u8; 2048]).unwrap();
        std::fs::write(temp.path().join("empty.bin"), Vec::new()).unwrap();
        std::fs::create_dir(temp.path().join("nested")).unwrap();

        // `stale_days: 0` makes every file stale, which is what isolates the
        // shape of the result from the clock.
        let mut artifacts = scan_stale_cache_files(temp.path(), 0, "Test cache");
        artifacts.sort_by_key(|artifact| artifact.size_bytes);

        assert_eq!(
            artifacts.len(),
            2,
            "an empty file and a directory are not artifacts"
        );
        assert_eq!(artifacts[0].path, temp.path().join("a.bin"));
        assert_eq!(artifacts[0].size_bytes, 1024);
        assert_eq!(artifacts[1].path, temp.path().join("b.bin"));
        assert_eq!(artifacts[1].size_bytes, 2048);
    }
}
