//! RAD Game Tools Bink Video (.bik / .bk2) Handler & Micro-Stub Generator.
//!
//! Parses Bink Video headers. Experimental micro-stub fixture generation remains
//! test-only in practice; replacement is disabled until independent RAD/BinkOpen
//! validation exists.
//!
//! Common videos (logos, splash screens, intros) without language markers are
//! safely preserved with 0 trimmable bytes to avoid false duplicate calculations.

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::{
    ArchiveAnalysis, ArchiveError, ArchiveHandler, ArchiveType, TrimOptions, TrimResult,
    TrimmableChunk,
};
use crate::sparse::get_on_disk_size;

#[derive(Debug, Clone)]
pub struct BinkHeader {
    pub signature: [u8; 4],
    pub file_size: u32,
    pub frame_count: u32,
    pub max_frame_size: u32,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub flags: u32,
    pub audio_tracks: u32,
}

pub struct BinkHandler;

impl ArchiveHandler for BinkHandler {
    fn archive_type(&self) -> ArchiveType {
        ArchiveType::BinkVideo
    }

    fn analyze(&self, path: &Path) -> Result<ArchiveAnalysis, ArchiveError> {
        let mut file = File::open(path)?;
        let total_size = file.metadata()?.len();
        let on_disk_size = get_on_disk_size(path).unwrap_or(total_size);

        let header = parse_bink_header(&mut file, total_size)?;

        let sig_str = String::from_utf8_lossy(&header.signature);
        let fps = if header.fps_den > 0 {
            header.fps_num as f32 / header.fps_den as f32
        } else {
            30.0
        };

        let lang_opt = extract_bink_language(path);
        let is_language = lang_opt.is_some();
        let detected_languages = if let Some(ref lang) = lang_opt {
            vec![lang.clone()]
        } else {
            Vec::new()
        };

        let category = if let Some(ref lang) = lang_opt {
            format!("Bink Localized Video ({lang})")
        } else {
            "Bink Common Video".to_string()
        };

        let details = format!(
            "Bink Video [{}] ({}x{}, {} frames, {:.2} FPS, {} audio tracks)",
            sig_str, header.width, header.height, header.frame_count, fps, header.audio_tracks
        );

        let trimmable_chunk = TrimmableChunk {
            id: path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("video")
                .to_string(),
            name: path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("video")
                .to_string(),
            offset: 0,
            length: total_size,
            is_language,
            language: lang_opt,
            category,
            can_zero_in_place: false, // Replacement is disabled pending independent validation.
        };

        Ok(ArchiveAnalysis {
            archive_type: ArchiveType::BinkVideo,
            path: path.to_path_buf(),
            total_size,
            on_disk_size,
            detected_languages,
            trimmable_chunks: vec![trimmable_chunk],
            total_trimmable_bytes: 0,
            estimated_savings_bytes: 0,
            details,
        })
    }

    fn trim(&self, _path: &Path, _options: &TrimOptions) -> Result<TrimResult, ArchiveError> {
        Err(ArchiveError::Unsupported(
            "Bink replacement is disabled until generated stubs pass independent RAD/BinkOpen validation"
                .to_string(),
        ))
    }
}

