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

/// A discovered game library (one root folder of one vendor).
#[derive(Debug, Clone)]
pub struct DiscoveredLibrary {
    /// Vendor tag stored in the DB, e.g. "steam".
    pub vendor: &'static str,
    /// Absolute path to the library root, e.g. `F:\SteamLibrary`.
    pub path: PathBuf,
    pub games: Vec<GameInstall>,
}

/// A source of game libraries (Steam, Epic, GOG, ...).
pub trait LibraryProvider {
    fn name(&self) -> &'static str;
    /// Discover all libraries of this vendor present on the machine.
    fn discover(&self) -> Result<Vec<DiscoveredLibrary>>;
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
        Box::new(xbox::XboxProvider),
        // Last on purpose: the heuristic folder scan re-finds libraries the
        // metadata providers already know about; merge_libraries_by_path
        // keeps the earlier (metadata) entries with their richer names/ids.
        Box::new(folderscan::FolderScanProvider),
    ]
}

/// How many directory levels below the probed folder [`holds_installed_files`]
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
pub fn holds_installed_files(dir: &Path) -> bool {
    let mut pending = vec![(dir.to_path_buf(), 0u32)];

    while let Some((current, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            // Unreadable (permissions, vanished mid-scan): nothing to prove a
            // game with, same silent-skip policy as `scanner::scan_dir`.
            continue;
        };

        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };

            // `file_type` comes free with the directory enumeration; the extra
            // `metadata` syscall is paid only for the rare reparse-point entry.
            let is_dir = if file_type.is_symlink() {
                match std::fs::metadata(entry.path()) {
                    Ok(resolved) => resolved.is_dir(),
                    Err(_) => continue,
                }
            } else {
                file_type.is_dir()
            };

            if !is_dir {
                return true;
            }
            if depth < INSTALL_PROBE_DEPTH {
                pending.push((entry.path(), depth + 1));
            }
        }
    }

    false
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
/// Safe with respect to orphan detection (GT-02) because the two vendors this
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
            })
        })
        .collect()
}

/// A path reduced to its comparable form: lowercase, backslash-separated, no
/// trailing separator. Windows paths are case-insensitive and providers are not
/// consistent about either the case or a trailing `\`.
fn comparable_path(path: &Path) -> String {
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
        assert!(!holds_installed_files(temp.path()));
    }

    /// The realistic shape of the residue this exists to reject: a removed
    /// install often leaves its directory skeleton behind with every file
    /// gone.
    #[test]
    fn holds_installed_files_rejects_a_tree_of_only_empty_folders() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(r"bin\x64")).unwrap();
        std::fs::create_dir_all(temp.path().join("data")).unwrap();

        assert!(!holds_installed_files(temp.path()));
    }

    #[test]
    fn holds_installed_files_accepts_a_file_at_the_top_level() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("readme.txt"), b"hi").unwrap();

        assert!(holds_installed_files(temp.path()));
    }

    /// A game whose top level is only folders is still a game - the probe has
    /// to look inside, not just count entries.
    #[test]
    fn holds_installed_files_accepts_a_file_nested_below_the_top_level() {
        let temp = tempfile::tempdir().unwrap();
        let deep = temp.path().join(r"bin\x64");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("game.exe"), b"MZ").unwrap();

        assert!(holds_installed_files(temp.path()));
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

        assert!(!holds_installed_files(&residue));
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

        assert!(holds_installed_files(&install));
    }

    #[test]
    fn holds_installed_files_is_false_for_a_path_that_does_not_exist() {
        assert!(!holds_installed_files(Path::new(
            r"Z:\definitely\not\a\folder"
        )));
    }

    #[test]
    fn merge_libraries_by_path_unions_games_of_same_root() {
        let from_metadata = DiscoveredLibrary {
            vendor: "epic",
            path: PathBuf::from(r"F:\Epic"),
            games: vec![game("Celeste (official name)", r"F:\Epic\Celeste")],
        };
        let from_folderscan = DiscoveredLibrary {
            vendor: "epic",
            path: PathBuf::from(r"f:\epic"),
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
                games: vec![game(
                    "Dragon Age Inquisition",
                    r"F:\SteamLibrary\steamapps\common\Dragon Age Inquisition",
                )],
            },
            DiscoveredLibrary {
                vendor: "ea",
                path: PathBuf::from(r"F:\SteamLibrary\steamapps\common"),
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
                games: Vec::new(),
            },
            DiscoveredLibrary {
                vendor: "steam",
                path: PathBuf::from(r"F:\SteamLibrary"),
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
                games: vec![game("Alpha", r"F:\SteamLibrary\steamapps\common\Alpha")],
            },
            DiscoveredLibrary {
                vendor: "ea",
                path: PathBuf::from(r"H:\EA"),
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
                games: vec![game("Alpha", r"F:\Epic\Alpha")],
            },
            DiscoveredLibrary {
                vendor: "gog",
                path: PathBuf::from(r"F:\GOG"),
                games: vec![game("Beta", r"F:\GOG\Beta")],
            },
        ]);

        assert_eq!(merged.len(), 2);
    }
}
