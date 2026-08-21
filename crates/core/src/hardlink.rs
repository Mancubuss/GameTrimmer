//! Hard-link aware space accounting.
//!
//! A file's on-disk allocation belongs to the *file*, not to the path that
//! names it. NTFS lets several directory entries (hard links) point at one
//! file, and deleting one of them frees nothing at all - the allocation only
//! goes away with the last link. Summing [`crate::scanner::FileEntry::size_on_disk`]
//! over paths therefore over-reports both "found" and "freed" the moment any
//! deduplicated content is in play: a tree of 1 026 links to one 8 MiB file
//! measured as 8 208 MB.
//!
//! This module supplies the two pieces needed to stop lying about it: the
//! per-file identity ([`file_share`]) and the arithmetic that folds a batch of
//! paths into the bytes a deletion actually reclaims ([`reclaimable_bytes`]).

use std::collections::HashMap;
use std::path::Path;

/// Identity of the file behind a path, plus how many paths currently name it.
/// Every hard link to one file reports the same `volume_serial`/`file_index`
/// pair and the same `link_count`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileShare {
    pub volume_serial: u64,
    pub file_index: u64,
    /// Number of hard links naming this file. `1` for an ordinary file.
    pub link_count: u32,
}

impl FileShare {
    /// Whether more than one path names this file, i.e. deleting a single path
    /// cannot free its allocation on its own.
    pub fn is_shared(&self) -> bool {
        self.link_count > 1
    }

    fn key(&self) -> (u64, u64) {
        (self.volume_serial, self.file_index)
    }
}

/// Reads the identity and hard-link count of the file at `path`.
///
/// Returns `None` when the file cannot be opened or queried - a locked file, a
/// vanished path, a filesystem without the notion. Callers must treat `None` as
/// "assume unshared", which keeps the accounting exactly as it was before this
/// module existed rather than silently zeroing a figure we failed to verify.
///
/// This opens the file, so it belongs on bounded, user-initiated paths (a
/// delete batch), never inside a full-library scan of hundreds of thousands of
/// files.
#[cfg(windows)]
pub fn file_share(path: &Path) -> Option<FileShare> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let file = std::fs::File::open(path).ok()?;
    let handle = HANDLE(file.as_raw_handle() as _);
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `handle` borrows the handle owned by `file`, which is alive for
    // the whole call; `info` is a live local that the API fills in. Neither
    // pointer escapes.
    unsafe { GetFileInformationByHandle(handle, &mut info) }.ok()?;

    Some(FileShare {
        volume_serial: info.dwVolumeSerialNumber as u64,
        file_index: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        link_count: info.nNumberOfLinks,
    })
}

/// Non-Windows stub: no hard-link accounting to do, so every file is treated
/// as unshared.
#[cfg(not(windows))]
pub fn file_share(_path: &Path) -> Option<FileShare> {
    None
}

