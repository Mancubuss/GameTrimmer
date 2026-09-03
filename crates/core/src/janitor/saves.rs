//! The save and settings listing, and the Zero-Data-Loss Shield (GT-184).
//!
//! Lists what is in the conventional save locations and leaves the choice to
//! the player: which of somebody's saves matters is not a question a rule can
//! answer. The two judgements kept are which *folder* a file belongs to (so a
//! listing reads as games rather than as 368 loose files) and which autosaves
//! are past the retention count.
//!
//! - Roots are resolved through the shell, not `%USERPROFILE%` - Documents and
//!   Saved Games are redirectable (see [`crate::knownfolders`]).
//! - `LocalLow` is shared with ordinary applications, so a folder there is
//!   listed only once it is *shown* to hold a save. Without that gate the
//!   listing offered Yandex's updater settings as game data.
//! - Deleting anything here goes through a timestamped ZIP backup first.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use crate::janitor::JanitorArtifact;
use crate::rules::Category;

/// Classification of a game save file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveKind {
    /// Named/manual save created explicitly by user - NEVER auto-pruned.
    Manual,
    /// Automated or quicksave generated repeatedly - candidate for retention pruning.
    AutoOrQuick,
}

/// Which Windows folder a game's saves hang off.
///
/// Named rather than a `bool`, because two of the three are *redirectable*:
/// "Documents" and "Saved Games" are shell known folders a user can move to
/// another drive entirely, and this machine is one where Documents lives on
/// `E:`. Assuming `%USERPROFILE%\Documents` made every Documents-based game
/// below silently undiscoverable - the pruner reported nothing and looked
/// like it simply had no work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveBase {
    /// `FOLDERID_Documents`.
    Documents,
    /// `FOLDERID_SavedGames`.
    SavedGames,
    /// `%USERPROFILE%` itself - for `AppData` paths, which do not move.
    UserProfile,
}

/// Classifies whether a filename is an autosave/quicksave or a manual save.
pub fn classify_save_file(filename: &str) -> SaveKind {
    let lower = filename.to_ascii_lowercase();

    // Specific patterns for quick/autosaves
    if lower.starts_with("autosave")
        || lower.starts_with("quicksave")
        || lower.starts_with("auto_")
        || lower.starts_with("quick_")
        || lower.contains("_autosave")
        || lower.contains("_quicksave")
        || lower.starts_with("checkpoint")
    {
        SaveKind::AutoOrQuick
    } else {
        SaveKind::Manual
    }
}

/// Every place a game's saves could sit for `base`, most authoritative first.
///
/// Windows answers "where is Documents" through the shell, not through an
/// environment variable, and the answer moves: a redirected Documents folder,
/// a OneDrive-backed one, or the plain profile default are all normal. All
/// three are returned rather than only the first, because a game may have
/// written its saves before the folder was moved.
fn base_dirs(base: SaveBase) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut push = |dir: PathBuf| {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };

    match base {
        SaveBase::Documents => {
            if let Some(dir) = crate::knownfolders::documents_dir() {
                push(dir);
            }
            if let Ok(user_profile) = std::env::var("USERPROFILE") {
                push(PathBuf::from(user_profile).join("Documents"));
            }
            if let Ok(onedrive) = std::env::var("OneDrive") {
                push(PathBuf::from(onedrive).join("Documents"));
            }
        }
        SaveBase::SavedGames => {
            if let Some(dir) = crate::knownfolders::saved_games_dir() {
                push(dir);
            }
            if let Ok(user_profile) = std::env::var("USERPROFILE") {
                push(PathBuf::from(user_profile).join("Saved Games"));
            }
        }
        SaveBase::UserProfile => {
            if let Ok(user_profile) = std::env::var("USERPROFILE") {
                push(PathBuf::from(user_profile));
            }
        }
    }

    dirs
}

