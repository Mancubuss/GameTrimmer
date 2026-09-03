//! Targeted single-game re-trim execution.
//!
//! Re-scans and cleans only the files of an updated game without requiring a
//! full library re-scan. Fails closed if the game executable is currently
//! running, or if that cannot be determined at all - see [`RunningCheck`].

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use rusqlite::Connection;

use crate::error::{CoreError, Result};
use crate::langdetect::LangDetector;
use crate::rules::RuleEngine;
use crate::settings::DeleteMethod;

/// Summary report returned by [`retrim_game`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetrimReport {
    pub game_id: i64,
    pub game_name: String,
    pub files_deleted: usize,
    pub bytes_freed: u64,
    /// Something went wrong: a delete that failed, a micro-stub that could
    /// not be written after one succeeded.
    pub errors: Vec<String>,
    /// Files the run left alone, each with the reason - an anti-cheat
    /// protected game, a multi-asset container no whole-file delete may
    /// touch, an intro this build has no micro-stub for.
    ///
    /// Separate from `errors` because a caller cannot act on "one file was
    /// left alone, exactly as designed" the way it acts on "one file failed",
    /// and a single list forces it to read English to tell them apart.
    ///
    /// One entry kind does not fit that framing: when the anti-cheat check
    /// itself cannot complete (`ops::unattended_skip_reason`'s `Err` arm -
    /// typically an install directory that went unreadable, e.g. a permission
    /// error), the file is still fail-closed into `skipped` rather than
    /// `errors`, because refusing to delete on an unproven verdict is the
    /// deliberate, designed outcome. But the *cause* is a genuine I/O
    /// failure, not an intentional keep, and a caller that only branches on
    /// `errors` vs `skipped` cannot tell the two apart from the shape of this
    /// struct alone. The distinction survives in the reason text: an
    /// entry whose reason contains "could not complete" is this case: an
    /// operational problem worth surfacing to the user (fix permissions,
    /// investigate the drive), not a normal anti-cheat keep. Every other
    /// `skipped` entry is the deliberate kind and has no such phrase.
    pub skipped: Vec<String>,
}

/// Outcome of asking the OS whether a game's executable is currently
/// running.
///
/// Not a `bool`. A bare `bool` can only say "running" or "not running", and
/// that forces a caller who can't get a real answer - `K32EnumProcesses`
/// failed, or the process list didn't fit in a fixed buffer - to collapse
/// their uncertainty into one of those two values. Every such collapse in
/// the wild has picked `false` ("not running"), because that is the value
/// that lets the rest of the function keep going. `Unknown` exists so that
/// choice has to be made explicitly, once, in [`RunningCheck::blocks_deletion`]
/// - and made the safe way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunningCheck {
    /// A process under `install_dir` was found.
    Running,
    /// The full process list was enumerated and none matched.
    NotRunning,
    /// The process list could not be trusted - the enumeration API failed,
    /// or returned exactly as many process IDs as the fixed-size buffer
    /// holds, which Win32 does not distinguish from "the real list is
    /// longer than this buffer and got truncated".
    Unknown,
}

impl RunningCheck {
    /// Whether a caller about to delete files should refuse to proceed.
    ///
    /// True for both [`RunningCheck::Running`] and [`RunningCheck::Unknown`],
    /// since an ambiguous answer is treated exactly like a positive one -
    /// the entire point of having three variants instead of a `bool`.
    pub fn blocks_deletion(self) -> bool {
        !matches!(self, RunningCheck::NotRunning)
    }
}

/// Decides whether a `K32EnumProcesses` result is trustworthy enough to
/// search for a match, without looking at the process IDs themselves.
///
/// Split out from [`is_game_running`] so both failure modes - `api_ok` false
/// (the call itself failed) and `count >= capacity` (the buffer may have
/// been truncated, since Win32 reports "bytes written" rather than "process
/// count would have been") - can be exercised by a unit test without a real
/// Win32 failure. `count == capacity` is treated as untrustworthy on
/// purpose: `K32EnumProcesses` gives no signal to tell an exact fit apart
/// from a truncated one, so a full buffer has to be assumed truncated.
fn enum_processes_is_trustworthy(api_ok: bool, count: usize, capacity: usize) -> bool {
    api_ok && count < capacity
}

/// Checks whether any executable running on the system originates from `install_dir`.
#[cfg(windows)]
pub fn is_game_running(install_dir: &Path) -> RunningCheck {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::ProcessStatus::K32EnumProcesses;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let target_dir = match install_dir.canonicalize() {
        Ok(p) => p,
        Err(_) => install_dir.to_path_buf(),
    };
    let target_dir_str = target_dir.to_string_lossy().to_lowercase();

    let mut pids = [0u32; 2048];
    let mut bytes_returned = 0u32;
    // SAFETY: `pids` is a stack array of `pids.len()` `u32`s and the byte
    // length passed is exactly `pids.len() * size_of::<u32>()`, so
    // `K32EnumProcesses` cannot write past the end of the array; it reports
    // how many bytes it actually wrote back through `bytes_returned`.
    let ok = unsafe {
        K32EnumProcesses(
            pids.as_mut_ptr(),
            (pids.len() * std::mem::size_of::<u32>()) as u32,
            &mut bytes_returned,
        )
    };

    let count = (bytes_returned as usize) / std::mem::size_of::<u32>();
    if !enum_processes_is_trustworthy(ok.as_bool(), count, pids.len()) {
        // Either the enumeration API failed outright, or it may have handed
        // back a truncated list - either way we cannot say the game isn't
        // running, so fail closed rather than search a list we don't trust.
        return RunningCheck::Unknown;
    }

    let mut image_path = [0u16; 1024];

    for &pid in &pids[..count] {
        if pid == 0 {
            continue;
        }
        // SAFETY: `pid` came from the trusted portion of `pids`, which
        // `K32EnumProcesses` just filled; `OpenProcess` either returns a
        // handle we close below or an error we skip past.
        let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
            Ok(h) if !h.is_invalid() => h,
            _ => continue,
        };

        let mut size = image_path.len() as u32;
        // SAFETY: `image_path` is a stack array of `image_path.len()` `u16`s
        // and `size` is initialized to that length, which
        // `QueryFullProcessImageNameW` treats as the buffer's capacity in
        // characters; it writes the actual length used back into `size`.
        let success = unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_FORMAT(0),
                windows::core::PWSTR(image_path.as_mut_ptr()),
                &mut size,
            )
        };
        // SAFETY: `handle` was opened above by this same iteration and is
        // not used again after this point.
        let _ = unsafe { CloseHandle(handle) };

        if success.is_ok() && size > 0 {
            let exe_path_str =
                String::from_utf16_lossy(&image_path[..size as usize]).to_lowercase();
            let exe_path = PathBuf::from(&exe_path_str);
            let exe_canon = exe_path.canonicalize().unwrap_or(exe_path);
            let exe_canon_str = exe_canon.to_string_lossy().to_lowercase();

            if exe_path_str.starts_with(&target_dir_str)
                || exe_canon_str.starts_with(&target_dir_str)
            {
                return RunningCheck::Running;
            }
        }
    }

    RunningCheck::NotRunning
}

