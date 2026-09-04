//! Epic Games Store library discovery: registry -> `Manifests\*.item` (JSON).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use winreg::enums::HKEY_LOCAL_MACHINE;
use winreg::RegKey;

use crate::error::Result;
use crate::gamestate::EpicItemState;

use super::{
    diagnostic, DiscoveredLibrary, DiscoveryReport, DiscoveryStatus, GameInstall, LibraryProvider,
    OrphanEvidence,
};

// `GAME_ABSENT` and `degrades_evidence` live in `super` - see
// `providers::GAME_ABSENT` for why an absent install dir must not degrade a
// library the same way an unexaminable one does.
use super::{degrades_evidence, GAME_ABSENT};

const REGISTRY_KEY: &str = r"SOFTWARE\WOW6432Node\Epic Games\EpicGamesLauncher";
const REGISTRY_VALUE: &str = "AppDataPath";
const DEFAULT_DATA_DIR: &str = r"C:\ProgramData\Epic\EpicGamesLauncher\Data";

pub struct EpicProvider;

impl LibraryProvider for EpicProvider {
    fn name(&self) -> &'static str {
        "epic"
    }

    fn try_discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        Ok(discover_epic().data)
    }

    fn discover(&self) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
        discover_epic()
    }
}

fn discover_epic() -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let manifests_dir = match find_manifests_dir() {
        Ok(path) => path,
        Err(err) => {
            return DiscoveryReport::failed(Vec::new(), diagnostic("epic", "registry", None, err))
        }
    };
    discover_manifests(&manifests_dir)
}

/// Reads every `*.item` manifest in `manifests_dir` and builds the games it
/// describes. Split out from `discover_epic` so the manifest-directory walk -
/// including the `try_is_dir` absent-vs-unexaminable split below - is
/// reachable from a test with a temp dir, without going through the registry
/// lookup in `find_manifests_dir`.
fn discover_manifests(manifests_dir: &Path) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let entries = match std::fs::read_dir(manifests_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return DiscoveryReport::not_installed(Vec::new())
        }
        Err(err) => {
            return DiscoveryReport::failed(
                Vec::new(),
                diagnostic(
                    "epic",
                    "manifest-enumeration",
                    Some(manifests_dir.to_path_buf()),
                    err,
                ),
            )
        }
    };
    let mut diagnostics = Vec::new();
    let mut games = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                diagnostics.push(diagnostic(
                    "epic",
                    "manifest-entry",
                    Some(manifests_dir.to_path_buf()),
                    err,
                ));
                continue;
            }
        };
        let path = entry.path();
        if !is_item_manifest(&path) {
            continue;
        }
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) => {
                diagnostics.push(diagnostic("epic", "manifest-read", Some(path), err));
                continue;
            }
        };
        let game = match parse_item_result(&contents) {
            Ok(game) => game,
            Err(err) => {
                diagnostics.push(diagnostic("epic", "manifest-parse", Some(path), err));
                continue;
            }
        };
        // A manifest whose install dir is simply not there is normal -
        // uninstalled outside the Epic launcher, or a queued/paused
        // download - and a folder that does not exist cannot be mistaken
        // for orphan residue either. A folder we merely failed to read is
        // the dangerous case: it stays on disk and drops out of `games`,
        // which is the shape orphan detection reads as residue. Latent for
        // Epic rather than live - `orphan_spec_for` wires only Steam, Xbox
        // and itch - so diagnose it and keep the guarantee true in advance.
        match super::try_is_dir(&game.install_dir) {
            Ok(true) => games.push(game),
            // Recorded, but explicitly not degrading - see `GAME_ABSENT`.
            Ok(false) => diagnostics.push(diagnostic("epic", 
                GAME_ABSENT,
                Some(game.install_dir),
                "manifest present, install directory absent (uninstalled outside the Epic launcher, or a queued/paused download)",
            )),
            Err(err) => diagnostics.push(diagnostic("epic", 
                "game-path",
                Some(game.install_dir),
                err,
            )),
        }
    }
    let mut libraries = super::group_by_parent_dir("epic", games);
    if degrades_evidence(&diagnostics) {
        for library in &mut libraries {
            library.orphan_evidence = OrphanEvidence::Degraded;
        }
        DiscoveryReport::degraded(libraries, diagnostics)
    } else {
        // Complete, but not necessarily silent: a `GAME_ABSENT` note still
        // travels so it reaches the log and `scan_diagnostics`.
        // `DiscoveryReport::complete` would drop it, which is the whole
        // behaviour this card exists to change.
        DiscoveryReport {
            data: libraries,
            status: DiscoveryStatus::Complete,
            diagnostics,
        }
    }
}

