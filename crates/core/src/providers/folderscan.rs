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

use super::{
    diagnostic, try_holds_installed_files, DiscoveredLibrary, DiscoveryDiagnostic, DiscoveryReport,
    GameInstall, LibraryProvider, OrphanEvidence,
};

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

    fn try_discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        Ok(discover_folder_scan().data)
    }

    fn discover(&self) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
        discover_folder_scan()
    }
}

fn discover_folder_scan() -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let mut libraries = Vec::new();
    let mut diagnostics = Vec::new();
    for drive in drive_roots() {
        match std::fs::metadata(&drive) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => continue,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                diagnostics.push(diagnostic(
                    "folderscan",
                    "drive-metadata",
                    drive.to_path_buf(),
                    err,
                ));
                continue;
            }
        }
        let (mut found, mut issues) = scan_container_report(&drive);
        libraries.append(&mut found);
        diagnostics.append(&mut issues);
        for container in GAMES_CONTAINERS {
            let (mut found, mut issues) = scan_container_report(&drive.join(container));
            libraries.append(&mut found);
            diagnostics.append(&mut issues);
        }
    }
    if diagnostics.is_empty() {
        DiscoveryReport::complete(libraries)
    } else {
        for library in &mut libraries {
            library.orphan_evidence = OrphanEvidence::Degraded;
        }
        DiscoveryReport::degraded(libraries, diagnostics)
    }
}

/// All possible drive roots (`A:\` .. `Z:\`); nonexistent drives fail the
/// `is_dir()` check in `discover` and are skipped.
fn drive_roots() -> impl Iterator<Item = PathBuf> {
    (b'A'..=b'Z').map(|letter| PathBuf::from(format!(r"{}:\", letter as char)))
}

/// Scans one container directory for vendor-named library folders.
#[cfg(test)]
fn scan_container(container: &Path) -> Vec<DiscoveredLibrary> {
    scan_container_report(container).0
}

fn scan_container_report(container: &Path) -> (Vec<DiscoveredLibrary>, Vec<DiscoveryDiagnostic>) {
    let mut libraries = Vec::new();
    let mut diagnostics = Vec::new();
    for (vendor, names) in VENDOR_ROOTS {
        for name in *names {
            let root = container.join(name);
            match std::fs::metadata(&root) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => continue,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    diagnostics.push(diagnostic(
                        vendor,
                        "vendor-root-metadata",
                        root.to_path_buf(),
                        err,
                    ));
                    continue;
                }
            }
            let (library, mut issues) = read_vendor_library_report(vendor, &root);
            diagnostics.append(&mut issues);
            if let Some(library) = library {
                libraries.push(library);
            }
        }
    }
    (libraries, diagnostics)
}

/// Builds one library from a vendor root folder: every non-infrastructure
/// subfolder that actually holds files is a game. Returns `None` when no games
/// remain - an empty vendor folder is not worth registering as a library.
///
/// The `holds_installed_files` filter is installed-content validation: without it a contentless
/// subfolder became a phantom game, counted in the totals of a tool that
/// deletes files. Unlike the metadata providers, folder-name discovery has no
/// launcher to ask whether a game is installed - the files on disk are the
/// only evidence there is.
#[cfg(test)]
fn read_vendor_library(vendor: &'static str, root: &Path) -> Option<DiscoveredLibrary> {
    read_vendor_library_report(vendor, root).0
}

fn read_vendor_library_report(
    vendor: &'static str,
    root: &Path,
) -> (Option<DiscoveredLibrary>, Vec<DiscoveryDiagnostic>) {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) => {
            return (
                None,
                vec![diagnostic(
                    vendor,
                    "vendor-root-enumeration",
                    root.to_path_buf(),
                    err,
                )],
            )
        }
    };
    let mut games = Vec::new();
    let mut diagnostics = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                diagnostics.push(diagnostic(
                    vendor,
                    "vendor-root-entry",
                    root.to_path_buf(),
                    err,
                ));
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) => {
                diagnostics.push(diagnostic(
                    vendor,
                    "game-entry-type",
                    path.to_path_buf(),
                    err,
                ));
                continue;
            }
        };
        if !file_type.is_dir() || is_infrastructure_dir(&path) {
            continue;
        }
        match try_holds_installed_files(&path) {
            Ok(false) => continue,
            Ok(true) => {}
            Err(err) => {
                diagnostics.push(diagnostic(
                    vendor,
                    "game-content-probe",
                    path.to_path_buf(),
                    err,
                ));
                continue;
            }
        }
        let (game, mut issues) = build_game_report(vendor, path);
        diagnostics.append(&mut issues);
        if let Some(game) = game {
            games.push(game);
        }
    }

    let library = (!games.is_empty()).then(|| DiscoveredLibrary {
        vendor,
        path: root.to_path_buf(),
        games,
        orphan_evidence: OrphanEvidence::Heuristic,
    });
    (library, diagnostics)
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
#[cfg(test)]
fn build_game(vendor: &'static str, install_dir: PathBuf) -> Option<GameInstall> {
    build_game_report(vendor, install_dir).0
}

