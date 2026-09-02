//! Unreal Engine 4 & 5 PAK Archive Parser.
//!
//! Parses Unreal Engine `.pak` files by reading the `FPakInfo` trailer at the end
//! of the archive, extracting the Table of Contents (TOC) / Index, identifying
//! localized assets (`Content/Localization/`, `Content/WwiseAudio/`, `Content/Movies/`, `*LocRes*`),
//! and reporting localized entries read-only. Mutation requires validated repacking.

use byteorder::{LittleEndian, ReadBytesExt};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::{
    ArchiveAnalysis, ArchiveError, ArchiveHandler, ArchiveType, TrimOptions, TrimResult,
    TrimmableChunk,
};
use crate::sparse::get_on_disk_size;

pub const PAK_MAGIC: u32 = 0x5A6F12E1;

#[derive(Debug, Clone)]
pub struct PakInfo {
    pub magic: u32,
    pub version: i32,
    pub index_offset: i64,
    pub index_size: i64,
    pub index_sha1: [u8; 20],
    pub is_encrypted: bool,
}

#[derive(Debug, Clone)]
pub struct PakEntry {
    pub path: String,
    pub offset: i64,
    pub size: i64,
    pub uncompressed_size: i64,
    pub compression_method: i32,
    pub is_encrypted: bool,
}

pub struct UnrealPakHandler;

impl ArchiveHandler for UnrealPakHandler {
    fn archive_type(&self) -> ArchiveType {
        ArchiveType::UnrealPak
    }

    fn analyze(&self, path: &Path) -> Result<ArchiveAnalysis, ArchiveError> {
        let mut file = File::open(path)?;
        let total_size = file.metadata()?.len();
        let on_disk_size = get_on_disk_size(path).unwrap_or(total_size);

        let (pak_info, entries) = parse_unreal_pak(&mut file, total_size)?;

        let mut detected_languages = Vec::new();
        let mut trimmable_chunks = Vec::new();
        let mut total_trimmable_bytes = 0u64;

        for entry in &entries {
            let (is_loc, lang_opt, category) = classify_ue_asset(&entry.path);

            if let Some(ref lang) = lang_opt {
                if !detected_languages.contains(lang) {
                    detected_languages.push(lang.clone());
                }
            }

            // Zero-in-place is safe only for entries stored without compression
            // or encryption: their payload bytes are the raw asset data, so
            // overwriting them with zeros cannot corrupt a compressed block or
            // land inside ciphertext. Compressed and encrypted entries stay
            // `false` and belong to the "needs repacking" bucket.
            //
            // Open precondition: the offset semantics used here have never been
            // checked against a real UE PAK. `create_synthetic_unreal_pak` below
            // writes bare payload bytes with no serialized `FPakEntry` header in
            // front of them, so it cannot catch an inline-header offset shift
            // that a real pak may have. That verification must happen before
            // mutation is ever enabled for this format — it is harmless today
            // only because `trim()` unconditionally refuses to run.
            let can_zero_in_place = entry.compression_method == 0 && !entry.is_encrypted;

            if is_loc && can_zero_in_place {
                total_trimmable_bytes = total_trimmable_bytes.saturating_add(entry.size as u64);
            }

            trimmable_chunks.push(TrimmableChunk {
                id: entry.path.clone(),
                name: entry.path.clone(),
                offset: entry.offset as u64,
                length: entry.size as u64,
                is_language: is_loc,
                language: lang_opt,
                category,
                can_zero_in_place,
            });
        }

        let details = format!(
            "Unreal Engine PAK (Version {}, {} entries, {} encrypted)",
            pak_info.version,
            entries.len(),
            if pak_info.is_encrypted { "YES" } else { "NO" }
        );

        Ok(ArchiveAnalysis {
            archive_type: ArchiveType::UnrealPak,
            path: path.to_path_buf(),
            total_size,
            on_disk_size,
            detected_languages,
            trimmable_chunks,
            total_trimmable_bytes,
            estimated_savings_bytes: total_trimmable_bytes,
            details,
        })
    }

    fn trim(&self, _path: &Path, _options: &TrimOptions) -> Result<TrimResult, ArchiveError> {
        Err(ArchiveError::Unsupported(
            "Unreal PAK mutation is disabled until a validated repak-based implementation is available"
                .to_string(),
        ))
    }
}

