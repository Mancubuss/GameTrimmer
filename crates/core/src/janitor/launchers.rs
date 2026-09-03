//! Launcher CEF/Chromium web caches and mod manager downloads analyzer (GT-185).
//!
//! Scans:
//! - Steam CEF web cache (`%LOCALAPPDATA%\Steam\htmlcache`)
//! - Ubisoft Connect HTTP cache (`%LOCALAPPDATA%\Ubisoft Game Launcher\cache\http`)
//! - EA Desktop webcache & logs (`%LOCALAPPDATA%\Electronic Arts\EA Desktop\webcache`)
//! - GOG Galaxy CEF cache (`%LOCALAPPDATA%\GOG.com\Galaxy\webcache`)
//! - Battle.net client and browser caches (`%LOCALAPPDATA%\Battle.net\{Cache,BrowserCaches}`)
//! - Epic Games Launcher CEF cache (`%LOCALAPPDATA%\EpicGamesLauncher\Saved\webcache_*`)
//! - Vortex mod manager downloaded archives (`%APPDATA%\Vortex\downloads`)
//! - Mod Organizer 2 downloaded archives

use crate::janitor::JanitorArtifact;
use crate::rules::Category;
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

/// One launcher's web-cache directory.
///
/// A table rather than four (now seven) copies of the same
/// exists-measure-threshold block: every entry differs only in the path, the
/// label and the size worth reporting, and adding Battle.net or the Epic
/// launcher should mean adding a row.
struct WebCacheDef {
    /// Environment variable naming the root directory (`LOCALAPPDATA`, `APPDATA`).
    root_var: &'static str,
    /// Path under that root, `/`-separated.
    rel_path: &'static str,
    /// Whether the last segment of `rel_path` is a *prefix*: the Epic launcher
    /// versions its cache per Chromium build (`webcache_4430`), so the exact
    /// name changes underneath the user.
    wildcard_leaf: bool,
    description: &'static str,
    launcher: &'static str,
    /// Below this, the cache is not worth a row in the results.
    min_bytes: u64,
}

const WEB_CACHES: &[WebCacheDef] = &[
    WebCacheDef {
        root_var: "LOCALAPPDATA",
        rel_path: "Steam/htmlcache",
        wildcard_leaf: false,
        description: "Steam CEF webview browser cache (htmlcache)",
        launcher: "Steam",
        min_bytes: 10 * 1024 * 1024,
    },
    WebCacheDef {
        root_var: "LOCALAPPDATA",
        rel_path: "Ubisoft Game Launcher/cache/http",
        wildcard_leaf: false,
        description: "Ubisoft Connect launcher HTTP web cache",
        launcher: "Ubisoft Connect",
        min_bytes: 5 * 1024 * 1024,
    },
    WebCacheDef {
        root_var: "LOCALAPPDATA",
        rel_path: "Electronic Arts/EA Desktop/webcache",
        wildcard_leaf: false,
        description: "EA App / EA Desktop webcache",
        launcher: "EA App",
        min_bytes: 5 * 1024 * 1024,
    },
    WebCacheDef {
        root_var: "LOCALAPPDATA",
        rel_path: "GOG.com/Galaxy/webcache",
        wildcard_leaf: false,
        description: "GOG Galaxy CEF web browser cache",
        launcher: "GOG Galaxy",
        min_bytes: 5 * 1024 * 1024,
    },
    WebCacheDef {
        root_var: "LOCALAPPDATA",
        rel_path: "Battle.net/Cache",
        wildcard_leaf: false,
        description: "Battle.net client asset and metadata cache",
        launcher: "Battle.net",
        min_bytes: 5 * 1024 * 1024,
    },
    WebCacheDef {
        root_var: "LOCALAPPDATA",
        rel_path: "Battle.net/BrowserCaches",
        wildcard_leaf: false,
        description: "Battle.net embedded browser caches",
        launcher: "Battle.net",
        min_bytes: 5 * 1024 * 1024,
    },
    WebCacheDef {
        root_var: "LOCALAPPDATA",
        rel_path: "EpicGamesLauncher/Saved/webcache",
        wildcard_leaf: true,
        description: "Epic Games Launcher CEF web cache",
        launcher: "Epic Games Launcher",
        min_bytes: 5 * 1024 * 1024,
    },
];

/// Every directory one [`WebCacheDef`] names on this machine: one path, or -
/// for a versioned cache - every sibling sharing its prefix.
fn cache_dirs(def: &WebCacheDef) -> Vec<PathBuf> {
    let Ok(root) = std::env::var(def.root_var) else {
        return Vec::new();
    };
    cache_dirs_in(Path::new(&root), def)
}

