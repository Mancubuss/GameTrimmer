//! Humble App library discovery: `%APPDATA%\Humble App\config.json`,
//! array `game-collection-4` with per-game `status` / `filePath` /
//! `gameName` / `machineName` fields.
//!
//! The config also names `settings.downloadLocation`, and that root is
//! registered even when no game is installed under it. Humble owns a large
//! catalogue the user has not necessarily downloaded any of - every entry sits
//! at status `available` until it is - so "Humble App present, nothing
//! installed" is the ordinary state, and reporting nothing at all makes it
//! indistinguishable from a broken provider (see `super::register_root`).

use std::path::PathBuf;

use serde::Deserialize;

use crate::error::Result;

use super::{
    degrades_evidence, DiscoveredLibrary, DiscoveryDiagnostic, DiscoveryReport, DiscoveryStatus,
    GameInstall, LibraryProvider, OrphanEvidence, GAME_ABSENT,
};

const CONFIG_RELATIVE_PATH: &str = r"Humble App\config.json";

/// Statuses the Humble App uses for games present on disk.
const INSTALLED_STATUSES: &[&str] = &["installed", "downloaded"];

pub struct HumbleProvider;

impl LibraryProvider for HumbleProvider {
    fn name(&self) -> &'static str {
        "humble"
    }

    fn try_discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        Ok(discover_humble().data)
    }

    fn discover(&self) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
        discover_humble()
    }
}

fn diagnostic(
    stage: &'static str,
    path: Option<PathBuf>,
    message: impl std::fmt::Display,
) -> DiscoveryDiagnostic {
    DiscoveryDiagnostic {
        provider: "humble",
        stage,
        path,
        message: message.to_string(),
    }
}

fn discover_humble() -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let Some(config_path) = config_path().filter(|path| path.is_file()) else {
        return DiscoveryReport::not_installed(Vec::new());
    };

    let contents = match std::fs::read_to_string(&config_path) {
        Ok(contents) => contents,
        Err(err) => {
            return DiscoveryReport::failed(
                Vec::new(),
                diagnostic("config-read", Some(config_path), err),
            )
        }
    };
    let config = match parse_config_report(&contents) {
        Ok(config) => config,
        Err(err) => {
            return DiscoveryReport::failed(
                Vec::new(),
                diagnostic("config-parse", Some(config_path), err),
            )
        }
    };
    discover_humble_from_config(config)
}

