//! Amazon Games library discovery: SQLite database
//! `%LOCALAPPDATA%\Amazon Games\Data\Games\Sql\GameInstallInfo.sqlite`,
//! table `DbSet` (`Id`, `ProductTitle`, `InstallDirectory`, `Installed`).
//!
//! The database is opened read-only; the running launcher may hold locks, in
//! which case discovery fails with a warning rather than corrupting anything.

use std::path::PathBuf;

use rusqlite::{Connection, OpenFlags};

use crate::error::Result;

use super::{
    degrades_evidence, DiscoveredLibrary, DiscoveryDiagnostic, DiscoveryReport, DiscoveryStatus,
    GameInstall, LibraryProvider, OrphanEvidence, GAME_ABSENT,
};

const DATABASE_RELATIVE_PATH: &str = r"Amazon Games\Data\Games\Sql\GameInstallInfo.sqlite";

pub struct AmazonProvider;

impl LibraryProvider for AmazonProvider {
    fn name(&self) -> &'static str {
        "amazon"
    }

    fn try_discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        Ok(discover_amazon().data)
    }

    fn discover(&self) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
        discover_amazon()
    }
}

fn diagnostic(
    stage: &'static str,
    path: Option<PathBuf>,
    message: impl std::fmt::Display,
) -> DiscoveryDiagnostic {
    DiscoveryDiagnostic {
        provider: "amazon",
        stage,
        path,
        message: message.to_string(),
    }
}

fn discover_amazon() -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let Some(db_path) = database_path() else {
        return DiscoveryReport::not_installed(Vec::new());
    };
    match std::fs::metadata(&db_path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return DiscoveryReport::failed(
                Vec::new(),
                diagnostic(
                    "database-path",
                    Some(db_path),
                    "database path is not a file",
                ),
            )
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return DiscoveryReport::not_installed(Vec::new())
        }
        Err(err) => {
            return DiscoveryReport::failed(
                Vec::new(),
                diagnostic("database-metadata", Some(db_path), err),
            )
        }
    }
    let conn = match Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => conn,
        Err(err) => {
            return DiscoveryReport::failed(
                Vec::new(),
                diagnostic("database-open", Some(db_path), err),
            )
        }
    };
    discover_amazon_from_conn(&conn, db_path)
}

/// The testable core of Amazon discovery: everything past an open
/// `GameInstallInfo.sqlite` connection. Split out so tests can drive it
/// against an in-memory database instead of the real launcher install.
fn discover_amazon_from_conn(
    conn: &Connection,
    db_path: PathBuf,
) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let (rows, mut diagnostics) = match read_games_report(conn) {
        Ok(report) => report,
        Err(err) => {
            return DiscoveryReport::failed(
                Vec::new(),
                diagnostic("games-query", Some(db_path), err),
            )
        }
    };
    let mut games = Vec::new();
    for game in rows {
        // A row whose install directory is simply not there is normal (the
        // game was uninstalled outside Amazon Games, or the record is stale)
        // and an absent folder cannot be mistaken for orphan residue. A
        // folder we merely failed to examine is the dangerous case: it stays
        // on disk, drops out of `games`, and would look unmanaged. Diagnose
        // it instead of collapsing both into one `game-path` stage.
        match super::try_is_dir(&game.install_dir) {
            Ok(true) => games.push(game),
            // Recorded, but explicitly not degrading - see `GAME_ABSENT`.
            Ok(false) => diagnostics.push(diagnostic(
                GAME_ABSENT,
                Some(game.install_dir),
                "database entry present, install directory absent (uninstalled outside Amazon Games, or a stale record)",
            )),
            Err(err) => diagnostics.push(diagnostic("game-path", Some(game.install_dir), err)),
        }
    }
    let mut libraries = super::group_by_parent_dir("amazon", games);
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

fn database_path() -> Option<PathBuf> {
    let local_app_data = std::env::var("LOCALAPPDATA").ok()?;
    Some(PathBuf::from(local_app_data).join(DATABASE_RELATIVE_PATH))
}

/// Reads installed games from an open `GameInstallInfo.sqlite` connection.
#[cfg(test)]
fn read_games(conn: &Connection) -> rusqlite::Result<Vec<GameInstall>> {
    read_games_report(conn).map(|(games, _)| games)
}

