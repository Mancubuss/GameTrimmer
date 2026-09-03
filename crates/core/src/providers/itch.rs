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

use super::{
    degrades_evidence, diagnostic, DiscoveredLibrary, DiscoveryDiagnostic, DiscoveryReport,
    DiscoveryStatus, GameInstall, LibraryProvider, OrphanEvidence, GAME_ABSENT,
};

const DATABASE_RELATIVE_PATH: &str = r"itch\db\butler.db";

pub struct ItchProvider;

impl LibraryProvider for ItchProvider {
    fn name(&self) -> &'static str {
        "itch"
    }

    fn try_discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        Ok(discover_itch().data)
    }

    fn discover(&self) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
        discover_itch()
    }
}

fn discover_itch() -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let Some(db_path) = database_path().filter(|path| path.is_file()) else {
        return DiscoveryReport::not_installed(Vec::new());
    };

    let conn = match Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => conn,
        Err(err) => {
            return DiscoveryReport::failed(
                Vec::new(),
                diagnostic("itch", "database-open", Some(db_path), err),
            )
        }
    };
    discover_itch_from_conn(&conn, db_path)
}

/// The testable core of itch discovery: everything past an open `butler.db`
/// connection. Split out so tests can drive it against an in-memory database
/// instead of the real `%APPDATA%\itch\db\butler.db`.
fn discover_itch_from_conn(
    conn: &Connection,
    db_path: PathBuf,
) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let (read_games, mut diagnostics) = match read_games_report(conn) {
        Ok(report) => report,
        Err(err) => {
            return DiscoveryReport::failed(
                Vec::new(),
                diagnostic("itch", "games-query", Some(db_path.clone()), err),
            )
        }
    };
    let mut games = Vec::new();
    for game in read_games {
        // A cave whose install directory is simply not there is normal - the
        // game was uninstalled outside the itch app, or the cave record is
        // stale - and an absent folder cannot be mistaken for orphan
        // residue. A folder we merely failed to examine is the dangerous
        // case: it stays on disk, drops out of `games`, and `itch_spec`'s
        // leftover detection would then call it residue. Diagnose it
        // instead of collapsing both into one `game-path` stage.
        match super::try_is_dir(&game.install_dir) {
            Ok(true) => games.push(game),
            // Recorded, but explicitly not degrading - see `GAME_ABSENT`.
            Ok(false) => diagnostics.push(diagnostic("itch", 
                GAME_ABSENT,
                Some(game.install_dir.clone()),
                "cave record present, install directory absent (uninstalled outside the itch app, or a stale record)",
            )),
            Err(err) => diagnostics.push(diagnostic("itch", 
                "game-path",
                Some(game.install_dir.clone()),
                err,
            )),
        }
    }

    let mut libraries = super::group_by_parent_dir("itch", games);
    // `install_locations` is itch's own list of roots, independent of what
    // is currently installed in them - so a location the user emptied is
    // still reportable rather than silently vanishing. It is also exactly
    // where `orphans::itch_spec` looks for `.itch` receipts, which is the
    // case worth surfacing: every game removed, the folders left behind.
    let (roots, mut root_diagnostics) = match read_install_locations_report(conn) {
        Ok(report) => report,
        Err(err) => {
            return DiscoveryReport::failed(
                Vec::new(),
                diagnostic("itch", "locations-query", Some(db_path), err),
            )
        }
    };
    diagnostics.append(&mut root_diagnostics);
    for root in roots {
        // Left as a plain `is_dir()` on purpose - see the ticket report for
        // the reasoning. In short: `register_root` only ever fires when no
        // library already covers this path, i.e. when `group_by_parent_dir`
        // found no games under it, so a false "absent" here cannot strip a
        // live installation out of the managed set the way the per-game
        // check above can. This stage is also explicitly out of scope.
        if root.is_dir() {
            super::register_root(&mut libraries, "itch", root);
        } else {
            diagnostics.push(diagnostic(
                "itch",
                "location-path",
                Some(root),
                "configured itch install location is unavailable",
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

/// Reads itch's configured install locations from an open `butler.db`.
#[cfg(test)]
fn read_install_locations(conn: &Connection) -> rusqlite::Result<Vec<PathBuf>> {
    read_install_locations_report(conn).map(|(locations, _)| locations)
}

fn read_install_locations_report(
    conn: &Connection,
) -> rusqlite::Result<(Vec<PathBuf>, Vec<DiscoveryDiagnostic>)> {
    let mut stmt = conn.prepare("SELECT path FROM install_locations")?;
    let rows = stmt.query_map([], |row| row.get::<_, Option<String>>(0))?;
    let mut locations = Vec::new();
    let mut diagnostics = Vec::new();
    for (index, row) in rows.enumerate() {
        match row {
            Ok(Some(path)) if !path.trim().is_empty() => {
                locations.push(PathBuf::from(path.trim().trim_end_matches(['\\', '/'])))
            }
            // A row that decoded fine but carries no usable path is itch
            // saying this location slot is empty or unset - not a failure.
            // It must vanish silently rather than cost the library its
            // `Authoritative` evidence.
            Ok(_) => {}
            // A row that failed to *decode* is the real failure and keeps
            // its own diagnostic, distinct from the silent case above.
            Err(err) => diagnostics.push(diagnostic(
                "itch",
                "location-decode",
                None,
                format!("row #{index} could not be decoded: {err}"),
            )),
        }
    }
    Ok((locations, diagnostics))
}

fn database_path() -> Option<PathBuf> {
    let app_data = std::env::var("APPDATA").ok()?;
    Some(PathBuf::from(app_data).join(DATABASE_RELATIVE_PATH))
}

/// Reads installed games from an open `butler.db` connection.
#[cfg(test)]
fn read_games(conn: &Connection) -> rusqlite::Result<Vec<GameInstall>> {
    read_games_report(conn).map(|(games, _)| games)
}

fn read_games_report(
    conn: &Connection,
) -> rusqlite::Result<(Vec<GameInstall>, Vec<DiscoveryDiagnostic>)> {
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

    let mut games = Vec::new();
    let mut diagnostics = Vec::new();
    for (index, row) in rows.enumerate() {
        match row {
            // A joined row that decoded fine but has no location or folder
            // name is butler saying nothing is installed there - a leftover
            // cave record, not a failure. It must vanish silently rather
            // than cost the library its `Authoritative` evidence.
            Ok(raw) => {
                if let Some(game) = build_game_install(raw) {
                    games.push(game);
                }
            }
            // A row that failed to *decode* is the real failure: it keeps
            // its own diagnostic, distinct from the silent case above.
            Err(err) => diagnostics.push(diagnostic(
                "itch",
                "game-decode",
                None,
                format!("row #{index} could not be decoded: {err}"),
            )),
        }
    }
    Ok((games, diagnostics))
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

    /// The case this exists for: itch is installed and configured, every game
    /// removed. The location is still itch's, and still worth reporting.
    #[test]
    fn read_install_locations_returns_roots_with_no_caves_in_them() {
        let conn = test_db();
        conn.execute_batch(
            "INSERT INTO install_locations VALUES ('loc-1', 'F:\\itch');
             INSERT INTO install_locations VALUES ('loc-2', 'H:\\itch.io\\');",
        )
        .unwrap();

        let roots = read_install_locations(&conn).unwrap();

        assert_eq!(
            roots,
            vec![PathBuf::from(r"F:\itch"), PathBuf::from(r"H:\itch.io")]
        );
    }

    #[test]
    fn read_install_locations_skips_blank_and_null_paths() {
        let conn = test_db();
        conn.execute_batch(
            "INSERT INTO install_locations VALUES ('loc-1', NULL);
             INSERT INTO install_locations VALUES ('loc-2', '   ');",
        )
        .unwrap();

        assert!(read_install_locations(&conn).unwrap().is_empty());
    }

    /// The regression this slice exists to prevent: a location row with no
    /// usable path used to add a `location-row` diagnostic shared with the
    /// decode-failure case, and any diagnostic at all flips every library
    /// from `Authoritative` to `Degraded`. A blank/unset location slot is
    /// ordinary itch state and must vanish silently.
    #[test]
    fn read_install_locations_report_skips_a_row_with_no_path_without_a_diagnostic() {
        let conn = test_db();
        conn.execute_batch(
            "INSERT INTO install_locations VALUES ('loc-1', NULL);
             INSERT INTO install_locations VALUES ('loc-2', '   ');",
        )
        .unwrap();

        let (locations, diagnostics) = read_install_locations_report(&conn).unwrap();

        assert!(locations.is_empty());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    /// The counterpart: a row that fails to *decode* is a real failure and
    /// keeps its own diagnostic, now named distinctly from the silent case
    /// above (`location-decode`, not the shared `location-row`).
    #[test]
    fn read_install_locations_report_flags_a_row_that_fails_to_decode() {
        let conn = test_db();
        conn.execute_batch(
            // A BLOB where the query expects TEXT fails to decode as a string.
            "INSERT INTO install_locations (id, path) VALUES ('loc-1', X'010203');",
        )
        .unwrap();

        let (locations, diagnostics) = read_install_locations_report(&conn).unwrap();

        assert!(locations.is_empty());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].stage, "location-decode");
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

    /// A joined row that decoded fine but has no folder name is ordinary
    /// leftover cave state - it must vanish silently, not leave a `game-row`
    /// diagnostic behind.
    #[test]
    fn read_games_report_skips_a_row_with_no_folder_name_without_a_diagnostic() {
        let conn = test_db();
        conn.execute_batch(
            "INSERT INTO games VALUES (123, 'Celeste');
             INSERT INTO install_locations VALUES ('loc-1', 'F:\\itch');
             INSERT INTO caves VALUES ('cave-1', 123, 'loc-1', NULL);",
        )
        .unwrap();

        let (games, diagnostics) = read_games_report(&conn).unwrap();

        assert!(games.is_empty());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    /// The counterpart: a row that fails to *decode* is a real failure and
    /// keeps its own diagnostic, now named distinctly from the silent case
    /// above (`game-decode`, not the shared `game-row`).
    #[test]
    fn read_games_report_flags_a_row_that_fails_to_decode() {
        let conn = test_db();
        conn.execute_batch(
            "INSERT INTO games VALUES (123, 'Celeste');
             INSERT INTO install_locations VALUES ('loc-1', 'F:\\itch');
             -- a BLOB where the join expects TEXT fails to decode as a string
             INSERT INTO caves VALUES ('cave-1', 123, 'loc-1', X'010203');",
        )
        .unwrap();

        let (games, diagnostics) = read_games_report(&conn).unwrap();

        assert!(games.is_empty());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].stage, "game-decode");
    }

    /// The regression this slice exists to prevent: one cave record with no
    /// usable location used to add a `game-row` diagnostic, and any
    /// diagnostic at all flips every library from `Authoritative` to
    /// `Degraded` (`discover_itch_from_conn`) - which matters for itch in
    /// particular, since it is the one provider here with real leftover-file
    /// detection to lose. A leftover cave must not disable it for a library
    /// that has a perfectly good other game in it.
    #[test]
    fn a_row_with_no_folder_name_does_not_degrade_the_library() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("celeste");
        std::fs::create_dir(&install_dir).unwrap();

        let conn = test_db();
        conn.execute(
            "INSERT INTO games VALUES (?1, ?2)",
            rusqlite::params![123, "Celeste"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO games VALUES (?1, ?2)",
            rusqlite::params![456, "Broken"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO install_locations VALUES (?1, ?2)",
            rusqlite::params!["loc-1", temp.path().to_str().unwrap()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO caves VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["cave-1", 123, "loc-1", "celeste"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO caves (id, game_id, install_location_id, install_folder_name)
             VALUES (?1, ?2, ?3, NULL)",
            rusqlite::params!["cave-2", 456, "loc-1"],
        )
        .unwrap();

        let report = discover_itch_from_conn(&conn, temp.path().join("butler.db"));

        assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
        assert_eq!(report.data.len(), 1);
        assert_eq!(
            report.data[0].orphan_evidence,
            OrphanEvidence::Authoritative
        );
        assert_eq!(report.data[0].games.len(), 1);
        assert_eq!(report.data[0].games[0].name, "Celeste");
    }

    /// The other half: a row that fails to decode is a real failure and does
    /// degrade the library it lands in.
    #[test]
    fn a_row_that_fails_to_decode_degrades_the_library() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("celeste");
        std::fs::create_dir(&install_dir).unwrap();

        let conn = test_db();
        conn.execute_batch(&format!(
            "INSERT INTO games VALUES (123, 'Celeste');
             INSERT INTO install_locations VALUES ('loc-1', '{}');
             INSERT INTO caves VALUES ('cave-1', 123, 'loc-1', 'celeste');
             INSERT INTO caves VALUES ('cave-2', 123, 'loc-1', X'010203');",
            temp.path().to_str().unwrap().replace('\\', "\\\\")
        ))
        .unwrap();

        let report = discover_itch_from_conn(&conn, temp.path().join("butler.db"));

        assert_eq!(report.status, crate::providers::DiscoveryStatus::Degraded);
        assert_eq!(report.data.len(), 1);
        assert_eq!(report.data[0].orphan_evidence, OrphanEvidence::Degraded);
    }

    /// A cave whose install directory is provably absent - uninstalled
    /// outside the itch app, or a stale cave record - must not degrade the
    /// library: an absent folder can never be mistaken for orphan residue.
    #[test]
    fn a_game_whose_install_dir_is_absent_keeps_the_library_authoritative() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("celeste");
        std::fs::create_dir(&install_dir).unwrap();

        let conn = test_db();
        conn.execute(
            "INSERT INTO games VALUES (?1, ?2)",
            rusqlite::params![123, "Celeste"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO games VALUES (?1, ?2)",
            rusqlite::params![456, "Never Downloaded"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO install_locations VALUES (?1, ?2)",
            rusqlite::params!["loc-1", temp.path().to_str().unwrap()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO caves VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["cave-1", 123, "loc-1", "celeste"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO caves VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["cave-2", 456, "loc-1", "never-downloaded"],
        )
        .unwrap();

        let report = discover_itch_from_conn(&conn, temp.path().join("butler.db"));

        assert_eq!(report.status, crate::providers::DiscoveryStatus::Complete);
        assert_eq!(report.data.len(), 1);
        assert_eq!(
            report.data[0].orphan_evidence,
            OrphanEvidence::Authoritative
        );
        assert_eq!(report.data[0].games.len(), 1);
        assert_eq!(report.data[0].games[0].name, "Celeste");
        assert_eq!(
            report
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.stage)
                .collect::<Vec<_>>(),
            vec![GAME_ABSENT],
            "the absent cave must still leave a trace: {:?}",
            report.diagnostics
        );
    }

    /// The dangerous counterpart: an install directory that cannot be
    /// examined - as opposed to one that is provably absent - must degrade
    /// the library, because it may still be sitting on disk and would
    /// otherwise be misread as orphan residue by `itch_spec`'s leftover
    /// detection - the exact scenario this fix exists for.
    #[test]
    fn a_game_with_an_unexaminable_install_dir_degrades_the_library() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("celeste");
        std::fs::create_dir(&install_dir).unwrap();

        let conn = test_db();
        conn.execute(
            "INSERT INTO games VALUES (?1, ?2)",
            rusqlite::params![123, "Celeste"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO games VALUES (?1, ?2)",
            rusqlite::params![456, "Broken"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO install_locations VALUES (?1, ?2)",
            rusqlite::params!["loc-1", temp.path().to_str().unwrap()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO caves VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["cave-1", 123, "loc-1", "celeste"],
        )
        .unwrap();
        // `<` is invalid in a Windows path component, so the probe fails
        // with ERROR_INVALID_NAME rather than "not found" - a portable
        // stand-in for a DACL denial, offline placeholder, or drive not yet
        // spun up.
        conn.execute(
            "INSERT INTO caves VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["cave-2", 456, "loc-1", "bad<name"],
        )
        .unwrap();

        let report = discover_itch_from_conn(&conn, temp.path().join("butler.db"));

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
