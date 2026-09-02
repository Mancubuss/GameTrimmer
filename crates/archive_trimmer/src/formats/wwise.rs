//! Audiokinetic Wwise PCK & BNK Archive Handler.
//!
//! Parses Wwise `.pck` files (`AKPK` container):
//! - Header and Language Map table (extracts UTF-16LE / ASCII language strings).
//! - Bank Table and Stream Table (`.wem` audio files and `.bnk` soundbanks).
//! - Maps language IDs to localized dialogue streams vs SFX / common audio.
//! - Reports candidate ranges read-only; mutation is disabled pending full rollback.

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::{
    is_known_language, is_language_kept, ArchiveAnalysis, ArchiveError, ArchiveHandler,
    ArchiveType, TrimOptions, TrimResult, TrimmableChunk,
};
use crate::sparse::get_on_disk_size;

#[derive(Debug, Clone)]
pub struct WwiseLanguage {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct WwiseFileEntry {
    pub id: u32,
    pub block_size: u32,
    pub file_size: u64,
    pub file_offset: u64,
    pub language_id: u32,
    pub is_stream: bool,
}

#[derive(Debug, Clone)]
pub struct WwisePckHeader {
    pub header_size: u32,
    pub version: u32,
    pub language_map: HashMap<u32, String>,
    pub entries: Vec<WwiseFileEntry>,
}

pub struct WwiseHandler;

impl ArchiveHandler for WwiseHandler {
    fn archive_type(&self) -> ArchiveType {
        ArchiveType::WwisePck
    }

