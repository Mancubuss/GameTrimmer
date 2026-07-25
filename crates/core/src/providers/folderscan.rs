//! Fallback discovery of orphaned vendor libraries by well-known folder names.
//!
//! The metadata-based providers (registry, manifests, launcher databases) see
//! only what the launcher itself currently knows about. After a Windows or
//! launcher reinstall that state is reset while the games survive on data
//! drives - exactly the situation GameTrimmer exists for. This provider walks
//! every drive root (and common "Games" containers) looking for well-known
//! vendor folder names ("Epic", "GOG", "Blizzard", ...) and treats their
//! subfolders as games.
//!
//! For GOG the per-game `goggame-<id>.info` manifest (shipped inside every
//! GOG install, DRM-free) restores the real display name and product id.
//! Other vendors fall back to folder names.
//!
//! Registered after the metadata providers; `merge_libraries_by_path` in the
//! scan pipeline de-duplicates libraries and games discovered by both, and
//! the metadata entries win (richer names/ids).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::Result;

use super::{holds_installed_files, DiscoveredLibrary, GameInstall, LibraryProvider};

/// Vendor tag -> folder names (relative to a container directory) that hold
/// that vendor's games.
const VENDOR_ROOTS: &[(&str, &[&str])] = &[
    ("epic", &["Epic", "Epic Games"]),
    ("gog", &["GOG", "GOG Games"]),
    ("ubisoft", &["Ubisoft", "Ubisoft Games"]),
    ("ea", &["EA Games", "Origin Games"]),
    ("battlenet", &["Blizzard", "Battle.net", "Blizzard Games"]),
    ("rockstar", &["Rockstar Games"]),
    ("riot", &["Riot Games"]),
    ("amazon", &[r"Amazon Games\Library"]),
];

/// Subfolder names that are launcher infrastructure rather than games.
const INFRASTRUCTURE_DIRS: &[&str] = &[
    "Ubisoft Game Launcher",
    "Epic Games Launcher",
    "Battle.net",
    "GOG Galaxy",
    "Origin",
    "EA app",
    "EA Desktop",
    "Riot Client",
    "Launcher",
    "Social Club",
];

/// Container folders under a drive root that commonly hold vendor libraries.
const GAMES_CONTAINERS: &[&str] = &["Games", "Ігри", "Игры"];

pub struct FolderScanProvider;

impl LibraryProvider for FolderScanProvider {
    fn name(&self) -> &'static str {
        "folderscan"
    }

    fn discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        let mut libraries = Vec::new();

        for drive in drive_roots() {
            if !drive.is_dir() {
                continue;
            }
            libraries.extend(scan_container(&drive));
            for container in GAMES_CONTAINERS {
                libraries.extend(scan_container(&drive.join(container)));
            }
        }

        Ok(libraries)
    }
}

