//! Computing a file's *on-disk* allocated size - what Windows Explorer shows
//! as "Size on disk" - for the `walkdir` scan path.
//!
//! The MFT scan path reads the true `allocated_size` straight out of each
//! record (see [`crate::mftscan`]), so it never needs this. The `walkdir`
//! path only has `std::fs` metadata, whose `len()` is the *logical* size; for
//! a library of thousands of small localisation files that undercounts real
//! occupancy badly, because every non-resident file rounds up to a whole
//! cluster. This module reconstructs the on-disk figure the way Explorer does:
//! the file's physical size (compression/sparse-aware, via
//! `GetCompressedFileSizeW`) rounded up to the volume's cluster size.

use std::path::Path;

/// Rounds `bytes` up to the next multiple of `cluster`. A `cluster` of 0
/// (unknown cluster size) is treated as "no rounding" and returns `bytes`
/// unchanged, so a failed cluster-size probe degrades gracefully instead of
/// dividing by zero.
pub fn round_up_to_cluster(bytes: u64, cluster: u64) -> u64 {
    if cluster == 0 {
        return bytes;
    }
    let remainder = bytes % cluster;
    if remainder == 0 {
        bytes
    } else {
        // `bytes + (cluster - remainder)` can only overflow for values within
        // one cluster of u64::MAX, which no real file reaches; saturate rather
        // than panic in that impossible case.
        bytes.saturating_add(cluster - remainder)
    }
}

