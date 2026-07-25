use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use crate::error::{CoreError, Result};

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
    app_id      TEXT,
    build_id    TEXT
);

CREATE TABLE IF NOT EXISTS files (
    id           INTEGER PRIMARY KEY,
    game_id      INTEGER REFERENCES games(id),
    rel_path     TEXT NOT NULL,
    size         INTEGER NOT NULL,
    size_on_disk INTEGER,
    mtime        INTEGER
);

CREATE TABLE IF NOT EXISTS findings (
    file_id    INTEGER NOT NULL REFERENCES files(id),
    category   TEXT NOT NULL,
    rule_id    TEXT,
    confidence INTEGER NOT NULL,
    lang_tag   TEXT,
    group_dir  TEXT
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
    migrate(&conn)?;
    Ok(conn)
}

/// Opens an in-memory database with the schema applied. Intended for tests.
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    apply_schema(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    configure_journal_mode(conn)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // With WAL, `synchronous=NORMAL` only fsyncs at checkpoints instead of on
    // every commit - safe here (a crash loses at most the last few WAL
    // frames, never corrupts the DB) and removes the dominant per-transaction
    // cost for a scan that commits once per game (or per batch of games).
    // Still safe (if more conservative than necessary) if `journal_mode`
    // fell back to `DELETE` below - `synchronous=NORMAL` with a rollback
    // journal only risks losing the *last* transaction on power loss, never
    // corruption.
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    // Bigger page cache (in KiB, negative = KiB rather than pages) so a full
    // rescan's repeated `files`/`findings` writes for the same game hit
    // cache instead of round-tripping to disk.
    conn.pragma_update(None, "cache_size", -20_000i64)?;
    // Keep SQLite's own temp files (used by `VACUUM`, large sorts, and
    // transient indices) in memory rather than the OS temp directory.
    // Without this, `compact()`'s `VACUUM` can spill to `%TEMP%` - the one
    // way the app's state could leak outside its own portable folder (see
    // docs/portability-audit.md, finding #10). `MEMORY` is fine here: the
    // database itself is already file-backed, so this only affects
    // short-lived scratch space, not durability.
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(())
}

/// Requests WAL journal mode and reads back what SQLite actually applied.
///
/// SQLite does not error when it can't honor `journal_mode=WAL` (e.g. on a
/// network share, or certain FAT32 removable media where memory-mapping the
/// `-wal` file isn't supported) - it silently keeps the previous mode
/// instead. Left unchecked, `pragma_update` (which ignores the returned
/// value) would hide that: the app would believe it configured WAL while
/// SQLite quietly stayed on its prior journal mode. This reads the mode
/// back and, if it isn't `wal`, explicitly downgrades to `DELETE` - SQLite's
/// original rollback-journal default, supported everywhere a file can be
/// written at all - so the effective mode is always one this codebase
/// actually intended, not whatever SQLite happened to leave in place.
fn configure_journal_mode(conn: &Connection) -> Result<()> {
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        conn.pragma_update(None, "journal_mode", "DELETE")?;
    }
    Ok(())
}

fn apply_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

/// Brings an already-existing database file up to the current schema.
///
/// `apply_schema`'s `CREATE TABLE IF NOT EXISTS` is a no-op for a table that
/// already exists, so a column added to the schema after a user's database
/// was first created never appears through that path alone - it needs an
/// explicit `ALTER TABLE`. `findings.group_dir` (the persisted UI folder-
/// grouping key, added so startup load no longer has to recompute it from the
/// full file list) is such a column: a fresh database gets it from `SCHEMA`,
/// while a database from an earlier build gets it here. The added column is
/// `NULL` for every pre-existing finding, which is exactly "no group" - the
/// next scan repopulates real values, and load treats `NULL` as an
/// ungrouped (orphan) finding in the meantime.
fn migrate(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "findings", "group_dir", "TEXT")?;
    // `files.size_on_disk` (GT-05a): the on-disk allocated size, added so
    // sizes shown and reported match Explorer's "Size on disk" instead of the
    // logical total. `NULL` for every file indexed by an earlier build; load
    // falls back to `size` (via COALESCE) for those until the next scan
    // repopulates the real on-disk figure.
    add_column_if_missing(conn, "files", "size_on_disk", "INTEGER")?;
    // `games.build_id` (GT-09): the launcher's content build id recorded at
    // scan time, so a later startup can tell whether a game was updated or
    // re-verified (and therefore may have its trimmed files back) without
    // rescanning. `NULL` for every game indexed by an earlier build - and
    // deliberately treated as "unknown, claim nothing" by
    // `gamestate::changed_games`, so the first run after an upgrade does not
    // flag the whole library.
    add_column_if_missing(conn, "games", "build_id", "TEXT")?;
    Ok(())
}

