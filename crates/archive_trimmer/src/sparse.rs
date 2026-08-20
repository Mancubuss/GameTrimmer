//! Win32 NTFS sparse-file inspection helpers.
//!
//! Production exports are read-only. Direct sparse mutation helpers exist only
//! in this module's unit-test build until a transactional rollback contract is
//! available.

use std::fs::File;
use std::path::Path;
use thiserror::Error;

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Foundation::{GetLastError, ERROR_MORE_DATA, HANDLE};
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{
    GetCompressedFileSizeW, GetDiskFreeSpaceW, GetFileAttributesW, FILE_ATTRIBUTE_SPARSE_FILE,
};
#[cfg(windows)]
use windows::Win32::System::Ioctl::FSCTL_QUERY_ALLOCATED_RANGES;
#[cfg(all(test, windows))]
use windows::Win32::System::Ioctl::{FSCTL_SET_SPARSE, FSCTL_SET_ZERO_DATA};
#[cfg(windows)]
use windows::Win32::System::IO::DeviceIoControl;

/// Win32 `FILE_SET_SPARSE_BUFFER` structure.
#[repr(C)]
#[derive(Copy, Clone, Default, Debug)]
#[cfg(test)]
struct FileSetSparseBuffer {
    set_sparse: u8,
}

/// Win32 `FILE_ZERO_DATA_INFORMATION` structure for `FSCTL_SET_ZERO_DATA`.
#[repr(C)]
#[derive(Copy, Clone, Default, Debug)]
#[cfg(test)]
struct FileZeroDataInformation {
    file_offset: i64,
    beyond_final_zero: i64,
}

/// Win32 `FILE_ALLOCATED_RANGE_BUFFER` structure for `FSCTL_QUERY_ALLOCATED_RANGES`.
#[repr(C)]
#[derive(Copy, Clone, Default, Debug)]
struct FileAllocatedRangeBuffer {
    file_offset: i64,
    length: i64,
}

#[derive(Error, Debug)]
pub enum SparseError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Win32 error ({0}): {1}")]
    Win32(u32, String),
    #[error("Invalid range requested: offset {offset}, length {length}, file size {file_size}")]
    InvalidRange {
        offset: u64,
        length: u64,
        file_size: u64,
    },
    #[error("Failed to query volume cluster size for path: {0}")]
    ClusterQueryFailed(String),
}

/// Marks a file as a sparse file on NTFS (`FSCTL_SET_SPARSE`).
///
/// If the file is already sparse, this call is idempotent and succeeds without error.
#[cfg(all(test, windows))]
fn make_sparse(file: &File) -> Result<(), SparseError> {
    let handle = HANDLE(file.as_raw_handle());
    let mut sparse_buffer = FileSetSparseBuffer { set_sparse: 1u8 };
    let mut bytes_returned = 0u32;

    let res = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_SET_SPARSE,
            Some(&mut sparse_buffer as *mut _ as *mut _),
            std::mem::size_of::<FileSetSparseBuffer>() as u32,
            None,
            0,
            Some(&mut bytes_returned),
            None,
        )
    };

    if res.is_err() {
        let err = unsafe { GetLastError() };
        return Err(SparseError::Win32(
            err.0,
            format!("FSCTL_SET_SPARSE failed with error code {}", err.0),
        ));
    }

    Ok(())
}

#[cfg(all(test, not(windows)))]
fn make_sparse(_file: &File) -> Result<(), SparseError> {
    Ok(())
}

/// Checks if a file has the `FILE_ATTRIBUTE_SPARSE_FILE` attribute set.
#[cfg(windows)]
pub fn is_sparse(path: &Path) -> Result<bool, SparseError> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let attrs = unsafe { GetFileAttributesW(PCWSTR::from_raw(wide.as_ptr())) };
    if attrs == u32::MAX {
        let err = unsafe { GetLastError() };
        return Err(SparseError::Win32(
            err.0,
            format!("GetFileAttributesW failed with code {}", err.0),
        ));
    }
    Ok((attrs & FILE_ATTRIBUTE_SPARSE_FILE.0) != 0)
}