/// The testable core of Humble discovery: everything past a parsed config.
/// Split out so tests can drive it directly instead of round-tripping through
/// the real `%APPDATA%\Humble App\config.json`.
fn discover_humble_from_config(config: ParsedConfig) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let mut diagnostics = Vec::new();
    let mut games = Vec::new();
    for game in config.games {
        // A configured install whose directory is simply not there is
        // normal - uninstalled outside the Humble App, or the config entry
        // is stale - and an absent folder cannot be mistaken for orphan
        // residue. A folder we merely failed to examine is the dangerous
        // case: it stays on disk, drops out of `games`, and would look
        // unmanaged. Diagnose it instead of collapsing both into one
        // `game-path` stage.
        match super::try_is_dir(&game.install_dir) {
            Ok(true) => games.push(game),
            // Recorded, but explicitly not degrading - see `GAME_ABSENT`.
            Ok(false) => diagnostics.push(diagnostic(
                GAME_ABSENT,
                Some(game.install_dir),
                "config entry present, install directory absent (uninstalled outside the Humble App, or a stale entry)",
            )),
            Err(err) => diagnostics.push(diagnostic("game-path", Some(game.install_dir), err)),
        }
    }

    let mut libraries = super::group_by_parent_dir("humble", games);
    if let Some(root) = config.download_location {
        // Left as a plain `is_dir()` on purpose - see the ticket report for
        // the reasoning. In short: `register_root` only ever fires when no
        // library already covers this path, i.e. when `group_by_parent_dir`
        // found no games under it, so a false "absent" here cannot strip a
        // live installation out of the managed set the way the per-game
        // check above can. This stage is also explicitly out of scope.
        if root.is_dir() {
            super::register_root(&mut libraries, "humble", root);
        } else {
            diagnostics.push(diagnostic(
                "download-location",
                Some(root),
                "configured Humble download location is unavailable",
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
        // behaviour this card exists to change.
        DiscoveryReport {
            data: libraries,
            status: DiscoveryStatus::Complete,
            diagnostics,
        }
    }
}

fn config_path() -> Option<PathBuf> {
    let app_data = std::env::var("APPDATA").ok()?;
    Some(PathBuf::from(app_data).join(CONFIG_RELATIVE_PATH))
}

/// The subset of the Humble App config we care about.
#[derive(Debug, Deserialize)]
struct HumbleConfig {
    #[serde(default)]
    settings: HumbleSettings,
    #[serde(rename = "game-collection-4", default)]
    game_collection: Vec<HumbleGame>,
}

#[derive(Debug, Default, Deserialize)]
struct HumbleSettings {
    #[serde(rename = "downloadLocation")]
    download_location: Option<String>,
}

/// What one parsed config yields: where Humble downloads to (if it says), and
/// the games it reports as present on disk.
#[derive(Debug, Default)]
struct ParsedConfig {
    download_location: Option<PathBuf>,
    games: Vec<GameInstall>,
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

/// Parses the config JSON into the download location and installed games it
/// describes. Malformed JSON yields the empty result - the config is
/// launcher-owned state, not user input worth surfacing an error for.
#[cfg(test)]
fn parse_config(json: &str) -> ParsedConfig {
    parse_config_report(json).unwrap_or_default()
}

fn parse_config_report(json: &str) -> serde_json::Result<ParsedConfig> {
    let config = serde_json::from_str::<HumbleConfig>(json)?;
    // An entry that decodes fine but yields no `GameInstall` (no path, or a
    // status other than installed/downloaded) is Humble's normal state - the
    // catalogue lists everything the account owns, not just what is on disk.
    // There is no per-entry decode failure to distinguish it from here: a
    // genuinely malformed entry fails the whole-document parse above via
    // `?` and is reported as `config-parse`, distinctly. So a missing path
    // is dropped silently rather than counted toward a diagnostic.
    let games = config
        .game_collection
        .into_iter()
        .filter_map(build_game_install)
        .collect();

    Ok(ParsedConfig {
        download_location: config
            .settings
            .download_location
            .filter(|path| !path.trim().is_empty())
            .map(|path| PathBuf::from(path.trim().trim_end_matches(['\\', '/']))),
        games,
    })
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

        let config = parse_config(json);
        let games = config.games;

        assert_eq!(config.download_location, Some(PathBuf::from(r"F:\Humble")));
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
        assert_eq!(parse_config(json).games.len(), 1);
    }

    /// The state on a machine with the Humble App installed and nothing
    /// downloaded yet: every catalogue entry sits at `available`, so there are
    /// no games - but the download location is known, and that is what makes
    /// "installed, empty" reportable instead of silent.
    #[test]
    fn parse_config_reads_the_download_location_with_no_installed_games() {
        let json = r#"
{
    "settings": { "downloadLocation": "H:\\Humble" },
    "game-collection-4": [
        { "status": "available", "gameName": "Owned But Not Installed" }
    ]
}
"#;

        let config = parse_config(json);

        assert_eq!(config.download_location, Some(PathBuf::from(r"H:\Humble")));
        assert!(config.games.is_empty());
    }

    #[test]
    fn parse_config_ignores_a_blank_download_location() {
        let json = r#"{ "settings": { "downloadLocation": "   " } }"#;
        assert!(parse_config(json).download_location.is_none());
    }

    #[test]
    fn parse_config_trims_a_trailing_separator_from_the_download_location() {
        let json = r#"{ "settings": { "downloadLocation": "H:\\Humble\\" } }"#;
        assert_eq!(
            parse_config(json).download_location,
            Some(PathBuf::from(r"H:\Humble"))
        );
    }

    #[test]
    fn parse_config_returns_empty_for_garbage_input() {
        assert!(parse_config("not json").games.is_empty());
        assert!(parse_config("not json").download_location.is_none());
        assert!(parse_config("{}").games.is_empty());
        assert!(parse_config("{}").download_location.is_none());
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

    /// An "installed" row with no `filePath` decodes fine - it is just
    /// missing a path - and must vanish from `parse_config_report` silently,
    /// with no diagnostic left for it to raise later.
    #[test]
    fn parse_config_drops_an_installed_row_with_no_path_silently() {
        let json = r#"
{
    "game-collection-4": [
        { "status": "installed", "gameName": "Broken", "machineName": "broken_game" }
    ]
}
"#;
        assert!(parse_config(json).games.is_empty());
    }

    /// The regression this slice exists to prevent: an installed row with no
    /// usable path used to add a `game-entry` diagnostic, and any diagnostic
    /// at all flips every library from `Authoritative` to `Degraded`
    /// (`discover_humble_from_config`). One ordinary "nothing installed here"
    /// row must not disable orphan detection for a library that has a
    /// perfectly good other game in it.
    #[test]
    fn an_installed_row_with_no_path_does_not_degrade_the_library() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("FTL");
        std::fs::create_dir(&install_dir).unwrap();

        let json = format!(
            r#"{{
    "game-collection-4": [
        {{ "status": "installed", "gameName": "FTL", "machineName": "ftl_game", "filePath": {:?} }},
        {{ "status": "installed", "gameName": "Broken", "machineName": "broken_game" }}
    ]
}}"#,
            install_dir.to_string_lossy()
        );

        let config = parse_config_report(&json).unwrap();
        let report = discover_humble_from_config(config);

        assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
        assert_eq!(report.data.len(), 1);
        assert_eq!(
            report.data[0].orphan_evidence,
            OrphanEvidence::Authoritative
        );
        assert_eq!(report.data[0].games.len(), 1);
        assert_eq!(report.data[0].games[0].name, "FTL");
    }

    /// A configured install whose directory is provably absent - uninstalled
    /// outside the Humble App, or a stale config entry - must not degrade
    /// the library: an absent folder can never be mistaken for orphan
    /// residue.
    #[test]
    fn a_game_whose_install_dir_is_absent_keeps_the_library_authoritative() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("FTL");
        std::fs::create_dir(&install_dir).unwrap();
        let absent_dir = temp.path().join("Never Downloaded");

        let json = format!(
            r#"{{
    "game-collection-4": [
        {{ "status": "installed", "gameName": "FTL", "machineName": "ftl_game", "filePath": {:?} }},
        {{ "status": "installed", "gameName": "Never Downloaded", "machineName": "nd_game", "filePath": {:?} }}
    ]
}}"#,
            install_dir.to_string_lossy(),
            absent_dir.to_string_lossy()
        );

        let config = parse_config_report(&json).unwrap();
        let report = discover_humble_from_config(config);

        assert_eq!(report.status, crate::providers::DiscoveryStatus::Complete);
        assert_eq!(report.data.len(), 1);
        assert_eq!(
            report.data[0].orphan_evidence,
            OrphanEvidence::Authoritative
        );
        assert_eq!(report.data[0].games.len(), 1);
        assert_eq!(
            report
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.stage)
                .collect::<Vec<_>>(),
            vec![GAME_ABSENT],
            "the absent install must still leave a trace: {:?}",
            report.diagnostics
        );
    }

    /// The dangerous counterpart: an install directory that cannot be
    /// examined - as opposed to one that is provably absent - must degrade
    /// the library, because it may still be sitting on disk and would
    /// otherwise be misread as orphan residue.
    #[test]
    fn a_game_with_an_unexaminable_install_dir_degrades_the_library() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("FTL");
        std::fs::create_dir(&install_dir).unwrap();
        // `<` is invalid in a Windows path component, so the probe fails with
        // ERROR_INVALID_NAME rather than "not found" - a portable stand-in
        // for a DACL denial, offline placeholder, or drive not yet spun up.
        let unexaminable = temp.path().join("bad<name");

        let json = format!(
            r#"{{
    "game-collection-4": [
        {{ "status": "installed", "gameName": "FTL", "machineName": "ftl_game", "filePath": {:?} }},
        {{ "status": "installed", "gameName": "Broken", "machineName": "broken_game", "filePath": {:?} }}
    ]
}}"#,
            install_dir.to_string_lossy(),
            unexaminable.to_string_lossy()
        );

        let config = parse_config_report(&json).unwrap();
        let report = discover_humble_from_config(config);

        assert_eq!(report.status, crate::providers::DiscoveryStatus::Degraded);
        assert_eq!(report.data.len(), 1);
        assert_eq!(report.data[0].orphan_evidence, OrphanEvidence::Degraded);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.stage == "game-path"),
            "the failed probe must be visible, not silently dropped: {:?}",
            report.diagnostics
        );
    }
}
