//! Ubisoft Connect library discovery: registry subkeys under
//! `HKLM\SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs\<id>`.
//!
//! Unlike Steam/Epic/GOG, Ubisoft's registry doesn't store a game name -
//! only an `InstallDir` value per subkey (the subkey name is Ubisoft's
//! internal numeric game id). The display name is derived from the last
//! path component of `InstallDir`.

use std::path::PathBuf;

use winreg::enums::HKEY_LOCAL_MACHINE;
use winreg::RegKey;

use crate::error::Result;

use super::{
    degrades_evidence, diagnostic, DiscoveredLibrary, DiscoveryDiagnostic, DiscoveryReport,
    DiscoveryStatus, GameInstall, LibraryProvider, OrphanEvidence, GAME_ABSENT,
};

const REGISTRY_KEY: &str = r"SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs";

pub struct UbisoftProvider;

impl LibraryProvider for UbisoftProvider {
    fn name(&self) -> &'static str {
        "ubisoft"
    }

    fn try_discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        Ok(discover_ubisoft().data)
    }

    fn discover(&self) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
        discover_ubisoft()
    }
}

fn discover_ubisoft() -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let installs_key = match hklm.open_subkey(REGISTRY_KEY) {
        Ok(key) => key,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return DiscoveryReport::not_installed(Vec::new())
        }
        Err(err) => {
            return DiscoveryReport::failed(
                Vec::new(),
                diagnostic("ubisoft", "registry-open", None, err),
            )
        }
    };
    let mut games = Vec::new();
    let mut diagnostics = Vec::new();
    for id in installs_key.enum_keys() {
        let id = match id {
            Ok(id) => id,
            Err(err) => {
                diagnostics.push(diagnostic("ubisoft", "registry-enumeration", None, err));
                continue;
            }
        };
        let subkey = match installs_key.open_subkey(&id) {
            Ok(key) => key,
            Err(err) => {
                diagnostics.push(diagnostic("ubisoft", "game-key-open", None, err));
                continue;
            }
        };
        // A missing `InstallDir` is ordinary - an uninstalled leftover
        // subkey, a DLC/edition record with nothing installed - and must
        // not be conflated with a genuine read failure (permissions, a
        // corrupt hive), which is the case that actually needs to keep
        // degrading this provider's evidence.
        let install_dir = match subkey.get_value::<String, _>("InstallDir") {
            Ok(value) => Some(value),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                diagnostics.push(diagnostic("ubisoft", "game-value-read", None, err));
                continue;
            }
        };
        // Whatever `build_game_install` rejects at this point is already an
        // ordinary case (no install dir, or one with no usable name), not a
        // failure worth diagnosing.
        let Some(game) = build_game_install(&id, install_dir) else {
            continue;
        };
        // A directory that isn't there is ordinary - Ubisoft left the
        // registry key behind after an uninstall, or the entry was never
        // finished - and nothing absent can be mistaken for residue. A
        // directory that could not be *examined* is the dangerous shape and
        // stays diagnosed; `super::try_is_dir` explains why. For Ubisoft that
        // hazard is latent rather than live: orphan detection is wired to
        // Steam, Xbox and itch only (`orphan_spec_for`). Splitting it here
        // keeps the guarantee true ahead of that changing, and stops an
        // ordinary uninstall from degrading the whole provider meanwhile.
        match super::try_is_dir(&game.install_dir) {
            Ok(true) => games.push(game),
            // Recorded, but explicitly not degrading - see `GAME_ABSENT`.
            Ok(false) => diagnostics.push(DiscoveryDiagnostic {
                provider: "ubisoft",
                stage: GAME_ABSENT,
                path: Some(game.install_dir),
                message:
                    "registry key present, install directory absent (uninstalled without cleanup)"
                        .into(),
            }),
            Err(err) => diagnostics.push(DiscoveryDiagnostic {
                provider: "ubisoft",
                stage: "game-path",
                path: Some(game.install_dir),
                message: err.to_string(),
            }),
        }
    }
    let mut libraries = super::group_by_parent_dir("ubisoft", games);
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

/// Builds a `GameInstall` from a raw registry entry: `id` is the subkey name
/// (Ubisoft's internal game id), `install_dir` is the `InstallDir` value if
/// present. Returns `None` when `install_dir` is missing/empty, or has no
/// final path component to use as a name (e.g. a bare drive root).
fn build_game_install(id: &str, install_dir: Option<String>) -> Option<GameInstall> {
    let install_dir = install_dir.filter(|s| !s.trim().is_empty())?;
    let path = PathBuf::from(install_dir);
    let name = path.file_name()?.to_string_lossy().into_owned();

    Some(GameInstall {
        name,
        install_dir: path,
        app_id: Some(id.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_game_install_names_game_after_install_dir_leaf() {
        let game = build_game_install(
            "5586",
            Some(r"F:\Ubisoft\Assassin's Creed Origins".to_string()),
        )
        .expect("expected a parsed game");

        assert_eq!(game.name, "Assassin's Creed Origins");
        assert_eq!(game.app_id.as_deref(), Some("5586"));
        assert_eq!(
            game.install_dir,
            PathBuf::from(r"F:\Ubisoft\Assassin's Creed Origins")
        );
    }

    #[test]
    fn build_game_install_returns_none_when_install_dir_missing() {
        assert!(build_game_install("5586", None).is_none());
    }

    #[test]
    fn build_game_install_returns_none_when_install_dir_empty() {
        assert!(build_game_install("5586", Some("".to_string())).is_none());
    }

    #[test]
    fn build_game_install_returns_none_for_bare_drive_root() {
        assert!(build_game_install("5586", Some(r"F:\".to_string())).is_none());
    }

    #[test]
    fn group_by_parent_dir_groups_synthetic_ubisoft_games_by_shared_parent() {
        let games = vec![
            build_game_install("1", Some(r"F:\Ubisoft\Alpha".to_string())).unwrap(),
            build_game_install("2", Some(r"F:\Ubisoft\Beta".to_string())).unwrap(),
        ];

        let libraries = super::super::group_by_parent_dir("ubisoft", games);

        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].vendor, "ubisoft");
        assert_eq!(libraries[0].path, PathBuf::from(r"F:\Ubisoft"));
        assert_eq!(libraries[0].games.len(), 2);
    }
}
