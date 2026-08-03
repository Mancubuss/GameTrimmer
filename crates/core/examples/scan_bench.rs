//! Read-only benchmark for the walkdir-based scan pipeline (discover -> scan
//! -> classify -> store) - the permanent fallback path used whenever no MFT
//! index is available. Measures the current sequential implementation
//! against the parallel-scan + batched-write pipeline landed in
//! `crates/app/src/worker/scan.rs`, one optimization at a time.
//!
//! Run with (PowerShell, `$env:Path` already containing cargo):
//!   cargo run -p gametrimmer-core --release --example scan_bench -- \
//!       --dir H:\SteamLibrary --mode full
//!
//! Modes (each is a data point in the before/after report):
//!   baseline - sequential game loop; `files` replaced in one transaction
//!              per game (as `store_files` always did), but every `findings`
//!              row is its own autocommit transaction; `synchronous=FULL`.
//!              This is the pipeline exactly as it shipped before this pass.
//!   parallel - same DB-write pattern as `baseline` (still one autocommit
//!              per finding row, still `synchronous=FULL`), but games are
//!              scanned+classified on a bounded rayon pool (`--threads`).
//!              Isolates the parallelization gain alone.
//!   pragma   - same sequential, single-threaded game loop as `baseline`,
//!              but `files`+`findings` writes for a batch of `--batch` games
//!              share one transaction, and `synchronous=NORMAL` with a
//!              tuned `cache_size` (see `db::configure`). Isolates the
//!              SQLite-tuning gain alone.
//!   full     - parallel scan+classify AND batched/tuned writes together -
//!              the pipeline `worker::scan::run_scan` now runs.
//!
//! Note: `full` here measures each phase (scan, classify, db) with a clean
//! boundary, run one after another, for a legible breakdown. The real
//! `run_scan` pipeline additionally overlaps scanning/classifying later
//! games with writing earlier ones through a channel (see
//! `worker::scan::dispatch_scans`/`run_writer`), so real wall-clock speedup
//! in the app is somewhat better than `full`'s numbers alone suggest.
//!
//! Read-only on the scanned drive: only walks directories and reads file
//! metadata (via `scan_dir`). All database writes go to a fresh `tempfile`
//! database, deleted when the process exits - never to `gametrimmer.db`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use gametrimmer_core::db;
use gametrimmer_core::langdetect::{LangDetector, LangFinding, LangKind};
use gametrimmer_core::providers::steam::SteamProvider;
use gametrimmer_core::providers::LibraryProvider;
use gametrimmer_core::rules::{Category, Finding, RuleEngine};
use gametrimmer_core::scanner::{
    scan_dir, scan_games_bounded, store_files, store_files_no_tx, FileEntry,
};
use rayon::prelude::*;
use rusqlite::{params, Connection};

const DEFAULT_LIBRARY: &str = r"H:\SteamLibrary";
const DEFAULT_THREADS: usize = 6;
const DEFAULT_BATCH: usize = 24;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Baseline,
    Parallel,
    Pragma,
    Full,
}

impl Mode {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "baseline" => Some(Mode::Baseline),
            "parallel" => Some(Mode::Parallel),
            "pragma" => Some(Mode::Pragma),
            "full" => Some(Mode::Full),
            _ => None,
        }
    }

    fn is_parallel(self) -> bool {
        matches!(self, Mode::Parallel | Mode::Full)
    }

    fn is_batched(self) -> bool {
        matches!(self, Mode::Pragma | Mode::Full)
    }

    fn label(self) -> &'static str {
        match self {
            Mode::Baseline => "baseline",
            Mode::Parallel => "parallel",
            Mode::Pragma => "pragma",
            Mode::Full => "full",
        }
    }
}

struct Args {
    dir: PathBuf,
    mode: Mode,
    threads: usize,
    batch: usize,
}

