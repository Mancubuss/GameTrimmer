//! Battle.net (Blizzard) library discovery via the Windows uninstall registry.
//!
//! Battle.net's own install database (`C:\ProgramData\Battle.net\Agent\
//! product.db`) is a protobuf blob without a published schema, but the client
//! reliably registers every installed game under the standard uninstall keys
//! with `Publisher = "Blizzard Entertainment"`, `DisplayName` and
//! `InstallLocation` - so those are read instead. All three uninstall roots
//! (HKLM 64-bit, HKLM 32-bit, HKCU) are scanned and de-duplicated by
//! install directory.

use std::path::PathBuf;

use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winreg::RegKey;

use crate::error::Result;

use super::{
    degrades_evidence, diagnostic, DiscoveredLibrary, DiscoveryDiagnostic, DiscoveryReport,
    DiscoveryStatus, GameInstall, LibraryProvider, OrphanEvidence, GAME_ABSENT,
};

const PUBLISHER: &str = "Blizzard Entertainment";

/// Uninstall entries that belong to launcher infrastructure, not games.
const NON_GAME_NAMES: &[&str] = &["Battle.net"];

pub struct BattleNetProvider;

impl LibraryProvider for BattleNetProvider {
    fn name(&self) -> &'static str {
        "battlenet"
    }

    fn try_discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        Ok(discover_battlenet().data)
    }

    fn discover(&self) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
        discover_battlenet()
    }
}

fn discover_battlenet() -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let mut games = Vec::new();
    let mut diagnostics = Vec::new();
    for (root, path) in uninstall_roots() {
        let uninstall_key = match root.open_subkey(path) {
            Ok(key) => key,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                diagnostics.push(diagnostic("battlenet", "registry-root-open", None, err));
                continue;
            }
        };
        for key_name in uninstall_key.enum_keys() {
            let key_name = match key_name {
                Ok(name) => name,
                Err(err) => {
                    diagnostics.push(diagnostic("battlenet", "registry-enumeration", None, err));
                    continue;
                }
            };
            let subkey = match uninstall_key.open_subkey(&key_name) {
                Ok(key) => key,
                Err(err) => {
                    diagnostics.push(diagnostic("battlenet", "game-key-open", None, err));
                    continue;
                }
            };
            let publisher = match subkey.get_value::<String, _>("Publisher") {
                Ok(publisher) => publisher,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    diagnostics.push(diagnostic("battlenet", "publisher-read", None, err));
                    continue;
                }
            };
            if publisher.trim() != PUBLISHER {
                continue;
            }
            // A missing `InstallLocation` is ordinary - the uninstall entry
            // is for launcher infrastructure or a leftover record with
            // nothing installed - and must not be conflated with a genuine
            // read failure (permissions, a corrupt hive), which is the case
            // that actually needs to keep degrading this provider's
            // evidence.
            let install_location = match subkey.get_value::<String, _>("InstallLocation") {
                Ok(value) => Some(value),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                Err(err) => {
                    diagnostics.push(diagnostic("battlenet", "game-value-read", None, err));
                    continue;
                }
            };
            let entry = RawUninstallEntry {
                key_name: key_name.clone(),
                display_name: subkey.get_value::<String, _>("DisplayName").ok(),
                publisher: Some(publisher),
                install_location,
            };
            // Whatever `build_game_install` rejects at this point is already
            // an ordinary case (no install location, an empty one, or the
            // Battle.net client entry itself via `NON_GAME_NAMES`), not a
            // failure worth diagnosing.
            let Some(game) = build_game_install(entry) else {
                continue;
            };
            // A directory that isn't there is ordinary - the uninstall entry
            // survived a manual removal, or Battle.net left the registry
            // record behind - and nothing absent can be mistaken for residue.
            // A directory that could not be *examined* is the dangerous shape
            // and stays diagnosed; `super::try_is_dir` explains why. For
            // Battle.net that hazard is latent rather than live: orphan
            // detection is wired to Steam, Xbox and itch only
            // (`orphan_spec_for`). Splitting it here keeps the guarantee true
            // ahead of that changing, and stops an ordinary uninstall from
            // degrading the whole provider meanwhile.
            match super::try_is_dir(&game.install_dir) {
                Ok(true) => games.push(game),
                // Recorded, but explicitly not degrading - see `GAME_ABSENT`.
                Ok(false) => diagnostics.push(DiscoveryDiagnostic {
                    provider: "battlenet",
                    stage: GAME_ABSENT,
                    path: Some(game.install_dir),
                    message: "uninstall entry present, install directory absent (uninstalled without cleanup)".into(),
                }),
                Err(err) => diagnostics.push(DiscoveryDiagnostic {
                    provider: "battlenet",
                    stage: "game-path",
                    path: Some(game.install_dir),
                    message: err.to_string(),
                }),
            }
        }
    }
    let games = super::dedupe_by_install_dir(games);
    let mut libraries = super::group_by_parent_dir("battlenet", games);
    if degrades_evidence(&diagnostics) {
        for library in &mut libraries {
            library.orphan_evidence = OrphanEvidence::Degraded;
        }
        DiscoveryReport::degraded(libraries, diagnostics)
    } else if libraries.is_empty() && diagnostics.is_empty() {
        DiscoveryReport::not_installed(libraries)
    } else {
        // Complete, but not necessarily silent: a `GAME_ABSENT` note still
        // travels so it reaches the log and `scan_diagnostics` - including
        // the case where every entry found turned out absent and no library
        // survived. `DiscoveryReport::complete`/`not_installed` would drop it.
        DiscoveryReport {
            data: libraries,
            status: DiscoveryStatus::Complete,
            diagnostics,
        }
    }
}

