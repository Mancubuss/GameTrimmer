//! Capcom RE Engine Monolithic PAK Handler.
//!
//! Parses Capcom RE Engine `re_chunk_*.pak` files (Magic `KPKA`):
//! - Scans monolithic archive for embedded Audiokinetic Wwise `AKPK` / `.wem` audio blocks.
//! - Locates localized dialogue chunks and tables.
//! - Reports embedded candidates read-only; broad traversal/repacking is not implemented.

use byteorder::{LittleEndian, ReadBytesExt};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::{
    ArchiveAnalysis, ArchiveError, ArchiveHandler, ArchiveType, TrimOptions, TrimResult,
    TrimmableChunk,
};
use crate::formats::wwise::parse_wwise_pck;
use crate::sparse::get_on_disk_size;

pub const RE_PAK_MAGIC: &[u8; 4] = b"KPKA";

pub struct ReEngineHandler;

impl ArchiveHandler for ReEngineHandler {
    fn archive_type(&self) -> ArchiveType {
        ArchiveType::CapcomRePak
    }

    fn analyze(&self, path: &Path) -> Result<ArchiveAnalysis, ArchiveError> {
        let mut file = File::open(path)?;
        let total_size = file.metadata()?.len();
        let on_disk_size = get_on_disk_size(path).unwrap_or(total_size);

        if total_size < 16 {
            return Err(ArchiveError::InvalidFormat(
                ArchiveType::CapcomRePak,
                "File too small for RE Engine PAK header".to_string(),
            ));
        }

        let mut magic = [0u8; 4];
        file.read_exact(&mut magic)?;
        if &magic != RE_PAK_MAGIC {
            return Err(ArchiveError::InvalidFormat(
                ArchiveType::CapcomRePak,
                format!("Expected KPKA magic, found {:?}", magic),
            ));
        }

        let major_version = file.read_u16::<LittleEndian>()?;
        let minor_version = file.read_u16::<LittleEndian>()?;
        let resource_count = file.read_u32::<LittleEndian>()?;

        // Scan for embedded Wwise AKPK chunks inside the RE PAK
        let embedded_akpk_offsets = scan_for_akpk_offsets(&mut file, total_size)?;

        let mut detected_languages = Vec::new();
        let mut trimmable_chunks = Vec::new();
        let mut total_trimmable_bytes = 0u64;

        for &akpk_offset in &embedded_akpk_offsets {
            if let Ok(pck) = parse_wwise_pck(&mut file, akpk_offset, total_size) {
                for name in pck.language_map.values() {
                    if !name.is_empty() && !detected_languages.contains(name) {
                        detected_languages.push(name.clone());
                    }
                }

                for entry in pck.entries {
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

                    let is_sfx = entry.language_id == 0
                        || lang_name.eq_ignore_ascii_case("sfx")
                        || lang_name.eq_ignore_ascii_case("common");

                    let is_language = !is_sfx;
                    let abs_offset = entry.file_offset;

                    // Unlike the outer PAK container (repacking is intentionally
                    // out of scope for RE Engine — proven unworkable), the
                    // embedded Wwise AKPK block is located precisely by
                    // `parse_wwise_pck`, and point-zeroing it in place is the
                    // actual working trim method for RE Engine archives, not a
                    // stand-in for a future repacker.
                    let can_zero_in_place = true;

                    if is_language && can_zero_in_place {
                        total_trimmable_bytes =
                            total_trimmable_bytes.saturating_add(entry.file_size);
                    }

                    trimmable_chunks.push(TrimmableChunk {
                        id: format!("RE_{}_{}", akpk_offset, entry.id),
                        name: format!("Embedded_Wwise_{}.wem", entry.id),
                        offset: abs_offset,
                        length: entry.file_size,
                        is_language,
                        language: Some(lang_name),
                        category: "Capcom RE Audio Chunk".to_string(),
                        can_zero_in_place,
                    });
                }
            }
        }

        let details = format!(
            "Capcom RE Engine PAK (v{}.{}, {} resources, {} embedded AKPK audio blocks)",
            major_version,
            minor_version,
            resource_count,
            embedded_akpk_offsets.len()
        );

        Ok(ArchiveAnalysis {
            archive_type: ArchiveType::CapcomRePak,
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
            "RE Engine PAK mutation is disabled until full resource-table traversal and a validated repack path are available"
                .to_string(),
        ))
    }
}

