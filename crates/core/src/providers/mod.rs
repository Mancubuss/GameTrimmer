//! Discovery of game libraries across launcher vendors.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::Result;

pub mod amazon;
pub mod battlenet;
pub mod ea;
pub mod epic;
pub mod folderscan;
pub mod gog;
pub mod humble;
pub mod itch;
pub mod paradox;
pub mod riot;
pub mod rockstar;
pub mod steam;
pub mod ubisoft;
pub mod xbox;

/// One installed game inside a library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameInstall {
    /// Display name, e.g. "DOOM The Dark Ages".
    pub name: String,
    /// Absolute path to the game's install directory.
    pub install_dir: PathBuf,
    /// Vendor-specific id (Steam appid etc.), if known.
    pub app_id: Option<String>,
}

/// Whether a provider supplied enough authoritative evidence to prove that a
/// directory is no longer managed by that provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OrphanEvidence {
    /// Folder-name heuristics can find content, but cannot prove launcher
    /// ownership or absence and therefore never authorize orphan deletion.
    Heuristic,
    /// Some launcher evidence was unreadable or malformed.
    Degraded,
    /// The launcher's inventory for this library was read without omissions.
    Authoritative,
}

/// Completeness of one provider discovery attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryStatus {
    NotInstalled,
    Complete,
    Degraded,
    Failed,
}

/// Machine-readable evidence explaining why discovery is not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryDiagnostic {
    pub provider: &'static str,
    pub stage: &'static str,
    pub path: Option<PathBuf>,
    pub message: String,
}

/// Provider data plus an explicit statement about its completeness.
#[derive(Debug, Clone)]
pub struct DiscoveryReport<T> {
    pub data: T,
    pub status: DiscoveryStatus,
    pub diagnostics: Vec<DiscoveryDiagnostic>,
}

impl<T> DiscoveryReport<T> {
    pub fn complete(data: T) -> Self {
        Self {
            data,
            status: DiscoveryStatus::Complete,
            diagnostics: Vec::new(),
        }
    }

    pub fn not_installed(data: T) -> Self {
        Self {
            data,
            status: DiscoveryStatus::NotInstalled,
            diagnostics: Vec::new(),
        }
    }

    pub fn degraded(data: T, diagnostics: Vec<DiscoveryDiagnostic>) -> Self {
        debug_assert!(!diagnostics.is_empty());
        Self {
            data,
            status: DiscoveryStatus::Degraded,
            diagnostics,
        }
    }

    pub fn failed(data: T, diagnostic: DiscoveryDiagnostic) -> Self {
        Self {
            data,
            status: DiscoveryStatus::Failed,
            diagnostics: vec![diagnostic],
        }
    }
}

/// A discovered game library (one root folder of one vendor).
#[derive(Debug, Clone)]
pub struct DiscoveredLibrary {
    /// Vendor tag stored in the DB, e.g. "steam".
    pub vendor: &'static str,
    /// Absolute path to the library root, e.g. `F:\SteamLibrary`.
    pub path: PathBuf,
    pub games: Vec<GameInstall>,
    pub orphan_evidence: OrphanEvidence,
}

/// A source of game libraries (Steam, Epic, GOG, ...).
pub trait LibraryProvider {
    fn name(&self) -> &'static str;
    /// Legacy fallible implementation used by providers that have no partial
    /// result to preserve. Providers with item-level failures override
    /// [`Self::discover`] and return diagnostics for every skipped record.
    fn try_discover(&self) -> Result<Vec<DiscoveredLibrary>>;

    /// Discover all libraries of this vendor present on the machine without
    /// confusing "not installed" with unreadable evidence.
    fn discover(&self) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
        match self.try_discover() {
            Ok(libraries) if libraries.is_empty() => DiscoveryReport::not_installed(libraries),
            Ok(libraries) => DiscoveryReport::complete(libraries),
            Err(err) => DiscoveryReport::failed(
                Vec::new(),
                DiscoveryDiagnostic {
                    provider: self.name(),
                    stage: "provider",
                    path: None,
                    message: err.to_string(),
                },
            ),
        }
    }
}

