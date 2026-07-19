//! The startup "load previous scan" job: reads whatever `games`/`files`/
//! `findings` rows already exist in the database (from a prior "Сканувати
//! бібліотеки" run) and turns them back into [`FindingRow`]s, so the app can
//! show results immediately instead of an empty screen. Runs on a background
//! thread exactly like [`super::scan`], communicating back through the same
//! [`WorkerMsg::Done`] the scan worker uses.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;

use gametrimmer_core::db;
use gametrimmer_core::error::Result as CoreResult;
use gametrimmer_core::scanner::FileEntry;
use rusqlite::{params, Connection};

use crate::i18n::{self, Lang};
use crate::model::{parse_source_key, FindingRow};

use super::scan::assign_group_dirs;
use super::WorkerMsg;

/// Spawns the load job on a new thread.
pub fn spawn_load(db_path: PathBuf, tx: Sender<WorkerMsg>, lang: Lang) -> JoinHandle<()> {
    std::thread::spawn(move || run_load(&db_path, &tx, lang))
}

fn run_load(db_path: &Path, tx: &Sender<WorkerMsg>, lang: Lang) {
    let conn = match db::open(db_path) {
        Ok(conn) => conn,
        Err(err) => {
            let _ = tx.send(WorkerMsg::Error {
                msg: i18n::db_open_error_short(lang, err),
            });
            return;
        }
    };

    match load_findings(&conn) {
        Ok(findings) => {
            let _ = tx.send(WorkerMsg::Done {
                findings,
                scan_summary: i18n::loaded_saved_results(lang),
            });
        }
        Err(err) => {
            let _ = tx.send(WorkerMsg::Error {
                msg: i18n::load_previous_results_failed(lang, err),
            });
        }
    }
}

/// One `findings` row as read straight out of the database, before its
/// `category` has been reparsed into a [`crate::model::FindingSource`].
struct RawFinding {
    file_id: i64,
    category: String,
    rule_id: Option<String>,
    confidence: u8,
    lang_tag: Option<String>,
}