/// Extracts detected language name from a Bink video file path or filename.
pub fn extract_bink_language(path: &Path) -> Option<String> {
    let path_str = path.to_string_lossy().to_lowercase().replace('\\', "/");
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    // 1. Check directory path components for explicit language folders
    let dir_tokens: &[(&str, &str)] = &[
        ("/french/", "French"),
        ("/fra/", "French"),
        ("/fre/", "French"),
        ("/german/", "German"),
        ("/deutsch/", "German"),
        ("/ger/", "German"),
        ("/deu/", "German"),
        ("/de/", "German"),
        ("/spanish/", "Spanish"),
        ("/espanol/", "Spanish"),
        ("/spa/", "Spanish"),
        ("/es/", "Spanish"),
        ("/italian/", "Italian"),
        ("/italiano/", "Italian"),
        ("/ita/", "Italian"),
        ("/it/", "Italian"),
        ("/russian/", "Russian"),
        ("/rus/", "Russian"),
        ("/ru/", "Russian"),
        ("/japanese/", "Japanese"),
        ("/jpn/", "Japanese"),
        ("/ja/", "Japanese"),
        ("/chinese/", "Chinese"),
        ("/chn/", "Chinese"),
        ("/zho/", "Chinese"),
        ("/zh-cn/", "Chinese"),
        ("/zh-tw/", "Chinese"),
        ("/zh/", "Chinese"),
        ("/korean/", "Korean"),
        ("/kor/", "Korean"),
        ("/ko/", "Korean"),
        ("/polish/", "Polish"),
        ("/pol/", "Polish"),
        ("/pl/", "Polish"),
        ("/portuguese/", "Portuguese"),
        ("/pt-br/", "Portuguese"),
        ("/pt/", "Portuguese"),
        ("/ukrainian/", "Ukrainian"),
        ("/ukr/", "Ukrainian"),
        ("/turkish/", "Turkish"),
        ("/tur/", "Turkish"),
        ("/czech/", "Czech"),
        ("/cze/", "Czech"),
        ("/hungarian/", "Hungarian"),
        ("/hun/", "Hungarian"),
        ("/dutch/", "Dutch"),
        ("/nld/", "Dutch"),
        ("/arabic/", "Arabic"),
        ("/ara/", "Arabic"),
        ("/english/", "English"),
        ("/eng/", "English"),
        ("/en-us/", "English"),
        ("/en-gb/", "English"),
        ("/en/", "English"),
    ];

    for &(token, lang) in dir_tokens {
        if path_str.contains(token) {
            return Some(lang.to_string());
        }
    }

    // 2. Check filename suffixes with delimiters (_ - .)
    let suffix_tokens: &[(&str, &str)] = &[
        ("_fra", "French"),
        ("-fra", "French"),
        ("_fre", "French"),
        ("-fre", "French"),
        ("_fr", "French"),
        ("-fr", "French"),
        ("_french", "French"),
        ("-french", "French"),
        ("_deu", "German"),
        ("-deu", "German"),
        ("_ger", "German"),
        ("-ger", "German"),
        ("_de", "German"),
        ("-de", "German"),
        ("_german", "German"),
        ("-german", "German"),
        ("_deutsch", "German"),
        ("-deutsch", "German"),
        ("_spa", "Spanish"),
        ("-spa", "Spanish"),
        ("_es", "Spanish"),
        ("-es", "Spanish"),
        ("_esn", "Spanish"),
        ("-esn", "Spanish"),
        ("_spanish", "Spanish"),
        ("-spanish", "Spanish"),
        ("_espanol", "Spanish"),
        ("-espanol", "Spanish"),
        ("_ita", "Italian"),
        ("-ita", "Italian"),
        ("_it", "Italian"),
        ("-it", "Italian"),
        ("_italian", "Italian"),
        ("-italian", "Italian"),
        ("_italiano", "Italian"),
        ("-italiano", "Italian"),
        ("_rus", "Russian"),
        ("-rus", "Russian"),
        ("_ru", "Russian"),
        ("-ru", "Russian"),
        ("_russian", "Russian"),
        ("-russian", "Russian"),
        ("_ja", "Japanese"),
        ("-ja", "Japanese"),
        ("_jpn", "Japanese"),
        ("-jpn", "Japanese"),
        ("_japanese", "Japanese"),
        ("-japanese", "Japanese"),
        ("_zh", "Chinese"),
        ("-zh", "Chinese"),
        ("_zho", "Chinese"),
        ("-zho", "Chinese"),
        ("_chi", "Chinese"),
        ("-chi", "Chinese"),
        ("_chn", "Chinese"),
        ("-chn", "Chinese"),
        ("_chinese", "Chinese"),
        ("-chinese", "Chinese"),
        ("_ko", "Korean"),
        ("-ko", "Korean"),
        ("_kor", "Korean"),
        ("-kor", "Korean"),
        ("_korean", "Korean"),
        ("-korean", "Korean"),
        ("_pl", "Polish"),
        ("-pl", "Polish"),
        ("_pol", "Polish"),
        ("-pol", "Polish"),
        ("_polish", "Polish"),
        ("-polish", "Polish"),
        ("_pt", "Portuguese"),
        ("-pt", "Portuguese"),
        ("_por", "Portuguese"),
        ("-por", "Portuguese"),
        ("_ptbr", "Portuguese"),
        ("-ptbr", "Portuguese"),
        ("_portuguese", "Portuguese"),
        ("-portuguese", "Portuguese"),
        ("_uk", "Ukrainian"),
        ("-uk", "Ukrainian"),
        ("_ukr", "Ukrainian"),
        ("-ukr", "Ukrainian"),
        ("_ukrainian", "Ukrainian"),
        ("-ukrainian", "Ukrainian"),
        ("_tr", "Turkish"),
        ("-tr", "Turkish"),
        ("_tur", "Turkish"),
        ("-tur", "Turkish"),
        ("_turkish", "Turkish"),
        ("-turkish", "Turkish"),
        ("_cs", "Czech"),
        ("-cs", "Czech"),
        ("_cz", "Czech"),
        ("-cz", "Czech"),
        ("_cze", "Czech"),
        ("-cze", "Czech"),
        ("_czech", "Czech"),
        ("-czech", "Czech"),
        ("_hu", "Hungarian"),
        ("-hu", "Hungarian"),
        ("_hun", "Hungarian"),
        ("-hun", "Hungarian"),
        ("_hungarian", "Hungarian"),
        ("-hungarian", "Hungarian"),
        ("_nl", "Dutch"),
        ("-nl", "Dutch"),
        ("_nld", "Dutch"),
        ("-nld", "Dutch"),
        ("_dutch", "Dutch"),
        ("-dutch", "Dutch"),
        ("_ar", "Arabic"),
        ("-ar", "Arabic"),
        ("_ara", "Arabic"),
        ("-ara", "Arabic"),
        ("_arabic", "Arabic"),
        ("-arabic", "Arabic"),
        ("_eng", "English"),
        ("-eng", "English"),
        ("_en", "English"),
        ("-en", "English"),
        ("_english", "English"),
        ("-english", "English"),
        ("_enus", "English"),
        ("-enus", "English"),
        ("_engb", "English"),
        ("-engb", "English"),
    ];

    for &(suffix, lang) in suffix_tokens {
        if stem.ends_with(suffix) {
            return Some(lang.to_string());
        }
    }

    // 3. Tokenize stem by separators and match exact standalone words
    let tokens: Vec<&str> = stem
        .split(['_', '-', '.', '(', ')', ' '])
        .filter(|t| !t.is_empty())
        .collect();

    for &tok in &tokens {
        let matched = match tok {
            "fra" | "fre" | "french" | "francais" => Some("French"),
            "deu" | "ger" | "german" | "deutsch" => Some("German"),
            "spa" | "esn" | "spanish" | "espanol" | "es419" => Some("Spanish"),
            "ita" | "italian" | "italiano" => Some("Italian"),
            "rus" | "russian" => Some("Russian"),
            "jpn" | "japanese" => Some("Japanese"),
            "zho" | "chi" | "chn" | "chinese" | "zhcn" | "zhtw" => Some("Chinese"),
            "kor" | "korean" => Some("Korean"),
            "pol" | "polish" => Some("Polish"),
            "por" | "ptbr" | "portuguese" | "brazilian" => Some("Portuguese"),
            "ukr" | "ukrainian" => Some("Ukrainian"),
            "tur" | "turkish" => Some("Turkish"),
            "cze" | "czech" => Some("Czech"),
            "hun" | "hungarian" => Some("Hungarian"),
            "nld" | "dutch" => Some("Dutch"),
            "ara" | "arabic" => Some("Arabic"),
            "eng" | "english" | "enus" | "engb" => Some("English"),
            _ => None,
        };
        if let Some(lang) = matched {
            return Some(lang.to_string());
        }
    }

    None
}

