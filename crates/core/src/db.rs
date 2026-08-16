use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};

use crate::error::{CoreError, Result};

/// SQL schema for the GameTrimmer database. Idempotent (`IF NOT EXISTS`).
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS scan_runs (
    id               INTEGER PRIMARY KEY,
    started_at       INTEGER NOT NULL,
    completed_at     INTEGER,
    state            TEXT NOT NULL,
    discovery_status TEXT NOT NULL,
    scan_ms          INTEGER,
    analyze_ms       INTEGER,
    total_ms         INTEGER
);

CREATE TABLE IF NOT EXISTS scan_state (
    singleton      INTEGER PRIMARY KEY CHECK(singleton = 1),
    active_scan_id INTEGER REFERENCES scan_runs(id)
);

CREATE TABLE IF NOT EXISTS game_libraries (
    id     INTEGER PRIMARY KEY,
    vendor TEXT NOT NULL,
    path   TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS games (
    id           INTEGER PRIMARY KEY,
    scan_id      INTEGER NOT NULL DEFAULT 0,
    library_id   INTEGER NOT NULL REFERENCES game_libraries(id),
    name         TEXT NOT NULL,
    install_dir  TEXT NOT NULL,
    app_id       TEXT,
    build_id     TEXT,
    files        INTEGER,
    bytes        INTEGER,
    bytes_on_disk INTEGER,
    scan_route   TEXT
);

CREATE TABLE IF NOT EXISTS files (
    id           INTEGER PRIMARY KEY,
    scan_id      INTEGER NOT NULL DEFAULT 0,
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
    group_dir  TEXT,
    provenance TEXT NOT NULL DEFAULT 'builtin'
);

CREATE TABLE IF NOT EXISTS operations (
    id       INTEGER PRIMARY KEY,
    ts       INTEGER NOT NULL,
    action   TEXT NOT NULL,
    src_path TEXT NOT NULL,
    dst_path TEXT,
    status   TEXT NOT NULL,
    scan_id  INTEGER,
    file_id  INTEGER,
    trusted_root TEXT,
    rel_path TEXT,
    expected_identity TEXT,
    completed_at INTEGER,
    outcome TEXT,
    error TEXT
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS scan_library_evidence (
    scan_id      INTEGER NOT NULL REFERENCES scan_runs(id),
    library_path TEXT NOT NULL,
    provider     TEXT NOT NULL,
    status       TEXT NOT NULL,
    PRIMARY KEY(scan_id, library_path)
);

CREATE TABLE IF NOT EXISTS scan_diagnostics (
    id       INTEGER PRIMARY KEY,
    scan_id  INTEGER NOT NULL REFERENCES scan_runs(id),
    provider TEXT NOT NULL,
    stage    TEXT NOT NULL,
    path     TEXT,
    message  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS file_safety (
    file_id          INTEGER PRIMARY KEY REFERENCES files(id),
    scan_id          INTEGER NOT NULL REFERENCES scan_runs(id),
    evidence_library_path TEXT,
    trusted_root     TEXT NOT NULL,
    rel_path         TEXT NOT NULL,
    root_identity    TEXT,
    target_identity  TEXT,
    target_kind      TEXT,
    tree_fingerprint TEXT,
    block_reason     TEXT
);

CREATE INDEX IF NOT EXISTS idx_files_game_id     ON files(game_id);
CREATE INDEX IF NOT EXISTS idx_findings_file_id  ON findings(file_id);
CREATE INDEX IF NOT EXISTS idx_diagnostics_scan   ON scan_diagnostics(scan_id);
";

/// `pub` so the diagnostic bundle can report the database's own
/// `user_version` beside the version this build understands - the pair is
/// what distinguishes "your database is from a newer build" from "it is
/// damaged".
pub const CURRENT_SCHEMA_VERSION: i64 = 4;

/// Opens (or creates) the database at `path` and applies the schema.
pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    configure(&conn)?;
    ensure_supported_schema_version(&conn)?;
    apply_schema(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Opens an in-memory database with the schema applied. Intended for tests.
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    ensure_supported_schema_version(&conn)?;
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
    // way the app's state could leak outside its own portable folder.
    // `MEMORY` is fine here: the database itself is already file-backed, so
    // this only affects short-lived scratch space, not durability.
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
    let mut version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    ensure_supported_schema_version(conn)?;
    if version < 1 {
        migrate_v1(conn)?;
        conn.pragma_update(None, "user_version", 1)?;
        version = 1;
    }
    if version < 2 {
        migrate_v2(conn)?;
        conn.pragma_update(None, "user_version", 2)?;
        version = 2;
    }
    if version < 3 {
        migrate_v3(conn)?;
        conn.pragma_update(None, "user_version", 3)?;
        version = 3;
    }
    if version < 4 {
        migrate_v4(conn)?;
        conn.pragma_update(None, "user_version", 4)?;
    }
    Ok(())
}

fn ensure_supported_schema_version(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > CURRENT_SCHEMA_VERSION {
        return Err(CoreError::Other(format!(
            "database schema version {version} is newer than supported version {CURRENT_SCHEMA_VERSION}"
        )));
    }
    Ok(())
}

fn migrate_v1(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "findings", "group_dir", "TEXT")?;
    add_column_if_missing(
        conn,
        "findings",
        "provenance",
        "TEXT NOT NULL DEFAULT 'builtin'",
    )?;
    // `files.size_on_disk` (allocated-size accounting): the on-disk allocated size, added so
    // sizes shown and reported match Explorer's "Size on disk" instead of the
    // logical total. `NULL` for every file indexed by an earlier build; load
    // falls back to `size` (via COALESCE) for those until the next scan
    // repopulates the real on-disk figure.
    add_column_if_missing(conn, "files", "size_on_disk", "INTEGER")?;
    // `games.build_id` (game-state tracking): the launcher's content build id recorded at
    // scan time, so a later startup can tell whether a game was updated or
    // re-verified (and therefore may have its trimmed files back) without
    // rescanning. `NULL` for every game indexed by an earlier build - and
    // deliberately treated as "unknown, claim nothing" by
    // `gamestate::changed_games`, so the first run after an upgrade does not
    // flag the whole library.
    add_column_if_missing(conn, "games", "build_id", "TEXT")?;
    add_column_if_missing(conn, "games", "scan_id", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(conn, "files", "scan_id", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(conn, "operations", "scan_id", "INTEGER")?;
    add_column_if_missing(conn, "operations", "file_id", "INTEGER")?;
    add_column_if_missing(conn, "operations", "trusted_root", "TEXT")?;
    add_column_if_missing(conn, "operations", "rel_path", "TEXT")?;
    add_column_if_missing(conn, "operations", "expected_identity", "TEXT")?;
    add_column_if_missing(conn, "operations", "completed_at", "INTEGER")?;
    add_column_if_missing(conn, "operations", "outcome", "TEXT")?;
    add_column_if_missing(conn, "operations", "error", "TEXT")?;
    add_column_if_missing(conn, "file_safety", "block_reason", "TEXT")?;
    if table_exists(conn, "games")? {
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_games_scan_id ON games(scan_id)",
            [],
        )?;
    }
    // `idx_files_scan_id` used to be created here too. `migrate_v4` drops it
    // (see that function's doc comment for the measurements behind why), so
    // creating it on a database that is about to migrate straight through to
    // v4 anyway - true for every fresh install, and for any real upgrade
    // since this code shipped - would only leave behind the free pages of a
    // create-immediately-undone index. A genuinely old (pre-v1) database
    // still ends up without the index: `migrate_v4`'s `DROP INDEX IF EXISTS`
    // is a no-op when it was never created in the first place.

    // Generation 0 is the read-only compatibility snapshot for databases
    // created before scan identity and delete-safety evidence existed.
    if table_exists(conn, "scan_runs")? && table_exists(conn, "scan_state")? {
        conn.execute(
            "INSERT OR IGNORE INTO scan_runs
             (id, started_at, completed_at, state, discovery_status)
             VALUES (0, 0, 0, 'legacy', 'unknown')",
            [],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO scan_state (singleton, active_scan_id)
             VALUES (1, 0)",
            [],
        )?;
    }
    Ok(())
}

fn migrate_v2(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "file_safety", "evidence_library_path", "TEXT")?;
    Ok(())
}

/// Adds the durable diagnostic columns: what a scan actually did, rather
/// than only what it found.
///
/// All seven were computed and then discarded before this - per-game file
/// and byte counts came back from `store_files_no_tx` and were dropped on
/// the floor, the phase timing lived only in the bottom bar, and the chosen
/// scan route survived exactly as long as one localized counter string in
/// the settings dialog. They are the evidence behind "the scan took 40
/// minutes" and "I turned on the MFT index and nothing got faster", neither
/// of which could be answered after the fact.
///
/// `NULL` for every row written by an earlier build, which is the same
/// convention `migrate_v1`'s columns follow: unknown, never backfilled, and
/// populated from the next scan on.
fn migrate_v3(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "games", "files", "INTEGER")?;
    add_column_if_missing(conn, "games", "bytes", "INTEGER")?;
    add_column_if_missing(conn, "games", "bytes_on_disk", "INTEGER")?;
    add_column_if_missing(conn, "games", "scan_route", "TEXT")?;
    add_column_if_missing(conn, "scan_runs", "scan_ms", "INTEGER")?;
    add_column_if_missing(conn, "scan_runs", "analyze_ms", "INTEGER")?;
    add_column_if_missing(conn, "scan_runs", "total_ms", "INTEGER")?;
    Ok(())
}

/// Drops `idx_files_scan_id` (added by `migrate_v1`).
///
/// Every `files` row in one generation shares the same `scan_id`, so the
/// index has no selectivity anywhere it is used - "find the rows for this
/// generation" always means "find (nearly) every row". Measured on a 4.9M-row
/// `files` table (850 MB database, real reference data): inserting the
/// generation cost 11.7s with the index against 9.9s without it; the two
/// full-generation deletes that touch `files.scan_id`
/// (`prune_superseded_generations`, `abort_scan`) cost ~7.0s/7.9s with it
/// against ~6.3s/6.9s without; and the two read queries that filter on
/// `f.scan_id` (`worker::load::load_findings`,
/// `worker::rules_io::preview_active_impact`) showed no measurable
/// difference, because the planner still has to walk essentially the whole
/// table either way. The index costs real time to maintain on every insert
/// and every generation-wide delete and buys nothing back anywhere it is
/// consulted - dropping it is a net win on every measured path.
fn migrate_v4(conn: &Connection) -> Result<()> {
    conn.execute("DROP INDEX IF EXISTS idx_files_scan_id", [])?;
    Ok(())
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Starts an isolated scan generation. The current active generation is not
/// modified until [`activate_scan`] succeeds.
pub fn begin_scan(conn: &Connection, discovery_status: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO scan_runs (started_at, state, discovery_status)
         VALUES (?1, 'staging', ?2)",
        params![unix_timestamp(), discovery_status],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn record_scan_library_evidence(
    conn: &Connection,
    scan_id: i64,
    library_path: &Path,
    provider: &str,
    status: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO scan_library_evidence
         (scan_id, library_path, provider, status) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(scan_id, library_path) DO UPDATE SET
             provider = excluded.provider,
             status = excluded.status",
        params![scan_id, library_path.to_string_lossy(), provider, status],
    )?;
    Ok(())
}

/// Records how long each phase of a scan took, in milliseconds.
///
/// Kept as three explicit numbers rather than derived from
/// `completed_at - started_at`: those are second-precision Unix timestamps,
/// and the question this answers ("which phase was slow") needs the split,
/// not the total.
pub fn record_scan_timing(
    conn: &Connection,
    scan_id: i64,
    scan_ms: u64,
    analyze_ms: u64,
    total_ms: u64,
) -> Result<()> {
    conn.execute(
        "UPDATE scan_runs SET scan_ms = ?2, analyze_ms = ?3, total_ms = ?4 WHERE id = ?1",
        params![scan_id, scan_ms as i64, analyze_ms as i64, total_ms as i64],
    )?;
    Ok(())
}

/// Records which route each game's install root took (the MFT index or a
/// directory walk) and, for a walked root, why.
///
/// One column on the `games` row rather than a `scan_diagnostics` row per
/// root: a library of a few thousand games would otherwise add a few
/// thousand diagnostic rows to every scan, and the route is a property of
/// the game's row in this generation anyway.
pub fn record_scan_routes(conn: &Connection, routes: &[(i64, String)]) -> Result<()> {
    let mut stmt = conn.prepare("UPDATE games SET scan_route = ?2 WHERE id = ?1")?;
    for (game_id, route) in routes {
        stmt.execute(params![game_id, route])?;
    }
    Ok(())
}

pub fn record_scan_diagnostic(
    conn: &Connection,
    scan_id: i64,
    provider: &str,
    stage: &str,
    path: Option<&Path>,
    message: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO scan_diagnostics (scan_id, provider, stage, path, message)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            scan_id,
            provider,
            stage,
            path.map(|value| value.to_string_lossy().into_owned()),
            message
        ],
    )?;
    Ok(())
}