fn parse_args() -> Args {
    let mut dir = PathBuf::from(DEFAULT_LIBRARY);
    let mut mode = Mode::Baseline;
    let mut threads = DEFAULT_THREADS;
    let mut batch = DEFAULT_BATCH;

    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--dir" => {
                if let Some(v) = raw.get(i + 1) {
                    dir = PathBuf::from(v);
                    i += 1;
                }
            }
            "--mode" => {
                if let Some(v) = raw.get(i + 1) {
                    mode = Mode::parse(v).unwrap_or_else(|| {
                        eprintln!(
                            "Невідомий режим \"{v}\", доступні: baseline, parallel, pragma, full"
                        );
                        std::process::exit(1);
                    });
                    i += 1;
                }
            }
            "--threads" => {
                if let Some(v) = raw.get(i + 1) {
                    threads = v.parse().unwrap_or(DEFAULT_THREADS);
                    i += 1;
                }
            }
            "--batch" => {
                if let Some(v) = raw.get(i + 1) {
                    batch = v.parse().unwrap_or(DEFAULT_BATCH);
                    i += 1;
                }
            }
            other => eprintln!("Невідомий аргумент, ігнорується: {other}"),
        }
        i += 1;
    }

    Args {
        dir,
        mode,
        threads,
        batch,
    }
}

/// One resolved, un-classified game ready to scan.
struct GameRef {
    game_id: i64,
    name: String,
    install_dir: PathBuf,
}

/// One game's raw scan result, before classification.
struct ScannedGame {
    game_id: i64,
    entries: Vec<FileEntry>,
}

/// One finding ready to persist, mirroring
/// `worker::scan::PreparedFinding` (duplicated here rather than shared,
/// since this example lives in `gametrimmer-core` and the app's combine
/// logic is app-crate-private - the DB-shape is what matters for the
/// benchmark, not sharing the exact type).
struct PreparedFinding {
    rel_path: String,
    size: u64,
    category: &'static str,
    rule_desc: String,
    confidence: u8,
    lang_tag: Option<String>,
}

struct PreparedGame {
    game_id: i64,
    entries: Vec<FileEntry>,
    findings: Vec<PreparedFinding>,
}

#[derive(Default, Clone, Copy)]
struct Timings {
    discover: Duration,
    scan: Duration,
    classify: Duration,
    db: Duration,
}

