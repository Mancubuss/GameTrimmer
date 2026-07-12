//! Discovery of game libraries across launcher vendors.

use std::path::PathBuf;

use crate::error::Result;

pub mod ea;
pub mod epic;
pub mod gog;
pub mod steam;
pub mod ubisoft;

/// One installed game inside a library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameInstall {
    /// Display name, e.g. "DOOM The Dark Ages".
    pub name: String,
    /// Absolute path to the game's install directory.
    pub install_dir: PathBuf,
    /// Vendor-specific id (Steam appid etc.), if known.
    pub app_id: Option<String>,
}

/// A discovered game library (one root folder of one vendor).
#[derive(Debug, Clone)]
pub struct DiscoveredLibrary {
    /// Vendor tag stored in the DB, e.g. "steam".
    pub vendor: &'static str,
    /// Absolute path to the library root, e.g. `F:\SteamLibrary`.
    pub path: PathBuf,
    pub games: Vec<GameInstall>,
}

/// A source of game libraries (Steam, Epic, GOG, ...).
pub trait LibraryProvider {
    fn name(&self) -> &'static str;
    /// Discover all libraries of this vendor present on the machine.
    fn discover(&self) -> Result<Vec<DiscoveredLibrary>>;
}

/// All built-in providers, in discovery order. A provider whose launcher is
/// not installed returns an empty list from `discover()` — that is not an error.
pub fn all() -> Vec<Box<dyn LibraryProvider>> {
    vec![
        Box::new(steam::SteamProvider),
        Box::new(epic::EpicProvider),
        Box::new(gog::GogProvider),
        Box::new(ubisoft::UbisoftProvider),
        Box::new(ea::EaProvider),
    ]
}

/// Groups already-filtered games into libraries by the parent directory of
/// each game's install directory. Every unique parent directory (case-insensitive
/// comparison, since Windows paths are not case-sensitive) becomes one
/// `DiscoveredLibrary` of `vendor`, in first-seen order. Games whose install
/// directory has no parent (e.g. a bare drive root) are dropped defensively -
/// that shape never occurs for a real game install.
pub(crate) fn group_by_parent_dir(
    vendor: &'static str,
    games: Vec<GameInstall>,
) -> Vec<DiscoveredLibrary> {
    let mut libraries: Vec<DiscoveredLibrary> = Vec::new();

    for game in games {
        let Some(parent) = game.install_dir.parent() else {
            continue;
        };
        let parent = parent.to_path_buf();

        let existing = libraries.iter_mut().find(|library| {
            library.path.to_string_lossy().to_lowercase() == parent.to_string_lossy().to_lowercase()
        });

        match existing {
            Some(library) => library.games.push(game),
            None => libraries.push(DiscoveredLibrary {
                vendor,
                path: parent,
                games: vec![game],
            }),
        }
    }

    libraries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(name: &str, install_dir: &str) -> GameInstall {
        GameInstall {
            name: name.to_string(),
            install_dir: PathBuf::from(install_dir),
            app_id: None,
        }
    }

    #[test]
    fn group_by_parent_dir_groups_games_under_shared_parent() {
        let games = vec![
            game("Alpha", r"F:\Epic\Alpha"),
            game("Beta", r"F:\Epic\Beta"),
        ];

        let libraries = group_by_parent_dir("epic", games);

        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].vendor, "epic");
        assert_eq!(libraries[0].path, PathBuf::from(r"F:\Epic"));
        assert_eq!(libraries[0].games.len(), 2);
    }

    #[test]
    fn group_by_parent_dir_splits_games_under_different_parents() {
        let games = vec![
            game("Alpha", r"F:\Epic\Alpha"),
            game("Gamma", r"G:\Games\Epic\Gamma"),
        ];

        let libraries = group_by_parent_dir("epic", games);

        assert_eq!(libraries.len(), 2);
        assert_eq!(libraries[0].path, PathBuf::from(r"F:\Epic"));
        assert_eq!(libraries[1].path, PathBuf::from(r"G:\Games\Epic"));
    }

    #[test]
    fn group_by_parent_dir_is_case_insensitive_on_parent_path() {
        let games = vec![
            game("Alpha", r"F:\Epic\Alpha"),
            game("Beta", r"f:\epic\Beta"),
        ];

        let libraries = group_by_parent_dir("epic", games);

        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].games.len(), 2);
    }

    #[test]
    fn group_by_parent_dir_returns_empty_for_no_games() {
        assert!(group_by_parent_dir("epic", Vec::new()).is_empty());
    }
}
