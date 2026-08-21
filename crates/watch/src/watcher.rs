//! Windows IOCP Directory Watcher for game manifest directories.
//!
//! Uses `CreateIoCompletionPort` and `ReadDirectoryChangesW` with fixed 8192-byte
//! buffers and `bWatchSubtree = FALSE` for high-efficiency, zero-CPU background
//! monitoring of Steam `steamapps/`, Epic `Manifests/`, GOG, and custom library folders.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gametrimmer_core::db;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadDirectoryChangesW, FILE_ACTION_ADDED, FILE_ACTION_MODIFIED,
    FILE_ACTION_RENAMED_NEW_NAME, FILE_ACTION_RENAMED_OLD_NAME, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_NOTIFY_INFORMATION,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::{CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED};

pub const BUFFER_SIZE: usize = 8192;

#[repr(C, align(4))]
pub struct AlignedBuffer([u8; BUFFER_SIZE]);

impl AlignedBuffer {
    pub fn new() -> Self {
        Self([0u8; BUFFER_SIZE])
    }
}

impl Default for AlignedBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Launcher vendor / origin for the watched directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LauncherKind {
    Steam,
    Epic,
    Gog,
    Custom,
}

impl LauncherKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            LauncherKind::Steam => "steam",
            LauncherKind::Epic => "epic",
            LauncherKind::Gog => "gog",
            LauncherKind::Custom => "custom",
        }
    }
}

/// Action detected by ReadDirectoryChangesW.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatcherAction {
    Added,
    Modified,
    Removed,
    Renamed,
}

/// An individual manifest file event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEvent {
    pub dir_path: PathBuf,
    pub file_path: PathBuf,
    pub file_name: String,
    pub action: WatcherAction,
    pub launcher: LauncherKind,
}

/// Structure holding state for one active watched directory.
struct WatchedDirectory {
    path: PathBuf,
    launcher: LauncherKind,
    handle: HANDLE,
    buffer: Box<AlignedBuffer>,
    overlapped: Box<OVERLAPPED>,
}

// HANDLE is safe to send across threads when owned exclusively
unsafe impl Send for WatchedDirectory {}

