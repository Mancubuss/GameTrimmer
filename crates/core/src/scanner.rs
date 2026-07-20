//! Filesystem scanning and indexing into SQLite.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use rayon::prelude::*;
use rusqlite::{params, Connection};
use walkdir::WalkDir;

use crate::error::{CoreError, Result};

/// One file found under a game directory. `rel_path` uses `\` separators and
/// is relative to the game's install dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub rel_path: String,
    pub size: u64,
    /// Unix seconds; None when the mtime is unavailable.
    pub mtime: Option<i64>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ScanStats {
    pub files: u64,
    pub bytes: u64,
}

/// How many items pass between polls of `cancel` inside
/// [`collect_cancellable`]: frequent enough that a cancellation request
/// lands within a small, bounded number of entries even on the largest game
/// libraries (hundreds of thousands of files), while an atomic load only
/// once per interval - rather than once per item - keeps the check's cost
/// immaterial in the common, never-cancelled case.
///
/// `pub(crate)` alongside [`collect_cancellable`] so a future in-crate
/// incremental-scan path (USN journal) can drive the same cancel cadence.
pub(crate) const CANCEL_POLL_INTERVAL: usize = 1024;

/// Recursively walks `dir` (not following symlinks/junctions) and returns all
/// regular files, paths relative to `dir`.
///
/// Individual entries that cannot be accessed (e.g. permission denied) are
/// skipped rather than failing the whole scan. Only a problem with `dir`
/// itself (e.g. it does not exist) results in an `Err`.
///
/// Never cancellable: a thin wrapper over [`scan_dir_cancellable`] with a
/// flag that is never set. Callers that need to abort a running walk should
/// call `scan_dir_cancellable` directly instead.
pub fn scan_dir(dir: &Path) -> Result<Vec<FileEntry>> {
    scan_dir_cancellable(dir, &AtomicBool::new(false))
}

/// Recursively walks `dir`, exactly like [`scan_dir`], but polls `cancel`
/// every [`CANCEL_POLL_INTERVAL`] entries and returns promptly with
/// `Err(CoreError::Other("cancelled"))` once it is observed set, instead of
/// always enumerating the whole tree to completion.
///
/// This is the strategy-agnostic enumeration+cancel point: the cancellable
/// collection step ([`collect_cancellable`]) is written against a plain
/// `&AtomicBool` and a generic `Iterator<Item = FileEntry>`, with no
/// `walkdir`-specific logic of its own, specifically so the future
/// USN-journal incremental-scan path can reuse the exact same
/// enumeration+cancel mechanism over its own entry stream instead of `walkdir`.
pub fn scan_dir_cancellable(dir: &Path, cancel: &AtomicBool) -> Result<Vec<FileEntry>> {
    let metadata = std::fs::metadata(dir)?;
    if !metadata.is_dir() {
        return Err(CoreError::Other(format!(
            "{} is not a directory",
            dir.display()
        )));
    }

    let entries = WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| map_walkdir_entry(dir, entry));

    collect_cancellable(entries, cancel)
}

/// Maps one `walkdir::DirEntry` into a `FileEntry` relative to `dir`,
/// applying the same filtering `scan_dir` has always applied: only regular
/// files are kept, and any entry whose relative path or metadata cannot be
/// read is skipped rather than failing the whole scan.
fn map_walkdir_entry(dir: &Path, entry: walkdir::DirEntry) -> Option<FileEntry> {
    if !entry.file_type().is_file() {
        return None;
    }

    let rel_path = entry
        .path()
        .strip_prefix(dir)
        .ok()?
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\\");

    let meta = entry.metadata().ok()?;
    let size = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);

    Some(FileEntry {
        rel_path,
        size,
        mtime,
    })
}

/// Consumes `iter`, collecting every item into a `Vec`, polling `cancel`
/// every [`CANCEL_POLL_INTERVAL`] items (checked *before* pulling the next
/// item, so a flag that is already set when this is called returns `Err`
/// without consuming a single item from `iter`). Once `cancel` is observed
/// set, this returns `Err(CoreError::Other("cancelled"))` immediately,
/// without pulling any further items out of `iter`.
///
/// Generic over the item type `T` (not tied to [`FileEntry`]) and
/// `pub(crate)` on purpose: this is the strategy-agnostic enumeration+cancel
/// primitive the future USN-journal incremental-scan path is meant to reuse
/// over its own change-record stream, without re-implementing the cadence.
pub(crate) fn collect_cancellable<I, T>(mut iter: I, cancel: &AtomicBool) -> Result<Vec<T>>
where
    I: Iterator<Item = T>,
{
    let mut out = Vec::new();
    loop {
        if out.len() % CANCEL_POLL_INTERVAL == 0 && cancel.load(Ordering::Relaxed) {
            return Err(CoreError::Other("cancelled".to_string()));
        }
        match iter.next() {
            Some(item) => out.push(item),
            None => return Ok(out),
        }
    }
}