/// Records a late completeness failure (for example orphan enumeration or
/// measurement) before a staging generation is activated.
pub fn mark_scan_degraded(conn: &Connection, scan_id: i64) -> Result<()> {
    let changed = conn.execute(
        "UPDATE scan_runs SET discovery_status = 'degraded'
         WHERE id = ?1 AND state = 'staging'",
        [scan_id],
    )?;
    if changed != 1 {
        return Err(CoreError::Other(format!(
            "cannot degrade non-staging scan generation {scan_id}"
        )));
    }
    Ok(())
}

/// Atomically makes a fully persisted generation visible to readers.
///
/// Moves the pointer and nothing else. The generation this one supersedes is
/// marked but not deleted - see [`prune_superseded`], which the caller runs
/// once the results are in front of the user.
pub fn activate_scan(conn: &mut Connection, scan_id: i64) -> Result<()> {
    validate_scan_generation(conn, scan_id)?;
    let tx = conn.transaction()?;
    let state: String = tx.query_row(
        "SELECT state FROM scan_runs WHERE id = ?1",
        [scan_id],
        |row| row.get(0),
    )?;
    if state != "staging" {
        return Err(CoreError::Other(format!(
            "scan generation {scan_id} is {state}, not staging"
        )));
    }

    let foreign_key_errors: i64 =
        tx.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    if foreign_key_errors != 0 {
        return Err(CoreError::Other(format!(
            "scan generation failed foreign-key validation ({foreign_key_errors} error(s))"
        )));
    }

    if let Some(previous) = tx
        .query_row(
            "SELECT active_scan_id FROM scan_state WHERE singleton = 1",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )
        .unwrap_or(None)
    {
        if previous != 0 && previous != scan_id {
            tx.execute(
                "UPDATE scan_runs SET state = 'superseded' WHERE id = ?1 AND state = 'active'",
                [previous],
            )?;
        }
    }

    tx.execute(
        "UPDATE scan_runs SET state = 'active', completed_at = ?1 WHERE id = ?2",
        params![unix_timestamp(), scan_id],
    )?;
    tx.execute(
        "INSERT INTO scan_state (singleton, active_scan_id) VALUES (1, ?1)
         ON CONFLICT(singleton) DO UPDATE SET active_scan_id = excluded.active_scan_id",
        [scan_id],
    )?;
    tx.commit()?;
    Ok(())
}

