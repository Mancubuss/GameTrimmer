//! A sector-aligned `Read + Seek` adapter for raw NTFS volume handles.
//!
//! # Why this exists
//! Windows requires every read issued against a raw volume handle
//! (`\\.\<letter>:`) to be sector-aligned: both the byte offset being read
//! from and the length of the read must be a multiple of the volume's
//! physical sector size. This holds regardless of whether
//! `FILE_FLAG_NO_BUFFERING` was used to open the handle - it is a property
//! of volume handles specifically, not of the buffering mode.
//!
//! The `ntfs` crate (via `binrw`) reads small, arbitrarily-offset fields
//! directly off whatever reader it is given - e.g. 3 bytes for `bootjmp` at
//! byte offset 0 of the boot sector - with no awareness of this
//! constraint. Handing it a bare `File` wrapping a volume handle causes the
//! very first such read to fail with `ERROR_INVALID_PARAMETER (87)`, which
//! `ntfs` turns into a panic (`ntfs-0.4.0/src/error.rs:236`) instead of a
//! recoverable `Err`.
//!
//! A plain [`std::io::BufReader`] does **not** fix this: `BufReader`
//! discards its internal buffer on `seek()` and reads from the new,
//! potentially unaligned position on the next `read()`. [`AlignedReader`]
//! instead keeps its own logical position independent of the wrapped
//! reader's actual file pointer, and only ever touches the wrapped reader
//! with an aligned `seek` + aligned-length `read` pair.

use std::io::{self, Read, Seek, SeekFrom};

#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceW;

/// Fallback alignment used when the physical sector size of a volume
/// cannot be queried. `4096` is a multiple of every sector size in common
/// use (512 and 4096 byte sectors), so aligning to it also satisfies
/// alignment to the true, unknown sector size.
pub const FALLBACK_SECTOR_SIZE: u64 = 4096;

/// Wraps a `Read + Seek` byte source so every read actually issued to it is
/// sector-aligned, while presenting an ordinary unaligned `Read + Seek`
/// interface to callers. See the module docs for why this is required for
/// raw NTFS volume handles.
pub struct AlignedReader<T> {
    inner: T,
    alignment: u64,
    buf: Vec<u8>,
    /// Aligned byte offset (relative to the start of `inner`) that `buf[0]`
    /// corresponds to. Only meaningful when `buf_len > 0`.
    buf_start: u64,
    /// Number of valid bytes currently in `buf`, starting at `buf_start`.
    /// Less than `buf.len()` only at the tail of `inner`.
    buf_len: usize,
    /// This reader's logical position, tracked independently of `inner`'s
    /// actual file pointer. `inner`'s file pointer is only ever left at an
    /// aligned offset, immediately after a buffer fill.
    pos: u64,
}

impl<T: Read + Seek> AlignedReader<T> {
    /// Wraps `inner`, aligning all reads to `alignment` bytes using an
    /// internal buffer of (at least) `buffer_size` bytes - the effective
    /// size is rounded up to the next multiple of `alignment` if
    /// `buffer_size` is not already one.
    ///
    /// `alignment` should be the volume's physical sector size (see
    /// [`sector_size`]); when in doubt, [`FALLBACK_SECTOR_SIZE`] is always
    /// safe to pass since it is a multiple of every real-world sector size.
    ///
    /// # Panics
    /// Panics if `alignment` is `0`.
    pub fn with_buffer_size(inner: T, alignment: u64, buffer_size: usize) -> Self {
        assert!(alignment > 0, "AlignedReader: alignment must be non-zero");
        let buffer_size = align_up(buffer_size.max(1) as u64, alignment) as usize;
        Self {
            inner,
            alignment,
            buf: vec![0u8; buffer_size],
            buf_start: 0,
            buf_len: 0,
            pos: 0,
        }
    }

    /// Whether `self.pos` currently falls inside the valid buffered range.
    fn pos_in_buffer(&self) -> bool {
        self.buf_len > 0
            && self.pos >= self.buf_start
            && self.pos < self.buf_start + self.buf_len as u64
    }

