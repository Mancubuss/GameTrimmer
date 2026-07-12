//! The "Сканувати бібліотеки" job: discover Steam libraries, persist them,
//! then scan and classify every game's files. Runs entirely on a
//! background thread; the only database connection used here is opened
//! and dropped within this thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;

use gametrimmer_core::db;
use gametrimmer_core::error::Result as CoreResult;
use gametrimmer_core::langdetect::{LangDetector, LangFinding};
use gametrimmer_core::providers::steam::SteamProvider;
use gametrimmer_core::providers::{DiscoveredLibrary, LibraryProvider};
use gametrimmer_core::rules::{Finding, RuleEngine};
use gametrimmer_core::scanner::{scan_dir, store_files};
use rusqlite::{params, Connection};

use crate::model::{category_key, DisplayCategory, FindingRow};

use super::{resolve_rules_path, WorkerMsg};

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

    let libraries = match SteamProvider.discover() {
        Ok(libraries) => libraries,
        Err(err) => {
            send_error(tx, format!("Помилка пошуку бібліотек Steam: {err}"));
            return;
        }
    };

    if libraries.is_empty() {
        send_error(tx, "Steam-бібліотек не знайдено.".to_string());
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
    let mut findings = Vec::new();

    for (index, (game_id, name, install_dir)) in games.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            let _ = tx.send(WorkerMsg::Cancelled);
            return;
        }

        let _ = tx.send(WorkerMsg::Progress {
            current: index + 1,
            total,
            game_name: name.clone(),
        });

        match scan_and_classify_game(
            &mut conn,
            &engine,
            &lang_detector,
            game_id,
            &name,
            &install_dir,
        ) {
            Ok(mut game_findings) => findings.append(&mut game_findings),
            Err(err) => {
                // A single game failing to scan (permissions, moved folder, ...)
                // must not abort the whole run.
                eprintln!(
                    "Помилка сканування \"{name}\" ({}): {err}",
                    install_dir.display()
                );
            }
        }
    }

    let _ = tx.send(WorkerMsg::Done { findings });
}

fn send_error(tx: &Sender<WorkerMsg>, msg: String) {
    let _ = tx.send(WorkerMsg::Error { msg });
}

/// Writes discovered libraries and their games into the database,
/// replacing each library's game list (`INSERT OR IGNORE` on the library
/// itself, keyed by path; full delete+reinsert of its games).
fn persist_libraries(
    conn: &mut Connection,
    libraries: &[DiscoveredLibrary],
) -> CoreResult<Vec<(i64, String, PathBuf)>> {
    let mut games = Vec::new();

    for library in libraries {
        let path_str = library.path.to_string_lossy().to_string();

        conn.execute(
            "INSERT OR IGNORE INTO game_libraries (vendor, path) VALUES (?1, ?2)",
            params![library.vendor, path_str],
        )?;
        let library_id: i64 = conn.query_row(
            "SELECT id FROM game_libraries WHERE path = ?1",
            params![path_str],
            |row| row.get(0),
        )?;

        conn.execute(
            "DELETE FROM games WHERE library_id = ?1",
            params![library_id],
        )?;

        for game in &library.games {
            conn.execute(
                "INSERT INTO games (library_id, name, install_dir, app_id) VALUES (?1, ?2, ?3, ?4)",
                params![
                    library_id,
                    game.name,
                    game.install_dir.to_string_lossy(),
                    game.app_id
                ],
            )?;
            games.push((
                conn.last_insert_rowid(),
                game.name.clone(),
                game.install_dir.clone(),
            ));
        }
    }

    Ok(games)
}

/// Scans one game's install directory, replaces its indexed files, and
/// classifies each file through both the rule engine and the localization
/// detector, persisting the winning finding per file and returning them for
/// the UI.
fn scan_and_classify_game(
    conn: &mut Connection,
    engine: &RuleEngine,
    lang_detector: &LangDetector,
    game_id: i64,
    name: &str,
    install_dir: &Path,
) -> CoreResult<Vec<FindingRow>> {
    let entries = scan_dir(install_dir)?;

    // `findings.file_id` has no `ON DELETE CASCADE`, and `store_files` is
    // about to delete this game's old `files` rows - drop their findings
    // first, while the old ids are still known.
    conn.execute(
        "DELETE FROM findings WHERE file_id IN (SELECT id FROM files WHERE game_id = ?1)",
        params![game_id],
    )?;

    store_files(conn, game_id, &entries)?;

    let file_ids: HashMap<String, i64> = {
        let mut stmt = conn.prepare("SELECT id, rel_path FROM files WHERE game_id = ?1")?;
        let rows = stmt.query_map(params![game_id], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(0)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    // `analyze_game` needs sibling context (the language-family heuristic),
    // so it runs once over all of this game's files rather than per-file.
    let lang_findings: HashMap<usize, LangFinding> =
        lang_detector.analyze_game(&entries).into_iter().collect();

    let mut findings = Vec::with_capacity(entries.len());
    {
        let mut insert_finding = conn.prepare_cached(
            "INSERT INTO findings (file_id, category, rule_id, confidence, lang_tag) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;

        for (index, entry) in entries.iter().enumerate() {
            let Some(&file_id) = file_ids.get(&entry.rel_path) else {
                continue;
            };

            let rule_finding = engine.classify(&entry.rel_path);
            let lang_finding = lang_findings.get(&index);

            let Some(combined) = combine_finding(rule_finding, lang_finding) else {
                continue;
            };

            insert_finding.execute(params![
                file_id,
                category_key(combined.category),
                combined.rule_id,
                combined.confidence,
                combined.lang_tag.as_deref(),
            ])?;

            findings.push(FindingRow {
                file_id,
                game_id,
                game_name: name.to_string(),
                install_dir: install_dir.to_path_buf(),
                rel_path: entry.rel_path.clone(),
                size: entry.size,
                category: combined.category,
                rule_desc: combined.rule_id,
                confidence: combined.confidence,
                lang_tag: combined.lang_tag,
            });
        }
    }

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
