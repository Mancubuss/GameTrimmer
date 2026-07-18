//! Xbox / Microsoft Store (Game Pass) library discovery.
//!
//! Each drive that hosts an Xbox library carries a hidden `<drive>:\.GamingRoot`
//! file: magic bytes `RGBX`, a little-endian u32 folder count, then that many
//! null-terminated UTF-16LE relative paths (usually just `XboxGames`). Every
//! subfolder of such a root that has a `Content` payload dir (or a top-level
//! `MicrosoftGame.config`) is one installed game.
//!
//! Display names come from `Content\MicrosoftGame.config` when readable and
//! not an `ms-resource:` indirection; otherwise the folder name is used.
//! Note that the `Content` dirs are ACL-protected by the Gaming Services -
//! discovery works, but size scanning and trimming inside them may be denied
//! by the system.

use std::path::Path;

use crate::error::Result;

use super::{DiscoveredLibrary, GameInstall, LibraryProvider};

const GAMING_ROOT_FILE: &str = ".GamingRoot";
const GAMING_ROOT_MAGIC: &[u8; 4] = b"RGBX";

/// Upper bound on the folder count field - a defense against parsing a
/// corrupt/foreign file as thousands of roots.
const MAX_ROOTS_PER_DRIVE: usize = 64;

pub struct XboxProvider;

impl LibraryProvider for XboxProvider {
    fn name(&self) -> &'static str {
        "xbox"
    }

    fn discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        let games: Vec<GameInstall> = drive_letters()
            .filter_map(|drive| {
                let bytes = std::fs::read(Path::new(&drive).join(GAMING_ROOT_FILE)).ok()?;
                Some((drive, parse_gaming_root(&bytes)))
            })
            .flat_map(|(drive, relative_roots)| {
                relative_roots
                    .into_iter()
                    .map(move |relative| Path::new(&drive).join(relative.trim_start_matches('\\')))
            })
            .flat_map(|root| read_root_games(&root))
            .collect();

        Ok(super::group_by_parent_dir("xbox", games))
    }
}

/// All possible drive roots (`A:\` .. `Z:\`); nonexistent drives fail the
/// subsequent `.GamingRoot` read and are skipped.
fn drive_letters() -> impl Iterator<Item = String> {
    (b'A'..=b'Z').map(|letter| format!(r"{}:\", letter as char))
}

/// Parses the binary `.GamingRoot` format into relative library paths.
/// Returns an empty list for anything that doesn't match the format.
fn parse_gaming_root(bytes: &[u8]) -> Vec<String> {
    if bytes.len() < 8 || &bytes[0..4] != GAMING_ROOT_MAGIC {
        return Vec::new();
    }

    let count = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    if count == 0 || count > MAX_ROOTS_PER_DRIVE {
        return Vec::new();
    }

    let code_units: Vec<u16> = bytes[8..]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();

    code_units
        .split(|&unit| unit == 0)
        .filter(|chunk| !chunk.is_empty())
        .take(count)
        .map(String::from_utf16_lossy)
        .collect()
}

/// Enumerates the game folders directly under one Xbox library root.
fn read_root_games(root: &Path) -> Vec<GameInstall> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };

    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && looks_like_xbox_game(path))
        .filter_map(|game_dir| {
            let name = display_name_for(&game_dir)?;
            Some(GameInstall {
                name,
                install_dir: game_dir,
                app_id: None,
            })
        })
        .collect()
}

/// A game folder holds either a `Content` payload dir (the usual layout) or
/// a top-level `MicrosoftGame.config`.
fn looks_like_xbox_game(dir: &Path) -> bool {
    dir.join("Content").is_dir() || dir.join("MicrosoftGame.config").is_file()
}

/// Best-effort display name: `MicrosoftGame.config` first, folder name as
/// fallback. Returns `None` only for a nameless path (never for real dirs).
fn display_name_for(game_dir: &Path) -> Option<String> {
    let fallback = game_dir.file_name()?.to_string_lossy().into_owned();

    let name = [
        game_dir.join("Content").join("MicrosoftGame.config"),
        game_dir.join("MicrosoftGame.config"),
    ]
    .iter()
    .filter_map(|config| std::fs::read_to_string(config).ok())
    .find_map(|xml| extract_display_name(&xml));

    Some(name.unwrap_or(fallback))
}

/// Extracts `DefaultDisplayName="..."` from `MicrosoftGame.config` XML.
/// `ms-resource:` values are localization indirections we can't resolve -
/// treated as absent. Deliberately not a real XML parser - the attribute is
/// optional metadata, not load-bearing for discovery.
fn extract_display_name(xml: &str) -> Option<String> {
    let marker = "DefaultDisplayName=\"";
    let start = xml.find(marker)? + marker.len();
    let end = start + xml[start..].find('"')?;
    let name = xml[start..end].trim();

    (!name.is_empty() && !name.starts_with("ms-resource")).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds `.GamingRoot` bytes: magic, count, null-terminated UTF-16LE paths.
    fn gaming_root_bytes(paths: &[&str]) -> Vec<u8> {
        let mut bytes = Vec::from(*GAMING_ROOT_MAGIC);
        bytes.extend((paths.len() as u32).to_le_bytes());
        for path in paths {
            for unit in path.encode_utf16() {
                bytes.extend(unit.to_le_bytes());
            }
            bytes.extend(0u16.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn parse_gaming_root_reads_single_folder() {
        let bytes = gaming_root_bytes(&["XboxGames"]);
        assert_eq!(parse_gaming_root(&bytes), vec!["XboxGames".to_string()]);
    }

    #[test]
    fn parse_gaming_root_reads_multiple_folders() {
        let bytes = gaming_root_bytes(&["XboxGames", r"Games\Xbox"]);
        assert_eq!(
            parse_gaming_root(&bytes),
            vec!["XboxGames".to_string(), r"Games\Xbox".to_string()]
        );
    }

    #[test]
    fn parse_gaming_root_rejects_wrong_magic() {
        let mut bytes = gaming_root_bytes(&["XboxGames"]);
        bytes[0] = b'X';
        assert!(parse_gaming_root(&bytes).is_empty());
    }

    #[test]
    fn parse_gaming_root_rejects_truncated_input() {
        assert!(parse_gaming_root(b"RGBX").is_empty());
        assert!(parse_gaming_root(b"").is_empty());
    }

    #[test]
    fn parse_gaming_root_rejects_absurd_count() {
        let mut bytes = Vec::from(*GAMING_ROOT_MAGIC);
        bytes.extend(10_000u32.to_le_bytes());
        bytes.extend(0u16.to_le_bytes());
        assert!(parse_gaming_root(&bytes).is_empty());
    }

    #[test]
    fn extract_display_name_reads_attribute() {
        let xml =
            r#"<Game configVersion="1"><ShellVisuals DefaultDisplayName="Starfield" /></Game>"#;
        assert_eq!(extract_display_name(xml).as_deref(), Some("Starfield"));
    }

    #[test]
    fn extract_display_name_rejects_ms_resource_indirection() {
        let xml = r#"<ShellVisuals DefaultDisplayName="ms-resource:AppDisplayName" />"#;
        assert!(extract_display_name(xml).is_none());
    }

    #[test]
    fn extract_display_name_returns_none_when_absent() {
        assert!(extract_display_name("<Game></Game>").is_none());
    }
}
