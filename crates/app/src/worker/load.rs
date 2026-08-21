//! The startup "load previous scan" job: reads whatever `games`/`files`/
//! `findings` rows already exist in the database (from a prior "Scan
//! Libraries" run) and turns them back into [`FindingRow`]s, so the app can
//! show results immediately instead of an empty screen. Runs on a background
//! thread exactly like [`super::scan`], communicating back through the same
//! [`WorkerMsg::Done`] the scan worker uses.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;

use eframe::egui;
use gametrimmer_core::db;
use gametrimmer_core::error::Result as CoreResult;
use rusqlite::Connection;

use crate::i18n::{self, Lang};
use crate::model::{
    parse_source_key, rootless_branch_id, rootless_split, FindingRow, FindingSource, LibraryOrigin,
};

use super::{Notifier, WorkerMsg};
#[cfg(test)]
use crate::model::ORPHAN_GAME_ID;

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
    // `open_reconciling` rather than `open`: every open now settles delete
    // intents left pending by a crash, but this is the one path with a user
    // watching, so it is also the one that says what was settled.
    let (mut conn, reconciliation) = match db::open_reconciling(db_path) {
        Ok(opened) => opened,
        Err(err) => {
            notifier.report_error(i18n::Reported::new(lang, |l| {
                i18n::db_open_error_short(l, &err)
            }));
            return;
        }
    };

    if let Err(err) = db::cleanup_abandoned_scans(&mut conn) {
        notifier.report_warning(i18n::Reported::new(lang, |l| {
            i18n::scan_incomplete(l, &err)
        }));
    }

    if !reconciliation.reconciled.is_empty() {
        let count = reconciliation.reconciled.len();
        notifier.report_warning(i18n::Reported::new(lang, move |l| {
            i18n::pending_delete_reconciled(l, count)
        }));
    }
    if let Some(err) = reconciliation.error {
        notifier.report_warning(i18n::Reported::new(lang, move |l| {
            i18n::db_update_after_delete_failed(l, &err)
        }));
    }

    match load_scan_diagnostics(&conn) {
        Ok(diagnostics) => {
            for (provider, stage, path, message) in diagnostics {
                let detail = match path {
                    Some(path) => format!("{message} [{stage}: {path}]"),
                    None => format!("{message} [{stage}]"),
                };
                notifier.report_warning(i18n::Reported::new(lang, |l| {
                    i18n::provider_message(l, &provider, &detail)
                }));
            }
        }
        Err(err) => notifier.report_warning(i18n::Reported::new(lang, |l| {
            i18n::provider_message(l, "database", &err)
        })),
    }

    match load_findings_with_lang(&conn, lang) {
        Ok(findings) => {
            // Live occupied-space snapshot for the UI (see
            // `occupancy_or_default`); degrades to 0 on aggregation failure.
            let occupancy = super::occupancy_or_default(&conn);
            notifier.send(WorkerMsg::Done {
                findings,
                scan_summary: i18n::loaded_saved_results(lang),
                occupancy,
                // Loading a previous snapshot did not scan anything this
                // session - there is no fresh timing to show, and no
                // routing decisions were made either.
                timing: None,
                routing_breakdown: String::new(),
            });
        }
        Err(err) => {
            notifier.report_error(i18n::Reported::new(lang, |l| {
                i18n::load_previous_results_failed(l, &err)
            }));
        }
    }
}

type StoredDiagnostic = (String, String, Option<String>, String);

