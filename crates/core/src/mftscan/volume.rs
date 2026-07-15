//! Opens raw NTFS volume handles for MFT scanning.
//!
//! Reading `\\.\<letter>:` directly requires the process to hold
//! Administrator privileges (or `SeBackupPrivilege`) - a normal,
//! non-elevated process gets `ERROR_ACCESS_DENIED`. That failure is
//! expected and is surfaced as a clear [`CoreError::Other`] rather than a
//! panic, so callers can fall back to a regular directory walk.

use std::fs::File;
use std::os::windows::io::FromRawHandle;

use windows::core::PCWSTR;
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};

use crate::error::{CoreError, Result};

use super::aligned::{sector_size, AlignedReader};

/// Buffer size for the [`AlignedReader`] wrapping a volume handle. The
/// dominant access pattern is the streaming `$MFT` pass, which reads the
/// table strictly sequentially - a large buffer turns that into few big
/// `ReadFile` syscalls instead of one per 64 KiB default-buffer fill. Raw
/// volume reads bypass the OS read-ahead cache, so the request size is the
/// only thing that amortizes per-syscall latency here. One buffer of this
/// size exists per concurrently-open volume, so the memory cost is bounded
/// and small.
const VOLUME_BUFFER_SIZE: usize = 4 * 1024 * 1024;

/// Opens `\\.\<letter>:` for raw, read-only, non-exclusive access, wrapped
/// in an [`AlignedReader`] so every read against the handle is
/// sector-aligned. Raw volume handles reject unaligned reads with
/// `ERROR_INVALID_PARAMETER`, which the `ntfs` crate turns into a panic
/// rather than an `Err` - see [`super::aligned`] for the full explanation.
/// The alignment used is the volume's actual physical sector size (via
/// [`sector_size`]), falling back to a safe default if that query fails.
pub fn open_volume(letter: char) -> Result<AlignedReader<File>> {
    let file = open_volume_file(letter)?;
    let alignment = sector_size(letter);
    Ok(AlignedReader::with_buffer_size(
        file,
        alignment,
        VOLUME_BUFFER_SIZE,
    ))
}

/// Opens `\\.\<letter>:` for raw, read-only, non-exclusive access, returning
/// the bare `File`. Internal: [`open_volume`] is the entry point callers
/// outside this module should use, since a bare `File` over a volume handle
/// must never be read from directly (see module docs).
fn open_volume_file(letter: char) -> Result<File> {
    let device_path = format!(r"\\.\{}:", letter.to_ascii_uppercase());
    let wide: Vec<u16> = device_path
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let filename = PCWSTR::from_raw(wide.as_ptr());

    // SAFETY: `filename` points into `wide`, a null-terminated UTF-16
    // buffer that outlives this call. No other arguments carry aliasing
    // requirements. `CreateFileW` itself validates the returned handle and
    // maps failure to `Err`, so we never observe an invalid handle here.
    let handle = unsafe {
        CreateFileW(
            filename,
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|e| {
        CoreError::Other(format!(
            "cannot open {device_path} for raw MFT scan (this needs Administrator \
             privileges, or SeBackupPrivilege, on a non-elevated process): {e}"
        ))
    })?;

    // SAFETY: `handle` was just returned by a successful `CreateFileW` call,
    // so it is a valid, uniquely-owned handle. Wrapping it in `File`
    // transfers ownership: `File`'s `Drop` impl closes it exactly once, and
    // we do not close it ourselves anywhere else.
    Ok(unsafe { File::from_raw_handle(handle.0 as *mut _) })
}

/// Best-effort check for whether `volume` can likely be MFT-scanned right
/// now: it must be a real volume, openable for raw read access. This
/// implies both "is NTFS-or-at-least-openable" and "we currently hold
/// sufficient privileges" - both are collapsed into one boolean because the
/// only reliable way to test either is to attempt the same open that
/// `scan_roots` will perform.
pub fn is_available(volume: char) -> bool {
    open_volume(volume).is_ok()
}