/// File extensions that are somebody's saved progress.
///
/// Deliberately conservative. A bare `.dat` or `.bin` in a game folder is as
/// likely to be a cache as a save, and a listing that offers a cache file
/// labelled "save" is worse than one that misses it - so those are reached
/// through the name test below (`savegame.dat`) rather than through their
/// extension.
const SAVE_EXTENSIONS: &[&str] = &[
    "sav", "save", "ess", "fos", "sfs", "lsv", "whs", "omwsave", "sgd", "slot", "profile",
    "savegame",
];

/// Extensions of the settings files that sit beside saves. Listed, never
/// pre-ticked: deleting one resets a game's controls and graphics, which is
/// a nuisance rather than a loss, and only the user knows if they want it.
const CONFIG_EXTENSIONS: &[&str] = &["ini", "cfg", "config", "xml", "json", "toml", "yaml", "yml"];

/// Names that mean "this is a save" whatever the extension says.
fn looks_like_save_name(stem: &str) -> bool {
    let stem = stem.to_ascii_lowercase();
    stem.starts_with("save")
        || stem.starts_with("autosave")
        || stem.starts_with("quicksave")
        || stem.starts_with("quick")
        || stem.starts_with("checkpoint")
        || stem.starts_with("slot")
        || stem.contains("savegame")
        || stem.contains("_save")
}

/// What one file in a save folder is, for the purpose of listing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveFileRole {
    Save,
    Config,
}

/// Windows' own per-folder files. `desktop.ini` matches the settings
/// extensions exactly and belongs to Explorer, not to any game.
const NOT_GAME_FILES: &[&str] = &["desktop.ini", "thumbs.db"];

fn role_of(path: &Path) -> Option<SaveFileRole> {
    let name = path.file_name()?.to_str()?;
    if NOT_GAME_FILES.contains(&name.to_ascii_lowercase().as_str()) {
        return None;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) => (stem, ext.to_ascii_lowercase()),
        None => (name, String::new()),
    };
    if SAVE_EXTENSIONS.contains(&ext.as_str()) || looks_like_save_name(stem) {
        return Some(SaveFileRole::Save);
    }
    if CONFIG_EXTENSIONS.contains(&ext.as_str()) {
        return Some(SaveFileRole::Config);
    }
    None
}

/// One place saves are looked for, and how the files under it map onto games.
struct SaveRoot {
    /// Where the walk starts.
    dir: PathBuf,
    /// How many segments below [`SaveRoot::dir`] name the game these saves
    /// belong to. `My Games\<game>` is one; `LocalLow\<company>\<game>` is
    /// two; `0` means the root *is* one game's folder.
    group_depth: usize,
    /// How deep below the root files are looked for.
    max_depth: usize,
    /// Whether a folder has to hold an actual save before its settings files
    /// are listed. Set for the roots ordinary applications share with games -
    /// `LocalLow` holds Unity saves *and* Yandex's updater, and the `.xml`
    /// beside either looks identical.
    needs_save_evidence: bool,
}

/// How deep below a root a save file is looked for. Three segments covers
/// `<company>/<game>/<profile>` (Unity) and `<game>/Saves/<character>`
/// (Bethesda) without descending into a whole documents tree.
const MAX_SAVE_DEPTH: usize = 3;

/// Save folders that sit outside every conventional root, as
/// `(base, path below it, group depth, walk depth)`.
///
/// Short by design: a game under `My Games` or `Saved Games` needs no entry,
/// and one that only exists here is one the roots below cannot reach at all.
const EXTRA_SAVE_ROOTS: &[(SaveBase, &str, usize, usize)] = &[
    // Directly under Documents rather than under `My Games`.
    (SaveBase::Documents, "The Witcher 3", 0, MAX_SAVE_DEPTH),
    // `AppData\Local` is not walked - one publisher folder of it is, and the
    // saves sit six segments down (`<game>\PlayerProfiles\<profile>\...`).
    (SaveBase::UserProfile, r"AppData\Local\Larian Studios", 1, 7),
];

