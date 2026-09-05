//! How many bytes of a Battle.net CASC install are language packs the
//! player does not read, and how many of those overlap with the language
//! they kept - a port of a Python prototype that was hand-verified against
//! the numbers Battle.net's own "Modify Installation" dialog shows.
//!
//! GameTrimmer never deletes any of this itself: the data lives inside
//! shared 1 GB `data.NNN` archives, and only the Battle.net launcher can
//! remove entries from them safely. This module exists purely to *name the
//! number* ("deselecting every language but the one you play in frees N GB")
//! so the app can point the player at the launcher's own dialog instead of
//! silently ignoring the biggest opportunity on the drive.
//!
//! # Format, in one paragraph
//!
//! `.build.info` in the game folder lists one or more products (a build
//! `key` each) sharing the folder's storage. Each product's build config
//! (`Data/config/<k0:2>/<k2:4>/<key>`) names a `download` manifest by its
//! encoding key (EKey). The local index (`Data/data/*.idx`) maps a 9-byte
//! EKey prefix to an `(archive, offset, size)` triple inside one of the
//! `Data/data/data.NNN` archive blobs; reading that triple and stripping a
//! 30-byte entry header yields a BLTE-compressed blob, which decompresses to
//! the `download` manifest itself. That manifest tags every content blob it
//! knows about with zero or more tags, and *some* of those tags are locale
//! codes (`enUS`, `deDE`, ...) - recognised only by their four-letter
//! `^[a-z]{2}[A-Z]{2}$` shape, never by a tag-type number, because the
//! number space is reused for unrelated tag classes and differs per product
//! (Diablo III's locales are type 2, Diablo II Resurrected's are type 1,
//! World of Warcraft's are type 3 - filtering by number once summed a
//! completely different tag class and reported 245 GB removable on a 229 GB
//! game).
//!
//! A blob is *removable* if the local index actually has it, it carries at
//! least one locale tag, and no product in the folder still needs it for
//! the kept locale (or needs it because it carries no locale tag at all -
//! shared/neutral content). That last clause matters because a folder can
//! hold more than one product against one storage (World of Warcraft ships
//! retail and a classic build together): dropping a language for one
//! product must never orphan a blob the other product still needs.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use flate2::read::ZlibDecoder;

use crate::error::Result;

/// What deselecting every language but `kept` would free in one game
/// folder, across every product sharing its storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocaleFootprint {
    /// The locale kept - echoes the `keep` argument back for display.
    pub kept: String,
    /// Every locale tag seen across every product in this folder, sorted
    /// and deduplicated. What the launcher's own language list would show.
    pub offered: Vec<String>,
    /// Bytes freed by deselecting every locale but `kept`: the sum of the
    /// download-manifest sizes of blobs that are present locally, carry at
    /// least one locale tag, and are not needed by any product for the kept
    /// locale (or for no locale at all).
    pub removable_bytes: u64,
}

/// Computes [`LocaleFootprint`] for one Battle.net game folder.
///
/// Returns `Ok(None)` for anything this cannot read: no `.build.info` (not
/// a CASC game, or not a Battle.net folder at all), no local index at
/// `Data/data/*.idx` (StarCraft II still uses the older MPQ layout and has
/// no CASC storage here), or a download manifest this port could not
/// decode (an encrypted or recursive-frame BLTE chunk, or a header shape
/// this port has not seen). Silence, never a guess - a wrong number here
/// sends someone to deselect the language they actually play in.
pub fn locale_footprint(install_dir: &Path, keep: &str) -> Result<Option<LocaleFootprint>> {
    let Some(rows) = build_rows(install_dir) else {
        return Ok(None);
    };

    let index = local_index(install_dir);
    if index.is_empty() {
        // Either there is no `Data/data` at all, or every `.idx` file in it
        // used a layout this port does not recognise - either way, there is
        // nothing here to account against.
        return Ok(None);
    }

    let manifests: Vec<DownloadManifest> = rows
        .iter()
        .filter_map(|row| product_manifest(install_dir, row, &index))
        .collect();

    let (removable_bytes, offered) = compute_footprint(&manifests, &index, keep);
    Ok(Some(LocaleFootprint {
        kept: keep.to_string(),
        offered,
        removable_bytes,
    }))
}

