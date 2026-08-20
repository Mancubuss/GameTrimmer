//! Electron ASAR Archive Handler.
//!
//! Parses Electron `.asar` archives:
//! - Reads Chromium pickle header and extracts the JSON filesystem tree.
//! - Identifies localized Chrome / Electron locale bundles (`locales/*.pak`, `translations/`).
//! - Calculates payload metadata for read-only inspection.
//! - Mutation stays disabled until validated loose unpacking (`app/`) exists.

use byteorder::{LittleEndian, ReadBytesExt};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::{
    ArchiveAnalysis, ArchiveError, ArchiveHandler, ArchiveType, TrimOptions, TrimResult,
    TrimmableChunk,
};
use crate::sparse::get_on_disk_size;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsarEntry {
    pub size: Option<u64>,
    pub offset: Option<String>,
    pub executable: Option<bool>,
    pub unpacked: Option<bool>,
    pub files: Option<BTreeMap<String, AsarEntry>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsarHeader {
    pub files: BTreeMap<String, AsarEntry>,
}

#[derive(Debug, Clone)]
pub struct FlatAsarFile {
    pub path: String,
    pub size: u64,
    pub absolute_offset: u64,
    pub is_locale: bool,
    pub language: Option<String>,
}

pub struct AsarHandler;

impl ArchiveHandler for AsarHandler {
    fn archive_type(&self) -> ArchiveType {
        ArchiveType::ElectronAsar
    }

    fn analyze(&self, path: &Path) -> Result<ArchiveAnalysis, ArchiveError> {
        let mut file = File::open(path)?;
        let total_size = file.metadata()?.len();
        let on_disk_size = get_on_disk_size(path).unwrap_or(total_size);

        let (header, payload_offset) = parse_asar_header(&mut file, total_size)?;
        let mut flat_files = Vec::new();
        flatten_asar_tree(&header.files, "", payload_offset, &mut flat_files);

        let mut detected_languages = Vec::new();
        let mut trimmable_chunks = Vec::new();

        for entry in &flat_files {
            if let Some(ref lang) = entry.language {
                if !detected_languages.contains(lang) {
                    detected_languages.push(lang.clone());
                }
            }

            trimmable_chunks.push(TrimmableChunk {
                id: entry.path.clone(),
                name: entry.path.clone(),
                offset: entry.absolute_offset,
                length: entry.size,
                is_language: entry.is_locale,
                language: entry.language.clone(),
                category: if entry.is_locale {
                    "Electron / Chromium Locale".to_string()
                } else {
                    "Electron Application File".to_string()
                },
                // ASAR requires unpacking and header/tree rewriting; zeroing its
                // payload in place is intentionally not exposed as a safe action.
                can_zero_in_place: false,
            });
        }

        let details = format!(
            "Electron ASAR ({} files total, {} locales detected, payload offset {} bytes)",
            flat_files.len(),
            detected_languages.len(),
            payload_offset
        );

        Ok(ArchiveAnalysis {
            archive_type: ArchiveType::ElectronAsar,
            path: path.to_path_buf(),
            total_size,
            on_disk_size,
            detected_languages,
            trimmable_chunks,
            total_trimmable_bytes: 0,
            estimated_savings_bytes: 0,
            details,
        })
    }

    fn trim(&self, _path: &Path, _options: &TrimOptions) -> Result<TrimResult, ArchiveError> {
        Err(ArchiveError::Unsupported(
            "Electron ASAR mutation is disabled until validated unpack-to-app support is available"
                .to_string(),
        ))
    }
}

/// Parses the Chromium Pickle / ASAR header and returns `(AsarHeader, payload_start_offset)`.
pub fn parse_asar_header(
    file: &mut File,
    file_len: u64,
) -> Result<(AsarHeader, u64), ArchiveError> {
    if file_len < 16 {
        return Err(ArchiveError::InvalidFormat(
            ArchiveType::ElectronAsar,
            "File too small for ASAR header".to_string(),
        ));
    }

    file.seek(SeekFrom::Start(0))?;

    let _p1 = file.read_u32::<LittleEndian>()?;
    let header_size = file.read_u32::<LittleEndian>()?;
    let _p2 = file.read_u32::<LittleEndian>()?;
    let json_len = file.read_u32::<LittleEndian>()?;

    if (json_len as u64) > file_len
        || (header_size as u64) > file_len
        || json_len > 50_000_000
        || header_size > 50_000_000
    {
        return Err(ArchiveError::InvalidFormat(
            ArchiveType::ElectronAsar,
            format!(
                "Invalid ASAR header size ({}) or json len ({})",
                header_size, json_len
            ),
        ));
    }

    let mut json_bytes = vec![0u8; json_len as usize];
    file.read_exact(&mut json_bytes)?;

    let json_str = String::from_utf8(json_bytes).map_err(|e| {
        ArchiveError::InvalidFormat(
            ArchiveType::ElectronAsar,
            format!("Invalid UTF-8 in ASAR header JSON: {}", e),
        )
    })?;

    let header: AsarHeader = serde_json::from_str(&json_str).map_err(|e| {
        ArchiveError::InvalidFormat(
            ArchiveType::ElectronAsar,
            format!("Failed to parse ASAR header JSON: {}", e),
        )
    })?;

    let payload_offset = (header_size as u64).saturating_add(8);

    Ok((header, payload_offset))
}

/// Flattens the nested ASAR directory tree into flat file entries with absolute offsets.
fn flatten_asar_tree(
    files: &BTreeMap<String, AsarEntry>,
    current_path: &str,
    payload_offset: u64,
    out: &mut Vec<FlatAsarFile>,
) {
    for (name, entry) in files {
        let entry_path = if current_path.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", current_path, name)
        };

        if let Some(ref sub_files) = entry.files {
            flatten_asar_tree(sub_files, &entry_path, payload_offset, out);
        } else if let (Some(size), Some(ref offset_str)) = (entry.size, &entry.offset) {
            let rel_offset = offset_str.parse::<u64>().unwrap_or(0);
            let absolute_offset = payload_offset.saturating_add(rel_offset);

            let (is_loc, lang) = classify_asar_locale(&entry_path);

            out.push(FlatAsarFile {
                path: entry_path,
                size,
                absolute_offset,
                is_locale: is_loc,
                language: lang,
            });
        }
    }
}