/// Non-Windows fallback for process checking.
///
/// This crate's real target is Windows-only - `is_game_running` exists to
/// query `K32EnumProcesses`, which has no counterpart to fall back to here.
/// `NotRunning` (rather than `Unknown`) keeps this branch harmless for
/// cross-compiling and running the test suite on a non-Windows host; nothing
/// in a real deployment ever executes it.
#[cfg(not(windows))]
pub fn is_game_running(_install_dir: &Path) -> RunningCheck {
    RunningCheck::NotRunning
}

/// What an unattended re-trim classifies with: the two engines, the user's
/// category setting, and the flag that stops it.
///
/// Grouped rather than passed as four more parameters because they always
/// travel together and are handed straight to the shared classification
/// cycle - the same reason [`crate::worker::GameIdentity`] exists.
///
/// [`ClassifyPolicy::imported_rules`] is deliberately *not* in here.
/// Unattended is unattended: an imported rule's match is refused because
/// nobody is looking, and that is not a caller's to soften.
///
/// [`ClassifyPolicy::imported_rules`]: crate::worker::ClassifyPolicy::imported_rules
pub struct RetrimContext<'a> {
    pub rule_engine: &'a RuleEngine,
    pub lang_detector: &'a LangDetector,
    /// The user's persisted `enabled_categories`. Re-trim used to have no
    /// notion of it, so a category switched off in the window was still
    /// deleted here after a game update.
    pub enabled_categories: &'a [String],
    /// Polled inside the classification cycle's hot loops, so a Stop reaches
    /// a 400 000-file game mid-classification rather than after it.
    pub cancel: &'a AtomicBool,
}

/// Executes a targeted re-trim for a single game by its database ID.
///
/// The uncancellable, every-category-enabled convenience wrapper over
/// [`retrim_game_with_new_build`] - the same shape
/// [`crate::scanner::scan_dir`] and
/// [`crate::langdetect::LangDetector::analyze_game`] have. A real unattended
/// run builds a [`RetrimContext`] with the user's settings and a flag it can
/// set; this one is for tests and for a caller with neither.
pub fn retrim_game(
    conn: &mut Connection,
    game_id: i64,
    rule_engine: &RuleEngine,
    lang_detector: &LangDetector,
    delete_method: DeleteMethod,
) -> Result<RetrimReport> {
    retrim_game_with_new_build(
        conn,
        game_id,
        None,
        &RetrimContext {
            rule_engine,
            lang_detector,
            enabled_categories: &[],
            cancel: &AtomicBool::new(false),
        },
        delete_method,
    )
}

