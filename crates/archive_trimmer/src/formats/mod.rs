//! Game Archive Format Parsers & Trimming Engine.
//!
//! Provides conservative inspection and analysis for monolithic game archives.
//! Destructive handlers are disabled until full payload rollback and independent
//! format validation are available.
//! - Wwise PCK / BNK (`.pck`, `.bnk`)
//! - Unreal Engine 4 & 5 PAK (`.pak`)
//! - Electron ASAR (`.asar`)
//! - Bink Video (`.bik`, `.bk2`)
//! - Capcom RE Engine PAK (`re_chunk_*.pak` / `KPKA`)
//! - Unity AssetBundle / UnityFS (`.bundle`, `.unity3d`, `*.assets`)

pub mod asar;
pub mod bink;
pub mod re_engine;
pub mod ue_pak;
pub mod unity;
pub mod wwise;

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::sparse::SparseError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchiveType {
    WwisePck,
    WwiseBnk,
    UnrealPak,
    CapcomRePak,
    ElectronAsar,
    UnityAssetBundle,
    BinkVideo,
}

impl std::fmt::Display for ArchiveType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArchiveType::WwisePck => write!(f, "Audiokinetic Wwise PCK"),
            ArchiveType::WwiseBnk => write!(f, "Audiokinetic Wwise SoundBank (BNK)"),
            ArchiveType::UnrealPak => write!(f, "Unreal Engine PAK"),
            ArchiveType::CapcomRePak => write!(f, "Capcom RE Engine PAK (KPKA)"),
            ArchiveType::ElectronAsar => write!(f, "Electron ASAR Archive"),
            ArchiveType::UnityAssetBundle => write!(f, "Unity AssetBundle (UnityFS)"),
            ArchiveType::BinkVideo => write!(f, "RAD Game Tools Bink Video"),
        }
    }
}

impl ArchiveType {
    /// Whether this format packs many separately-addressable assets into one
    /// file. Such a container must be trimmed in place: deleting it whole
    /// would take every asset in it, including the ones the user never asked
    /// to lose. A format that is a single asset in its own right has no such
    /// problem and may be removed like any other file.
    ///
    /// The match is exhaustive on purpose - a new format has to state which
    /// side of that line it falls on rather than inherit an answer.
    pub fn is_multi_asset_container(self) -> bool {
        match self {
            ArchiveType::WwisePck
            | ArchiveType::WwiseBnk
            | ArchiveType::UnrealPak
            | ArchiveType::CapcomRePak
            | ArchiveType::ElectronAsar
            | ArchiveType::UnityAssetBundle => true,
            // One video, not a container of separable assets. The Bink
            // handler reports zero trimmable bytes for it and its replacement
            // path is disabled (see `bink.rs`), so treating it as a container
            // only ever had the effect of pinning intro videos in place - the
            // one thing the intro rules exist to remove. See GT-204.
            ArchiveType::BinkVideo => false,
        }
    }
}

#[derive(Error, Debug)]
pub enum ArchiveError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Sparse zeroing error: {0}")]
    Sparse(#[from] SparseError),
    #[error("Corrupt or invalid archive format for {0}: {1}")]
    InvalidFormat(ArchiveType, String),
    #[error("Archive is encrypted (e.g. Unreal Pak AES) and cannot be modified without key")]
    Encrypted(String),
    #[error("Unsupported version or feature: {0}")]
    Unsupported(String),
}

/// A trimmable chunk or embedded file inside an archive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrimmableChunk {
    pub id: String,
    pub name: String,
    pub offset: u64,
    pub length: u64,
    pub is_language: bool,
    pub language: Option<String>,
    pub category: String,
    pub can_zero_in_place: bool,
}

/// Detailed analysis of an archive file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveAnalysis {
    pub archive_type: ArchiveType,
    pub path: PathBuf,
    pub total_size: u64,
    pub on_disk_size: u64,
    pub detected_languages: Vec<String>,
    pub trimmable_chunks: Vec<TrimmableChunk>,
    pub total_trimmable_bytes: u64,
    pub estimated_savings_bytes: u64,
    pub details: String,
}

/// Options controlling the trimming operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrimOptions {
    /// Languages to KEEP (e.g. `["en", "english", "sfx", "common"]`).
    /// All other localized streams/chunks will be targeted for trimming.
    pub keep_languages: Vec<String>,
    /// If true, performs dry run without modifying any files.
    pub dry_run: bool,
    /// Legacy compatibility field. Header-only snapshots are not rollback and
    /// destructive handlers currently reject trim requests.
    pub create_snapshot: bool,
    /// Legacy compatibility field. `true` is explicitly rejected and never
    /// bypasses anti-cheat protection.
    pub force_unsafe: bool,
    /// Legacy custom directory for header-only diagnostic snapshots.
    pub custom_backup_dir: Option<PathBuf>,
}

impl Default for TrimOptions {
    fn default() -> Self {
        Self {
            keep_languages: vec![
                "english".to_string(),
                "en".to_string(),
                "en-us".to_string(),
                "en-gb".to_string(),
                "sfx".to_string(),
                "common".to_string(),
            ],
            dry_run: false,
            create_snapshot: true,
            force_unsafe: false,
            custom_backup_dir: None,
        }
    }
}

