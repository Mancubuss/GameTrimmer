//! The database projections that make up a diagnostic bundle.
//!
//! Every function here returns a `serde_json::Value` rather than a typed
//! struct. That is deliberate: these shapes exist only to be serialized
//! once and read by a human on the other end of a bug report, so a named
//! type per section would be a second description of the same thing to keep
//! in sync with the SQL. The manifest's `schema_version` is what a reader
//! keys off, not a Rust type.
//!
//! What is *not* here is as load-bearing as what is. No machine GUID, no
//! volume serial, no content hash, and game titles only when the user opts
//! in - see `super::BundleOptions`.

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::error::Result;

/// The cap on any opt-in detail list. Measured on a real library: 50 000
/// rows is 1.3 MB compressed and well under a second to write, while the
/// unbounded version of the same list is 16 MB and about twenty seconds -
/// no longer a file anyone reviews before sending. Truncation is declared
/// in the payload, never silent.
pub const DETAIL_ROW_CAP: usize = 50_000;

/// How many findings the always-included sample carries. Small enough that
/// the default bundle stays in the tens of kilobytes.
pub const FINDINGS_SAMPLE: usize = 500;

/// `PRAGMA`-level facts about the database file: is it intact, how much of
/// it is free space, and which journal mode actually took effect.
///
/// `integrity_check` is asked for one row only. The full check on a large
/// database is slow and the bundle wants "is it damaged", which the first
/// row answers - `ok` or the first complaint.
pub fn db_health(conn: &Connection) -> Result<Value> {
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let freelist_count: i64 = conn.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    let journal_mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let integrity: String = conn
        .query_row("PRAGMA integrity_check(1)", [], |row| row.get(0))
        .unwrap_or_else(|err| format!("integrity_check failed: {err}"));

    Ok(json!({
        "page_count": page_count,
        "page_size": page_size,
        "freelist_count": freelist_count,
        "journal_mode": journal_mode,
        "user_version": user_version,
        "current_schema_version": crate::db::CURRENT_SCHEMA_VERSION,
        "integrity_check": integrity,
        "active_scan_id": crate::db::active_scan_id(conn)?,
    }))
}

/// Every scan generation, newest first, with the v3 phase timings.
///
/// All of them rather than only the active one: "slower every time" and
/// "an interrupted scan left staging rows behind" are both questions about
/// the *sequence* of runs, and neither is visible in a single row.
pub fn scan_runs(conn: &Connection) -> Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT id, started_at, completed_at, state, discovery_status,
                scan_ms, analyze_ms, total_ms
         FROM scan_runs ORDER BY id DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(json!({
            "id": row.get::<_, i64>(0)?,
            "started_at": row.get::<_, i64>(1)?,
            "completed_at": row.get::<_, Option<i64>>(2)?,
            "state": row.get::<_, String>(3)?,
            "discovery_status": row.get::<_, String>(4)?,
            "scan_ms": row.get::<_, Option<i64>>(5)?,
            "analyze_ms": row.get::<_, Option<i64>>(6)?,
            "total_ms": row.get::<_, Option<i64>>(7)?,
        }))
    })?;
    Ok(Value::Array(rows.collect::<rusqlite::Result<_>>()?))
}

/// Per-game facts for the active generation: where it came from, which way
/// it was scanned, and how much of it there is.
///
/// `include_titles` is the whole privacy question for this section. A
/// library of a few dozen titles is close to a fingerprint, so the default
/// replaces each name with its slot (`Game 1`, `Game 2`) while keeping
/// everything that explains a scan - the vendor, the route, the counts.
pub fn games(conn: &Connection, include_titles: bool) -> Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT g.id, g.name, gl.vendor, gl.path, g.install_dir,
                g.files, g.bytes, g.bytes_on_disk, g.scan_route
         FROM games g
         LEFT JOIN game_libraries gl ON gl.id = g.library_id
         WHERE g.scan_id = (SELECT active_scan_id FROM scan_state WHERE singleton = 1)
         ORDER BY g.id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<i64>>(5)?,
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, Option<i64>>(7)?,
            row.get::<_, Option<String>>(8)?,
        ))
    })?;

    let games: Vec<Value> = rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .enumerate()
        .map(
            |(index, (id, name, vendor, library, install_dir, files, bytes, on_disk, route))| {
                json!({
                    "id": id,
                    "name": if include_titles { name } else { format!("Game {}", index + 1) },
                    "vendor": vendor,
                    "library": library,
                    "install_dir": install_dir,
                    "files": files,
                    "bytes": bytes,
                    "bytes_on_disk": on_disk,
                    "scan_route": route,
                })
            },
        )
        .collect();
    Ok(Value::Array(games))
}

