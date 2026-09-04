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
use std::path::Path;

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

/// Build ids for one library right now, keyed by the app id its provider
/// reports - the right-hand side of [`changed_games`].
///
/// The single place that knows which launcher publishes a content version and
/// where. Both callers need the identical answer for opposite reasons: the
/// scan records it so a later run has something to compare against, and the
/// startup check compares against what the last scan recorded. Two copies of
/// this list would diverge exactly once - the day a launcher is added to one
/// and not the other - and the symptom would be a banner that reports every
/// game of that launcher as changed on every start, because "the value moved"
/// and "nobody read the value" are indistinguishable downstream.
///
/// Steam and GOG answer per library root. The other four keep one
/// machine-wide record each and ignore `library_root` entirely; the caller
/// looks the result up by its own games' app ids, so a wider answer cannot
/// put a foreign game's build id on a local row.
///
/// A vendor that publishes nothing yields an empty map rather than an error.
pub fn current_build_ids(vendor: &str, library_root: &Path) -> Result<HashMap<String, String>> {
    use crate::providers;

    match vendor {
        "steam" => Ok(providers::steam::manifest_states(library_root)?
            .into_iter()
            .filter_map(|state| Some((state.app_id, state.build_id?)))
            .collect()),
        "epic" => providers::epic::build_ids(),
        "gog" => providers::folderscan::gog_build_ids(library_root),
        "amazon" => providers::amazon::build_ids(),
        "itch" => providers::itch::build_ids(),
        "humble" => providers::humble::build_ids(),
        _ => Ok(HashMap::new()),
    }
}

/// What a previous trim took out of one game and the filesystem has since put
/// back: the files this app deleted whose paths hold a file again.
///
/// Returns the count and the sum of their sizes *as they are on disk now*,
/// not as they were when they were deleted. The question the number answers
/// is "how much space is occupied again", and the launcher is free to have
/// re-downloaded a differently-sized file under the same path.
///
/// Only deletions made since the last scan can be counted, and that is the
/// right window rather than a limitation: a scan replaces a game's `files`
/// rows, so the journal entries pointing at the previous generation stop
/// joining - which is exactly when their subject stopped being current.
///
/// A game with no deletion history returns `(0, 0)`. That is a real answer,
/// not a missing one: nothing was removed, so nothing can have come back.
pub fn returned_since_trim(conn: &Connection, game_id: i64) -> Result<(usize, u64)> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT o.src_path \
         FROM operations o \
         JOIN files f ON f.id = o.file_id \
         WHERE o.status = 'done' AND f.game_id = ?1",
    )?;
    let paths = stmt.query_map([game_id], |row| row.get::<_, String>(0))?;

    let mut files = 0usize;
    let mut bytes = 0u64;
    for path in paths {
        // A path that is not there is the ordinary case - the deletion still
        // holds. Anything that cannot be examined at all (a permission error,
        // a drive that went away) is counted as "not back" for the same
        // reason: this figure exists to tell the user what their trim lost,
        // and inventing a loss out of an unreadable path is the one failure
        // that would make the banner untrustworthy.
        if let Ok(meta) = std::fs::metadata(path?) {
            if meta.is_file() {
                files += 1;
                bytes += meta.len();
            }
        }
    }

    Ok((files, bytes))
}

/// One updated game, with what its update put back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnedGame {
    pub game_id: i64,
    pub name: String,
    /// Files a previous trim deleted that are on disk again.
    pub files: usize,
    /// Their combined size right now, in bytes.
    pub bytes: u64,
}

