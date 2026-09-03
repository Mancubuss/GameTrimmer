//! Detection of orphaned launcher residue (orphan-residue safety): folders that sit inside a
//! launcher's managed install area but have no live game behind them, plus the
//! launcher's fixed download/cache scratch folders.
//!
//! This is the cheapest new finding category with the largest typical payoff:
//! the most common real complaint is not "a game is too big" but "I uninstalled
//! a game and the space never came back" - a folder left in `steamapps/common`
//! after its `appmanifest_*.acf` is gone, an aborted download in
//! `steamapps/downloading`, and so on. Ordinary scanning starts *from* the games
//! a provider reports, so it never reaches these by construction.
//!
//! # Scope of this module
//!
//! This is the **detection core**: a provider-agnostic diff ([`unmanaged_subdirs`])
//! plus the per-vendor "where do games live / what is pure scratch" spec
//! ([`OrphanScanSpec`], [`steam_spec`]) and the IO that ties them together
//! ([`find_orphans`]). Turning the returned [`OrphanCandidate`]s into findings
//! rows, a separate UI tree branch, and an autoselect-off category is the next
//! increment - deliberately kept out of here so the detection logic can be
//! verified in isolation against the acceptance criteria of "orphan-residue safety - orphaned
//! installs and launcher residue" first. (That increment has since landed; the
//! ticket lives on the Kanban board, not in a file in this repo.)
//!
//! # Safety stance
//!
//! False "orphan" is the failure that matters: a game installed *past* the
//! launcher (portable, repack, a manual copy dropped into `common`) must never
//! be flagged with any confidence that would let it be auto-selected. Two things
//! guard that: only subfolders of a vendor's *own* managed container are ever
//! considered (a game installed outside it is never even looked at), and the
//! whole category is designed to carry low confidence and stay out of the
//! default selection (enforced by the pipeline that consumes these candidates,
//! not here).
//!
//! That precondition - the launcher created the container and only its own
//! content lives there - was checked against a real Steam installation before
//! GT-23 extended detection to `steamapps/workshop` and `depotcache`. Both
//! hold: `workshop/content/<appid>/<publishedfileid>` is written by the Steam
//! client alone (never a user-chosen or shared path), and `depotcache/` on
//! both real library roots inspected held nothing but `.manifest` cache
//! files - no subfolders, no foreign content. `depotcache` turned out **not**
//! to live under `steamapps` as GT-23 first assumed (it is a sibling of
//! `steamapps`, directly under the library root - see
//! [`steam_depotcache_dir`]), which changes *where* the container is, not
//! whether the precondition holds.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Why a path is considered orphaned residue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrphanKind {
    /// A subfolder of a launcher's managed install container (e.g.
    /// `steamapps/common/<name>`) with no matching manifest - the classic
    /// "uninstalled the game, the folder stayed" leftover.
    UnmanagedFolder,
    /// A launcher's fixed download/cache scratch folder (aborted or partial
    /// downloads, depot cache, ...): residue regardless of what is installed.
    ServiceFolder,
    /// A single leftover *file* inside a launcher's own cache/service area,
    /// proven unreferenced by explicit per-item evidence rather than by the
    /// area merely existing - contrast [`OrphanKind::ServiceFolder`], which
    /// flags a whole scratch folder on existence alone with no per-item
    /// check. Steam's `depotcache/<depotid>_<manifestid>.manifest` is the
    /// first user (GT-23): a manifest no installed app's `InstalledDepots`
    /// still names, across every discovered Steam library.
    UnreferencedFile,
    /// A Steam Workshop item folder under
    /// `steamapps/workshop/content/<appid>/<publishedfileid>` that appid's own
    /// `appworkshop_<appid>.acf` no longer lists as installed - a mod left on
    /// disk after unsubscribing, or after the game itself went away.
    ///
    /// Evidence-wise this is an [`UnmanagedFolder`](OrphanKind::UnmanagedFolder)
    /// with a narrower proof (one appid's own state file rather than a whole
    /// library's manifests), and it was that variant until GT-187. It has its
    /// own kind because it is the only orphan that belongs to a *different*
    /// display category: Workshop mods are residue the user recognises by name
    /// and may well want back, so they get their own branch and their own
    /// "have a look" badge instead of being poured into the generic orphan
    /// pile. See `worker::classify::display_category`.
    WorkshopItem,
}

