//! Finite State Machine (FSM) & Debounce Tracker for Game Manifests.
//!
//! Provides a 2.5s debounce settling window, non-blocking file-lock checks with
//! 1.0s backoff for `ERROR_SHARING_VIOLATION`, strict state validation
//! (`StateFlags == 4` for Steam, `bIsIncompleteInstall == false` for Epic),
//! and comparison against the SQLite `gametrimmer.db` baseline.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde::Deserialize;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION,
    INVALID_HANDLE_VALUE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_SHARE_READ, OPEN_EXISTING,
};

use crate::watcher::{LauncherKind, ManifestEvent};

pub const SETTLE_DURATION: Duration = Duration::from_millis(2500);
pub const RETRY_LOCK_DELAY: Duration = Duration::from_millis(1000);

/// Verification status of a manifest file on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestStateStatus {
    /// File is still being downloaded or updated by the launcher (not fully installed).
    Incomplete {
        app_id: String,
        name: String,
        reason: String,
    },
    /// File is fully installed and verified.
    Ready {
        app_id: String,
        name: String,
        build_id: Option<String>,
        install_dir: Option<PathBuf>,
        launcher: LauncherKind,
    },
    /// File could not be parsed or is irrelevant.
    Invalid,
}

/// A detected and verified game update event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedGameUpdate {
    pub app_id: String,
    pub name: String,
    pub old_build_id: Option<String>,
    pub new_build_id: Option<String>,
    pub launcher: String,
    pub install_dir: Option<PathBuf>,
}

/// State of a pending manifest path in the debounce FSM.
#[derive(Debug, Clone)]
struct PendingManifest {
    last_event_time: Instant,
    launcher: LauncherKind,
    retry_count: u32,
}

/// FSM Debounce Tracker.
pub struct FsmDebouncer {
    pending: HashMap<PathBuf, PendingManifest>,
    baseline_established: bool,
    known_build_ids: HashMap<String, String>,
}