/// Extracts the volume root (`"X:\\"`) of an absolute drive-letter path,
/// tolerating a `\\?\` verbatim prefix. Returns `None` for UNC paths,
/// relative paths, or anything without a drive letter - callers treat that as
/// "cluster size unknown" and skip rounding.
pub fn drive_root(path: &Path) -> Option<String> {
    let s = path.to_str()?;
    let s = s.strip_prefix(r"\\?\").unwrap_or(s);
    let bytes = s.as_bytes();
    // Need at least "X:" and a following separator (or the string ending
    // right after the colon, e.g. bare "X:").
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let letter = (bytes[0] as char).to_ascii_uppercase();
        Some(format!(r"{letter}:\"))
    } else {
        None
    }
}

/// On-disk allocated size of `path` in bytes: its physical size (via
/// `GetCompressedFileSizeW`, which already accounts for NTFS compression and
/// sparse ranges) rounded up to `cluster`. Falls back to
/// `round_up_to_cluster(logical, cluster)` when the physical-size query fails
/// for any reason, so a caller always gets a usable figure that is never
/// smaller than the logical size would round to.
#[cfg(windows)]
pub fn on_disk_size(path: &Path, logical: u64, cluster: u64) -> u64 {
    match compressed_file_size(path) {
        Some(physical) => round_up_to_cluster(physical, cluster),
        None => round_up_to_cluster(logical, cluster),
    }
}

/// Non-Windows stub (tests, CI on other platforms): there is no on-disk
/// notion to query, so the logical size is the honest answer.
#[cfg(not(windows))]
pub fn on_disk_size(_path: &Path, logical: u64, _cluster: u64) -> u64 {
    logical
}

/// The volume cluster size (bytes) for the disk `path` lives on, via
/// `GetDiskFreeSpaceW` on the extracted drive root. Returns 0 when it cannot
/// be determined (non-drive path, query failure), which [`round_up_to_cluster`]
/// treats as "do not round". Query this once per scan root and reuse it for
/// every file under that root rather than per file.
#[cfg(windows)]
pub fn cluster_size(path: &Path) -> u64 {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceW;

    let Some(root) = drive_root(path) else {
        return 0;
    };
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    let root_ptr = PCWSTR::from_raw(wide.as_ptr());

    let mut sectors_per_cluster: u32 = 0;
    let mut bytes_per_sector: u32 = 0;
    let mut free_clusters: u32 = 0;
    let mut total_clusters: u32 = 0;

    // SAFETY: `root_ptr` points into `wide`, a null-terminated UTF-16 buffer
    // that outlives this call. Every output pointer is `Some(&mut _)` at a
    // live local `u32` outliving the call, as `GetDiskFreeSpaceW` expects.
    let result = unsafe {
        GetDiskFreeSpaceW(
            root_ptr,
            Some(&mut sectors_per_cluster),
            Some(&mut bytes_per_sector),
            Some(&mut free_clusters),
            Some(&mut total_clusters),
        )
    };

    match result {
        Ok(()) => (bytes_per_sector as u64).saturating_mul(sectors_per_cluster as u64),
        Err(_) => 0,
    }
}

/// Non-Windows stub: no cluster notion, so nothing to round to.
#[cfg(not(windows))]
pub fn cluster_size(_path: &Path) -> u64 {
    0
}

/// Wraps `GetCompressedFileSizeW`, returning the file's physical size in
/// bytes or `None` on error. The Win32 call returns the low 32 bits and
/// writes the high 32 bits through an out-param; a low word of
/// `INVALID_FILE_SIZE` (`u32::MAX`) means "check `GetLastError`", and a
/// non-`NO_ERROR` code there is a real failure.
#[cfg(windows)]
fn compressed_file_size(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::Storage::FileSystem::GetCompressedFileSizeW;

    // `GetCompressedFileSizeW` returns this in its low DWORD on failure; the
    // caller must then consult `GetLastError` to distinguish it from a genuine
    // 4 GiB-boundary size. (`INVALID_FILE_SIZE` is not re-exported by the
    // `windows` crate under `Win32::Foundation`, so it is spelled out here.)
    const INVALID_FILE_SIZE: u32 = u32::MAX;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let path_ptr = PCWSTR::from_raw(wide.as_ptr());

    let mut high: u32 = 0;
    // SAFETY: `path_ptr` points into `wide`, a null-terminated UTF-16 buffer
    // that outlives the call; `&mut high` is a live local `u32`.
    let low = unsafe { GetCompressedFileSizeW(path_ptr, Some(&mut high)) };

    if low == INVALID_FILE_SIZE {
        // SAFETY: querying thread-local last-error immediately after the call
        // that may have set it; no other Win32 call runs in between.
        // `WIN32_ERROR(0)` is `ERROR_SUCCESS` - a nonzero code is a real error.
        let last = unsafe { GetLastError() };
        if last.0 != 0 {
            return None;
        }
    }

    Some(((high as u64) << 32) | low as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn round_up_to_cluster_rounds_partial_clusters_up() {
        assert_eq!(round_up_to_cluster(1, 4096), 4096);
        assert_eq!(round_up_to_cluster(4095, 4096), 4096);
        assert_eq!(round_up_to_cluster(4096, 4096), 4096);
        assert_eq!(round_up_to_cluster(4097, 4096), 8192);
        assert_eq!(round_up_to_cluster(10_000, 4096), 12_288);
    }

    #[test]
    fn round_up_to_cluster_keeps_zero_and_exact_multiples() {
        assert_eq!(round_up_to_cluster(0, 4096), 0);
        assert_eq!(round_up_to_cluster(8192, 4096), 8192);
    }

    #[test]
    fn round_up_to_cluster_with_unknown_cluster_is_identity() {
        // A failed cluster probe (0) must never divide by zero, and must not
        // fabricate rounding it can't justify.
        assert_eq!(round_up_to_cluster(1234, 0), 1234);
        assert_eq!(round_up_to_cluster(0, 0), 0);
    }

    #[test]
    fn drive_root_extracts_letter_root() {
        assert_eq!(
            drive_root(Path::new(r"D:\Games\Steam\common\Game")),
            Some(r"D:\".to_string())
        );
        assert_eq!(drive_root(Path::new(r"c:\x")), Some(r"C:\".to_string()));
        assert_eq!(drive_root(Path::new(r"C:")), Some(r"C:\".to_string()));
    }

    #[test]
    fn drive_root_tolerates_verbatim_prefix() {
        assert_eq!(
            drive_root(Path::new(r"\\?\E:\SteamLibrary\steamapps")),
            Some(r"E:\".to_string())
        );
    }

    #[test]
    fn drive_root_rejects_unc_and_relative_paths() {
        assert_eq!(drive_root(Path::new(r"\\server\share\dir")), None);
        assert_eq!(drive_root(Path::new(r"relative\dir")), None);
        assert_eq!(drive_root(Path::new("")), None);
    }

    /// Real-filesystem check of the Windows on-disk path (allocated-size accounting acceptance
    /// criterion: the figure must match Explorer's cluster-rounded "Size on
    /// disk"). A ~5 KiB file is comfortably non-resident, so on a cluster-sized
    /// volume its on-disk size must be a whole number of clusters and strictly
    /// larger than its logical size - the exact "cluster slack" the feature
    /// exists to surface.
    #[cfg(windows)]
    #[test]
    fn on_disk_size_is_cluster_aligned_for_a_real_non_resident_file() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let file = dir.path().join("loc.xml");
        let logical: u64 = 5000;
        std::fs::write(&file, vec![b'x'; logical as usize]).expect("write test file");

        let cluster = cluster_size(dir.path());
        if cluster == 0 {
            // Temp dir on a volume whose cluster size can't be queried (network
            // share, unusual FS): nothing to assert about rounding.
            return;
        }

        let on_disk = on_disk_size(&file, logical, cluster);
        assert_eq!(
            on_disk % cluster,
            0,
            "on-disk size must be a whole number of clusters (got {on_disk}, cluster {cluster})"
        );
        assert!(
            on_disk >= logical,
            "on-disk size ({on_disk}) must not be smaller than the logical size ({logical})"
        );
        assert_eq!(
            on_disk,
            round_up_to_cluster(logical, cluster),
            "an uncompressed file's on-disk size is its logical size rounded up to a cluster"
        );
    }
}