/// One detected piece of orphaned residue on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanCandidate {
    /// Absolute path to the orphaned directory.
    pub path: PathBuf,
    pub kind: OrphanKind,
}

/// Per-vendor description of where to look for orphans in one discovered
/// library. Data-only so every provider can supply its own without the diff
/// logic ([`unmanaged_subdirs`]) knowing anything vendor-specific.
#[derive(Debug, Clone)]
pub struct OrphanScanSpec {
    /// The directory holding one subfolder per installed game (e.g.
    /// `<lib>/steamapps/common`). Every subfolder here whose path is not a
    /// known managed install dir is an [`OrphanKind::UnmanagedFolder`].
    pub container: PathBuf,
    /// Fixed scratch folders that are residue no matter what is installed
    /// (download staging, etc.), as absolute paths. A path that does not exist
    /// on disk is simply skipped by [`find_orphans`].
    pub service_folders: Vec<PathBuf>,
    /// Optional structural proof-of-ownership. When non-empty, a container
    /// subfolder is only reported as an [`OrphanKind::UnmanagedFolder`] orphan
    /// if it *contains* at least one of these marker paths (each relative to
    /// the subfolder, checked with [`find_orphans`]).
    ///
    /// This is what lets a launcher whose install root can be a *shared*
    /// user-chosen folder (itch, whose location the user picks and may point at
    /// a folder also holding Steam/manual games) still honor orphan-residue safety's invariant:
    /// a foreign or manually-installed game in that same folder never carries
    /// this launcher's residue marker (e.g. itch's `.itch` receipt directory),
    /// so it is never flagged. Empty means the container is *structurally*
    /// launcher-exclusive - a fixed subfolder the launcher creates and fills
    /// alone (Steam's `steamapps/common`, Xbox's `XboxGames`) - and no marker
    /// is required.
    pub ownership_markers: Vec<PathBuf>,
}

/// Lower-cases a path to a comparison key. Windows paths are case-insensitive,
/// so orphan matching must be too - otherwise `steamapps\common\Portal 2` and
/// a manifest's `steamapps\common\portal 2` would look like different folders
/// and a live game would be misreported as orphaned.
fn key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// The core, IO-free diff: of `container_subdirs` (every immediate subfolder
/// found on disk under a vendor's managed container), returns those whose path
/// is not among `managed_install_dirs` (the install directories the vendor's
/// manifests actually point at).
///
/// Both sides are compared case-insensitively (see [`key`]). Order of the
/// input is preserved in the output so callers get a stable, disk-order result.
/// This is the single place the "on disk but not in any manifest" rule lives,
/// kept pure so it can be exhaustively unit-tested without a filesystem.
pub fn unmanaged_subdirs(
    container_subdirs: &[PathBuf],
    managed_install_dirs: &HashSet<String>,
) -> Vec<PathBuf> {
    container_subdirs
        .iter()
        .filter(|subdir| !managed_install_dirs.contains(&key(subdir)))
        .cloned()
        .collect()
}

/// Lists the immediate subdirectories of `dir` (non-recursive), skipping files.
/// A missing container is an empty result; every other enumeration or metadata
/// error is returned because an incomplete directory listing cannot prove that
/// any child is unmanaged.
///
/// Symlinks/junctions are intentionally *not* followed into: a junction inside
/// `common` is reported as its own entry (a candidate for removal as the link),
/// never traversed into whatever it targets - mirroring the deletion layer's
/// `symlink_metadata` stance in `ops`.
pub fn list_subdirs(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut subdirs = Vec::new();
    for entry in entries {
        let entry = entry?;
        // `file_type()` here does not follow the link, so a junction is
        // classified by what it is, not by whatever it targets.
        if entry.file_type()?.is_dir() {
            subdirs.push(entry.path());
        }
    }
    Ok(subdirs)
}

/// Lists the immediate *files* (not subdirectories) of `dir` - the file-typed
/// counterpart to [`list_subdirs`], needed for `depotcache/`'s flat manifest
/// files (Steam never nests it, and neither did the real installs this was
/// checked against). Same contract: a missing directory is an empty result,
/// any other enumeration or metadata error is returned rather than silently
/// dropped.
pub fn list_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            files.push(entry.path());
        }
    }
    Ok(files)
}

