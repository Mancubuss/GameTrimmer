//! Steam library discovery: registry -> libraryfolders.vdf -> appmanifest_*.acf.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winreg::RegKey;

use crate::error::Result;

use super::{
    diagnostic, DiscoveredLibrary, DiscoveryDiagnostic, DiscoveryReport, DiscoveryStatus,
    GameInstall, LibraryProvider, OrphanEvidence,
};

pub struct SteamProvider;

impl LibraryProvider for SteamProvider {
    fn name(&self) -> &'static str {
        "steam"
    }

    fn try_discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        Ok(discover_steam().data)
    }

    fn discover(&self) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
        discover_steam()
    }
}

fn discover_steam() -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let Some(root) = find_steam_root() else {
        return DiscoveryReport::not_installed(Vec::new());
    };

    let mut diagnostics = Vec::new();
    let mut library_roots = vec![root.clone()];
    let libraryfolders_path = root.join("steamapps").join("libraryfolders.vdf");
    match std::fs::read_to_string(&libraryfolders_path) {
        Ok(contents) if vdf_well_formed(&contents) => {
            library_roots.extend(parse_libraryfolders(&contents));
        }
        Ok(_) => diagnostics.push(diagnostic(
            "steam",
            "libraryfolders-parse",
            libraryfolders_path.to_path_buf(),
            "malformed libraryfolders.vdf",
        )),
        Err(err) => diagnostics.push(diagnostic(
            "steam",
            "libraryfolders-read",
            libraryfolders_path.to_path_buf(),
            err,
        )),
    }

    let mut seen = HashSet::new();
    let unique_roots: Vec<PathBuf> = library_roots
        .into_iter()
        .filter(|path| seen.insert(path.to_string_lossy().to_lowercase()))
        .collect();

    let mut libraries = Vec::new();
    for library_root in unique_roots {
        let (library, mut library_diagnostics) = discover_library(&library_root);
        diagnostics.append(&mut library_diagnostics);
        if let Some(library) = library {
            libraries.push(library);
        }
    }

    if degrades_evidence(&diagnostics) {
        for library in &mut libraries {
            library.orphan_evidence = OrphanEvidence::Degraded;
        }
        DiscoveryReport::degraded(libraries, diagnostics)
    } else {
        // Complete, but not necessarily silent: a `GAME_ABSENT` note still
        // travels so it reaches the log and `scan_diagnostics`.
        // `DiscoveryReport::complete` would drop it, which is the whole
        // behaviour this card exists to change.
        DiscoveryReport {
            data: libraries,
            status: DiscoveryStatus::Complete,
            diagnostics,
        }
    }
}

// `GAME_ABSENT` and `degrades_evidence` started here, for Steam's paused
// downloads. Every other provider turned out to need the same distinction, so
// they now live in `super` - see `providers::GAME_ABSENT`.
use super::{degrades_evidence, GAME_ABSENT};

/// Reads one library's `steamapps` directory and returns its discovered games.
/// Returns `None` if the library root or its `steamapps` folder doesn't exist.
fn discover_library(library_root: &Path) -> (Option<DiscoveredLibrary>, Vec<DiscoveryDiagnostic>) {
    let steamapps_dir = library_root.join("steamapps");
    if !steamapps_dir.is_dir() {
        return (
            None,
            vec![diagnostic(
                "steam",
                "library-root",
                steamapps_dir.to_path_buf(),
                "declared Steam library is unavailable",
            )],
        );
    }

    let entries = match std::fs::read_dir(&steamapps_dir) {
        Ok(entries) => entries,
        Err(err) => {
            return (
                None,
                vec![diagnostic(
                    "steam",
                    "manifest-enumeration",
                    steamapps_dir.to_path_buf(),
                    err,
                )],
            )
        }
    };
    let mut games = Vec::new();
    let mut diagnostics = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                diagnostics.push(diagnostic(
                    "steam",
                    "manifest-entry",
                    steamapps_dir.to_path_buf(),
                    err,
                ));
                continue;
            }
        };
        let path = entry.path();
        if !is_appmanifest(&path) {
            continue;
        }
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) => {
                diagnostics.push(diagnostic(
                    "steam",
                    "manifest-read",
                    path.to_path_buf(),
                    err,
                ));
                continue;
            }
        };
        if !vdf_well_formed(&contents) {
            diagnostics.push(diagnostic(
                "steam",
                "manifest-parse",
                path.to_path_buf(),
                "malformed VDF",
            ));
            continue;
        }
        let Some(game) = parse_appmanifest(&contents, library_root) else {
            diagnostics.push(diagnostic(
                "steam",
                "manifest-parse",
                path.to_path_buf(),
                "missing required AppState fields",
            ));
            continue;
        };
        // A manifest whose install dir is simply not there is normal (a queued
        // or paused download), and a folder that does not exist cannot be
        // mistaken for an orphan either. A folder we merely failed to read is
        // the dangerous case: it stays on disk, drops out of `games`, and
        // `unmanaged_subdirs` would then call it residue. Diagnose it.
        match super::try_is_dir(&game.install_dir) {
            Ok(true) => games.push(game),
            // Recorded, but explicitly not degrading - see `GAME_ABSENT`.
            Ok(false) => diagnostics.push(diagnostic(
                "steam",
                GAME_ABSENT,
                game.install_dir.clone(),
                "manifest present, install directory absent (queued or paused download)",
            )),
            Err(err) => diagnostics.push(diagnostic(
                "steam",
                "game-path",
                game.install_dir.clone(),
                err,
            )),
        }
    }

    let evidence = if degrades_evidence(&diagnostics) {
        OrphanEvidence::Degraded
    } else {
        OrphanEvidence::Authoritative
    };
    (
        Some(DiscoveredLibrary {
            vendor: "steam",
            path: library_root.to_path_buf(),
            games,
            orphan_evidence: evidence,
        }),
        diagnostics,
    )
}

