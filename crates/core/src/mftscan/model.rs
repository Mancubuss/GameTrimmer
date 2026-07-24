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
    /// All `$FILE_NAME` aliases for this record (usually one; more than one
    /// only for hard-linked files). The first alias is treated as the
    /// "primary" one when this record is used as an ancestor of another.
    pub aliases: Vec<NameAlias>,
}

/// FRN -> reconstructed record, covering an entire NTFS volume.
pub type FrnMap = HashMap<u64, MftRecord>;