#[cfg(not(windows))]
pub fn is_sparse(_path: &Path) -> Result<bool, SparseError> {
    Ok(false)
}

/// Deallocates (punches a hole) in the byte range `[offset, offset + length)` using `FSCTL_SET_ZERO_DATA`.
///
/// Automatically marks the file as sparse if it was not already sparse.
#[cfg(all(test, windows))]
fn zero_range(file: &File, offset: u64, length: u64) -> Result<(), SparseError> {
    if length == 0 {
        return Ok(());
    }

    if offset > i64::MAX as u64 || offset.saturating_add(length) > i64::MAX as u64 {
        return Err(SparseError::InvalidRange {
            offset,
            length,
            file_size: 0,
        });
    }

    // Ensure sparse attribute is enabled
    make_sparse(file)?;

    let handle = HANDLE(file.as_raw_handle());
    let mut zero_data = FileZeroDataInformation {
        file_offset: offset as i64,
        beyond_final_zero: (offset.saturating_add(length)) as i64,
    };
    let mut bytes_returned = 0u32;

    let res = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_SET_ZERO_DATA,
            Some(&mut zero_data as *mut _ as *mut _),
            std::mem::size_of::<FileZeroDataInformation>() as u32,
            None,
            0,
            Some(&mut bytes_returned),
            None,
        )
    };

    if res.is_err() {
        let err = unsafe { GetLastError() };
        return Err(SparseError::Win32(
            err.0,
            format!(
                "FSCTL_SET_ZERO_DATA failed for range [{}, {}) with error code {}",
                offset,
                offset.saturating_add(length),
                err.0
            ),
        ));
    }

    Ok(())
}

#[cfg(all(test, not(windows)))]
fn zero_range(file: &File, offset: u64, length: u64) -> Result<(), SparseError> {
    // Non-Windows simulation: write zero bytes directly
    use std::io::{Seek, SeekFrom, Write};
    let mut f = file;
    f.seek(SeekFrom::Start(offset))?;
    let zero_buf = vec![0u8; length.min(65536) as usize];
    let mut written = 0u64;
    while written < length {
        let to_write = (length - written).min(zero_buf.len() as u64) as usize;
        f.write_all(&zero_buf[..to_write])?;
        written += to_write as u64;
    }
    Ok(())
}

/// Queries allocated (non-zero) byte ranges in a sparse file using `FSCTL_QUERY_ALLOCATED_RANGES`.
/// Returns a list of `(offset, length)` tuples representing non-empty data extents.
#[cfg(windows)]
pub fn query_allocated_ranges(
    file: &File,
    start: u64,
    length: u64,
) -> Result<Vec<(u64, u64)>, SparseError> {
    if start > i64::MAX as u64 || length > i64::MAX as u64 {
        return Err(SparseError::InvalidRange {
            offset: start,
            length,
            file_size: 0,
        });
    }

    let handle = HANDLE(file.as_raw_handle());
    let mut query_range = FileAllocatedRangeBuffer {
        file_offset: start as i64,
        length: length as i64,
    };

    let mut result_ranges = Vec::new();
    let mut out_buffer = vec![FileAllocatedRangeBuffer::default(); 256];

    loop {
        let mut bytes_returned = 0u32;
        let res = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_QUERY_ALLOCATED_RANGES,
                Some(&mut query_range as *mut _ as *mut _),
                std::mem::size_of::<FileAllocatedRangeBuffer>() as u32,
                Some(out_buffer.as_mut_ptr() as *mut _),
                (out_buffer.len() * std::mem::size_of::<FileAllocatedRangeBuffer>()) as u32,
                Some(&mut bytes_returned),
                None,
            )
        };

        let count = bytes_returned as usize / std::mem::size_of::<FileAllocatedRangeBuffer>();
        for item in out_buffer.iter().take(count) {
            let offset = item.file_offset as u64;
            let len = item.length as u64;
            result_ranges.push((offset, len));
        }

        if res.is_ok() {
            // Completed all ranges in query window
            break;
        }

        let err = unsafe { GetLastError() };
        if err == ERROR_MORE_DATA {
            // Continue query starting from beyond the last received range
            if let Some(&(last_offset, last_len)) = result_ranges.last() {
                let next_start = last_offset.saturating_add(last_len);
                let end_query = start.saturating_add(length);
                if next_start >= end_query {
                    break;
                }
                query_range.file_offset = next_start as i64;
                query_range.length = (end_query - next_start) as i64;
            } else {
                break;
            }
        } else {
            return Err(SparseError::Win32(
                err.0,
                format!(
                    "FSCTL_QUERY_ALLOCATED_RANGES failed with error code {}",
                    err.0
                ),
            ));
        }
    }

    Ok(result_ranges)
}

