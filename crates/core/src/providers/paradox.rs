//! Paradox Launcher library discovery:
//! `%APPDATA%\Paradox Interactive\launcher-v2\userSettings.json`, array
//! `gameLibraryPaths`, cross-referenced with the ownership list
//! `%APPDATA%\Paradox Interactive\launcher-v2\game-metadata\game-metadata`
//! (a JSON file with no extension) for display names.
//!
//! # `gameLibraryPaths` is heterogeneous
//!
//! Confirmed on this machine (2026-08-17):
//! ```json
//! "gameLibraryPaths": [
//!   "../games",
//!   {
//!     "gameId": "surviving_mars",
//!     "repositoryPath": "surviving_mars-windows-64",
//!     "repositoryType": "cdn",
//!     "installationPath": "H:\\Paradox\\surviving_mars",
//!     "launcherSettingsDirPath": "H:\\Paradox\\surviving_mars"
//!   }
//! ]
//! ```
//! A plain string is a default install root, relative to the launcher
//! directory (`"../games"` under `...\launcher-v2` resolves to
//! `%APPDATA%\Paradox Interactive\games`). An object is one explicit
//! per-game install. See [`LibraryPathEntry`] for how a third, unforeseen
//! element shape is tolerated rather than treated as a parse failure.
//!
//! # `game-metadata` is an ownership list, not an install list
//!
//! It holds every game the account owns, installed or not (35 entries on
//! this machine) - a name lookup only. Building the library from it would
//! report games that were never downloaded; see [`read_game_names`] and the
//! `ownership_list_is_not_an_install_list` test.
//!
//! # The `.cpatch` trap
//!
//! An entry appears in `gameLibraryPaths` at the *start* of a download. In
//! July reconnaissance the install directory held only a `.cpatch\` service
//! folder (112 MB of a multi-gigabyte game) - handing that straight to a
//! tool that deletes files would have been catastrophic. Re-confirmed today
//! (2026-08-17) with a twist: that download has since finished, and
//! `H:\Paradox\surviving_mars` now holds `MarsPDX.exe`, `DLC\`, `Packs\`,
//! `Movies\`, etc. - but **`.cpatch\` is still there**, still holding 118 MB.
//! It does not disappear once the install completes. So "install directory
//! exists" is not enough; only a directory holding something *besides*
//! `.cpatch` counts as installed (see [`has_content_besides_cpatch`]).
//! `pendingGameInstallations` (a sibling key, empty `[]` on this machine) is
//! the launcher's own second signal for "still downloading" and is honored
//! independently - see [`pending_game_ids`].
//!
//! # `OrphanEvidence` choice
//!
//! Libraries built from explicit per-game entries get
//! [`OrphanEvidence::Authoritative`] (via [`super::group_by_parent_dir`]),
//! the same as Epic, GOG, Amazon and every other provider whose evidence is
//! a real launcher-owned manifest rather than a folder-name guess. That is
//! deliberate, not an oversight of the "arbitrary user directory" concern:
//! `OrphanEvidence` answers "did we read the launcher's own inventory
//! without omission", not "is a container diff safe here" - the doc
//! comments on [`OrphanEvidence`] and [`DiscoveredLibrary`] say exactly
//! that. Container-diff safety is a *separate* gate
//! (`orphan_spec_for` in `gametrimmer_app::worker::scan::orphan_analysis`),
//! keyed on `vendor` name, and it already excludes every registry/JSON
//! provider whose install root is an arbitrary user folder (Epic, GOG, EA,
//! Ubisoft, Battle.net, Rockstar, Riot) for exactly the reason the card
//! raises - there is no fixed, enumerable container to diff. Paradox
//! belongs in that same bucket: this provider does not add a `paradox_spec`
//! to `orphans.rs` or wire itself into `orphan_spec_for`, so no container
//! diff ever runs against an arbitrary `installationPath`'s parent, without
//! needing a special-cased `Heuristic` evidence value to prevent it.
//! `Heuristic` stays reserved for `folderscan.rs`, the one provider with no
//! manifest at all. The default-root libraries (via
//! [`super::register_root`]) are `Authoritative` for the same reason
//! Humble's and itch's are: the root itself is launcher-named, not guessed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::Result;

