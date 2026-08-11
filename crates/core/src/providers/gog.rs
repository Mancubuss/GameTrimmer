//! GOG Galaxy library discovery: registry subkeys under
//! `HKLM\SOFTWARE\WOW6432Node\GOG.com\Games\<id>`.

use std::path::PathBuf;

use winreg::enums::HKEY_LOCAL_MACHINE;
use winreg::RegKey;

use crate::error::Result;

use super::{
    DiscoveredLibrary, DiscoveryDiagnostic, DiscoveryReport, GameInstall, LibraryProvider,
    OrphanEvidence,
};

const REGISTRY_KEY: &str = r"SOFTWARE\WOW6432Node\GOG.com\Games";

pub struct GogProvider;

impl LibraryProvider for GogProvider {
    fn name(&self) -> &'static str {
        "gog"
    }

    fn try_discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        Ok(discover_gog().data)
    }

    fn discover(&self) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
        discover_gog()
    }
}

fn diagnostic(
    stage: &'static str,
    path: Option<PathBuf>,
    message: impl std::fmt::Display,
) -> DiscoveryDiagnostic {
    DiscoveryDiagnostic {
        provider: "gog",
        stage,
        path,
        message: message.to_string(),
    }
}

fn discover_gog() -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let games_key = match hklm.open_subkey(REGISTRY_KEY) {
        Ok(key) => key,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return DiscoveryReport::not_installed(Vec::new())
        }
        Err(err) => {
            return DiscoveryReport::failed(Vec::new(), diagnostic("registry-open", None, err))
        }
    };

    let mut games = Vec::new();
    let mut diagnostics = Vec::new();
    for app_id in games_key.enum_keys() {
        let app_id = match app_id {
            Ok(app_id) => app_id,
            Err(err) => {
                diagnostics.push(diagnostic("registry-enumeration", None, err));
                continue;
            }
        };
        let subkey = match games_key.open_subkey(&app_id) {
            Ok(subkey) => subkey,
            Err(err) => {
                diagnostics.push(diagnostic("game-key-open", None, err));
                continue;
            }
        };
        let entry = RawGogEntry {
            app_id: app_id.clone(),
            game_name: subkey.get_value::<String, _>("gameName").ok(),
            path: subkey.get_value::<String, _>("path").ok(),
        };
        let Some(game) = build_game_install(entry) else {
            diagnostics.push(diagnostic(
                "game-entry",
                None,
                format!("GOG registry entry {app_id} has no usable name or path"),
            ));
            continue;
        };
        if game.install_dir.is_dir() {
            games.push(game);
        } else {
            diagnostics.push(diagnostic(
                "game-path",
                Some(game.install_dir),
                "configured GOG install is unavailable",
            ));
        }
    }

    let mut libraries = super::group_by_parent_dir("gog", games);
    if diagnostics.is_empty() {
        DiscoveryReport::complete(libraries)
    } else {
        for library in &mut libraries {
            library.orphan_evidence = OrphanEvidence::Degraded;
        }
        DiscoveryReport::degraded(libraries, diagnostics)
    }
}

/// One raw entry read from a `HKLM\...\GOG.com\Games\<id>` subkey (or a
/// synthetic stand-in in tests), sufficient to attempt building a `GameInstall`.
struct RawGogEntry {
    app_id: String,
    game_name: Option<String>,
    path: Option<String>,
}

/// Builds a `GameInstall` from a raw registry entry. Returns `None` when
/// either `gameName` or `path` is missing/empty - i.e. a broken/partial entry.
fn build_game_install(entry: RawGogEntry) -> Option<GameInstall> {
    let name = entry.game_name.filter(|s| !s.trim().is_empty())?;
    let path = entry.path.filter(|s| !s.trim().is_empty())?;

    Some(GameInstall {
        name,
        install_dir: PathBuf::from(path),
        app_id: Some(entry.app_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_game_install_reads_name_and_path() {
        let entry = RawGogEntry {
            app_id: "1207658930".to_string(),
            game_name: Some("The Witcher 3: Wild Hunt".to_string()),
            path: Some(r"F:\GOG Games\The Witcher 3".to_string()),
        };

        let game = build_game_install(entry).expect("expected a parsed game");

        assert_eq!(game.name, "The Witcher 3: Wild Hunt");
        assert_eq!(game.app_id.as_deref(), Some("1207658930"));
        assert_eq!(
            game.install_dir,
            PathBuf::from(r"F:\GOG Games\The Witcher 3")
        );
    }

    #[test]
    fn build_game_install_returns_none_when_name_missing() {
        let entry = RawGogEntry {
            app_id: "123".to_string(),
            game_name: None,
            path: Some(r"F:\GOG Games\Broken".to_string()),
        };
        assert!(build_game_install(entry).is_none());
    }

    #[test]
    fn build_game_install_returns_none_when_path_missing() {
        let entry = RawGogEntry {
            app_id: "123".to_string(),
            game_name: Some("Broken".to_string()),
            path: None,
        };
        assert!(build_game_install(entry).is_none());
    }

    #[test]
    fn build_game_install_ignores_empty_name_and_path() {
        let entry = RawGogEntry {
            app_id: "123".to_string(),
            game_name: Some("".to_string()),
            path: Some("".to_string()),
        };
        assert!(build_game_install(entry).is_none());
    }

    #[test]
    fn group_by_parent_dir_groups_synthetic_gog_games_by_shared_parent() {
        let games = vec![
            build_game_install(RawGogEntry {
                app_id: "1".to_string(),
                game_name: Some("Alpha".to_string()),
                path: Some(r"F:\GOG Games\Alpha".to_string()),
            })
            .unwrap(),
            build_game_install(RawGogEntry {
                app_id: "2".to_string(),
                game_name: Some("Beta".to_string()),
                path: Some(r"F:\GOG Games\Beta".to_string()),
            })
            .unwrap(),
        ];

        let libraries = super::super::group_by_parent_dir("gog", games);

        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].vendor, "gog");
        assert_eq!(libraries[0].path, PathBuf::from(r"F:\GOG Games"));
        assert_eq!(libraries[0].games.len(), 2);
    }
}
