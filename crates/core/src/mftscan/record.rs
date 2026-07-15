//! Pure, `ntfs`-crate-independent parser for the raw bytes of one NTFS FILE
//! record, plus in-memory accumulation of a base record's attributes with
//! those contributed by its extension records.
//!
//! No I/O happens anywhere in this module - everything here operates on
//! byte slices already in memory, which is what makes it fully unit
//! testable with synthetic records (no real NTFS volume required). See
//! [`super::reader`] for the streaming disk-read glue that feeds this
//! module chunk by chunk.
//!
//! # On-disk layout this module understands
//! Every FILE record starts with a common header (magic, Update Sequence
//! Array location, flags, `base_file_record` reference, first attribute
//! offset), followed by a sequence of attributes, each with its own header
//! (type, length, resident/non-resident flag) and - for the handful of
//! attribute types this module cares about - a value laid out exactly as
//! documented in the field comments below. These offsets are the stable,
//! on-disk NTFS format (unrelated to any particular library's in-memory
//! representation); they match what the `ntfs` crate itself parses,
//! cross-checked against its source.

use std::collections::HashMap;

use super::model::{FrnMap, MftRecord, NameAlias};

/// Magic bytes at the start of every FILE record.
const FILE_SIGNATURE: [u8; 4] = *b"FILE";

/// Every Update Sequence Array fixup applies to 512-byte chunks of a
/// record, regardless of the volume's actual physical sector size - this is
/// a fixed property of the on-disk NTFS format, not a tunable.
const FIXUP_SECTOR_SIZE: usize = 512;

/// Smallest number of bytes this parser needs to be able to read the fixed
/// FILE record header fields it uses (up through `base_file_record` at byte
/// 32..40). Anything shorter cannot be a real record.
const MIN_HEADER_SIZE: usize = 42;

/// Difference between the NTFS/Windows epoch (1601-01-01) and the Unix
/// epoch (1970-01-01), in 100-nanosecond intervals.
const NT_TO_UNIX_INTERVALS: i64 = 116_444_736_000_000_000;
const INTERVALS_PER_SECOND: i64 = 10_000_000;

fn nt_time_to_unix_secs(nt_timestamp: u64) -> i64 {
    (nt_timestamp as i64 - NT_TO_UNIX_INTERVALS).div_euclid(INTERVALS_PER_SECOND)
}