/// Strict enough to distinguish a truncated/unbalanced Valve KeyValues file
/// from a syntactically complete file before the permissive parser runs.
fn vdf_well_formed(input: &str) -> bool {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        if ch == '"' {
            quoted = true;
        } else if ch == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for comment in chars.by_ref() {
                if comment == '\n' {
                    break;
                }
            }
        } else if ch == '{' {
            depth += 1;
        } else if ch == '}' {
            let Some(next) = depth.checked_sub(1) else {
                return false;
            };
            depth = next;
        }
    }
    !quoted && depth == 0
}

fn is_appmanifest(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("appmanifest_") && name.ends_with(".acf"))
}

/// Locates the Steam installation root via the Windows registry
/// (`HKCU\Software\Valve\Steam\SteamPath`, fallback `HKLM\SOFTWARE\WOW6432Node\Valve\Steam\InstallPath`).
pub fn find_steam_root() -> Option<PathBuf> {
    let raw = read_hkcu_steam_path().or_else(read_hklm_install_path)?;
    let path = PathBuf::from(super::normalize_slashes(&raw));
    path.is_dir().then_some(path)
}

fn read_hkcu_steam_path() -> Option<String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu.open_subkey("Software\\Valve\\Steam").ok()?;
    key.get_value::<String, _>("SteamPath").ok()
}

fn read_hklm_install_path() -> Option<String> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let key = hklm
        .open_subkey("SOFTWARE\\WOW6432Node\\Valve\\Steam")
        .ok()?;
    key.get_value::<String, _>("InstallPath").ok()
}

/// Parses the text of `steamapps/libraryfolders.vdf` and returns the library root paths.
pub fn parse_libraryfolders(vdf: &str) -> Vec<PathBuf> {
    let root = parse_vdf(vdf);
    let mut paths = Vec::new();
    collect_library_paths(&root, &mut paths);

    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.to_string_lossy().to_lowercase()))
        .collect()
}

/// Recursively collects the value of every `"path"` key found in the tree.
/// Valve's `libraryfolders.vdf` nests each library under a numbered block
/// (`"0"`, `"1"`, ...) that carries a `"path"` field alongside other metadata
/// (an `"apps"` block, etc.) - walking the whole tree is robust to that shape
/// without hard-coding the numbered-block structure.
fn collect_library_paths(value: &VdfValue, out: &mut Vec<PathBuf>) {
    let VdfValue::Obj(entries) = value else {
        return;
    };

    for (key, val) in entries {
        if key.eq_ignore_ascii_case("path") {
            if let VdfValue::Str(s) = val {
                out.push(PathBuf::from(super::normalize_slashes(s)));
            }
        }
    }
    for (_, val) in entries {
        collect_library_paths(val, out);
    }
}

/// Parses the text of one `appmanifest_<id>.acf`; returns the game it describes.
/// `library_root` is the library the manifest belongs to (used to build `install_dir`).
pub fn parse_appmanifest(acf: &str, library_root: &Path) -> Option<GameInstall> {
    let root = parse_vdf(acf);
    let VdfValue::Obj(entries) = &root else {
        return None;
    };

    let app_state = entries.iter().find_map(|(key, val)| {
        if key.eq_ignore_ascii_case("AppState") {
            match val {
                VdfValue::Obj(fields) => Some(fields),
                _ => None,
            }
        } else {
            None
        }
    })?;

    let get_field = |field: &str| -> Option<String> {
        app_state.iter().find_map(|(key, val)| {
            if key.eq_ignore_ascii_case(field) {
                match val {
                    VdfValue::Str(s) => Some(s.clone()),
                    _ => None,
                }
            } else {
                None
            }
        })
    };

    let name = get_field("name").filter(|s| !s.trim().is_empty())?;
    let installdir = get_field("installdir").filter(|s| !s.trim().is_empty())?;
    let app_id = get_field("appid");

    let install_dir = library_root
        .join("steamapps")
        .join("common")
        .join(installdir);

    Some(GameInstall {
        name,
        install_dir,
        app_id,
    })
}