/// One `(archive, offset, size)` triple from a local `.idx` file: where a
/// blob keyed by a 9-byte EKey prefix lives inside `Data/data/data.NNN`.
#[derive(Debug, Clone, Copy)]
struct IndexEntry {
    archive: u32,
    offset: u64,
    size: u32,
}

/// A parsed `download` manifest: which blobs (by 9-byte EKey prefix and
/// manifest-reported size) this product references, and which tags -
/// locale or otherwise - mark which of those blobs.
struct DownloadManifest {
    entries: Vec<([u8; 9], u64)>,
    tags: Vec<(String, u16, Vec<u8>)>,
}

/// Reads every data row of `.build.info`, keyed by its named header
/// (`Branch!STRING:0|...`) rather than by column position - the column
/// order is not part of the contract, only the names are. `None` if the
/// file is absent, unreadable, or carries no data rows at all.
fn build_rows(install_dir: &Path) -> Option<Vec<HashMap<String, String>>> {
    let text = fs::read_to_string(install_dir.join(".build.info")).ok()?;
    let mut lines = text.lines();
    let header: Vec<String> = lines
        .next()?
        .split('|')
        .map(|cell| cell.split('!').next().unwrap_or("").to_string())
        .collect();

    let rows: Vec<HashMap<String, String>> = lines
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            header
                .iter()
                .cloned()
                .zip(line.split('|').map(str::to_string))
                .collect()
        })
        .collect();

    if rows.is_empty() {
        None
    } else {
        Some(rows)
    }
}

/// Reads one build config: plain UTF-8 `key = value...` lines, each value
/// split on whitespace into tokens (a `download` line carries `<ckey>
/// <ekey>`, for instance). `None` if the config file for `key` is absent.
fn read_config(install_dir: &Path, key: &str) -> Option<HashMap<String, Vec<String>>> {
    if key.len() < 4 {
        return None;
    }
    let path = install_dir
        .join("Data")
        .join("config")
        .join(&key[0..2])
        .join(&key[2..4])
        .join(key);
    let text = fs::read_to_string(path).ok()?;

    let mut cfg = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // `split_once` mirrors Python's `str.partition`: a line with no `=`
        // still contributes (the whole line as the key, an empty value)
        // rather than aborting the rest of the config.
        let (k, v) = line.split_once('=').unwrap_or((line, ""));
        let values = v.split_whitespace().map(str::to_string).collect();
        cfg.insert(k.trim().to_string(), values);
    }
    Some(cfg)
}

/// Reads every `.idx` file under `Data/data` into one prefix -> location
/// map. Missing entirely (StarCraft II's MPQ layout) or empty (every
/// `.idx` file used a header this port does not recognise) both come back
/// as an empty map - the caller treats that as "no CASC storage here".
fn local_index(install_dir: &Path) -> HashMap<[u8; 9], IndexEntry> {
    let mut index = HashMap::new();
    let data_dir = install_dir.join("Data").join("data");
    let Ok(read_dir) = fs::read_dir(&data_dir) else {
        return index;
    };

    let mut idx_paths: Vec<PathBuf> = read_dir
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("idx"))
        })
        .collect();
    idx_paths.sort();

    for path in idx_paths {
        if let Ok(bytes) = fs::read(&path) {
            if let Some(entries) = parse_idx_file(&bytes) {
                index.extend(entries);
            }
        }
    }
    index
}

