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

use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::error::Result;

use super::{
    DiscoveredLibrary, DiscoveryDiagnostic, DiscoveryReport, GameInstall, LibraryProvider,
    OrphanEvidence,
};

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

    fn try_discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        Ok(discover_xbox_from_roots(drive_letters().map(PathBuf::from)).data)
    }

    fn discover(&self) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
        discover_xbox_from_roots(drive_letters().map(PathBuf::from))
    }
}

fn diagnostic(
    stage: &'static str,
    path: impl Into<Option<PathBuf>>,
    message: impl std::fmt::Display,
) -> DiscoveryDiagnostic {
    DiscoveryDiagnostic {
        provider: "xbox",
        stage,
        path: path.into(),
        message: message.to_string(),
    }
}

fn discover_xbox_from_roots(
    drive_roots: impl IntoIterator<Item = PathBuf>,
) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let mut diagnostics = Vec::new();
    let mut libraries = Vec::new();
    let mut seen_roots = HashSet::new();
    let mut launcher_evidence_found = false;

    for drive in drive_roots {
        let marker = drive.join(GAMING_ROOT_FILE);
        let bytes = match std::fs::read(&marker) {
            Ok(bytes) => {
                launcher_evidence_found = true;
                bytes
            }
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => {
                diagnostics.push(diagnostic("gaming-root-read", marker, err));
                continue;
            }
        };
        let relative_roots = match parse_gaming_root_checked(&bytes) {
            Ok(roots) => roots,
            Err(err) => {
                diagnostics.push(diagnostic("gaming-root-parse", marker, err));
                continue;
            }
        };

        for relative in relative_roots {
            let relative = match crate::safety::normalize_relative_path(&relative) {
                Ok(relative) => relative,
                Err(err) => {
                    diagnostics.push(diagnostic("library-path", marker.clone(), err));
                    continue;
                }
            };
            let root = drive.join(relative);
            let key = root.to_string_lossy().to_lowercase();
            if !seen_roots.insert(key) {
                continue;
            }
            let (games, mut root_diagnostics) = read_root_games_report(&root);
            diagnostics.append(&mut root_diagnostics);
            libraries.push(DiscoveredLibrary {
                vendor: "xbox",
                path: root,
                games,
                orphan_evidence: OrphanEvidence::Authoritative,
            });
        }
    }

    if !diagnostics.is_empty() {
        for library in &mut libraries {
            library.orphan_evidence = OrphanEvidence::Degraded;
        }
        DiscoveryReport::degraded(libraries, diagnostics)
    } else if launcher_evidence_found {
        DiscoveryReport::complete(libraries)
    } else {
        DiscoveryReport::not_installed(libraries)
    }
}

/// All possible drive roots (`A:\` .. `Z:\`); nonexistent drives fail the
/// subsequent `.GamingRoot` read and are skipped.
fn drive_letters() -> impl Iterator<Item = String> {
    (b'A'..=b'Z').map(|letter| format!(r"{}:\", letter as char))
}

/// Parses the binary `.GamingRoot` format into relative library paths.
/// Returns an empty list for anything that doesn't match the format.
fn parse_gaming_root_checked(bytes: &[u8]) -> std::result::Result<Vec<String>, &'static str> {
    if bytes.len() < 8 || &bytes[0..4] != GAMING_ROOT_MAGIC {
        return Err("invalid or truncated .GamingRoot header");
    }

    let count = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    if count == 0 || count > MAX_ROOTS_PER_DRIVE {
        return Err("invalid .GamingRoot folder count");
    }
    if !(bytes.len() - 8).is_multiple_of(2) {
        return Err("truncated UTF-16 path data in .GamingRoot");
    }

    let code_units: Vec<u16> = bytes[8..]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();

    let roots: std::result::Result<Vec<_>, _> = code_units
        .split(|&unit| unit == 0)
        .filter(|chunk| !chunk.is_empty())
        .take(count)
        .map(String::from_utf16)
        .collect();
    let roots = roots.map_err(|_| "invalid UTF-16 path in .GamingRoot")?;
    if roots.len() != count {
        return Err(".GamingRoot declares more paths than it contains");
    }
    Ok(roots)
}

#[cfg(test)]
fn parse_gaming_root(bytes: &[u8]) -> Vec<String> {
    parse_gaming_root_checked(bytes).unwrap_or_default()
}

/// Enumerates the game folders directly under one Xbox library root.
fn read_root_games_report(root: &Path) -> (Vec<GameInstall>, Vec<DiscoveryDiagnostic>) {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) => {
            return (
                Vec::new(),
                vec![diagnostic("library-enumeration", root.to_path_buf(), err)],
            )
        }
    };
    let mut games = Vec::new();
    let mut diagnostics = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                diagnostics.push(diagnostic("library-entry", root.to_path_buf(), err));
                continue;
            }
        };
        let game_dir = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) => {
                diagnostics.push(diagnostic("entry-type", game_dir, err));
                continue;
            }
        };
        if !file_type.is_dir() {
            continue;
        }
        // The Xbox root is a structurally exclusive orphan container: every
        // subfolder that is not a recognized game becomes residue. So a probe
        // that fails must not answer "not a game" - that is the difference
        // between skipping an unrelated folder and offering a live, protected
        // installation for deletion.
        match looks_like_xbox_game(&game_dir) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(err) => {
                diagnostics.push(diagnostic("game-probe", game_dir, err));
                continue;
            }
        }
        let (name, mut name_diagnostics) = display_name_for(&game_dir);
        diagnostics.append(&mut name_diagnostics);
        let Some(name) = name else {
            diagnostics.push(diagnostic(
                "game-name",
                game_dir,
                "game directory has no usable file name",
            ));
            continue;
        };
        games.push(GameInstall {
            name,
            install_dir: game_dir,
            app_id: None,
        });
    }
    (games, diagnostics)
}