/// Parses an Unreal Engine PAK file trailer and TOC index.
pub fn parse_unreal_pak(
    file: &mut File,
    file_len: u64,
) -> Result<(PakInfo, Vec<PakEntry>), ArchiveError> {
    if file_len < 44 {
        return Err(ArchiveError::InvalidFormat(
            ArchiveType::UnrealPak,
            "File too small for Unreal Pak trailer".to_string(),
        ));
    }

    // Read last 256 bytes to locate magic 0x5A6F12E1
    let seek_len = file_len.min(256);
    file.seek(SeekFrom::End(-(seek_len as i64)))?;
    let mut trailer_buf = vec![0u8; seek_len as usize];
    file.read_exact(&mut trailer_buf)?;

    let magic_bytes = [0xE1, 0x12, 0x6F, 0x5A]; // 0x5A6F12E1 in LE
    let magic_pos = trailer_buf
        .windows(4)
        .rposition(|w| w == magic_bytes)
        .ok_or_else(|| {
            ArchiveError::InvalidFormat(
                ArchiveType::UnrealPak,
                "Unreal Pak magic signature 0x5A6F12E1 not found in footer".to_string(),
            )
        })?;

    // The FPakInfo starts either at magic_pos - 44 or is structured with magic at offset
    // In standard FPakInfo:
    // [Magic: u32] [Version: i32] [IndexOffset: i64] [IndexSize: i64] [IndexSHA1: 20 bytes] [bEncrypted: u8] ...
    let pak_info_start = file_len - (seek_len - magic_pos as u64);
    file.seek(SeekFrom::Start(pak_info_start))?;

    let magic = file.read_u32::<LittleEndian>()?;
    let version = file.read_i32::<LittleEndian>()?;
    let index_offset = file.read_i64::<LittleEndian>()?;
    let index_size = file.read_i64::<LittleEndian>()?;

    let mut index_sha1 = [0u8; 20];
    let _ = file.read(&mut index_sha1);
    let is_encrypted = file.read_u8().unwrap_or(0) != 0;

    let pak_info = PakInfo {
        magic,
        version,
        index_offset,
        index_size,
        index_sha1,
        is_encrypted,
    };

    if is_encrypted {
        return Err(ArchiveError::Encrypted(
            "Unreal Pak index is AES-encrypted".to_string(),
        ));
    }

    if index_offset < 0 || (index_offset as u64) >= file_len {
        return Err(ArchiveError::InvalidFormat(
            ArchiveType::UnrealPak,
            format!("Invalid IndexOffset {}", index_offset),
        ));
    }

    // Seek to index
    file.seek(SeekFrom::Start(index_offset as u64))?;

    // Read Mount Point (FString)
    let _mount_point = read_fstring(file)?;

    // Read Entry Count
    let entry_count = match file.read_i32::<LittleEndian>() {
        Ok(c) if (0..2_000_000).contains(&c) => c as usize,
        _ => {
            return Ok((pak_info, Vec::new()));
        }
    };

    let mut entries = Vec::with_capacity(entry_count.min(100_000));

    for _ in 0..entry_count {
        let path = match read_fstring(file) {
            Ok(p) => p,
            Err(_) => break,
        };

        let offset = file.read_i64::<LittleEndian>().unwrap_or(0);
        let size = file.read_i64::<LittleEndian>().unwrap_or(0);
        let uncompressed_size = file.read_i64::<LittleEndian>().unwrap_or(0);
        let compression_method = file.read_i32::<LittleEndian>().unwrap_or(0);

        // Skip SHA1 / hash (20 bytes)
        let mut _hash = [0u8; 20];
        let _ = file.read(&mut _hash);

        // If compressed, skip block list
        if compression_method != 0 {
            if let Ok(block_count) = file.read_i32::<LittleEndian>() {
                if block_count > 0 && block_count < 100_000 {
                    let _ = file.seek(SeekFrom::Current((block_count as i64) * 16));
                }
            }
        }

        let entry_encrypted = file.read_u8().unwrap_or(0) != 0;

        // Skip compression block size if version >= 3
        if version >= 3 {
            let _ = file.read_i32::<LittleEndian>();
        }

        if offset >= 0 && size >= 0 {
            entries.push(PakEntry {
                path,
                offset,
                size,
                uncompressed_size,
                compression_method,
                is_encrypted: entry_encrypted,
            });
        }
    }

    Ok((pak_info, entries))
}

