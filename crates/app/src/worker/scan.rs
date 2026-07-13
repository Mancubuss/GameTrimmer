//! The "Сканувати бібліотеки" job: discover Steam libraries, persist them,
//! then scan and classify every game's files. Runs entirely on a
//! background thread; the only database connection used here is opened
//! and dropped within this thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use gametrimmer_core::db;
use gametrimmer_core::error::{CoreError, Result as CoreResult};
use gametrimmer_core::langdetect::{LangDetector, LangFinding};
use gametrimmer_core::providers::{self, DiscoveredLibrary};
use gametrimmer_core::rules::{Finding, RuleEngine};
use gametrimmer_core::scanner::{scan_dir, store_files_no_tx, FileEntry};
use rusqlite::{params, Connection};

use crate::model::{category_key, DisplayCategory, FindingRow};

use super::{manual, resolve_rules_path, WorkerMsg};

/// Worker threads used for scanning+classifying games in parallel. Chosen
/// deliberately smaller than "one thread per game" and not tied to CPU count:
/// this workload is IO-bound (`scan_dir` walks disk), so a handful more
/// threads than cores keeps multiple directory-read/stat queues busy without
/// the diminishing returns (and cache thrashing) of one thread per game.
/// See `crates/core/examples/scan_bench.rs` for the measurements behind this
/// choice.
const SCAN_THREADS: usize = 6;

/// How many games' `files`/`findings` writes share one database transaction.
/// Bigger batches commit less often (fewer WAL syncs), but hold a batch's
/// results in memory and delay their visibility to the UI's "Done" message
/// by up to one batch - 16-32 is the sweet spot named in the benchmark.
const WRITE_BATCH_SIZE: usize = 24;

/// Spawns the scan job on a new thread. `cancel` is polled between games so
/// the user can abort a long scan early.
pub fn spawn_scan(
    db_path: PathBuf,
    cancel: Arc<AtomicBool>,
    tx: Sender<WorkerMsg>,
) -> JoinHandle<()> {
    std::thread::spawn(move || run_scan(&db_path, &cancel, &tx))
}

fn run_scan(db_path: &Path, cancel: &AtomicBool, tx: &Sender<WorkerMsg>) {
    let Some(rules_path) = resolve_rules_path() else {
        send_error(
            tx,
            "Не знайдено rules.json (ні поруч з програмою, ні в корені репозиторію).".to_string(),
        );
        return;
    };

    let engine = match RuleEngine::load(&rules_path) {
        Ok(engine) => engine,
        Err(err) => {
            send_error(tx, format!("Помилка завантаження rules.json: {err}"));
            return;
        }
    };

    // Keep-list is Ukrainian + English for now; a configurable keep-list is
    // a future phase (see docs/04_implementation_plan.md).
    let lang_detector = LangDetector::new();

    let mut conn = match db::open(db_path) {
        Ok(conn) => conn,
        Err(err) => {
            send_error(tx, format!("Помилка відкриття бази даних: {err}"));
            return;
        }
    };

    // Every vendor provider is tried; one provider failing (registry key
    // missing, launcher config unreadable, ...) must not abort the whole
    // scan - it is reported as a warning and the rest still run.
    let mut libraries: Vec<DiscoveredLibrary> = Vec::new();
    for provider in providers::all() {
        match provider.discover() {
            Ok(mut discovered) => libraries.append(&mut discovered),
            Err(err) => send_warning(tx, format!("Провайдер \"{}\": {err}", provider.name())),
        }
    }

    match manual::discover_manual_libraries(&conn) {
        Ok((manual_libraries, manual_warnings)) => {
            for warning in manual_warnings {
                send_warning(tx, warning);
            }
            libraries.extend(manual_libraries);
        }
        Err(err) => {
            send_error(tx, format!("Помилка читання ручних бібліотек: {err}"));
            return;
        }
    }

    if libraries.is_empty() {
        send_error(tx, "Бібліотек не знайдено.".to_string());
        return;
    }

    let games = match persist_libraries(&mut conn, &libraries) {
        Ok(games) => games,
        Err(err) => {
            send_error(tx, format!("Помилка запису бібліотек у базу даних: {err}"));
            return;
        }
    };

    let _ = tx.send(WorkerMsg::LibrariesFound {
        libraries: libraries.len(),
        games: games.len(),
    });

    let total = games.len();

    // Scanning+classification (IO and CPU work, no DB) happens in parallel
    // across `SCAN_THREADS` worker threads; only the DB writes are
    // serialized, on a single writer thread that drains `result_rx` as
    // results arrive. This way scanning game N+1 never waits on game N's
    // write, and the write side never has more than one connection open.
    // See `crates/core/examples/scan_bench.rs` for the measurements that
    // motivated this over the previous fully-sequential loop.
    let (result_tx, result_rx) = std::sync::mpsc::channel::<GameOutcome>();

    let write_outcome = std::thread::scope(|scope| {
        let writer = scope.spawn(|| run_writer(&mut conn, result_rx, tx, total, cancel));

        dispatch_scans(&games, &engine, &lang_detector, cancel, &result_tx);
        // Dropping the last sender lets the writer's `for outcome in rx`
        // loop end once every dispatched scan has reported in.
        drop(result_tx);

        writer.join()
    });

    let findings = match write_outcome {
        Ok(findings) => findings,
        Err(_) => {
            send_error(
                tx,
                "Потік запису результатів сканування завершився аварійно.".to_string(),
            );
            return;
        }
    };

    if cancel.load(Ordering::Relaxed) {
        let _ = tx.send(WorkerMsg::Cancelled);
        return;
    }

    let _ = tx.send(WorkerMsg::Done { findings });
}

