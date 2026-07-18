//! Riot Games library discovery: per-product settings files under
//! `%ProgramData%\Riot Games\Metadata\<product>.<region>\
//! <product>.<region>.product_settings.yaml`.
//!
//! Only the `product_install_full_path` line is needed, so the YAML is read
//! with a targeted line scan rather than a full YAML parser (no extra
//! dependency; the file is machine-written and flat).

use std::path::{Path, PathBuf};

use crate::error::Result;

use super::{DiscoveredLibrary, GameInstall, LibraryProvider};

const METADATA_RELATIVE_PATH: &str = r"Riot Games\Metadata";
const DEFAULT_PROGRAM_DATA: &str = r"C:\ProgramData";
const INSTALL_PATH_KEY: &str = "product_install_full_path:";

/// Metadata entries that are launcher infrastructure, not games.
const NON_GAME_SLUGS: &[&str] = &["riot_client"];

pub struct RiotProvider;

impl LibraryProvider for RiotProvider {
    fn name(&self) -> &'static str {
        "riot"
    }

    fn discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        let metadata_dir = program_data_dir().join(METADATA_RELATIVE_PATH);
        let Ok(entries) = std::fs::read_dir(&metadata_dir) else {
            // Riot Client not installed - not an error.
            return Ok(Vec::new());
        };

        let games: Vec<GameInstall> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .filter_map(|product_dir| read_product_game(&product_dir))
            .filter(|game| game.install_dir.is_dir())
            .collect();

        Ok(super::group_by_parent_dir("riot", games))
    }
}

fn program_data_dir() -> PathBuf {
    std::env::var("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_PROGRAM_DATA))
}

/// Reads one `Metadata\<product>.<region>` directory into a `GameInstall`,
/// if it describes an installed game.
fn read_product_game(product_dir: &Path) -> Option<GameInstall> {
    let dir_name = product_dir.file_name()?.to_string_lossy().into_owned();
    let slug = dir_name.split('.').next().unwrap_or(&dir_name);

    if NON_GAME_SLUGS
        .iter()
        .any(|excluded| excluded.eq_ignore_ascii_case(slug))
    {
        return None;
    }

    let settings_path = product_dir.join(format!("{dir_name}.product_settings.yaml"));
    let contents = std::fs::read_to_string(settings_path).ok()?;
    let install_path = extract_install_path(&contents)?;
    let path = PathBuf::from(install_path);

    Some(GameInstall {
        name: display_name_for(slug, &path),
        install_dir: path,
        app_id: Some(dir_name),
    })
}

/// Extracts `product_install_full_path` from the settings YAML: a flat
/// `key: "value"` line with forward slashes in the value.
fn extract_install_path(yaml: &str) -> Option<String> {
    let raw = yaml
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(INSTALL_PATH_KEY))?
        .trim()
        .trim_matches('"');

    let normalized = raw.replace('/', "\\");
    let trimmed = normalized.trim_end_matches('\\');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Maps Riot's internal product slugs to display names, falling back to the
/// install directory's last path component (which Riot names after the game).
fn display_name_for(slug: &str, install_dir: &Path) -> String {
    match slug.to_ascii_lowercase().as_str() {
        "valorant" => "VALORANT".to_string(),
        "league_of_legends" => "League of Legends".to_string(),
        "bacon" => "Legends of Runeterra".to_string(),
        _ => install_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| slug.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_install_path_reads_quoted_forward_slash_path() {
        let yaml = concat!(
            "channel: live\n",
            "product_install_full_path: \"F:/Riot Games/VALORANT/live\"\n",
            "product_install_root: \"F:/Riot Games\"\n",
        );

        assert_eq!(
            extract_install_path(yaml).as_deref(),
            Some(r"F:\Riot Games\VALORANT\live")
        );
    }

    #[test]
    fn extract_install_path_reads_unquoted_value() {
        let yaml = "product_install_full_path: C:/Riot Games/League of Legends\n";
        assert_eq!(
            extract_install_path(yaml).as_deref(),
            Some(r"C:\Riot Games\League of Legends")
        );
    }

    #[test]
    fn extract_install_path_returns_none_when_key_absent() {
        assert!(extract_install_path("channel: live\n").is_none());
    }

    #[test]
    fn extract_install_path_returns_none_for_empty_value() {
        assert!(extract_install_path("product_install_full_path: \"\"\n").is_none());
    }

    #[test]
    fn display_name_for_maps_known_slugs() {
        let dir = PathBuf::from(r"F:\Riot Games\VALORANT\live");
        assert_eq!(display_name_for("valorant", &dir), "VALORANT");
        assert_eq!(
            display_name_for("league_of_legends", &dir),
            "League of Legends"
        );
        assert_eq!(display_name_for("bacon", &dir), "Legends of Runeterra");
    }

    #[test]
    fn display_name_for_falls_back_to_install_dir_leaf() {
        let dir = PathBuf::from(r"F:\Riot Games\2XKO");
        assert_eq!(display_name_for("2xko", &dir), "2XKO");
    }
}