/// True when `candidate` may be reported as an unmanaged-folder orphan under
/// `markers`: either no marker is required (`markers` empty - a structurally
/// launcher-exclusive container), or the candidate contains at least one marker
/// path, proving it is this launcher's residue. `symlink_metadata` (not
/// `exists`) so a marker that is itself a dangling junction still counts as
/// present - it is still proof the folder belonged to this launcher.
fn has_ownership_marker(candidate: &Path, markers: &[PathBuf]) -> std::io::Result<bool> {
    if markers.is_empty() {
        return Ok(true);
    }
    for marker in markers {
        match std::fs::symlink_metadata(candidate.join(marker)) {
            Ok(_) => return Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    Ok(false)
}

/// Ties [`list_subdirs`] and [`unmanaged_subdirs`] together for one library:
/// every unmanaged subfolder of `spec.container` becomes an
/// [`OrphanKind::UnmanagedFolder`] candidate, and every existing
/// `spec.service_folders` entry an [`OrphanKind::ServiceFolder`] candidate.
///
/// `managed_install_dirs` is the set of install directories the vendor's
/// manifests point at (case-insensitive keys via [`key`]); typically built from
/// the provider's already-discovered `GameInstall::install_dir`s. Anything in
/// the container not in that set is residue.
///
/// When `spec.ownership_markers` is non-empty, an unmanaged subfolder is only
/// kept if it carries one of those markers (see [`has_ownership_marker`]) - the
/// structural guard that keeps a shared-root launcher (itch) from ever flagging
/// a foreign or manual game that merely happens to live in the same folder.
pub fn find_orphans(
    spec: &OrphanScanSpec,
    managed_install_dirs: &HashSet<String>,
) -> std::io::Result<Vec<OrphanCandidate>> {
    let mut candidates = Vec::new();

    let subdirs = list_subdirs(&spec.container)?;
    for path in unmanaged_subdirs(&subdirs, managed_install_dirs) {
        if !has_ownership_marker(&path, &spec.ownership_markers)? {
            continue;
        }
        candidates.push(OrphanCandidate {
            path,
            kind: OrphanKind::UnmanagedFolder,
        });
    }

    for service in &spec.service_folders {
        // Only report a scratch folder that actually exists; `symlink_metadata`
        // (not `exists`) so a dangling junction still counts as present - it is
        // itself a removable entry.
        match std::fs::symlink_metadata(service) {
            Ok(_) => candidates.push(OrphanCandidate {
                path: service.clone(),
                kind: OrphanKind::ServiceFolder,
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }

    Ok(candidates)
}

/// Builds the case-insensitive lookup set [`find_orphans`] expects from an
/// iterator of managed install directories (a provider's discovered
/// `GameInstall::install_dir`s).
pub fn managed_dir_set<'a, I>(install_dirs: I) -> HashSet<String>
where
    I: IntoIterator<Item = &'a Path>,
{
    install_dirs.into_iter().map(key).collect()
}

/// The Steam orphan-scan spec for one discovered library root (e.g.
/// `F:\SteamLibrary`).
///
/// Container is `<root>/steamapps/common` - where every Steam game's
/// `installdir` lives (see `providers::steam::parse_appmanifest`). Service
/// folders are limited to `steamapps/downloading` (aborted/partial depot
/// downloads): the single scratch location safe to sweep on existence alone.
///
/// Workshop content (`steamapps/workshop`) and the depot manifest cache
/// (`depotcache`) are deliberately **not** folded into this spec (GT-23):
/// both need a per-item "is anything still referencing this?" check against
/// evidence a single [`OrphanScanSpec`] can't express - a separate small state
/// file per Workshop appid, and cross-*library* evidence for depotcache. See
/// [`workshop_item_orphans`] and [`depotcache_orphans`], and their call sites
/// in `orphan_analysis::collect_steam_service_area_orphans`.
pub fn steam_spec(library_root: &Path) -> OrphanScanSpec {
    let steamapps = library_root.join("steamapps");
    OrphanScanSpec {
        container: steamapps.join("common"),
        service_folders: vec![steamapps.join("downloading")],
        // `steamapps/common` is a fixed folder Steam creates and fills alone:
        // structurally exclusive, so no per-folder ownership marker is needed.
        ownership_markers: Vec::new(),
    }
}

/// GT-23's Steam Workshop content root for one discovered library: every
/// installed item lives at
/// `<library_root>/steamapps/workshop/content/<appid>/<publishedfileid>`.
/// Confirmed against a real Steam installation on this machine.
pub fn steam_workshop_content_dir(library_root: &Path) -> PathBuf {
    library_root
        .join("steamapps")
        .join("workshop")
        .join("content")
}

/// Where Steam records one appid's Workshop live-subscription state:
/// `<library_root>/steamapps/workshop/appworkshop_<appid>.acf`. See
/// `providers::steam::parse_workshop_installed_items` for what it contains
/// and how it was verified against real data.
pub fn steam_workshop_state_path(library_root: &Path, appid: &str) -> PathBuf {
    library_root
        .join("steamapps")
        .join("workshop")
        .join(format!("appworkshop_{appid}.acf"))
}

/// GT-23's Steam depot manifest cache for one discovered library root.
///
/// **Not** nested under `steamapps` - this is the one place the card's
/// starting assumption (`steamapps/depotcache`) did not match a real
/// installation. On this machine `depotcache` sits directly under every
/// library root, a sibling of `steamapps`: `F:\SteamLibrary\depotcache` and
/// `D:\...\Steam\depotcache` (the primary install) both exist there, not
/// inside `steamapps`. Steam creates the folder itself at every library root
/// and both real instances held only `.manifest` cache files - no
/// subfolders, nothing foreign - so it is structurally exclusive exactly like
/// `steamapps/common`, just at a different path.
pub fn steam_depotcache_dir(library_root: &Path) -> PathBuf {
    library_root.join("depotcache")
}

/// Every `workshop/content/<appid>/<publishedfileid>` subfolder of
/// `appid_dir` whose id is not in `live_items` - Steam's own
/// `WorkshopItemsInstalled` record for that appid, the only evidence that
/// proves a Workshop item is still a live subscription (see
/// `providers::steam::parse_workshop_installed_items`). Reuses
/// [`unmanaged_subdirs`]'s case-insensitive, disk-order-preserving diff by
/// building the "managed" set from the live ids' full paths under
/// `appid_dir`.
///
/// The caller decides what `live_items` is when evidence is missing or
/// unreadable; this function has no fail-closed logic of its own; it is pure
/// diff-against-what-you-hand-it, same as [`find_orphans`]'s managed set.
pub fn workshop_item_orphans(
    appid_dir: &Path,
    live_items: &HashSet<String>,
) -> std::io::Result<Vec<OrphanCandidate>> {
    let live_paths: Vec<PathBuf> = live_items.iter().map(|id| appid_dir.join(id)).collect();
    let managed = managed_dir_set(live_paths.iter().map(PathBuf::as_path));
    let subdirs = list_subdirs(appid_dir)?;
    Ok(unmanaged_subdirs(&subdirs, &managed)
        .into_iter()
        .map(|path| OrphanCandidate {
            path,
            kind: OrphanKind::WorkshopItem,
        })
        .collect())
}

/// Steam's on-disk depot manifest filename shape:
/// `<depotid>_<manifestid>.manifest`, both purely numeric on every real
/// manifest inspected on this machine. A file that doesn't match this shape
/// is left alone entirely, never flagged either way - this check has no
/// evidence about what it is, so like every other unproven case in this
/// module it stays untouched.
fn depot_manifest_filename_is_well_formed(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".manifest") else {
        return false;
    };
    let Some((depot_id, manifest_id)) = stem.split_once('_') else {
        return false;
    };
    !depot_id.is_empty()
        && !manifest_id.is_empty()
        && depot_id.bytes().all(|b| b.is_ascii_digit())
        && manifest_id.bytes().all(|b| b.is_ascii_digit())
}

/// Every `depotcache/<depotid>_<manifestid>.manifest` in `depotcache_dir`
/// whose filename (compared case-insensitively) is not in `needed_files`.
///
/// `needed_files` must be built by the caller from *every* discovered Steam
/// library's installed depots, not just the one this `depotcache_dir` belongs
/// to - see `providers::steam::installed_depots_for_library` for the real
/// cross-library evidence that makes a per-library set unsafe here.
pub fn depotcache_orphans(
    depotcache_dir: &Path,
    needed_files: &HashSet<String>,
) -> std::io::Result<Vec<OrphanCandidate>> {
    let files = list_files(depotcache_dir)?;
    Ok(files
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    depot_manifest_filename_is_well_formed(name)
                        && !needed_files.contains(&name.to_lowercase())
                })
        })
        .map(|path| OrphanCandidate {
            path,
            kind: OrphanKind::UnreferencedFile,
        })
        .collect())
}