/// Result of an archive trimming operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrimResult {
    pub archive_path: PathBuf,
    pub archive_type: ArchiveType,
    pub original_size: u64,
    pub original_on_disk_size: u64,
    pub new_on_disk_size: u64,
    pub chunks_trimmed: usize,
    pub logical_bytes_trimmed: u64,
    pub physical_bytes_freed: u64,
    pub snapshot_path: Option<PathBuf>,
    pub is_dry_run: bool,
}

/// Trait implemented by all archive format handlers.
pub trait ArchiveHandler {
    fn archive_type(&self) -> ArchiveType;
    fn analyze(&self, path: &Path) -> Result<ArchiveAnalysis, ArchiveError>;
    fn trim(&self, path: &Path, options: &TrimOptions) -> Result<TrimResult, ArchiveError>;
}

/// Read-only handler for formats whose detector exists but whose destructive
/// implementation has not met the format-specific validation contract yet.
struct UnsupportedArchiveHandler {
    archive_type: ArchiveType,
    reason: &'static str,
}

impl ArchiveHandler for UnsupportedArchiveHandler {
    fn archive_type(&self) -> ArchiveType {
        self.archive_type
    }

    fn analyze(&self, _path: &Path) -> Result<ArchiveAnalysis, ArchiveError> {
        Err(ArchiveError::Unsupported(self.reason.to_string()))
    }

    fn trim(&self, _path: &Path, _options: &TrimOptions) -> Result<TrimResult, ArchiveError> {
        Err(ArchiveError::Unsupported(self.reason.to_string()))
    }
}

/// Canonicalizes language names/codes into normalized language families.
///
/// Uses strict word-boundary and exact token matching to prevent false positives
/// on English words (e.g. "engine" vs "eng", "green"/"open" vs "en", "freeze" vs "fre", "space" vs "spa").
pub fn canonical_language(lang: &str) -> &'static str {
    let lower = lang.to_lowercase();
    let clean = lower.replace(['_', '-', '(', ')', ' ', '.'], "");

    if clean.is_empty() {
        return "other";
    }

    // 1. English
    if clean == "en"
        || clean == "eng"
        || clean == "enus"
        || clean == "engb"
        || clean == "enuk"
        || clean == "enau"
        || clean == "enca"
        || clean.starts_with("english")
    {
        return "english";
    }

    // 2. French
    if clean == "fr"
        || clean == "fra"
        || clean == "fre"
        || clean.starts_with("french")
        || clean.starts_with("francais")
    {
        return "french";
    }

    // 3. German
    if clean == "de"
        || clean == "ger"
        || clean == "deu"
        || clean.starts_with("german")
        || clean.starts_with("deutsch")
    {
        return "german";
    }

    // 4. Spanish
    if clean == "es"
        || clean == "spa"
        || clean == "esn"
        || clean == "es419"
        || clean.starts_with("spanish")
        || clean.starts_with("espanol")
    {
        return "spanish";
    }

    // 5. Italian
    if clean == "it"
        || clean == "ita"
        || clean.starts_with("italian")
        || clean.starts_with("italiano")
    {
        return "italian";
    }

    // 6. Russian
    if clean == "ru" || clean == "rus" || clean.starts_with("russian") {
        return "russian";
    }

    // 7. Japanese
    if clean == "ja"
        || clean == "jpn"
        || clean.starts_with("japanese")
        || clean.starts_with("japan")
    {
        return "japanese";
    }

    // 8. Chinese
    if clean == "zh"
        || clean == "zho"
        || clean == "chi"
        || clean == "chn"
        || clean == "zhcn"
        || clean == "zhtw"
        || clean == "zhhans"
        || clean == "zhhant"
        || clean.starts_with("chinese")
    {
        return "chinese";
    }

    // 9. Korean
    if clean == "ko" || clean == "kor" || clean.starts_with("korean") {
        return "korean";
    }

    // 10. Polish
    if clean == "pl" || clean == "pol" || clean.starts_with("polish") {
        return "polish";
    }

    // 11. Portuguese
    if clean == "pt"
        || clean == "por"
        || clean == "ptbr"
        || clean.starts_with("portuguese")
        || clean.starts_with("brazilian")
    {
        return "portuguese";
    }

    // 12. Ukrainian
    if clean == "uk" || clean == "ukr" || clean.starts_with("ukrainian") {
        return "ukrainian";
    }

    // 13. Turkish
    if clean == "tr" || clean == "tur" || clean.starts_with("turkish") {
        return "turkish";
    }

    // 14. Czech
    if clean == "cz" || clean == "cs" || clean == "cze" || clean.starts_with("czech") {
        return "czech";
    }

    // 15. Hungarian
    if clean == "hu" || clean == "hun" || clean.starts_with("hungarian") {
        return "hungarian";
    }

    // 16. Dutch
    if clean == "nl" || clean == "nld" || clean.starts_with("dutch") {
        return "dutch";
    }

    // 17. Arabic
    if clean == "ar" || clean == "ara" || clean.starts_with("arabic") {
        return "arabic";
    }

    // 18. Special common audio categories
    if clean == "sfx" || clean == "soundfx" || clean.starts_with("sfx") {
        return "sfx";
    }
    if clean == "common" || clean.starts_with("common") {
        return "common";
    }
    if clean == "voices" || clean == "voice" || clean.starts_with("voices") {
        return "voices";
    }

    "other"
}

