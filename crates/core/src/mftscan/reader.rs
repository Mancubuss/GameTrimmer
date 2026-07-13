//! Performs the single linear pass over the NTFS Master File Table (MFT),
//! turning every in-use file/directory record into a [`MftRecord`] keyed by
//! File Record Number (FRN). This is the same core technique Everything
//! uses to enumerate an entire volume in one shot, instead of walking each
//! directory individually.

use std::io::{Read, Seek};

use ntfs::structured_values::{NtfsFileName, NtfsFileNamespace};
use ntfs::{Ntfs, NtfsAttributeType, NtfsFile, NtfsFileFlags};

use crate::error::{CoreError, Result};

use super::model::{FrnMap, MftRecord, NameAlias};

/// First File Record Number that can belong to a user file or directory.
/// Records `0..16` are reserved NTFS metadata: `$MFT`, `$MFTMirr`,
/// `$LogFile`, `$Volume`, `$AttrDef`, the root directory (5), `$Bitmap`,
/// `$Boot`, `$BadClus`, `$Secure`, `$UpCase`, `$Extend`, and reserved slots
/// 12-15. The root directory is intentionally excluded too: it has no
/// `$FILE_NAME` of its own and is only ever referenced as a parent.
const FIRST_USER_RECORD: u64 = 16;

/// Difference between the NTFS/Windows epoch (1601-01-01) and the Unix
/// epoch (1970-01-01), in 100-nanosecond intervals.
const NT_TO_UNIX_INTERVALS: i64 = 116_444_736_000_000_000;
const INTERVALS_PER_SECOND: i64 = 10_000_000;

fn nt_time_to_unix_secs(nt_timestamp: u64) -> i64 {
    (nt_timestamp as i64 - NT_TO_UNIX_INTERVALS).div_euclid(INTERVALS_PER_SECOND)
}

/// Walks every File Record of the NTFS volume backing `fs` and returns a map
/// of FRN to reconstructed record info, for in-use file and directory
/// records only.
pub fn build_frn_map<T>(fs: &mut T) -> Result<FrnMap>
where
    T: Read + Seek,
{
    let ntfs = Ntfs::new(fs).map_err(|e| CoreError::Other(format!("not an NTFS volume: {e}")))?;

    let mft_file = ntfs
        .file(fs, 0)
        .map_err(|e| CoreError::Other(format!("cannot read $MFT record: {e}")))?;
    let mft_data_item = mft_file
        .data(fs, "")
        .ok_or_else(|| CoreError::Other("$MFT has no $DATA attribute".to_string()))?
        .map_err(|e| CoreError::Other(format!("cannot read $MFT $DATA attribute: {e}")))?;
    let mft_data_attribute = mft_data_item
        .to_attribute()
        .map_err(|e| CoreError::Other(format!("cannot read $MFT $DATA attribute: {e}")))?;

    let record_size = ntfs.file_record_size() as u64;
    if record_size == 0 {
        return Err(CoreError::Other("NTFS file record size is 0".to_string()));
    }
    let record_count = mft_data_attribute.value_length() / record_size;

    let mut map = FrnMap::with_capacity((record_count as usize / 2).max(16));

    for frn in FIRST_USER_RECORD..record_count {
        let file = match ntfs.file(fs, frn) {
            Ok(file) => file,
            // Unused MFT slot (never allocated) or an unreadable/corrupt
            // record - skip it rather than aborting the whole volume scan.
            Err(_) => continue,
        };

        if !file.flags().contains(NtfsFileFlags::IN_USE) {
            continue; // deleted file whose slot has not been reused yet
        }

        if let Some(record) = collect_record(&file, fs) {
            map.insert(frn, record);
        }
    }

    Ok(map)
}

/// Returns the current length, in bytes, of `file`'s unnamed `$DATA`
/// stream (the "real" file size), or `0` if it has none (e.g. a file with
/// only a named alternate data stream).
///
/// For a non-resident `$DATA` attribute this is a free header read: the
/// byte length is stored directly in the attribute header alongside the
/// data-run list, so no extra disk I/O beyond the record itself is needed.
fn unnamed_data_size<T>(file: &NtfsFile<'_>, fs: &mut T) -> u64
where
    T: Read + Seek,
{
    let Some(Ok(item)) = file.data(fs, "") else {
        return 0;
    };
    let Ok(attribute) = item.to_attribute() else {
        return 0;
    };
    attribute.value_length()
}

/// Extracts every `$FILE_NAME` alias plus current size/mtime for one file
/// record. Returns `None` when the record has no usable `$FILE_NAME` at all
/// (should not happen for a healthy in-use record, but MFT corruption is
/// not impossible).
fn collect_record<T>(file: &NtfsFile<'_>, fs: &mut T) -> Option<MftRecord>
where
    T: Read + Seek,
{
    let is_directory = file.is_directory();
    let aliases = collect_aliases(file, fs);
    if aliases.is_empty() {
        return None;
    }

    let size = if is_directory {
        0
    } else {
        unnamed_data_size(file, fs)
    };

    let mtime = file
        .info()
        .ok()
        .map(|info| nt_time_to_unix_secs(info.modification_time().nt_timestamp()));

    Some(MftRecord {
        is_directory,
        size,
        mtime,
        aliases,
    })
}

/// Collects every `$FILE_NAME` attribute of `file`, skipping short
/// (`Dos`-namespace-only) 8.3 aliases: any file that only has a `Dos` name
/// also has a separate `Win32`/`Posix` `$FILE_NAME` with the real name, so
/// nothing is lost, and skipping avoids emitting bogus 8.3-shaped duplicate
/// paths.
fn collect_aliases<T>(file: &NtfsFile<'_>, fs: &mut T) -> Vec<NameAlias>
where
    T: Read + Seek,
{
    let mut aliases = Vec::new();
    let mut iter = file.attributes();

    while let Some(item) = iter.next(fs) {
        let Ok(item) = item else { continue };
        let Ok(attribute) = item.to_attribute() else {
            continue;
        };
        if !matches!(attribute.ty(), Ok(NtfsAttributeType::FileName)) {
            continue;
        }
        let Ok(file_name) = attribute.structured_value::<T, NtfsFileName>(fs) else {
            continue;
        };
        if file_name.namespace() == NtfsFileNamespace::Dos {
            continue;
        }

        aliases.push(NameAlias {
            parent_frn: file_name.parent_directory_reference().file_record_number(),
            name: file_name.name().to_string_lossy(),
        });
    }

    aliases
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nt_epoch_converts_to_unix_epoch() {
        // NT_TO_UNIX_INTERVALS is, by definition, the NT timestamp of the
        // Unix epoch (1970-01-01T00:00:00Z).
        assert_eq!(nt_time_to_unix_secs(NT_TO_UNIX_INTERVALS as u64), 0);
    }

    #[test]
    fn nt_time_one_second_after_unix_epoch() {
        let one_second_later = NT_TO_UNIX_INTERVALS as u64 + INTERVALS_PER_SECOND as u64;
        assert_eq!(nt_time_to_unix_secs(one_second_later), 1);
    }
}