/// Adds `column` (`decl` is its SQL type) to `table` if the table exists and
/// the column doesn't. The table-existence guard matters only for the
/// partial-schema migration unit tests: the real `open()` path always runs
/// `apply_schema` (which creates every table) before `migrate`, so in
/// production every target table is guaranteed present. `table`/`column`/`decl`
/// are hardcoded literals at every call site (never user input), so
/// interpolating them into DDL - which can't take bound parameters for
/// identifiers - is safe.
fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    if table_exists(conn, table)? && !column_exists(conn, table, column)? {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )?;
    }
    Ok(())
}

/// Whether a table named `table` exists in the database.
fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Whether `table` has a column named `column`, via `PRAGMA table_info`. The
/// table name is a hardcoded literal at every call site (never user input),
/// so interpolating it into the pragma text - which cannot take a bound
/// parameter for the table name - is safe here.
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    // `table_info` column 1 is the column name.
    let mut found = false;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            found = true;
            break;
        }
    }
    Ok(found)
}

/// Runs `f` with SQLite foreign-key enforcement disabled on `conn`, then
/// restores it afterward - always, even if `f` returns `Err`.
///
/// This exists for bulk wipes. With `PRAGMA foreign_keys = ON` (the app's
/// default - see [`configure`]), deleting a parent row makes SQLite verify no
/// child row still references it, one index probe per deleted row, and a
/// `DELETE FROM t` with no `WHERE` can no longer use its whole-table
/// "truncate" fast path at all. Turning enforcement off for the duration of a
/// controlled, whole-subtree delete (where the caller already deletes
/// child-before-parent, so integrity holds without the checks) turns an
/// O(rows) row-by-row wipe into a near-constant-time one.
///
/// The `foreign_keys` pragma is a silent no-op while a transaction is open,
/// so it must be toggled with no `BEGIN`/`SAVEPOINT` active: `f` is expected
/// to open (and commit) its own transaction strictly between the two toggles,
/// never to leave one pending. A failure to *restore* enforcement is only
/// surfaced when `f` itself succeeded, so it can't mask `f`'s own error.
pub fn with_foreign_keys_off<T>(
    conn: &Connection,
    f: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let result = f(conn);
    let restore = conn.pragma_update(None, "foreign_keys", "ON");
    match result {
        // `restore` is a `rusqlite::Result`; `?` converts its error into
        // `CoreError` when `f` itself succeeded (a restore failure is only
        // surfaced here, never masking `f`'s own error on the `Err` arm).
        Ok(value) => Ok(restore.map(|()| value)?),
        Err(err) => Err(err),
    }
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

/// Deletes all scan results - `findings`, `files`, `games` - plus the
/// `operations` journal, in one transaction, while leaving `game_libraries`
/// and `settings` untouched.
///
/// This backs the "Clear database" action: unlike a single deleted game's
/// files (handled row-by-row elsewhere), the user wants to throw away
/// *everything* a scan ever found and start clean, without losing the
/// libraries they registered or the preferences they configured.
///
/// The wipe runs inside [`with_foreign_keys_off`], which is what makes it
/// fast: with enforcement on, each of these `DELETE FROM t` statements would
/// have to walk and integrity-check every row (millions, on a real library),
/// but with it off a `WHERE`-less delete drops the whole table b-tree in one
/// step. Integrity is preserved by clearing children before parents
/// (`findings` -> `files` -> `games`) regardless. The delete is one
/// transaction so a mid-wipe failure can't leave the tables partially
/// cleared. The freed pages are reclaimed separately by the caller's
/// checkpoint + `VACUUM` (see `worker::clear`).
pub fn clear_scan_data(conn: &Connection) -> Result<()> {
    with_foreign_keys_off(conn, |conn| {
        let tx = conn.unchecked_transaction()?;
        tx.execute("DELETE FROM findings", [])?;
        tx.execute("DELETE FROM files", [])?;
        tx.execute("DELETE FROM games", [])?;
        tx.execute("DELETE FROM operations", [])?;
        tx.commit()?;
        Ok(())
    })
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

/// Total bytes occupied on disk by every scanned game, grouped by the
/// library each game belongs to. Aggregated live from the `files` table (a
/// single grouped SUM) rather than a persisted column, so nothing has to be
/// kept in sync as files change - the next scan/load recomputes it.
/// Libraries with no files (or no games) are simply absent from the map;
/// callers treat an absent library as 0 bytes.
pub fn occupied_by_library(conn: &Connection) -> Result<HashMap<i64, u64>> {
    // On-disk allocation (GT-05a), falling back to the logical size for rows
    // indexed before `size_on_disk` existed - so occupancy matches the honest
    // per-finding figures the tree sums, not the logical total.
    let mut stmt = conn.prepare(
        "SELECT g.library_id, SUM(COALESCE(f.size_on_disk, f.size)) FROM files f \
         JOIN games g ON g.id = f.game_id \
         GROUP BY g.library_id",
    )?;
    let mut by_library = HashMap::new();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let library_id: i64 = row.get(0)?;
        let total: i64 = row.get(1)?;
        by_library.insert(library_id, total as u64);
    }
    Ok(by_library)
}

/// Whether `err` is a genuine SQLite-reported corruption signal - "database
/// disk image is malformed" (`SQLITE_CORRUPT`) or "file is not a database"
/// (`SQLITE_NOTADB`, e.g. a zero-byte/truncated file, or a non-SQLite file at
/// the path). This is the gate for [`rebuild_database`]: anything else
/// (busy, locked, disk full, ...) is a transient or unrelated failure that
/// destructive recovery must never be triggered for.
pub fn is_corruption_error(err: &CoreError) -> bool {
    match err {
        CoreError::Db(rusqlite::Error::SqliteFailure(e, _)) => matches!(
            e.code,
            rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
        ),
        _ => false,
    }
}

/// Destructive recovery for a database file SQLite can no longer read (see
/// [`is_corruption_error`]). This is the safety net behind "Clear database":
/// if the clear operation itself fails because the file is unusable, there
/// is no SQL fix for that - the file has to be replaced.
///
/// Recovery is deliberately narrow and best-effort:
/// - `game_libraries` (the user's registered libraries) and `settings`
///   (their preferences) are salvaged if at all readable.
/// - Scan data (`games`, `files`, `findings`, `operations`) is intentionally
///   NOT salvaged - it is exactly what "Clear database" throws away anyway,
///   and may well be the corrupt part of the file in the first place.
///
/// Steps: best-effort salvage of the two tables above from the existing file
/// (swallowing any error into "nothing salvaged"); build the replacement in a
/// temporary sibling file first and only touch the original once it is fully
/// populated - so a mid-rebuild failure (disk full, permission denied, ...)
/// leaves the known-bad-but-present original in place rather than deleting it
/// with no working replacement. Concretely: populate `db_path.rebuild-tmp`,
/// checkpoint + drop it, then delete the original database file (and its
/// `-wal`/`-shm`/`-journal` sidecars) and rename the temp file into place.
pub fn rebuild_database(db_path: &Path) -> Result<Connection> {
    let (libraries, settings) = salvage_libraries_and_settings(db_path);

    // Build the replacement alongside the original, not over it. A leftover
    // temp from a previously interrupted rebuild must not be reused, so its
    // whole file set is cleared first.
    let tmp_path = sidecar_path(db_path, ".rebuild-tmp");
    delete_database_files(&tmp_path)?;

    {
        let conn = open(&tmp_path)?;
        for (vendor, path) in &libraries {
            conn.execute(
                "INSERT OR IGNORE INTO game_libraries (vendor, path) VALUES (?1, ?2)",
                params![vendor, path],
            )?;
        }
        for (key, value) in &settings {
            conn.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
        }
        // Fold the temp WAL back into the main temp file so it can be moved
        // as a single file, then drop the connection to release its lock
        // before the rename below.
        checkpoint_truncate(&conn)?;
    }
    // Post-checkpoint the temp sidecars are empty/absent; make sure none tag
    // along with the rename of the bare `.rebuild-tmp` file.
    delete_if_exists(&sidecar_path(&tmp_path, "-wal"))?;
    delete_if_exists(&sidecar_path(&tmp_path, "-shm"))?;

    // The replacement is ready: only now is it safe to drop the original and
    // move the fresh file into its place.
    delete_database_files(db_path)?;
    std::fs::rename(&tmp_path, db_path)?;

    open(db_path)
}

/// Deletes a SQLite database file together with every sidecar it may have
/// left behind - `-wal`/`-shm` (WAL mode) and `-journal` (the `DELETE`
/// rollback-journal fallback, see [`configure_journal_mode`]). A missing file
/// counts as success (see [`delete_if_exists`]).
fn delete_database_files(db_path: &Path) -> Result<()> {
    delete_if_exists(db_path)?;
    delete_if_exists(&sidecar_path(db_path, "-wal"))?;
    delete_if_exists(&sidecar_path(db_path, "-shm"))?;
    delete_if_exists(&sidecar_path(db_path, "-journal"))?;
    Ok(())
}

/// Rows salvaged from a two-text-column table (`(vendor, path)` for
/// `game_libraries`, `(key, value)` for `settings`) - named so
/// `salvage_libraries_and_settings`'s signature doesn't trip clippy's
/// `type_complexity` lint on the nested tuple-of-`Vec`-of-tuples.
type TextRows = Vec<(String, String)>;

/// Best-effort read of `game_libraries` (vendor, path) and `settings` (key,
/// value) from a database file that may be partially or fully unreadable.
/// Opened and dropped in its own scope so the connection - and its file
/// lock - does not outlive this function, since [`rebuild_database`] must
/// delete the file right after this returns.
fn salvage_libraries_and_settings(db_path: &Path) -> (TextRows, TextRows) {
    let libraries;
    let settings;
    {
        let conn = match open(db_path) {
            Ok(conn) => conn,
            Err(_) => return (Vec::new(), Vec::new()),
        };
        libraries = salvage_two_text_columns(&conn, "SELECT vendor, path FROM game_libraries");
        settings = salvage_two_text_columns(&conn, "SELECT key, value FROM settings");
    } // `conn` drops here, releasing its file handle before the file is deleted.
    (libraries, settings)
}

/// Runs `sql` (expected to select exactly two text columns) and collects the
/// rows, swallowing any error - including a mid-query corruption hit - into
/// an empty result. Salvage is best-effort, never fatal.
fn salvage_two_text_columns(conn: &Connection, sql: &str) -> TextRows {
    (|| -> rusqlite::Result<Vec<(String, String)>> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect()
    })()
    .unwrap_or_default()
}

