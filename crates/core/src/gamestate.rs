//! Per-game state between scans (game-state tracking): telling what changed since the last
//! scan *without* rescanning.
//!
//! The expensive part of a scan is enumerating files; detecting that a game
//! changed is not. Steam bumps a game's `buildid` on every content update (and
//! a `Verify` that re-downloads files), so comparing the `buildid` recorded at
//! the last scan against the one in the manifest right now answers "did this
//! game come back?" for the price of reading a few dozen small text files - see
//! `providers::steam::manifest_states`.
//!
//! This module holds the state comparison logic, launcher manifest parsers
//! (`SteamAppState`, `EpicItemState`, `GogGameState`), and database query helpers.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension};

use crate::error::Result;

/// What the last scan recorded about one game, read back from `games`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredGameState {
    pub game_id: i64,
    pub name: String,
    /// Vendor id (`games.app_id`). `None` for games discovered by folder scan,
    /// which therefore can never be matched to a manifest - see
    /// [`changed_games`].
    pub app_id: Option<String>,
    /// The `buildid` recorded at the last scan, or `None` for a game scanned
    /// before this tracking existed (or by a provider that has no build id).
    pub build_id: Option<String>,
}

/// Full game record from the `games` SQLite table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameRecord {
    pub id: i64,
    pub library_id: i64,
    pub name: String,
    pub install_dir: String,
    pub app_id: Option<String>,
    pub build_id: Option<String>,
}

/// Why a game is reported as changed since the last scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// The build id moved: the launcher updated (or re-verified) the game, so
    /// files removed by a previous trim are likely back.
    Updated,
    /// The game is gone from its launcher's manifests entirely - uninstalled
    /// since the last scan, so its recorded findings are stale.
    Uninstalled,
}

/// One game whose on-disk state no longer matches what the last scan recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedGame {
    pub game_id: i64,
    pub name: String,
    pub kind: ChangeKind,
    /// The build id the last scan recorded (`None` if it recorded none).
    pub previous_build_id: Option<String>,
    /// The build id in the manifest right now; `None` for
    /// [`ChangeKind::Uninstalled`].
    pub current_build_id: Option<String>,
}

/// Compares what the last scan recorded against the launcher's manifests right
/// now, returning only the games that actually changed.
pub fn changed_games(
    stored: &[StoredGameState],
    current: &HashMap<String, String>,
) -> Vec<ChangedGame> {
    stored
        .iter()
        .filter_map(|game| {
            let app_id = game.app_id.as_deref()?;
            // Never claim a change we cannot evidence (see doc comment).
            let previous = game.build_id.as_deref()?;

            match current.get(app_id) {
                Some(now) if now != previous => Some(ChangedGame {
                    game_id: game.game_id,
                    name: game.name.clone(),
                    kind: ChangeKind::Updated,
                    previous_build_id: Some(previous.to_string()),
                    current_build_id: Some(now.clone()),
                }),
                Some(_) => None,
                None => Some(ChangedGame {
                    game_id: game.game_id,
                    name: game.name.clone(),
                    kind: ChangeKind::Uninstalled,
                    previous_build_id: Some(previous.to_string()),
                    current_build_id: None,
                }),
            }
        })
        .collect()
}

/// Parsed Steam AppState from an `appmanifest_*.acf` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamAppState {
    pub appid: String,
    pub name: String,
    pub buildid: Option<String>,
    pub state_flags: u32,
    pub installdir: String,
}