/// All built-in providers, in discovery order. A provider whose launcher is
/// not installed returns an empty list from `discover()` — that is not an error.
pub fn all() -> Vec<Box<dyn LibraryProvider>> {
    vec![
        Box::new(steam::SteamProvider),
        Box::new(epic::EpicProvider),
        Box::new(gog::GogProvider),
        Box::new(ubisoft::UbisoftProvider),
        Box::new(ea::EaProvider),
        Box::new(battlenet::BattleNetProvider),
        Box::new(rockstar::RockstarProvider),
        Box::new(amazon::AmazonProvider),
        Box::new(riot::RiotProvider),
        Box::new(itch::ItchProvider),
        Box::new(humble::HumbleProvider),
        Box::new(paradox::ParadoxProvider),
        Box::new(xbox::XboxProvider),
        // Last on purpose: the heuristic folder scan re-finds libraries the
        // metadata providers already know about; merge_libraries_by_path
        // keeps the earlier (metadata) entries with their richer names/ids.
        Box::new(folderscan::FolderScanProvider),
    ]
}

/// How many directory levels below the probed folder [`try_holds_installed_files`]
/// descends before giving up. Covers every realistic layout
/// (`Game\bin\x64\game.exe` is found at depth 2) while keeping the probe
/// bounded - an unbounded walk here would run on every drive root at every
/// scan, and the bound is also what makes a reparse-point cycle terminate.
const INSTALL_PROBE_DEPTH: u32 = 3;

/// Whether a folder actually holds an installation - that is, whether it
/// contains at least one file, at any depth down to [`INSTALL_PROBE_DEPTH`].
///
/// Folder-name-based discovery cannot ask a launcher whether a game is
/// installed; all it sees is a subfolder of a vendor root. An installed game
/// always has files, so a folder with none is residue (a partially removed
/// install typically leaves the empty directory skeleton behind), not a game.
/// Registering it anyway put a phantom entry into the model of a tool that
/// DELETES FILES, counted in the same totals as real games - which is why
/// this is a correctness matter and not cosmetics.
///
/// Stops at the first file found, so the common case costs one `read_dir`.
///
/// Reparse points (junctions, symlinks) need resolving rather than trusting:
/// on Windows a junction reports `is_dir() == false` from `read_dir`, so
/// taking that at face value would count a *dangling* junction as a file and
/// hand back "installed" for a folder that is pure residue - the exact case
/// this function exists to reject. `fs::metadata` follows the link, which
/// separates the three outcomes that matter: it resolves to a directory (a
/// game really can live behind a junction, so descend), it resolves to a file
/// (content), or it resolves to nothing (no evidence either way - skip).
/// Cycles are safe because [`INSTALL_PROBE_DEPTH`] bounds the descent, not
/// because links go unfollowed.
///
/// Errors (a permission denial, a missing path) are handed to the caller
/// rather than folded into `false` - a fail-open answer here would read an
/// unreadable but still-installed game as residue. This used to be wrapped
/// in a `holds_installed_files(dir) -> bool` convenience that did exactly
/// that with `.unwrap_or(false)`; every production caller (`folderscan.rs`,
/// `manual.rs`) already called this `try_` form directly and turned `Err`
/// into a discovery diagnostic, so the fail-open wrapper had no real callers
/// left and was deleted rather than kept around as a trap for the next one.
pub fn try_holds_installed_files(dir: &Path) -> std::io::Result<bool> {
    let mut pending = vec![(dir.to_path_buf(), 0u32)];

    while let Some((current, depth)) = pending.pop() {
        let entries = std::fs::read_dir(&current)?;

        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;

            // `file_type` comes free with the directory enumeration; the extra
            // `metadata` syscall is paid only for the rare reparse-point entry.
            let is_dir = if file_type.is_symlink() {
                match std::fs::metadata(entry.path()) {
                    Ok(resolved) => resolved.is_dir(),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(err) => return Err(err),
                }
            } else {
                file_type.is_dir()
            };

            if !is_dir {
                return Ok(true);
            }
            if depth < INSTALL_PROBE_DEPTH {
                pending.push((entry.path(), depth + 1));
            }
        }
    }

    Ok(false)
}