/// Builds a map from Epic's `AppName` - the same value this provider reports
/// as `app_id` in `parse_item_result` below - to `AppVersionString`, for every
/// readable `.item` manifest. This is Epic's counterpart to Steam's
/// `buildid`: `gamestate::changed_games` diffs the value recorded at the last
/// scan against the one read here to answer "did this game come back after
/// an update?" without rescanning its files.
///
/// Epic simply not being installed (no `Manifests` dir) is not an error, it
/// is the common case on a machine without the launcher - see
/// `build_ids_from_manifests`. A registry read failure, by contrast, is
/// unexpected and propagated, matching how `discover_epic` treats the same
/// failure from `find_manifests_dir`.
pub fn build_ids() -> Result<HashMap<String, String>> {
    let manifests_dir = find_manifests_dir()?;
    build_ids_from_manifests(&manifests_dir)
}

/// Reads every `*.item` manifest in `manifests_dir` and extracts its version.
/// Split out from `build_ids` so a test can point it at a temp dir without
/// going through the registry lookup - mirrors `discover_manifests` /
/// `discover_epic` below.
///
/// A manifest is left out of the map, rather than mapped to an empty string,
/// when it has no `AppVersionString` (some Epic titles never report one) or
/// fails to parse at all (one corrupt file must not cost every other game its
/// entry). `changed_games` reads a missing key as "unknown, claim nothing" -
/// exactly the honest behaviour absence should have here.
fn build_ids_from_manifests(manifests_dir: &Path) -> Result<HashMap<String, String>> {
    let entries = match std::fs::read_dir(manifests_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(err) => return Err(err.into()),
    };

    let mut build_ids = HashMap::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        if !is_item_manifest(&path) {
            continue;
        }
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(_) => continue,
        };
        // Reuses `gamestate::EpicItemState` rather than adding a second,
        // duplicate `AppVersionString` field to `EpicItemManifest` below -
        // one parse of the manifest's shape, not two drifting in step.
        let Some(state) = EpicItemState::parse(&contents) else {
            continue;
        };
        if let Some(version) = state
            .app_version_string
            .filter(|version| !version.trim().is_empty())
        {
            build_ids.insert(state.app_name, version);
        }
    }
    Ok(build_ids)
}

/// Locates the `Manifests` directory holding `*.item` files, preferring the
/// `AppDataPath` reported by `HKLM\...\EpicGamesLauncher`, falling back to the
/// well-known default `ProgramData` location.
fn find_manifests_dir() -> std::io::Result<PathBuf> {
    let base = read_app_data_path()?
        .map(|raw| PathBuf::from(super::normalize_slashes(&raw)))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR));
    Ok(base.join("Manifests"))
}