    /// Refills the internal buffer so it covers `self.pos`, issuing a
    /// single aligned `seek` + aligned-length `read` pair against `inner`.
    ///
    /// Assumes `inner` never produces a "short" read (fewer bytes than
    /// requested) except at true end-of-medium - true for `File`/raw
    /// volume handles (a `ReadFile` against a block device either
    /// satisfies the full request or hits the true end in one call), and
    /// true for the `Cursor`-backed mocks used in this module's tests. A
    /// reader that can produce spurious short reads for other reasons
    /// (e.g. a pipe) is not a valid `T` for this adapter.
    fn fill_buffer(&mut self) -> io::Result<()> {
        let aligned_start = align_down(self.pos, self.alignment);
        self.inner.seek(SeekFrom::Start(aligned_start))?;

        let n = loop {
            match self.inner.read(&mut self.buf[..]) {
                Ok(n) => break n,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        };

        self.buf_start = aligned_start;
        self.buf_len = n;
        Ok(())
    }
}

impl<T: Read + Seek> Read for AlignedReader<T> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }

        if !self.pos_in_buffer() {
            self.fill_buffer()?;
        }

        if self.buf_len == 0 {
            return Ok(0); // true EOF: the aligned block at `pos` is empty.
        }

        let buf_offset = (self.pos - self.buf_start) as usize;
        if buf_offset >= self.buf_len {
            // `pos` lands inside the aligned block we just fetched, but
            // past the real data it contained - i.e. we are exactly at the
            // tail of `inner`.
            return Ok(0);
        }

        let available = self.buf_len - buf_offset;
        let n = available.min(out.len());
        out[..n].copy_from_slice(&self.buf[buf_offset..buf_offset + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<T: Read + Seek> Seek for AlignedReader<T> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        // Widen to i128 so `Start`/`Current`/`End` all compose without a
        // risk of intermediate overflow, then range-check once at the end.
        let new_pos: i128 = match pos {
            SeekFrom::Start(offset) => offset as i128,
            SeekFrom::Current(delta) => self.pos as i128 + delta as i128,
            SeekFrom::End(delta) => {
                // The only way to learn the true end of `inner` without
                // tracking the medium size ourselves is to ask `inner`
                // directly. This is a plain position query
                // (`SetFilePointer`-equivalent), not a read, so it carries
                // no sector-alignment requirement - unlike `read()`, it is
                // safe to forward straight through.
                let end = self.inner.seek(SeekFrom::End(0))?;
                end as i128 + delta as i128
            }
        };

        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "AlignedReader: attempted to seek to a negative position",
            ));
        }

        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}

fn align_down(value: u64, alignment: u64) -> u64 {
    value - (value % alignment)
}

fn align_up(value: u64, alignment: u64) -> u64 {
    let remainder = value % alignment;
    if remainder == 0 {
        value
    } else {
        value + (alignment - remainder)
    }
}