use super::{
    degrades_evidence, DiscoveredLibrary, DiscoveryDiagnostic, DiscoveryReport, DiscoveryStatus,
    GameInstall, LibraryProvider, OrphanEvidence, GAME_ABSENT,
};

const LAUNCHER_RELATIVE_DIR: &str = r"Paradox Interactive\launcher-v2";
const SETTINGS_FILE_NAME: &str = "userSettings.json";
const METADATA_RELATIVE_PATH: &str = r"game-metadata\game-metadata";

/// Paradox's per-game service folder, written at the start of every
/// download. See the module doc comment for why its mere presence is not
/// (and, since today's finding, its mere *absence* is not either) a usable
/// signal on its own.
const CPATCH_DIR_NAME: &str = ".cpatch";

pub struct ParadoxProvider;

impl LibraryProvider for ParadoxProvider {
    fn name(&self) -> &'static str {
        "paradox"
    }

    fn try_discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        Ok(discover_paradox().data)
    }

    fn discover(&self) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
        discover_paradox()
    }
}

fn paradox_diagnostic(
    stage: &'static str,
    path: Option<PathBuf>,
    message: impl std::fmt::Display,
) -> DiscoveryDiagnostic {
    DiscoveryDiagnostic {
        provider: "paradox",
        stage,
        path,
        message: message.to_string(),
    }
}

fn discover_paradox() -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let Some(launcher_dir) = launcher_dir() else {
        return DiscoveryReport::not_installed(Vec::new());
    };
    discover_paradox_at(&launcher_dir)
}

fn launcher_dir() -> Option<PathBuf> {
    let app_data = std::env::var("APPDATA").ok()?;
    Some(PathBuf::from(app_data).join(LAUNCHER_RELATIVE_DIR))
}

/// The testable core of Paradox discovery: everything past locating the
/// launcher directory. Split out so tests can drive it against a temp dir
/// instead of the real `%APPDATA%\Paradox Interactive\launcher-v2`.
fn discover_paradox_at(launcher_dir: &Path) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let settings_path = launcher_dir.join(SETTINGS_FILE_NAME);
    if !settings_path.is_file() {
        return DiscoveryReport::not_installed(Vec::new());
    }

    let contents = match std::fs::read_to_string(&settings_path) {
        Ok(contents) => contents,
        Err(err) => {
            return DiscoveryReport::failed(
                Vec::new(),
                paradox_diagnostic("settings-read", Some(settings_path), err),
            )
        }
    };
    let settings: ParadoxSettings = match serde_json::from_str(&contents) {
        Ok(settings) => settings,
        Err(err) => {
            return DiscoveryReport::failed(
                Vec::new(),
                paradox_diagnostic("settings-parse", Some(settings_path), err),
            )
        }
    };

    let names = read_game_names(launcher_dir);
    discover_paradox_from_settings(settings, launcher_dir, &names)
}

