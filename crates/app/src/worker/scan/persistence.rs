//! Atomic scan-generation persistence and the single bounded writer.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;

use rusqlite::{params, Connection};

use gametrimmer_core::db;
use gametrimmer_core::error::{CoreError, Result as CoreResult};
use gametrimmer_core::perf;
use gametrimmer_core::providers::{self, DiscoveredLibrary};
use gametrimmer_core::rules::RuleProvenance;
use gametrimmer_core::scanner::{store_files_no_tx, ScanStats};

use crate::i18n::Verb;
use crate::model::{source_key, FindingRow, LibraryOrigin};

use super::{manual, GameOutcome, Notifier, PreparedGame, WorkerMsg, WRITE_BATCH_SIZE};

/// The single database writer: receives every game's scan outcome and
/// persists it, batching `WRITE_BATCH_SIZE` games per transaction to keep
/// the number of commits (and WAL syncs) low regardless of how many files a
/// game has. Sends one `Progress` message per finished game, in whatever
/// order results arrive (scanning is parallel, so this is no longer
/// necessarily the games' discovery order).
///
/// `scan_id` is the generation being staged, handed down from `run_scan`
/// (which got it from `db::begin_scan`) rather than re-read from the
/// database per game: it is fixed for the whole run, and every row this
/// writer inserts belongs to it.
pub(super) fn run_writer(
    conn: &mut Connection,
    result_rx: Receiver<GameOutcome>,
    notifier: &Notifier,
    total: usize,
    completed: &std::sync::atomic::AtomicUsize,
    cancel: &AtomicBool,
    scan_id: i64,
) -> CoreResult<Vec<FindingRow>> {
    let mut findings = Vec::new();
    let mut batch: Vec<PreparedGame> = Vec::with_capacity(WRITE_BATCH_SIZE);

    for (index, outcome) in result_rx.iter().enumerate() {
        let done = index + 1;
        // Publish the completed count for the scan workers' "started" progress
        // (see `dispatch_scans`). Only this thread writes it, so a plain store
        // of the running total is enough.
        completed.store(done, Ordering::Relaxed);
        match outcome {
            GameOutcome::Scanned(prepared) => {
                notifier.send(WorkerMsg::Progress {
                    verb: Verb::Analyze,
                    current: done,
                    total,
                    detail: prepared.name.clone(),
                });
                batch.push(prepared);
                if batch.len() >= WRITE_BATCH_SIZE {
                    flush_batch(conn, &mut batch, &mut findings, scan_id)?;
                }
            }
            GameOutcome::Failed {
                name,
                install_dir,
                error,
            } => {
                // Cancellation travels through the same error channel but is
                // not a failure - marking it ERROR would put a line in
                // `errors.txt` for every scan the user stopped on purpose.
                let cancelled = error.to_string() == "cancelled";
                let message = if cancelled {
                    "cancelled".to_string()
                } else {
                    format!(
                        "failed to scan \"{name}\" ({}): {error}",
                        install_dir.display()
                    )
                };
                if cancelled {
                    crate::logger::log(&message);
                } else {
                    crate::logger::error(&message);
                }
                return Err(CoreError::Other(message));
            }
        }

        // Once cancelled, stop accepting more batches promptly rather than
        // draining (and writing) the rest of an already-large in-flight
        // backlog; already-completed games up to this point are still
        // flushed below so their writes are not lost.
        if cancel.load(Ordering::Relaxed) {
            break;
        }
    }

    flush_batch(conn, &mut batch, &mut findings, scan_id)?;
    Ok(findings)
}