/// Drops whatever generations activation left behind, in a transaction of
/// its own.
///
/// Split out of [`activate_scan`] because it was three quarters of a rescan's
/// housekeeping and none of it is work a reader waits for. Measured on a real
/// library: activation took 13.1 s on a database that already held a
/// generation against 1.0 s on an empty one, and taking it apart put ~1.5 s in
/// the validation and foreign-key check, ~4.5 s in the deletes themselves and
/// **~7.7 s in the commit** - writing back the pages freed by 2.16 M rows.
///
/// Nothing reads a superseded generation: `load_findings` and
/// `occupied_by_library` both filter on `scan_state.active_scan_id`, which
/// activation has already moved. So the only thing that transaction's
/// atomicity bought was tidiness, at the price of holding the scan's
/// completion for the whole commit.
///
/// Safe to skip, crash through, or never reach: a generation left in
/// `superseded` is exactly what this deletes, so the next scan's activation
/// hands it right back here. Call it after the results have been reported.
pub fn prune_superseded(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;
    prune_superseded_generations(&tx)?;
    tx.commit()?;
    Ok(())
}

/// Drops every generation the active pointer has moved past.
///
/// Nothing reads a superseded generation - `load_findings` and
/// `occupied_by_library` both filter on `scan_state.active_scan_id` - so
/// keeping one only costs space. And it costs a *lot* of it: a generation is a
/// full copy of `games`, `files`, `findings` and `file_safety`, which on a real
/// library is millions of rows per scan. Left in place they accumulate
/// invisibly, and the only symptom the user ever sees is a database that grows
/// without bound and scans that get slower every run.
///
/// Only `superseded` rows are pruned. A `staging` generation may belong to a
/// scan still in flight, and `abort_scan` owns that path.
fn prune_superseded_generations(tx: &Connection) -> Result<()> {
    // Ordered child-first so the foreign keys hold at every step. `operations`
    // is deliberately untouched: it is the durable journal, its `scan_id` is
    // unconstrained, and a pending intent must outlive the generation that
    // produced it or crash recovery would lose its evidence.
    let statements = [
        "DELETE FROM findings WHERE file_id IN
             (SELECT f.id FROM files f
              JOIN scan_runs r ON r.id = f.scan_id
              WHERE r.state = 'superseded')",
        "DELETE FROM file_safety WHERE scan_id IN
             (SELECT id FROM scan_runs WHERE state = 'superseded')",
        "DELETE FROM files WHERE scan_id IN
             (SELECT id FROM scan_runs WHERE state = 'superseded')",
        "DELETE FROM games WHERE scan_id IN
             (SELECT id FROM scan_runs WHERE state = 'superseded')",
        "DELETE FROM scan_library_evidence WHERE scan_id IN
             (SELECT id FROM scan_runs WHERE state = 'superseded')",
        "DELETE FROM scan_diagnostics WHERE scan_id IN
             (SELECT id FROM scan_runs WHERE state = 'superseded')",
        "DELETE FROM scan_runs WHERE state = 'superseded'",
    ];
    for statement in statements {
        tx.execute(statement, [])?;
    }
    Ok(())
}

