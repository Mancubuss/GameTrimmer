//! Fail-closed filesystem evidence used by destructive operations.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::error::{CoreError, Result};
use crate::mftscan::MftIdentity;

// The reparse bit is read off two sources that were proven to agree on it:
// `BY_HANDLE_FILE_INFORMATION.dwFileAttributes`, which only the Windows
// `identity()` has, and an `$MFT` record's `$STANDARD_INFORMATION` bits (see
// [`stated_identity`]). The directory bit only ever comes from the former.
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
#[cfg(windows)]
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    File,
    Directory,
}

impl TargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "file" => Some(Self::File),
            "directory" => Some(Self::Directory),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIdentity {
    /// `u64` rather than the `u32` a Windows volume serial number actually
    /// is, because this field also carries `st_dev` on the non-Windows
    /// `identity()` used by tests run off-Windows - and `st_dev` is a `u64`
    /// on every platform `std::os::unix::fs::MetadataExt` supports. A `u32`
    /// field would force a truncating cast there, which is exactly the
    /// narrowing this type exists to avoid: two different devices could
    /// collide onto the same stored value and `same_object` would call them
    /// the same volume.
    pub volume_serial: u64,
    pub file_index: u64,
    pub kind: TargetKind,
    pub size: u64,
    pub last_write_time: u64,
    pub attributes: u32,
}

impl FileIdentity {
    pub fn encode(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}",
            self.volume_serial,
            self.file_index,
            self.kind.as_str(),
            self.size,
            self.last_write_time,
            self.attributes
        )
    }

    pub fn decode(value: &str) -> Result<Self> {
        let mut parts = value.split(':');
        let parse = |part: Option<&str>, name: &str| -> Result<u64> {
            part.and_then(|raw| raw.parse().ok())
                .ok_or_else(|| CoreError::Other(format!("invalid {name} in filesystem identity")))
        };
        let volume_serial = parse(parts.next(), "volume serial")?;
        let file_index = parse(parts.next(), "file index")?;
        let kind = parts
            .next()
            .and_then(TargetKind::parse)
            .ok_or_else(|| CoreError::Other("invalid target kind in filesystem identity".into()))?;
        let size = parse(parts.next(), "size")?;
        let last_write_time = parse(parts.next(), "last-write time")?;
        let attributes = u32::try_from(parse(parts.next(), "attributes")?)
            .map_err(|_| CoreError::Other("attributes exceed u32".into()))?;
        if parts.next().is_some() {
            return Err(CoreError::Other(
                "too many fields in filesystem identity".into(),
            ));
        }
        Ok(Self {
            volume_serial,
            file_index,
            kind,
            size,
            last_write_time,
            attributes,
        })
    }

    pub fn same_object(&self, other: &Self) -> bool {
        self.volume_serial == other.volume_serial
            && self.file_index == other.file_index
            && self.kind == other.kind
    }
}