/// Returns whether `language` is a recognized, safely classifiable language
/// or one of the always-kept common audio categories.
pub fn is_known_language(language: &str) -> bool {
    canonical_language(language) != "other"
}

/// Language tags as a filename spells them, shared by the external
/// single-language test and its corpus test. Module-level so both see one
/// list: two copies of a table this long drift the moment a tag is added.
const LANG_CODES: &[&str] = &[
    "en", "eng", "us", "gb", "fra", "fre", "fr", "ger", "deu", "de", "spa", "esn", "es", "es419",
    "ita", "it", "rus", "ru", "jpn", "ja", "jap", "zho", "chi", "chn", "zh", "zhcn", "zhtw", "kor",
    "ko", "pol", "pl", "por", "pt", "ptbr", "bra", "ukr", "uk", "tur", "tr", "cze", "cs", "cz",
    "hun", "hu", "nld", "nl", "ara", "ar", "dan", "da", "fin", "fi", "nor", "no", "swe", "sv",
    "ell", "el", "gre", "tha", "th", "vie", "vi", "ind", "id",
];

const LANG_NAMES: &[&str] = &[
    "english",
    "french",
    "german",
    "spanish",
    "italian",
    "russian",
    "japanese",
    "chinese",
    "korean",
    "polish",
    "portuguese",
    "ukrainian",
    "turkish",
    "czech",
    "hungarian",
    "dutch",
    "arabic",
    "danish",
    "finnish",
    "norwegian",
    "swedish",
    "greek",
    "thai",
    "vietnamese",
    "indonesian",
    "francais",
    "deutsch",
    "espanol",
    "italiano",
    "brazilian",
];

/// Whether `stem` ends with `tag` preceded by `_` or `-` (`sounds_fra` for
/// `fra`, `vo-german` for `german`).
///
/// Written as a byte check rather than `stem.ends_with(&format!("_{tag}"))`:
/// the caller runs it against ~100 language tags, twice, for every file in
/// every game, and the formatted version allocated a `String` per tag per
/// file - some 400 allocations to answer "is this an ordinary .exe". That was
/// the single largest cost in a full scan.
fn ends_with_separated_tag(stem: &str, tag: &str) -> bool {
    let Some(head) = stem.strip_suffix(tag) else {
        return false;
    };
    matches!(head.as_bytes().last(), Some(b'_') | Some(b'-'))
}