fn main() {
    let args = parse_args();
    println!(
        "scan_bench: mode={} dir={} threads={} batch={}",
        args.mode.label(),
        args.dir.display(),
        args.threads,
        args.batch
    );

    let rules_path = repo_rules_path();
    let engine = match RuleEngine::load(&rules_path) {
        Ok(engine) => engine,
        Err(err) => {
            eprintln!("Не вдалося завантажити {}: {err}", rules_path.display());
            std::process::exit(1);
        }
    };
    let lang_detector = LangDetector::new();

    let mut timings = Timings::default();

    // --- discover -----------------------------------------------------
    let t0 = Instant::now();
    let libraries = match SteamProvider.discover() {
        Ok(libraries) => libraries,
        Err(err) => {
            eprintln!("Помилка пошуку бібліотек Steam: {err}");
            std::process::exit(1);
        }
    };
    let Some(library) = libraries
        .iter()
        .find(|lib| paths_equal(&lib.path, &args.dir))
    else {
        eprintln!(
            "Бібліотеку {} не знайдено серед виявлених:",
            args.dir.display()
        );
        for lib in &libraries {
            eprintln!("  - {}", lib.path.display());
        }
        std::process::exit(1);
    };
    timings.discover = t0.elapsed();
    println!(
        "Знайдено {} ігор(и) у {} (discover: {:.3}s)",
        library.games.len(),
        library.path.display(),
        timings.discover.as_secs_f64()
    );

    // --- temp DB + persist games (folded into the "db" bucket) --------
    let tmp_dir = tempfile::tempdir().expect("create temp dir for bench db");
    let db_path = tmp_dir.path().join("scan_bench.db");
    let mut conn = db::open(&db_path).expect("open temp bench database (never gametrimmer.db)");
    if !args.mode.is_batched() {
        // `db::open` now always tunes these; revert to the pre-optimization
        // defaults so `baseline`/`parallel` measure the pipeline as it
        // shipped before this pass.
        conn.pragma_update(None, "synchronous", "FULL")
            .expect("reset synchronous=FULL for baseline measurement");
        conn.pragma_update(None, "cache_size", -2_000i64)
            .expect("reset cache_size to SQLite's default for baseline measurement");
    }

    let t_persist = Instant::now();
    let games = persist_games(
        &mut conn,
        library
            .games
            .iter()
            .map(|g| (g.name.as_str(), g.install_dir.as_path())),
    )
    .expect("persist discovered games into the temp database");
    timings.db += t_persist.elapsed();

    // --- scan -----------------------------------------------------------
    let t1 = Instant::now();
    let scanned = if args.mode.is_parallel() {
        scan_parallel(&games, args.threads)
    } else {
        scan_sequential(&games)
    };
    timings.scan = t1.elapsed();
    let total_files: usize = scanned.iter().map(|g| g.entries.len()).sum();
    println!(
        "Сканування: {} файл(ів) у {} грі(ах) за {:.3}s",
        total_files,
        scanned.len(),
        timings.scan.as_secs_f64()
    );

    // --- classify ---------------------------------------------------------
    let t2 = Instant::now();
    let prepared: Vec<PreparedGame> = if args.mode.is_parallel() {
        classify_parallel(&scanned, &engine, &lang_detector, args.threads)
    } else {
        classify_sequential(&scanned, &engine, &lang_detector)
    };
    timings.classify = t2.elapsed();
    let total_findings: usize = prepared.iter().map(|g| g.findings.len()).sum();
    let flagged_bytes: u64 = prepared
        .iter()
        .flat_map(|g| g.findings.iter())
        .map(|f| f.size)
        .sum();
    println!(
        "Класифікація: {total_findings} знахідок ({} МБ) за {:.3}s",
        flagged_bytes / (1024 * 1024),
        timings.classify.as_secs_f64()
    );

    // --- write to DB ------------------------------------------------------
    let t3 = Instant::now();
    if args.mode.is_batched() {
        write_batched(&mut conn, &prepared, args.batch);
    } else {
        write_unbatched(&mut conn, &prepared);
    }
    timings.db += t3.elapsed();

    let total_wall = timings.discover + timings.scan + timings.classify + timings.db;
    let files_per_sec = total_files as f64 / total_wall.as_secs_f64().max(0.000_001);

    println!("\n=== {} ===", args.mode.label());
    println!("  discover : {:>7.3}s", timings.discover.as_secs_f64());
    println!("  scan     : {:>7.3}s", timings.scan.as_secs_f64());
    println!("  classify : {:>7.3}s", timings.classify.as_secs_f64());
    println!("  db       : {:>7.3}s", timings.db.as_secs_f64());
    println!("  ---------------------");
    println!("  total    : {:>7.3}s", total_wall.as_secs_f64());
    println!("  files/s  : {files_per_sec:.0}");
    println!("  files={total_files} findings={total_findings} (sanity check across modes)");
}

fn persist_games<'a>(
    conn: &mut Connection,
    games: impl Iterator<Item = (&'a str, &'a Path)>,
) -> rusqlite::Result<Vec<GameRef>> {
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT OR IGNORE INTO game_libraries (vendor, path) VALUES ('steam', 'scan_bench')",
        [],
    )?;
    let library_id: i64 = tx.query_row(
        "SELECT id FROM game_libraries WHERE path = 'scan_bench'",
        [],
        |row| row.get(0),
    )?;

    let mut out = Vec::new();
    for (name, install_dir) in games {
        tx.execute(
            "INSERT INTO games (library_id, name, install_dir) VALUES (?1, ?2, ?3)",
            params![library_id, name, install_dir.to_string_lossy()],
        )?;
        out.push(GameRef {
            game_id: tx.last_insert_rowid(),
            name: name.to_string(),
            install_dir: install_dir.to_path_buf(),
        });
    }
    tx.commit()?;
    Ok(out)
}

