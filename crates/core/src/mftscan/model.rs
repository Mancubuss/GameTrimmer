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
