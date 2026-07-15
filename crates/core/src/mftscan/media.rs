//! Media-type detection for scan routing: does a volume's physical device
//! incur a seek penalty (rotational HDD) or not (SSD/NVMe)?
//!
//! Measured rationale (mft_bench on a real machine): reading a volume's
//! entire `$MFT` costs the same regardless of how much of the volume the
//! game libraries occupy, while a `walkdir` of just the library subtrees
//! costs only what those subtrees contain. On an SSD, cold-cache directory
//! walking is nearly as fast as warm (random metadata reads are cheap), so
//! the full-volume MFT read *loses* by an order of magnitude. On an HDD,
//! every cold directory read is a head seek and walking a large library
//! takes minutes, while the MFT read stays at tens of seconds - there the
//! MFT path wins by an order of magnitude. So the seek penalty of the
//! underlying device is exactly the property that decides the faster path.
//!
//! Unlike the raw `$MFT` read itself, this query needs no Administrator
//! rights: the volume handle is opened with zero access rights, which is
//! sufficient for `IOCTL_STORAGE_QUERY_PROPERTY`.

use std::fs::File;
use std::os::windows::io::{AsRawHandle, FromRawHandle};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Ioctl::{
    PropertyStandardQuery, StorageDeviceSeekPenaltyProperty, DEVICE_SEEK_PENALTY_DESCRIPTOR,
    IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_PROPERTY_QUERY,
};
use windows::Win32::System::IO::DeviceIoControl;

/// What kind of physical medium backs a volume, as far as scan routing
/// cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// The device reports a seek penalty - a rotational HDD. The MFT path
    /// is the right choice here.
    SeekPenalty,
    /// The device reports no seek penalty - an SSD/NVMe. A plain directory
    /// walk of just the library subtrees beats reading the whole volume's
    /// `$MFT`, even on a cold cache.
    NoSeekPenalty,
    /// The query failed (no such volume, an unusual device, a filter driver
    /// that doesn't forward the IOCTL, ...). Routing treats this like
    /// [`MediaKind::SeekPenalty`]: the MFT path is correct on any NTFS
    /// volume, merely not always the fastest, so unknown hardware keeps the
    /// behavior the user explicitly asked for.
    Unknown,
}

/// Queries whether the device backing `\\.\<letter>:` incurs a seek
/// penalty. Never fails - every failure mode collapses into
/// [`MediaKind::Unknown`], so callers always get a routable answer.
pub fn media_kind(letter: char) -> MediaKind {
    let Some(file) = open_volume_no_access(letter) else {
        return MediaKind::Unknown;
    };

    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceSeekPenaltyProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut descriptor = DEVICE_SEEK_PENALTY_DESCRIPTOR::default();
    let mut bytes_returned: u32 = 0;

    // SAFETY: the input/output pointers reference local variables that
    // outlive the call, with sizes reported from those exact types; the
    // handle is a valid volume handle owned by `file` for the duration of
    // the call (no OVERLAPPED, so the call is fully synchronous).
    let result = unsafe {
        DeviceIoControl(
            HANDLE(file.as_raw_handle()),
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query as *const _ as *const _),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut descriptor as *mut _ as *mut _),
            std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
            Some(&mut bytes_returned),
            None,
        )
    };

    match result {
        Ok(())
            if bytes_returned as usize >= std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() =>
        {
            if descriptor.IncursSeekPenalty {
                MediaKind::SeekPenalty
            } else {
                MediaKind::NoSeekPenalty
            }
        }
        _ => MediaKind::Unknown,
    }
}

/// Opens `\\.\<letter>:` with zero desired access - enough for device
/// property IOCTLs and, crucially, allowed without Administrator rights
/// (unlike the `GENERIC_READ` open the raw MFT scan itself needs).
fn open_volume_no_access(letter: char) -> Option<File> {
    let device_path = format!(r"\\.\{}:", letter.to_ascii_uppercase());
    let wide: Vec<u16> = device_path
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: the filename pointer references `wide`, a null-terminated
    // UTF-16 buffer that outlives the call; a failed open is mapped to
    // `Err` by the binding, so no invalid handle is ever observed.
    let handle = unsafe {
        CreateFileW(
            PCWSTR::from_raw(wide.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .ok()?;

    // SAFETY: `handle` was just returned by a successful `CreateFileW`
    // call, so ownership transfers to `File`, whose `Drop` closes it
    // exactly once.
    Some(unsafe { File::from_raw_handle(handle.0 as *mut _) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `media_kind` must be total: whatever hardware the test machine has
    /// (including no such volume at all), it returns some variant and never
    /// panics or blocks on elevation. `C:` always exists on a Windows
    /// machine; `1` is never a valid drive letter.
    #[test]
    fn media_kind_is_total_for_existing_and_bogus_volumes() {
        let _ = media_kind('C');
        assert_eq!(media_kind('1'), MediaKind::Unknown);
    }
}