/// Parses one `.idx` file's header and entry table. Header layout: u32 LE
/// `header_len`, u32 hash (skipped), then `header_len` bytes of `u16
/// version, u8 bucket, u8 extra, u8 size_bytes, u8 offset_bytes, u8
/// key_bytes, u8 offset_bits`. Entries start at the next 16-byte-aligned
/// position, after a u32 LE `entries_len` and 4 more skipped bytes; each
/// entry is `key_bytes` (an EKey prefix) + `offset_bytes` (big-endian,
/// packing `archive` and `offset` via `offset_bits`) + `size_bytes`
/// (little-endian).
fn parse_idx_file(bytes: &[u8]) -> Option<Vec<([u8; 9], IndexEntry)>> {
    if bytes.len() < 8 {
        return None;
    }
    let head_len = u32::from_le_bytes(bytes[0..4].try_into().ok()?) as usize;
    if head_len < 8 || 8 + head_len > bytes.len() {
        return None;
    }
    let head = &bytes[8..8 + head_len];
    let size_bytes = head[4] as usize;
    let off_bytes = head[5] as usize;
    let key_bytes = head[6] as usize;
    let off_bits = head[7] as u32;

    // This map is keyed on a fixed 9-byte EKey prefix (observed key_bytes
    // == 9 across Diablo III, Diablo II Resurrected, World of Warcraft, and
    // Call of Duty on this machine). A different key_bytes is a CASC index
    // layout this port has not seen - skip the file rather than guess a
    // shape for it.
    if key_bytes != 9
        || off_bytes == 0
        || off_bytes > 8
        || size_bytes == 0
        || size_bytes > 8
        || off_bits >= 64
    {
        return None;
    }

    let mut pos = (8 + head_len + 0x0F) & !0x0F;
    if pos + 8 > bytes.len() {
        return None;
    }
    let entries_len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().ok()?) as usize;
    pos += 8;

    let elen = key_bytes + size_bytes + off_bytes;
    let count = entries_len / elen;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let start = pos + i * elen;
        if start + elen > bytes.len() {
            return None;
        }
        let e = &bytes[start..start + elen];
        let mut key = [0u8; 9];
        key.copy_from_slice(&e[0..9]);
        let raw = read_uint_be(&e[key_bytes..key_bytes + off_bytes]);
        let archive = (raw >> off_bits) as u32;
        let offset = raw & ((1u64 << off_bits) - 1);
        let size =
            read_uint_le(&e[key_bytes + off_bytes..key_bytes + off_bytes + size_bytes]) as u32;
        out.push((
            key,
            IndexEntry {
                archive,
                offset,
                size,
            },
        ));
    }
    Some(out)
}

