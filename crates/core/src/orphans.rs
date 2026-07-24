//! Detection of orphaned launcher residue (GT-02): folders that sit inside a
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
//! increment (see `BACKLOG.md`, GT-02) - deliberately kept out of here so the
//! detection logic can be verified in isolation against GT-02's acceptance
//! criteria first.
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

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Why a path is considered orphaned residue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanKind {
    /// A subfolder of a launcher's managed install container (e.g.
    /// `steamapps/common/<name>`) with no matching manifest - the classic
    /// "uninstalled the game, the folder stayed" leftover.
    UnmanagedFolder,
    /// A launcher's fixed download/cache scratch folder (aborted or partial
    /// downloads, depot cache, ...): residue regardless of what is installed.
    ServiceFolder,
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

/// Lists the immediate subdirectories of `dir` (non-recursive), skipping files
/// and anything that cannot be read. Returns an empty vector when `dir` does
/// not exist or is not a directory - an absent container is simply "no orphans
/// here", never an error that should fail a scan.
///
/// Symlinks/junctions are intentionally *not* followed into: a junction inside
/// `common` is reported as its own entry (a candidate for removal as the link),
/// never traversed into whatever it targets - mirroring the deletion layer's
/// `symlink_metadata` stance in `ops`.
pub fn list_subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            // `file_type()` here does not follow the link, so a junction is
            // classified by what it *is* (a reparse point that is a directory),
            // not by its target.
            entry
                .file_type()
                .map(|file_type| file_type.is_dir())
                .unwrap_or(false)
        })
        .map(|entry| entry.path())
        .collect()
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
pub fn find_orphans(
    spec: &OrphanScanSpec,
    managed_install_dirs: &HashSet<String>,
) -> Vec<OrphanCandidate> {
    let mut candidates = Vec::new();

    for path in unmanaged_subdirs(&list_subdirs(&spec.container), managed_install_dirs) {
        candidates.push(OrphanCandidate {
            path,
            kind: OrphanKind::UnmanagedFolder,
        });
    }

    for service in &spec.service_folders {
        // Only report a scratch folder that actually exists; `symlink_metadata`
        // (not `exists`) so a dangling junction still counts as present - it is
        // itself a removable entry.
        if std::fs::symlink_metadata(service).is_ok() {
            candidates.push(OrphanCandidate {
                path: service.clone(),
                kind: OrphanKind::ServiceFolder,
            });
        }
    }

    candidates
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
/// folders are limited, for now, to `steamapps/downloading` (aborted/partial
/// depot downloads): the single unambiguously-safe scratch location. Broader
/// residue (`depotcache`, orphaned `workshop` content of uninstalled games)
/// is deferred - those need per-item reasoning about what is still referenced,
/// which is a later GT-02 increment, not a fixed-folder sweep.
pub fn steam_spec(library_root: &Path) -> OrphanScanSpec {
    let steamapps = library_root.join("steamapps");
    OrphanScanSpec {
        container: steamapps.join("common"),
        service_folders: vec![steamapps.join("downloading")],
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

        let mut subdirs = list_subdirs(root);
        subdirs.sort();

        assert_eq!(subdirs, vec![root.join("GameA"), root.join("GameB")]);
    }

    #[test]
    fn list_subdirs_on_missing_dir_is_empty_not_an_error() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let missing = dir.path().join("no-such-container");
        assert!(list_subdirs(&missing).is_empty());
    }

    /// The headline GT-02 acceptance case, end to end over a real (temp)
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

        let orphans = find_orphans(&spec, &managed);

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
        let orphans = find_orphans(&spec, &HashSet::new());

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
    }
}