impl SteamAppState {
    /// Parses the `AppState` block of a Steam `.acf` KeyValues VDF manifest.
    pub fn parse(acf: &str) -> Option<Self> {
        let root = crate::providers::steam::parse_vdf(acf);
        let crate::providers::steam::VdfValue::Obj(entries) = &root else {
            return None;
        };

        let app_state = entries.iter().find_map(|(key, val)| {
            if key.eq_ignore_ascii_case("AppState") {
                match val {
                    crate::providers::steam::VdfValue::Obj(fields) => Some(fields),
                    _ => None,
                }
            } else {
                None
            }
        })?;

        let get_field = |field: &str| -> Option<String> {
            app_state.iter().find_map(|(key, val)| {
                if key.eq_ignore_ascii_case(field) {
                    match val {
                        crate::providers::steam::VdfValue::Str(s) => Some(s.clone()),
                        _ => None,
                    }
                } else {
                    None
                }
            })
        };

        let appid = get_field("appid").filter(|s| !s.trim().is_empty())?;
        let name = get_field("name").unwrap_or_else(|| format!("Steam App {appid}"));
        let buildid = get_field("buildid").filter(|s| !s.trim().is_empty());
        let state_flags = get_field("StateFlags")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let installdir = get_field("installdir").unwrap_or_default();

        Some(Self {
            appid,
            name,
            buildid,
            state_flags,
            installdir,
        })
    }

    /// StateFlags == 4 indicates `StateFullyInstalled` (download and commit complete).
    pub fn is_fully_installed(&self) -> bool {
        self.state_flags == 4
    }
}

/// Parsed Epic Games Store item manifest (`Manifests/*.item`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct EpicItemState {
    #[serde(rename = "AppName")]
    pub app_name: String,
    #[serde(rename = "DisplayName")]
    pub display_name: String,
    #[serde(rename = "AppVersionString", default)]
    pub app_version_string: Option<String>,
    #[serde(rename = "bIsIncompleteInstall", default)]
    pub is_incomplete_install: bool,
    #[serde(rename = "InstallLocation")]
    pub install_location: String,
}

impl EpicItemState {
    /// Parses an Epic Games Store `.item` manifest JSON string.
    pub fn parse(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }

    /// Returns true if installation is complete and ready (`bIsIncompleteInstall == false`).
    pub fn is_ready(&self) -> bool {
        !self.is_incomplete_install
    }
}

/// Parsed GOG game manifest (`goggame-*.info`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct GogGameState {
    #[serde(rename = "gameId")]
    pub game_id: String,
    #[serde(rename = "rootGameId", default)]
    pub root_game_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(rename = "buildId", default)]
    pub build_id: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
}

impl GogGameState {
    /// Parses a `goggame-*.info` manifest JSON string.
    pub fn parse(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }

    /// Returns true if this manifest describes the base game rather than a DLC.
    pub fn is_base_game(&self) -> bool {
        match (&self.game_id, &self.root_game_id) {
            (id, Some(root)) => id == root,
            _ => true,
        }
    }
}