/// The Xbox / Microsoft Store (Game Pass) orphan-scan spec for one discovered
/// library root.
///
/// `xbox_games_root` is the `XboxGames` folder a `.GamingRoot` file points at
/// (see `providers::xbox`); it is where every Game Pass title installs as an
/// immediate subfolder. Like Steam's `steamapps/common`, `XboxGames` is a fixed
/// folder the platform creates and fills alone, so it is structurally exclusive
/// and needs no ownership marker - a subfolder with no live game behind it is a
/// leftover of the well-known "uninstalled a Game Pass title, the folder
/// stayed" kind.
///
/// No service folders: Xbox has no fixed download-staging directory alongside
/// `XboxGames` that is unambiguously safe to sweep.
///
/// Caveat (deletion, not detection): the game payload under `XboxGames\<game>\
/// Content` is ACL-protected by the Gaming Services, so removing a detected
/// leftover may be denied by the OS. That is handled honestly by the deletion
/// layer (a failed remove is reported as not-freed), and detection itself is
/// safe and correct regardless.
pub fn xbox_spec(xbox_games_root: &Path) -> OrphanScanSpec {
    OrphanScanSpec {
        container: xbox_games_root.to_path_buf(),
        service_folders: Vec::new(),
        ownership_markers: Vec::new(),
    }
}