/// Bytes a deletion of exactly these paths actually reclaims.
///
/// Each item pairs a path's [`FileShare`] (as captured *before* deletion) with
/// its on-disk size. The rules:
///
/// - an unshared file (`link_count <= 1`) contributes its full size;
/// - an unknown share (`None`) also contributes its full size - we never claim
///   a saving is void on the strength of a failed query;
/// - a shared file contributes its size **once**, and only if every link to it
///   is in this batch. Deleting 2 of 3 links frees nothing, so it contributes
///   nothing, which is the literal truth.
pub fn reclaimable_bytes(items: &[(Option<FileShare>, u64)]) -> u64 {
    let mut total: u64 = 0;
    // (volume, index) -> (paths in this batch, links on disk, size)
    let mut shared: HashMap<(u64, u64), (u32, u32, u64)> = HashMap::new();

    for (share, size) in items {
        match share {
            Some(share) if share.is_shared() => {
                let entry = shared
                    .entry(share.key())
                    .or_insert((0, share.link_count, *size));
                entry.0 += 1;
                // A batch that disagrees with itself about the link count (the
                // file changed under us) is resolved conservatively: the
                // highest count seen wins, so we under-claim rather than
                // promise space we cannot deliver.
                entry.1 = entry.1.max(share.link_count);
            }
            _ => total = total.saturating_add(*size),
        }
    }

    for (in_batch, link_count, size) in shared.into_values() {
        if in_batch >= link_count {
            total = total.saturating_add(size);
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn share(index: u64, links: u32) -> Option<FileShare> {
        Some(FileShare {
            volume_serial: 7,
            file_index: index,
            link_count: links,
        })
    }

    #[test]
    fn unshared_files_contribute_their_full_size() {
        let items = [(share(1, 1), 4096), (share(2, 1), 8192)];
        assert_eq!(reclaimable_bytes(&items), 12_288);
    }

    #[test]
    fn an_unknown_share_is_counted_in_full() {
        // A failed query must not silently erase a real saving.
        let items = [(None, 4096), (share(1, 1), 4096)];
        assert_eq!(reclaimable_bytes(&items), 8192);
    }

    #[test]
    fn deleting_one_of_several_links_frees_nothing() {
        // The whole point: the file survives behind its other name.
        let items = [(share(1, 2), 8 * 1024 * 1024)];
        assert_eq!(reclaimable_bytes(&items), 0);
    }

    #[test]
    fn deleting_every_link_frees_the_allocation_once() {
        let items = [
            (share(1, 2), 8 * 1024 * 1024),
            (share(1, 2), 8 * 1024 * 1024),
        ];
        assert_eq!(
            reclaimable_bytes(&items),
            8 * 1024 * 1024,
            "two links to one 8 MiB file free 8 MiB, not 16"
        );
    }

    #[test]
    fn a_partial_link_set_frees_nothing_even_when_most_are_selected() {
        let items = [(share(1, 3), 1024), (share(1, 3), 1024)];
        assert_eq!(reclaimable_bytes(&items), 0);
    }

    #[test]
    fn distinct_files_of_equal_size_are_not_confused_for_each_other() {
        let items = [(share(1, 2), 1024), (share(2, 2), 1024)];
        assert_eq!(
            reclaimable_bytes(&items),
            0,
            "two different files, one link each in the batch, free nothing"
        );
    }

    #[test]
    fn same_index_on_different_volumes_stays_distinct() {
        let a = Some(FileShare {
            volume_serial: 1,
            file_index: 42,
            link_count: 2,
        });
        let b = Some(FileShare {
            volume_serial: 2,
            file_index: 42,
            link_count: 2,
        });
        assert_eq!(reclaimable_bytes(&[(a, 512), (b, 512)]), 0);
    }

    #[test]
    fn a_mixed_batch_adds_unshared_bytes_to_completed_link_sets() {
        let items = [
            (share(1, 2), 1000), // both links present -> 1000
            (share(1, 2), 1000),
            (share(2, 2), 500), // only one of two links -> 0
            (None, 300),        // unknown -> 300
            (share(3, 1), 200), // plain file -> 200
        ];
        assert_eq!(reclaimable_bytes(&items), 1500);
    }

    #[test]
    fn an_empty_batch_reclaims_nothing() {
        assert_eq!(reclaimable_bytes(&[]), 0);
    }

    /// Real-filesystem check that [`file_share`] sees an actual NTFS hard link:
    /// two names, one file, link count 2, identical identity - the measured
    /// behaviour the whole module is built on.
    #[cfg(windows)]
    #[test]
    fn file_share_reports_a_real_hard_link() {
        use std::os::windows::fs::MetadataExt as _;

        let dir = tempfile::tempdir().expect("create temp dir");
        let original = dir.path().join("asset.pak");
        std::fs::write(&original, vec![b'x'; 4096]).expect("write test file");

        let plain = file_share(&original).expect("query the original");
        assert_eq!(plain.link_count, 1, "a fresh file has exactly one name");
        assert!(!plain.is_shared());

        let linked = dir.path().join("asset-copy.pak");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/H"])
            .arg(&linked)
            .arg(&original)
            .output();
        let Ok(output) = status else {
            return; // no shell available - nothing to assert
        };
        if !output.status.success() || !linked.exists() {
            // Hard links need NTFS; a temp dir on another filesystem simply
            // cannot exercise this.
            return;
        }

        let a = file_share(&original).expect("query after linking");
        let b = file_share(&linked).expect("query the new link");
        assert_eq!(a.link_count, 2);
        assert_eq!(a.key(), b.key(), "both names must resolve to one file");
        assert!(a.is_shared() && b.is_shared());

        // And the accounting that follows from it.
        let size = std::fs::metadata(&original).expect("metadata").file_size();
        assert_eq!(
            reclaimable_bytes(&[(Some(a), size)]),
            0,
            "deleting one of two links frees nothing"
        );
        assert_eq!(
            reclaimable_bytes(&[(Some(a), size), (Some(b), size)]),
            size,
            "deleting both frees the allocation exactly once"
        );
    }
}