/// Commits one batch of games' `files`/`findings` writes in a single
/// transaction. Any insert or commit failure rolls the whole batch back and
/// aborts the writer, so the staging generation cannot be activated with a
/// partial game set. UI rows are published only after the commit succeeds.
fn flush_batch(
    conn: &mut Connection,
    batch: &mut Vec<PreparedGame>,
    findings: &mut Vec<FindingRow>,
    scan_id: i64,
) -> CoreResult<()> {
    if batch.is_empty() {
        return Ok(());
    }

    // Timed as a whole batch, commit included: the writer is a single thread
    // and this is everything it does, so the figure is directly comparable
    // with the per-game stages the workers are charged for (`crate::perf`).
    let write_started = std::time::Instant::now();
    let db_tx = conn.transaction()?;
    let mut pending_rows = Vec::new();
    for prepared in batch.iter() {
        let mut rows = persist_prepared_game(&db_tx, prepared, scan_id).map_err(|err| {
            CoreError::Other(format!(
                "failed to write \"{}\" to the database: {err}",
                prepared.name
            ))
        })?;
        pending_rows.append(&mut rows);
    }
    let commit_started = std::time::Instant::now();
    db_tx.commit()?;
    perf::add(perf::Stage::PersistCommit, commit_started.elapsed());
    perf::add(perf::Stage::Persist, write_started.elapsed());
    findings.append(&mut pending_rows);
    batch.clear();
    Ok(())
}

/// One game as this scan wrote it: the row id its files and findings hang
/// off, and the three things the rest of the run needs about it.
///
/// `app_id` rides along because classification now depends on it - a rule may
/// be scoped to one game (see `gametrimmer_core::rules::Rule::app_id`), and a
/// personal exception always is. It is the game's *vendor* id, so it means the
/// same thing in the next generation, which `id` deliberately does not.
pub(super) struct ScannedGame {
    pub id: i64,
    pub name: String,
    pub install_dir: PathBuf,
    /// `None` for a game no launcher gave an id - a folder-scan discovery or
    /// a manually added library.
    pub app_id: Option<String>,
}