/// Executes a targeted re-trim for a single game, optionally updating its `build_id`.
///
/// `enabled_categories` is the user's own setting, passed straight to the
/// shared classification cycle. Re-trim used to have no notion of it at all,
/// which meant a category switched off in the window was still deleted here,
/// unattended, after a game update.
///
/// `cancel` is polled inside the cycle's hot loops, so a Stop reaches a
/// 400 000-file game in the middle of classification rather than after it.
pub fn retrim_game_with_new_build(
    conn: &mut Connection,
    game_id: i64,
    new_build_id: Option<&str>,
    ctx: &RetrimContext<'_>,
    delete_method: DeleteMethod,
) -> Result<RetrimReport> {
    // 1. Fetch game record
    let game = crate::gamestate::find_game_by_id(conn, game_id)?
        .ok_or_else(|| CoreError::Other(format!("game id {game_id} not found in database")))?;

    let install_path = PathBuf::from(&game.install_dir);
    if !install_path.is_dir() {
        return Err(CoreError::Other(format!(
            "install directory for \"{}\" does not exist or is not a directory: {}",
            game.name,
            install_path.display()
        )));
    }

    // 2. Fail-closed if the game executable is currently running, or if we
    // could not determine that with confidence - see `RunningCheck`.
    match is_game_running(&install_path) {
        RunningCheck::NotRunning => {}
        RunningCheck::Running => {
            return Err(CoreError::Other(format!(
                "cannot re-trim \"{}\": game executable is currently running",
                game.name
            )));
        }
        RunningCheck::Unknown => {
            return Err(CoreError::Other(format!(
                "cannot re-trim \"{}\": could not verify whether the game executable is running, refusing to delete",
                game.name
            )));
        }
    }

    // 3. Scan the game's directory
    let entries = crate::scanner::scan_dir(&install_path)?;
    let stats_before = crate::scanner::ScanStats::of(&entries);

    // 4. Classify through the one classification cycle
    // (`crate::worker::classify_game`) - the same eight steps, in the same
    // order, as the interactive scan. This used to be a second copy of that
    // loop, written out by hand here because the original lived a crate
    // higher and could not be called; it had drifted on three of the eight
    // steps, and nothing could catch that, because nothing calls re-trim yet.
    //
    // What is genuinely different between the two callers is the policy, and
    // only the policy: nobody is watching this run, so an imported rule's
    // match is refused rather than shown for review.
    let prepared = crate::worker::classify_game(
        ctx.rule_engine,
        ctx.lang_detector,
        crate::worker::GameIdentity {
            id: game.id,
            name: &game.name,
            install_dir: &install_path,
            app_id: game.app_id.as_deref(),
        },
        entries,
        &crate::worker::ClassifyPolicy {
            enabled_categories: ctx.enabled_categories,
            imported_rules: crate::worker::ImportedRules::Refused,
        },
        ctx.cancel,
    )?;

    if prepared.imported_rules_refused {
        return Err(CoreError::Other(
            "automatic re-trim blocked: an imported rule matched and requires explicit review"
                .to_string(),
        ));
    }

    let entries = prepared.entries;
    let candidates = prepared.findings;

    // 4b. Apply the micro-stub contract (see `crate::stub`) to every "intro"
    // candidate before anything is deleted - identification has to happen
    // while the file still exists, because after deletion there is nothing
    // left to sniff. A container this build has no stub for is dropped from
    // the candidate list entirely rather than deleted and left stub-less: an
    // intro video replaced by nothing is exactly the boot crash the stub
    // contract exists to prevent, and retrim runs unattended with no human
    // watching to catch it. Non-"intro" candidates are untouched by this step.
    let mut retained_candidates = Vec::with_capacity(candidates.len());
    let mut candidate_stub_bytes: Vec<Option<Vec<u8>>> = Vec::with_capacity(candidates.len());
    let mut skipped = Vec::new();

    for candidate in candidates {
        if crate::worker::display_category(candidate.source)
            != crate::worker::DisplayCategory::Intro
        {
            retained_candidates.push(candidate);
            candidate_stub_bytes.push(None);
            continue;
        }
        let full_path = install_path.join(&candidate.rel_path);
        match crate::stub::detect_stub_bytes(&full_path) {
            Some(bytes) => {
                retained_candidates.push(candidate);
                candidate_stub_bytes.push(Some(bytes));
            }
            None => {
                skipped.push(format!(
                    "kept {}: its video container is not one this build has a micro-stub for; \
                     deleting it would leave the game with no file there at all",
                    candidate.rel_path
                ));
            }
        }
    }
    let candidates = retained_candidates;

    if candidates.is_empty() {
        let final_build_id = new_build_id.or(game.build_id.as_deref());
        conn.execute(
            "UPDATE games SET files = ?2, bytes = ?3, bytes_on_disk = ?4, build_id = ?5 WHERE id = ?1",
            rusqlite::params![
                game.id,
                stats_before.files as i64,
                stats_before.bytes as i64,
                stats_before.bytes_on_disk as i64,
                final_build_id,
            ],
        )?;

        return Ok(RetrimReport {
            game_id: game.id,
            game_name: game.name,
            files_deleted: 0,
            bytes_freed: 0,
            errors: Vec::new(),
            skipped,
        });
    }

    // 5. Ensure an active scan generation and safety evidence exist
    let scan_id = match crate::db::active_scan_id(conn)? {
        Some(id) if crate::db::scan_allows_deletion(conn, id).unwrap_or(false) => id,
        _ => {
            let id = crate::db::begin_scan(conn, "complete")?;
            crate::db::activate_scan(conn, id)?;
            id
        }
    };

    let lib_path: String = conn
        .query_row(
            "SELECT path FROM game_libraries WHERE id = ?1",
            [game.library_id],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| game.install_dir.clone());

    conn.execute(
        "INSERT OR REPLACE INTO scan_library_evidence (scan_id, library_path, provider, status)
         VALUES (?1, ?2, 'retrim', 'complete')",
        rusqlite::params![scan_id, &lib_path],
    )?;

    // Clean previous findings/safety for this game
    conn.execute(
        "DELETE FROM findings WHERE file_id IN (SELECT id FROM files WHERE game_id = ?1)",
        [game.id],
    )?;
    conn.execute(
        "DELETE FROM file_safety WHERE file_id IN (SELECT id FROM files WHERE game_id = ?1)",
        [game.id],
    )?;
    conn.execute("DELETE FROM files WHERE game_id = ?1", [game.id])?;

    // 6. Build safety snapshots and delete plans
    let mut candidate_file_ids = Vec::new();
    let mut candidate_sizes = Vec::new();
    let mut candidate_sizes_on_disk = Vec::new();
    // Stays aligned with `candidate_file_ids`/`candidate_sizes` (pushed together
    // below), not with the original `candidates`/`candidate_stub_bytes` - a
    // candidate whose safety snapshot fails never gets a row, and its stub
    // entry must not shift every later candidate's stub out of place.
    let mut candidate_stubs: Vec<Option<Vec<u8>>> = Vec::new();

    for (candidate, stub) in candidates.into_iter().zip(candidate_stub_bytes) {
        // The snapshot the cycle already took while walking this game, not a
        // second capture of the same file: `classify_game` captures through
        // one `SnapshotCapture` per game, which is what shares the trusted
        // root and the intermediate directories across a game's findings
        // instead of re-proving them per file. A capture that failed is
        // still the same "no evidence, no row" it always was.
        let Ok(snapshot) = candidate.safety else {
            continue;
        };
        let entry = &entries[candidate.entry_index];

        conn.execute(
            "INSERT INTO files (scan_id, game_id, rel_path, size, size_on_disk, mtime)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                scan_id,
                game.id,
                &candidate.rel_path,
                candidate.size as i64,
                candidate.size_on_disk as i64,
                entry.mtime,
            ],
        )?;
        let file_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO findings (file_id, category, rule_id, confidence, lang_tag, group_dir, provenance)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                file_id,
                // The granular key the rest of the app reads back
                // (`worker::parse_source_key`). This used to write the word
                // "localization" for every language finding - a string that
                // is not one of the keys, so the loader skipped the row and
                // the finding silently vanished on the next load.
                crate::worker::source_key(candidate.source),
                candidate.rule_id,
                candidate.confidence,
                candidate.lang_tag,
                candidate.group_dir,
                match candidate.provenance {
                    crate::rules::RuleProvenance::Builtin => "builtin",
                    crate::rules::RuleProvenance::ImportedUntrusted => "imported_untrusted",
                },
            ],
        )?;

        let root_identity = snapshot.root_identity.encode();
        let target_identity = snapshot.target_identity.encode();
        conn.execute(
            "INSERT OR REPLACE INTO file_safety
             (file_id, scan_id, evidence_library_path, trusted_root, rel_path, root_identity,
              target_identity, target_kind, tree_fingerprint, block_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL)",
            rusqlite::params![
                file_id,
                scan_id,
                &lib_path,
                snapshot.trusted_root.to_string_lossy(),
                snapshot.rel_path.to_string_lossy(),
                root_identity,
                target_identity,
                snapshot.target_identity.kind.as_str(),
                snapshot.tree_fingerprint.as_deref(),
            ],
        )?;

        candidate_sizes.push(candidate.size);
        candidate_sizes_on_disk.push(candidate.size_on_disk);
        candidate_file_ids.push(file_id);
        candidate_stubs.push(stub);
    }

    if candidate_file_ids.is_empty() {
        let final_build_id = new_build_id.or(game.build_id.as_deref());
        conn.execute(
            "UPDATE games SET files = ?2, bytes = ?3, bytes_on_disk = ?4, build_id = ?5 WHERE id = ?1",
            rusqlite::params![
                game.id,
                stats_before.files as i64,
                stats_before.bytes as i64,
                stats_before.bytes_on_disk as i64,
                final_build_id,
            ],
        )?;

        return Ok(RetrimReport {
            game_id: game.id,
            game_name: game.name,
            files_deleted: 0,
            bytes_freed: 0,
            errors: Vec::new(),
            skipped,
        });
    }

    // 7. Execute delete plans. A removed "intro" candidate has its
    // micro-stub written from inside `on_outcome`, while the file it belongs
    // to still exists nowhere else to look up its bytes from - this mirrors
    // `crates/app/src/worker/delete.rs::run_delete`, the reference
    // implementation of this same stub contract. A stub write that fails is
    // never silently dropped: it lands in `stub_write_failures` and is folded
    // into this report's `errors` below rather than only reaching a log line.
    let (plans, skips) = crate::ops::prepare_delete_plans_with_skips(
        conn,
        &candidate_file_ids,
        delete_method,
        crate::ops::DeleteAttendance::Unattended,
    )?;
    // A multi-asset container the preflight held back leaves the batch, so the
    // per-candidate arrays have to lose the same entry: they are read by plan
    // index below, and a shifted one would write a stub over the wrong file.
    if !skips.is_empty() {
        let keep: Vec<bool> = candidate_file_ids
            .iter()
            .map(|file_id| !skips.iter().any(|skip| skip.file_id == *file_id))
            .collect();
        for skip in &skips {
            skipped.push(format!("{}: {}", skip.path.display(), skip.reason));
        }
        retain_by_flags(&mut candidate_file_ids, &keep);
        retain_by_flags(&mut candidate_sizes, &keep);
        retain_by_flags(&mut candidate_sizes_on_disk, &keep);
        retain_by_flags(&mut candidate_stubs, &keep);
    }
    let mut stub_write_failures: Vec<String> = Vec::new();
    let outcomes = crate::ops::execute_delete_plans_observed(
        conn,
        delete_method,
        &plans,
        |_current, _total, _path| {},
        |index, outcome| {
            if outcome.status == crate::ops::FsOutcome::Removed {
                if let Some(bytes) = &candidate_stubs[index] {
                    if let Err(err) = crate::stub::write_stub(&outcome.path, bytes) {
                        stub_write_failures.push(format!(
                            "{}: the intro micro-stub could not be written after the delete, \
                             so the game may not start: {err}",
                            outcome.path.display()
                        ));
                    }
                }
            }
        },
    )?;

    // 8. Tally results
    let mut files_deleted = 0;
    let mut bytes_freed = 0u64;
    let mut bytes_on_disk_freed = 0u64;
    let mut errors = stub_write_failures;

    for (idx, outcome) in outcomes.iter().enumerate() {
        if matches!(
            outcome.status,
            crate::ops::FsOutcome::Removed | crate::ops::FsOutcome::AlreadyAbsent
        ) {
            files_deleted += 1;
            bytes_freed = bytes_freed.saturating_add(candidate_sizes[idx]);
            bytes_on_disk_freed = bytes_on_disk_freed.saturating_add(candidate_sizes_on_disk[idx]);
        }
        if let Some(err) = &outcome.error {
            errors.push(format!("{}: {}", outcome.path.display(), err));
        }
    }

    // 9. Update games row
    let final_build_id = new_build_id.or(game.build_id.as_deref());
    let final_files = stats_before.files.saturating_sub(files_deleted as u64);
    let final_bytes = stats_before.bytes.saturating_sub(bytes_freed);
    let final_bytes_on_disk = stats_before
        .bytes_on_disk
        .saturating_sub(bytes_on_disk_freed);

    conn.execute(
        "UPDATE games SET files = ?2, bytes = ?3, bytes_on_disk = ?4, build_id = ?5 WHERE id = ?1",
        rusqlite::params![
            game.id,
            final_files as i64,
            final_bytes as i64,
            final_bytes_on_disk as i64,
            final_build_id,
        ],
    )?;

    Ok(RetrimReport {
        game_id: game.id,
        game_name: game.name,
        files_deleted,
        bytes_freed,
        errors,
        skipped,
    })
}