fn read_app_data_path() -> std::io::Result<Option<String>> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let key = match hklm.open_subkey(REGISTRY_KEY) {
        Ok(key) => key,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    match key.get_value::<String, _>(REGISTRY_VALUE) {
        Ok(value) => Ok(Some(value)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
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
    parse_item_result(json).ok()
}

fn parse_item_result(json: &str) -> std::result::Result<GameInstall, String> {
    let manifest: EpicItemManifest = serde_json::from_str(json).map_err(|err| err.to_string())?;

    let name = manifest
        .display_name
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing DisplayName".to_string())?;
    let install_location = manifest
        .install_location
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing InstallLocation".to_string())?;

    Ok(GameInstall {
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

    fn item_manifest(display_name: &str, install_location: &Path) -> String {
        format!(
            r#"{{"DisplayName": "{name}", "InstallLocation": "{loc}"}}"#,
            name = display_name,
            loc = install_location.display().to_string().replace('\\', "\\\\")
        )
    }

    /// The counterpart to Steam's paused-download case: a manifest for a
    /// game that was uninstalled outside the Epic launcher, or whose
    /// download is still queued, is ordinary and must not degrade the
    /// report - but it still has to leave a trace, which is the whole point
    /// of `GAME_ABSENT` existing separately from `DiscoveryStatus::Degraded`.
    #[test]
    fn discover_manifests_records_an_absent_install_dir_without_degrading() {
        let manifests_dir = tempfile::tempdir().unwrap();
        let install_dir = manifests_dir.path().join("NotThereYet");
        std::fs::write(
            manifests_dir.path().join("game.item"),
            item_manifest("Queued Game", &install_dir),
        )
        .unwrap();

        let report = discover_manifests(manifests_dir.path());

        assert_eq!(report.status, DiscoveryStatus::Complete);
        assert!(
            report.data.is_empty(),
            "no game was accepted, so there is no library to group it under"
        );
        assert_eq!(
            report
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.stage)
                .collect::<Vec<_>>(),
            vec![GAME_ABSENT],
            "the skipped manifest has to leave a trace: {:?}",
            report.diagnostics
        );
    }

    /// An install directory that cannot be examined - as opposed to one that
    /// is provably absent - must degrade the report. Silently dropping the
    /// game would leave its live folder unclaimed by any manifest, which is
    /// the shape orphan detection reads as residue.
    #[test]
    fn discover_manifests_degrades_on_an_unexaminable_install_dir() {
        let manifests_dir = tempfile::tempdir().unwrap();
        // `<` is invalid in a Windows path component, so the probe fails
        // with ERROR_INVALID_NAME rather than "not found" - a stand-in for
        // the real cases (DACL denial, offline placeholder, drive not
        // ready) that no test can create portably.
        let install_dir = manifests_dir.path().join("bad<name");
        std::fs::write(
            manifests_dir.path().join("game.item"),
            item_manifest("Broken", &install_dir),
        )
        .unwrap();

        let report = discover_manifests(manifests_dir.path());

        assert_eq!(report.status, DiscoveryStatus::Degraded);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.stage == "game-path"),
            "the failed probe must be visible, not silently dropped: {:?}",
            report.diagnostics
        );
    }

    fn versioned_manifest(app_name: &str, version: &str) -> String {
        format!(
            r#"{{"AppName": "{app_name}", "DisplayName": "{app_name}", "InstallLocation": "F:\\Epic\\{app_name}", "AppVersionString": "{version}"}}"#
        )
    }

    #[test]
    fn build_ids_from_manifests_maps_app_name_to_version() {
        let manifests_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            manifests_dir.path().join("fortnite.item"),
            versioned_manifest("Fortnite", "1.0.0"),
        )
        .unwrap();

        let build_ids = build_ids_from_manifests(manifests_dir.path()).unwrap();

        assert_eq!(build_ids.get("Fortnite").map(String::as_str), Some("1.0.0"));
    }

    /// A manifest with no `AppVersionString` must be absent from the map,
    /// not present with an empty value - `changed_games` reads a missing key
    /// as "unknown, claim nothing", which is the honest answer here, while an
    /// empty string would misrepresent it as a known (and matching-anything)
    /// build id.
    #[test]
    fn build_ids_from_manifests_omits_entry_when_version_missing() {
        let manifests_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            manifests_dir.path().join("no-version.item"),
            r#"{"AppName": "NoVersion", "DisplayName": "No Version", "InstallLocation": "F:\\Epic\\NoVersion"}"#,
        )
        .unwrap();

        let build_ids = build_ids_from_manifests(manifests_dir.path()).unwrap();

        assert!(!build_ids.contains_key("NoVersion"));
    }

    #[test]
    fn build_ids_from_manifests_returns_empty_map_for_missing_dir() {
        let manifests_dir = tempfile::tempdir().unwrap();
        let missing = manifests_dir.path().join("does-not-exist");

        let build_ids = build_ids_from_manifests(&missing).unwrap();

        assert!(build_ids.is_empty());
    }

    /// The counterpart to `discover_manifests_records_an_absent_install_dir_without_degrading`:
    /// one unparseable `.item` must not cost every other manifest its entry.
    #[test]
    fn build_ids_from_manifests_skips_corrupt_manifest_and_keeps_good_sibling() {
        let manifests_dir = tempfile::tempdir().unwrap();
        std::fs::write(manifests_dir.path().join("corrupt.item"), "not json at all").unwrap();
        std::fs::write(
            manifests_dir.path().join("good.item"),
            versioned_manifest("GoodGame", "2.3.4"),
        )
        .unwrap();

        let build_ids = build_ids_from_manifests(manifests_dir.path()).unwrap();

        assert_eq!(build_ids.get("GoodGame").map(String::as_str), Some("2.3.4"));
        assert_eq!(build_ids.len(), 1);
    }
}
