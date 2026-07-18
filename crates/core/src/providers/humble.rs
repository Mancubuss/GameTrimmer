//! Humble App library discovery: `%APPDATA%\Humble App\config.json`,
//! array `game-collection-4` with per-game `status` / `filePath` /
//! `gameName` / `machineName` fields.

use std::path::PathBuf;

use serde::Deserialize;

use crate::error::Result;

use super::{DiscoveredLibrary, GameInstall, LibraryProvider};

const CONFIG_RELATIVE_PATH: &str = r"Humble App\config.json";

/// Statuses the Humble App uses for games present on disk.
const INSTALLED_STATUSES: &[&str] = &["installed", "downloaded"];

pub struct HumbleProvider;

impl LibraryProvider for HumbleProvider {
    fn name(&self) -> &'static str {
        "humble"
    }

    fn discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        let Some(config_path) = config_path().filter(|path| path.is_file()) else {
            // Humble App not installed - not an error.
            return Ok(Vec::new());
        };

        let contents = std::fs::read_to_string(config_path)?;
        let games = parse_config(&contents)
            .into_iter()
            .filter(|game| game.install_dir.is_dir())
            .collect();

        Ok(super::group_by_parent_dir("humble", games))
    }
}

fn config_path() -> Option<PathBuf> {
    let app_data = std::env::var("APPDATA").ok()?;
    Some(PathBuf::from(app_data).join(CONFIG_RELATIVE_PATH))
}

/// The subset of the Humble App config we care about.
#[derive(Debug, Deserialize)]
struct HumbleConfig {
    #[serde(rename = "game-collection-4", default)]
    game_collection: Vec<HumbleGame>,
}

#[derive(Debug, Deserialize)]
struct HumbleGame {
    status: Option<String>,
    #[serde(rename = "gameName")]
    game_name: Option<String>,
    #[serde(rename = "machineName")]
    machine_name: Option<String>,
    #[serde(rename = "filePath")]
    file_path: Option<String>,
}

/// Parses the config JSON into the installed games it describes. Malformed
/// JSON yields an empty list - the config is launcher-owned state, not user
/// input worth surfacing an error for.
fn parse_config(json: &str) -> Vec<GameInstall> {
    let Ok(config) = serde_json::from_str::<HumbleConfig>(json) else {
        return Vec::new();
    };

    config
        .game_collection
        .into_iter()
        .filter_map(build_game_install)
        .collect()
}

/// Builds a `GameInstall` from one collection entry. Requires an
/// installed/downloaded status and a `filePath`; the name falls back from
/// `gameName` to `machineName` to the folder's last path component.
fn build_game_install(game: HumbleGame) -> Option<GameInstall> {
    let status = game.status?;
    if !INSTALLED_STATUSES
        .iter()
        .any(|installed| installed.eq_ignore_ascii_case(status.trim()))
    {
        return None;
    }

    let file_path = game.file_path.filter(|s| !s.trim().is_empty())?;
    let path = PathBuf::from(file_path);

    let name = game
        .game_name
        .filter(|s| !s.trim().is_empty())
        .or_else(|| game.machine_name.clone().filter(|s| !s.trim().is_empty()))
        .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))?;

    Some(GameInstall {
        name,
        install_dir: path,
        app_id: game.machine_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_reads_installed_games() {
        let json = r#"
{
    "settings": { "downloadLocation": "F:\\Humble" },
    "game-collection-4": [
        {
            "status": "installed",
            "gameName": "FTL: Faster Than Light",
            "machineName": "ftl_game",
            "filePath": "F:\\Humble\\FTL"
        },
        {
            "status": "available",
            "gameName": "Not Installed",
            "machineName": "not_installed",
            "filePath": ""
        }
    ]
}
"#;

        let games = parse_config(json);

        assert_eq!(games.len(), 1);
        assert_eq!(games[0].name, "FTL: Faster Than Light");
        assert_eq!(games[0].app_id.as_deref(), Some("ftl_game"));
        assert_eq!(games[0].install_dir, PathBuf::from(r"F:\Humble\FTL"));
    }

    #[test]
    fn parse_config_accepts_downloaded_status() {
        let json = r#"
{
    "game-collection-4": [
        { "status": "downloaded", "gameName": "Game", "filePath": "F:\\Humble\\Game" }
    ]
}
"#;
        assert_eq!(parse_config(json).len(), 1);
    }

    #[test]
    fn parse_config_returns_empty_for_garbage_input() {
        assert!(parse_config("not json").is_empty());
        assert!(parse_config("{}").is_empty());
    }

    #[test]
    fn build_game_install_falls_back_to_machine_name() {
        let game = build_game_install(HumbleGame {
            status: Some("installed".to_string()),
            game_name: None,
            machine_name: Some("ftl_game".to_string()),
            file_path: Some(r"F:\Humble\FTL".to_string()),
        })
        .expect("expected a parsed game");

        assert_eq!(game.name, "ftl_game");
    }

    #[test]
    fn build_game_install_requires_file_path() {
        assert!(build_game_install(HumbleGame {
            status: Some("installed".to_string()),
            game_name: Some("Game".to_string()),
            machine_name: None,
            file_path: None,
        })
        .is_none());
    }
}