/// A sample of findings from the active generation, with the provenance
/// that explains why each one was flagged.
///
/// A sample and not the set: the whole point of a finding row here is "show
/// me why this rule fired", which `limit` rows answer as well as 720 000
/// would. `total` is reported alongside so a reader can see what was left
/// out rather than mistaking the sample for the whole.
pub fn findings(conn: &Connection, include_titles: bool, limit: usize) -> Result<Value> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM findings fi
         JOIN files f ON f.id = fi.file_id
         WHERE f.scan_id = (SELECT active_scan_id FROM scan_state WHERE singleton = 1)",
        [],
        |row| row.get(0),
    )?;

    let mut stmt = conn.prepare(
        "SELECT fi.category, fi.rule_id, fi.confidence, fi.lang_tag, fi.provenance,
                f.rel_path, f.size, f.size_on_disk, g.name, gl.path, fs.block_reason
         FROM findings fi
         JOIN files f ON f.id = fi.file_id
         LEFT JOIN games g ON g.id = f.game_id
         LEFT JOIN game_libraries gl ON gl.id = g.library_id
         LEFT JOIN file_safety fs ON fs.file_id = f.id
         WHERE f.scan_id = (SELECT active_scan_id FROM scan_state WHERE singleton = 1)
         ORDER BY f.id
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |row| {
        Ok(json!({
            "category": row.get::<_, String>(0)?,
            "rule_id": row.get::<_, Option<String>>(1)?,
            "confidence": row.get::<_, i64>(2)?,
            "lang_tag": row.get::<_, Option<String>>(3)?,
            "provenance": row.get::<_, String>(4)?,
            "rel_path": row.get::<_, String>(5)?,
            "size": row.get::<_, i64>(6)?,
            "size_on_disk": row.get::<_, Option<i64>>(7)?,
            "game": row.get::<_, Option<String>>(8)?.map(|name| {
                if include_titles { name } else { "<title omitted>".to_string() }
            }),
            "library": row.get::<_, Option<String>>(9)?,
            "block_reason": row.get::<_, Option<String>>(10)?,
        }))
    })?;
    let sample: Vec<Value> = rows.collect::<rusqlite::Result<_>>()?;

    Ok(json!({
        "total_rows": total,
        "included_rows": sample.len(),
        "truncated": (sample.len() as i64) < total,
        "rows": sample,
    }))
}

/// Counts over the deletion journal, grouped the way the questions are
/// asked: how many operations reached which outcome.
///
/// Always included, because it is aggregate and carries no path at all -
/// the row detail that does is opt-in below.
pub fn operations_summary(conn: &Connection) -> Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT status, COALESCE(outcome, '<none>'), COUNT(*)
         FROM operations GROUP BY status, outcome ORDER BY status, outcome",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(json!({
            "status": row.get::<_, String>(0)?,
            "outcome": row.get::<_, String>(1)?,
            "count": row.get::<_, i64>(2)?,
        }))
    })?;
    Ok(Value::Array(rows.collect::<rusqlite::Result<_>>()?))
}

/// The deletion journal row by row - opt-in, capped, and path-tokenized by
/// the redaction pass like everything else.
pub fn operations_detail(conn: &Connection) -> Result<Value> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM operations", [], |row| row.get(0))?;

    let mut stmt = conn.prepare(
        "SELECT id, ts, action, status, outcome, error, scan_id, rel_path, trusted_root,
                completed_at
         FROM operations ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([DETAIL_ROW_CAP as i64], |row| {
        Ok(json!({
            "id": row.get::<_, i64>(0)?,
            "ts": row.get::<_, i64>(1)?,
            "action": row.get::<_, String>(2)?,
            "status": row.get::<_, String>(3)?,
            "outcome": row.get::<_, Option<String>>(4)?,
            "error": row.get::<_, Option<String>>(5)?,
            "scan_id": row.get::<_, Option<i64>>(6)?,
            "rel_path": row.get::<_, Option<String>>(7)?,
            "trusted_root": row.get::<_, Option<String>>(8)?,
            "completed_at": row.get::<_, Option<i64>>(9)?,
        }))
    })?;
    let detail: Vec<Value> = rows.collect::<rusqlite::Result<_>>()?;

    Ok(json!({
        "total_rows": total,
        "included_rows": detail.len(),
        "truncated": (detail.len() as i64) < total,
        "rows": detail,
    }))
}

/// Per-provider discovery and routing diagnostics for the active scan -
/// the rows GT-116 and GT-118 now write, plus everything the providers
/// already recorded.
pub fn scan_diagnostics(conn: &Connection) -> Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT provider, stage, path, message FROM scan_diagnostics
         WHERE scan_id = (SELECT active_scan_id FROM scan_state WHERE singleton = 1)
         ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(json!({
            "provider": row.get::<_, String>(0)?,
            "stage": row.get::<_, String>(1)?,
            "path": row.get::<_, Option<String>>(2)?,
            "message": row.get::<_, String>(3)?,
        }))
    })?;
    Ok(Value::Array(rows.collect::<rusqlite::Result<_>>()?))
}

/// The library roots the redaction pass needs in order to tokenize them.
/// Read separately from [`games`] because the token map has to exist before
/// any section is serialized.
pub fn library_roots(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT path FROM game_libraries ORDER BY id")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Counts used by `summary.txt`. One query rather than one per line so the
/// preview stays cheap enough to regenerate on every toggle.
pub fn counts(conn: &Connection) -> Result<(i64, i64, i64)> {
    let games: i64 = conn.query_row(
        "SELECT COUNT(*) FROM games
         WHERE scan_id = (SELECT active_scan_id FROM scan_state WHERE singleton = 1)",
        [],
        |row| row.get(0),
    )?;
    let files: i64 = conn.query_row(
        "SELECT COUNT(*) FROM files
         WHERE scan_id = (SELECT active_scan_id FROM scan_state WHERE singleton = 1)",
        [],
        |row| row.get(0),
    )?;
    let findings: i64 = conn.query_row(
        "SELECT COUNT(*) FROM findings fi JOIN files f ON f.id = fi.file_id
         WHERE f.scan_id = (SELECT active_scan_id FROM scan_state WHERE singleton = 1)",
        [],
        |row| row.get(0),
    )?;
    Ok((games, files, findings))
}