/// Rebuilds every persisted finding, game by game, from the database left
/// behind by a previous scan. `group_dir` cannot simply be read back (it was
/// never persisted - see [`FindingRow::group_dir`]), so it is recomputed
/// per-game exactly the way the scan worker computes it originally: from the
/// *full* file list (flagged and unflagged alike), via
/// [`assign_group_dirs`]. This means a game whose file list changed on disk
/// since the last scan (a file removed outside the app, say) still gets a
/// self-consistent grouping - it just reflects what's in the database, not
/// necessarily what's on disk right now (a fresh scan reconciles that).
///
/// A `findings.category` value this build's [`parse_source_key`] doesn't
/// recognize (e.g. written by a different `rules.json` version) is logged
/// and skipped rather than failing the whole load - one stale row must not
/// hide every other game's results.
pub fn load_findings(conn: &Connection) -> CoreResult<Vec<FindingRow>> {
    let mut games_stmt = conn.prepare("SELECT id, name, install_dir FROM games")?;
    let games: Vec<(i64, String, String)> = games_stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(games_stmt);

    let mut rows = Vec::new();

    for (game_id, game_name, install_dir) in games {
        let install_dir = PathBuf::from(install_dir);

        let mut files_stmt =
            conn.prepare("SELECT id, rel_path, size FROM files WHERE game_id = ?1")?;
        let files: Vec<(i64, String, u64)> = files_stmt
            .query_map(params![game_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? as u64,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(files_stmt);

        if files.is_empty() {
            continue;
        }

        // `files.id` -> its index into `files`/`entries`, so findings rows
        // (keyed by `file_id`) can be matched back to the right entry.
        let index_by_file_id: HashMap<i64, usize> = files
            .iter()
            .enumerate()
            .map(|(index, (file_id, _, _))| (*file_id, index))
            .collect();

        // The *full* file list - flagged and unflagged - is required by
        // `assign_group_dirs`, which needs sibling context to decide whether
        // a directory is wholly non-essential.
        let entries: Vec<FileEntry> = files
            .iter()
            .map(|(_, rel_path, size)| FileEntry {
                rel_path: rel_path.clone(),
                size: *size,
                mtime: None,
            })
            .collect();

        let mut findings_stmt = conn.prepare(
            "SELECT file_id, category, rule_id, confidence, lang_tag \
             FROM findings WHERE file_id IN (SELECT id FROM files WHERE game_id = ?1)",
        )?;
        let raw_findings: Vec<RawFinding> = findings_stmt
            .query_map(params![game_id], |row| {
                Ok(RawFinding {
                    file_id: row.get(0)?,
                    category: row.get(1)?,
                    rule_id: row.get(2)?,
                    confidence: row.get::<_, i64>(3)? as u8,
                    lang_tag: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(findings_stmt);

        let mut flagged: HashSet<usize> = HashSet::new();
        let mut parsed = Vec::with_capacity(raw_findings.len());
        for finding in raw_findings {
            let Some(&index) = index_by_file_id.get(&finding.file_id) else {
                // Orphaned findings row (its file was deleted without the
                // finding being cleaned up) - shouldn't happen given how
                // `persist_prepared_game` maintains the two tables, but
                // skipping it is safer than panicking on a stale database.
                continue;
            };
            let Some(source) = parse_source_key(&finding.category) else {
                eprintln!(
                    "Пропущено findings-рядок з невідомою категорією \"{}\" (file_id={})",
                    finding.category, finding.file_id
                );
                continue;
            };
            flagged.insert(index);
            parsed.push((index, finding, source));
        }

        let group_dirs = assign_group_dirs(&entries, &flagged);

        for (index, finding, source) in parsed {
            let entry = &entries[index];
            rows.push(FindingRow {
                file_id: finding.file_id,
                game_id,
                game_name: game_name.clone(),
                install_dir: install_dir.clone(),
                rel_path: entry.rel_path.clone(),
                size: entry.size,
                source,
                rule_desc: finding.rule_id.unwrap_or_default(),
                confidence: finding.confidence,
                lang_tag: finding.lang_tag,
                group_dir: group_dirs.get(&index).cloned(),
            });
        }
    }

    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gametrimmer_core::langdetect::LangKind;
    use gametrimmer_core::rules::Category;

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
    ) {
        conn.execute(
            "INSERT INTO findings (file_id, category, rule_id, confidence, lang_tag) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![file_id, category, rule_id, confidence, lang_tag],
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
        insert_finding(&conn, file_id, "redist_file", "installer pattern", 90, None);

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
        insert_finding(&conn, known_file, "bonus", "bonus rule", 80, None);
        insert_finding(
            &conn,
            unknown_file,
            "totally_unknown_category",
            "?",
            80,
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

    /// `group_dir` is not persisted - it must be recomputed. A folder where
    /// every file has a finding collapses into that folder's path, exactly
    /// as `assign_group_dirs` does during a live scan.
    #[test]
    fn load_findings_recomputes_group_dir_for_a_fully_flagged_folder() {
        let conn = db::open_in_memory().expect("open in-memory db");
        let library_id = insert_library(&conn, "C:/Games");
        let game_id = insert_game(&conn, library_id, "Folder Game", "C:\\Games\\Folder");
        let file_a = insert_file(&conn, game_id, "junk\\a.txt", 10);
        let file_b = insert_file(&conn, game_id, "junk\\b.txt", 20);
        insert_finding(&conn, file_a, "bonus", "bonus rule", 80, None);
        insert_finding(&conn, file_b, "bonus", "bonus rule", 80, None);

        let rows = load_findings(&conn).expect("load should succeed");

        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|row| row.group_dir.as_deref() == Some("junk")),
            "both members of the fully-flagged folder must collapse into it"
        );
    }

    /// Sibling case: a folder with an unflagged file must not collapse -
    /// `group_dir` stays `None` for its flagged member.
    #[test]
    fn load_findings_leaves_group_dir_none_when_a_sibling_file_is_unflagged() {
        let conn = db::open_in_memory().expect("open in-memory db");
        let library_id = insert_library(&conn, "C:/Games");
        let game_id = insert_game(&conn, library_id, "Mixed Game", "C:\\Games\\Mixed");
        let flagged_file = insert_file(&conn, game_id, "mixed\\a.txt", 10);
        let _unflagged_file = insert_file(&conn, game_id, "mixed\\b.txt", 20);
        insert_finding(&conn, flagged_file, "bonus", "bonus rule", 80, None);

        let rows = load_findings(&conn).expect("load should succeed");

        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].group_dir, None,
            "the folder has an unflagged sibling, so it must not collapse"
        );
    }
}
