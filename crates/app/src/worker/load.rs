//! The startup "load previous scan" job: reads whatever `games`/`files`/
//! `findings` rows already exist in the database (from a prior "Сканувати
//! бібліотеки" run) and turns them back into [`FindingRow`]s, so the app can
//! show results immediately instead of an empty screen. Runs on a background
//! thread exactly like [`super::scan`], communicating back through the same
//! [`WorkerMsg::Done`] the scan worker uses.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;

use eframe::egui;
use gametrimmer_core::db;
use gametrimmer_core::error::Result as CoreResult;
use rusqlite::Connection;

use crate::i18n::{self, Lang};
use crate::model::{
    orphan_install_dir_and_name, parse_source_key, FindingRow, FindingSource, ORPHAN_GAME_ID,
};

use super::{Notifier, WorkerMsg};

/// Spawns the load job on a new thread. `ctx` is the app's `egui::Context`
/// (see `Notifier`) so the UI keeps repainting - and so draining the
/// `WorkerMsg` channel - even while the main window is minimized.
pub fn spawn_load(
    db_path: PathBuf,
    tx: Sender<WorkerMsg>,
    lang: Lang,
    ctx: egui::Context,
) -> JoinHandle<()> {
    let notifier = Notifier::new(tx, ctx);
    std::thread::spawn(move || run_load(&db_path, &notifier, lang))
}

fn run_load(db_path: &Path, notifier: &Notifier, lang: Lang) {
    let conn = match db::open(db_path) {
        Ok(conn) => conn,
        Err(err) => {
            notifier.send(WorkerMsg::Error {
                msg: i18n::db_open_error_short(lang, err),
            });
            return;
        }
    };

    match load_findings(&conn) {
        Ok(findings) => {
            // Live occupied-space snapshot for the UI (see
            // `occupancy_or_default`); degrades to 0 on aggregation failure.
            let occupancy = super::occupancy_or_default(&conn);
            notifier.send(WorkerMsg::Done {
                findings,
                scan_summary: i18n::loaded_saved_results(lang),
                occupancy,
                // Loading a previous snapshot did not scan anything this
                // session - there is no fresh timing to show.
                timing: None,
            });
        }
        Err(err) => {
            notifier.send(WorkerMsg::Error {
                msg: i18n::load_previous_results_failed(lang, err),
            });
        }
    }
}