/// Reads an Unreal Engine FString (length-prefixed ASCII or UTF-16).
fn read_fstring(file: &mut File) -> Result<String, ArchiveError> {
    let len = file.read_i32::<LittleEndian>()?;
    if len == 0 {
        return Ok(String::new());
    }

    if len > 0 {
        // ASCII / UTF-8
        let str_len = len as usize;
        if str_len > 4096 {
            return Err(ArchiveError::InvalidFormat(
                ArchiveType::UnrealPak,
                "FString length exceeds reasonable limit".to_string(),
            ));
        }
        let mut buf = vec![0u8; str_len];
        file.read_exact(&mut buf)?;
        if buf.last() == Some(&0) {
            buf.pop();
        }
        Ok(String::from_utf8_lossy(&buf).to_string())
    } else {
        // UTF-16LE: len < 0. Use unsigned_abs to avoid panic on i32::MIN.
        let char_count = len.unsigned_abs() as usize;
        if char_count > 4096 {
            return Err(ArchiveError::InvalidFormat(
                ArchiveType::UnrealPak,
                "FString UTF-16 length exceeds limit".to_string(),
            ));
        }
        let mut u16_buf = Vec::with_capacity(char_count);
        for _ in 0..char_count {
            u16_buf.push(file.read_u16::<LittleEndian>()?);
        }
        if u16_buf.last() == Some(&0) {
            u16_buf.pop();
        }
        Ok(String::from_utf16_lossy(&u16_buf))
    }
}

/// Classifies an asset path inside an Unreal Engine PAK.
pub fn classify_ue_asset(path: &str) -> (bool, Option<String>, String) {
    let lower = path.to_lowercase();

    // Check for localization markers
    if lower.contains("localization") || lower.contains(".locres") || lower.contains(".locmeta") {
        let lang = extract_language_from_path(&lower);
        let is_loc = lang.is_some();
        return (is_loc, lang, "Unreal Localization Text".to_string());
    }

    if lower.contains("wwiseaudio")
        || lower.contains("audio/voices")
        || lower.contains("sound/dialogue")
    {
        let lang = extract_language_from_path(&lower);
        let is_loc = lang.is_some();
        return (is_loc, lang, "Unreal Localized Audio".to_string());
    }

    if lower.contains("movies") || lower.contains("cinematics") {
        let lang = extract_language_from_path(&lower);
        let is_loc = lang.is_some();
        return (is_loc, lang, "Unreal Localized Video".to_string());
    }

    let lang = extract_language_from_path(&lower);
    if let Some(l) = lang {
        return (true, Some(l), "Unreal Localized Asset".to_string());
    }

    (false, None, "Unreal General Asset".to_string())
}

/// Extracts language name / code from a path snippet.
fn extract_language_from_path(path: &str) -> Option<String> {
    let tokens: &[(&str, &str)] = &[
        ("/de/", "German"),
        ("/ger/", "German"),
        ("_de.", "German"),
        ("_ger.", "German"),
        ("/fr/", "French"),
        ("/fra/", "French"),
        ("/fre/", "French"),
        ("_fr.", "French"),
        ("_fre.", "French"),
        ("/es/", "Spanish"),
        ("/spa/", "Spanish"),
        ("_es.", "Spanish"),
        ("_spa.", "Spanish"),
        ("/ru/", "Russian"),
        ("/rus/", "Russian"),
        ("_ru.", "Russian"),
        ("_rus.", "Russian"),
        ("/it/", "Italian"),
        ("/ita/", "Italian"),
        ("_it.", "Italian"),
        ("/ja/", "Japanese"),
        ("/jpn/", "Japanese"),
        ("_ja.", "Japanese"),
        ("/zh/", "Chinese"),
        ("/chn/", "Chinese"),
        ("/zho/", "Chinese"),
        ("/ko/", "Korean"),
        ("/kor/", "Korean"),
        ("/pl/", "Polish"),
        ("/pt/", "Portuguese"),
        ("/en/", "English"),
        ("/eng/", "English"),
        ("_en.", "English"),
    ];

    for &(token, name) in tokens {
        if path.contains(token) {
            return Some(name.to_string());
        }
    }

    None
}

/// Helper to generate a valid synthetic Unreal Engine PAK for unit tests.
/// All entries are written uncompressed and unencrypted.
pub fn create_synthetic_unreal_pak(entries: &[(&str, &[u8])], /* (path, payload) */) -> Vec<u8> {
    let flagged: Vec<(&str, &[u8], i32, bool)> =
        entries.iter().map(|&(p, d)| (p, d, 0, false)).collect();
    create_synthetic_unreal_pak_with_flags(&flagged)
}

