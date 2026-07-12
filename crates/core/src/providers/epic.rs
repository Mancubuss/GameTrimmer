//! Epic Games Store library discovery: registry -> `Manifests\*.item` (JSON).

use std::path::{Path, PathBuf};

use serde::Deserialize;
use winreg::enums::HKEY_LOCAL_MACHINE;
use winreg::RegKey;

use crate::error::Result;

use super::{DiscoveredLibrary, GameInstall, LibraryProvider};

const REGISTRY_KEY: &str = r"SOFTWARE\WOW6432Node\Epic Games\EpicGamesLauncher";
const REGISTRY_VALUE: &str = "AppDataPath";
const DEFAULT_DATA_DIR: &str = r"C:\ProgramData\Epic\EpicGamesLauncher\Data";

pub struct EpicProvider;

impl LibraryProvider for EpicProvider {
    fn name(&self) -> &'static str {
        "epic"
    }

    fn discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        let manifests_dir = find_manifests_dir();
        let Ok(entries) = std::fs::read_dir(&manifests_dir) else {
            // Epic not installed (or manifests folder absent) - not an error.
            return Ok(Vec::new());
        };

        let games: Vec<GameInstall> = entries
            .flatten()
            .filter(|entry| is_item_manifest(&entry.path()))
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .filter_map(|contents| parse_item(&contents))
            .filter(|game| game.install_dir.is_dir())
            .collect();

        Ok(super::group_by_parent_dir("epic", games))
    }
}

/// Locates the `Manifests` directory holding `*.item` files, preferring the
/// `AppDataPath` reported by `HKLM\...\EpicGamesLauncher`, falling back to the
/// well-known default `ProgramData` location.
fn find_manifests_dir() -> PathBuf {
    let base = read_app_data_path()
        .map(|raw| PathBuf::from(normalize_slashes(&raw)))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR));
    base.join("Manifests")
}

fn read_app_data_path() -> Option<String> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let key = hklm.open_subkey(REGISTRY_KEY).ok()?;
    key.get_value::<String, _>(REGISTRY_VALUE).ok()
}

fn normalize_slashes(raw: &str) -> String {
    raw.replace('/', "\\")
}

fn is_item_manifest(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("item"))
}

/// The subset of an Epic `.item` manifest's fields we care about. Real
/// manifests carry many more fields (CatalogItemId, ManifestLocation,
/// InstallSize, LaunchExecutable, ...) which are ignored here.
#[derive(Debug, Deserialize)]
struct EpicItemManifest {
    #[serde(rename = "DisplayName")]
    display_name: Option<String>,
    #[serde(rename = "InstallLocation")]
    install_location: Option<String>,
    #[serde(rename = "AppName")]
    app_name: Option<String>,
}

/// Parses the JSON text of one `Manifests\<id>.item` file. Returns `None` for
/// malformed JSON or a manifest missing a usable `DisplayName`/`InstallLocation`.
pub fn parse_item(json: &str) -> Option<GameInstall> {
    let manifest: EpicItemManifest = serde_json::from_str(json).ok()?;

    let name = manifest.display_name.filter(|s| !s.trim().is_empty())?;
    let install_location = manifest.install_location.filter(|s| !s.trim().is_empty())?;

    Some(GameInstall {
        name,
        install_dir: PathBuf::from(install_location),
        app_id: manifest.app_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_item_reads_real_world_manifest() {
        let json = r#"
{
    "FormatVersion": 0,
    "bIsIncompleteInstall": false,
    "AppVersionString": "1.0.0",
    "CatalogNamespace": "5a2b...",
    "CatalogItemId": "5cb97847cee34581ab9576419a91d9f3",
    "AppName": "Fortnite",
    "AppCategories": ["games", "applications"],
    "DisplayName": "Fortnite",
    "InstallationGuid": "a1b2c3d4e5f6",
    "InstallLocation": "F:\\Epic\\Fortnite",
    "InstallSize": 26843545600,
    "LaunchExecutable": "FortniteLauncher.exe",
    "ManifestLocation": "C:\\ProgramData\\Epic\\EpicGamesLauncher\\Data\\Manifests",
    "bIsApplication": true,
    "bIsExecutable": true,
    "bIsManaged": false
}
"#;

        let game = parse_item(json).expect("expected a parsed game");

        assert_eq!(game.name, "Fortnite");
        assert_eq!(game.app_id.as_deref(), Some("Fortnite"));
        assert_eq!(game.install_dir, PathBuf::from(r"F:\Epic\Fortnite"));
    }

    #[test]
    fn parse_item_returns_none_when_display_name_missing() {
        let json = r#"{ "InstallLocation": "F:\\Epic\\Game", "AppName": "Game" }"#;
        assert!(parse_item(json).is_none());
    }

    #[test]
    fn parse_item_returns_none_when_install_location_missing() {
        let json = r#"{ "DisplayName": "Game", "AppName": "Game" }"#;
        assert!(parse_item(json).is_none());
    }

    #[test]
    fn parse_item_ignores_empty_display_name_and_install_location() {
        let json = r#"{ "DisplayName": "", "InstallLocation": "", "AppName": "Game" }"#;
        assert!(parse_item(json).is_none());
    }

    #[test]
    fn parse_item_returns_none_on_garbage_input() {
        assert!(parse_item("not json at all").is_none());
        assert!(parse_item("").is_none());
    }

    #[test]
    fn parse_item_allows_missing_app_name() {
        let json = r#"{ "DisplayName": "Game", "InstallLocation": "F:\\Epic\\Game" }"#;
        let game = parse_item(json).expect("expected a parsed game");
        assert_eq!(game.app_id, None);
    }

    #[test]
    fn is_item_manifest_matches_dot_item_case_insensitively() {
        assert!(is_item_manifest(Path::new("Fortnite.item")));
        assert!(is_item_manifest(Path::new("Fortnite.ITEM")));
        assert!(!is_item_manifest(Path::new("Fortnite.json")));
        assert!(!is_item_manifest(Path::new("Fortnite")));
    }
}
