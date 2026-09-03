//! Streams the NTFS Master File Table (MFT) sequentially and turns every
//! in-use file/directory record into a [`MftRecord`] keyed by File Record
//! Number (FRN). This is the same core technique Everything uses to
//! enumerate an entire volume in one shot, instead of walking each
//! directory individually - and, since this module reads the `$MFT`'s
//! `$DATA` stream strictly in order (never seeking backward), every byte of
//! the table is read from disk exactly once.
//!
//! The `ntfs` crate is used only for the initial bootstrap: parsing the
//! boot sector ([`Ntfs::new`]) and locating the `$MFT`'s own record and
//! `$DATA` attribute. From there, the actual record-by-record parsing is
//! done by [`super::record`] - a pure, `ntfs`-crate-independent parser
//! operating on the raw bytes streamed in by [`drive_scan`] below.

use std::io::{Read, Seek};
use std::sync::atomic::{AtomicBool, Ordering};

use ntfs::{Ntfs, NtfsReadSeek};

use crate::error::{CoreError, Result};
use crate::perf;

use super::model::FrnMap;
use super::record::{self, FrnAccumulator};
use super::{reborrow_progress, MftProgress};

/// First File Record Number that can belong to a user file or directory.
/// Records `0..16` are reserved NTFS metadata: `$MFT`, `$MFTMirr`,
/// `$LogFile`, `$Volume`, `$AttrDef`, the root directory (5), `$Bitmap`,
/// `$Boot`, `$BadClus`, `$Secure`, `$UpCase`, `$Extend`, and reserved slots
/// 12-15. The root directory is intentionally excluded too: it has no
/// `$FILE_NAME` of its own and is only ever referenced as a parent.
const FIRST_USER_RECORD: u64 = 16;

/// Target size of each streamed read against the `$MFT`'s `$DATA` stream,
/// rounded down to a whole number of records (see `build_frn_map`). Large
/// enough that a full-volume scan issues only a few dozen reads total
/// instead of one per record; small enough to keep peak memory use low.
const STREAM_CHUNK_BYTES: u64 = 8 * 1024 * 1024; // 8 MiB

/// Walks every File Record of the NTFS volume backing `fs`, streaming the
/// `$MFT`'s `$DATA` attribute sequentially in [`STREAM_CHUNK_BYTES`]-sized
/// chunks, and returns a map of FRN to reconstructed record info for
/// in-use file and directory records only.
///
/// `volume` is used only to label [`MftProgress`] updates. `progress`, if
/// given, is called once per chunk read (not per record - that would be far
/// too chatty for a table with hundreds of thousands of records). `cancel`,
/// if given, is polled once per chunk; once set, this returns
/// `Err(CoreError::Other("cancelled"))` without reading any further chunks.
pub fn build_frn_map<T>(
    fs: &mut T,
    volume: char,
    progress: Option<&mut dyn FnMut(MftProgress)>,
    cancel: Option<&AtomicBool>,
) -> Result<FrnMap>
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

    let mut value = mft_data_attribute
        .value(fs)
        .map_err(|e| CoreError::Other(format!("cannot open $MFT $DATA stream: {e}")))?;

    let chunk_records = (STREAM_CHUNK_BYTES / record_size).max(1);

    drive_scan(
        record_count,
        chunk_records,
        record_size,
        volume,
        |_frn_offset, take_records| {
            let take_bytes = (take_records * record_size) as usize;
            let mut buf = vec![0u8; take_bytes];
            value
                .read_exact(fs, &mut buf)
                .map_err(|e| CoreError::Other(format!("failed reading $MFT data: {e}")))?;
            Ok(buf)
        },
        progress,
        cancel,
    )
}