/// Whether a finding's launcher-discovery evidence is good enough to allow a
/// deletion, and if not, why. `None` means allowed.
///
/// This is the single copy of a policy that used to exist three times, in two
/// languages, with three different answers:
///
/// - the scan's persistence path blocked only on missing or `degraded`
///   evidence (`worker::scan::persistence`),
/// - the load query's SQL `CASE` blocked on missing, `degraded`, or a
///   non-`complete` status for an *orphan* row (`worker::load::load_findings`),
/// - the delete preflight below allowed only `complete`, plus `heuristic` for a
///   game-bound row, and rejected everything else (`crate::ops`).
///
/// Only three statuses are written today, so the disagreement was unreachable -
/// but "unreachable" is a property of the writer, not of the policy, and the
/// next status added would have silently taken three different paths. The
/// strictest of the three wins here, because this gates deletion: an evidence
/// status this build does not recognise is not evidence.
///
/// `game_bound` is true for a finding inside a known game, false for an orphan
/// candidate. The distinction is the whole reason `heuristic` is conditional:
/// folder heuristics can prove a directory holds a game, never that a launcher
/// has stopped managing it, so they are enough to delete *inside* a game and
/// never enough to call a folder abandoned.
pub fn discovery_block_reason(
    game_bound: bool,
    evidence_status: Option<&str>,
) -> Option<&'static str> {
    match evidence_status {
        None => Some("missing library discovery evidence"),
        Some("complete") => None,
        Some("degraded") => Some("library discovery was degraded"),
        Some("heuristic") if game_bound => None,
        Some(_) if !game_bound => Some("orphan inventory is not authoritative"),
        Some(_) => Some("library discovery is not authoritative"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteBlockReason {
    InvalidRelativePath(String),
    OutsideTrustedRoot,
    ReparsePoint(PathBuf),
    Missing,
    /// The trusted root itself could not be found. Distinct from [`Self::Missing`]
    /// because it says nothing about the target: it blocks, and must never be
    /// read as proof that the target was already deleted.
    RootMissing,
    Metadata(String),
    RootChanged,
    TargetChanged,
    TreeChanged,
    LegacySnapshot,
    DegradedDiscovery,
    MissingSafetyEvidence,
    StaleDatabaseRow,
}

impl fmt::Display for DeleteBlockReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRelativePath(reason) => write!(f, "invalid relative path: {reason}"),
            Self::OutsideTrustedRoot => write!(f, "target is outside its trusted root"),
            Self::ReparsePoint(path) => {
                write!(f, "reparse point is not deletable: {}", path.display())
            }
            Self::Missing => write!(f, "target no longer exists"),
            Self::RootMissing => write!(f, "the library root is currently unreachable"),
            Self::Metadata(error) => write!(f, "filesystem state could not be verified: {error}"),
            Self::RootChanged => write!(f, "trusted root identity changed since the scan"),
            Self::TargetChanged => write!(f, "target identity changed since the scan"),
            Self::TreeChanged => write!(f, "directory contents changed since the scan"),
            Self::LegacySnapshot => write!(f, "a fresh safety scan is required"),
            Self::DegradedDiscovery => write!(f, "launcher discovery was incomplete"),
            Self::MissingSafetyEvidence => write!(f, "scan-time filesystem identity is missing"),
            Self::StaleDatabaseRow => write!(f, "the selected database row is no longer active"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetySnapshot {
    pub trusted_root: PathBuf,
    pub rel_path: PathBuf,
    pub root_identity: FileIdentity,
    pub target_identity: FileIdentity,
    pub tree_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletePlan {
    pub file_id: i64,
    pub scan_id: i64,
    pub action: String,
    pub snapshot: SafetySnapshot,
}

pub fn normalize_relative_path(raw: &str) -> std::result::Result<PathBuf, DeleteBlockReason> {
    if raw.is_empty() || raw.contains('\0') || raw.contains(':') {
        return Err(DeleteBlockReason::InvalidRelativePath(
            "empty, NUL, and alternate-data-stream paths are forbidden".into(),
        ));
    }
    if raw.starts_with(['\\', '/']) || raw.ends_with(['\\', '/']) {
        return Err(DeleteBlockReason::InvalidRelativePath(
            "rooted or empty path components are forbidden".into(),
        ));
    }
    // Win32 silently strips trailing dots and spaces off a path component
    // when it resolves the name (a legacy DOS 8.3 compatibility quirk), so
    // `root\foo   ` and `root\foo` name the same object on disk even though
    // they compare unequal as strings. Rejecting `"."`/`".."` outright but
    // letting `"foo."` or `"foo "` through would leave that stripping as an
    // unaudited path from a snapshot's `rel_path` to a *different* file than
    // the one it was captured against - still inside the trusted root, so
    // `OutsideTrustedRoot` would never catch it, but the wrong object all the
    // same. `ends_with('.')` alone already covers the bare `"."`/`".."` cases.
    if raw
        .split(['\\', '/'])
        .any(|part| part.is_empty() || part.ends_with('.') || part.ends_with(' '))
    {
        return Err(DeleteBlockReason::InvalidRelativePath(
            "only non-empty normal components are allowed".into(),
        ));
    }

    let path = PathBuf::from(raw);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(DeleteBlockReason::InvalidRelativePath(
            "absolute, drive, UNC, current, and parent components are forbidden".into(),
        ));
    }
    Ok(path)
}

/// Classifies a failed `CreateFileW` exactly as the metadata call it replaced
/// would have been classified.
///
/// This distinction is the whole point of the module. `Missing` is a claim
/// that the target provably does not exist, and a caller is entitled to act on
/// it by purging rows; anything that merely means "could not be determined"
/// has to come back as `Metadata` and block. Win32 reports genuine absence as
/// `ERROR_FILE_NOT_FOUND`/`ERROR_PATH_NOT_FOUND` - the two codes `std` turned
/// into `io::ErrorKind::NotFound` - and everything else (access denied, a
/// sharing violation, a not-ready volume, a name Win32 will not parse) as some
/// other code. The code is read off the error rather than matched out of its
/// message, which is localized.
///
/// One case needs more than the code: a path longer than the legacy `MAX_PATH`
/// limit. `std` silently rewrote those into `\\?\` verbatim form, so the old
/// metadata call resolved them; the raw `CreateFileW` here does not, and Win32
/// answers `ERROR_PATH_NOT_FOUND` for a file that may be sitting right there.
/// Believing that would be exactly the failure this function exists to
/// prevent, so on an over-long path absence is never claimed - it blocks, and
/// says why.
#[cfg(windows)]
fn classify_open_failure(path: &Path, error: &windows::core::Error) -> DeleteBlockReason {
    use windows::core::HRESULT;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND};

    // MAX_PATH (260) less the 12 characters Win32 reserves for an 8.3 name in
    // a directory path: the conservative end of the limit, since guessing long
    // only ever blocks a deletion that would otherwise have been allowed.
    const LEGACY_PATH_LIMIT: usize = 248;

    let is_not_found = [ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND]
        .iter()
        .any(|code| error.code() == HRESULT::from_win32(code.0));
    if !is_not_found {
        return DeleteBlockReason::Metadata(error.to_string());
    }
    if path.as_os_str().len() >= LEGACY_PATH_LIMIT {
        return DeleteBlockReason::Metadata(format!(
            "path exceeds the {LEGACY_PATH_LIMIT}-character limit this open can address, \
             so its absence cannot be established: {error}"
        ));
    }
    DeleteBlockReason::Missing
}

#[cfg(windows)]
fn identity(path: &Path) -> std::result::Result<FileIdentity, DeleteBlockReason> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    // One open, not two. `BY_HANDLE_FILE_INFORMATION` already carries
    // everything the old `fs::symlink_metadata` call was consulted for - the
    // reparse-point bit, the directory bit, the size - so reading the metadata
    // first only bought a second trip through the filesystem on a path this
    // scan walks 720 000 times.
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives this call
    // (it is not dropped until after `CreateFileW` returns), so the `PCWSTR`
    // built from its pointer stays valid for the whole call. `None` is passed
    // for both the security-attributes and template-handle parameters, which
    // Win32 accepts as "use defaults" for `CreateFileW`. The flags requested
    // (`FILE_READ_ATTRIBUTES`, share-everything, `OPEN_EXISTING`,
    // backup-semantics plus open-reparse-point) only ever read metadata and
    // never require the caller to hold any other resource alive. On success
    // the returned `HANDLE` is a freshly opened, uniquely owned handle; it is
    // moved into a `File` immediately below, which becomes solely responsible
    // for closing it.
    let handle = unsafe {
        CreateFileW(
            PCWSTR::from_raw(wide.as_ptr()),
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map_err(|error| classify_open_failure(path, &error))?;
    // SAFETY: ownership of the valid handle returned by CreateFileW is moved
    // into File, which closes it exactly once.
    let file = unsafe { fs::File::from_raw_handle(handle.0 as *mut _) };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` owns a live handle for the duration of the call and
    // `info` is a valid, fully initialized out-parameter.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle() as *mut _), &mut info) }
        .map_err(|error| DeleteBlockReason::Metadata(error.to_string()))?;
    // The only reparse-point check, and the stronger of the two the old code
    // did: FILE_FLAG_OPEN_REPARSE_POINT means this handle is on the link
    // itself rather than on whatever it points at, so these attributes
    // describe the same object the metadata pre-check described.
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(DeleteBlockReason::ReparsePoint(path.to_path_buf()));
    }
    // Windows has no third answer here. Every object reachable through a
    // filesystem path is either a directory or it is not, and the reparse
    // case - the one thing that used to make `is_dir()` and `is_file()` both
    // false - has already been rejected above. The old "neither a regular
    // file nor a directory" branch was unreachable on this platform; it is
    // gone rather than left behind as a misleading message.
    let kind = if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        TargetKind::Directory
    } else {
        TargetKind::File
    };
    let file_index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    let last_write_time = (u64::from(info.ftLastWriteTime.dwHighDateTime) << 32)
        | u64::from(info.ftLastWriteTime.dwLowDateTime);
    let file_size = (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow);
    Ok(FileIdentity {
        volume_serial: u64::from(info.dwVolumeSerialNumber),
        file_index,
        kind,
        size: file_size,
        last_write_time,
        attributes: info.dwFileAttributes,
    })
}

/// Reads the current filesystem identity without following a reparse point.
/// This is exposed for startup reconciliation, which is read-only and must
/// classify a pending intent without ever retrying the deletion.
pub fn current_identity(path: &Path) -> std::result::Result<FileIdentity, DeleteBlockReason> {
    identity(path)
}

#[cfg(not(windows))]
fn identity(path: &Path) -> std::result::Result<FileIdentity, DeleteBlockReason> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => DeleteBlockReason::Missing,
        _ => DeleteBlockReason::Metadata(error.to_string()),
    })?;
    if metadata.file_type().is_symlink() {
        return Err(DeleteBlockReason::ReparsePoint(path.to_path_buf()));
    }
    let kind = if metadata.is_dir() {
        TargetKind::Directory
    } else if metadata.is_file() {
        TargetKind::File
    } else {
        return Err(DeleteBlockReason::Metadata(
            "target is neither a regular file nor a directory".into(),
        ));
    };
    Ok(FileIdentity {
        volume_serial: metadata.dev(),
        file_index: metadata.ino(),
        kind,
        size: metadata.size(),
        last_write_time: metadata.mtime() as u64,
        attributes: 0,
    })
}

/// Builds the leaf's identity out of what the `$MFT` already said about it,
/// instead of opening the file to ask again.
///
/// The scan reads every record on the volume in one linear pass; opening each
/// finding afterwards asks the filesystem for facts that pass already
/// collected, at the price of a random seek per finding - 39 us in the best
/// measured layout and 17 798 us in the worst. `examples/mft_identity_check.rs`
/// compared the two sources field by field on 50 000 real files: everything
/// the deletion contract compares agreed, once the attribute bits NTFS keeps
/// to itself were masked off (see `MftRecord::identity`).
///
/// The reparse check is not skipped, only re-sourced: the bit was among the
/// fields that agreed, so a junction is refused here exactly as
/// [`identity`] refuses it.
///
/// Applies to the finding's own leaf only. The trusted root and every
/// directory above it are still opened, and so is the leaf when no record
/// stated it - a walkdir-scanned game, or a record missing a field.
fn stated_identity(
    path: &Path,
    stated: &MftIdentity,
) -> std::result::Result<FileIdentity, DeleteBlockReason> {
    if stated.nt_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(DeleteBlockReason::ReparsePoint(path.to_path_buf()));
    }
    Ok(FileIdentity {
        volume_serial: u64::from(stated.volume_serial),
        file_index: stated.file_index,
        kind: TargetKind::File,
        size: stated.size,
        last_write_time: stated.last_write_time,
        attributes: stated.nt_attributes,
    })
}

fn validate_chain(root: &Path, rel_path: &Path) -> std::result::Result<(), DeleteBlockReason> {
    // A missing *root* is not evidence about the target. An unplugged external
    // drive, a removed drive letter or an unmounted volume all report
    // `NotFound` for the root, and reporting that as `Missing` would let the
    // caller record the target as already deleted and purge its rows - freeing
    // nothing while losing the findings. Only the components *below* a root
    // that is provably there may establish absence.
    identity(root).map_err(|reason| match reason {
        DeleteBlockReason::Missing => DeleteBlockReason::RootMissing,
        other => other,
    })?;
    let mut current = root.to_path_buf();
    for component in rel_path.components() {
        let Component::Normal(component) = component else {
            return Err(DeleteBlockReason::InvalidRelativePath(
                "non-normal component".into(),
            ));
        };
        current.push(component);
        identity(&current)?;
    }
    Ok(())
}

fn fnv1a_update(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}

/// `pub(crate)` for reconciliation, which re-walks a directory that survived a
/// crash to tell a half-finished delete from one that never started (see
/// [`crate::ops::reconcile_pending_operations`]).
pub(crate) fn tree_fingerprint(root: &Path) -> std::result::Result<String, DeleteBlockReason> {
    let mut pending = vec![root.to_path_buf()];
    let mut rows = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .map_err(|error| DeleteBlockReason::Metadata(error.to_string()))?;
        for entry in entries {
            let entry = entry.map_err(|error| DeleteBlockReason::Metadata(error.to_string()))?;
            let path = entry.path();
            let entry_identity = identity(&path)?;
            let relative = path
                .strip_prefix(root)
                .map_err(|_| DeleteBlockReason::Metadata("tree entry escaped its root".into()))?;
            rows.push((
                relative.to_string_lossy().replace('/', "\\").to_lowercase(),
                entry_identity.encode(),
            ));
            if entry_identity.kind == TargetKind::Directory {
                pending.push(path);
            }
        }
    }
    rows.sort_unstable();
    let mut hash = 0xcbf29ce484222325u64;
    for (path, entry_identity) in rows {
        fnv1a_update(&mut hash, path.as_bytes());
        fnv1a_update(&mut hash, &[0]);
        fnv1a_update(&mut hash, entry_identity.as_bytes());
        fnv1a_update(&mut hash, &[0xff]);
    }
    Ok(format!("{hash:016x}"))
}

/// Captures snapshots for many findings that share directories, remembering
/// the directories it has already proven.
///
/// Every finding needs the chain from its trusted root down to it proven free
/// of reparse points, and a scan produces thousands of findings under the same
/// few directories. Proving each chain from scratch reopens the same folders
/// once per finding: measured on a real library that is ~159 us per finding
/// against ~28 us for a single identity read, so five sixths of the cost is
/// re-proving what this scan already proved.
///
/// The cache is scoped to one capture pass and covers *directories only* - the
/// target is always read fresh. It is not a weakening of the safety contract:
/// a snapshot has always been a point-in-time record, and the checks that
/// actually gate a deletion re-walk the whole chain, uncached, at delete time
/// (see [`validate_delete_plan`]).
#[derive(Default)]
pub struct SnapshotCapture {
    proven_directories: HashMap<PathBuf, FileIdentity>,
}

impl SnapshotCapture {
    pub fn new() -> Self {
        Self::default()
    }

    /// `stated_leaf` is what the scan's `$MFT` pass already recorded about the
    /// target, when there was one; see [`stated_identity`] for what it
    /// replaces and what it deliberately does not.
    pub fn capture(
        &mut self,
        trusted_root: &Path,
        raw_rel_path: &str,
        stated_leaf: Option<&MftIdentity>,
    ) -> std::result::Result<SafetySnapshot, DeleteBlockReason> {
        if !trusted_root.is_absolute() {
            return Err(DeleteBlockReason::InvalidRelativePath(
                "trusted root is not absolute".into(),
            ));
        }
        let rel_path = normalize_relative_path(raw_rel_path)?;

        // Same distinction as `validate_chain`: a root that is not there says
        // nothing about the target, so it must not read as absence.
        let root_identity = self
            .proven_directory(trusted_root)
            .map_err(|reason| match reason {
                DeleteBlockReason::Missing => DeleteBlockReason::RootMissing,
                other => other,
            })?;

        let mut current = trusted_root.to_path_buf();
        let mut components = rel_path.components().peekable();
        while let Some(component) = components.next() {
            let Component::Normal(component) = component else {
                return Err(DeleteBlockReason::InvalidRelativePath(
                    "non-normal component".into(),
                ));
            };
            current.push(component);
            // Everything above the target is a directory this scan may see
            // again; the target itself is never cached.
            if components.peek().is_some() {
                self.proven_directory(&current)?;
            }
        }

        let target = current;
        if !target.starts_with(trusted_root) {
            return Err(DeleteBlockReason::OutsideTrustedRoot);
        }
        let target_identity = match stated_leaf {
            // A directory target is read live regardless: its snapshot needs a
            // `tree_fingerprint`, which walks the whole directory and opens
            // every entry in it, so skipping the one open at its root saves
            // nothing and would leave two ways to reach the same value.
            Some(stated) if !stated.is_directory => stated_identity(&target, stated)?,
            _ => identity(&target)?,
        };
        let tree_fingerprint = (target_identity.kind == TargetKind::Directory)
            .then(|| tree_fingerprint(&target))
            .transpose()?;
        Ok(SafetySnapshot {
            trusted_root: trusted_root.to_path_buf(),
            rel_path,
            root_identity,
            target_identity,
            tree_fingerprint,
        })
    }

    fn proven_directory(
        &mut self,
        path: &Path,
    ) -> std::result::Result<FileIdentity, DeleteBlockReason> {
        if let Some(known) = self.proven_directories.get(path) {
            return Ok(known.clone());
        }
        let identity = identity(path)?;
        self.proven_directories
            .insert(path.to_path_buf(), identity.clone());
        Ok(identity)
    }
}

/// One-off snapshot capture. Prefer [`SnapshotCapture`] when capturing many
/// snapshots that share directories.
pub fn capture_safety_snapshot(
    trusted_root: &Path,
    raw_rel_path: &str,
) -> std::result::Result<SafetySnapshot, DeleteBlockReason> {
    if !trusted_root.is_absolute() {
        return Err(DeleteBlockReason::InvalidRelativePath(
            "trusted root is not absolute".into(),
        ));
    }
    let rel_path = normalize_relative_path(raw_rel_path)?;
    validate_chain(trusted_root, &rel_path)?;
    let target = trusted_root.join(&rel_path);
    if !target.starts_with(trusted_root) {
        return Err(DeleteBlockReason::OutsideTrustedRoot);
    }
    let root_identity = identity(trusted_root)?;
    let target_identity = identity(&target)?;
    let tree_fingerprint = (target_identity.kind == TargetKind::Directory)
        .then(|| tree_fingerprint(&target))
        .transpose()?;
    Ok(SafetySnapshot {
        trusted_root: trusted_root.to_path_buf(),
        rel_path,
        root_identity,
        target_identity,
        tree_fingerprint,
    })
}

/// A target that has been proven to be the object the plan named, and is being
/// *held open* so it cannot become a different object before it is removed.
///
/// [`validate_delete_plan`] proves identity through a handle, closes it, and
/// returns a path; whoever removes that path resolves the name a second time.
/// Between those two resolutions the name can be made to point somewhere else.
/// Escaping the trusted root that way is still impossible - the root's own
/// identity is re-checked and the relative path is normalized - but deleting a
/// *different object inside the root* was not, and for a tool whose whole
/// contract is "we only remove what we showed you", that gap is the contract.
///
/// This closes it by never letting go: the handle opened to prove identity is
/// the same handle the removal acts on, and its share mode denies other
/// openers the DELETE access that renaming or replacing the object would need.
/// There is no second name resolution to race.
#[cfg(windows)]
pub struct VerifiedTarget {
    /// Owns the open handle; dropping it closes the handle.
    file: fs::File,
    path: PathBuf,
    kind: TargetKind,
}

/// The non-Windows stand-in. This platform is only ever exercised by tests
/// (the product ships for Windows), and it deliberately does **not** claim to
/// close the window: there is no portable equivalent of holding a name pinned,
/// and pretending otherwise in a type named "verified" would be worse than
/// saying so.
#[cfg(not(windows))]
pub struct VerifiedTarget {
    path: PathBuf,
    kind: TargetKind,
}

impl VerifiedTarget {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn kind(&self) -> TargetKind {
        self.kind
    }

    /// Releases the handle and hands back the path, for removers that can only
    /// work by name.
    ///
    /// The Recycle Bin is the one that needs this: it goes through the shell's
    /// `IFileOperation`, which takes a path and offers no way to pass a handle
    /// in, so recycling cannot be made handle-based at all. Recycling therefore
    /// keeps the original window, knowingly - and it is the *safer* of the two
    /// modes to keep it in, because the object it might race onto is moved to
    /// the bin rather than destroyed. Making the trade-off explicit here, in
    /// the type, rather than leaving it as an unremarked difference between two
    /// call sites.
    pub fn into_path(self) -> PathBuf {
        self.path
    }
}

#[cfg(windows)]
impl VerifiedTarget {
    /// Removes the target through the handle that proved it, with no second
    /// name resolution.
    ///
    /// Files are deleted by setting the disposition on the handle itself.
    /// Directories cannot be: Windows only accepts a delete disposition on an
    /// *empty* directory, so their contents still have to be removed by path.
    /// Holding this handle throughout is what makes that safe - the directory
    /// the paths are relative to cannot be swapped for another while the walk
    /// is running, because this handle's share mode denies the DELETE access a
    /// rename would require.
    pub fn delete(self) -> std::result::Result<(), std::io::Error> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            FileDispositionInfo, FileDispositionInfoEx, SetFileInformationByHandle,
            FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
            FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX,
        };

        if self.kind == TargetKind::Directory {
            // Empty it first; the handle above keeps the directory itself
            // pinned while this runs.
            remove_dir_contents(&self.path)?;
        }

        let handle = HANDLE(self.file.as_raw_handle() as *mut _);

        // POSIX semantics unlinks the name immediately instead of deferring
        // until the last handle closes, so the name is free the moment this
        // returns - which is what the caller's "removed" report claims.
        let mut ex = FILE_DISPOSITION_INFO_EX {
            // The generated newtype has no `BitOr`, so the flags are combined
            // on the underlying bits and wrapped back up.
            Flags: windows::Win32::Storage::FileSystem::FILE_DISPOSITION_INFO_EX_FLAGS(
                FILE_DISPOSITION_FLAG_DELETE.0 | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0,
            ),
        };
        // SAFETY: `handle` is the live handle owned by `self.file`, which is
        // alive for this whole call; `ex` is a fully initialized structure of
        // exactly the size passed, and matches the class requested.
        let posix = unsafe {
            SetFileInformationByHandle(
                handle,
                FileDispositionInfoEx,
                &mut ex as *mut _ as *const std::ffi::c_void,
                std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
            )
        };
        if posix.is_ok() {
            return Ok(());
        }

        // `FileDispositionInfoEx` needs Windows 10 1709 and a filesystem that
        // implements it; older builds and non-NTFS volumes (a FAT32 flash
        // drive holding a portable install) answer with an error rather than
        // ignoring the request. Fall back to the classic class, which every
        // supported Windows has: same effect, except the name survives until
        // this handle closes - which is immediately after, when `self` drops.
        let mut classic = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: as above, for the classic information class.
        unsafe {
            SetFileInformationByHandle(
                handle,
                FileDispositionInfo,
                &mut classic as *mut _ as *const std::ffi::c_void,
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        }
        .map_err(std::io::Error::other)
    }
}

#[cfg(not(windows))]
impl VerifiedTarget {
    pub fn delete(self) -> std::result::Result<(), std::io::Error> {
        if self.kind == TargetKind::Directory {
            fs::remove_dir_all(&self.path)
        } else {
            fs::remove_file(&self.path)
        }
    }
}

/// Empties a directory without removing the directory itself.
///
/// `symlink_metadata`, never `metadata`: a link inside the tree is removed as
/// the link, never followed into whatever it points at. This mirrors what
/// `PermanentDelete` already does for a top-level target.
#[cfg(windows)]
fn remove_dir_contents(dir: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// The path checks shared by [`validate_delete_plan`] and
/// [`open_verified_for_delete`]: normalize, refuse to leave the trusted root,
/// and prove the root is still the root the scan saw.
fn validated_target_path(plan: &DeletePlan) -> std::result::Result<PathBuf, DeleteBlockReason> {
    let raw = plan.snapshot.rel_path.to_string_lossy();
    let rel_path = normalize_relative_path(&raw)?;
    validate_chain(&plan.snapshot.trusted_root, &rel_path)?;
    let target = plan.snapshot.trusted_root.join(&rel_path);
    if !target.starts_with(&plan.snapshot.trusted_root) {
        return Err(DeleteBlockReason::OutsideTrustedRoot);
    }
    if !identity(&plan.snapshot.trusted_root)?.same_object(&plan.snapshot.root_identity) {
        return Err(DeleteBlockReason::RootChanged);
    }
    Ok(target)
}

/// Proves the plan's target is still the object the scan saw, and returns it
/// still open - see [`VerifiedTarget`] for why that matters.
///
/// Every check [`validate_delete_plan`] makes is made here too, with the one
/// difference that identity is read from the handle that is kept, not from a
/// handle that is thrown away.
pub fn open_verified_for_delete(
    plan: &DeletePlan,
) -> std::result::Result<VerifiedTarget, DeleteBlockReason> {
    let target = validated_target_path(plan)?;
    open_verified_at(&target, plan)
}

#[cfg(windows)]
fn open_verified_at(
    target: &Path,
    plan: &DeletePlan,
) -> std::result::Result<VerifiedTarget, DeleteBlockReason> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, DELETE,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let wide: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer alive across the call,
    // so the `PCWSTR` from its pointer stays valid. `None` for the
    // security-attributes and template-handle parameters is Win32's "use
    // defaults". The share mode deliberately omits `FILE_SHARE_DELETE`: that
    // is the whole mechanism - while this handle lives, nobody else can obtain
    // the DELETE access that renaming or deleting this object requires, so the
    // name cannot be re-pointed between the identity check below and the
    // removal. On success the returned `HANDLE` is uniquely owned and is moved
    // into a `File` immediately, which becomes solely responsible for closing
    // it.
    let handle = unsafe {
        CreateFileW(
            PCWSTR::from_raw(wide.as_ptr()),
            DELETE.0 | FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map_err(|error| classify_open_failure(target, &error))?;
    // SAFETY: ownership of the valid handle is moved into File, which closes
    // it exactly once.
    let file = unsafe { fs::File::from_raw_handle(handle.0 as *mut _) };

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` owns a live handle for the duration of the call and
    // `info` is a valid, fully initialized out-parameter.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle() as *mut _), &mut info) }
        .map_err(|error| DeleteBlockReason::Metadata(error.to_string()))?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(DeleteBlockReason::ReparsePoint(target.to_path_buf()));
    }
    let kind = if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        TargetKind::Directory
    } else {
        TargetKind::File
    };
    let current = FileIdentity {
        volume_serial: u64::from(info.dwVolumeSerialNumber),
        file_index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        kind,
        size: (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
        last_write_time: (u64::from(info.ftLastWriteTime.dwHighDateTime) << 32)
            | u64::from(info.ftLastWriteTime.dwLowDateTime),
        attributes: info.dwFileAttributes,
    };
    if current != plan.snapshot.target_identity {
        return Err(DeleteBlockReason::TargetChanged);
    }
    if let Some(expected) = &plan.snapshot.tree_fingerprint {
        if &tree_fingerprint(target)? != expected {
            return Err(DeleteBlockReason::TreeChanged);
        }
    }
    Ok(VerifiedTarget {
        file,
        path: target.to_path_buf(),
        kind,
    })
}

#[cfg(not(windows))]
fn open_verified_at(
    target: &Path,
    plan: &DeletePlan,
) -> std::result::Result<VerifiedTarget, DeleteBlockReason> {
    let current = identity(target)?;
    if current != plan.snapshot.target_identity {
        return Err(DeleteBlockReason::TargetChanged);
    }
    if let Some(expected) = &plan.snapshot.tree_fingerprint {
        if &tree_fingerprint(target)? != expected {
            return Err(DeleteBlockReason::TreeChanged);
        }
    }
    Ok(VerifiedTarget {
        path: target.to_path_buf(),
        kind: current.kind,
    })
}

pub fn validate_delete_plan(plan: &DeletePlan) -> std::result::Result<PathBuf, DeleteBlockReason> {
    let raw = plan.snapshot.rel_path.to_string_lossy();
    let rel_path = normalize_relative_path(&raw)?;
    validate_chain(&plan.snapshot.trusted_root, &rel_path)?;
    let target = plan.snapshot.trusted_root.join(&rel_path);
    if !target.starts_with(&plan.snapshot.trusted_root) {
        return Err(DeleteBlockReason::OutsideTrustedRoot);
    }
    if !identity(&plan.snapshot.trusted_root)?.same_object(&plan.snapshot.root_identity) {
        return Err(DeleteBlockReason::RootChanged);
    }
    let current_target = identity(&target)?;
    if current_target != plan.snapshot.target_identity {
        return Err(DeleteBlockReason::TargetChanged);
    }
    if let Some(expected) = &plan.snapshot.tree_fingerprint {
        if &tree_fingerprint(&target)? != expected {
            return Err(DeleteBlockReason::TreeChanged);
        }
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_for(root: &Path, rel_path: &str) -> DeletePlan {
        DeletePlan {
            file_id: 1,
            scan_id: 1,
            action: "delete".to_string(),
            snapshot: capture_safety_snapshot(root, rel_path).expect("capture snapshot"),
        }
    }

    /// The ordinary case, exercised for real rather than through a stub
    /// remover: a file is removed through the handle that proved it.
    #[test]
    fn a_verified_file_is_deleted_through_its_own_handle() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("library");
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("redist.exe");
        std::fs::write(&target, b"MZ").unwrap();

        let plan = plan_for(&root, "redist.exe");
        open_verified_for_delete(&plan)
            .expect("the unchanged file must verify")
            .delete()
            .expect("delete through the handle must succeed");

        assert!(!target.exists(), "the file should be gone");
    }

    /// Directories cannot be disposed of while non-empty, so their contents
    /// still go by path - the handle is what keeps the directory those paths
    /// are relative to from being swapped mid-walk.
    #[test]
    fn a_verified_directory_is_emptied_and_removed() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("library");
        let target = root.join("bonus");
        std::fs::create_dir_all(target.join("nested")).unwrap();
        std::fs::write(target.join("art.pdf"), b"pdf").unwrap();
        std::fs::write(target.join("nested").join("track.flac"), b"flac").unwrap();

        let plan = plan_for(&root, "bonus");
        open_verified_for_delete(&plan)
            .expect("the unchanged directory must verify")
            .delete()
            .expect("delete through the handle must succeed");

        assert!(!target.exists(), "the directory should be gone");
        assert!(root.exists(), "the trusted root must be left alone");
    }

    /// The failure this whole mechanism exists for, in its observable form: if
    /// the name now points at a different object than the scan recorded, the
    /// delete is refused rather than carried out on the impostor.
    #[test]
    fn a_target_swapped_for_another_object_is_refused() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("library");
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("redist.exe");
        std::fs::write(&target, b"original").unwrap();

        let plan = plan_for(&root, "redist.exe");

        // Someone replaces the file between the scan and the delete. The name
        // is the same; the object behind it is not.
        std::fs::remove_file(&target).unwrap();
        std::fs::write(&target, b"substituted").unwrap();

        match open_verified_for_delete(&plan) {
            Err(DeleteBlockReason::TargetChanged) => {}
            Err(other) => panic!("a substituted target must be refused, got {other:?}"),
            Ok(_) => panic!("a substituted target must not verify"),
        }
        assert!(
            target.exists(),
            "the substituted object must be left untouched"
        );
    }

    /// The mechanism itself, asserted directly rather than inferred: while the
    /// verified handle is held, the object cannot be renamed out from under
    /// it. That is what removes the window - not the identity check, which
    /// only proves what was true a moment ago.
    #[cfg(windows)]
    #[test]
    fn a_held_target_cannot_be_renamed_away_before_it_is_deleted() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("library");
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("redist.exe");
        std::fs::write(&target, b"MZ").unwrap();

        let plan = plan_for(&root, "redist.exe");
        let held = open_verified_for_delete(&plan).expect("verify");

        let rename = std::fs::rename(&target, root.join("moved.exe"));
        assert!(
            rename.is_err(),
            "the held handle must deny the DELETE access a rename needs, \
             otherwise the name could still be re-pointed before the removal"
        );

        held.delete().expect("delete through the handle");
        assert!(!target.exists());
    }

    /// The three statuses the scan actually writes today. Both callers agreed
    /// on these already; the test exists so that agreement is pinned rather
    /// than coincidental.
    #[test]
    fn discovery_policy_allows_exactly_complete_and_game_bound_heuristic() {
        assert_eq!(discovery_block_reason(true, Some("complete")), None);
        assert_eq!(discovery_block_reason(false, Some("complete")), None);
        assert_eq!(discovery_block_reason(true, Some("heuristic")), None);

        // Heuristic evidence can show a folder holds a game; it can never show
        // a launcher has stopped managing one, so it cannot authorize calling
        // a folder orphaned.
        assert!(discovery_block_reason(false, Some("heuristic")).is_some());
        assert!(discovery_block_reason(true, Some("degraded")).is_some());
        assert!(discovery_block_reason(false, Some("degraded")).is_some());
        assert!(discovery_block_reason(true, None).is_some());
        assert!(discovery_block_reason(false, None).is_some());
    }

    /// The case the three copies of this policy disagreed about, and the
    /// reason they were merged. A status this build has never heard of used to
    /// be *allowed* by the load query's SQL and by the scan's persistence path
    /// (both only looked for `NULL` and `degraded`) while the delete preflight
    /// refused it. Unreachable while only three statuses are ever written -
    /// which is a fact about today's writer, not about the policy. Whatever
    /// gets added next must fail closed on all three paths by default.
    #[test]
    fn an_unrecognised_discovery_status_blocks_rather_than_being_waved_through() {
        for status in ["failed", "partial", "", "COMPLETE"] {
            assert!(
                discovery_block_reason(true, Some(status)).is_some(),
                "game-bound finding must be blocked by unknown status {status:?}"
            );
            assert!(
                discovery_block_reason(false, Some(status)).is_some(),
                "orphan candidate must be blocked by unknown status {status:?}"
            );
        }
    }

    #[test]
    fn rejects_unsafe_relative_paths() {
        for path in [
            "",
            "..\\outside",
            ".\\file",
            "C:\\absolute",
            "\\\\server\\share",
            "file:stream",
            "a\\\\b",
            "a\\",
            // Win32 strips trailing dots and spaces when resolving a
            // component, so these name the same object as "dir\\foo" despite
            // comparing unequal as strings - accepting them would let a
            // snapshot's rel_path silently drift onto a different file.
            "dir\\foo.",
            "dir\\foo..",
            "dir\\foo ",
            "dir\\foo.  ",
        ] {
            assert!(normalize_relative_path(path).is_err(), "accepted {path:?}");
        }
        assert_eq!(
            normalize_relative_path("dir\\file.bin").unwrap(),
            PathBuf::from("dir\\file.bin")
        );
    }

    /// `attributes` is still stored as a narrower `u32` after decode, so it
    /// keeps its overflow check. `volume_serial` used to share this same
    /// ceiling but no longer does - see the next test.
    #[test]
    fn identity_decode_rejects_narrowing_overflow_in_attributes() {
        let too_large = u64::from(u32::MAX) + 1;
        assert!(FileIdentity::decode(&format!("1:1:file:1:1:{too_large}")).is_err());
    }

    /// `volume_serial` was widened to `u64` so the non-Windows `identity()`
    /// branch can store a full `st_dev` without truncating it (see the
    /// field's doc comment on [`FileIdentity`]); a value beyond `u32::MAX`
    /// must now round-trip through encode/decode instead of erroring.
    #[test]
    fn identity_round_trips_a_volume_serial_beyond_u32() {
        let beyond_u32 = u64::from(u32::MAX) + 12345;
        let identity = FileIdentity {
            volume_serial: beyond_u32,
            file_index: 1,
            kind: TargetKind::File,
            size: 1,
            last_write_time: 1,
            attributes: 0,
        };
        let decoded = FileIdentity::decode(&identity.encode()).unwrap();
        assert_eq!(decoded.volume_serial, beyond_u32);
    }

    #[test]
    fn changed_target_identity_is_blocked() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.bin");
        fs::write(&target, b"first").unwrap();
        let snapshot = capture_safety_snapshot(temp.path(), "target.bin").unwrap();
        fs::remove_file(&target).unwrap();
        fs::write(&target, b"replacement").unwrap();
        let plan = DeletePlan {
            file_id: 1,
            scan_id: 1,
            action: "delete".into(),
            snapshot,
        };
        let result = validate_delete_plan(&plan);
        assert!(
            matches!(result, Err(DeleteBlockReason::TargetChanged)),
            "replacement was not classified as a target identity change: {result:?}"
        );
    }

    #[test]
    fn changed_directory_tree_is_blocked() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("orphan");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("old.bin"), b"old").unwrap();
        let snapshot = capture_safety_snapshot(temp.path(), "orphan").unwrap();
        fs::write(target.join("new.bin"), b"new").unwrap();
        let plan = DeletePlan {
            file_id: 1,
            scan_id: 1,
            action: "delete".into(),
            snapshot,
        };
        assert!(matches!(
            validate_delete_plan(&plan),
            Err(DeleteBlockReason::TreeChanged) | Err(DeleteBlockReason::TargetChanged)
        ));
    }

    /// A root that has gone away - an unplugged drive, a removed letter, an
    /// unmounted volume - says nothing about the target. Reporting it as
    /// `Missing` would let the caller record the delete as already done and
    /// purge the rows, freeing nothing and losing the findings.
    #[test]
    fn an_unreachable_trusted_root_blocks_instead_of_reporting_absence() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("library");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("target.bin"), b"first").unwrap();
        let snapshot = capture_safety_snapshot(&root, "target.bin").unwrap();
        // Stands in for the volume disappearing: the whole root is gone, not
        // just the target inside it.
        fs::remove_dir_all(&root).unwrap();

        let plan = DeletePlan {
            file_id: 1,
            scan_id: 1,
            action: "delete".into(),
            snapshot,
        };
        let result = validate_delete_plan(&plan);
        assert!(
            matches!(result, Err(DeleteBlockReason::RootMissing)),
            "an unreachable root must block, not read as a deleted target: {result:?}"
        );
    }

    /// The counterpart: the root is still there and only the target is gone,
    /// which really is proof of absence.
    #[test]
    fn a_missing_target_under_a_live_root_is_reported_as_missing() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.bin");
        fs::write(&target, b"first").unwrap();
        let snapshot = capture_safety_snapshot(temp.path(), "target.bin").unwrap();
        fs::remove_file(&target).unwrap();

        let plan = DeletePlan {
            file_id: 1,
            scan_id: 1,
            action: "delete".into(),
            snapshot,
        };
        let result = validate_delete_plan(&plan);
        assert!(
            matches!(result, Err(DeleteBlockReason::Missing)),
            "a target gone from a live root is genuinely absent: {result:?}"
        );
    }

    #[test]
    fn changed_trusted_root_identity_is_blocked() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("target.bin"), b"first").unwrap();
        let snapshot = capture_safety_snapshot(&root, "target.bin").unwrap();
        fs::rename(&root, parent.path().join("old-root")).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("target.bin"), b"first").unwrap();
        let plan = DeletePlan {
            file_id: 1,
            scan_id: 1,
            action: "delete".into(),
            snapshot,
        };
        assert!(matches!(
            validate_delete_plan(&plan),
            Err(DeleteBlockReason::RootChanged)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn a_leaf_symlink_is_never_captured_as_deletable() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real.bin");
        let link = temp.path().join("link.bin");
        fs::write(&real, b"data").unwrap();
        if std::os::windows::fs::symlink_file(&real, &link).is_err() {
            eprintln!("skipping: this Windows account cannot create symlinks");
            return;
        }
        assert!(matches!(
            capture_safety_snapshot(temp.path(), "link.bin"),
            Err(DeleteBlockReason::ReparsePoint(_))
        ));
    }

    #[cfg(windows)]
    fn create_junction(link: &Path, target: &Path) {
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .expect("start mklink");
        assert!(
            output.status.success(),
            "mklink /J failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// The cache must be an optimization and nothing else: same inputs, same
    /// snapshot, including for repeated captures under one shared chain.
    #[cfg(windows)]
    #[test]
    fn snapshot_capture_matches_the_uncached_capture() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("Data").join("Localization").join("es");
        fs::create_dir_all(&nested).unwrap();
        let relatives = [
            r"Data\Localization\es\voice.pak",
            r"Data\Localization\es\text.pak",
            r"Data\Localization\es",
        ];
        fs::write(nested.join("voice.pak"), b"one").unwrap();
        fs::write(nested.join("text.pak"), b"two").unwrap();

        let mut capture = SnapshotCapture::new();
        for relative in relatives {
            let cached = capture.capture(temp.path(), relative, None).unwrap();
            let uncached = capture_safety_snapshot(temp.path(), relative).unwrap();
            assert_eq!(cached.trusted_root, uncached.trusted_root);
            assert_eq!(cached.rel_path, uncached.rel_path);
            assert_eq!(cached.root_identity, uncached.root_identity);
            assert_eq!(cached.target_identity, uncached.target_identity);
            assert_eq!(cached.tree_fingerprint, uncached.tree_fingerprint);
        }
    }

    /// Caching directories must not let a reparse point in the chain slip
    /// past - the cache only ever stores chains that were already proven
    /// clean, so a junction has to fail on the first capture and stay failing.
    #[test]
    fn snapshot_capture_still_blocks_a_junction_in_the_chain() {
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        let root = temp.path().join("root");
        let junction = root.join("linked");
        fs::create_dir(&outside).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(outside.join("target.bin"), b"data").unwrap();
        fs::write(outside.join("second.bin"), b"data").unwrap();
        create_junction(&junction, &outside);

        let mut capture = SnapshotCapture::new();
        for relative in [r"linked\target.bin", r"linked\second.bin"] {
            let result = capture.capture(&root, relative, None);
            assert!(
                matches!(result, Err(DeleteBlockReason::ReparsePoint(_))),
                "a cached chain must not launder a junction: {result:?}"
            );
        }

        fs::remove_dir(&junction).unwrap();
    }

    /// A record that states the leaf replaces the open on the leaf, and on
    /// nothing else: the chain above it is still walked and still proven.
    #[cfg(windows)]
    #[test]
    fn a_stated_leaf_is_taken_from_the_record_and_the_chain_is_still_walked() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("Data");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("voice.pak"), b"one").unwrap();

        // Deliberately unlike anything on disk: if the capture opened the file
        // it would report the real values and this would not match.
        let stated = MftIdentity {
            volume_serial: 0xfeed_face,
            file_index: 0x0007_0000_0000_002a,
            is_directory: false,
            size: 4096,
            last_write_time: 132_000_000_000_000_000,
            nt_attributes: 0x20,
        };
        let mut capture = SnapshotCapture::new();
        let snapshot = capture
            .capture(temp.path(), r"Data\voice.pak", Some(&stated))
            .unwrap();

        assert_eq!(snapshot.target_identity.volume_serial, 0xfeed_face);
        assert_eq!(snapshot.target_identity.file_index, 0x0007_0000_0000_002a);
        assert_eq!(snapshot.target_identity.size, 4096);
        assert_eq!(snapshot.target_identity.kind, TargetKind::File);
        assert_eq!(snapshot.target_identity.attributes, 0x20);

        // The root came from the filesystem, not from the record.
        let live_root = identity(temp.path()).unwrap();
        assert_eq!(snapshot.root_identity, live_root);
    }

    /// The reparse check is re-sourced, not skipped: a record that says the
    /// leaf is a junction blocks exactly as the live open would.
    #[test]
    fn a_stated_leaf_that_is_a_reparse_point_still_blocks() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("linked.bin"), b"data").unwrap();

        let stated = MftIdentity {
            volume_serial: 1,
            file_index: 2,
            is_directory: false,
            size: 4,
            last_write_time: 3,
            nt_attributes: FILE_ATTRIBUTE_REPARSE_POINT,
        };
        let mut capture = SnapshotCapture::new();
        let result = capture.capture(temp.path(), "linked.bin", Some(&stated));
        assert!(
            matches!(result, Err(DeleteBlockReason::ReparsePoint(_))),
            "unexpected: {result:?}"
        );
    }

    /// A directory target is read live even when a record states it, because
    /// its `tree_fingerprint` opens the whole subtree anyway.
    #[cfg(windows)]
    #[test]
    fn a_stated_directory_is_still_read_live() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("es");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("voice.pak"), b"one").unwrap();

        let stated = MftIdentity {
            volume_serial: 0xfeed_face,
            file_index: 0x2a,
            is_directory: true,
            size: 0,
            last_write_time: 1,
            nt_attributes: 0x10,
        };
        let mut capture = SnapshotCapture::new();
        let snapshot = capture.capture(temp.path(), "es", Some(&stated)).unwrap();

        assert_eq!(snapshot.target_identity, identity(&nested).unwrap());
        assert!(snapshot.tree_fingerprint.is_some());
    }

    #[test]
    fn snapshot_capture_reports_an_unreachable_root_as_root_missing() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("gone");

        let mut capture = SnapshotCapture::new();
        let result = capture.capture(&root, "target.bin", None);
        assert!(
            matches!(result, Err(DeleteBlockReason::RootMissing)),
            "unexpected: {result:?}"
        );
    }

    #[test]
    fn a_junction_in_the_target_chain_is_never_deletable() {
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        let root = temp.path().join("root");
        let junction = root.join("linked");
        fs::create_dir(&outside).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(outside.join("target.bin"), b"data").unwrap();
        create_junction(&junction, &outside);

        let result = capture_safety_snapshot(&root, r"linked\target.bin");
        assert!(matches!(result, Err(DeleteBlockReason::ReparsePoint(_))));

        fs::remove_dir(&junction).unwrap();
    }

    /// `Missing` is a claim a caller may act on by purging rows, so a target
    /// that is demonstrably *there* and merely unopenable must never produce
    /// it. A path past the legacy `MAX_PATH` limit is the reachable form of
    /// that on Windows: `std` reaches such a file by rewriting the path into
    /// `\\?\` form, while a raw `CreateFileW` answers `ERROR_PATH_NOT_FOUND`
    /// for a file sitting right there.
    #[cfg(windows)]
    #[test]
    fn a_target_that_exists_but_cannot_be_opened_is_never_reported_as_missing() {
        let temp = tempfile::tempdir().unwrap();
        let mut deep = temp.path().to_path_buf();
        while deep.as_os_str().len() < 300 {
            deep.push("a-directory-with-a-fairly-long-name");
        }
        fs::create_dir_all(&deep).unwrap();
        let target = deep.join("target.bin");
        fs::write(&target, b"data").unwrap();
        // The premise of the test: the file really is there.
        assert!(
            fs::symlink_metadata(&target).is_ok(),
            "the long-path target was not created, so nothing is being tested"
        );

        let result = identity(&target);
        match result {
            Err(DeleteBlockReason::Metadata(_)) => {}
            Ok(_) => {
                eprintln!("skipping: this system reaches over-long paths without a verbatim prefix")
            }
            other => panic!("an unopenable but present target must block, not vanish: {other:?}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_root_replaced_by_a_junction_is_blocked() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let old_root = temp.path().join("old-root");
        let replacement = temp.path().join("replacement");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&replacement).unwrap();
        fs::write(root.join("target.bin"), b"original").unwrap();
        fs::write(replacement.join("target.bin"), b"replacement").unwrap();
        let snapshot = capture_safety_snapshot(&root, "target.bin").unwrap();
        fs::rename(&root, &old_root).unwrap();
        create_junction(&root, &replacement);
        let plan = DeletePlan {
            file_id: 1,
            scan_id: 1,
            action: "delete".into(),
            snapshot,
        };

        assert!(matches!(
            validate_delete_plan(&plan),
            Err(DeleteBlockReason::ReparsePoint(_))
        ));

        fs::remove_dir(&root).unwrap();
    }
}
