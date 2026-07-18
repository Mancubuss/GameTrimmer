//! Amazon Games library discovery: SQLite database
//! `%LOCALAPPDATA%\Amazon Games\Data\Games\Sql\GameInstallInfo.sqlite`,
//! table `DbSet` (`Id`, `ProductTitle`, `InstallDirectory`, `Installed`).
//!
//! The database is opened read-only; the running launcher may hold locks, in
//! which case discovery fails with a warning rather than corrupting anything.

use std::path::PathBuf;

use rusqlite::{Connection, OpenFlags};

use crate::error::Result;

use super::{DiscoveredLibrary, GameInstall, LibraryProvider};

const DATABASE_RELATIVE_PATH: &str = r"Amazon Games\Data\Games\Sql\GameInstallInfo.sqlite";

pub struct AmazonProvider;

impl LibraryProvider for AmazonProvider {
    fn name(&self) -> &'static str {
        "amazon"
    }

    fn discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        let Some(db_path) = database_path().filter(|path| path.is_file()) else {
            // Amazon Games not installed - not an error.
            return Ok(Vec::new());
        };

        let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let games = read_games(&conn)?
            .into_iter()
            .filter(|game| game.install_dir.is_dir())
            .collect();

        Ok(super::group_by_parent_dir("amazon", games))
    }
}

fn database_path() -> Option<PathBuf> {
    let local_app_data = std::env::var("LOCALAPPDATA").ok()?;
    Some(PathBuf::from(local_app_data).join(DATABASE_RELATIVE_PATH))
}

/// Reads installed games from an open `GameInstallInfo.sqlite` connection.
fn read_games(conn: &Connection) -> rusqlite::Result<Vec<GameInstall>> {
    let mut stmt =
        conn.prepare("SELECT Id, ProductTitle, InstallDirectory FROM DbSet WHERE Installed = 1")?;

    let rows = stmt.query_map([], |row| {
        Ok(RawAmazonEntry {
            id: row.get(0)?,
            product_title: row.get(1)?,
            install_directory: row.get(2)?,
        })
    })?;

    Ok(rows.flatten().filter_map(build_game_install).collect())
}

/// One raw `DbSet` row (or a synthetic stand-in in tests).
struct RawAmazonEntry {
    id: String,
    product_title: Option<String>,
    install_directory: Option<String>,
}

/// Builds a `GameInstall` from a raw `DbSet` row. `InstallDirectory` is
/// required; `ProductTitle` falls back to the directory's last path component.
fn build_game_install(entry: RawAmazonEntry) -> Option<GameInstall> {
    let install_directory = entry.install_directory.filter(|s| !s.trim().is_empty())?;
    let path = PathBuf::from(install_directory);

    let name = entry
        .product_title
        .filter(|s| !s.trim().is_empty())
        .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))?;

    Some(GameInstall {
        name,
        install_dir: path,
        app_id: Some(entry.id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory replica of the launcher's `DbSet` table.
    fn test_db(rows: &[(&str, Option<&str>, Option<&str>, i64)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE DbSet (
                Id TEXT PRIMARY KEY,
                ProductTitle TEXT,
                InstallDirectory TEXT,
                Installed INTEGER
            )",
        )
        .unwrap();

        for (id, title, dir, installed) in rows {
            conn.execute(
                "INSERT INTO DbSet (Id, ProductTitle, InstallDirectory, Installed)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, title, dir, installed],
            )
            .unwrap();
        }

        conn
    }

    #[test]
    fn read_games_returns_installed_games_only() {
        let conn = test_db(&[
            (
                "amzn1.adg.product.1",
                Some("Fallout 76"),
                Some(r"F:\Amazon Games\Library\Fallout 76"),
                1,
            ),
            (
                "amzn1.adg.product.2",
                Some("Uninstalled Game"),
                Some(r"F:\Amazon Games\Library\Old"),
                0,
            ),
        ]);

        let games = read_games(&conn).unwrap();

        assert_eq!(games.len(), 1);
        assert_eq!(games[0].name, "Fallout 76");
        assert_eq!(games[0].app_id.as_deref(), Some("amzn1.adg.product.1"));
        assert_eq!(
            games[0].install_dir,
            PathBuf::from(r"F:\Amazon Games\Library\Fallout 76")
        );
    }

    #[test]
    fn read_games_skips_rows_without_install_directory() {
        let conn = test_db(&[("amzn1.adg.product.1", Some("Broken"), None, 1)]);
        assert!(read_games(&conn).unwrap().is_empty());
    }

    #[test]
    fn read_games_falls_back_to_folder_name_when_title_missing() {
        let conn = test_db(&[(
            "amzn1.adg.product.1",
            None,
            Some(r"F:\Amazon Games\Library\Lost Ark"),
            1,
        )]);

        let games = read_games(&conn).unwrap();

        assert_eq!(games.len(), 1);
        assert_eq!(games[0].name, "Lost Ark");
    }
}