/// Determines if a file path points to a standalone external single-language file.
///
/// These whole-file localizations are deleted as whole files by GameTrimmer core;
/// archive-trimmer is exclusively for monolithic archives where deleting the whole file would break the game.
///
/// Matches paths such as:
/// - `*/locales/*.pak`, `*/locales/*.json`, `locales/*`, `*/locale/*`
/// - `*_fra.pck`, `*_ger.pck`, `*_rus.pck`, `*_spa.pck`, `*_deu.pck`, `*_ita.pck`
/// - `*German.pck`, `*French.pck`, `*Spanish.pck`, `*Russian.pck`
/// - `*/Localization/Spanish.pak`, `*/Sound/Russian.pck`, `*/Audio/de.pck`, `*/Audio/German.pck`
///
/// Returns `false` for monolithic archives containing internal multi-language data:
/// - `VO_AMICIA_MEDIA.PC.PCK`, `VO_D1_MEDIA.PC.PCK`
/// - `re_chunk_000.pak`, `app.asar`, `pakchunk0.pak`, `voices.pck`, `soundbanks.pck`, `audio.pck`
pub fn is_external_single_language_file(path: &str) -> bool {
    // Separator normalization and ASCII-only lowering in one pass, one
    // allocation instead of two. Every tag compared below is ASCII, so a
    // non-ASCII character left as it is cannot change any answer - while
    // `str::to_lowercase` walks the Unicode tables for every character of
    // every path in the library.
    let lower: String = path
        .chars()
        .map(|c| {
            if c == '\\' {
                '/'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();

    // 1. Locales directory match (e.g. 3DMark/bin/x64/locales/ar.pak, locales/en-US.pak, locales/fr.json)
    if lower.starts_with("locales/")
        || lower.starts_with("locale/")
        || lower.contains("/locales/")
        || lower.contains("/locale/")
    {
        return true;
    }

    // Extract filename and stem
    let filename = lower.rsplit('/').next().unwrap_or(&lower);
    let stem = match filename.rsplit_once('.') {
        Some((s, _)) => s,
        None => filename,
    };

    // Strip common secondary platform tags (e.g., sounds_fra.pc.pck -> sounds_fra)
    let effective_stem = stem
        .strip_suffix(".pc")
        .or_else(|| stem.strip_suffix(".win"))
        .or_else(|| stem.strip_suffix(".windows"))
        .or_else(|| stem.strip_suffix(".ps4"))
        .or_else(|| stem.strip_suffix(".xbox"))
        .unwrap_or(stem);

    for s in [stem, effective_stem] {
        // 2. Exact match on stem (e.g. ar.pak, de.pak, spanish.pak, russian.pck, en-us.pak, zh-cn.pak)
        let clean_s = s.replace(['-', '_'], "");
        if LANG_CODES.contains(&s)
            || LANG_CODES.contains(&clean_s.as_str())
            || LANG_NAMES.contains(&s)
            || LANG_NAMES.contains(&clean_s.as_str())
        {
            return true;
        }

        // 3. Suffix with underscore / hyphen (e.g. sounds_fra.pck, vo_german.pak, speech_rus.pck, audio_de.pck)
        if LANG_CODES
            .iter()
            .chain(LANG_NAMES.iter())
            .any(|tag| ends_with_separated_tag(s, tag))
        {
            return true;
        }

        // 4. Suffix without separator for language names (e.g. *German.pck, *French.pck, *Spanish.pck, *Russian.pck)
        for name in LANG_NAMES {
            if s.ends_with(name) {
                return true;
            }
        }
    }

    // 5. Parent directory is a dedicated localization/audio/language folder and stem is a language code or name
    // e.g. Localization/Spanish.pak, Sound/Russian.pck, Audio/de.pck, Audio/German.pck
    //
    // Outside the stem loop, and last: the folder test is fourteen substring
    // searches over the whole path and does not depend on the stem, so running
    // it per stem scanned every path twice for the same answer. The checks
    // above are pure "does anything match" tests, so the order between them
    // and this one cannot change the result.
    if [stem, effective_stem]
        .iter()
        .any(|s| LANG_CODES.contains(s) || LANG_NAMES.contains(s))
    {
        let is_loc_folder = lower.contains("/localization/")
            || lower.contains("/localisation/")
            || lower.contains("/languages/")
            || lower.contains("/language/")
            || lower.contains("/lang/")
            || lower.contains("/audio/")
            || lower.contains("/sound/")
            || lower.contains("/sounds/")
            || lower.contains("/speech/")
            || lower.contains("/dialogue/")
            || lower.contains("/dialogues/")
            || lower.contains("/vo/")
            || lower.contains("/voice/")
            || lower.contains("/voices/");
        if is_loc_folder {
            return true;
        }
    }

    false
}

/// Helper to normalize language names for matching against keep-lists.
pub fn is_language_kept(language: &str, keep_languages: &[String]) -> bool {
    let canon_lang = canonical_language(language);
    if canon_lang == "sfx" || canon_lang == "common" || canon_lang == "voices" {
        return true;
    }

    let clean_lang = language
        .to_lowercase()
        .replace(['_', '-', '(', ')', ' ', '.'], "");

    for keep in keep_languages {
        let canon_keep = canonical_language(keep);
        if canon_keep != "other" && canon_keep == canon_lang {
            return true;
        }

        let clean_keep = keep
            .to_lowercase()
            .replace(['_', '-', '(', ')', ' ', '.'], "");
        if !clean_lang.is_empty() && clean_lang == clean_keep {
            return true;
        }
    }
    false
}

/// Automatic detector for archive formats based on magic bytes, footer trailers, and extensions.
pub struct FormatDetector;

impl FormatDetector {
    /// Detects archive type from a file path.
    pub fn detect_file(path: &Path) -> Result<Option<ArchiveType>, std::io::Error> {
        let mut file = File::open(path)?;

        let file_len = file.metadata()?.len();
        if file_len < 16 {
            return Ok(None);
        }

        // Read first 64 bytes for header magic
        let mut header = [0u8; 64];
        let bytes_read = file.read(&mut header)?;
        let header_slice = &header[..bytes_read];

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        // 1. Wwise PCK: Magic 'AKPK'
        if header_slice.starts_with(b"AKPK") {
            return Ok(Some(ArchiveType::WwisePck));
        }

        // 2. Wwise BNK: Magic 'BKHD'
        if header_slice.starts_with(b"BKHD") {
            return Ok(Some(ArchiveType::WwiseBnk));
        }

        // 3. Capcom RE Engine PAK: Magic 'KPKA'
        if header_slice.starts_with(b"KPKA") {
            return Ok(Some(ArchiveType::CapcomRePak));
        }

        // 4. Bink 2 only: signatures 'KB2a'..'KB2n'.
        //
        // Bink 1 ('BIKb', 'BIKf', 'BIKg', 'BIKh', 'BIKi') used to be claimed
        // here too, by magic and by extension. It is a plain video, not a
        // container of separable language streams - the Bink handler reports
        // zero trimmable bytes for it and refuses to trim it - so claiming it
        // only had the effect of blocking whole-file deletion, which is exactly
        // what the intro rules need to do to it. Bink 2 stays claimed until a
        // stub for it has been validated against a real game; see GT-204.
        if header_slice.len() >= 4 {
            let sig = &header_slice[0..4];
            if (sig[0] == b'K' && sig[1] == b'B' && sig[2] == b'2') || ext == "bk2" {
                return Ok(Some(ArchiveType::BinkVideo));
            }
        }

        // 5. Unity AssetBundle: 'UnityFS', 'UnityRaw', 'UnityWeb'
        if header_slice.starts_with(b"UnityFS")
            || header_slice.starts_with(b"UnityRaw")
            || header_slice.starts_with(b"UnityWeb")
        {
            return Ok(Some(ArchiveType::UnityAssetBundle));
        }

        // 6. Electron ASAR: 4-byte pickle length + JSON tree with "files"
        if ext == "asar" {
            return Ok(Some(ArchiveType::ElectronAsar));
        }
        // An asar is a chain of Chromium Pickles, and the whole chain sits in
        // the bytes already read: a size pickle holding the length of the
        // header pickle, the header pickle holding a string pickle, and then
        // the JSON tree. The test used to be `p1 == 4` plus a second word of
        // plausible magnitude, which claimed any file beginning with a
        // little-endian 4 - Chromium's own resource `.pak` among them, whose
        // version-4 header reads `04 00 00 00 07 04 00 00`. That made every
        // `locales/*.pak` in an Electron game a container no handler would
        // ever trim. Being strict costs nothing: a real `.asar` is claimed by
        // extension just above, and this branch only ever sees one under
        // another name.
        if header_slice.len() >= 17 {
            let word = |at: usize| {
                u32::from_le_bytes([
                    header_slice[at],
                    header_slice[at + 1],
                    header_slice[at + 2],
                    header_slice[at + 3],
                ])
            };
            let (size_pickle, header_pickle, string_pickle, json_len) =
                (word(0), word(4), word(8), word(12));
            // `writeString` pads its payload to a four-byte boundary, and each
            // enclosing pickle prefixes its own four-byte length.
            let padded = u64::from(json_len).div_ceil(4) * 4;
            if size_pickle == 4
                && json_len > 0
                && u64::from(string_pickle) == padded + 4
                && header_pickle == string_pickle + 4
                && header_slice[16] == b'{'
            {
                return Ok(Some(ArchiveType::ElectronAsar));
            }
        }

        // 7. Unreal Pak: Magic 0x5A6F12E1 in trailer (last 256 bytes)
        if file_len >= 44 {
            let seek_back = file_len.min(256);
            if file.seek(SeekFrom::End(-(seek_back as i64))).is_ok() {
                let mut trailer = vec![0u8; seek_back as usize];
                if file.read_exact(&mut trailer).is_ok()
                    && trailer.windows(4).any(|w| w == [0xE1, 0x12, 0x6F, 0x5A])
                {
                    return Ok(Some(ArchiveType::UnrealPak));
                }
            }
        }

        // Fallback checks by extension.
        //
        // Everything above identified the file by its bytes. What follows is a
        // guess from the name, and it loses to a more specific guess from the
        // same name: an external single-language file (`locales/ar.pak`,
        // `sounds_fra.pck`) is a whole-file deletion candidate, never a
        // container - which is the rule `is_candidate_archive_path` has always
        // applied on the scan side. The delete preflight probes content
        // independently, so without this the two disagreed, and a Chromium
        // `.pak` that matched nothing by magic was claimed here as an Unreal
        // pak and held back. A file whose bytes really are a container is still
        // claimed above, by magic, under whatever name it wears - see
        // `supported_archive_magic_overrides_external_language_or_document_name`.
        if is_external_single_language_file(&path.to_string_lossy()) {
            return Ok(None);
        }

        match ext.as_str() {
            "pck" => Ok(Some(ArchiveType::WwisePck)),
            "bnk" => Ok(Some(ArchiveType::WwiseBnk)),
            "pak" => {
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if stem.starts_with("re_chunk") {
                    Ok(Some(ArchiveType::CapcomRePak))
                } else {
                    Ok(Some(ArchiveType::UnrealPak))
                }
            }
            "asar" => Ok(Some(ArchiveType::ElectronAsar)),
            "bk2" => Ok(Some(ArchiveType::BinkVideo)),
            "bundle" | "assets" | "unity3d" => Ok(Some(ArchiveType::UnityAssetBundle)),
            _ => Ok(None),
        }
    }

    /// Returns the handler implementation for a given archive type.
    pub fn get_handler(archive_type: ArchiveType) -> Box<dyn ArchiveHandler> {
        match archive_type {
            ArchiveType::WwisePck => Box::new(wwise::WwiseHandler),
            ArchiveType::WwiseBnk => Box::new(UnsupportedArchiveHandler {
                archive_type,
                reason: "Wwise BNK SoundBank mutation is disabled until a BKHD-specific parser and independent fixtures are available",
            }),
            ArchiveType::UnrealPak => Box::new(ue_pak::UnrealPakHandler),
            ArchiveType::CapcomRePak => Box::new(re_engine::ReEngineHandler),
            ArchiveType::ElectronAsar => Box::new(asar::AsarHandler),
            ArchiveType::UnityAssetBundle => Box::new(unity::UnityHandler),
            ArchiveType::BinkVideo => Box::new(bink::BinkHandler),
        }
    }
}

#[cfg(test)]
mod tests {
    /// The shape [`is_external_single_language_file`] had before the
    /// allocation-free rewrite, kept verbatim so the two can be compared over
    /// a wide corpus. A file this function misroutes is either a container
    /// deleted whole or a localization left on disk, so "it looks equivalent"
    /// is not good enough - see the corpus test below.
    fn reference_is_external_single_language_file(path: &str) -> bool {
        let clean = path.replace('\\', "/");
        let lower = clean.to_lowercase();

        if lower.starts_with("locales/")
            || lower.starts_with("locale/")
            || lower.contains("/locales/")
            || lower.contains("/locale/")
        {
            return true;
        }

        let filename = lower.rsplit('/').next().unwrap_or(&lower);
        let stem = match filename.rsplit_once('.') {
            Some((s, _)) => s,
            None => filename,
        };
        let effective_stem = stem
            .strip_suffix(".pc")
            .or_else(|| stem.strip_suffix(".win"))
            .or_else(|| stem.strip_suffix(".windows"))
            .or_else(|| stem.strip_suffix(".ps4"))
            .or_else(|| stem.strip_suffix(".xbox"))
            .unwrap_or(stem);

        for s in [stem, effective_stem] {
            let clean_s = s.replace(['-', '_'], "");
            if LANG_CODES.contains(&s)
                || LANG_CODES.contains(&clean_s.as_str())
                || LANG_NAMES.contains(&s)
                || LANG_NAMES.contains(&clean_s.as_str())
            {
                return true;
            }
            for code in LANG_CODES {
                if s.ends_with(&format!("_{code}")) || s.ends_with(&format!("-{code}")) {
                    return true;
                }
            }
            for name in LANG_NAMES {
                if s.ends_with(&format!("_{name}")) || s.ends_with(&format!("-{name}")) {
                    return true;
                }
            }
            for name in LANG_NAMES {
                if s.ends_with(name) {
                    return true;
                }
            }
            let is_loc_folder = lower.contains("/localization/")
                || lower.contains("/localisation/")
                || lower.contains("/languages/")
                || lower.contains("/language/")
                || lower.contains("/lang/")
                || lower.contains("/audio/")
                || lower.contains("/sound/")
                || lower.contains("/sounds/")
                || lower.contains("/speech/")
                || lower.contains("/dialogue/")
                || lower.contains("/dialogues/")
                || lower.contains("/vo/")
                || lower.contains("/voice/")
                || lower.contains("/voices/");
            if is_loc_folder && (LANG_CODES.contains(&s) || LANG_NAMES.contains(&s)) {
                return true;
            }
        }

        false
    }

    /// Every language tag, in every shape a real game writes it, against both
    /// implementations. ~40 000 paths: the rewrite is a hot-path optimization,
    /// and the only acceptable difference between the two is none.
    #[test]
    fn the_allocation_free_language_test_matches_the_original_over_a_wide_corpus() {
        const FOLDERS: &[&str] = &[
            "",
            "Data/",
            "Game/Content/Paks/",
            "Data\\Sound\\",
            "Engine/Localization/",
            "bin/x64/locales/",
            "Data/Audio/",
            "Data/VO/",
        ];
        const EXTENSIONS: &[&str] = &["pck", "bnk", "pak", "asar", "unity3d", "assets", "exe", ""];
        const STEMS: &[&str] = &[
            "sounds",
            "vo",
            "speech",
            "audio",
            "voices",
            "pakchunk0",
            "app",
            "re_chunk_000",
            "UE4_Logo",
            "Game",
            "",
        ];

        let mut tags: Vec<String> = Vec::new();
        for tag in LANG_CODES.iter().chain(LANG_NAMES.iter()) {
            tags.push((*tag).to_string());
            tags.push(tag.to_uppercase());
        }

        let mut checked = 0usize;
        for folder in FOLDERS {
            for stem in STEMS {
                for tag in &tags {
                    for joiner in ["_", "-", "", "."] {
                        for ext in EXTENSIONS {
                            let name = format!("{stem}{joiner}{tag}");
                            let path = if ext.is_empty() {
                                format!("{folder}{name}")
                            } else {
                                format!("{folder}{name}.{ext}")
                            };
                            assert_eq!(
                                is_external_single_language_file(&path),
                                reference_is_external_single_language_file(&path),
                                "the two implementations disagree about {path:?}"
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
        // Plus the shapes the tags alone do not produce: platform tags,
        // bare containers and mixed separators.
        for path in [
            "VO_AMICIA_MEDIA.PC.PCK",
            "sounds_fra.pc.pck",
            r"Data\Sound\Russian.pck",
            "Data/Localization/Spanish.pak",
            "locales/en-US.pak",
            "Locale/fr.json",
            "app.asar",
            "re_chunk_000.pak",
            "voices.pck",
            "soundbanks.pck",
            "zh-cn.pak",
            "es419.pak",
            "Binaries/Win64/Game.exe",
        ] {
            assert_eq!(
                is_external_single_language_file(path),
                reference_is_external_single_language_file(path),
                "the two implementations disagree about {path:?}"
            );
            checked += 1;
        }
        assert!(checked > 30_000, "the corpus shrank to {checked} paths");
    }

    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn bnk_detection_does_not_route_to_akpk_parser() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("soundbank.bnk");
        let mut bytes = vec![0u8; 32];
        bytes[0..4].copy_from_slice(b"BKHD");
        fs::write(&path, bytes).expect("write bnk");

        let detected = FormatDetector::detect_file(&path).expect("detect");
        assert_eq!(detected, Some(ArchiveType::WwiseBnk));
        assert!(matches!(
            FormatDetector::get_handler(ArchiveType::WwiseBnk).analyze(&path),
            Err(ArchiveError::Unsupported(_))
        ));
    }

    /// Wraps a JSON tree in the Pickle chain Electron writes around one.
    fn asar_bytes(json: &str) -> Vec<u8> {
        let padded = json.len().div_ceil(4) * 4;
        let string_pickle = (padded + 4) as u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&(string_pickle + 4).to_le_bytes());
        bytes.extend_from_slice(&string_pickle.to_le_bytes());
        bytes.extend_from_slice(&(json.len() as u32).to_le_bytes());
        bytes.extend_from_slice(json.as_bytes());
        bytes.resize(16 + padded, 0);
        bytes
    }

    #[test]
    fn a_chromium_resource_pak_under_locales_is_not_a_container() {
        let dir = tempdir().expect("tempdir");
        let locales = dir.path().join("bin").join("x64").join("locales");
        fs::create_dir_all(&locales).expect("create locales");

        // The first bytes of 3DMark's `bin/x64/locales/ar.pak`: a Chromium
        // resource pack, version 4, 1031 entries. `04 00 00 00` alone used to
        // read as an asar, and failing that the `.pak` extension read as an
        // Unreal pak, so the delete preflight held it back by one route or the
        // other - for every Electron game in the library.
        let mut bytes = vec![0u8; 64];
        bytes[0..8].copy_from_slice(&[0x04, 0x00, 0x00, 0x00, 0x07, 0x04, 0x00, 0x00]);
        let pak = locales.join("ar.pak");
        fs::write(&pak, &bytes).expect("write pak");
        assert_eq!(FormatDetector::detect_file(&pak).expect("detect"), None);

        // The name only beats a guess from the extension. Real container bytes
        // still win under a single-language name.
        let mut akpk = bytes.clone();
        akpk[0..4].copy_from_slice(b"AKPK");
        let disguised = dir.path().join("sounds_fra.pck");
        fs::write(&disguised, &akpk).expect("write pck");
        assert_eq!(
            FormatDetector::detect_file(&disguised).expect("detect"),
            Some(ArchiveType::WwisePck)
        );

        // And a genuine asar wearing another extension is still recognized.
        let renamed = dir.path().join("app.bin");
        fs::write(&renamed, asar_bytes(r#"{"files":{}}"#)).expect("write asar");
        assert_eq!(
            FormatDetector::detect_file(&renamed).expect("detect"),
            Some(ArchiveType::ElectronAsar)
        );
    }

    #[test]
    fn test_canonical_language_valid() {
        assert_eq!(canonical_language("en"), "english");
        assert_eq!(canonical_language("eng"), "english");
        assert_eq!(canonical_language("en-US"), "english");
        assert_eq!(canonical_language("English (US)"), "english");
        assert_eq!(canonical_language("fra"), "french");
        assert_eq!(canonical_language("fre"), "french");
        assert_eq!(canonical_language("French (France)"), "french");
        assert_eq!(canonical_language("de"), "german");
        assert_eq!(canonical_language("ger"), "german");
        assert_eq!(canonical_language("deu"), "german");
        assert_eq!(canonical_language("German"), "german");
        assert_eq!(canonical_language("es"), "spanish");
        assert_eq!(canonical_language("es-419"), "spanish");
        assert_eq!(canonical_language("spa"), "spanish");
        assert_eq!(canonical_language("Spanish (Latin America)"), "spanish");
        assert_eq!(canonical_language("it"), "italian");
        assert_eq!(canonical_language("ita"), "italian");
        assert_eq!(canonical_language("Italian"), "italian");
        assert_eq!(canonical_language("ru"), "russian");
        assert_eq!(canonical_language("rus"), "russian");
        assert_eq!(canonical_language("Russian"), "russian");
        assert_eq!(canonical_language("ja"), "japanese");
        assert_eq!(canonical_language("jpn"), "japanese");
        assert_eq!(canonical_language("Japanese"), "japanese");
        assert_eq!(canonical_language("zh-Hans"), "chinese");
        assert_eq!(canonical_language("zh-Hant"), "chinese");
        assert_eq!(canonical_language("pt-BR"), "portuguese");
        assert!(is_known_language("zh-Hans"));
        assert!(!is_known_language("Language_424242"));
        assert_eq!(canonical_language("zh"), "chinese");
        assert_eq!(canonical_language("zh-cn"), "chinese");
        assert_eq!(canonical_language("zh-tw"), "chinese");
        assert_eq!(canonical_language("Chinese"), "chinese");
        assert_eq!(canonical_language("ko"), "korean");
        assert_eq!(canonical_language("kor"), "korean");
        assert_eq!(canonical_language("Korean"), "korean");
        assert_eq!(canonical_language("pl"), "polish");
        assert_eq!(canonical_language("Polish"), "polish");
        assert_eq!(canonical_language("pt"), "portuguese");
        assert_eq!(canonical_language("pt-br"), "portuguese");
        assert_eq!(canonical_language("Portuguese"), "portuguese");
        assert_eq!(canonical_language("SFX"), "sfx");
        assert_eq!(canonical_language("common"), "common");
        assert_eq!(canonical_language("voices"), "voices");
    }

    #[test]
    fn test_canonical_language_false_positive_prevention() {
        // Words that contain substrings of language codes but are NOT language tags
        assert_eq!(canonical_language("engine"), "other");
        assert_eq!(canonical_language("engineer"), "other");
        assert_eq!(canonical_language("green"), "other");
        assert_eq!(canonical_language("open"), "other");
        assert_eq!(canonical_language("frame"), "other");
        assert_eq!(canonical_language("freeze"), "other");
        assert_eq!(canonical_language("space"), "other");
        assert_eq!(canonical_language("spawn"), "other");
        assert_eq!(canonical_language("child"), "other");
        assert_eq!(canonical_language("polygon"), "other");
        assert_eq!(canonical_language("portal"), "other");
        assert_eq!(canonical_language("turret"), "other");
        assert_eq!(canonical_language("hunter"), "other");
        assert_eq!(canonical_language("arena"), "other");
        assert_eq!(canonical_language("rust"), "other");
        assert_eq!(canonical_language("item"), "other");
        assert_eq!(canonical_language("germ"), "other");
        assert_eq!(canonical_language(""), "other");
    }

    #[test]
    fn test_is_language_kept() {
        let keep_en = vec!["english".to_string(), "en".to_string()];

        // Kept
        assert!(is_language_kept("en", &keep_en));
        assert!(is_language_kept("eng", &keep_en));
        assert!(is_language_kept("en-US", &keep_en));
        assert!(is_language_kept("English (US)", &keep_en));
        assert!(is_language_kept("SFX", &keep_en));
        assert!(is_language_kept("common", &keep_en));
        assert!(is_language_kept("voices", &keep_en));

        // Not Kept
        assert!(!is_language_kept("french", &keep_en));
        assert!(!is_language_kept("fra", &keep_en));
        assert!(!is_language_kept("de", &keep_en));
        assert!(!is_language_kept("German", &keep_en));
        assert!(!is_language_kept("Russian", &keep_en));
        assert!(!is_language_kept("Japanese", &keep_en));

        // False positives should NOT be kept as English
        assert!(!is_language_kept("green", &keep_en));
        assert!(!is_language_kept("open", &keep_en));
        assert!(!is_language_kept("engine", &keep_en));
    }

    #[test]
    fn test_is_external_single_language_file() {
        // True positives: Standalone whole external language files
        assert!(is_external_single_language_file(
            "3DMark\\bin\\x64\\locales\\ar.pak"
        ));
        assert!(is_external_single_language_file("bin/x64/locales/de.pak"));
        assert!(is_external_single_language_file("locales/de.pak"));
        assert!(is_external_single_language_file("locales/en-US.pak"));
        assert!(is_external_single_language_file(
            "resources/locales/fr.json"
        ));
        assert!(is_external_single_language_file("sounds_fra.pck"));
        assert!(is_external_single_language_file("sounds_fra.pc.pck"));
        assert!(is_external_single_language_file("audio_ger.pck"));
        assert!(is_external_single_language_file("speech_rus.pck"));
        assert!(is_external_single_language_file("speech_ru.pck"));
        assert!(is_external_single_language_file("dialogue_spa.pck"));
        assert!(is_external_single_language_file("voices_deu.pck"));
        assert!(is_external_single_language_file("sound_ita.pck"));
        assert!(is_external_single_language_file("SoundsGerman.pck"));
        assert!(is_external_single_language_file("AudioFrench.pck"));
        assert!(is_external_single_language_file("DialogueSpanish.pck"));
        assert!(is_external_single_language_file("VoicesRussian.pck"));
        assert!(is_external_single_language_file(
            "Content/Localization/Spanish.pak"
        ));
        assert!(is_external_single_language_file("Sound/Russian.pck"));
        assert!(is_external_single_language_file("Audio/de.pck"));
        assert!(is_external_single_language_file("Audio/German.pck"));
        assert!(is_external_single_language_file("ar.pak"));
        assert!(is_external_single_language_file("de.pak"));
        assert!(is_external_single_language_file("es-es.pak"));

        // True negatives: Monolithic archives containing internal multi-language data
        assert!(!is_external_single_language_file(
            "A Plague Tale Innocence\\SOUNDBANKS\\VO_AMICIA_MEDIA.PC.PCK"
        ));
        assert!(!is_external_single_language_file("VO_AMICIA_MEDIA.PC.PCK"));
        assert!(!is_external_single_language_file(
            "SOUNDBANKS/VO_D1_MEDIA.PC.PCK"
        ));
        assert!(!is_external_single_language_file(
            "SOUNDBANKS/VO_D2_MEDIA.PC.PCK"
        ));
        assert!(!is_external_single_language_file(
            "Content/Paks/pakchunk0-WindowsNoEditor.pak"
        ));
        assert!(!is_external_single_language_file("re_chunk_000.pak"));
        assert!(!is_external_single_language_file("resources/app.asar"));
        assert!(!is_external_single_language_file("app.asar"));
        assert!(!is_external_single_language_file("audio/voices.pck"));
        assert!(!is_external_single_language_file("voices.pck"));
        assert!(!is_external_single_language_file("soundbanks.pck"));
        assert!(!is_external_single_language_file("sharedassets0.assets"));
        assert!(!is_external_single_language_file("global.bundle"));
        assert!(!is_external_single_language_file("movies/intro.bk2"));
    }
}
