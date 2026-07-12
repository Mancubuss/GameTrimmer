//! GOG Galaxy library discovery: registry subkeys under
//! `HKLM\SOFTWARE\WOW6432Node\GOG.com\Games\<id>`.

use std::path::PathBuf;

use winreg::enums::HKEY_LOCAL_MACHINE;
use winreg::RegKey;

use crate::error::Result;

use super::{DiscoveredLibrary, GameInstall, LibraryProvider};

const REGISTRY_KEY: &str = r"SOFTWARE\WOW6432Node\GOG.com\Games";

pub struct GogProvider;

impl LibraryProvider for GogProvider {
    fn name(&self) -> &'static str {
        "gog"
    }

    fn discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let Ok(games_key) = hklm.open_subkey(REGISTRY_KEY) else {
            // GOG Galaxy not installed, or no games registered - not an error.
            return Ok(Vec::new());
        };

        let games: Vec<GameInstall> = games_key
            .enum_keys()
            .flatten()
            .filter_map(|app_id| {
                let subkey = games_key.open_subkey(&app_id).ok()?;
                let entry = RawGogEntry {
                    app_id,
                    game_name: subkey.get_value::<String, _>("gameName").ok(),
                    path: subkey.get_value::<String, _>("path").ok(),
                };
                build_game_install(entry)
            })
            .filter(|game| game.install_dir.is_dir())
            .collect();

        Ok(super::group_by_parent_dir("gog", games))
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
