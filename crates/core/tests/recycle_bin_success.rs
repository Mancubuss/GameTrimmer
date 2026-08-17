//! The ordinary [`RecycleBin`] success path: an item comfortably under the
//! volume's Recycle Bin quota really is recycled, and really is recoverable.
//!
//! Every other test in this workspace removes files through a stub `Remover`
//! precisely so it never touches the real bin - which left the one claim the
//! app actually makes to the user ("moved to the Recycle Bin, recoverable")
//! resting on nothing but the `trash` crate's own word. The only test that
//! reached `RecycleBin::remove` was the `#[ignore]`d over-quota probe in
//! `recycle_bin_quota.rs`, i.e. the *edge* case; the normal case was
//! unverified. That gap is GT-109's sixth item.
//!
//! This test does touch the real Recycle Bin, and is deliberately kept safe to
//! run anywhere:
//!
//! - the item is a few bytes in a fresh temp directory, so it cannot evict
//!   anything already in the bin (the over-quota probe's warning about
//!   eviction applies to gigabyte-sized items, not to this),
//! - it purges its own entry afterwards, whatever the assertions did, so a
//!   failing run leaves nothing behind in the user's bin,
//! - it matches its own entry by original path, so a bin holding unrelated
//!   items is not disturbed.
//!
//! Windows-only: `trash` needs the shell's `IFileOperation`, and this project
//! ships for Windows. On any other platform the test compiles away rather than
//! asserting something untrue about a different trash implementation.

#![cfg(windows)]

use gametrimmer_core::ops::{RecycleBin, Remover};

/// Bin entries whose original path is `path`. Returned rather than a bool
/// because the caller needs the items themselves to purge them again.
fn bin_entries_for(path: &std::path::Path) -> Vec<trash::TrashItem> {
    match trash::os_limited::list() {
        Ok(items) => items
            .into_iter()
            .filter(|item| item.original_path().as_path() == path)
            .collect(),
        // A failure to list is not evidence of absence. Report it and let the
        // caller's assertion fail on its own terms rather than silently
        // claiming the item is not there.
        Err(err) => {
            println!("WARN: could not list the Recycle Bin: {err}");
            Vec::new()
        }
    }
}

/// A temp directory that is unique per test run without pulling in a
/// dev-dependency for it: the process id plus the test name is enough, and the
/// directory is removed on drop.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("gt-recycle-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }

    fn join(&self, name: &str) -> std::path::PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Removes every bin entry that came from `path`. Called on both the pass and
/// the fail path - a test that leaves debris in the user's Recycle Bin is not
/// a test anyone will keep running.
fn purge_bin_entries_for(path: &std::path::Path) {
    let entries = bin_entries_for(path);
    if entries.is_empty() {
        return;
    }
    if let Err(err) = trash::os_limited::purge_all(entries) {
        println!("WARN: could not purge this test's Recycle Bin entry: {err}");
    }
}

#[test]
fn a_recycled_file_leaves_the_disk_and_is_recoverable_from_the_bin() {
    let dir = TempDir::new("file");
    let target = dir.join("recyclable.txt");
    std::fs::write(&target, b"gametrimmer recycle-bin success-path probe")
        .expect("write the file to be recycled");

    // `trash` records the path it was given; canonicalizing here would compare
    // a verbatim `\\?\` path against the plain one the bin stores.
    let result = RecycleBin.remove(&target);
    let still_on_disk = target.exists();
    let entries = bin_entries_for(&target);

    purge_bin_entries_for(&target);

    result.expect("recycling a small file on a normal volume must succeed");
    assert!(
        !still_on_disk,
        "`RecycleBin::remove` reported success but the file is still on disk - \
         the app would drop a finding for a file it never removed"
    );
    assert_eq!(
        entries.len(),
        1,
        "`RecycleBin::remove` reported success but the file is not in the \
         Recycle Bin - the app's \"moved to the Recycle Bin, recoverable\" \
         wording would be a lie"
    );
}

#[test]
fn a_recycled_directory_leaves_the_disk_and_is_recoverable_from_the_bin() {
    let dir = TempDir::new("dir");
    let target = dir.join("recyclable-dir");
    std::fs::create_dir_all(target.join("nested")).expect("create the directory to be recycled");
    std::fs::write(target.join("nested").join("leaf.txt"), b"leaf").expect("write nested file");

    let result = RecycleBin.remove(&target);
    let still_on_disk = target.exists();
    let entries = bin_entries_for(&target);

    purge_bin_entries_for(&target);

    result.expect("recycling a small directory on a normal volume must succeed");
    assert!(
        !still_on_disk,
        "`RecycleBin::remove` reported success but the directory is still on disk"
    );
    assert_eq!(
        entries.len(),
        1,
        "a recycled directory must be recoverable from the bin as one entry"
    );
}

#[test]
fn removing_an_absent_path_is_an_error_rather_than_a_silent_success() {
    let dir = TempDir::new("absent");
    let target = dir.join("never-existed.txt");

    // The deletion pipeline distinguishes `AlreadyAbsent` from `Removed` by
    // re-checking the filesystem after an error (see `ops.rs`), which only
    // works if the remover reports the failure instead of swallowing it.
    let result = RecycleBin.remove(&target);

    purge_bin_entries_for(&target);

    assert!(
        result.is_err(),
        "recycling a path that does not exist must fail, otherwise the \
         already-absent branch in the deletion pipeline is never reached"
    );
}
