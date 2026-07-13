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

/// Opens `\\.\<letter>:` for raw, read-only, non-exclusive access.
pub fn open_volume(letter: char) -> Result<File> {
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
