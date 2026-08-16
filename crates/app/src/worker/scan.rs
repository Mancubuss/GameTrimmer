//! The "Scan Libraries" job: discover Steam libraries, persist them,
//! then scan and classify every game's files. Runs entirely on a
//! background thread; the only database connection used here is opened
//! and dropped within this thread.

mod discovery;
mod generation;
mod orphan_analysis;
mod persistence;

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use eframe::egui;
use gametrimmer_core::db;
use gametrimmer_core::error::{CoreError, Result as CoreResult};
use gametrimmer_core::langdetect::{LangData, LangDetector, LangFinding};
use gametrimmer_core::mftscan;
use gametrimmer_core::orphans::{self, OrphanKind};
use gametrimmer_core::perf;
use gametrimmer_core::providers::{
    self, DiscoveredLibrary, DiscoveryDiagnostic, DiscoveryReport, DiscoveryStatus, OrphanEvidence,
};
use gametrimmer_core::rules::{Finding, RuleEngine, RuleProvenance};
use gametrimmer_core::safety::{SafetySnapshot, SnapshotCapture};
use gametrimmer_core::scanner::{scan_dir_cancellable, store_files_no_tx, FileEntry, ScanStats};
use gametrimmer_core::sysinfo;
use rusqlite::{params, Connection};

use crate::i18n::{self, Lang, Verb};
use crate::model::{
    category_enabled, display_category, orphan_confidence, orphan_install_dir_and_name, source_key,
    DisplayCategory, FindingRow, FindingSource, LibraryOrigin, ORPHAN_GAME_ID,
};

use super::scan_route::{self, ScanRoute};
use super::{manual, Notifier, WorkerMsg};
use generation::ScanGenerationGuard;
#[cfg(test)]
use orphan_analysis::PreparedOrphan;
use orphan_analysis::{collect_orphans, persist_orphans};
#[cfg(test)]
use persistence::persist_prepared_game;
use persistence::{persist_libraries, run_writer};

/// Worker threads used for scanning+classifying games in parallel.
///
/// This was a constant 6 until the seeks came out of the pool. Widening it
/// was tried on 2026-08-16 at 12 workers and measured **70.7 s -> 136.8 s**,
/// with the whole loss in one stage: `safety` went from 37.7 s to 617.0 s of
/// thread time, 52 us per open to 857 us. That was never CPU work - it was
/// one `CreateFileW` per flagged file, 720 k random metadata reads against
/// the same mechanical volume the `$MFT` had just been read from, and twelve
/// queues into one set of heads is a seek storm rather than parallelism.
/// Everything that *is* CPU measured the same at either width, which was the
/// tell.
///
/// Those opens are gone: the leaf identity now comes from the `$MFT` record
/// the scan already read (see `safety::stated_identity`), and `safety` fell
/// to 10.1 s of thread time. What is left in the pool is arithmetic on
/// strings, so it is worth the machine's full width again.
///
/// Resolved once per process: the sites below must agree on one number, and
/// the run's own log line reports which one it got rather than leaving it to
/// be assumed from the hardware.
fn scan_threads() -> usize {
    static WIDTH: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *WIDTH.get_or_init(|| {
        std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(6)
    })
}

/// Size of the database's write-ahead log, or 0 when it is absent (the
/// journal fell back to `DELETE` mode, or nothing has been written yet).
fn wal_bytes(db_path: &Path) -> u64 {
    let mut wal = db_path.as_os_str().to_owned();
    wal.push("-wal");
    std::fs::metadata(PathBuf::from(wal))
        .map(|meta| meta.len())
        .unwrap_or(0)
}

fn format_bytes(bytes: u64) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.0} KB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0),
    }
}

/// Describes what this scan will have to write over, for the environment
/// line above.
///
/// Generation **0 is not a generation**: it is the legacy sentinel every
/// database is created with, and `db::activate_scan` skips it by the same
/// `!= 0` test. Reading `active_scan_id` as a plain `Option` reports a
/// brand-new database as "holds a previous generation", which is the exact
/// opposite of the truth and is the condition every baseline run is measured
/// under - this got into the log once already.
///
/// The row count rides along because that is what the cost is proportional
/// to: activating a generation deletes the superseded one's rows in one
/// transaction, which is why a rescan has cost 2.4x a first scan. `files`
/// has no `scan_id` index (dropped in A3, it only ever cost writes), so this
/// is a table scan - tens of milliseconds once per scan, against a phase
/// measured in tens of seconds.
fn previous_generation(conn: &Connection) -> String {
    match db::active_scan_id(conn) {
        Ok(Some(id)) if id != 0 => {
            match conn.query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, i64>(0)) {
                Ok(rows) => format!("holds generation {id} ({rows} file rows to supersede)"),
                Err(_) => format!("holds generation {id}"),
            }
        }
        Ok(_) => "empty".to_string(),
        Err(err) => format!("state unreadable ({err})"),
    }
}

/// How many games' `files`/`findings` writes share one database transaction.
/// Bigger batches commit less often (fewer WAL syncs), but hold a batch's
/// results in memory and delay their visibility to the UI's "Done" message
/// by up to one batch - 16-32 is the sweet spot named in the benchmark.
const WRITE_BATCH_SIZE: usize = 24;

/// How many files pass between `cancel` polls inside [`classify_game`]'s
/// rule-engine loop. Mirrors `gametrimmer_core::scanner::CANCEL_POLL_INTERVAL`
/// (which is `pub(crate)` to the core crate and so not reachable here): the
/// core localization pass already polls at that cadence internally, and this
/// keeps the app-side rule pass equally responsive without an atomic load per
/// file in the common, never-cancelled case.
const CLASSIFY_CANCEL_POLL_INTERVAL: usize = 1024;

/// The settings-derived knobs a scan runs under, captured once at spawn
/// time: a scan keeps the options it started with even if the user changes
/// settings mid-run (the next scan picks the changes up).
pub struct ScanOptions {
    /// UI language for the status/warning strings this worker produces.
    pub lang: Lang,
    /// The persisted `keep_languages` setting - languages the localization
    /// detector never flags.
    pub keep_languages: Vec<String>,
    /// The persisted `enabled_categories` setting - findings in unchecked
    /// categories are dropped at classification time (empty = all enabled).
    pub enabled_categories: Vec<String>,
    /// The persisted `excluded_libraries` setting - registered library roots
    /// (normalized, see `gametrimmer_core::providers::comparable_path`) the
    /// scan does not descend into. Applied in `discovery::discover_libraries`,
    /// after the cross-provider merge/dedupe pass - see `discovery::drop_excluded`.
    pub excluded_libraries: Vec<String>,
}

/// Spawns the scan job on a new thread. `cancel` is polled between games so
/// the user can abort a long scan early. `elevated` reflects whether this
/// process holds Administrator rights right now (see `crate::elevation`) -
/// it gates the MFT index scan path, which needs raw volume read access.
/// `ctx` is the app's `egui::Context` (see `Notifier`) so scan progress keeps
/// updating even while the main window is minimized.
pub fn spawn_scan(
    db_path: PathBuf,
    cancel: Arc<AtomicBool>,
    tx: Sender<WorkerMsg>,
    ctx: egui::Context,
    elevated: bool,
    options: ScanOptions,
) -> JoinHandle<()> {
    let notifier = Notifier::new(tx, ctx);
    std::thread::spawn(move || run_scan(&db_path, &cancel, &notifier, elevated, &options))
}