fn build_game_report(
    vendor: &'static str,
    install_dir: PathBuf,
) -> (Option<GameInstall>, Vec<DiscoveryDiagnostic>) {
    let mut diagnostics = Vec::new();
    let Some(folder_name) = install_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
    else {
        diagnostics.push(diagnostic(
            vendor,
            "game-name",
            install_dir,
            "game path has no usable folder name",
        ));
        return (None, diagnostics);
    };
    let (name, app_id) = match vendor {
        "gog" => {
            let (info, mut issues) = read_gog_info_report(&install_dir);
            diagnostics.append(&mut issues);
            info.unwrap_or((folder_name, None))
        }
        _ => (folder_name, None),
    };
    (
        Some(GameInstall {
            name,
            install_dir,
            app_id,
        }),
        diagnostics,
    )
}

/// Reads the base game's `goggame-<id>.info` manifest from a GOG install
/// directory. A directory can hold several manifests (base game + DLCs);
/// the one whose `gameId` equals `rootGameId` is the base game.
#[cfg(test)]
fn read_gog_info(dir: &Path) -> Option<(String, Option<String>)> {
    read_gog_info_report(dir).0
}

fn read_gog_info_report(
    dir: &Path,
) -> (Option<(String, Option<String>)>, Vec<DiscoveryDiagnostic>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            return (
                None,
                vec![diagnostic(
                    "gog",
                    "manifest-enumeration",
                    dir.to_path_buf(),
                    err,
                )],
            )
        }
    };
    let mut parsed = Vec::new();
    let mut diagnostics = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                diagnostics.push(diagnostic("gog", "manifest-entry", dir.to_path_buf(), err));
                continue;
            }
        };
        let path = entry.path();
        if !is_gog_info_file(&path) {
            continue;
        }
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) => {
                diagnostics.push(diagnostic("gog", "manifest-read", path.to_path_buf(), err));
                continue;
            }
        };
        match serde_json::from_str::<GogInfo>(&contents) {
            Ok(info) => parsed.push(info),
            Err(err) => {
                diagnostics.push(diagnostic("gog", "manifest-parse", path.to_path_buf(), err))
            }
        }
    }

    let info = parsed
        .iter()
        .find(|info| info.is_base_game())
        .or_else(|| parsed.first())
        .and_then(GogInfo::name_and_id);
    (info, diagnostics)
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
    // GOG's own manifests write this as a JSON string (e.g. "50596806354071163"),
    // but nothing enforces that beyond convention. If this field were typed as
    // `Option<String>` and one install wrote it as a bare JSON number, serde
    // would fail to deserialize the *entire* `GogInfo` struct for that
    // manifest - silently losing the name and gameId read too, not just the
    // build id. Accepting either shape here keeps a numeric buildId from
    // regressing the name/id lookup that already ships.
    #[serde(rename = "buildId", default, deserialize_with = "deserialize_build_id")]
    build_id: Option<String>,
}

/// Accepts `buildId` as either a JSON string or a JSON number and normalizes
/// both to a `String` - see the comment on `GogInfo::build_id` for why this
/// must not be a strict `Option<String>`.
fn deserialize_build_id<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNumber {
        String(String),
        Number(serde_json::Number),
    }

    Ok(
        Option::<StringOrNumber>::deserialize(deserializer)?.map(|value| match value {
            StringOrNumber::String(s) => s,
            StringOrNumber::Number(n) => n.to_string(),
        }),
    )
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

/// Reads the base game's `buildId` from every GOG game folder directly under
/// `library_root`, keyed by the same `gameId` value folderscan reports as
/// `app_id`. This is GOG's answer to Steam's build id: the manifest carries
/// no update timestamp, so `buildId` is the only field that changes when a
/// game's content changes, which is what lets `core::gamestate::changed_games`
/// tell "this game came back after an update" apart from "this is the same
/// install as last scan".
///
/// Uses the same game/non-game boundary discovery already applies for GOG
/// folders (`is_infrastructure_dir`, `try_holds_installed_files`) so this map
/// never reports a build id for - or omits one for - a directory discovery
/// itself would disagree about.
///
/// Deliberately narrower than `read_gog_info_report`'s name lookup: a folder
/// whose only manifest is a DLC (or otherwise fails `is_base_game`) is simply
/// absent here, with no fallback to "the first manifest found". Name lookup
/// falls back for display purposes, where an approximate name is harmless;
/// a build id borrowed from the wrong product would misreport whether the
/// *base game* changed, which is actively wrong rather than imprecise.
///
/// A missing or unreadable `library_root`, a directory with no base manifest,
/// or a base manifest with no (or empty) `buildId` all yield an absent entry
/// rather than an error - `changed_games` reads a missing entry as "unknown,
/// claim nothing", which is the honest outcome for all three cases.
pub fn gog_build_ids(
    library_root: &std::path::Path,
) -> crate::error::Result<std::collections::HashMap<String, String>> {
    let mut build_ids = std::collections::HashMap::new();
    let entries = match std::fs::read_dir(library_root) {
        Ok(entries) => entries,
        Err(_) => return Ok(build_ids),
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() || is_infrastructure_dir(&path) {
            continue;
        }
        if !matches!(try_holds_installed_files(&path), Ok(true)) {
            continue;
        }
        if let Some((game_id, build_id)) = read_gog_build_id(&path) {
            build_ids.insert(game_id, build_id);
        }
    }
    Ok(build_ids)
}

