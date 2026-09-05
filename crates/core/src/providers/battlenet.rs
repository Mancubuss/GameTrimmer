//! Battle.net (Blizzard) library discovery via the Windows uninstall registry.
//!
//! Battle.net's own install database (`C:\ProgramData\Battle.net\Agent\
//! product.db`) is a protobuf blob without a published schema, but the client
//! reliably registers every installed game under the standard uninstall keys
//! with `Publisher = "Blizzard Entertainment"`, `DisplayName` and
//! `InstallLocation` - so those are read instead. All three uninstall roots
//! (HKLM 64-bit, HKLM 32-bit, HKCU) are scanned and de-duplicated by
//! install directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

/// Whether a language appears in `.build.info`'s `Tags` column tagged for
/// speech (spoken/voice audio) or text (subtitles/UI strings) - a game can
/// carry either, both, or (if the tag group parses to a language with
/// neither recognized kind) neither.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LocaleOffer {
    pub speech: bool,
    pub text: bool,
}

/// Direct subdirectories of `library_root` - one per game, mirroring how
/// Battle.net lays out a library (`<root>\<Game Name>\...`). A missing root
/// is not a failure, matching how `discover_battlenet` treats an absent
/// uninstall registry root: the library simply isn't installed.
fn game_dirs(library_root: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(library_root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    Ok(entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect())
}

/// Parses `.build.info`'s pipe-separated table into its first data row,
/// keyed by column name with the header's `!TYPE:SIZE` suffix stripped.
/// Pure (text in, data out) like Steam's `parse_manifest_state`, so
/// header-reordering and missing-row edge cases are unit-tested without
/// touching a real Battle.net install.
///
/// The column index is deliberately never hardcoded: Blizzard has reordered
/// or added columns to this file across client versions before (this crate
/// has no way to pin a version), and a fixed index would silently read the
/// wrong field instead of failing loudly.
///
/// WHY THE FIRST DATA ROW, NOT EVERY ROW: `.build.info` can hold more than
/// one row when an install folder carries multiple Blizzard "products" -
/// World of Warcraft's folder on this machine held two: retail (`Product` =
/// `wow`, `Version` = `12.1.0.69404`) and the Anniversary/Classic realms
/// (`Product` = `wow_anniversary`, `Version` = `2.5.6.69110`), both marked
/// `Active` = `1` - so `Active` cannot pick a "the" row either. Every
/// single-product game read on this machine (Diablo III, Diablo II
/// Resurrected, StarCraft, StarCraft II, Call of Duty Black Ops 4 and Cold
/// War) has exactly one row, and both of World of Warcraft's rows kept the
/// base product first. Reading the first row therefore reports the primary
/// product consistently and never silently mixes two products' fields
/// together; exposing the secondary product is out of this ticket's scope.
fn parse_build_info(contents: &str) -> Option<HashMap<String, String>> {
    let mut lines = contents.lines();
    let header = lines.next()?;
    let names = header
        .split('|')
        .map(|col| col.split('!').next().unwrap_or(col));
    let row = lines.next()?;
    let values = row.split('|');
    Some(
        names
            .zip(values)
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect(),
    )
}

/// Reads `.build.info`'s `Version` column for every game folder directly
/// under `library_root`, keyed by the game's install directory (absolute
/// path, as a string).
///
/// WHY KEYED BY INSTALL DIRECTORY, NOT `app_id`: this provider's `app_id`
/// (see `build_game_install` above) comes from the Windows uninstall
/// registry key name - a value that lives in a completely different data
/// source than a filesystem root, with no relationship to the game's folder
/// name that could be derived from `library_root` alone. Guessing a
/// folder-name-to-registry-key mapping would be exactly the kind of fragile
/// matching this crate avoids elsewhere. The provider already resolves
/// `app_id` -> `install_dir` per game during the registry scan
/// (`RawUninstallEntry`/`GameInstall`), so a caller wanting "the build id for
/// this app_id" joins there: look up this map by `GameInstall::install_dir`,
/// not by `app_id`.
///
/// A missing or unreadable `.build.info`, or an empty `Version` field, is
/// ordinary rather than an error - not every Blizzard game uses the
/// Agent-based `.build.info` manifest at all (Hearthstone and Diablo
/// Immortal on this machine ship `.product.db` instead, an older/different
/// updater generation) - so such games are simply absent from the map,
/// never an empty-string entry. A missing `library_root` is the same: `Ok`
/// with an empty map.
pub fn build_ids(library_root: &Path) -> Result<HashMap<String, String>> {
    let mut result = HashMap::new();
    for dir in game_dirs(library_root)? {
        let Ok(contents) = std::fs::read_to_string(dir.join(".build.info")) else {
            continue;
        };
        let Some(row) = parse_build_info(&contents) else {
            continue;
        };
        let Some(version) = row.get("Version").filter(|v| !v.is_empty()) else {
            continue;
        };
        result.insert(dir.to_string_lossy().into_owned(), version.clone());
    }
    Ok(result)
}

/// `true` for a 4-ASCII-letter token shaped like a Battle.net language code:
/// two lowercase letters (language) then two uppercase letters (region) -
/// `deDE`, `enUS`, `zhTW`, `arSA`. Deliberately narrow: the region/account
/// flags that share a tag group (`EU?`, `acct-UKR?`, `geoip-UA?`, `noigr`,
/// `code`) never match this shape, so they're never mistaken for a language.
fn is_language_code(token: &str) -> bool {
    let chars: Vec<char> = token.chars().collect();
    chars.len() == 4
        && chars[0].is_ascii_lowercase()
        && chars[1].is_ascii_lowercase()
        && chars[2].is_ascii_uppercase()
        && chars[3].is_ascii_uppercase()
}

/// Parses one `.build.info` `Tags` value into language codes with whether
/// each is tagged for speech, text, or both. Pure (text in, data out) like
/// `parse_build_info`.
///
/// Each `:`-separated group is one selectable option's flag set, e.g.
/// `Windows EU? acct-UKR? geoip-UA? deDE speech?`. The language code is the
/// group's one token matching `is_language_code`; the group's own kind
/// (`speech` or `text`) is its last token with the trailing `?` stripped. A
/// group with no such language token (pure region/account flags on their
/// own) contributes nothing - this stays conservative about what counts as
/// a language rather than guessing from position.
fn parse_locale_tags(tags: &str) -> HashMap<String, LocaleOffer> {
    let mut languages: HashMap<String, LocaleOffer> = HashMap::new();
    for group in tags.split(':') {
        let tokens: Vec<&str> = group.split_whitespace().collect();
        let Some(lang) = tokens.iter().find(|token| is_language_code(token)) else {
            continue;
        };
        let Some(kind) = tokens.last().map(|token| token.trim_end_matches('?')) else {
            continue;
        };
        let offer = languages.entry((*lang).to_string()).or_default();
        match kind {
            "speech" => offer.speech = true,
            "text" => offer.text = true,
            _ => {}
        }
    }
    languages
}

/// Reads `.build.info`'s `Tags` column for every game folder directly under
/// `library_root`: which language codes it lists, and whether each is
/// tagged for speech audio, text, or both. Keyed by install directory - see
/// `build_ids` for why `app_id` isn't achievable here.
///
/// WHAT THIS DATA ACTUALLY MEANS - READ BEFORE REPORTING "INSTALLED
/// LANGUAGES": every tag group in `Tags` carries a trailing `?`
/// (`deDE speech?`, `enUS text?`, ...) - Blizzard's own syntax for "the
/// launcher can select this," i.e. an offer, not a receipt of what was
/// downloaded. Nothing on this machine lets that offer be checked against
/// disk for any game this function can actually read: Diablo III, Diablo II
/// Resurrected, StarCraft, StarCraft II, Call of Duty Black Ops 4/Cold War
/// and World of Warcraft all store game data inside CASC archives
/// (`Data\data\*.idx` plus opaque blobs) with no per-locale file to point
/// at - the bytes for all 2-19 listed languages, installed or not, sit
/// inside the same archive indistinguishably. The one game on this machine
/// with genuinely inspectable per-locale evidence is Hearthstone (loose,
/// locale-named `.unity3d` audio under `Data\Win\`, e.g.
/// `soundotherminion_base_dede-...-audio-0.unity3d`,
/// `..._frfr-...`, `..._ruru-...`) - and Hearthstone has no `.build.info` at
/// all (it and Diablo Immortal use `.product.db`, an older/different
/// updater generation), so this function cannot see it either way. In
/// short: what this returns must be surfaced as "offered by the launcher,"
/// never as "installed" - there is no evidence on this machine for the
/// latter claim, for any game `.build.info` can be read for.
pub fn locale_tags(library_root: &Path) -> Result<HashMap<String, HashMap<String, LocaleOffer>>> {
    let mut result = HashMap::new();
    for dir in game_dirs(library_root)? {
        let Ok(contents) = std::fs::read_to_string(dir.join(".build.info")) else {
            continue;
        };
        let Some(row) = parse_build_info(&contents) else {
            continue;
        };
        let Some(tags) = row.get("Tags").filter(|v| !v.is_empty()) else {
            continue;
        };
        let languages = parse_locale_tags(tags);
        if !languages.is_empty() {
            result.insert(dir.to_string_lossy().into_owned(), languages);
        }
    }
    Ok(result)
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

    // -- GT-104: .build.info (Version, Tags) --

    /// Shaped after the real `.build.info` read on this machine for Diablo
    /// III, with columns in their observed order.
    fn diablo3_build_info() -> String {
        concat!(
            "Branch!STRING:0|Active!DEC:1|Build Key!HEX:16|CDN Key!HEX:16|",
            "Install Key!HEX:16|IM Size!DEC:4|CDN Path!STRING:0|",
            "CDN Hosts!STRING:0|CDN Servers!STRING:0|Tags!STRING:0|",
            "Armadillo!STRING:0|Last Activated!STRING:0|Version!STRING:0|",
            "KeyRing!HEX:16|Product!STRING:0\n",
            "eu|1|e34fc3fb|6547fc64|||tpr/diablo3|level3.blizzard.com|",
            "http://level3.blizzard.com|",
            "Windows EU? acct-UKR? geoip-UA? deDE speech?:",
            "Windows EU? acct-UKR? geoip-UA? deDE text?:",
            "Windows EU? acct-UKR? geoip-UA? enUS speech?:",
            "Windows EU? acct-UKR? geoip-UA? enUS text?",
            "|||2.8.0.99920||\n",
        )
        .to_string()
    }

    #[test]
    fn parse_build_info_reads_version_by_header_lookup() {
        let row = parse_build_info(&diablo3_build_info()).expect("expected a parsed row");
        assert_eq!(row.get("Version").map(String::as_str), Some("2.8.0.99920"));
    }

    #[test]
    fn parse_build_info_finds_version_despite_reordered_and_extra_columns() {
        // The whole point of header-driven lookup: `Version` is no longer at
        // its Diablo III position, and an unknown extra column exists.
        let acf =
            "Product!STRING:0|Extra!STRING:0|Version!STRING:0|Active!DEC:1\nwow|xyz|1.2.3|1\n";
        let row = parse_build_info(acf).expect("expected a parsed row");
        assert_eq!(row.get("Version").map(String::as_str), Some("1.2.3"));
    }

    #[test]
    fn parse_build_info_returns_none_without_a_data_row() {
        let header_only = "Branch!STRING:0|Version!STRING:0\n";
        assert!(parse_build_info(header_only).is_none());
    }

    #[test]
    fn build_ids_reads_version_for_a_game_folder() {
        let root = tempfile::tempdir().unwrap();
        let game_dir = root.path().join("Diablo III");
        std::fs::create_dir(&game_dir).unwrap();
        std::fs::write(game_dir.join(".build.info"), diablo3_build_info()).unwrap();

        let ids = build_ids(root.path()).unwrap();
        assert_eq!(
            ids.get(&game_dir.to_string_lossy().into_owned())
                .map(String::as_str),
            Some("2.8.0.99920")
        );
    }

    #[test]
    fn build_ids_skips_a_game_folder_with_no_build_info() {
        // Hearthstone and Diablo Immortal ship `.product.db` instead - no
        // `.build.info` at all - and that must be silent, not an error.
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("Hearthstone")).unwrap();

        let ids = build_ids(root.path()).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn build_ids_skips_a_game_with_an_empty_version() {
        let root = tempfile::tempdir().unwrap();
        let game_dir = root.path().join("Some Game");
        std::fs::create_dir(&game_dir).unwrap();
        std::fs::write(
            game_dir.join(".build.info"),
            "Version!STRING:0|Active!DEC:1\n|1\n",
        )
        .unwrap();

        let ids = build_ids(root.path()).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn build_ids_on_a_missing_library_root_is_an_empty_map_not_an_error() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("does-not-exist");

        assert_eq!(build_ids(&missing).unwrap(), HashMap::new());
    }

    #[test]
    fn parse_locale_tags_splits_speech_and_text_per_language() {
        let tags = "Windows EU? acct-UKR? geoip-UA? deDE speech?:\
                     Windows EU? acct-UKR? geoip-UA? deDE text?:\
                     Windows EU? acct-UKR? geoip-UA? enUS speech?";
        let languages = parse_locale_tags(tags);

        let de = languages.get("deDE").expect("expected deDE");
        assert!(de.speech && de.text);

        let en = languages.get("enUS").expect("expected enUS");
        assert!(en.speech && !en.text);
    }

    #[test]
    fn parse_locale_tags_is_empty_with_no_language_codes() {
        // Only region/account flags, no `aaAA`-shaped language token anywhere.
        let tags = "Windows noigr EU? acct-UKR? geoip-UA?";
        assert!(parse_locale_tags(tags).is_empty());
    }
}