fn scan_sequential(games: &[GameRef]) -> Vec<ScannedGame> {
    games
        .iter()
        .filter_map(|g| match scan_dir(&g.install_dir) {
            Ok(entries) => Some(ScannedGame {
                game_id: g.game_id,
                entries,
            }),
            Err(err) => {
                eprintln!("Помилка сканування \"{}\": {err}", g.name);
                None
            }
        })
        .collect()
}

fn scan_parallel(games: &[GameRef], threads: usize) -> Vec<ScannedGame> {
    let dirs: Vec<(i64, PathBuf)> = games
        .iter()
        .map(|g| (g.game_id, g.install_dir.clone()))
        .collect();
    let cancel = AtomicBool::new(false);
    let results = scan_games_bounded(&dirs, threads, &cancel);

    let by_id: HashMap<i64, &str> = games.iter().map(|g| (g.game_id, g.name.as_str())).collect();

    results
        .into_iter()
        .filter_map(|(game_id, res)| match res {
            Ok(entries) => Some(ScannedGame { game_id, entries }),
            Err(err) => {
                let name = by_id.get(&game_id).copied().unwrap_or("?");
                eprintln!("Помилка сканування \"{name}\" (#{game_id}): {err}");
                None
            }
        })
        .collect()
}

fn classify_one(
    game: &ScannedGame,
    engine: &RuleEngine,
    lang_detector: &LangDetector,
) -> PreparedGame {
    let lang_findings: HashMap<usize, LangFinding> = lang_detector
        .analyze_game(&game.entries)
        .into_iter()
        .collect();

    let mut findings = Vec::with_capacity(game.entries.len());
    for (index, entry) in game.entries.iter().enumerate() {
        let rule_finding = engine.classify(&entry.rel_path);
        let lang_finding = lang_findings.get(&index);
        let Some((category, rule_desc, confidence, lang_tag)) = combine(rule_finding, lang_finding)
        else {
            continue;
        };
        findings.push(PreparedFinding {
            rel_path: entry.rel_path.clone(),
            size: entry.size,
            category,
            rule_desc,
            confidence,
            lang_tag,
        });
    }

    PreparedGame {
        game_id: game.game_id,
        entries: game.entries.clone(),
        findings,
    }
}

fn classify_sequential(
    scanned: &[ScannedGame],
    engine: &RuleEngine,
    lang_detector: &LangDetector,
) -> Vec<PreparedGame> {
    scanned
        .iter()
        .map(|g| classify_one(g, engine, lang_detector))
        .collect()
}

fn classify_parallel(
    scanned: &[ScannedGame],
    engine: &RuleEngine,
    lang_detector: &LangDetector,
    threads: usize,
) -> Vec<PreparedGame> {
    match rayon::ThreadPoolBuilder::new()
        .num_threads(threads.max(1))
        .build()
    {
        Ok(pool) => pool.install(|| {
            scanned
                .par_iter()
                .map(|g| classify_one(g, engine, lang_detector))
                .collect()
        }),
        Err(_) => classify_sequential(scanned, engine, lang_detector),
    }
}