fn run_scan(
    db_path: &Path,
    cancel: &AtomicBool,
    notifier: &Notifier,
    elevated: bool,
    options: &ScanOptions,
) {
    let ScanOptions {
        lang,
        keep_languages,
        enabled_categories,
        excluded_libraries,
    } = options;
    let (lang, keep_languages, enabled_categories, excluded_libraries) = (
        *lang,
        keep_languages.as_slice(),
        enabled_categories.as_slice(),
        excluded_libraries.as_slice(),
    );
    let started_at = Instant::now();
    crate::logger::log(&format!(
        "Scan started (elevated: {elevated}, keep: {})",
        keep_languages.join(",")
    ));
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
            notifier.report_warning(i18n::Reported::new(lang, |l| {
                i18n::rules_json_load_failed(l, &err)
            }));
            match RuleEngine::from_json(gametrimmer_core::rules::BUILTIN_RULES_JSON) {
                Ok(engine) => engine,
                Err(err) => {
                    // The embedded defaults are validated by core tests, so
                    // this is unreachable short of a broken build.
                    notifier.report_error(i18n::Reported::new(lang, |l| {
                        i18n::builtin_rules_corrupted(l, &err)
                    }));
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
            notifier.report_warning(i18n::Reported::new(lang, |l| {
                i18n::l10n_rules_load_failed(l, &err)
            }));
            LangData::builtin()
        }
    };
    let lang_detector = LangDetector::with_data(lang_data, keep_languages);

    let mut conn = match db::open(db_path) {
        Ok(conn) => conn,
        Err(err) => {
            notifier.report_error(i18n::Reported::new(lang, |l| {
                i18n::db_open_error_long(l, &err)
            }));
            return;
        }
    };

    // This connection writes a whole generation and checkpoints once, at the
    // end (below, after the prune). Non-fatal: failing to set it costs the
    // scan time, not correctness.
    if let Err(err) = db::defer_wal_checkpoints(&conn) {
        crate::logger::error(&format!(
            "Could not defer WAL checkpoints for the scan: {err}"
        ));
    }

    // The conditions two runs of the same scan differ by. Without this line
    // a six-second gap between two otherwise identical runs has nothing to
    // be attributed to - see `gametrimmer_core::sysinfo`, which also
    // explains why "was the cache warm" is answered by the `$MFT` read's
    // throughput rather than by anything queryable here. Whether the
    // database already holds a generation belongs on the same line: it is
    // the largest known factor (a rescan has cost 2.4x a first scan's
    // analyze time) and it is recorded nowhere else.
    crate::logger::log(&format!(
        "Environment: {}, database {}",
        sysinfo::system_state(),
        previous_generation(&conn)
    ));

    // Discovery + persist below is the phase that used to show nothing at
    // all for its whole (15-20s on a big library) duration - the UI cleared
    // its status line on scan start and the first `Progress` only arrives
    // once a game finishes scanning. A couple of coarse status updates give
    // the spinner something to say so the app doesn't look frozen.
    notifier.send(WorkerMsg::Status {
        text: i18n::strings(lang).detecting_libraries.to_string(),
    });

    let discovery::DiscoveryOutcome {
        libraries,
        diagnostics: discovery_diagnostics,
        degraded: discovery_degraded,
    } = match discovery::discover_libraries(&conn, lang, notifier, excluded_libraries) {
        Ok(discovery) => discovery,
        Err(error) => {
            notifier.report_error(error);
            return;
        }
    };

    notifier.send(WorkerMsg::Status {
        text: i18n::strings(lang).preparing_database.to_string(),
    });

    let discovery_status = if discovery_degraded {
        "degraded"
    } else {
        "complete"
    };
    let scan_id = match db::begin_scan(&conn, discovery_status) {
        Ok(scan_id) => scan_id,
        Err(err) => {
            notifier.report_error(i18n::Reported::new(lang, |l| {
                i18n::libraries_write_failed(l, &err)
            }));
            return;
        }
    };
    // From here to the end of the scan, every logged line names this
    // generation - including the ones written deep inside persistence and
    // inside the `catch_unwind` sites, which is where the association is
    // worth the most. Dropped on every exit path, cancellation included.
    let _log_scope = crate::logger::ScanScope::new(scan_id);
    let mut generation = ScanGenerationGuard::new(db_path, scan_id);

    for library in &libraries {
        let status = match library.orphan_evidence {
            OrphanEvidence::Authoritative => "complete",
            OrphanEvidence::Degraded => "degraded",
            OrphanEvidence::Heuristic => "heuristic",
        };
        if let Err(err) =
            db::record_scan_library_evidence(&conn, scan_id, &library.path, library.vendor, status)
        {
            notifier.report_error(i18n::Reported::new(lang, |l| {
                i18n::libraries_write_failed(l, &err)
            }));
            return;
        }
    }
    for diagnostic in &discovery_diagnostics {
        if let Err(err) = db::record_scan_diagnostic(
            &conn,
            scan_id,
            diagnostic.provider,
            diagnostic.stage,
            diagnostic.path.as_deref(),
            &diagnostic.message,
        ) {
            notifier.report_error(i18n::Reported::new(lang, |l| {
                i18n::libraries_write_failed(l, &err)
            }));
            return;
        }
    }

    let games = match persist_libraries(&conn, &libraries, scan_id) {
        Ok(games) => games,
        Err(err) => {
            notifier.report_error(i18n::Reported::new(lang, |l| {
                i18n::libraries_write_failed(l, &err)
            }));
            return;
        }
    };

    notifier.send(WorkerMsg::LibrariesFound {
        libraries: libraries.len(),
        games: games.len(),
    });
    crate::logger::log(&format!(
        "Libraries: {}, games: {}",
        libraries.len(),
        games.len()
    ));

    if cancel.load(Ordering::Relaxed) {
        crate::logger::log("Scan cancelled");
        generation.abort(&mut conn, "cancelled");
        notifier.send(WorkerMsg::Cancelled);
        return;
    }

    let total = games.len();

    // Everything the MFT routing can decide before a single byte is read:
    // which install roots are even eligible (elevated, on an NTFS volume
    // that opens, not behind a junction/symlink/mount point/`subst`), and
    // which volume each eligible root will get its file list from. Cheap - a
    // canonicalization per root and a media/availability probe per volume.
    // The expensive half, reading each volume's Master File Table, happens
    // inside `dispatch_scans`, underneath the classification it used to run
    // strictly before.
    let plan = plan_mft_pass(elevated, &games);

    if cancel.load(Ordering::Relaxed) {
        crate::logger::log("Scan cancelled");
        generation.abort(&mut conn, "cancelled");
        notifier.send(WorkerMsg::Cancelled);
        return;
    }

    // Scanning+classification (IO and CPU work, no DB) happens in parallel
    // across `scan_threads()` worker threads; only the DB writes are
    // serialized, on a single writer thread that drains `result_rx` as
    // results arrive. This way scanning game N+1 never waits on game N's
    // write, and the write side never has more than one connection open.
    // See `crates/core/examples/scan_bench.rs` for the measurements that
    // motivated this over the previous fully-sequential loop.
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel::<GameOutcome>(2 * scan_threads());

    // Shared count of games the writer has finished persisting. The writer
    // owns writing it; the scan workers only read it, to label the "started
    // scanning <game>" progress they emit (see `dispatch_scans`) with a
    // sensible `current/total`. Reads may lag the true count by a game or two
    // under the race with in-flight completions, which is harmless for a
    // progress label.
    let completed = std::sync::atomic::AtomicUsize::new(0);

    // Reading one volume's `$MFT`. Passed to `dispatch_scans` rather than
    // called by it, because this is the seam the overlap has to be tested
    // at: "a volume's worth of file lists arrives, some time later" is
    // exactly what a test needs to control, and it cannot conjure an NTFS
    // volume to control it with.
    let read_volume = |roots: &[(i64, PathBuf)]| {
        let game_ids: Vec<i64> = roots.iter().map(|(game_id, _)| *game_id).collect();
        // One bar, one meaning. The bar counts games classified - the work
        // that finishes last - and the file-table read, which now runs
        // underneath it, writes only the detail line. Reporting records read
        // as the bar's own fraction would put two producers with two
        // different totals on one bar, and the fraction would jump backwards
        // every time the other one won.
        let mut progress_cb = |p: mftscan::MftProgress| {
            let pct = (p.records_done * 100)
                .checked_div(p.records_total)
                .unwrap_or(0);
            notifier.send(WorkerMsg::Progress {
                verb: Verb::Analyze,
                current: completed.load(Ordering::Relaxed),
                total,
                detail: i18n::reading_mft_detail(lang, p.volume, pct),
            });
        };

        scan_volume_catching_panics(
            || mftscan::scan_roots(roots, Some(&mut progress_cb), Some(cancel)),
            &game_ids,
        )
    };

    let context = ClassifyContext {
        engine: &engine,
        lang_detector: &lang_detector,
        enabled_categories,
        cancel,
        notifier,
        completed: &completed,
        total,
    };

    // Opens the overlapped window: from here until `analyze_phase_end` the
    // MFT reading and the classification of everything already read run at
    // the same time. The two durations reported below therefore overlap
    // rather than partition the run - see `crate::model::ScanTiming`.
    let dispatch_started = Instant::now();
    // So the breakdown logged at the end describes this run, not every scan
    // since the app started.
    perf::reset();

    let (write_outcome, mft_pass) = std::thread::scope(|scope| {
        let writer = scope.spawn(|| {
            run_writer(
                &mut conn, result_rx, notifier, total, &completed, cancel, scan_id,
            )
        });

        let mft_pass = dispatch_scans(&context, &games, plan, &read_volume, &result_tx);
        // Dropping the last sender lets the writer's `for outcome in rx`
        // loop end once every dispatched scan has reported in.
        drop(result_tx);

        (writer.join(), mft_pass)
    });

    // Closes the overlapped window: the writer has joined, so nothing is
    // reading, classifying or writing any more. Everything past here is
    // post-scan housekeeping (WAL checkpoint, summary/occupancy
    // computation) that the user does not see as part of the progress bar.
    // See `crate::model::ScanTiming`.
    let analyze_phase_end = Instant::now();

    crate::logger::log(&format!(
        "MFT pass: {} via MFT, {} via walkdir",
        mft_pass.mft_count, mft_pass.walkdir_count
    ));

    let mut findings = match write_outcome {
        Ok(Ok(findings)) => findings,
        Ok(Err(err)) => {
            if cancel.load(Ordering::Relaxed) || err.to_string() == "cancelled" {
                generation.abort(&mut conn, "cancelled");
                notifier.send(WorkerMsg::Cancelled);
            } else {
                notifier.report_error(i18n::Reported::new(lang, |l| {
                    i18n::scan_incomplete(l, &err)
                }));
            }
            return;
        }
        Err(_) => {
            notifier.report_error(i18n::Reported::new(lang, i18n::write_thread_crashed));
            return;
        }
    };

    // Written here rather than before the pool starts, which is where it
    // used to go: half of this evidence - the candidate roots the pass
    // itself rejected, see `scan_route::finalize_mft_result` - is only
    // decided while the pool is running, and the writer thread owns `conn`
    // for that whole window. Nothing reads `games.scan_route` before the
    // generation is activated below, so deferring it changes nothing but
    // when the rows are stamped.
    record_routing_evidence(&conn, scan_id, &games, &mft_pass);

    if cancel.load(Ordering::Relaxed) {
        crate::logger::log("Scan cancelled");
        generation.abort(&mut conn, "cancelled");
        notifier.send(WorkerMsg::Cancelled);
        return;
    }

    // orphan-residue safety: orphaned launcher residue as its own tree branch. Runs after the
    // per-game writer thread has joined (so `conn` is ours again) and only on a
    // scan that reached here without cancellation. Detection is Steam-only for
    // now - see `collect_orphans`. `persist_orphans` always replaces the whole
    // set of `NULL`-game orphan rows first, so it is called even when the
    // category is disabled (with an empty list) to clear any stale rows a
    // prior scan left behind - otherwise disabling the category wouldn't hide
    // them on the next load.
    if category_enabled(enabled_categories, DisplayCategory::Orphan) {
        let orphan_collection = collect_orphans(&libraries, cancel);
        if !cancel.load(Ordering::Relaxed) {
            for issue in &orphan_collection.issues {
                // Same as provider discovery: full detail to the log, nothing
                // to the window. The user-visible consequence is already in
                // the result - no leftovers are offered for this library.
                crate::logger::error(&i18n::provider_failed(
                    Lang::En,
                    issue.provider,
                    format!(
                        "{} [{}: {}]",
                        issue.message,
                        issue.stage,
                        issue.path.display()
                    ),
                ));
                if let Err(err) = db::mark_scan_degraded(&conn, scan_id)
                    .and_then(|_| {
                        db::record_scan_library_evidence(
                            &conn,
                            scan_id,
                            &issue.library_path,
                            issue.provider,
                            "degraded",
                        )
                    })
                    .and_then(|_| {
                        db::record_scan_diagnostic(
                            &conn,
                            scan_id,
                            issue.provider,
                            issue.stage,
                            Some(&issue.path),
                            &issue.message,
                        )
                    })
                {
                    notifier.report_error(i18n::Reported::new(lang, |l| {
                        i18n::libraries_write_failed(l, &err)
                    }));
                    return;
                }
                for row in &mut findings {
                    let root = issue.library_path.to_string_lossy().to_lowercase();
                    let install = row.install_dir.to_string_lossy().to_lowercase();
                    let nested = install == root
                        || install
                            .strip_prefix(&root)
                            .is_some_and(|tail| tail.starts_with(['\\', '/']));
                    if nested {
                        row.deletion_block_reason =
                            Some("library discovery was degraded".to_string());
                    }
                }
            }
            match persist_orphans(&mut conn, &orphan_collection.orphans, scan_id) {
                Ok(mut rows) => {
                    crate::logger::log(&format!("Orphans: {} found", rows.len()));
                    findings.append(&mut rows);
                }
                Err(err) => {
                    notifier.report_error(i18n::Reported::new(lang, |l| {
                        i18n::orphans_persist_failed(l, &err)
                    }));
                    return;
                }
            }
        }
    } else if let Err(err) = persist_orphans(&mut conn, &[], scan_id) {
        notifier.report_error(i18n::Reported::new(lang, |l| {
            i18n::orphans_persist_failed(l, &err)
        }));
        return;
    }

    if cancel.load(Ordering::Relaxed) {
        generation.abort(&mut conn, "cancelled");
        notifier.send(WorkerMsg::Cancelled);
        return;
    }
    // Timed on its own because it is the largest thing neither progress verb
    // covers: activation validates the whole new generation and runs a
    // database-wide foreign-key check before the pointer may move. Deleting
    // the generation it supersedes used to happen here too, in the same
    // transaction, and was three quarters of the cost; it now runs after the
    // results are reported (see the prune below).
    let activate_started = Instant::now();
    if let Err(err) = generation.activate(&mut conn) {
        notifier.report_error(i18n::Reported::new(lang, |l| {
            i18n::libraries_write_failed(l, &err)
        }));
        return;
    }
    crate::logger::log(&format!(
        "Generation activated in {:?}",
        activate_started.elapsed()
    ));

    let walked_reasons: Vec<scan_route::WalkdirReason> = mft_pass
        .walkdir_reasons
        .iter()
        .map(|(_, reason)| *reason)
        .collect();
    let routing_breakdown = scan_route::format_walkdir_breakdown(lang, total, &walked_reasons);

    let scan_summary = scan_route::format_scan_summary(
        lang,
        total,
        mft_pass.mft_count,
        mft_pass.walkdir_count,
        started_at.elapsed().as_secs_f64(),
    );

    // Live occupied-space snapshot for the UI (see `occupancy_or_default`);
    // an aggregation failure degrades to 0 rather than hiding the results.
    let occupancy = super::occupancy_or_default(&conn);

    // `scan` and `analyze` are each measured directly from their own Instant
    // pair rather than derived by subtracting one from the other, so they
    // stay internally consistent. Since the MFT reading moved underneath the
    // classification the two spans *overlap*: both start when the libraries
    // are on disk, `scan` ends when the last volume has been read and
    // `analyze` when the writer has joined. Their sum therefore exceeds the
    // total, and the log says so rather than leaving a reader to discover it
    // by adding two numbers that no longer add up.
    //
    // Housekeeping is what happens after `analyze_phase_end`: orphan
    // collection, routing evidence, generation activation, the WAL
    // checkpoint, this summary and the occupancy query. It is measured
    // directly here for the same reason - a rescan has spent a sixth of its
    // wall clock there, and it can no longer be inferred by subtraction. See
    // `crate::model::ScanTiming`.
    let timing = crate::model::ScanTiming {
        scan: mft_pass.read_finished.duration_since(started_at),
        analyze: analyze_phase_end.duration_since(dispatch_started),
        total: started_at.elapsed(),
    };
    let housekeeping = timing
        .total
        .saturating_sub(analyze_phase_end.duration_since(started_at));
    crate::logger::log(&format!(
        "Scan done in {:?} (scan {:?} and analyze {:?} overlap - the file table is read \
         underneath the classification, so these two do not sum to the total; \
         housekeeping {housekeeping:?}), findings: {}",
        timing.total,
        timing.scan,
        timing.analyze,
        findings.len()
    ));
    // Where that analyze window actually went, stage by stage - the thing
    // three rounds of optimisation had to guess at. See `perf::report` for
    // why the sum exceeds the wall clock. The worker count rides along
    // because the whole line is per-thread sums, and dividing them by the
    // wrong number of threads is the easiest way to misread it.
    crate::logger::log(&format!(
        "{} across {} workers",
        perf::report(),
        scan_threads()
    ));
    // The writer is the only stage that is a single thread, so its total is
    // wall clock rather than a per-thread sum, and it is the whole exposed
    // tail once the `$MFT` read finishes. Split three ways because it has
    // measured 17.6 s, 20.5 s and 26.3 s on identical work.
    if let Some(breakdown) = perf::persist_breakdown() {
        crate::logger::log(&breakdown);
    }
    // Same numbers the bottom bar shows, kept past this session: "the scan
    // took 40 minutes" is otherwise unanswerable without asking the user to
    // sit through it again. Non-fatal - a scan that produced results must
    // not fail because a note about it could not be written.
    if let Err(err) = db::record_scan_timing(
        &conn,
        scan_id,
        timing.scan.as_millis() as u64,
        timing.analyze.as_millis() as u64,
        timing.total.as_millis() as u64,
    ) {
        crate::logger::error(&format!("Failed to record scan timing: {err}"));
    }

    notifier.send(WorkerMsg::Done {
        findings,
        scan_summary,
        occupancy,
        timing: Some(timing),
        routing_breakdown,
    });

    // Everything below happens with the results already on screen. Deleting
    // the superseded generation was 12.1 s of a 13.5 s rescan housekeeping,
    // and no reader was ever waiting for it - the active pointer moved above,
    // and both `load_findings` and `occupied_by_library` filter on it. Timed
    // and logged, because work that no longer shows up in `Scan done in` is
    // work that quietly stops being measured. Non-fatal: the next scan's
    // activation hands the same rows straight back here.
    let prune_started = Instant::now();
    match db::prune_superseded(&mut conn) {
        Ok(()) => crate::logger::log(&format!(
            "Superseded generation pruned in {:?} (after the results were reported)",
            prune_started.elapsed()
        )),
        Err(err) => {
            crate::logger::error(&format!("Failed to prune the superseded generation: {err}"))
        }
    }

    // Fold this connection's own WAL into the main file and truncate it
    // before dropping the connection, rather than leaving a large,
    // uncheckpointed `-wal` behind for whatever connection opens the
    // database next (e.g. "Clear database"). This is what triggered the
    // reported "database disk image is malformed" error: a big uncheckpointed
    // WAL left after a completed scan put the next connection's WAL-recovery
    // into an ambiguous state. It runs *after* the prune, which is by far the
    // largest thing this connection writes. Non-fatal by design - an
    // otherwise-successful scan must not be reported as failed just because
    // its final housekeeping checkpoint didn't take.
    let wal_before = wal_bytes(db_path);
    if let Err(err) = db::checkpoint_truncate(&conn) {
        crate::logger::error(&format!(
            "Failed to checkpoint the WAL after the scan: {err}"
        ));
    }
    // Both sizes, because deferring the checkpoints is a trade and this is
    // the side of it that costs something: the WAL is allowed to grow for a
    // whole scan instead of being folded back sixty-seven times. The second
    // figure is the one to watch - `wal_checkpoint(TRUNCATE)` reports whether
    // it was blocked in a row this helper discards, so a WAL that is still
    // large here is the only signal that a reader held it off.
    crate::logger::log(&format!(
        "WAL {} before the final checkpoint, {} after",
        format_bytes(wal_before),
        format_bytes(wal_bytes(db_path))
    ));
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

/// The inputs every game's classification shares for a whole run, grouped
/// so the dispatch path carries one reference instead of seven parameters
/// that always travel together.
struct ClassifyContext<'a> {
    engine: &'a RuleEngine,
    lang_detector: &'a LangDetector,
    /// The persisted `enabled_categories` setting; empty means "all".
    enabled_categories: &'a [String],
    cancel: &'a AtomicBool,
    notifier: &'a Notifier,
    /// Games the writer has finished persisting - written by the writer,
    /// only read here, to label the "started scanning <game>" progress.
    completed: &'a std::sync::atomic::AtomicUsize,
    /// Games in this run: the denominator of every progress message.
    total: usize,
}

/// One game's work as handed to the pool.
///
/// Everything is owned - including `entries`, which are *moved* out of the
/// MFT pass rather than cloned out of a map that outlives the whole scan.
/// That is what keeps peak memory down: a game's file list is dropped the
/// moment its task finishes, so what is resident is "read but not yet
/// classified" rather than "everything every volume held", and the
/// per-in-flight-game copy the old `entries.clone()` made is gone entirely
/// (on a 400 000-file game that clone was tens of megabytes, six at a time).
struct GameTask {
    game_id: i64,
    name: String,
    install_dir: PathBuf,
    /// `Some` when the MFT pass produced this game's file list. `None` means
    /// walk the directory instead - either the root was never an MFT
    /// candidate, or the pass rejected the result it got for it.
    entries: Option<Vec<FileEntry>>,
    result_tx: SyncSender<GameOutcome>,
    /// Cloned per task on the dispatching thread, before the task is
    /// spawned, rather than shared across the pool's threads.
    notifier: Notifier,
}

/// Scans and classifies one game, then reports it to the writer.
///
/// `cancel` is polled here, right before the game's work starts - a game not
/// yet started is reported as cancelled immediately instead of being
/// scanned. It is additionally threaded into the walkdir enumeration
/// (`scan_and_prepare_game` -> `scan_dir_cancellable`) AND into
/// classification (`classify_game` -> `analyze_game_cancellable` + the
/// rule-engine pass), so a game already running on a worker thread is
/// interrupted promptly through whichever phase it is in - directory walk or
/// analysis - rather than finishing its whole tree or its whole file list
/// first. This holds on the MFT-entries branch too: it skips the walk but
/// its classification is just as cancellable, so a huge game (ARK) no longer
/// runs analysis to completion after Stop is pressed.
fn run_one(ctx: &ClassifyContext<'_>, task: GameTask) {
    let GameTask {
        game_id,
        name,
        install_dir,
        entries,
        result_tx,
        notifier,
    } = task;

    if ctx.cancel.load(Ordering::Relaxed) {
        let _ = result_tx.send(GameOutcome::Failed {
            name,
            install_dir,
            error: CoreError::Other("cancelled".to_string()),
        });
        return;
    }

    // Report the game as it *starts*, not just when it finishes. The
    // completed counter only advances when the writer persists a game, so at
    // the tail of a scan - when every quick game is done and one huge game
    // (e.g. ARK) is still being walked on a single worker thread - the bar
    // would otherwise sit at "N-1/N: <some finished game>" with no hint of
    // what it's waiting on. This makes the still-running game's name the
    // visible detail instead.
    notifier.send(WorkerMsg::Progress {
        verb: Verb::Analyze,
        current: ctx.completed.load(Ordering::Relaxed),
        total: ctx.total,
        detail: name.clone(),
    });

    // Both paths funnel the game through `classify_game`; either reports a
    // cancelled game through the same `Failed { .. "cancelled" .. }` channel
    // `run_writer` already treats as a normal user action.
    let result = match entries {
        Some(entries) => classify_game(
            ctx.engine,
            ctx.lang_detector,
            game_id,
            &name,
            &install_dir,
            entries,
            ctx.enabled_categories,
            ctx.cancel,
        ),
        None => scan_and_prepare_game(
            ctx.engine,
            ctx.lang_detector,
            game_id,
            &name,
            &install_dir,
            ctx.enabled_categories,
            ctx.cancel,
        ),
    };
    let outcome = match result {
        Ok(prepared) => GameOutcome::Scanned(prepared),
        Err(error) => GameOutcome::Failed {
            name,
            install_dir,
            error,
        },
    };
    let _ = result_tx.send(outcome);
}