/// Checks the cross-table generation contract before the active pointer can
/// move. In particular, every finding must have a same-generation safety row.
pub fn validate_scan_generation(conn: &Connection, scan_id: i64) -> Result<()> {
    let mismatched_files: i64 = conn.query_row(
        "SELECT COUNT(*) FROM files f
         LEFT JOIN games g ON g.id = f.game_id
         WHERE f.scan_id = ?1 AND f.game_id IS NOT NULL
           AND (g.id IS NULL OR g.scan_id <> ?1)",
        [scan_id],
        |row| row.get(0),
    )?;
    let missing_safety: i64 = conn.query_row(
        "SELECT COUNT(*) FROM findings fi
         JOIN files f ON f.id = fi.file_id
         LEFT JOIN file_safety fs ON fs.file_id = f.id AND fs.scan_id = f.scan_id
         WHERE f.scan_id = ?1 AND fs.file_id IS NULL",
        [scan_id],
        |row| row.get(0),
    )?;
    if mismatched_files != 0 || missing_safety != 0 {
        return Err(CoreError::Other(format!(
            "scan generation {scan_id} failed consistency validation: \
             {mismatched_files} file/game mismatch(es), {missing_safety} finding(s) without safety evidence"
        )));
    }
    Ok(())
}

/// Marks an unfinished generation and removes only its scan-produced rows.
pub fn abort_scan(conn: &mut Connection, scan_id: i64, state: &str) -> Result<()> {
    if !matches!(state, "failed" | "cancelled") {
        return Err(CoreError::Other(format!(
            "invalid terminal scan state: {state}"
        )));
    }
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM file_safety WHERE scan_id = ?1", [scan_id])?;
    tx.execute(
        "DELETE FROM findings WHERE file_id IN
         (SELECT id FROM files WHERE scan_id = ?1)",
        [scan_id],
    )?;
    tx.execute("DELETE FROM files WHERE scan_id = ?1", [scan_id])?;
    tx.execute("DELETE FROM games WHERE scan_id = ?1", [scan_id])?;
    tx.execute(
        "UPDATE scan_runs SET state = ?1, completed_at = ?2 WHERE id = ?3",
        params![state, unix_timestamp(), scan_id],
    )?;
    tx.commit()?;
    Ok(())
}