/// Writes discovered libraries and their games into the database,
/// replacing each library's game list (`INSERT OR IGNORE` on the library
/// itself, keyed by path; full delete+reinsert of its games).
///
/// Rescanning a library that already has data must not fail: `games.id` is
/// referenced by `files.game_id`, and `files.id` by `findings.file_id`,
/// neither with `ON DELETE CASCADE`, and `PRAGMA foreign_keys = ON` is set
/// (see `db::configure`). So before a library's old `games` rows are
/// deleted, their `files` and (transitively) `findings` rows must be
/// deleted first, child-to-parent, in the same transaction - otherwise
/// SQLite raises `FOREIGN KEY constraint failed`. This also takes care of
/// games that disappeared from a library between scans: their rows are
/// unconditionally part of the old set being replaced, so no orphaned
/// `files`/`findings` rows are left behind for them either.
///
/// The library id itself is always resolved via `SELECT ... WHERE path`
/// after the `INSERT OR IGNORE`, never via `last_insert_rowid()`: on a
/// no-op ignore (the library already exists) `last_insert_rowid()` would
/// return whatever row - in whatever table - was last inserted on this
/// connection, not this library's id.
pub(super) fn persist_libraries(
    conn: &Connection,
    libraries: &[DiscoveredLibrary],
    scan_id: i64,
) -> CoreResult<Vec<ScannedGame>> {
    // Foreign-key enforcement is disabled for the whole delete+reinsert.
    // This is the silent 15-20s phase at the start of every scan on a large
    // library: the three `DELETE ... WHERE library_id = ?` statements below
    // otherwise pay a per-row child-existence check for every one of the
    // (millions of) `files`/`findings` rows being replaced. Integrity is
    // preserved regardless by deleting child-before-parent (findings ->
    // files -> games); the checks it skips were only re-proving that
    // ordering. `with_foreign_keys_off` restores enforcement before
    // returning, so the per-game writes that follow (see `run_writer`) run
    // with it back on. It also opens the transaction via
    // `unchecked_transaction` (needing only `&Connection`), since the
    // `foreign_keys` pragma may not be toggled inside an open transaction.
    db::with_foreign_keys_off(conn, |conn| {
        let tx = conn.unchecked_transaction()?;
        let mut games = Vec::new();

        for library in libraries {
            let path_str = library.path.to_string_lossy().to_string();

            // Upsert, not `INSERT OR IGNORE` (manual/discovered library reconciliation): on a path already known
            // the ignore kept whatever vendor was stored first - typically
            // `manual`, from the user registering the folder by hand before
            // any provider knew it - so the stored vendor never caught up with
            // what discovery had since learned.
            //
            // What that actually costs, having traced it: the library list in
            // the UI (`manual::list_libraries`) keeps labelling a real Steam
            // library "manual", and `discover_manual_libraries`, which selects
            // `WHERE vendor = 'manual'`, re-enumerates that path as a manual
            // library on every later scan - redundant work resolved each time
            // by `merge_libraries_by_path`. Orphan detection is NOT affected,
            // contrary to what this comment first claimed: `collect_orphans`
            // runs `orphan_spec_for` over the in-memory `DiscoveredLibrary`
            // list, whose `vendor` is the provider's own `&'static str`, and
            // never reads this column.
            //
            // `manual` is a floor, never a destination: a scan where a
            // provider dropped out (registry key missing, drive briefly
            // absent) re-offers the folder as `manual`, and demoting on that
            // would switch orphan detection off exactly when a scan half
            // failed. Any other vendor is the provider's verdict from this
            // scan and wins - `merge_libraries_by_path` has already reduced a
            // path to its single best-known vendor by the time we get here.
            tx.execute(
                "INSERT INTO game_libraries (vendor, path) VALUES (?1, ?2)
                 ON CONFLICT(path) DO UPDATE SET vendor = excluded.vendor
                 WHERE excluded.vendor <> ?3 AND vendor <> excluded.vendor",
                params![library.vendor, path_str, manual::MANUAL_VENDOR],
            )?;
            let library_id: i64 = tx.query_row(
                "SELECT id FROM game_libraries WHERE path = ?1",
                params![path_str],
                |row| row.get(0),
            )?;

            // Retrying preparation of the same staging generation is allowed,
            // but rows from the active/previous generations are immutable.
            tx.execute(
                "DELETE FROM file_safety WHERE file_id IN (
                    SELECT id FROM files WHERE scan_id = ?1 AND game_id IN (
                        SELECT id FROM games WHERE library_id = ?2 AND scan_id = ?1
                    )
                )",
                params![scan_id, library_id],
            )?;
            tx.execute(
                "DELETE FROM findings WHERE file_id IN (
                    SELECT id FROM files WHERE scan_id = ?1 AND game_id IN (
                        SELECT id FROM games WHERE library_id = ?2 AND scan_id = ?1
                    )
                )",
                params![scan_id, library_id],
            )?;
            tx.execute(
                "DELETE FROM files WHERE scan_id = ?1 AND game_id IN
                 (SELECT id FROM games WHERE library_id = ?2 AND scan_id = ?1)",
                params![scan_id, library_id],
            )?;
            tx.execute(
                "DELETE FROM games WHERE library_id = ?1 AND scan_id = ?2",
                params![library_id, scan_id],
            )?;

            let build_ids = build_ids_for(library);

            for game in &library.games {
                // build-ID history: record now, show later. The build id costs nothing to
                // store and nothing in the UI shows it yet, but users of v1
                // start accumulating history from their first scan - so the
                // "what came back" diff in a later release works for them
                // immediately, instead of only after one more full scan.
                let build_id = game
                    .app_id
                    .as_ref()
                    .and_then(|app_id| build_ids.get(app_id.as_str()));

                tx.execute(
                    "INSERT INTO games (scan_id, library_id, name, install_dir, app_id, build_id) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        scan_id,
                        library_id,
                        game.name,
                        game.install_dir.to_string_lossy(),
                        game.app_id,
                        build_id
                    ],
                )?;
                games.push(ScannedGame {
                    id: tx.last_insert_rowid(),
                    name: game.name.clone(),
                    install_dir: game.install_dir.clone(),
                    app_id: game.app_id.clone(),
                });
            }
        }

        tx.commit()?;
        Ok(games)
    })
}