/// One volume's worth of file lists, in the shape `mftscan::scan_roots`
/// reports them: one entry per game rooted on that volume, each holding
/// either the game's files or the error that stands in for them.
type VolumeResults = Vec<(i64, CoreResult<Vec<FileEntry>>)>;

/// Reads one volume's `$MFT` and returns what it found for each of that
/// volume's candidate roots. `Sync` because the rayon scope body that calls
/// it must be `Send`, and it is called through a shared reference.
type VolumeReader<'a> = dyn Fn(&[(i64, PathBuf)]) -> VolumeResults + Sync + 'a;

/// Runs the whole "get every game's files, classify them, hand them to the
/// writer" half of a scan, with the two things that used to be strictly
/// sequential now overlapped: this thread reads each eligible volume's
/// `$MFT` while the pool classifies everything already in hand.
///
/// Three groups of games, in the order they start:
///
/// 1. Games the MFT pass will never produce entries for - not elevated, no
///    drive letter, an SSD volume, a junction, a volume that would not open.
///    They need nothing from the pass, so they start before it does, where
///    they used to wait behind a pass they never used (33 of 1 603 games on
///    the reference library).
/// 2. Games whose volume has just finished being read. A volume's file
///    entries are complete the moment that volume is, so its games start
///    then, while the next volume streams.
/// 3. Games whose MFT result the pass rejected (see
///    `scan_route::finalize_mft_result`) - started at the same moment as
///    (2), but walked instead.
///
/// The reading itself deliberately stays on this one thread. That is what
/// keeps `scan_volume_catching_panics` correct (a `catch_unwind` only
/// catches panics unwinding through its own thread), it keeps two partitions
/// of one physical disk from fighting over one head, and it buys nothing
/// anyway: once the reading is hidden under the classification, halving it
/// changes no wall clock.
///
/// `read_volume` is a parameter rather than a direct call to
/// `mftscan::scan_roots` so the overlap can be tested without an NTFS
/// volume - see `walkdir_games_start_before_a_volume_has_been_read`.
fn dispatch_scans(
    ctx: &ClassifyContext<'_>,
    games: &[(i64, String, PathBuf)],
    plan: MftPlan,
    read_volume: &VolumeReader<'_>,
    result_tx: &SyncSender<GameOutcome>,
) -> MftPassOutcome {
    match rayon::ThreadPoolBuilder::new()
        .num_threads(scan_threads())
        .build()
    {
        Ok(pool) => pool.scope(|scope| {
            drive_scan(ctx, games, plan, read_volume, result_tx, &|task| {
                scope.spawn(move |_| run_one(ctx, task));
            })
        }),
        // A pool failing to build (extremely unlikely) must not lose the
        // scan entirely - fall back to running everything on this thread.
        // Nothing overlaps in that case: each game finishes before the next
        // volume is read, which is the behaviour this whole function used to
        // have, only slower. It is also why `read_finished` below is
        // measured rather than assumed to be an early instant.
        Err(_) => drive_scan(ctx, games, plan, read_volume, result_tx, &|task| {
            run_one(ctx, task)
        }),
    }
}

/// The body of [`dispatch_scans`], with "start this game's work" left to the
/// caller: on the pool that is `scope.spawn`, on the fallback path a direct
/// call. Split out only so those two share one description of what happens
/// in which order.
fn drive_scan(
    ctx: &ClassifyContext<'_>,
    games: &[(i64, String, PathBuf)],
    plan: MftPlan,
    read_volume: &VolumeReader<'_>,
    result_tx: &SyncSender<GameOutcome>,
    spawn: &dyn Fn(GameTask),
) -> MftPassOutcome {
    let MftPlan {
        candidates_by_volume,
        mut walkdir_reasons,
        volume_probes,
    } = plan;

    let by_id: HashMap<i64, (&str, &Path)> = games
        .iter()
        .map(|(game_id, name, install_dir)| (*game_id, (name.as_str(), install_dir.as_path())))
        .collect();
    let candidates: HashSet<i64> = candidates_by_volume
        .values()
        .flatten()
        .map(|(game_id, _)| *game_id)
        .collect();

    let start = |game_id: i64, name: &str, install_dir: &Path, entries: Option<Vec<FileEntry>>| {
        spawn(GameTask {
            game_id,
            name: name.to_string(),
            install_dir: install_dir.to_path_buf(),
            entries,
            result_tx: result_tx.clone(),
            notifier: ctx.notifier.clone(),
        });
    };

    // (1) Everything no volume owes a file list for starts immediately.
    for (game_id, name, install_dir) in games {
        if !candidates.contains(game_id) {
            start(*game_id, name, install_dir, None);
        }
    }

    // (2) and (3): one volume at a time, spawning as each one lands.
    let mut mft_count = 0usize;
    for roots in candidates_by_volume.into_values() {
        let mut reported: HashSet<i64> = HashSet::new();
        for (game_id, result) in read_volume(&roots) {
            // Ids come straight from `games`, so a miss here is structurally
            // impossible; skipping rather than indexing keeps it that way
            // without a panic if it ever stops being true.
            let Some((name, install_dir)) = by_id.get(&game_id).copied() else {
                continue;
            };
            reported.insert(game_id);
            let mft_ok = result.is_ok();
            let entries = result.unwrap_or_default();
            let entries_empty = entries.is_empty();
            let nonempty_on_disk = entries_empty && root_nonempty_on_disk(install_dir);

            match scan_route::finalize_mft_result(mft_ok, entries_empty, nonempty_on_disk) {
                ScanRoute::Mft => {
                    mft_count += 1;
                    start(game_id, name, install_dir, Some(entries));
                }
                // A candidate the pass itself rejected: it is walked, so it
                // belongs in the breakdown alongside the roots never tried.
                ScanRoute::Walkdir(reason) => {
                    walkdir_reasons.push((game_id, reason));
                    start(game_id, name, install_dir, None);
                }
            }
        }

        // A candidate the read said nothing at all about. `scan_roots` seeds
        // a slot per root and so should never do this, but the old shape
        // dispatched from `games` and was robust to it for free: a game
        // missing from the results simply got walked. Preserve that. Losing
        // a game here would cost its findings with nothing on screen or in
        // the log to say one went missing.
        for (game_id, _) in &roots {
            if reported.contains(game_id) {
                continue;
            }
            let Some((name, install_dir)) = by_id.get(game_id).copied() else {
                continue;
            };
            walkdir_reasons.push((*game_id, scan_route::WalkdirReason::MftFailed));
            start(*game_id, name, install_dir, None);
        }
    }

    MftPassOutcome {
        mft_count,
        walkdir_count: games.len() - mft_count,
        walkdir_reasons,
        volume_probes,
        read_finished: Instant::now(),
    }
}

/// What the MFT pass ended up doing, once every volume has been read: how
/// many games went each way (for the final status line - see
/// `scan_route::format_scan_summary`), why each walked root walked, and when
/// the reading finished.
///
/// No file entries: they are consumed as they are produced (see
/// [`GameTask`]) rather than collected into a map that has to outlive the
/// whole classification.
struct MftPassOutcome {
    mft_count: usize,
    walkdir_count: usize,
    /// Why each walked root was walked, one entry per root, paired with the
    /// game whose root it was. Kept rather than discarded so the settings
    /// dialog can tell a user who turned on "prefer the MFT index" and saw
    /// no speed-up what actually happened (see
    /// `scan_route::format_walkdir_breakdown`), and so the decision itself
    /// survives the process - the aggregate counter string it used to
    /// collapse into could say "3 roots walked, not elevated" but never
    /// *which* three.
    walkdir_reasons: Vec<(i64, scan_route::WalkdirReason)>,
    /// What the per-volume probe saw, one entry per volume actually
    /// checked. Bounded by the number of drive letters in play, unlike the
    /// per-root list.
    volume_probes: Vec<VolumeProbe>,
    /// When the last volume finished being read - the end of the `scan`
    /// phase, which no longer coincides with the start of `analyze`. Taken
    /// here rather than in `run_scan` because only this side knows when the
    /// reading stopped; the classification it overlaps with runs on past it.
    read_finished: Instant,
}

/// One volume's routing inputs, as observed rather than as decided.
///
/// `error` is the whole point: `is_available` used to collapse "not NTFS",
/// "blocked by an ACL", "held by a filter driver" and "no such volume" into
/// a single `false`, which is why "MFT fails on drive X" could not be
/// diagnosed from a bug report.
struct VolumeProbe {
    letter: char,
    /// `false` when the media reports a seek penalty (or is unknown), which
    /// is what keeps a root on the MFT path.
    ssd: bool,
    /// `None` when the volume opened, `Some(message)` when it did not.
    /// Never set for a volume skipped as SSD - nothing was attempted there.
    error: Option<String>,
}

/// Writes the MFT pass's routing decisions where they outlive the process.
///
/// Two shapes, because the two questions have different cardinalities. The
/// per-root route is a column on the game's own row - a library of a few
/// thousand games would otherwise add a few thousand diagnostic rows to
/// every scan. The per-volume probe is a `scan_diagnostics` row, bounded by
/// the number of drive letters in play, and it is the only place the real
/// Win32 open error is recorded at all.
///
/// Entirely non-fatal: this is evidence about a scan, and failing to write
/// it must never fail the scan it describes. Both failures degrade to a log
/// line, which is where the same information went before this existed.
fn record_routing_evidence(
    conn: &Connection,
    scan_id: i64,
    games: &[(i64, String, PathBuf)],
    mft_pass: &MftPassOutcome,
) {
    let walked: HashMap<i64, scan_route::WalkdirReason> =
        mft_pass.walkdir_reasons.iter().copied().collect();
    let routes: Vec<(i64, String)> = games
        .iter()
        .map(|(game_id, _, _)| {
            let route = match walked.get(game_id) {
                // The label is the machine-readable variant name, not the
                // localized one: this is read back by whoever receives a
                // report, and `format_walkdir_breakdown` still produces the
                // sentence the user sees.
                Some(reason) => format!("walkdir:{reason:?}"),
                None => "mft".to_string(),
            };
            (*game_id, route)
        })
        .collect();
    if let Err(err) = db::record_scan_routes(conn, &routes) {
        crate::logger::error(&format!("Failed to record scan routes: {err}"));
    }

    for probe in &mft_pass.volume_probes {
        let message = match (&probe.error, probe.ssd) {
            (Some(err), _) => format!("unavailable: {err}"),
            (None, true) => "no seek penalty (SSD/NVMe), routed to walkdir".to_string(),
            (None, false) => "available, seek penalty or unknown media".to_string(),
        };
        if let Err(err) = db::record_scan_diagnostic(
            conn,
            scan_id,
            "mftscan",
            "volume-probe",
            Some(Path::new(&format!("{}:", probe.letter))),
            &message,
        ) {
            crate::logger::error(&format!(
                "Failed to record the probe of volume {}: {err}",
                probe.letter
            ));
        }
    }
}

/// What the MFT routing decides before any volume is read: which candidate
/// roots each volume owes file lists for, which roots are already known to
/// need a walk, and what each volume's probe saw.
struct MftPlan {
    /// Candidates grouped by volume, so that a panic or error while reading
    /// one volume (see `scan_volume_catching_panics`) can never affect
    /// another volume's already-decided-good results - each volume gets its
    /// own `mftscan::scan_roots` call.
    candidates_by_volume: HashMap<char, Vec<(i64, PathBuf)>>,
    /// Roots routed to walkdir before the pass began. `drive_scan` appends
    /// the candidates the pass itself then rejects.
    walkdir_reasons: Vec<(i64, scan_route::WalkdirReason)>,
    volume_probes: Vec<VolumeProbe>,
}