/// The generation the app currently reads from.
///
/// `Some(0)` is **not** a scan: generation 0 is the legacy compatibility
/// sentinel written into every database at creation (see `migrate_v1`), so a
/// brand-new database answers `Some(0)` here, not `None`. Callers asking
/// "has anything been scanned yet" must test `id != 0` - `activate_scan` and
/// `scan_allows_deletion` both do.
pub fn active_scan_id(conn: &Connection) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT active_scan_id FROM scan_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or(None))
}

/// Legacy generation 0 and any non-active generation are read-only.
pub fn scan_allows_deletion(conn: &Connection, scan_id: i64) -> Result<bool> {
    if scan_id == 0 || active_scan_id(conn)? != Some(scan_id) {
        return Ok(false);
    }
    let state: Option<String> = conn
        .query_row(
            "SELECT state FROM scan_runs WHERE id = ?1",
            [scan_id],
            |row| row.get(0),
        )
        .ok();
    Ok(state.as_deref() == Some("active"))
}

/// Startup recovery: discard data from interrupted staging generations while
/// leaving the active snapshot untouched.
pub fn cleanup_abandoned_scans(conn: &mut Connection) -> Result<usize> {
    let ids: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM scan_runs WHERE state = 'staging'")?;
        let rows = stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    for scan_id in &ids {
        abort_scan(conn, *scan_id, "failed")?;
    }
    Ok(ids.len())
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
/// after a "Delete selected" run don't shrink the file on disk). `VACUUM`
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
        tx.execute("DELETE FROM file_safety", [])?;
        tx.execute("DELETE FROM findings", [])?;
        tx.execute("DELETE FROM files", [])?;
        tx.execute("DELETE FROM games", [])?;
        tx.execute("DELETE FROM operations", [])?;
        tx.execute("DELETE FROM scan_library_evidence", [])?;
        tx.execute("DELETE FROM scan_diagnostics", [])?;
        tx.execute("DELETE FROM scan_runs WHERE id <> 0", [])?;
        tx.execute(
            "UPDATE scan_state SET active_scan_id = NULL WHERE singleton = 1",
            [],
        )?;
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
    // Summed from the per-game totals, not from `files`. `files` holds only
    // the flagged files now, so summing it would report what the *findings*
    // occupy - on a real library roughly a thirtieth of the truth - and the
    // "selected frees N %" figure built on top of it would read ~100 %.
    // `games.bytes_on_disk` covers every file of the install (see
    // `scanner::ScanStats::of`), falling back to the logical size for rows
    // written before `bytes_on_disk` existed.
    //
    // A game with no totals at all contributes nothing rather than 0, so a
    // library whose games are all like that stays absent from the map - the
    // distinction this function's callers rely on.
    let mut stmt = conn.prepare(
        "SELECT g.library_id, SUM(COALESCE(g.bytes_on_disk, g.bytes)) FROM games g \
         WHERE g.scan_id = (SELECT active_scan_id FROM scan_state WHERE singleton = 1) \
           AND COALESCE(g.bytes_on_disk, g.bytes) IS NOT NULL \
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

    validate_database_file(&tmp_path)?;

    let recovery = sidecar_path(db_path, ".recovery.bak");
    let recovery_sidecars = ["-wal", "-shm", "-journal"];
    for suffix in recovery_sidecars {
        let source = sidecar_path(db_path, suffix);
        let destination = sidecar_path(&recovery, suffix);
        delete_if_exists(&destination)?;
        if source.is_file() {
            std::fs::copy(&source, &destination)?;
        }
    }

    crate::atomic_file::replace_staged_with_backup(&tmp_path, db_path, &recovery, |path| {
        validate_database_file(path).map_err(std::io::Error::other)
    })?;
    delete_if_exists(&sidecar_path(db_path, "-wal"))?;
    delete_if_exists(&sidecar_path(db_path, "-shm"))?;
    delete_if_exists(&sidecar_path(db_path, "-journal"))?;

    let conn = open(db_path)?;
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(CoreError::Other(format!(
            "rebuilt database failed integrity_check: {integrity}"
        )));
    }
    delete_if_exists(&recovery)?;
    for suffix in recovery_sidecars {
        delete_if_exists(&sidecar_path(&recovery, suffix))?;
    }
    Ok(conn)
}

