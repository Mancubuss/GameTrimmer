//! Test-only header snapshot and repair semantics.
//!
//! This module is not compiled into the production crate API. It retains tests
//! for lightweight header and table-of-contents snapshot semantics.
//! These snapshots can repair only the captured header bytes. They are not a
//! rollback mechanism for payload ranges removed or zeroed elsewhere in the
//! archive.

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

const SNAPSHOT_HEADER_BYTES: usize = 65536; // 64 KiB header snapshot

#[derive(Error, Debug)]
enum SnapshotError {
    #[error("I/O error during snapshot operation: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Snapshot integrity mismatch: expected checksum {expected:#X}, found {actual:#X}")]
    ChecksumMismatch { expected: u64, actual: u64 },
    #[error("Snapshot target path mismatch: snapshot is for {expected}, restoring to {actual}")]
    PathMismatch { expected: String, actual: String },
    #[error("Snapshot target size mismatch: expected {expected} bytes, found {actual} bytes")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("Snapshot header length mismatch: metadata says {expected} bytes, payload has {actual} bytes")]
    HeaderLengthMismatch { expected: usize, actual: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HeaderSnapshot {
    archive_name: String,
    original_path: String,
    original_size: u64,
    header_size: usize,
    checksum: u64,
    header_bytes_base64: String,
    timestamp_epoch_secs: u64,
}

/// Simple fast 64-bit FNV-1a checksum for header integrity validation.
fn calculate_checksum(data: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Helper to encode bytes as basic base64 string.
fn base64_encode(data: &[u8]) -> String {
    const CHARSET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = if chunk.len() > 1 { chunk[1] } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] } else { 0 };

        out.push(CHARSET[(b0 >> 2) as usize] as char);
        out.push(CHARSET[(((b0 & 3) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARSET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARSET[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Helper to decode base64 string back to bytes.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u32;

    for &b in input.as_bytes() {
        if b == b'=' || b.is_ascii_whitespace() {
            continue;
        }
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32;

        buffer = (buffer << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

/// Captures a lightweight `.gt_snap.json` header repair record for
/// `archive_path`.
///
/// If `snapshot_dir` is specified, the snapshot is saved into that directory;
/// otherwise, it is saved in a `.gametrimmer_backups` folder next to the archive.
fn create_header_snapshot(
    archive_path: &Path,
    custom_dir: Option<&Path>,
) -> Result<PathBuf, SnapshotError> {
    let canonical_archive_path = fs::canonicalize(archive_path)?;
    let mut file = File::open(&canonical_archive_path)?;
    let metadata = file.metadata()?;
    let original_size = metadata.len();

    let read_size = (SNAPSHOT_HEADER_BYTES as u64).min(original_size) as usize;
    let mut header_bytes = vec![0u8; read_size];
    file.read_exact(&mut header_bytes)?;

    let checksum = calculate_checksum(&header_bytes);
    let encoded = base64_encode(&header_bytes);

    let snapshot_dir = if let Some(dir) = custom_dir {
        dir.to_path_buf()
    } else {
        canonical_archive_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(".gametrimmer_backups")
    };

    fs::create_dir_all(&snapshot_dir)?;

    let file_stem = canonical_archive_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("archive");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let now_epoch = now.as_secs();
    let snapshot_file_name = format!("{file_stem}.{}.{}.gt_snap.json", now.as_nanos(), checksum);
    let snapshot_path = snapshot_dir.join(snapshot_file_name);

    let snapshot = HeaderSnapshot {
        archive_name: file_stem.to_string(),
        original_path: canonical_archive_path.to_string_lossy().to_string(),
        original_size,
        header_size: read_size,
        checksum,
        header_bytes_base64: encoded,
        timestamp_epoch_secs: now_epoch,
    };

    let json_content = serde_json::to_string_pretty(&snapshot)?;
    let mut snapshot_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&snapshot_path)?;
    snapshot_file.write_all(json_content.as_bytes())?;
    snapshot_file.sync_all()?;

    Ok(snapshot_path)
}

/// Repairs an archive's captured header bytes from a `.gt_snap.json` record.
///
/// This deliberately refuses a different path or file size. It does not and
/// cannot restore payload ranges outside the captured header.
fn restore_header_snapshot(archive_path: &Path, snapshot_path: &Path) -> Result<(), SnapshotError> {
    let json_content = fs::read_to_string(snapshot_path)?;
    let snapshot: HeaderSnapshot = serde_json::from_str(&json_content)?;

    let actual_path = fs::canonicalize(archive_path)?;
    let expected_path = fs::canonicalize(Path::new(&snapshot.original_path))?;
    if actual_path != expected_path {
        return Err(SnapshotError::PathMismatch {
            expected: expected_path.to_string_lossy().into_owned(),
            actual: actual_path.to_string_lossy().into_owned(),
        });
    }

    let actual_size = fs::metadata(&actual_path)?.len();
    if actual_size != snapshot.original_size {
        return Err(SnapshotError::SizeMismatch {
            expected: snapshot.original_size,
            actual: actual_size,
        });
    }

    let header_bytes = base64_decode(&snapshot.header_bytes_base64).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid base64 payload")
    })?;
    if header_bytes.len() != snapshot.header_size {
        return Err(SnapshotError::HeaderLengthMismatch {
            expected: snapshot.header_size,
            actual: header_bytes.len(),
        });
    }

    let actual_checksum = calculate_checksum(&header_bytes);
    if actual_checksum != snapshot.checksum {
        return Err(SnapshotError::ChecksumMismatch {
            expected: snapshot.checksum,
            actual: actual_checksum,
        });
    }

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&actual_path)?;

    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header_bytes)?;
    file.sync_all()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_create_and_restore_snapshot() {
        let dir = tempdir().expect("tempdir");
        let archive_path = dir.path().join("game_audio.pck");

        // Write initial data
        let mut original_data = vec![0u8; 128 * 1024];
        for (i, byte) in original_data.iter_mut().enumerate() {
            *byte = (i % 256) as u8;
        }
        fs::write(&archive_path, &original_data).expect("write archive");

        // Create snapshot
        let snap_path = create_header_snapshot(&archive_path, None).expect("create snapshot");
        assert!(snap_path.exists());

        // Mutate the header of the archive
        let mut file = OpenOptions::new()
            .write(true)
            .open(&archive_path)
            .expect("open for mutate");
        file.seek(SeekFrom::Start(0)).expect("seek");
        file.write_all(&[0xFF; 1024]).expect("corrupt header");
        drop(file);

        // Verify it was mutated
        let mutated = fs::read(&archive_path).expect("read mutated");
        assert_eq!(&mutated[0..4], &[0xFF, 0xFF, 0xFF, 0xFF]);

        // Restore from snapshot
        restore_header_snapshot(&archive_path, &snap_path).expect("repair header");

        // Verify restoration
        let restored = fs::read(&archive_path).expect("read restored");
        assert_eq!(
            &restored[0..65536],
            &original_data[0..65536],
            "Restored header matches original"
        );
    }

    #[test]
    fn snapshot_cannot_be_restored_to_a_different_same_size_file() {
        let dir = tempdir().expect("tempdir");
        let original = dir.path().join("original.pck");
        let other = dir.path().join("other.pck");
        fs::write(&original, vec![0x11; 4096]).expect("write original");
        fs::write(&other, vec![0x22; 4096]).expect("write other");

        let snapshot = create_header_snapshot(&original, None).expect("create snapshot");
        let before = fs::read(&other).expect("read before");
        assert!(matches!(
            restore_header_snapshot(&other, &snapshot),
            Err(SnapshotError::PathMismatch { .. })
        ));
        assert_eq!(fs::read(&other).expect("read after"), before);
    }

    #[test]
    fn snapshot_refuses_a_changed_target_size() {
        let dir = tempdir().expect("tempdir");
        let archive = dir.path().join("changed.pck");
        fs::write(&archive, vec![0x33; 4096]).expect("write archive");
        let snapshot = create_header_snapshot(&archive, None).expect("create snapshot");
        fs::write(&archive, vec![0x44; 2048]).expect("resize archive");

        assert!(matches!(
            restore_header_snapshot(&archive, &snapshot),
            Err(SnapshotError::SizeMismatch {
                expected: 4096,
                actual: 2048
            })
        ));
    }

    #[test]
    fn repeated_snapshots_do_not_overwrite_each_other() {
        let dir = tempdir().expect("tempdir");
        let archive = dir.path().join("repeat.pck");
        fs::write(&archive, vec![0x55; 4096]).expect("write archive");

        let first = create_header_snapshot(&archive, None).expect("first snapshot");
        let second = create_header_snapshot(&archive, None).expect("second snapshot");
        assert_ne!(first, second);
        assert!(first.exists());
        assert!(second.exists());
    }
}