fn load_scan_diagnostics(conn: &Connection) -> CoreResult<Vec<StoredDiagnostic>> {
    let Some(scan_id) = db::active_scan_id(conn)? else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(
        "SELECT provider, stage, path, message FROM scan_diagnostics
         WHERE scan_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([scan_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
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
/// Orphaned-residue findings (orphan-residue safety) are stored with a `NULL` `files.game_id`
/// (there is no game), so the `games` join is a `LEFT JOIN` and the finding's
/// own [`FindingSource`] - not the join's nullness - decides how a row is
/// rebuilt: an `Orphan` source takes the synthetic [`ORPHAN_GAME_ID`] and
/// splits the full path stored in `files.rel_path` back into its
/// `(install_dir, rel_path)` pair (see [`orphan_install_dir_and_name`]); every
/// other source requires its `games` row and is skipped (logged) if that row
/// is somehow absent, which foreign-key enforcement makes impossible for a
/// non-`NULL` `game_id` anyway.
#[cfg(test)]
pub fn load_findings(conn: &Connection) -> CoreResult<Vec<FindingRow>> {
    load_findings_with_lang(conn, Lang::En)
}

fn load_findings_with_lang(conn: &Connection, lang: Lang) -> CoreResult<Vec<FindingRow>> {
    let mut stmt = conn.prepare(
        "SELECT g.id, g.name, g.install_dir, \
                fi.file_id, f.rel_path, f.size, \
                fi.category, fi.rule_id, fi.confidence, fi.lang_tag, fi.group_dir, \
                COALESCE(f.size_on_disk, f.size), \
                CASE \
                  WHEN f.scan_id = 0 THEN 'legacy snapshot is read-only' \
                  WHEN fs.file_id IS NULL THEN 'missing filesystem safety evidence' \
                  WHEN fs.block_reason IS NOT NULL THEN fs.block_reason \
                  WHEN fs.root_identity IS NULL OR fs.target_identity IS NULL \
                    THEN 'missing filesystem identity' \
                  ELSE NULL \
                END, COALESCE(fi.provenance, 'builtin'), \
                COALESCE(gl.vendor, glo.vendor), \
                COALESCE(gl.path, fs.evidence_library_path), \
                sle.status, f.game_id IS NULL, g.app_id, fi.action \
         FROM findings fi \
         JOIN files f ON f.id = fi.file_id \
         LEFT JOIN games g ON g.id = f.game_id \
         LEFT JOIN game_libraries gl ON gl.id = g.library_id \
         LEFT JOIN file_safety fs ON fs.file_id = f.id \
         LEFT JOIN game_libraries glo ON glo.path = fs.evidence_library_path \
         LEFT JOIN scan_library_evidence sle \
           ON sle.scan_id = f.scan_id \
          AND sle.library_path = COALESCE(gl.path, fs.evidence_library_path) \
         WHERE f.scan_id = (SELECT active_scan_id FROM scan_state WHERE singleton = 1)",
    )?;

    let mut rows = Vec::new();
    // A game may contribute thousands of findings, but its anti-cheat verdict
    // requires one complete directory walk. Cache only monolithic-relevant
    // games: ordinary DirectDelete rows do not consult this protection flag.
    let mut anti_cheat_cache = HashMap::new();
    let mut result = stmt.query([])?;
    while let Some(row) = result.next()? {
        let category: String = row.get(6)?;
        let file_id: i64 = row.get(3)?;
        let Some(source) = parse_source_key(&category) else {
            crate::logger::error(&format!(
                "Skipped a findings row with an unknown category \"{category}\" (file_id={file_id})"
            ));
            continue;
        };

        let rel_path: String = row.get(4)?;
        let Some(size) = nonnegative_persisted_size(row.get(5)?, "size", file_id) else {
            continue;
        };
        let rule_desc = row.get::<_, Option<String>>(7)?.unwrap_or_default();
        let confidence = row.get::<_, i64>(8)? as u8;
        let lang_tag: Option<String> = row.get(9)?;
        let Some(full_size_on_disk) =
            nonnegative_persisted_size(row.get(11)?, "size_on_disk", file_id)
        else {
            continue;
        };
        let action_raw: Option<String> = row.get(19)?;
        let Some(action) = restored_action(&source, action_raw.as_deref(), file_id) else {
            continue;
        };
        // The filesystem-evidence half of the verdict is still decided in SQL
        // (it reads columns and nothing else). The launcher-discovery half is
        // decided here instead, by the same function the scan's persistence
        // path and the delete preflight use, so the three can no longer drift
        // apart - which they had, the SQL `CASE` being the most permissive of
        // the three. SQL wins when both have something to say: a legacy
        // snapshot or missing identity is the more fundamental problem.
        let evidence_status: Option<String> = row.get(16)?;
        let is_orphan_row: bool = row.get(17)?;
        let deletion_block_reason: Option<String> =
            row.get::<_, Option<String>>(12)?.or_else(|| {
                gametrimmer_core::safety::discovery_block_reason(
                    !is_orphan_row,
                    evidence_status.as_deref(),
                )
                .map(str::to_string)
            });
        let imported_untrusted = row.get::<_, String>(13)? == "imported_untrusted";
        // A game row is attributed through its `games.library_id`; an orphan
        // row has no game, so its root comes from the library path the scan
        // recorded as its safety evidence, resolved back to the same
        // `game_libraries` row. Either way the vendor and the root come from
        // the row the launcher actually wrote, which is what lets the fresh
        // scan agree with this load.
        let library_vendor: Option<String> = row.get(14)?;
        let library_root: Option<String> = row.get(15)?;
        let library = library_root.map(|root| LibraryOrigin {
            vendor: library_vendor,
            root: PathBuf::from(root),
        });

        if is_orphan_row {
            // A row with no game - orphaned residue or a janitor artifact -
            // keeps its full path in `rel_path`; split it back into the parent
            // (`install_dir`) + name the UI model expects - shifted up to the
            // group's parent when the row carries one, so the tree can draw a
            // folder header per game over an area that is otherwise hundreds
            // of loose files.
            //
            // Keyed off the row's own `game_id IS NULL`, not off the finding
            // source: the janitor's artifacts carry ordinary rule categories
            // (`crash_dump`, `shader_cache`, ...) yet have no game row either,
            // and matching on the source alone dropped every one of them as a
            // "finding with no game".
            let persisted_group: Option<String> = row.get(10)?;
            let (install_dir, name, group_dir) =
                rootless_split(&PathBuf::from(&rel_path), persisted_group.as_deref());
            rows.push(FindingRow {
                file_id,
                game_id: rootless_branch_id(source),
                game_name: String::new(),
                app_id: None,
                install_dir,
                rel_path: name,
                size,
                size_on_disk: full_size_on_disk,
                source,
                rule_desc,
                confidence,
                lang_tag,
                group_dir,
                deletion_block_reason,
                imported_untrusted,
                library,
                action,
                anti_cheat_protected: false,
                monolith_badge: None,
            });
            continue;
        }

        let Some(game_id) = row.get::<_, Option<i64>>(0)? else {
            // A non-orphan finding whose game row is missing - impossible under
            // foreign-key enforcement, but skip rather than fabricate a game.
            crate::logger::error(&format!(
                "Skipped a findings row with no game (category \"{category}\", file_id={file_id})"
            ));
            continue;
        };
        let install_dir: String = row.get(2)?;
        let install_path = PathBuf::from(&install_dir);
        let anti_cheat_protected = cached_anti_cheat_protection(
            &mut anti_cheat_cache,
            game_id,
            action.is_monolithic_archive(),
            || archive_trimmer::anti_cheat::AntiCheatShield::is_safe(&install_path),
        );
        let size_on_disk = if deletion_block_reason.as_deref()
            == Some("archive container is read-only until safe rollback is implemented")
        {
            0
        } else {
            reclaimable_size_on_load(&action, size, full_size_on_disk)
        };
        let monolith_badge = action
            .is_monolithic_archive()
            .then(|| i18n::monolithic_badge(lang, size_on_disk, size));
        rows.push(FindingRow {
            file_id,
            game_id,
            game_name: row.get(1)?,
            app_id: row.get(18)?,
            install_dir: install_path,
            rel_path,
            size,
            size_on_disk,
            source,
            rule_desc,
            confidence,
            lang_tag,
            group_dir: row.get(10)?,
            deletion_block_reason,
            imported_untrusted,
            library,
            action,
            anti_cheat_protected,
            monolith_badge,
        });
    }

    Ok(rows)
}

/// SQLite does not have an unsigned integer type. A corrupt negative size
/// must never cross the `i64 -> u64` boundary, where it would become a huge
/// reclaim estimate and poison every UI aggregate. Skip the affected row and
/// retain a diagnostic instead.
fn nonnegative_persisted_size(value: i64, field: &str, file_id: i64) -> Option<u64> {
    match u64::try_from(value) {
        Ok(value) => Some(value),
        Err(_) => {
            crate::logger::error(&format!(
                "Skipped finding {file_id}: persisted {field} is negative ({value})"
            ));
            None
        }
    }
}

fn cached_anti_cheat_protection(
    cache: &mut HashMap<i64, bool>,
    game_id: i64,
    monolithic_relevant: bool,
    check_safe: impl FnOnce() -> bool,
) -> bool {
    if !monolithic_relevant {
        return false;
    }
    *cache.entry(game_id).or_insert_with(|| !check_safe())
}

/// Restored monolithic rows display the bytes their validated action can
/// reclaim, not the allocation of the whole archive. Persisted estimates are
/// untrusted accounting hints, so they cannot exceed either logical or
/// allocated archive size. Ordinary whole-file deletes retain their full
/// allocated size.
fn reclaimable_size_on_load(
    action: &gametrimmer_core::models::FindingAction,
    logical_size: u64,
    physical_size: u64,
) -> u64 {
    if action.is_monolithic_archive() {
        action
            .estimated_savings()
            .min(logical_size)
            .min(physical_size)
    } else {
        physical_size
    }
}

/// Restores the action contract persisted alongside a finding without ever
/// converting corruption into a deletion. Older ordinary findings did not
/// carry action JSON, so a missing/blank value remains a compatible direct
/// delete only for non-monolithic categories. A monolithic row without a
/// valid monolithic action is skipped and diagnosed: showing it as an
/// ordinary whole-file delete would be materially unsafe.
fn restored_action(
    source: &FindingSource,
    raw: Option<&str>,
    file_id: i64,
) -> Option<gametrimmer_core::models::FindingAction> {
    use gametrimmer_core::models::FindingAction;
    use gametrimmer_core::rules::Category;

    let is_monolithic = matches!(source, FindingSource::Rule(Category::MonolithicArchive));
    let legacy_blank = raw.map(str::trim).is_none_or(str::is_empty);
    let action = match FindingAction::from_json(raw) {
        Ok(action) => action,
        Err(err) => {
            crate::logger::error(&format!(
                "Skipped finding {file_id}: persisted action JSON is invalid: {err}"
            ));
            return None;
        }
    };

    if is_monolithic && (legacy_blank || !action.is_monolithic_archive()) {
        crate::logger::error(&format!(
            "Skipped monolithic finding {file_id}: missing or non-monolithic action contract"
        ));
        return None;
    }
    if !is_monolithic && !matches!(action, FindingAction::DirectDelete) {
        crate::logger::error(&format!(
            "Skipped non-monolithic finding {file_id}: unexpected non-delete action contract"
        ));
        return None;
    }
    Some(action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gametrimmer_core::langdetect::LangKind;
    use gametrimmer_core::rules::Category;
    use rusqlite::params;

    use crate::model::FindingSource;

    #[test]
    fn malformed_persisted_action_never_becomes_direct_delete() {
        let source = FindingSource::Rule(Category::RedistFile);
        assert!(restored_action(&source, Some("{not valid json"), 7).is_none());
    }

    #[test]
    fn legacy_blank_action_is_kept_only_for_ordinary_findings() {
        let ordinary = FindingSource::Rule(Category::RedistFile);
        assert!(matches!(
            restored_action(&ordinary, None, 7),
            Some(gametrimmer_core::models::FindingAction::DirectDelete)
        ));

        let monolith = FindingSource::Rule(Category::MonolithicArchive);
        assert!(restored_action(&monolith, Some("  "), 8).is_none());
    }

    #[test]
    fn anti_cheat_check_is_lazy_and_cached_once_per_relevant_game() {
        let mut cache = HashMap::new();
        let calls = std::cell::Cell::new(0usize);

        assert!(!cached_anti_cheat_protection(&mut cache, 1, false, || {
            calls.set(calls.get() + 1);
            false
        }));
        assert_eq!(calls.get(), 0, "ordinary findings need no directory walk");

        for _ in 0..2 {
            assert!(cached_anti_cheat_protection(&mut cache, 1, true, || {
                calls.set(calls.get() + 1);
                false
            }));
        }
        assert_eq!(
            calls.get(),
            1,
            "multiple monolithic findings from one game share one fail-closed verdict"
        );
    }

    #[test]
    fn restored_archive_savings_are_capped_by_logical_and_physical_size() {
        use gametrimmer_core::models::FindingAction;

        let action = FindingAction::SparseZero {
            format: "Wwise".to_string(),
            languages: vec!["de".to_string()],
            stream_count: 1,
            offsets: vec![(0, 1_200)],
            streams: vec![],
            estimated_savings: 1_200,
        };
        assert_eq!(reclaimable_size_on_load(&action, 1_000, 800), 800);
        assert_eq!(reclaimable_size_on_load(&action, 700, 900), 700);
        assert_eq!(
            reclaimable_size_on_load(&FindingAction::DirectDelete, 700, 900),
            900,
            "ordinary delete rows retain allocated-size accounting"
        );
    }

    #[test]
    fn load_rebuilds_archive_reclaimable_size_and_localized_badge() {
        use gametrimmer_core::models::FindingAction;

        let conn = db::open_in_memory().expect("open in-memory db");
        let install = tempfile::tempdir().expect("create install dir");
        let library_id = insert_library(&conn, &install.path().to_string_lossy());
        let game_id = insert_game(
            &conn,
            library_id,
            "Archive Game",
            &install.path().to_string_lossy(),
        );
        let file_id = insert_file(&conn, game_id, "voices.pck", 1_000);
        conn.execute(
            "UPDATE files SET size_on_disk = 800 WHERE id = ?1",
            [file_id],
        )
        .expect("set physical archive size");
        insert_finding(
            &conn,
            file_id,
            "monolithic_archive",
            "archive inspector",
            90,
            None,
            None,
        );
        let action = FindingAction::SparseZero {
            format: "Wwise".to_string(),
            languages: vec!["de".to_string()],
            stream_count: 1,
            offsets: vec![(0, 1_200)],
            streams: vec![],
            estimated_savings: 1_200,
        };
        conn.execute(
            "UPDATE findings SET action = ?1 WHERE file_id = ?2",
            params![action.to_json(), file_id],
        )
        .expect("persist archive action");

        let rows = load_findings_with_lang(&conn, Lang::Uk).expect("load archive finding");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].size_on_disk, 800);
        assert_eq!(
            rows[0].monolith_badge,
            Some(i18n::monolithic_badge(Lang::Uk, 800, 1_000)),
            "reload rebuilds the badge from capped reclaimable bytes in the active locale"
        );
    }

    #[test]
    fn load_skips_negative_persisted_sizes_instead_of_wrapping_to_u64() {
        let conn = db::open_in_memory().expect("open in-memory db");
        let library_id = insert_library(&conn, "C:/Games");
        let game_id = insert_game(&conn, library_id, "Corrupt Game", "C:/Games/Corrupt");
        let corrupt_file = insert_file(&conn, game_id, "negative.bin", -1);
        insert_finding(&conn, corrupt_file, "bonus", "corrupt size", 80, None, None);
        let valid_file = insert_file(&conn, game_id, "valid.bin", 42);
        insert_finding(&conn, valid_file, "bonus", "valid size", 80, None, None);

        let rows = load_findings(&conn).expect("one corrupt row must not abort the load");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].file_id, valid_file);
        assert_eq!(rows[0].size, 42);
    }

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
        assert_eq!(
            row.library,
            Some(LibraryOrigin {
                vendor: Some("steam".to_string()),
                root: PathBuf::from("C:/Games"),
            }),
            "a game row is attributed through its games.library_id"
        );
    }

    /// An orphan row has no game to be attributed through, so its library
    /// comes from the root the scan recorded as its safety evidence, resolved
    /// back to the `game_libraries` row that owns that path. Leaving it blank
    /// would put every leftover outside any launcher grouping.
    #[test]
    fn load_findings_attributes_an_orphan_row_through_its_safety_evidence() {
        let conn = db::open_in_memory().expect("open in-memory db");
        insert_library(&conn, r"F:\SteamLibrary");

        let orphan_full = r"F:\SteamLibrary\steamapps\common\Leftover";
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
            "residue",
            60,
            None,
            None,
        );
        conn.execute(
            "INSERT INTO file_safety \
             (file_id, scan_id, evidence_library_path, trusted_root, rel_path) \
             VALUES (?1, 0, ?2, ?3, 'Leftover')",
            params![
                orphan_file,
                r"F:\SteamLibrary",
                r"F:\SteamLibrary\steamapps\common"
            ],
        )
        .expect("insert orphan safety evidence");

        let rows = load_findings(&conn).expect("load should succeed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].game_id, ORPHAN_GAME_ID);
        assert_eq!(
            rows[0].library,
            Some(LibraryOrigin {
                vendor: Some("steam".to_string()),
                root: PathBuf::from(r"F:\SteamLibrary"),
            }),
            "the orphan must name the library its evidence points at"
        );
    }

    /// Nothing may be fabricated when the attribution genuinely isn't there:
    /// an orphan finding with no safety evidence (a row from a database
    /// written before the evidence existed) has no library root to resolve, so
    /// it loads unattributed rather than borrowing some other library's.
    #[test]
    fn load_findings_leaves_library_unattributed_when_nothing_backs_it() {
        let conn = db::open_in_memory().expect("open in-memory db");
        insert_library(&conn, r"F:\SteamLibrary");

        conn.execute(
            "INSERT INTO files (game_id, rel_path, size, mtime) \
             VALUES (NULL, 'F:\\Elsewhere\\Leftover', 10, NULL)",
            [],
        )
        .expect("insert orphan file");
        let orphan_file = conn.last_insert_rowid();
        insert_finding(
            &conn,
            orphan_file,
            "orphan_folder",
            "residue",
            60,
            None,
            None,
        );

        let rows = load_findings(&conn).expect("load should succeed");

        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].library, None,
            "with no evidence path there is no library to name"
        );
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

    /// An orphaned-residue finding (orphan-residue safety) is stored with a `NULL`
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
