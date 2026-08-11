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
    DiscoveredLibrary, DiscoveryDiagnostic, DiscoveryReport, GameInstall, LibraryProvider,
    OrphanEvidence,
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
        Err(err) => return DiscoveryReport::failed(Vec::new(), diagnostic("registry-open", err)),
    };
    let mut games = Vec::new();
    let mut diagnostics = Vec::new();
    for name in root_key.enum_keys() {
        let name = match name {
            Ok(name) => name,
            Err(err) => {
                diagnostics.push(diagnostic("registry-enumeration", err));
                continue;
            }
        };
        let subkey = match root_key.open_subkey(&name) {
            Ok(key) => key,
            Err(err) => {
                diagnostics.push(diagnostic("game-key-open", err));
                continue;
            }
        };
        let install_folder = subkey.get_value::<String, _>("InstallFolder").ok();
        let Some(game) = build_game_install(&name, install_folder) else {
            if !NON_GAME_SUBKEYS
                .iter()
                .any(|excluded| excluded.eq_ignore_ascii_case(&name))
            {
                diagnostics.push(diagnostic(
                    "game-entry",
                    format!("{name} has no usable InstallFolder"),
                ));
            }
            continue;
        };
        if game.install_dir.is_dir() {
            games.push(game);
        } else {
            diagnostics.push(DiscoveryDiagnostic {
                provider: "rockstar",
                stage: "game-path",
                path: Some(game.install_dir),
                message: "configured Rockstar install is unavailable".into(),
            });
        }
    }
    finish_report("rockstar", games, diagnostics)
}

fn diagnostic(stage: &'static str, message: impl std::fmt::Display) -> DiscoveryDiagnostic {
    DiscoveryDiagnostic {
        provider: "rockstar",
        stage,
        path: None,
        message: message.to_string(),
    }
}

fn finish_report(
    provider: &'static str,
    games: Vec<GameInstall>,
    diagnostics: Vec<DiscoveryDiagnostic>,
) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let mut libraries = super::group_by_parent_dir(provider, games);
    if diagnostics.is_empty() {
        DiscoveryReport::complete(libraries)
    } else {
        for library in &mut libraries {
            library.orphan_evidence = OrphanEvidence::Degraded;
        }
        DiscoveryReport::degraded(libraries, diagnostics)
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