impl WatchedDirectory {
    fn new(path: PathBuf, launcher: LauncherKind) -> std::io::Result<Self> {
        let wide_path = to_wide(&path.to_string_lossy());
        let handle = unsafe {
            CreateFileW(
                PCWSTR::from_raw(wide_path.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .map_err(|e| std::io::Error::other(format!("CreateFileW on {:?}: {e}", path)))?;

        if handle == INVALID_HANDLE_VALUE || handle.is_invalid() {
            return Err(std::io::Error::last_os_error());
        }

        Ok(Self {
            path,
            launcher,
            handle,
            buffer: Box::new(AlignedBuffer::new()),
            overlapped: Box::new(OVERLAPPED::default()),
        })
    }

    fn arm_read(&mut self) -> std::io::Result<()> {
        // Reset overlapped before calling
        self.overlapped.Internal = 0;
        self.overlapped.InternalHigh = 0;
        self.overlapped.Anonymous.Anonymous.Offset = 0;
        self.overlapped.Anonymous.Anonymous.OffsetHigh = 0;

        let notify_filter =
            FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE;

        let ok = unsafe {
            ReadDirectoryChangesW(
                self.handle,
                self.buffer.0.as_mut_ptr() as *mut _,
                BUFFER_SIZE as u32,
                false, // bWatchSubtree = FALSE
                notify_filter,
                None,
                Some(self.overlapped.as_mut() as *mut _),
                None,
            )
        };

        if ok.is_err() {
            let err = unsafe { GetLastError() };
            return Err(std::io::Error::from_raw_os_error(err.0 as i32));
        }

        Ok(())
    }
}

impl Drop for WatchedDirectory {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Outcome of one `poll_events` call: parsed manifest events, plus any
/// directories whose `ReadDirectoryChangesW` buffer overflowed (or whose
/// watch could not be re-armed) since the previous call.
///
/// Overflow means events were lost for that directory - there is no way to
/// get them back - so the caller's job is not to recover them but to notice
/// and fall back to a full rescan of that directory instead of continuing
/// to trust an IOCP stream it now knows is incomplete.
#[derive(Debug, Default)]
pub struct PollResult {
    pub events: Vec<ManifestEvent>,
    pub overflowed_dirs: Vec<PathBuf>,
}

/// What one `GetQueuedCompletionStatus` completion means for the directory
/// it belongs to. See `ManifestWatcher::classify_completion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionOutcome {
    Error,
    Overflow,
    Data,
}

/// The ManifestWatcher daemon manager.
pub struct ManifestWatcher {
    iocp: HANDLE,
    directories: Vec<WatchedDirectory>,
    paused: Arc<AtomicBool>,
}

unsafe impl Send for ManifestWatcher {}
unsafe impl Sync for ManifestWatcher {}

impl ManifestWatcher {
    /// Creates a new `ManifestWatcher` and registers the provided directories.
    pub fn new(paths: &[(PathBuf, LauncherKind)]) -> std::io::Result<Self> {
        let iocp = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 0) }
            .map_err(|e| std::io::Error::other(format!("CreateIoCompletionPort: {e}")))?;

        if iocp == INVALID_HANDLE_VALUE || iocp.is_invalid() {
            return Err(std::io::Error::last_os_error());
        }

        let mut watcher = Self {
            iocp,
            directories: Vec::new(),
            paused: Arc::new(AtomicBool::new(false)),
        };

        for (path, launcher) in paths {
            if path.is_dir() {
                let _ = watcher.add_directory(path.clone(), *launcher);
            }
        }

        Ok(watcher)
    }

    /// Adds a directory to monitor.
    pub fn add_directory(&mut self, path: PathBuf, launcher: LauncherKind) -> std::io::Result<()> {
        if self.directories.iter().any(|d| d.path == path) {
            return Ok(());
        }

        let mut watched = WatchedDirectory::new(path, launcher)?;
        let key = self.directories.len();

        unsafe {
            CreateIoCompletionPort(watched.handle, Some(self.iocp), key, 0)
                .map_err(|e| std::io::Error::other(format!("Attach to IOCP: {e}")))?;
        }

        watched.arm_read()?;
        self.directories.push(watched);
        Ok(())
    }

    /// Decides what a `GetQueuedCompletionStatus` completion means, without
    /// looking at the directory or buffer it belongs to.
    ///
    /// Split out from `poll_events` so the two outcomes that were previously
    /// collapsed into one early return - a genuine IOCP error, and a
    /// successful completion that reports zero bytes transferred - can be
    /// told apart by a unit test without a real IOCP handle. Zero bytes on a
    /// *successful* completion is the documented `ReadDirectoryChangesW`
    /// overflow signal (the kernel-side buffer filled faster than we could
    /// drain it), not "nothing happened" - treating it the same as a hard
    /// error is what let a directory go permanently deaf: the caller
    /// returned early and skipped `arm_read`, so no further notification for
    /// that directory could ever arrive.
    fn classify_completion(ok: bool, bytes_transferred: u32) -> CompletionOutcome {
        if !ok {
            CompletionOutcome::Error
        } else if bytes_transferred == 0 {
            CompletionOutcome::Overflow
        } else {
            CompletionOutcome::Data
        }
    }

    /// Polls for completed directory change notifications with a timeout.
    pub fn poll_events(&mut self, timeout_ms: u32) -> PollResult {
        if self.paused.load(Ordering::SeqCst) || self.directories.is_empty() {
            if timeout_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(timeout_ms as u64));
            }
            return PollResult::default();
        }

        let mut bytes_transferred: u32 = 0;
        let mut completion_key: usize = 0;
        let mut lp_overlapped: *mut OVERLAPPED = ptr::null_mut();