/// Rebuilds every persisted finding from the database left behind by a
/// previous scan, as a single three-table join: `findings` inner-joined to
/// its `files` row (for `rel_path`/`size`) and on to that file's `games` row
/// (for the game name and install dir). Because a scan now persists each
/// finding's `group_dir` (see [`crate::worker::scan`]), the value is read
/// straight back from the column - no need to reload every game's *entire*
/// file list and re-run `assign_group_dirs` over it, which at real-world
/// scale (millions of `files` rows) was the load's dominant cost.
///
/// Only rows that actually join survive: a game with no findings, and any
/// `findings` row whose `file_id` no longer resolves (an orphan left by a
/// deletion that didn't clean up its finding) both simply drop out of the
/// `INNER JOIN`, so neither adds work or a bogus result. A
/// `findings.category` value this build's [`parse_source_key`] doesn't
/// recognize (e.g. written by a different `rules.json` version) is logged and
/// skipped rather than failing the whole load - one stale row must not hide
/// every other game's results.
///
/// A `group_dir` of `NULL` (either a genuinely ungrouped/orphan finding, or a
/// finding written by a build from before the column existed - see
/// `db::migrate`) comes back as `None`, exactly the "no grouping" the UI
/// tree already handles; the next scan repopulates real values.
///
/// Orphaned-residue findings (GT-02) are stored with a `NULL` `files.game_id`
/// (there is no game), so the `games` join is a `LEFT JOIN` and the finding's
/// own [`FindingSource`] - not the join's nullness - decides how a row is
/// rebuilt: an `Orphan` source takes the synthetic [`ORPHAN_GAME_ID`] and
/// splits the full path stored in `files.rel_path` back into its
/// `(install_dir, rel_path)` pair (see [`orphan_install_dir_and_name`]); every
/// other source requires its `games` row and is skipped (logged) if that row
/// is somehow absent, which foreign-key enforcement makes impossible for a
/// non-`NULL` `game_id` anyway.
pub fn load_findings(conn: &Connection) -> CoreResult<Vec<FindingRow>> {
    let mut stmt = conn.prepare(
        "SELECT g.id, g.name, g.install_dir, \
                fi.file_id, f.rel_path, f.size, \
                fi.category, fi.rule_id, fi.confidence, fi.lang_tag, fi.group_dir, \
                COALESCE(f.size_on_disk, f.size) \
         FROM findings fi \
         JOIN files f ON f.id = fi.file_id \
         LEFT JOIN games g ON g.id = f.game_id",
    )?;

    let mut rows = Vec::new();
    let mut result = stmt.query([])?;
    while let Some(row) = result.next()? {
        let category: String = row.get(6)?;
        let file_id: i64 = row.get(3)?;
        let Some(source) = parse_source_key(&category) else {
            crate::logger::log(&format!(
                "Пропущено findings-рядок з невідомою категорією \"{category}\" (file_id={file_id})"
            ));
            continue;
        };

        let rel_path: String = row.get(4)?;
        let size = row.get::<_, i64>(5)? as u64;
        let rule_desc = row.get::<_, Option<String>>(7)?.unwrap_or_default();
        let confidence = row.get::<_, i64>(8)? as u8;
        let lang_tag: Option<String> = row.get(9)?;
        let size_on_disk = row.get::<_, i64>(11)? as u64;

        if matches!(source, FindingSource::Orphan(_)) {
            // The orphan's full path lives in `rel_path`; split it back into the
            // parent (`install_dir`) + folder name the UI model expects. Orphan
            // findings never carry a `group_dir`.
            let (install_dir, name) = orphan_install_dir_and_name(&PathBuf::from(&rel_path));
            rows.push(FindingRow {
                file_id,
                game_id: ORPHAN_GAME_ID,
                game_name: String::new(),
                install_dir,
                rel_path: name,
                size,
                size_on_disk,
                source,
                rule_desc,
                confidence,
                lang_tag,
                group_dir: None,
            });
            continue;
        }

        let Some(game_id) = row.get::<_, Option<i64>>(0)? else {
            // A non-orphan finding whose game row is missing - impossible under
            // foreign-key enforcement, but skip rather than fabricate a game.
            crate::logger::log(&format!(
                "Пропущено findings-рядок без гри (категорія \"{category}\", file_id={file_id})"
            ));
            continue;
        };
        let install_dir: String = row.get(2)?;
        rows.push(FindingRow {
            file_id,
            game_id,
            game_name: row.get(1)?,
            install_dir: PathBuf::from(install_dir),
            rel_path,
            size,
            size_on_disk,
            source,
            rule_desc,
            confidence,
            lang_tag,
            group_dir: row.get(10)?,
        });
    }

    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gametrimmer_core::langdetect::LangKind;
    use gametrimmer_core::rules::Category;
    use rusqlite::params;

    use crate::model::FindingSource;

    fn insert_library(conn: &Connection, path: &str) -> i64 {
        conn.execute(
            "INSERT INTO game_libraries (vendor, path) VALUES ('steam', ?1)",
            params![path],
        )
        .expect("insert library");
        conn.last_insert_rowid()
    }

    fn insert_game(conn: &Connection, library_id: i64, name: &str, install_dir: &str) -> i64 {
        conn.execute(
            "INSERT INTO games (library_id, name, install_dir, app_id) VALUES (?1, ?2, ?3, NULL)",
            params![library_id, name, install_dir],
        )
        .expect("insert game");
        conn.last_insert_rowid()
    }

    fn insert_file(conn: &Connection, game_id: i64, rel_path: &str, size: i64) -> i64 {
        conn.execute(
            "INSERT INTO files (game_id, rel_path, size, mtime) VALUES (?1, ?2, ?3, NULL)",
            params![game_id, rel_path, size],
        )
        .expect("insert file");
        conn.last_insert_rowid()
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_finding(
        conn: &Connection,
        file_id: i64,
        category: &str,
        rule_id: &str,
        confidence: i64,
        lang_tag: Option<&str>,
        group_dir: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO findings (file_id, category, rule_id, confidence, lang_tag, group_dir) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![file_id, category, rule_id, confidence, lang_tag, group_dir],
        )
        .expect("insert finding");
    }

    /// A minimal single-game, single-finding database round-trips every
    /// field `load_findings` is responsible for restoring.
    #[test]
    fn load_findings_restores_row_fields() {
        let conn = db::open_in_memory().expect("open in-memory db");
        let library_id = insert_library(&conn, "C:/Games");
        let game_id = insert_game(&conn, library_id, "Test Game", "C:\\Games\\Test");
        let file_id = insert_file(&conn, game_id, "setup.exe", 12345);
        insert_finding(
            &conn,
            file_id,
            "redist_file",
            "installer pattern",
            90,
            None,
            None,
        );

        let rows = load_findings(&conn).expect("load should succeed");

        assert_eq!(rows.len(), 1, "row count must match the findings count");
        let row = &rows[0];
        assert_eq!(row.file_id, file_id);
        assert_eq!(row.game_id, game_id);
        assert_eq!(row.game_name, "Test Game");
        assert_eq!(row.install_dir, PathBuf::from("C:\\Games\\Test"));
        assert_eq!(row.rel_path, "setup.exe");
        assert_eq!(row.size, 12345);
        assert_eq!(row.confidence, 90);
        assert_eq!(row.lang_tag, None);
        assert_eq!(row.rule_desc, "installer pattern");
        assert_eq!(row.source, FindingSource::Rule(Category::RedistFile));
    }

    /// A localization finding's `category`/`lang_tag` round-trip through
    /// `parse_source_key` correctly.
    #[test]
    fn load_findings_parses_localization_source_and_lang_tag() {
        let conn = db::open_in_memory().expect("open in-memory db");
        let library_id = insert_library(&conn, "C:/Games");
        let game_id = insert_game(&conn, library_id, "Loc Game", "C:\\Games\\Loc");
        let file_id = insert_file(&conn, game_id, "audio\\es.bank", 500);
        insert_finding(
            &conn,
            file_id,
            "loc_audio",
            "маркер 'voices'",
            88,
            Some("es"),
            None,
        );

        let rows = load_findings(&conn).expect("load should succeed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, FindingSource::Loc(LangKind::Audio));
        assert_eq!(rows[0].lang_tag, Some("es".to_string()));
    }

    /// A row with a `category` that doesn't map to any known
    /// `FindingSource` must be dropped, not abort the whole load - a good
    /// row from a different game must still come back.
    #[test]
    fn load_findings_skips_rows_with_unknown_category_without_failing() {
        let conn = db::open_in_memory().expect("open in-memory db");
        let library_id = insert_library(&conn, "C:/Games");
        let game_id = insert_game(&conn, library_id, "Weird Game", "C:\\Games\\Weird");
        let known_file = insert_file(&conn, game_id, "a.txt", 10);
        let unknown_file = insert_file(&conn, game_id, "b.txt", 20);
        insert_finding(&conn, known_file, "bonus", "bonus rule", 80, None, None);
        insert_finding(
            &conn,
            unknown_file,
            "totally_unknown_category",
            "?",
            80,
            None,
            None,
        );

        let rows = load_findings(&conn).expect("an unknown category must not fail the whole load");

        assert_eq!(
            rows.len(),
            1,
            "the unknown-category row must be skipped, the known one kept"
        );
        assert_eq!(rows[0].rel_path, "a.txt");
    }

    /// A game with a large file list but zero findings must contribute
    /// nothing to the result - this is the case the games-with-findings
    /// filter (an `EXISTS` clause in the `games` query) is meant to skip
    /// entirely without ever reading its files. A second, unrelated game
    /// with an actual finding must still come back unaffected, proving the
    /// filter selects the *right* games rather than just excluding
    /// everything.
    #[test]
    fn load_findings_skips_a_clean_game_entirely_but_keeps_others_with_findings() {
        let conn = db::open_in_memory().expect("open in-memory db");
        let library_id = insert_library(&conn, "C:/Games");

        let clean_game = insert_game(&conn, library_id, "Clean Game", "C:\\Games\\Clean");
        for index in 0..50 {
            insert_file(&conn, clean_game, &format!("file{index}.txt"), 10);
        }

        let flagged_game = insert_game(&conn, library_id, "Flagged Game", "C:\\Games\\Flagged");
        let flagged_file = insert_file(&conn, flagged_game, "setup.exe", 12345);
        insert_finding(
            &conn,
            flagged_file,
            "redist_file",
            "installer pattern",
            90,
            None,
            None,
        );

        let rows = load_findings(&conn).expect("load should succeed");

        assert_eq!(
            rows.len(),
            1,
            "only the flagged game's finding should come back, the clean game must be skipped"
        );
        assert_eq!(rows[0].game_id, flagged_game);
        assert_eq!(rows[0].rel_path, "setup.exe");
    }

    /// `group_dir` is now persisted at scan time, so load reads it straight
    /// back from the column rather than recomputing it. A finding stored with
    /// its collapsing folder must come back carrying that same folder.
    #[test]
    fn load_findings_reads_persisted_group_dir() {
        let conn = db::open_in_memory().expect("open in-memory db");
        let library_id = insert_library(&conn, "C:/Games");
        let game_id = insert_game(&conn, library_id, "Folder Game", "C:\\Games\\Folder");
        let file_a = insert_file(&conn, game_id, "junk\\a.txt", 10);
        let file_b = insert_file(&conn, game_id, "junk\\b.txt", 20);
        insert_finding(&conn, file_a, "bonus", "bonus rule", 80, None, Some("junk"));
        insert_finding(&conn, file_b, "bonus", "bonus rule", 80, None, Some("junk"));

        let rows = load_findings(&conn).expect("load should succeed");

        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|row| row.group_dir.as_deref() == Some("junk")),
            "both findings must come back with the persisted group folder"
        );
    }

    /// An orphaned-residue finding (GT-02) is stored with a `NULL`
    /// `files.game_id` and the leftover's full path in `files.rel_path`. Load
    /// must reconstruct it as the synthetic orphan branch: `ORPHAN_GAME_ID`,
    /// empty game name, and the full path split back into install_dir + name.
    /// A normal game finding in the same database must still load unaffected -
    /// proving the `LEFT JOIN` didn't drop or corrupt either kind.
    #[test]
    fn load_findings_reconstructs_orphan_rows_and_keeps_game_rows() {
        let conn = db::open_in_memory().expect("open in-memory db");

        // A normal game finding.
        let library_id = insert_library(&conn, "F:/lib");
        let game_id = insert_game(&conn, library_id, "Real Game", "F:\\lib\\Real Game");
        let game_file = insert_file(&conn, game_id, "redist\\setup.exe", 500);
        insert_finding(&conn, game_file, "redist_file", "installer", 90, None, None);

        // An orphan finding: NULL game_id, full path in rel_path.
        let orphan_full = r"F:\lib\steamapps\common\Leftover";
        conn.execute(
            "INSERT INTO files (game_id, rel_path, size, mtime) VALUES (NULL, ?1, ?2, NULL)",
            params![orphan_full, 4096i64],
        )
        .expect("insert orphan file");
        let orphan_file = conn.last_insert_rowid();
        insert_finding(
            &conn,
            orphan_file,
            "orphan_folder",
            "осиротіла тека",
            60,
            None,
            None,
        );

        let rows = load_findings(&conn).expect("load should succeed");
        assert_eq!(rows.len(), 2, "both the game and the orphan finding load");

        let orphan = rows
            .iter()
            .find(|row| row.game_id == ORPHAN_GAME_ID)
            .expect("the orphan row must come back under the sentinel game id");
        assert!(orphan.game_name.is_empty());
        assert_eq!(
            orphan.install_dir,
            PathBuf::from(r"F:\lib\steamapps\common")
        );
        assert_eq!(orphan.rel_path, "Leftover");
        assert_eq!(
            orphan.install_dir.join(&orphan.rel_path),
            PathBuf::from(orphan_full),
            "install_dir + rel_path must reconstruct the stored full path"
        );
        assert_eq!(
            orphan.source,
            FindingSource::Orphan(gametrimmer_core::orphans::OrphanKind::UnmanagedFolder)
        );

        let game = rows
            .iter()
            .find(|row| row.game_id == game_id)
            .expect("the normal game finding must still load");
        assert_eq!(game.game_name, "Real Game");
        assert_eq!(game.rel_path, "redist\\setup.exe");
    }

    /// A finding persisted with a `NULL` `group_dir` (an ungrouped/orphan
    /// finding, or one written before the column existed) must come back as
    /// `None`, not error out.
    #[test]
    fn load_findings_reads_null_group_dir_as_none() {
        let conn = db::open_in_memory().expect("open in-memory db");
        let library_id = insert_library(&conn, "C:/Games");
        let game_id = insert_game(&conn, library_id, "Mixed Game", "C:\\Games\\Mixed");
        let flagged_file = insert_file(&conn, game_id, "mixed\\a.txt", 10);
        insert_finding(&conn, flagged_file, "bonus", "bonus rule", 80, None, None);

        let rows = load_findings(&conn).expect("load should succeed");

        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].group_dir, None,
            "a NULL group_dir column must load as None"
        );
    }

    /// Not run in normal `cargo test` - this is the before/after measurement
    /// harness used to size the `load_findings` optimization against the
    /// reported real-world shape: ~1500 games, 300-500 files each (hundreds
    /// of thousands of `files` rows), with only a handful of games actually
    /// carrying findings - the common case, since most scanned games come
    /// back clean.
    ///
    /// Run with:
    /// `cargo test -p gametrimmer-app --lib worker::load::tests::load_findings_at_realistic_scale -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn load_findings_at_realistic_scale() {
        const GAMES: i64 = 1500;
        const FILES_PER_GAME_MIN: i64 = 300;
        const FILES_PER_GAME_SPAN: i64 = 201; // 300..=500
        const GAMES_WITH_FINDINGS: i64 = 30;

        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("gametrimmer.db");
        let mut conn = db::open(&db_path).expect("open file-backed db");

        let library_id = insert_library(&conn, "C:/Games");

        {
            // One transaction for the whole setup - this is test-fixture
            // cost, not what's being measured, so it should not dominate the
            // wall-clock time of the run.
            let tx = conn.transaction().expect("begin setup transaction");
            for game_index in 0..GAMES {
                let files_count = FILES_PER_GAME_MIN + (game_index % FILES_PER_GAME_SPAN);
                tx.execute(
                    "INSERT INTO games (library_id, name, install_dir, app_id) \
                     VALUES (?1, ?2, ?3, NULL)",
                    params![
                        library_id,
                        format!("Game {game_index}"),
                        format!("C:\\Games\\Game{game_index}")
                    ],
                )
                .expect("insert game");
                let game_id = tx.last_insert_rowid();

                for file_index in 0..files_count {
                    tx.execute(
                        "INSERT INTO files (game_id, rel_path, size, mtime) \
                         VALUES (?1, ?2, ?3, NULL)",
                        params![
                            game_id,
                            format!("dir{}\\file{file_index}.txt", file_index % 10),
                            1024
                        ],
                    )
                    .expect("insert file");
                    let file_id = tx.last_insert_rowid();

                    // Only a small minority of games get any findings at
                    // all - the dominant real-world shape this optimization
                    // targets.
                    if game_index < GAMES_WITH_FINDINGS && file_index % 5 == 0 {
                        tx.execute(
                            "INSERT INTO findings \
                             (file_id, category, rule_id, confidence, lang_tag) \
                             VALUES (?1, 'bonus', 'bonus rule', 80, NULL)",
                            params![file_id],
                        )
                        .expect("insert finding");
                    }
                }
            }
            tx.commit().expect("commit setup transaction");
        }

        let started = std::time::Instant::now();
        let rows = load_findings(&conn).expect("load should succeed");
        let elapsed = started.elapsed();

        println!(
            "load_findings_at_realistic_scale: games={GAMES} \
             games_with_findings={GAMES_WITH_FINDINGS} rows={} elapsed={elapsed:?}",
            rows.len()
        );
    }

    /// Manual benchmark against a copy of a real user database. Never run in
    /// CI (`#[ignore]` + requires an env var); works on a scratch copy so the
    /// original file is never opened, let alone written to. Run with:
    /// `GT_REAL_DB=<path> cargo test -p gametrimmer --release real_db_benchmarks -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_db_benchmarks() {
        use std::time::Instant;

        let Ok(source) = std::env::var("GT_REAL_DB") else {
            panic!("set GT_REAL_DB to the path of a real database copy");
        };

        let scratch = std::env::temp_dir().join("gt_real_db_bench.db");
        let copy_started = Instant::now();
        std::fs::copy(&source, &scratch).expect("copy real db to scratch");
        println!(
            "copied {source} -> {} in {:?}",
            scratch.display(),
            copy_started.elapsed()
        );

        let conn = db::open(&scratch).expect("open scratch db");

        for table in ["game_libraries", "games", "files", "findings", "operations"] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .expect("count");
            println!("{table}: {count} rows");
        }
        let flagged_games: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT f.game_id) FROM findings fi \
                 JOIN files f ON f.id = fi.file_id",
                [],
                |r| r.get(0),
            )
            .expect("flagged games");
        println!("games with findings: {flagged_games}");

        let started = Instant::now();
        let rows = load_findings(&conn).expect("load should succeed");
        println!(
            "load_findings: rows={} elapsed={:?}",
            rows.len(),
            started.elapsed()
        );

        let started = Instant::now();
        db::clear_scan_data(&conn).expect("clear");
        println!("clear_scan_data: {:?}", started.elapsed());

        let started = Instant::now();
        db::checkpoint_truncate(&conn).expect("checkpoint");
        println!("checkpoint_truncate: {:?}", started.elapsed());

        let fraction = db::free_page_fraction(&conn).expect("free fraction");
        let started = Instant::now();
        db::compact(&conn).expect("vacuum");
        println!(
            "free_fraction={fraction:.2} vacuum: {:?}",
            started.elapsed()
        );

        drop(conn);
        let _ = std::fs::remove_file(&scratch);
        let _ = std::fs::remove_file(scratch.with_extension("db-wal"));
        let _ = std::fs::remove_file(scratch.with_extension("db-shm"));
    }
}