/// Content build ids for one library, keyed by vendor app id (build-ID history).
///
/// Only Steam publishes one: `buildid` in each `appmanifest_*.acf`, bumped by
/// Valve on every content update and by a `Verify` that re-downloads files.
/// Every other vendor yields an empty map, so their games are stored with
/// `build_id = NULL` - which `gamestate::changed_games` reads as "unknown,
/// claim nothing" rather than "changed".
///
/// Cheap by construction: this reads a few dozen small text files, never
/// walking the games themselves.
///
/// Unreadable manifests are not a scan failure. A build id is only ever used
/// to report that a game changed since the last scan, and that report already
/// refuses to claim a change it cannot evidence - so losing one costs a line
/// in a summary, and nothing in the deletion path consults it. Failing the
/// whole scan here would mean a Steam library that went offline mid-scan takes
/// every other library's results down with it, which is the opposite of what
/// per-library evidence is for.
fn build_ids_for(library: &DiscoveredLibrary) -> HashMap<String, String> {
    if library.vendor != "steam" {
        return HashMap::new();
    }

    match providers::steam::manifest_states(&library.path) {
        Ok(states) => states
            .into_iter()
            .filter_map(|state| Some((state.app_id, state.build_id?)))
            .collect(),
        Err(err) => {
            crate::logger::error(&format!(
                "build ids unavailable for {}: {err}",
                library.path.display()
            ));
            HashMap::new()
        }
    }
}