/// Decompresses one BLTE blob. `header_size == 0` means a single raw chunk
/// starting at offset 8; otherwise a `u8` flags byte, a `u24` BE chunk
/// count, and a `(u32 compressed_size, u32 decompressed_size, 16-byte md5)`
/// header per chunk precede the chunk data itself. Each chunk's data opens
/// with a one-byte mode: `N` (raw copy) and `Z` (zlib) are decoded; `E`
/// (encrypted) and `F` (recursive frame), and any mode this port does not
/// know, give up on the *whole* blob and return `None` rather than hand
/// back a partially-decoded result that would look like a real size.
fn blte_decode(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() < 8 || &raw[0..4] != b"BLTE" {
        return None;
    }
    let _header_size = u32::from_be_bytes(raw[4..8].try_into().ok()?);

    let mut pos;
    let mut chunk_sizes: Vec<usize> = Vec::new();
    if _header_size == 0 {
        pos = 8;
        chunk_sizes.push(raw.len().checked_sub(8)?);
    } else {
        if raw.len() < 12 {
            return None;
        }
        let count = ((raw[9] as usize) << 16) | ((raw[10] as usize) << 8) | (raw[11] as usize);
        pos = 12;
        for _ in 0..count {
            if pos + 24 > raw.len() {
                return None;
            }
            let csize = u32::from_be_bytes(raw[pos..pos + 4].try_into().ok()?) as usize;
            pos += 24; // 4 (compressed_size) + 4 (decompressed_size, unused) + 16 (md5, unused)
            chunk_sizes.push(csize);
        }
    }

    let mut out = Vec::new();
    for csize in chunk_sizes {
        if csize == 0 || pos + csize > raw.len() {
            return None;
        }
        let mode = raw[pos];
        let data = &raw[pos + 1..pos + csize];
        pos += csize;
        match mode {
            b'N' => out.extend_from_slice(data),
            b'Z' => {
                let mut decoder = ZlibDecoder::new(data);
                decoder.read_to_end(&mut out).ok()?;
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Parses a decompressed `download` manifest. Header: magic `DL`, `u8
/// version`, `u8 ekey_size`, `u8 has_checksum`, `u32` BE `entry_count`,
/// `u16` BE `tag_count`, then (version >= 2) a `u8 flag_bytes`, then
/// (version >= 3) 4 more skipped bytes. Each entry is `ekey_size + 5 (u40
/// BE size) + 1 (priority, unused) + (4 if has_checksum) + flag_bytes`
/// bytes; only the first 9 bytes of the EKey (the same prefix the local
/// index keys on) and the size are kept. Tags follow: a NUL-terminated
/// name, a `u16` BE type, and a `ceil(entry_count/8)`-byte bitmap.
fn parse_download(data: &[u8]) -> Option<DownloadManifest> {
    if data.len() < 11 || &data[0..2] != b"DL" {
        return None;
    }
    let version = data[2];
    let ekey_size = data[3] as usize;
    let has_checksum = data[4] != 0;
    let entry_count = u32::from_be_bytes(data[5..9].try_into().ok()?) as usize;
    let tag_count = u16::from_be_bytes(data[9..11].try_into().ok()?) as usize;

    let mut pos = 11usize;
    let mut flag_bytes = 0usize;
    if version >= 2 {
        flag_bytes = *data.get(pos)? as usize;
        pos += 1;
        if version >= 3 {
            pos += 4;
        }
    }

    // A shorter EKey than the local index's 9-byte prefix is an unfamiliar
    // manifest layout this port has not seen - bail rather than guess.
    if ekey_size < 9 {
        return None;
    }
    let elen = ekey_size + 5 + 1 + if has_checksum { 4 } else { 0 } + flag_bytes;

    let mut entries = Vec::with_capacity(entry_count);
    for _ in 0..entry_count {
        if pos + elen > data.len() {
            return None;
        }
        let e = &data[pos..pos + elen];
        pos += elen;
        let mut prefix = [0u8; 9];
        prefix.copy_from_slice(&e[0..9]);
        let size = read_uint_be(&e[ekey_size..ekey_size + 5]);
        entries.push((prefix, size));
    }

    let bitmap_len = entry_count.div_ceil(8);
    let mut tags = Vec::with_capacity(tag_count);
    for _ in 0..tag_count {
        let nul = data[pos..].iter().position(|&b| b == 0)?;
        let name = String::from_utf8_lossy(&data[pos..pos + nul]).into_owned();
        pos += nul + 1;
        if pos + 2 + bitmap_len > data.len() {
            return None;
        }
        let ttype = u16::from_be_bytes(data[pos..pos + 2].try_into().ok()?);
        pos += 2;
        let bitmap = data[pos..pos + bitmap_len].to_vec();
        pos += bitmap_len;
        tags.push((name, ttype, bitmap));
    }
    Some(DownloadManifest { entries, tags })
}

/// Reads one product's `download` manifest: build config -> `download`
/// line's EKey -> local index lookup -> seek-and-read exactly the stored
/// length from the right `data.NNN` archive (never the whole 1 GB file) ->
/// strip the 30-byte entry header -> BLTE decode -> manifest parse. `None`
/// anywhere along that chain (missing build key column, no config on disk,
/// no `download` entry, blob not present locally, or an undecodable BLTE
/// chunk) drops just this product row - other products in the same folder
/// are still accounted for.
fn product_manifest(
    install_dir: &Path,
    row: &HashMap<String, String>,
    index: &HashMap<[u8; 9], IndexEntry>,
) -> Option<DownloadManifest> {
    let build_key = row.get("Build Key")?;
    let cfg = read_config(install_dir, build_key)?;
    let download = cfg.get("download")?;
    let ekey_hex = download.get(1)?;
    let ekey = hex_decode(ekey_hex)?;
    if ekey.len() < 9 {
        return None;
    }
    let mut prefix = [0u8; 9];
    prefix.copy_from_slice(&ekey[0..9]);
    let entry = index.get(&prefix)?;

    let archive_path = install_dir
        .join("Data")
        .join("data")
        .join(format!("data.{:03}", entry.archive));
    let mut file = fs::File::open(archive_path).ok()?;
    file.seek(SeekFrom::Start(entry.offset)).ok()?;
    let mut stored = vec![0u8; entry.size as usize];
    file.read_exact(&mut stored).ok()?;
    if stored.len() <= 30 {
        return None;
    }

    let decoded = blte_decode(&stored[30..])?;
    parse_download(&decoded)
}

/// A locale tag is recognised purely by its four-letter `xxYY` shape
/// (`^[a-z]{2}[A-Z]{2}$`) - never by its tag-type number, which is reused
/// for unrelated tag classes and differs per product. See the module doc
/// for the 245 GB-on-a-229-GB-game incident this rule exists to prevent.
fn is_locale_tag_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 4
        && b[0].is_ascii_lowercase()
        && b[1].is_ascii_lowercase()
        && b[2].is_ascii_uppercase()
        && b[3].is_ascii_uppercase()
}

/// The needed/removable accounting itself, factored out of I/O so it can
/// be exercised directly in tests: a blob is needed if any product tags it
/// with the kept locale or with no locale tag at all, and removable if it
/// carries at least one locale tag, is present locally, and is not needed
/// by any product. Returns `(removable_bytes, offered_locales)`.
fn compute_footprint(
    manifests: &[DownloadManifest],
    index: &HashMap<[u8; 9], IndexEntry>,
    keep: &str,
) -> (u64, Vec<String>) {
    let mut needed: HashSet<[u8; 9]> = HashSet::new();
    let mut localized: HashMap<[u8; 9], u64> = HashMap::new();
    let mut offered: BTreeSet<String> = BTreeSet::new();

    for manifest in manifests {
        let locales: Vec<(&str, &Vec<u8>)> = manifest
            .tags
            .iter()
            .filter(|(name, _, _)| is_locale_tag_name(name))
            .map(|(name, _, bitmap)| (name.as_str(), bitmap))
            .collect();
        offered.extend(locales.iter().map(|(name, _)| name.to_string()));

        for (i, (key, size)) in manifest.entries.iter().enumerate() {
            if !index.contains_key(key) {
                continue;
            }
            let mut tagged = false;
            let mut kept = false;
            for (name, bitmap) in &locales {
                let byte = bitmap.get(i >> 3).copied().unwrap_or(0);
                if byte & (0x80 >> (i & 7)) != 0 {
                    tagged = true;
                    if *name == keep {
                        kept = true;
                    }
                }
            }
            if !tagged || kept {
                needed.insert(*key);
            } else {
                localized.insert(*key, *size);
            }
        }
    }

    let removable_bytes = localized
        .iter()
        .filter(|(key, _)| !needed.contains(*key))
        .map(|(_, &size)| size)
        .sum();
    (removable_bytes, offered.into_iter().collect())
}

/// Decodes a lowercase- or uppercase-hex string into bytes. `None` on odd
/// length or a non-hex digit, rather than panicking on hand-authored test
/// fixtures or a corrupt config line.
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim().as_bytes();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    s.chunks(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            Some(((hi << 4) | lo) as u8)
        })
        .collect()
}

/// Big-endian bytes (up to 8 of them) to `u64` - the general form of
/// `int.from_bytes(b, "big")` for the variable-width fields the CASC index
/// and download-manifest headers declare their own widths for.
fn read_uint_be(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |acc, &b| (acc << 8) | b as u64)
}