/// All possible drive roots (`A:\` .. `Z:\`); nonexistent drives fail the
/// `is_dir()` check in `discover` and are skipped.
fn drive_roots() -> impl Iterator<Item = PathBuf> {
    (b'A'..=b'Z').map(|letter| PathBuf::from(format!(r"{}:\", letter as char)))
}

/// Scans one container directory for vendor-named library folders.
fn scan_container(container: &Path) -> Vec<DiscoveredLibrary> {
    VENDOR_ROOTS
        .iter()
        .flat_map(|(vendor, names)| {
            names
                .iter()
                .map(move |name| (*vendor, container.join(name)))
        })
        .filter(|(_, root)| root.is_dir())
        .filter_map(|(vendor, root)| read_vendor_library(vendor, &root))
        .collect()
}

/// Builds one library from a vendor root folder: every non-infrastructure
/// subfolder that actually holds files is a game. Returns `None` when no games
/// remain - an empty vendor folder is not worth registering as a library.
///
/// The `holds_installed_files` filter is GT-29: without it a contentless
/// subfolder became a phantom game, counted in the totals of a tool that
/// deletes files. Unlike the metadata providers, folder-name discovery has no
/// launcher to ask whether a game is installed - the files on disk are the
/// only evidence there is.
fn read_vendor_library(vendor: &'static str, root: &Path) -> Option<DiscoveredLibrary> {
    let entries = std::fs::read_dir(root).ok()?;

    let games: Vec<GameInstall> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && !is_infrastructure_dir(path))
        .filter(|dir| holds_installed_files(dir))
        .filter_map(|dir| build_game(vendor, dir))
        .collect();

    (!games.is_empty()).then(|| DiscoveredLibrary {
        vendor,
        path: root.to_path_buf(),
        games,
    })
}

/// Hidden/system folders and launcher clients are not games.
fn is_infrastructure_dir(dir: &Path) -> bool {
    let Some(name) = dir.file_name().map(|n| n.to_string_lossy()) else {
        return true;
    };

    name.starts_with('.')
        || name.starts_with('$')
        || INFRASTRUCTURE_DIRS
            .iter()
            .any(|infra| infra.eq_ignore_ascii_case(&name))
}

/// Builds a `GameInstall` for one game subfolder. GOG folders get their real
/// name/id from the `goggame-*.info` manifest when present; everything else
/// (and GOG folders without a manifest) uses the folder name.
fn build_game(vendor: &str, install_dir: PathBuf) -> Option<GameInstall> {
    let folder_name = install_dir.file_name()?.to_string_lossy().into_owned();

    let (name, app_id) = match vendor {
        "gog" => read_gog_info(&install_dir).unwrap_or((folder_name, None)),
        _ => (folder_name, None),
    };

    Some(GameInstall {
        name,
        install_dir,
        app_id,
    })
}

/// Reads the base game's `goggame-<id>.info` manifest from a GOG install
/// directory. A directory can hold several manifests (base game + DLCs);
/// the one whose `gameId` equals `rootGameId` is the base game.
fn read_gog_info(dir: &Path) -> Option<(String, Option<String>)> {
    let entries = std::fs::read_dir(dir).ok()?;

    let parsed: Vec<GogInfo> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_gog_info_file(path))
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|contents| serde_json::from_str::<GogInfo>(&contents).ok())
        .collect();

    parsed
        .iter()
        .find(|info| info.is_base_game())
        .or_else(|| parsed.first())
        .and_then(GogInfo::name_and_id)
}

fn is_gog_info_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            let lower = name.to_ascii_lowercase();
            lower.starts_with("goggame-") && lower.ends_with(".info")
        })
}

/// The subset of a `goggame-<id>.info` manifest we care about.
#[derive(Debug, Deserialize)]
struct GogInfo {
    name: Option<String>,
    #[serde(rename = "gameId")]
    game_id: Option<String>,
    #[serde(rename = "rootGameId")]
    root_game_id: Option<String>,
}

impl GogInfo {
    /// A manifest describes the base game (not a DLC) when its `gameId`
    /// matches `rootGameId`, or when no `rootGameId` is present at all.
    fn is_base_game(&self) -> bool {
        match (&self.game_id, &self.root_game_id) {
            (Some(id), Some(root)) => id == root,
            _ => true,
        }
    }

    fn name_and_id(&self) -> Option<(String, Option<String>)> {
        let name = self.name.clone().filter(|s| !s.trim().is_empty())?;
        Some((name, self.game_id.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
    }

    /// Creates a game folder that looks installed - i.e. with a file in it.
    /// Fixtures here used to be bare directories, which only passed because
    /// contentless folders were wrongly accepted as games (GT-29).
    fn create_installed_game(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        write_file(&dir.join("game.exe"), "MZ");
    }

    #[test]
    fn scan_container_finds_vendor_roots_with_games() {
        let temp = tempfile::tempdir().unwrap();
        create_installed_game(&temp.path().join(r"Epic\Celeste"));
        create_installed_game(&temp.path().join(r"Blizzard\StarCraft II"));
        create_installed_game(&temp.path().join(r"Unrelated\Stuff"));

        let libraries = scan_container(temp.path());

        assert_eq!(libraries.len(), 2);
        let vendors: Vec<&str> = libraries.iter().map(|lib| lib.vendor).collect();
        assert!(vendors.contains(&"epic"));
        assert!(vendors.contains(&"battlenet"));
    }

    #[test]
    fn scan_container_skips_empty_vendor_roots() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("Epic")).unwrap();

        assert!(scan_container(temp.path()).is_empty());
    }

    #[test]
    fn read_vendor_library_excludes_infrastructure_and_hidden_dirs() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Blizzard");
        create_installed_game(&root.join("Diablo III"));
        create_installed_game(&root.join("Battle.net"));
        create_installed_game(&root.join(".hidden"));