/// Classifies if an ASAR file path is a localized file (e.g. `locales/de.pak`).
fn classify_asar_locale(path: &str) -> (bool, Option<String>) {
    let lower = path.to_lowercase();

    if lower.starts_with("locales/") || lower.contains("/locales/") {
        let file_name = lower.rsplit('/').next().unwrap_or(&lower);
        let lang_code = file_name.strip_suffix(".pak").unwrap_or(file_name);

        let lang_name = match lang_code {
            "en-us" | "en-gb" | "en" => "English",
            "de" => "German",
            "fr" => "French",
            "es" | "es-419" => "Spanish",
            "ru" => "Russian",
            "it" => "Italian",
            "ja" => "Japanese",
            "zh-cn" | "zh-tw" | "zh" => "Chinese",
            "ko" => "Korean",
            "pl" => "Polish",
            "pt-br" | "pt-pt" | "pt" => "Portuguese",
            "uk" => "Ukrainian",
            "tr" => "Turkish",
            "nl" => "Dutch",
            "sv" => "Swedish",
            "no" | "nb" => "Norwegian",
            "da" => "Danish",
            "fi" => "Finnish",
            "cs" => "Czech",
            "hu" => "Hungarian",
            other => other,
        };

        let is_loc = !lang_code.starts_with("en");
        return (is_loc, Some(lang_name.to_string()));
    }

    if lower.contains("translations/") || lower.contains("i18n/") {
        return (true, Some("Localization".to_string()));
    }

    (false, None)
}