fn discover_paradox_from_settings(
    settings: ParadoxSettings,
    launcher_dir: &Path,
    names: &HashMap<String, String>,
) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let pending = pending_game_ids(&settings.pending_game_installations);

    let mut diagnostics = Vec::new();
    let mut games = Vec::new();
    let mut default_roots = Vec::new();

    for entry in settings.game_library_paths {
        match entry {
            LibraryPathEntry::Explicit(raw) => {
                let Some(candidate) = build_candidate(raw, names) else {
                    // Missing gameId or installationPath: an ordinary
                    // incomplete entry, not a failure - dropped silently
                    // like every other provider's per-row misses (see
                    // amazon.rs / humble.rs for the same pattern).
                    continue;
                };
                if pending.contains(&candidate.game_id) {
                    diagnostics.push(paradox_diagnostic(
                        GAME_ABSENT,
                        Some(candidate.install_dir),
                        "gameId listed in pendingGameInstallations (download in progress)",
                    ));
                    continue;
                }
                classify_candidate(candidate, &mut games, &mut diagnostics);
            }
            LibraryPathEntry::Root(raw_path) => {
                if let Some(root) = resolve_relative_root(launcher_dir, &raw_path) {
                    default_roots.push(root);
                }
            }
            // A future launcher version's third `gameLibraryPaths` element
            // shape: ignored, not fatal - see the doc comment on
            // `LibraryPathEntry`.
            LibraryPathEntry::Other(_) => {}
        }
    }

    let mut libraries = super::group_by_parent_dir("paradox", games);
    for root in default_roots {
        // Plain `is_dir()`, not `try_is_dir()`, on purpose - same reasoning
        // as itch's and Humble's install-location registration:
        // `register_root` only ever fires when no library already covers
        // this path, so a false "absent" here cannot strip a live install
        // out of the managed set the way the per-game check above can.
        if root.is_dir() {
            super::register_root(&mut libraries, "paradox", root);
        } else {
            diagnostics.push(paradox_diagnostic(
                "default-root",
                Some(root),
                "configured default Paradox install root is unavailable",
            ));
        }
    }

    if degrades_evidence(&diagnostics) {
        for library in &mut libraries {
            library.orphan_evidence = OrphanEvidence::Degraded;
        }
        DiscoveryReport::degraded(libraries, diagnostics)
    } else {
        // Complete, but not necessarily silent: a `GAME_ABSENT` note still
        // travels so it reaches the log and `scan_diagnostics`.
        // `DiscoveryReport::complete` would drop it, which is the whole
        // behaviour GT-107 exists to guarantee.
        DiscoveryReport {
            data: libraries,
            status: DiscoveryStatus::Complete,
            diagnostics,
        }
    }
}

/// Resolves one explicit candidate's install directory: present and holding
/// real content -> a game; present but `.cpatch`-only, or absent outright ->
/// `GAME_ABSENT` (ordinary, non-degrading); unexaminable -> a real,
/// degrading diagnostic. Mirrors the `try_is_dir` absent-vs-unexaminable
/// split every other provider in this module directory uses.
fn classify_candidate(
    candidate: ParadoxCandidate,
    games: &mut Vec<GameInstall>,
    diagnostics: &mut Vec<DiscoveryDiagnostic>,
) {
    match super::try_is_dir(&candidate.install_dir) {
        Ok(true) => match has_content_besides_cpatch(&candidate.install_dir) {
            Ok(true) => games.push(GameInstall {
                name: candidate.name,
                install_dir: candidate.install_dir,
                app_id: Some(candidate.game_id),
            }),
            Ok(false) => diagnostics.push(paradox_diagnostic(
                GAME_ABSENT,
                Some(candidate.install_dir),
                "install directory holds only .cpatch - still mid-download (.cpatch is confirmed \
                 to persist past a completed install too, so its mere presence proves nothing; \
                 its exclusivity here does)",
            )),
            Err(err) => {
                diagnostics.push(paradox_diagnostic("game-path", Some(candidate.install_dir), err))
            }
        },
        Ok(false) => diagnostics.push(paradox_diagnostic(
            GAME_ABSENT,
            Some(candidate.install_dir),
            "entry present, install directory absent (not yet downloaded, or uninstalled outside the launcher)",
        )),
        Err(err) => {
            diagnostics.push(paradox_diagnostic("game-path", Some(candidate.install_dir), err))
        }
    }
}

