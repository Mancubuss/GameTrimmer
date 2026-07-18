//! itch.io app library discovery: SQLite database `%APPDATA%\itch\db\butler.db`.
//!
//! Installed games ("caves" in butler's vocabulary) are joined with their
//! titles and install locations:
//!   - `caves(game_id, install_location_id, install_folder_name)`
//!   - `games(id, title)`
//!   - `install_locations(id, path)`
//!
//! The install directory is `install_locations.path\install_folder_name`.
//! The database is opened read-only.

use std::path::PathBuf;

use rusqlite::{Connection, OpenFlags};

use crate::error::Result;

use super::{DiscoveredLibrary, GameInstall, LibraryProvider};

const DATABASE_RELATIVE_PATH: &str = r"itch\db\butler.db";

pub struct ItchProvider;

impl LibraryProvider for ItchProvider {
    fn name(&self) -> &'static str {
        "itch"
    }

    fn discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        let Some(db_path) = database_path().filter(|path| path.is_file()) else {
            // itch app not installed - not an error.
            return Ok(Vec::new());
        };

        let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let games = read_games(&conn)?
            .into_iter()
            .filter(|game| game.install_dir.is_dir())
            .collect();

        Ok(super::group_by_parent_dir("itch", games))
    }
}

fn database_path() -> Option<PathBuf> {
    let app_data = std::env::var("APPDATA").ok()?;
    Some(PathBuf::from(app_data).join(DATABASE_RELATIVE_PATH))
}

/// Reads installed games from an open `butler.db` connection.
fn read_games(conn: &Connection) -> rusqlite::Result<Vec<GameInstall>> {
    let mut stmt = conn.prepare(
        "SELECT games.title, caves.game_id, install_locations.path, caves.install_folder_name
         FROM caves
         JOIN games ON games.id = caves.game_id
         JOIN install_locations ON install_locations.id = caves.install_location_id",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(RawItchEntry {
            title: row.get(0)?,
            game_id: row.get(1)?,
            location_path: row.get(2)?,
            install_folder_name: row.get(3)?,
        })
    })?;

    Ok(rows.flatten().filter_map(build_game_install).collect())
}

/// One raw joined row (or a synthetic stand-in in tests).
struct RawItchEntry {
    title: Option<String>,
    game_id: i64,
    location_path: Option<String>,
    install_folder_name: Option<String>,
}

/// Builds a `GameInstall` from a raw joined row. Location path and folder
/// name are both required; the title falls back to the folder name.
fn build_game_install(entry: RawItchEntry) -> Option<GameInstall> {
    let location_path = entry.location_path.filter(|s| !s.trim().is_empty())?;
    let folder_name = entry.install_folder_name.filter(|s| !s.trim().is_empty())?;

    let name = entry
        .title
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| folder_name.clone());

    Some(GameInstall {
        name,
        install_dir: PathBuf::from(location_path).join(folder_name),
        app_id: Some(entry.game_id.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory replica of the subset of butler.db's schema we read.
    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE games (id INTEGER PRIMARY KEY, title TEXT);
             CREATE TABLE install_locations (id TEXT PRIMARY KEY, path TEXT);
             CREATE TABLE caves (
                 id TEXT PRIMARY KEY,
                 game_id INTEGER,
                 install_location_id TEXT,
                 install_folder_name TEXT
             );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn read_games_joins_titles_and_install_locations() {
        let conn = test_db();
        conn.execute_batch(
            "INSERT INTO games VALUES (123, 'Celeste');
             INSERT INTO install_locations VALUES ('loc-1', 'F:\\itch');
             INSERT INTO caves VALUES ('cave-1', 123, 'loc-1', 'celeste');",
        )
        .unwrap();

        let games = read_games(&conn).unwrap();

        assert_eq!(games.len(), 1);
        assert_eq!(games[0].name, "Celeste");
        assert_eq!(games[0].app_id.as_deref(), Some("123"));
        assert_eq!(games[0].install_dir, PathBuf::from(r"F:\itch\celeste"));
    }

    #[test]
    fn read_games_skips_caves_without_matching_location() {
        let conn = test_db();
        conn.execute_batch(
            "INSERT INTO games VALUES (123, 'Celeste');
             INSERT INTO caves VALUES ('cave-1', 123, 'missing-loc', 'celeste');",
        )
        .unwrap();

        assert!(read_games(&conn).unwrap().is_empty());
    }

    #[test]
    fn build_game_install_falls_back_to_folder_name_when_title_missing() {
        let game = build_game_install(RawItchEntry {
            title: None,
            game_id: 7,
            location_path: Some(r"F:\itch".to_string()),
            install_folder_name: Some("celeste".to_string()),
        })
        .expect("expected a parsed game");

        assert_eq!(game.name, "celeste");
    }

    #[test]
    fn build_game_install_requires_folder_name() {
        assert!(build_game_install(RawItchEntry {
            title: Some("Celeste".to_string()),
            game_id: 7,
            location_path: Some(r"F:\itch".to_string()),
            install_folder_name: None,
        })
        .is_none());
    }
}