fn read_u16(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

fn read_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

fn read_u64(buf: &[u8], offset: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buf[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

/// Reads a File Reference (an 8-byte field pairing a 48-bit File Record
/// Number with a 16-bit sequence number) and returns just the FRN, matching
/// `ntfs::NtfsFileReference::file_record_number`.
fn read_frn48(buf: &[u8], offset: usize) -> u64 {
    read_u64(buf, offset) & 0xFFFF_FFFF_FFFF
}

/// Everything one parsed FILE record (base or extension) contributes
/// towards its logical file's final [`MftRecord`]. An extension record
/// (`base_frn != 0`) contributes to a *different* FRN's entry than its own;
/// see [`FrnAccumulator`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRecord {
    /// This record's own File Record Number (its position in the table).
    pub frn: u64,
    /// The base record's FRN if this is an extension record, or `0` if
    /// this record *is* the base record for its logical file.
    pub base_frn: u64,
    /// Whether the `IN_USE` flag is set. `false` for a deleted file whose
    /// slot has not been reused yet - every other field is left at its
    /// default in that case, since nothing else is worth extracting.
    pub in_use: bool,
    /// Whether the `IS_DIRECTORY` flag is set. Only meaningful (and only
    /// consulted by [`FrnAccumulator`]) when `base_frn == 0`.
    pub is_directory: bool,
    /// The unnamed `$DATA` attribute's size, if this record carries one.
    pub unnamed_data_size: Option<u64>,
    /// `$STANDARD_INFORMATION`'s modification time, converted to Unix
    /// seconds, if this record carries that attribute.
    pub mtime: Option<i64>,
    /// Every non-DOS-only `$FILE_NAME` alias this record carries.
    pub aliases: Vec<NameAlias>,
}

/// Applies Update Sequence Array fixups to `buf` in place: verifies the
/// Update Sequence Number (USN) stored at the end of every 512-byte chunk
/// of the record, then replaces those 2 bytes with the real on-disk content
/// (which NTFS moves into the USA specifically so the true bytes survive
/// this round trip). Returns `None` if the USA is out of bounds or any
/// sector's USN does not match - both indicate a corrupt or torn record,
/// which the caller skips entirely rather than trusting partially-fixed-up
/// data.
///
/// This mirrors `ntfs::record::Record::fixup` exactly (cross-checked
/// against that crate's source), including the fixed 512-byte stride.
fn apply_fixups(buf: &mut [u8]) -> Option<()> {
    if buf.len() < 8 {
        return None;
    }

    let usa_offset = read_u16(buf, 4) as usize;
    let usa_count = read_u16(buf, 6);

    // `usa_count` includes the USN itself; the number of fixup-able sectors
    // is one less.
    let sector_count = usa_count.checked_sub(1)? as usize;

    let array_start = usa_offset + 2;
    let array_end = usa_offset + usa_count as usize * 2;
    let sectors_end = sector_count * FIXUP_SECTOR_SIZE;

    if usa_offset + 2 > buf.len() || array_end > buf.len() || sectors_end > buf.len() {
        return None;
    }

    let usn = [buf[usa_offset], buf[usa_offset + 1]];

    for i in 0..sector_count {
        let array_pos = array_start + i * 2;
        let sector_pos = i * FIXUP_SECTOR_SIZE + (FIXUP_SECTOR_SIZE - 2);

        let new_bytes = [buf[array_pos], buf[array_pos + 1]];

        if buf[sector_pos] != usn[0] || buf[sector_pos + 1] != usn[1] {
            return None; // sector's USN doesn't match - corrupt/torn record
        }

        buf[sector_pos] = new_bytes[0];
        buf[sector_pos + 1] = new_bytes[1];
    }

    Some(())
}

/// Returns the absolute offset of a resident attribute's value, or `None`
/// if the attribute's declared bounds don't fit inside `buf`/its own
/// `attr_length`. `attr_offset` is the attribute's start within `buf`.
fn resident_value_offset(buf: &[u8], attr_offset: usize, attr_length: usize) -> Option<usize> {
    if attr_offset + 22 > buf.len() {
        return None;
    }

    let value_length = read_u32(buf, attr_offset + 16) as usize;
    let value_offset = read_u16(buf, attr_offset + 20) as usize;

    if value_offset.checked_add(value_length)? > attr_length {
        return None;
    }

    let abs = attr_offset + value_offset;
    if abs.checked_add(value_length)? > buf.len() {
        return None;
    }

    Some(abs)
}

/// Returns a resident attribute's `value_length` field directly (used for
/// the unnamed `$DATA` attribute, where the size is all that's needed).
fn resident_value_length(buf: &[u8], attr_offset: usize) -> Option<u64> {
    if attr_offset + 20 > buf.len() {
        return None;
    }
    Some(read_u32(buf, attr_offset + 16) as u64)
}

fn utf16le_to_string(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Parses one `$FILE_NAME` attribute's resident value (starting at
/// `value_off` within `buf`) into a [`NameAlias`], or `None` if it is a
/// DOS-only (short 8.3) name - see [`parse_record`]'s doc comment for why
/// those are skipped - or if the value doesn't fit in `buf`.
///
/// Assumes `$FILE_NAME` is resident, which it always is in practice (its
/// value is far below the resident/non-resident size threshold); a
/// theoretical non-resident `$FILE_NAME` is not handled, matching this
/// module's "read the $MFT once, sequentially, no extra disk I/O" design.
fn parse_file_name(buf: &[u8], value_off: usize) -> Option<NameAlias> {
    const HEADER_LEN: usize = 66; // see FileNameHeader layout below
    if value_off + HEADER_LEN > buf.len() {
        return None;
    }

    // FileNameHeader (matches ntfs::structured_values::file_name::FileNameHeader):
    //   parent_directory_reference: 8 bytes @0
    //   creation_time..access_time: 4x 8 bytes @8..40
    //   allocated_size, data_size:  2x 8 bytes @40..56
    //   file_attributes:            4 bytes  @56
    //   reparse_point_tag:          4 bytes  @60
    //   name_length:                1 byte   @64
    //   namespace:                  1 byte   @65
    let parent_frn = read_frn48(buf, value_off);
    let name_length_chars = buf[value_off + 64] as usize;
    let namespace = buf[value_off + 65];

    // Namespace 2 = Dos (short 8.3 name). Every file with a Dos-only alias
    // also has a separate Win32/Posix $FILE_NAME with the real name, so
    // skipping the Dos one loses nothing and avoids bogus 8.3-shaped
    // duplicate paths - mirrors the previous `ntfs`-crate-based parser.
    if namespace == 2 {
        return None;
    }

    let name_start = value_off + HEADER_LEN;
    let name_end = name_start + name_length_chars * 2;
    if name_end > buf.len() {
        return None;
    }

    Some(NameAlias {
        parent_frn,
        name: utf16le_to_string(&buf[name_start..name_end]),
    })
}

/// Parses one already-fixed-up-or-not FILE record's raw bytes (`record_size`
/// bytes, sliced from a streamed `$MFT` chunk) into a [`ParsedRecord`].
///
/// Returns `None` if `buf` is not a valid FILE record at all: wrong magic
/// (an unallocated slot, or corruption) or a fixup that fails validation
/// (see [`apply_fixups`]). Returns `Some` with `in_use == false` for a
/// well-formed but currently-deleted record - the caller / accumulator is
/// responsible for filtering those out.
///
/// `buf` is mutated in place: fixups must be applied before any attribute
/// bytes are read, since a fixup can affect bytes anywhere a 512-byte
/// sector boundary falls inside the record.
pub fn parse_record(buf: &mut [u8], frn: u64) -> Option<ParsedRecord> {
    if buf.len() < MIN_HEADER_SIZE {
        return None;
    }
    if buf[0..4] != FILE_SIGNATURE {
        return None; // unallocated slot or corrupt - not a FILE record
    }

    apply_fixups(buf)?;

    let flags = read_u16(buf, 22);
    let in_use = flags & 0x0001 != 0;
    let is_directory = flags & 0x0002 != 0;

    if !in_use {
        return Some(ParsedRecord {
            frn,
            base_frn: 0,
            in_use: false,
            is_directory: false,
            unnamed_data_size: None,
            mtime: None,
            aliases: Vec::new(),
        });
    }

    let first_attribute_offset = read_u16(buf, 20) as usize;
    let base_frn = read_frn48(buf, 32);

    let mut unnamed_data_size = None;
    let mut mtime = None;
    let mut aliases = Vec::new();

    let mut offset = first_attribute_offset;
    while offset + 8 <= buf.len() {
        let attr_type = read_u32(buf, offset);
        if attr_type == 0xFFFF_FFFF {
            break; // end-of-attributes marker
        }

        let attr_length = read_u32(buf, offset + 4) as usize;
        if attr_length < 16 || offset + attr_length > buf.len() {
            break; // corrupt tail - stop, keep whatever was parsed so far
        }

        let is_non_resident = buf[offset + 8] != 0;
        let name_length_chars = buf[offset + 9];

        match attr_type {
            0x10 if !is_non_resident => {
                // $STANDARD_INFORMATION: modification_time is the second
                // of four NtfsTime fields at the start of the value.
                if let Some(value_off) = resident_value_offset(buf, offset, attr_length) {
                    let mtime_pos = value_off + 8;
                    if mtime_pos + 8 <= buf.len() {
                        mtime = Some(nt_time_to_unix_secs(read_u64(buf, mtime_pos)));
                    }
                }
            }
            0x30 if !is_non_resident => {
                // $FILE_NAME
                if let Some(value_off) = resident_value_offset(buf, offset, attr_length) {
                    if let Some(alias) = parse_file_name(buf, value_off) {
                        aliases.push(alias);
                    }
                }
            }
            0x80 if name_length_chars == 0 => {
                // unnamed $DATA
                let size = if is_non_resident {
                    // Non-resident header: lowest_vcn @+16, data_size @+48.
                    // A $DATA attribute fragmented across several attribute
                    // records (large files with an $ATTRIBUTE_LIST) has
                    // valid size fields only in the fragment whose
                    // lowest_vcn == 0; the other fragments carry zeros
                    // there and must not overwrite the real size.
                    if attr_length >= 64 && read_u64(buf, offset + 16) == 0 {
                        Some(read_u64(buf, offset + 48))
                    } else {
                        None
                    }
                } else {
                    resident_value_length(buf, offset)
                };
                if let Some(size) = size {
                    unnamed_data_size = Some(size);
                }
            }
            _ => {}
        }

        offset += attr_length;
    }

    Some(ParsedRecord {
        frn,
        base_frn,
        in_use: true,
        is_directory,
        unnamed_data_size,
        mtime,
        aliases,
    })
}

/// Parses every `record_size`-sized record in `buf` (a streamed `$MFT`
/// chunk, or several concatenated records in a test), assigning FRNs
/// sequentially starting at `first_frn`. Records that fail to parse (see
/// [`parse_record`]) are silently skipped - one corrupt record must never
/// abort the whole chunk.
pub fn parse_chunk(buf: &mut [u8], record_size: u64, first_frn: u64) -> Vec<ParsedRecord> {
    let record_size = record_size as usize;
    if record_size == 0 {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(buf.len() / record_size);
    let mut offset = 0usize;
    let mut frn = first_frn;

    while offset + record_size <= buf.len() {
        if let Some(parsed) = parse_record(&mut buf[offset..offset + record_size], frn) {
            out.push(parsed);
        }
        offset += record_size;
        frn += 1;
    }

    out
}

/// One logical file's attributes as accumulated so far from its base
/// record and any extension records encountered - order-independent, since
/// records are visited in whatever order the linear `$MFT` scan finds them
/// (an extension record's target FRN may be processed before or after the
/// base record itself).
#[derive(Default)]
struct PartialRecord {
    is_directory: bool,
    size: Option<u64>,
    mtime: Option<i64>,
    aliases: Vec<NameAlias>,
}

/// Accumulates [`ParsedRecord`]s - base and extension alike - into a
/// [`FrnMap`], merging an extension record's contribution into its base
/// record's entry. Since the whole `$MFT` is scanned linearly in one pass,
/// no extra disk I/O is needed to resolve extension records: whichever
/// record (base or extension) carries a given attribute is simply visited
/// at some point during the scan, in no particular order relative to its
/// base.
pub struct FrnAccumulator {
    entries: HashMap<u64, PartialRecord>,
}

impl FrnAccumulator {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity),
        }
    }

    /// Folds one parsed record into the accumulator. Not-in-use records are
    /// ignored here (rather than requiring every caller to check first).
    pub fn add(&mut self, parsed: ParsedRecord) {
        if !parsed.in_use {
            return;
        }

        let target_frn = if parsed.base_frn == 0 {
            parsed.frn
        } else {
            parsed.base_frn
        };

        let entry = self.entries.entry(target_frn).or_default();

        // Only the true base record's flags are trusted for
        // `is_directory` - an extension record's own flags aren't
        // meaningful for the logical file as a whole.
        if parsed.base_frn == 0 {
            entry.is_directory = parsed.is_directory;
        }
        if let Some(size) = parsed.unnamed_data_size {
            entry.size = Some(size);
        }
        if let Some(mtime) = parsed.mtime {
            entry.mtime = Some(mtime);
        }
        entry.aliases.extend(parsed.aliases);
    }

    /// Finalizes accumulation into a [`FrnMap`], keeping only entries whose
    /// FRN is at least `first_user_frn` (skips reserved NTFS metadata
    /// records) and that have at least one usable `$FILE_NAME` alias -
    /// mirrors the previous `ntfs`-crate-based parser's exact semantics
    /// (see `collect_record` in the old `reader.rs`).
    pub fn finish(self, first_user_frn: u64) -> FrnMap {
        let mut map = FrnMap::with_capacity(self.entries.len());

        for (frn, partial) in self.entries {
            if frn < first_user_frn || partial.aliases.is_empty() {
                continue;
            }

            map.insert(
                frn,
                MftRecord {
                    is_directory: partial.is_directory,
                    size: if partial.is_directory {
                        0
                    } else {
                        partial.size.unwrap_or(0)
                    },
                    mtime: partial.mtime,
                    aliases: partial.aliases,
                },
            );
        }

        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORD_SIZE: usize = 1024;

    /// Minimal builder for synthetic FILE records: writes attributes
    /// sequentially from `first_attribute_offset`, then - on [`finish`] -
    /// simulates the on-disk Update Sequence Array encoding exactly as
    /// real NTFS does: captures the *true* bytes at each 512-byte sector
    /// boundary into the USA, then overwrites those boundary bytes with
    /// the USN marker. Every test built through this therefore exercises
    /// `apply_fixups`' restore path for free.
    struct RecordBuilder {
        buf: Vec<u8>,
        cursor: usize,
    }

    impl RecordBuilder {
        fn new(record_size: usize) -> Self {
            let mut buf = vec![0u8; record_size];
            buf[0..4].copy_from_slice(&FILE_SIGNATURE);

            let usa_offset = 42usize;
            let sector_count = record_size / FIXUP_SECTOR_SIZE;
            let usa_count = sector_count + 1;
            buf[4..6].copy_from_slice(&(usa_offset as u16).to_le_bytes());
            buf[6..8].copy_from_slice(&(usa_count as u16).to_le_bytes());

            let first_attribute_offset = usa_offset + usa_count * 2;
            buf[20..22].copy_from_slice(&(first_attribute_offset as u16).to_le_bytes());

            Self {
                buf,
                cursor: first_attribute_offset,
            }
        }

        fn set_flags(mut self, in_use: bool, is_directory: bool) -> Self {
            let mut flags = 0u16;
            if in_use {
                flags |= 0x0001;
            }
            if is_directory {
                flags |= 0x0002;
            }
            self.buf[22..24].copy_from_slice(&flags.to_le_bytes());
            self
        }

        fn set_base_record(mut self, base_frn: u64) -> Self {
            self.buf[32..40].copy_from_slice(&base_frn.to_le_bytes());
            self
        }

        /// Writes a generic attribute header at the cursor. Returns the
        /// absolute offset the resident value should be written at.
        fn write_attr_header(
            &mut self,
            attr_type: u32,
            attr_len: usize,
            is_non_resident: bool,
            header_len: u16,
        ) -> usize {
            let start = self.cursor;
            self.buf[start..start + 4].copy_from_slice(&attr_type.to_le_bytes());
            self.buf[start + 4..start + 8].copy_from_slice(&(attr_len as u32).to_le_bytes());
            self.buf[start + 8] = is_non_resident as u8;
            self.buf[start + 9] = 0; // name_length (unnamed)
            self.buf[start + 10..start + 12].copy_from_slice(&0u16.to_le_bytes()); // name_offset
            self.buf[start + 12..start + 14].copy_from_slice(&0u16.to_le_bytes()); // flags
            self.buf[start + 14..start + 16].copy_from_slice(&0u16.to_le_bytes()); // instance
            if !is_non_resident {
                let value_len = attr_len - header_len as usize;
                self.buf[start + 16..start + 20].copy_from_slice(&(value_len as u32).to_le_bytes());
                self.buf[start + 20..start + 22].copy_from_slice(&header_len.to_le_bytes());
                self.buf[start + 22] = 0; // indexed_flag
            }
            self.cursor = start + attr_len;
            start
        }

        fn add_standard_information(mut self, mtime_nt: u64) -> Self {
            const HEADER_LEN: u16 = 24;
            const VALUE_LEN: usize = 48;
            let attr_len = HEADER_LEN as usize + VALUE_LEN;
            let start = self.write_attr_header(0x10, attr_len, false, HEADER_LEN);
            let v = start + HEADER_LEN as usize;
            // creation_time @0, modification_time @8, mft_record_mod @16, access @24
            self.buf[v + 8..v + 16].copy_from_slice(&mtime_nt.to_le_bytes());
            self
        }

        fn add_file_name(mut self, parent_frn: u64, name: &str, namespace: u8) -> Self {
            let name_units: Vec<u16> = name.encode_utf16().collect();
            const HEADER_LEN: u16 = 24;
            const VALUE_HEADER_LEN: usize = 66;
            let value_len = VALUE_HEADER_LEN + name_units.len() * 2;
            let attr_len = HEADER_LEN as usize + value_len;
            let start = self.write_attr_header(0x30, attr_len, false, HEADER_LEN);
            let v = start + HEADER_LEN as usize;
            self.buf[v..v + 8].copy_from_slice(&parent_frn.to_le_bytes());
            self.buf[v + 64] = name_units.len() as u8;
            self.buf[v + 65] = namespace;
            let name_start = v + 66;
            for (i, unit) in name_units.iter().enumerate() {
                self.buf[name_start + i * 2..name_start + i * 2 + 2]
                    .copy_from_slice(&unit.to_le_bytes());
            }
            self
        }

        /// Adds an attribute type this parser ignores, purely to nudge
        /// `cursor` forward by a controlled amount for boundary tests.
        fn add_padding(mut self, value_len: usize) -> Self {
            const HEADER_LEN: u16 = 24;
            let attr_len = HEADER_LEN as usize + value_len;
            self.write_attr_header(0x40, attr_len, false, HEADER_LEN);
            self
        }

        fn add_resident_data(mut self, data: &[u8]) -> Self {
            const HEADER_LEN: u16 = 24;
            let attr_len = HEADER_LEN as usize + data.len();
            let start = self.write_attr_header(0x80, attr_len, false, HEADER_LEN);
            let v = start + HEADER_LEN as usize;
            self.buf[v..v + data.len()].copy_from_slice(data);
            self
        }

        fn add_nonresident_data(self, data_size: u64) -> Self {
            self.add_nonresident_data_fragment(0, data_size)
        }

        /// A non-resident `$DATA` fragment with an explicit `lowest_vcn`.
        /// Fragments with `lowest_vcn > 0` carry zeros (or garbage) in
        /// their size fields on a real volume, which is what `data_size`
        /// simulates here.
        fn add_nonresident_data_fragment(mut self, lowest_vcn: u64, data_size: u64) -> Self {
            const ATTR_LEN: usize = 64;
            let start = self.cursor;
            self.buf[start..start + 4].copy_from_slice(&0x80u32.to_le_bytes());
            self.buf[start + 4..start + 8].copy_from_slice(&(ATTR_LEN as u32).to_le_bytes());
            self.buf[start + 8] = 1; // non-resident
            self.buf[start + 9] = 0; // unnamed
            self.buf[start + 16..start + 24].copy_from_slice(&lowest_vcn.to_le_bytes());
            self.buf[start + 48..start + 56].copy_from_slice(&data_size.to_le_bytes());
            self.cursor = start + ATTR_LEN;
            self
        }

        fn finish(mut self) -> Vec<u8> {
            // end-of-attributes marker
            self.buf[self.cursor..self.cursor + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());

            let usa_offset = read_u16(&self.buf, 4) as usize;
            let usa_count = read_u16(&self.buf, 6) as usize;
            let sector_count = usa_count - 1;
            let usn: u16 = 0xABCD;
            self.buf[usa_offset..usa_offset + 2].copy_from_slice(&usn.to_le_bytes());

            for i in 0..sector_count {
                let sector_last2 = i * FIXUP_SECTOR_SIZE + (FIXUP_SECTOR_SIZE - 2);
                let true_bytes = [self.buf[sector_last2], self.buf[sector_last2 + 1]];
                let array_pos = usa_offset + 2 + i * 2;
                self.buf[array_pos] = true_bytes[0];
                self.buf[array_pos + 1] = true_bytes[1];
                self.buf[sector_last2..sector_last2 + 2].copy_from_slice(&usn.to_le_bytes());
            }

            self.buf
        }
    }

    /// Regression test for a real-volume bug: a large fragmented file
    /// ($ATTRIBUTE_LIST) has several non-resident `$DATA` fragments, and
    /// only the `lowest_vcn == 0` one carries the real size - a later
    /// fragment's zero `data_size` used to overwrite it, shrinking
    /// multi-GB files to 0 bytes in scan results.
    #[test]
    fn later_data_fragment_does_not_overwrite_real_size() {
        let mut buf = RecordBuilder::new(RECORD_SIZE)
            .set_flags(true, false)
            .add_file_name(5, "huge.pak", 1)
            .add_nonresident_data(68_464_150_752)
            .add_nonresident_data_fragment(1000, 0)
            .finish();

        let parsed = parse_record(&mut buf, 30).expect("record parses");
        assert_eq!(parsed.unnamed_data_size, Some(68_464_150_752));
    }

    /// A record carrying only a `lowest_vcn > 0` fragment (the real size
    /// lives in another record of the same file) must report no size at
    /// all rather than a bogus zero.
    #[test]
    fn size_less_data_fragment_alone_yields_no_size() {
        let mut buf = RecordBuilder::new(RECORD_SIZE)
            .set_flags(true, false)
            .add_file_name(5, "huge.pak", 1)
            .add_nonresident_data_fragment(2000, 0)
            .finish();

        let parsed = parse_record(&mut buf, 31).expect("record parses");
        assert_eq!(parsed.unnamed_data_size, None);
    }

    #[test]
    fn bad_magic_is_skipped() {
        let buf = vec![0u8; RECORD_SIZE]; // all zeros - never allocated slot
        let mut buf = buf;
        assert!(parse_record(&mut buf, 42).is_none());
    }

    #[test]
    fn not_in_use_record_is_reported_but_carries_no_data() {
        let mut buf = RecordBuilder::new(RECORD_SIZE)
            .set_flags(false, false)
            .finish();

        let parsed = parse_record(&mut buf, 20).expect("well-formed record parses");
        assert!(!parsed.in_use);
        assert!(parsed.aliases.is_empty());

        // The accumulator must ignore it entirely.
        let mut acc = FrnAccumulator::with_capacity(4);
        acc.add(parsed);
        assert!(acc.finish(16).is_empty());
    }

    #[test]
    fn fixups_restore_data_straddling_a_sector_boundary() {
        // Lay the $FILE_NAME's name bytes out so 2 of its bytes land
        // exactly on the sector-0/sector-1 fixup boundary (offsets
        // 510..512): padding is sized so the name starts at byte 508, and
        // "AB" (4 bytes UTF-16LE) spans 508..512 - bytes 510/511 hold the
        // second character, 'B'. If `apply_fixups` did not restore them,
        // they would still hold the USN marker instead.
        // first_attribute_offset is 48 for a 1024-byte record (usa_offset
        // 42 + usa_count 3 * 2 = 48). Want $FILE_NAME's header to start at
        // 418, so its value (header + 66) starts at 442, and its name
        // (value + 66) starts at 508.
        let mut buf = RecordBuilder::new(RECORD_SIZE)
            .set_flags(true, false)
            .add_padding(418 - 48 - 24)
            .add_file_name(5, "AB", 1) // namespace 1 = Win32
            .finish();
        let parsed = parse_record(&mut buf, 100).expect("well-formed record parses");

        assert!(parsed.in_use);
        assert_eq!(parsed.aliases.len(), 1);
        assert_eq!(parsed.aliases[0].parent_frn, 5);
        assert_eq!(parsed.aliases[0].name, "AB");
    }

    #[test]
    fn fixup_mismatch_is_treated_as_corrupt_and_skipped() {
        let mut buf = RecordBuilder::new(RECORD_SIZE)
            .set_flags(true, false)
            .finish();

        // Corrupt the encoded sector-boundary bytes so they no longer
        // match the USN that `apply_fixups` expects to find there.
        buf[510] = 0x00;
        buf[511] = 0x00;

        assert!(parse_record(&mut buf, 7).is_none());
    }

    #[test]
    fn directory_record_has_zero_size() {
        let mut buf = RecordBuilder::new(RECORD_SIZE)
            .set_flags(true, true)
            .add_file_name(5, "SteamLibrary", 1)
            .finish();

        let parsed = parse_record(&mut buf, 30).expect("well-formed record parses");
        assert!(parsed.is_directory);

        let mut acc = FrnAccumulator::with_capacity(4);
        acc.add(parsed);
        let map = acc.finish(16);
        let record = &map[&30];
        assert!(record.is_directory);
        assert_eq!(record.size, 0);
    }

    #[test]
    fn resident_data_size_is_read_from_the_value_length_field() {
        let data = vec![7u8; 100];
        let mut buf = RecordBuilder::new(RECORD_SIZE)
            .set_flags(true, false)
            .add_file_name(5, "readme.txt", 1)
            .add_resident_data(&data)
            .finish();

        let parsed = parse_record(&mut buf, 31).expect("well-formed record parses");
        assert_eq!(parsed.unnamed_data_size, Some(100));
    }

    #[test]
    fn non_resident_data_size_is_read_from_the_data_size_field() {
        let mut buf = RecordBuilder::new(RECORD_SIZE)
            .set_flags(true, false)
            .add_file_name(5, "big_file.bin", 1)
            .add_nonresident_data(123_456_789)
            .finish();

        let parsed = parse_record(&mut buf, 32).expect("well-formed record parses");
        assert_eq!(parsed.unnamed_data_size, Some(123_456_789));
    }

    #[test]
    fn mtime_is_converted_from_standard_information() {
        // NT timestamp of the Unix epoch itself, for an easy round trip.
        let nt_epoch = NT_TO_UNIX_INTERVALS as u64;
        let mut buf = RecordBuilder::new(RECORD_SIZE)
            .set_flags(true, false)
            .add_standard_information(nt_epoch)
            .add_file_name(5, "save.dat", 1)
            .finish();

        let parsed = parse_record(&mut buf, 33).expect("well-formed record parses");
        assert_eq!(parsed.mtime, Some(0));
    }

    #[test]
    fn dos_only_alias_is_skipped_but_win32_alias_is_kept() {
        let mut buf = RecordBuilder::new(RECORD_SIZE)
            .set_flags(true, false)
            .add_file_name(5, "LONGFILENAME.TXT", 1) // Win32
            .add_file_name(5, "LONGFI~1.TXT", 2) // Dos - must be skipped
            .finish();

        let parsed = parse_record(&mut buf, 34).expect("well-formed record parses");
        assert_eq!(parsed.aliases.len(), 1);
        assert_eq!(parsed.aliases[0].name, "LONGFILENAME.TXT");
    }

    #[test]
    fn extension_record_merges_into_its_base_record() {
        let mut base_buf = RecordBuilder::new(RECORD_SIZE)
            .set_flags(true, false)
            .add_file_name(5, "game.exe", 1)
            .finish();
        let base = parse_record(&mut base_buf, 200).expect("base record parses");
        assert_eq!(base.base_frn, 0);
        assert!(base.unnamed_data_size.is_none());

        let mut ext_buf = RecordBuilder::new(RECORD_SIZE)
            .set_flags(true, false)
            .set_base_record(200)
            .add_resident_data(&[1u8; 42])
            .finish();
        let ext = parse_record(&mut ext_buf, 500).expect("extension record parses");
        assert_eq!(ext.base_frn, 200);
        assert!(ext.aliases.is_empty());

        let mut acc = FrnAccumulator::with_capacity(4);
        acc.add(base);
        acc.add(ext);
        let map = acc.finish(16);

        let record = map.get(&200).expect("base FRN present in the final map");
        assert_eq!(record.aliases.len(), 1);
        assert_eq!(record.aliases[0].name, "game.exe");
        assert_eq!(record.size, 42);
        // The extension record's own FRN must never appear as a key.
        assert!(!map.contains_key(&500));
    }

    #[test]
    fn extension_record_merges_regardless_of_visit_order() {
        let mut base_buf = RecordBuilder::new(RECORD_SIZE)
            .set_flags(true, false)
            .add_file_name(5, "game.exe", 1)
            .finish();
        let base = parse_record(&mut base_buf, 201).unwrap();

        let mut ext_buf = RecordBuilder::new(RECORD_SIZE)
            .set_flags(true, false)
            .set_base_record(201)
            .add_resident_data(&[9u8; 7])
            .finish();
        let ext = parse_record(&mut ext_buf, 501).unwrap();

        // Extension visited *before* its base this time.
        let mut acc = FrnAccumulator::with_capacity(4);
        acc.add(ext);
        acc.add(base);
        let map = acc.finish(16);

        let record = &map[&201];
        assert_eq!(record.aliases[0].name, "game.exe");
        assert_eq!(record.size, 7);
    }

    #[test]
    fn record_below_first_user_frn_is_excluded_from_the_final_map() {
        let mut buf = RecordBuilder::new(RECORD_SIZE)
            .set_flags(true, false)
            .add_file_name(5, "$LogFile", 1)
            .finish();
        let parsed = parse_record(&mut buf, 2).unwrap(); // reserved system FRN

        let mut acc = FrnAccumulator::with_capacity(4);
        acc.add(parsed);
        assert!(acc.finish(16).is_empty());
    }

    #[test]
    fn record_with_no_aliases_is_excluded_from_the_final_map() {
        let mut buf = RecordBuilder::new(RECORD_SIZE)
            .set_flags(true, false)
            .finish(); // no $FILE_NAME at all
        let parsed = parse_record(&mut buf, 40).unwrap();
        assert!(parsed.aliases.is_empty());

        let mut acc = FrnAccumulator::with_capacity(4);
        acc.add(parsed);
        assert!(acc.finish(16).is_empty());
    }

    #[test]
    fn parse_chunk_parses_several_concatenated_records() {
        let mut chunk = Vec::new();
        chunk.extend(
            RecordBuilder::new(RECORD_SIZE)
                .set_flags(true, false)
                .add_file_name(5, "a.txt", 1)
                .finish(),
        );
        chunk.extend(
            RecordBuilder::new(RECORD_SIZE)
                .set_flags(false, false)
                .finish(),
        );
        chunk.extend(
            RecordBuilder::new(RECORD_SIZE)
                .set_flags(true, true)
                .add_file_name(5, "subdir", 1)
                .finish(),
        );

        let parsed = parse_chunk(&mut chunk, RECORD_SIZE as u64, 100);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].frn, 100);
        assert!(parsed[0].in_use);
        assert_eq!(parsed[1].frn, 101);
        assert!(!parsed[1].in_use);
        assert_eq!(parsed[2].frn, 102);
        assert!(parsed[2].in_use);
        assert!(parsed[2].is_directory);
    }

    #[test]
    fn parse_chunk_ignores_a_trailing_partial_record() {
        let mut chunk = RecordBuilder::new(RECORD_SIZE)
            .set_flags(true, false)
            .add_file_name(5, "a.txt", 1)
            .finish();
        chunk.extend(vec![0u8; 100]); // shorter than one full record

        let parsed = parse_chunk(&mut chunk, RECORD_SIZE as u64, 0);
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn nt_epoch_converts_to_unix_epoch() {
        assert_eq!(nt_time_to_unix_secs(NT_TO_UNIX_INTERVALS as u64), 0);
    }

    #[test]
    fn nt_time_one_second_after_unix_epoch() {
        let one_second_later = NT_TO_UNIX_INTERVALS as u64 + INTERVALS_PER_SECOND as u64;
        assert_eq!(nt_time_to_unix_secs(one_second_later), 1);
    }
}