        let ok = unsafe {
            GetQueuedCompletionStatus(
                self.iocp,
                &mut bytes_transferred,
                &mut completion_key,
                &mut lp_overlapped,
                timeout_ms,
            )
        };

        let outcome = Self::classify_completion(ok.is_ok(), bytes_transferred);
        if matches!(outcome, CompletionOutcome::Error) || completion_key >= self.directories.len() {
            // A genuine IOCP error, or a completion key we no longer track -
            // there is no live watch to re-arm here.
            return PollResult::default();
        }

        let dir = &mut self.directories[completion_key];
        let mut result = PollResult::default();

        // Only `Data` and `Overflow` can reach this point - `Error` already
        // returned above - so this is a plain two-way branch rather than a
        // match with an arm that can never run.
        if outcome == CompletionOutcome::Overflow {
            // The events inside this window are gone for good; surface the
            // directory so the caller can fall back to a full rescan instead
            // of silently trusting a stream that just lost data.
            result.overflowed_dirs.push(dir.path.clone());
        } else {
            let buffer_slice = &dir.buffer.0[..bytes_transferred as usize];
            result.events = parse_notify_records(buffer_slice, &dir.path, dir.launcher);
        }

        // Re-arm in both the data and the overflow case - a live directory
        // handle must always come back under watch, or the very first
        // overflow would be the last notification it ever produced. If the
        // re-arm itself fails (e.g. the directory was removed), treat that
        // the same as an overflow: either way the caller's picture of this
        // directory can no longer be trusted without a rescan.
        if dir.arm_read().is_err() && !result.overflowed_dirs.contains(&dir.path) {
            result.overflowed_dirs.push(dir.path.clone());
        }

        result
    }

    pub fn watched_paths(&self) -> Vec<PathBuf> {
        self.directories.iter().map(|d| d.path.clone()).collect()
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
}