/// Drives one full `$MFT` streaming pass: for `total_records` records, in
/// chunks of up to `chunk_records`, calls `fetch_chunk(frn_offset,
/// take_records)` to obtain each chunk's raw bytes, parses and accumulates
/// them via [`record::parse_chunk`]/[`FrnAccumulator`], and reports
/// progress/cancellation.
///
/// Pulled out of [`build_frn_map`] specifically so this control flow - the
/// part that actually matters for correctness (every chunk gets fetched
/// exactly once and in order, progress fires once per chunk, cancellation
/// is honored promptly) - can be unit tested with a synthetic in-memory
/// `fetch_chunk`, without a real NTFS volume or the `ntfs` crate's
/// bootstrap.
fn drive_scan<F>(
    total_records: u64,
    chunk_records: u64,
    record_size: u64,
    volume: char,
    mut fetch_chunk: F,
    mut progress: Option<&mut dyn FnMut(MftProgress)>,
    cancel: Option<&AtomicBool>,
) -> Result<FrnMap>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>>,
{
    // See `reborrow_progress`'s doc comment for why this indirection is
    // needed instead of `progress.as_deref_mut()` inside the loop below.
    let mut acc = FrnAccumulator::with_capacity((total_records as usize / 2).max(16));
    let mut frn_offset = 0u64;

    while frn_offset < total_records {
        if let Some(flag) = cancel {
            if flag.load(Ordering::Relaxed) {
                return Err(CoreError::Other("cancelled".to_string()));
            }
        }

        let take = (total_records - frn_offset).min(chunk_records);
        // Reading and parsing are timed apart because the proposal to split
        // them across two threads only pays if both halves are substantial;
        // this loop does them strictly alternately, so whichever is smaller
        // is the ceiling on that split. See `crate::perf`.
        let mut chunk_bytes = perf::timed(perf::Stage::MftRead, || fetch_chunk(frn_offset, take))?;
        // Paired with the stage's duration this is the run's warm/cold
        // reading - see `perf::mft_throughput`.
        perf::add_mft_bytes(chunk_bytes.len() as u64);

        perf::timed(perf::Stage::MftParse, || {
            for parsed in record::parse_chunk(&mut chunk_bytes, record_size, frn_offset) {
                // Nearly-free tally off data `add` already has in hand -
                // see `perf::add_mft_record`.
                perf::add_mft_record(parsed.in_use);
                acc.add(parsed);
            }
        });

        frn_offset += take;

        if let Some(cb) = reborrow_progress(&mut progress) {
            cb(MftProgress {
                volume,
                records_done: frn_offset,
                records_total: total_records,
            });
        }
    }

    Ok(perf::timed(perf::Stage::MftFinish, || {
        acc.finish(FIRST_USER_RECORD)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// A chunk's worth of all-zero bytes never has a "FILE" signature, so
    /// every record in it is skipped by the parser - fine for exercising
    /// the chunk loop's own control flow (fetch/progress/cancel) without
    /// needing valid record content.
    fn zeroed_chunk(take_records: u64, record_size: u64) -> Vec<u8> {
        vec![0u8; (take_records * record_size) as usize]
    }

    #[test]
    fn drive_scan_fetches_every_chunk_and_reports_progress_once_each() {
        let record_size = 64u64;
        let total_records = 10u64;
        let chunk_records = 4u64;

        let mut fetch_calls = 0u32;
        let fetch = |_offset: u64, take: u64| -> Result<Vec<u8>> {
            fetch_calls += 1;
            Ok(zeroed_chunk(take, record_size))
        };

        let mut progress_calls: Vec<(u64, u64)> = Vec::new();
        let mut progress_cb = |p: MftProgress| {
            assert_eq!(p.volume, 'G');
            progress_calls.push((p.records_done, p.records_total));
        };

        let result = drive_scan(
            total_records,
            chunk_records,
            record_size,
            'G',
            fetch,
            Some(&mut progress_cb),
            None,
        );

        assert!(result.is_ok());
        assert_eq!(fetch_calls, 3); // chunks of 4, 4, 2
        assert_eq!(
            progress_calls,
            vec![(4, 10), (8, 10), (10, 10)],
            "progress must fire exactly once per chunk, with a running total"
        );
    }

    #[test]
    fn drive_scan_stops_promptly_once_cancelled() {
        let record_size = 64u64;
        let total_records = 10u64;
        let chunk_records = 4u64;
        let cancel = AtomicBool::new(false);

        let mut fetch_calls = 0u32;
        let fetch = |_offset: u64, take: u64| -> Result<Vec<u8>> {
            fetch_calls += 1;
            Ok(zeroed_chunk(take, record_size))
        };

        // Flip the cancel flag as soon as the first chunk's progress is
        // reported - the loop's *next* cancellation check (before fetching
        // chunk 2) must see it and stop, never fetching another chunk.
        let mut progress_cb = |_p: MftProgress| {
            cancel.store(true, Ordering::Relaxed);
        };

        let result = drive_scan(
            total_records,
            chunk_records,
            record_size,
            'G',
            fetch,
            Some(&mut progress_cb),
            Some(&cancel),
        );

        assert!(result.is_err(), "a cancelled scan must return Err");
        assert_eq!(
            fetch_calls, 1,
            "no further chunk should be fetched once cancelled"
        );
    }

    #[test]
    fn drive_scan_never_checks_cancel_flag_when_none_is_given() {
        let record_size = 64u64;
        let fetch =
            |_offset: u64, take: u64| -> Result<Vec<u8>> { Ok(zeroed_chunk(take, record_size)) };

        let result = drive_scan(5, 2, record_size, 'D', fetch, None, None);
        assert!(result.is_ok());
    }
}