/// Whether `path` exists and is a directory, keeping "definitely not there"
/// and "could not find out" apart.
///
/// [`Path::is_dir`] collapses both into `false`, which is fail-open in a tool
/// that deletes files. A manifest-backed game whose install directory cannot
/// be read - a per-folder DACL denial, an offline cloud placeholder, a drive
/// that has not spun up, a corrupt entry - would drop out of the managed set
/// while its folder is still sitting on disk. Orphan detection then diffs the
/// library against that incomplete set and classifies the live installation as
/// unmanaged residue.
///
/// So the probe reports absence only for `NotFound`, and every other error is
/// handed to the caller, which must turn it into a discovery diagnostic. A
/// diagnostic downgrades the library to [`OrphanEvidence::Degraded`], which is
/// what actually stops orphan classification for it.
pub fn try_is_dir(path: &Path) -> std::io::Result<bool> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

/// The one stage that records something without claiming the library's
/// inventory is unreliable.
///
/// A launcher record whose install directory is missing is normal - the game
/// was uninstalled elsewhere, or the download is queued or paused - and the
/// folder cannot be mistaken for orphan residue because it is not there. But
/// it is also the exact path the complaint "my game isn't in the list"
/// arrives on, so silence is the wrong answer too.
///
/// The distinction has to be explicit because every provider treats "any
/// diagnostic at all" as "the inventory is no longer authoritative", which is
/// what lets orphan detection trust it. Recording this one the ordinary way
/// strips a whole library's orphan-deletion authority over a single absent
/// folder.
///
/// The dangerous neighbour is a folder that *is* there and merely could not be
/// examined - see [`try_is_dir`]. That one stays an ordinary diagnostic and
/// keeps degrading, because it is the case where a live installation could be
/// classified as residue.
///
/// ponytail: matched by stage name; if a second non-degrading stage ever
/// appears, promote this to a field on [`DiscoveryDiagnostic`].
pub const GAME_ABSENT: &str = "game-absent";

/// Whether this diagnostic set means the library's inventory can no longer be
/// trusted for orphan detection. Everything except [`GAME_ABSENT`] does.
pub fn degrades_evidence(diagnostics: &[DiscoveryDiagnostic]) -> bool {
    diagnostics.iter().any(DiscoveryDiagnostic::is_failure)
}

impl DiscoveryDiagnostic {
    /// Whether this line reports something that actually went wrong.
    ///
    /// The per-diagnostic half of [`degrades_evidence`], and deliberately the
    /// same question: a note that leaves a library authoritative is also one
    /// that must not be stamped `ERROR` in the log. Keeping both answers in
    /// one predicate is what stops the log and the orphan-evidence decision
    /// from drifting into disagreeing about what counts as a failure.
    pub fn is_failure(&self) -> bool {
        self.stage != GAME_ABSENT
    }
}

/// The [`try_is_dir`] contract for a regular file.
pub fn try_is_file(path: &Path) -> std::io::Result<bool> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

/// Merges libraries that share the same root path (case-insensitive, since
/// Windows paths are not case-sensitive): the first occurrence keeps its
/// vendor tag and game entries, later occurrences only contribute games with
/// install directories not seen yet. Without this, two providers discovering
/// the same folder would double-register it - and `persist_libraries` would
/// let the later one clobber the earlier one's games.
pub fn merge_libraries_by_path(libraries: Vec<DiscoveredLibrary>) -> Vec<DiscoveredLibrary> {
    let mut merged: Vec<DiscoveredLibrary> = Vec::new();

    for library in libraries {
        let key = library.path.to_string_lossy().to_lowercase();
        let existing = merged
            .iter_mut()
            .find(|m| m.path.to_string_lossy().to_lowercase() == key);

        match existing {
            Some(target) => {
                target.orphan_evidence = target.orphan_evidence.min(library.orphan_evidence);
                for game in library.games {
                    let already_known = target.games.iter().any(|known| {
                        known
                            .install_dir
                            .to_string_lossy()
                            .eq_ignore_ascii_case(&game.install_dir.to_string_lossy())
                    });
                    if !already_known {
                        target.games.push(game);
                    }
                }
            }
            None => merged.push(library),
        }
    }

    merged
}