#[cfg(not(windows))]
pub fn query_allocated_ranges(
    _file: &File,
    start: u64,
    length: u64,
) -> Result<Vec<(u64, u64)>, SparseError> {
    Ok(vec![(start, length)])
}

/// Aligns a zeroing range to whole volume cluster boundaries.
///
/// To safely zero data inside a container without corrupting adjacent records:
/// - Start is rounded **UP** to the next cluster boundary (`offset.div_ceil(cluster_size) * cluster_size`).
/// - End is rounded **DOWN** to the preceding cluster boundary (`(offset + length) / cluster_size * cluster_size`).
///
/// If `aligned_end <= aligned_start`, no full cluster can be zeroed without risking adjacent data,
/// so `None` is returned.
pub fn cluster_align_range(offset: u64, length: u64, cluster_size: u64) -> Option<(u64, u64)> {
    if cluster_size == 0 || length == 0 {
        return Some((offset, length));
    }

    // Round start UP
    let aligned_start = if offset.is_multiple_of(cluster_size) {
        offset
    } else {
        offset.saturating_add(cluster_size - (offset % cluster_size))
    };

    // Round end DOWN
    let end = offset.saturating_add(length);
    let aligned_end = end - (end % cluster_size);

    if aligned_end > aligned_start {
        Some((aligned_start, aligned_end - aligned_start))
    } else {
        None
    }
}

/// Retrieves the on-disk allocated physical size of `path` (in bytes).
/// Takes into account NTFS compression and sparse zeroed extents.
#[cfg(windows)]
pub fn get_on_disk_size(path: &Path) -> Result<u64, SparseError> {
    const INVALID_FILE_SIZE: u32 = u32::MAX;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let path_ptr = PCWSTR::from_raw(wide.as_ptr());

    let mut high: u32 = 0;
    let low = unsafe { GetCompressedFileSizeW(path_ptr, Some(&mut high)) };

    if low == INVALID_FILE_SIZE {
        let last = unsafe { GetLastError() };
        if last.0 != 0 {
            return Err(SparseError::Win32(
                last.0,
                format!("GetCompressedFileSizeW failed with code {}", last.0),
            ));
        }
    }

    Ok(((high as u64) << 32) | (low as u64))
}

#[cfg(not(windows))]
pub fn get_on_disk_size(path: &Path) -> Result<u64, SparseError> {
    std::fs::metadata(path)
        .map(|m| m.len())
        .map_err(SparseError::Io)
}

/// Queries the file system cluster size (in bytes) for the drive where `path` is located.
/// Returns 4096 (standard default) on error or non-Windows systems.
#[cfg(windows)]
pub fn get_cluster_size(path: &Path) -> u64 {
    let drive_root = get_drive_root(path);
    let wide: Vec<u16> = drive_root
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let root_ptr = PCWSTR::from_raw(wide.as_ptr());

    let mut sectors_per_cluster = 0u32;
    let mut bytes_per_sector = 0u32;
    let mut free_clusters = 0u32;
    let mut total_clusters = 0u32;

    let res = unsafe {
        GetDiskFreeSpaceW(
            root_ptr,
            Some(&mut sectors_per_cluster),
            Some(&mut bytes_per_sector),
            Some(&mut free_clusters),
            Some(&mut total_clusters),
        )
    };

    if res.is_ok() && bytes_per_sector > 0 && sectors_per_cluster > 0 {
        (bytes_per_sector as u64) * (sectors_per_cluster as u64)
    } else {
        4096
    }
}