impl FsmDebouncer {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            baseline_established: false,
            known_build_ids: HashMap::new(),
        }
    }

    /// Feeds a new file change event into the debouncer.
    pub fn record_event(&mut self, event: ManifestEvent) {
        let entry = self.pending.entry(event.file_path).or_insert_with(|| PendingManifest {
            last_event_time: Instant::now(),
            launcher: event.launcher,
            retry_count: 0,
        });
        entry.last_event_time = Instant::now();
        entry.launcher = event.launcher;
    }

    /// Whether any manifest files are currently awaiting settling.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Check for settled files, verify locks, parse manifest states, and compare against DB.
    pub fn check_settled(
        &mut self,
        db_conn: Option<&Connection>,
    ) -> Vec<VerifiedGameUpdate> {
        let now = Instant::now();
        let mut ready_paths = Vec::new();
        let mut updates = Vec::new();

        for (path, pending) in &mut self.pending {
            if now.duration_since(pending.last_event_time) >= SETTLE_DURATION {
                ready_paths.push((path.clone(), pending.launcher, pending.retry_count));
            }
        }

        for (path, launcher, retry_count) in ready_paths {
            if !path.exists() {
                self.pending.remove(&path);
                continue;
            }

            // 1. File lock check: test open with CreateFileW
            if is_file_locked(&path) {
                if retry_count < 10 {
                    // Delay retry by 1.0s
                    if let Some(entry) = self.pending.get_mut(&path) {
                        entry.last_event_time = now - SETTLE_DURATION + RETRY_LOCK_DELAY;
                        entry.retry_count += 1;
                    }
                } else {
                    self.pending.remove(&path);
                }
                continue;
            }

            // 2. Read and verify state
            match read_manifest_state(&path, launcher) {
                ManifestStateStatus::Ready {
                    app_id,
                    name,
                    build_id,
                    install_dir,
                    launcher,
                } => {
                    self.pending.remove(&path);

                    if let Some(update) = self.evaluate_game_state(
                        &app_id,
                        &name,
                        build_id.as_deref(),
                        install_dir,
                        launcher,
                        db_conn,
                    ) {
                        updates.push(update);
                    }
                }
                ManifestStateStatus::Incomplete { .. } => {
                    // Launcher is actively installing/downloading; wait for subsequent event
                    if let Some(entry) = self.pending.get_mut(&path) {
                        entry.last_event_time = now - SETTLE_DURATION + Duration::from_secs(2);
                    }
                }
                ManifestStateStatus::Invalid => {
                    self.pending.remove(&path);
                }
            }
        }

        updates
    }

    /// Evaluates game build_id against in-memory state and SQLite database.
    fn evaluate_game_state(
        &mut self,
        app_id: &str,
        name: &str,
        new_build_id: Option<&str>,
        install_dir: Option<PathBuf>,
        launcher: LauncherKind,
        db_conn: Option<&Connection>,
    ) -> Option<VerifiedGameUpdate> {
        let current_known = self.known_build_ids.get(app_id).cloned();

        // Check against database
        let db_build_id = db_conn.and_then(|conn| query_db_game_build_id(conn, app_id));

        let baseline_id = db_build_id.or(current_known);

        if let Some(new_id) = new_build_id {
            if let Some(ref old_id) = baseline_id {
                if old_id != new_id {
                    self.known_build_ids.insert(app_id.to_string(), new_id.to_string());
                    return Some(VerifiedGameUpdate {
                        app_id: app_id.to_string(),
                        name: name.to_string(),
                        old_build_id: Some(old_id.clone()),
                        new_build_id: Some(new_id.to_string()),
                        launcher: launcher.as_str().to_string(),
                        install_dir,
                    });
                }
            } else {
                // Cold start / establishing baseline
                self.known_build_ids.insert(app_id.to_string(), new_id.to_string());
            }
        }

        None
    }

    pub fn set_baseline_established(&mut self, established: bool) {
        self.baseline_established = established;
    }

    pub fn is_baseline_established(&self) -> bool {
        self.baseline_established
    }

    pub fn tracked_count(&self) -> usize {
        self.known_build_ids.len()
    }
}