        let library = read_vendor_library("battlenet", &root).expect("expected a library");

        assert_eq!(library.games.len(), 1);
        assert_eq!(library.games[0].name, "Diablo III");
    }

    /// GT-29. A contentless subfolder of a vendor root used to be registered
    /// as an installed game. It is residue - typically the directory skeleton
    /// a removed install leaves behind - and a phantom entry in the model of a
    /// tool that deletes files must not exist, even when the phantom itself is
    /// empty: it is counted in the same totals as real games.
    #[test]
    fn read_vendor_library_ignores_a_contentless_folder() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Epic");
        create_installed_game(&root.join("Celeste"));
        // Residue: the folder is there, every file is gone.
        std::fs::create_dir_all(root.join(r"Uninstalled Game\bin")).unwrap();

        let library = read_vendor_library("epic", &root).expect("expected a library");

        assert_eq!(
            library.games.len(),
            1,
            "only the folder with files is a game, got {:?}",
            library.games.iter().map(|g| &g.name).collect::<Vec<_>>()
        );
        assert_eq!(library.games[0].name, "Celeste");
    }

    /// The whole-library case of the same rule: a vendor root holding nothing
    /// but empty folders registers no library at all, rather than a library of
    /// phantoms.
    #[test]
    fn read_vendor_library_is_none_when_every_subfolder_is_contentless() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("GOG");
        std::fs::create_dir_all(root.join("Ghost One")).unwrap();
        std::fs::create_dir_all(root.join("Ghost Two")).unwrap();

        assert!(read_vendor_library("gog", &root).is_none());
    }

    #[test]
    fn build_game_reads_gog_manifest_name_and_id() {
        let temp = tempfile::tempdir().unwrap();
        let game_dir = temp.path().join("Fallout 2");
        std::fs::create_dir_all(&game_dir).unwrap();
        write_file(
            &game_dir.join("goggame-1440151285.info"),
            r#"{ "name": "Fallout 2", "gameId": "1440151285", "rootGameId": "1440151285" }"#,
        );

        let game = build_game("gog", game_dir.clone()).expect("expected a game");

        assert_eq!(game.name, "Fallout 2");
        assert_eq!(game.app_id.as_deref(), Some("1440151285"));
        assert_eq!(game.install_dir, game_dir);
    }

    #[test]
    fn read_gog_info_prefers_base_game_over_dlc_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let game_dir = temp.path().join("Witcher");
        std::fs::create_dir_all(&game_dir).unwrap();
        write_file(
            &game_dir.join("goggame-2.info"),
            r#"{ "name": "Some DLC", "gameId": "2", "rootGameId": "1" }"#,
        );
        write_file(
            &game_dir.join("goggame-1.info"),
            r#"{ "name": "The Witcher 2", "gameId": "1", "rootGameId": "1" }"#,
        );

        let (name, id) = read_gog_info(&game_dir).expect("expected gog info");

        assert_eq!(name, "The Witcher 2");
        assert_eq!(id.as_deref(), Some("1"));
    }

    #[test]
    fn build_game_falls_back_to_folder_name_without_gog_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let game_dir = temp.path().join("Some Game");
        std::fs::create_dir_all(&game_dir).unwrap();

        let game = build_game("gog", game_dir).expect("expected a game");

        assert_eq!(game.name, "Some Game");
        assert_eq!(game.app_id, None);
    }

    #[test]
    fn is_gog_info_file_matches_case_insensitively() {
        assert!(is_gog_info_file(Path::new("goggame-123.info")));
        assert!(is_gog_info_file(Path::new("GOGGAME-123.INFO")));
        assert!(!is_gog_info_file(Path::new("goggame-123.ico")));
        assert!(!is_gog_info_file(Path::new("other.info")));
    }

    #[test]
    fn is_infrastructure_dir_matches_known_names_and_prefixes() {
        assert!(is_infrastructure_dir(Path::new(r"F:\GOG\GOG Galaxy")));
        assert!(is_infrastructure_dir(Path::new(r"F:\Blizzard\Battle.net")));
        assert!(is_infrastructure_dir(Path::new(r"F:\Epic\.egstore")));
        assert!(is_infrastructure_dir(Path::new(r"C:\$Recycle.Bin")));
        assert!(!is_infrastructure_dir(Path::new(r"F:\Epic\Celeste")));
    }
}