/// The cheap *state* subset of an `appmanifest_*.acf` (game-state tracking): just enough to
/// tell whether a game changed since the last scan, without walking a single
/// file of it.
///
/// Deliberately separate from [`GameInstall`] rather than a field on it: the
/// state probe runs on its own schedule (once at startup, over manifests only)
/// and adding a field to `GameInstall` would touch all twelve providers for
/// data only Steam can supply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestState {
    /// Steam appid - the stable key matching `games.app_id`.
    pub app_id: String,
    /// Steam's `buildid`, bumped by Valve on every content update (and by a
    /// `Verify` that re-downloads). A change since the last scan means the
    /// installed files changed - i.e. trimmed files may well be back.
    /// `None` when the manifest omits it (older/partial manifests).
    pub build_id: Option<String>,
}

/// Reads one `appmanifest_*.acf`'s state fields. Pure (text in, data out), so
/// the parsing is unit-tested without touching a real Steam install.
/// `None` when the text isn't a manifest or carries no `appid` - without the
/// appid there is nothing to match a stored game against.
pub fn parse_manifest_state(acf: &str) -> Option<ManifestState> {
    let root = parse_vdf(acf);
    let VdfValue::Obj(entries) = &root else {
        return None;
    };

    let app_state = entries.iter().find_map(|(key, val)| {
        if key.eq_ignore_ascii_case("AppState") {
            match val {
                VdfValue::Obj(fields) => Some(fields),
                _ => None,
            }
        } else {
            None
        }
    })?;

    let get_field = |field: &str| -> Option<String> {
        app_state.iter().find_map(|(key, val)| {
            if key.eq_ignore_ascii_case(field) {
                match val {
                    VdfValue::Str(s) => Some(s.clone()),
                    _ => None,
                }
            } else {
                None
            }
        })
    };

    let app_id = get_field("appid").filter(|s| !s.trim().is_empty())?;
    let build_id = get_field("buildid").filter(|s| !s.trim().is_empty());

    Some(ManifestState { app_id, build_id })
}

/// Collects the [`ManifestState`] of every game in one Steam library root.
///
/// Reads only `steamapps/appmanifest_*.acf` - a few dozen small text files, no
/// directory walk of the games themselves - so this is cheap enough to run on
/// every startup (that is the whole point: detecting "what changed" must not
/// cost a scan). An unreadable manifest is skipped, never fatal.
pub fn manifest_states(library_root: &Path) -> Result<Vec<ManifestState>> {
    let steamapps = library_root.join("steamapps");
    let entries = std::fs::read_dir(&steamapps)?;
    let mut states = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !is_appmanifest(&path) {
            continue;
        }
        let contents = std::fs::read_to_string(&path)?;
        if !vdf_well_formed(&contents) {
            return Err(crate::error::CoreError::Other(format!(
                "malformed Steam manifest: {}",
                path.display()
            )));
        }
        let state = parse_manifest_state(&contents).ok_or_else(|| {
            crate::error::CoreError::Other(format!(
                "Steam manifest has no usable app id: {}",
                path.display()
            ))
        })?;
        states.push(state);
    }
    Ok(states)
}

/// One depot's `(depot_id, manifest_id)` pair as declared by an installed
/// app's `InstalledDepots` block - together the exact stem of the cache
/// filename Steam expects under `depotcache/`: `<depot_id>_<manifest_id>.manifest`.
///
/// Confirmed against a real installed game on this machine (Portal 2, appid
/// 620): its `appmanifest_620.acf` declared depot `621` at manifest
/// `9122696443005314333`, and `depotcache/621_9122696443005314333.manifest`
/// existed - in *both* of this machine's two Steam library roots, not only
/// the one Portal 2 is actually installed in (see
/// [`installed_depots_for_library`] for why that forces the orphan check to
/// be global across every library rather than per-library).
///
/// Parses the whole `InstalledDepots` block regardless of how many depot
/// sub-blocks it has; a sub-block with no readable `"manifest"` field is
/// skipped rather than guessed at - GT-23's orphan check for `depotcache/`
/// only ever proves a file is *needed*, so an unparseable depot entry just
/// contributes nothing to that proof (it never marks anything as orphaned).
pub fn parse_installed_depots(acf: &str) -> Vec<(String, String)> {
    let root = parse_vdf(acf);
    let VdfValue::Obj(entries) = &root else {
        return Vec::new();
    };

    let Some(app_state) = entries.iter().find_map(|(key, val)| {
        if key.eq_ignore_ascii_case("AppState") {
            match val {
                VdfValue::Obj(fields) => Some(fields),
                _ => None,
            }
        } else {
            None
        }
    }) else {
        return Vec::new();
    };

    let Some(installed_depots) = app_state.iter().find_map(|(key, val)| {
        if key.eq_ignore_ascii_case("InstalledDepots") {
            match val {
                VdfValue::Obj(fields) => Some(fields),
                _ => None,
            }
        } else {
            None
        }
    }) else {
        return Vec::new();
    };

    installed_depots
        .iter()
        .filter_map(|(depot_id, val)| {
            let VdfValue::Obj(fields) = val else {
                return None;
            };
            let manifest_id = fields.iter().find_map(|(key, val)| {
                if key.eq_ignore_ascii_case("manifest") {
                    match val {
                        VdfValue::Str(s) if !s.trim().is_empty() => Some(s.clone()),
                        _ => None,
                    }
                } else {
                    None
                }
            })?;
            Some((depot_id.clone(), manifest_id))
        })
        .collect()
}