/// Replaces the indexed files of `game_id` with `entries`, using whatever
/// transaction (if any) is already open on `conn`. Callers that want a
/// standalone commit for just this game should use [`store_files`] instead;
/// callers batching several games into one commit (see
/// `worker::scan::persist_prepared_game` in the app crate) call this
/// directly on a [`rusqlite::Transaction`] (which derefs to `&Connection`)
/// they opened themselves and commit once after several games.
pub fn store_files_no_tx(
    conn: &Connection,
    game_id: i64,
    entries: &[FileEntry],
) -> Result<ScanStats> {
    conn.execute("DELETE FROM files WHERE game_id = ?1", params![game_id])?;

    let mut stats = ScanStats::default();
    {
        let mut stmt = conn.prepare_cached(
            "INSERT INTO files (game_id, rel_path, size, mtime) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for entry in entries {
            let size_i64 = entry.size as i64;
            stmt.execute(params![game_id, entry.rel_path, size_i64, entry.mtime])?;
            stats.files += 1;
            stats.bytes += entry.size;
        }
    }

    Ok(stats)
}

/// Replaces the indexed files of `game_id` with `entries` in its own,
/// single-game transaction (delete + batch insert with a prepared
/// statement). A thin wrapper around [`store_files_no_tx`] for callers that
/// don't need to share a transaction across multiple games.
pub fn store_files(
    conn: &mut Connection,
    game_id: i64,
    entries: &[FileEntry],
) -> Result<ScanStats> {
    let tx = conn.transaction()?;
    let stats = store_files_no_tx(&tx, game_id, entries)?;
    tx.commit()?;
    Ok(stats)
}

/// Scans multiple game directories in parallel via rayon. The database
/// connection is not passed here (`Connection` is not `Sync`); callers must
/// persist results sequentially via [`store_files`].
pub fn scan_games_parallel(
    dirs: &[(i64, std::path::PathBuf)],
) -> Vec<(i64, Result<Vec<FileEntry>>)> {
    dirs.par_iter()
        .map(|(game_id, dir)| (*game_id, scan_dir(dir)))
        .collect()
}

