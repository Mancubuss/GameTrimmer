//! Ubisoft Connect library discovery: registry subkeys under
//! `HKLM\SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs\<id>`.
//!
//! Unlike Steam/Epic/GOG, Ubisoft's registry doesn't store a game name -
//! only an `InstallDir` value per subkey (the subkey name is Ubisoft's
//! internal numeric game id). The display name is derived from the last
//! path component of `InstallDir`.

use std::path::PathBuf;

use winreg::enums::HKEY_LOCAL_MACHINE;
use winreg::RegKey;

use crate::error::Result;

use super::{DiscoveredLibrary, GameInstall, LibraryProvider};

const REGISTRY_KEY: &str = r"SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs";

pub struct UbisoftProvider;

impl LibraryProvider for UbisoftProvider {
    fn name(&self) -> &'static str {
        "ubisoft"
    }

    fn discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let Ok(installs_key) = hklm.open_subkey(REGISTRY_KEY) else {
            // Ubisoft Connect not installed, or no games registered - not an error.
            return Ok(Vec::new());
        };

        let games: Vec<GameInstall> = installs_key
            .enum_keys()
            .flatten()
            .filter_map(|id| {
                let subkey = installs_key.open_subkey(&id).ok()?;
                let install_dir = subkey.get_value::<String, _>("InstallDir").ok();
                build_game_install(&id, install_dir)
            })
            .filter(|game| game.install_dir.is_dir())
            .collect();

        Ok(super::group_by_parent_dir("ubisoft", games))
    }
}

/// Builds a `GameInstall` from a raw registry entry: `id` is the subkey name
/// (Ubisoft's internal game id), `install_dir` is the `InstallDir` value if
/// present. Returns `None` when `install_dir` is missing/empty, or has no
/// final path component to use as a name (e.g. a bare drive root).
fn build_game_install(id: &str, install_dir: Option<String>) -> Option<GameInstall> {
    let install_dir = install_dir.filter(|s| !s.trim().is_empty())?;
    let path = PathBuf::from(install_dir);
    let name = path.file_name()?.to_string_lossy().into_owned();

    Some(GameInstall {
        name,
        install_dir: path,
        app_id: Some(id.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_game_install_names_game_after_install_dir_leaf() {
        let game = build_game_install(
            "5586",
            Some(r"F:\Ubisoft\Assassin's Creed Origins".to_string()),
        )
        .expect("expected a parsed game");

        assert_eq!(game.name, "Assassin's Creed Origins");
        assert_eq!(game.app_id.as_deref(), Some("5586"));
        assert_eq!(
            game.install_dir,
            PathBuf::from(r"F:\Ubisoft\Assassin's Creed Origins")
        );
    }

    #[test]
    fn build_game_install_returns_none_when_install_dir_missing() {
        assert!(build_game_install("5586", None).is_none());
    }

    #[test]
    fn build_game_install_returns_none_when_install_dir_empty() {
        assert!(build_game_install("5586", Some("".to_string())).is_none());
    }

    #[test]
    fn build_game_install_returns_none_for_bare_drive_root() {
        assert!(build_game_install("5586", Some(r"F:\".to_string())).is_none());
    }

    #[test]
    fn group_by_parent_dir_groups_synthetic_ubisoft_games_by_shared_parent() {
        let games = vec![
            build_game_install("1", Some(r"F:\Ubisoft\Alpha".to_string())).unwrap(),
            build_game_install("2", Some(r"F:\Ubisoft\Beta".to_string())).unwrap(),
        ];

        let libraries = super::super::group_by_parent_dir("ubisoft", games);

        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].vendor, "ubisoft");
        assert_eq!(libraries[0].path, PathBuf::from(r"F:\Ubisoft"));
        assert_eq!(libraries[0].games.len(), 2);
    }
}
