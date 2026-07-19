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

/// Folds the WAL back into the main database file and shrinks the WAL file
/// itself to zero bytes. Cheap (no file rewrite) - safe to run unconditionally
/// before deciding whether a full `compact` is worthwhile.
pub fn checkpoint_truncate(conn: &Connection) -> Result<()> {
    // Like `journal_mode` in `configure()`, `wal_checkpoint` returns a row
    // (busy, log frames, checkpointed frames) - `execute`/`execute_batch`
    // would fail with "query returned unexpected row", so use `query_row`.
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
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
    checkpoint_truncate(conn)?;
    conn.execute("VACUUM", [])?;
    // `VACUUM` itself writes through the WAL, re-inflating the file that the
    // first checkpoint just truncated. A second checkpoint folds those pages
    // back in, so the on-disk result doesn't depend on this being the last
    // open connection (SQLite only auto-truncates on final close).
    checkpoint_truncate(conn)?;
    Ok(())
}

/// Number of VDBE instructions between successive `progress_handler` ticks
/// (see `compact_observed`). Smaller means finer-grained progress but more
/// callback overhead during `VACUUM`; 10,000 was the value used to calibrate
/// `TICKS_PER_PAGE_ESTIMATE` below, so it must not change without
/// re-calibrating that constant.
const PROGRESS_NUM_OPS: std::os::raw::c_int = 10_000;

/// Empirically measured VDBE ticks (at `PROGRESS_NUM_OPS` = 10,000) per
/// database page rewritten by `VACUUM`. Measured with the ignored
/// `calibrate_ticks_per_page` test below at two scales: 50,000 `files` rows
/// (468 pages after checkpoint) logged 22 ticks = 0.0470 ticks/page, and
/// 200,000 rows (1,943 pages) logged 90 ticks = 0.0463 ticks/page - close
/// agreement across a ~4x change in page count. Rounded up to 0.05 (rather
/// than down) on purpose: this is only ever an estimate (`VACUUM`'s actual
/// instruction count depends on row/index shape, not just page count), and
/// `compact_observed` clamps the reported fraction at 0.99, so a slight
/// overestimate here makes the bar undershoot and jump straight to "done" -
/// better than overshooting and sitting at 99-100% while `VACUUM` is still
/// running.
const TICKS_PER_PAGE_ESTIMATE: f64 = 0.05;

/// Runs `compact` while reporting approximate progress in `[0.0, 1.0]`.
///
/// SQLite has no native `VACUUM` progress API; this installs a
/// `progress_handler` and estimates completion from the number of callback
/// ticks expected for the file's page count (see `TICKS_PER_PAGE_ESTIMATE`).
/// `on_progress` is only invoked when the integer percent changes, so
/// callers don't need their own throttling.
pub fn compact_observed(
    conn: &Connection,
    mut on_progress: impl FnMut(f64) + Send + 'static,
) -> Result<()> {
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let estimated_total_ticks = (page_count as f64 * TICKS_PER_PAGE_ESTIMATE).max(1.0);

    let mut ticks: u64 = 0;
    let mut last_percent: i64 = -1;
    conn.progress_handler(
        PROGRESS_NUM_OPS,
        Some(move || {
            ticks += 1;
            let fraction = (ticks as f64 / estimated_total_ticks).min(0.99);
            let percent = (fraction * 100.0) as i64;
            if percent != last_percent {
                last_percent = percent;
                on_progress(fraction);
            }
            // Never abort the VACUUM - this handler only observes progress.
            false
        }),
    )?;

    let result = compact(conn);

    // Always uninstall the handler before returning, success or not - it
    // must not outlive the closure's borrows into a later, unrelated query
    // on this same connection.
    let _ = conn.progress_handler(0, None::<fn() -> bool>);

    result
}

