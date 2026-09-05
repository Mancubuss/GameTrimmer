//! Ubisoft Connect library discovery: registry subkeys under
//! `HKLM\SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs\<id>`.
//!
//! Unlike Steam/Epic/GOG, Ubisoft's registry doesn't store a game name -
//! only an `InstallDir` value per subkey (the subkey name is Ubisoft's
//! internal numeric game id). The display name is derived from the last
//! path component of `InstallDir`.
//!
//! # GT-103: where the installed language and build actually live
//!
//! The card's premise - a `language` value sitting next to `InstallDir`
//! under `REGISTRY_KEY` - does not hold on a machine actually carrying
//! Ubisoft games. Verified directly: `Installs\1081` there has `language =
//! en-US` but no `InstallDir`; `Installs\6101` is empty; none of the ten
//! Ubisoft games on this machine (Syndicate, Unity, For Honor and seven
//! others, all under `F:\Ubisoft`) has an `InstallDir` entry at all, so
//! `discover_ubisoft` below finds zero of them - they were all found by the
//! generic folder scan instead (see the module's own note on that further
//! down). Per-title keys like `HKLM\...\Ubisoft\FarCry6\Language` exist for
//! some titles but are empty stubs, and titles actually installed here
//! (`Assassin's Creed Syndicate`, `Assassin's Creed Unity`, `ForHonor`) have
//! *no* per-title registry key whatsoever, live or stub.
//!
//! What every one of those ten install directories DOES have is a sidecar
//! file Ubisoft Connect itself writes and reads, `uplay_install.state`,
//! sitting right next to the game's `.exe`. It is a small Protobuf message
//! (no `.proto` shipped, reverse-engineered from the bytes on this machine)
//! whose first two length-delimited top-level fields are stable across
//! three unrelated titles sampled here:
//!
//! ```text
//! field 1 (tag 0x0a, len-delimited): a 40-char hex manifest/build id
//!         e.g. "EBD8CD1D9D65EF88823448F42B74AD961B0848846" (Syndicate)
//! field 2 (tag 0x10, varint):        a small enum-looking value, unidentified
//! field 3 (tag 0x1a, len-delimited): the installed language as a BCP-47
//!         locale, e.g. "en-US" - identical field position in Syndicate,
//!         Unity and For Honor's files despite unrelated content otherwise
//! ```
//!
//! Answering the card's three questions directly:
//!
//! (a) The installed language: recorded in `uplay_install.state`, field 3,
//!     as above. Confirmed real on this machine for all three sampled
//!     titles (all read "en-US", matching the single language actually
//!     installed). See `installed_languages` below.
//! (b) A local version/build identifier: field 1's 40-char hex string is
//!     real and present per-game, but it is a manifest/content-hash
//!     fingerprint, not a human version string ("1.5.2") - useful only to
//!     detect "did this game's build change since we last scanned it", not
//!     to display. See `build_ids` below.
//! (c) Tying a folder to a Ubisoft product id without the launcher running:
//!     NOT POSSIBLE from anything found here. The 40-char hex id in (b) is
//!     not the small numeric id the registry uses (`1081`, `6101` do not
//!     appear anywhere in any of the three `uplay_install.state` files,
//!     confirmed by byte search). The launcher's own catalog cache
//!     (`%LOCALAPPDATA%\Ubisoft Game Launcher\cache\configuration\configurations`,
//!     ~570 KB, holds every product's display name in plain text) might
//!     hold that mapping internally, but it is itself a nested
//!     binary/YAML blob with no plain-text numeric ids next to the names
//!     found in this investigation, needs the launcher to have synced once
//!     to exist at all, and reverse-engineering its layout is out of scope
//!     here. Documented as absent per the card's own allowance for that
//!     outcome. Because of this, `installed_languages` and `build_ids`
//!     below key their results by install directory - the one join key a
//!     caller (who already has that directory from folder-scan discovery)
//!     can actually use.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

/// The sidecar Ubisoft Connect writes into every game's own install
/// directory - see the module doc for how this was found and why it, not
/// the registry, is where GT-103's language/build questions get answered.
const INSTALL_STATE_FILE: &str = "uplay_install.state";