impl Drop for ManifestWatcher {
    fn drop(&mut self) {
        self.directories.clear();
        unsafe {
            let _ = CloseHandle(self.iocp);
        }
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Parses raw `FILE_NOTIFY_INFORMATION` structs from buffer.
pub fn parse_notify_records(
    buffer: &[u8],
    dir_path: &Path,
    launcher: LauncherKind,
) -> Vec<ManifestEvent> {
    let mut events = Vec::new();
    let mut offset = 0;

    while offset + std::mem::size_of::<FILE_NOTIFY_INFORMATION>() <= buffer.len() {
        let info = unsafe { &*(buffer.as_ptr().add(offset) as *const FILE_NOTIFY_INFORMATION) };
        let name_len_bytes = info.FileNameLength as usize;
        let name_offset = offset + std::mem::offset_of!(FILE_NOTIFY_INFORMATION, FileName);

        if name_offset + name_len_bytes <= buffer.len() {
            let name_slice = unsafe {
                std::slice::from_raw_parts(
                    buffer.as_ptr().add(name_offset) as *const u16,
                    name_len_bytes / 2,
                )
            };
            let file_name = String::from_utf16_lossy(name_slice);

            if is_manifest_file(&file_name, launcher) {
                let action = match info.Action {
                    FILE_ACTION_ADDED => WatcherAction::Added,
                    FILE_ACTION_MODIFIED => WatcherAction::Modified,
                    FILE_ACTION_RENAMED_NEW_NAME | FILE_ACTION_RENAMED_OLD_NAME => {
                        WatcherAction::Renamed
                    }
                    _ => WatcherAction::Modified,
                };

                let file_path = dir_path.join(&file_name);
                events.push(ManifestEvent {
                    dir_path: dir_path.to_path_buf(),
                    file_path,
                    file_name,
                    action,
                    launcher,
                });
            }
        }

        if info.NextEntryOffset == 0 {
            break;
        }
        offset += info.NextEntryOffset as usize;
    }

    events
}

/// Checks if a file is a manifest of interest.
pub fn is_manifest_file(file_name: &str, launcher: LauncherKind) -> bool {
    let name_lower = file_name.to_lowercase();
    match launcher {
        LauncherKind::Steam => {
            name_lower.starts_with("appmanifest_") && name_lower.ends_with(".acf")
        }
        LauncherKind::Epic => name_lower.ends_with(".item"),
        LauncherKind::Gog => {
            (name_lower.starts_with("goggame-") && name_lower.ends_with(".info"))
                || name_lower == "goggame.info"
                || name_lower.ends_with(".manifest")
        }
        LauncherKind::Custom => {
            (name_lower.starts_with("appmanifest_") && name_lower.ends_with(".acf"))
                || name_lower.ends_with(".item")
                || (name_lower.starts_with("goggame-") && name_lower.ends_with(".info"))
                || name_lower.ends_with(".manifest")
        }
    }
}

/// Discovers all manifest directories across Steam, Epic, GOG, and custom DB libraries.
pub fn discover_watch_directories(db_path: Option<&Path>) -> Vec<(PathBuf, LauncherKind)> {
    let mut dirs = Vec::new();
    let mut seen = HashSet::new();

    // 1. Steam discovery
    for steam_dir in discover_steam_manifest_dirs() {
        if seen.insert(steam_dir.to_string_lossy().to_lowercase()) {
            dirs.push((steam_dir, LauncherKind::Steam));
        }
    }

    // 2. Epic discovery
    if let Some(epic_dir) = discover_epic_manifest_dir() {
        if seen.insert(epic_dir.to_string_lossy().to_lowercase()) {
            dirs.push((epic_dir, LauncherKind::Epic));
        }
    }

    // 3. Custom and DB registered libraries
    if let Some(db_p) = db_path {
        if db_p.exists() {
            if let Ok(conn) = db::open(db_p) {
                if let Ok(mut stmt) = conn.prepare("SELECT vendor, path FROM game_libraries") {
                    if let Ok(rows) = stmt.query_map([], |row| {
                        let vendor: String = row.get(0)?;
                        let path: String = row.get(1)?;
                        Ok((vendor, PathBuf::from(path)))
                    }) {
                        for row in rows.flatten() {
                            let (vendor, path) = row;
                            let (target_dir, kind) = match vendor.to_lowercase().as_str() {
                                "steam" => (path.join("steamapps"), LauncherKind::Steam),
                                "epic" => (path, LauncherKind::Epic),
                                "gog" => (path, LauncherKind::Gog),
                                _ => (path, LauncherKind::Custom),
                            };
                            if target_dir.is_dir()
                                && seen.insert(target_dir.to_string_lossy().to_lowercase())
                            {
                                dirs.push((target_dir, kind));
                            }
                        }
                    }
                }
            }
        }
    }

    dirs
}

fn discover_steam_manifest_dirs() -> Vec<PathBuf> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let mut result = Vec::new();
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) = hkcu.open_subkey(r"SOFTWARE\Valve\Steam") {
        if let Ok(steam_path) = key.get_value::<String, _>("SteamPath") {
            let root = PathBuf::from(steam_path.replace('/', "\\"));
            let main_steamapps = root.join("steamapps");
            if main_steamapps.is_dir() {
                result.push(main_steamapps.clone());
            }

            // Read libraryfolders.vdf
            let vdf_path = main_steamapps.join("libraryfolders.vdf");
            if let Ok(vdf_content) = std::fs::read_to_string(&vdf_path) {
                for line in vdf_content.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("\"path\"") {
                        let parts: Vec<&str> = trimmed
                            .split('"')
                            .filter(|s| !s.trim().is_empty())
                            .collect();
                        if parts.len() >= 2 {
                            let lib_path = PathBuf::from(parts[1].replace(r"\\", r"\"));
                            let lib_steamapps = lib_path.join("steamapps");
                            if lib_steamapps.is_dir() {
                                result.push(lib_steamapps);
                            }
                        }
                    }
                }
            }
        }
    }

    result
}

fn discover_epic_manifest_dir() -> Option<PathBuf> {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let manifests_dir =
        if let Ok(key) = hklm.open_subkey(r"SOFTWARE\WOW6432Node\Epic Games\EpicGamesLauncher") {
            if let Ok(app_data) = key.get_value::<String, _>("AppDataPath") {
                let p = PathBuf::from(app_data.replace('/', "\\")).join("Manifests");
                if p.is_dir() {
                    Some(p)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

    manifests_dir.or_else(|| {
        let default_p = PathBuf::from(r"C:\ProgramData\Epic\EpicGamesLauncher\Data\Manifests");
        if default_p.is_dir() {
            Some(default_p)
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_manifest_file_checks_proper_extensions() {
        assert!(is_manifest_file("appmanifest_730.acf", LauncherKind::Steam));
        assert!(is_manifest_file(
            "appmanifest_123456.acf",
            LauncherKind::Steam
        ));
        assert!(!is_manifest_file("libraryfolders.vdf", LauncherKind::Steam));
        assert!(!is_manifest_file(
            "appmanifest_730.txt",
            LauncherKind::Steam
        ));

        assert!(is_manifest_file("Fortnite.item", LauncherKind::Epic));
        assert!(is_manifest_file(
            "5CB97847CEE34581AB9576419A91D9F3.item",
            LauncherKind::Epic
        ));
        assert!(!is_manifest_file("Fortnite.json", LauncherKind::Epic));

        assert!(is_manifest_file("goggame-12345.info", LauncherKind::Gog));
        assert!(is_manifest_file("goggame.info", LauncherKind::Gog));
        assert!(is_manifest_file("game.manifest", LauncherKind::Gog));

        assert!(is_manifest_file(
            "appmanifest_550.acf",
            LauncherKind::Custom
        ));
        assert!(is_manifest_file("game.item", LauncherKind::Custom));
    }

    #[test]
    fn manifest_watcher_creation_with_tempdir() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let steamapps = temp_dir.path().join("steamapps");
        std::fs::create_dir_all(&steamapps).expect("create steamapps");

        let paths = vec![(steamapps.clone(), LauncherKind::Steam)];
        let mut watcher = ManifestWatcher::new(&paths).expect("create watcher");

        assert_eq!(watcher.watched_paths().len(), 1);
        assert!(!watcher.is_paused());

        watcher.pause();
        assert!(watcher.is_paused());
        watcher.resume();
        assert!(!watcher.is_paused());

        // Polling with short timeout should return empty
        let result = watcher.poll_events(10);
        assert!(result.events.is_empty());
        assert!(result.overflowed_dirs.is_empty());
    }

    #[test]
    fn classify_completion_treats_zero_bytes_as_overflow_not_absence() {
        // The bug this guards against: a successful completion with zero
        // bytes transferred used to be folded into the same early return as
        // a hard IOCP error, which skipped re-arming the watch and left the
        // directory permanently deaf after the first burst that overflowed
        // its buffer.
        assert_eq!(
            ManifestWatcher::classify_completion(true, 0),
            CompletionOutcome::Overflow
        );
    }

    #[test]
    fn classify_completion_distinguishes_error_from_overflow() {
        assert_eq!(
            ManifestWatcher::classify_completion(false, 0),
            CompletionOutcome::Error
        );
        // A failed call reporting a nonzero byte count (should not happen in
        // practice, but the classification must not depend on it) is still
        // an error - `ok` alone decides this branch.
        assert_eq!(
            ManifestWatcher::classify_completion(false, 42),
            CompletionOutcome::Error
        );
    }

    #[test]
    fn classify_completion_recognizes_ordinary_data() {
        assert_eq!(
            ManifestWatcher::classify_completion(true, 128),
            CompletionOutcome::Data
        );
    }
}
