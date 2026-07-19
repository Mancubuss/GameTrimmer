//! The "Сканувати бібліотеки" job: discover Steam libraries, persist them,
//! then scan and classify every game's files. Runs entirely on a
//! background thread; the only database connection used here is opened
//! and dropped within this thread.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use gametrimmer_core::db;
use gametrimmer_core::error::{CoreError, Result as CoreResult};
use gametrimmer_core::langdetect::{LangData, LangDetector, LangFinding};
use gametrimmer_core::mftscan;
use gametrimmer_core::providers::{self, DiscoveredLibrary};
use gametrimmer_core::rules::{Finding, RuleEngine};
use gametrimmer_core::scanner::{scan_dir, store_files_no_tx, FileEntry};
use gametrimmer_core::settings::ScanRouting;
use rusqlite::{params, Connection};

use crate::i18n::{self, Lang, Verb};
use crate::model::{source_key, FindingRow, FindingSource};

use super::scan_route::{self, ScanRoute};
use super::{manual, WorkerMsg};

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
/// the user can abort a long scan early. `elevated` reflects whether this
/// process holds Administrator rights right now (see `crate::elevation`) -
/// it gates the MFT index scan path, which needs raw volume read access.
/// `scan_routing` is the persisted `scan_routing` setting (see
/// `gametrimmer_core::settings::ScanRouting`) - it only ever narrows or
/// widens which routing outcomes are considered, never bypasses a
/// correctness gate (see `scan_route::initial_route`).
pub fn spawn_scan(
    db_path: PathBuf,
    cancel: Arc<AtomicBool>,
    tx: Sender<WorkerMsg>,
    elevated: bool,
    lang: Lang,
    keep_languages: Vec<String>,
    scan_routing: ScanRouting,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        run_scan(
            &db_path,
            &cancel,
            &tx,
            elevated,
            lang,
            &keep_languages,
            scan_routing,
        )
    })
}