/// The two `uplay_install.state` fields GameTrimmer has an actual use for.
/// Either can be absent: a corrupt/truncated file (Protobuf field 1 or 3
/// missing or non-UTF8) is treated the same as a missing file - see
/// `read_install_state` - because this data is an enrichment on top of
/// discovery that already succeeded via the folder scan, not something
/// that should ever fail a scan by itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct UplayInstallState {
    /// Field 1: a manifest/build fingerprint (hex string). Not a human
    /// version number - see module doc part (b).
    manifest_id: Option<String>,
    /// Field 3: the installed language as a BCP-47 locale (e.g. "en-US").
    language: Option<String>,
}

/// Reads a Protobuf varint starting at `pos`. Returns `None` on running off
/// the end of `bytes` or a varint that never terminates within 64 bits (a
/// corrupt file) - both are ordinary parse failures here, not panics.
fn read_varint(bytes: &[u8], mut pos: usize) -> Option<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *bytes.get(pos)?;
        pos += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some((value, pos));
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

/// Walks the top-level fields of a `uplay_install.state` message far enough
/// to pull out fields 1 and 3 (see module doc), without a Protobuf
/// dependency for what is, at the two fields GameTrimmer needs, a varint
/// walk: read a tag, and either skip a scalar (wire types 0/1/5) or capture
/// a length-delimited payload (wire type 2) when its field number is 1 or
/// 3. Any other shape (an unsupported wire type, a length that runs past
/// the end of `bytes`) stops the walk and returns whatever was already
/// found - a format GameTrimmer doesn't recognize is not the same failure
/// as a file that isn't there, but it still isn't worth more than a
/// best-effort read for an enrichment feature.
fn parse_uplay_install_state(bytes: &[u8]) -> UplayInstallState {
    let mut state = UplayInstallState::default();
    let mut pos = 0usize;
    while pos < bytes.len() {
        let Some((tag, next)) = read_varint(bytes, pos) else {
            break;
        };
        pos = next;
        let field_number = tag >> 3;
        let wire_type = tag & 0x7;
        match wire_type {
            0 => {
                let Some((_, next)) = read_varint(bytes, pos) else {
                    break;
                };
                pos = next;
            }
            1 => pos += 8,
            2 => {
                let Some((len, next)) = read_varint(bytes, pos) else {
                    break;
                };
                pos = next;
                let len = len as usize;
                let Some(end) = pos.checked_add(len).filter(|&end| end <= bytes.len()) else {
                    break;
                };
                let payload = &bytes[pos..end];
                pos = end;
                match field_number {
                    1 if state.manifest_id.is_none() => {
                        state.manifest_id = Some(String::from_utf8_lossy(payload).into_owned());
                    }
                    3 if state.language.is_none() => {
                        state.language = Some(String::from_utf8_lossy(payload).into_owned());
                    }
                    _ => {}
                }
            }
            5 => pos += 4,
            _ => break,
        }
        if state.manifest_id.is_some() && state.language.is_some() {
            break;
        }
    }
    state
}

/// Reads `<install_dir>\uplay_install.state`, if it exists and parses. A
/// missing sidecar is ordinary - it is Ubisoft Connect's own bookkeeping
/// file, not something every install directory is guaranteed to carry (an
/// older install, a game moved by hand, one from a disc rip) - so this
/// returns `None` rather than an error, the same stance `discover_ubisoft`
/// already takes for a missing `InstallDir`.
fn read_install_state(install_dir: &Path) -> Option<UplayInstallState> {
    let bytes = std::fs::read(install_dir.join(INSTALL_STATE_FILE)).ok()?;
    Some(parse_uplay_install_state(&bytes))
}

/// The installed language for each of `install_dirs`, keyed by the
/// directory exactly as passed in. Only directories with a readable,
/// parseable `uplay_install.state` naming a language are present in the
/// result - see module doc part (a) and `read_install_state` for why an
/// absence here is ordinary, not a caller-visible error.
pub fn installed_languages(install_dirs: &[PathBuf]) -> HashMap<PathBuf, String> {
    install_dirs
        .iter()
        .filter_map(|dir| {
            let language = read_install_state(dir)?.language?;
            Some((dir.clone(), language))
        })
        .collect()
}