/// One game's outcome after scanning+classifying it, sent from a scan worker
/// thread to the single DB-writer thread. Carries no open DB state - only
/// data - so it can freely cross threads.
enum GameOutcome {
    Scanned(PreparedGame),
    Failed {
        name: String,
        install_dir: PathBuf,
        error: CoreError,
    },
}

/// Dispatches every game's scan+classify onto a bounded thread pool. Each
/// task gets its own cloned `Sender` (cloned here, on the single dispatching
/// thread, before the task is spawned) - `mpsc::Sender` is `Send` but not
/// `Sync`, so a clone-per-task is required rather than sharing one `&Sender`
/// across the pool's worker threads.
///
/// `cancel` is polled once per game, right before that game's work starts:
/// once set, games not yet started are reported as cancelled immediately
/// instead of being scanned, while games already running on a worker thread
/// (up to `SCAN_THREADS` of them) still finish normally.
fn dispatch_scans(
    games: &[(i64, String, PathBuf)],
    engine: &RuleEngine,
    lang_detector: &LangDetector,
    cancel: &AtomicBool,
    result_tx: &Sender<GameOutcome>,
) {
    let run_one = |game_id: i64, name: &str, install_dir: &Path, result_tx: Sender<GameOutcome>| {
        if cancel.load(Ordering::Relaxed) {
            let _ = result_tx.send(GameOutcome::Failed {
                name: name.to_string(),
                install_dir: install_dir.to_path_buf(),
                error: CoreError::Other("cancelled".to_string()),
            });
            return;
        }

        let outcome = match scan_and_prepare_game(engine, lang_detector, game_id, name, install_dir)
        {
            Ok(prepared) => GameOutcome::Scanned(prepared),
            Err(error) => GameOutcome::Failed {
                name: name.to_string(),
                install_dir: install_dir.to_path_buf(),
                error,
            },
        };
        let _ = result_tx.send(outcome);
    };

    match rayon::ThreadPoolBuilder::new()
        .num_threads(SCAN_THREADS.max(1))
        .build()
    {
        Ok(pool) => pool.scope(|scope| {
            for (game_id, name, install_dir) in games {
                let result_tx = result_tx.clone();
                scope.spawn(move |_| run_one(*game_id, name, install_dir, result_tx));
            }
        }),
        // A pool failing to build (extremely unlikely) must not lose the
        // scan entirely - fall back to running everything on this thread.
        Err(_) => {
            for (game_id, name, install_dir) in games {
                run_one(*game_id, name, install_dir, result_tx.clone());
            }
        }
    }
}