/// Registers `root` as an empty library of `vendor` unless some library
/// already covers that path (case-insensitive, since Windows paths are not
/// case-sensitive).
///
/// Most providers can only name a library root by deriving it from the games
/// they found ([`group_by_parent_dir`]), so with no games there is nothing to
/// report. A few know their root outright - Humble's download location, itch's
/// install locations - and for those "launcher installed, nothing in it" is a
/// distinguishable state worth showing: silence otherwise reads exactly like
/// "discovery is broken", which is a support question the user has to ask to
/// resolve.
///
/// Safe with respect to orphan detection (orphan-residue safety) because the two vendors this
/// applies to cannot mass-flag an empty root: Humble has no orphan spec at all,
/// and itch's requires a per-folder `.itch` receipt to call anything residue.
pub(crate) fn register_root(
    libraries: &mut Vec<DiscoveredLibrary>,
    vendor: &'static str,
    root: PathBuf,
) {
    let key = root.to_string_lossy().to_lowercase();
    let known = libraries
        .iter()
        .any(|library| library.path.to_string_lossy().to_lowercase() == key);

    if !known {
        libraries.push(DiscoveredLibrary {
            vendor,
            path: root,
            games: Vec::new(),
            orphan_evidence: OrphanEvidence::Authoritative,
        });
    }
}

/// Drops games already registered by an earlier library, comparing install
/// directories case-insensitively.
///
/// [`merge_libraries_by_path`] only reconciles providers that agree on the
/// library *root* - and two providers describing the same install routinely do
/// not. Steam registers the library root `F:\SteamLibrary`, while the Windows
/// uninstall entry an EA-published game writes for the same install yields
/// `F:\SteamLibrary\steamapps\common`: different roots, same game. Left alone
/// it is scanned twice and its findings counted twice, in a tool whose whole
/// output is "how much can you reclaim".
///
/// Runs after the merge, over the provider order from [`all`], so the first
/// description of a game is the one kept - and since the vendor-folder scan is
/// deliberately last, that is always a metadata provider's richer name and id
/// rather than a folder name. Nested install dirs count as the same game; see
/// [`same_or_nested`] for why that is not optional.
///
/// A library that *had* games and has none left is dropped, because it was
/// never a library: providers derive their root from the games they found
/// ([`group_by_parent_dir`]), so an EA-published game sitting in a Steam
/// library produces the "EA library" `F:\SteamLibrary\steamapps\common` - the
/// inside of another launcher's container, named after nothing. Once its only
/// game is recognized as Steam's, nothing of it remains but the fiction.
///
/// A library that was *already* empty survives, and the difference is the whole
/// point: [`register_root`] entries are roots the launcher itself named, so
/// they were found, not inferred. Showing those is deliberate; showing this is
/// not.
pub fn dedupe_games_across_libraries(libraries: Vec<DiscoveredLibrary>) -> Vec<DiscoveredLibrary> {
    let mut claimed: Vec<String> = Vec::new();

    libraries
        .into_iter()
        .filter_map(|library| {
            let DiscoveredLibrary {
                vendor,
                path,
                games,
                orphan_evidence,
            } = library;
            let was_derived_from_games = !games.is_empty();

            let games: Vec<GameInstall> = games
                .into_iter()
                .filter(|game| {
                    let key = comparable_path(&game.install_dir);
                    if claimed.iter().any(|known| same_or_nested(known, &key)) {
                        return false;
                    }
                    claimed.push(key);
                    true
                })
                .collect();

            if was_derived_from_games && games.is_empty() {
                return None;
            }

            Some(DiscoveredLibrary {
                vendor,
                path,
                games,
                orphan_evidence,
            })
        })
        .collect()
}

/// A path reduced to its comparable form: lowercase, backslash-separated, no
/// trailing separator. Windows paths are case-insensitive and providers are not
/// consistent about either the case or a trailing `\`.
///
/// `pub` rather than crate-private: this is the one path-comparison routine
/// in the codebase, and other path-keyed comparisons (library exclusion -
/// see `Settings::excluded_libraries`) reuse it rather than growing a second,
/// possibly-inconsistent normalization elsewhere.
pub fn comparable_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// Whether two [`comparable_path`] strings describe the same install or one
/// inside the other.
///
/// Nesting has to count as a duplicate because two providers can describe one
/// game at two depths: Riot's metadata points at the channel folder
/// `H:\Riot Games\VALORANT\live`, while the vendor-folder scan sees the game
/// folder `H:\Riot Games\VALORANT` above it. Registering both makes the scan
/// walk VALORANT twice and report its bytes twice - in the number the whole
/// tool exists to produce.
///
/// The boundary check is what keeps `...\Foo` from swallowing `...\FooBar`:
/// the remainder after the shared prefix must start at a path separator.
fn same_or_nested(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }

    let (shorter, longer) = if a.len() < b.len() { (a, b) } else { (b, a) };
    longer
        .strip_prefix(shorter)
        .is_some_and(|rest| rest.starts_with('\\'))
}