/// Finds a stored game in SQLite by its launcher app/vendor ID.
pub fn find_stored_game_by_app_id(
    conn: &Connection,
    app_id: &str,
) -> Result<Option<StoredGameState>> {
    conn.query_row(
        "SELECT id, name, app_id, build_id FROM games WHERE app_id = ?1 ORDER BY id DESC LIMIT 1",
        [app_id],
        |row| {
            Ok(StoredGameState {
                game_id: row.get(0)?,
                name: row.get(1)?,
                app_id: row.get(2)?,
                build_id: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// Finds a game record in SQLite by its launcher app/vendor ID.
pub fn find_game_by_app_id(
    conn: &Connection,
    app_id: &str,
) -> Result<Option<GameRecord>> {
    conn.query_row(
        "SELECT id, library_id, name, install_dir, app_id, build_id FROM games WHERE app_id = ?1 ORDER BY id DESC LIMIT 1",
        [app_id],
        |row| {
            Ok(GameRecord {
                id: row.get(0)?,
                library_id: row.get(1)?,
                name: row.get(2)?,
                install_dir: row.get(3)?,
                app_id: row.get(4)?,
                build_id: row.get(5)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// Finds a game record in SQLite by its primary key (`id`).
pub fn find_game_by_id(
    conn: &Connection,
    game_id: i64,
) -> Result<Option<GameRecord>> {
    conn.query_row(
        "SELECT id, library_id, name, install_dir, app_id, build_id FROM games WHERE id = ?1",
        [game_id],
        |row| {
            Ok(GameRecord {
                id: row.get(0)?,
                library_id: row.get(1)?,
                name: row.get(2)?,
                install_dir: row.get(3)?,
                app_id: row.get(4)?,
                build_id: row.get(5)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(
        game_id: i64,
        name: &str,
        app_id: Option<&str>,
        build: Option<&str>,
    ) -> StoredGameState {
        StoredGameState {
            game_id,
            name: name.to_string(),
            app_id: app_id.map(str::to_string),
            build_id: build.map(str::to_string),
        }
    }

    fn current(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn reports_a_game_whose_build_id_moved() {
        let stored_games = [stored(1, "Portal 2", Some("620"), Some("100"))];
        let changed = changed_games(&stored_games, &current(&[("620", "201")]));

        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].game_id, 1);
        assert_eq!(changed[0].kind, ChangeKind::Updated);
        assert_eq!(changed[0].previous_build_id.as_deref(), Some("100"));
        assert_eq!(changed[0].current_build_id.as_deref(), Some("201"));
    }

    #[test]
    fn ignores_an_unchanged_game() {
        let stored_games = [stored(1, "Portal 2", Some("620"), Some("100"))];
        assert!(changed_games(&stored_games, &current(&[("620", "100")])).is_empty());
    }

    #[test]
    fn never_reports_a_game_with_no_recorded_build_id() {
        let stored_games = [stored(1, "Portal 2", Some("620"), None)];
        assert!(
            changed_games(&stored_games, &current(&[("620", "999")])).is_empty(),
            "an unknown previous build id is not evidence of a change"
        );
    }

    #[test]
    fn never_reports_a_game_without_an_app_id() {
        let stored_games = [stored(1, "Repack Game", None, Some("100"))];
        assert!(changed_games(&stored_games, &current(&[("620", "100")])).is_empty());
    }

    #[test]
    fn reports_a_game_missing_from_the_manifests_as_uninstalled() {
        let stored_games = [stored(7, "Deleted Game", Some("440"), Some("55"))];
        let changed = changed_games(&stored_games, &current(&[("620", "100")]));

        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].kind, ChangeKind::Uninstalled);
        assert_eq!(changed[0].current_build_id, None);
    }

    #[test]
    fn parse_steam_app_state_fully_installed() {
        let acf = r#"
"AppState"
{
    "appid"       "730"
    "name"        "Counter-Strike 2"
    "buildid"     "12345678"
    "StateFlags"  "4"
    "installdir"  "Counter-Strike Global Offensive"
}
"#;
        let state = SteamAppState::parse(acf).expect("parse steam manifest");
        assert_eq!(state.appid, "730");
        assert_eq!(state.name, "Counter-Strike 2");
        assert_eq!(state.buildid.as_deref(), Some("12345678"));
        assert_eq!(state.state_flags, 4);
        assert_eq!(state.installdir, "Counter-Strike Global Offensive");
        assert!(state.is_fully_installed());
    }

    #[test]
    fn parse_steam_app_state_incomplete_download() {
        let acf = r#"
"AppState"
{
    "appid"       "730"
    "name"        "Counter-Strike 2"
    "buildid"     "12345678"
    "StateFlags"  "1026"
    "installdir"  "Counter-Strike Global Offensive"
}
"#;
        let state = SteamAppState::parse(acf).expect("parse steam manifest");
        assert_eq!(state.state_flags, 1026);
        assert!(!state.is_fully_installed());
    }

    #[test]
    fn parse_epic_item_state_ready_and_incomplete() {
        let json_ready = r#"
{
    "FormatVersion": 0,
    "bIsIncompleteInstall": false,
    "AppVersionString": "1.0.0",
    "AppName": "Fortnite",
    "DisplayName": "Fortnite",
    "InstallLocation": "F:\\Epic\\Fortnite"
}
"#;
        let ready_state = EpicItemState::parse(json_ready).expect("parse ready epic item");
        assert_eq!(ready_state.app_name, "Fortnite");
        assert_eq!(ready_state.display_name, "Fortnite");
        assert_eq!(ready_state.app_version_string.as_deref(), Some("1.0.0"));
        assert!(!ready_state.is_incomplete_install);
        assert!(ready_state.is_ready());
        assert_eq!(ready_state.install_location, "F:\\Epic\\Fortnite");

        let json_incomplete = r#"
{
    "bIsIncompleteInstall": true,
    "AppName": "Cyberpunk2077",
    "DisplayName": "Cyberpunk 2077",
    "InstallLocation": "F:\\Epic\\Cyberpunk2077"
}
"#;
        let incomplete_state = EpicItemState::parse(json_incomplete).expect("parse incomplete epic item");
        assert!(incomplete_state.is_incomplete_install);
        assert!(!incomplete_state.is_ready());
    }

    #[test]
    fn parse_gog_game_state_base_and_dlc() {
        let json_base = r#"
{
    "gameId": "1207658930",
    "rootGameId": "1207658930",
    "name": "The Witcher 3: Wild Hunt",
    "version": "4.04",
    "buildId": "56382168912345",
    "language": "English"
}
"#;
        let base_state = GogGameState::parse(json_base).expect("parse gog base game");
        assert_eq!(base_state.game_id, "1207658930");
        assert_eq!(base_state.name, "The Witcher 3: Wild Hunt");
        assert_eq!(base_state.version.as_deref(), Some("4.04"));
        assert_eq!(base_state.build_id.as_deref(), Some("56382168912345"));
        assert!(base_state.is_base_game());

        let json_dlc = r#"
{
    "gameId": "1440151285",
    "rootGameId": "1207658930",
    "name": "The Witcher 3: Blood and Wine"
}
"#;
        let dlc_state = GogGameState::parse(json_dlc).expect("parse gog dlc");
        assert!(!dlc_state.is_base_game());
    }

    #[test]
    fn db_find_stored_game_and_record_by_app_id() {
        let conn = crate::db::open_in_memory().expect("open memory db");
        conn.execute(
            "INSERT INTO game_libraries (id, vendor, path) VALUES (1, 'steam', 'C:\\SteamLibrary')",
            [],
        )
        .expect("insert library");
        conn.execute(
            "INSERT INTO games (id, library_id, name, install_dir, app_id, build_id)
             VALUES (42, 1, 'Portal 2', 'C:\\SteamLibrary\\steamapps\\common\\Portal 2', '620', '8888')",
            [],
        )
        .expect("insert game");

        let stored = find_stored_game_by_app_id(&conn, "620").expect("query stored game");
        assert!(stored.is_some());
        let stored = stored.unwrap();
        assert_eq!(stored.game_id, 42);
        assert_eq!(stored.name, "Portal 2");
        assert_eq!(stored.app_id.as_deref(), Some("620"));
        assert_eq!(stored.build_id.as_deref(), Some("8888"));

        let record = find_game_by_app_id(&conn, "620").expect("query game record");
        assert!(record.is_some());
        let record = record.unwrap();
        assert_eq!(record.id, 42);
        assert_eq!(record.library_id, 1);
        assert_eq!(record.name, "Portal 2");
        assert_eq!(record.install_dir, "C:\\SteamLibrary\\steamapps\\common\\Portal 2");

        let by_id = find_game_by_id(&conn, 42).expect("query by id");
        assert_eq!(by_id, Some(record));

        let not_found = find_stored_game_by_app_id(&conn, "99999").expect("query non-existent");
        assert!(not_found.is_none());
    }
}
