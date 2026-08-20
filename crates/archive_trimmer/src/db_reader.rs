//! SQLite Database Reader & Archive Candidate Extractor.
//!
//! Connects to `gametrimmer.db` (SQLite) produced by GameTrimmer scans,
//! queries games and candidate monolithic archive files (.pck, .bnk, .pak, .asar, .bik, .bk2, .assets, .unity3d, .bundle),
//! and constructs structured candidate batches for inspection and trimming.

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::formats::is_external_single_language_file;

#[derive(Error, Debug)]
pub enum DbError {
    #[error("Database file not found: {0}")]
    NotFound(PathBuf),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// A candidate archive file referenced in the GameTrimmer database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateFile {
    pub file_id: i64,
    pub rel_path: String,
    pub full_path: PathBuf,
    pub size: u64,
    pub size_on_disk: Option<u64>,
}

/// A game and its collection of discovered candidate archive files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameArchiveCandidates {
    pub game_id: i64,
    pub game_name: String,
    pub install_dir: PathBuf,
    pub library_path: PathBuf,
    pub candidate_files: Vec<CandidateFile>,
}

impl GameArchiveCandidates {
    /// Resolves the absolute root directory of the game.
    pub fn game_root(&self) -> PathBuf {
        if self.install_dir.is_absolute() {
            self.install_dir.clone()
        } else {
            let clean_install = self
                .install_dir
                .to_string_lossy()
                .trim_start_matches(['/', '\\'])
                .to_string();
            self.library_path.join(clean_install)
        }
    }
}

/// Discovers candidate locations for `gametrimmer.db` on the system.
///
/// Searches in order:
/// 1. Current working directory `./gametrimmer.db`
/// 2. Executable parent directory `<exe_dir>/gametrimmer.db`
/// 3. Cargo workspace build targets (`target/debug/gametrimmer.db`, `target/release/gametrimmer.db`)
/// 4. Parent directories for workspace roots (`../gametrimmer.db`, `../../gametrimmer.db`)
/// 5. `%APPDATA%/GameTrimmer/gametrimmer.db`
/// 6. `%LOCALAPPDATA%/GameTrimmer/gametrimmer.db`
pub fn find_default_db_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();

    // 1. Current directory
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("gametrimmer.db"));
    }

    // 2. Next to executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("gametrimmer.db"));
        }
    }

    // 3. Workspace target folders
    candidates.push(PathBuf::from("gametrimmer.db"));
    candidates.push(PathBuf::from("target/debug/gametrimmer.db"));
    candidates.push(PathBuf::from("target/release/gametrimmer.db"));
    candidates.push(PathBuf::from("../gametrimmer.db"));
    candidates.push(PathBuf::from("../target/debug/gametrimmer.db"));
    candidates.push(PathBuf::from("../../target/debug/gametrimmer.db"));

    // 4. Windows AppData
    if let Ok(appdata) = std::env::var("APPDATA") {
        candidates.push(
            PathBuf::from(appdata)
                .join("GameTrimmer")
                .join("gametrimmer.db"),
        );
    }
    if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(local_appdata)
                .join("GameTrimmer")
                .join("gametrimmer.db"),
        );
    }

    // Return the first candidate that exists
    candidates
        .into_iter()
        .find(|candidate| candidate.exists() && candidate.is_file())
}

