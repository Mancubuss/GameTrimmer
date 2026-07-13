//! Fast, Everything-style volume index scanning.
//!
//! Reads a game library's files straight out of the NTFS Master File Table
//! (MFT) in one linear pass per volume, instead of walking each game's
//! directory tree individually with `readdir`/`FindNextFile` calls. This
//! mirrors the technique the "Everything" search tool uses.
//!
//! This requires raw read access to `\\.\<letter>:`, which in turn requires
//! Administrator privileges (or `SeBackupPrivilege`) - see [`is_available`].
//! When unavailable, [`scan_roots`] returns a clear [`CoreError::Other`] for
//! the affected games so the caller can fall back to
//! [`crate::scanner::scan_games_parallel`] (a regular `walkdir` scan).
//!
//! # Module layout
//! - [`model`]: pure FRN/record data types, no I/O.
//! - [`pathmap`]: pure path reconstruction + root filtering (unit tested
//!   with synthetic data - the part of this module worth testing in depth).
//! - [`volume`]: raw volume handle opening (Windows-specific).
//! - [`reader`]: the single linear MFT-scan pass that builds a
//!   [`model::FrnMap`] (Windows- and `ntfs`-crate-specific).

mod model;
mod pathmap;
mod reader;
mod volume;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{CoreError, Result};
use crate::scanner::FileEntry;

pub use volume::is_available;

/// Scans `roots` (game id + absolute install directory) using direct MFT
/// reads. Mirrors [`crate::scanner::scan_games_parallel`]'s per-game result
/// shape so callers can swap between the two implementations transparently.
///
/// Roots are grouped by drive letter so each NTFS volume is read exactly
/// once, no matter how many games in `roots` live on it. If a volume cannot
/// be opened (not NTFS, or insufficient privileges), every game on that
/// volume gets the same descriptive `Err` in its result slot - the caller
/// can then fall back to `scan_games_parallel` for just those games.
///
/// Roots that are not on a lettered local drive (e.g. a UNC path) are
/// silently omitted from the results, since there is no volume to scan;
/// callers should fall back to a regular walk for those too.
pub fn scan_roots(roots: &[(i64, PathBuf)]) -> Result<Vec<(i64, Result<Vec<FileEntry>>)>> {
    let mut by_volume: HashMap<char, Vec<(i64, PathBuf)>> = HashMap::new();

    for (game_id, path) in roots {
        if let Some(letter) = volume_letter(path) {
            by_volume
                .entry(letter)
                .or_default()
                .push((*game_id, path.clone()));
        }
    }

    let mut results = Vec::with_capacity(roots.len());

    for (letter, games) in by_volume {
        match scan_volume(letter, &games) {
            Ok(per_game) => results.extend(per_game),
            Err(err) => {
                for (game_id, _) in &games {
                    results.push((*game_id, Err(CoreError::Other(err.to_string()))));
                }
            }
        }
    }

    Ok(results)
}

/// Scans one NTFS volume for all games in `games`, all of which must be on
/// drive `letter`.
fn scan_volume(
    letter: char,
    games: &[(i64, PathBuf)],
) -> Result<Vec<(i64, Result<Vec<FileEntry>>)>> {
    let mut volume_file = volume::open_volume(letter)?;
    let frn_map = reader::build_frn_map(&mut volume_file)?;

    let scan_roots: Vec<pathmap::ScanRoot> = games
        .iter()
        .filter_map(|(game_id, path)| {
            root_rel_to_volume(path, letter).map(|root_rel| pathmap::ScanRoot {
                game_id: *game_id,
                root_rel,
            })
        })
        .collect();

    let by_game = pathmap::scan_frn_map(&frn_map, &scan_roots);
    Ok(by_game
        .into_iter()
        .map(|(game_id, entries)| (game_id, Ok(entries)))
        .collect())
}

/// Extracts the drive letter of an absolute Windows path, e.g. `'G'` from
/// `G:\SteamLibrary\Game`. Returns `None` for anything without a `<letter>:`
/// prefix (UNC paths, relative paths, ...).
fn volume_letter(path: &Path) -> Option<char> {
    let s = path.to_str()?;
    let mut chars = s.chars();
    let letter = chars.next()?;
    if letter.is_ascii_alphabetic() && chars.next() == Some(':') {
        Some(letter.to_ascii_uppercase())
    } else {
        None
    }
}

/// Strips the `<letter>:` prefix and any leading separator from `path`,
/// returning the remainder with `\` separators. Returns `None` if `path` is
/// not on `letter`.
fn root_rel_to_volume(path: &Path, letter: char) -> Option<String> {
    let s = path.to_str()?;
    if volume_letter(path)? != letter {
        return None;
    }
    // Safe to byte-slice at index 2: `volume_letter` already confirmed the
    // first two characters are an ASCII letter followed by ':', both
    // single-byte in UTF-8.
    let rest = s[2..].trim_start_matches(['\\', '/']);
    Some(rest.replace('/', "\\"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_letter_extracts_uppercase_drive_letter() {
        assert_eq!(volume_letter(Path::new(r"g:\SteamLibrary")), Some('G'));
        assert_eq!(volume_letter(Path::new(r"C:\Games")), Some('C'));
    }

    #[test]
    fn volume_letter_none_for_unc_or_relative_paths() {
        assert_eq!(volume_letter(Path::new(r"\\server\share\Games")), None);
        assert_eq!(volume_letter(Path::new("relative/path")), None);
    }

    #[test]
    fn root_rel_to_volume_strips_drive_and_normalizes_separators() {
        assert_eq!(
            root_rel_to_volume(Path::new(r"G:\SteamLibrary\HalfLife"), 'G'),
            Some("SteamLibrary\\HalfLife".to_string())
        );
    }

    #[test]
    fn root_rel_to_volume_none_for_wrong_drive() {
        assert_eq!(
            root_rel_to_volume(Path::new(r"G:\SteamLibrary\HalfLife"), 'D'),
            None
        );
    }
}