/// Whether `dir` holds anything besides Paradox's `.cpatch` service folder.
/// See the module doc comment's "`.cpatch` trap" section for why this - and
/// not a plain `is_dir` / "any file present" check - is the load-bearing
/// guard against handing an in-progress download to the scanner.
fn has_content_besides_cpatch(dir: &Path) -> std::io::Result<bool> {
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        if !entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(CPATCH_DIR_NAME)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Resolves `relative` (as it appears in `gameLibraryPaths`, e.g.
/// `"../games"`) against `launcher_dir`, collapsing `..`/`.` components so
/// the result is the tidy path the launcher actually means
/// (`%APPDATA%\Paradox Interactive\games`), not a literal
/// `...\launcher-v2\..\games` string. Pure path algebra - the filesystem is
/// never touched, so this is safe to call on a root that may not exist.
fn resolve_relative_root(launcher_dir: &Path, relative: &str) -> Option<PathBuf> {
    let relative = relative.trim();
    if relative.is_empty() {
        return None;
    }

    let mut components: Vec<std::path::Component> = launcher_dir.components().collect();
    for part in Path::new(relative).components() {
        match part {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            other => components.push(other),
        }
    }
    if components.is_empty() {
        return None;
    }
    Some(components.iter().collect())
}

/// The subset of `userSettings.json` we read.
#[derive(Debug, Default, Deserialize)]
struct ParadoxSettings {
    #[serde(rename = "gameLibraryPaths", default)]
    game_library_paths: Vec<LibraryPathEntry>,
    #[serde(rename = "pendingGameInstallations", default)]
    pending_game_installations: Vec<serde_json::Value>,
}

/// One element of `gameLibraryPaths`. The array is heterogeneous by design -
/// see the module doc comment. `#[serde(untagged)]` tries each variant in
/// declaration order: [`Self::Explicit`] first (every field is `Option`, so
/// it matches any JSON object), then [`Self::Root`] (a plain string), then
/// [`Self::Other`] as a catch-all for anything else - a bare `serde_json`
/// value that is parsed but never interpreted. That last arm is the whole
/// point: a future launcher version adding a third element shape (or a
/// malformed one) degrades to "ignored", never to a parse failure that
/// takes the entire settings file - and therefore Paradox discovery - down
/// with it.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LibraryPathEntry {
    Explicit(ParadoxGameEntry),
    Root(String),
    #[allow(dead_code)]
    Other(serde_json::Value),
}

/// One explicit per-game entry from `gameLibraryPaths`. Both fields are
/// `Option`: a missing `gameId` or `installationPath` is an entry
/// `build_candidate` cannot use, not a parse failure.
///
/// `repositoryPath` / `repositoryType` are present in real entries but carry
/// nothing discovery needs. `launcherSettingsDirPath` is deliberately NOT
/// captured here either: on the one confirmed real sample it is
/// byte-identical to `installationPath`, and serde already ignores
/// unrecognized object fields by default, so adding a field for it would
/// track a second copy of data GameTrimmer already has under a name nothing
/// else in this file uses.
#[derive(Debug, Deserialize)]
struct ParadoxGameEntry {
    #[serde(rename = "gameId")]
    game_id: Option<String>,
    #[serde(rename = "installationPath")]
    installation_path: Option<String>,
}

/// One explicit entry reduced to what discovery needs: an id (also the
/// `pendingGameInstallations` join key), a directory to examine, and a
/// display name already resolved against `game-metadata`.
struct ParadoxCandidate {
    game_id: String,
    install_dir: PathBuf,
    name: String,
}

/// Builds a candidate from one raw entry. `None` for an entry missing a
/// usable `gameId` or `installationPath` - ordinary, not a failure (see the
/// call site in `discover_paradox_from_settings`).
fn build_candidate(
    raw: ParadoxGameEntry,
    names: &HashMap<String, String>,
) -> Option<ParadoxCandidate> {
    let game_id = raw.game_id.filter(|s| !s.trim().is_empty())?;
    let installation_path = raw.installation_path.filter(|s| !s.trim().is_empty())?;
    let install_dir = PathBuf::from(installation_path);

    // `game-metadata` is Paradox's OWNERSHIP list (every game on the
    // account, installed or not) - never the install source, so a missing
    // or malformed lookup must not stop a game from being discovered. It is
    // used purely to prettify the name; a game with no metadata entry still
    // shows up, just named by its install folder or gameId.
    let name = names
        .get(&game_id)
        .cloned()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            install_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| game_id.clone());

    Some(ParadoxCandidate {
        game_id,
        install_dir,
        name,
    })
}

