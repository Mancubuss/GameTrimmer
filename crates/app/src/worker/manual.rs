//! Manual (user-added) game libraries: CRUD against `game_libraries` rows
//! with `vendor = 'manual'`, plus first-level subfolder enumeration used by
//! the scan worker to treat each subfolder of a manual library as one
//! "game".

use std::path::{Path, PathBuf};

use gametrimmer_core::error::Result as CoreResult;
use gametrimmer_core::providers::{
    try_holds_installed_files, DiscoveredLibrary, DiscoveryDiagnostic, GameInstall, OrphanEvidence,
};
use rusqlite::{params, Connection};

use crate::i18n::{self, Lang};

/// Vendor tag used for libraries the user added by hand through the UI.
pub const MANUAL_VENDOR: &str = "manual";

/// One row of `game_libraries`, as shown in the library management list.
#[derive(Debug, Clone)]
pub struct LibraryRow {
    pub id: i64,
    pub vendor: String,
    pub path: PathBuf,
}

/// Lists every known library (all vendors), for the library management UI.
pub fn list_libraries(conn: &Connection) -> CoreResult<Vec<LibraryRow>> {
    let mut stmt =
        conn.prepare("SELECT id, vendor, path FROM game_libraries ORDER BY vendor, path")?;
    let rows = stmt.query_map([], |row| {
        Ok(LibraryRow {
            id: row.get(0)?,
            vendor: row.get(1)?,
            path: PathBuf::from(row.get::<_, String>(2)?),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Registers a user-picked folder as a manual library. A no-op if the path
/// is already registered under any vendor - `path` is `UNIQUE` in
/// `game_libraries`.
pub fn add_manual_library(conn: &Connection, path: &Path) -> CoreResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO game_libraries (vendor, path) VALUES (?1, ?2)",
        params![MANUAL_VENDOR, path.to_string_lossy()],
    )?;
    Ok(())
}

/// Removes a library and cascades the delete through its `games` / `files`
/// / `findings` rows, child-to-parent, in one transaction - the same
/// ordering `worker::scan::persist_libraries` uses when replacing a
/// library's games, required because neither foreign key carries
/// `ON DELETE CASCADE` and `PRAGMA foreign_keys = ON` is set (see
/// `gametrimmer_core::db::configure`).
pub fn remove_library(conn: &mut Connection, library_id: i64) -> CoreResult<()> {
    let tx = conn.transaction()?;

    tx.execute(
        "DELETE FROM findings WHERE file_id IN (
            SELECT id FROM files WHERE game_id IN (
                SELECT id FROM games WHERE library_id = ?1
            )
        )",
        params![library_id],
    )?;
    tx.execute(
        "DELETE FROM files WHERE game_id IN (SELECT id FROM games WHERE library_id = ?1)",
        params![library_id],
    )?;
    tx.execute(
        "DELETE FROM games WHERE library_id = ?1",
        params![library_id],
    )?;
    tx.execute(
        "DELETE FROM game_libraries WHERE id = ?1",
        params![library_id],
    )?;

    tx.commit()?;
    Ok(())
}

/// Discovers "games" inside every registered manual library: each existing
/// library's first-level subdirectory becomes one game (name = subfolder
/// name, install_dir = its full path, no app id). A manual library whose
/// path no longer exists (disk unplugged, folder moved away, ...) yields a
/// warning instead of a [`DiscoveredLibrary`] - its `game_libraries` row is
/// left untouched, since the disk may only be temporarily unavailable.
pub fn discover_manual_libraries(
    conn: &Connection,
    lang: Lang,
) -> CoreResult<(
    Vec<DiscoveredLibrary>,
    Vec<i18n::Reported>,
    Vec<DiscoveryDiagnostic>,
)> {
    let mut stmt = conn.prepare("SELECT path FROM game_libraries WHERE vendor = ?1")?;
    let paths: Vec<PathBuf> = stmt
        .query_map(params![MANUAL_VENDOR], |row| {
            row.get::<_, String>(0).map(PathBuf::from)
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut libraries = Vec::new();
    let mut warnings = Vec::new();
    let mut diagnostics = Vec::new();

    for path in paths {
        let discovery = match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_dir() => enumerate_subfolders(&path),
            Ok(_) => Err(std::io::Error::other(
                "manual library path is not a directory",
            )),
            Err(err) => Err(err),
        };
        match discovery {
            Ok(games) => libraries.push(DiscoveredLibrary {
                vendor: MANUAL_VENDOR,
                games,
                path,
                orphan_evidence: OrphanEvidence::Heuristic,
            }),
            Err(err) => {
                warnings.push(i18n::Reported::new(lang, |l| {
                    format!(
                        "{}: {err}",
                        i18n::manual_library_unavailable(l, path.display())
                    )
                }));
                diagnostics.push(DiscoveryDiagnostic {
                    provider: MANUAL_VENDOR,
                    stage: "library-enumeration",
                    path: Some(path.clone()),
                    message: err.to_string(),
                });
                libraries.push(DiscoveredLibrary {
                    vendor: MANUAL_VENDOR,
                    games: Vec::new(),
                    path,
                    orphan_evidence: OrphanEvidence::Degraded,
                });
            }
        }
    }

    Ok((libraries, warnings, diagnostics))
}

/// Every first-level subdirectory of `library_path` that actually holds
/// files, sorted by name. Any enumeration or metadata error is returned so a
/// partial manual inventory can be persisted as degraded.
///
/// The `holds_installed_files` filter is the manual-library half of installed-content validation: a
/// hand-picked folder is enumerated by name just like a vendor root, so a
/// contentless subfolder became a phantom game here for the same reason.
/// Pointing GameTrimmer at a folder is not a claim that every subfolder of it
/// is an installed game.
fn enumerate_subfolders(library_path: &Path) -> std::io::Result<Vec<GameInstall>> {
    let entries = std::fs::read_dir(library_path)?;
    let mut games = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || !try_holds_installed_files(&entry.path())? {
            continue;
        }
        games.push(GameInstall {
            name: entry.file_name().to_string_lossy().into_owned(),
            install_dir: entry.path(),
            app_id: None,
        });
    }

    games.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(games)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gametrimmer_core::db;

    #[test]
    fn discover_manual_libraries_enumerates_first_level_subfolders_as_games() {
        let conn = db::open_in_memory().expect("open in-memory db");
        let dir = tempfile::tempdir().expect("create temp dir");

        let sub_a = dir.path().join("Game A");
        let sub_b = dir.path().join("Game B");
        let sub_c = dir.path().join("Game C");
        for sub in [&sub_a, &sub_b, &sub_c] {
            std::fs::create_dir_all(sub).expect("create subfolder");
            std::fs::write(sub.join("file.txt"), b"x").expect("write file");
        }
        // A file directly inside the library root must not be treated as a game.
        std::fs::write(dir.path().join("readme.txt"), b"not a game").expect("write root file");

        add_manual_library(&conn, dir.path()).expect("register manual library");

        let (libraries, warnings, diagnostics) =
            discover_manual_libraries(&conn, Lang::En).expect("discover should succeed");

        assert!(
            warnings.is_empty(),
            "existing library should produce no warnings: {warnings:?}"
        );
        assert!(diagnostics.is_empty());
        assert_eq!(libraries.len(), 1);
        let library = &libraries[0];
        assert_eq!(library.vendor, MANUAL_VENDOR);
        assert_eq!(library.games.len(), 3, "only subfolders count as games");

        let mut names: Vec<&str> = library.games.iter().map(|g| g.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["Game A", "Game B", "Game C"]);

        let game_a = library
            .games
            .iter()
            .find(|g| g.name == "Game A")
            .expect("Game A present");
        assert_eq!(game_a.install_dir, sub_a);
        assert!(game_a.app_id.is_none());
    }

    /// The manual-library half of installed-content validation: pointing GameTrimmer at a folder is
    /// not a claim that every subfolder of it is an installed game. A
    /// contentless subfolder is residue, and a phantom game must not enter the
    /// model of a tool that deletes files.
    #[test]
    fn discover_manual_libraries_ignores_contentless_subfolders() {
        let conn = db::open_in_memory().expect("open in-memory db");
        let dir = tempfile::tempdir().expect("create temp dir");

        let real = dir.path().join("Real Game");
        std::fs::create_dir_all(&real).expect("create subfolder");
        std::fs::write(real.join("game.exe"), b"MZ").expect("write file");
        // Residue: the folder survived, its files did not.
        std::fs::create_dir_all(dir.path().join(r"Ghost Game\bin")).expect("create empty tree");

        add_manual_library(&conn, dir.path()).expect("register manual library");

        let (libraries, _, _) =
            discover_manual_libraries(&conn, Lang::En).expect("discover should succeed");

        let games = &libraries[0].games;
        assert_eq!(
            games.len(),
            1,
            "only the subfolder with files is a game, got {:?}",
            games.iter().map(|g| &g.name).collect::<Vec<_>>()
        );
        assert_eq!(games[0].name, "Real Game");
    }

    #[test]
    fn discover_manual_libraries_warns_but_keeps_row_when_folder_missing() {
        let conn = db::open_in_memory().expect("open in-memory db");
        let dir = tempfile::tempdir().expect("create temp dir");
        let missing_path = dir.path().join("does-not-exist-anymore");

        add_manual_library(&conn, &missing_path).expect("register manual library");

        let (libraries, warnings, diagnostics) = discover_manual_libraries(&conn, Lang::En)
            .expect("discover must not error on a missing folder");

        assert_eq!(libraries.len(), 1);
        assert!(libraries[0].games.is_empty());
        assert_eq!(libraries[0].orphan_evidence, OrphanEvidence::Degraded);
        assert_eq!(warnings.len(), 1);
        assert_eq!(diagnostics.len(), 1);
        assert!(warnings[0]
            .shown
            .contains(&missing_path.display().to_string()));
        assert_eq!(
            warnings[0].log, warnings[0].shown,
            "an English interface renders both audiences the same",
        );

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM game_libraries WHERE vendor = 'manual'",
                [],
                |row| row.get(0),
            )
            .expect("count manual libraries");
        assert_eq!(
            count, 1,
            "the manual library row must survive a temporarily missing disk"
        );
    }

    #[test]
    fn remove_library_cascades_through_games_files_and_findings() {
        let mut conn = db::open_in_memory().expect("open in-memory db");

        conn.execute(
            "INSERT INTO game_libraries (vendor, path) VALUES ('manual', 'C:/Manual')",
            [],
        )
        .expect("insert library");
        let library_id: i64 = conn
            .query_row(
                "SELECT id FROM game_libraries WHERE path = 'C:/Manual'",
                [],
                |row| row.get(0),
            )
            .expect("read library id");

        conn.execute(
            "INSERT INTO games (library_id, name, install_dir) VALUES (?1, 'Game A', 'C:/Manual/Game A')",
            params![library_id],
        )
        .expect("insert game");
        let game_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO files (game_id, rel_path, size) VALUES (?1, 'a.txt', 10)",
            params![game_id],
        )
        .expect("insert file");
        let file_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO findings (file_id, category, confidence) VALUES (?1, 'bonus', 50)",
            params![file_id],
        )
        .expect("insert finding");

        remove_library(&mut conn, library_id).expect("remove_library should succeed");

        for (table, sql) in [
            ("game_libraries", "SELECT COUNT(*) FROM game_libraries"),
            ("games", "SELECT COUNT(*) FROM games"),
            ("files", "SELECT COUNT(*) FROM files"),
            ("findings", "SELECT COUNT(*) FROM findings"),
        ] {
            let count: i64 = conn
                .query_row(sql, [], |row| row.get(0))
                .unwrap_or_else(|err| panic!("count query for `{table}` failed: {err}"));
            assert_eq!(
                count, 0,
                "table `{table}` should be empty after cascade removal"
            );
        }
    }
}