/// The manifest/build fingerprint for each of `install_dirs`, keyed by the
/// directory exactly as passed in. See module doc part (b): this is a hex
/// content-hash, useful for "did this build change", not a display
/// version. Same absence stance as `installed_languages`.
pub fn build_ids(install_dirs: &[PathBuf]) -> HashMap<PathBuf, String> {
    install_dirs
        .iter()
        .filter_map(|dir| {
            let manifest_id = read_install_state(dir)?.manifest_id?;
            Some((dir.clone(), manifest_id))
        })
        .collect()
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

    /// Builds a minimal `uplay_install.state`-shaped message: field 1 (the
    /// manifest id) then field 3 (the language), matching the byte layout
    /// observed on-machine (see module doc) closely enough to prove the
    /// parser reads the real shape, without needing the surrounding
    /// EULA/prerequisite fields this test doesn't care about.
    fn fake_install_state(manifest_id: &str, language: &str) -> Vec<u8> {
        let mut bytes = vec![0x0a, manifest_id.len() as u8];
        bytes.extend_from_slice(manifest_id.as_bytes());
        bytes.extend_from_slice(&[0x1a, language.len() as u8]);
        bytes.extend_from_slice(language.as_bytes());
        bytes
    }

    #[test]
    fn parse_uplay_install_state_reads_manifest_id_and_language() {
        let bytes = fake_install_state("EBD8CD1D9D65EF88823448F42B74AD961B0848846", "en-US");

        let state = parse_uplay_install_state(&bytes);

        assert_eq!(
            state.manifest_id.as_deref(),
            Some("EBD8CD1D9D65EF88823448F42B74AD961B0848846")
        );
        assert_eq!(state.language.as_deref(), Some("en-US"));
    }

    #[test]
    fn parse_uplay_install_state_skips_an_intervening_varint_field() {
        // field1, then field2 (tag 0x10) as a two-byte varint (300), then
        // field3 - proves the walker actually decodes varint width rather
        // than assuming every scalar field is one byte wide.
        let mut bytes = vec![0x0a, 2, b'A', b'B', 0x10, 0xac, 0x02];
        bytes.extend_from_slice(&[0x1a, 5]);
        bytes.extend_from_slice(b"en-US");

        let state = parse_uplay_install_state(&bytes);

        assert_eq!(state.manifest_id.as_deref(), Some("AB"));
        assert_eq!(state.language.as_deref(), Some("en-US"));
    }

    #[test]
    fn parse_uplay_install_state_returns_none_for_absent_language_field() {
        let state = parse_uplay_install_state(&[0x0a, 2, b'A', b'B']);

        assert_eq!(state.manifest_id.as_deref(), Some("AB"));
        assert_eq!(state.language, None);
    }

    #[test]
    fn parse_uplay_install_state_stops_cleanly_on_truncated_length() {
        // Field 1 claims a 10-byte payload but only 2 bytes follow -
        // must not panic on the out-of-bounds slice.
        let state = parse_uplay_install_state(&[0x0a, 10, b'A', b'B']);

        assert_eq!(state, UplayInstallState::default());
    }

    #[test]
    fn parse_uplay_install_state_returns_default_for_empty_input() {
        assert_eq!(parse_uplay_install_state(&[]), UplayInstallState::default());
    }

    #[test]
    fn installed_languages_reads_real_sidecar_file_from_disk() {
        let temp = tempfile::tempdir().unwrap();
        let game_dir = temp.path().join("Assassin's Creed Syndicate");
        std::fs::create_dir_all(&game_dir).unwrap();
        std::fs::write(
            game_dir.join(INSTALL_STATE_FILE),
            fake_install_state("EBD8CD1D9D65EF88823448F42B74AD961B0848846", "en-US"),
        )
        .unwrap();
        let other_dir = temp.path().join("No Sidecar Here");
        std::fs::create_dir_all(&other_dir).unwrap();

        let languages = installed_languages(&[game_dir.clone(), other_dir.clone()]);
        let ids = build_ids(&[game_dir.clone(), other_dir]);

        assert_eq!(languages.get(&game_dir).map(String::as_str), Some("en-US"));
        assert_eq!(languages.len(), 1);
        assert_eq!(
            ids.get(&game_dir).map(String::as_str),
            Some("EBD8CD1D9D65EF88823448F42B74AD961B0848846")
        );
    }
}