fn run_scan(
    db_path: &Path,
    cancel: &AtomicBool,
    tx: &Sender<WorkerMsg>,
    elevated: bool,
    lang: Lang,
    keep_languages: &[String],
    scan_routing: ScanRouting,
) {
    let started_at = Instant::now();
    // Both rule files live next to the executable and are materialized from
    // the embedded defaults on first use (see `super::ensure_rules_path`) -
    // the scan reads them exclusively from disk, so what the user can audit
    // in those files is exactly what runs. A file that cannot be created or
    // parsed must not silently kill or degrade the scan: warn, fall back to
    // the built-ins, keep going.
    let engine = match super::ensure_rules_path()
        .map_err(CoreError::from)
        .and_then(|path| {
            RuleEngine::load(&path)
                .map_err(|err| CoreError::Other(format!("{}: {err}", path.display())))
        }) {
        Ok(engine) => engine,
        Err(err) => {
            send_warning(tx, i18n::rules_json_load_failed(lang, err));
            match RuleEngine::from_json(gametrimmer_core::rules::BUILTIN_RULES_JSON) {
                Ok(engine) => engine,
                Err(err) => {
                    // The embedded defaults are validated by core tests, so
                    // this is unreachable short of a broken build.
                    send_error(tx, i18n::builtin_rules_corrupted(lang, err));
                    return;
                }
            }
        }
    };

    // Keep-list comes from the persisted `keep_languages` setting (see
    // `gametrimmer_core::settings`), configurable in the settings dialog -
    // defaults to Ukrainian + English.
    let lang_data = match super::ensure_l10n_rules_path()
        .map_err(CoreError::from)
        .and_then(|path| {
            std::fs::read_to_string(&path)
                .map_err(CoreError::from)
                .and_then(|text| LangData::from_json(&text))
                .map_err(|err| CoreError::Other(format!("{}: {err}", path.display())))
        }) {
        Ok(data) => data,
        Err(err) => {
            send_warning(tx, i18n::l10n_rules_load_failed(lang, err));
            LangData::builtin()
        }
    };
    let lang_detector = LangDetector::with_data(lang_data, keep_languages);

    let mut conn = match db::open(db_path) {
        Ok(conn) => conn,
        Err(err) => {
            send_error(tx, i18n::db_open_error_long(lang, err));
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
            Err(err) => send_warning(tx, i18n::provider_failed(lang, provider.name(), err)),
        }
    }

    match manual::discover_manual_libraries(&conn, lang) {
        Ok((manual_libraries, manual_warnings)) => {
            for warning in manual_warnings {
                send_warning(tx, warning);
            }
            libraries.extend(manual_libraries);
        }
        Err(err) => {
            send_error(tx, i18n::manual_libraries_read_failed(lang, err));
            return;
        }
    }

    // Different providers (and the manual list) can discover the same root
    // folder - e.g. the Epic manifests and the vendor-folder scan both find
    // F:\Epic. Merge them so persist_libraries sees each library once.
    let libraries = providers::merge_libraries_by_path(libraries);

    if libraries.is_empty() {
        send_error(tx, i18n::no_libraries_found(lang));
        return;
    }

    let games = match persist_libraries(&mut conn, &libraries) {
        Ok(games) => games,
        Err(err) => {
            send_error(tx, i18n::libraries_write_failed(lang, err));
            return;
        }
    };

    let _ = tx.send(WorkerMsg::LibrariesFound {
        libraries: libraries.len(),
        games: games.len(),
    });

    if cancel.load(Ordering::Relaxed) {
        let _ = tx.send(WorkerMsg::Cancelled);
        return;
    }

    let total = games.len();

    // A single, non-cancellable pre-pass: for every game whose install root
    // is eligible (elevated, on an NTFS volume that opens, not behind a
    // junction/symlink/mount point/`subst`), try reading its files straight
    // out of that volume's Master File Table instead of walking its
    // directory tree. Ineligible roots, and roots the pass itself rejects,
    // simply have no entry in `mft_pass.entries` - `dispatch_scans` falls
    // back to a normal `scan_dir` walk for those, exactly as before this
    // path existed.
    let mft_pass = run_mft_pass(elevated, scan_routing, &games, cancel, tx, lang);

    if cancel.load(Ordering::Relaxed) {
        let _ = tx.send(WorkerMsg::Cancelled);
        return;
    }

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

        dispatch_scans(
            &games,
            &engine,
            &lang_detector,
            cancel,
            &result_tx,
            &mft_pass.entries,
        );
        // Dropping the last sender lets the writer's `for outcome in rx`
        // loop end once every dispatched scan has reported in.
        drop(result_tx);

        writer.join()
    });

    let findings = match write_outcome {
        Ok(findings) => findings,
        Err(_) => {
            send_error(tx, i18n::write_thread_crashed(lang));
            return;
        }
    };

    if cancel.load(Ordering::Relaxed) {
        let _ = tx.send(WorkerMsg::Cancelled);
        return;
    }

    let scan_summary = scan_route::format_scan_summary(
        lang,
        total,
        mft_pass.mft_count,
        mft_pass.walkdir_count,
        started_at.elapsed().as_secs_f64(),
    );

    let _ = tx.send(WorkerMsg::Done {
        findings,
        scan_summary,
    });
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
///
/// `mft_entries` holds the file lists the MFT pass already obtained for
/// some games (see `run_mft_pass`) - a game present here skips `scan_dir`
/// entirely and goes straight to classification; a game absent from it (the
/// common case when not elevated, or whenever the MFT pass rejected that
/// root) is scanned with `scan_dir` exactly as before this path existed.
fn dispatch_scans(
    games: &[(i64, String, PathBuf)],
    engine: &RuleEngine,
    lang_detector: &LangDetector,
    cancel: &AtomicBool,
    result_tx: &Sender<GameOutcome>,
    mft_entries: &HashMap<i64, Vec<FileEntry>>,
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

        let outcome = match mft_entries.get(&game_id) {
            Some(entries) => GameOutcome::Scanned(classify_game(
                engine,
                lang_detector,
                game_id,
                name,
                install_dir,
                entries.clone(),
            )),
            None => {
                match scan_and_prepare_game(engine, lang_detector, game_id, name, install_dir) {
                    Ok(prepared) => GameOutcome::Scanned(prepared),
                    Err(error) => GameOutcome::Failed {
                        name: name.to_string(),
                        install_dir: install_dir.to_path_buf(),
                        error,
                    },
                }
            }
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

/// Result of the MFT index pre-pass: file entries for every game that ended
/// up going through the MFT path, plus how many games went each way (for
/// the final status line - see `scan_route::format_scan_summary`).
struct MftPassOutcome {
    entries: HashMap<i64, Vec<FileEntry>>,
    mft_count: usize,
    walkdir_count: usize,
}

/// Runs the (single, non-cancellable) MFT index pass ahead of the per-game
/// scan dispatch: for every game whose install root is eligible (see
/// `scan_route::initial_route`), tries to read its files straight out of
/// the NTFS volume's Master File Table instead of walking its directory
/// tree. Ineligible roots, and roots the pass itself rejects (see
/// `scan_route::finalize_mft_result`), are simply left out of the returned
/// map - `dispatch_scans` transparently falls back to `scan_dir` (walkdir)
/// for any game id not present in it.
///
/// Every game ends up in exactly one of the two buckets that make up
/// `mft_count + walkdir_count`, by construction: `walkdir_count` is derived
/// as `total - mft_count` rather than tracked incrementally, so the two
/// numbers can never drift apart even if a routing edge case is missed.
///
/// `cancel` is threaded down into `mftscan::scan_roots` so a cancellation
/// during this (otherwise non-cancellable) pass stops promptly instead of
/// reading an entire large volume to completion first. `tx` receives one
/// `WorkerMsg::Progress` per chunk of `$MFT` records read on each volume,
/// so the UI shows something during what used to be a silent, seemingly
/// stuck phase - see `mftscan::MftProgress`.
fn run_mft_pass(
    elevated: bool,
    scan_routing: ScanRouting,
    games: &[(i64, String, PathBuf)],
    cancel: &AtomicBool,
    tx: &Sender<WorkerMsg>,
    lang: Lang,
) -> MftPassOutcome {
    let total = games.len();

    if !elevated {
        return MftPassOutcome {
            entries: HashMap::new(),
            mft_count: 0,
            walkdir_count: total,
        };
    }

    let checks: Vec<scan_route::RootCheck> = games
        .iter()
        .map(|(game_id, _, install_dir)| scan_route::RootCheck {
            game_id: *game_id,
            install_dir: install_dir.clone(),
            volume_letter: mftscan::volume_letter(install_dir),
            canonical_mismatch: canonical_mismatch(install_dir),
        })
        .collect();

    // Media-type routing: on a volume without a seek penalty (SSD/NVMe) a
    // directory walk of just the library subtrees beats reading the whole
    // volume's $MFT, so such volumes are routed to walkdir without even
    // probing raw-open availability. HDDs (and unknown media) stay on the
    // MFT path - that is where it wins by orders of magnitude on a cold
    // cache. See scan_route::mft_worthwhile / WalkdirReason::SsdVolume.
    // `ScanRouting::ForceMft` still needs `is_available` for every checked
    // volume regardless of media kind - it only bypasses the SSD speed
    // heuristic, never the volume-availability correctness gate - so the
    // probe runs unconditionally in that mode even for SSD/NVMe volumes.
    let mut volume_available: HashMap<char, bool> = HashMap::new();
    let mut volume_ssd: HashMap<char, bool> = HashMap::new();
    for letter in scan_route::volumes_to_check(elevated, scan_routing, &checks) {
        if scan_routing == ScanRouting::ForceMft
            || scan_route::mft_worthwhile(mftscan::media_kind(letter))
        {
            volume_ssd.insert(letter, false);
            volume_available.insert(letter, mftscan::is_available(letter));
        } else {
            volume_ssd.insert(letter, true);
        }
    }

    // Group MFT candidates by volume so that a panic or error while
    // scanning one volume (see `scan_volume_catching_panics`) can never
    // affect another volume's already-decided-good results - each volume
    // gets its own `mftscan::scan_roots` call.
    let mut candidates_by_volume: HashMap<char, Vec<(i64, PathBuf)>> = HashMap::new();
    for check in &checks {
        if scan_route::initial_route(
            elevated,
            scan_routing,
            check,
            &volume_available,
            &volume_ssd,
        ) != ScanRoute::Mft
        {
            continue;
        }
        let Some(letter) = check.volume_letter else {
            continue; // structurally unreachable (Mft route implies Some), never panics either way
        };
        candidates_by_volume
            .entry(letter)
            .or_default()
            .push((check.game_id, check.install_dir.clone()));
    }

    let install_dir_by_id: HashMap<i64, &PathBuf> =
        games.iter().map(|(id, _, dir)| (*id, dir)).collect();

    let mut entries_by_id: HashMap<i64, Vec<FileEntry>> = HashMap::new();

    for roots in candidates_by_volume.into_values() {
        let game_ids: Vec<i64> = roots.iter().map(|(id, _)| *id).collect();

        let mut progress_cb = |p: mftscan::MftProgress| {
            let pct = (p.records_done * 100)
                .checked_div(p.records_total)
                .unwrap_or(0);
            let _ = tx.send(WorkerMsg::Progress {
                verb: Verb::Scan,
                current: p.records_done as usize,
                total: p.records_total as usize,
                detail: i18n::reading_mft_detail(lang, p.volume, pct),
            });
        };

        let results = scan_volume_catching_panics(
            || mftscan::scan_roots(&roots, Some(&mut progress_cb), Some(cancel)),
            &game_ids,
        );

        for (game_id, result) in results {
            let mft_ok = result.is_ok();
            let entries = result.unwrap_or_default();
            let entries_empty = entries.is_empty();
            let nonempty_on_disk = entries_empty
                && install_dir_by_id
                    .get(&game_id)
                    .is_some_and(|dir| root_nonempty_on_disk(dir));

            if let ScanRoute::Mft =
                scan_route::finalize_mft_result(mft_ok, entries_empty, nonempty_on_disk)
            {
                entries_by_id.insert(game_id, entries);
            }
        }
    }

    let mft_count = entries_by_id.len();
    MftPassOutcome {
        entries: entries_by_id,
        mft_count,
        walkdir_count: total - mft_count,
    }
}

/// Whether canonicalizing `install_dir` resolves to a path other than the
/// nominal one - a junction, symlink, mount point, or `subst` drive - which
/// means the volume's raw MFT contents can't be trusted to reflect what's
/// actually at `install_dir`. Uses `dunce::canonicalize` rather than
/// `std::fs::canonicalize` so the comparison isn't defeated by the `\\?\`
/// verbatim prefix Windows' own canonicalization adds. A canonicalization
/// failure (e.g. a permissions issue) gets the same safe treatment as a
/// mismatch: fall back to walkdir rather than trust the nominal path.
fn canonical_mismatch(install_dir: &Path) -> bool {
    match dunce::canonicalize(install_dir) {
        Ok(canonical) => !scan_route::paths_case_insensitively_equal(&canonical, install_dir),
        Err(_) => true,
    }
}

/// Cheap, non-recursive check for whether `dir` has at least one entry on
/// disk. Used only to catch the rare case where the MFT pass reports zero
/// files for a root that plainly isn't empty (see
/// `scan_route::WalkdirReason::MftEmptyOnNonEmptyDisk`) - a full recursive
/// walk here would defeat the point of the MFT path being fast, so this
/// only looks at the root's immediate directory entries.
fn root_nonempty_on_disk(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

/// Calls `scan_fn` - one volume's worth of `mftscan::scan_roots` - catching
/// any panic it raises rather than letting it tear down the whole scan job.
///
/// This exists because the underlying `ntfs` crate has been observed to
/// panic (rather than return `Err`) on certain real-world volume layouts;
/// `mftscan` itself is hardened against this at the source, but this is a
/// second, independent safety net at the call site, since a panic must
/// never be allowed to escape the scan worker thread. A caught panic is
/// treated exactly like a volume-level `Err` from `scan_roots` (case "c" of
/// the MFT/walkdir fallback contract): every game on that volume falls back
/// to `walkdir`.
///
/// This is only safe to call from the scan worker's own thread (the MFT
/// pass is not parallelized across threads) - `catch_unwind` only catches a
/// panic unwinding through the *current* thread. If this were ever
/// parallelized (rayon, spawned threads), each parallel task would need its
/// own `catch_unwind` inside its own closure; a panic on another thread does
/// not unwind through this one, and (for rayon specifically) a panicked
/// task's `scope()` call re-raises the panic on the *joining* thread only
/// after every task in the scope has finished, not from inside a
/// `catch_unwind` wrapped around a single task.
fn scan_volume_catching_panics(
    scan_fn: impl FnOnce() -> CoreResult<Vec<(i64, CoreResult<Vec<FileEntry>>)>>,
    game_ids: &[i64],
) -> Vec<(i64, CoreResult<Vec<FileEntry>>)> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(scan_fn)) {
        Ok(Ok(results)) => results,
        Ok(Err(err)) => volume_failure_results(game_ids, err.to_string()),
        Err(_) => {
            volume_failure_results(game_ids, "паніка під час MFT-сканування тому".to_string())
        }
    }
}

fn volume_failure_results(
    game_ids: &[i64],
    message: String,
) -> Vec<(i64, CoreResult<Vec<FileEntry>>)> {
    game_ids
        .iter()
        .map(|&game_id| (game_id, Err(CoreError::Other(message.clone()))))
        .collect()
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
                    verb: Verb::Scan,
                    current: completed,
                    total,
                    detail: prepared.name.clone(),
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
                    verb: Verb::Scan,
                    current: completed,
                    total,
                    detail: name,
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
    source: FindingSource,
    rule_id: String,
    confidence: u8,
    lang_tag: Option<String>,
    /// UI-only folder-grouping metadata; see [`assign_group_dirs`]. Not
    /// persisted to the database - `persist_prepared_game` writes only the
    /// columns backing `Finding`/`FindingRow`'s other fields.
    group_dir: Option<String>,
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

/// Scans one game's install directory with a regular directory walk, then
/// classifies the result via [`classify_game`]. This is the `walkdir` path;
/// games the MFT pass already has entries for skip straight to
/// `classify_game` instead (see `dispatch_scans`).
fn scan_and_prepare_game(
    engine: &RuleEngine,
    lang_detector: &LangDetector,
    game_id: i64,
    name: &str,
    install_dir: &Path,
) -> CoreResult<PreparedGame> {
    let entries = scan_dir(install_dir)?;
    Ok(classify_game(
        engine,
        lang_detector,
        game_id,
        name,
        install_dir,
        entries,
    ))
}

/// Classifies an already-obtained file list - from either `scan_dir`
/// (walkdir) or the MFT index pass - through both the rule engine and the
/// localization detector. Pure CPU work, no filesystem or database access,
/// so this is what actually runs in parallel across scan worker threads
/// regardless of which path supplied `entries`; only
/// [`persist_prepared_game`] needs a `Connection`.
fn classify_game(
    engine: &RuleEngine,
    lang_detector: &LangDetector,
    game_id: i64,
    name: &str,
    install_dir: &Path,
    entries: Vec<FileEntry>,
) -> PreparedGame {
    // `analyze_game` needs sibling context (the language-family heuristic),
    // so it runs once over all of this game's files rather than per-file.
    let lang_findings: HashMap<usize, LangFinding> =
        lang_detector.analyze_game(&entries).into_iter().collect();

    // First pass: combine each entry's rule/localization findings, keeping
    // the entry's index into `entries` so `assign_group_dirs` (which needs
    // the full file list, not just the flagged ones) can be run afterwards.
    let mut combined_by_index: Vec<(usize, CombinedFinding)> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let rule_finding = engine.classify(&entry.rel_path);
        let lang_finding = lang_findings.get(&index);

        if let Some(combined) = combine_finding(rule_finding, lang_finding) {
            combined_by_index.push((index, combined));
        }
    }

    let flagged: HashSet<usize> = combined_by_index.iter().map(|(index, _)| *index).collect();
    let group_dirs = assign_group_dirs(&entries, &flagged);

    let findings = combined_by_index
        .into_iter()
        .map(|(index, combined)| {
            let entry = &entries[index];
            PreparedFinding {
                rel_path: entry.rel_path.clone(),
                size: entry.size,
                source: combined.source,
                rule_id: combined.rule_id,
                confidence: combined.confidence,
                lang_tag: combined.lang_tag,
                group_dir: group_dirs.get(&index).cloned(),
            }
        })
        .collect();

    PreparedGame {
        game_id,
        name: name.to_string(),
        install_dir: install_dir.to_path_buf(),
        entries,
        findings,
    }
}

/// Assigns each flagged file (identified by its index into `entries`) the
/// `\`-separated path of its shallowest fully-flagged ancestor directory,
/// for UI-only tree grouping (see `model::build_tree`).
///
/// Rationale: a folder where *every* file is flagged as non-essential can be
/// shown - and deleted - as one unit instead of scattering its files across
/// whichever categories happen to match each of them individually. The
/// *shallowest* such ancestor is chosen deliberately: it is the largest unit
/// that is still wholly non-essential, so collapsing to it merges the most
/// files while remaining exactly as safe to remove as any single flagged
/// descendant. A directory must contain at least 2 files to be collapsible -
/// a single-file "folder" gains nothing from collapsing, since the file's
/// own row already represents it - and the (implicit) game root is never a
/// candidate, since there is no bounding folder above it to collapse into.
pub(crate) fn assign_group_dirs(
    entries: &[FileEntry],
    flagged: &HashSet<usize>,
) -> HashMap<usize, String> {
    // Directory path -> (total files under it, flagged files under it).
    let mut dir_stats: HashMap<String, (u32, u32)> = HashMap::new();
    let mut dir_chains: Vec<Vec<String>> = Vec::with_capacity(entries.len());

    for (index, entry) in entries.iter().enumerate() {
        let chain = dir_prefixes(&entry.rel_path);
        for dir in &chain {
            let stats = dir_stats.entry(dir.clone()).or_insert((0, 0));
            stats.0 += 1;
            if flagged.contains(&index) {
                stats.1 += 1;
            }
        }
        dir_chains.push(chain);
    }

    let mut group_dirs = HashMap::new();
    for &index in flagged {
        // `chain` is shallowest-first, so the first collapsible entry found
        // is the shallowest collapsible ancestor.
        let collapsible = dir_chains[index].iter().find(|dir| {
            let (total, flagged_count) = dir_stats.get(dir.as_str()).copied().unwrap_or((0, 0));
            total >= 2 && total == flagged_count
        });
        if let Some(dir) = collapsible {
            group_dirs.insert(index, dir.clone());
        }
    }

    group_dirs
}

/// The `\`-separated ancestor directory paths of `rel_path`, shallowest
/// first, excluding the (implicit, empty) game root and the file name
/// itself. E.g. `"a\b\c\file.txt"` -> `["a", "a\\b", "a\\b\\c"]`; a file
/// directly under the game root (no directory segments) yields an empty
/// list.
fn dir_prefixes(rel_path: &str) -> Vec<String> {
    let segments: Vec<&str> = rel_path
        .split(['\\', '/'])
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.len() <= 1 {
        return Vec::new();
    }

    let mut prefixes = Vec::with_capacity(segments.len() - 1);
    let mut acc = String::new();
    for segment in &segments[..segments.len() - 1] {
        if !acc.is_empty() {
            acc.push('\\');
        }
        acc.push_str(segment);
        prefixes.push(acc.clone());
    }
    prefixes
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
            source_key(finding.source),
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
            source: finding.source,
            rule_desc: finding.rule_id.clone(),
            confidence: finding.confidence,
            lang_tag: finding.lang_tag.clone(),
            group_dir: finding.group_dir.clone(),
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
    source: FindingSource,
    rule_id: String,
    confidence: u8,
    lang_tag: Option<String>,
}

/// Merges a rules-engine finding with a localization finding for the same
/// file. Categories are checked in a fixed precedence order (redist → dev
/// leftovers → bonus → docs → localization; see `Category::priority_rank`),
/// so a rule finding always beats a localization cue regardless of
/// confidence: a localized readme (`ReadMe_DE.rtf`) is documentation, and a
/// per-language file inside `Support\` is support material (also the docs
/// category) - the language split inside such folders does not change what
/// the folder is. Localization applies only to files no rule claimed.
fn combine_finding(rule: Option<Finding>, lang: Option<&LangFinding>) -> Option<CombinedFinding> {
    match (rule, lang) {
        (Some(r), _) => Some(CombinedFinding {
            source: FindingSource::Rule(r.category),
            rule_id: r.rule_desc,
            confidence: r.confidence,
            lang_tag: None,
        }),
        (None, Some(l)) => Some(CombinedFinding {
            source: FindingSource::Loc(l.kind),
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
    use gametrimmer_core::langdetect::{LangDetector, LangKind};
    use gametrimmer_core::providers::GameInstall;
    use gametrimmer_core::rules::{Category, RuleEngine};
    use std::fs;

    fn lang_finding_de() -> LangFinding {
        LangFinding {
            lang_tag: "de".to_string(),
            kind: LangKind::Text,
            confidence: 90,
            reason: "мовна сім'я ReadMe_*".to_string(),
        }
    }

    #[test]
    fn combine_finding_prefers_any_rule_category_over_localization() {
        // The localization cue is MORE confident (90 vs 85), but category
        // precedence is fixed: a localized readme is documentation first.
        let rule = Finding {
            category: Category::DocsFile,
            rule_desc: "Файл документації (PDF/RTF)".to_string(),
            confidence: 85,
        };

        let combined = combine_finding(Some(rule), Some(&lang_finding_de()))
            .expect("a rule match must produce a finding");

        assert!(matches!(
            combined.source,
            FindingSource::Rule(Category::DocsFile)
        ));
        assert_eq!(combined.lang_tag, None);
    }

    #[test]
    fn combine_finding_uses_localization_only_when_no_rule_matches() {
        let combined = combine_finding(None, Some(&lang_finding_de()))
            .expect("a localization finding alone must survive");

        assert!(matches!(
            combined.source,
            FindingSource::Loc(LangKind::Text)
        ));
        assert_eq!(combined.lang_tag.as_deref(), Some("de"));
    }

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

    /// The `ntfs` crate has been observed to panic (rather than return
    /// `Err`) on certain real-world volume layouts. `scan_volume_catching_panics`
    /// is the safety net around every per-volume MFT scan call - this
    /// reproduces that scenario with a mock scanner that panics, and asserts
    /// the whole volume falls back to a per-game `Err` (i.e. `walkdir`)
    /// instead of the panic escaping and taking down the scan worker
    /// thread (or the process).
    #[test]
    fn scan_volume_catching_panics_converts_panic_to_per_game_walkdir_fallback() {
        let game_ids = vec![10i64, 20i64, 30i64];

        let results = scan_volume_catching_panics(
            || -> CoreResult<Vec<(i64, CoreResult<Vec<FileEntry>>)>> {
                panic!("simulated ntfs crate panic on a malformed volume")
            },
            &game_ids,
        );

        assert_eq!(
            results.len(),
            game_ids.len(),
            "every game on the panicking volume must still get a result slot"
        );
        for (game_id, result) in &results {
            assert!(
                game_ids.contains(game_id),
                "unexpected game id {game_id} in fallback results"
            );
            assert!(
                result.is_err(),
                "a panicking MFT scan must fall back to walkdir (Err), not silently succeed"
            );
        }
    }

    /// Sibling case: a plain `Err` (no panic) from the scan function must be
    /// handled the same way as a panic - every game on that volume falls
    /// back to walkdir.
    #[test]
    fn scan_volume_catching_panics_converts_err_to_per_game_walkdir_fallback() {
        let game_ids = vec![1i64, 2i64];

        let results = scan_volume_catching_panics(
            || -> CoreResult<Vec<(i64, CoreResult<Vec<FileEntry>>)>> {
                Err(CoreError::Other("volume would not open".to_string()))
            },
            &game_ids,
        );

        assert_eq!(results.len(), game_ids.len());
        assert!(results.iter().all(|(_, result)| result.is_err()));
    }

    /// A successful scan must pass its results through unchanged.
    #[test]
    fn scan_volume_catching_panics_passes_through_successful_results() {
        let game_ids = vec![1i64];

        let results = scan_volume_catching_panics(
            || {
                Ok(vec![(
                    1i64,
                    Ok(vec![FileEntry {
                        rel_path: "a.txt".into(),
                        size: 1,
                        mtime: None,
                    }]),
                )])
            },
            &game_ids,
        );

        assert_eq!(results.len(), 1);
        let (game_id, result) = &results[0];
        assert_eq!(*game_id, 1);
        let entries = result.as_ref().expect("successful scan stays Ok");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].rel_path, "a.txt");
    }

    /// `run_mft_pass` must never attempt the MFT path at all when not
    /// elevated - every game goes to `walkdir`, and no volume is even
    /// probed (there is nothing to unit-test for "no volume probed"
    /// directly without real disks, but the entries map being empty and the
    /// counts matching `total`/`0` is the observable contract).
    #[test]
    fn run_mft_pass_routes_everything_to_walkdir_when_not_elevated() {
        let games = vec![
            (1i64, "Game A".to_string(), PathBuf::from(r"G:\Games\A")),
            (2i64, "Game B".to_string(), PathBuf::from(r"D:\Games\B")),
        ];

        let cancel = AtomicBool::new(false);
        let (tx, _rx) = std::sync::mpsc::channel();
        let outcome = run_mft_pass(false, ScanRouting::Auto, &games, &cancel, &tx, Lang::En);

        assert!(outcome.entries.is_empty());
        assert_eq!(outcome.mft_count, 0);
        assert_eq!(outcome.walkdir_count, 2);
    }

    /// Games with no drive letter (e.g. a UNC path) must never end up in the
    /// MFT entries map, even when elevated - there is no volume to probe.
    #[test]
    fn run_mft_pass_routes_unc_paths_to_walkdir_even_when_elevated() {
        let games = vec![(
            1i64,
            "Networked Game".to_string(),
            PathBuf::from(r"\\server\share\Games\A"),
        )];

        let cancel = AtomicBool::new(false);
        let (tx, _rx) = std::sync::mpsc::channel();
        let outcome = run_mft_pass(true, ScanRouting::Auto, &games, &cancel, &tx, Lang::En);

        assert!(outcome.entries.is_empty());
        assert_eq!(outcome.mft_count, 0);
        assert_eq!(outcome.walkdir_count, 1);
    }

    /// `ScanRouting::ForceWalkdir` must route every game to walkdir even
    /// when elevated, lettered, and otherwise MFT-eligible - and must never
    /// touch `mftscan::is_available`/`media_kind` (unreachable in a unit
    /// test without real volumes, but the resulting counts are the
    /// observable contract, matching `run_mft_pass_routes_everything_to_walkdir_when_not_elevated`).
    #[test]
    fn run_mft_pass_routes_everything_to_walkdir_when_force_walkdir() {
        let games = vec![
            (1i64, "Game A".to_string(), PathBuf::from(r"G:\Games\A")),
            (2i64, "Game B".to_string(), PathBuf::from(r"D:\Games\B")),
        ];

        let cancel = AtomicBool::new(false);
        let (tx, _rx) = std::sync::mpsc::channel();
        let outcome = run_mft_pass(
            true,
            ScanRouting::ForceWalkdir,
            &games,
            &cancel,
            &tx,
            Lang::En,
        );

        assert!(outcome.entries.is_empty());
        assert_eq!(outcome.mft_count, 0);
        assert_eq!(outcome.walkdir_count, 2);
    }

    #[test]
    fn canonical_mismatch_is_true_for_a_path_that_does_not_exist() {
        // A directory that doesn't exist can't be canonicalized, and the
        // safe assumption for a canonicalization failure is "mismatch" -
        // walkdir handles (and reports) a missing directory on its own.
        let missing = Path::new(r"Z:\this\path\does\not\exist\at\all");
        assert!(canonical_mismatch(missing));
    }

    #[test]
    fn canonical_mismatch_is_false_for_a_real_directory_scanned_by_its_own_canonical_path() {
        let dir = tempfile::tempdir().expect("create temp dir");
        // Canonicalize once up front so the "nominal" path passed in is
        // already the canonical one - the whole point of this test is that
        // a root with no junction/symlink in its way must not be flagged.
        let canonical = dunce::canonicalize(dir.path()).expect("canonicalize temp dir");
        assert!(!canonical_mismatch(&canonical));
    }

    #[test]
    fn root_nonempty_on_disk_is_false_for_an_empty_directory() {
        let dir = tempfile::tempdir().expect("create temp dir");
        assert!(!root_nonempty_on_disk(dir.path()));
    }

    #[test]
    fn root_nonempty_on_disk_is_true_once_a_file_exists_inside() {
        let dir = tempfile::tempdir().expect("create temp dir");
        write_file(&dir.path().join("save.dat"), b"data");
        assert!(root_nonempty_on_disk(dir.path()));
    }

    fn entry(rel_path: &str) -> FileEntry {
        FileEntry {
            rel_path: rel_path.to_string(),
            size: 1,
            mtime: None,
        }
    }

    #[test]
    fn dir_prefixes_lists_ancestors_shallowest_first_excluding_root_and_file_name() {
        assert_eq!(
            dir_prefixes(r"a\b\c\file.txt"),
            vec!["a".to_string(), r"a\b".to_string(), r"a\b\c".to_string()]
        );
    }

    #[test]
    fn dir_prefixes_is_empty_for_a_file_directly_under_the_game_root() {
        assert!(dir_prefixes("readme.txt").is_empty());
    }

    #[test]
    fn assign_group_dirs_collapses_a_folder_where_every_file_is_flagged() {
        let entries = vec![entry(r"junk\a.txt"), entry(r"junk\b.txt")];
        let flagged: HashSet<usize> = [0, 1].into_iter().collect();

        let groups = assign_group_dirs(&entries, &flagged);

        assert_eq!(groups.get(&0), Some(&"junk".to_string()));
        assert_eq!(groups.get(&1), Some(&"junk".to_string()));
    }

    #[test]
    fn assign_group_dirs_does_not_collapse_a_folder_with_an_unflagged_file() {
        let entries = vec![
            entry(r"mixed\a.txt"),
            entry(r"mixed\b.txt"), // not flagged below
        ];
        let flagged: HashSet<usize> = [0].into_iter().collect();

        let groups = assign_group_dirs(&entries, &flagged);

        assert_eq!(
            groups.get(&0),
            None,
            "the folder has an unflagged member, so it must not collapse"
        );
    }

    #[test]
    fn assign_group_dirs_does_not_collapse_a_single_file_folder() {
        let entries = vec![entry(r"lonely\only.txt")];
        let flagged: HashSet<usize> = [0].into_iter().collect();

        let groups = assign_group_dirs(&entries, &flagged);

        assert_eq!(
            groups.get(&0),
            None,
            "a folder with only one file gains nothing from collapsing"
        );
    }

    #[test]
    fn assign_group_dirs_picks_the_shallowest_collapsible_ancestor() {
        // Both "top" and "top\\nested" are fully flagged and have >= 2
        // files; "top" is shallower and must win.
        let entries = vec![
            entry(r"top\nested\a.txt"),
            entry(r"top\nested\b.txt"),
            entry(r"top\c.txt"),
        ];
        let flagged: HashSet<usize> = [0, 1, 2].into_iter().collect();

        let groups = assign_group_dirs(&entries, &flagged);

        assert_eq!(groups.get(&0), Some(&"top".to_string()));
        assert_eq!(groups.get(&1), Some(&"top".to_string()));
        assert_eq!(groups.get(&2), Some(&"top".to_string()));
    }

    #[test]
    fn assign_group_dirs_never_collapses_the_game_root() {
        // Two flagged files directly at the root: there is no directory
        // string representing the root itself for them to collapse into.
        let entries = vec![entry("a.txt"), entry("b.txt")];
        let flagged: HashSet<usize> = [0, 1].into_iter().collect();

        let groups = assign_group_dirs(&entries, &flagged);

        assert!(groups.is_empty(), "root-level files are always orphans");
    }

    #[test]
    fn assign_group_dirs_leaves_unflagged_files_out_of_the_result() {
        let entries = vec![entry(r"junk\a.txt"), entry(r"junk\b.txt")];
        let flagged: HashSet<usize> = [0].into_iter().collect();

        let groups = assign_group_dirs(&entries, &flagged);

        assert_eq!(
            groups.len(),
            0,
            "\"junk\" has only 1 of 2 files flagged, so it can't collapse, \
             and the one flagged file has no other collapsible ancestor"
        );
    }
}
