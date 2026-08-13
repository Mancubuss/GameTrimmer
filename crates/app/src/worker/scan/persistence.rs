//! Atomic scan-generation persistence and the single bounded writer.

use super::*;

/// The single database writer: receives every game's scan outcome and
/// persists it, batching `WRITE_BATCH_SIZE` games per transaction to keep
/// the number of commits (and WAL syncs) low regardless of how many files a
/// game has. Sends one `Progress` message per finished game, in whatever
/// order results arrive (scanning is parallel, so this is no longer
/// necessarily the games' discovery order).
pub(super) fn run_writer(
    conn: &mut Connection,
    result_rx: Receiver<GameOutcome>,
    notifier: &Notifier,
    total: usize,
    completed: &std::sync::atomic::AtomicUsize,
    cancel: &AtomicBool,
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
                    flush_batch(conn, &mut batch, &mut findings)?;
                }
            }
            GameOutcome::Failed {
                name,
                install_dir,
                error,
            } => {
                let message = if error.to_string() == "cancelled" {
                    "cancelled".to_string()
                } else {
                    format!(
                        "failed to scan \"{name}\" ({}): {error}",
                        install_dir.display()
                    )
                };
                crate::logger::log(&message);
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

    flush_batch(conn, &mut batch, &mut findings)?;
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
) -> CoreResult<()> {
    if batch.is_empty() {
        return Ok(());
    }

    let db_tx = conn.transaction()?;
    let mut pending_rows = Vec::new();
    for prepared in batch.iter() {
        let mut rows = persist_prepared_game(&db_tx, prepared).map_err(|err| {
            CoreError::Other(format!(
                "failed to write \"{}\" to the database: {err}",
                prepared.name
            ))
        })?;
        pending_rows.append(&mut rows);
    }
    db_tx.commit()?;
    findings.append(&mut pending_rows);
    batch.clear();
    Ok(())
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
) -> CoreResult<Vec<(i64, String, PathBuf)>> {
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
                games.push((
                    tx.last_insert_rowid(),
                    game.name.clone(),
                    game.install_dir.clone(),
                ));
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
            crate::logger::log(&format!(
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
pub(super) fn persist_prepared_game(
    conn: &Connection,
    prepared: &PreparedGame,
) -> CoreResult<Vec<FindingRow>> {
    // `findings.file_id` has no `ON DELETE CASCADE`, and `store_files_no_tx`
    // is about to delete this game's old `files` rows - drop their findings
    // first, while the old ids are still known.
    conn.execute(
        "DELETE FROM findings WHERE file_id IN (SELECT id FROM files WHERE game_id = ?1)",
        params![prepared.game_id],
    )?;

    // The stats come back from the insert loop that was going to run
    // anyway, so keeping them is free - they were simply dropped before.
    // Per-game file and byte counts are the natural unit for explaining a
    // slow scan ("this one game holds 400 000 files"), which the totals
    // alone never showed.
    let stats = store_files_no_tx(conn, prepared.game_id, &prepared.entries)?;
    conn.execute(
        "UPDATE games SET files = ?2, bytes = ?3, bytes_on_disk = ?4 WHERE id = ?1",
        params![
            prepared.game_id,
            stats.files as i64,
            stats.bytes as i64,
            stats.bytes_on_disk as i64
        ],
    )?;
    conn.execute(
        "UPDATE files SET scan_id = (SELECT scan_id FROM games WHERE id = ?1)
         WHERE game_id = ?1",
        [prepared.game_id],
    )?;

    let file_ids: HashMap<String, i64> = {
        let mut stmt = conn.prepare("SELECT id, rel_path FROM files WHERE game_id = ?1")?;
        let rows = stmt.query_map(params![prepared.game_id], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(0)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let scan_id: i64 = conn.query_row(
        "SELECT scan_id FROM games WHERE id = ?1",
        [prepared.game_id],
        |row| row.get(0),
    )?;
    // One read serves two purposes: the path is this game's safety evidence,
    // and (vendor, path) together are the row's library attribution. Both are
    // read from `game_libraries` rather than from the in-memory
    // `DiscoveredLibrary`, so a later `worker::load` - which can only read this
    // table - reconstructs exactly the same attribution.
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
    let evidence_block_reason = match evidence_status.as_deref() {
        None => Some("missing library discovery evidence".to_string()),
        Some("degraded") => Some("library discovery was degraded".to_string()),
        Some(_) => None,
    };

    let mut rows = Vec::with_capacity(prepared.findings.len());
    let mut insert_finding = conn.prepare_cached(
        "INSERT INTO findings
         (file_id, category, rule_id, confidence, lang_tag, group_dir, provenance) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    let mut insert_safety = conn.prepare_cached(
        "INSERT OR REPLACE INTO file_safety
         (file_id, scan_id, evidence_library_path, trusted_root, rel_path, root_identity,
          target_identity, target_kind, tree_fingerprint, block_reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;

    // Counted rather than logged per finding: one game can drop thousands
    // at once, and "12 404 findings had no file row" is the same
    // information as twelve thousand identical lines. One diagnostic row
    // per game follows the loop.
    let mut findings_without_file = 0usize;

    for finding in &prepared.findings {
        let Some(&file_id) = file_ids.get(&finding.rel_path) else {
            findings_without_file += 1;
            continue;
        };

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
        ])?;

        // Captured on the scan pool (see `PreparedFinding::safety`); the
        // writer only records the outcome.
        let deletion_block_reason = match &finding.safety {
            Ok(snapshot) => {
                insert_safety.execute(params![
                    file_id,
                    scan_id,
                    &evidence_library_path,
                    snapshot.trusted_root.to_string_lossy(),
                    snapshot.rel_path.to_string_lossy(),
                    snapshot.root_identity.encode(),
                    snapshot.target_identity.encode(),
                    snapshot.target_identity.kind.as_str(),
                    &snapshot.tree_fingerprint,
                    None::<String>,
                ])?;
                evidence_block_reason.clone()
            }
            Err(reason) => {
                let reason = reason.clone();
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
                Some(reason)
            }
        };

        rows.push(FindingRow {
            file_id,
            game_id: prepared.game_id,
            game_name: prepared.name.clone(),
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
        });
    }

    // The drop above is the exact shape of "findings vanish between runs",
    // and it used to leave nothing at all - no log line, no counter, no
    // row. Recorded, not propagated: a finding whose file row is missing is
    // a reason to explain the gap later, never a reason to fail the scan
    // that produced the rest of them.
    if findings_without_file > 0 {
        let message = format!(
            "{findings_without_file} of {} findings had no matching files row and were dropped",
            prepared.findings.len()
        );
        if let Err(err) = db::record_scan_diagnostic(
            conn,
            scan_id,
            "scan",
            "finding-without-file",
            Some(&prepared.install_dir),
            &message,
        ) {
            crate::logger::log(&format!(
                "Failed to record dropped findings for \"{}\": {err}",
                prepared.name
            ));
        }
    }

    Ok(rows)
}