/// Drops the entries of `items` whose position is `false` in `keep`, so a set
/// of parallel per-candidate arrays can lose the same entry together.
fn retain_by_flags<T>(items: &mut Vec<T>, keep: &[bool]) {
    let mut index = 0;
    items.retain(|_| {
        let retained = keep.get(index).copied().unwrap_or(true);
        index += 1;
        retained
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_game(temp_dir: &tempfile::TempDir, game_dir: &Path) -> Connection {
        fs::create_dir_all(game_dir).expect("create game dir");
        let conn = crate::db::open_in_memory().expect("open memory db");
        conn.execute(
            "INSERT INTO game_libraries (id, vendor, path) VALUES (1, 'steam', ?1)",
            [temp_dir.path().to_string_lossy()],
        )
        .expect("insert library");
        conn.execute(
            // The verdict column is not nullable in practice: every game a
            // scan writes carries one, and a NULL reads as protected, which
            // would make an unattended re-trim skip the whole fixture.
            "INSERT INTO games (id, library_id, name, install_dir, app_id, build_id,
                                anti_cheat_protected)
             VALUES (10, 1, 'Test Game', ?1, 'test-app', '100', 0)",
            [game_dir.to_string_lossy()],
        )
        .expect("insert game");
        conn
    }

    #[test]
    fn retrim_game_deletes_flagged_files_and_updates_database() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let game_dir = temp_dir.path().join("Portal 2");
        fs::create_dir_all(&game_dir).expect("create game dir");

        // Create synthetic game files
        let game_exe = game_dir.join("portal2.exe");
        fs::write(&game_exe, b"MZ_GAME_BINARY").expect("write exe");

        let redist_dir = game_dir.join("_CommonRedist");
        fs::create_dir_all(&redist_dir).expect("create redist dir");
        let vcredist = redist_dir.join("vcredist_x64.exe");
        fs::write(&vcredist, vec![0u8; 1024]).expect("write vcredist");

        let bonus_dir = game_dir.join("Bonus");
        fs::create_dir_all(&bonus_dir).expect("create bonus dir");
        let soundtrack = bonus_dir.join("soundtrack.mp3");
        fs::write(&soundtrack, vec![0u8; 2048]).expect("write soundtrack");

        // Database setup
        let mut conn = crate::db::open_in_memory().expect("open memory db");
        let lib_path = temp_dir.path().to_string_lossy().to_string();
        conn.execute(
            "INSERT INTO game_libraries (id, vendor, path) VALUES (1, 'steam', ?1)",
            [&lib_path],
        )
        .expect("insert library");

        let install_dir_str = game_dir.to_string_lossy().to_string();
        conn.execute(
            "INSERT INTO games (id, library_id, name, install_dir, app_id, build_id,
                                anti_cheat_protected)
             VALUES (10, 1, 'Portal 2', ?1, '620', '100', 0)",
            [&install_dir_str],
        )
        .expect("insert game");

        let rule_json = format!(
            r#"{{"version":{},"rules":[
                {{"category": "redist_folder", "pattern": "^_?commonredist(s)?$", "desc": "Common redist folder", "confidence": 90}},
                {{"category": "bonus", "pattern": "^bonus$", "desc": "Bonus material", "confidence": 85}}
            ]}}"#,
            crate::rules::RULE_PACK_VERSION
        );
        let rule_engine = RuleEngine::from_json(&rule_json).expect("parse rules");
        let lang_detector = LangDetector::new();

        let report = retrim_game_with_new_build(
            &mut conn,
            10,
            Some("200"),
            &RetrimContext {
                rule_engine: &rule_engine,
                lang_detector: &lang_detector,
                enabled_categories: &[],
                cancel: &AtomicBool::new(false),
            },
            DeleteMethod::Permanent,
        )
        .expect("execute retrim");

        assert_eq!(report.game_id, 10);
        assert_eq!(report.game_name, "Portal 2");
        assert_eq!(report.files_deleted, 2);
        assert_eq!(report.bytes_freed, 3072);
        assert!(report.errors.is_empty());

        // Verify files on disk
        assert!(game_exe.exists(), "game executable must remain intact");
        assert!(!vcredist.exists(), "vcredist must be deleted");
        assert!(!soundtrack.exists(), "soundtrack must be deleted");

        // Verify DB update
        let updated_build_id: String = conn
            .query_row("SELECT build_id FROM games WHERE id = 10", [], |row| {
                row.get(0)
            })
            .expect("query updated build_id");
        assert_eq!(updated_build_id, "200");
    }

    #[test]
    fn retrim_non_existent_game_returns_error() {
        let mut conn = crate::db::open_in_memory().expect("open memory db");
        let rule_engine =
            RuleEngine::from_json(crate::rules::BUILTIN_RULES_JSON).expect("builtin rules");
        let lang_detector = LangDetector::new();

        let result = retrim_game(
            &mut conn,
            999,
            &rule_engine,
            &lang_detector,
            DeleteMethod::Permanent,
        );
        assert!(result.is_err());
    }

    #[test]
    fn enum_processes_is_trustworthy_fails_closed_on_api_error() {
        // The enumeration call itself failed - `count` is whatever garbage
        // was left in `bytes_returned`, and must not be trusted either.
        assert!(!enum_processes_is_trustworthy(false, 0, 2048));
        assert!(!enum_processes_is_trustworthy(false, 5, 2048));
    }

    #[test]
    fn enum_processes_is_trustworthy_fails_closed_on_a_full_buffer() {
        // A count equal to capacity is indistinguishable from a truncated
        // list - Win32 reports bytes written, not "there were more".
        assert!(!enum_processes_is_trustworthy(true, 2048, 2048));
    }

    #[test]
    fn enum_processes_is_trustworthy_trusts_a_normal_result() {
        assert!(enum_processes_is_trustworthy(true, 0, 2048));
        assert!(enum_processes_is_trustworthy(true, 137, 2048));
        assert!(enum_processes_is_trustworthy(true, 2047, 2048));
    }

    #[test]
    fn running_check_blocks_deletion_for_running_and_unknown_only() {
        assert!(RunningCheck::Running.blocks_deletion());
        assert!(RunningCheck::Unknown.blocks_deletion());
        assert!(!RunningCheck::NotRunning.blocks_deletion());
    }

    #[test]
    fn imported_rule_match_blocks_automatic_retrim_without_deleting() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let game_dir = temp_dir.path().join("Imported Rule Game");
        let mut conn = setup_game(&temp_dir, &game_dir);
        let target = game_dir.join("manual.txt");
        fs::write(&target, b"ordinary document").expect("write target");
        let rules = format!(
            r#"{{"version":{},"rules":[{{"category":"docs_file","pattern":"^manual\\.txt$","desc":"Imported docs rule","confidence":90,"provenance":"imported_untrusted"}}]}}"#,
            crate::rules::RULE_PACK_VERSION
        );
        let engine = RuleEngine::from_json(&rules).expect("parse imported rule");

        let error = retrim_game(
            &mut conn,
            10,
            &engine,
            &LangDetector::new(),
            DeleteMethod::Permanent,
        )
        .expect_err("imported match must block unattended re-trim");

        assert!(error.to_string().contains("imported rule matched"));
        assert!(target.is_file());
    }

    #[test]
    /// GT-216, closed. This used to assert the opposite - a trusted rule
    /// naming the file, so an unattended re-trim removed it, EAC marker and
    /// all - and said so loudly, on purpose, to record a real gap:
    /// `retrim_game` had no anti-cheat gate of its own, and the marker below
    /// was setup nothing read.
    ///
    /// `prepare_delete_plans_for_action` (crates/core/src/ops.rs) now reads
    /// it: when the preflight runs `DeleteAttendance::Unattended` and the
    /// file's game is anti-cheat protected, the file is skipped rather than
    /// deleted, and the skip is reported. Ticking the box by hand is still
    /// consent enough for the interactive delete path - this only closes the
    /// unattended one, where nobody is present to give it.
    fn unattended_retrim_refuses_to_delete_in_an_anti_cheat_game() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let game_dir = temp_dir.path().join("EAC Game");
        let mut conn = setup_game(&temp_dir, &game_dir);
        fs::create_dir_all(game_dir.join("EasyAntiCheat")).expect("create EAC marker");
        fs::write(
            game_dir.join("EasyAntiCheat").join("EasyAntiCheat.exe"),
            b"MZ",
        )
        .expect("write EAC marker");
        let target = game_dir.join("manual.txt");
        fs::write(&target, b"bytes do not matter, only the path does")
            .expect("write disguised archive");
        let rules = format!(
            r#"{{"version":{},"rules":[{{"category":"docs_file","pattern":"^manual\\.txt$","desc":"Trusted docs rule","confidence":90}}]}}"#,
            crate::rules::RULE_PACK_VERSION
        );
        let engine = RuleEngine::from_json(&rules).expect("parse trusted rule");

        let report = retrim_game(
            &mut conn,
            10,
            &engine,
            &LangDetector::new(),
            DeleteMethod::Permanent,
        )
        .expect("re-trim runs - a skip is reported, not a hard failure");

        assert_eq!(
            report.files_deleted, 0,
            "no one is present to consent, so an unattended re-trim leaves an \
             anti-cheat-protected game alone"
        );
        assert!(
            report.errors.is_empty(),
            "a deliberate skip is not a failure: {:?}",
            report.errors
        );
        assert_eq!(
            report.skipped.len(),
            1,
            "the skip must be reported, not silently dropped: {:?}",
            report.skipped
        );
        assert!(
            report.skipped[0].contains("manual.txt"),
            "the report must say which file was left alone: {:?}",
            report.skipped
        );
        assert!(
            target.is_file(),
            "the file survives - the game's EasyAntiCheat marker is what saves it"
        );
    }

    #[test]
    fn ordinary_localization_category_remains_retrim_deletable() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let game_dir = temp_dir.path().join("Localized Game");
        let mut conn = setup_game(&temp_dir, &game_dir);
        let target = game_dir
            .join("localization")
            .join("french")
            .join("captions.json");
        fs::create_dir_all(target.parent().expect("localization parent"))
            .expect("create localization tree");
        fs::write(&target, br#"{"caption":"bonjour"}"#).expect("write localization");
        let rules = format!(
            r#"{{"version":{},"rules":[]}}"#,
            crate::rules::RULE_PACK_VERSION
        );
        let engine = RuleEngine::from_json(&rules).expect("parse empty rule pack");

        let report = retrim_game(
            &mut conn,
            10,
            &engine,
            &LangDetector::new(),
            DeleteMethod::Permanent,
        )
        .expect("valid localization cleanup remains supported");

        assert_eq!(report.files_deleted, 1);
        assert!(!target.exists());
        let category: String = conn
            .query_row("SELECT category FROM findings LIMIT 1", [], |row| {
                row.get(0)
            })
            .expect("persisted localization finding");
        // The granular key, which is what `worker::parse_source_key` reads
        // back. This assertion used to demand the word "localization", and
        // that is not one of the keys: every row re-trim wrote for a
        // language finding was skipped by the loader, so the finding
        // disappeared from the window on the next load and only the deleted
        // bytes were left to show for it. The scan has always written the
        // granular key here; sharing one cycle is what made re-trim agree.
        assert_eq!(category, "loc_text");
        assert_eq!(
            crate::worker::parse_source_key(&category),
            Some(crate::worker::FindingSource::Loc(
                crate::langdetect::LangKind::Text
            )),
            "whatever re-trim writes has to survive the round trip the loader makes"
        );
    }

    /// GT-448: a category the user switched off in the window is not
    /// deleted here either.
    ///
    /// Unattended re-trim had no notion of `enabled_categories` at all - it
    /// never read the setting, so a user who had unticked "Documentation"
    /// still lost their documentation after a game update, with no dialog to
    /// object to. Same class as GT-205 (the daemon ignoring
    /// `excluded_libraries`), minus the confirmation step.
    ///
    /// The counterpart is the second half of the test: the identical file,
    /// the identical rule, an empty list - and it goes. If both halves
    /// start agreeing, the filter has stopped filtering, not started.
    #[test]
    fn a_category_the_user_switched_off_is_not_deleted_unattended() {
        let docs_rule = format!(
            r#"{{"version":{},"rules":[
                {{"category":"docs_file","pattern":"manual","desc":"Manual","confidence":90}}
            ]}}"#,
            crate::rules::RULE_PACK_VERSION
        );
        let engine = RuleEngine::from_json(&docs_rule).expect("parse rules");

        for (enabled, expect_deleted) in
            [(vec!["redist".to_string()], 0usize), (Vec::new(), 1usize)]
        {
            let temp_dir = tempfile::tempdir().expect("create temp dir");
            let game_dir = temp_dir.path().join("Documented Game");
            let mut conn = setup_game(&temp_dir, &game_dir);
            let target = game_dir.join("manual.pdf");
            fs::write(&target, b"a manual").expect("write manual");

            let report = retrim_game_with_new_build(
                &mut conn,
                10,
                None,
                &RetrimContext {
                    rule_engine: &engine,
                    lang_detector: &LangDetector::new(),
                    enabled_categories: &enabled,
                    cancel: &AtomicBool::new(false),
                },
                DeleteMethod::Permanent,
            )
            .expect("re-trim runs whether or not the category is enabled");

            assert_eq!(
                report.files_deleted, expect_deleted,
                "enabled_categories = {enabled:?}"
            );
            assert_eq!(
                target.exists(),
                expect_deleted == 0,
                "the file itself has to follow the setting, not just the count"
            );
        }
    }

    /// GT-449: a container the localization detector flags is still refused,
    /// because the manual scan refuses it too.
    ///
    /// The rule engine's own container veto cannot cover this: it answers
    /// `Unmatched`, which is the very verdict that hands the file to the
    /// localization findings. A `.pck` under a language folder looks to the
    /// detector exactly like the `.json` next to it - and the detector is the
    /// one that has no idea an archive holds more than one asset.
    ///
    /// The counterpart is `ordinary_localization_category_remains_retrim_deletable`
    /// above: same tree, same language folder, an ordinary file - deleted.
    /// If that one starts failing too, the veto has grown past containers.
    ///
    /// The fixture is `SoundbanksNoMedia.pck` (Albion Online's real bank),
    /// which the detector reads as Norwegian out of the English word "No" in
    /// "NoMedia". It used to be `localization/french/voices.pck`, but GT-454
    /// made a container under a language folder a *single-language* file -
    /// Mafia III ships 744 of them - so that path is now legitimately
    /// deletable and no longer tests the veto. What still does is the
    /// boundary the container gate keeps and the detector does not: the
    /// language has to be its own atom of the name, never a syllable inside
    /// a longer word.
    #[test]
    fn a_localization_flagged_container_is_never_deleted_unattended() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let game_dir = temp_dir.path().join("Container Game");
        let mut conn = setup_game(&temp_dir, &game_dir);
        let target = game_dir.join("sound").join("SoundbanksNoMedia.pck");
        fs::create_dir_all(target.parent().expect("sound parent")).expect("create sound tree");
        fs::write(&target, b"AKPK-shaped bytes, many assets inside").expect("write container");
        let rules = format!(
            r#"{{"version":{},"rules":[]}}"#,
            crate::rules::RULE_PACK_VERSION
        );
        let engine = RuleEngine::from_json(&rules).expect("parse empty rule pack");

        let report = retrim_game(
            &mut conn,
            10,
            &engine,
            &LangDetector::new(),
            DeleteMethod::Permanent,
        )
        .expect("a refused container is not an error");

        assert_eq!(report.files_deleted, 0);
        assert!(
            target.exists(),
            "a multi-asset container must survive an unattended re-trim"
        );
    }

    #[test]
    fn intro_candidate_with_recognized_container_is_deleted_and_stubbed() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let game_dir = temp_dir.path().join("Stubbable Intro Game");
        let mut conn = setup_game(&temp_dir, &game_dir);
        let target = game_dir.join("intro.mp4");
        // No recognizable magic bytes on purpose - `detect_stub_bytes` must
        // fall back to the ".mp4" extension, exactly as it does for a real
        // engine intro whose header this build cannot sniff.
        fs::write(&target, b"NOT REAL MP4 BYTES, JUST FILLER CONTENT").expect("write intro");
        let rules = format!(
            r#"{{"version":{},"rules":[{{"category":"intro","pattern":"^intro\\.mp4$","desc":"Intro video","confidence":95}}]}}"#,
            crate::rules::RULE_PACK_VERSION
        );
        let engine = RuleEngine::from_json(&rules).expect("parse intro rule");

        let report = retrim_game(
            &mut conn,
            10,
            &engine,
            &LangDetector::new(),
            DeleteMethod::Permanent,
        )
        .expect("a recognized intro container must be retrimmed, not blocked");

        assert_eq!(report.files_deleted, 1);
        assert!(report.errors.is_empty(), "{:?}", report.errors);

        // The contract: the original bytes are gone, but the path is never
        // empty - a micro-stub with the right container now lives there.
        assert!(
            target.is_file(),
            "intro file must be replaced with a micro-stub, not simply vanish"
        );
        let contents = fs::read(&target).expect("read stubbed intro");
        assert_eq!(
            contents,
            crate::stub::MP4_STUB,
            "must be stubbed with the MP4 micro-stub"
        );
    }

    #[test]
    fn intro_candidate_with_unrecognized_container_is_kept_and_reported() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let game_dir = temp_dir.path().join("Unstubbable Intro Game");
        let mut conn = setup_game(&temp_dir, &game_dir);
        let target = game_dir.join("intro.smk");
        let original = b"NOT A KNOWN CONTAINER AT ALL".to_vec();
        fs::write(&target, &original).expect("write intro");
        let rules = format!(
            r#"{{"version":{},"rules":[{{"category":"intro","pattern":"^intro\\.smk$","desc":"Intro video","confidence":95}}]}}"#,
            crate::rules::RULE_PACK_VERSION
        );
        let engine = RuleEngine::from_json(&rules).expect("parse intro rule");

        let report = retrim_game(
            &mut conn,
            10,
            &engine,
            &LangDetector::new(),
            DeleteMethod::Permanent,
        )
        .expect("an unstubbable intro must be reported, not turned into an error");

        // Fails closed per-file, not as a dropped warning: nothing was
        // deleted, and the reason is visible on the report rather than only
        // in a log line - see `show-found-but-empty-not-silence` in project
        // memory for why silence here would be indistinguishable from a
        // broken detector.
        assert_eq!(report.files_deleted, 0);
        assert_eq!(report.bytes_freed, 0);
        assert!(
            report.errors.is_empty(),
            "keeping a file on purpose is not a failure: {:?}",
            report.errors
        );
        assert_eq!(report.skipped.len(), 1, "{:?}", report.skipped);
        assert!(
            report.skipped[0].contains("intro.smk"),
            "{:?}",
            report.skipped
        );
        assert!(
            report.skipped[0].contains("micro-stub"),
            "{:?}",
            report.skipped
        );

        assert!(
            target.is_file(),
            "an intro with no known stub must never be deleted outright"
        );
        assert_eq!(fs::read(&target).expect("read kept intro"), original);
    }

    #[test]
    fn skipped_intro_does_not_disturb_an_adjacent_deletable_candidate() {
        // Two candidates in one batch: an intro with no stub (must be kept)
        // and an ordinary bonus file (must be deleted). This is the shape
        // that would expose a stub-detection filter that shifts indices and
        // attributes one candidate's outcome to the other - see
        // `candidate_stub_bytes`/`candidate_stubs` alignment in
        // `retrim_game_with_new_build`.
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let game_dir = temp_dir.path().join("Mixed Batch Game");
        let mut conn = setup_game(&temp_dir, &game_dir);

        let intro_path = game_dir.join("intro.smk");
        fs::write(&intro_path, b"NOT A KNOWN CONTAINER AT ALL").expect("write intro");

        let bonus_dir = game_dir.join("Bonus");
        fs::create_dir_all(&bonus_dir).expect("create bonus dir");
        let bonus_path = bonus_dir.join("soundtrack.mp3");
        fs::write(&bonus_path, vec![0u8; 512]).expect("write bonus file");

        let rules = format!(
            r#"{{"version":{},"rules":[
                {{"category": "intro", "pattern": "^intro\\.smk$", "desc": "Intro video", "confidence": 95}},
                {{"category": "bonus", "pattern": "^bonus$", "desc": "Bonus material", "confidence": 85}}
            ]}}"#,
            crate::rules::RULE_PACK_VERSION
        );
        let engine = RuleEngine::from_json(&rules).expect("parse rules");

        let report = retrim_game(
            &mut conn,
            10,
            &engine,
            &LangDetector::new(),
            DeleteMethod::Permanent,
        )
        .expect("a skipped intro must not block the rest of the batch");

        assert_eq!(
            report.files_deleted, 1,
            "only the bonus file should have been deleted"
        );
        assert!(
            report.errors.is_empty(),
            "keeping a file on purpose is not a failure: {:?}",
            report.errors
        );
        assert_eq!(report.skipped.len(), 1, "{:?}", report.skipped);
        assert!(
            report.skipped[0].contains("intro.smk"),
            "{:?}",
            report.skipped
        );

        assert!(intro_path.is_file(), "the unstubbable intro must survive");
        assert_eq!(
            fs::read(&intro_path).expect("read kept intro"),
            b"NOT A KNOWN CONTAINER AT ALL"
        );
        assert!(!bonus_path.exists(), "the bonus file must still be deleted");
    }

    /// The unattended half of the same guard, drawing the same line: the
    /// startup screen goes in every language, the kept-language reel stays.
    /// Re-trim runs with nobody watching, and two paths disagreeing about one
    /// file is the failure GT-206 exists to fix, which is why both call one
    /// policy function.
    ///
    /// The pack here is written inline and sets `localized_content` itself:
    /// no rule the repo ships does any more (see
    /// `worker::tests::no_builtin_rule_marks_itself_as_localized_content`),
    /// so this is the shape a *personal or imported* pack would take - which
    /// is the only way the veto reaches re-trim now, and exactly why it still
    /// has to hold there.
    #[test]
    fn retrim_stubs_a_startup_screen_but_not_kept_language_content() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let game_dir = temp_dir.path().join("Localized Intro Game");
        let mut conn = setup_game(&temp_dir, &game_dir);

        let screen = game_dir.join("legal_german.mp4");
        let kept = game_dir.join("movies").join("german").join("attract.mp4");
        let removable = game_dir.join("movies").join("french").join("attract.mp4");
        for path in [&screen, &kept, &removable] {
            fs::create_dir_all(path.parent().expect("parent")).expect("create tree");
            fs::write(path, b"ORIGINAL VIDEO BYTES, NOT A REAL CONTAINER").expect("write video");
        }

        let rules = format!(
            r#"{{"version":{},"rules":[
                {{"category":"intro","pattern":"^legal.*\\.mp4$","desc":"Legal screen","confidence":95}},
                {{"category":"intro","pattern":"^attract\\.mp4$","desc":"Attract reel","confidence":80,"max_depth":4,"localized_content":true}}
            ]}}"#,
            crate::rules::RULE_PACK_VERSION
        );
        let engine = RuleEngine::from_json(&rules).expect("parse intro rules");

        let report = retrim_game(
            &mut conn,
            10,
            &engine,
            &LangDetector::with_keep_list(&["de".to_string()]),
            DeleteMethod::Permanent,
        )
        .expect("execute retrim");

        assert_eq!(
            report.files_deleted, 2,
            "the screen and the reel the user does not keep: {:?}",
            report.errors
        );
        assert_eq!(
            fs::read(&screen).expect("read legal screen"),
            crate::stub::MP4_STUB,
            "a legal screen is stubbed in the user's own language too"
        );
        assert_eq!(
            fs::read(&kept).expect("read kept reel").len(),
            42,
            "content in a kept language must be untouched, not stubbed"
        );
        assert_eq!(
            fs::read(&removable).expect("read removable reel"),
            crate::stub::MP4_STUB
        );
    }

    /// GT-206: Source searches `portal2_dlc2` before `portal2`, so a stub
    /// written into the copy an intro rule happened to reach can leave the
    /// logo playing from the copy it did not. Unattended re-trim must stub
    /// every copy of the name it judged, not one of them.
    #[test]
    fn retrim_stubs_every_copy_of_a_flagged_intro_name() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let game_dir = temp_dir.path().join("Overlay Intro Game");
        let mut conn = setup_game(&temp_dir, &game_dir);

        // Only the first is within the rule's (depth-limited) reach; the
        // second is the copy a search-path overlay actually plays.
        let reached = game_dir.join("media").join("valve.mp4");
        let overlay = game_dir
            .join("dlc2")
            .join("deeper")
            .join("media")
            .join("valve.mp4");
        for path in [&reached, &overlay] {
            fs::create_dir_all(path.parent().expect("media parent")).expect("create media tree");
            fs::write(path, b"ORIGINAL VIDEO BYTES, NOT A REAL CONTAINER").expect("write video");
        }

        let rules = format!(
            r#"{{"version":{},"rules":[{{"category":"intro","pattern":"^valve\\.mp4$","desc":"Publisher logo","confidence":95}}]}}"#,
            crate::rules::RULE_PACK_VERSION
        );
        let engine = RuleEngine::from_json(&rules).expect("parse intro rule");

        let report = retrim_game(
            &mut conn,
            10,
            &engine,
            &LangDetector::new(),
            DeleteMethod::Permanent,
        )
        .expect("execute retrim");

        assert_eq!(
            report.files_deleted, 2,
            "both copies of the flagged name must be handled, not just the one the rule reached: {:?}",
            report.errors
        );
        // Every copy keeps a playable file at its path, and the freed figure
        // counts both - the report is what the caller shows as space gained.
        for path in [&reached, &overlay] {
            assert_eq!(
                fs::read(path).expect("read stubbed copy"),
                crate::stub::MP4_STUB,
                "{} must hold the micro-stub",
                path.display()
            );
        }
        assert!(report.bytes_freed > 0);

        // Each copy is persisted with its own attribution: the one the rule
        // reached cites the rule, the one the sweep added says so. Copying
        // the rule's description onto a path that rule declines to match
        // would put a claim in `findings.rule_id` the path disproves.
        let mut stmt = conn
            .prepare("SELECT rule_id FROM findings ORDER BY rule_id")
            .expect("read back finding descriptions");
        let descs: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query findings")
            .collect::<std::result::Result<_, _>>()
            .expect("collect descriptions");
        assert_eq!(
            descs,
            vec![
                crate::scanner::SIBLING_FINDING_DESC.to_string(),
                "Publisher logo".to_string()
            ]
        );
    }

    /// The other half of GT-206: one file per language is not one file seen
    /// several times. Every copy is already flagged on its own, so the sweep
    /// must add nothing - no second finding, no double-counted bytes.
    #[test]
    fn retrim_keeps_language_copies_at_exactly_one_finding_each() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let game_dir = temp_dir.path().join("Localized Intro Game");
        let mut conn = setup_game(&temp_dir, &game_dir);

        // Three languages the default keep-list does not keep - see the
        // interactive twin of this test for why.
        let copies: Vec<PathBuf> = ["de", "fr", "it"]
            .iter()
            .map(|lang| game_dir.join("videos").join(lang).join("warning.mp4"))
            .collect();
        for path in &copies {
            fs::create_dir_all(path.parent().expect("language parent")).expect("create video tree");
            fs::write(path, b"ORIGINAL VIDEO BYTES, NOT A REAL CONTAINER").expect("write video");
        }

        let rules = format!(
            r#"{{"version":{},"rules":[{{"category":"intro","pattern":"^warning\\.mp4$","desc":"Legal warning","confidence":95,"max_depth":3}}]}}"#,
            crate::rules::RULE_PACK_VERSION
        );
        let engine = RuleEngine::from_json(&rules).expect("parse intro rule");

        let report = retrim_game(
            &mut conn,
            10,
            &engine,
            &LangDetector::new(),
            DeleteMethod::Permanent,
        )
        .expect("execute retrim");

        assert_eq!(
            report.files_deleted, 3,
            "three language copies are three files, counted once each: {:?}",
            report.errors
        );
        let findings: i64 = conn
            .query_row("SELECT COUNT(*) FROM findings", [], |row| row.get(0))
            .expect("count findings");
        assert_eq!(
            findings, 3,
            "a language copy must never receive a second finding for its own name"
        );
        for path in &copies {
            assert_eq!(
                fs::read(path).expect("read stubbed copy"),
                crate::stub::MP4_STUB
            );
        }
    }
}