impl Default for FsmDebouncer {
    fn default() -> Self {
        Self::new()
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Checks if a file is exclusively locked by another process (e.g. Steam writing).
pub fn is_file_locked(path: &Path) -> bool {
    let wide_path = to_wide(&path.to_string_lossy());
    let handle = unsafe {
        CreateFileW(
            PCWSTR::from_raw(wide_path.as_ptr()),
            FILE_GENERIC_READ.0,
            FILE_SHARE_READ,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    };

    match handle {
        Ok(h) if h != INVALID_HANDLE_VALUE && !h.is_invalid() => {
            unsafe {
                let _ = CloseHandle(h);
            }
            false
        }
        _ => {
            let err = unsafe { GetLastError() };
            err == ERROR_SHARING_VIOLATION || err == ERROR_LOCK_VIOLATION
        }
    }
}

/// Reads and validates the manifest state from disk.
pub fn read_manifest_state(path: &Path, launcher: LauncherKind) -> ManifestStateStatus {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return ManifestStateStatus::Invalid,
    };

    match launcher {
        LauncherKind::Steam => parse_steam_manifest(&content, path),
        LauncherKind::Epic => parse_epic_manifest(&content),
        LauncherKind::Gog | LauncherKind::Custom => {
            if path.extension().and_then(|e| e.to_str()).map_or(false, |e| e.eq_ignore_ascii_case("acf")) {
                parse_steam_manifest(&content, path)
            } else if path.extension().and_then(|e| e.to_str()).map_or(false, |e| e.eq_ignore_ascii_case("item")) {
                parse_epic_manifest(&content)
            } else {
                ManifestStateStatus::Invalid
            }
        }
    }
}

/// Steam ACF manifest state parsing with StateFlags validation.
pub fn parse_steam_manifest(acf: &str, manifest_path: &Path) -> ManifestStateStatus {
    let app_id = extract_vdf_field(acf, "appid");
    let name = extract_vdf_field(acf, "name");
    let state_flags_str = extract_vdf_field(acf, "StateFlags");
    let build_id = extract_vdf_field(acf, "buildid");
    let install_dir_name = extract_vdf_field(acf, "installdir");

    let Some(app_id) = app_id.filter(|s| !s.trim().is_empty()) else {
        return ManifestStateStatus::Invalid;
    };
    let name = name.unwrap_or_else(|| format!("Steam App {app_id}"));

    // StateFlags == 4 means StateFullyInstalled
    let state_flags: u64 = state_flags_str.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0);
    if state_flags != 4 {
        return ManifestStateStatus::Incomplete {
            app_id,
            name,
            reason: format!("StateFlags is {state_flags} (expected 4 for FullyInstalled)"),
        };
    }

    let install_dir = install_dir_name.and_then(|dir_name| {
        manifest_path.parent().map(|steamapps| {
            steamapps.join("common").join(dir_name)
        })
    });

    ManifestStateStatus::Ready {
        app_id,
        name,
        build_id,
        install_dir,
        launcher: LauncherKind::Steam,
    }
}

#[derive(Debug, Deserialize)]
struct EpicItemJson {
    #[serde(rename = "AppName")]
    app_name: Option<String>,
    #[serde(rename = "DisplayName")]
    display_name: Option<String>,
    #[serde(rename = "bIsIncompleteInstall")]
    is_incomplete: Option<bool>,
    #[serde(rename = "AppVersionString")]
    app_version: Option<String>,
    #[serde(rename = "InstallationGuid")]
    install_guid: Option<String>,
    #[serde(rename = "InstallLocation")]
    install_location: Option<String>,
}

/// Epic JSON manifest state parsing with bIsIncompleteInstall validation.
pub fn parse_epic_manifest(json_str: &str) -> ManifestStateStatus {
    let manifest: EpicItemJson = match serde_json::from_str(json_str) {
        Ok(m) => m,
        Err(_) => return ManifestStateStatus::Invalid,
    };

    let Some(app_id) = manifest.app_name.filter(|s| !s.trim().is_empty()) else {
        return ManifestStateStatus::Invalid;
    };
    let name = manifest.display_name.unwrap_or_else(|| app_id.clone());

    if manifest.is_incomplete == Some(true) {
        return ManifestStateStatus::Incomplete {
            app_id,
            name,
            reason: "bIsIncompleteInstall is true".to_string(),
        };
    }

    let build_id = manifest.app_version.or(manifest.install_guid);
    let install_dir = manifest.install_location.map(PathBuf::from);

    ManifestStateStatus::Ready {
        app_id,
        name,
        build_id,
        install_dir,
        launcher: LauncherKind::Epic,
    }
}

/// Simple regex/token extraction for key-value pair in Valve ACF text.
fn extract_vdf_field(acf: &str, target_key: &str) -> Option<String> {
    for line in acf.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('"') {
            let tokens: Vec<&str> = trimmed
                .split('"')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            if tokens.len() >= 2 && tokens[0].eq_ignore_ascii_case(target_key) {
                return Some(tokens[1].to_string());
            }
        }
    }
    None
}