#[cfg(not(windows))]
pub fn get_cluster_size(_path: &Path) -> u64 {
    4096
}

/// Helper to get the drive root string (e.g. `C:\` or `D:\`) from a path.
pub fn get_drive_root(path: &Path) -> String {
    if let Some(s) = path.to_str() {
        let s = s.strip_prefix(r"\\?\").unwrap_or(s);
        let bytes = s.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            let letter = (bytes[0] as char).to_ascii_uppercase();
            return format!(r"{letter}:\");
        }
    }
    r"C:\".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_cluster_align_range_exact() {
        let aligned = cluster_align_range(4096, 8192, 4096);
        assert_eq!(aligned, Some((4096, 8192)));
    }

    #[test]
    fn test_cluster_align_range_unaligned() {
        let aligned = cluster_align_range(5000, 20000, 4096);
        assert_eq!(aligned, Some((8192, 16384)));
    }

    #[test]
    fn test_cluster_align_range_too_small() {
        let aligned = cluster_align_range(1000, 2000, 4096);
        assert_eq!(aligned, None);
    }

    #[test]
    fn test_cluster_align_range_zero_cluster() {
        let aligned = cluster_align_range(100, 500, 0);
        assert_eq!(aligned, Some((100, 500)));
    }

    #[test]
    fn test_drive_root_extraction() {
        assert_eq!(get_drive_root(Path::new(r"C:\Games\Steam")), r"C:\");
        assert_eq!(get_drive_root(Path::new(r"d:\test\file.pck")), r"D:\");
        assert_eq!(get_drive_root(Path::new(r"\\?\E:\Library")), r"E:\");
    }

    #[test]
    fn test_sparse_file_operations_on_tempfile() {
        let mut file = NamedTempFile::new().expect("create tempfile");
        // Write 1 MiB of test data
        let buffer = vec![0xAAu8; 1024 * 1024];
        file.write_all(&buffer).expect("write data");
        file.flush().expect("flush data");

        // Make sparse
        let make_res = make_sparse(file.as_file());
        assert!(make_res.is_ok(), "make_sparse should succeed");

        // Zero a 256 KiB middle range [256 KiB, 512 KiB)
        let zero_res = zero_range(file.as_file(), 256 * 1024, 256 * 1024);
        assert!(zero_res.is_ok(), "zero_range should succeed");

        #[cfg(windows)]
        {
            let is_sp = is_sparse(file.path()).unwrap_or(false);
            assert!(is_sp, "file should have sparse attribute set");

            let ranges = query_allocated_ranges(file.as_file(), 0, 1024 * 1024).unwrap_or_default();
            assert!(
                !ranges.is_empty(),
                "allocated ranges query returned results"
            );
        }
    }

    #[test]
    fn test_cluster_align_range_extreme_values() {
        // Test near u64::MAX should not panic
        let aligned = cluster_align_range(u64::MAX - 1000, 500, 4096);
        assert_eq!(aligned, None);

        let aligned_zero_len = cluster_align_range(4096, 0, 4096);
        assert_eq!(aligned_zero_len, Some((4096, 0)));
    }

    #[test]
    fn test_zero_range_out_of_bounds() {
        let file = NamedTempFile::new().expect("create tempfile");
        // Request offset > i64::MAX
        let res = zero_range(file.as_file(), (i64::MAX as u64) + 100, 4096);
        assert!(res.is_err(), "Offset > i64::MAX must return error");
    }
}