/// Fast scan for embedded `AKPK` signatures inside an RE PAK.
///
/// Reads resource table entries at offset 16 (up to 256 entries) and seeks directly
/// to check candidate offsets. Falls back to scanning at most the first 2 MB of the file.
fn scan_for_akpk_offsets(
    file: &mut (impl Read + Seek),
    file_len: u64,
) -> Result<Vec<u64>, ArchiveError> {
    if file_len < 16 {
        return Ok(Vec::new());
    }

    let mut offsets = Vec::new();

    file.seek(SeekFrom::Start(8))?;
    let resource_count = file.read_u32::<LittleEndian>().unwrap_or(0);
    let entries_to_check = (resource_count as usize).min(256);

    if entries_to_check > 0 && file_len >= 16 + 24 {
        // Read up to 256 entries (max 12 KB buffer) from offset 16
        let table_bytes = (entries_to_check * 48).min(file_len.saturating_sub(16) as usize);
        let mut table_buf = vec![0u8; table_bytes];
        if file.seek(SeekFrom::Start(16)).is_ok() && file.read_exact(&mut table_buf).is_ok() {
            // 1. Try 48-byte entry stride (RE Engine PAK v4+ standard)
            for i in 0..entries_to_check {
                let entry_pos = i * 48;
                if entry_pos + 16 <= table_buf.len() {
                    let offset = u64::from_le_bytes([
                        table_buf[entry_pos + 8],
                        table_buf[entry_pos + 9],
                        table_buf[entry_pos + 10],
                        table_buf[entry_pos + 11],
                        table_buf[entry_pos + 12],
                        table_buf[entry_pos + 13],
                        table_buf[entry_pos + 14],
                        table_buf[entry_pos + 15],
                    ]);
                    if offset > 0
                        && offset.saturating_add(4) <= file_len
                        && file.seek(SeekFrom::Start(offset)).is_ok()
                    {
                        let mut magic = [0u8; 4];
                        if file.read_exact(&mut magic).is_ok() && &magic == b"AKPK" {
                            offsets.push(offset);
                        }
                    }
                }
            }

            // 2. Try 24-byte entry stride if 48-byte yielded no hits
            if offsets.is_empty() {
                for i in 0..entries_to_check {
                    let entry_pos = i * 24;
                    if entry_pos + 16 <= table_buf.len() {
                        let offset = u64::from_le_bytes([
                            table_buf[entry_pos + 8],
                            table_buf[entry_pos + 9],
                            table_buf[entry_pos + 10],
                            table_buf[entry_pos + 11],
                            table_buf[entry_pos + 12],
                            table_buf[entry_pos + 13],
                            table_buf[entry_pos + 14],
                            table_buf[entry_pos + 15],
                        ]);
                        if offset > 0
                            && offset.saturating_add(4) <= file_len
                            && file.seek(SeekFrom::Start(offset)).is_ok()
                        {
                            let mut magic = [0u8; 4];
                            if file.read_exact(&mut magic).is_ok() && &magic == b"AKPK" {
                                offsets.push(offset);
                            }
                        }
                    }
                }
            }
        }
    }

    // 3. Fallback: scan at most the first 2 MB of the file
    if offsets.is_empty() {
        let max_scan_len = (file_len.min(2 * 1024 * 1024)) as usize;
        if max_scan_len >= 4 {
            file.seek(SeekFrom::Start(0))?;
            let mut scan_buf = vec![0u8; max_scan_len];
            let bytes_read = file.read(&mut scan_buf)?;
            let slice = &scan_buf[..bytes_read];
            let mut i = 0;
            while i + 4 <= slice.len() {
                if slice[i] == b'A'
                    && slice[i + 1] == b'K'
                    && slice[i + 2] == b'P'
                    && slice[i + 3] == b'K'
                {
                    offsets.push(i as u64);
                    i += 4;
                } else {
                    i += 1;
                }
            }
        }
    }

    offsets.sort_unstable();
    offsets.dedup();
    Ok(offsets)
}