fn read_games_report(
    conn: &Connection,
) -> rusqlite::Result<(Vec<GameInstall>, Vec<DiscoveryDiagnostic>)> {
    let mut stmt =
        conn.prepare("SELECT Id, ProductTitle, InstallDirectory FROM DbSet WHERE Installed = 1")?;

    let rows = stmt.query_map([], |row| {
        Ok(RawAmazonEntry {
            id: row.get(0)?,
            product_title: row.get(1)?,
            install_directory: row.get(2)?,
        })
    })?;

    let mut games = Vec::new();
    let mut diagnostics = Vec::new();
    for (index, row) in rows.enumerate() {
        let entry = match row {
            Ok(entry) => entry,
            Err(err) => {
                diagnostics.push(diagnostic(
                    "row-decode",
                    None,
                    format!("row {index}: {err}"),
                ));
                continue;
            }
        };
        // A row that decoded fine but yields no `GameInstall` is the launcher
        // saying nothing is installed there - a leftover from an uninstall, a
        // DLC/edition record, or a catalogue entry never downloaded. That is
        // not a failure and must not cost the library its `Authoritative`
        // evidence, so it is dropped without a diagnostic. A row that failed
        // to *decode* (the `Err` arm above) is the real failure and keeps its
        // own `row-decode` diagnostic.
        if let Some(game) = build_game_install(entry) {
            games.push(game);
        }
    }
    Ok((games, diagnostics))
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

    /// A row that decoded fine but has no install directory is ordinary
    /// launcher state (uninstalled, DLC, never downloaded) - it must vanish
    /// silently, not leave a `game-entry` diagnostic behind.
    #[test]
    fn read_games_report_skips_a_row_with_no_directory_without_a_diagnostic() {
        let conn = test_db(&[("amzn1.adg.product.1", Some("Broken"), None, 1)]);

        let (games, diagnostics) = read_games_report(&conn).unwrap();

        assert!(games.is_empty());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    /// The counterpart: a row that fails to *decode* is a real failure and
    /// keeps its own diagnostic (already named distinctly, `row-decode`).
    #[test]
    fn read_games_report_flags_a_row_that_fails_to_decode() {
        let conn = test_db(&[]);
        conn.execute(
            "INSERT INTO DbSet (Id, ProductTitle, InstallDirectory, Installed)
             VALUES (NULL, 'Broken', 'F:\\Amazon Games\\Broken', 1)",
            [],
        )
        .unwrap();

        let (games, diagnostics) = read_games_report(&conn).unwrap();

        assert!(games.is_empty());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].stage, "row-decode");
    }

    /// The regression this slice exists to prevent: one installed record with
    /// no usable path used to add a diagnostic, and any diagnostic at all
    /// flips every library from `Authoritative` to `Degraded`
    /// (`discover_amazon_from_conn`). A leftover record must not disable
    /// orphan detection for a library that has a perfectly good other game.
    #[test]
    fn a_row_with_no_directory_does_not_degrade_the_library() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("Fallout 76");
        std::fs::create_dir(&install_dir).unwrap();

        let conn = test_db(&[
            (
                "amzn1.adg.product.1",
                Some("Fallout 76"),
                Some(install_dir.to_str().unwrap()),
                1,
            ),
            ("amzn1.adg.product.2", Some("Broken"), None, 1),
        ]);

        let report = discover_amazon_from_conn(&conn, temp.path().join("GameInstallInfo.sqlite"));

        assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
        assert_eq!(report.data.len(), 1);
        assert_eq!(
            report.data[0].orphan_evidence,
            OrphanEvidence::Authoritative
        );
        assert_eq!(report.data[0].games.len(), 1);
    }

    /// The other half: a row that fails to decode is a real failure and does
    /// degrade the library it lands in.
    #[test]
    fn a_row_that_fails_to_decode_degrades_the_library() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("Fallout 76");
        std::fs::create_dir(&install_dir).unwrap();

        let conn = test_db(&[(
            "amzn1.adg.product.1",
            Some("Fallout 76"),
            Some(install_dir.to_str().unwrap()),
            1,
        )]);
        conn.execute(
            "INSERT INTO DbSet (Id, ProductTitle, InstallDirectory, Installed)
             VALUES (NULL, 'Broken', 'F:\\Amazon Games\\Broken', 1)",
            [],
        )
        .unwrap();

        let report = discover_amazon_from_conn(&conn, temp.path().join("GameInstallInfo.sqlite"));

        assert_eq!(report.status, crate::providers::DiscoveryStatus::Degraded);
        assert_eq!(report.data.len(), 1);
        assert_eq!(report.data[0].orphan_evidence, OrphanEvidence::Degraded);
    }

    /// A record whose install directory is provably absent - uninstalled
    /// outside Amazon Games, or a stale record - must not degrade the
    /// library: an absent folder can never be mistaken for orphan residue.
    #[test]
    fn a_game_whose_install_dir_is_absent_keeps_the_library_authoritative() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("Fallout 76");
        std::fs::create_dir(&install_dir).unwrap();
        let absent_dir = temp.path().join("Never Downloaded");

        let conn = test_db(&[
            (
                "amzn1.adg.product.1",
                Some("Fallout 76"),
                Some(install_dir.to_str().unwrap()),
                1,
            ),
            (
                "amzn1.adg.product.2",
                Some("Never Downloaded"),
                Some(absent_dir.to_str().unwrap()),
                1,
            ),
        ]);

        let report = discover_amazon_from_conn(&conn, temp.path().join("GameInstallInfo.sqlite"));

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
        let install_dir = temp.path().join("Fallout 76");
        std::fs::create_dir(&install_dir).unwrap();
        // `<` is invalid in a Windows path component, so the probe fails with
        // ERROR_INVALID_NAME rather than "not found" - a portable stand-in
        // for a DACL denial, offline placeholder, or drive not yet spun up.
        let unexaminable = temp.path().join("bad<name");

        let conn = test_db(&[
            (
                "amzn1.adg.product.1",
                Some("Fallout 76"),
                Some(install_dir.to_str().unwrap()),
                1,
            ),
            (
                "amzn1.adg.product.2",
                Some("Broken"),
                Some(unexaminable.to_str().unwrap()),
                1,
            ),
        ]);

        let report = discover_amazon_from_conn(&conn, temp.path().join("GameInstallInfo.sqlite"));

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