/// Extracts the set of `gameId`s currently mid-download from
/// `pendingGameInstallations`. The shape of a non-empty array has not been
/// confirmed on real data (it is `[]` on this machine), so both a bare id
/// string and an object carrying a `gameId` field are accepted defensively -
/// consistent with `LibraryPathEntry::Other` tolerating an unconfirmed shape
/// elsewhere in this file rather than erroring on it.
fn pending_game_ids(raw: &[serde_json::Value]) -> HashSet<String> {
    raw.iter()
        .filter_map(|value| match value {
            serde_json::Value::String(id) => Some(id.clone()),
            serde_json::Value::Object(fields) => fields
                .get("gameId")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            _ => None,
        })
        .collect()
}

/// Best-effort read of `game-metadata\game-metadata` (id -> display name).
/// A missing or malformed copy yields an empty map rather than a
/// diagnostic: unlike `userSettings.json`, this file is never the install
/// source (see the module doc comment), so its absence affects name quality
/// only, never whether a game is discovered or a library's orphan evidence.
fn read_game_names(launcher_dir: &Path) -> HashMap<String, String> {
    let path = launcher_dir.join(METADATA_RELATIVE_PATH);
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    parse_game_names(&contents)
}

fn parse_game_names(json: &str) -> HashMap<String, String> {
    #[derive(Deserialize)]
    struct Metadata {
        data: MetadataData,
    }
    #[derive(Deserialize)]
    struct MetadataData {
        #[serde(default)]
        games: Vec<MetadataGame>,
    }
    #[derive(Deserialize)]
    struct MetadataGame {
        id: Option<String>,
        name: Option<String>,
    }

    let Ok(parsed) = serde_json::from_str::<Metadata>(json) else {
        return HashMap::new();
    };
    parsed
        .data
        .games
        .into_iter()
        .filter_map(|game| {
            let id = game.id.filter(|s| !s.trim().is_empty())?;
            let name = game.name.filter(|s| !s.trim().is_empty())?;
            Some((id, name))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_json(entries_json: &str) -> String {
        format!(r#"{{"gameLibraryPaths": [{entries_json}]}}"#)
    }

    #[test]
    fn parses_the_confirmed_heterogeneous_array() {
        let json = r#"
{
    "gameLibraryPaths": [
        "../games",
        {
            "gameId": "surviving_mars",
            "repositoryPath": "surviving_mars-windows-64",
            "repositoryType": "cdn",
            "installationPath": "H:\\Paradox\\surviving_mars",
            "launcherSettingsDirPath": "H:\\Paradox\\surviving_mars"
        }
    ],
    "pendingGameInstallations": []
}
"#;
        let settings: ParadoxSettings = serde_json::from_str(json).unwrap();

        assert_eq!(settings.game_library_paths.len(), 2);
        assert!(matches!(
            settings.game_library_paths[0],
            LibraryPathEntry::Root(ref s) if s == "../games"
        ));
        match &settings.game_library_paths[1] {
            LibraryPathEntry::Explicit(entry) => {
                assert_eq!(entry.game_id.as_deref(), Some("surviving_mars"));
                assert_eq!(
                    entry.installation_path.as_deref(),
                    Some(r"H:\Paradox\surviving_mars")
                );
            }
            other => panic!("expected an explicit entry, got {other:?}"),
        }
        assert!(settings.pending_game_installations.is_empty());
    }

    /// A future launcher version's third element shape (or outright garbage)
    /// must not fail the whole parse - it lands in `Other` and is ignored.
    #[test]
    fn unknown_element_shapes_are_ignored_not_fatal() {
        let json = settings_json(r#"42, null, [1, 2, 3], true, "../games""#);
        let settings: ParadoxSettings = serde_json::from_str(&json).unwrap();

        assert_eq!(settings.game_library_paths.len(), 5);
        assert!(matches!(
            settings.game_library_paths[4],
            LibraryPathEntry::Root(ref s) if s == "../games"
        ));
        for entry in &settings.game_library_paths[..4] {
            assert!(
                matches!(entry, LibraryPathEntry::Other(_)),
                "expected the garbage entries to fall through to Other, got {entry:?}"
            );
        }
    }

    #[test]
    fn has_content_besides_cpatch_rejects_a_cpatch_only_folder() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".cpatch")).unwrap();
        std::fs::write(temp.path().join(".cpatch").join("part.bin"), b"partial").unwrap();

        assert!(!has_content_besides_cpatch(temp.path()).unwrap());
    }

    /// The finding that changed since the card was written: `.cpatch` stays
    /// even after a real install finishes, so its *presence* proves nothing
    /// - only the presence of something else does.
    #[test]
    fn has_content_besides_cpatch_accepts_a_finished_install_that_still_carries_cpatch() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".cpatch")).unwrap();
        std::fs::write(temp.path().join("MarsPDX.exe"), b"MZ").unwrap();

        assert!(has_content_besides_cpatch(temp.path()).unwrap());
    }

    #[test]
    fn resolve_relative_root_collapses_parent_dir_components() {
        let launcher_dir =
            Path::new(r"C:\Users\Mancubus\AppData\Roaming\Paradox Interactive\launcher-v2");
        let resolved = resolve_relative_root(launcher_dir, "../games").unwrap();

        assert_eq!(
            resolved,
            PathBuf::from(r"C:\Users\Mancubus\AppData\Roaming\Paradox Interactive\games")
        );
    }

    #[test]
    fn resolve_relative_root_ignores_a_blank_string() {
        let launcher_dir = Path::new(r"C:\launcher-v2");
        assert!(resolve_relative_root(launcher_dir, "   ").is_none());
    }

    #[test]
    fn discover_paradox_at_reports_not_installed_when_settings_file_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let report = discover_paradox_at(temp.path());

        assert_eq!(report.status, DiscoveryStatus::NotInstalled);
        assert!(report.data.is_empty());
    }

    #[test]
    fn discover_paradox_at_fails_with_a_diagnostic_on_malformed_settings() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(SETTINGS_FILE_NAME), "{ not json").unwrap();

        let report = discover_paradox_at(temp.path());

        assert_eq!(report.status, DiscoveryStatus::Failed);
        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(report.diagnostics[0].stage, "settings-parse");
        assert!(report.data.is_empty());
    }

    /// End-to-end: a real install directory, named via `game-metadata`
    /// rather than by its raw `gameId` or folder name.
    #[test]
    fn discover_paradox_at_names_a_game_from_game_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("smars");
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::write(install_dir.join("MarsPDX.exe"), b"MZ").unwrap();

        std::fs::write(
            temp.path().join(SETTINGS_FILE_NAME),
            format!(
                r#"{{"gameLibraryPaths": [{{"gameId": "surviving_mars", "installationPath": {:?}}}]}}"#,
                install_dir.to_string_lossy()
            ),
        )
        .unwrap();
        std::fs::create_dir(temp.path().join("game-metadata")).unwrap();
        std::fs::write(
            temp.path().join(METADATA_RELATIVE_PATH),
            r#"{"data": {"games": [{"id": "surviving_mars", "name": "Surviving Mars"}]}}"#,
        )
        .unwrap();

        let report = discover_paradox_at(temp.path());

        assert_eq!(report.status, DiscoveryStatus::Complete);
        assert_eq!(report.data.len(), 1);
        assert_eq!(report.data[0].games.len(), 1);
        assert_eq!(report.data[0].games[0].name, "Surviving Mars");
        assert_eq!(
            report.data[0].orphan_evidence,
            OrphanEvidence::Authoritative
        );
    }

    /// The distinction the module doc comment insists on: `game-metadata`
    /// lists everything the account owns, not what is installed. A game
    /// present only in the metadata's ownership list - never in
    /// `gameLibraryPaths` - must not appear as a discovered game.
    #[test]
    fn ownership_list_is_not_an_install_list() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("smars");
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::write(install_dir.join("MarsPDX.exe"), b"MZ").unwrap();

        std::fs::write(
            temp.path().join(SETTINGS_FILE_NAME),
            format!(
                r#"{{"gameLibraryPaths": [{{"gameId": "surviving_mars", "installationPath": {:?}}}]}}"#,
                install_dir.to_string_lossy()
            ),
        )
        .unwrap();
        std::fs::create_dir(temp.path().join("game-metadata")).unwrap();
        // Owns three games; only one is actually installed.
        std::fs::write(
            temp.path().join(METADATA_RELATIVE_PATH),
            r#"{"data": {"games": [
                {"id": "surviving_mars", "name": "Surviving Mars"},
                {"id": "stellaris", "name": "Stellaris"},
                {"id": "crusader_kings_3", "name": "Crusader Kings III"}
            ]}}"#,
        )
        .unwrap();

        let report = discover_paradox_at(temp.path());

        let total_games: usize = report.data.iter().map(|library| library.games.len()).sum();
        assert_eq!(
            total_games, 1,
            "only the installed game may appear, never the two owned-but-not-installed ones"
        );
    }

    /// A `.cpatch`-only install directory must never be handed to the
    /// scanner as a game - the core guard this card exists for.
    #[test]
    fn a_cpatch_only_install_dir_is_excluded_without_degrading() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("stellaris");
        std::fs::create_dir_all(install_dir.join(".cpatch")).unwrap();
        std::fs::write(install_dir.join(".cpatch").join("part.bin"), b"partial").unwrap();

        std::fs::write(
            temp.path().join(SETTINGS_FILE_NAME),
            format!(
                r#"{{"gameLibraryPaths": [{{"gameId": "stellaris", "installationPath": {:?}}}]}}"#,
                install_dir.to_string_lossy()
            ),
        )
        .unwrap();

        let report = discover_paradox_at(temp.path());

        assert_eq!(report.status, DiscoveryStatus::Complete);
        assert!(
            report.data.iter().all(|library| library.games.is_empty()),
            "a folder holding only .cpatch must never be reported as an installed game"
        );
        assert_eq!(
            report
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.stage)
                .collect::<Vec<_>>(),
            vec![GAME_ABSENT],
            "the skipped in-progress download still has to leave a trace: {:?}",
            report.diagnostics
        );
    }

    /// The other in-flight signal: a gameId listed in
    /// `pendingGameInstallations` is excluded regardless of what its folder
    /// currently holds, and - like every other `GAME_ABSENT` case - without
    /// degrading the library.
    #[test]
    fn a_pending_game_installation_is_excluded_without_degrading() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("hoi4");
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::write(install_dir.join("hoi4.exe"), b"MZ").unwrap();

        std::fs::write(
            temp.path().join(SETTINGS_FILE_NAME),
            format!(
                r#"{{
    "gameLibraryPaths": [{{"gameId": "hoi4", "installationPath": {:?}}}],
    "pendingGameInstallations": ["hoi4"]
}}"#,
                install_dir.to_string_lossy()
            ),
        )
        .unwrap();

        let report = discover_paradox_at(temp.path());

        assert_eq!(report.status, DiscoveryStatus::Complete);
        assert!(report.data.iter().all(|library| library.games.is_empty()));
        assert_eq!(
            report
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.stage)
                .collect::<Vec<_>>(),
            vec![GAME_ABSENT]
        );
    }

    /// An unexaminable install directory - as opposed to one provably
    /// absent - must degrade the library: it may still hold a live game and
    /// would otherwise risk being misread as unmanaged.
    #[test]
    fn an_unexaminable_install_dir_degrades_the_library() {
        let temp = tempfile::tempdir().unwrap();
        let good_dir = temp.path().join("stellaris");
        std::fs::create_dir_all(&good_dir).unwrap();
        std::fs::write(good_dir.join("stellaris.exe"), b"MZ").unwrap();
        // `<` is invalid in a Windows path component, so the probe fails
        // with ERROR_INVALID_NAME rather than "not found" - a portable
        // stand-in for a DACL denial, offline placeholder, or drive not yet
        // spun up.
        let bad_dir = temp.path().join("bad<name");

        std::fs::write(
            temp.path().join(SETTINGS_FILE_NAME),
            format!(
                r#"{{"gameLibraryPaths": [
                    {{"gameId": "stellaris", "installationPath": {:?}}},
                    {{"gameId": "broken", "installationPath": {:?}}}
                ]}}"#,
                good_dir.to_string_lossy(),
                bad_dir.to_string_lossy()
            ),
        )
        .unwrap();

        let report = discover_paradox_at(temp.path());

        assert_eq!(report.status, DiscoveryStatus::Degraded);
        assert!(report
            .data
            .iter()
            .all(|library| library.orphan_evidence == OrphanEvidence::Degraded));
        assert!(report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.stage == "game-path"));
    }

    /// The default root ("../games") is registered as its own empty library
    /// when nothing else already covers it, exactly like Humble's download
    /// location and itch's install locations.
    #[test]
    fn discover_paradox_at_registers_the_default_root_when_empty() {
        let temp = tempfile::tempdir().unwrap();
        let games_root = temp.path().join("games");
        std::fs::create_dir_all(&games_root).unwrap();

        std::fs::write(
            temp.path().join(SETTINGS_FILE_NAME),
            r#"{"gameLibraryPaths": ["games"]}"#,
        )
        .unwrap();

        let report = discover_paradox_at(temp.path());

        assert_eq!(report.status, DiscoveryStatus::Complete);
        assert!(report
            .data
            .iter()
            .any(|library| library.path == games_root && library.games.is_empty()));
    }

    #[test]
    fn parse_game_names_reads_id_to_name_map() {
        let json = r#"{"data": {"games": [
            {"id": "surviving_mars", "name": "Surviving Mars"},
            {"id": "no_name"},
            {"name": "no_id"}
        ]}}"#;

        let names = parse_game_names(json);

        assert_eq!(names.len(), 1);
        assert_eq!(
            names.get("surviving_mars").map(String::as_str),
            Some("Surviving Mars")
        );
    }

    #[test]
    fn parse_game_names_returns_empty_on_garbage_input() {
        assert!(parse_game_names("not json").is_empty());
        assert!(parse_game_names("{}").is_empty());
    }

    #[test]
    fn build_candidate_requires_game_id_and_installation_path() {
        assert!(build_candidate(
            ParadoxGameEntry {
                game_id: None,
                installation_path: Some(r"H:\Paradox\x".to_string()),
            },
            &HashMap::new()
        )
        .is_none());
        assert!(build_candidate(
            ParadoxGameEntry {
                game_id: Some("x".to_string()),
                installation_path: None,
            },
            &HashMap::new()
        )
        .is_none());
    }

    #[test]
    fn build_candidate_falls_back_to_folder_name_when_no_metadata() {
        let candidate = build_candidate(
            ParadoxGameEntry {
                game_id: Some("surviving_mars".to_string()),
                installation_path: Some(r"H:\Paradox\surviving_mars".to_string()),
            },
            &HashMap::new(),
        )
        .unwrap();

        assert_eq!(candidate.name, "surviving_mars");
    }
}
