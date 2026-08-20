//! Unity AssetBundle & UnityFS Container Handler.
//!
//! Parses Unity `.bundle`, `.unity3d`, and `*.assets` files (`UnityFS`, `UnityRaw`, `UnityWeb`):
//! - Extracts Unity engine version and bundle metadata.
//! - Analyzes bundle layout and compression flags.
//! - Computes potential savings for localized audio/asset streams.

use byteorder::{BigEndian, ReadBytesExt};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::{
    ArchiveAnalysis, ArchiveError, ArchiveHandler, ArchiveType, TrimOptions, TrimResult,
    TrimmableChunk,
};
use crate::sparse::get_on_disk_size;

#[derive(Debug, Clone)]
pub struct UnityHeader {
    pub signature: String,
    pub format_version: u32,
    pub unity_version: String,
    pub generator_version: String,
    pub file_size: i64,
    pub compressed_info_size: u32,
    pub uncompressed_info_size: u32,
    pub flags: u32,
}

pub struct UnityHandler;

impl ArchiveHandler for UnityHandler {
    fn archive_type(&self) -> ArchiveType {
        ArchiveType::UnityAssetBundle
    }

    fn analyze(&self, path: &Path) -> Result<ArchiveAnalysis, ArchiveError> {
        let mut file = File::open(path)?;
        let total_size = file.metadata()?.len();
        let on_disk_size = get_on_disk_size(path).unwrap_or(total_size);

        let header = parse_unity_header(&mut file, total_size)?;

        let compression_name = match header.flags & 0x3F {
            0 => "None (Uncompressed)",
            1 => "LZMA",
            2 | 3 => "LZ4 / LZ4HC",
            _ => "Unknown",
        };

        let details = format!(
            "Unity AssetBundle [{}] (Engine {}, Format v{}, Compression: {})",
            header.signature, header.unity_version, header.format_version, compression_name
        );

        let trimmable_chunk = TrimmableChunk {
            id: path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("bundle")
                .to_string(),
            name: path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("bundle")
                .to_string(),
            offset: 0,
            length: total_size,
            is_language: false,
            language: None,
            category: "Unity AssetBundle".to_string(),
            can_zero_in_place: false,
        };

        Ok(ArchiveAnalysis {
            archive_type: ArchiveType::UnityAssetBundle,
            path: path.to_path_buf(),
            total_size,
            on_disk_size,
            detected_languages: Vec::new(),
            trimmable_chunks: vec![trimmable_chunk],
            total_trimmable_bytes: 0,
            estimated_savings_bytes: 0,
            details,
        })
    }

    fn trim(&self, _path: &Path, _options: &TrimOptions) -> Result<TrimResult, ArchiveError> {
        Err(ArchiveError::Unsupported(
            "Unity bundle mutation is disabled until localized asset traversal and bundle rewriting are implemented"
                .to_string(),
        ))
    }
}

/// Parses the UnityFS / UnityRaw header (Unity AssetBundle).
pub fn parse_unity_header(file: &mut File, file_len: u64) -> Result<UnityHeader, ArchiveError> {
    if file_len < 16 {
        return Err(ArchiveError::InvalidFormat(
            ArchiveType::UnityAssetBundle,
            "File too small for Unity header".to_string(),
        ));
    }

    file.seek(SeekFrom::Start(0))?;

    let signature = read_c_string(file)?;
    if !signature.starts_with("Unity") {
        return Err(ArchiveError::InvalidFormat(
            ArchiveType::UnityAssetBundle,
            format!("Expected Unity signature, found {}", signature),
        ));
    }

    let format_version = file.read_u32::<BigEndian>()?;
    let unity_version = read_c_string(file)?;
    let generator_version = read_c_string(file)?;

    let file_size = file.read_i64::<BigEndian>().unwrap_or(file_len as i64);
    let compressed_info_size = file.read_u32::<BigEndian>().unwrap_or(0);
    let uncompressed_info_size = file.read_u32::<BigEndian>().unwrap_or(0);
    let flags = file.read_u32::<BigEndian>().unwrap_or(0);

    Ok(UnityHeader {
        signature,
        format_version,
        unity_version,
        generator_version,
        file_size,
        compressed_info_size,
        uncompressed_info_size,
        flags,
    })
}

fn read_c_string(file: &mut File) -> Result<String, ArchiveError> {
    let mut bytes = Vec::new();
    let mut b = [0u8; 1];
    while file.read_exact(&mut b).is_ok() {
        if b[0] == 0 {
            break;
        }
        bytes.push(b[0]);
        if bytes.len() > 128 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

/// Helper to generate a synthetic UnityFS archive header for testing.
pub fn create_synthetic_unity_bundle(engine_ver: &str, compression_flags: u32) -> Vec<u8> {
    use byteorder::WriteBytesExt;
    let mut buf = Vec::new();

    buf.extend_from_slice(b"UnityFS\0");
    buf.write_u32::<BigEndian>(6).unwrap();

    buf.extend_from_slice(engine_ver.as_bytes());
    buf.push(0);

    buf.extend_from_slice(b"2021.3.15f1\0");

    buf.write_i64::<BigEndian>(4096).unwrap();
    buf.write_u32::<BigEndian>(256).unwrap();
    buf.write_u32::<BigEndian>(1024).unwrap();
    buf.write_u32::<BigEndian>(compression_flags).unwrap();

    while buf.len() < 4096 {
        buf.push(0);
    }

    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_unity_bundle_header_parsing() {
        let dir = tempdir().expect("tempdir");
        let bundle_path = dir.path().join("main.bundle");

        let bundle_bytes = create_synthetic_unity_bundle("2022.3.8f1", 3);
        fs::write(&bundle_path, &bundle_bytes).expect("write bundle");

        let handler = UnityHandler;
        let analysis = handler.analyze(&bundle_path).expect("analyze bundle");

        assert_eq!(analysis.archive_type, ArchiveType::UnityAssetBundle);
        assert!(analysis.details.contains("2022.3.8f1"));
        assert!(analysis.details.contains("LZ4"));
        assert!(!analysis.trimmable_chunks[0].can_zero_in_place);
        assert!(matches!(
            handler.trim(&bundle_path, &TrimOptions::default()),
            Err(ArchiveError::Unsupported(_))
        ));
    }

    #[test]
    fn test_unity_malformed_headers() {
        let dir = tempdir().expect("tempdir");

        // 1. Truncated (< 16 bytes)
        let truncated_path = dir.path().join("short.bundle");
        fs::write(&truncated_path, b"UnityFS").expect("write");
        let handler = UnityHandler;
        let res = handler.analyze(&truncated_path);
        assert!(res.is_err());

        // 2. Bad signature
        let bad_sig_path = dir.path().join("bad_sig.bundle");
        let mut bad_bytes = vec![0u8; 128];
        bad_bytes[0..7].copy_from_slice(b"UnrealP");
        fs::write(&bad_sig_path, &bad_bytes).expect("write");
        let res2 = handler.analyze(&bad_sig_path);
        assert!(res2.is_err());
    }
}