/// Helper to generate a synthetic Capcom RE Engine PAK for testing.
pub fn create_synthetic_re_pak(embedded_pck: &[u8]) -> Vec<u8> {
    use byteorder::WriteBytesExt;
    let mut buf = Vec::new();

    buf.extend_from_slice(RE_PAK_MAGIC);
    buf.write_u16::<LittleEndian>(4).unwrap();
    buf.write_u16::<LittleEndian>(0).unwrap();
    buf.write_u32::<LittleEndian>(10).unwrap();
    buf.write_u32::<LittleEndian>(0).unwrap();

    while buf.len() % 4096 != 0 {
        buf.push(0);
    }

    buf.extend_from_slice(embedded_pck);

    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::wwise::create_synthetic_wwise_pck;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_capcom_re_pak_embedded_pck_detection_and_trimming() {
        let dir = tempdir().expect("tempdir");
        let pak_path = dir.path().join("re_chunk_000.pak");

        let languages = vec![
            (0, "SFX"),
            (1, "English(US)"),
            (2, "Japanese"),
            (3, "German"),
        ];
        let streams = vec![
            (201, 0, 8192),
            (202, 1, 12288),
            (203, 2, 16384),
            (204, 3, 16384),
        ];

        let embedded_pck = create_synthetic_wwise_pck(&languages, &streams);
        let pak_bytes = create_synthetic_re_pak(&embedded_pck);
        fs::write(&pak_path, &pak_bytes).expect("write re pak");

        let handler = ReEngineHandler;
        let analysis = handler.analyze(&pak_path).expect("analyze re pak");

        assert_eq!(analysis.archive_type, ArchiveType::CapcomRePak);
        assert!(analysis
            .detected_languages
            .contains(&"Japanese".to_string()));
        assert!(analysis.detected_languages.contains(&"German".to_string()));

        // Verify chunk offset is exact (embedded PCK offset 4096 + stream offset in PCK 4096 = 8192)
        assert_eq!(analysis.trimmable_chunks[0].offset, 8192);

        let options = TrimOptions {
            keep_languages: vec!["english".to_string(), "sfx".to_string()],
            dry_run: false,
            create_snapshot: true,
            force_unsafe: false,
            custom_backup_dir: None,
        };

        // Embedded AKPK entries are point-zeroable regardless of language (RE
        // repacking is out of scope, so this is the working trim method here).
        assert!(analysis
            .trimmable_chunks
            .iter()
            .all(|chunk| chunk.can_zero_in_place));
        // The total counts only the non-SFX language streams: English (202,
        // 12288) + Japanese (203, 16384) + German (204, 16384). SFX (201, 8192)
        // is excluded because it is not localized content.
        assert_eq!(analysis.total_trimmable_bytes, 12288 + 16384 + 16384);
        assert_eq!(analysis.estimated_savings_bytes, 12288 + 16384 + 16384);
        assert!(matches!(
            handler.trim(&pak_path, &options),
            Err(ArchiveError::Unsupported(_))
        ));
    }

    #[test]
    fn capcom_re_pak_embedded_akpk_savings_sum_matches_expected_total() {
        let dir = tempdir().expect("tempdir");
        let pak_path = dir.path().join("re_chunk_001.pak");

        let languages = vec![(0, "SFX"), (1, "English(US)"), (2, "French")];
        let streams = vec![(301, 0, 4096), (302, 1, 8192), (303, 2, 8192)];

        let embedded_pck = create_synthetic_wwise_pck(&languages, &streams);
        let pak_bytes = create_synthetic_re_pak(&embedded_pck);
        fs::write(&pak_path, &pak_bytes).expect("write re pak");

        let handler = ReEngineHandler;
        let analysis = handler.analyze(&pak_path).expect("analyze re pak");

        assert!(!analysis.trimmable_chunks.is_empty());
        assert!(
            analysis
                .trimmable_chunks
                .iter()
                .all(|chunk| chunk.can_zero_in_place),
            "embedded AKPK entries must be zero-in-place-eligible"
        );

        let expected: u64 = 8192 + 8192; // English + French streams, SFX excluded
        assert_ne!(expected, 0);
        assert_eq!(analysis.total_trimmable_bytes, expected);
        assert_eq!(analysis.estimated_savings_bytes, expected);
    }

    #[test]
    fn test_capcom_re_pak_malformed_headers() {
        let dir = tempdir().expect("tempdir");

        // 1. Truncated (< 16 bytes)
        let truncated_path = dir.path().join("short.pak");
        fs::write(&truncated_path, b"KPKA12").expect("write");
        let handler = ReEngineHandler;
        let res = handler.analyze(&truncated_path);
        assert!(res.is_err());

        // 2. Bad magic
        let bad_magic_path = dir.path().join("bad_magic.pak");
        fs::write(&bad_magic_path, b"NOT_KPKA_HEADER_TEST_BYTES_HERE").expect("write");
        let res2 = handler.analyze(&bad_magic_path);
        assert!(res2.is_err());
    }

    #[test]
    fn test_capcom_re_pak_large_file_fast_inspection() {
        let dir = tempdir().expect("tempdir");
        let large_pak_path = dir.path().join("re_chunk_huge.pak");

        let languages = vec![(1, "English(US)"), (2, "Japanese")];
        let streams = vec![(301, 1, 4096), (302, 2, 4096)];
        let embedded_pck = create_synthetic_wwise_pck(&languages, &streams);
        let pak_bytes = create_synthetic_re_pak(&embedded_pck);

        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&large_pak_path)
            .expect("create file");

        // Set file length to 50 GB
        file.set_len(50 * 1024 * 1024 * 1024).expect("set 50GB len");
        drop(file);

        // Write synthetic header with embedded PCK at the beginning
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(&large_pak_path)
            .expect("open");
        file.seek(SeekFrom::Start(0)).expect("seek");
        std::io::Write::write_all(&mut file, &pak_bytes).expect("write header");
        drop(file);

        // What this test is actually about is that inspecting a 50 GB archive
        // does not read 50 GB of it. That used to be asserted as a 10 ms wall
        // clock budget, which under `cargo test -j 4` passed or failed with the
        // machine's load rather than with the code - three people spent a turn
        // each this session working out whose failure it was. Counting the
        // bytes states the claim directly and does not care how busy the disk
        // is.
        //
        // The count also says which path ran, which the clock never did: this
        // fixture reads 2 097 636 bytes, so the capped entry table finds
        // nothing here and the bound is the fallback's own `min(file_len, 2 MB)`
        // sweep plus a few hundred bytes of table probing. 4 MB is that bound
        // with room to spare, and twelve thousand times short of the file.
        let mut f = CountingReader {
            inner: File::open(&large_pak_path).expect("open"),
            read_bytes: 0,
        };
        let offsets = scan_for_akpk_offsets(&mut f, 50 * 1024 * 1024 * 1024).expect("scan");

        assert!(!offsets.is_empty());
        assert!(
            f.read_bytes < 4 * 1024 * 1024,
            "scan_for_akpk_offsets read {} bytes of a 50GB file",
            f.read_bytes
        );

        let start = std::time::Instant::now();
        let handler = ReEngineHandler;
        let analysis = handler.analyze(&large_pak_path).expect("analyze huge pak");
        let duration = start.elapsed();

        assert_eq!(analysis.archive_type, ArchiveType::CapcomRePak);
        assert_eq!(analysis.total_size, 50 * 1024 * 1024 * 1024);
        assert!(analysis
            .detected_languages
            .contains(&"Japanese".to_string()));
        // `analyze` opens its own handle, so its reads cannot be counted from
        // here. This stays a wall clock ceiling, but a smoke one rather than a
        // performance budget: no disk reads 50 GB in five seconds, so it still
        // catches the regression it exists for without failing under load.
        assert!(duration.as_secs() < 5, "50GB analyze took {:?}", duration);
    }

    /// Counts what a scan pulls off the disk. Wraps a `File` for the test
    /// above; `scan_for_akpk_offsets` takes any `Read + Seek` for this reason.
    struct CountingReader<R> {
        inner: R,
        read_bytes: u64,
    }

    impl<R: Read> Read for CountingReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.read_bytes += n as u64;
            Ok(n)
        }
    }

    impl<R: Seek> Seek for CountingReader<R> {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }
}