/// Finds the base-game `goggame-<id>.info` manifest in `dir` and returns its
/// `(gameId, buildId)`, if both are present and non-empty.
fn read_gog_build_id(dir: &Path) -> Option<(String, String)> {
    let entries = std::fs::read_dir(dir).ok()?;
    let base = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| is_gog_info_file(path))
        .filter_map(|path| std::fs::read_to_string(&path).ok())
        .filter_map(|contents| serde_json::from_str::<GogInfo>(&contents).ok())
        .find(GogInfo::is_base_game)?;
    let game_id = base.game_id?;
    let build_id = base.build_id.filter(|s| !s.trim().is_empty())?;
    Some((game_id, build_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
    }

    /// Creates a game folder that looks installed - i.e. with a file in it.
    /// Fixtures here used to be bare directories, which only passed because
    /// contentless folders were wrongly accepted as games (installed-content validation).
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

    /// installed-content validation. A contentless subfolder of a vendor root used to be registered
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

    #[test]
    fn gog_build_ids_maps_game_id_to_build_id() {
        let temp = tempfile::tempdir().unwrap();
        let game_dir = temp.path().join("AD2044");
        std::fs::create_dir_all(&game_dir).unwrap();
        write_file(
            &game_dir.join("goggame-2075976504.info"),
            r#"{ "name": "AD 2044", "gameId": "2075976504", "rootGameId": "2075976504", "buildId": "50596806354071163" }"#,
        );

        let build_ids = gog_build_ids(temp.path()).unwrap();

        assert_eq!(
            build_ids.get("2075976504").map(String::as_str),
            Some("50596806354071163")
        );
    }

    #[test]
    fn gog_build_ids_omits_manifest_without_build_id() {
        let temp = tempfile::tempdir().unwrap();
        let game_dir = temp.path().join("Fallout 2");
        std::fs::create_dir_all(&game_dir).unwrap();
        write_file(
            &game_dir.join("goggame-1440151285.info"),
            r#"{ "name": "Fallout 2", "gameId": "1440151285", "rootGameId": "1440151285" }"#,
        );

        let build_ids = gog_build_ids(temp.path()).unwrap();

        assert!(
            !build_ids.contains_key("1440151285"),
            "a missing buildId must be absent, never mapped to an empty string"
        );
    }

    #[test]
    fn gog_build_ids_ignores_dlc_only_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let game_dir = temp.path().join("Witcher DLC");
        std::fs::create_dir_all(&game_dir).unwrap();
        write_file(
            &game_dir.join("goggame-2.info"),
            r#"{ "name": "Some DLC", "gameId": "2", "rootGameId": "1", "buildId": "999" }"#,
        );

        let build_ids = gog_build_ids(temp.path()).unwrap();

        assert!(build_ids.is_empty());
    }

    #[test]
    fn gog_build_ids_is_empty_for_missing_library_root() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("does-not-exist");

        let build_ids = gog_build_ids(&missing).unwrap();

        assert!(build_ids.is_empty());
    }

    #[test]
    fn gog_build_ids_numeric_build_id_does_not_break_name_and_id_read() {
        // Same fixture shape as `build_game_reads_gog_manifest_name_and_id`,
        // but with `buildId` written as a bare JSON number - proving that
        // shape does not regress the existing name/id read (see the comment
        // on `GogInfo::build_id`).
        let temp = tempfile::tempdir().unwrap();
        let game_dir = temp.path().join("Fallout 2");
        std::fs::create_dir_all(&game_dir).unwrap();
        write_file(
            &game_dir.join("goggame-1440151285.info"),
            r#"{ "name": "Fallout 2", "gameId": "1440151285", "rootGameId": "1440151285", "buildId": 50596806354071163 }"#,
        );

        let game = build_game("gog", game_dir.clone()).expect("expected a game");
        assert_eq!(game.name, "Fallout 2");
        assert_eq!(game.app_id.as_deref(), Some("1440151285"));

        let build_ids = gog_build_ids(temp.path()).unwrap();
        assert_eq!(
            build_ids.get("1440151285").map(String::as_str),
            Some("50596806354071163")
        );
    }
}
