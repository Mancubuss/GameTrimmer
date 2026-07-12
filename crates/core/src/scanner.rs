//! Filesystem scanning and indexing into SQLite.

use std::path::Path;
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

/// Recursively walks `dir` (not following symlinks/junctions) and returns all
/// regular files, paths relative to `dir`.
///
/// Individual entries that cannot be accessed (e.g. permission denied) are
/// skipped rather than failing the whole scan. Only a problem with `dir`
/// itself (e.g. it does not exist) results in an `Err`.
pub fn scan_dir(dir: &Path) -> Result<Vec<FileEntry>> {
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
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
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
        })
        .collect();

    Ok(entries)
}

/// Replaces the indexed files of `game_id` with `entries` in a single
/// transaction (delete + batch insert with a prepared statement).
pub fn store_files(
    conn: &mut Connection,
    game_id: i64,
    entries: &[FileEntry],
) -> Result<ScanStats> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM files WHERE game_id = ?1", params![game_id])?;

    let mut stats = ScanStats::default();
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO files (game_id, rel_path, size, mtime) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for entry in entries {
            let size_i64 = entry.size as i64;
            stmt.execute(params![game_id, entry.rel_path, size_i64, entry.mtime])?;
            stats.files += 1;
            stats.bytes += entry.size;
        }
    }

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
}
