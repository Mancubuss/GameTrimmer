//! Pure data types describing MFT-derived file/directory records, kept free
//! of the `ntfs` crate and Windows APIs so path-reconstruction logic can be
//! unit tested with synthetic data (no real NTFS volume required).

use std::collections::HashMap;

/// File-Record-Number of the NTFS volume root directory. Every record's
/// ancestor chain terminates here.
pub const ROOT_FRN: u64 = 5;

/// One name a file/directory is known by inside a parent directory (one
/// NTFS `$FILE_NAME` attribute). A record has more than one alias when it
/// has hard links into different directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameAlias {
    pub parent_frn: u64,
    pub name: String,
}

/// One MFT record's worth of information needed for path reconstruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MftRecord {
    pub is_directory: bool,
    /// Current size in bytes of the unnamed `$DATA` stream (the *logical*
    /// file size). Always `0` for directories.
    pub size: u64,
    /// Size in bytes actually allocated on disk for the unnamed `$DATA`
    /// stream - the non-resident attribute's `allocated_size` field, which
    /// NTFS keeps cluster-aligned and compression-aware, so it equals what
    /// Explorer reports as "Size on disk". `0` for directories and for
    /// resident (tiny, MFT-embedded) files, which occupy no data clusters -
    /// matching Explorer, which shows `0 bytes` on disk for them.
    pub alloc_size: u64,
    /// Unix seconds of `$STANDARD_INFORMATION` modification time.
    pub mtime: Option<i64>,
    /// The same modification time, unconverted: the raw 100-nanosecond NT
    /// timestamp. `mtime` above is derived from it and rounds to whole
    /// seconds, which is fine for display and useless for proving a file has
    /// not changed - `safety::FileIdentity` compares `ftLastWriteTime`
    /// exactly, and a second of slack there is a second in which a file may
    /// be rewritten unnoticed.
    pub mtime_nt: Option<u64>,
    /// This record's own sequence number - the 16 bits NTFS increments every
    /// time the record is reused for a different file.
    ///
    /// Kept because the File Record Number alone does **not** identify a
    /// file: NTFS reuses record numbers, and it is the sequence number that
    /// tells a live file from a deleted one whose slot was taken. Win32
    /// reports the two together as one 64-bit index
    /// (`nFileIndexHigh`/`nFileIndexLow`), and anything comparing identities
    /// must do the same.
    pub sequence: u16,
    /// `$STANDARD_INFORMATION`'s file attribute bits, as NTFS stores them.
    ///
    /// **Not** interchangeable with Win32's `dwFileAttributes` without
    /// checking: NTFS does not keep the directory bit here, and the two sets
    /// have diverged before. The one bit worth having is
    /// `FILE_ATTRIBUTE_REPARSE_POINT` - a junction or symlink must never be
    /// treated as an ordinary deletable file.
    pub nt_attributes: Option<u32>,
    /// All `$FILE_NAME` aliases for this record (usually one; more than one
    /// only for hard-linked files). The first alias is treated as the
    /// "primary" one when this record is used as an ancestor of another.
    pub aliases: Vec<NameAlias>,
}

/// FRN -> reconstructed record, covering an entire NTFS volume.
pub type FrnMap = HashMap<u64, MftRecord>;

/// What an `$MFT` record says about a file's identity, in the shape
/// `safety::FileIdentity` uses.
///
/// Deliberately a **separate type**, not a `FileIdentity`. Every field here
/// is read from a table that was streamed off the volume some seconds ago,
/// whereas `FileIdentity` means "what the filesystem said when this handle
/// was opened", and the deletion contract is built on the latter. Until the
/// two are proven to agree field for field on real data (see
/// `examples/mft_identity_check.rs`), giving this the safety type's name
/// would be claiming a guarantee nobody has checked.
///
/// The one field an `$MFT` record cannot supply is the volume serial, which
/// describes the volume rather than the file - see
/// [`super::volume::serial_number`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MftIdentity {
    pub volume_serial: u32,
    /// `sequence << 48 | frn`, the composition Win32 reports as
    /// `nFileIndexHigh`/`nFileIndexLow`.
    pub file_index: u64,
    pub is_directory: bool,
    pub size: u64,
    /// The raw 100-nanosecond NT timestamp, matching `ftLastWriteTime`'s
    /// resolution rather than the rounded seconds in [`MftRecord::mtime`].
    pub last_write_time: u64,
    /// NTFS's own attribute bits - see [`MftRecord::nt_attributes`]. Not yet
    /// known to equal Win32's `dwFileAttributes`.
    pub nt_attributes: u32,
}

impl MftRecord {
    /// Builds the identity this record states, or `None` when the record is
    /// missing a field identity needs. A partial identity is never returned:
    /// the caller must be able to treat `Some` as "everything is known" and
    /// `None` as "open the file instead".
    pub fn identity(&self, frn: u64, volume_serial: u32) -> Option<MftIdentity> {
        Some(MftIdentity {
            volume_serial,
            file_index: (self.sequence as u64) << 48 | frn,
            is_directory: self.is_directory,
            size: self.size,
            last_write_time: self.mtime_nt?,
            nt_attributes: self.nt_attributes?,
        })
    }
}