/// The directories games conventionally keep saves and settings in.
///
/// Resolved through the shell rather than through `%USERPROFILE%`: Documents
/// and Saved Games are redirectable known folders, and on a machine where
/// Documents lives on another drive every profile-relative guess misses (see
/// [`crate::knownfolders`]). `LocalLow` is where Unity games write, and that
/// one does not move.
fn save_location_roots() -> Vec<SaveRoot> {
    let mut roots: Vec<SaveRoot> = Vec::new();
    let mut push = |root: SaveRoot| {
        // A root inside one already listed would report its files twice, under
        // two different group names.
        if root.dir.is_dir()
            && !roots
                .iter()
                .any(|listed: &SaveRoot| root.dir.starts_with(&listed.dir))
        {
            roots.push(root);
        }
    };

    for documents in base_dirs(SaveBase::Documents) {
        push(SaveRoot {
            dir: documents.join("My Games"),
            group_depth: 1,
            max_depth: MAX_SAVE_DEPTH,
            needs_save_evidence: false,
        });
    }
    for saved_games in base_dirs(SaveBase::SavedGames) {
        push(SaveRoot {
            dir: saved_games,
            group_depth: 1,
            max_depth: MAX_SAVE_DEPTH,
            needs_save_evidence: false,
        });
    }
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        push(SaveRoot {
            dir: PathBuf::from(user_profile).join("AppData").join("LocalLow"),
            group_depth: 2,
            max_depth: MAX_SAVE_DEPTH,
            needs_save_evidence: true,
        });
    }
    for (base, rel_path, group_depth, max_depth) in EXTRA_SAVE_ROOTS {
        for dir in base_dirs(*base) {
            push(SaveRoot {
                dir: dir.join(rel_path),
                group_depth: *group_depth,
                max_depth: *max_depth,
                needs_save_evidence: false,
            });
        }
    }

    roots
}

/// How many files one root may contribute. A root the user has pointed at
/// something enormous cannot flood the results; the caller reports when the
/// cap is reached rather than quietly truncating.
const MAX_FILES_PER_ROOT: usize = 2000;

fn collect_files(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    budget: &mut usize,
    out: &mut Vec<(PathBuf, SaveFileRole)>,
    cancel: &AtomicBool,
) {
    if depth > max_depth || *budget == 0 || cancel.load(Ordering::Relaxed) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if *budget == 0 || cancel.load(Ordering::Relaxed) {
            return;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_files(&path, depth + 1, max_depth, budget, out, cancel);
        } else if file_type.is_file() {
            if let Some(role) = role_of(&path) {
                *budget -= 1;
                out.push((path, role));
            }
        }
    }
}

/// Lists the save and settings files in the conventional save locations.
///
/// This lists; it does not decide. Every file comes back unselected, with its
/// size and its folder, and the user picks - which is the only workable answer
/// to "which of these saves matters", because the answer differs per player.
/// The previous shape asked the opposite question, and could only answer it
/// for nine hardcoded games.
///
/// The one judgement left is retention: within a folder, files whose *name*
/// says autosave or quicksave are sorted newest first, and everything past
/// `retain_count` is described as excess. Same rule as before, applied
/// wherever the files actually are.
pub fn scan_save_locations(retain_count: usize, cancel: &AtomicBool) -> Vec<JanitorArtifact> {
    let mut artifacts = Vec::new();
    for root in save_location_roots() {
        if cancel.load(Ordering::Relaxed) {
            return artifacts;
        }
        artifacts.extend(scan_save_root(&root, retain_count, cancel));
    }
    artifacts
}

/// Folder names that are part of a game's own save layout rather than the name
/// of anything a player would recognise. A folder made of these is a game's
/// internals, so the group stops above it.
const STRUCTURAL_FOLDERS: &[&str] = &[
    "save",
    "saves",
    "savegame",
    "savegames",
    "saved",
    "savedata",
    "saveddata",
    "savedgames",
    "profile",
    "profiles",
    "settings",
    "config",
    "configs",
    "shaders",
    "user",
    "users",
    "backup",
    "backups",
    "slots",
    "autosave",
    "autosaves",
    "storage",
    "data",
    "cache",
    "logs",
    "screenshots",
];