/// Parses the Bink video header from a file.
pub fn parse_bink_header(file: &mut File, file_len: u64) -> Result<BinkHeader, ArchiveError> {
    if file_len < 44 {
        return Err(ArchiveError::InvalidFormat(
            ArchiveType::BinkVideo,
            "File too small for Bink header".to_string(),
        ));
    }

    file.seek(SeekFrom::Start(0))?;

    let mut signature = [0u8; 4];
    file.read_exact(&mut signature)?;

    let is_valid_bik = signature.starts_with(b"BIK")
        || (signature[0] == b'K' && signature[1] == b'B' && signature[2] == b'2');

    if !is_valid_bik {
        return Err(ArchiveError::InvalidFormat(
            ArchiveType::BinkVideo,
            format!("Invalid Bink signature: {:?}", signature),
        ));
    }

    let file_size = file.read_u32::<LittleEndian>()?;
    let frame_count = file.read_u32::<LittleEndian>()?;
    let max_frame_size = file.read_u32::<LittleEndian>()?;
    let _frame_count_dup = file.read_u32::<LittleEndian>()?;
    let width = file.read_u32::<LittleEndian>()?;
    let height = file.read_u32::<LittleEndian>()?;
    let fps_num = file.read_u32::<LittleEndian>()?;
    let fps_den = file.read_u32::<LittleEndian>()?;
    let flags = file.read_u32::<LittleEndian>()?;
    let audio_tracks = file.read_u32::<LittleEndian>()?;

    Ok(BinkHeader {
        signature,
        file_size,
        frame_count,
        max_frame_size,
        width,
        height,
        fps_num,
        fps_den,
        flags,
        audio_tracks,
    })
}