/// Reads games and their candidate archive files from the SQLite database at `db_path`.
///
/// Supported archive extensions:
/// - `.pck`, `.bnk` (Audiokinetic Wwise)
/// - `.pak` (Unreal Engine & Capcom RE Engine)
/// - `.asar` (Electron)
/// - `.bik`, `.bk2` (RAD Game Tools Bink Video)
/// - `.assets`, `.unity3d`, `.bundle` (Unity AssetBundle / UnityFS)
///
/// Note: Excludes files categorized in the `findings` table as localization and files whose paths
/// indicate they are standalone single-language files (e.g. `locales/*.pak`, `sounds_fra.pck`).
/// These whole-file localizations are deleted as whole files by GameTrimmer core; archive-trimmer
/// is exclusively for monolithic archives where deleting the whole file would break the game.
pub fn read_games_with_candidates(db_path: &Path) -> Result<Vec<GameArchiveCandidates>, DbError> {
    if !db_path.exists() {
        return Err(DbError::NotFound(db_path.to_path_buf()));
    }

    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;

    // Set busy timeout to gracefully handle transient database locks
    let _ = conn.busy_timeout(std::time::Duration::from_secs(5));

    // Check if scan_state table exists and has an active scan id
    let active_scan_id: Option<i64> = conn
        .query_row(
            "SELECT active_scan_id FROM scan_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .ok()
        .flatten();

    // Check if findings table exists to exclude whole-file localizations already handled by GameTrimmer core
    let findings_table_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='findings'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count > 0)
        .unwrap_or(false);

    // Base query for games, libraries, and matching candidate files
    let mut base_query = String::from(
        "SELECT \
            g.id AS game_id, \
            g.name AS game_name, \
            g.install_dir AS install_dir, \
            l.path AS library_path, \
            f.id AS file_id, \
            f.rel_path AS rel_path, \
            f.size AS file_size, \
            f.size_on_disk AS size_on_disk \
         FROM games g \
         JOIN game_libraries l ON g.library_id = l.id \
         JOIN files f ON f.game_id = g.id \
         WHERE ( \
             f.rel_path LIKE '%.pck' OR \
             f.rel_path LIKE '%.bnk' OR \
             f.rel_path LIKE '%.pak' OR \
             f.rel_path LIKE '%.asar' OR \
             f.rel_path LIKE '%.bik' OR \
             f.rel_path LIKE '%.bk2' OR \
             f.rel_path LIKE '%.assets' OR \
             f.rel_path LIKE '%.unity3d' OR \
             f.rel_path LIKE '%.bundle' \
         )",
    );

    if findings_table_exists {
        base_query.push_str(
            " AND f.id NOT IN (SELECT file_id FROM findings WHERE category = 'localization')",
        );
    }

    // Group candidate files by game_id preserving alphabetical game order
    let mut games_map: BTreeMap<i64, GameArchiveCandidates> = BTreeMap::new();
    let mut game_order = Vec::new();

    // Execute query with parameterized scan_id filtering when an active scan exists,
    // or fallback gracefully to unfiltered scan across all games.
    let mut process_rows = |stmt: &mut rusqlite::Statement,
                            params: &[&dyn rusqlite::ToSql]|
     -> Result<(), rusqlite::Error> {
        let mut rows = stmt.query(params)?;
        while let Some(row) = rows.next()? {
            let game_id: i64 = row.get("game_id")?;
            let game_name: String = row.get("game_name")?;
            let install_dir_str: String = row.get("install_dir")?;
            let library_path_str: String = row.get("library_path")?;
            let file_id: i64 = row.get("file_id")?;
            let rel_path: String = row.get("rel_path")?;
            let file_size: i64 = row.get("file_size")?;
            let size_on_disk: Option<i64> = row.get("size_on_disk")?;

            // Exclude whole external single-language files (handled as whole files by GameTrimmer core)
            if is_external_single_language_file(&rel_path) {
                continue;
            }

            let install_dir = PathBuf::from(install_dir_str);
            let library_path = PathBuf::from(library_path_str);

            let game_root = if install_dir.is_absolute() {
                install_dir.clone()
            } else {
                let clean_install = install_dir
                    .to_string_lossy()
                    .trim_start_matches(['/', '\\'])
                    .to_string();
                library_path.join(clean_install)
            };

            let clean_rel = rel_path.trim_start_matches(['/', '\\']);
            let full_path = game_root.join(clean_rel);

            let candidate_file = CandidateFile {
                file_id,
                rel_path,
                full_path,
                size: file_size as u64,
                size_on_disk: size_on_disk.map(|s| s.max(0) as u64),
            };

            let game_entry = match games_map.entry(game_id) {
                std::collections::btree_map::Entry::Vacant(v) => {
                    game_order.push(game_id);
                    v.insert(GameArchiveCandidates {
                        game_id,
                        game_name,
                        install_dir,
                        library_path,
                        candidate_files: Vec::new(),
                    })
                }
                std::collections::btree_map::Entry::Occupied(o) => o.into_mut(),
            };

            game_entry.candidate_files.push(candidate_file);
        }
        Ok(())
    };

    if let Some(scan_id) = active_scan_id {
        if scan_id > 0 {
            let active_query = format!("{base_query} AND g.scan_id = ?1 AND f.scan_id = ?1 ORDER BY g.name ASC, g.id ASC, f.rel_path ASC;");
            if let Ok(mut stmt) = conn.prepare(&active_query) {
                process_rows(&mut stmt, &[&scan_id])?;
            } else {
                // If scan_id columns are missing or prepare failed, fallback to base query
                let fallback_query =
                    format!("{base_query} ORDER BY g.name ASC, g.id ASC, f.rel_path ASC;");
                let mut stmt = conn.prepare(&fallback_query)?;
                process_rows(&mut stmt, &[])?;
            }
        } else {
            let query = format!("{base_query} ORDER BY g.name ASC, g.id ASC, f.rel_path ASC;");
            let mut stmt = conn.prepare(&query)?;
            process_rows(&mut stmt, &[])?;
        }
    } else {
        let query = format!("{base_query} ORDER BY g.name ASC, g.id ASC, f.rel_path ASC;");
        let mut stmt = conn.prepare(&query)?;
        process_rows(&mut stmt, &[])?;
    }

    let mut result = Vec::with_capacity(game_order.len());
    for game_id in game_order {
        if let Some(game) = games_map.remove(&game_id) {
            if !game.candidate_files.is_empty() {
                result.push(game);
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_read_games_with_candidates_mock_db() {
        let temp_db = NamedTempFile::new().expect("create temp db file");
        let conn = Connection::open(temp_db.path()).expect("open db");

        conn.execute_batch(
            "
            CREATE TABLE game_libraries (
                id INTEGER PRIMARY KEY,
                vendor TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE
            );

            CREATE TABLE games (
                id INTEGER PRIMARY KEY,
                scan_id INTEGER NOT NULL DEFAULT 1,
                library_id INTEGER NOT NULL REFERENCES game_libraries(id),
                name TEXT NOT NULL,
                install_dir TEXT NOT NULL
            );

            CREATE TABLE files (
                id INTEGER PRIMARY KEY,
                scan_id INTEGER NOT NULL DEFAULT 1,
                game_id INTEGER REFERENCES games(id),
                rel_path TEXT NOT NULL,
                size INTEGER NOT NULL,
                size_on_disk INTEGER
            );

            CREATE TABLE scan_state (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                active_scan_id INTEGER
            );
            ",
        )
        .expect("create tables");

        // Insert library
        conn.execute(
            "INSERT INTO game_libraries (id, vendor, path) VALUES (1, 'steam', 'C:\\Games')",
            [],
        )
        .expect("insert library");

        // Insert games
        conn.execute(
            "INSERT INTO games (id, scan_id, library_id, name, install_dir) VALUES
             (10, 1, 1, 'Cyber Game 2077', 'CyberGame'),
             (20, 1, 1, 'Space Adventure', 'SpaceAdv')",
            [],
        )
        .expect("insert games");

        // Insert files (including candidates and non-candidates)
        conn.execute(
            "INSERT INTO files (id, scan_id, game_id, rel_path, size, size_on_disk) VALUES
             (101, 1, 10, 'audio/voices.pck', 104857600, 104857600),
             (102, 1, 10, 'content/paks/pakchunk0.pak', 524288000, 524288000),
             (103, 1, 10, 'readme.txt', 1024, 4096),
             (201, 1, 20, 'resources/app.asar', 41943040, 41943040),
             (202, 1, 20, 'movies/intro.bk2', 20971520, 20971520),
             (203, 1, 20, 'game.exe', 10485760, 10485760)",
            [],
        )
        .expect("insert files");

        // Set active scan
        conn.execute(
            "INSERT INTO scan_state (singleton, active_scan_id) VALUES (1, 1)",
            [],
        )
        .expect("insert scan_state");

        drop(conn);

        // Read candidates
        let games = read_games_with_candidates(temp_db.path()).expect("read candidates");

        assert_eq!(games.len(), 2);

        let game1 = &games[0];
        assert_eq!(game1.game_id, 10);
        assert_eq!(game1.game_name, "Cyber Game 2077");
        assert_eq!(game1.candidate_files.len(), 2);
        assert_eq!(game1.candidate_files[0].rel_path, "audio/voices.pck");
        assert_eq!(
            game1.candidate_files[1].rel_path,
            "content/paks/pakchunk0.pak"
        );
        assert_eq!(
            game1.game_root(),
            PathBuf::from("C:\\Games").join("CyberGame")
        );

        let game2 = &games[1];
        assert_eq!(game2.game_id, 20);
        assert_eq!(game2.game_name, "Space Adventure");
        assert_eq!(game2.candidate_files.len(), 2);
        assert_eq!(game2.candidate_files[0].rel_path, "movies/intro.bk2");
        assert_eq!(game2.candidate_files[1].rel_path, "resources/app.asar");
    }

    #[test]
    fn test_read_non_existent_db() {
        let missing_path = PathBuf::from("C:/non_existent_path_gametrimmer_test.db");
        let res = read_games_with_candidates(&missing_path);
        assert!(res.is_err());
        match res.unwrap_err() {
            DbError::NotFound(p) => assert_eq!(p, missing_path),
            _ => panic!("Expected NotFound error"),
        }
    }

    #[test]
    fn test_read_games_active_scan_filtering() {
        let temp_db = NamedTempFile::new().expect("create temp db file");
        let conn = Connection::open(temp_db.path()).expect("open db");

        conn.execute_batch(
            "
            CREATE TABLE game_libraries (
                id INTEGER PRIMARY KEY,
                vendor TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE
            );
            CREATE TABLE games (
                id INTEGER PRIMARY KEY,
                scan_id INTEGER NOT NULL DEFAULT 1,
                library_id INTEGER NOT NULL REFERENCES game_libraries(id),
                name TEXT NOT NULL,
                install_dir TEXT NOT NULL
            );
            CREATE TABLE files (
                id INTEGER PRIMARY KEY,
                scan_id INTEGER NOT NULL DEFAULT 1,
                game_id INTEGER REFERENCES games(id),
                rel_path TEXT NOT NULL,
                size INTEGER NOT NULL,
                size_on_disk INTEGER
            );
            CREATE TABLE scan_state (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                active_scan_id INTEGER
            );

            INSERT INTO game_libraries (id, vendor, path) VALUES (1, 'steam', 'C:\\Games');
            INSERT INTO games (id, scan_id, library_id, name, install_dir) VALUES
             (1, 1, 1, 'Old Game Generation', 'OldGame'),
             (2, 2, 1, 'Current Game Generation', 'CurrentGame');

            INSERT INTO files (id, scan_id, game_id, rel_path, size, size_on_disk) VALUES
             (101, 1, 1, 'old_audio.pck', 5000, 5000),
             (201, 2, 2, 'current_audio.pck', 8000, 8000);

            -- Active scan set to 2
            INSERT INTO scan_state (singleton, active_scan_id) VALUES (1, 2);
            ",
        )
        .expect("setup tables and data");

        drop(conn);

        let games = read_games_with_candidates(temp_db.path()).expect("read candidates");
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].game_id, 2);
        assert_eq!(games[0].game_name, "Current Game Generation");
        assert_eq!(games[0].candidate_files.len(), 1);
        assert_eq!(games[0].candidate_files[0].rel_path, "current_audio.pck");
    }

    #[test]
    fn test_read_games_missing_scan_state_fallback() {
        let temp_db = NamedTempFile::new().expect("create temp db file");
        let conn = Connection::open(temp_db.path()).expect("open db");

        // Schema without scan_state table
        conn.execute_batch(
            "
            CREATE TABLE game_libraries (
                id INTEGER PRIMARY KEY,
                vendor TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE
            );
            CREATE TABLE games (
                id INTEGER PRIMARY KEY,
                library_id INTEGER NOT NULL REFERENCES game_libraries(id),
                name TEXT NOT NULL,
                install_dir TEXT NOT NULL
            );
            CREATE TABLE files (
                id INTEGER PRIMARY KEY,
                game_id INTEGER REFERENCES games(id),
                rel_path TEXT NOT NULL,
                size INTEGER NOT NULL,
                size_on_disk INTEGER
            );

            INSERT INTO game_libraries (id, vendor, path) VALUES (1, 'gog', 'D:\\GOG Games');
            INSERT INTO games (id, library_id, name, install_dir) VALUES (1, 1, 'Legacy RPG', 'LegacyRPG');
            INSERT INTO files (id, game_id, rel_path, size, size_on_disk) VALUES
             (10, 1, 'sound.bnk', 123456, 123456);
            ",
        )
        .expect("setup tables without scan_state");

        drop(conn);

        let games = read_games_with_candidates(temp_db.path()).expect("fallback read candidates");
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].game_name, "Legacy RPG");
        assert_eq!(games[0].candidate_files.len(), 1);
        assert_eq!(games[0].candidate_files[0].rel_path, "sound.bnk");
    }

    #[test]
    fn test_path_resolution_with_leading_slashes_and_unc() {
        // Test relative install_dir with leading slashes
        let game = GameArchiveCandidates {
            game_id: 1,
            game_name: "Test Game".to_string(),
            install_dir: PathBuf::from("\\LeadingSlashGame"),
            library_path: PathBuf::from("C:\\SteamLibrary\\common"),
            candidate_files: Vec::new(),
        };
        assert_eq!(
            game.game_root(),
            PathBuf::from("C:\\SteamLibrary\\common\\LeadingSlashGame")
        );

        // Test forward slash relative install_dir
        let game_forward = GameArchiveCandidates {
            game_id: 2,
            game_name: "Test Game 2".to_string(),
            install_dir: PathBuf::from("/ForwardSlashGame"),
            library_path: PathBuf::from("D:\\Games"),
            candidate_files: Vec::new(),
        };
        assert_eq!(
            game_forward.game_root(),
            PathBuf::from("D:\\Games\\ForwardSlashGame")
        );

        // Test absolute install_dir
        let game_abs = GameArchiveCandidates {
            game_id: 3,
            game_name: "Test Game Abs".to_string(),
            install_dir: PathBuf::from("E:\\CustomGames\\RPG"),
            library_path: PathBuf::from("C:\\SteamLibrary"),
            candidate_files: Vec::new(),
        };
        assert_eq!(game_abs.game_root(), PathBuf::from("E:\\CustomGames\\RPG"));
    }

    #[test]
    fn test_read_corrupt_db() {
        let temp_db = NamedTempFile::new().expect("create temp db file");
        std::fs::write(temp_db.path(), b"THIS_IS_NOT_A_VALID_SQLITE_HEADER")
            .expect("write corrupt");

        let res = read_games_with_candidates(temp_db.path());
        assert!(res.is_err());
        match res.unwrap_err() {
            DbError::Sqlite(_) => {}
            other => panic!("Expected DbError::Sqlite, got {other:?}"),
        }
    }

    #[test]
    fn test_find_default_db_path() {
        // Just verify it doesn't panic
        let _ = find_default_db_path();
    }

    #[test]
    fn test_read_games_excludes_findings_and_external_single_language_files() {
        let temp_db = NamedTempFile::new().expect("create temp db file");
        let conn = Connection::open(temp_db.path()).expect("open db");

        conn.execute_batch(
            "
            CREATE TABLE game_libraries (
                id INTEGER PRIMARY KEY,
                vendor TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE
            );
            CREATE TABLE games (
                id INTEGER PRIMARY KEY,
                scan_id INTEGER NOT NULL DEFAULT 1,
                library_id INTEGER NOT NULL REFERENCES game_libraries(id),
                name TEXT NOT NULL,
                install_dir TEXT NOT NULL
            );
            CREATE TABLE files (
                id INTEGER PRIMARY KEY,
                scan_id INTEGER NOT NULL DEFAULT 1,
                game_id INTEGER REFERENCES games(id),
                rel_path TEXT NOT NULL,
                size INTEGER NOT NULL,
                size_on_disk INTEGER
            );
            CREATE TABLE findings (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL,
                category TEXT NOT NULL
            );

            INSERT INTO game_libraries (id, vendor, path) VALUES (1, 'steam', 'C:\\Games');
            INSERT INTO games (id, scan_id, library_id, name, install_dir) VALUES
             (1, 1, 1, 'Plague Tale', 'PlagueTale'),
             (2, 1, 1, 'Benchmark Tool', 'BenchmarkTool');

            INSERT INTO files (id, scan_id, game_id, rel_path, size, size_on_disk) VALUES
             -- Monolithic internal archive (should be kept)
             (10, 1, 1, 'SOUNDBANKS/VO_AMICIA_MEDIA.PC.PCK', 200000, 200000),
             -- External single language file (should be excluded by path matcher)
             (11, 1, 1, 'sounds_fra.pck', 50000, 50000),
             -- External single language file in locales folder (excluded by path)
             (20, 1, 2, 'bin/x64/locales/ar.pak', 10000, 10000),
             -- File in findings table categorized as localization (excluded by SQL filter)
             (21, 1, 2, 'data/custom_audio.pck', 30000, 30000);

            INSERT INTO findings (id, file_id, category) VALUES (1, 21, 'localization');
            ",
        )
        .expect("setup tables and data");

        drop(conn);

        let games = read_games_with_candidates(temp_db.path()).expect("read candidates");

        // Game 1 should only have the monolithic VO_AMICIA_MEDIA.PC.PCK
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].game_id, 1);
        assert_eq!(games[0].candidate_files.len(), 1);
        assert_eq!(
            games[0].candidate_files[0].rel_path,
            "SOUNDBANKS/VO_AMICIA_MEDIA.PC.PCK"
        );
    }
}