/// Helper to generate a valid synthetic Electron ASAR archive for unit tests.
pub fn create_synthetic_asar(entries: &[(&str, &[u8])], // (relative_path, payload)
) -> Vec<u8> {
    use byteorder::WriteBytesExt;

    let mut root_files: BTreeMap<String, AsarEntry> = BTreeMap::new();
    let mut payload_data = Vec::new();

    for &(rel_path, data) in entries {
        while payload_data.len() % 4096 != 0 {
            payload_data.push(0);
        }
        let offset_in_payload = payload_data.len();
        let size = data.len() as u64;
        payload_data.extend_from_slice(data);

        let parts: Vec<&str> = rel_path.split('/').collect();
        if parts.len() == 1 {
            root_files.insert(
                parts[0].to_string(),
                AsarEntry {
                    size: Some(size),
                    offset: Some(offset_in_payload.to_string()),
                    executable: None,
                    unpacked: None,
                    files: None,
                },
            );
        } else if parts.len() == 2 {
            let dir_name = parts[0].to_string();
            let file_name = parts[1].to_string();

            let dir_entry = root_files.entry(dir_name).or_insert_with(|| AsarEntry {
                size: None,
                offset: None,
                executable: None,
                unpacked: None,
                files: Some(BTreeMap::new()),
            });

            if let Some(ref mut sub_files) = dir_entry.files {
                sub_files.insert(
                    file_name,
                    AsarEntry {
                        size: Some(size),
                        offset: Some(offset_in_payload.to_string()),
                        executable: None,
                        unpacked: None,
                        files: None,
                    },
                );
            }
        }
    }

    let header_obj = AsarHeader { files: root_files };
    let json_string = serde_json::to_string(&header_obj).unwrap();
    let json_bytes = json_string.as_bytes();

    let json_len = json_bytes.len() as u32;
    let header_size = json_len + 4;

    let mut buf = Vec::new();
    buf.write_u32::<LittleEndian>(4).unwrap();
    buf.write_u32::<LittleEndian>(header_size).unwrap();
    buf.write_u32::<LittleEndian>(header_size + 4).unwrap();
    buf.write_u32::<LittleEndian>(json_len).unwrap();
    buf.extend_from_slice(json_bytes);

    let expected_payload_start = (header_size as usize) + 8;
    while buf.len() < expected_payload_start {
        buf.push(0);
    }

    buf.extend_from_slice(&payload_data);

    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_asar_synthetic_parsing_and_trimming() {
        let dir = tempdir().expect("tempdir");
        let asar_path = dir.path().join("app.asar");

        let en_bytes = vec![0x11u8; 8192];
        let de_bytes = vec![0x22u8; 16384];
        let fr_bytes = vec![0x33u8; 16384];
        let ru_bytes = vec![0x44u8; 16384];

        let entries = vec![
            ("package.json", b"{\"name\":\"game\"}".as_slice()),
            ("locales/en-US.pak", en_bytes.as_slice()),
            ("locales/de.pak", de_bytes.as_slice()),
            ("locales/fr.pak", fr_bytes.as_slice()),
            ("locales/ru.pak", ru_bytes.as_slice()),
        ];

        let asar_bytes = create_synthetic_asar(&entries);
        fs::write(&asar_path, &asar_bytes).expect("write asar");

        let handler = AsarHandler;
        let analysis = handler.analyze(&asar_path).expect("analyze asar");

        assert_eq!(analysis.archive_type, ArchiveType::ElectronAsar);
        assert!(analysis.detected_languages.contains(&"German".to_string()));
        assert!(analysis.detected_languages.contains(&"French".to_string()));
        assert!(analysis.detected_languages.contains(&"Russian".to_string()));

        let options = TrimOptions {
            keep_languages: vec!["english".to_string()],
            dry_run: false,
            create_snapshot: true,
            force_unsafe: false,
            custom_backup_dir: None,
        };

        assert_eq!(analysis.total_trimmable_bytes, 0);
        assert!(analysis
            .trimmable_chunks
            .iter()
            .all(|chunk| !chunk.can_zero_in_place));
        assert!(matches!(
            handler.trim(&asar_path, &options),
            Err(ArchiveError::Unsupported(_))
        ));
    }

    #[test]
    fn test_asar_malformed_and_corrupt_headers() {
        let dir = tempdir().expect("tempdir");

        // 1. Truncated (< 16 bytes)
        let truncated_path = dir.path().join("truncated.asar");
        fs::write(&truncated_path, b"short").expect("write");
        let handler = AsarHandler;
        let res = handler.analyze(&truncated_path);
        assert!(res.is_err());

        // 2. Corrupt JSON
        let corrupt_json_path = dir.path().join("corrupt_json.asar");
        let mut bad_buf = Vec::new();
        bad_buf.extend_from_slice(&4u32.to_le_bytes());
        bad_buf.extend_from_slice(&20u32.to_le_bytes()); // header size
        bad_buf.extend_from_slice(&24u32.to_le_bytes());
        bad_buf.extend_from_slice(&16u32.to_le_bytes()); // json len
        bad_buf.extend_from_slice(b"NOT_A_VALID_JSON");
        fs::write(&corrupt_json_path, &bad_buf).expect("write");

        let res2 = handler.analyze(&corrupt_json_path);
        assert!(res2.is_err(), "Corrupt JSON must return an error cleanly");
    }
}
