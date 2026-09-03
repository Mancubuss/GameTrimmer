//! Rockstar Games Launcher discovery: registry subkeys under
//! `HKLM\SOFTWARE\WOW6432Node\Rockstar Games\<GameName>` -> `InstallFolder`.
//!
//! The subkey name is the human-readable game title (e.g. "Grand Theft Auto V").
//! The launcher itself and the Social Club runtime live under the same root
//! and also carry an `InstallFolder` - they are excluded by name.

use std::path::PathBuf;

use winreg::enums::HKEY_LOCAL_MACHINE;
use winreg::RegKey;

use crate::error::Result;

use super::{
    degrades_evidence, diagnostic, DiscoveredLibrary, DiscoveryDiagnostic, DiscoveryReport,
    DiscoveryStatus, GameInstall, LibraryProvider, OrphanEvidence, GAME_ABSENT,
};

const REGISTRY_KEY: &str = r"SOFTWARE\WOW6432Node\Rockstar Games";

/// Subkeys under the Rockstar root that are launcher infrastructure, not games.
const NON_GAME_SUBKEYS: &[&str] = &["Launcher", "Rockstar Games Social Club"];

pub struct RockstarProvider;

impl LibraryProvider for RockstarProvider {
    fn name(&self) -> &'static str {
        "rockstar"
    }

    fn try_discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        Ok(discover_rockstar().data)
    }

    fn discover(&self) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
        discover_rockstar()
    }
}

fn discover_rockstar() -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let root_key = match hklm.open_subkey(REGISTRY_KEY) {
        Ok(key) => key,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return DiscoveryReport::not_installed(Vec::new())
        }
        Err(err) => {
            return DiscoveryReport::failed(
                Vec::new(),
                diagnostic("rockstar", "registry-open", None, err),
            )
        }
    };
    let mut games = Vec::new();
    let mut diagnostics = Vec::new();
    for name in root_key.enum_keys() {
        let name = match name {
            Ok(name) => name,
            Err(err) => {
                diagnostics.push(diagnostic("rockstar", "registry-enumeration", None, err));
                continue;
            }
        };
        let subkey = match root_key.open_subkey(&name) {
            Ok(key) => key,
            Err(err) => {
                diagnostics.push(diagnostic("rockstar", "game-key-open", None, err));
                continue;
            }
        };
        // A missing `InstallFolder` is ordinary - a leftover subkey, or one
        // of the non-game entries `build_game_install` already filters by
        // name - and must not be conflated with a genuine read failure
        // (permissions, a corrupt hive), which is the case that actually
        // needs to keep degrading this provider's evidence.
        let install_folder = match subkey.get_value::<String, _>("InstallFolder") {
            Ok(value) => Some(value),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                diagnostics.push(diagnostic("rockstar", "game-value-read", None, err));
                continue;
            }
        };
        // Whatever `build_game_install` rejects at this point is already an
        // ordinary case (launcher infrastructure by name, or no usable
        // install folder), not a failure worth diagnosing.
        let Some(game) = build_game_install(&name, install_folder) else {
            continue;
        };
        // A directory that isn't there is ordinary - the registry key
        // survived an uninstall that left it behind - and nothing absent can
        // be mistaken for residue. A directory that could not be *examined*
        // is the dangerous shape and stays diagnosed; `super::try_is_dir`
        // explains why. For Rockstar that hazard is latent rather than live:
        // orphan detection is wired to Steam, Xbox and itch only
        // (`orphan_spec_for`). Splitting it here keeps the guarantee true
        // ahead of that changing, and stops an ordinary uninstall from
        // degrading the whole provider meanwhile.
        match super::try_is_dir(&game.install_dir) {
            Ok(true) => games.push(game),
            // Recorded, but explicitly not degrading - see `GAME_ABSENT`.
            Ok(false) => diagnostics.push(DiscoveryDiagnostic {
                provider: "rockstar",
                stage: GAME_ABSENT,
                path: Some(game.install_dir),
                message:
                    "registry key present, install directory absent (uninstalled without cleanup)"
                        .into(),
            }),
            Err(err) => diagnostics.push(DiscoveryDiagnostic {
                provider: "rockstar",
                stage: "game-path",
                path: Some(game.install_dir),
                message: err.to_string(),
            }),
        }
    }
    finish_report("rockstar", games, diagnostics)
}

fn finish_report(
    provider: &'static str,
    games: Vec<GameInstall>,
    diagnostics: Vec<DiscoveryDiagnostic>,
) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let mut libraries = super::group_by_parent_dir(provider, games);
    if degrades_evidence(&diagnostics) {
        for library in &mut libraries {
            library.orphan_evidence = OrphanEvidence::Degraded;
        }
        DiscoveryReport::degraded(libraries, diagnostics)
    } else {
        // Complete, but not necessarily silent: a `GAME_ABSENT` note still
        // travels so it reaches the log and `scan_diagnostics`.
        // `DiscoveryReport::complete` would drop it.
        DiscoveryReport {
            data: libraries,
            status: DiscoveryStatus::Complete,
            diagnostics,
        }
    }
}
/// Builds a `GameInstall` from a raw registry entry: `name` is the subkey name
/// (the game title), `install_folder` is the `InstallFolder` value if present.
/// Returns `None` for launcher-infrastructure subkeys and for entries without
/// a usable install folder.
fn build_game_install(name: &str, install_folder: Option<String>) -> Option<GameInstall> {
    if NON_GAME_SUBKEYS
        .iter()
        .any(|excluded| excluded.eq_ignore_ascii_case(name))
    {
        return None;
    }

    let install_folder = install_folder.filter(|s| !s.trim().is_empty())?;

    Some(GameInstall {
        name: name.to_string(),
        install_dir: PathBuf::from(install_folder),
        app_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_game_install_reads_title_and_install_folder() {
        let game = build_game_install(
            "Grand Theft Auto V",
            Some(r"F:\Rockstar Games\Grand Theft Auto V".to_string()),
        )
        .expect("expected a parsed game");

        assert_eq!(game.name, "Grand Theft Auto V");
        assert_eq!(game.app_id, None);
        assert_eq!(
            game.install_dir,
            PathBuf::from(r"F:\Rockstar Games\Grand Theft Auto V")
        );
    }

    #[test]
    fn build_game_install_excludes_launcher_infrastructure() {
        assert!(build_game_install("Launcher", Some(r"C:\Rockstar".to_string())).is_none());
        assert!(build_game_install(
            "Rockstar Games Social Club",
            Some(r"C:\Rockstar".to_string())
        )
        .is_none());
    }

    #[test]
    fn build_game_install_returns_none_when_install_folder_missing() {
        assert!(build_game_install("Grand Theft Auto V", None).is_none());
    }

    #[test]
    fn build_game_install_returns_none_when_install_folder_empty() {
        assert!(build_game_install("Grand Theft Auto V", Some("  ".to_string())).is_none());
    }
}