/// [`cache_dirs`] against an explicit root, so the wildcard rule can be tested
/// without touching the process environment.
fn cache_dirs_in(root: &Path, def: &WebCacheDef) -> Vec<PathBuf> {
    let full = def
        .rel_path
        .split('/')
        .fold(root.to_path_buf(), |path, segment| path.join(segment));

    if !def.wildcard_leaf {
        return if full.is_dir() {
            vec![full]
        } else {
            Vec::new()
        };
    }

    let (Some(parent), Some(prefix)) = (
        full.parent(),
        full.file_name().and_then(|name| name.to_str()),
    ) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(prefix))
        })
        .collect()
}

/// Discovers and scans standard launcher CEF/Chromium web cache directories.
pub fn scan_launcher_web_caches(cancel: &AtomicBool) -> Vec<JanitorArtifact> {
    let mut artifacts = Vec::new();

    for def in WEB_CACHES {
        if cancel.load(Ordering::Relaxed) {
            return artifacts;
        }
        for dir in cache_dirs(def) {
            let size = dir_size(&dir);
            if size < def.min_bytes {
                continue;
            }
            artifacts.push(JanitorArtifact {
                path: dir,
                category: Category::LauncherWebCache,
                size_bytes: size,
                description: def.description.to_string(),
                is_safe_default: true,
                requires_backup: false,
                app_id: None,
                game_title: Some(def.launcher.to_string()),
                group_dir: None,
            });
        }
    }

    artifacts
}

/// Scans for mod manager downloaded archives (Vortex, MO2).
pub fn scan_mod_manager_downloads(cancel: &AtomicBool) -> Vec<JanitorArtifact> {
    let mut artifacts = Vec::new();
    if cancel.load(Ordering::Relaxed) {
        return artifacts;
    }

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
                    group_dir: None,
                });
            }
        }
    }

    artifacts
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const EPIC: WebCacheDef = WebCacheDef {
        root_var: "LOCALAPPDATA",
        rel_path: "EpicGamesLauncher/Saved/webcache",
        wildcard_leaf: true,
        description: "Epic Games Launcher CEF web cache",
        launcher: "Epic Games Launcher",
        min_bytes: 0,
    };

    const STEAM: WebCacheDef = WebCacheDef {
        root_var: "LOCALAPPDATA",
        rel_path: "Steam/htmlcache",
        wildcard_leaf: false,
        description: "Steam CEF webview browser cache (htmlcache)",
        launcher: "Steam",
        min_bytes: 0,
    };

    #[test]
    fn a_versioned_cache_is_found_under_every_build_suffix() {
        // The Epic launcher names its cache after the Chromium build it ships
        // (`webcache_4430`), and renames it on update - an exact-path lookup
        // finds nothing the day after.
        let temp = tempdir().expect("tempdir");
        let saved = temp.path().join("EpicGamesLauncher").join("Saved");
        std::fs::create_dir_all(saved.join("webcache_4430")).expect("create webcache_4430");
        std::fs::create_dir_all(saved.join("webcache_4147")).expect("create webcache_4147");
        std::fs::create_dir_all(saved.join("Logs")).expect("create Logs");

        let mut found = cache_dirs_in(temp.path(), &EPIC);
        found.sort();

        assert_eq!(
            found,
            vec![saved.join("webcache_4147"), saved.join("webcache_4430")],
            "both cache builds, and nothing else under Saved"
        );
    }

    #[test]
    fn an_exact_cache_is_found_only_where_it_is() {
        let temp = tempdir().expect("tempdir");
        assert!(cache_dirs_in(temp.path(), &STEAM).is_empty());

        let htmlcache = temp.path().join("Steam").join("htmlcache");
        std::fs::create_dir_all(&htmlcache).expect("create htmlcache");
        assert_eq!(cache_dirs_in(temp.path(), &STEAM), vec![htmlcache]);
    }

    #[test]
    fn scan_launcher_web_caches_returns_immediately_when_cancelled() {
        let cancel = AtomicBool::new(true);
        let artifacts = scan_launcher_web_caches(&cancel);
        assert!(artifacts.is_empty());
    }

    #[test]
    fn scan_mod_manager_downloads_returns_immediately_when_cancelled() {
        let cancel = AtomicBool::new(true);
        let artifacts = scan_mod_manager_downloads(&cancel);
        assert!(artifacts.is_empty());
    }
}