/// Collects every `(depot_id, manifest_id)` pair declared by the installed
/// games of one Steam library - the proof-of-need set GT-23's `depotcache/`
/// orphan check uses.
///
/// Real evidence from this machine changed the design here: a depot manifest
/// currently needed by a game installed on `F:\SteamLibrary` was *also*
/// cached in the other library's `depotcache/` (`D:\...\Steam\depotcache`,
/// the primary Steam install root) - almost certainly left behind from before
/// the game moved drives, but still bearing the exact manifest id the game
/// currently needs. So a manifest sitting in one library's `depotcache/` is
/// not proven safe to delete by that library's own installed games alone;
/// the caller must union this function's result across *every* discovered
/// Steam library before checking any single one's cache files.
///
/// Unreadable or malformed evidence is an error, not a partial result -
/// mirrors [`manifest_states`]'s stance for the same reason: silently
/// skipping one manifest here would understate what is needed and risk
/// flagging a depot cache file some other installed game still depends on.
pub fn installed_depots_for_library(library_root: &Path) -> Result<Vec<(String, String)>> {
    let steamapps = library_root.join("steamapps");
    let entries = std::fs::read_dir(&steamapps)?;
    let mut pairs = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !is_appmanifest(&path) {
            continue;
        }
        let contents = std::fs::read_to_string(&path)?;
        if !vdf_well_formed(&contents) {
            return Err(crate::error::CoreError::Other(format!(
                "malformed Steam manifest: {}",
                path.display()
            )));
        }
        pairs.extend(parse_installed_depots(&contents));
    }
    Ok(pairs)
}

/// The set of Steam Workshop published-file ids one `appworkshop_<appid>.acf`
/// currently lists under `WorkshopItemsInstalled` - the only place Steam
/// records "this item is a live subscription for this appid", and it is
/// authoritative independent of whether the appid's own game is still
/// installed (see `orphans` module docs and `orphan_analysis` for why that
/// matters: a game can be uninstalled while its Workshop subscriptions and
/// this state file both survive).
///
/// Confirmed against a real `appworkshop_217140.acf` on this machine: it
/// listed 78 ids under `WorkshopItemsInstalled`, of which only 39 had a
/// matching folder under `workshop/content/217140/` on disk - every folder
/// that *was* on disk was also in this list (nothing orphaned on this
/// machine), and the other 39 are subscriptions Steam has not (or no longer)
/// materialized locally. That is the direction the check runs in: a folder on
/// disk is only proven live by appearing in this set; an id in this set with
/// no folder on disk is not this module's concern (it is not residue - there
/// is nothing on disk to flag).
///
/// Returns `None` when the text cannot be trusted as this app's Workshop
/// state at all: not a VDF object, no `AppWorkshop` block, or no
/// `WorkshopItemsInstalled` key. The last case is deliberately not treated as
/// "confidently zero items" - this machine's real files never exercised that
/// shape, so nothing here proves an absent key means zero rather than
/// unwritten/older-format state, and GT-23's fail-closed rule is to leave
/// content alone rather than guess. An explicit-but-empty
/// `"WorkshopItemsInstalled" { }` block, by contrast, *is* trusted as zero -
/// Steam wrote the key, it is just empty.
pub fn parse_workshop_installed_items(acf: &str) -> Option<HashSet<String>> {
    let root = parse_vdf(acf);
    let VdfValue::Obj(entries) = &root else {
        return None;
    };

    let app_workshop = entries.iter().find_map(|(key, val)| {
        if key.eq_ignore_ascii_case("AppWorkshop") {
            match val {
                VdfValue::Obj(fields) => Some(fields),
                _ => None,
            }
        } else {
            None
        }
    })?;

    let installed = app_workshop.iter().find_map(|(key, val)| {
        if key.eq_ignore_ascii_case("WorkshopItemsInstalled") {
            match val {
                VdfValue::Obj(fields) => Some(fields),
                _ => None,
            }
        } else {
            None
        }
    })?;

    Some(installed.iter().map(|(id, _)| id.clone()).collect())
}

/// A minimal in-memory representation of Valve's KeyValues (VDF) text format:
/// either a leaf string, or an object holding an ordered list of key/value pairs
/// (values may themselves be nested objects).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VdfValue {
    Str(String),
    Obj(Vec<(String, VdfValue)>),
}

enum VdfToken {
    Text(String),
    Open,
    Close,
}