/// A game folder holds either a `Content` payload dir (the usual layout) or
/// a top-level `MicrosoftGame.config`.
///
/// Returns `Err` when neither marker could be examined, so the caller can
/// degrade the library instead of reading an unreadable folder as "not a
/// game" - see [`super::try_is_dir`].
fn looks_like_xbox_game(dir: &Path) -> std::io::Result<bool> {
    if super::try_is_dir(&dir.join("Content"))? {
        return Ok(true);
    }
    super::try_is_file(&dir.join("MicrosoftGame.config"))
}

/// Best-effort display name: `MicrosoftGame.config` first, folder name as
/// fallback. Returns `None` only for a nameless path (never for real dirs).
fn display_name_for(game_dir: &Path) -> (Option<String>, Vec<DiscoveryDiagnostic>) {
    let fallback = game_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let mut diagnostics = Vec::new();

    for config in [
        game_dir.join("Content").join("MicrosoftGame.config"),
        game_dir.join("MicrosoftGame.config"),
    ] {
        match std::fs::read_to_string(&config) {
            Ok(xml) => {
                if let Some(name) = extract_display_name(&xml) {
                    return (Some(name), diagnostics);
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => diagnostics.push(diagnostic("game-config-read", config, err)),
        }
    }

    (fallback, diagnostics)
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

    #[test]
    fn malformed_gaming_root_degrades_the_provider() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(GAMING_ROOT_FILE), b"RGBX").unwrap();

        let report = discover_xbox_from_roots([temp.path().to_path_buf()]);

        assert_eq!(report.status, super::super::DiscoveryStatus::Degraded);
        assert!(report.data.is_empty());
        assert_eq!(report.diagnostics[0].stage, "gaming-root-parse");
    }

    #[test]
    fn unreadable_declared_library_never_authorizes_orphans() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(GAMING_ROOT_FILE),
            gaming_root_bytes(&["missing-library"]),
        )
        .unwrap();

        let report = discover_xbox_from_roots([temp.path().to_path_buf()]);

        assert_eq!(report.status, super::super::DiscoveryStatus::Degraded);
        assert_eq!(report.data.len(), 1);
        assert_eq!(report.data[0].orphan_evidence, OrphanEvidence::Degraded);
        assert_eq!(report.diagnostics[0].stage, "library-enumeration");
    }

    /// The Xbox root is an exclusive orphan container, so "could not tell
    /// whether this is a game" must never collapse into "not a game" - that
    /// would offer a live, unreadable installation up as residue.
    #[test]
    fn an_unexaminable_game_probe_is_an_error_not_a_no() {
        let temp = tempfile::tempdir().unwrap();
        let unexaminable = temp.path().join("bad<name");

        assert!(
            looks_like_xbox_game(&unexaminable).is_err(),
            "a failed probe must not answer `false`"
        );
    }

    #[test]
    fn a_folder_without_either_marker_is_not_a_game() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!looks_like_xbox_game(temp.path()).unwrap());

        std::fs::create_dir(temp.path().join("Content")).unwrap();
        assert!(looks_like_xbox_game(temp.path()).unwrap());
    }
}