/// Queries the physical sector size of the volume mounted at `letter` via
/// `GetDiskFreeSpaceW`. Falls back to [`FALLBACK_SECTOR_SIZE`] if the query
/// fails for any reason (no such volume, permissions, an unusual
/// filesystem, ...) - callers always get a usable alignment value, even if
/// it ends up coarser than strictly necessary.
#[cfg(windows)]
pub fn sector_size(letter: char) -> u64 {
    let root = format!(r"{}:\", letter.to_ascii_uppercase());
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    let path = PCWSTR::from_raw(wide.as_ptr());

    let mut bytes_per_sector: u32 = 0;
    let mut sectors_per_cluster: u32 = 0;
    let mut free_clusters: u32 = 0;
    let mut total_clusters: u32 = 0;

    // SAFETY: `path` points into `wide`, a null-terminated UTF-16 buffer
    // that outlives this call. Every output pointer is `Some(&mut _)`
    // pointing at a local, live `u32` that outlives the call, matching
    // what `GetDiskFreeSpaceW` expects to write through.
    let result = unsafe {
        GetDiskFreeSpaceW(
            path,
            Some(&mut sectors_per_cluster),
            Some(&mut bytes_per_sector),
            Some(&mut free_clusters),
            Some(&mut total_clusters),
        )
    };

    match result {
        Ok(()) if bytes_per_sector > 0 => bytes_per_sector as u64,
        _ => FALLBACK_SECTOR_SIZE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Wraps a `Cursor<Vec<u8>>`, panicking if a `read` is ever issued at a
    /// position or with a requested length that is not a multiple of
    /// `alignment` - exactly the constraint a real raw NTFS volume handle
    /// enforces. A test built on this mock that completes without
    /// panicking is a direct proof that every I/O call `AlignedReader`
    /// actually issues is sector-aligned, regardless of what unaligned
    /// offsets/lengths callers ask *it* for.
    struct CheckedCursor {
        inner: Cursor<Vec<u8>>,
        alignment: u64,
    }

    impl CheckedCursor {
        fn new(data: Vec<u8>, alignment: u64) -> Self {
            Self {
                inner: Cursor::new(data),
                alignment,
            }
        }
    }

    impl Read for CheckedCursor {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let pos = self.inner.position();
            assert_eq!(
                pos % self.alignment,
                0,
                "unaligned read offset: {pos} (alignment {})",
                self.alignment
            );
            assert_eq!(
                buf.len() as u64 % self.alignment,
                0,
                "unaligned read length: {} (alignment {})",
                buf.len(),
                self.alignment
            );
            self.inner.read(buf)
        }
    }

    impl Seek for CheckedCursor {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    fn make_data(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 256) as u8).collect()
    }

    #[test]
    fn reads_bootjmp_sized_field_at_offset_zero() {
        let alignment = 512;
        let data = make_data(4096);
        let cursor = CheckedCursor::new(data.clone(), alignment);
        let mut reader = AlignedReader::with_buffer_size(cursor, alignment, 1024);

        let mut buf = [0u8; 3];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(buf, data[0..3]);
    }

    #[test]
    fn reads_small_fields_at_various_unaligned_offsets() {
        let alignment = 512;
        let data = make_data(8192);
        let cursor = CheckedCursor::new(data.clone(), alignment);
        let mut reader = AlignedReader::with_buffer_size(cursor, alignment, 2048);

        for &(offset, len) in &[
            (0u64, 3usize),
            (1, 1),
            (7, 8),
            (511, 2),
            (513, 4),
            (1000, 6),
            (2047, 1),
            (2048, 5),
            (4095, 1),
        ] {
            reader.seek(SeekFrom::Start(offset)).unwrap();
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf).unwrap();
            assert_eq!(
                buf,
                data[offset as usize..offset as usize + len],
                "offset {offset} len {len}"
            );
        }
    }

    #[test]
    fn seek_forward_then_backward_reuses_and_refills_buffer_correctly() {
        let alignment = 512;
        let data = make_data(8192);
        let cursor = CheckedCursor::new(data.clone(), alignment);
        let mut reader = AlignedReader::with_buffer_size(cursor, alignment, 1024);

        // Forward past the first buffer window (forces a refill).
        reader.seek(SeekFrom::Start(3000)).unwrap();
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(buf, data[3000..3004]);

        // Backward, before the current buffer window (forces another
        // refill, seeking `inner` backwards).
        reader.seek(SeekFrom::Start(10)).unwrap();
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(buf, data[10..14]);

        // Forward again, back inside a window already read once before.
        reader.seek(SeekFrom::Start(3002)).unwrap();
        let mut buf = [0u8; 2];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(buf, data[3002..3004]);
    }

    #[test]
    fn seek_current_and_end_resolve_to_correct_positions() {
        let alignment = 512;
        let data = make_data(4096);
        let cursor = CheckedCursor::new(data.clone(), alignment);
        let mut reader = AlignedReader::with_buffer_size(cursor, alignment, 1024);

        reader.seek(SeekFrom::Start(100)).unwrap();
        let pos = reader.seek(SeekFrom::Current(50)).unwrap();
        assert_eq!(pos, 150);

        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(buf, data[150..154]);

        let end_pos = reader.seek(SeekFrom::End(-10)).unwrap();
        assert_eq!(end_pos, data.len() as u64 - 10);
        let mut tail = [0u8; 10];
        reader.read_exact(&mut tail).unwrap();
        assert_eq!(tail, data[data.len() - 10..]);
    }

    #[test]
    fn seek_to_negative_position_is_an_error() {
        let alignment = 512;
        let cursor = CheckedCursor::new(make_data(4096), alignment);
        let mut reader = AlignedReader::with_buffer_size(cursor, alignment, 1024);

        let err = reader.seek(SeekFrom::Current(-1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn read_spans_a_buffer_refill_boundary() {
        let alignment = 512;
        let buffer_size = 1024; // small buffer: easy to force a mid-read refill.
        let data = make_data(4096);
        let cursor = CheckedCursor::new(data.clone(), alignment);
        let mut reader = AlignedReader::with_buffer_size(cursor, alignment, buffer_size);

        // [1000, 1100) straddles the 1024-byte buffer window boundary.
        reader.seek(SeekFrom::Start(1000)).unwrap();
        let mut buf = vec![0u8; 100];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(buf, data[1000..1100]);
    }

    #[test]
    fn read_trims_correctly_at_the_tail_of_the_medium() {
        let alignment = 512;
        let data_len = 4096 + 200; // deliberately not a multiple of `alignment`.
        let data = make_data(data_len);
        let cursor = CheckedCursor::new(data.clone(), alignment);
        let mut reader = AlignedReader::with_buffer_size(cursor, alignment, 2048);

        reader
            .seek(SeekFrom::Start((data_len - 10) as u64))
            .unwrap();
        let mut buf = vec![0u8; 10];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(buf, data[data_len - 10..]);

        // Exactly at true EOF now: further reads return Ok(0), not an
        // error and not a panic.
        let mut extra = [0u8; 1];
        let n = reader.read(&mut extra).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn read_exact_past_true_eof_yields_unexpected_eof_error() {
        let alignment = 512;
        let data = make_data(100);
        let cursor = CheckedCursor::new(data, alignment);
        let mut reader = AlignedReader::with_buffer_size(cursor, alignment, 512);

        reader.seek(SeekFrom::Start(95)).unwrap();
        let mut buf = [0u8; 10]; // only 5 bytes actually remain.
        let err = reader.read_exact(&mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn with_buffer_size_rounds_buffer_up_to_alignment_multiple() {
        let alignment = 512;
        let cursor = CheckedCursor::new(make_data(4096), alignment);
        // 100 is not a multiple of 512; must be rounded up to 512, not
        // truncated to 0 or left unaligned (which would itself violate the
        // invariant this whole type exists to guarantee).
        let mut reader = AlignedReader::with_buffer_size(cursor, alignment, 100);
        assert_eq!(reader.buf.len(), 512);

        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).unwrap();
    }
}