/// Tokenizes VDF text into quoted-string, `{`, and `}` tokens.
/// Handles `\\` and `\"` escapes inside quoted strings and skips `//` comments.
fn tokenize_vdf(input: &str) -> Vec<VdfToken> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&c) = chars.peek() {
        match c {
            '"' => {
                chars.next();
                let mut s = String::new();
                while let Some(&c) = chars.peek() {
                    match c {
                        '"' => {
                            chars.next();
                            break;
                        }
                        '\\' => {
                            chars.next();
                            if let Some(&escaped) = chars.peek() {
                                match escaped {
                                    '\\' => s.push('\\'),
                                    '"' => s.push('"'),
                                    'n' => s.push('\n'),
                                    't' => s.push('\t'),
                                    other => s.push(other),
                                }
                                chars.next();
                            }
                        }
                        _ => {
                            s.push(c);
                            chars.next();
                        }
                    }
                }
                tokens.push(VdfToken::Text(s));
            }
            '{' => {
                tokens.push(VdfToken::Open);
                chars.next();
            }
            '}' => {
                tokens.push(VdfToken::Close);
                chars.next();
            }
            '/' => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    for c in chars.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                }
            }
            _ => {
                chars.next();
            }
        }
    }

    tokens
}

/// Parses a sequence of key/value (and key/object) pairs starting at `*pos`,
/// stopping at a matching `Close` token or end of input.
fn parse_object(tokens: &[VdfToken], pos: &mut usize) -> Vec<(String, VdfValue)> {
    let mut entries = Vec::new();

    while *pos < tokens.len() {
        match &tokens[*pos] {
            VdfToken::Close => {
                *pos += 1;
                break;
            }
            VdfToken::Open => {
                // Unexpected object without a preceding key; skip it defensively.
                *pos += 1;
            }
            VdfToken::Text(key) => {
                let key = key.clone();
                *pos += 1;
                if *pos >= tokens.len() {
                    break;
                }
                match &tokens[*pos] {
                    VdfToken::Open => {
                        *pos += 1;
                        let child = parse_object(tokens, pos);
                        entries.push((key, VdfValue::Obj(child)));
                    }
                    VdfToken::Text(value) => {
                        let value = value.clone();
                        *pos += 1;
                        entries.push((key, VdfValue::Str(value)));
                    }
                    VdfToken::Close => {
                        // Malformed: key without a value; drop it and keep going.
                        *pos += 1;
                    }
                }
            }
        }
    }

    entries
}