/// Mirrors `worker::scan::combine_finding`: keeps whichever of the rule
/// engine / localization detector produced the higher-confidence finding for
/// this file, favoring the rule engine on a tie.
fn combine(
    rule: Option<Finding>,
    lang: Option<&LangFinding>,
) -> Option<(&'static str, String, u8, Option<String>)> {
    match (rule, lang) {
        (Some(r), Some(l)) if l.confidence > r.confidence => Some((
            lang_category_key(l.kind),
            l.reason.to_string(),
            l.confidence,
            Some(l.lang_tag.clone()),
        )),
        (Some(r), _) => Some((
            rule_category_key(r.category),
            r.rule_desc,
            r.confidence,
            None,
        )),
        (None, Some(l)) => Some((
            lang_category_key(l.kind),
            l.reason.to_string(),
            l.confidence,
            Some(l.lang_tag.clone()),
        )),
        (None, None) => None,
    }
}

fn rule_category_key(category: Category) -> &'static str {
    match category {
        Category::RedistFolder => "redist_folder",
        Category::RedistFile => "redist_file",
        Category::DocsFolder => "docs_folder",
        Category::DocsFile => "docs_file",
        Category::Bonus => "bonus",
        Category::DevLeftovers => "dev_leftovers",
    }
}

fn lang_category_key(kind: LangKind) -> &'static str {
    match kind {
        LangKind::Audio => "loc_audio",
        LangKind::Text => "loc_text",
        LangKind::Video => "loc_video",
        LangKind::Font => "loc_font",
        LangKind::Graphic => "loc_graphic",
        LangKind::Unknown => "loc_unknown",
    }
}

/// Reproduces the pipeline exactly as it shipped before this pass: `files`
/// gets its own commit (`store_files`), and every `findings` row is inserted
/// with no wrapping transaction at all - i.e. one autocommit per row. This
/// is the dominant cost the benchmark is meant to expose.
fn write_unbatched(conn: &mut Connection, games: &[PreparedGame]) {
    for game in games {
        store_files(conn, game.game_id, &game.entries).expect("store_files (own transaction)");

        let file_ids = load_file_ids(conn, game.game_id).expect("load file ids back");
        for finding in &game.findings {
            let Some(&file_id) = file_ids.get(&finding.rel_path) else {
                continue;
            };
            conn.execute(
                "INSERT INTO findings (file_id, category, rule_id, confidence, lang_tag) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![file_id, finding.category, finding.rule_desc, finding.confidence, finding.lang_tag.as_deref()],
            )
            .expect("insert finding (autocommit)");
        }
    }
}

/// Batches `batch_size` games' `files`+`findings` writes into one shared
/// transaction, mirroring `worker::scan::run_writer`/`flush_batch`.
fn write_batched(conn: &mut Connection, games: &[PreparedGame], batch_size: usize) {
    for chunk in games.chunks(batch_size.max(1)) {
        let tx = conn.transaction().expect("begin batch transaction");
        for game in chunk {
            store_files_no_tx(&tx, game.game_id, &game.entries).expect("store_files_no_tx");

            let file_ids = load_file_ids(&tx, game.game_id).expect("load file ids back");
            let mut insert_finding = tx
                .prepare_cached(
                    "INSERT INTO findings (file_id, category, rule_id, confidence, lang_tag) VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .expect("prepare finding insert");
            for finding in &game.findings {
                let Some(&file_id) = file_ids.get(&finding.rel_path) else {
                    continue;
                };
                insert_finding
                    .execute(params![
                        file_id,
                        finding.category,
                        finding.rule_desc,
                        finding.confidence,
                        finding.lang_tag.as_deref()
                    ])
                    .expect("insert finding (batched)");
            }
        }
        tx.commit().expect("commit batch transaction");
    }
}

fn load_file_ids(conn: &Connection, game_id: i64) -> rusqlite::Result<HashMap<String, i64>> {
    let mut stmt = conn.prepare("SELECT id, rel_path FROM files WHERE game_id = ?1")?;
    let rows = stmt.query_map(params![game_id], |row| {
        Ok((row.get::<_, String>(1)?, row.get::<_, i64>(0)?))
    })?;
    rows.collect()
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

fn repo_rules_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("rules.json")
}