/// The single database writer: receives every game's scan outcome and
/// persists it, batching `WRITE_BATCH_SIZE` games per transaction to keep
/// the number of commits (and WAL syncs) low regardless of how many files a
/// game has. Sends one `Progress` message per finished game, in whatever
/// order results arrive (scanning is parallel, so this is no longer
/// necessarily the games' discovery order).
fn run_writer(
    conn: &mut Connection,
    result_rx: Receiver<GameOutcome>,
    tx: &Sender<WorkerMsg>,
    total: usize,
    cancel: &AtomicBool,
) -> Vec<FindingRow> {
    let mut findings = Vec::new();
    let mut batch: Vec<PreparedGame> = Vec::with_capacity(WRITE_BATCH_SIZE);

    for (index, outcome) in result_rx.iter().enumerate() {
        let completed = index + 1;
        match outcome {
            GameOutcome::Scanned(prepared) => {
                let _ = tx.send(WorkerMsg::Progress {
                    current: completed,
                    total,
                    game_name: prepared.name.clone(),
                });
                batch.push(prepared);
                if batch.len() >= WRITE_BATCH_SIZE {
                    flush_batch(conn, &mut batch, &mut findings);
                }
            }
            GameOutcome::Failed {
                name,
                install_dir,
                error,
            } => {
                // A single game failing to scan (permissions, moved folder,
                // cancellation, ...) must not abort the whole run.
                eprintln!(
                    "Помилка сканування \"{name}\" ({}): {error}",
                    install_dir.display()
                );
                let _ = tx.send(WorkerMsg::Progress {
                    current: completed,
                    total,
                    game_name: name,
                });
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

    flush_batch(conn, &mut batch, &mut findings);
    findings
}

/// Commits one batch of games' `files`/`findings` writes in a single
/// transaction. A batch failing to open or commit its transaction (disk
/// full, ...) drops that batch's writes but must not crash the writer
/// thread - remaining batches still get a chance to persist.
fn flush_batch(
    conn: &mut Connection,
    batch: &mut Vec<PreparedGame>,
    findings: &mut Vec<FindingRow>,
) {
    if batch.is_empty() {
        return;
    }

    match conn.transaction() {
        Ok(db_tx) => {
            for prepared in batch.drain(..) {
                match persist_prepared_game(&db_tx, &prepared) {
                    Ok(mut rows) => findings.append(&mut rows),
                    Err(err) => {
                        eprintln!("Помилка запису \"{}\" у базу даних: {err}", prepared.name)
                    }
                }
            }
            if let Err(err) = db_tx.commit() {
                eprintln!("Помилка збереження пакету ігор у базу даних: {err}");
            }
        }
        Err(err) => {
            eprintln!("Помилка початку транзакції запису: {err}");
            batch.clear();
        }
    }
}

fn send_error(tx: &Sender<WorkerMsg>, msg: String) {
    let _ = tx.send(WorkerMsg::Error { msg });
}

fn send_warning(tx: &Sender<WorkerMsg>, msg: String) {
    let _ = tx.send(WorkerMsg::Warning { msg });
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
fn persist_libraries(
    conn: &mut Connection,
    libraries: &[DiscoveredLibrary],
) -> CoreResult<Vec<(i64, String, PathBuf)>> {
    let tx = conn.transaction()?;
    let mut games = Vec::new();

    for library in libraries {
        let path_str = library.path.to_string_lossy().to_string();

        tx.execute(
            "INSERT OR IGNORE INTO game_libraries (vendor, path) VALUES (?1, ?2)",
            params![library.vendor, path_str],
        )?;
        let library_id: i64 = tx.query_row(
            "SELECT id FROM game_libraries WHERE path = ?1",
            params![path_str],
            |row| row.get(0),
        )?;

        tx.execute(
            "DELETE FROM findings WHERE file_id IN (
                SELECT id FROM files WHERE game_id IN (
                    SELECT id FROM games WHERE library_id = ?1
                )
            )",
            params![library_id],
        )?;
        tx.execute(
            "DELETE FROM files WHERE game_id IN (SELECT id FROM games WHERE library_id = ?1)",
            params![library_id],
        )?;
        tx.execute(
            "DELETE FROM games WHERE library_id = ?1",
            params![library_id],
        )?;

        for game in &library.games {
            tx.execute(
                "INSERT INTO games (library_id, name, install_dir, app_id) VALUES (?1, ?2, ?3, ?4)",
                params![
                    library_id,
                    game.name,
                    game.install_dir.to_string_lossy(),
                    game.app_id
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
}

/// One file's finding, already resolved (rule engine vs. localization
/// detector) but not yet persisted - `rel_path` is looked back up against
/// `files.id` once the file rows exist, since that id doesn't exist until
/// `store_files_no_tx` has run. Carrying `size` here (rather than
/// re-deriving it from `entries` at persist time by `rel_path`) avoids an
/// O(files x findings) rescan per game.
struct PreparedFinding {
    rel_path: String,
    size: u64,
    category: DisplayCategory,
    rule_id: String,
    confidence: u8,
    lang_tag: Option<String>,
}

/// The result of scanning and classifying one game: no DB state, so it can
/// be produced on any thread and handed off to the single writer thread.
struct PreparedGame {
    game_id: i64,
    name: String,
    install_dir: PathBuf,
    entries: Vec<FileEntry>,
    findings: Vec<PreparedFinding>,
}

/// Scans one game's install directory and classifies each file through both
/// the rule engine and the localization detector. Pure CPU+IO - touches no
/// database, so this is what runs in parallel across scan worker threads;
/// only [`persist_prepared_game`] needs a `Connection`.
fn scan_and_prepare_game(
    engine: &RuleEngine,
    lang_detector: &LangDetector,
    game_id: i64,
    name: &str,
    install_dir: &Path,
) -> CoreResult<PreparedGame> {
    let entries = scan_dir(install_dir)?;

    // `analyze_game` needs sibling context (the language-family heuristic),
    // so it runs once over all of this game's files rather than per-file.
    let lang_findings: HashMap<usize, LangFinding> =
        lang_detector.analyze_game(&entries).into_iter().collect();

    let mut findings = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let rule_finding = engine.classify(&entry.rel_path);
        let lang_finding = lang_findings.get(&index);

        let Some(combined) = combine_finding(rule_finding, lang_finding) else {
            continue;
        };

        findings.push(PreparedFinding {
            rel_path: entry.rel_path.clone(),
            size: entry.size,
            category: combined.category,
            rule_id: combined.rule_id,
            confidence: combined.confidence,
            lang_tag: combined.lang_tag,
        });
    }

    Ok(PreparedGame {
        game_id,
        name: name.to_string(),
        install_dir: install_dir.to_path_buf(),
        entries,
        findings,
    })
}

/// Persists one already-scanned-and-classified game: replaces its indexed
/// files and inserts its findings, returning them for the UI. Uses whatever
/// transaction (if any) is already open on `conn` - callers that want a
/// single game per commit pass a fresh `Transaction`; the scan pipeline's
/// writer thread instead shares one transaction across a batch of games
/// (see `WRITE_BATCH_SIZE`).
fn persist_prepared_game(
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

    store_files_no_tx(conn, prepared.game_id, &prepared.entries)?;

    let file_ids: HashMap<String, i64> = {
        let mut stmt = conn.prepare("SELECT id, rel_path FROM files WHERE game_id = ?1")?;
        let rows = stmt.query_map(params![prepared.game_id], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(0)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    let mut rows = Vec::with_capacity(prepared.findings.len());
    let mut insert_finding = conn.prepare_cached(
        "INSERT INTO findings (file_id, category, rule_id, confidence, lang_tag) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;

    for finding in &prepared.findings {
        let Some(&file_id) = file_ids.get(&finding.rel_path) else {
            continue;
        };

        insert_finding.execute(params![
            file_id,
            category_key(finding.category),
            finding.rule_id,
            finding.confidence,
            finding.lang_tag.as_deref(),
        ])?;

        rows.push(FindingRow {
            file_id,
            game_id: prepared.game_id,
            game_name: prepared.name.clone(),
            install_dir: prepared.install_dir.clone(),
            rel_path: finding.rel_path.clone(),
            size: finding.size,
            category: finding.category,
            rule_desc: finding.rule_id.clone(),
            confidence: finding.confidence,
            lang_tag: finding.lang_tag.clone(),
        });
    }

    Ok(rows)
}

/// Scans, classifies, and persists one game in its own single-game
/// transaction. A thin composition of [`scan_and_prepare_game`] and
/// [`persist_prepared_game`], kept as the entry point tests exercise
/// directly - `run_scan`'s real pipeline instead scans games in parallel and
/// batches several games per commit (see [`dispatch_scans`], [`run_writer`]).
#[cfg(test)]
fn scan_and_classify_game(
    conn: &mut Connection,
    engine: &RuleEngine,
    lang_detector: &LangDetector,
    game_id: i64,
    name: &str,
    install_dir: &Path,
) -> CoreResult<Vec<FindingRow>> {
    let prepared = scan_and_prepare_game(engine, lang_detector, game_id, name, install_dir)?;
    let db_tx = conn.transaction()?;
    let findings = persist_prepared_game(&db_tx, &prepared)?;
    db_tx.commit()?;
    Ok(findings)
}

/// One file's finding after reconciling the rule engine and the localization
/// detector, ready to persist and display.
struct CombinedFinding {
    category: DisplayCategory,
    rule_id: String,
    confidence: u8,
    lang_tag: Option<String>,
}

/// Merges a rules-engine finding with a localization finding for the same
/// file. A file can match both (e.g. a "bonus" folder that also contains
/// Spanish voice-over); only the higher-confidence finding is kept. Ties are
/// resolved in favor of the rules engine, since a specific category match is
/// more informative than a bare language cue at equal confidence.
fn combine_finding(rule: Option<Finding>, lang: Option<&LangFinding>) -> Option<CombinedFinding> {
    match (rule, lang) {
        (Some(r), Some(l)) if l.confidence > r.confidence => Some(CombinedFinding {
            category: DisplayCategory::Loc(l.kind),
            rule_id: l.reason.clone(),
            confidence: l.confidence,
            lang_tag: Some(l.lang_tag.clone()),
        }),
        (Some(r), _) => Some(CombinedFinding {
            category: DisplayCategory::Rule(r.category),
            rule_id: r.rule_desc,
            confidence: r.confidence,
            lang_tag: None,
        }),
        (None, Some(l)) => Some(CombinedFinding {
            category: DisplayCategory::Loc(l.kind),
            rule_id: l.reason.clone(),
            confidence: l.confidence,
            lang_tag: Some(l.lang_tag.clone()),
        }),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gametrimmer_core::db;
    use gametrimmer_core::langdetect::LangDetector;
    use gametrimmer_core::providers::GameInstall;
    use gametrimmer_core::rules::RuleEngine;
    use std::fs;

    /// A rule that matches every file name, so `scan_and_classify_game`
    /// always inserts a `findings` row per file - needed to exercise the
    /// `findings` -> `files` -> `games` cleanup chain.
    fn match_all_engine() -> RuleEngine {
        RuleEngine::from_json(
            r#"[{"category":"docs_file","pattern":".","desc":"test rule","confidence":50}]"#,
        )
        .expect("valid test rules.json")
    }

    fn write_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write file");
    }

    fn library_id_for(conn: &Connection, path: &str) -> i64 {
        conn.query_row(
            "SELECT id FROM game_libraries WHERE path = ?1",
            params![path],
            |row| row.get(0),
        )
        .expect("library row should exist")
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |row| row.get(0))
            .expect("count query should succeed")
    }

    /// Runs one full scan cycle (persist library + game, then scan its
    /// install dir and persist findings) exactly as `run_scan` does.
    fn run_one_cycle(
        conn: &mut Connection,
        engine: &RuleEngine,
        lang_detector: &LangDetector,
        library: &DiscoveredLibrary,
    ) -> CoreResult<Vec<(i64, String, PathBuf)>> {
        let games = persist_libraries(conn, std::slice::from_ref(library))?;
        for (game_id, name, install_dir) in &games {
            scan_and_classify_game(conn, engine, lang_detector, *game_id, name, install_dir)?;
        }
        Ok(games)
    }

    /// Reproduces the reported bug: scanning a library a second time (data
    /// from the first scan already in the DB) used to fail with
    /// `FOREIGN KEY constraint failed`, because `persist_libraries` deleted
    /// `games` rows without first deleting the `files`/`findings` rows that
    /// referenced them (hypothesis 1).
    #[test]
    fn rescanning_same_library_does_not_violate_foreign_keys() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();

        let install_dir = tempfile::tempdir().expect("create temp install dir");
        write_file(&install_dir.path().join("readme.txt"), b"hello");
        write_file(&install_dir.path().join("bin").join("game.exe"), b"exe");

        let library = DiscoveredLibrary {
            vendor: "steam",
            path: PathBuf::from("C:/Games"),
            games: vec![GameInstall {
                name: "Test Game".to_string(),
                install_dir: install_dir.path().to_path_buf(),
                app_id: Some("123".to_string()),
            }],
        };

        run_one_cycle(&mut conn, &engine, &lang_detector, &library)
            .expect("first scan of a fresh database should succeed");

        assert!(
            count(&conn, "SELECT COUNT(*) FROM findings") > 0,
            "first scan should have produced findings to clean up on rescan"
        );

        // This is the exact failure reported by the user: rescanning a
        // library that already has data must not error out.
        let second = run_one_cycle(&mut conn, &engine, &lang_detector, &library);
        assert!(
            second.is_ok(),
            "rescanning an already-populated library must not fail: {:?}",
            second.err()
        );

        // No orphaned rows should remain from the first scan's games/files/findings.
        let games = second.unwrap();
        assert_eq!(games.len(), 1, "the library still has exactly one game");
        let (game_id, _, _) = games[0];

        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM games"),
            1,
            "old game row must have been replaced, not duplicated"
        );
        assert_eq!(
            count(
                &conn,
                &format!("SELECT COUNT(*) FROM files WHERE game_id != {game_id}")
            ),
            0,
            "no file rows should reference a stale game id"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM findings WHERE file_id NOT IN (SELECT id FROM files)"
            ),
            0,
            "no finding should reference a deleted file id"
        );
    }

    /// Hypothesis 2: after `INSERT OR IGNORE` on an already-existing library
    /// row, the library id must come from a `SELECT ... WHERE path = ?`, not
    /// from `last_insert_rowid()` (which reflects whatever row - possibly in
    /// a different table - was last inserted on this connection, i.e.
    /// garbage). Simulate the drift opportunity by inserting a `games` row
    /// (bumping the connection's last-insert-rowid) between two
    /// `persist_libraries` calls for the very same library.
    #[test]
    fn library_id_is_looked_up_by_path_not_taken_from_stale_last_insert_rowid() {
        let mut conn = db::open_in_memory().expect("open in-memory db");

        let library = DiscoveredLibrary {
            vendor: "steam",
            path: PathBuf::from("D:/SteamLibrary"),
            games: vec![],
        };

        persist_libraries(&mut conn, std::slice::from_ref(&library))
            .expect("first persist should succeed");
        let original_library_id = library_id_for(&conn, "D:/SteamLibrary");

        // Bump last_insert_rowid() on this connection with an unrelated
        // insert into a *different* table, so a buggy
        // `last_insert_rowid()`-based lookup of the (already-existing)
        // library id would pick up this row's id instead.
        conn.execute(
            "INSERT INTO operations (ts, action, src_path, status) VALUES (0, 'noop', 'x', 'ok')",
            [],
        )
        .expect("insert unrelated operations row");

        let games = persist_libraries(&mut conn, std::slice::from_ref(&library))
            .expect("re-persisting the same library must not fail");
        assert!(games.is_empty(), "library has no games in this test");

        let library_id_after = library_id_for(&conn, "D:/SteamLibrary");
        assert_eq!(
            library_id_after, original_library_id,
            "library id must stay stable across rescans, not drift to a stale rowid"
        );
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM game_libraries"),
            1,
            "the library must not be duplicated"
        );
    }

    /// Sibling case: a game that disappears from a library between two
    /// scans (uninstalled, folder moved away, ...) must have its old
    /// `files`/`findings` rows cleaned up too - no FK failure, no orphans.
    #[test]
    fn game_removed_from_library_leaves_no_orphaned_rows() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();

        let dir_a = tempfile::tempdir().expect("create temp dir a");
        let dir_b = tempfile::tempdir().expect("create temp dir b");
        write_file(&dir_a.path().join("a.txt"), b"aaaa");
        write_file(&dir_b.path().join("b.txt"), b"bbbb");

        let library_first = DiscoveredLibrary {
            vendor: "steam",
            path: PathBuf::from("E:/Games"),
            games: vec![
                GameInstall {
                    name: "Game A".to_string(),
                    install_dir: dir_a.path().to_path_buf(),
                    app_id: Some("1".to_string()),
                },
                GameInstall {
                    name: "Game B".to_string(),
                    install_dir: dir_b.path().to_path_buf(),
                    app_id: Some("2".to_string()),
                },
            ],
        };
        run_one_cycle(&mut conn, &engine, &lang_detector, &library_first)
            .expect("first scan with two games should succeed");

        // Game B is gone on the second scan (e.g. uninstalled).
        let library_second = DiscoveredLibrary {
            vendor: "steam",
            path: PathBuf::from("E:/Games"),
            games: vec![GameInstall {
                name: "Game A".to_string(),
                install_dir: dir_a.path().to_path_buf(),
                app_id: Some("1".to_string()),
            }],
        };
        let games = run_one_cycle(&mut conn, &engine, &lang_detector, &library_second)
            .expect("rescanning with a game removed must not fail");

        assert_eq!(games.len(), 1, "only Game A should remain");
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM games"),
            1,
            "Game B's row must have been removed, not left orphaned"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM files WHERE game_id NOT IN (SELECT id FROM games)"
            ),
            0,
            "no file may reference a game that no longer exists"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM findings WHERE file_id NOT IN (SELECT id FROM files)"
            ),
            0,
            "no finding may reference a file that no longer exists"
        );
    }

    /// Sibling case: a library that stops being discovered (drive
    /// unplugged, launcher uninstalled, ...) is simply absent from
    /// `libraries` on the next scan. `persist_libraries` only touches
    /// libraries it is given, so the old library/games/files/findings rows
    /// must survive untouched, without any FK error.
    #[test]
    fn library_no_longer_discovered_is_left_untouched() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();

        let install_dir = tempfile::tempdir().expect("create temp install dir");
        write_file(&install_dir.path().join("save.dat"), b"save");

        let vanished_library = DiscoveredLibrary {
            vendor: "steam",
            path: PathBuf::from("F:/OldDrive"),
            games: vec![GameInstall {
                name: "Old Game".to_string(),
                install_dir: install_dir.path().to_path_buf(),
                app_id: None,
            }],
        };
        run_one_cycle(&mut conn, &engine, &lang_detector, &vanished_library)
            .expect("initial scan should succeed");

        // Next scan only discovers a different library; the old one is gone
        // from the discovery results (but still exists in the DB).
        let other_dir = tempfile::tempdir().expect("create temp dir");
        write_file(&other_dir.path().join("x.txt"), b"x");
        let current_library = DiscoveredLibrary {
            vendor: "steam",
            path: PathBuf::from("G:/NewDrive"),
            games: vec![GameInstall {
                name: "New Game".to_string(),
                install_dir: other_dir.path().to_path_buf(),
                app_id: None,
            }],
        };
        let result = run_one_cycle(&mut conn, &engine, &lang_detector, &current_library);
        assert!(
            result.is_ok(),
            "persisting a newly discovered library must not fail: {:?}",
            result.err()
        );

        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM game_libraries"),
            2,
            "the vanished library's row must still be present, untouched"
        );
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM games WHERE name = 'Old Game'"),
            1,
            "the vanished library's game must still be present, untouched"
        );
    }
}
