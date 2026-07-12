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
use gametrimmer_core::providers::{self, DiscoveredLibrary};
use gametrimmer_core::rules::{Finding, RuleEngine};
use gametrimmer_core::scanner::{scan_dir, store_files};
use rusqlite::{params, Connection};

use crate::model::{category_key, DisplayCategory, FindingRow};

use super::{manual, resolve_rules_path, WorkerMsg};

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