/// The three registry roots where Windows uninstall entries live.
fn uninstall_roots() -> Vec<(RegKey, &'static str)> {
    vec![
        (
            RegKey::predef(HKEY_LOCAL_MACHINE),
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
        (
            RegKey::predef(HKEY_LOCAL_MACHINE),
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
        (
            RegKey::predef(HKEY_CURRENT_USER),
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
    ]
}

/// Reads every uninstall subkey under one root and keeps the Blizzard games.
/// A missing root (possible for HKCU) is simply an empty result.
/// One raw uninstall registry entry (or a synthetic stand-in in tests).
struct RawUninstallEntry {
    key_name: String,
    display_name: Option<String>,
    publisher: Option<String>,
    install_location: Option<String>,
}

/// Builds a `GameInstall` from a raw uninstall entry. Returns `None` unless
/// the publisher is exactly Blizzard, an install location is present, and the
/// entry is not the Battle.net client itself.
fn build_game_install(entry: RawUninstallEntry) -> Option<GameInstall> {
    let publisher = entry.publisher?;
    if publisher.trim() != PUBLISHER {
        return None;
    }

    let install_location = entry.install_location.filter(|s| !s.trim().is_empty())?;
    let path = PathBuf::from(install_location.trim_end_matches(['\\', '/']));

    let name = entry
        .display_name
        .filter(|s| !s.trim().is_empty())
        .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))?;

    if NON_GAME_NAMES
        .iter()
        .any(|excluded| excluded.eq_ignore_ascii_case(name.trim()))
    {
        return None;
    }

    Some(GameInstall {
        name,
        install_dir: path,
        app_id: Some(entry.key_name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        key_name: &str,
        display_name: Option<&str>,
        publisher: Option<&str>,
        install_location: Option<&str>,
    ) -> RawUninstallEntry {
        RawUninstallEntry {
            key_name: key_name.to_string(),
            display_name: display_name.map(str::to_string),
            publisher: publisher.map(str::to_string),
            install_location: install_location.map(str::to_string),
        }
    }

    #[test]
    fn build_game_install_reads_blizzard_game() {
        let game = build_game_install(entry(
            "Overwatch",
            Some("Overwatch"),
            Some("Blizzard Entertainment"),
            Some(r"F:\Battle.net\Overwatch"),
        ))
        .expect("expected a parsed game");

        assert_eq!(game.name, "Overwatch");
        assert_eq!(game.app_id.as_deref(), Some("Overwatch"));
        assert_eq!(game.install_dir, PathBuf::from(r"F:\Battle.net\Overwatch"));
    }

    #[test]
    fn build_game_install_trims_trailing_slash_from_install_location() {
        let game = build_game_install(entry(
            "Diablo IV",
            Some("Diablo IV"),
            Some("Blizzard Entertainment"),
            Some(r"F:\Battle.net\Diablo IV\"),
        ))
        .expect("expected a parsed game");

        assert_eq!(game.install_dir, PathBuf::from(r"F:\Battle.net\Diablo IV"));
    }

    #[test]
    fn build_game_install_rejects_other_publishers() {
        assert!(build_game_install(entry(
            "SomeTool",
            Some("Some Tool"),
            Some("Contoso"),
            Some(r"C:\Program Files\SomeTool"),
        ))
        .is_none());
    }

    #[test]
    fn build_game_install_rejects_missing_publisher() {
        assert!(build_game_install(entry(
            "SomeTool",
            Some("Some Tool"),
            None,
            Some(r"C:\Program Files\SomeTool"),
        ))
        .is_none());
    }

    #[test]
    fn build_game_install_rejects_battlenet_client_itself() {
        assert!(build_game_install(entry(
            "Battle.net",
            Some("Battle.net"),
            Some("Blizzard Entertainment"),
            Some(r"C:\Program Files (x86)\Battle.net"),
        ))
        .is_none());
    }

    #[test]
    fn build_game_install_rejects_missing_install_location() {
        assert!(build_game_install(entry(
            "Overwatch",
            Some("Overwatch"),
            Some("Blizzard Entertainment"),
            None,
        ))
        .is_none());
    }

    #[test]
    fn build_game_install_falls_back_to_folder_name_when_display_name_missing() {
        let game = build_game_install(entry(
            "wow",
            None,
            Some("Blizzard Entertainment"),
            Some(r"F:\Battle.net\World of Warcraft"),
        ))
        .expect("expected a parsed game");

        assert_eq!(game.name, "World of Warcraft");
    }
}