/// Whether a folder name reads as an identifier - a Steam id, a profile GUID -
/// rather than as a name. Such a folder is one campaign inside a game, so the
/// group stops above it too, or every character would become its own heading.
fn looks_like_identifier(name: &str) -> bool {
    let stripped: String = name
        .chars()
        .filter(|c| !matches!(c, '{' | '}' | '-' | '_'))
        .collect();
    stripped.len() >= 8
        && stripped.chars().all(|c| c.is_ascii_alphanumeric())
        && stripped.chars().any(|c| c.is_ascii_digit())
        && !stripped.chars().any(|c| c.is_ascii_lowercase())
}

/// Whether `dir` holds *other games* rather than one game's own files.
///
/// `Saved Games` mixes both shapes: `KingdomCome` is a game, and beside it
/// `id Software` is four of them. Grouping by the folder alone would file DOOM
/// and Quake under one heading, which is the flat list the grouping exists to
/// replace - so a folder with no save of its own, whose every child reads as a
/// game's name rather than as save-layout or an identifier, is descended past.
fn is_publisher_dir(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut has_child_dir = false;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            return false;
        };
        if file_type.is_file() {
            // A save or a settings file of its own makes this a game's folder.
            if role_of(&entry.path()).is_some() {
                return false;
            }
            continue;
        }
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if STRUCTURAL_FOLDERS.contains(&name.to_ascii_lowercase().as_str())
            || looks_like_identifier(&name)
        {
            return false;
        }
        has_child_dir = true;
    }
    has_child_dir
}

/// The folder that names the game a file belongs to: the root plus as many of
/// the segments below it as [`SaveRoot::group_depth`] asks for, and one more
/// when that lands on a publisher's folder (see [`is_publisher_dir`]). A file
/// lying directly in the root belongs to the root itself rather than to a
/// folder named after it.
fn group_dir_of(root: &SaveRoot, path: &Path, publishers: &mut HashMap<PathBuf, bool>) -> PathBuf {
    let Ok(relative) = path.strip_prefix(&root.dir) else {
        return root.dir.clone();
    };
    let segments: Vec<_> = relative.components().collect();
    // The file's own name is never part of its group.
    let available = segments.len().saturating_sub(1);
    let mut group = root.dir.clone();
    let mut taken = 0;
    while taken < root.group_depth.min(available) {
        group.push(segments[taken]);
        taken += 1;
    }
    if taken < available {
        let publisher = *publishers
            .entry(group.clone())
            .or_insert_with(|| is_publisher_dir(&group));
        if publisher {
            group.push(segments[taken]);
        }
    }
    group
}

