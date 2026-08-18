//! Launcher CEF/Chromium web caches and mod manager downloads analyzer (GT-185).
//!
//! Scans:
//! - Steam CEF web cache (`%LOCALAPPDATA%\Steam\htmlcache`)
//! - Ubisoft Connect HTTP cache (`%LOCALAPPDATA%\Ubisoft Game Launcher\cache\http`)
//! - EA Desktop webcache & logs (`%LOCALAPPDATA%\Electronic Arts\EA Desktop\webcache`)
//! - GOG Galaxy CEF cache (`%LOCALAPPDATA%\GOG.com\Galaxy\webcache`)
//! - Vortex mod manager downloaded archives (`%APPDATA%\Vortex\downloads`)
//! - Mod Organizer 2 downloaded archives

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

/// Discovers and scans standard launcher CEF/Chromium web cache directories.
pub fn scan_launcher_web_caches() -> Vec<JanitorArtifact> {
    let mut artifacts = Vec::new();

    if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
        let base = PathBuf::from(local_appdata);

        // 1. Steam htmlcache
        let steam_htmlcache = base.join("Steam").join("htmlcache");
        if steam_htmlcache.is_dir() {
            let size = dir_size(&steam_htmlcache);
            if size > 10 * 1024 * 1024 {
                // Only report if > 10MB
                artifacts.push(JanitorArtifact {
                    path: steam_htmlcache,
                    category: Category::LauncherWebCache,
                    size_bytes: size,
                    description: "Steam CEF webview browser cache (htmlcache)".to_string(),
                    is_safe_default: true,
                    requires_backup: false,
                    app_id: None,
                    game_title: Some("Steam".to_string()),
                });
            }
        }

        // 2. Ubisoft Connect
        let ubi_http = base
            .join("Ubisoft Game Launcher")
            .join("cache")
            .join("http");
        if ubi_http.is_dir() {
            let size = dir_size(&ubi_http);
            if size > 5 * 1024 * 1024 {
                artifacts.push(JanitorArtifact {
                    path: ubi_http,
                    category: Category::LauncherWebCache,
                    size_bytes: size,
                    description: "Ubisoft Connect launcher HTTP web cache".to_string(),
                    is_safe_default: true,
                    requires_backup: false,
                    app_id: None,
                    game_title: Some("Ubisoft Connect".to_string()),
                });
            }
        }

        // 3. EA Desktop
        let ea_web = base
            .join("Electronic Arts")
            .join("EA Desktop")
            .join("webcache");
        if ea_web.is_dir() {
            let size = dir_size(&ea_web);
            if size > 5 * 1024 * 1024 {
                artifacts.push(JanitorArtifact {
                    path: ea_web,
                    category: Category::LauncherWebCache,
                    size_bytes: size,
                    description: "EA App / EA Desktop webcache".to_string(),
                    is_safe_default: true,
                    requires_backup: false,
                    app_id: None,
                    game_title: Some("EA App".to_string()),
                });
            }
        }

        // 4. GOG Galaxy
        let gog_web = base.join("GOG.com").join("Galaxy").join("webcache");
        if gog_web.is_dir() {
            let size = dir_size(&gog_web);
            if size > 5 * 1024 * 1024 {
                artifacts.push(JanitorArtifact {
                    path: gog_web,
                    category: Category::LauncherWebCache,
                    size_bytes: size,
                    description: "GOG Galaxy CEF web browser cache".to_string(),
                    is_safe_default: true,
                    requires_backup: false,
                    app_id: None,
                    game_title: Some("GOG Galaxy".to_string()),
                });
            }
        }
    }

    artifacts
}

/// Scans for mod manager downloaded archives (Vortex, MO2).
pub fn scan_mod_manager_downloads() -> Vec<JanitorArtifact> {
    let mut artifacts = Vec::new();

    if let Ok(appdata) = std::env::var("APPDATA") {
        let vortex_dl = PathBuf::from(appdata).join("Vortex").join("downloads");
        if vortex_dl.is_dir() {
            let size = dir_size(&vortex_dl);
            if size > 100 * 1024 * 1024 {
                // Only report if substantial (>100MB)
                artifacts.push(JanitorArtifact {
                    path: vortex_dl,
                    category: Category::ModManagerDownloads,
                    size_bytes: size,
                    description: "Vortex Mod Manager downloaded mod archives".to_string(),
                    is_safe_default: false, // Informational / optional cleanup
                    requires_backup: false,
                    app_id: None,
                    game_title: Some("Vortex Mod Manager".to_string()),
                });
            }
        }
    }

    artifacts
}
