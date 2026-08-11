//! EA library discovery from three sources, in order of how specific they are:
//!   - `HKLM\SOFTWARE\WOW6432Node\Origin Games\<id>` (legacy Origin)
//!   - `HKLM\SOFTWARE\WOW6432Node\Electronic Arts\EA Desktop\InstalledGames\<id>` (EA app)
//!   - the Windows uninstall registry, entries published by Electronic Arts
//!
//! The first two use `Install Dir` / `DisplayName` values per subkey. The third
//! exists because neither is reliable on a current EA app install: observed on
//! a machine with two EA games installed, `EA Desktop\InstalledGames` did not
//! exist at all and the `Origin Games` subkeys carried only `displayname` and
//! `locale` - no `Install Dir` - so discovery found nothing at all while both
//! games were plainly registered under the standard uninstall keys with a full
//! `InstallLocation`. The EA app's own install database
//! (`%ProgramData%\EA Desktop\<hash>\IS`) is encrypted, so the uninstall
//! registry is the readable source of truth, the same one `battlenet` uses.
//!
//! Games found in more than one source (e.g. after an Origin -> EA app
//! migration) are de-duplicated by install directory, first source winning.

use std::path::PathBuf;

use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winreg::RegKey;

use crate::error::Result;

use super::{
    DiscoveredLibrary, DiscoveryDiagnostic, DiscoveryReport, GameInstall, LibraryProvider,
    OrphanEvidence,
};

const ORIGIN_GAMES_KEY: &str = r"SOFTWARE\WOW6432Node\Origin Games";
const EA_DESKTOP_INSTALLED_GAMES_KEY: &str =
    r"SOFTWARE\WOW6432Node\Electronic Arts\EA Desktop\InstalledGames";

/// Uninstall entries are kept when `Publisher` starts with this - EA ships
/// both bare "Electronic Arts" and "Electronic Arts, Inc." on the same machine.
const PUBLISHER_PREFIX: &str = "Electronic Arts";

/// Uninstall entries that belong to launcher infrastructure, not games.
const NON_GAME_NAMES: &[&str] = &["EA app", "EA Desktop", "Origin", "EA Play"];

pub struct EaProvider;

impl LibraryProvider for EaProvider {
    fn name(&self) -> &'static str {
        "ea"
    }

    fn try_discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        Ok(discover_ea().data)
    }

    fn discover(&self) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
        discover_ea()
    }
}

#[derive(Default)]
struct ProviderRead {
    games: Vec<GameInstall>,
    diagnostics: Vec<DiscoveryDiagnostic>,
    ea_evidence_found: bool,
}

fn diagnostic(stage: &'static str, message: impl std::fmt::Display) -> DiscoveryDiagnostic {
    DiscoveryDiagnostic {
        provider: "ea",
        stage,
        path: None,
        message: message.to_string(),
    }
}

fn discover_ea() -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let mut report = read_registry_games_report(ORIGIN_GAMES_KEY);
    merge_read(
        &mut report,
        read_registry_games_report(EA_DESKTOP_INSTALLED_GAMES_KEY),
    );
    merge_read(&mut report, read_uninstall_games_report());

    let mut games = Vec::new();
    for game in super::dedupe_by_install_dir(report.games) {
        if game.install_dir.is_dir() {
            games.push(refine_name_from_installer_xml(game));
        } else {
            report.diagnostics.push(DiscoveryDiagnostic {
                provider: "ea",
                stage: "game-path",
                path: Some(game.install_dir),
                message: "configured EA install is unavailable".into(),
            });
        }
    }
    let mut libraries = super::group_by_parent_dir("ea", games);
    if report.diagnostics.is_empty() {
        if report.ea_evidence_found {
            DiscoveryReport::complete(libraries)
        } else {
            DiscoveryReport::not_installed(libraries)
        }
    } else {
        for library in &mut libraries {
            library.orphan_evidence = OrphanEvidence::Degraded;
        }
        DiscoveryReport::degraded(libraries, report.diagnostics)
    }
}