/// Everything the app can say at startup about what happened while it was
/// closed, for the price of reading each launcher's own small records.
///
/// Deliberately reports only games whose build id *moved*. `changed_games`
/// also reports games that vanished from their launcher's manifests, and
/// those matter - their stored findings are stale - but they are a different
/// sentence to a different question ("this game is gone" rather than "your
/// trim was undone"), and folding both into one count would produce a banner
/// that cannot be acted on.
///
/// That filter is also the whole defence against the worst failure available
/// here. A launcher that answers with nothing - an external drive unplugged,
/// a renamed library, a config the app cannot read this morning - makes every
/// one of its games look "missing from the manifests", and a report that
/// counted those would announce that two hundred games were uninstalled
/// overnight. Dropping them means "no answer" produces silence, which is the
/// only honest thing it can produce. Anything that later wants to report
/// uninstalls has to solve "no" versus "no answer" first, and it is not
/// solved here.
pub fn returned_since_last_scan(conn: &Connection) -> Result<Vec<ReturnedGame>> {
    let mut libraries = conn.prepare(
        "SELECT id, vendor, path FROM game_libraries \
         WHERE id IN (SELECT DISTINCT library_id FROM games \
                      WHERE scan_id = (SELECT active_scan_id FROM scan_state WHERE singleton = 1))",
    )?;
    let libraries: Vec<(i64, String, String)> = libraries
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<std::result::Result<_, _>>()?;

    let mut updated = Vec::new();
    for (library_id, vendor, path) in libraries {
        // A launcher that cannot be read is not evidence of anything, and
        // this whole report is only ever additive information - so a failure
        // costs this library's line and nothing else. Empty is not special-
        // cased: it flows through `changed_games` and comes out as
        // uninstalls, which the filter below drops for exactly this reason.
        let Ok(current) = current_build_ids(&vendor, Path::new(&path)) else {
            continue;
        };

        let mut stored = conn.prepare(
            "SELECT id, name, app_id, build_id FROM games \
             WHERE library_id = ?1 \
               AND scan_id = (SELECT active_scan_id FROM scan_state WHERE singleton = 1)",
        )?;
        let stored: Vec<StoredGameState> = stored
            .query_map([library_id], |row| {
                Ok(StoredGameState {
                    game_id: row.get(0)?,
                    name: row.get(1)?,
                    app_id: row.get(2)?,
                    build_id: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;

        for change in changed_games(&stored, &current) {
            if change.kind != ChangeKind::Updated {
                continue;
            }
            let (files, bytes) = returned_since_trim(conn, change.game_id)?;
            updated.push(ReturnedGame {
                game_id: change.game_id,
                name: change.name,
                files,
                bytes,
            });
        }
    }

    Ok(updated)
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
pub fn find_game_by_app_id(conn: &Connection, app_id: &str) -> Result<Option<GameRecord>> {
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
pub fn find_game_by_id(conn: &Connection, game_id: i64) -> Result<Option<GameRecord>> {
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
        let incomplete_state =
            EpicItemState::parse(json_incomplete).expect("parse incomplete epic item");
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

    /// A database holding one GOG library, one game in it, and an active scan
    /// generation - the shape both startup checks read. `library_root` is a
    /// real directory so `current_build_ids` can answer from actual
    /// `goggame-*.info` files rather than from a mock, which is the whole
    /// point: the map's keys have to match what discovery calls `app_id`, and
    /// a mock is exactly the thing that cannot prove that.
    fn gog_fixture(library_root: &std::path::Path, stored_build_id: Option<&str>) -> Connection {
        let conn = crate::db::open_in_memory().expect("open memory db");
        let scan_id = crate::db::begin_scan(&conn, "complete").expect("begin scan");
        // `apply_schema` already seeds the singleton row, so this activates
        // the generation rather than creating the row.
        conn.execute(
            "INSERT INTO scan_state (singleton, active_scan_id) VALUES (1, ?1)
             ON CONFLICT(singleton) DO UPDATE SET active_scan_id = excluded.active_scan_id",
            [scan_id],
        )
        .expect("activate scan");
        conn.execute(
            "INSERT INTO game_libraries (id, vendor, path) VALUES (1, 'gog', ?1)",
            [library_root.to_string_lossy()],
        )
        .expect("insert library");
        conn.execute(
            "INSERT INTO games (id, scan_id, library_id, name, install_dir, app_id, build_id)
             VALUES (7, ?1, 1, 'A.D. 2044', ?2, '2075976504', ?3)",
            rusqlite::params![
                scan_id,
                library_root.join("AD2044").to_string_lossy(),
                stored_build_id,
            ],
        )
        .expect("insert game");
        conn
    }

    fn write_gog_manifest(library_root: &std::path::Path, build_id: &str) {
        let dir = library_root.join("AD2044");
        std::fs::create_dir_all(&dir).expect("create game dir");
        std::fs::write(
            dir.join("goggame-2075976504.info"),
            format!(
                r#"{{"gameId":"2075976504","rootGameId":"2075976504","name":"A.D. 2044","buildId":"{build_id}"}}"#
            ),
        )
        .expect("write manifest");
    }

    /// Records a finished deletion of `path` for game 7, the way `ops` does.
    fn record_deletion(conn: &Connection, file_id: i64, path: &std::path::Path, size: i64) {
        conn.execute(
            "INSERT INTO files (id, scan_id, game_id, rel_path, size)
             VALUES (?1, (SELECT active_scan_id FROM scan_state WHERE singleton = 1), 7, ?2, ?3)",
            rusqlite::params![file_id, path.to_string_lossy(), size],
        )
        .expect("insert file");
        conn.execute(
            "INSERT INTO operations (ts, action, src_path, status, file_id)
             VALUES (0, 'delete', ?1, 'done', ?2)",
            rusqlite::params![path.to_string_lossy(), file_id],
        )
        .expect("insert operation");
    }

    #[test]
    fn returned_since_trim_counts_only_the_deleted_files_that_are_back() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        let conn = gog_fixture(root, Some("100"));

        let back = root.join("came_back.pak");
        std::fs::write(&back, vec![0u8; 1234]).expect("write returned file");
        record_deletion(&conn, 1, &back, 1234);

        // Still deleted: the trim held, so it must not be counted.
        record_deletion(&conn, 2, &root.join("still_gone.pak"), 5678);

        let (files, bytes) = returned_since_trim(&conn, 7).expect("query returned files");
        assert_eq!(files, 1);
        assert_eq!(bytes, 1234);
    }

    #[test]
    fn returned_since_trim_measures_the_file_that_is_there_now() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        let conn = gog_fixture(root, Some("100"));

        // The launcher re-downloaded a bigger file under the same path. The
        // honest answer to "how much is occupied again" is the size on disk
        // now, not the size the deletion freed.
        let back = root.join("came_back.pak");
        std::fs::write(&back, vec![0u8; 4096]).expect("write returned file");
        record_deletion(&conn, 1, &back, 1024);

        let (files, bytes) = returned_since_trim(&conn, 7).expect("query returned files");
        assert_eq!(files, 1);
        assert_eq!(bytes, 4096);
    }

    #[test]
    fn returned_since_last_scan_reports_a_game_whose_launcher_moved_on() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        write_gog_manifest(root, "200");
        let conn = gog_fixture(root, Some("100"));

        let back = root.join("AD2044").join("came_back.pak");
        std::fs::write(&back, vec![0u8; 2048]).expect("write returned file");
        record_deletion(&conn, 1, &back, 2048);

        let returned = returned_since_last_scan(&conn).expect("query changes");
        assert_eq!(returned.len(), 1);
        assert_eq!(returned[0].game_id, 7);
        assert_eq!(returned[0].name, "A.D. 2044");
        assert_eq!(returned[0].files, 1);
        assert_eq!(returned[0].bytes, 2048);
    }

    #[test]
    fn returned_since_last_scan_says_nothing_when_the_build_id_did_not_move() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        write_gog_manifest(root, "200");
        let conn = gog_fixture(root, Some("200"));

        assert!(
            returned_since_last_scan(&conn)
                .expect("query changes")
                .is_empty(),
            "an unchanged build id is not an update"
        );
    }

    #[test]
    fn returned_since_last_scan_stays_silent_when_the_library_answers_with_nothing() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        // No manifest written at all: the drive is there but the launcher's
        // records are not, which is what an unplugged or renamed library
        // looks like. This pins the user-visible guarantee, not the line that
        // happens to enforce it today - announcing that every game on the
        // drive vanished overnight is the failure, wherever it gets stopped.
        let conn = gog_fixture(root, Some("100"));

        assert!(
            returned_since_last_scan(&conn)
                .expect("query changes")
                .is_empty(),
            "an empty answer from a launcher is not evidence of a change"
        );
    }

    #[test]
    fn returned_since_last_scan_reports_an_updated_game_with_no_trim_history() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        write_gog_manifest(root, "200");
        let conn = gog_fixture(root, Some("100"));

        // Nothing was ever deleted from this game, so nothing came back. The
        // update is still worth reporting - it is the size that is absent,
        // not the event.
        let returned = returned_since_last_scan(&conn).expect("query changes");
        assert_eq!(returned.len(), 1);
        assert_eq!(returned[0].files, 0);
        assert_eq!(returned[0].bytes, 0);
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
        assert_eq!(
            record.install_dir,
            "C:\\SteamLibrary\\steamapps\\common\\Portal 2"
        );

        let by_id = find_game_by_id(&conn, 42).expect("query by id");
        assert_eq!(by_id, Some(record));

        let not_found = find_stored_game_by_app_id(&conn, "99999").expect("query non-existent");
        assert!(not_found.is_none());
    }
}