/// Persists one already-scanned-and-classified game: replaces its indexed
/// files and inserts its findings, returning them for the UI. Uses whatever
/// transaction (if any) is already open on `conn` - callers that want a
/// single game per commit pass a fresh `Transaction`; the scan pipeline's
/// writer thread instead shares one transaction across a batch of games
/// (see `WRITE_BATCH_SIZE`).
///
/// `scan_id` is passed in rather than read back from `games`: the caller has
/// held it since `db::begin_scan`, and it is the same value for every game in
/// the run, so querying it per game only bought a round trip.
pub(super) fn persist_prepared_game(
    conn: &Connection,
    prepared: &PreparedGame,
    scan_id: i64,
) -> CoreResult<Vec<FindingRow>> {
    // Every span charged to `PersistSql` below is time inside SQLite. What is
    // left of `Persist` after it and the commit is this function's own work:
    // one `FindingRow` per finding and two `FileIdentity::encode` calls. See
    // `perf::persist_breakdown` for why the three are being told apart.
    let mut sql = std::time::Duration::ZERO;
    let sql_started = std::time::Instant::now();

    // `findings.file_id` and `file_safety.file_id` have no `ON DELETE CASCADE`, and
    // `store_files_no_tx` is about to delete this game's old `files` rows - drop their
    // findings and safety records first, while the old ids are still known.
    conn.execute(
        "DELETE FROM file_safety WHERE file_id IN (SELECT id FROM files WHERE game_id = ?1)",
        params![prepared.game_id],
    )?;
    conn.execute(
        "DELETE FROM findings WHERE file_id IN (SELECT id FROM files WHERE game_id = ?1)",
        params![prepared.game_id],
    )?;
    // The anti-cheat verdict is decided once, here, from the complete
    // inventory this scan just walked (`classify_game`), and stored so every
    // later reader - the delete preflight, the startup load - can read the
    // same answer instead of re-deriving its own. A game whose scan never
    // reaches this line keeps `NULL`, which reads as protected.
    conn.execute(
        "UPDATE games SET anti_cheat_protected = ?2 WHERE id = ?1",
        params![prepared.game_id, prepared.anti_cheat_protected],
    )?;
    sql += sql_started.elapsed();

    // Per-game totals cover the *whole* install - every file, flagged or not.
    // They are what the UI's occupancy figures are built from, so they are
    // computed from `entries` rather than from the rows written below, which
    // are only the flagged subset. Per-game file and byte counts are also the
    // natural unit for explaining a slow scan ("this one game holds 400 000
    // files"), which the totals alone never showed.
    let stats = ScanStats::of(&prepared.entries);

    // Only the flagged files get a row in `files`.
    // The findings are in strictly ascending `entry_index` order, one per
    // entry, because `classify_game` builds them by walking `entries` once -
    // so handing their files over in that order makes `file_ids[i]` the id of
    // `prepared.findings[i]`'s file.
    //
    // Unflagged protected containers used to get a row here too, left over
    // from the in-place archive trimmer that read them. Nothing has read them
    // since it was removed: every other consumer reaches this table through
    // `JOIN files f ON f.id = fi.file_id` (see `store_files_no_tx`), and an
    // unflagged container has no finding to join through - by construction,
    // because the classifier vetoes it before one can exist. They were
    // written on every scan, deleted by the next, and read by nobody. The
    // delete preflight's own container block is by path
    // (`worker::is_protected_container` on the row it already has), so it
    // never depended on these rows either.
    debug_assert!(
        prepared
            .findings
            .windows(2)
            .all(|pair| pair[0].entry_index < pair[1].entry_index),
        "findings must be in strictly ascending entry order for positional file ids"
    );

    let flagged_entries = prepared
        .findings
        .iter()
        .map(|finding| &prepared.entries[finding.entry_index]);

    let sql_started = std::time::Instant::now();
    let file_ids = store_files_no_tx(conn, scan_id, prepared.game_id, flagged_entries)?;
    conn.execute(
        "UPDATE games SET files = ?2, bytes = ?3, bytes_on_disk = ?4 WHERE id = ?1",
        params![
            prepared.game_id,
            stats.files as i64,
            stats.bytes as i64,
            stats.bytes_on_disk as i64
        ],
    )?;
    sql += sql_started.elapsed();

    // One read serves two purposes: the path is this game's safety evidence,
    // and (vendor, path) together are the row's library attribution. Both are
    // read from `game_libraries` rather than from the in-memory
    // `DiscoveredLibrary`, so a later `worker::load` - which can only read this
    // table - reconstructs exactly the same attribution.
    let sql_started = std::time::Instant::now();
    let (library_vendor, evidence_library_path): (String, String) = conn.query_row(
        "SELECT gl.vendor, gl.path FROM games g
         JOIN game_libraries gl ON gl.id = g.library_id
         WHERE g.id = ?1",
        [prepared.game_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let library = LibraryOrigin {
        vendor: Some(library_vendor),
        root: PathBuf::from(&evidence_library_path),
    };
    let evidence_status: Option<String> = conn
        .query_row(
            "SELECT status FROM scan_library_evidence
             WHERE scan_id = ?1 AND library_path = ?2",
            params![scan_id, &evidence_library_path],
            |row| row.get(0),
        )
        .ok();
    sql += sql_started.elapsed();
    // `true`: this path only ever persists findings belonging to a known game
    // (`prepared.game_id`); orphan candidates are persisted elsewhere. Shared
    // with the delete preflight and the load query so all three agree - see
    // `gametrimmer_core::safety::discovery_block_reason`.
    let evidence_block_reason =
        gametrimmer_core::safety::discovery_block_reason(true, evidence_status.as_deref())
            .map(str::to_string);

    let mut rows = Vec::with_capacity(prepared.findings.len());
    let mut insert_finding = conn.prepare_cached(
        "INSERT INTO findings
         (file_id, category, rule_id, confidence, lang_tag, group_dir, provenance, action) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    let mut insert_safety = conn.prepare_cached(
        "INSERT OR REPLACE INTO file_safety
         (file_id, scan_id, evidence_library_path, trusted_root, rel_path, root_identity,
          target_identity, target_kind, tree_fingerprint, block_reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;

    // There used to be a counter here for findings whose `files` row could
    // not be found, plus a `scan_diagnostic` row explaining the gap - the
    // exact shape of "findings vanish between runs" when the lookup was a
    // `rel_path` string match against a map. Resolving by position removes
    // the failure rather than reporting it: the flagged files were handed to
    // `store_files_no_tx` in finding order, and it returned exactly one id
    // per row it inserted, so there is no longer a case in which a finding
    // has no row. The `debug_assert` pins that invariant where it is set up
    // instead of re-checking it 720 000 times in release.
    debug_assert!(
        file_ids.len() >= prepared.findings.len(),
        "store_files_no_tx must return at least one id per finding, in finding order"
    );

    for (position, finding) in prepared.findings.iter().enumerate() {
        let file_id = file_ids[position];

        let sql_started = std::time::Instant::now();
        // `action` has carried no meaning since the in-place archive trimmer
        // was removed - every finding is a whole-file delete now. The
        // column stays in the schema (existing databases keep whatever an
        // older build wrote into it), but nothing this build writes reads
        // it back as anything but the legacy blank/`NULL` representation.
        insert_finding.execute(params![
            file_id,
            source_key(finding.source),
            finding.rule_id,
            finding.confidence,
            finding.lang_tag.as_deref(),
            finding.group_dir.as_deref(),
            match finding.provenance {
                RuleProvenance::Builtin => "builtin",
                RuleProvenance::ImportedUntrusted => "imported_untrusted",
            },
            None::<String>,
        ])?;
        sql += sql_started.elapsed();

        // Captured on the scan pool (see `PreparedFinding::safety`); the
        // writer only records the outcome.
        let deletion_block_reason = match &finding.safety {
            Ok(snapshot) => {
                // Hoisted out of `params!` so the two `format!`s in `encode`
                // and the two `to_string_lossy` calls are charged to this
                // function rather than to SQLite - which is the whole
                // question this instrumentation exists to answer.
                let trusted_root = snapshot.trusted_root.to_string_lossy();
                let rel_path = snapshot.rel_path.to_string_lossy();
                let root_identity = snapshot.root_identity.encode();
                let target_identity = snapshot.target_identity.encode();
                let sql_started = std::time::Instant::now();
                insert_safety.execute(params![
                    file_id,
                    scan_id,
                    &evidence_library_path,
                    trusted_root,
                    rel_path,
                    root_identity,
                    target_identity,
                    snapshot.target_identity.kind.as_str(),
                    &snapshot.tree_fingerprint,
                    None::<String>,
                ])?;
                sql += sql_started.elapsed();
                evidence_block_reason.clone()
            }
            Err(reason) => {
                let reason = reason.clone();
                let sql_started = std::time::Instant::now();
                insert_safety.execute(params![
                    file_id,
                    scan_id,
                    &evidence_library_path,
                    prepared.install_dir.to_string_lossy(),
                    finding.rel_path,
                    None::<String>,
                    None::<String>,
                    None::<String>,
                    None::<String>,
                    &reason,
                ])?;
                sql += sql_started.elapsed();
                Some(reason)
            }
        };

        rows.push(FindingRow {
            file_id,
            game_id: prepared.game_id,
            game_name: prepared.name.clone(),
            app_id: prepared.app_id.clone(),
            install_dir: prepared.install_dir.clone(),
            rel_path: finding.rel_path.clone(),
            size: finding.size,
            size_on_disk: finding.size_on_disk,
            source: finding.source,
            rule_desc: finding.rule_id.clone(),
            confidence: finding.confidence,
            lang_tag: finding.lang_tag.clone(),
            group_dir: finding.group_dir.clone(),
            deletion_block_reason,
            imported_untrusted: finding.provenance == RuleProvenance::ImportedUntrusted,
            library: Some(library.clone()),
            anti_cheat_protected: prepared.anti_cheat_protected,
        });
    }

    perf::add(perf::Stage::PersistSql, sql);
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use gametrimmer_core::scanner::FileEntry;
    use gametrimmer_core::worker::PreparedFinding;

    use crate::model::FindingSource;

    use super::*;

    /// Seeds a real `game_libraries`/`games` row pair and returns the game's
    /// id - `persist_prepared_game` looks the game back up (its vendor/path
    /// JOIN) to build the finding's `LibraryOrigin`, so a `PreparedGame`
    /// naming a `game_id` with no backing row fails before ever reaching the
    /// commit this test is actually about.
    fn seed_game(conn: &Connection, scan_id: i64) -> i64 {
        conn.execute(
            "INSERT INTO game_libraries (vendor, path) VALUES ('test', 'D:\\Library')",
            [],
        )
        .expect("seed library");
        let library_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO games (scan_id, library_id, name, install_dir)
             VALUES (?1, ?2, 'Test Game', 'D:\\Library\\Test Game')",
            params![scan_id, library_id],
        )
        .expect("seed game");
        conn.last_insert_rowid()
    }

    /// A game with exactly one flagged file, so `flush_batch` actually has a
    /// `FindingRow` in hand to (not) publish - a `PreparedGame` with empty
    /// `findings` would make the "findings must not be published" assertion
    /// trivially true no matter what `flush_batch` does with the ordering.
    fn prepared_game_with_one_finding(game_id: i64) -> PreparedGame {
        PreparedGame {
            game_id,
            name: "Test Game".to_string(),
            app_id: None,
            install_dir: PathBuf::from("D:\\Library\\Test Game"),
            entries: vec![FileEntry::logical_only("loc.pak", 1024, None)],
            findings: vec![PreparedFinding {
                entry_index: 0,
                rel_path: "loc.pak".to_string(),
                size: 1024,
                size_on_disk: 1024,
                source: FindingSource::Loc(gametrimmer_core::langdetect::LangKind::Text),
                rule_id: "test rule".to_string(),
                confidence: 90,
                provenance: RuleProvenance::Builtin,
                lang_tag: Some("de".to_string()),
                group_dir: None,
                // A capture failure, not a real snapshot - cheapest path
                // through `persist_prepared_game`'s safety branch, and this
                // test has no opinion on `deletion_block_reason`.
                safety: Err("no snapshot needed for this test".to_string()),
            }],
            kept: 0,
            anti_cheat_protected: false,
            imported_rules_refused: false,
        }
    }

    /// GT-109 item 4: `flush_batch`'s doc comment promises "UI rows are
    /// published only after the commit succeeds" - but nothing exercised the
    /// failure half of that sentence. A `commit_hook` that always asks SQLite
    /// to roll back stands in for a real commit failure (disk full, a locked
    /// WAL file, ...) without needing to manufacture one.
    #[test]
    fn a_failed_commit_never_publishes_that_batchs_findings() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let scan_id = db::begin_scan(&conn, "complete").expect("begin scan");
        let game_id = seed_game(&conn, scan_id);
        conn.commit_hook(Some(|| true))
            .expect("register a commit hook that forces a rollback");

        let mut batch = vec![prepared_game_with_one_finding(game_id)];
        let mut findings: Vec<FindingRow> = Vec::new();

        let result = flush_batch(&mut conn, &mut batch, &mut findings, scan_id);

        assert!(
            result.is_err(),
            "a commit forced into rollback must surface as an error, not succeed silently"
        );
        assert!(
            findings.is_empty(),
            "findings must never be published for a batch whose commit never actually happened"
        );
        assert_eq!(
            batch.len(),
            1,
            "the batch is only cleared after a successful commit (same line as the findings \
             append below it) - a failed commit must leave it as-is rather than silently \
             discarding the unwritten game"
        );
    }
}