/// Plans the MFT index pass: for every game whose install root is eligible
/// (see `scan_route::initial_route`), works out which NTFS volume's Master
/// File Table would supply its files instead of a directory walk. Ineligible
/// roots go straight into `walkdir_reasons` and never reach a volume.
///
/// Everything here is cheap and local - a canonicalization per root, a media
/// kind and an open probe per *volume*. The expensive part, reading each
/// volume's `$MFT`, is `drive_scan`'s, where it runs underneath the
/// classification instead of ahead of it.
///
/// Every game ends up in exactly one of the two buckets that make up
/// `mft_count + walkdir_count`, by construction: `walkdir_count` is derived
/// as `total - mft_count` rather than tracked incrementally, so the two
/// numbers can never drift apart even if a routing edge case is missed.
fn plan_mft_pass(elevated: bool, games: &[(i64, String, PathBuf)]) -> MftPlan {
    if !elevated {
        // The one early exit: no volume can be opened for raw MFT reads at
        // all, so every root walks for the same reason.
        let reason = scan_route::WalkdirReason::NotElevated;
        return MftPlan {
            candidates_by_volume: HashMap::new(),
            walkdir_reasons: games.iter().map(|(id, _, _)| (*id, reason)).collect(),
            volume_probes: Vec::new(),
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
    let mut volume_available: HashMap<char, bool> = HashMap::new();
    let mut volume_ssd: HashMap<char, bool> = HashMap::new();
    let mut volume_probes: Vec<VolumeProbe> = Vec::new();
    for letter in scan_route::volumes_to_check(elevated, &checks) {
        if scan_route::mft_worthwhile(mftscan::media_kind(letter)) {
            // `availability` rather than `is_available`: same probe, but the
            // error survives instead of collapsing into `false`.
            let probe = mftscan::availability(letter);
            volume_ssd.insert(letter, false);
            volume_available.insert(letter, probe.is_ok());
            volume_probes.push(VolumeProbe {
                letter,
                ssd: false,
                error: probe.err().map(|err| err.to_string()),
            });
        } else {
            volume_ssd.insert(letter, true);
            volume_probes.push(VolumeProbe {
                letter,
                ssd: true,
                error: None,
            });
        }
    }

    let mut candidates_by_volume: HashMap<char, Vec<(i64, PathBuf)>> = HashMap::new();
    let mut walkdir_reasons: Vec<(i64, scan_route::WalkdirReason)> = Vec::new();
    for check in &checks {
        if let ScanRoute::Walkdir(reason) =
            scan_route::initial_route(elevated, check, &volume_available, &volume_ssd)
        {
            walkdir_reasons.push((check.game_id, reason));
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

    MftPlan {
        candidates_by_volume,
        walkdir_reasons,
        volume_probes,
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
/// This is only safe to call from the thread that does the reading -
/// `catch_unwind` only catches a panic unwinding through the *current*
/// thread. That the reading now runs *concurrently* with classification
/// (see `dispatch_scans`) changes nothing here: it is still one thread
/// calling `scan_roots` for one volume after another, which is precisely why
/// the containment below is still a containment. If the *reading* were ever
/// parallelized (rayon, spawned threads), each parallel task would need its
/// own `catch_unwind` inside its own closure; a panic on another thread does
/// not unwind through this one, and (for rayon specifically) a panicked
/// task's `scope()` call re-raises the panic on the *joining* thread only
/// after every task in the scope has finished, not from inside a
/// `catch_unwind` wrapped around a single task.
fn scan_volume_catching_panics(
    scan_fn: impl FnOnce() -> CoreResult<VolumeResults>,
    game_ids: &[i64],
) -> VolumeResults {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(scan_fn)) {
        Ok(Ok(results)) => results,
        Ok(Err(err)) => volume_failure_results(game_ids, err.to_string()),
        Err(_) => {
            volume_failure_results(game_ids, "panic during the volume's MFT scan".to_string())
        }
    }
}

fn volume_failure_results(game_ids: &[i64], message: String) -> VolumeResults {
    game_ids
        .iter()
        .map(|&game_id| (game_id, Err(CoreError::Other(message.clone()))))
        .collect()
}

/// One file's finding, already resolved (rule engine vs. localization
/// detector) but not yet persisted - the `files.id` it will reference does
/// not exist until `store_files_no_tx` has run, so the finding carries
/// `entry_index` (below) and the writer resolves the id from that. Carrying
/// `size` here (rather than re-deriving it from `entries` at persist time by
/// `rel_path`) avoids an O(files x findings) rescan per game.
struct PreparedFinding {
    /// Index of this finding's file into [`PreparedGame::entries`].
    ///
    /// `store_files_no_tx` returns the inserted `files.id`s in entry order,
    /// so this is all the writer needs to attach the finding to its row -
    /// where it used to select every row of the game back out and match on
    /// `rel_path`. `classify_game` has the index in hand anyway (it is what
    /// `combined_by_index` is keyed by) and simply threw it away before.
    entry_index: usize,
    /// The path itself, still carried alongside the index: the UI row, the
    /// safety evidence written when a snapshot could not be captured, and
    /// the orphan/finding comparisons all want it, and at one clone per
    /// *finding* (720 k) rather than per file it is not the cost that
    /// mattered.
    rel_path: String,
    size: u64,
    size_on_disk: u64,
    source: FindingSource,
    rule_id: String,
    confidence: u8,
    provenance: RuleProvenance,
    lang_tag: Option<String>,
    /// Folder-grouping key for the UI tree; see [`assign_group_dirs`].
    /// Persisted to `findings.group_dir` by `persist_prepared_game` so a
    /// later startup load can read it straight back instead of recomputing
    /// it from the whole file list (the dominant cost of the old load path).
    group_dir: Option<String>,
    /// Scan-time deletion evidence, or the reason it could not be captured.
    ///
    /// Captured here, on the scan pool, rather than in the writer: it costs a
    /// handful of file opens per finding, and doing it inside the writer's
    /// transaction made one thread pay for every finding in the scan while
    /// holding the database lock. The writer only inserts what it is handed.
    safety: std::result::Result<SafetySnapshot, String>,
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
///
/// `cancel` is threaded down into `scan_dir_cancellable` so a Stop request
/// during a single huge game's walk (hundreds of thousands of files) takes
/// effect promptly instead of only being observed once the whole tree has
/// been enumerated. It is checked once more right after the walk returns,
/// before classification starts, so a game that finishes walking just as
/// Stop is pressed doesn't still pay the cost of `classify_game`.
// Same flat-list reasoning as `classify_game`, which this forwards to.
#[allow(clippy::too_many_arguments)]
fn scan_and_prepare_game(
    engine: &RuleEngine,
    lang_detector: &LangDetector,
    game_id: i64,
    name: &str,
    install_dir: &Path,
    enabled_categories: &[String],
    cancel: &AtomicBool,
) -> CoreResult<PreparedGame> {
    let entries = scan_dir_cancellable(install_dir, cancel)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(CoreError::Other("cancelled".to_string()));
    }
    classify_game(
        engine,
        lang_detector,
        game_id,
        name,
        install_dir,
        entries,
        enabled_categories,
        cancel,
    )
}

/// Classifies an already-obtained file list - from either `scan_dir`
/// (walkdir) or the MFT index pass - through both the rule engine and the
/// localization detector. Pure CPU work, no filesystem or database access,
/// so this is what actually runs in parallel across scan worker threads
/// regardless of which path supplied `entries`; only
/// [`persist_prepared_game`] needs a `Connection`.
///
/// `cancel` is polled inside both hot passes (the localization
/// `analyze_game_cancellable` and the per-file rule-engine loop); once it is
/// observed set this returns `Err(CoreError::Other("cancelled"))` promptly
/// instead of classifying the whole (possibly enormous) file list, so a Stop
/// during a big game's analysis is honored rather than swallowed. When
/// `cancel` is never set the result is exactly the non-cancellable one.
///
/// `enabled_categories` (the persisted `enabled_categories` setting - see
/// `gametrimmer_core::settings`) is applied right here, before a finding
/// even enters `combined_by_index`: a file whose category is disabled is
/// treated exactly as if no rule/localization cue had matched it at all.
/// This is the single choke point for the category filter - doing it this
/// early (rather than at persistence or display) means a disabled
/// category's files never affect folder-collapsing (`assign_group_dirs`)
/// either, and the database ends up holding exactly what the setting says
/// should be scanned, not a superset filtered later.
// Eight parameters: the game identity (id/name/dir), the two classifiers
// (rule engine + localization detector), the category filter, the file list,
// and the cancel token - each is a distinct, unrelated input with no natural
// grouping into a struct that would read more clearly than the flat list.
#[allow(clippy::too_many_arguments)]
fn classify_game(
    engine: &RuleEngine,
    lang_detector: &LangDetector,
    game_id: i64,
    name: &str,
    install_dir: &Path,
    entries: Vec<FileEntry>,
    enabled_categories: &[String],
    cancel: &AtomicBool,
) -> CoreResult<PreparedGame> {
    // `analyze_game` needs sibling context (the language-family heuristic),
    // so it runs once over all of this game's files rather than per-file.
    // The cancellable variant polls `cancel` inside its own hot loops, so a
    // Stop request lands promptly even mid-analysis of a huge game.
    let lang_findings: HashMap<usize, LangFinding> = lang_detector
        .analyze_game_cancellable(&entries, cancel)?
        .into_iter()
        .collect();

    // First pass: combine each entry's rule/localization findings, keeping
    // the entry's index into `entries` so `assign_group_dirs` (which needs
    // the full file list, not just the flagged ones) can be run afterwards.
    let rules_started = Instant::now();
    let mut combined_by_index: Vec<(usize, CombinedFinding)> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        // The rule-engine pass is per-file regex work; on a game with
        // hundreds of thousands of files it is long enough to be worth
        // interrupting too (the same cadence the core cancel path uses).
        if index % CLASSIFY_CANCEL_POLL_INTERVAL == 0 && cancel.load(Ordering::Relaxed) {
            return Err(CoreError::Other("cancelled".to_string()));
        }
        let rule_finding = engine.classify(&entry.rel_path);
        let lang_finding = lang_findings.get(&index);

        if let Some(combined) = combine_finding(rule_finding, lang_finding) {
            if category_enabled(enabled_categories, display_category(combined.source)) {
                combined_by_index.push((index, combined));
            }
        }
    }

    perf::add(perf::Stage::Rules, rules_started.elapsed());

    let flagged: HashSet<usize> = combined_by_index.iter().map(|(index, _)| *index).collect();
    let group_dirs = perf::timed(perf::Stage::Grouping, || {
        assign_group_dirs(&entries, &flagged)
    });

    // One cache per game: every finding here shares the same trusted root and
    // most of the same intermediate directories, which is exactly the
    // redundancy `SnapshotCapture` exists to remove.
    let safety_started = Instant::now();
    let mut capture = SnapshotCapture::new();
    let findings = combined_by_index
        .into_iter()
        .map(|(index, combined)| {
            let entry = &entries[index];
            let safety = capture
                .capture(install_dir, &entry.rel_path, entry.mft_identity.as_ref())
                .map_err(|reason| reason.to_string());
            PreparedFinding {
                entry_index: index,
                rel_path: entry.rel_path.clone(),
                size: entry.size,
                size_on_disk: entry.size_on_disk,
                source: combined.source,
                rule_id: combined.rule_id,
                confidence: combined.confidence,
                provenance: combined.provenance,
                lang_tag: combined.lang_tag,
                group_dir: group_dirs.get(&index).cloned(),
                safety,
            }
        })
        .collect();
    perf::add(perf::Stage::Safety, safety_started.elapsed());

    Ok(PreparedGame {
        game_id,
        name: name.to_string(),
        install_dir: install_dir.to_path_buf(),
        entries,
        findings,
    })
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
///
/// The directory chains are *borrowed* from each entry's `rel_path` wherever
/// possible (see [`dir_prefixes`]): every ancestor path is a prefix of the
/// file's own path, so counting them needs no allocation at all. Only the
/// handful of paths that survive as group keys are turned into `String`s, at
/// the end. Building them the other way - an owned `String` per directory
/// level per file - meant roughly 25 million allocations, and 25 million
/// owned-string hashes, per scan of a large library.
pub(crate) fn assign_group_dirs(
    entries: &[FileEntry],
    flagged: &HashSet<usize>,
) -> HashMap<usize, String> {
    // Directory path -> (total files under it, flagged files under it).
    let mut dir_stats: HashMap<Cow<'_, str>, (u32, u32)> = HashMap::new();

    for (index, entry) in entries.iter().enumerate() {
        let is_flagged = flagged.contains(&index);
        for dir in dir_prefixes(&entry.rel_path) {
            let stats = dir_stats.entry(dir).or_insert((0, 0));
            stats.0 += 1;
            if is_flagged {
                stats.1 += 1;
            }
        }
    }

    // The chains are recomputed here rather than kept from the loop above:
    // only the flagged files (a small fraction of a game's tree) need one,
    // and holding a chain per file was the other half of the old memory
    // cost.
    let mut group_dirs = HashMap::new();
    for &index in flagged {
        let Some(entry) = entries.get(index) else {
            continue;
        };
        // The chain is shallowest-first, so the first collapsible entry
        // found is the shallowest collapsible ancestor.
        let collapsible = dir_prefixes(&entry.rel_path).into_iter().find(|dir| {
            let (total, flagged_count) = dir_stats.get(dir).copied().unwrap_or((0, 0));
            total >= 2 && total == flagged_count
        });
        if let Some(dir) = collapsible {
            group_dirs.insert(index, dir.into_owned());
        }
    }

    group_dirs
}

/// The `\`-separated ancestor directory paths of `rel_path`, shallowest
/// first, excluding the (implicit, empty) game root and the file name
/// itself. E.g. `"a\b\c\file.txt"` -> `["a", "a\\b", "a\\b\\c"]`; a file
/// directly under the game root (no directory segments) yields an empty
/// list.
///
/// Borrowed where it can be, owned where it must be. Both producers of
/// `rel_path` - `scan_dir_cancellable`, which joins components with `\`, and
/// the MFT path (`mftscan::pathmap::scan_frn_map`), which does the same -
/// hand over paths that are already exactly `\`-separated with no empty
/// segments. For those, every ancestor is literally `&rel_path[..end]` at a
/// separator, so the whole chain costs nothing but the `Vec`.
///
/// A path that is *not* in that shape (a `/` separator, a leading or doubled
/// separator, a trailing one) has to be normalised, and a normalised prefix
/// is no longer a substring of the input - so those keep the original owned
/// build. This is a deliberate fallback rather than a simplification:
/// silently treating `a/b/c.txt` as one flat segment would change which
/// folders collapse in the UI tree, which is a behaviour change, not an
/// optimisation.
fn dir_prefixes(rel_path: &str) -> Vec<Cow<'_, str>> {
    if is_canonically_separated(rel_path) {
        return rel_path
            .match_indices('\\')
            .map(|(end, _)| Cow::Borrowed(&rel_path[..end]))
            .collect();
    }

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
        prefixes.push(Cow::Owned(acc.clone()));
    }
    prefixes
}

/// Whether `rel_path` is already in the shape [`dir_prefixes`] can slice
/// prefixes out of: `\` separators only, and no empty segment (no leading,
/// doubled, or trailing separator). Under those conditions splitting on `\`
/// and rejoining with `\` is the identity, so a prefix ending at any
/// separator is exactly the normalised ancestor path.
fn is_canonically_separated(rel_path: &str) -> bool {
    !rel_path.contains('/')
        && !rel_path.starts_with('\\')
        && !rel_path.ends_with('\\')
        && !rel_path.contains("\\\\")
}

/// Scans, classifies, and persists one game in its own single-game
/// transaction. A thin composition of [`scan_and_prepare_game`] and
/// [`persist_prepared_game`], kept as the entry point tests exercise
/// directly - `run_scan`'s real pipeline instead scans games in parallel and
/// batches several games per commit (see [`dispatch_scans`], [`run_writer`]).
///
/// `scan_id` names the generation being written, exactly as `run_scan` hands
/// it to the writer; tests that never call `db::begin_scan` pass `0`, which
/// is what `persist_libraries` stamped their `games` rows with.
#[cfg(test)]
fn scan_and_classify_game(
    conn: &mut Connection,
    engine: &RuleEngine,
    lang_detector: &LangDetector,
    game_id: i64,
    name: &str,
    install_dir: &Path,
    scan_id: i64,
) -> CoreResult<Vec<FindingRow>> {
    // Empty `enabled_categories` means "every category enabled" - the right
    // default for tests that aren't specifically exercising the filter.
    // Never cancelled - this helper exists for tests exercising the rest of
    // the scan+persist pipeline, not cancellation itself (see the dedicated
    // `scan_and_prepare_game_returns_cancelled_when_flag_pre_set` test below).
    let never_cancel = AtomicBool::new(false);
    let prepared = scan_and_prepare_game(
        engine,
        lang_detector,
        game_id,
        name,
        install_dir,
        &[],
        &never_cancel,
    )?;
    let db_tx = conn.transaction()?;
    let findings = persist_prepared_game(&db_tx, &prepared, scan_id)?;
    db_tx.commit()?;
    Ok(findings)
}

/// One file's finding after reconciling the rule engine and the localization
/// detector, ready to persist and display.
struct CombinedFinding {
    source: FindingSource,
    rule_id: String,
    confidence: u8,
    provenance: RuleProvenance,
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
///
/// `ui_lang` is what the reason is written in. It is resolved here, at scan
/// time, rather than when the row is drawn, because `rule_id` is persisted as
/// text: the same choice the orphan pass already makes. The cost is that
/// switching the interface language leaves already-scanned findings describing
/// themselves in the previous one until the next scan.
fn combine_finding(rule: Option<Finding>, lang: Option<&LangFinding>) -> Option<CombinedFinding> {
    match (rule, lang) {
        (Some(r), _) => Some(CombinedFinding {
            source: FindingSource::Rule(r.category),
            rule_id: r.rule_desc,
            confidence: r.confidence,
            provenance: r.provenance,
            lang_tag: None,
        }),
        (None, Some(l)) => Some(CombinedFinding {
            source: FindingSource::Loc(l.kind),
            rule_id: i18n::lang_reason(Lang::En, &l.reason),
            confidence: l.confidence,
            provenance: RuleProvenance::Builtin,
            lang_tag: Some(l.lang_tag.clone()),
        }),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh database reports `active_scan_id == Some(0)` - the legacy
    /// sentinel - and the first version of the environment line read that as
    /// "holds a previous generation", describing every baseline run as the
    /// opposite of what it was.
    #[test]
    fn a_fresh_database_is_reported_as_empty_not_as_a_previous_generation() {
        let conn = db::open_in_memory().expect("open in-memory db");

        assert_eq!(
            db::active_scan_id(&conn).expect("read active generation"),
            Some(0),
            "precondition: a new database carries the legacy sentinel",
        );
        assert_eq!(previous_generation(&conn), "empty");
    }

    /// The other half: a real generation is named, with the row count that
    /// predicts what superseding it will cost.
    #[test]
    fn an_activated_generation_is_reported_with_its_row_count() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let scan_id = db::begin_scan(&conn, "complete").expect("begin scan");
        db::activate_scan(&mut conn, scan_id).expect("activate scan");

        let reported = previous_generation(&conn);
        assert!(
            reported.contains(&format!("generation {scan_id}")),
            "{reported:?} does not name the generation",
        );
        assert!(reported.contains("0 file rows"), "{reported:?}");
    }

    /// GT-127. The whole point of routing reports through `Reported`: the
    /// log gets English whatever the interface language is, and the window
    /// gets the user's own. Before this, one rendering served both, so a
    /// Ukrainian install produced a log nobody could grep against the
    /// source.
    ///
    /// GT-115's log half lives here now, for the same reason - this is the
    /// last place the message exists in both languages.
    #[test]
    fn a_fatal_error_is_logged_in_english_and_shown_in_the_users_language() {
        let (tx, rx) = std::sync::mpsc::channel();
        let notifier = Notifier::new(tx, egui::Context::default());

        let contents = crate::logger::captured_for_test(|_dir| {
            notifier.report_error(i18n::Reported::new(Lang::Uk, |l| {
                i18n::libraries_write_failed(l, "gt_probe_disk_full")
            }));
        });

        assert!(
            contents.contains("[ERROR]") && contents.contains("Failed to write libraries"),
            "the log takes the English rendering, marked as a failure: {contents}",
        );
        assert!(
            !contents.contains("Помилка запису"),
            "the interface language must not reach the log: {contents}",
        );

        let WorkerMsg::Error { msg } = rx.try_recv().expect("the window is told too") else {
            panic!("report_error must send an Error message");
        };
        assert!(
            msg.contains("Помилка запису"),
            "the window keeps the user's language: {msg}",
        );
        // Both halves name the underlying cause - the split is the wording,
        // not the evidence.
        assert!(msg.contains("gt_probe_disk_full"), "{msg}");
    }
    use gametrimmer_core::db;
    use gametrimmer_core::langdetect::{LangDetector, LangEvidence, LangKind, LangReason};
    use gametrimmer_core::providers::GameInstall;
    use gametrimmer_core::rules::{Category, RuleEngine};
    use std::fs;

    fn lang_finding_de() -> LangFinding {
        LangFinding {
            lang_tag: "de".to_string(),
            kind: LangKind::Text,
            confidence: 90,
            reason: LangReason::new(LangEvidence::Family {
                languages: 3,
                dir: Some("Docs".to_string()),
            }),
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
            provenance: RuleProvenance::Builtin,
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
            r#"{"version":1,"rules":[{"category":"docs_file","pattern":".","desc":"test rule","confidence":50}]}"#,
        )
        .expect("valid test rules.json")
    }

    fn write_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write file");
    }

    /// A pre-set cancel flag must be honored even though the walk itself
    /// (a couple of small files) finishes instantly - mirrors the check
    /// `scan_and_prepare_game` runs right after `scan_dir_cancellable`
    /// returns, before `classify_game` starts.
    #[test]
    fn scan_and_prepare_game_returns_cancelled_when_flag_pre_set() {
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();

        let install_dir = tempfile::tempdir().expect("create temp install dir");
        write_file(&install_dir.path().join("a.txt"), b"a");
        write_file(&install_dir.path().join("b.txt"), b"b");

        let cancel = AtomicBool::new(true);

        let result = scan_and_prepare_game(
            &engine,
            &lang_detector,
            1,
            "Test Game",
            install_dir.path(),
            &[],
            &cancel,
        );

        match result {
            Ok(_) => panic!("a pre-cancelled scan must return Err"),
            Err(err) => assert!(
                err.to_string().contains("cancelled"),
                "error message should mention cancellation, got: {err}"
            ),
        }
    }

    /// `classify_game` itself must honor a pre-set cancel flag - this is the
    /// MFT branch's guarantee (it skips the walk and calls `classify_game`
    /// directly), and the reason the "Analysis" phase of a huge game (ARK) can
    /// now be stopped. With the flag already set, the first `collect_cancellable`
    /// checkpoint inside `analyze_game_cancellable` fires before any real work.
    #[test]
    fn classify_game_returns_cancelled_when_flag_pre_set() {
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();

        let entries = vec![
            FileEntry::logical_only("a.txt", 1, None),
            FileEntry::logical_only("b\\c.txt", 1, None),
        ];
        let cancel = AtomicBool::new(true);

        let result = classify_game(
            &engine,
            &lang_detector,
            1,
            "Test Game",
            Path::new("C:/Games/Test"),
            entries,
            &[],
            &cancel,
        );

        match result {
            Ok(_) => panic!("a pre-cancelled classify_game must return Err"),
            Err(err) => assert!(
                err.to_string().contains("cancelled"),
                "error message should mention cancellation, got: {err}"
            ),
        }
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
        let games = persist_libraries(conn, std::slice::from_ref(library), 0)?;
        for (game_id, name, install_dir) in &games {
            scan_and_classify_game(conn, engine, lang_detector, *game_id, name, install_dir, 0)?;
        }
        Ok(games)
    }

    /// End-to-end for `group_dir` persistence: a full scan cycle must write
    /// each finding's collapsing folder into `findings.group_dir`, so a later
    /// startup load reads it straight back instead of recomputing it. With
    /// `match_all_engine` flagging every file, a folder whose files are all
    /// flagged (`junk\`, two files) collapses to itself.
    #[test]
    fn scan_persists_group_dir_for_a_fully_flagged_folder() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();

        let install_dir = tempfile::tempdir().expect("create temp install dir");
        write_file(&install_dir.path().join("junk").join("a.txt"), b"a");
        write_file(&install_dir.path().join("junk").join("b.txt"), b"b");

        let library = DiscoveredLibrary {
            vendor: "test",
            path: PathBuf::from("C:/Games"),
            orphan_evidence: OrphanEvidence::Heuristic,
            games: vec![GameInstall {
                name: "Test Game".to_string(),
                install_dir: install_dir.path().to_path_buf(),
                app_id: None,
            }],
        };

        run_one_cycle(&mut conn, &engine, &lang_detector, &library)
            .expect("scan cycle should succeed");

        let group_dirs: Vec<Option<String>> = {
            let mut stmt = conn
                .prepare("SELECT group_dir FROM findings")
                .expect("prepare group_dir query");
            stmt.query_map([], |row| row.get::<_, Option<String>>(0))
                .expect("query group_dir")
                .collect::<rusqlite::Result<_>>()
                .expect("collect group_dir")
        };

        assert_eq!(group_dirs.len(), 2, "both junk files must be findings");
        assert!(
            group_dirs.iter().all(|dir| dir.as_deref() == Some("junk")),
            "every finding must persist group_dir = junk, got {group_dirs:?}"
        );
    }

    /// Library attribution has two producers - the fresh scan builds rows in
    /// memory, and `worker::load` rebuilds them from the database - and after
    /// a scan the load path is never called, so a green test on either one
    /// alone proves nothing about the other. This runs both over the same
    /// database and compares row by row: the two must name the same launcher
    /// and the same root, or grouping the tree by library shows one thing
    /// after a scan and another after a restart.
    #[test]
    fn scan_and_load_agree_on_a_game_row_library() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();

        let install_dir = tempfile::tempdir().expect("create temp install dir");
        write_file(&install_dir.path().join("readme.txt"), b"a");
        write_file(&install_dir.path().join("manual.pdf"), b"b");

        let library_root = PathBuf::from(r"D:\SteamLibrary");
        let library = DiscoveredLibrary {
            vendor: "steam",
            path: library_root.clone(),
            orphan_evidence: OrphanEvidence::Heuristic,
            games: vec![GameInstall {
                name: "Test Game".to_string(),
                install_dir: install_dir.path().to_path_buf(),
                app_id: None,
            }],
        };

        let games =
            persist_libraries(&conn, std::slice::from_ref(&library), 0).expect("persist library");
        let (game_id, name, path) = &games[0];
        let scanned =
            scan_and_classify_game(&mut conn, &engine, &lang_detector, *game_id, name, path, 0)
                .expect("scan game");

        let expected = Some(LibraryOrigin {
            vendor: Some("steam".to_string()),
            root: library_root,
        });
        assert_eq!(scanned.len(), 2, "both files must be findings");
        assert!(
            scanned.iter().all(|row| row.library == expected),
            "a fresh scan must attribute every row to the library it came from, got {:?}",
            scanned.iter().map(|row| &row.library).collect::<Vec<_>>()
        );

        let loaded = crate::worker::load::load_findings(&conn).expect("load should succeed");
        assert_eq!(loaded.len(), scanned.len());
        for row in &scanned {
            let mirrored = loaded
                .iter()
                .find(|candidate| candidate.file_id == row.file_id)
                .expect("every scanned row must come back from the load path");
            assert_eq!(
                mirrored.library, row.library,
                "load must reconstruct the same library the scan reported for file {}",
                row.file_id
            );
        }
    }

    /// GT-117. The counts came back from the insert loop and were dropped
    /// on the floor, so "which game made the scan slow" could not be
    /// answered after the fact - only re-measured by running it again.
    #[test]
    fn a_scanned_game_keeps_its_file_and_byte_counts() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();

        let install_dir = tempfile::tempdir().expect("create temp install dir");
        write_file(&install_dir.path().join("readme.txt"), b"abc");
        write_file(&install_dir.path().join("manual.pdf"), b"de");

        let library = DiscoveredLibrary {
            vendor: "steam",
            path: PathBuf::from(r"D:\SteamLibrary"),
            orphan_evidence: OrphanEvidence::Heuristic,
            games: vec![GameInstall {
                name: "Test Game".to_string(),
                install_dir: install_dir.path().to_path_buf(),
                app_id: None,
            }],
        };
        let games =
            persist_libraries(&conn, std::slice::from_ref(&library), 0).expect("persist library");
        let (game_id, name, path) = &games[0];
        scan_and_classify_game(&mut conn, &engine, &lang_detector, *game_id, name, path, 0)
            .expect("scan game");

        let (files, bytes): (i64, i64) = conn
            .query_row(
                "SELECT files, bytes FROM games WHERE id = ?1",
                [game_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read back the per-game stats");

        assert_eq!(files, 2);
        assert_eq!(bytes, 5, "3 + 2 bytes of content");
    }

    /// GT-116. The routing decision used to survive only as a localized
    /// counter string in the settings dialog: it could say "2 roots were
    /// walked, not elevated" but never which two, and not after a restart.
    #[test]
    fn the_route_each_root_took_survives_in_the_database() {
        let conn = db::open_in_memory().expect("open in-memory db");
        let library = DiscoveredLibrary {
            vendor: "steam",
            path: PathBuf::from(r"D:\SteamLibrary"),
            orphan_evidence: OrphanEvidence::Heuristic,
            games: vec![
                GameInstall {
                    name: "Walked".to_string(),
                    install_dir: PathBuf::from(r"D:\SteamLibrary\Walked"),
                    app_id: None,
                },
                GameInstall {
                    name: "Indexed".to_string(),
                    install_dir: PathBuf::from(r"D:\SteamLibrary\Indexed"),
                    app_id: None,
                },
            ],
        };
        let scan_id = db::begin_scan(&conn, "complete").expect("begin scan");
        let games = persist_libraries(&conn, std::slice::from_ref(&library), scan_id)
            .expect("persist library");
        let walked_id = games[0].0;

        record_routing_evidence(
            &conn,
            scan_id,
            &games,
            &MftPassOutcome {
                mft_count: 1,
                walkdir_count: 1,
                walkdir_reasons: vec![(walked_id, scan_route::WalkdirReason::SsdVolume)],
                volume_probes: vec![VolumeProbe {
                    letter: 'D',
                    ssd: false,
                    error: Some("cannot open \\\\.\\D: for raw MFT scan".to_string()),
                }],
                read_finished: Instant::now(),
            },
        );

        let mut routes: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare("SELECT name, scan_route FROM games ORDER BY name")
                .expect("prepare");
            let rows = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .expect("query");
            rows.collect::<rusqlite::Result<_>>().expect("collect")
        };
        routes.sort();
        assert_eq!(
            routes,
            vec![
                ("Indexed".to_string(), "mft".to_string()),
                ("Walked".to_string(), "walkdir:SsdVolume".to_string()),
            ],
        );

        // The volume's own error text is the half `is_available` used to
        // throw away, and the only thing that tells "not NTFS" from "blocked
        // by an ACL" in a report.
        let (path, message): (String, String) = conn
            .query_row(
                "SELECT path, message FROM scan_diagnostics
                 WHERE scan_id = ?1 AND stage = 'volume-probe'",
                [scan_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read back the volume probe");
        assert_eq!(path, "D:");
        assert!(message.contains("raw MFT scan"), "{message}");
    }

    /// The same agreement for the orphan path, which reaches the library by a
    /// different route on both sides (the recorded evidence root rather than a
    /// game's `library_id`) and so can drift independently of the game path.
    #[test]
    fn scan_and_load_agree_on_an_orphan_row_library() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let library_root = PathBuf::from(r"F:\SteamLibrary");
        let library = DiscoveredLibrary {
            vendor: "steam",
            path: library_root.clone(),
            orphan_evidence: OrphanEvidence::Authoritative,
            games: Vec::new(),
        };
        persist_libraries(&conn, std::slice::from_ref(&library), 0).expect("persist library");

        let orphans = vec![PreparedOrphan {
            full_path: library_root.join(r"steamapps\common\Leftover"),
            evidence_library_path: library_root.clone(),
            size: 10,
            size_on_disk: 4096,
            kind: OrphanKind::UnmanagedFolder,
        }];
        let scanned = persist_orphans(&mut conn, &orphans, 0).expect("persist should succeed");

        let expected = Some(LibraryOrigin {
            vendor: Some("steam".to_string()),
            root: library_root,
        });
        assert_eq!(scanned.len(), 1);
        assert_eq!(
            scanned[0].library, expected,
            "an orphan must name the library it was found in, not be left blank"
        );

        let loaded = crate::worker::load::load_findings(&conn).expect("load should succeed");
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].library, scanned[0].library,
            "load must reconstruct the same library the scan reported for the orphan"
        );
    }

    #[test]
    fn degraded_library_findings_are_read_only_before_they_reach_the_ui() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();
        let install_dir = tempfile::tempdir().expect("create temp install dir");
        write_file(&install_dir.path().join("target.bin"), b"data");
        let library = DiscoveredLibrary {
            vendor: "test",
            path: install_dir.path().to_path_buf(),
            orphan_evidence: OrphanEvidence::Degraded,
            games: vec![GameInstall {
                name: "Test Game".to_string(),
                install_dir: install_dir.path().to_path_buf(),
                app_id: None,
            }],
        };
        let scan_id = db::begin_scan(&conn, "degraded").expect("begin staging scan");
        db::record_scan_library_evidence(&conn, scan_id, &library.path, library.vendor, "degraded")
            .expect("record degraded evidence");
        let games = persist_libraries(&conn, std::slice::from_ref(&library), scan_id)
            .expect("persist library");
        let (game_id, name, path) = &games[0];

        let rows = scan_and_classify_game(
            &mut conn,
            &engine,
            &lang_detector,
            *game_id,
            name,
            path,
            scan_id,
        )
        .expect("scan game");

        assert!(!rows.is_empty());
        assert!(rows.iter().all(|row| {
            row.deletion_block_reason.as_deref() == Some("library discovery was degraded")
        }));
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
            vendor: "test",
            path: PathBuf::from("C:/Games"),
            orphan_evidence: OrphanEvidence::Heuristic,
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
        let conn = db::open_in_memory().expect("open in-memory db");

        let library = DiscoveredLibrary {
            vendor: "test",
            path: PathBuf::from("D:/SteamLibrary"),
            orphan_evidence: OrphanEvidence::Heuristic,
            games: vec![],
        };

        persist_libraries(&conn, std::slice::from_ref(&library), 0)
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

        let games = persist_libraries(&conn, std::slice::from_ref(&library), 0)
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
            vendor: "test",
            path: PathBuf::from("E:/Games"),
            orphan_evidence: OrphanEvidence::Heuristic,
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
            vendor: "test",
            path: PathBuf::from("E:/Games"),
            orphan_evidence: OrphanEvidence::Heuristic,
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
            vendor: "test",
            path: PathBuf::from("F:/OldDrive"),
            orphan_evidence: OrphanEvidence::Heuristic,
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
            vendor: "test",
            path: PathBuf::from("G:/NewDrive"),
            orphan_evidence: OrphanEvidence::Heuristic,
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

    fn vendor_of(conn: &Connection, path: &str) -> String {
        conn.query_row(
            "SELECT vendor FROM game_libraries WHERE path = ?1",
            params![path],
            |row| row.get(0),
        )
        .expect("library row should exist")
    }

    fn build_id_of(conn: &Connection, game_name: &str) -> Option<String> {
        conn.query_row(
            "SELECT build_id FROM games WHERE name = ?1",
            params![game_name],
            |row| row.get(0),
        )
        .expect("game row should exist")
    }

    /// Writes a Steam library root holding one `appmanifest_*.acf` for app
    /// 620 with the given build id, and returns the root.
    fn steam_library_with_build_id(root: &Path, build_id: &str) {
        let steamapps = root.join("steamapps");
        std::fs::create_dir_all(&steamapps).expect("create steamapps");
        write_file(
            &steamapps.join("appmanifest_620.acf"),
            format!(
                "\"AppState\"\n{{\n\t\"appid\"\t\t\"620\"\n\t\"name\"\t\t\"Portal 2\"\n\t\"buildid\"\t\t\"{build_id}\"\n}}\n"
            )
            .as_bytes(),
        );
    }

    /// build-ID history: record now, show later. Nothing in the UI reads `build_id` yet,
    /// but writing it during every scan is what lets a later release tell the
    /// user "these games came back" from their very first scan, instead of
    /// only after one more full scan.
    #[test]
    fn scan_records_the_steam_build_id_for_a_game() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();

        let library_root = tempfile::tempdir().expect("create temp library root");
        steam_library_with_build_id(library_root.path(), "17038203");
        let install_dir = library_root.path().join(r"steamapps\common\Portal 2");
        write_file(&install_dir.join("portal2.exe"), b"MZ");

        let library = DiscoveredLibrary {
            vendor: "steam",
            path: library_root.path().to_path_buf(),
            orphan_evidence: OrphanEvidence::Authoritative,
            games: vec![GameInstall {
                name: "Portal 2".to_string(),
                install_dir,
                app_id: Some("620".to_string()),
            }],
        };
        run_one_cycle(&mut conn, &engine, &lang_detector, &library)
            .expect("scan cycle should succeed");

        assert_eq!(build_id_of(&conn, "Portal 2").as_deref(), Some("17038203"));
    }

    /// Steam is the only vendor that publishes a content build id. Everyone
    /// else stores NULL, which `gamestate::changed_games` reads as "unknown,
    /// claim nothing" - never as "changed".
    #[test]
    fn scan_leaves_build_id_null_for_a_non_steam_vendor() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();

        let library_root = tempfile::tempdir().expect("create temp library root");
        // Even with a Steam-shaped manifest present, a GOG library must not
        // borrow it: the vendor decides, not the folder layout.
        steam_library_with_build_id(library_root.path(), "17038203");
        let install_dir = library_root.path().join("Fallout 2");
        write_file(&install_dir.join("fallout2.exe"), b"MZ");

        let library = DiscoveredLibrary {
            vendor: "gog",
            path: library_root.path().to_path_buf(),
            orphan_evidence: OrphanEvidence::Authoritative,
            games: vec![GameInstall {
                name: "Fallout 2".to_string(),
                install_dir,
                app_id: Some("620".to_string()),
            }],
        };
        run_one_cycle(&mut conn, &engine, &lang_detector, &library)
            .expect("scan cycle should succeed");

        assert_eq!(build_id_of(&conn, "Fallout 2"), None);
    }

    /// A Steam game the manifests say nothing about (manifest deleted, game
    /// discovered by folder scan, ...) stores NULL rather than a stale or
    /// borrowed id.
    #[test]
    fn scan_leaves_build_id_null_for_a_steam_game_without_a_manifest() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();

        let library_root = tempfile::tempdir().expect("create temp library root");
        steam_library_with_build_id(library_root.path(), "17038203");
        let install_dir = library_root.path().join(r"steamapps\common\Other Game");
        write_file(&install_dir.join("other.exe"), b"MZ");

        let library = DiscoveredLibrary {
            vendor: "steam",
            path: library_root.path().to_path_buf(),
            orphan_evidence: OrphanEvidence::Authoritative,
            games: vec![GameInstall {
                name: "Other Game".to_string(),
                install_dir,
                app_id: Some("999999".to_string()),
            }],
        };
        run_one_cycle(&mut conn, &engine, &lang_detector, &library)
            .expect("scan cycle should succeed");

        assert_eq!(build_id_of(&conn, "Other Game"), None);
    }

    /// The recorded id has to track the manifest, otherwise the very first
    /// comparison a later release makes would be against a value frozen at
    /// the first scan.
    #[test]
    fn rescanning_updates_the_recorded_build_id() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();

        let library_root = tempfile::tempdir().expect("create temp library root");
        steam_library_with_build_id(library_root.path(), "17038203");
        let install_dir = library_root.path().join(r"steamapps\common\Portal 2");
        write_file(&install_dir.join("portal2.exe"), b"MZ");

        let library = DiscoveredLibrary {
            vendor: "steam",
            path: library_root.path().to_path_buf(),
            orphan_evidence: OrphanEvidence::Authoritative,
            games: vec![GameInstall {
                name: "Portal 2".to_string(),
                install_dir,
                app_id: Some("620".to_string()),
            }],
        };
        run_one_cycle(&mut conn, &engine, &lang_detector, &library)
            .expect("first scan should succeed");
        assert_eq!(build_id_of(&conn, "Portal 2").as_deref(), Some("17038203"));

        // Valve ships an update: the manifest's build id moves.
        steam_library_with_build_id(library_root.path(), "17999999");
        run_one_cycle(&mut conn, &engine, &lang_detector, &library)
            .expect("second scan should succeed");

        assert_eq!(build_id_of(&conn, "Portal 2").as_deref(), Some("17999999"));
    }

    /// manual/discovered library reconciliation. A folder the user registered by hand is stored as `manual`; once
    /// a provider recognises that same folder as a real Steam library, the
    /// stored vendor must follow. Otherwise the library list keeps labelling
    /// it "manual", and every later scan re-enumerates it through the
    /// manual-library path (`WHERE vendor = 'manual'`) on top of the provider
    /// that already found it. See the upsert in `persist_libraries` for why
    /// this does *not* affect orphan detection.
    #[test]
    fn rescanning_upgrades_a_manual_library_to_its_real_vendor() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();

        let install_dir = tempfile::tempdir().expect("create temp install dir");
        write_file(&install_dir.path().join("readme.txt"), b"hi");

        // A real directory, not a hardcoded one: a literal like "F:/SteamLibrary"
        // happens to exist on some developer machines, so the test passed there
        // and failed on CI for reasons that had nothing to do with vendors.
        let library_root = tempfile::tempdir().expect("create temp library root");
        let library_path = library_root.path().to_string_lossy().to_string();

        // The user adds the folder by hand, before any provider knows it.
        manual::add_manual_library(&conn, library_root.path()).expect("manual add should succeed");
        assert_eq!(
            vendor_of(&conn, &library_path),
            manual::MANUAL_VENDOR,
            "precondition: a hand-added folder starts out as manual"
        );

        // A later scan: the Steam provider now discovers the same root.
        let discovered = DiscoveredLibrary {
            vendor: "steam",
            path: library_root.path().to_path_buf(),
            orphan_evidence: OrphanEvidence::Authoritative,
            games: vec![GameInstall {
                name: "Portal 2".to_string(),
                install_dir: install_dir.path().to_path_buf(),
                app_id: Some("620".to_string()),
            }],
        };
        run_one_cycle(&mut conn, &engine, &lang_detector, &discovered)
            .expect("scan cycle should succeed");

        assert_eq!(
            vendor_of(&conn, &library_path),
            "steam",
            "a rescan must record the most precise vendor known, not keep manual"
        );
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM game_libraries"),
            1,
            "the upgrade must update the existing row, not add a second one"
        );
    }

    /// The other direction of manual/discovered library reconciliation, and the reason the upgrade is not a
    /// plain unconditional overwrite: when a provider fails mid-scan (registry
    /// key missing, launcher config unreadable, drive briefly absent) its
    /// libraries are absent from the discovery results, and a folder the user
    /// also added by hand comes through tagged `manual`. Letting that
    /// overwrite a known `steam` would switch orphan detection off exactly
    /// when a scan half-failed. `manual` is a floor, never a destination.
    #[test]
    fn rescanning_never_downgrades_a_known_vendor_to_manual() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();

        let install_dir = tempfile::tempdir().expect("create temp install dir");
        write_file(&install_dir.path().join("readme.txt"), b"hi");

        let library_root = tempfile::tempdir().expect("create temp library root");
        let library_path = library_root.path().to_string_lossy().to_string();
        let as_steam = DiscoveredLibrary {
            vendor: "steam",
            path: library_root.path().to_path_buf(),
            orphan_evidence: OrphanEvidence::Authoritative,
            games: vec![GameInstall {
                name: "Portal 2".to_string(),
                install_dir: install_dir.path().to_path_buf(),
                app_id: Some("620".to_string()),
            }],
        };
        run_one_cycle(&mut conn, &engine, &lang_detector, &as_steam)
            .expect("initial scan should succeed");

        let as_manual = DiscoveredLibrary {
            vendor: manual::MANUAL_VENDOR,
            ..as_steam
        };
        run_one_cycle(&mut conn, &engine, &lang_detector, &as_manual)
            .expect("degraded scan should still succeed");

        assert_eq!(
            vendor_of(&conn, &library_path),
            "steam",
            "a scan where the provider dropped out must not demote the library"
        );
    }

    /// Build ids are a convenience: they let a later scan say "this game
    /// changed since last time". A Steam library whose manifests cannot be
    /// read loses that line and nothing else - the deletion path never
    /// consults it, and `gamestate::changed_games` already refuses to claim a
    /// change it cannot evidence. Propagating the read error instead took
    /// every other library's results down with it, which is exactly what
    /// per-library evidence exists to prevent.
    #[test]
    fn a_steam_library_with_unreadable_manifests_still_persists() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();

        let install_dir = tempfile::tempdir().expect("create temp install dir");
        write_file(&install_dir.path().join("readme.txt"), b"hi");

        // No `steamapps` under it, so reading manifest states fails.
        let library_root = tempfile::tempdir().expect("create temp library root");
        let library = DiscoveredLibrary {
            vendor: "steam",
            path: library_root.path().to_path_buf(),
            orphan_evidence: OrphanEvidence::Authoritative,
            games: vec![GameInstall {
                name: "Portal 2".to_string(),
                install_dir: install_dir.path().to_path_buf(),
                app_id: Some("620".to_string()),
            }],
        };

        let games = run_one_cycle(&mut conn, &engine, &lang_detector, &library)
            .expect("an unreadable manifest must not fail the scan");
        assert_eq!(games.len(), 1, "the game is still recorded");

        let build_id: Option<String> = conn
            .query_row(
                "SELECT build_id FROM games WHERE name = ?1",
                ["Portal 2"],
                |row| row.get(0),
            )
            .expect("read back the game");
        assert!(
            build_id.is_none(),
            "an unavailable build id is stored as unknown, not guessed: {build_id:?}"
        );
    }

    /// `classify_game`'s `enabled_categories` filter is the single choke
    /// point for the "scanned artifact categories" setting - a disabled
    /// category's files must never reach `combined_by_index` at all, so
    /// they neither show up in the returned findings nor influence
    /// `assign_group_dirs` folder-collapsing.
    #[test]
    fn classify_game_drops_findings_whose_category_is_disabled() {
        let engine = match_all_engine(); // every file classifies as docs_file
        let lang_detector = LangDetector::new();
        let entries = vec![entry("readme.txt"), entry("manual.pdf")];

        let never_cancel = AtomicBool::new(false);
        let prepared_all_enabled = classify_game(
            &engine,
            &lang_detector,
            1,
            "Test Game",
            Path::new("C:/Games/Test"),
            entries.clone(),
            &[], // empty = every category enabled
            &never_cancel,
        )
        .expect("uncancelled classify_game should succeed");
        assert_eq!(
            prepared_all_enabled.findings.len(),
            2,
            "with no categories disabled, both files should be classified"
        );

        let prepared_docs_disabled = classify_game(
            &engine,
            &lang_detector,
            1,
            "Test Game",
            Path::new("C:/Games/Test"),
            entries,
            &["redist".to_string()], // "docs" is not in the enabled list
            &never_cancel,
        )
        .expect("uncancelled classify_game should succeed");
        assert!(
            prepared_docs_disabled.findings.is_empty(),
            "disabling \"docs\" must drop every docs_file finding, not just filter it later"
        );
    }

    /// Sibling case: when the finding's category *is* in the enabled list,
    /// it must still come through unaffected.
    #[test]
    fn classify_game_keeps_findings_whose_category_is_enabled() {
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();
        let entries = vec![entry("readme.txt")];

        let prepared = classify_game(
            &engine,
            &lang_detector,
            1,
            "Test Game",
            Path::new("C:/Games/Test"),
            entries,
            &["docs".to_string()],
            &AtomicBool::new(false),
        )
        .expect("uncancelled classify_game should succeed");
        assert_eq!(prepared.findings.len(), 1);
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
            || -> CoreResult<VolumeResults> {
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
            || -> CoreResult<VolumeResults> {
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
                    Ok(vec![FileEntry::logical_only("a.txt", 1, None)]),
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

    /// The MFT path must never even be planned when not elevated - every
    /// game is a walkdir game with the same reason, and no volume is left
    /// owing a file list, so `drive_scan` starts all of them before it reads
    /// anything.
    #[test]
    fn planning_routes_everything_to_walkdir_when_not_elevated() {
        let games = vec![
            (1i64, "Game A".to_string(), PathBuf::from(r"G:\Games\A")),
            (2i64, "Game B".to_string(), PathBuf::from(r"D:\Games\B")),
        ];

        let plan = plan_mft_pass(false, &games);

        assert!(plan.candidates_by_volume.is_empty());
        assert!(plan.volume_probes.is_empty());
        assert_eq!(
            plan.walkdir_reasons,
            vec![
                (1i64, scan_route::WalkdirReason::NotElevated),
                (2i64, scan_route::WalkdirReason::NotElevated),
            ],
            "the diagnostics line has to name the reason and the root, not just the count",
        );
    }

    /// Games with no drive letter (e.g. a UNC path) must never be made a
    /// volume's candidate, even when elevated - there is no volume to probe.
    #[test]
    fn planning_routes_unc_paths_to_walkdir_even_when_elevated() {
        let games = vec![(
            1i64,
            "Networked Game".to_string(),
            PathBuf::from(r"\\server\share\Games\A"),
        )];

        let plan = plan_mft_pass(true, &games);

        assert!(plan.candidates_by_volume.is_empty());
        assert_eq!(
            plan.walkdir_reasons,
            vec![(1i64, scan_route::WalkdirReason::NoVolumeLetter)],
        );
    }

    /// The name of a game an outcome is about, whichever way it went.
    fn outcome_name(outcome: &GameOutcome) -> &str {
        match outcome {
            GameOutcome::Scanned(prepared) => &prepared.name,
            GameOutcome::Failed { name, .. } => name,
        }
    }

    /// A plan with one volume owing some games' file lists, and nothing else
    /// decided - the shape `drive_scan` overlaps against.
    fn plan_owing(volume: char, roots: Vec<(i64, PathBuf)>) -> MftPlan {
        MftPlan {
            candidates_by_volume: HashMap::from([(volume, roots)]),
            walkdir_reasons: Vec::new(),
            volume_probes: Vec::new(),
        }
    }

    /// The owned halves of a [`ClassifyContext`], in one binding, so the
    /// dispatch tests below can borrow a whole context out of it instead of
    /// each repeating six `let`s that say nothing about what they assert.
    struct Harness {
        engine: RuleEngine,
        lang_detector: LangDetector,
        cancel: AtomicBool,
        notifier: Notifier,
        completed: std::sync::atomic::AtomicUsize,
        /// Held only so the notifier's channel stays open; nothing reads it.
        _msg_rx: Receiver<WorkerMsg>,
    }

    impl Harness {
        fn new() -> Self {
            let (msg_tx, msg_rx) = std::sync::mpsc::channel();
            Self {
                engine: match_all_engine(),
                lang_detector: LangDetector::new(),
                cancel: AtomicBool::new(false),
                notifier: Notifier::new(msg_tx, egui::Context::default()),
                completed: std::sync::atomic::AtomicUsize::new(0),
                _msg_rx: msg_rx,
            }
        }

        /// Empty `enabled_categories` means "every category enabled".
        fn context(&self, total: usize) -> ClassifyContext<'_> {
            ClassifyContext {
                engine: &self.engine,
                lang_detector: &self.lang_detector,
                enabled_categories: &[],
                cancel: &self.cancel,
                notifier: &self.notifier,
                completed: &self.completed,
                total,
            }
        }
    }

    /// The point of the whole overlap: a game that needs nothing from the
    /// MFT pass must not wait for it. Asserted at the seam rather than with
    /// a sleep - the fake volume read *blocks until the walkdir game has
    /// already been classified*, so the only way this test passes is if that
    /// game was dispatched before the read began. Under the old sequential
    /// shape the reader would wait forever, which is what the timeout below
    /// reports; it is a failure detector, not a delay this test spends.
    #[test]
    fn walkdir_games_start_before_a_volume_has_been_read() {
        let walked_dir = tempfile::tempdir().expect("create temp install dir");
        write_file(&walked_dir.path().join("readme.txt"), b"a");

        let indexed_dir = PathBuf::from(r"D:\SteamLibrary\Indexed");
        let games = vec![
            (1i64, "Walked".to_string(), walked_dir.path().to_path_buf()),
            (2i64, "Indexed".to_string(), indexed_dir.clone()),
        ];
        let plan = plan_owing('D', vec![(2, indexed_dir)]);

        let harness = Harness::new();
        let ctx = harness.context(games.len());

        let (result_tx, result_rx) = std::sync::mpsc::sync_channel::<GameOutcome>(8);
        // The receiver is read from the reading thread (inside `drive_scan`)
        // and again from this one afterwards; a `Mutex` is what makes the
        // `&dyn Fn(..) + Sync` reader able to hold it at all.
        let result_rx = std::sync::Mutex::new(result_rx);
        let first_seen = std::sync::Mutex::new(None::<String>);

        let read_volume = |roots: &[(i64, PathBuf)]| {
            let outcome = result_rx
                .lock()
                .expect("the receiver lock is never poisoned")
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("a walkdir game must finish while the volume is still being read");
            *first_seen.lock().expect("lock") = Some(outcome_name(&outcome).to_string());
            roots
                .iter()
                .map(|(game_id, _)| (*game_id, Ok(vec![entry("data\\loc_de.pak")])))
                .collect()
        };

        let outcome = dispatch_scans(&ctx, &games, plan, &read_volume, &result_tx);
        drop(result_tx);

        assert_eq!(
            first_seen.lock().expect("lock").as_deref(),
            Some("Walked"),
            "the game that needs no file table has to be the one that finished first",
        );
        let indexed = result_rx
            .lock()
            .expect("lock")
            .recv()
            .expect("the indexed game reports in too");
        assert_eq!(outcome_name(&indexed), "Indexed");
        assert!(
            matches!(indexed, GameOutcome::Scanned(_)),
            "entries handed over by the volume read are classified, not walked",
        );
        assert_eq!(outcome.mft_count, 1);
        assert_eq!(outcome.walkdir_count, 1);
    }

    /// Stop pressed while the reading and the classifying are both live: the
    /// games a volume hands over after that point must not be classified at
    /// all. `run_one` checks the flag before it starts, and every task here
    /// is spawned after the fake read sets it, so this is a decision rather
    /// than a race.
    #[test]
    fn cancelling_while_a_volume_is_being_read_stops_the_games_it_hands_over() {
        let games = vec![
            (
                1i64,
                "First".to_string(),
                PathBuf::from(r"D:\SteamLibrary\First"),
            ),
            (
                2i64,
                "Second".to_string(),
                PathBuf::from(r"D:\SteamLibrary\Second"),
            ),
        ];
        let plan = plan_owing(
            'D',
            games
                .iter()
                .map(|(game_id, _, dir)| (*game_id, dir.clone()))
                .collect(),
        );

        let harness = Harness::new();
        let ctx = harness.context(games.len());

        let (result_tx, result_rx) = std::sync::mpsc::sync_channel::<GameOutcome>(8);
        let read_volume = |roots: &[(i64, PathBuf)]| {
            harness.cancel.store(true, Ordering::Relaxed);
            roots
                .iter()
                .map(|(game_id, _)| (*game_id, Ok(vec![entry("data\\loc_de.pak")])))
                .collect()
        };

        dispatch_scans(&ctx, &games, plan, &read_volume, &result_tx);
        drop(result_tx);

        let outcomes: Vec<GameOutcome> = result_rx.into_iter().collect();
        assert_eq!(outcomes.len(), 2, "every game still reports in");
        for outcome in &outcomes {
            match outcome {
                GameOutcome::Failed { error, .. } => assert_eq!(
                    error.to_string(),
                    "cancelled",
                    "a cancelled game travels as a cancellation, not as a failure",
                ),
                GameOutcome::Scanned(prepared) => {
                    panic!("{} was classified after Stop", prepared.name)
                }
            }
        }
    }

    /// A volume read that answers about nothing must not swallow the games
    /// it was asked about. `mftscan::scan_roots` seeds a slot per root, so
    /// this is a guard rather than an observed bug - but the old shape
    /// dispatched from the game list and so could not lose a game, and a
    /// game lost here would cost its findings with nothing on screen or in
    /// the log to say one went missing.
    #[test]
    fn a_candidate_the_volume_never_answers_about_is_walked_instead() {
        let install_dir = tempfile::tempdir().expect("create temp install dir");
        write_file(&install_dir.path().join("readme.txt"), b"a");

        let games = vec![(1i64, "Silent".to_string(), install_dir.path().to_path_buf())];
        let plan = plan_owing('D', vec![(1, install_dir.path().to_path_buf())]);

        let harness = Harness::new();
        let ctx = harness.context(games.len());

        let (result_tx, result_rx) = std::sync::mpsc::sync_channel::<GameOutcome>(8);
        let read_volume = |_roots: &[(i64, PathBuf)]| VolumeResults::new();

        let outcome = dispatch_scans(&ctx, &games, plan, &read_volume, &result_tx);
        drop(result_tx);

        let outcomes: Vec<GameOutcome> = result_rx.into_iter().collect();
        assert_eq!(outcomes.len(), 1, "the game still reports in");
        assert!(
            matches!(&outcomes[0], GameOutcome::Scanned(prepared) if prepared.name == "Silent"),
            "a candidate with no answer must be walked, not dropped",
        );
        assert_eq!(outcome.mft_count, 0);
        assert_eq!(outcome.walkdir_count, 1);
        assert_eq!(
            outcome.walkdir_reasons,
            vec![(1i64, scan_route::WalkdirReason::MftFailed)],
            "and the breakdown has to name it, like any other walked root",
        );
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
        FileEntry::logical_only(rel_path, 1, None)
    }

    /// localized rule descriptions. An English interface used to show Ukrainian sentences in its row
    /// tooltips and CSV export: the rule descriptions came straight out of
    /// `rules.json`, and the detector built its reasons with Ukrainian format
    /// strings. Both now go through a language, so this scans in English and
    /// insists nothing Cyrillic comes back.
    ///
    /// `rule_id` is the single field both surfaces read - `hover_reason` and
    /// the CSV's "Rule/reason" column each take it verbatim - so checking it
    /// here covers both without building a tree and a `FindingItem` list.
    #[test]
    fn an_english_scan_produces_no_cyrillic_reasons() {
        let engine = RuleEngine::from_json(gametrimmer_core::rules::BUILTIN_RULES_JSON)
            .expect("builtin rules compile");
        let entries = vec![
            // Claimed by a rule: the docs_file pattern.
            entry(r"Docs\manual.pdf"),
            // Claimed by the detector: a language family of four, of which
            // English is on the keep-list and the rest are flagged.
            entry(r"Voices\Voice_english.pak"),
            entry(r"Voices\Voice_french.pak"),
            entry(r"Voices\Voice_german.pak"),
            entry(r"Voices\Voice_spanish.pak"),
        ];

        let prepared = classify_game(
            &engine,
            &LangDetector::new(),
            1,
            "Test Game",
            Path::new("C:/Games/Test"),
            entries,
            &[],
            &AtomicBool::new(false),
        )
        .expect("classify_game should succeed");

        // Both paths have to be exercised, or the test could pass by finding
        // nothing at all.
        assert!(
            prepared
                .findings
                .iter()
                .any(|f| matches!(f.source, FindingSource::Rule(_))),
            "expected the docs rule to claim manual.pdf"
        );
        assert!(
            prepared
                .findings
                .iter()
                .any(|f| matches!(f.source, FindingSource::Loc(_))),
            "expected the voice family to be detected"
        );

        for finding in &prepared.findings {
            assert!(
                !finding.rule_id.chars().any(is_cyrillic),
                "an English scan produced a Cyrillic reason: {:?}",
                finding.rule_id
            );
        }
    }

    /// The inventory decision: `files` carries a row for a flagged file and
    /// for nothing else, while the game's own totals still describe the whole
    /// install.
    ///
    /// This is the shape that used to be inverted - a row per file of every
    /// game, 4.9 million against 720 thousand findings - kept solely so the
    /// rule-import preview could re-classify the inventory without a rescan.
    /// Both halves matter and they pull opposite ways, which is why they are
    /// asserted together: store less, but do not start under-reporting what a
    /// game occupies.
    #[test]
    fn only_flagged_files_get_a_row_while_the_game_totals_cover_everything() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        // The real engine, so "flagged" means what it means in production:
        // `manual.pdf` is documentation, `game.exe` and `data.bin` are not.
        let engine = RuleEngine::from_json(gametrimmer_core::rules::BUILTIN_RULES_JSON)
            .expect("builtin rules compile");
        let lang_detector = LangDetector::new();

        let install_dir = tempfile::tempdir().expect("create temp install dir");
        write_file(&install_dir.path().join("manual.pdf"), b"flagged");
        write_file(&install_dir.path().join("game.exe"), b"not flagged at all");
        write_file(&install_dir.path().join("data.bin"), b"nor this one");

        let library = DiscoveredLibrary {
            vendor: "steam",
            path: PathBuf::from(r"D:\SteamLibrary"),
            orphan_evidence: OrphanEvidence::Heuristic,
            games: vec![GameInstall {
                name: "Test Game".to_string(),
                install_dir: install_dir.path().to_path_buf(),
                app_id: None,
            }],
        };
        let games =
            persist_libraries(&conn, std::slice::from_ref(&library), 0).expect("persist library");
        let (game_id, name, path) = &games[0];

        let scanned =
            scan_and_classify_game(&mut conn, &engine, &lang_detector, *game_id, name, path, 0)
                .expect("scan game");
        assert_eq!(scanned.len(), 1, "only manual.pdf should be a finding");

        let stored: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT rel_path FROM files WHERE game_id = ?1 ORDER BY rel_path")
                .expect("prepare");
            stmt.query_map([game_id], |row| row.get(0))
                .expect("query")
                .collect::<rusqlite::Result<_>>()
                .expect("collect")
        };
        assert_eq!(
            stored,
            vec!["manual.pdf".to_string()],
            "the unflagged files must not get a row"
        );

        let (files, bytes): (i64, i64) = conn
            .query_row(
                "SELECT files, bytes FROM games WHERE id = ?1",
                [game_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read game totals");
        assert_eq!(files, 3, "the game's totals must count every file it has");
        assert_eq!(
            bytes,
            (b"flagged".len() + b"not flagged at all".len() + b"nor this one".len()) as i64,
            "occupancy must not shrink to just the flagged bytes"
        );
    }

    /// GT-129, and the inverse of what this test used to assert.
    ///
    /// It used to demand that a Ukrainian scan produce Ukrainian
    /// descriptions - true of the window, but the same text is what
    /// `findings.rule_id` stores and what the diagnostic bundle ships to
    /// whoever is diagnosing the report. Storage is English now whatever the
    /// interface language is; the translation happens on the way to the
    /// screen (`worker::descriptions`), which the tests there cover.
    #[test]
    fn a_ukrainian_scan_still_stores_its_descriptions_in_english() {
        // Built the way `run_scan` builds it now - unconditionally English,
        // with no interface language reaching the engine at all.
        let engine = RuleEngine::from_json(gametrimmer_core::rules::BUILTIN_RULES_JSON)
            .expect("builtin rules compile");
        let entries = vec![
            entry(r"Docs\manual.pdf"),
            entry(r"Voices\Voice_english.pak"),
            entry(r"Voices\Voice_french.pak"),
            entry(r"Voices\Voice_german.pak"),
            entry(r"Voices\Voice_spanish.pak"),
        ];

        let prepared = classify_game(
            &engine,
            &LangDetector::new(),
            1,
            "Test Game",
            Path::new("C:/Games/Test"),
            entries,
            &[],
            &AtomicBool::new(false),
        )
        .expect("classify_game should succeed");

        for source in [
            FindingSource::Rule(Category::DocsFile),
            FindingSource::Loc(LangKind::Unknown),
        ] {
            let found = prepared
                .findings
                .iter()
                .find(|f| std::mem::discriminant(&f.source) == std::mem::discriminant(&source));
            let found = found.unwrap_or_else(|| panic!("no finding for {source:?}"));
            assert!(
                !found.rule_id.chars().any(is_cyrillic),
                "what reaches the database must be English for {source:?}, got {:?}",
                found.rule_id
            );
        }
    }

    fn is_cyrillic(ch: char) -> bool {
        ('\u{0400}'..='\u{04FF}').contains(&ch)
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

    /// The chains are sliced straight out of `rel_path` when it is already
    /// `\`-separated, which no path either scan producer emits can violate -
    /// but a `/`, a doubled separator or a leading one would make a borrowed
    /// prefix mean something different from the normalised ancestor. Those
    /// take the owned path, and must still normalise exactly as before.
    #[test]
    fn dir_prefixes_normalizes_separators_it_cannot_slice_through() {
        assert_eq!(
            dir_prefixes("a/b/c/file.txt"),
            vec!["a".to_string(), r"a\b".to_string(), r"a\b\c".to_string()],
            "forward slashes must normalize to the same chain as backslashes"
        );
        assert_eq!(
            dir_prefixes(r"a\\b\file.txt"),
            vec!["a".to_string(), r"a\b".to_string()],
            "a doubled separator is one separator, not an empty directory"
        );
        assert_eq!(
            dir_prefixes(r"\a\file.txt"),
            vec!["a".to_string()],
            "a leading separator does not create a nameless root directory"
        );
    }

    /// The grouping itself must not care which separator wrote the path:
    /// two files in the same folder, spelled differently, still collapse
    /// into one group.
    #[test]
    fn assign_group_dirs_groups_across_mixed_separators() {
        let entries = vec![entry("junk/a.txt"), entry(r"junk\b.txt")];
        let flagged: HashSet<usize> = [0, 1].into_iter().collect();

        let groups = assign_group_dirs(&entries, &flagged);

        assert_eq!(groups.get(&0), Some(&"junk".to_string()));
        assert_eq!(groups.get(&1), Some(&"junk".to_string()));
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

    // -- orphan-residue safety: orphaned residue pass --

    /// Builds a Steam library on disk with a live game, a leftover folder, and
    /// an aborted-download service folder, returning the temp dir plus the
    /// `DiscoveredLibrary` describing only the live game (the manifest set).
    fn steam_library_with_a_leftover() -> (tempfile::TempDir, DiscoveredLibrary) {
        let dir = tempfile::tempdir().expect("create temp library");
        let root = dir.path();
        let common = root.join("steamapps").join("common");

        // A live game (its install_dir goes into the managed set).
        let live = common.join("Live Game");
        write_file(&live.join("game.exe"), b"payload");

        // A leftover folder with two files (total 7 bytes) - the orphan.
        let leftover = common.join("Leftover Game");
        write_file(&leftover.join("a.bin"), b"aaaa"); // 4 bytes
        write_file(&leftover.join("sub").join("b.bin"), b"bbb"); // 3 bytes

        // An aborted-download service folder (steamapps/downloading).
        write_file(
            &root.join("steamapps").join("downloading").join("state.tmp"),
            b"xy",
        );

        let library = DiscoveredLibrary {
            vendor: "steam",
            path: root.to_path_buf(),
            orphan_evidence: OrphanEvidence::Authoritative,
            games: vec![GameInstall {
                name: "Live Game".to_string(),
                install_dir: live,
                app_id: Some("1".to_string()),
            }],
        };
        (dir, library)
    }

    #[test]
    fn collect_orphans_flags_leftovers_and_service_folder_but_not_a_live_game() {
        let (_dir, library) = steam_library_with_a_leftover();
        let cancel = AtomicBool::new(false);

        let collection = collect_orphans(std::slice::from_ref(&library), &cancel);
        let orphans = collection.orphans;
        assert!(collection.issues.is_empty());

        let leftover = library
            .path
            .join("steamapps")
            .join("common")
            .join("Leftover Game");
        let downloading = library.path.join("steamapps").join("downloading");
        let live = library
            .path
            .join("steamapps")
            .join("common")
            .join("Live Game");

        let unmanaged = orphans
            .iter()
            .find(|o| o.full_path == leftover)
            .expect("the leftover folder must be detected");
        assert_eq!(unmanaged.kind, OrphanKind::UnmanagedFolder);
        assert_eq!(
            unmanaged.size, 7,
            "the leftover's size is the sum of its files (4 + 3)"
        );

        assert!(
            orphans
                .iter()
                .any(|o| o.full_path == downloading && o.kind == OrphanKind::ServiceFolder),
            "the downloading/ service folder must be detected"
        );
        assert!(
            !orphans.iter().any(|o| o.full_path == live),
            "a game still installed via the launcher must never be flagged"
        );
    }

    #[test]
    fn collect_orphans_ignores_registry_based_libraries() {
        let (_dir, mut library) = steam_library_with_a_leftover();
        // Same folders on disk, but discovered under a registry-based vendor
        // whose `library.path` is an arbitrary user folder, not a launcher-
        // exclusive container - it has no orphan spec, so nothing is reported.
        library.vendor = "epic";
        let cancel = AtomicBool::new(false);

        assert!(
            collect_orphans(std::slice::from_ref(&library), &cancel)
                .orphans
                .is_empty(),
            "registry-based providers have no exclusive container and are skipped"
        );
    }

    #[test]
    fn collect_orphans_flags_xbox_leftover_in_xboxgames_root() {
        let dir = tempfile::tempdir().expect("create temp xbox root");
        // For Xbox, `library.path` IS the XboxGames container (games are its
        // immediate subfolders), so no `steamapps/common` nesting here.
        let root = dir.path().join("XboxGames");
        let live = root.join("Starfield");
        write_file(&live.join("Content").join("game.exe"), b"payload");
        let leftover = root.join("UninstalledTitle");
        write_file(&leftover.join("leftover.bin"), b"abcd"); // 4 bytes

        let library = DiscoveredLibrary {
            vendor: "xbox",
            path: root.clone(),
            orphan_evidence: OrphanEvidence::Authoritative,
            games: vec![GameInstall {
                name: "Starfield".to_string(),
                install_dir: live.clone(),
                app_id: None,
            }],
        };
        let cancel = AtomicBool::new(false);

        let collection = collect_orphans(std::slice::from_ref(&library), &cancel);
        let orphans = collection.orphans;
        assert!(collection.issues.is_empty());

        assert!(
            orphans
                .iter()
                .any(|o| o.full_path == leftover && o.kind == OrphanKind::UnmanagedFolder),
            "an XboxGames folder with no live game must be flagged"
        );
        assert!(
            !orphans.iter().any(|o| o.full_path == live),
            "a live Xbox game must never be flagged"
        );
    }

    #[test]
    fn collect_orphans_flags_itch_leftover_only_when_it_carries_a_receipt() {
        let dir = tempfile::tempdir().expect("create temp itch location");
        // For itch, `library.path` IS the install location (parent of each
        // cave), which may be shared - so the `.itch` receipt is the guard.
        let location = dir.path().join("itch");
        let live = location.join("celeste");
        write_file(&live.join(".itch").join("receipt.json.gz"), b"r");
        write_file(&live.join("game.exe"), b"payload");

        let leftover = location.join("old-jam");
        write_file(&leftover.join(".itch").join("receipt.json.gz"), b"r");
        write_file(&leftover.join("data.bin"), b"abcd"); // 4 bytes

        // A foreign/manual game in the same shared location, no itch receipt.
        let foreign = location.join("ManualRepack");
        write_file(&foreign.join("repack.exe"), b"nope");

        let library = DiscoveredLibrary {
            vendor: "itch",
            path: location.clone(),
            orphan_evidence: OrphanEvidence::Authoritative,
            games: vec![GameInstall {
                name: "Celeste".to_string(),
                install_dir: live.clone(),
                app_id: Some("123".to_string()),
            }],
        };
        let cancel = AtomicBool::new(false);

        let collection = collect_orphans(std::slice::from_ref(&library), &cancel);
        let orphans = collection.orphans;
        assert!(collection.issues.is_empty());

        assert!(
            orphans
                .iter()
                .any(|o| o.full_path == leftover && o.kind == OrphanKind::UnmanagedFolder),
            "a receipt-bearing itch folder with no live cave must be flagged"
        );
        assert!(
            !orphans.iter().any(|o| o.full_path == foreign),
            "a foreign folder without an itch receipt must never be flagged, \
             even in a shared location"
        );
        assert!(
            !orphans.iter().any(|o| o.full_path == live),
            "a live itch game must never be flagged"
        );
    }

    #[test]
    fn collect_orphans_stops_early_when_cancelled() {
        let (_dir, library) = steam_library_with_a_leftover();
        let cancel = AtomicBool::new(true);

        assert!(
            collect_orphans(std::slice::from_ref(&library), &cancel)
                .orphans
                .is_empty(),
            "a pre-cancelled orphan pass must produce nothing"
        );
    }

    #[test]
    fn orphan_enumeration_error_creates_no_finding_and_degrades_the_library() {
        let dir = tempfile::tempdir().expect("create temp root");
        let root = dir.path().join("XboxGames");
        std::fs::write(&root, b"not a directory").expect("create invalid container");
        let library = DiscoveredLibrary {
            vendor: "xbox",
            path: root.clone(),
            orphan_evidence: OrphanEvidence::Authoritative,
            games: Vec::new(),
        };

        let collection = collect_orphans(&[library], &AtomicBool::new(false));

        assert!(collection.orphans.is_empty());
        assert_eq!(collection.issues.len(), 1);
        assert_eq!(collection.issues[0].stage, "orphan-enumeration");
        assert_eq!(collection.issues[0].library_path, root);
    }

    #[test]
    fn persist_orphans_writes_null_game_rows_that_load_back_as_the_orphan_branch() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        let full_path = PathBuf::from(r"F:\SteamLibrary\steamapps\common\Leftover");
        let orphans = vec![PreparedOrphan {
            full_path: full_path.clone(),
            evidence_library_path: PathBuf::from(r"F:\SteamLibrary"),
            size: 3000,
            // Deliberately distinct from the logical size: many small files
            // round up to more clusters on disk (allocated-size accounting).
            size_on_disk: 4096,
            kind: OrphanKind::UnmanagedFolder,
        }];

        let rows = persist_orphans(&mut conn, &orphans, 0).expect("persist should succeed");

        // The returned row is shaped for the UI: sentinel game id, empty name,
        // container as install_dir, folder name as rel_path.
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].game_id, ORPHAN_GAME_ID);
        assert_eq!(rows[0].size, 3000, "logical size preserved");
        assert_eq!(
            rows[0].size_on_disk, 4096,
            "on-disk allocation is the primary orphan size (allocated-size accounting)"
        );
        assert!(rows[0].game_name.is_empty());
        assert_eq!(
            rows[0].install_dir,
            PathBuf::from(r"F:\SteamLibrary\steamapps\common")
        );
        assert_eq!(rows[0].rel_path, "Leftover");
        assert_eq!(rows[0].install_dir.join(&rows[0].rel_path), full_path);
        assert_eq!(
            rows[0].source,
            FindingSource::Orphan(OrphanKind::UnmanagedFolder)
        );

        // The persisted file row has a NULL game_id (there is no game).
        let game_id: Option<i64> = conn
            .query_row("SELECT game_id FROM files", [], |row| row.get(0))
            .expect("read the single file row");
        assert_eq!(
            game_id, None,
            "orphan files must be stored with game_id NULL"
        );

        // And it round-trips through the real load path as the orphan branch.
        let loaded = crate::worker::load::load_findings(&conn).expect("load should succeed");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].game_id, ORPHAN_GAME_ID);
        assert_eq!(
            loaded[0].size_on_disk, 4096,
            "the stored on-disk size must survive the load round-trip"
        );
        assert_eq!(loaded[0].install_dir.join(&loaded[0].rel_path), full_path);
        assert_eq!(
            loaded[0].source,
            FindingSource::Orphan(OrphanKind::UnmanagedFolder)
        );
    }

    #[test]
    fn persist_orphans_replaces_the_previous_orphan_set_each_call() {
        let mut conn = db::open_in_memory().expect("open in-memory db");

        let first = vec![PreparedOrphan {
            full_path: PathBuf::from(r"F:\lib\steamapps\common\OldLeftover"),
            evidence_library_path: PathBuf::from(r"F:\lib"),
            size: 10,
            size_on_disk: 10,
            kind: OrphanKind::UnmanagedFolder,
        }];
        persist_orphans(&mut conn, &first, 0).expect("first persist");

        // A later scan finds a different leftover; the old one is gone.
        let second = vec![PreparedOrphan {
            full_path: PathBuf::from(r"F:\lib\steamapps\common\NewLeftover"),
            evidence_library_path: PathBuf::from(r"F:\lib"),
            size: 20,
            size_on_disk: 20,
            kind: OrphanKind::UnmanagedFolder,
        }];
        persist_orphans(&mut conn, &second, 0).expect("second persist");

        let paths: Vec<String> = {
            let mut stmt = conn.prepare("SELECT rel_path FROM files").expect("prepare");
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .expect("query")
                .collect::<rusqlite::Result<_>>()
                .expect("collect");
            rows
        };
        assert_eq!(
            paths,
            vec![r"F:\lib\steamapps\common\NewLeftover".to_string()],
            "each persist replaces the whole NULL-game orphan set"
        );
    }

    #[test]
    fn persist_orphans_with_empty_list_clears_stale_orphan_rows() {
        let mut conn = db::open_in_memory().expect("open in-memory db");

        let existing = vec![PreparedOrphan {
            full_path: PathBuf::from(r"F:\lib\steamapps\common\Leftover"),
            evidence_library_path: PathBuf::from(r"F:\lib"),
            size: 10,
            size_on_disk: 10,
            kind: OrphanKind::UnmanagedFolder,
        }];
        persist_orphans(&mut conn, &existing, 0).expect("seed orphan rows");

        // The empty-list call is how a disabled category clears residue.
        let rows = persist_orphans(&mut conn, &[], 0).expect("clear should succeed");
        assert!(rows.is_empty());

        let file_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE game_id IS NULL",
                [],
                |r| r.get(0),
            )
            .expect("count NULL-game files");
        assert_eq!(
            file_count, 0,
            "an empty persist must wipe stale orphan rows"
        );
    }

    #[test]
    fn persist_orphans_leaves_real_game_files_untouched() {
        let mut conn = db::open_in_memory().expect("open in-memory db");
        // A normal game file (non-NULL game_id) must survive the orphan wipe.
        conn.execute(
            "INSERT INTO game_libraries (id, vendor, path) VALUES (1, 'steam', 'F:/lib')",
            [],
        )
        .expect("insert library");
        conn.execute(
            "INSERT INTO games (id, library_id, name, install_dir, app_id) \
             VALUES (1, 1, 'Game', 'F:/lib/Game', NULL)",
            [],
        )
        .expect("insert game");
        conn.execute(
            "INSERT INTO files (game_id, rel_path, size, mtime) VALUES (1, 'game.exe', 100, NULL)",
            [],
        )
        .expect("insert game file");

        persist_orphans(
            &mut conn,
            &[PreparedOrphan {
                full_path: PathBuf::from(r"F:\lib\steamapps\common\Leftover"),
                evidence_library_path: PathBuf::from(r"F:\lib"),
                size: 10,
                size_on_disk: 10,
                kind: OrphanKind::UnmanagedFolder,
            }],
            0,
        )
        .expect("persist");

        let game_files: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE game_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .expect("count game files");
        assert_eq!(game_files, 1, "a real game's files must not be wiped");
    }
}