/// Path to one of a SQLite database's WAL-mode sidecar files - the suffix is
/// appended to the whole filename (not the extension), matching SQLite's own
/// naming (`mydb.db` -> `mydb.db-wal` / `mydb.db-shm`).
fn sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut name = db_path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Deletes `path` if it exists. A missing file counts as success - the
/// `-wal`/`-shm` sidecars are routinely absent (e.g. right after a
/// checkpoint) - but any other failure (e.g. still locked by another
/// process) propagates.
fn delete_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
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

    /// `temp_store=MEMORY` must be in effect after `open` - this is the
    /// portability guarantee from finding #10 in `docs/portability-audit.md`:
    /// `VACUUM`/large sorts must not spill temp files into `%TEMP%`.
    #[test]
    fn open_sets_temp_store_to_memory() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("gametrimmer.db");
        let conn = open(&db_path).expect("open file-backed db");

        let temp_store: i64 = conn
            .query_row("PRAGMA temp_store", [], |row| row.get(0))
            .expect("read temp_store");
        // SQLite reports `temp_store` numerically: 0=default, 1=file, 2=memory.
        assert_eq!(temp_store, 2, "temp_store must be MEMORY (2)");
    }

    /// On an ordinary local filesystem (the common case, and what CI runs
    /// on), WAL must actually take - this pins the happy path so a
    /// regression in `configure_journal_mode` that always fell back to
    /// `DELETE` would be caught.
    #[test]
    fn open_uses_wal_journal_mode_on_ordinary_filesystem() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("gametrimmer.db");
        let conn = open(&db_path).expect("open file-backed db");

        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("read journal_mode");
        assert_eq!(mode.to_lowercase(), "wal");
    }

    /// SQLite cannot use WAL for `:memory:` databases - it silently keeps
    /// `journal_mode=memory` no matter what is requested. This exercises the
    /// fallback branch of `configure_journal_mode` (the "WAL didn't take"
    /// path) without needing a real network share or FAT32 volume: `open`
    /// must still succeed rather than propagating an error, exactly as it
    /// should on the exotic media described in finding #11.
    #[test]
    fn configure_does_not_fail_when_wal_is_unavailable() {
        let conn = open_in_memory().expect("in-memory db should open despite no WAL support");

        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("read journal_mode");
        assert_ne!(
            mode.to_lowercase(),
            "wal",
            "sanity check: :memory: databases are not expected to support WAL"
        );
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

    /// `clear_scan_data` must wipe every scan-produced table (`findings`,
    /// `files`, `games`, `operations`) while leaving `game_libraries` (what
    /// the user registered) and `settings` (their preferences) exactly as
    /// they were - the whole point of "Clear database" is to reset scan
    /// state without forcing the user to re-add libraries or reconfigure.
    #[test]
    fn clear_scan_data_wipes_scan_tables_but_keeps_libraries_and_settings() {
        let conn = open_in_memory().expect("in-memory db should open");

        conn.execute(
            "INSERT INTO game_libraries (vendor, path) VALUES (?1, ?2)",
            ("steam", "D:/SteamLibrary"),
        )
        .expect("insert library");
        conn.execute(
            "INSERT INTO games (library_id, name, install_dir, app_id) VALUES (1, ?1, ?2, ?3)",
            ("Some Game", "D:/SteamLibrary/Some Game", "12345"),
        )
        .expect("insert game");
        conn.execute(
            "INSERT INTO files (game_id, rel_path, size, mtime) VALUES (1, ?1, ?2, ?3)",
            ("redist/setup.exe", 1024i64, 0i64),
        )
        .expect("insert file");
        conn.execute(
            "INSERT INTO findings (file_id, category, rule_id, confidence, lang_tag) \
             VALUES (1, ?1, ?2, ?3, ?4)",
            ("redist", "rule_1", 90i64, Option::<&str>::None),
        )
        .expect("insert finding");
        conn.execute(
            "INSERT INTO operations (ts, action, src_path, dst_path, status) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                0i64,
                "delete",
                "D:/SteamLibrary/Some Game/redist/setup.exe",
                Option::<&str>::None,
                "ok",
            ),
        )
        .expect("insert operation");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)",
            ("app_language", "en"),
        )
        .expect("insert setting");

        clear_scan_data(&conn).expect("clear_scan_data should succeed");

        let count = |table: &str| -> i64 {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count rows")
        };

        assert_eq!(count("findings"), 0, "findings must be wiped");
        assert_eq!(count("files"), 0, "files must be wiped");
        assert_eq!(count("games"), 0, "games must be wiped");
        assert_eq!(count("operations"), 0, "operations must be wiped");
        assert_eq!(
            count("game_libraries"),
            1,
            "game_libraries must survive clear_scan_data"
        );
        assert_eq!(
            count("settings"),
            1,
            "settings must survive clear_scan_data"
        );
    }

    /// `migrate` must add `findings.group_dir` to a database created before
    /// that column existed, and be idempotent (safe to run on an already-
    /// migrated database without erroring on a duplicate column).
    #[test]
    fn migrate_adds_group_dir_to_a_legacy_findings_table_and_is_idempotent() {
        let conn = Connection::open_in_memory().expect("open bare in-memory db");
        // The pre-`group_dir` schema.
        conn.execute_batch(
            "CREATE TABLE findings (
                file_id    INTEGER NOT NULL,
                category   TEXT NOT NULL,
                rule_id    TEXT,
                confidence INTEGER NOT NULL,
                lang_tag   TEXT
            );",
        )
        .expect("create legacy findings table");
        assert!(
            !column_exists(&conn, "findings", "group_dir").expect("probe legacy column"),
            "precondition: the legacy table must not have group_dir"
        );

        migrate(&conn).expect("first migrate should add the column");
        assert!(
            column_exists(&conn, "findings", "group_dir").expect("probe migrated column"),
            "group_dir must exist after migrate"
        );

        migrate(&conn).expect("second migrate must be a no-op, not a duplicate-column error");
    }

    /// `migrate` must add `files.size_on_disk` (GT-05a) to a database created
    /// before that column existed, be idempotent, and leave pre-existing rows
    /// with a `NULL` in it (so load's `COALESCE(size_on_disk, size)` fallback
    /// keeps working until the next scan repopulates real values).
    #[test]
    fn migrate_adds_size_on_disk_to_a_legacy_files_table_and_is_idempotent() {
        let conn = Connection::open_in_memory().expect("open bare in-memory db");
        // The pre-`size_on_disk` files schema, with one row already in it.
        conn.execute_batch(
            "CREATE TABLE files (
                id       INTEGER PRIMARY KEY,
                game_id  INTEGER,
                rel_path TEXT NOT NULL,
                size     INTEGER NOT NULL,
                mtime    INTEGER
            );
            INSERT INTO files (id, game_id, rel_path, size, mtime)
                VALUES (1, NULL, 'a.txt', 123, NULL);",
        )
        .expect("create legacy files table");
        assert!(
            !column_exists(&conn, "files", "size_on_disk").expect("probe legacy column"),
            "precondition: the legacy table must not have size_on_disk"
        );

        migrate(&conn).expect("first migrate should add the column");
        assert!(
            column_exists(&conn, "files", "size_on_disk").expect("probe migrated column"),
            "size_on_disk must exist after migrate"
        );

        let on_disk: Option<i64> = conn
            .query_row("SELECT size_on_disk FROM files WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("read migrated row");
        assert_eq!(on_disk, None, "pre-existing rows must be NULL, not 0");

        // COALESCE fallback: a legacy NULL row still counts at its logical size.
        let coalesced: i64 = conn
            .query_row(
                "SELECT COALESCE(size_on_disk, size) FROM files WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .expect("coalesce legacy row");
        assert_eq!(coalesced, 123);

        migrate(&conn).expect("second migrate must be a no-op, not a duplicate-column error");
    }

    /// GT-09: `games.build_id` must be added to a database created before that
    /// column existed, be idempotent, and leave pre-existing games with `NULL`,
    /// which `gamestate::changed_games` reads as "unknown, claim nothing" - so
    /// the first startup after an upgrade never flags the whole library as
    /// changed.
    #[test]
    fn migrate_adds_build_id_to_a_legacy_games_table_and_is_idempotent() {
        let conn = Connection::open_in_memory().expect("open bare in-memory db");
        // The pre-`build_id` games schema, with one row already in it.
        conn.execute_batch(
            "CREATE TABLE games (
                id          INTEGER PRIMARY KEY,
                library_id  INTEGER NOT NULL,
                name        TEXT NOT NULL,
                install_dir TEXT NOT NULL,
                app_id      TEXT
            );
            INSERT INTO games (id, library_id, name, install_dir, app_id)
                VALUES (1, 1, 'Portal 2', 'F:\\SteamLibrary\\common\\Portal 2', '620');",
        )
        .expect("create legacy games table");
        assert!(
            !column_exists(&conn, "games", "build_id").expect("probe legacy column"),
            "precondition: the legacy table must not have build_id"
        );

        migrate(&conn).expect("first migrate should add the column");
        assert!(
            column_exists(&conn, "games", "build_id").expect("probe migrated column"),
            "build_id must exist after migrate"
        );

        let build_id: Option<String> = conn
            .query_row("SELECT build_id FROM games WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("read migrated row");
        assert_eq!(
            build_id, None,
            "pre-existing games must be NULL so no change is claimed for them"
        );

        migrate(&conn).expect("second migrate must be a no-op, not a duplicate-column error");
    }

    /// `clear_scan_data`'s fast path disables foreign-key enforcement only for
    /// the duration of the wipe - it must be back ON when the call returns,
    /// and really enforcing again.
    #[test]
    fn clear_scan_data_restores_foreign_key_enforcement() {
        let conn = open_in_memory().expect("in-memory db should open");

        clear_scan_data(&conn).expect("clear should succeed");

        let fk_on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("read foreign_keys pragma");
        assert_eq!(
            fk_on, 1,
            "foreign_keys must be back ON after clear_scan_data"
        );

        // Enforcement must actually bite: a finding referencing a file id that
        // doesn't exist must be rejected.
        let orphan = conn.execute(
            "INSERT INTO findings (file_id, category, confidence) VALUES (999999, 'redist', 90)",
            [],
        );
        assert!(
            orphan.is_err(),
            "FK enforcement must be active again: an orphaned finding insert must fail"
        );
    }

    /// `with_foreign_keys_off` must restore enforcement even when the closure
    /// returns `Err`, so a failed wipe can't silently leave the connection
    /// running without foreign-key checks.
    #[test]
    fn with_foreign_keys_off_restores_enforcement_even_when_closure_errors() {
        let conn = open_in_memory().expect("in-memory db should open");

        let result: Result<()> = with_foreign_keys_off(&conn, |_| {
            Err(crate::error::CoreError::Other("boom".into()))
        });
        assert!(result.is_err(), "the closure's error must propagate");

        let fk_on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("read foreign_keys pragma");
        assert_eq!(
            fk_on, 1,
            "foreign_keys must be restored to ON even after the closure failed"
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

    /// Two libraries, each with several games and files, must each report
    /// the correct summed byte total under their own `library_id`.
    #[test]
    fn occupied_by_library_sums_per_library() {
        let conn = open_in_memory().expect("in-memory db should open");

        conn.execute(
            "INSERT INTO game_libraries (vendor, path) VALUES ('steam', 'D:/SteamLibrary')",
            [],
        )
        .expect("insert library 1");
        conn.execute(
            "INSERT INTO game_libraries (vendor, path) VALUES ('gog', 'E:/GOG Games')",
            [],
        )
        .expect("insert library 2");

        conn.execute(
            "INSERT INTO games (library_id, name, install_dir, app_id) \
             VALUES (1, 'Game A', 'D:/SteamLibrary/A', NULL)",
            [],
        )
        .expect("insert game A");
        conn.execute(
            "INSERT INTO games (library_id, name, install_dir, app_id) \
             VALUES (1, 'Game B', 'D:/SteamLibrary/B', NULL)",
            [],
        )
        .expect("insert game B");
        conn.execute(
            "INSERT INTO games (library_id, name, install_dir, app_id) \
             VALUES (2, 'Game C', 'E:/GOG Games/C', NULL)",
            [],
        )
        .expect("insert game C");

        conn.execute(
            "INSERT INTO files (game_id, rel_path, size, mtime) VALUES (1, 'a1.bin', 100, NULL)",
            [],
        )
        .expect("insert file a1");
        conn.execute(
            "INSERT INTO files (game_id, rel_path, size, mtime) VALUES (2, 'b1.bin', 200, NULL)",
            [],
        )
        .expect("insert file b1");
        conn.execute(
            "INSERT INTO files (game_id, rel_path, size, mtime) VALUES (3, 'c1.bin', 50, NULL)",
            [],
        )
        .expect("insert file c1");

        let by_library = occupied_by_library(&conn).expect("aggregate by library");

        assert_eq!(
            by_library.get(&1),
            Some(&300u64),
            "library 1 must sum both its games' files (100 + 200)"
        );
        assert_eq!(
            by_library.get(&2),
            Some(&50u64),
            "library 2 must report its own game's files only"
        );
    }

    /// A library with a game but no files, and a library with no games at
    /// all, must both be absent from the map - callers treat an absent key
    /// as 0 bytes, per the function's doc comment.
    #[test]
    fn occupied_by_library_omits_libraries_with_no_files_or_no_games() {
        let conn = open_in_memory().expect("in-memory db should open");

        conn.execute(
            "INSERT INTO game_libraries (vendor, path) VALUES ('steam', 'D:/SteamLibrary')",
            [],
        )
        .expect("insert library with a fileless game");
        conn.execute(
            "INSERT INTO game_libraries (vendor, path) VALUES ('gog', 'E:/GOG Games')",
            [],
        )
        .expect("insert library with no games");
        conn.execute(
            "INSERT INTO games (library_id, name, install_dir, app_id) \
             VALUES (1, 'Empty Game', 'D:/SteamLibrary/Empty', NULL)",
            [],
        )
        .expect("insert fileless game");

        let by_library = occupied_by_library(&conn).expect("aggregate by library");

        assert_eq!(
            by_library.get(&1),
            None,
            "a library whose only game has no files must be absent, not 0"
        );
        assert_eq!(
            by_library.get(&2),
            None,
            "a library with no games at all must be absent"
        );
    }

    /// An empty database (no libraries, games, or files) must yield an empty
    /// map rather than erroring.
    #[test]
    fn occupied_by_library_is_empty_for_a_fresh_database() {
        let conn = open_in_memory().expect("in-memory db should open");

        let by_library = occupied_by_library(&conn).expect("aggregate by library");

        assert!(
            by_library.is_empty(),
            "a fresh database must report no occupied libraries"
        );
    }

    /// `is_corruption_error` must recognize both SQLite codes that map to
    /// "the file is unusable" (`SQLITE_CORRUPT`, `SQLITE_NOTADB`), and must
    /// reject everything else - a non-corruption `SqliteFailure` (e.g.
    /// `SQLITE_BUSY`, a transient lock) and a non-`Db` `CoreError` variant
    /// alike - so `rebuild_database` is never triggered by an unrelated
    /// failure.
    #[test]
    fn is_corruption_error_matches_only_corrupt_codes() {
        let corrupt = CoreError::Db(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            None,
        ));
        assert!(is_corruption_error(&corrupt), "SQLITE_CORRUPT must match");

        let not_a_db = CoreError::Db(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOTADB),
            None,
        ));
        assert!(is_corruption_error(&not_a_db), "SQLITE_NOTADB must match");

        let busy = CoreError::Db(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        ));
        assert!(
            !is_corruption_error(&busy),
            "a non-corruption SqliteFailure (SQLITE_BUSY) must not match"
        );

        let other = CoreError::Other("boom".to_string());
        assert!(
            !is_corruption_error(&other),
            "a non-Db CoreError variant must not match"
        );
    }

    /// `rebuild_database` must preserve `game_libraries` and `settings` from
    /// a (here: healthy) existing database file, while dropping every row of
    /// scan data (`games`, `files`, `findings`) - the whole point being that
    /// only what "Clear database" already preserves survives recovery too.
    #[test]
    fn rebuild_database_preserves_libraries_and_settings_and_drops_scan_data() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("gametrimmer.db");

        {
            let conn = open(&db_path).expect("open file-backed db");
            conn.execute(
                "INSERT INTO game_libraries (vendor, path) VALUES ('steam', 'D:/SteamLibrary')",
                [],
            )
            .expect("insert library");
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('app_language', 'en')",
                [],
            )
            .expect("insert setting");
            conn.execute(
                "INSERT INTO games (library_id, name, install_dir, app_id) \
                 VALUES (1, 'Some Game', 'D:/SteamLibrary/Some Game', NULL)",
                [],
            )
            .expect("insert game");
            conn.execute(
                "INSERT INTO files (game_id, rel_path, size, mtime) VALUES (1, 'a.bin', 100, NULL)",
                [],
            )
            .expect("insert file");
            conn.execute(
                "INSERT INTO findings (file_id, category, confidence) VALUES (1, 'redist', 90)",
                [],
            )
            .expect("insert finding");
        }

        let conn = rebuild_database(&db_path).expect("rebuild_database should succeed");

        let (vendor, path): (String, String) = conn
            .query_row(
                "SELECT vendor, path FROM game_libraries WHERE path = ?1",
                ["D:/SteamLibrary"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("library must survive rebuild");
        assert_eq!(vendor, "steam");
        assert_eq!(path, "D:/SteamLibrary");

        let value: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                ["app_language"],
                |row| row.get(0),
            )
            .expect("setting must survive rebuild");
        assert_eq!(value, "en");

        let count = |table: &str| -> i64 {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count rows")
        };
        assert_eq!(count("games"), 0, "scan data (games) must not be salvaged");
        assert_eq!(count("files"), 0, "scan data (files) must not be salvaged");
        assert_eq!(
            count("findings"),
            0,
            "scan data (findings) must not be salvaged"
        );
    }

    /// A path with no existing file at all (the file was already deleted, or
    /// this is the very first run) must still yield a working, empty
    /// database rather than erroring - `rebuild_database`'s delete step is a
    /// no-op and `open` simply creates a fresh file.
    #[test]
    fn rebuild_database_on_a_fresh_path_creates_an_empty_valid_db() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("gametrimmer.db");

        let conn = rebuild_database(&db_path)
            .expect("rebuild_database on a nonexistent path should succeed");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM game_libraries", [], |row| row.get(0))
            .expect("fresh db should be queryable");
        assert_eq!(count, 0, "a fresh rebuild should have no libraries");
    }

    /// The real scenario `rebuild_database` exists for: the file on disk is
    /// not a readable SQLite database at all. Recovery must still yield a
    /// working, empty database, and must sweep away stale sidecars left next
    /// to the bad file.
    #[test]
    fn rebuild_database_recovers_a_corrupt_file_and_clears_stale_sidecars() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("gametrimmer.db");

        // Bytes that are not a SQLite database at all -> SQLITE_NOTADB on
        // open, exactly what a truncated/overwritten file looks like. Salvage
        // recovers nothing, so recovery falls back to a fresh empty database.
        std::fs::write(&db_path, b"this is definitely not a sqlite database file")
            .expect("write corrupt db");
        // A stale `-journal` sidecar: a WAL database never recreates one, so
        // its absence afterwards is a clean signal the cleanup ran.
        let journal = sidecar_path(&db_path, "-journal");
        std::fs::write(&journal, b"stale journal").expect("write stale journal");

        let conn = rebuild_database(&db_path).expect("rebuild must recover a corrupt file");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM game_libraries", [], |row| row.get(0))
            .expect("recovered db must be queryable");
        assert_eq!(count, 0, "an unsalvageable corrupt db recovers to empty");

        drop(conn);
        assert!(
            !journal.exists(),
            "the stale -journal sidecar must be removed by the rebuild"
        );
    }
}