/// Generates an experimental 1-frame-like Bink fixture (~1 KiB).
///
/// This output has not passed independent `BinkOpen()`/decoder validation and
/// must not be used as a production replacement.
pub fn generate_bink_micro_stub(signature: &[u8; 4], width: u32, height: u32, fps: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(512);

    // Signature: 'BIKi' or 'KB2i'
    buf.extend_from_slice(signature);

    // Placeholders for header fields
    let file_size_pos = buf.len();
    buf.write_u32::<LittleEndian>(0).unwrap(); // file_size - 8
    buf.write_u32::<LittleEndian>(1).unwrap(); // frame_count = 1
    buf.write_u32::<LittleEndian>(64).unwrap(); // max_frame_size = 64
    buf.write_u32::<LittleEndian>(1).unwrap(); // frame_count dup
    buf.write_u32::<LittleEndian>(width.max(16)).unwrap();
    buf.write_u32::<LittleEndian>(height.max(16)).unwrap();
    buf.write_u32::<LittleEndian>(fps.saturating_mul(1000))
        .unwrap(); // fps_num
    buf.write_u32::<LittleEndian>(1000).unwrap(); // fps_den
    buf.write_u32::<LittleEndian>(0x00080000).unwrap(); // flags (standard 24/32-bit video)
    buf.write_u32::<LittleEndian>(0).unwrap(); // audio_tracks = 0

    // Frame size table: 1 entry (offset/length with keyframe bit set)
    buf.write_u32::<LittleEndian>(64 | 1).unwrap(); // 64 bytes + keyframe bit

    // Seek table / frame index
    buf.write_u32::<LittleEndian>(buf.len() as u32 + 4).unwrap();

    // 1-frame dummy payload (blank compressed frame)
    let payload = vec![0u8; 64];
    buf.extend_from_slice(&payload);

    // Fix up file_size field (file_size - 8 as expected by Bink container)
    let total_len = buf.len() as u32;
    let size_val = total_len.saturating_sub(8);
    buf[file_size_pos..file_size_pos + 4].copy_from_slice(&size_val.to_le_bytes());

    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_bink_micro_stub_generation_and_parsing() {
        let dir = tempdir().expect("tempdir");
        let bik_path = dir.path().join("intro_fra.bik");

        let stub_bytes = generate_bink_micro_stub(b"BIKi", 1920, 1080, 30);
        fs::write(&bik_path, &stub_bytes).expect("write stub");

        let handler = BinkHandler;
        let analysis = handler.analyze(&bik_path).expect("analyze bink");

        assert_eq!(analysis.archive_type, ArchiveType::BinkVideo);
        assert_eq!(analysis.detected_languages, vec!["French".to_string()]);
        assert_eq!(analysis.trimmable_chunks.len(), 1);
        assert!(analysis.trimmable_chunks[0].is_language);

        let mut file = File::open(&bik_path).expect("open bik");
        let header = parse_bink_header(&mut file, stub_bytes.len() as u64).expect("parse header");
        assert_eq!(&header.signature, b"BIKi");
        assert_eq!(header.frame_count, 1);
        assert_eq!(header.width, 1920);
        assert_eq!(header.height, 1080);
    }

    #[test]
    fn test_bink_common_video_preserves_zero_savings() {
        let dir = tempdir().expect("tempdir");
        let bik_path = dir.path().join("unreal_engine_logo.bik");

        let stub_bytes = generate_bink_micro_stub(b"BIKi", 1920, 1080, 30);
        fs::write(&bik_path, &stub_bytes).expect("write stub");

        let handler = BinkHandler;
        let analysis = handler.analyze(&bik_path).expect("analyze bink");

        // Common video without language markers should have 0 trimmable bytes
        assert_eq!(analysis.total_trimmable_bytes, 0);
        assert!(analysis.detected_languages.is_empty());
        assert!(!analysis.trimmable_chunks[0].is_language);

        // No Bink file is modified before independent decoder validation exists.
        let options = TrimOptions::default();
        assert!(matches!(
            handler.trim(&bik_path, &options),
            Err(ArchiveError::Unsupported(_))
        ));
    }

    #[test]
    fn test_bink_language_detection_patterns() {
        assert_eq!(
            extract_bink_language(Path::new("cinematic_intro_fra.bik")),
            Some("French".to_string())
        );
        assert_eq!(
            extract_bink_language(Path::new("scene_deu.bk2")),
            Some("German".to_string())
        );
        assert_eq!(
            extract_bink_language(Path::new("cutscene_ita.bik")),
            Some("Italian".to_string())
        );
        assert_eq!(
            extract_bink_language(Path::new("movie_spa.bik")),
            Some("Spanish".to_string())
        );
        assert_eq!(
            extract_bink_language(Path::new("dialogue_rus.bik")),
            Some("Russian".to_string())
        );
        assert_eq!(
            extract_bink_language(Path::new("intro_ja.bik")),
            Some("Japanese".to_string())
        );
        assert_eq!(
            extract_bink_language(Path::new("outro_zh.bik")),
            Some("Chinese".to_string())
        );
        assert_eq!(
            extract_bink_language(Path::new("trailer_pl.bik")),
            Some("Polish".to_string())
        );
        assert_eq!(
            extract_bink_language(Path::new("cutscene_kor.bik")),
            Some("Korean".to_string())
        );
        assert_eq!(
            extract_bink_language(Path::new("intro_eng.bik")),
            Some("English".to_string())
        );
        assert_eq!(
            extract_bink_language(Path::new("Game/Movies/German/intro.bk2")),
            Some("German".to_string())
        );

        // Common videos that MUST NOT match
        assert_eq!(extract_bink_language(Path::new("bink_logo.bik")), None);
        assert_eq!(extract_bink_language(Path::new("intro.bik")), None);
        assert_eq!(extract_bink_language(Path::new("credits.bk2")), None);
        assert_eq!(extract_bink_language(Path::new("nvidia_splash.bik")), None);
    }

    #[test]
    fn test_bink_malformed_and_bk2_signature() {
        let dir = tempdir().expect("tempdir");

        // 1. Truncated (< 44 bytes)
        let truncated_path = dir.path().join("short_fra.bik");
        fs::write(&truncated_path, b"BIKi1234").expect("write");
        let handler = BinkHandler;
        let res = handler.analyze(&truncated_path);
        assert!(res.is_err());

        // 2. Bad signature
        let bad_sig_path = dir.path().join("bad_sig_fra.bik");
        let mut bad_bytes = vec![0u8; 128];
        bad_bytes[0..4].copy_from_slice(b"WAVE");
        fs::write(&bad_sig_path, &bad_bytes).expect("write");
        let res2 = handler.analyze(&bad_sig_path);
        assert!(res2.is_err());

        // 3. Bink 2 (KB2k) is detected, but destructive replacement remains disabled.
        let bk2_path = dir.path().join("movie_fre.bk2");
        let bk2_stub = generate_bink_micro_stub(b"KB2k", 1280, 720, 60);
        fs::write(&bk2_path, &bk2_stub).expect("write bk2");

        let options = TrimOptions::default();
        assert!(matches!(
            handler.trim(&bk2_path, &options),
            Err(ArchiveError::Unsupported(_))
        ));
        assert_eq!(&fs::read(&bk2_path).expect("read")[0..4], b"KB2k");
    }
}
