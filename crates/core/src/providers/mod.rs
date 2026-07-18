//! Discovery of game libraries across launcher vendors.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::error::Result;

pub mod amazon;
pub mod battlenet;
pub mod ea;
pub mod epic;
pub mod folderscan;
pub mod gog;
pub mod humble;
pub mod itch;
pub mod riot;
pub mod rockstar;
pub mod steam;
pub mod ubisoft;
pub mod xbox;

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
        Box::new(battlenet::BattleNetProvider),
        Box::new(rockstar::RockstarProvider),
        Box::new(amazon::AmazonProvider),
        Box::new(riot::RiotProvider),
        Box::new(itch::ItchProvider),
        Box::new(humble::HumbleProvider),
        Box::new(xbox::XboxProvider),
        // Last on purpose: the heuristic folder scan re-finds libraries the
        // metadata providers already know about; merge_libraries_by_path
        // keeps the earlier (metadata) entries with their richer names/ids.
        Box::new(folderscan::FolderScanProvider),
    ]
}

/// Merges libraries that share the same root path (case-insensitive, since
/// Windows paths are not case-sensitive): the first occurrence keeps its
/// vendor tag and game entries, later occurrences only contribute games with
/// install directories not seen yet. Without this, two providers discovering
/// the same folder would double-register it - and `persist_libraries` would
/// let the later one clobber the earlier one's games.
pub fn merge_libraries_by_path(libraries: Vec<DiscoveredLibrary>) -> Vec<DiscoveredLibrary> {
    let mut merged: Vec<DiscoveredLibrary> = Vec::new();

    for library in libraries {
        let key = library.path.to_string_lossy().to_lowercase();
        let existing = merged
            .iter_mut()
            .find(|m| m.path.to_string_lossy().to_lowercase() == key);

        match existing {
            Some(target) => {
                for game in library.games {
                    let already_known = target.games.iter().any(|known| {
                        known
                            .install_dir
                            .to_string_lossy()
                            .eq_ignore_ascii_case(&game.install_dir.to_string_lossy())
                    });
                    if !already_known {
                        target.games.push(game);
                    }
                }
            }
            None => merged.push(library),
        }
    }

    merged
}

/// De-duplicates games by install directory (case-insensitive, since Windows
/// paths are not case-sensitive), keeping the first occurrence. Used by
/// providers that read the same game from several sources (EA's two registry
/// generations, the three Windows uninstall registry roots, ...).
pub(crate) fn dedupe_by_install_dir(games: Vec<GameInstall>) -> Vec<GameInstall> {
    let mut seen = HashSet::new();
    games
        .into_iter()
        .filter(|game| seen.insert(game.install_dir.to_string_lossy().to_lowercase()))
        .collect()
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

    #[test]
    fn dedupe_by_install_dir_keeps_first_occurrence_case_insensitively() {
        let games = vec![
            GameInstall {
                name: "From Origin".to_string(),
                install_dir: PathBuf::from(r"F:\EA Games\Apex Legends"),
                app_id: Some("origin-id".to_string()),
            },
            GameInstall {
                name: "From EA Desktop".to_string(),
                install_dir: PathBuf::from(r"f:\ea games\apex legends"),
                app_id: Some("ea-desktop-id".to_string()),
            },
        ];

        let deduped = dedupe_by_install_dir(games);

        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].name, "From Origin");
    }

    #[test]
    fn merge_libraries_by_path_unions_games_of_same_root() {
        let from_metadata = DiscoveredLibrary {
            vendor: "epic",
            path: PathBuf::from(r"F:\Epic"),
            games: vec![game("Celeste (official name)", r"F:\Epic\Celeste")],
        };
        let from_folderscan = DiscoveredLibrary {
            vendor: "epic",
            path: PathBuf::from(r"f:\epic"),
            games: vec![
                game("Celeste", r"f:\epic\Celeste"),
                game("Inside", r"f:\epic\Inside"),
            ],
        };

        let merged = merge_libraries_by_path(vec![from_metadata, from_folderscan]);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].path, PathBuf::from(r"F:\Epic"));
        assert_eq!(merged[0].games.len(), 2);
        // The first (metadata) entry's richer name wins for the shared game.
        assert_eq!(merged[0].games[0].name, "Celeste (official name)");
        assert_eq!(merged[0].games[1].name, "Inside");
    }

    #[test]
    fn merge_libraries_by_path_keeps_distinct_roots_apart() {
        let merged = merge_libraries_by_path(vec![
            DiscoveredLibrary {
                vendor: "epic",
                path: PathBuf::from(r"F:\Epic"),
                games: vec![game("Alpha", r"F:\Epic\Alpha")],
            },
            DiscoveredLibrary {
                vendor: "gog",
                path: PathBuf::from(r"F:\GOG"),
                games: vec![game("Beta", r"F:\GOG\Beta")],
            },
        ]);

        assert_eq!(merged.len(), 2);
    }
}
