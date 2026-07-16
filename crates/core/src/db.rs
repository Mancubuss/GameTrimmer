use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;

/// SQL schema for the GameTrimmer database. Idempotent (`IF NOT EXISTS`).
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS game_libraries (
    id     INTEGER PRIMARY KEY,
    vendor TEXT NOT NULL,
    path   TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS games (
    id          INTEGER PRIMARY KEY,
    library_id  INTEGER NOT NULL REFERENCES game_libraries(id),
    name        TEXT NOT NULL,
    install_dir TEXT NOT NULL,
    app_id      TEXT
);

CREATE TABLE IF NOT EXISTS files (
    id       INTEGER PRIMARY KEY,
    game_id  INTEGER REFERENCES games(id),
    rel_path TEXT NOT NULL,
    size     INTEGER NOT NULL,
    mtime    INTEGER
);

CREATE TABLE IF NOT EXISTS findings (
    file_id    INTEGER NOT NULL REFERENCES files(id),
    category   TEXT NOT NULL,
    rule_id    TEXT,
    confidence INTEGER NOT NULL,
    lang_tag   TEXT
);

CREATE TABLE IF NOT EXISTS operations (
    id       INTEGER PRIMARY KEY,
    ts       INTEGER NOT NULL,
    action   TEXT NOT NULL,
    src_path TEXT NOT NULL,
    dst_path TEXT,
    status   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_files_game_id     ON files(game_id);
CREATE INDEX IF NOT EXISTS idx_findings_file_id  ON findings(file_id);
";

/// Opens (or creates) the database at `path` and applies the schema.
pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    configure(&conn)?;
    apply_schema(&conn)?;
    Ok(conn)
}

/// Opens an in-memory database with the schema applied. Intended for tests.
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    apply_schema(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    // `journal_mode` returns a row, so use `pragma_update` instead of `execute`.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // With WAL, `synchronous=NORMAL` only fsyncs at checkpoints instead of on
    // every commit - safe here (a crash loses at most the last few WAL
    // frames, never corrupts the DB) and removes the dominant per-transaction
    // cost for a scan that commits once per game (or per batch of games).
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    // Bigger page cache (in KiB, negative = KiB rather than pages) so a full
    // rescan's repeated `files`/`findings` writes for the same game hit
    // cache instead of round-tripping to disk.
    conn.pragma_update(None, "cache_size", -20_000i64)?;
    Ok(())
}

fn apply_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

/// Compacts the database: truncates the WAL and rebuilds the main file.
///
/// Deletes accumulate free pages in the main file over time (rows removed
/// after a "Видалити вибране" run don't shrink the file on disk). `VACUUM`
/// rebuilds the file without that free space, but it cannot see rows still
/// sitting in the WAL, so `wal_checkpoint(TRUNCATE)` runs first to fold the
/// WAL back into the main file and shrink the WAL file itself to zero bytes.
pub fn compact(conn: &Connection) -> Result<()> {
    // Like `journal_mode` in `configure()`, `wal_checkpoint` returns a row
    // (busy, log frames, checkpointed frames) - `execute`/`execute_batch`
    // would fail with "query returned unexpected row", so use `query_row`.
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    conn.execute("VACUUM", [])?;
    // `VACUUM` itself writes through the WAL, re-inflating the file that the
    // first checkpoint just truncated. A second checkpoint folds those pages
    // back in, so the on-disk result doesn't depend on this being the last
    // open connection (SQLite only auto-truncates on final close).
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED_TABLES: [&str; 6] = [
        "game_libraries",
        "games",
        "files",
        "findings",
        "operations",
        "settings",
    ];

    #[test]
    fn open_in_memory_creates_all_tables() {
        let conn = open_in_memory().expect("in-memory db should open");

        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .expect("query sqlite_master");
        let tables: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("map rows")
            .collect::<std::result::Result<_, _>>()
            .expect("collect table names");

        for expected in EXPECTED_TABLES {
            assert!(
                tables.iter().any(|t| t == expected),
                "table `{expected}` is missing, got: {tables:?}"
            );
        }
    }

    #[test]
    fn insert_and_read_back_library() {
        let conn = open_in_memory().expect("in-memory db should open");

        conn.execute(
            "INSERT INTO game_libraries (vendor, path) VALUES (?1, ?2)",
            ("steam", "D:/SteamLibrary"),
        )
        .expect("insert library");

        let (vendor, path): (String, String) = conn
            .query_row(
                "SELECT vendor, path FROM game_libraries WHERE path = ?1",
                ["D:/SteamLibrary"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read library back");

        assert_eq!(vendor, "steam");
        assert_eq!(path, "D:/SteamLibrary");
    }

    #[test]
    fn reopening_same_path_is_idempotent() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("gametrimmer.db");

        {
            let conn = open(&db_path).expect("first open should succeed");
            conn.execute(
                "INSERT INTO game_libraries (vendor, path) VALUES (?1, ?2)",
                ("gog", "E:/GOG Games"),
            )
            .expect("insert library");
        }

        let conn = open(&db_path).expect("second open should not fail");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM game_libraries", [], |row| row.get(0))
            .expect("count libraries");
        assert_eq!(count, 1, "data must survive reopen");
    }

    /// Deleting a large batch of rows leaves free pages behind; `compact`
    /// must reclaim them (shrinking `page_count`) without touching data
    /// written before the delete.
    #[test]
    fn compact_shrinks_page_count_and_preserves_earlier_data() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("gametrimmer.db");
        let conn = open(&db_path).expect("open file-backed db");

        conn.execute(
            "INSERT INTO game_libraries (vendor, path) VALUES (?1, ?2)",
            ("steam", "D:/SteamLibrary"),
        )
        .expect("insert library");

        for i in 0..5_000 {
            conn.execute(
                "INSERT INTO files (game_id, rel_path, size, mtime) VALUES (NULL, ?1, ?2, NULL)",
                (format!("file_{i}.txt"), i as i64),
            )
            .expect("insert file");
        }

        let page_count_before: i64 = conn
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .expect("read page_count before compact");

        conn.execute("DELETE FROM files", [])
            .expect("delete all files");

        compact(&conn).expect("compact should succeed");

        let page_count_after: i64 = conn
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .expect("read page_count after compact");

        assert!(
            page_count_after < page_count_before,
            "compact should shrink page_count: before={page_count_before}, after={page_count_after}"
        );

        let (vendor, path): (String, String) = conn
            .query_row(
                "SELECT vendor, path FROM game_libraries WHERE path = ?1",
                ["D:/SteamLibrary"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("data written before compaction must survive");
        assert_eq!(vendor, "steam");
        assert_eq!(path, "D:/SteamLibrary");
    }
}