/// Same as [`create_synthetic_unreal_pak`], but lets each entry declare its own
/// compression method and encrypted flag, for tests that need to distinguish
/// zero-in-place-eligible entries from compressed/encrypted ones.
pub fn create_synthetic_unreal_pak_with_flags(
    entries: &[(&str, &[u8], i32, bool)], // (path, payload, compression_method, encrypted)
) -> Vec<u8> {
    use byteorder::WriteBytesExt;
    let mut buf = Vec::new();

    // 1. Write file entries
    let mut entry_records = Vec::new();

    for &(path, data, compression_method, encrypted) in entries {
        // Cluster align offset
        while buf.len() % 4096 != 0 {
            buf.push(0);
        }
        let offset = buf.len() as i64;
        let size = data.len() as i64;
        buf.extend_from_slice(data);

        entry_records.push((
            path.to_string(),
            offset,
            size,
            compression_method,
            encrypted,
        ));
    }

    // 2. Index TOC
    let index_offset = buf.len() as i64;

    // Mount point FString ("../../../")
    let mount = "../../../\0";
    buf.write_i32::<LittleEndian>(mount.len() as i32).unwrap();
    buf.extend_from_slice(mount.as_bytes());

    // Entry count
    buf.write_i32::<LittleEndian>(entry_records.len() as i32)
        .unwrap();

    for (path, offset, size, compression_method, encrypted) in &entry_records {
        let path_with_null = format!("{}\0", path);
        buf.write_i32::<LittleEndian>(path_with_null.len() as i32)
            .unwrap();
        buf.extend_from_slice(path_with_null.as_bytes());

        buf.write_i64::<LittleEndian>(*offset).unwrap();
        buf.write_i64::<LittleEndian>(*size).unwrap();
        buf.write_i64::<LittleEndian>(*size).unwrap(); // uncompressed size
        buf.write_i32::<LittleEndian>(*compression_method).unwrap();
        buf.extend_from_slice(&[0u8; 20]); // sha1 hash
        if *compression_method != 0 {
            buf.write_i32::<LittleEndian>(0).unwrap(); // block_count = 0 (no blocks to skip)
        }
        buf.write_u8(u8::from(*encrypted)).unwrap();
        buf.write_i32::<LittleEndian>(65536).unwrap(); // compression block size (v3+)
    }

    let index_size = (buf.len() as i64) - index_offset;

    // 3. FPakInfo Trailer
    buf.write_u32::<LittleEndian>(PAK_MAGIC).unwrap(); // Magic
    buf.write_i32::<LittleEndian>(8).unwrap(); // Version 8
    buf.write_i64::<LittleEndian>(index_offset).unwrap();
    buf.write_i64::<LittleEndian>(index_size).unwrap();
    buf.extend_from_slice(&[0u8; 20]); // Index SHA1
    buf.write_u8(0).unwrap(); // Encrypted flag

    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_unreal_pak_synthetic_parsing_and_trimming() {
        let dir = tempdir().expect("tempdir");
        let pak_path = dir.path().join("Game-WindowsNoEditor.pak");

        let entries = [
            ("Content/Paks/Core.uasset", vec![0x11u8; 8192]),
            (
                "Content/Localization/Game/de/Game.locres",
                vec![0x22u8; 16384],
            ),
            (
                "Content/Localization/Game/fr/Game.locres",
                vec![0x33u8; 16384],
            ),
            (
                "Content/Localization/Game/en/Game.locres",
                vec![0x44u8; 16384],
            ),
        ];

        let entries_ref: Vec<(&str, &[u8])> =
            entries.iter().map(|(p, d)| (*p, d.as_slice())).collect();
        let pak_bytes = create_synthetic_unreal_pak(&entries_ref);
        fs::write(&pak_path, &pak_bytes).expect("write pak");

        let handler = UnrealPakHandler;
        let analysis = handler.analyze(&pak_path).expect("analyze pak");

        assert_eq!(analysis.archive_type, ArchiveType::UnrealPak);
        assert!(analysis.detected_languages.contains(&"German".to_string()));
        assert!(analysis.detected_languages.contains(&"French".to_string()));
        assert!(analysis.detected_languages.contains(&"English".to_string()));

        let options = TrimOptions {
            keep_languages: vec!["english".to_string()],
            dry_run: false,
            create_snapshot: true,
            force_unsafe: false,
            custom_backup_dir: None,
        };

        // Every entry in this fixture is stored uncompressed and unencrypted, so
        // all four are individually zero-in-place-eligible, but only the three
        // localized entries (de/fr/en) count toward the trimmable total —
        // Core.uasset is a general asset, not localized content.
        assert!(analysis
            .trimmable_chunks
            .iter()
            .all(|chunk| chunk.can_zero_in_place));
        assert_eq!(analysis.total_trimmable_bytes, 16384 * 3);
        assert_eq!(analysis.estimated_savings_bytes, 16384 * 3);
        assert!(matches!(
            handler.trim(&pak_path, &options),
            Err(ArchiveError::Unsupported(_))
        ));

        let ambiguous = classify_ue_asset("Content/Localization/Game/Game.locres");
        assert!(
            !ambiguous.0,
            "unknown UE localization language must fail closed"
        );
    }

    #[test]
    fn unreal_pak_marks_compressed_and_encrypted_language_entries_not_zeroable() {
        let dir = tempdir().expect("tempdir");
        let pak_path = dir.path().join("Game-Compressed.pak");

        let de_data = vec![0x22u8; 16384];
        let fr_data = vec![0x33u8; 16384];
        let es_data = vec![0x44u8; 16384];
        let en_data = vec![0x55u8; 16384];

        let entries: [(&str, &[u8], i32, bool); 4] = [
            (
                "Content/Localization/Game/de/Game.locres",
                &de_data,
                0,
                false,
            ), // uncompressed, unencrypted -> zeroable
            (
                "Content/Localization/Game/fr/Game.locres",
                &fr_data,
                1,
                false,
            ), // compressed -> NOT zeroable
            (
                "Content/Localization/Game/es/Game.locres",
                &es_data,
                0,
                true,
            ), // encrypted -> NOT zeroable
            (
                "Content/Localization/Game/en/Game.locres",
                &en_data,
                0,
                false,
            ), // uncompressed, unencrypted -> zeroable
        ];

        let pak_bytes = create_synthetic_unreal_pak_with_flags(&entries);
        fs::write(&pak_path, &pak_bytes).expect("write pak");

        let handler = UnrealPakHandler;
        let analysis = handler.analyze(&pak_path).expect("analyze pak");

        let by_lang = |lang: &str| -> &TrimmableChunk {
            analysis
                .trimmable_chunks
                .iter()
                .find(|c| c.language.as_deref() == Some(lang))
                .unwrap_or_else(|| panic!("no chunk for language {lang}"))
        };

        assert!(by_lang("German").can_zero_in_place);
        assert!(
            !by_lang("French").can_zero_in_place,
            "compressed entries must not be zero-in-place-eligible"
        );
        assert!(
            !by_lang("Spanish").can_zero_in_place,
            "encrypted entries must not be zero-in-place-eligible"
        );
        assert!(by_lang("English").can_zero_in_place);

        // The sum must equal exactly the entries that passed both zero-in-place
        // conditions (uncompressed AND unencrypted), not all four language
        // entries — the compressed French and encrypted Spanish entries are
        // localized but must not contribute to the trimmable total.
        assert_eq!(analysis.total_trimmable_bytes, 16384 * 2);
        assert_eq!(analysis.estimated_savings_bytes, 16384 * 2);

        assert!(matches!(
            handler.trim(&pak_path, &TrimOptions::default()),
            Err(ArchiveError::Unsupported(_))
        ));
    }

    #[test]
    fn test_unreal_pak_malformed_and_edge_cases() {
        let dir = tempdir().expect("tempdir");

        // 1. Truncated file (< 44 bytes)
        let truncated_path = dir.path().join("truncated.pak");
        fs::write(&truncated_path, b"tiny pak").expect("write");
        let handler = UnrealPakHandler;
        let res = handler.analyze(&truncated_path);
        assert!(res.is_err());

        // 2. Missing magic in trailer
        let no_magic_path = dir.path().join("no_magic.pak");
        fs::write(&no_magic_path, vec![0xEEu8; 1024]).expect("write");
        let res2 = handler.analyze(&no_magic_path);
        assert!(res2.is_err());

        // 3. FString length i32::MIN test (read_fstring overflow safety)
        let fstring_test_path = dir.path().join("fstring_min.bin");
        let mut min_len_bytes = Vec::new();
        min_len_bytes.extend_from_slice(&i32::MIN.to_le_bytes());
        min_len_bytes.extend_from_slice(&[0u8; 64]);
        fs::write(&fstring_test_path, &min_len_bytes).expect("write");

        let mut f = File::open(&fstring_test_path).expect("open");
        let fstr_res = read_fstring(&mut f);
        assert!(
            fstr_res.is_err(),
            "i32::MIN length must return error without panicking"
        );
    }
}