/// Like [`scan_games_parallel`], but bounded to `num_threads` workers instead
/// of rayon's default (one per CPU core): scanning is IO-bound, so a handful
/// more threads than cores can keep multiple disk queues busy, while an
/// unbounded pool would oversubscribe for no benefit and complicate reasoning
/// about how many games are "in flight" when cancellation is requested.
///
/// `cancel` is checked once per game, right before that game's scan starts:
/// games not yet dispatched are skipped (returned as
/// [`CoreError::Other`]("cancelled")) as soon as cancellation is observed,
/// while games already handed to a worker thread still run to completion -
/// at most `num_threads` of them.
pub fn scan_games_bounded(
    dirs: &[(i64, std::path::PathBuf)],
    num_threads: usize,
    cancel: &AtomicBool,
) -> Vec<(i64, Result<Vec<FileEntry>>)> {
    let scan_one = |game_id: i64, dir: &Path| -> (i64, Result<Vec<FileEntry>>) {
        if cancel.load(Ordering::Relaxed) {
            return (game_id, Err(CoreError::Other("cancelled".to_string())));
        }
        (game_id, scan_dir(dir))
    };

    match rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads.max(1))
        .build()
    {
        Ok(pool) => pool.install(|| {
            dirs.par_iter()
                .map(|(game_id, dir)| scan_one(*game_id, dir))
                .collect()
        }),
        // A pool failing to build (extremely unlikely) must not lose the
        // scan entirely - fall back to sequential scanning.
        Err(_) => dirs
            .iter()
            .map(|(game_id, dir)| scan_one(*game_id, dir))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write file");
    }

    #[test]
    fn scan_dir_finds_all_files_with_backslash_rel_paths() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let root = dir.path();

        write_file(&root.join("readme.txt"), b"hello");
        write_file(&root.join("data").join("save1.sav"), b"save-data-1");
        write_file(&root.join("data").join("nested").join("save2.sav"), b"sd2");
        write_file(&root.join("bin").join("game.exe"), b"exebytes");

        let entries = scan_dir(root).expect("scan should succeed");

        assert_eq!(entries.len(), 4, "expected 4 files, got {entries:?}");

        let by_path: std::collections::HashMap<&str, &FileEntry> =
            entries.iter().map(|e| (e.rel_path.as_str(), e)).collect();

        let readme = by_path.get("readme.txt").expect("readme.txt present");
        assert_eq!(readme.size, 5);

        let save1 = by_path
            .get("data\\save1.sav")
            .expect("nested rel_path uses backslash");
        assert_eq!(save1.size, 11);

        let save2 = by_path
            .get("data\\nested\\save2.sav")
            .expect("doubly-nested rel_path uses backslash");
        assert_eq!(save2.size, 3);

        let exe = by_path.get("bin\\game.exe").expect("bin\\game.exe present");
        assert_eq!(exe.size, 8);

        // mtime should be populated on a freshly written file.
        assert!(readme.mtime.is_some());
    }

    /// A synthetic `FileEntry` iterator that counts how many items it has
    /// yielded, so tests can assert how far a cancelled `collect_cancellable`
    /// run got without needing a real filesystem.
    struct CountingEntries {
        total: usize,
        yielded: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl Iterator for CountingEntries {
        type Item = FileEntry;

        fn next(&mut self) -> Option<FileEntry> {
            let done = self.yielded.get();
            if done >= self.total {
                return None;
            }
            self.yielded.set(done + 1);
            Some(FileEntry {
                rel_path: format!("file{done}.txt"),
                size: 1,
                mtime: None,
            })
        }
    }

    #[test]
    fn collect_cancellable_stops_promptly_when_flag_already_set() {
        let yielded = std::rc::Rc::new(std::cell::Cell::new(0));
        let iter = CountingEntries {
            total: 10_000,
            yielded: yielded.clone(),
        };
        let cancel = AtomicBool::new(true);

        let result = collect_cancellable(iter, &cancel);

        assert!(result.is_err(), "a pre-cancelled run must return Err");
        assert_eq!(
            yielded.get(),
            0,
            "no item should be pulled from the iterator once already cancelled"
        );
    }

    #[test]
    fn collect_cancellable_returns_all_when_not_cancelled() {
        let yielded = std::rc::Rc::new(std::cell::Cell::new(0));
        let iter = CountingEntries {
            total: 50,
            yielded: yielded.clone(),
        };
        let cancel = AtomicBool::new(false);

        let result = collect_cancellable(iter, &cancel).expect("uncancelled run should succeed");

        assert_eq!(result.len(), 50);
        assert_eq!(yielded.get(), 50);
    }

    #[test]
    fn scan_dir_cancellable_equivalent_to_scan_dir_when_not_cancelled() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let root = dir.path();

        write_file(&root.join("readme.txt"), b"hello");
        write_file(&root.join("data").join("save1.sav"), b"save-data-1");

        let cancel = AtomicBool::new(false);
        let cancellable =
            scan_dir_cancellable(root, &cancel).expect("cancellable scan should succeed");
        let plain = scan_dir(root).expect("plain scan should succeed");

        let mut cancellable_paths: Vec<&str> =
            cancellable.iter().map(|e| e.rel_path.as_str()).collect();
        let mut plain_paths: Vec<&str> = plain.iter().map(|e| e.rel_path.as_str()).collect();
        cancellable_paths.sort_unstable();
        plain_paths.sort_unstable();

        assert_eq!(cancellable_paths, plain_paths);
        assert_eq!(cancellable.len(), 2);
    }

    #[test]
    fn scan_dir_cancellable_errors_when_pre_cancelled() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let root = dir.path();
        write_file(&root.join("readme.txt"), b"hello");

        let cancel = AtomicBool::new(true);
        let result = scan_dir_cancellable(root, &cancel);

        let err = result.expect_err("a pre-cancelled scan must return Err");
        assert!(
            err.to_string().contains("cancelled"),
            "error message should mention cancellation, got: {err}"
        );
    }

    #[test]
    fn scan_dir_on_missing_dir_returns_err() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let missing = dir.path().join("does-not-exist");

        let result = scan_dir(&missing);

        assert!(result.is_err(), "expected Err for missing directory");
    }

    #[test]
    fn store_files_replaces_previous_entries_for_same_game() {
        let mut conn = crate::db::open_in_memory().expect("open in-memory db");

        conn.execute(
            "INSERT INTO game_libraries (vendor, path) VALUES ('steam', 'C:/Games')",
            [],
        )
        .expect("insert library");
        conn.execute(
            "INSERT INTO games (library_id, name, install_dir) VALUES (1, 'Test Game', 'C:/Games/Test')",
            [],
        )
        .expect("insert game");

        let game_id = 1i64;

        let first_batch = vec![
            FileEntry {
                rel_path: "a.txt".into(),
                size: 10,
                mtime: Some(100),
            },
            FileEntry {
                rel_path: "b\\c.txt".into(),
                size: 20,
                mtime: Some(200),
            },
        ];
        let stats = store_files(&mut conn, game_id, &first_batch).expect("store first batch");
        assert_eq!(stats.files, 2);
        assert_eq!(stats.bytes, 30);

        let second_batch = vec![FileEntry {
            rel_path: "only.txt".into(),
            size: 42,
            mtime: None,
        }];
        let stats = store_files(&mut conn, game_id, &second_batch).expect("store second batch");
        assert_eq!(stats.files, 1);
        assert_eq!(stats.bytes, 42);

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE game_id = ?1",
                params![game_id],
                |row| row.get(0),
            )
            .expect("count files");
        assert_eq!(count, 1, "old entries must be replaced, not accumulated");

        let total_bytes: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(size), 0) FROM files WHERE game_id = ?1",
                params![game_id],
                |row| row.get(0),
            )
            .expect("sum sizes");
        assert_eq!(total_bytes, 42);
    }

    #[test]
    fn scan_games_parallel_scans_multiple_dirs() {
        let dir_a = tempfile::tempdir().expect("create temp dir a");
        let dir_b = tempfile::tempdir().expect("create temp dir b");

        write_file(&dir_a.path().join("fileA.txt"), b"aaaa");
        write_file(&dir_b.path().join("sub").join("fileB.txt"), b"bb");

        let dirs = vec![
            (1i64, dir_a.path().to_path_buf()),
            (2i64, dir_b.path().to_path_buf()),
        ];

        let results = scan_games_parallel(&dirs);
        assert_eq!(results.len(), 2);

        let mut by_id: std::collections::HashMap<i64, Vec<FileEntry>> = results
            .into_iter()
            .map(|(id, res)| (id, res.expect("scan should succeed")))
            .collect();

        let entries_a = by_id.remove(&1).expect("game 1 present");
        assert_eq!(entries_a.len(), 1);
        assert_eq!(entries_a[0].rel_path, "fileA.txt");
        assert_eq!(entries_a[0].size, 4);

        let entries_b = by_id.remove(&2).expect("game 2 present");
        assert_eq!(entries_b.len(), 1);
        assert_eq!(entries_b[0].rel_path, "sub\\fileB.txt");
        assert_eq!(entries_b[0].size, 2);
    }

    #[test]
    fn scan_games_bounded_scans_multiple_dirs_with_a_small_pool() {
        let dir_a = tempfile::tempdir().expect("create temp dir a");
        let dir_b = tempfile::tempdir().expect("create temp dir b");

        write_file(&dir_a.path().join("fileA.txt"), b"aaaa");
        write_file(&dir_b.path().join("sub").join("fileB.txt"), b"bb");

        let dirs = vec![
            (1i64, dir_a.path().to_path_buf()),
            (2i64, dir_b.path().to_path_buf()),
        ];
        let cancel = AtomicBool::new(false);

        let results = scan_games_bounded(&dirs, 2, &cancel);
        assert_eq!(results.len(), 2);

        let mut by_id: std::collections::HashMap<i64, Vec<FileEntry>> = results
            .into_iter()
            .map(|(id, res)| (id, res.expect("scan should succeed")))
            .collect();

        let entries_a = by_id.remove(&1).expect("game 1 present");
        assert_eq!(entries_a[0].rel_path, "fileA.txt");

        let entries_b = by_id.remove(&2).expect("game 2 present");
        assert_eq!(entries_b[0].rel_path, "sub\\fileB.txt");
    }

    #[test]
    fn scan_games_bounded_skips_games_not_yet_dispatched_once_cancelled() {
        let dir_a = tempfile::tempdir().expect("create temp dir a");
        write_file(&dir_a.path().join("fileA.txt"), b"aaaa");

        let dirs = vec![(1i64, dir_a.path().to_path_buf())];
        let cancel = AtomicBool::new(true);

        let results = scan_games_bounded(&dirs, 2, &cancel);
        assert_eq!(results.len(), 1);
        assert!(
            results[0].1.is_err(),
            "a game not yet dispatched when cancel is already set must be skipped"
        );
    }

    #[test]
    fn store_files_no_tx_replaces_previous_entries_within_a_caller_owned_transaction() {
        let mut conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO game_libraries (vendor, path) VALUES ('steam', 'C:/Games')",
            [],
        )
        .expect("insert library");
        conn.execute(
            "INSERT INTO games (library_id, name, install_dir) VALUES (1, 'Test Game', 'C:/Games/Test')",
            [],
        )
        .expect("insert game");

        let game_id = 1i64;
        let entries = vec![FileEntry {
            rel_path: "a.txt".into(),
            size: 10,
            mtime: Some(100),
        }];

        let tx = conn.transaction().expect("begin transaction");
        let stats = store_files_no_tx(&tx, game_id, &entries).expect("store within shared tx");
        assert_eq!(stats.files, 1);
        tx.commit().expect("commit shared tx");

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE game_id = ?1",
                params![game_id],
                |row| row.get(0),
            )
            .expect("count files");
        assert_eq!(count, 1);
    }
}