/// De-duplicates games by install directory (case-insensitive, since Windows
/// paths are not case-sensitive), keeping the first occurrence. Used by
/// providers that read the same game from several sources (EA's two registry
/// generations, the three Windows uninstall registry roots, ...).
pub(crate) fn dedupe_by_install_dir(games: Vec<GameInstall>) -> Vec<GameInstall> {
    let mut seen = HashSet::new();
    games
        .into_iter()
        .filter(|game| seen.insert(game.install_dir.to_string_lossy().to_lowercase()))
        .collect()
}

/// Groups already-filtered games into libraries by the parent directory of
/// each game's install directory. Every unique parent directory (case-insensitive
/// comparison, since Windows paths are not case-sensitive) becomes one
/// `DiscoveredLibrary` of `vendor`, in first-seen order. Games whose install
/// directory has no parent (e.g. a bare drive root) are dropped defensively -
/// that shape never occurs for a real game install.
pub(crate) fn group_by_parent_dir(
    vendor: &'static str,
    games: Vec<GameInstall>,
) -> Vec<DiscoveredLibrary> {
    let mut libraries: Vec<DiscoveredLibrary> = Vec::new();

    for game in games {
        let Some(parent) = game.install_dir.parent() else {
            continue;
        };
        let parent = parent.to_path_buf();

        let existing = libraries.iter_mut().find(|library| {
            library.path.to_string_lossy().to_lowercase() == parent.to_string_lossy().to_lowercase()
        });

        match existing {
            Some(library) => library.games.push(game),
            None => libraries.push(DiscoveredLibrary {
                vendor,
                path: parent,
                games: vec![game],
                orphan_evidence: OrphanEvidence::Authoritative,
            }),
        }
    }

    libraries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(name: &str, install_dir: &str) -> GameInstall {
        GameInstall {
            name: name.to_string(),
            install_dir: PathBuf::from(install_dir),
            app_id: None,
        }
    }

    #[test]
    fn group_by_parent_dir_groups_games_under_shared_parent() {
        let games = vec![
            game("Alpha", r"F:\Epic\Alpha"),
            game("Beta", r"F:\Epic\Beta"),
        ];

        let libraries = group_by_parent_dir("epic", games);

        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].vendor, "epic");
        assert_eq!(libraries[0].path, PathBuf::from(r"F:\Epic"));
        assert_eq!(libraries[0].games.len(), 2);
    }

    #[test]
    fn group_by_parent_dir_splits_games_under_different_parents() {
        let games = vec![
            game("Alpha", r"F:\Epic\Alpha"),
            game("Gamma", r"G:\Games\Epic\Gamma"),
        ];

        let libraries = group_by_parent_dir("epic", games);

        assert_eq!(libraries.len(), 2);
        assert_eq!(libraries[0].path, PathBuf::from(r"F:\Epic"));
        assert_eq!(libraries[1].path, PathBuf::from(r"G:\Games\Epic"));
    }

    #[test]
    fn group_by_parent_dir_is_case_insensitive_on_parent_path() {
        let games = vec![
            game("Alpha", r"F:\Epic\Alpha"),
            game("Beta", r"f:\epic\Beta"),
        ];

        let libraries = group_by_parent_dir("epic", games);

        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].games.len(), 2);
    }

    #[test]
    fn group_by_parent_dir_returns_empty_for_no_games() {
        assert!(group_by_parent_dir("epic", Vec::new()).is_empty());
    }

    #[test]
    fn dedupe_by_install_dir_keeps_first_occurrence_case_insensitively() {
        let games = vec![
            GameInstall {
                name: "From Origin".to_string(),
                install_dir: PathBuf::from(r"F:\EA Games\Apex Legends"),
                app_id: Some("origin-id".to_string()),
            },
            GameInstall {
                name: "From EA Desktop".to_string(),
                install_dir: PathBuf::from(r"f:\ea games\apex legends"),
                app_id: Some("ea-desktop-id".to_string()),
            },
        ];

        let deduped = dedupe_by_install_dir(games);

        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].name, "From Origin");
    }

    #[test]
    fn holds_installed_files_rejects_an_empty_folder() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!try_holds_installed_files(temp.path()).unwrap());
    }

    /// The realistic shape of the residue this exists to reject: a removed
    /// install often leaves its directory skeleton behind with every file
    /// gone.
    #[test]
    fn holds_installed_files_rejects_a_tree_of_only_empty_folders() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(r"bin\x64")).unwrap();
        std::fs::create_dir_all(temp.path().join("data")).unwrap();

        assert!(!try_holds_installed_files(temp.path()).unwrap());
    }

    #[test]
    fn holds_installed_files_accepts_a_file_at_the_top_level() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("readme.txt"), b"hi").unwrap();

        assert!(try_holds_installed_files(temp.path()).unwrap());
    }

    /// A game whose top level is only folders is still a game - the probe has
    /// to look inside, not just count entries.
    #[test]
    fn holds_installed_files_accepts_a_file_nested_below_the_top_level() {
        let temp = tempfile::tempdir().unwrap();
        let deep = temp.path().join(r"bin\x64");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("game.exe"), b"MZ").unwrap();

        assert!(try_holds_installed_files(temp.path()).unwrap());
    }

    /// Creates a directory junction, or returns `false` when this machine
    /// won't make one. Junctions need no elevation (unlike symlinks), but the
    /// filesystem under a temp dir is not guaranteed to support reparse
    /// points, so the tests below skip rather than fail on a machine that
    /// cannot host the scenario.
    #[cfg(windows)]
    fn try_make_junction(link: &Path, target: &Path) -> bool {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|out| out.status.success())
    }

    /// A junction reports `is_dir() == false` from `read_dir` on Windows, so
    /// trusting that flag would count a *dangling* one as a file - and a
    /// folder holding nothing but a broken link is exactly the residue this
    /// probe exists to reject.
    #[cfg(windows)]
    #[test]
    fn holds_installed_files_rejects_a_folder_holding_only_a_dangling_junction() {
        let temp = tempfile::tempdir().unwrap();
        let residue = temp.path().join("Uninstalled Game");
        std::fs::create_dir_all(&residue).unwrap();

        if !try_make_junction(&residue.join("shared"), &temp.path().join("gone")) {
            eprintln!("skipping: this filesystem would not create a junction");
            return;
        }

        assert!(!try_holds_installed_files(&residue).unwrap());
    }

    /// The other side of following the link: a game really can be installed
    /// behind a junction (launchers do this to move content to another drive),
    /// and that is a game, not residue.
    #[cfg(windows)]
    #[test]
    fn holds_installed_files_accepts_a_game_that_lives_behind_a_junction() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("elsewhere");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("game.exe"), b"MZ").unwrap();

        let install = temp.path().join("Game");
        std::fs::create_dir_all(&install).unwrap();

        if !try_make_junction(&install.join("content"), &real) {
            eprintln!("skipping: this filesystem would not create a junction");
            return;
        }

        assert!(try_holds_installed_files(&install).unwrap());
    }

    /// A missing directory is a `NotFound` `Err`, not a silent `false`: this
    /// is exactly the distinction the old `holds_installed_files` wrapper's
    /// `.unwrap_or(false)` erased, and production callers rely on getting the
    /// error so they can record a diagnostic instead of guessing "no files".
    #[test]
    fn holds_installed_files_errs_for_a_path_that_does_not_exist() {
        assert!(try_holds_installed_files(Path::new(r"Z:\definitely\not\a\folder")).is_err());
    }

    #[test]
    fn merge_libraries_by_path_unions_games_of_same_root() {
        let from_metadata = DiscoveredLibrary {
            vendor: "epic",
            path: PathBuf::from(r"F:\Epic"),
            orphan_evidence: OrphanEvidence::Authoritative,
            games: vec![game("Celeste (official name)", r"F:\Epic\Celeste")],
        };
        let from_folderscan = DiscoveredLibrary {
            vendor: "epic",
            path: PathBuf::from(r"f:\epic"),
            orphan_evidence: OrphanEvidence::Heuristic,
            games: vec![
                game("Celeste", r"f:\epic\Celeste"),
                game("Inside", r"f:\epic\Inside"),
            ],
        };

        let merged = merge_libraries_by_path(vec![from_metadata, from_folderscan]);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].path, PathBuf::from(r"F:\Epic"));
        assert_eq!(merged[0].games.len(), 2);
        // The first (metadata) entry's richer name wins for the shared game.
        assert_eq!(merged[0].games[0].name, "Celeste (official name)");
        assert_eq!(merged[0].games[1].name, "Inside");
    }

    #[test]
    fn register_root_adds_an_empty_library_when_the_root_is_unknown() {
        let mut libraries = Vec::new();
        register_root(&mut libraries, "humble", PathBuf::from(r"H:\Humble"));

        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].vendor, "humble");
        assert_eq!(libraries[0].path, PathBuf::from(r"H:\Humble"));
        assert!(libraries[0].games.is_empty());
    }

    /// The root a provider knows outright is usually also the parent the games
    /// were grouped under - registering it again would double the library.
    #[test]
    fn register_root_does_not_duplicate_a_root_already_covered() {
        let mut libraries = vec![DiscoveredLibrary {
            vendor: "humble",
            path: PathBuf::from(r"H:\Humble"),
            orphan_evidence: OrphanEvidence::Authoritative,
            games: vec![game("FTL", r"H:\Humble\FTL")],
        }];
        register_root(&mut libraries, "humble", PathBuf::from(r"h:\humble"));

        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].games.len(), 1);
    }

    /// The real shape: Steam claims the library root, an EA-published game in
    /// that same library is also described by its own uninstall entry whose
    /// parent dir is `steamapps\common`. Merging by root cannot see they are
    /// the same install; this pass can.
    #[test]
    fn dedupe_games_across_libraries_drops_a_game_claimed_by_an_earlier_library() {
        let deduped = dedupe_games_across_libraries(vec![
            DiscoveredLibrary {
                vendor: "steam",
                path: PathBuf::from(r"F:\SteamLibrary"),
                orphan_evidence: OrphanEvidence::Authoritative,
                games: vec![game(
                    "Dragon Age Inquisition",
                    r"F:\SteamLibrary\steamapps\common\Dragon Age Inquisition",
                )],
            },
            DiscoveredLibrary {
                vendor: "ea",
                path: PathBuf::from(r"F:\SteamLibrary\steamapps\common"),
                orphan_evidence: OrphanEvidence::Heuristic,
                games: vec![game(
                    "Dragon Age\u{2122}: Inquisition",
                    r"f:\steamlibrary\steamapps\common\dragon age inquisition",
                )],
            },
        ]);

        assert_eq!(
            deduped.len(),
            1,
            "the EA 'library' was only ever the parent dir of a game Steam owns - \
             with that game gone it must not be left behind as an empty entry \
             pointing inside steamapps"
        );
        assert_eq!(deduped[0].vendor, "steam");
        assert_eq!(deduped[0].games.len(), 1);
    }

    /// Riot's real shape: the metadata provider reports the channel folder,
    /// the vendor-folder scan reports the game folder one level above it, and
    /// both land in the same library. Scanning both walks VALORANT twice.
    #[test]
    fn dedupe_games_across_libraries_drops_a_game_nested_inside_a_claimed_one() {
        let deduped = dedupe_games_across_libraries(vec![DiscoveredLibrary {
            vendor: "riot",
            path: PathBuf::from(r"H:\Riot Games"),
            orphan_evidence: OrphanEvidence::Authoritative,
            games: vec![
                game("VALORANT", r"H:\Riot Games\VALORANT\live"),
                game("VALORANT", r"H:\Riot Games\VALORANT"),
            ],
        }]);

        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].games.len(), 1);
        // Provider order decides: the metadata provider ran first, so its name
        // and app id are the ones kept.
        assert_eq!(
            deduped[0].games[0].install_dir,
            PathBuf::from(r"H:\Riot Games\VALORANT\live")
        );
    }

    #[test]
    fn same_or_nested_requires_a_separator_boundary() {
        assert!(same_or_nested(r"h:\games\foo", r"h:\games\foo\bin"));
        assert!(same_or_nested(r"h:\games\foo\bin", r"h:\games\foo"));
        assert!(same_or_nested(r"h:\games\foo", r"h:\games\foo"));
        // The trap a plain `starts_with` falls into.
        assert!(!same_or_nested(r"h:\games\foo", r"h:\games\foobar"));
        assert!(!same_or_nested(r"h:\games\foo", r"h:\games\bar"));
    }

    #[test]
    fn comparable_path_normalizes_case_slashes_and_trailing_separator() {
        assert_eq!(
            comparable_path(Path::new("H:/Riot Games/VALORANT\\")),
            r"h:\riot games\valorant"
        );
    }

    /// Sibling folders must survive - only the nesting relation is a duplicate.
    #[test]
    fn dedupe_games_across_libraries_keeps_siblings_with_a_shared_prefix() {
        let deduped = dedupe_games_across_libraries(vec![DiscoveredLibrary {
            vendor: "steam",
            path: PathBuf::from(r"F:\SteamLibrary\steamapps\common"),
            orphan_evidence: OrphanEvidence::Authoritative,
            games: vec![
                game("Portal", r"F:\SteamLibrary\steamapps\common\Portal"),
                game("Portal 2", r"F:\SteamLibrary\steamapps\common\Portal 2"),
            ],
        }]);

        assert_eq!(deduped[0].games.len(), 2);
    }

    /// The distinction that keeps both behaviours: a root the launcher named
    /// itself (`register_root`) is empty on purpose and must survive, unlike a
    /// root that was inferred from games another provider turned out to own.
    #[test]
    fn dedupe_games_across_libraries_keeps_a_deliberately_empty_root() {
        let deduped = dedupe_games_across_libraries(vec![
            DiscoveredLibrary {
                vendor: "humble",
                path: PathBuf::from(r"H:\Humble"),
                orphan_evidence: OrphanEvidence::Authoritative,
                games: Vec::new(),
            },
            DiscoveredLibrary {
                vendor: "steam",
                path: PathBuf::from(r"F:\SteamLibrary"),
                orphan_evidence: OrphanEvidence::Authoritative,
                games: vec![game("Alpha", r"F:\SteamLibrary\steamapps\common\Alpha")],
            },
        ]);

        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].vendor, "humble");
        assert!(deduped[0].games.is_empty());
    }

    #[test]
    fn dedupe_games_across_libraries_keeps_distinct_installs() {
        let deduped = dedupe_games_across_libraries(vec![
            DiscoveredLibrary {
                vendor: "steam",
                path: PathBuf::from(r"F:\SteamLibrary"),
                orphan_evidence: OrphanEvidence::Authoritative,
                games: vec![game("Alpha", r"F:\SteamLibrary\steamapps\common\Alpha")],
            },
            DiscoveredLibrary {
                vendor: "ea",
                path: PathBuf::from(r"H:\EA"),
                orphan_evidence: OrphanEvidence::Authoritative,
                games: vec![game("Beta", r"H:\EA\Beta")],
            },
        ]);

        assert_eq!(deduped[0].games.len(), 1);
        assert_eq!(deduped[1].games.len(), 1);
    }

    #[test]
    fn merge_libraries_by_path_keeps_distinct_roots_apart() {
        let merged = merge_libraries_by_path(vec![
            DiscoveredLibrary {
                vendor: "epic",
                path: PathBuf::from(r"F:\Epic"),
                orphan_evidence: OrphanEvidence::Authoritative,
                games: vec![game("Alpha", r"F:\Epic\Alpha")],
            },
            DiscoveredLibrary {
                vendor: "gog",
                path: PathBuf::from(r"F:\GOG"),
                orphan_evidence: OrphanEvidence::Authoritative,
                games: vec![game("Beta", r"F:\GOG\Beta")],
            },
        ]);

        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn try_is_dir_reports_absence_only_for_not_found() {
        let temp = tempfile::tempdir().unwrap();
        assert!(try_is_dir(temp.path()).unwrap());
        assert!(!try_is_dir(&temp.path().join("nothing-here")).unwrap());

        // An invalid component makes the probe fail with something other than
        // "not found" - the portable stand-in for a folder that exists but
        // cannot be examined. It must surface as `Err`, never as "absent".
        let unexaminable = temp.path().join("bad<name");
        assert!(
            try_is_dir(&unexaminable).is_err(),
            "an unexaminable path must not be reported as absent"
        );
    }

    #[test]
    fn try_is_file_reports_absence_only_for_not_found() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("marker.txt");
        std::fs::write(&file, b"x").unwrap();

        assert!(try_is_file(&file).unwrap());
        assert!(!try_is_file(&temp.path().join("nothing-here")).unwrap());
        assert!(
            !try_is_file(temp.path()).unwrap(),
            "a directory is not a file"
        );
        assert!(try_is_file(&temp.path().join("bad<name")).is_err());
    }
}