/// [`scan_save_locations`] for one root, so the listing, the grouping and the
/// retention rule can be tested against a directory rather than against
/// whatever this machine happens to have in Documents.
fn scan_save_root(
    root: &SaveRoot,
    retain_count: usize,
    cancel: &AtomicBool,
) -> Vec<JanitorArtifact> {
    let mut budget = MAX_FILES_PER_ROOT;
    let mut files = Vec::new();
    collect_files(
        &root.dir,
        0,
        root.max_depth,
        &mut budget,
        &mut files,
        cancel,
    );

    // Which groups hold a real save. `LocalLow` is shared with applications
    // that have no saves at all, and their settings files are indistinguishable
    // from a game's by name alone - so there, a group has to earn its listing.
    // One `read_dir` per candidate folder, not per file under it.
    let mut publishers: HashMap<PathBuf, bool> = HashMap::new();
    let proven: HashSet<PathBuf> = files
        .iter()
        .filter(|(_, role)| *role == SaveFileRole::Save)
        .map(|(path, _)| group_dir_of(root, path, &mut publishers))
        .collect();

    // Retention is counted per folder: two games' autosaves are not one
    // series, and neither are two characters' within one game.
    let mut series: HashMap<PathBuf, Vec<(PathBuf, SystemTime)>> = HashMap::new();
    for (path, role) in &files {
        if *role != SaveFileRole::Save {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if classify_save_file(name) != SaveKind::AutoOrQuick {
            continue;
        }
        let Ok(mtime) = std::fs::metadata(path).and_then(|meta| meta.modified()) else {
            continue;
        };
        series
            .entry(path.parent().unwrap_or(&root.dir).to_path_buf())
            .or_default()
            .push((path.clone(), mtime));
    }
    let mut excess: HashSet<PathBuf> = HashSet::new();
    for (_folder, mut folder_series) in series {
        folder_series.sort_by_key(|(_, mtime)| std::cmp::Reverse(*mtime));
        for (path, _) in folder_series.into_iter().skip(retain_count) {
            excess.insert(path);
        }
    }

    let mut artifacts = Vec::new();
    for (path, role) in files {
        let group = group_dir_of(root, &path, &mut publishers);
        if root.needs_save_evidence && !proven.contains(&group) {
            continue;
        }
        let Ok(size) = std::fs::metadata(&path).map(|meta| meta.len()) else {
            continue;
        };
        if size == 0 {
            continue;
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        // The group as the app should show it: relative to the root when the
        // root holds several games, and the root's own name when it is one
        // game's folder. Both resolve back to `group` as a real directory,
        // which is what lets the tree draw one header per game.
        let group_dir = group
            .strip_prefix(&root.dir)
            .ok()
            .map(|relative| relative.to_string_lossy().to_string())
            .filter(|relative| !relative.is_empty())
            .or_else(|| {
                root.dir
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
            });
        // The title is the last segment of the group - the game's own folder,
        // not the studio's.
        let title = group
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let description = match role {
            SaveFileRole::Save if excess.contains(&path) => {
                format!("Autosave beyond the {retain_count} most recent in {title} ({name})")
            }
            SaveFileRole::Save => format!("Save file in {title} ({name})"),
            SaveFileRole::Config => format!("Settings file in {title} ({name})"),
        };
        artifacts.push(JanitorArtifact {
            path,
            category: Category::SaveBloat,
            size_bytes: size,
            description,
            // Never pre-ticked, whatever it is: only the player knows
            // which of their saves is the one that matters.
            is_safe_default: false,
            requires_backup: true,
            app_id: None,
            game_title: Some(title),
            group_dir,
        });
    }

    artifacts
}

/// Zero-Data-Loss Shield: Creates a ZIP backup of save files before deletion.
pub fn create_save_backup_zip(
    save_files: &[PathBuf],
    target_backup_dir: &Path,
    game_prefix: &str,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(target_backup_dir)?;

    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let safe_prefix = game_prefix.replace([' ', ':', '\\', '/', '<', '>', '"', '|', '?', '*'], "_");
    let zip_filename = format!("save_backup_{safe_prefix}_{timestamp}.zip");
    let zip_path = target_backup_dir.join(zip_filename);

    let file = File::create(&zip_path)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for save_path in save_files {
        if save_path.is_file() {
            let fname = save_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("save.dat");
            zip.start_file(fname, options)?;
            let mut f = File::open(save_path)?;
            let mut buffer = Vec::new();
            f.read_to_end(&mut buffer)?;
            zip.write_all(&buffer)?;
        }
    }

    zip.finish()?;
    Ok(zip_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_classify_saves() {
        assert_eq!(classify_save_file("autosave1.ess"), SaveKind::AutoOrQuick);
        assert_eq!(classify_save_file("quicksave.ess"), SaveKind::AutoOrQuick);
        assert_eq!(
            classify_save_file("AutoSave_001.sav"),
            SaveKind::AutoOrQuick
        );
        assert_eq!(classify_save_file("Save_001.ess"), SaveKind::Manual);
        assert_eq!(
            classify_save_file("Character_Hardcore_Ending.sav"),
            SaveKind::Manual
        );
    }

    /// A root shaped like `My Games`: one segment below it names the game.
    fn test_root(dir: &Path, needs_save_evidence: bool) -> SaveRoot {
        SaveRoot {
            dir: dir.to_path_buf(),
            group_depth: 1,
            max_depth: MAX_SAVE_DEPTH,
            needs_save_evidence,
        }
    }

    #[test]
    fn a_save_folder_is_listed_whole_and_only_excess_autosaves_are_called_excess() {
        // The listing has to work for a game nobody wrote a definition for,
        // which is the whole point of scanning the folder instead of a table
        // of nine titles.
        let temp = tempdir().expect("tempdir");
        let game = temp.path().join("Some Unlisted RPG");
        std::fs::create_dir_all(&game).expect("create game folder");

        // Three autosaves, written oldest-first so their mtimes order.
        for name in ["autosave1.sav", "autosave2.sav", "autosave3.sav"] {
            std::fs::write(game.join(name), b"x").expect("write autosave");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::fs::write(game.join("MyHardcoreRun.sav"), b"x").expect("write manual save");
        std::fs::write(game.join("settings.ini"), b"x").expect("write settings");
        std::fs::write(game.join("readme.txt"), b"x").expect("write unrelated file");

        let cancel = AtomicBool::new(false);
        let artifacts = scan_save_root(&test_root(temp.path(), false), 2, &cancel);
        let named: Vec<&str> = artifacts
            .iter()
            .filter_map(|artifact| artifact.path.file_name().and_then(|name| name.to_str()))
            .collect();

        assert!(
            !named.contains(&"readme.txt"),
            "only saves and settings are listed"
        );
        for expected in [
            "autosave1.sav",
            "autosave2.sav",
            "autosave3.sav",
            "MyHardcoreRun.sav",
            "settings.ini",
        ] {
            assert!(
                named.contains(&expected),
                "{expected} is missing from {named:?}"
            );
        }

        // Retention: with two kept, exactly the oldest autosave is excess -
        // and a manual save is never excess, whatever its age.
        let excess: Vec<&str> = artifacts
            .iter()
            .filter(|artifact| artifact.description.contains("beyond the"))
            .filter_map(|artifact| artifact.path.file_name().and_then(|name| name.to_str()))
            .collect();
        assert_eq!(excess, vec!["autosave1.sav"]);

        // Nothing here is ever pre-selected, and everything is backed up first.
        assert!(artifacts.iter().all(|artifact| !artifact.is_safe_default));
        assert!(artifacts.iter().all(|artifact| artifact.requires_backup));
    }

    #[test]
    fn every_listed_file_names_the_game_folder_it_belongs_to() {
        // Nested exactly like `LocalLow\<company>\<game>\...`, so the group is
        // two segments down and the files sit below that again.
        let temp = tempdir().expect("tempdir");
        let game = temp.path().join("Fumi Games").join("MOUSE").join("Save");
        std::fs::create_dir_all(&game).expect("create game folder");
        std::fs::write(game.join("save0.rsf"), b"x").expect("write save");

        let root = SaveRoot {
            dir: temp.path().to_path_buf(),
            group_depth: 2,
            max_depth: MAX_SAVE_DEPTH,
            needs_save_evidence: true,
        };
        let cancel = AtomicBool::new(false);
        let artifacts = scan_save_root(&root, 2, &cancel);

        assert_eq!(artifacts.len(), 1);
        assert_eq!(
            artifacts[0].group_dir.as_deref(),
            Some(format!("Fumi Games{}MOUSE", std::path::MAIN_SEPARATOR).as_str()),
            "the group is the game's folder, not the file's own"
        );
        assert_eq!(artifacts[0].game_title.as_deref(), Some("MOUSE"));
    }

    #[test]
    fn a_folder_with_no_save_in_it_is_not_offered_as_game_data() {
        // `AppData\LocalLow\Yandex\Updater\punto` - five `.xml` files with the
        // same extensions a game's settings have, and nothing playing.
        let temp = tempdir().expect("tempdir");
        let updater = temp.path().join("Yandex").join("Updater");
        std::fs::create_dir_all(&updater).expect("create updater folder");
        for name in ["appinfo.xml", "statistics.xml", "updinfo.xml"] {
            std::fs::write(updater.join(name), b"x").expect("write config");
        }
        let game = temp.path().join("Some Studio").join("Some Game");
        std::fs::create_dir_all(&game).expect("create game folder");
        std::fs::write(game.join("save0.dat"), b"x").expect("write save");
        std::fs::write(game.join("settings.xml"), b"x").expect("write settings");

        let root = SaveRoot {
            dir: temp.path().to_path_buf(),
            group_depth: 2,
            max_depth: MAX_SAVE_DEPTH,
            needs_save_evidence: true,
        };
        let cancel = AtomicBool::new(false);
        let listed: Vec<String> = scan_save_root(&root, 2, &cancel)
            .iter()
            .map(|artifact| artifact.path.file_name().unwrap().to_string_lossy().into())
            .collect();

        assert!(
            !listed.iter().any(|name| name.ends_with("info.xml")),
            "an application's own settings are not saves: {listed:?}"
        );
        // The settings *beside a save* still count - that folder proved itself.
        assert!(listed.iter().any(|name| name == "save0.dat"));
        assert!(listed.iter().any(|name| name == "settings.xml"));
    }

    #[test]
    fn a_publisher_folder_is_descended_past_but_a_game_folder_is_not() {
        // `Saved Games` mixes both shapes, so one rule has to tell them apart.
        let temp = tempdir().expect("tempdir");
        // A publisher: two games below it, nothing of its own.
        for game in ["DOOM", "Rage 2"] {
            let dir = temp.path().join("id Software").join(game);
            std::fs::create_dir_all(&dir).expect("create game folder");
            std::fs::write(dir.join("save0.sav"), b"x").expect("write save");
        }
        // A game whose saves sit in a folder of the usual name.
        let structural = temp.path().join("KingdomCome").join("saves");
        std::fs::create_dir_all(&structural).expect("create saves folder");
        std::fs::write(structural.join("save0.whs"), b"x").expect("write save");
        // A game that keeps one folder per campaign, named by id.
        let campaign = temp
            .path()
            .join("The Outer Worlds")
            .join("2C04DB6E4B3ED103995AECB00C3FA687");
        std::fs::create_dir_all(&campaign).expect("create campaign folder");
        std::fs::write(campaign.join("save0.sav"), b"x").expect("write save");

        let cancel = AtomicBool::new(false);
        let groups: HashSet<String> = scan_save_root(&test_root(temp.path(), false), 2, &cancel)
            .iter()
            .filter_map(|artifact| artifact.group_dir.clone())
            .collect();

        let sep = std::path::MAIN_SEPARATOR;
        assert!(groups.contains(&format!("id Software{sep}DOOM")));
        assert!(groups.contains(&format!("id Software{sep}Rage 2")));
        assert!(
            groups.contains("KingdomCome"),
            "a game's own `saves` folder is not a game of its own: {groups:?}"
        );
        assert!(
            groups.contains("The Outer Worlds"),
            "one heading per campaign id is the flat list again: {groups:?}"
        );
    }

    #[test]
    fn scan_save_root_returns_immediately_when_cancelled() {
        let temp = tempdir().expect("tempdir");
        let game = temp.path().join("Some Unlisted RPG");
        std::fs::create_dir_all(&game).expect("create game folder");
        std::fs::write(game.join("autosave1.sav"), b"x").expect("write autosave");

        let cancel = AtomicBool::new(true);
        let artifacts = scan_save_root(&test_root(temp.path(), false), 2, &cancel);

        assert!(artifacts.is_empty());
    }

    #[test]
    fn scan_save_locations_returns_immediately_when_cancelled() {
        let cancel = AtomicBool::new(true);
        let artifacts = scan_save_locations(2, &cancel);
        assert!(artifacts.is_empty());
    }

    #[test]
    fn test_save_backup_zip() {
        let temp = tempdir().unwrap();
        let save1 = temp.path().join("autosave1.ess");
        let save2 = temp.path().join("quicksave.ess");
        std::fs::write(&save1, b"SAVE_DATA_1").unwrap();
        std::fs::write(&save2, b"SAVE_DATA_2").unwrap();

        let backup_dir = temp.path().join("backups");
        let zip_path = create_save_backup_zip(&[save1, save2], &backup_dir, "Skyrim_SE").unwrap();

        assert!(zip_path.exists());
        assert!(zip_path.metadata().unwrap().len() > 0);
    }
}