/// Fraction of the main database file occupied by free (reclaimable)
/// pages - what `VACUUM` would give back. Instant: SQLite tracks the
/// freelist itself (`PRAGMA freelist_count` / `PRAGMA page_count`).
pub fn free_page_fraction(conn: &Connection) -> Result<f64> {
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    if page_count == 0 {
        return Ok(0.0);
    }
    let freelist_count: i64 = conn.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
    Ok(freelist_count as f64 / page_count as f64)
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

    /// A fresh database has (essentially) no free pages to reclaim.
    #[test]
    fn free_page_fraction_is_near_zero_for_fresh_db() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("gametrimmer.db");
        let conn = open(&db_path).expect("open file-backed db");

        let fraction = free_page_fraction(&conn).expect("read free_page_fraction");
        assert!(
            fraction < 0.01,
            "fresh db should have ~0 free fraction, got {fraction}"
        );
    }

    /// Deleting a large batch of rows and folding the WAL back in (without a
    /// full `VACUUM`) leaves the freed pages sitting in the main file's
    /// freelist - `free_page_fraction` must report that as a high fraction.
    #[test]
    fn free_page_fraction_is_high_after_delete_and_checkpoint() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("gametrimmer.db");
        let conn = open(&db_path).expect("open file-backed db");

        for i in 0..5_000 {
            conn.execute(
                "INSERT INTO files (game_id, rel_path, size, mtime) VALUES (NULL, ?1, ?2, NULL)",
                (format!("file_{i}.txt"), i as i64),
            )
            .expect("insert file");
        }

        conn.execute("DELETE FROM files", [])
            .expect("delete all files");
        checkpoint_truncate(&conn).expect("checkpoint should succeed");

        let fraction = free_page_fraction(&conn).expect("read free_page_fraction");
        assert!(
            fraction > 0.5,
            "post-delete db should have a high free fraction, got {fraction}"
        );
    }

    /// After a full `compact`, the freelist is empty again - `VACUUM`
    /// rebuilds the file without the reclaimable pages.
    #[test]
    fn free_page_fraction_drops_after_compact() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("gametrimmer.db");
        let conn = open(&db_path).expect("open file-backed db");

        for i in 0..5_000 {
            conn.execute(
                "INSERT INTO files (game_id, rel_path, size, mtime) VALUES (NULL, ?1, ?2, NULL)",
                (format!("file_{i}.txt"), i as i64),
            )
            .expect("insert file");
        }

        conn.execute("DELETE FROM files", [])
            .expect("delete all files");
        compact(&conn).expect("compact should succeed");

        let fraction = free_page_fraction(&conn).expect("read free_page_fraction");
        assert!(
            fraction < 0.01,
            "compacted db should have ~0 free fraction, got {fraction}"
        );
    }

    /// Not run in normal `cargo test` - this is the experiment used to derive
    /// `TICKS_PER_PAGE_ESTIMATE`. Re-run with
    /// `cargo test -p gametrimmer-core calibrate_ticks_per_page -- --ignored --nocapture`
    /// after changing `PROGRESS_NUM_OPS` or the schema, and update the
    /// constant (and its doc comment) from the printed ratio.
    #[test]
    #[ignore]
    fn calibrate_ticks_per_page() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("gametrimmer.db");
        let conn = open(&db_path).expect("open file-backed db");

        for i in 0..200_000 {
            conn.execute(
                "INSERT INTO files (game_id, rel_path, size, mtime) VALUES (NULL, ?1, ?2, NULL)",
                (format!("file_{i}.txt"), i as i64),
            )
            .expect("insert file");
        }
        conn.execute("DELETE FROM files WHERE id % 2 = 0", [])
            .expect("delete half the rows");
        checkpoint_truncate(&conn).expect("checkpoint before measuring page_count");

        let page_count: i64 = conn
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .expect("read page_count");

        // `progress_handler` requires `Send + 'static`, so the counter needs
        // an `Arc<AtomicU64>` rather than a plain `Rc<Cell<_>>`.
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counter_cb = std::sync::Arc::clone(&counter);
        conn.progress_handler(
            PROGRESS_NUM_OPS,
            Some(move || {
                counter_cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                false
            }),
        )
        .expect("install progress handler");

        compact(&conn).expect("compact should succeed");
        let _ = conn.progress_handler(0, None::<fn() -> bool>);

        let total_ticks = counter.load(std::sync::atomic::Ordering::Relaxed);
        let ticks_per_page = total_ticks as f64 / page_count as f64;
        println!(
            "page_count={page_count} num_ops={PROGRESS_NUM_OPS} total_ticks={total_ticks} \
             ticks_per_page={ticks_per_page:.4}"
        );
    }

    /// `compact_observed` must report a monotonically non-decreasing sequence
    /// of fractions in `[0.0, 1.0]` and must actually complete the `VACUUM`
    /// (mirrors `compact_shrinks_page_count_and_preserves_earlier_data`, plus
    /// the progress-reporting contract).
    #[test]
    fn compact_observed_reports_monotonic_progress_and_completes() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("gametrimmer.db");
        let conn = open(&db_path).expect("open file-backed db");

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

        let reported = std::sync::Arc::new(std::sync::Mutex::new(Vec::<f64>::new()));
        let reported_cb = std::sync::Arc::clone(&reported);
        compact_observed(&conn, move |fraction| {
            reported_cb.lock().expect("lock reported").push(fraction);
        })
        .expect("compact_observed should succeed");

        let page_count_after: i64 = conn
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .expect("read page_count after compact");
        assert!(
            page_count_after < page_count_before,
            "compact_observed should shrink page_count: before={page_count_before}, \
             after={page_count_after}"
        );

        let values = reported.lock().expect("lock reported").clone();
        for window in values.windows(2) {
            assert!(
                window[1] >= window[0],
                "progress must be non-decreasing, got {values:?}"
            );
        }
        for &fraction in &values {
            assert!(
                (0.0..=1.0).contains(&fraction),
                "progress must be in [0.0, 1.0], got {fraction} in {values:?}"
            );
        }
    }
}