fn merge_read(target: &mut ProviderRead, mut incoming: ProviderRead) {
    target.games.append(&mut incoming.games);
    target.diagnostics.append(&mut incoming.diagnostics);
    target.ea_evidence_found |= incoming.ea_evidence_found;
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

/// Every EA-published game described by the Windows uninstall registry. A
/// missing root (possible for HKCU) contributes nothing.
fn read_uninstall_games_report() -> ProviderRead {
    let mut report = ProviderRead::default();
    for (root, path) in uninstall_roots() {
        let uninstall_key = match root.open_subkey(path) {
            Ok(key) => key,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                report
                    .diagnostics
                    .push(diagnostic("uninstall-root-open", err));
                continue;
            }
        };
        for key_name in uninstall_key.enum_keys() {
            let key_name = match key_name {
                Ok(name) => name,
                Err(err) => {
                    report
                        .diagnostics
                        .push(diagnostic("uninstall-enumeration", err));
                    continue;
                }
            };
            let subkey = match uninstall_key.open_subkey(&key_name) {
                Ok(key) => key,
                Err(err) => {
                    report
                        .diagnostics
                        .push(diagnostic("uninstall-entry-open", err));
                    continue;
                }
            };
            let publisher = match subkey.get_value::<String, _>("Publisher") {
                Ok(publisher) => publisher,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    report.diagnostics.push(diagnostic("publisher-read", err));
                    continue;
                }
            };
            if !publisher.trim().starts_with(PUBLISHER_PREFIX) {
                continue;
            }
            report.ea_evidence_found = true;
            let entry = RawUninstallEntry {
                key_name: key_name.clone(),
                display_name: subkey.get_value::<String, _>("DisplayName").ok(),
                publisher: Some(publisher),
                install_location: subkey.get_value::<String, _>("InstallLocation").ok(),
            };
            match build_from_uninstall_entry(entry) {
                Some(game) => report.games.push(game),
                None => {
                    let display_name = subkey
                        .get_value::<String, _>("DisplayName")
                        .unwrap_or_else(|_| key_name.clone());
                    if !NON_GAME_NAMES
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(display_name.trim()))
                    {
                        report.diagnostics.push(diagnostic(
                            "uninstall-entry",
                            format!("{key_name} has no usable InstallLocation"),
                        ));
                    }
                }
            }
        }
    }
    report
}

/// One raw uninstall registry entry (or a synthetic stand-in in tests).
struct RawUninstallEntry {
    key_name: String,
    display_name: Option<String>,
    publisher: Option<String>,
    install_location: Option<String>,
}

/// Builds a `GameInstall` from a raw uninstall entry. Returns `None` unless the
/// publisher is EA, an install location is present, and the entry is not one of
/// EA's own clients. The client entries are also the ones that carry an empty
/// `InstallLocation` in practice, so this is belt and braces.
fn build_from_uninstall_entry(entry: RawUninstallEntry) -> Option<GameInstall> {
    let publisher = entry.publisher?;
    if !publisher.trim().starts_with(PUBLISHER_PREFIX) {
        return None;
    }

    let install_location = entry.install_location.filter(|s| !s.trim().is_empty())?;
    let path = PathBuf::from(install_location.trim().trim_end_matches(['\\', '/']));

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

/// Reads every subkey under `registry_key` (`HKLM`-rooted) and builds the
/// `GameInstall`s it describes. Returns an empty vec if the key itself
/// doesn't exist - that's expected when this particular launcher generation
/// isn't installed.
fn read_registry_games_report(registry_key: &str) -> ProviderRead {
    let mut report = ProviderRead::default();
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let games_key = match hklm.open_subkey(registry_key) {
        Ok(key) => {
            report.ea_evidence_found = true;
            key
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return report,
        Err(err) => {
            report.diagnostics.push(diagnostic("games-root-open", err));
            return report;
        }
    };
    for id in games_key.enum_keys() {
        let id = match id {
            Ok(id) => id,
            Err(err) => {
                report
                    .diagnostics
                    .push(diagnostic("games-enumeration", err));
                continue;
            }
        };
        let subkey = match games_key.open_subkey(&id) {
            Ok(key) => key,
            Err(err) => {
                report.diagnostics.push(diagnostic("game-key-open", err));
                continue;
            }
        };
        let entry = RawEaEntry {
            id: id.clone(),
            display_name: subkey.get_value::<String, _>("DisplayName").ok(),
            install_dir: subkey.get_value::<String, _>("Install Dir").ok(),
        };
        match build_game_install(entry) {
            Some(game) => report.games.push(game),
            None => report.diagnostics.push(diagnostic(
                "game-entry",
                format!("{id} has no usable Install Dir"),
            )),
        }
    }
    report
}

/// One raw entry read from an `Origin Games`/`EA Desktop\InstalledGames`
/// subkey (or a synthetic stand-in in tests).
struct RawEaEntry {
    id: String,
    display_name: Option<String>,
    install_dir: Option<String>,
}

/// Builds a `GameInstall` from a raw registry entry. `install_dir` is
/// required; `display_name` falls back to the install directory's last path
/// component when absent (some EA app entries omit it).
fn build_game_install(entry: RawEaEntry) -> Option<GameInstall> {
    let install_dir = entry.install_dir.filter(|s| !s.trim().is_empty())?;
    let path = PathBuf::from(install_dir);

    let name = entry
        .display_name
        .filter(|s| !s.trim().is_empty())
        .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))?;

    Some(GameInstall {
        name,
        install_dir: path,
        app_id: Some(entry.id),
    })
}