fn validate_database_file(path: &Path) -> Result<()> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity == "ok" {
        Ok(())
    } else {
        Err(CoreError::Other(format!(
            "database integrity_check failed: {integrity}"
        )))
    }
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

    /// Seeds one generation's worth of scan rows and returns its `scan_id`.
    fn seed_generation(conn: &mut Connection, library_path: &str, game: &str) -> i64 {
        let scan_id = begin_scan(conn, "complete").expect("begin scan");
        conn.execute(
            "INSERT OR IGNORE INTO game_libraries (vendor, path) VALUES ('test', ?1)",
            [library_path],
        )
        .expect("library");
        let library_id: i64 = conn
            .query_row(
                "SELECT id FROM game_libraries WHERE path = ?1",
                [library_path],
                |row| row.get(0),
            )
            .expect("library id");
        conn.execute(
            "INSERT INTO games (scan_id, library_id, name, install_dir)
             VALUES (?1, ?2, ?3, ?4)",
            params![scan_id, library_id, game, library_path],
        )
        .expect("game");
        let game_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO files (scan_id, game_id, rel_path, size) VALUES (?1, ?2, 'x.bin', 1)",
            params![scan_id, game_id],
        )
        .expect("file");
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO findings (file_id, category, confidence) VALUES (?1, 'bonus', 90)",
            [file_id],
        )
        .expect("finding");
        conn.execute(
            "INSERT INTO file_safety (file_id, scan_id, trusted_root, rel_path)
             VALUES (?1, ?2, ?3, 'x.bin')",
            params![file_id, scan_id, library_path],
        )
        .expect("safety");
        conn.execute(
            "INSERT INTO scan_diagnostics (scan_id, provider, stage, message)
             VALUES (?1, 'test', 'probe', 'note')",
            [scan_id],
        )
        .expect("diagnostic");
        scan_id
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |row| row.get(0)).expect("count")
    }

    /// A superseded generation is unreachable - every reader filters on
    /// `scan_state.active_scan_id` - so leaving it behind would grow the
    /// database by a full copy of the scan on every single run, invisibly.
    #[test]
    fn activating_then_pruning_drops_the_generation_it_supersedes() {
        let mut conn = open_in_memory().expect("in-memory db");

        let first = seed_generation(&mut conn, "D:/Library", "First");
        activate_scan(&mut conn, first).expect("activate first");
        prune_superseded(&mut conn).expect("prune after first");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 1);

        let second = seed_generation(&mut conn, "D:/Library", "Second");
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM files"),
            2,
            "both generations coexist while the second is staging"
        );
        activate_scan(&mut conn, second).expect("activate second");

        // Activation moves the pointer and stops. This is the window the
        // scan now reports its results in, so what a reader sees here is the
        // whole point of the split.
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM files"),
            2,
            "activation alone leaves the superseded rows in place"
        );
        assert_eq!(
            count(
                &conn,
                &format!("SELECT COUNT(*) FROM files WHERE scan_id = {second}")
            ),
            1,
            "and a reader filtering on the active generation sees only the new one"
        );

        prune_superseded(&mut conn).expect("prune after second");

        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM games"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM findings"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM file_safety"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM scan_diagnostics"), 1);
        assert_eq!(
            count(
                &conn,
                &format!("SELECT COUNT(*) FROM scan_runs WHERE id = {first}")
            ),
            0,
            "the superseded run row goes too"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM scan_runs WHERE state = 'legacy'"
            ),
            1,
            "the legacy generation anchors pre-migration rows and must survive"
        );
        assert_eq!(
            count(
                &conn,
                &format!("SELECT COUNT(*) FROM files WHERE scan_id = {second}")
            ),
            1,
            "the surviving rows belong to the newly active generation"
        );

        // The libraries table is generation-independent and must survive.
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM game_libraries"), 1);
    }

    /// The prune no longer runs inside activation, so it can be missed: the
    /// app can be killed, or the disk can refuse the write, between the two.
    /// That must cost space until the next scan and nothing else - in
    /// particular the next run must clean up both generations, not just its
    /// own predecessor.
    #[test]
    fn a_prune_that_never_ran_is_caught_by_the_next_one() {
        let mut conn = open_in_memory().expect("in-memory db");

        let first = seed_generation(&mut conn, "D:/Library", "First");
        activate_scan(&mut conn, first).expect("activate first");
        let second = seed_generation(&mut conn, "D:/Library", "Second");
        activate_scan(&mut conn, second).expect("activate second");
        // The prune that belonged here never happened.

        let third = seed_generation(&mut conn, "D:/Library", "Third");
        activate_scan(&mut conn, third).expect("activate third");
        prune_superseded(&mut conn).expect("prune after third");

        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM files"),
            1,
            "the skipped generation is collected along with the one it preceded"
        );
        assert_eq!(
            count(
                &conn,
                &format!("SELECT COUNT(*) FROM files WHERE scan_id = {third}")
            ),
            1
        );
    }

    /// Pruning must not reach into a generation that is still being written.
    #[test]
    fn pruning_leaves_a_concurrently_staging_generation_alone() {
        let mut conn = open_in_memory().expect("in-memory db");

        let first = seed_generation(&mut conn, "D:/Library", "First");
        activate_scan(&mut conn, first).expect("activate first");
        prune_superseded(&mut conn).expect("prune after first");
        let staging = seed_generation(&mut conn, "D:/Library", "Staging");
        let second = seed_generation(&mut conn, "D:/Library", "Second");
        activate_scan(&mut conn, second).expect("activate second");
        prune_superseded(&mut conn).expect("prune after second");

        assert_eq!(
            count(
                &conn,
                &format!("SELECT COUNT(*) FROM files WHERE scan_id = {staging}")
            ),
            1,
            "an in-flight staging generation must not be pruned"
        );
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

    /// `temp_store=MEMORY` must be in effect after `open` - this is what
    /// keeps the app's "everything stays next to the exe" promise honest:
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

    /// `migrate_v3` must reach a database created before the diagnostic
    /// columns existed, be idempotent, and leave the row already in it with
    /// `NULL`s rather than inventing counts for a scan that predates them.
    #[test]
    fn migrate_adds_the_diagnostic_columns_to_legacy_tables_and_is_idempotent() {
        let conn = Connection::open_in_memory().expect("open bare in-memory db");
        conn.execute_batch(
            "CREATE TABLE games (
                id          INTEGER PRIMARY KEY,
                scan_id     INTEGER NOT NULL DEFAULT 0,
                library_id  INTEGER NOT NULL,
                name        TEXT NOT NULL,
                install_dir TEXT NOT NULL
            );
            CREATE TABLE scan_runs (
                id               INTEGER PRIMARY KEY,
                started_at       INTEGER NOT NULL,
                completed_at     INTEGER,
                state            TEXT NOT NULL,
                discovery_status TEXT NOT NULL
            );
            INSERT INTO games (library_id, name, install_dir)
                VALUES (1, 'Older Than The Column', 'C:/Games/Old');",
        )
        .expect("create legacy tables");

        migrate(&conn).expect("first migrate should add the columns");
        for (table, column) in [
            ("games", "files"),
            ("games", "bytes"),
            ("games", "bytes_on_disk"),
            ("games", "scan_route"),
            ("scan_runs", "scan_ms"),
            ("scan_runs", "analyze_ms"),
            ("scan_runs", "total_ms"),
        ] {
            assert!(
                column_exists(&conn, table, column).expect("probe migrated column"),
                "{table}.{column} must exist after migrate",
            );
        }

        let stats: Option<i64> = conn
            .query_row("SELECT files FROM games", [], |row| row.get(0))
            .expect("read the pre-existing row");
        assert_eq!(stats, None, "a row from an older build has no counts");

        migrate(&conn).expect("second migrate must be a no-op, not a duplicate-column error");
    }

    /// `migrate` must add `files.size_on_disk` (allocated-size accounting) to a database created
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

    /// game-state tracking: `games.build_id` must be added to a database created before that
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

    #[test]
    fn schema_v1_migrates_to_v2_with_orphan_evidence_linkage() {
        let conn = Connection::open_in_memory().expect("open bare in-memory db");
        conn.execute_batch(
            "CREATE TABLE file_safety (
                file_id          INTEGER PRIMARY KEY,
                scan_id          INTEGER NOT NULL,
                trusted_root     TEXT NOT NULL,
                rel_path         TEXT NOT NULL,
                root_identity    TEXT,
                target_identity  TEXT,
                target_kind      TEXT,
                tree_fingerprint TEXT,
                block_reason     TEXT
            );
            PRAGMA user_version = 1;",
        )
        .expect("create schema-v1 safety table");

        migrate(&conn).expect("v1 to v2 migration");

        assert!(
            column_exists(&conn, "file_safety", "evidence_library_path").expect("probe v2 column")
        );
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read user_version");
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
    }

    /// `migrate_v4` must drop `idx_files_scan_id` from a schema-v3 database
    /// that still has it (measurements showed the index costs real time to
    /// maintain on insert and on the generation-wide deletes and buys
    /// nothing back anywhere it is consulted - see `migrate_v4`'s doc
    /// comment), reach `CURRENT_SCHEMA_VERSION`, and be idempotent.
    #[test]
    fn schema_v3_migrates_to_v4_by_dropping_the_files_scan_id_index() {
        let conn = Connection::open_in_memory().expect("open bare in-memory db");
        conn.execute_batch(
            "CREATE TABLE files (
                id           INTEGER PRIMARY KEY,
                scan_id      INTEGER NOT NULL DEFAULT 0,
                game_id      INTEGER,
                rel_path     TEXT NOT NULL,
                size         INTEGER NOT NULL,
                size_on_disk INTEGER,
                mtime        INTEGER
            );
            CREATE INDEX idx_files_scan_id ON files(scan_id);
            PRAGMA user_version = 3;",
        )
        .expect("create schema-v3 files table with the old index");

        let has_index = |conn: &Connection| -> bool {
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'index' AND name = 'idx_files_scan_id'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("probe sqlite_master")
                > 0
        };
        assert!(
            has_index(&conn),
            "precondition: the schema-v3 table must have the index"
        );

        migrate(&conn).expect("v3 to v4 migration");

        assert!(
            !has_index(&conn),
            "idx_files_scan_id must be dropped after migrating to v4"
        );
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read user_version");
        assert_eq!(version, CURRENT_SCHEMA_VERSION);

        migrate(&conn).expect("second migrate must be a no-op, not an error on a missing index");
    }

    #[test]
    fn a_newer_unknown_schema_version_is_rejected_without_downgrade() {
        let conn = Connection::open_in_memory().expect("open bare in-memory db");
        conn.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION + 1)
            .expect("set future version");

        let error = migrate(&conn).expect_err("future schema must not be opened");

        assert!(error.to_string().contains("newer than supported"));
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read future version");
        assert_eq!(version, CURRENT_SCHEMA_VERSION + 1);
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

        // Occupancy comes from the per-game totals, which is what a scan
        // writes for the whole install - `files` only ever holds the flagged
        // subset, so setting it here would describe a different quantity.
        for (game_id, bytes) in [(1, 100), (2, 200), (3, 50)] {
            conn.execute(
                "UPDATE games SET files = 1, bytes = ?2, bytes_on_disk = ?2 WHERE id = ?1",
                params![game_id, bytes],
            )
            .expect("set per-game totals");
        }

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

    /// Occupancy describes the whole install, not the flagged part of it.
    ///
    /// This is a shipped regression, pinned so it cannot come back: when
    /// `files` was narrowed to hold only flagged files, this function was
    /// still summing that table, so a 23 TB library reported 744 GB and the
    /// "selected frees N %" line beneath it read 99.9 %. The fixture is built
    /// the way a real scan leaves the database - per-game totals covering
    /// every file, a `files` row only for the flagged one - so summing the
    /// wrong table fails it.
    #[test]
    fn occupied_by_library_counts_unflagged_files_too() {
        let conn = open_in_memory().expect("in-memory db should open");
        conn.execute(
            "INSERT INTO game_libraries (vendor, path) VALUES ('steam', 'D:/SteamLibrary')",
            [],
        )
        .expect("insert library");
        // 1000 bytes on disk across three files; one of them is flagged.
        conn.execute(
            "INSERT INTO games (library_id, name, install_dir, files, bytes, bytes_on_disk) \
             VALUES (1, 'Game A', 'D:/SteamLibrary/A', 3, 1000, 1000)",
            [],
        )
        .expect("insert game");
        conn.execute(
            "INSERT INTO files (game_id, rel_path, size, size_on_disk, mtime) \
             VALUES (1, 'manual.pdf', 40, 40, NULL)",
            [],
        )
        .expect("insert the one flagged file");

        let by_library = occupied_by_library(&conn).expect("aggregate by library");

        assert_eq!(
            by_library.get(&1),
            Some(&1000u64),
            "occupancy must be the whole install, not the 40 bytes that happen to be flagged"
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
