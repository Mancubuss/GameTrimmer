//! Battle.net (Blizzard) library discovery via the Windows uninstall registry.
//!
//! Battle.net's own install database (`C:\ProgramData\Battle.net\Agent\
//! product.db`) is a protobuf blob without a published schema, but the client
//! reliably registers every installed game under the standard uninstall keys
//! with `Publisher = "Blizzard Entertainment"`, `DisplayName` and
//! `InstallLocation` - so those are read instead. All three uninstall roots
//! (HKLM 64-bit, HKLM 32-bit, HKCU) are scanned and de-duplicated by
//! install directory.

use std::path::PathBuf;

use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winreg::RegKey;

use crate::error::Result;

use super::{DiscoveredLibrary, GameInstall, LibraryProvider};

const PUBLISHER: &str = "Blizzard Entertainment";

/// Uninstall entries that belong to launcher infrastructure, not games.
const NON_GAME_NAMES: &[&str] = &["Battle.net"];

pub struct BattleNetProvider;

impl LibraryProvider for BattleNetProvider {
    fn name(&self) -> &'static str {
        "battlenet"
    }

    fn discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        let games: Vec<GameInstall> = uninstall_roots()
            .into_iter()
            .flat_map(|(root, path)| read_uninstall_games(&root, path))
            .collect();

        let games = super::dedupe_by_install_dir(games)
            .into_iter()
            .filter(|game| game.install_dir.is_dir())
            .collect();

        Ok(super::group_by_parent_dir("battlenet", games))
    }
}

/// The three registry roots where Windows uninstall entries live.
fn uninstall_roots() -> Vec<(RegKey, &'static str)> {
    vec![
        (
            RegKey::predef(HKEY_LOCAL_MACHINE),
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
        (
            RegKey::predef(HKEY_LOCAL_MACHINE),
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
        (
            RegKey::predef(HKEY_CURRENT_USER),
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
    ]
}

/// Reads every uninstall subkey under one root and keeps the Blizzard games.
/// A missing root (possible for HKCU) is simply an empty result.
fn read_uninstall_games(root: &RegKey, path: &str) -> Vec<GameInstall> {
    let Ok(uninstall_key) = root.open_subkey(path) else {
        return Vec::new();
    };

    uninstall_key
        .enum_keys()
        .flatten()
        .filter_map(|key_name| {
            let subkey = uninstall_key.open_subkey(&key_name).ok()?;
            let entry = RawUninstallEntry {
                key_name,
                display_name: subkey.get_value::<String, _>("DisplayName").ok(),
                publisher: subkey.get_value::<String, _>("Publisher").ok(),
                install_location: subkey.get_value::<String, _>("InstallLocation").ok(),
            };
            build_game_install(entry)
        })
        .collect()
}

/// One raw uninstall registry entry (or a synthetic stand-in in tests).
struct RawUninstallEntry {
    key_name: String,
    display_name: Option<String>,
    publisher: Option<String>,
    install_location: Option<String>,
}

/// Builds a `GameInstall` from a raw uninstall entry. Returns `None` unless
/// the publisher is exactly Blizzard, an install location is present, and the
/// entry is not the Battle.net client itself.
fn build_game_install(entry: RawUninstallEntry) -> Option<GameInstall> {
    let publisher = entry.publisher?;
    if publisher.trim() != PUBLISHER {
        return None;
    }

    let install_location = entry.install_location.filter(|s| !s.trim().is_empty())?;
    let path = PathBuf::from(install_location.trim_end_matches(['\\', '/']));

    let name = entry
        .display_name
        .filter(|s| !s.trim().is_empty())
        .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))?;

    if NON_GAME_NAMES
        .iter()
        .any(|excluded| excluded.eq_ignore_ascii_case(name.trim()))
    {
        return None;
    }

    Some(GameInstall {
        name,
        install_dir: path,
        app_id: Some(entry.key_name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        key_name: &str,
        display_name: Option<&str>,
        publisher: Option<&str>,
        install_location: Option<&str>,
    ) -> RawUninstallEntry {
        RawUninstallEntry {
            key_name: key_name.to_string(),
            display_name: display_name.map(str::to_string),
            publisher: publisher.map(str::to_string),
            install_location: install_location.map(str::to_string),
        }
    }

    #[test]
    fn build_game_install_reads_blizzard_game() {
        let game = build_game_install(entry(
            "Overwatch",
            Some("Overwatch"),
            Some("Blizzard Entertainment"),
            Some(r"F:\Battle.net\Overwatch"),
        ))
        .expect("expected a parsed game");

        assert_eq!(game.name, "Overwatch");
        assert_eq!(game.app_id.as_deref(), Some("Overwatch"));
        assert_eq!(game.install_dir, PathBuf::from(r"F:\Battle.net\Overwatch"));
    }

    #[test]
    fn build_game_install_trims_trailing_slash_from_install_location() {
        let game = build_game_install(entry(
            "Diablo IV",
            Some("Diablo IV"),
            Some("Blizzard Entertainment"),
            Some(r"F:\Battle.net\Diablo IV\"),
        ))
        .expect("expected a parsed game");

        assert_eq!(game.install_dir, PathBuf::from(r"F:\Battle.net\Diablo IV"));
    }

    #[test]
    fn build_game_install_rejects_other_publishers() {
        assert!(build_game_install(entry(
            "SomeTool",
            Some("Some Tool"),
            Some("Contoso"),
            Some(r"C:\Program Files\SomeTool"),
        ))
        .is_none());
    }

    #[test]
    fn build_game_install_rejects_missing_publisher() {
        assert!(build_game_install(entry(
            "SomeTool",
            Some("Some Tool"),
            None,
            Some(r"C:\Program Files\SomeTool"),
        ))
        .is_none());
    }

    #[test]
    fn build_game_install_rejects_battlenet_client_itself() {
        assert!(build_game_install(entry(
            "Battle.net",
            Some("Battle.net"),
            Some("Blizzard Entertainment"),
            Some(r"C:\Program Files (x86)\Battle.net"),
        ))
        .is_none());
    }

    #[test]
    fn build_game_install_rejects_missing_install_location() {
        assert!(build_game_install(entry(
            "Overwatch",
            Some("Overwatch"),
            Some("Blizzard Entertainment"),
            None,
        ))
        .is_none());
    }

    #[test]
    fn build_game_install_falls_back_to_folder_name_when_display_name_missing() {
        let game = build_game_install(entry(
            "wow",
            None,
            Some("Blizzard Entertainment"),
            Some(r"F:\Battle.net\World of Warcraft"),
        ))
        .expect("expected a parsed game");

        assert_eq!(game.name, "World of Warcraft");
    }
}