    fn analyze(&self, path: &Path) -> Result<ArchiveAnalysis, ArchiveError> {
        let mut file = File::open(path)?;
        let total_size = file.metadata()?.len();
        let on_disk_size = get_on_disk_size(path).unwrap_or(total_size);

        let pck = parse_wwise_pck(&mut file, 0, total_size)?;

        let default_keep = vec![
            "english".to_string(),
            "en".to_string(),
            "en-us".to_string(),
            "en-gb".to_string(),
            "sfx".to_string(),
            "common".to_string(),
        ];

        let mut detected_languages = Vec::new();
        for (lid, name) in &pck.language_map {
            if *lid != 0
                && !name.is_empty()
                && !name.eq_ignore_ascii_case("sfx")
                && !name.eq_ignore_ascii_case("common")
                && !detected_languages.contains(name)
            {
                detected_languages.push(name.clone());
            }
        }
        detected_languages.sort();

        let mut trimmable_chunks = Vec::new();
        let mut total_trimmable_bytes = 0u64;
        let header_end = u64::from(pck.header_size);

        for entry in &pck.entries {
            // Only valid streams (.wem files) are trimmable dialogue/voice audio streams
            if !entry.is_stream {
                continue;
            }

            // Must be within file bounds and size > 0
            if entry.file_size == 0
                || entry.file_offset < header_end
                || entry.file_offset >= total_size
                || entry.file_offset.saturating_add(entry.file_size) > total_size
            {
                continue;
            }

            let lang_name = pck
                .language_map
                .get(&entry.language_id)
                .cloned()
                .unwrap_or_else(|| {
                    if entry.language_id == 0 {
                        "SFX".to_string()
                    } else {
                        format!("Language_{}", entry.language_id)
                    }
                });

            let is_sfx_or_common = entry.language_id == 0
                || lang_name.eq_ignore_ascii_case("sfx")
                || lang_name.eq_ignore_ascii_case("common");

            let is_language = !is_sfx_or_common && is_known_language(&lang_name);
            let is_default_trim = is_language && !is_language_kept(&lang_name, &default_keep);

            let chunk_name = format!("{}.wem", entry.id);

            if is_default_trim {
                total_trimmable_bytes = total_trimmable_bytes.saturating_add(entry.file_size);
            }

            trimmable_chunks.push(TrimmableChunk {
                id: entry.id.to_string(),
                name: chunk_name,
                offset: entry.file_offset,
                length: entry.file_size,
                is_language,
                language: Some(lang_name),
                category: "Wwise Voice Stream".to_string(),
                // Unknown language IDs/names are intentionally read-only.  A guessed
                // `Language_<id>` must never become a destructive target.
                can_zero_in_place: is_language,
            });
        }

        let bank_count = pck.entries.iter().filter(|e| !e.is_stream).count();
        let stream_count = pck.entries.iter().filter(|e| e.is_stream).count();

        let details = format!(
            "Wwise PCK ({} banks, {} streams, {} languages detected)",
            bank_count,
            stream_count,
            detected_languages.len()
        );

        Ok(ArchiveAnalysis {
            archive_type: ArchiveType::WwisePck,
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
            "Wwise PCK mutation is disabled until a full payload rollback transaction is available"
                .to_string(),
        ))
    }
}

/// Parses standard Audiokinetic Wwise PCK archive header and tables at `base_offset`.
pub fn parse_wwise_pck(
    file: &mut File,
    base_offset: u64,
    file_len: u64,
) -> Result<WwisePckHeader, ArchiveError> {
    file.seek(SeekFrom::Start(base_offset))?;

    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    if &magic != b"AKPK" {
        return Err(ArchiveError::InvalidFormat(
            ArchiveType::WwisePck,
            format!("Expected magic AKPK, found {:?}", magic),
        ));
    }

    let header_size = file.read_u32::<LittleEndian>()?;
    let unknown1 = file.read_u32::<LittleEndian>()?;
    let lang_table_size = file.read_u32::<LittleEndian>()?;
    let bank_table_size = file.read_u32::<LittleEndian>()?;
    let stream_table_size = file.read_u32::<LittleEndian>()?;
    // Present on real AKPK files at base+24, immediately before the language
    // map (which starts at base+28). Not part of the trimmable data itself,
    // but its declared size is real header content: a corrupt/truncated
    // externals table means the header is corrupt too, so it belongs in the
    // bounds check below.
    let externals_table_size = file.read_u32::<LittleEndian>()?;

    let lang_sec_start = base_offset.saturating_add(24);
    let bank_sec_start = lang_sec_start
        .saturating_add(4)
        .saturating_add(lang_table_size as u64);
    let stream_sec_start = bank_sec_start.saturating_add(bank_table_size as u64);
    let lang_sec_end = bank_sec_start;
    let bank_sec_end = stream_sec_start;
    let stream_sec_end = stream_sec_start.saturating_add(stream_table_size as u64);
    let externals_sec_end = stream_sec_end.saturating_add(externals_table_size as u64);
    // `header_size` is measured from offset 8 (right after the magic and the
    // header_size field itself), not from the start of the file. Real AKPK
    // files were byte-verified against this: base+8+header_size lands exactly
    // at the end of the declared section tables (including the externals
    // table that trails the stream table).
    let declared_header_end = base_offset
        .saturating_add(8)
        .saturating_add(header_size as u64);
    if externals_sec_end > file_len
        || externals_sec_end > declared_header_end
        || declared_header_end > file_len
    {
        return Err(ArchiveError::InvalidFormat(
            ArchiveType::WwisePck,
            "Declared Wwise sections exceed header or file bounds".to_string(),
        ));
    }

    let mut language_map = HashMap::new();
    language_map.insert(0, "SFX".to_string());

    // Parse Language Map Table. The real language map starts at base_offset +
    // 28 (base+24 is `externals_table_size`, read above), so the count field
    // itself lives at lang_sec_start + 4, not at lang_sec_start. Byte-verified
    // against a real AKPK: reading the count from lang_sec_start directly
    // only "worked" when externals_table_size happened to equal the true
    // language count by coincidence.
    if lang_table_size > 0 && lang_sec_start.saturating_add(8) <= file_len {
        file.seek(SeekFrom::Start(lang_sec_start.saturating_add(4)))?;
        let lang_count = file.read_u32::<LittleEndian>()?;
        if lang_count > 100_000
            || lang_sec_start
                .saturating_add(8)
                .saturating_add(u64::from(lang_count).saturating_mul(8))
                > lang_sec_end
        {
            return Err(ArchiveError::InvalidFormat(
                ArchiveType::WwisePck,
                "Language descriptor count exceeds declared section".to_string(),
            ));
        }

        // Bound count to prevent excessive allocations on corrupt data
        let safe_lang_count = lang_count;
        let mut lang_descriptors = Vec::with_capacity(safe_lang_count.min(1024) as usize);

        for i in 0..safe_lang_count {
            let desc_offset = lang_sec_start
                .saturating_add(8)
                .saturating_add((i as u64).saturating_mul(8));
            if desc_offset.saturating_add(8) > lang_sec_end {
                break;
            }
            if file.seek(SeekFrom::Start(desc_offset)).is_err() {
                break;
            }
            let str_offset = match file.read_u32::<LittleEndian>() {
                Ok(off) => off,
                Err(_) => break,
            };
            let lang_id = match file.read_u32::<LittleEndian>() {
                Ok(id) => id,
                Err(_) => break,
            };
            lang_descriptors.push((str_offset, lang_id));
        }

        for (str_offset, lang_id) in lang_descriptors {
            let target_pos = lang_sec_start
                .saturating_add(4)
                .saturating_add(str_offset as u64);
            if target_pos < lang_sec_end && file.seek(SeekFrom::Start(target_pos)).is_ok() {
                if let Ok(lang_name) = read_wwise_string(file, lang_sec_end) {
                    if !lang_name.is_empty() {
                        language_map.insert(lang_id, lang_name);
                    }
                }
            }
        }
    }

    let mut entries = Vec::new();

    // Parse Bank Table
    if bank_table_size > 0 && bank_sec_start.saturating_add(4) <= file_len {
        file.seek(SeekFrom::Start(bank_sec_start))?;
        let bank_count = file.read_u32::<LittleEndian>()?;
        if bank_count > 500_000
            || bank_sec_start
                .saturating_add(4)
                .saturating_add(u64::from(bank_count).saturating_mul(20))
                > bank_sec_end
        {
            return Err(ArchiveError::InvalidFormat(
                ArchiveType::WwisePck,
                "Bank descriptor count exceeds declared section".to_string(),
            ));
        }
        let safe_bank_count = bank_count;

        for i in 0..safe_bank_count {
            let entry_offset = bank_sec_start
                .saturating_add(4)
                .saturating_add((i as u64).saturating_mul(20));
            if entry_offset.saturating_add(20) > bank_sec_end {
                break;
            }
            if file.seek(SeekFrom::Start(entry_offset)).is_err() {
                break;
            }
            let id = match file.read_u32::<LittleEndian>() {
                Ok(v) => v,
                Err(_) => break,
            };
            let block_size = file.read_u32::<LittleEndian>()?.max(1);
            let file_size = file.read_u32::<LittleEndian>()? as u64;
            let file_offset_raw = file.read_u32::<LittleEndian>()? as u64;
            let language_id = file.read_u32::<LittleEndian>()?;

            let file_offset =
                base_offset.saturating_add(file_offset_raw.saturating_mul(block_size as u64));

            entries.push(WwiseFileEntry {
                id,
                block_size,
                file_size,
                file_offset,
                language_id,
                is_stream: false,
            });
        }
    }

    // Parse Stream Table (.wem files)
    if stream_table_size > 0 && stream_sec_start.saturating_add(4) <= file_len {
        file.seek(SeekFrom::Start(stream_sec_start))?;
        let stream_count = file.read_u32::<LittleEndian>()?;
        if stream_count > 500_000
            || stream_sec_start
                .saturating_add(4)
                .saturating_add(u64::from(stream_count).saturating_mul(20))
                > stream_sec_end
        {
            return Err(ArchiveError::InvalidFormat(
                ArchiveType::WwisePck,
                "Stream descriptor count exceeds declared section".to_string(),
            ));
        }
        let safe_stream_count = stream_count;

        for i in 0..safe_stream_count {
            let entry_offset = stream_sec_start
                .saturating_add(4)
                .saturating_add((i as u64).saturating_mul(20));
            if entry_offset.saturating_add(20) > stream_sec_end {
                break;
            }
            if file.seek(SeekFrom::Start(entry_offset)).is_err() {
                break;
            }
            let id = match file.read_u32::<LittleEndian>() {
                Ok(v) => v,
                Err(_) => break,
            };
            let block_size = file.read_u32::<LittleEndian>()?.max(1);
            let file_size = file.read_u32::<LittleEndian>()? as u64;
            let file_offset_raw = file.read_u32::<LittleEndian>()? as u64;
            let language_id = file.read_u32::<LittleEndian>()?;

            let file_offset =
                base_offset.saturating_add(file_offset_raw.saturating_mul(block_size as u64));

            entries.push(WwiseFileEntry {
                id,
                block_size,
                file_size,
                file_offset,
                language_id,
                is_stream: true,
            });
        }
    }

    Ok(WwisePckHeader {
        header_size,
        version: unknown1,
        language_map,
        entries,
    })
}

/// Reads a UTF-16LE null-terminated string from Wwise PCK header.
fn read_wwise_string(file: &mut File, section_end: u64) -> Result<String, ArchiveError> {
    let mut u16_buf = Vec::new();
    loop {
        if file.stream_position()?.saturating_add(2) > section_end {
            return Err(ArchiveError::InvalidFormat(
                ArchiveType::WwisePck,
                "Language string exceeds declared section".to_string(),
            ));
        }
        match file.read_u16::<LittleEndian>() {
            Ok(0) | Err(_) => break,
            Ok(code) => {
                u16_buf.push(code);
                if u16_buf.len() > 256 {
                    break;
                }
            }
        }
    }

    if !u16_buf.is_empty() {
        Ok(String::from_utf16_lossy(&u16_buf).trim().to_string())
    } else {
        Ok(String::new())
    }
}

/// Helper to generate a valid synthetic Wwise PCK byte buffer for unit tests.
pub fn create_synthetic_wwise_pck(
    languages: &[(u32, &str)],
    streams: &[(u32, u32, usize)], // (id, language_id, data_len)
) -> Vec<u8> {
    let mut buf = Vec::new();

    // 1. Magic
    buf.extend_from_slice(b"AKPK");

    // 2. Placeholders for header fields: header_size, unknown, language_map_size,
    // bank_table_size, stream_table_size, externals_table_size. Real AKPK files
    // carry all six fields (magic + 6*u32 = 28 bytes) before the language map
    // starts; earlier versions of this fixture stopped at 5 fields (24 bytes)
    // and let the language map's own count field double as the "6th field",
    // which is exactly the offset bug the real parser had.
    let header_pos = buf.len();
    buf.write_u32::<LittleEndian>(0).unwrap(); // header_size (fixed up below)
    buf.write_u32::<LittleEndian>(1).unwrap(); // unknown / version
    buf.write_u32::<LittleEndian>(0).unwrap(); // language_map_size (fixed up below)
    buf.write_u32::<LittleEndian>(0).unwrap(); // bank_table_size (fixed up below)
    buf.write_u32::<LittleEndian>(0).unwrap(); // stream_table_size (fixed up below)
    buf.write_u32::<LittleEndian>(0).unwrap(); // externals_table_size (no externals in test fixtures)

    assert_eq!(buf.len(), 28);

    // 3. Real language section at base_offset + 28
    let lang_sec_start = buf.len(); // 28 -- position of the true lang_count field
    buf.write_u32::<LittleEndian>(languages.len() as u32)
        .unwrap(); // lang_count at 28..32

    let desc_start = buf.len(); // 32
    for _ in languages {
        buf.write_u32::<LittleEndian>(0).unwrap(); // placeholder str_offset
        buf.write_u32::<LittleEndian>(0).unwrap(); // placeholder lang_id
    }

    // Write strings and fix up descriptors (str_offset relative to lang_sec_start = 28,
    // matching the parser's declared_header_end-adjacent anchor: base+24+4)
    let mut lang_string_offsets = Vec::new();
    for (id, name) in languages {
        let str_pos = buf.len();
        let rel_off = (str_pos - lang_sec_start) as u32;
        lang_string_offsets.push((rel_off, *id));
        for u in name.encode_utf16() {
            buf.write_u16::<LittleEndian>(u).unwrap();
        }
        buf.write_u16::<LittleEndian>(0).unwrap(); // null terminator
    }

    // Align language section to 4 bytes
    while (buf.len() - lang_sec_start) % 4 != 0 {
        buf.push(0);
    }

    // Rewrite descriptors
    for (i, &(str_off, id)) in lang_string_offsets.iter().enumerate() {
        let entry_pos = desc_start + (i * 8);
        buf[entry_pos..entry_pos + 4].copy_from_slice(&str_off.to_le_bytes());
        buf[entry_pos + 4..entry_pos + 8].copy_from_slice(&id.to_le_bytes());
    }

    let lang_table_size = (buf.len() - lang_sec_start) as u32;

    // 4. Bank table (empty in test)
    let bank_sec_start = buf.len();
    buf.write_u32::<LittleEndian>(0).unwrap(); // bank_count = 0
    let bank_table_size = (buf.len() - bank_sec_start) as u32;

    // 5. Stream table
    let stream_sec_start = buf.len();
    buf.write_u32::<LittleEndian>(streams.len() as u32).unwrap();

    let stream_desc_start = buf.len();
    for _ in streams {
        buf.write_u32::<LittleEndian>(0).unwrap(); // id
        buf.write_u32::<LittleEndian>(1).unwrap(); // block size (1 byte)
        buf.write_u32::<LittleEndian>(0).unwrap(); // size
        buf.write_u32::<LittleEndian>(0).unwrap(); // offset
        buf.write_u32::<LittleEndian>(0).unwrap(); // lang_id
    }

    let stream_table_size = (buf.len() - stream_sec_start) as u32;
    // header_size is measured from offset 8 (right after magic + header_size
    // itself), matching real AKPK files -- not the absolute buffer length.
    let header_size = (buf.len() - 8) as u32;

    // Fix up header fields in 0..28
    buf[header_pos..header_pos + 4].copy_from_slice(&header_size.to_le_bytes());
    buf[header_pos + 8..header_pos + 12].copy_from_slice(&lang_table_size.to_le_bytes());
    buf[header_pos + 12..header_pos + 16].copy_from_slice(&bank_table_size.to_le_bytes());
    buf[header_pos + 16..header_pos + 20].copy_from_slice(&stream_table_size.to_le_bytes());
    // externals_table_size at header_pos+20..24 is left as 0 (written above).

    // 6. Data payload
    for (i, &(id, lang_id, data_len)) in streams.iter().enumerate() {
        while buf.len() % 4096 != 0 {
            buf.push(0);
        }
        let stream_offset = buf.len() as u32;
        let stream_size = data_len as u32;

        buf.extend(vec![(id % 250) as u8; data_len]);

        let entry_pos = stream_desc_start + (i * 20);
        buf[entry_pos..entry_pos + 4].copy_from_slice(&id.to_le_bytes());
        buf[entry_pos + 4..entry_pos + 8].copy_from_slice(&1u32.to_le_bytes());
        buf[entry_pos + 8..entry_pos + 12].copy_from_slice(&stream_size.to_le_bytes());
        buf[entry_pos + 12..entry_pos + 16].copy_from_slice(&stream_offset.to_le_bytes());
        buf[entry_pos + 16..entry_pos + 20].copy_from_slice(&lang_id.to_le_bytes());
    }

    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_wwise_pck_synthetic_parsing_and_trimming() {
        let dir = tempdir().expect("tempdir");
        let pck_path = dir.path().join("VO_AMICIA_MEDIA.PC.PCK");

        let languages = vec![
            (0, "SFX"),
            (1, "English(US)"),
            (2, "German"),
            (3, "French"),
            (4, "Russian"),
        ];

        let streams = vec![
            (101, 0, 8192),  // SFX (8 KB) -> Kept / not trimmable
            (102, 1, 12288), // English (12 KB) -> Kept / not trimmable
            (103, 2, 16384), // German (16 KB) -> Trimmable
            (104, 3, 16384), // French (16 KB) -> Trimmable
            (105, 4, 16384), // Russian (16 KB) -> Trimmable
        ];

        let pck_bytes = create_synthetic_wwise_pck(&languages, &streams);
        fs::write(&pck_path, &pck_bytes).expect("write pck");

        let handler = WwiseHandler;
        let analysis = handler.analyze(&pck_path).expect("analyze pck");

        assert_eq!(analysis.archive_type, ArchiveType::WwisePck);
        assert!(analysis.detected_languages.contains(&"German".to_string()));
        assert!(analysis.detected_languages.contains(&"French".to_string()));
        assert!(analysis.detected_languages.contains(&"Russian".to_string()));
        assert_eq!(analysis.trimmable_chunks.len(), 5);
        assert_eq!(analysis.total_trimmable_bytes, 16384 * 3);

        let trimmable_chunks: Vec<_> = analysis
            .trimmable_chunks
            .iter()
            .filter(|c| c.is_language)
            .collect();
        // All recognized localized languages, including kept English, are
        // classified as language chunks; keep-list selection happens later.
        assert_eq!(trimmable_chunks.len(), 4);

        // Test trimming: keep English & SFX
        let options = TrimOptions {
            keep_languages: vec!["english".to_string(), "sfx".to_string()],
            dry_run: false,
            create_snapshot: true,
            force_unsafe: false,
            custom_backup_dir: None,
        };

        assert!(matches!(
            handler.trim(&pck_path, &options),
            Err(ArchiveError::Unsupported(_))
        ));
    }

    #[test]
    fn test_unknown_language_id_is_read_only() {
        let dir = tempdir().expect("tempdir");
        let pck_path = dir.path().join("unknown_language.pck");
        let pck_bytes = create_synthetic_wwise_pck(&[(1, "English(US)")], &[(10, 999, 8192)]);
        fs::write(&pck_path, pck_bytes).expect("write pck");

        let analysis = WwiseHandler.analyze(&pck_path).expect("analyze");
        assert_eq!(analysis.total_trimmable_bytes, 0);
        assert_eq!(
            analysis.trimmable_chunks[0].language.as_deref(),
            Some("Language_999")
        );
        assert!(!analysis.trimmable_chunks[0].is_language);
        assert!(!analysis.trimmable_chunks[0].can_zero_in_place);

        assert!(matches!(
            WwiseHandler.trim(&pck_path, &TrimOptions::default()),
            Err(ArchiveError::Unsupported(_))
        ));
    }

    #[test]
    fn test_wwise_malformed_headers() {
        let dir = tempdir().expect("tempdir");

        // 1. Bad magic
        let bad_magic_path = dir.path().join("bad_magic.pck");
        fs::write(&bad_magic_path, b"NOPE_BAD_MAGIC_HEADER_TEST_BYTES").expect("write");
        let handler = WwiseHandler;
        let res = handler.analyze(&bad_magic_path);
        assert!(res.is_err());

        // 2. Truncated header
        let truncated_path = dir.path().join("truncated.pck");
        fs::write(&truncated_path, b"AKPK\x00\x00").expect("write");
        let res2 = handler.analyze(&truncated_path);
        assert!(res2.is_err());

        // 3. Huge fake lang count (should not panic or OOM)
        let mut huge_count_pck = Vec::new();
        huge_count_pck.extend_from_slice(b"AKPK");
        huge_count_pck.extend_from_slice(&100u32.to_le_bytes()); // header size
        huge_count_pck.extend_from_slice(&0u32.to_le_bytes()); // unknown
        huge_count_pck.extend_from_slice(&50u32.to_le_bytes()); // lang map size
        huge_count_pck.extend_from_slice(&0u32.to_le_bytes()); // bank table size
        huge_count_pck.extend_from_slice(&0u32.to_le_bytes()); // stream table size
        huge_count_pck.extend_from_slice(&u32::MAX.to_le_bytes()); // huge lang_count!
        huge_count_pck.extend_from_slice(&[0u8; 128]);

        let huge_pck_path = dir.path().join("huge_count.pck");
        fs::write(&huge_pck_path, &huge_count_pck).expect("write");
        let res3 = handler.analyze(&huge_pck_path);
        assert!(
            matches!(res3, Err(ArchiveError::InvalidFormat(_, _))),
            "Oversized counts must fail closed"
        );

        // 4. Extreme table sizes with u32::MAX and large stream offset overflow checks
        let mut overflow_pck = Vec::new();
        overflow_pck.extend_from_slice(b"AKPK");
        overflow_pck.extend_from_slice(&1000u32.to_le_bytes()); // header size
        overflow_pck.extend_from_slice(&0u32.to_le_bytes()); // unknown
        overflow_pck.extend_from_slice(&u32::MAX.to_le_bytes()); // lang table size = u32::MAX
        overflow_pck.extend_from_slice(&u32::MAX.to_le_bytes()); // bank table size = u32::MAX
        overflow_pck.extend_from_slice(&100u32.to_le_bytes()); // stream table size
        overflow_pck.extend_from_slice(&[0u8; 256]);

        let overflow_pck_path = dir.path().join("overflow_tables.pck");
        fs::write(&overflow_pck_path, &overflow_pck).expect("write");
        let res4 = handler.analyze(&overflow_pck_path);
        assert!(
            matches!(res4, Err(ArchiveError::InvalidFormat(_, _))),
            "Overflowing section sizes must fail closed"
        );
    }
}