/// Queries `gametrimmer.db` for the game's stored `build_id`.
fn query_db_game_build_id(conn: &Connection, app_id: &str) -> Option<String> {
    let mut stmt = conn
        .prepare("SELECT build_id FROM games WHERE app_id = ?1 LIMIT 1")
        .ok()?;
    stmt.query_row([app_id], |row| row.get(0)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn parse_steam_manifest_fully_installed() {
        let acf = r#"
"AppState"
{
    "appid"       "730"
    "name"        "Counter-Strike 2"
    "StateFlags"  "4"
    "installdir"  "Counter-Strike Global Offensive"
    "buildid"     "1337420"
}
"#;
        let manifest_path = Path::new(r"C:\Steam\steamapps\appmanifest_730.acf");
        let res = parse_steam_manifest(acf, manifest_path);

        assert_eq!(
            res,
            ManifestStateStatus::Ready {
                app_id: "730".to_string(),
                name: "Counter-Strike 2".to_string(),
                build_id: Some("1337420".to_string()),
                install_dir: Some(PathBuf::from(
                    r"C:\Steam\steamapps\common\Counter-Strike Global Offensive"
                )),
                launcher: LauncherKind::Steam,
            }
        );
    }

    #[test]
    fn parse_steam_manifest_downloading_incomplete() {
        let acf = r#"
"AppState"
{
    "appid"       "730"
    "name"        "Counter-Strike 2"
    "StateFlags"  "1026"
    "installdir"  "Counter-Strike Global Offensive"
    "buildid"     "1337420"
}
"#;
        let manifest_path = Path::new(r"C:\Steam\steamapps\appmanifest_730.acf");
        let res = parse_steam_manifest(acf, manifest_path);

        match res {
            ManifestStateStatus::Incomplete { app_id, .. } => {
                assert_eq!(app_id, "730");
            }
            other => panic!("expected Incomplete, got {:?}", other),
        }
    }

    #[test]
    fn parse_epic_manifest_ready_and_incomplete() {
        let ready_json = r#"{
            "AppName": "Fortnite",
            "DisplayName": "Fortnite Battle Royale",
            "bIsIncompleteInstall": false,
            "AppVersionString": "29.40.1",
            "InstallLocation": "E:\\Games\\Fortnite"
        }"#;

        let res = parse_epic_manifest(ready_json);
        assert_eq!(
            res,
            ManifestStateStatus::Ready {
                app_id: "Fortnite".to_string(),
                name: "Fortnite Battle Royale".to_string(),
                build_id: Some("29.40.1".to_string()),
                install_dir: Some(PathBuf::from(r"E:\Games\Fortnite")),
                launcher: LauncherKind::Epic,
            }
        );

        let incomplete_json = r#"{
            "AppName": "Fortnite",
            "DisplayName": "Fortnite Battle Royale",
            "bIsIncompleteInstall": true,
            "AppVersionString": "29.40.1",
            "InstallLocation": "E:\\Games\\Fortnite"
        }"#;

        let inc_res = parse_epic_manifest(incomplete_json);
        match inc_res {
            ManifestStateStatus::Incomplete { app_id, .. } => {
                assert_eq!(app_id, "Fortnite");
            }
            other => panic!("expected Incomplete, got {:?}", other),
        }
    }

    #[test]
    fn fsm_evaluates_game_update_against_sqlite() {
        let conn = Connection::open_in_memory().expect("open memory db");
        conn.execute(
            "CREATE TABLE games (id INTEGER PRIMARY KEY, app_id TEXT, name TEXT, build_id TEXT)",
            [],
        )
        .expect("create table");
        conn.execute(
            "INSERT INTO games (app_id, name, build_id) VALUES ('730', 'CS2', '1000')",
            [],
        )
        .expect("insert game");

        let mut fsm = FsmDebouncer::new();

        // Same build id -> no update
        let res1 = fsm.evaluate_game_state("730", "CS2", Some("1000"), None, LauncherKind::Steam, Some(&conn));
        assert!(res1.is_none());

        // New build id -> VerifiedGameUpdate!
        let res2 = fsm.evaluate_game_state("730", "CS2", Some("2000"), None, LauncherKind::Steam, Some(&conn));
        assert!(res2.is_some());
        let update = res2.unwrap();
        assert_eq!(update.old_build_id, Some("1000".to_string()));
        assert_eq!(update.new_build_id, Some("2000".to_string()));
        assert_eq!(update.app_id, "730");
    }
}