pub(crate) fn parse_vdf(input: &str) -> VdfValue {
    let tokens = tokenize_vdf(input);
    let mut pos = 0;
    VdfValue::Obj(parse_object(&tokens, &mut pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with_installdir(name: &str, installdir: &str) -> String {
        format!(
            "\"AppState\"\n{{\n\t\"appid\"\t\t\"1\"\n\t\"name\"\t\t\"{name}\"\n\t\"installdir\"\t\t\"{installdir}\"\n}}\n"
        )
    }

    /// Opens `path` (a directory) with no sharing at all, so that a
    /// `FindFirstFileW` issued by another handle on the same process while
    /// this one is alive - which is exactly what `std::fs::read_dir` does
    /// internally - collides with it and fails. Dropping the returned
    /// `File` closes the handle and releases the lock.
    ///
    /// A stand-in for the real causes of a `read_dir` failure on a directory
    /// that genuinely exists (a DACL denial, a disconnected network share, a
    /// removable drive pulled mid-read) - none of which a test can create
    /// portably, unlike an exclusive open of a path this test itself owns.
    #[cfg(windows)]
    fn lock_directory_exclusively(path: &Path) -> std::fs::File {
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::FromRawHandle;

        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ, FILE_SHARE_MODE,
            OPEN_EXISTING,
        };

        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 buffer alive for the
        // duration of the call. `None` is accepted by Win32 as "use
        // defaults" for the security-attributes and template-handle
        // parameters. On success the returned handle is uniquely owned and
        // moved into the `File` immediately below, which becomes solely
        // responsible for closing it.
        let handle = unsafe {
            CreateFileW(
                PCWSTR::from_raw(wide.as_ptr()),
                FILE_GENERIC_READ.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        }
        .expect("open the test directory exclusively");
        // SAFETY: ownership of the valid handle returned above is moved into
        // `File`, which closes it exactly once on drop.
        unsafe { std::fs::File::from_raw_handle(handle.0 as *mut _) }
    }

    /// GT-109 item 5 (Steam half): a `steamapps` directory that exists but
    /// cannot be enumerated - as opposed to one that is provably absent -
    /// must surface a visible diagnostic rather than reading as "empty
    /// library". `discover_library`'s `read_dir` failure branch had never
    /// been reached by a test before this.
    #[cfg(windows)]
    #[test]
    fn an_unlistable_steamapps_dir_reports_a_visible_diagnostic() {
        let root = tempfile::tempdir().unwrap();
        let steamapps = root.path().join("steamapps");
        std::fs::create_dir(&steamapps).unwrap();

        let _lock = lock_directory_exclusively(&steamapps);

        let (library, diagnostics) = discover_library(root.path());

        assert!(
            library.is_none(),
            "an unlistable steamapps dir must not be reported as a valid, empty library"
        );
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.stage == "manifest-enumeration"),
            "the read_dir failure must be visible, not silently dropped: {diagnostics:?}"
        );
    }

    #[test]
    fn malformed_manifest_degrades_library_and_never_authorizes_orphans() {
        let root = tempfile::tempdir().unwrap();
        let steamapps = root.path().join("steamapps");
        std::fs::create_dir(&steamapps).unwrap();
        std::fs::write(steamapps.join("appmanifest_1.acf"), "\"AppState\" {").unwrap();

        let (library, diagnostics) = discover_library(root.path());
        let library = library.expect("the declared library is still reported");
        assert_eq!(library.orphan_evidence, OrphanEvidence::Degraded);
        assert!(library.games.is_empty());
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.stage == "manifest-parse"));
    }

    /// An install directory that cannot be examined - as opposed to one that
    /// is provably absent - must degrade the library. Dropping the game
    /// silently would leave its live folder in `steamapps\common` with nothing
    /// claiming it, which is precisely how orphan detection invents residue.
    #[test]
    fn an_unexaminable_install_dir_degrades_the_library() {
        let root = tempfile::tempdir().unwrap();
        let steamapps = root.path().join("steamapps");
        std::fs::create_dir_all(steamapps.join("common")).unwrap();
        // `<` is invalid in a Windows path component, so the probe fails with
        // ERROR_INVALID_NAME rather than "not found" - a stand-in for the real
        // cases (DACL denial, offline placeholder, drive not ready) that no
        // test can create portably.
        std::fs::write(
            steamapps.join("appmanifest_1.acf"),
            manifest_with_installdir("Broken", "bad<name"),
        )
        .unwrap();

        let (library, diagnostics) = discover_library(root.path());
        let library = library.expect("the declared library is still reported");
        assert_eq!(library.orphan_evidence, OrphanEvidence::Degraded);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.stage == "game-path"),
            "the failed probe must be visible, not silently dropped: {diagnostics:?}"
        );
    }

    /// The counterpart: a manifest for a game that is queued but not yet
    /// downloaded is ordinary, and an absent folder cannot be mistaken for
    /// residue - so it must not degrade the library.
    #[test]
    fn a_manifest_for_a_not_yet_downloaded_game_keeps_the_library_authoritative() {
        let root = tempfile::tempdir().unwrap();
        let steamapps = root.path().join("steamapps");
        std::fs::create_dir(&steamapps).unwrap();
        std::fs::write(
            steamapps.join("appmanifest_1.acf"),
            manifest_with_installdir("Queued", "NotThereYet"),
        )
        .unwrap();

        let (library, diagnostics) = discover_library(root.path());
        let library = library.expect("the declared library is still reported");
        assert!(library.games.is_empty());
        // The two halves of the rule, and they have to hold together: the
        // skipped manifest is named, and naming it costs the library
        // nothing. One paused download must not strip a whole library's
        // orphan-deletion authority.
        assert_eq!(library.orphan_evidence, OrphanEvidence::Authoritative);
        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.stage)
                .collect::<Vec<_>>(),
            vec![GAME_ABSENT],
            "the skipped manifest has to leave a trace: {diagnostics:?}",
        );
    }

    /// The severity distinction, stated on its own: a genuinely degrading
    /// diagnostic alongside a `GAME_ABSENT` one still degrades. Without
    /// this, "ignore game-absent" could silently grow into "ignore
    /// everything once a download is paused".
    #[test]
    fn an_absent_game_does_not_mask_a_real_diagnostic() {
        let absent = DiscoveryDiagnostic {
            provider: "steam",
            stage: GAME_ABSENT,
            path: None,
            message: String::new(),
        };
        let real = DiscoveryDiagnostic {
            provider: "steam",
            stage: "game-path",
            path: None,
            message: String::new(),
        };

        assert!(!degrades_evidence(&[]));
        assert!(!degrades_evidence(std::slice::from_ref(&absent)));
        assert!(degrades_evidence(&[absent, real]));
    }

    #[test]
    fn parse_libraryfolders_reads_all_numbered_blocks_with_escaped_backslashes() {
        let vdf = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"D:\\PortableApps\\Portable\\Games\\Steam"
		"label"		"Steam Home"
		"contentid"		"882536142594950971"
		"apps"
		{
			"228980"		"1366752304"
			"812140"		"116303258020"
		}
	}
	"1"
	{
		"path"		"F:\\SteamLibrary"
		"label"		""
		"apps"
		{
			"620"		"12753874736"
			"730"		"36644248671"
		}
	}
	"2"
	{
		"path"		"G:\\SteamLibrary"
		"label"		""
		"apps"
		{
			"22320"		"1142479546"
		}
	}
}
"#;

        let paths = parse_libraryfolders(vdf);

        assert_eq!(
            paths,
            vec![
                PathBuf::from(r"D:\PortableApps\Portable\Games\Steam"),
                PathBuf::from(r"F:\SteamLibrary"),
                PathBuf::from(r"G:\SteamLibrary"),
            ]
        );
    }

    #[test]
    fn parse_libraryfolders_dedupes_repeated_paths() {
        let vdf = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"F:\\SteamLibrary"
	}
	"1"
	{
		"path"		"F:\\SteamLibrary"
	}
}
"#;

        let paths = parse_libraryfolders(vdf);

        assert_eq!(paths, vec![PathBuf::from(r"F:\SteamLibrary")]);
    }

    #[test]
    fn parse_libraryfolders_returns_empty_for_no_libraries() {
        let vdf = r#"
"libraryfolders"
{
}
"#;

        assert!(parse_libraryfolders(vdf).is_empty());
    }

    #[test]
    fn parse_manifest_state_reads_appid_and_buildid() {
        let acf = r#"
"AppState"
{
	"appid"		"620"
	"name"		"Portal 2"
	"installdir"		"Portal 2"
	"buildid"		"17038203"
}
"#;
        let state = parse_manifest_state(acf).expect("expected a parsed state");
        assert_eq!(state.app_id, "620");
        assert_eq!(state.build_id.as_deref(), Some("17038203"));
    }

    #[test]
    fn parse_manifest_state_tolerates_a_manifest_without_buildid() {
        // Older/partial manifests omit it; the appid alone is still useful, and
        // `gamestate::changed_games` treats a missing build id as "unknown".
        let acf = r#"
"AppState"
{
	"appid"		"620"
	"name"		"Portal 2"
	"installdir"		"Portal 2"
}
"#;
        let state = parse_manifest_state(acf).expect("expected a parsed state");
        assert_eq!(state.app_id, "620");
        assert_eq!(state.build_id, None);
    }

    #[test]
    fn parse_manifest_state_returns_none_without_an_appid() {
        // Without the appid there is no key to match a stored game against.
        let acf = r#"
"AppState"
{
	"name"		"Portal 2"
	"buildid"		"17038203"
}
"#;
        assert!(parse_manifest_state(acf).is_none());
        assert!(parse_manifest_state("not a vdf file at all").is_none());
        assert!(parse_manifest_state("").is_none());
    }

    #[test]
    fn parse_manifest_state_ignores_blank_fields() {
        let acf = r#"
"AppState"
{
	"appid"		"620"
	"buildid"		""
}
"#;
        let state = parse_manifest_state(acf).expect("expected a parsed state");
        assert_eq!(
            state.build_id, None,
            "a blank buildid is no build id, not an empty-string one"
        );
    }

    #[test]
    fn manifest_states_on_a_missing_library_root_is_an_error() {
        assert!(manifest_states(Path::new(r"Z:\definitely\not\a\steam\library")).is_err());
    }

    #[test]
    fn parse_appmanifest_reads_real_world_acf() {
        let acf = r#"
"AppState"
{
	"appid"		"620"
	"universe"		"1"
	"LauncherPath"		"E:\\SteamLibrary\\steamcmd.exe"
	"name"		"Portal 2"
	"StateFlags"		"4"
	"installdir"		"Portal 2"
	"LastUpdated"		"1713987003"
	"LastPlayed"		"0"
	"SizeOnDisk"		"12753874736"
	"InstalledDepots"
	{
		"621"
		{
			"manifest"		"9163237585984972139"
			"size"		"12753874736"
		}
	}
	"UserConfig"
	{
		"language"		"english"
	}
}
"#;
        let library_root = Path::new(r"F:\SteamLibrary");

        let game = parse_appmanifest(acf, library_root).expect("expected a parsed game");

        assert_eq!(game.name, "Portal 2");
        assert_eq!(game.app_id.as_deref(), Some("620"));
        assert_eq!(
            game.install_dir,
            PathBuf::from(r"F:\SteamLibrary\steamapps\common\Portal 2")
        );
    }

    #[test]
    fn parse_appmanifest_returns_none_when_name_missing() {
        let acf = r#"
"AppState"
{
	"appid"		"620"
	"installdir"		"Portal 2"
}
"#;
        assert!(parse_appmanifest(acf, Path::new(r"F:\SteamLibrary")).is_none());
    }

    #[test]
    fn parse_appmanifest_returns_none_when_installdir_missing() {
        let acf = r#"
"AppState"
{
	"appid"		"620"
	"name"		"Portal 2"
}
"#;
        assert!(parse_appmanifest(acf, Path::new(r"F:\SteamLibrary")).is_none());
    }

    #[test]
    fn parse_appmanifest_returns_none_on_garbage_input() {
        assert!(
            parse_appmanifest("not a vdf file at all", Path::new(r"F:\SteamLibrary")).is_none()
        );
        assert!(parse_appmanifest("", Path::new(r"F:\SteamLibrary")).is_none());
    }

    #[test]
    fn parse_appmanifest_ignores_empty_name_and_installdir() {
        let acf = r#"
"AppState"
{
	"appid"		"620"
	"name"		""
	"installdir"		""
}
"#;
        assert!(parse_appmanifest(acf, Path::new(r"F:\SteamLibrary")).is_none());
    }

    // -- GT-23: InstalledDepots (depotcache proof-of-need) --

    #[test]
    fn parse_installed_depots_reads_every_depot_manifest_pair() {
        // Shaped after the real appmanifest_620.acf read on this machine.
        let acf = r#"
"AppState"
{
	"appid"		"620"
	"name"		"Portal 2"
	"installdir"		"Portal 2"
	"InstalledDepots"
	{
		"621"
		{
			"manifest"		"9122696443005314333"
			"size"		"12674356162"
		}
		"622"
		{
			"manifest"		"916560128540961379"
		}
	}
}
"#;
        let mut depots = parse_installed_depots(acf);
        depots.sort();
        assert_eq!(
            depots,
            vec![
                ("621".to_string(), "9122696443005314333".to_string()),
                ("622".to_string(), "916560128540961379".to_string()),
            ]
        );
    }

    #[test]
    fn parse_installed_depots_is_empty_without_the_block() {
        let acf = r#"
"AppState"
{
	"appid"		"620"
}
"#;
        assert!(parse_installed_depots(acf).is_empty());
        assert!(parse_installed_depots("not a vdf file at all").is_empty());
    }

    #[test]
    fn parse_installed_depots_skips_a_depot_with_no_manifest_field() {
        let acf = r#"
"AppState"
{
	"appid"		"620"
	"InstalledDepots"
	{
		"621"
		{
			"size"		"12674356162"
		}
	}
}
"#;
        assert!(
            parse_installed_depots(acf).is_empty(),
            "a depot entry the parser cannot read proves nothing needed - it \
             must not silently vanish from the needed set as if it were read \
             and found empty"
        );
    }

    #[test]
    fn installed_depots_for_library_unions_every_manifest_in_the_library() {
        let dir = tempfile::tempdir().unwrap();
        let steamapps = dir.path().join("steamapps");
        std::fs::create_dir(&steamapps).unwrap();
        std::fs::write(
            steamapps.join("appmanifest_620.acf"),
            r#"
"AppState"
{
	"appid"		"620"
	"InstalledDepots"
	{
		"621"
		{
			"manifest"		"999"
		}
	}
}
"#,
        )
        .unwrap();
        std::fs::write(
            steamapps.join("appmanifest_730.acf"),
            r#"
"AppState"
{
	"appid"		"730"
	"InstalledDepots"
	{
		"731"
		{
			"manifest"		"111"
		}
	}
}
"#,
        )
        .unwrap();

        let mut pairs = installed_depots_for_library(dir.path()).unwrap();
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                ("621".to_string(), "999".to_string()),
                ("731".to_string(), "111".to_string()),
            ]
        );
    }

    #[test]
    fn installed_depots_for_library_errs_on_a_malformed_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let steamapps = dir.path().join("steamapps");
        std::fs::create_dir(&steamapps).unwrap();
        std::fs::write(steamapps.join("appmanifest_620.acf"), "\"AppState\" {").unwrap();

        assert!(
            installed_depots_for_library(dir.path()).is_err(),
            "an unreadable manifest must fail the whole read, never be \
             skipped as if it needed nothing"
        );
    }

    #[test]
    fn installed_depots_for_library_errs_on_a_missing_steamapps_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(installed_depots_for_library(dir.path()).is_err());
    }

    // -- GT-23: WorkshopItemsInstalled (workshop proof-of-liveness) --

    #[test]
    fn parse_workshop_installed_items_reads_the_id_set() {
        // Shaped after the real appworkshop_217140.acf read on this machine.
        let acf = r#"
"AppWorkshop"
{
	"appid"		"217140"
	"SizeOnDisk"		"1710646703"
	"WorkshopItemsInstalled"
	{
		"314615005"
		{
			"size"		"53915155"
			"manifest"		"-1"
		}
		"321449983"
		{
			"size"		"18948098"
			"manifest"		"-1"
		}
	}
}
"#;
        let items = parse_workshop_installed_items(acf).expect("expected a parsed item set");
        let expected: HashSet<String> = ["314615005".to_string(), "321449983".to_string()]
            .into_iter()
            .collect();
        assert_eq!(items, expected);
    }

    #[test]
    fn parse_workshop_installed_items_trusts_an_explicit_empty_block_as_zero() {
        let acf = r#"
"AppWorkshop"
{
	"appid"		"217140"
	"WorkshopItemsInstalled"
	{
	}
}
"#;
        assert_eq!(
            parse_workshop_installed_items(acf),
            Some(HashSet::new()),
            "Steam wrote the key - it is just empty - so zero items is trusted"
        );
    }

    #[test]
    fn parse_workshop_installed_items_is_none_without_the_installed_key() {
        // No real file on this machine ever showed this shape; treat it as
        // unproven rather than "confidently zero" (fail-closed).
        let acf = r#"
"AppWorkshop"
{
	"appid"		"217140"
}
"#;
        assert!(parse_workshop_installed_items(acf).is_none());
    }

    #[test]
    fn parse_workshop_installed_items_is_none_on_garbage_input() {
        assert!(parse_workshop_installed_items("not a vdf file at all").is_none());
        assert!(parse_workshop_installed_items("").is_none());
    }
}