/// Best-effort override of the display name using `<title>` from
/// `__Installer\installerdata.xml` inside the game's install directory, if
/// present. This is optional metadata sourced from EA's own installer
/// manifest - absence or any parse failure is silently ignored and the
/// original name is kept.
fn refine_name_from_installer_xml(game: GameInstall) -> GameInstall {
    let xml_path = game
        .install_dir
        .join("__Installer")
        .join("installerdata.xml");

    let Ok(contents) = std::fs::read_to_string(&xml_path) else {
        return game;
    };

    match extract_title(&contents) {
        Some(name) => GameInstall { name, ..game },
        None => game,
    }
}

/// Extracts the text of the first `<title>...</title>` element, if any.
/// Deliberately not a real XML parser - `installerdata.xml` is optional,
/// best-effort metadata, not load-bearing for discovery.
fn extract_title(xml: &str) -> Option<String> {
    let start_tag = "<title>";
    let end_tag = "</title>";

    let start = xml.find(start_tag)? + start_tag.len();
    let end = start + xml[start..].find(end_tag)?;
    let title = xml[start..end].trim();

    (!title.is_empty()).then(|| title.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_game_install_reads_display_name_and_install_dir() {
        let entry = RawEaEntry {
            id: "Origin.OFR.50.0001456".to_string(),
            display_name: Some("Battlefield 2042".to_string()),
            install_dir: Some(r"F:\EA Games\Battlefield 2042".to_string()),
        };

        let game = build_game_install(entry).expect("expected a parsed game");

        assert_eq!(game.name, "Battlefield 2042");
        assert_eq!(game.app_id.as_deref(), Some("Origin.OFR.50.0001456"));
        assert_eq!(
            game.install_dir,
            PathBuf::from(r"F:\EA Games\Battlefield 2042")
        );
    }

    #[test]
    fn build_game_install_falls_back_to_folder_name_when_display_name_missing() {
        let entry = RawEaEntry {
            id: "12345".to_string(),
            display_name: None,
            install_dir: Some(r"F:\EA Games\Apex Legends".to_string()),
        };

        let game = build_game_install(entry).expect("expected a parsed game");

        assert_eq!(game.name, "Apex Legends");
    }

    #[test]
    fn build_game_install_returns_none_when_install_dir_missing() {
        let entry = RawEaEntry {
            id: "12345".to_string(),
            display_name: Some("Broken".to_string()),
            install_dir: None,
        };
        assert!(build_game_install(entry).is_none());
    }

    #[test]
    fn build_game_install_returns_none_when_install_dir_empty() {
        let entry = RawEaEntry {
            id: "12345".to_string(),
            display_name: Some("Broken".to_string()),
            install_dir: Some("".to_string()),
        };
        assert!(build_game_install(entry).is_none());
    }

    fn uninstall_entry(
        display_name: Option<&str>,
        publisher: Option<&str>,
        install_location: Option<&str>,
    ) -> RawUninstallEntry {
        RawUninstallEntry {
            key_name: "{0767ae68-13a1-4e07-af66-6c1034a95f07}".to_string(),
            display_name: display_name.map(str::to_string),
            publisher: publisher.map(str::to_string),
            install_location: install_location.map(str::to_string),
        }
    }

    /// The exact shape observed on a real machine, trailing separator and all -
    /// the case that made EA discovery come up empty before this source existed.
    #[test]
    fn uninstall_entry_yields_the_game_and_trims_the_trailing_separator() {
        let game = build_from_uninstall_entry(uninstall_entry(
            Some("Battlefield 3\u{2122}"),
            Some("Electronic Arts"),
            Some(r"H:\EA\Battlefield 3\"),
        ))
        .expect("expected a parsed game");

        assert_eq!(game.name, "Battlefield 3\u{2122}");
        assert_eq!(game.install_dir, PathBuf::from(r"H:\EA\Battlefield 3"));
    }

    /// Both publisher spellings ship on the same machine.
    #[test]
    fn uninstall_entry_accepts_the_inc_suffixed_publisher() {
        assert!(build_from_uninstall_entry(uninstall_entry(
            Some("Dragon Age\u{2122}: Inquisition"),
            Some("Electronic Arts, Inc."),
            Some(r"F:\Games\Dragon Age Inquisition"),
        ))
        .is_some());
    }

    #[test]
    fn uninstall_entry_rejects_other_publishers() {
        assert!(build_from_uninstall_entry(uninstall_entry(
            Some("Some Game"),
            Some("Ubisoft"),
            Some(r"F:\Games\Some Game"),
        ))
        .is_none());
    }

    #[test]
    fn uninstall_entry_rejects_the_ea_client_itself() {
        assert!(build_from_uninstall_entry(uninstall_entry(
            Some("EA app"),
            Some("Electronic Arts"),
            Some(r"C:\Program Files\Electronic Arts\EA Desktop"),
        ))
        .is_none());
    }

    #[test]
    fn uninstall_entry_rejects_a_missing_or_empty_install_location() {
        assert!(build_from_uninstall_entry(uninstall_entry(
            Some("Some Game"),
            Some("Electronic Arts"),
            None,
        ))
        .is_none());
        assert!(build_from_uninstall_entry(uninstall_entry(
            Some("Some Game"),
            Some("Electronic Arts"),
            Some("   "),
        ))
        .is_none());
    }

    #[test]
    fn extract_title_reads_first_title_element() {
        let xml = r#"<?xml version="1.0"?><DiPManifest><gameTitles><title>Apex Legends</title></gameTitles></DiPManifest>"#;
        assert_eq!(extract_title(xml).as_deref(), Some("Apex Legends"));
    }

    #[test]
    fn extract_title_returns_none_when_absent() {
        assert!(extract_title("<DiPManifest></DiPManifest>").is_none());
    }

    #[test]
    fn extract_title_returns_none_when_empty() {
        assert!(extract_title("<title></title>").is_none());
    }

    #[test]
    fn group_by_parent_dir_groups_synthetic_ea_games_by_shared_parent() {
        let games = vec![
            build_game_install(RawEaEntry {
                id: "1".to_string(),
                display_name: Some("Alpha".to_string()),
                install_dir: Some(r"F:\EA Games\Alpha".to_string()),
            })
            .unwrap(),
            build_game_install(RawEaEntry {
                id: "2".to_string(),
                display_name: Some("Beta".to_string()),
                install_dir: Some(r"F:\EA Games\Beta".to_string()),
            })
            .unwrap(),
        ];

        let libraries = super::super::group_by_parent_dir("ea", games);

        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].vendor, "ea");
        assert_eq!(libraries[0].path, PathBuf::from(r"F:\EA Games"));
        assert_eq!(libraries[0].games.len(), 2);
    }
}