/// Little-endian counterpart of [`read_uint_be`].
fn read_uint_le(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .rev()
        .fold(0u64, |acc, &b| (acc << 8) | b as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a single-chunk-header-less BLTE blob (`header_size == 0`)
    /// wrapping one `N` (raw) chunk.
    fn blte_single_raw(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"BLTE");
        out.extend_from_slice(&0u32.to_be_bytes()); // header_size == 0
        out.push(b'N');
        out.extend_from_slice(payload);
        out
    }

    /// Builds a multi-chunk BLTE blob with an explicit chunk table, one
    /// chunk per `(mode, data)` pair.
    fn blte_multi(chunks: &[(u8, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"BLTE");
        let chunk_table_size = 12 + chunks.len() * 24;
        out.extend_from_slice(&(chunk_table_size as u32).to_be_bytes());
        out.push(0); // flags, unused
        let count = chunks.len() as u32;
        out.extend_from_slice(&count.to_be_bytes()[1..4]); // u24 BE chunk count
        for (mode, data) in chunks {
            let csize = 1 + data.len();
            out.extend_from_slice(&(csize as u32).to_be_bytes());
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(&[0u8; 16]); // md5, unused by this port
            let _ = mode;
        }
        for (mode, data) in chunks {
            out.push(*mode);
            out.extend_from_slice(data);
        }
        out
    }

    #[test]
    fn blte_n_chunk_round_trips_raw_bytes() {
        let blob = blte_single_raw(b"hello locales");
        assert_eq!(blte_decode(&blob).unwrap(), b"hello locales");
    }

    #[test]
    fn blte_z_chunk_round_trips_zlib_bytes() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"compressed payload").unwrap();
        let compressed = encoder.finish().unwrap();

        let blob = blte_multi(&[(b'Z', &compressed)]);
        assert_eq!(blte_decode(&blob).unwrap(), b"compressed payload");
    }

    #[test]
    fn blte_multi_chunk_n_round_trips_and_concatenates() {
        let blob = blte_multi(&[(b'N', b"abc"), (b'N', b"def")]);
        assert_eq!(blte_decode(&blob).unwrap(), b"abcdef");
    }

    #[test]
    fn blte_encrypted_chunk_yields_none_not_partial_output() {
        let blob = blte_multi(&[(b'N', b"kept"), (b'E', b"encrypted-junk")]);
        // Must not return `Some(b"kept")` - a partial decode reads as a
        // (wrong) real size to every caller.
        assert_eq!(blte_decode(&blob), None);
    }

    #[test]
    fn blte_frame_chunk_yields_none() {
        let blob = blte_multi(&[(b'F', b"nested-frame")]);
        assert_eq!(blte_decode(&blob), None);
    }

    /// Builds a `download` manifest (version 2, no checksums) with the
    /// given entries and tags.
    fn download_manifest_bytes(entries: &[([u8; 9], u64)], tags: &[(&str, u16)]) -> Vec<u8> {
        let ekey_size = 9usize;
        let has_checksum = 0u8;
        let flag_bytes = 0usize;
        let mut out = Vec::new();
        out.extend_from_slice(b"DL");
        out.push(2); // version
        out.push(ekey_size as u8);
        out.push(has_checksum);
        out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        out.extend_from_slice(&(tags.len() as u16).to_be_bytes());
        out.push(flag_bytes as u8);

        for (key, size) in entries {
            out.extend_from_slice(key);
            out.extend_from_slice(&size.to_be_bytes()[3..8]); // u40 BE
            out.push(0); // priority, unused
        }

        let bitmap_len = entries.len().div_ceil(8);
        for (name, ttype) in tags {
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            out.extend_from_slice(&ttype.to_be_bytes());
            out.extend_from_slice(&vec![0u8; bitmap_len]);
        }
        out
    }

    #[test]
    fn download_manifest_parses_entries_and_tag_bitmaps() {
        let key1 = [1u8; 9];
        let key2 = [2u8; 9];
        let raw = download_manifest_bytes(&[(key1, 100), (key2, 200)], &[("enUS", 2)]);

        let manifest = parse_download(&raw).unwrap();
        assert_eq!(manifest.entries, vec![(key1, 100), (key2, 200)]);
        assert_eq!(manifest.tags.len(), 1);
        assert_eq!(manifest.tags[0].0, "enUS");
        assert_eq!(manifest.tags[0].1, 2);
        // 2 entries -> 1 bitmap byte, entry 0 set only.
        assert_eq!(manifest.tags[0].2, vec![0u8]);
    }

    #[test]
    fn locale_tag_name_accepts_locale_shapes() {
        assert!(is_locale_tag_name("deDE"));
        assert!(is_locale_tag_name("zhCN"));
        assert!(is_locale_tag_name("enUS"));
    }

    #[test]
    fn locale_tag_name_rejects_non_locale_shapes() {
        assert!(!is_locale_tag_name("Windows"));
        assert!(!is_locale_tag_name("x86_64"));
        assert!(!is_locale_tag_name("speech"));
        assert!(!is_locale_tag_name("text"));
        assert!(!is_locale_tag_name("HighRes"));
    }

    /// Two products share one storage. Product A (e.g. retail) tags a blob
    /// as `deDE`-only; product B (e.g. a classic build) tags the *same*
    /// blob as language-neutral (no locale tag at all). The blob must come
    /// out needed, not removable - dropping German for product A must not
    /// orphan a blob product B still needs.
    #[test]
    fn accounting_keeps_a_blob_a_sibling_product_still_needs() {
        let de_only = [9u8; 9];
        let en_only = [8u8; 9];
        let shared_neutral_blob = [7u8; 9];

        let mut index = HashMap::new();
        for key in [de_only, en_only, shared_neutral_blob] {
            index.insert(
                key,
                IndexEntry {
                    archive: 0,
                    offset: 0,
                    size: 0,
                },
            );
        }

        // Product A: entry 0 = deDE-tagged (removable if deDE dropped),
        // entry 1 = enUS-tagged (kept), entry 2 = the shared blob, tagged
        // deDE-only *by this product*.
        let product_a = DownloadManifest {
            entries: vec![
                (de_only, 1_000),
                (en_only, 2_000),
                (shared_neutral_blob, 3_000),
            ],
            tags: vec![
                ("deDE".to_string(), 2, vec![0b1010_0000]), // entries 0 and 2
                ("enUS".to_string(), 2, vec![0b0100_0000]), // entry 1
            ],
        };

        // Product B: only references the shared blob, with no locale tag
        // at all (language-neutral, e.g. game logic shared by both
        // products).
        let product_b = DownloadManifest {
            entries: vec![(shared_neutral_blob, 3_000)],
            tags: vec![],
        };

        let (removable, offered) = compute_footprint(&[product_a, product_b], &index, "enUS");

        // Only the deDE-only blob is removable; the shared blob is needed
        // because product B has no locale tag on it at all, and the enUS
        // blob is needed because enUS is kept.
        assert_eq!(removable, 1_000);
        assert_eq!(offered, vec!["deDE".to_string(), "enUS".to_string()]);
    }

    #[test]
    fn accounting_ignores_blobs_absent_from_the_local_index() {
        let on_disk = [1u8; 9];
        let not_on_disk = [2u8; 9];
        let mut index = HashMap::new();
        index.insert(
            on_disk,
            IndexEntry {
                archive: 0,
                offset: 0,
                size: 0,
            },
        );

        let manifest = DownloadManifest {
            entries: vec![(on_disk, 500), (not_on_disk, 999_999)],
            tags: vec![("deDE".to_string(), 2, vec![0b1100_0000])],
        };

        let (removable, _) = compute_footprint(&[manifest], &index, "enUS");
        // The huge `not_on_disk` size must never be counted - it is not
        // actually stored locally, so removing it frees nothing.
        assert_eq!(removable, 500);
    }

    #[test]
    fn hex_decode_round_trips_and_rejects_bad_input() {
        assert_eq!(hex_decode("00ff10").unwrap(), vec![0x00, 0xff, 0x10]);
        assert!(hex_decode("odd").is_none());
        assert!(hex_decode("zz").is_none());
    }
}