/// The name of itch's per-game receipt directory, written inside every folder
/// the itch app installs (`<game>/.itch/receipt.json.gz`). Its presence is
/// structural proof a folder is an itch install - a foreign or manual game in
/// the same location never has it.
const ITCH_RECEIPT_MARKER: &str = ".itch";

/// The itch.io app orphan-scan spec for one discovered install location.
///
/// `location_root` is an itch install location (see `providers::itch`), where
/// every installed game ("cave") lives as an immediate subfolder. Unlike Steam,
/// this root is *user-chosen* and may be a folder shared with other launchers
/// or manual games, so it is **not** structurally exclusive. The ownership
/// marker [`ITCH_RECEIPT_MARKER`] closes that gap: only a subfolder that both
/// has no live cave behind it *and* still carries itch's own `.itch` receipt is
/// flagged, which is exactly an itch install the app forgot to clean up - never
/// a foreign or manually-installed game (which never carries that receipt).
///
/// No service folders for now: itch's `<name>-stage` staging dirs are wiped on
/// install success/discard and a lingering one may belong to a download in
/// progress, so sweeping them by a fixed rule is deferred.
pub fn itch_spec(location_root: &Path) -> OrphanScanSpec {
    OrphanScanSpec {
        container: location_root.to_path_buf(),
        service_folders: Vec::new(),
        ownership_markers: vec![PathBuf::from(ITCH_RECEIPT_MARKER)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed(paths: &[&str]) -> HashSet<String> {
        managed_dir_set(paths.iter().map(Path::new))
    }

    #[test]
    fn unmanaged_subdirs_returns_only_folders_without_a_manifest() {
        let subdirs = vec![
            PathBuf::from(r"F:\SteamLibrary\steamapps\common\Portal 2"),
            PathBuf::from(r"F:\SteamLibrary\steamapps\common\Half-Life"),
            PathBuf::from(r"F:\SteamLibrary\steamapps\common\LeftoverGame"),
        ];
        // Portal 2 and Half-Life are still installed (their manifests point
        // here); LeftoverGame is not.
        let managed = managed(&[
            r"F:\SteamLibrary\steamapps\common\Portal 2",
            r"F:\SteamLibrary\steamapps\common\Half-Life",
        ]);

        let orphans = unmanaged_subdirs(&subdirs, &managed);

        assert_eq!(
            orphans,
            vec![PathBuf::from(
                r"F:\SteamLibrary\steamapps\common\LeftoverGame"
            )],
            "only the folder with no matching manifest is an orphan"
        );
    }

    #[test]
    fn unmanaged_subdirs_matches_case_insensitively() {
        // Manifest-declared casing differs from the on-disk casing - on
        // Windows these are the same folder and it must NOT be flagged.
        let subdirs = vec![PathBuf::from(r"F:\SteamLibrary\steamapps\common\Portal 2")];
        let managed = managed(&[r"f:\steamlibrary\steamapps\common\portal 2"]);

        assert!(
            unmanaged_subdirs(&subdirs, &managed).is_empty(),
            "case-only differences must not make a live game look orphaned"
        );
    }

    #[test]
    fn unmanaged_subdirs_preserves_input_order() {
        let subdirs = vec![
            PathBuf::from(r"F:\lib\steamapps\common\Zeta"),
            PathBuf::from(r"F:\lib\steamapps\common\Alpha"),
        ];
        let orphans = unmanaged_subdirs(&subdirs, &HashSet::new());
        assert_eq!(orphans, subdirs, "output keeps disk order, does not sort");
    }

    #[test]
    fn unmanaged_subdirs_empty_when_every_folder_is_managed() {
        let subdirs = vec![PathBuf::from(r"F:\lib\steamapps\common\Portal 2")];
        let managed = managed(&[r"F:\lib\steamapps\common\Portal 2"]);
        assert!(unmanaged_subdirs(&subdirs, &managed).is_empty());
    }

    #[test]
    fn list_subdirs_returns_only_directories_and_skips_files() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let root = dir.path();
        std::fs::create_dir(root.join("GameA")).expect("create GameA");
        std::fs::create_dir(root.join("GameB")).expect("create GameB");
        std::fs::write(root.join("loose.txt"), b"x").expect("write loose file");

        let mut subdirs = list_subdirs(root).unwrap();
        subdirs.sort();

        assert_eq!(subdirs, vec![root.join("GameA"), root.join("GameB")]);
    }

    #[test]
    fn list_subdirs_on_missing_dir_is_empty_not_an_error() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let missing = dir.path().join("no-such-container");
        assert!(list_subdirs(&missing).unwrap().is_empty());
    }

    /// The primary orphan-residue acceptance case, end to end over a real (temp)
    /// filesystem: a known leftover folder is found, a game still installed
    /// via the launcher is NOT flagged, and an existing service folder is
    /// reported.
    #[test]
    fn find_orphans_flags_residue_and_service_folder_but_not_a_live_game() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let root = dir.path();
        let common = root.join("steamapps").join("common");
        std::fs::create_dir_all(&common).expect("create common");

        // A live game (its manifest will be in the managed set) and a leftover.
        std::fs::create_dir(common.join("Portal 2")).expect("create live game folder");
        std::fs::create_dir(common.join("UninstalledGame")).expect("create leftover folder");

        // An aborted download staging folder.
        let downloading = root.join("steamapps").join("downloading");
        std::fs::create_dir_all(&downloading).expect("create downloading");

        let spec = steam_spec(root);
        let managed = managed_dir_set(std::iter::once(common.join("Portal 2").as_path()));

        let orphans = find_orphans(&spec, &managed).unwrap();

        assert!(
            orphans.contains(&OrphanCandidate {
                path: common.join("UninstalledGame"),
                kind: OrphanKind::UnmanagedFolder,
            }),
            "the leftover folder must be reported as an unmanaged-folder orphan"
        );
        assert!(
            orphans.contains(&OrphanCandidate {
                path: downloading,
                kind: OrphanKind::ServiceFolder,
            }),
            "the existing downloading/ scratch folder must be reported"
        );
        assert!(
            !orphans
                .iter()
                .any(|orphan| orphan.path == common.join("Portal 2")),
            "a game still installed via the launcher must never be flagged as orphaned"
        );
    }

    #[test]
    fn find_orphans_omits_a_service_folder_that_does_not_exist() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("steamapps").join("common"))
            .expect("create empty common");
        // Note: no `steamapps/downloading` created.

        let spec = steam_spec(root);
        let orphans = find_orphans(&spec, &HashSet::new()).unwrap();

        assert!(
            orphans
                .iter()
                .all(|orphan| orphan.kind != OrphanKind::ServiceFolder),
            "a non-existent service folder must not be reported"
        );
    }

    #[test]
    fn steam_spec_points_at_common_and_downloading() {
        let spec = steam_spec(Path::new(r"F:\SteamLibrary"));
        assert_eq!(
            spec.container,
            PathBuf::from(r"F:\SteamLibrary\steamapps\common")
        );
        assert_eq!(
            spec.service_folders,
            vec![PathBuf::from(r"F:\SteamLibrary\steamapps\downloading")]
        );
        assert!(
            spec.ownership_markers.is_empty(),
            "steamapps/common is structurally exclusive - no marker required"
        );
    }

    #[test]
    fn xbox_spec_uses_the_root_directly_and_needs_no_marker() {
        let spec = xbox_spec(Path::new(r"F:\XboxGames"));
        assert_eq!(spec.container, PathBuf::from(r"F:\XboxGames"));
        assert!(spec.service_folders.is_empty());
        assert!(
            spec.ownership_markers.is_empty(),
            "XboxGames is a fixed platform-created folder - structurally exclusive"
        );
    }

    #[test]
    fn itch_spec_requires_the_receipt_marker() {
        let spec = itch_spec(Path::new(r"F:\itch"));
        assert_eq!(spec.container, PathBuf::from(r"F:\itch"));
        assert_eq!(spec.ownership_markers, vec![PathBuf::from(".itch")]);
    }

    /// Xbox: a structurally-exclusive container (no marker) reports a leftover
    /// and spares a live game, exactly like Steam.
    #[test]
    fn find_orphans_xbox_flags_leftover_but_not_a_live_game() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let root = dir.path().join("XboxGames");
        std::fs::create_dir_all(root.join("Starfield")).expect("live game");
        std::fs::create_dir_all(root.join("UninstalledTitle")).expect("leftover");

        let spec = xbox_spec(&root);
        let managed = managed_dir_set(std::iter::once(root.join("Starfield").as_path()));

        let orphans = find_orphans(&spec, &managed).unwrap();

        assert!(orphans.contains(&OrphanCandidate {
            path: root.join("UninstalledTitle"),
            kind: OrphanKind::UnmanagedFolder,
        }));
        assert!(
            !orphans.iter().any(|o| o.path == root.join("Starfield")),
            "a live Xbox game must never be flagged"
        );
    }

    /// itch: the ownership marker is what makes a *shared* install location
    /// safe. A leftover itch folder still carries `.itch` and is flagged; a
    /// foreign/manual game folder in the same location has no `.itch` and is
    /// never flagged, even though neither is in the managed set.
    #[test]
    fn find_orphans_itch_flags_only_folders_carrying_the_receipt() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let location = dir.path().join("itch");

        // A live itch game (in the managed set, carries a receipt).
        let live = location.join("celeste");
        std::fs::create_dir_all(live.join(".itch")).expect("live cave + receipt");

        // An itch leftover: receipt present, but no live cave behind it.
        let leftover = location.join("old-jam-game");
        std::fs::create_dir_all(leftover.join(".itch")).expect("leftover + receipt");

        // A foreign/manual game the user dropped into the same shared folder:
        // no itch receipt, so it must never be flagged.
        let foreign = location.join("ManualRepack");
        std::fs::create_dir_all(&foreign).expect("foreign folder, no receipt");

        let spec = itch_spec(&location);
        let managed = managed_dir_set(std::iter::once(live.as_path()));

        let orphans = find_orphans(&spec, &managed).unwrap();

        assert_eq!(
            orphans,
            vec![OrphanCandidate {
                path: leftover,
                kind: OrphanKind::UnmanagedFolder,
            }],
            "only the receipt-bearing leftover is an orphan; the live game and \
             the foreign folder are both spared"
        );
    }

    #[test]
    fn has_ownership_marker_is_true_when_no_marker_required() {
        // Empty marker list = structurally exclusive container: every candidate
        // qualifies (the managed-set diff is the only gate).
        assert!(has_ownership_marker(Path::new(r"F:\anything"), &[]).unwrap());
    }

    // -- GT-23: Workshop content --

    #[test]
    fn workshop_item_orphans_flags_an_id_not_listed_as_live() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let appid_dir = dir.path().join("217140");
        std::fs::create_dir_all(appid_dir.join("111")).expect("live item folder");
        std::fs::create_dir_all(appid_dir.join("999")).expect("leftover item folder");

        let live: HashSet<String> = ["111".to_string()].into_iter().collect();
        let orphans = workshop_item_orphans(&appid_dir, &live).unwrap();

        assert_eq!(
            orphans,
            vec![OrphanCandidate {
                path: appid_dir.join("999"),
                kind: OrphanKind::WorkshopItem,
            }],
            "only the id absent from the live set is residue, and it is a \
             Workshop item rather than a generic unmanaged folder (GT-187)"
        );
    }

    #[test]
    fn workshop_item_orphans_spares_every_id_still_listed_as_live() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let appid_dir = dir.path().join("217140");
        std::fs::create_dir_all(appid_dir.join("111")).expect("live item folder");

        let live: HashSet<String> = ["111".to_string()].into_iter().collect();
        assert!(
            workshop_item_orphans(&appid_dir, &live).unwrap().is_empty(),
            "a live subscription must never be flagged"
        );
    }

    #[test]
    fn workshop_item_orphans_on_a_missing_appid_dir_is_empty_not_an_error() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let missing = dir.path().join("no-such-appid");
        assert!(workshop_item_orphans(&missing, &HashSet::new())
            .unwrap()
            .is_empty());
    }

    // -- GT-23: depotcache --

    #[test]
    fn depotcache_orphans_flags_a_manifest_no_installed_app_names() {
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("621_999888777.manifest"), b"stale").unwrap();
        std::fs::write(dir.path().join("621_111222333.manifest"), b"needed").unwrap();

        let needed: HashSet<String> = ["621_111222333.manifest".to_string()].into_iter().collect();
        let orphans = depotcache_orphans(dir.path(), &needed).unwrap();

        assert_eq!(
            orphans,
            vec![OrphanCandidate {
                path: dir.path().join("621_999888777.manifest"),
                kind: OrphanKind::UnreferencedFile,
            }]
        );
    }

    #[test]
    fn depotcache_orphans_matches_needed_filenames_case_insensitively() {
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("621_999.MANIFEST"), b"needed").unwrap();

        let needed: HashSet<String> = ["621_999.manifest".to_string()].into_iter().collect();
        assert!(
            depotcache_orphans(dir.path(), &needed).unwrap().is_empty(),
            "case-only differences must not make a needed manifest look orphaned"
        );
    }

    #[test]
    fn depotcache_orphans_ignores_a_filename_that_does_not_match_the_manifest_shape() {
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("readme.txt"), b"not a manifest").unwrap();
        std::fs::write(dir.path().join("config.vdf"), b"not a manifest either").unwrap();

        assert!(
            depotcache_orphans(dir.path(), &HashSet::new())
                .unwrap()
                .is_empty(),
            "a file this check cannot positively identify must be left alone"
        );
    }

    #[test]
    fn depotcache_orphans_on_a_missing_dir_is_empty_not_an_error() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let missing = dir.path().join("no-such-depotcache");
        assert!(depotcache_orphans(&missing, &HashSet::new())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn steam_workshop_content_dir_and_state_path_are_nested_under_steamapps_workshop() {
        let root = Path::new(r"F:\SteamLibrary");
        assert_eq!(
            steam_workshop_content_dir(root),
            PathBuf::from(r"F:\SteamLibrary\steamapps\workshop\content")
        );
        assert_eq!(
            steam_workshop_state_path(root, "217140"),
            PathBuf::from(r"F:\SteamLibrary\steamapps\workshop\appworkshop_217140.acf")
        );
    }

    #[test]
    fn steam_depotcache_dir_is_a_sibling_of_steamapps_not_nested_inside_it() {
        // The one place real Steam data contradicted this card's starting
        // assumption - see the module doc comment.
        assert_eq!(
            steam_depotcache_dir(Path::new(r"F:\SteamLibrary")),
            PathBuf::from(r"F:\SteamLibrary\depotcache")
        );
    }
}
