//! UI-side data model: findings grouped for the tree view, plus selection
//! and formatting helpers. Nothing here touches the database directly.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf, Prefix};

use gametrimmer_core::langdetect::LangKind;
use gametrimmer_core::orphans::OrphanKind;
use gametrimmer_core::rules::Category;
use gametrimmer_core::settings::SelectionProfile;

/// Granular source of a finding: a rules-engine category (redist, docs,
/// bonus, ...), a localization-detector kind (audio, text, video, font,
/// unknown), or an orphaned-residue kind (orphan-residue safety: a folder inside a launcher's
/// managed area with no live game behind it, or the launcher's own
/// download/cache scratch folder). All variants wrap public `core` types
/// unchanged. Kept on every row so the persistence key and the file's original
/// rule/detector/orphan provenance survive the coarser [`DisplayCategory`]
/// grouping used by the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FindingSource {
    Rule(Category),
    Loc(LangKind),
    /// Orphaned launcher residue (see `gametrimmer_core::orphans`). Has no
    /// game behind it by definition, so its rows carry the synthetic
    /// [`ORPHAN_GAME_ID`] and are grouped into a per-disk pseudo-game branch
    /// rather than nested under a real game.
    Orphan(OrphanKind),
}

/// Top-level category shown in the tree, merged from the granular
/// rule/localization sources in [`FindingSource`] (see [`display_category`]
/// for the mapping).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayCategory {
    Redist,
    Intro,
    Docs,
    Bonus,
    Loc,
    Archives,
    /// Build and OS residue a shipped game has no use for: PDB symbol files,
    /// `Thumbs.db`, `desktop.ini`, test executables. It used to be called
    /// `Other`, which was never true - [`Category::DevLeftovers`] was the
    /// only source that ever landed here, so "other" named a bucket with
    /// exactly one thing in it and hid what that thing was.
    DevLeftovers,
    /// Orphaned launcher residue (orphan-residue safety) - shown under the per-disk pseudo-game
    /// branch ([`ORPHAN_GAME_ID`]), never mixed into a real game's categories.
    Orphan,
    Workshop,
    ShaderCache,
    Crashes,
    Saves,
    LauncherCache,
}

/// Which game library a finding came from: the launcher that owns the library
/// and the library's root directory.
///
/// Presentation metadata only - it exists so the tree can be grouped by
/// launcher or by library instead of only by disk. Nothing in the deletion
/// path may consult it: there `file_safety` stays the single source of truth
/// for what a row is allowed to touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryOrigin {
    /// The launcher key as stored in `game_libraries.vendor` ("steam",
    /// "epic", "manual", ...). `None` when the root is known but no
    /// `game_libraries` row backs it - possible for orphaned residue, which is
    /// attributed by path rather than through a game.
    pub vendor: Option<String>,
    /// The library root directory (`game_libraries.path`).
    pub root: PathBuf,
}

/// One classified file, as produced by the scan worker.
#[derive(Debug, Clone)]
pub struct FindingRow {
    pub file_id: i64,
    /// The owning game's id, or the [`ORPHAN_GAME_ID`] sentinel for orphaned
    /// residue (orphan-residue safety), which has no game behind it.
    pub game_id: i64,
    /// The owning game's display name. Empty for orphan rows - the tree renders
    /// the orphan branch with a localized label keyed off [`ORPHAN_GAME_ID`]
    /// instead (see `ui::tree_view`).
    pub game_name: String,
    /// The owning game's vendor id (`games.app_id`) - a Steam appid, a GOG
    /// product id, ... `None` for a game no launcher gave one (a folder-scan
    /// or manual library) and for orphaned residue.
    ///
    /// Carried on the row purely so the tree's "never touch this" action can
    /// bind a personal exception to the game (see
    /// `gametrimmer_core::rules::Rule::app_id`): `game_id` cannot do it, being
    /// a per-scan row id that changes with every generation, while the vendor
    /// id is the one identifier that survives a rescan.
    pub app_id: Option<String>,
    pub install_dir: PathBuf,
    pub rel_path: String,
    /// Logical size (bytes) - shown as a secondary figure (tooltip) since it
    /// isn't what deleting actually reclaims.
    pub size: u64,
    /// On-disk allocated size (bytes) - the honest "space freed" figure and the
    /// one shown as primary and summed for totals/estimates (allocated-size accounting). Falls
    /// back to `size` for rows loaded from a database created before allocated-size
    /// accounting was added (see
    /// `worker::load`).
    pub size_on_disk: u64,
    pub source: FindingSource,
    /// For rule findings: the rule's `desc`. For localization findings: the
    /// detector's `reason`. Persisted as-is into `findings.rule_id`.
    pub rule_desc: String,
    pub confidence: u8,
    /// Set only for localization findings; the normalized language key
    /// (e.g. "es", "pt-br"). Persisted into `findings.lang_tag`.
    pub lang_tag: Option<String>,
    /// `\`-separated path (relative to the game root) of the shallowest
    /// ancestor directory in which every file is flagged as non-essential -
    /// letting the tree collapse that folder into a single node instead of
    /// scattering its files across categories. `None` when no such ancestor
    /// exists (an "orphan" finding, shown as its own row). Computed by
    /// `worker::scan::assign_group_dirs`, persisted by
    /// `worker::scan::persistence` into `findings.group_dir`, and read back
    /// by `worker::load` - so a reload shows the same tree shape as the
    /// scan that produced it without recomputing it from the full file
    /// list. (This doc previously claimed it was never persisted, which
    /// stopped being true when the column was added in schema v1.)
    pub group_dir: Option<String>,
    /// Present when this row is read-only. The same core preflight enforces
    /// this at execution time; the UI uses it only to explain and disable the
    /// selection affordance early.
    pub deletion_block_reason: Option<String>,
    /// Imported community rules are visible and manually selectable, but no
    /// profile may preselect them until the user has reviewed the finding.
    pub imported_untrusted: bool,
    /// The library this row came from (see [`LibraryOrigin`]). `None` when the
    /// row cannot be attributed to one - a row from a database written before
    /// the attribution existed, or orphaned residue whose recorded library root
    /// no longer resolves. Both the fresh-scan path and the load path fill it
    /// from the same evidence, so the two must never disagree for the same
    /// data.
    ///
    /// Read by the tree's launcher and library grouping axes (see
    /// [`GroupAxis`]) and by nothing else - in particular by nothing on the
    /// deletion path, per [`LibraryOrigin`]'s own boundary.
    pub library: Option<LibraryOrigin>,
    /// The action to be taken for this finding (DirectDelete, SparseZero, Repack).
    pub action: gametrimmer_core::models::FindingAction,
    /// Whether the owning game has active anti-cheat protection.
    pub anti_cheat_protected: bool,
    /// Precomputed monolithic container badge text.
    pub monolith_badge: Option<String>,
}

impl FindingRow {
    /// The coarse category this row is grouped under in the tree.
    pub fn display_category(&self) -> DisplayCategory {
        display_category(self.source)
    }

    /// Whether this finding targets localized streams inside a monolithic container.
    pub fn is_monolithic_archive(&self) -> bool {
        self.action.is_monolithic_archive()
    }

    /// The GUI deletion worker currently executes only whole-file deletes.
    /// Archive actions remain visible for audit/review, but cannot be queued
    /// until their rollback-capable executor is available.
    pub fn action_is_executable_by_gui(&self) -> bool {
        matches!(
            self.action,
            gametrimmer_core::models::FindingAction::DirectDelete
        )
    }

    /// Whether the user may select this individual finding for a deletion
    /// request. This is deliberately narrower than [`Self::bulk_selectable`]:
    /// imported rows still require an explicit individual decision, but a
    /// protected monolithic archive may never be selected through either the
    /// mouse or keyboard.
    pub fn individually_selectable(&self) -> bool {
        self.deletion_block_reason.is_none()
            && self.action_is_executable_by_gui()
            && !(self.anti_cheat_protected && self.is_monolithic_archive())
    }

    /// A row may be taken by *bulk* selection (select-all, profile
    /// auto-select) only when nothing blocks its deletion and its safety
    /// evidence came from this scan. `imported_untrusted` rows carry evidence
    /// an older database supplied and this scan never re-checked, so they
    /// must be ticked one at a time, deliberately.
    ///
    /// Anti-cheat protection no longer has a clause of its own here: the
    /// owner narrowed "risky" from every byte-rewriting finding (a
    /// monolithic archive edit *or* an intro's micro-stub replacement) down
    /// to monolithic archives only, and a protected monolithic archive is
    /// already hard-blocked by [`Self::individually_selectable`] - which this
    /// method calls first. Re-excluding it here would just repeat that
    /// block. `anti_cheat_protected` is a per-*game* verdict, so in a
    /// protected game it is true on every one of that game's rows; the wider
    /// exclusion this used to carry (any byte-rewriting row, including an
    /// ordinary intro finding) took Select All and every group header dark
    /// for the game the moment any part of it tripped an anti-cheat
    /// detector, measured on the owner's library as 100% hand-ticking across
    /// 112k+ findings in 162 games, for content (redist, docs, unused
    /// language packs, intro videos) that is trivially safe to bulk-delete.
    /// A whole-file delete or an intro's stub swap in a protected game is
    /// exactly what this program did before anti-cheat detection existed,
    /// and the worst case is the launcher's own integrity check
    /// re-downloading it, not a ban.
    pub fn bulk_selectable(&self) -> bool {
        self.individually_selectable() && !self.imported_untrusted
    }
}

/// A [`FindingRow`] plus UI-only state. Kept in a flat `Vec` so tree nodes
/// can reference items by index instead of duplicating data.
#[derive(Debug, Clone)]
pub struct FindingItem {
    pub row: FindingRow,
    pub selected: bool,
    /// Set once the file has been successfully moved to the Recycle Bin;
    /// removed items are filtered out of the tree but kept around so the
    /// selection/index model stays stable.
    pub removed: bool,
}

/// Fixed display order for the top-level categories in the tree. `Orphan`
/// lives last: it only ever appears under the synthetic orphan branch (see
/// [`ORPHAN_GAME_ID`]), never inside a real game, so its position relative to
/// the other six is immaterial - but it must still be listed so the settings
/// dialog offers a checkbox for it and [`category_enabled`] can gate it.
pub const CATEGORY_ORDER: [DisplayCategory; 13] = [
    DisplayCategory::Redist,
    // Right behind redist on purpose: a PDB or a Thumbs.db is the second
    // easiest thing in a game folder to be certain about, and certainty is
    // what earns a place near the top of the list.
    DisplayCategory::DevLeftovers,
    DisplayCategory::Intro,
    DisplayCategory::Docs,
    DisplayCategory::Bonus,
    DisplayCategory::Loc,
    DisplayCategory::Archives,
    DisplayCategory::Orphan,
    DisplayCategory::Workshop,
    DisplayCategory::ShaderCache,
    DisplayCategory::Crashes,
    DisplayCategory::Saves,
    DisplayCategory::LauncherCache,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum SafetyBadge {
    Safe,
    Review,
    BackupShield,
}

impl DisplayCategory {
    #[allow(dead_code)]
    pub fn safety_badge(self) -> SafetyBadge {
        match self {
            DisplayCategory::Redist
            | DisplayCategory::Docs
            | DisplayCategory::Crashes
            | DisplayCategory::ShaderCache
            | DisplayCategory::LauncherCache => SafetyBadge::Safe,
            DisplayCategory::Intro
            | DisplayCategory::Bonus
            | DisplayCategory::Loc
            | DisplayCategory::Archives
            | DisplayCategory::DevLeftovers
            | DisplayCategory::Orphan
            | DisplayCategory::Workshop => SafetyBadge::Review,
            DisplayCategory::Saves => SafetyBadge::BackupShield,
        }
    }
}

/// Synthetic `game_id` shared by every orphaned-residue finding (orphan-residue safety). Real
/// game ids are SQLite rowids (always `>= 1`), so a single reserved negative
/// sentinel can never collide with one. Because [`build_tree`] groups by
/// `(disk, game_id)`, giving every orphan on a disk the same sentinel merges
/// them all into exactly one "orphaned residue" pseudo-game node per disk,
/// beside real games in the dedicated orphan-residue branch.
/// The rows themselves are persisted with a `NULL` `files.game_id` (there is
/// no game), and reconstructed with this sentinel at scan/load time.
pub const ORPHAN_GAME_ID: i64 = i64::MIN;

/// Synthetic `game_id` for the single node [`GroupAxis::Flat`] hangs every
/// finding from. Reserved and negative for the same reason
/// [`ORPHAN_GAME_ID`] is - no SQLite rowid can collide with it - and distinct
/// from the other sentinels so the synthetic nodes are never mistaken for one
/// another. Never drawn: the flat axis folds the game level away.
pub const FLAT_GAME_ID: i64 = i64::MIN + 1;

/// Synthetic `game_id` for the findings that live outside every game and
/// every launcher container: Windows crash dumps, GPU shader caches, launcher
/// web caches, mod-manager downloads and save bloat (see
/// `worker::scan::janitor_pass`).
///
/// Separate from [`ORPHAN_GAME_ID`] because the two answer different
/// questions. Orphaned residue is what a launcher stopped managing - a game
/// that was uninstalled and left its folder. A shader cache or a save file is
/// nobody's residue: it belongs to a game that is installed and being played,
/// it just does not live under the game's directory. Filing them together put
/// a crash dump under a heading that read "Orphaned residue", which is not
/// what the user is looking at.
pub const SYSTEM_GAME_ID: i64 = i64::MIN + 2;

/// Whether `game_id` is the orphan-branch sentinel (see [`ORPHAN_GAME_ID`]) -
/// the tree renders such a node with a localized branch label instead of a
/// quoted game name.
pub fn is_orphan_branch(game_id: i64) -> bool {
    game_id == ORPHAN_GAME_ID
}

/// Whether `game_id` is the system-branch sentinel (see [`SYSTEM_GAME_ID`]).
pub fn is_system_branch(game_id: i64) -> bool {
    game_id == SYSTEM_GAME_ID
}

/// Whether `game_id` stands for a drawn pseudo-game branch rather than a real
/// game: the orphan branch or the system branch.
///
/// The tree asks this wherever "there is no real game behind this node" is the
/// actual question - no quoted title to render, no single install directory to
/// open. Asking it once, by name, is what kept the second sentinel from having
/// to be threaded through six separate `is_orphan_branch` tests that each
/// meant this instead.
pub fn is_pseudo_branch(game_id: i64) -> bool {
    is_orphan_branch(game_id) || is_system_branch(game_id)
}

/// Which synthetic branch a finding with no game row belongs under: launcher
/// residue in the orphan branch ([`ORPHAN_GAME_ID`]), everything the janitor
/// found outside the games in the system branch ([`SYSTEM_GAME_ID`]).
///
/// Derived from the finding's own source rather than stored, so the scan that
/// writes the row and the load that reads it back cannot disagree - both call
/// this, on the same value.
pub fn rootless_branch_id(source: FindingSource) -> i64 {
    match source {
        FindingSource::Orphan(_) => ORPHAN_GAME_ID,
        FindingSource::Rule(_) | FindingSource::Loc(_) => SYSTEM_GAME_ID,
    }
}

/// Whether `game_id` stands for a real game rather than one of the three
/// synthetic nodes ([`ORPHAN_GAME_ID`], [`SYSTEM_GAME_ID`], [`FLAT_GAME_ID`]).
///
/// Real ids are SQLite rowids and therefore always `>= 1`, so this is one test
/// rather than a list of sentinels that a third one could silently fall off.
/// Used where a lookup is keyed by game id and would quietly answer "no" for a
/// synthetic node - see `ui::tree_view::game_matches_search`.
pub fn is_real_game(game_id: i64) -> bool {
    game_id >= 1
}

/// Default confidence for an [`OrphanKind::UnmanagedFolder`] finding: a folder
/// in the launcher's managed area with no matching manifest. Deliberately
/// below [`AUTO_SELECT_CONFIDENCE_THRESHOLD`] so orphaned residue is shown but
/// never auto-selected - a game installed *past* the launcher (portable,
/// repack, manual copy) would otherwise be a false positive the user must
/// opt into deleting, not have pre-checked.
pub const ORPHAN_UNMANAGED_CONFIDENCE: u8 = 60;

/// Default confidence for an [`OrphanKind::ServiceFolder`] finding (e.g.
/// `steamapps/downloading` - aborted/partial depot downloads). Safer than an
/// unmanaged folder (it is pure scratch space the launcher itself treats as
/// disposable), but still kept below [`AUTO_SELECT_CONFIDENCE_THRESHOLD`] so
/// the whole orphan category stays out of the default selection.
pub const ORPHAN_SERVICE_CONFIDENCE: u8 = 80;

/// Default confidence for an [`OrphanKind::UnreferencedFile`] finding (e.g.
/// Steam's `depotcache/*.manifest` - GT-23): a single file proven
/// unreferenced by an explicit per-item cross-check (every installed app's
/// own declared dependencies), stronger evidence than the plain
/// existence-only [`OrphanKind::ServiceFolder`] sweep. Still kept below
/// [`AUTO_SELECT_CONFIDENCE_THRESHOLD`] - the whole orphan category stays out
/// of the default selection regardless of how the evidence for one kind was
/// gathered.
pub const ORPHAN_UNREFERENCED_FILE_CONFIDENCE: u8 = 75;

/// The confidence [`FindingSource::Orphan`] carries for a given kind.
pub fn orphan_confidence(kind: OrphanKind) -> u8 {
    match kind {
        OrphanKind::UnmanagedFolder => ORPHAN_UNMANAGED_CONFIDENCE,
        OrphanKind::ServiceFolder => ORPHAN_SERVICE_CONFIDENCE,
        OrphanKind::UnreferencedFile => ORPHAN_UNREFERENCED_FILE_CONFIDENCE,
    }
}

/// Splits an orphan folder's absolute path into the `(install_dir, rel_path)`
/// pair a [`FindingRow`] carries: the parent directory as `install_dir` (so
/// the tree groups the row by disk and rebuilds its full path via
/// `install_dir.join(rel_path)` exactly like a normal file row), and the
/// folder's own name as `rel_path`. A path with no parent (a bare drive root,
/// which the orphan scan never produces) degrades to an empty parent and the
/// whole path as the name - which still reconstructs to the original path.
///
/// Orphan rows have no game row to hold an `install_dir`, so persistence stores
/// the full path in `files.rel_path`; both the scan worker (when producing the
/// row fresh) and `worker::load` (when reading it back) run it through here so
/// the in-memory [`FindingRow`] shape is identical either way.
pub fn orphan_install_dir_and_name(full_path: &Path) -> (PathBuf, String) {
    let parent = full_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let name = full_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| full_path.to_string_lossy().into_owned());
    (parent, name)
}

/// The `(install_dir, rel_path, group_dir)` triple a rootless [`FindingRow`]
/// carries, given its absolute path and the folder it is grouped under.
///
/// Without a group this is [`orphan_install_dir_and_name`] - parent plus name,
/// the shape orphan rows have always had. With one, the split moves up to the
/// group's *parent*, so `install_dir.join(group_dir)` is the group's real
/// directory and `rel_path` starts with `group_dir` - the two things the tree
/// needs to draw one folder header per game and strip that prefix off the rows
/// beneath it (see `ui::tree_view::show_folder_row`).
///
/// The group is found by walking up from the file rather than trusted
/// positionally: it is stored as a relative path (`Fumi Games\MOUSE`), and the
/// deepest ancestor ending in it is the one meant. A group that does not
/// appear in the path at all is discarded rather than believed - the row then
/// simply renders ungrouped.
pub fn rootless_split(
    full_path: &Path,
    group_dir: Option<&str>,
) -> (PathBuf, String, Option<String>) {
    if let Some(group) = group_dir.filter(|group| !group.is_empty()) {
        let depth = Path::new(group).components().count();
        for ancestor in full_path.ancestors().skip(1) {
            if !ancestor.ends_with(group) {
                continue;
            }
            let Some(install_dir) = ancestor.ancestors().nth(depth) else {
                continue;
            };
            let Ok(rel_path) = full_path.strip_prefix(install_dir) else {
                continue;
            };
            return (
                install_dir.to_path_buf(),
                rel_path.to_string_lossy().into_owned(),
                Some(group.to_string()),
            );
        }
    }
    let (install_dir, name) = orphan_install_dir_and_name(full_path);
    (install_dir, name, None)
}

/// Maps a granular finding source onto its top-level display category.
pub fn display_category(source: FindingSource) -> DisplayCategory {
    match source {
        FindingSource::Rule(Category::RedistFolder) | FindingSource::Rule(Category::RedistFile) => {
            DisplayCategory::Redist
        }
        FindingSource::Rule(Category::DocsFolder) | FindingSource::Rule(Category::DocsFile) => {
            DisplayCategory::Docs
        }
        FindingSource::Rule(Category::Bonus) => DisplayCategory::Bonus,
        FindingSource::Rule(Category::MonolithicArchive) => DisplayCategory::Archives,
        FindingSource::Rule(Category::DevLeftovers) => DisplayCategory::DevLeftovers,
        FindingSource::Rule(Category::Intro) => DisplayCategory::Intro,
        FindingSource::Rule(Category::WorkshopOrphan) => DisplayCategory::Workshop,
        FindingSource::Rule(Category::DownloadingStaging) => DisplayCategory::Orphan,
        FindingSource::Rule(Category::ShaderCache) => DisplayCategory::ShaderCache,
        FindingSource::Rule(Category::CrashDump)
        | FindingSource::Rule(Category::DiagnosticLogs) => DisplayCategory::Crashes,
        FindingSource::Rule(Category::SaveBloat) => DisplayCategory::Saves,
        FindingSource::Rule(Category::LauncherWebCache)
        | FindingSource::Rule(Category::ModManagerDownloads) => DisplayCategory::LauncherCache,
        FindingSource::Loc(_) => DisplayCategory::Loc,
        FindingSource::Orphan(_) => DisplayCategory::Orphan,
    }
}

/// Human-readable, localized label for a category header.
pub fn category_display(lang: crate::i18n::Lang, category: DisplayCategory) -> &'static str {
    let s = crate::i18n::strings(lang);
    match category {
        DisplayCategory::Redist => s.category_redist,
        DisplayCategory::Intro => s.category_intro,
        DisplayCategory::Docs => s.category_docs,
        DisplayCategory::Bonus => s.category_bonus,
        DisplayCategory::Loc => s.category_loc,
        DisplayCategory::Archives => s.category_archives,
        DisplayCategory::DevLeftovers => s.category_dev_leftovers,
        DisplayCategory::Orphan => s.category_orphan,
        DisplayCategory::Workshop => s.category_workshop,
        DisplayCategory::ShaderCache => s.category_shader_cache,
        DisplayCategory::Crashes => s.category_crashes,
        DisplayCategory::Saves => s.category_saves,
        DisplayCategory::LauncherCache => s.category_launcher_cache,
    }
}

/// Stable string key used when persisting a finding's granular source into
/// `findings.category`. Mirrors the `category` values used in `rules.json`
/// for rule findings, and the `loc_*` scheme used for localization findings.
/// Unaffected by the 5-way display grouping: the DB always keeps the
/// granular value.
pub fn source_key(source: FindingSource) -> &'static str {
    match source {
        FindingSource::Rule(Category::RedistFolder) => "redist_folder",
        FindingSource::Rule(Category::RedistFile) => "redist_file",
        FindingSource::Rule(Category::DocsFolder) => "docs_folder",
        FindingSource::Rule(Category::DocsFile) => "docs_file",
        FindingSource::Rule(Category::Bonus) => "bonus",
        FindingSource::Rule(Category::MonolithicArchive) => "monolithic_archive",
        FindingSource::Rule(Category::DevLeftovers) => "dev_leftovers",
        FindingSource::Rule(Category::Intro) => "intro",
        FindingSource::Rule(Category::WorkshopOrphan) => "workshop_orphan",
        FindingSource::Rule(Category::DownloadingStaging) => "downloading_staging",
        FindingSource::Rule(Category::ShaderCache) => "shader_cache",
        FindingSource::Rule(Category::CrashDump) => "crash_dump",
        FindingSource::Rule(Category::DiagnosticLogs) => "diagnostic_logs",
        FindingSource::Rule(Category::SaveBloat) => "save_bloat",
        FindingSource::Rule(Category::LauncherWebCache) => "launcher_web_cache",
        FindingSource::Rule(Category::ModManagerDownloads) => "mod_manager_downloads",
        FindingSource::Loc(LangKind::Audio) => "loc_audio",
        FindingSource::Loc(LangKind::Text) => "loc_text",
        FindingSource::Loc(LangKind::Video) => "loc_video",
        FindingSource::Loc(LangKind::Font) => "loc_font",
        FindingSource::Loc(LangKind::Graphic) => "loc_graphic",
        FindingSource::Loc(LangKind::Unknown) => "loc_unknown",
        FindingSource::Orphan(OrphanKind::UnmanagedFolder) => "orphan_folder",
        FindingSource::Orphan(OrphanKind::ServiceFolder) => "orphan_service",
        FindingSource::Orphan(OrphanKind::UnreferencedFile) => "orphan_unreferenced_file",
    }
}

/// Inverse of [`source_key`]: reparses a `findings.category` value read back
/// from the database into its granular [`FindingSource`]. Returns `None` for
/// any string that isn't one of the keys `source_key` produces - e.g. a
/// category written by a future version of the app, or by `rules.json`
/// changes that predate a saved scan. Callers (see `worker::load`) must skip
/// such rows rather than fail the whole load, since the row is otherwise
/// perfectly readable.
pub fn parse_source_key(key: &str) -> Option<FindingSource> {
    match key {
        "redist_folder" => Some(FindingSource::Rule(Category::RedistFolder)),
        "redist_file" => Some(FindingSource::Rule(Category::RedistFile)),
        "docs_folder" => Some(FindingSource::Rule(Category::DocsFolder)),
        "docs_file" => Some(FindingSource::Rule(Category::DocsFile)),
        "bonus" => Some(FindingSource::Rule(Category::Bonus)),
        "monolithic_archive" => Some(FindingSource::Rule(Category::MonolithicArchive)),
        "dev_leftovers" => Some(FindingSource::Rule(Category::DevLeftovers)),
        "intro" => Some(FindingSource::Rule(Category::Intro)),
        "workshop_orphan" => Some(FindingSource::Rule(Category::WorkshopOrphan)),
        "downloading_staging" => Some(FindingSource::Rule(Category::DownloadingStaging)),
        "shader_cache" => Some(FindingSource::Rule(Category::ShaderCache)),
        "crash_dump" => Some(FindingSource::Rule(Category::CrashDump)),
        "diagnostic_logs" => Some(FindingSource::Rule(Category::DiagnosticLogs)),
        "save_bloat" => Some(FindingSource::Rule(Category::SaveBloat)),
        "launcher_web_cache" => Some(FindingSource::Rule(Category::LauncherWebCache)),
        "mod_manager_downloads" => Some(FindingSource::Rule(Category::ModManagerDownloads)),
        "loc_audio" => Some(FindingSource::Loc(LangKind::Audio)),
        "loc_text" => Some(FindingSource::Loc(LangKind::Text)),
        "loc_video" => Some(FindingSource::Loc(LangKind::Video)),
        "loc_font" => Some(FindingSource::Loc(LangKind::Font)),
        "loc_graphic" => Some(FindingSource::Loc(LangKind::Graphic)),
        "loc_unknown" => Some(FindingSource::Loc(LangKind::Unknown)),
        "orphan_folder" => Some(FindingSource::Orphan(OrphanKind::UnmanagedFolder)),
        "orphan_service" => Some(FindingSource::Orphan(OrphanKind::ServiceFolder)),
        "orphan_unreferenced_file" => Some(FindingSource::Orphan(OrphanKind::UnreferencedFile)),
        _ => None,
    }
}

/// Stable short key for a display category, used for egui persistent ids
/// (collapsing header open/closed state) instead of the Ukrainian label.
pub fn category_ui_key(category: DisplayCategory) -> &'static str {
    match category {
        DisplayCategory::Redist => "redist",
        DisplayCategory::Intro => "intro",
        DisplayCategory::Docs => "docs",
        DisplayCategory::Bonus => "bonus",
        DisplayCategory::Loc => "loc",
        DisplayCategory::Archives => "archives",
        DisplayCategory::DevLeftovers => "dev_leftovers",
        DisplayCategory::Orphan => "orphan",
        DisplayCategory::Workshop => "workshop",
        DisplayCategory::ShaderCache => "shader_cache",
        DisplayCategory::Crashes => "crashes",
        DisplayCategory::Saves => "saves",
        DisplayCategory::LauncherCache => "launcher_cache",
    }
}

/// Whether `category` should be kept by the scan, given the persisted
/// `enabled_categories` setting (see `gametrimmer_core::settings::Settings`).
/// An empty `enabled_categories` list means every category is enabled - see
/// that field's doc comment for why an empty list isn't "nothing enabled".
///
/// Matched through [`id_names_category`], so a list written under an older
/// key still selects the right categories.
pub fn category_enabled(enabled_categories: &[String], category: DisplayCategory) -> bool {
    enabled_categories.is_empty()
        || enabled_categories
            .iter()
            .any(|id| id_names_category(id, category))
}

/// Whether a stored `enabled_categories` id refers to `category` - by its
/// current [`category_ui_key`], or by the key it used to be stored under.
///
/// [`DisplayCategory::DevLeftovers`] is the one with a past: it was called
/// "other" until it got its own name. A settings file written back then
/// still says "other", and a rename that quietly dropped that id from the
/// list would turn every PDB and Thumbs.db invisible for exactly the users
/// who had gone to the trouble of picking their categories by hand.
pub fn id_names_category(id: &str, category: DisplayCategory) -> bool {
    id == category_ui_key(category) || (category == DisplayCategory::DevLeftovers && id == "other")
}

/// Default selection policy: auto-select only high-confidence
/// findings; lower-confidence ones are shown but left for the user to opt in.
/// This is the [`SelectionProfile::Custom`] policy - the confidence-only path.
pub const AUTO_SELECT_CONFIDENCE_THRESHOLD: u8 = 85;

pub fn default_selected(confidence: u8) -> bool {
    confidence >= AUTO_SELECT_CONFIDENCE_THRESHOLD
}

/// The confidence floor [`SelectionProfile::Aggressive`] adds on top of the
/// safe categories: any finding at or above it is pre-selected regardless of
/// its category. Set below [`AUTO_SELECT_CONFIDENCE_THRESHOLD`] on purpose -
/// "aggressive" reaches lower than the cautious default, pulling in the
/// mid-confidence rule findings (70-84) the `Custom` path leaves unchecked.
pub const AGGRESSIVE_CONFIDENCE_FLOOR: u8 = 70;

/// Whether a finding in `category` with `confidence` is pre-selected under
/// `profile` (selection profiles). A pure policy over already-scanned findings, so switching
/// profiles re-selects without re-scanning. Orthogonal to [`category_enabled`],
/// which decides what is scanned in the first place.
///
/// The "safe" categories - [`DisplayCategory::Bonus`], [`DisplayCategory::Docs`],
/// [`DisplayCategory::Orphan`] - are the residue a launcher will not restore on
/// its own. `Cautious` selects exactly those (at any confidence); `Balanced`
/// adds [`DisplayCategory::Loc`] (localization files only ever exist for
/// languages already outside the keep-list); `Aggressive` additionally selects
/// anything at or above [`AGGRESSIVE_CONFIDENCE_FLOOR`]; `Custom` defers to the
/// plain confidence threshold ([`default_selected`]).
///
/// Note (orphan-residue safety): unlike the confidence-threshold path, a profile *can*
/// pre-select orphaned residue - by the user's explicit choice of a profile
/// that includes the `Orphan` category. The orphan confidences deliberately
/// stay below [`AUTO_SELECT_CONFIDENCE_THRESHOLD`], so the `Custom` (and any
/// bare-confidence) path still never auto-selects them; the safety contract is
/// now scoped to that path rather than to every possible selection policy.
pub fn profile_auto_selects(
    profile: SelectionProfile,
    category: DisplayCategory,
    confidence: u8,
) -> bool {
    let is_safe_category = matches!(
        category,
        DisplayCategory::Bonus | DisplayCategory::Docs | DisplayCategory::Orphan
    );
    match profile {
        SelectionProfile::Cautious => is_safe_category,
        // Intro is deliberately absent here: unlike Bonus/Docs/Loc, a false
        // positive on an intro rule destroys a unique video with nothing to
        // re-download it from (see `RiskLevel::Low` on `DisplayCategory::Intro`
        // below), so the default profile leaves it for the user to opt into
        // rather than pre-checking it at any confidence.
        SelectionProfile::Balanced => is_safe_category || category == DisplayCategory::Loc,
        SelectionProfile::Aggressive => {
            is_safe_category
                || category == DisplayCategory::Loc
                || category == DisplayCategory::Intro
                || confidence >= AGGRESSIVE_CONFIDENCE_FLOOR
        }
        SelectionProfile::Custom => default_selected(confidence),
    }
}

/// Coarse deletion-risk band shown on a [`PlanCard`] (plan-action filtering). Deliberately a
/// small curated scale, *not* derived from a finding's raw `confidence`: the
/// action screen answers "how safe is it to sweep this whole category" in
/// human terms, which does not line up with per-file detector confidence (an
/// orphaned leftover carries low confidence yet is safe to remove - the game is
/// already gone). Ordered least-risky first so [`plan_cards`] can sort by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskLevel {
    /// Nothing of value is lost: the launcher already forgot it (orphaned
    /// residue) or it is trivially re-created (redistributable installers).
    None,
    /// Re-downloadable from the store on demand (bonus material, documentation,
    /// non-keep-list languages) - inconvenient to lose, never damaging.
    Low,
    /// Usually safe but less certain (developer leftovers): worth a look before
    /// a blanket sweep.
    Medium,
}

/// The curated deletion risk of a whole display category on the action screen
/// (plan-action filtering). See [`RiskLevel`] for why this is a hand-tuned table rather than a
/// function of confidence.
pub fn category_risk(category: DisplayCategory) -> RiskLevel {
    match category {
        // Orphaned residue: the game is already uninstalled. Redist: MSVC/DX
        // installers a game re-runs or the store re-fetches on demand.
        DisplayCategory::Orphan
        | DisplayCategory::Redist
        | DisplayCategory::ShaderCache
        | DisplayCategory::Crashes
        | DisplayCategory::LauncherCache => RiskLevel::None,
        // Intro: usually a safe micro-stub swap, but - unlike Redist/Orphan,
        // which lose nothing by construction - a false-positive match here
        // destroys a unique video with no upstream copy to re-fetch it from,
        // so it sits with the re-downloadable-but-inconvenient tier rather
        // than the zero-risk one.
        DisplayCategory::Bonus
        | DisplayCategory::Docs
        | DisplayCategory::Loc
        | DisplayCategory::Intro
        | DisplayCategory::Archives
        | DisplayCategory::Workshop => RiskLevel::Low,
        // Dev leftovers and saves: review / backup recommended
        DisplayCategory::DevLeftovers | DisplayCategory::Saves => RiskLevel::Medium,
    }
}

/// One aggregated action on the "plan of action" screen (plan-action filtering): a whole
/// display category rolled up across every disk and game, with the total space
/// it would reclaim and its curated risk band. Built by [`plan_cards`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanCard {
    pub category: DisplayCategory,
    /// Total on-disk allocation across the category's findings (allocated-size accounting) - the
    /// honest reclaimable figure, matching the tree and bottom-bar totals.
    pub total_size_on_disk: u64,
    /// How many findings the category holds.
    pub finding_count: usize,
    /// How many distinct games contribute (the orphan branch counts as one),
    /// so the card can say "unused languages in N games".
    pub game_count: usize,
    pub risk: RiskLevel,
}

/// Everything the plan row and bottom bar need from the findings collection.
/// Building these values together keeps those two per-frame surfaces to one
/// findings pass instead of independently walking the whole scan result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UiAggregates {
    pub cards: Vec<PlanCard>,
    pub totals: PlanTotals,
    pub selected_count: usize,
    pub selected_bytes_on_disk: u64,
}

/// Rolls the current findings up for both summary surfaces in one pass.
/// Removed items are excluded from every value.
pub fn ui_aggregates(items: &[FindingItem]) -> UiAggregates {
    use std::collections::HashSet;

    let mut by_category: HashMap<DisplayCategory, (u64, usize, HashSet<i64>)> = HashMap::new();
    let mut games = HashSet::new();
    let mut aggregates = UiAggregates::default();

    for item in items {
        if item.removed {
            continue;
        }

        aggregates.totals.finding_count += 1;
        games.insert(item.row.game_id);

        let entry = by_category.entry(item.row.display_category()).or_default();
        entry.0 = entry.0.saturating_add(item.row.size_on_disk);
        entry.1 += 1;
        entry.2.insert(item.row.game_id);

        if item.selected {
            aggregates.selected_count += 1;
            aggregates.selected_bytes_on_disk = aggregates
                .selected_bytes_on_disk
                .saturating_add(item.row.size_on_disk);
        }
    }

    aggregates.totals.game_count = games.len();
    aggregates.cards = by_category
        .into_iter()
        .map(|(category, (size, count, games))| PlanCard {
            category,
            total_size_on_disk: size,
            finding_count: count,
            game_count: games.len(),
            risk: category_risk(category),
        })
        .collect();
    aggregates.cards.sort_by(|a, b| {
        a.risk
            .cmp(&b.risk)
            .then(b.total_size_on_disk.cmp(&a.total_size_on_disk))
    });
    aggregates
}

/// Rolls the current findings up into one [`PlanCard`] per non-empty display
/// category, ordered "benefit ÷ risk": least-risky first, and within a risk
/// band the biggest reclaim first. Removed items are excluded (they are gone).
pub fn plan_cards(items: &[FindingItem]) -> Vec<PlanCard> {
    ui_aggregates(items).cards
}

/// The whole-plan roll-up behind the one-line summary above the tree
/// (plan summary). Built by [`plan_totals`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlanTotals {
    /// How many findings there are in total.
    pub finding_count: usize,
    /// How many distinct games contribute, counted across every category at
    /// once - a game that appears in three categories is one game here, which
    /// is why this cannot be the sum of the per-card counts.
    pub game_count: usize,
}

/// Rolls every finding up into one line's worth of numbers. Removed items are
/// excluded (they are gone), matching [`plan_cards`]. Pure, so it is cheap to
/// recompute each frame and unit-testable without any UI.
#[cfg(test)]
pub fn plan_totals(items: &[FindingItem]) -> PlanTotals {
    ui_aggregates(items).totals
}

/// One node in the tree, either a collapsed folder (every file under it is
/// flagged, see `worker::scan::assign_group_dirs`) or a single orphan file
/// with no collapsible ancestor. Always nested under a [`GameNode`], so it
/// carries no game identity of its own.
#[derive(Debug, Clone)]
pub enum TreeNode {
    Folder {
        /// `\`-separated path relative to the game root.
        group_dir: String,
        item_indices: Vec<usize>,
        total_bytes: u64,
    },
    File {
        index: usize,
    },
}

/// One display category's nodes within a game, in display order.
#[derive(Debug, Clone)]
pub struct CategoryNode {
    /// The display category every row under this node belongs to, or `None`
    /// when the level does not stand for a category at all.
    ///
    /// `None` is [`GroupAxis::Flat`], which puts every finding into one node
    /// so that an explicit sort can order the whole result set - a node list
    /// per category would sort each category separately, which is precisely
    /// the burial the flat axis exists to undo. That one node spans every
    /// category by construction, so there is no single value it could honestly
    /// carry, and its readers are made to say what they mean instead: the CSV
    /// export falls back to each finding's own category, and the row is never
    /// drawn (see `ui::tree_view`).
    pub category: Option<DisplayCategory>,
    pub nodes: Vec<TreeNode>,
    /// Concatenated `findings` indices of every node in `nodes` (a folder
    /// contributes its whole member list, a file its single index).
    /// Precomputed once in `build_tree` so the virtualized tree view's
    /// per-frame header rendering never has to re-walk `nodes` to collect
    /// this - see `ui::tree_view`.
    pub all_indices: Vec<usize>,
    /// Total bytes of `all_indices` - precomputed for the same reason.
    pub total_bytes: u64,
}

/// One game's categories, in display order. Every finding of a game lives
/// under exactly one such node, so the game never appears more than once
/// per disk in the tree.
#[derive(Debug, Clone)]
pub struct GameNode {
    pub game_id: i64,
    pub game_name: String,
    pub categories: Vec<CategoryNode>,
    /// Concatenated `findings` indices across every category. See
    /// `CategoryNode::all_indices` for why this is precomputed.
    pub all_indices: Vec<usize>,
    /// Total bytes of `all_indices` - precomputed for the same reason.
    pub total_bytes: u64,
}

/// What the tree's top level groups findings by.
///
/// The same findings, cut a different way - never a different set of findings.
/// Switching axes rebuilds the tree from the rows already in memory, so it
/// costs no rescan and can never surface or hide a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum GroupAxis {
    /// The physical disk the files sit on (the tree's original and default
    /// shape). Every row has one, so this axis never leaves anything
    /// unattributed.
    #[default]
    Disk,
    /// The launcher that owns the library the game came from (Steam, GOG, ...).
    Launcher,
    /// One specific library root, so two Steam libraries on two disks are two
    /// separate branches.
    Library,
    /// The display category, lifting every game's localizations (or bonus
    /// material, or redistributables) into one branch. The per-game category
    /// row folds away under this axis - it would only repeat the branch
    /// heading one indent in.
    Category,
    /// No grouping at all: every finding as its own row.
    ///
    /// This is what makes ordering by size mean what it says. In a
    /// branch -> game -> category -> folder hierarchy "the biggest files"
    /// cannot be asked for, because a large file stays buried under whichever
    /// folder node it belongs to and only its folder's total competes. The
    /// flat axis is the one cut where every finding is a row the sort can
    /// reach - so it also dissolves folders rather than merely hiding the
    /// headings above them.
    Flat,
}

/// Every axis, in the order the switcher offers them - the three that keep the
/// full hierarchy first, then the two that fold parts of it away.
pub const GROUP_AXIS_ORDER: [GroupAxis; 5] = [
    GroupAxis::Disk,
    GroupAxis::Launcher,
    GroupAxis::Library,
    GroupAxis::Category,
    GroupAxis::Flat,
];

/// Stable short key for an axis, used to namespace the tree's expand/collapse
/// state (see `ui::tree_view`) so the open/closed rows of one axis are not read
/// back as another's.
pub fn group_axis_key(axis: GroupAxis) -> &'static str {
    match axis {
        GroupAxis::Disk => "disk",
        GroupAxis::Launcher => "launcher",
        GroupAxis::Library => "library",
        GroupAxis::Category => "category",
        GroupAxis::Flat => "flat",
    }
}

/// Which top-level branch a finding belongs to under the active [`GroupAxis`].
///
/// Carries the axis inside every variant rather than beside the tree, so a key
/// built under one axis can never be compared against, or collapse-keyed as,
/// one built under another.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TopKey {
    /// [`GroupAxis::Disk`]: an uppercase drive label (`"E:"`) or a UNC root.
    Disk(String),
    /// [`GroupAxis::Launcher`]: the raw `game_libraries.vendor` tag
    /// (`"steam"`, `"manual"`, ...), rendered for display by
    /// `i18n::launcher_label`.
    Launcher(String),
    /// [`GroupAxis::Library`]: the library root directory.
    Library(PathBuf),
    /// [`GroupAxis::Category`]: the row's own display category.
    Category(DisplayCategory),
    /// [`GroupAxis::Flat`]: the single branch every finding hangs from.
    ///
    /// Never drawn, since the whole point of the axis is that the headings are
    /// gone. It exists so the tree has one shape under every axis and the
    /// export, the totals and the sort all keep working unchanged.
    Flat,
    /// The row carries nothing to group on under this axis - residue whose
    /// library root no longer resolves, or rows read back from a database
    /// written before the attribution existed (see [`FindingRow::library`]).
    ///
    /// A branch of its own rather than a silent omission: a tree that quietly
    /// dropped these would show fewer findings after a switch than before it,
    /// which reads as broken detection rather than as missing metadata.
    Unattributed(GroupAxis),
}

impl TopKey {
    /// The axis this key was built under.
    pub fn axis(&self) -> GroupAxis {
        match self {
            TopKey::Disk(_) => GroupAxis::Disk,
            TopKey::Launcher(_) => GroupAxis::Launcher,
            TopKey::Library(_) => GroupAxis::Library,
            TopKey::Category(_) => GroupAxis::Category,
            TopKey::Flat => GroupAxis::Flat,
            TopKey::Unattributed(axis) => *axis,
        }
    }

    /// The raw grouping value as text - what the branches are ordered by, and
    /// what the collapse key is built from.
    ///
    /// A category contributes its stable short key rather than its localized
    /// heading: the collapse state must not be reset by switching interface
    /// language. Empty for [`TopKey::Flat`] and [`TopKey::Unattributed`],
    /// which are one branch and a ranked-last branch respectively, so neither
    /// needs a name to be ordered by.
    pub fn value(&self) -> Cow<'_, str> {
        match self {
            TopKey::Disk(disk) => Cow::Borrowed(disk.as_str()),
            TopKey::Launcher(vendor) => Cow::Borrowed(vendor.as_str()),
            TopKey::Library(root) => root.to_string_lossy(),
            TopKey::Category(category) => Cow::Borrowed(category_ui_key(*category)),
            TopKey::Flat | TopKey::Unattributed(_) => Cow::Borrowed(""),
        }
    }

    /// Ordering band: attributed branches first, the unattributed one last.
    /// The leftovers belong at the bottom of the tree, not sorted into the
    /// middle of it by an empty name.
    fn rank(&self) -> u8 {
        u8::from(matches!(self, TopKey::Unattributed(_)))
    }

    /// Stable, axis-namespaced string identifying this branch - the base of
    /// every expand/collapse key beneath it (see `ui::tree_view`).
    pub fn collapse_key(&self) -> String {
        format!("{}:{}", group_axis_key(self.axis()), self.value())
    }
}

/// Orders two top-level branches by their own identity: unattributed last,
/// then by the branch's own name. The default tree order and the "Name"
/// column's order share this so a sort by name lands where the tree already
/// was rather than shuffling for a reason nothing on screen states.
///
/// Category branches order by [`CATEGORY_ORDER`] rather than by that name, for
/// the reason [`cmp_categories`] gives about the nested level: the taxonomy is
/// fixed and six entries long, and its order is the cleanup priority the
/// screen is built to communicate.
fn cmp_top_keys(a: &TopKey, b: &TopKey) -> std::cmp::Ordering {
    a.rank().cmp(&b.rank()).then_with(|| match (a, b) {
        (TopKey::Category(left), TopKey::Category(right)) => {
            category_rank(*left).cmp(&category_rank(*right))
        }
        _ => path_cmp(&a.value(), &b.value()),
    })
}

/// One top-level branch of the tree and its games, largest first. What the
/// branch stands for depends on the [`GroupAxis`] it was built under - see
/// [`TopKey`].
#[derive(Debug, Clone)]
pub struct TopGroup {
    pub key: TopKey,
    pub games: Vec<GameNode>,
    /// Concatenated `findings` indices across every game. See
    /// `CategoryNode::all_indices` for why this is precomputed.
    pub all_indices: Vec<usize>,
    /// Total bytes of `all_indices` - precomputed for the same reason.
    pub total_bytes: u64,
}

/// Uppercase drive-letter label (e.g. `"E:"`) for `install_dir`'s volume,
/// used to group games by physical disk in the tree. Non-drive roots (UNC
/// shares, verbatim device paths, ...) fall back to a label built from the
/// path's first component instead.
pub fn disk_label(install_dir: &Path) -> String {
    match install_dir.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                format!("{}:", (letter as char).to_ascii_uppercase())
            }
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                format!(
                    "\\\\{}\\{}",
                    server.to_string_lossy(),
                    share.to_string_lossy()
                )
            }
            _ => prefix.as_os_str().to_string_lossy().to_string(),
        },
        Some(Component::Normal(segment)) => segment.to_string_lossy().to_string(),
        _ => "?".to_string(),
    }
}

/// The display category holding the majority of `indices`' bytes. Ties are
/// resolved by `CATEGORY_ORDER` position (earliest wins), so a folder shared
/// across several granular sources always lands in exactly one category,
/// never split across several - this is the dedup guarantee the tree relies
/// on.
fn majority_category(items: &[FindingItem], indices: &[usize]) -> DisplayCategory {
    let mut bytes_by_category: HashMap<DisplayCategory, u64> = HashMap::new();
    for &index in indices {
        let row = &items[index].row;
        let total = bytes_by_category.entry(row.display_category()).or_insert(0);
        *total = total.saturating_add(row.size_on_disk);
    }

    // Only categories the group actually holds are candidates. Seeding the
    // winner with `CATEGORY_ORDER[0]` and unseating it on a strict `>` meant
    // an absent category won whenever nothing weighed more than nothing - and
    // a group can weigh nothing legitimately: a read-only monolithic archive
    // carries `size_on_disk == 0` because it frees nothing until in-place
    // trimming exists. A folder of those was filed under "Redistributables"
    // for no better reason than that being first in the list.
    //
    // Iterating `CATEGORY_ORDER` keeps the documented tie-break - earliest
    // wins - now genuinely among the categories present.
    CATEGORY_ORDER
        .iter()
        .filter_map(|category| {
            bytes_by_category
                .get(category)
                .map(|&bytes| (*category, bytes))
        })
        .reduce(|best, next| if next.1 > best.1 { next } else { best })
        .map(|(category, _)| category)
        .unwrap_or(CATEGORY_ORDER[0])
}

/// Total bytes represented by a tree node - a folder's precomputed total, or
/// a single file's size - used to sort nodes within a category. On-disk size,
/// to match the figure shown and summed everywhere else (allocated-size accounting).
fn node_bytes(items: &[FindingItem], node: &TreeNode) -> u64 {
    match node {
        TreeNode::Folder { total_bytes, .. } => *total_bytes,
        TreeNode::File { index } => items[*index].row.size_on_disk,
    }
}

/// All flat `findings` indices held under one node - a folder's whole
/// member list, or a single orphan file's index. Used to build
/// `CategoryNode::all_indices`/`TopGroup::all_indices` once in
/// `build_tree`.
fn node_all_indices(node: &TreeNode) -> Vec<usize> {
    match node {
        TreeNode::Folder { item_indices, .. } => item_indices.clone(),
        TreeNode::File { index } => vec![*index],
    }
}

/// Secondary, deterministic sort key for nodes whose byte totals tie -
/// otherwise their relative order would depend on hash-map iteration order.
fn node_sort_key(items: &[FindingItem], node: &TreeNode) -> String {
    match node {
        TreeNode::Folder { group_dir, .. } => group_dir.clone(),
        TreeNode::File { index } => items[*index].row.rel_path.clone(),
    }
}

/// Case-insensitive comparison with a case-sensitive tie-break. UI-only path
/// sorting (Windows paths, not full ICU collation) - lowercasing both sides
/// groups paths that only differ by case together, while the case-sensitive
/// second pass keeps the order fully deterministic instead of comparing two
/// differently-cased duplicates as equal.
fn path_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// Per-game accumulation used while building the tree: the game's display
/// name plus its findings split into folder-collapsible buckets and orphans.
struct GameBucket {
    game_name: String,
    /// group_dir -> indices, for folder-collapsible findings.
    folders: HashMap<String, Vec<usize>>,
    /// Findings with no collapsible ancestor.
    orphans: Vec<usize>,
}

/// How a game's findings are split into [`CategoryNode`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CategorySplit {
    /// One node per display category, in [`CATEGORY_ORDER`] - the tree's usual
    /// shape, where the category is a level of its own under each game.
    PerCategory,
    /// A single node holding everything, carrying the given category.
    ///
    /// [`GroupAxis::Category`] passes its branch's category: every row in the
    /// bucket already has it, so splitting again would only produce the one
    /// node the long way round. [`GroupAxis::Flat`] passes `None`, because its
    /// single node genuinely spans every category - see
    /// [`CategoryNode::category`].
    Whole(Option<DisplayCategory>),
}

/// Builds one game's category list (display order; within each category,
/// folders first - largest total first - then individual files by path).
fn build_game_categories(
    items: &[FindingItem],
    bucket: GameBucket,
    split: CategorySplit,
) -> Vec<CategoryNode> {
    let mut nodes_by_category: HashMap<Option<DisplayCategory>, Vec<TreeNode>> = HashMap::new();

    for (group_dir, mut indices) in bucket.folders {
        // Member files are collected in scan order (insertion order into the
        // group_dir bucket), which for the MFT scan path is not path order -
        // sort explicitly so the folder's children always read top-to-bottom
        // by path regardless of how the scan visited them.
        indices.sort_by(|&a, &b| path_cmp(&items[a].row.rel_path, &items[b].row.rel_path));
        let total_bytes = group_size_bytes(items, &indices);
        let category = match split {
            CategorySplit::PerCategory => Some(majority_category(items, &indices)),
            CategorySplit::Whole(category) => category,
        };
        nodes_by_category
            .entry(category)
            .or_default()
            .push(TreeNode::Folder {
                group_dir,
                item_indices: indices,
                total_bytes,
            });
    }
    for index in bucket.orphans {
        let category = match split {
            CategorySplit::PerCategory => Some(items[index].row.display_category()),
            CategorySplit::Whole(category) => category,
        };
        nodes_by_category
            .entry(category)
            .or_default()
            .push(TreeNode::File { index });
    }

    let order: Vec<Option<DisplayCategory>> = match split {
        CategorySplit::PerCategory => CATEGORY_ORDER.iter().copied().map(Some).collect(),
        CategorySplit::Whole(category) => vec![category],
    };

    order
        .into_iter()
        .filter_map(|category| {
            let nodes = nodes_by_category.remove(&category)?;
            // Folders first (largest total_bytes first, so the biggest
            // clean-up opportunities lead), then individual files by path -
            // deliberately not interleaved by size, per the tree's design:
            // top-level ordering communicates cleanup priority, file ordering
            // within that communicates "where is it".
            let (mut folders, mut files): (Vec<TreeNode>, Vec<TreeNode>) = nodes
                .into_iter()
                .partition(|node| matches!(node, TreeNode::Folder { .. }));
            folders.sort_by(|a, b| {
                node_bytes(items, b)
                    .cmp(&node_bytes(items, a))
                    .then_with(|| path_cmp(&node_sort_key(items, a), &node_sort_key(items, b)))
            });
            files.sort_by(|a, b| path_cmp(&node_sort_key(items, a), &node_sort_key(items, b)));
            let mut nodes = folders;
            nodes.append(&mut files);
            let all_indices: Vec<usize> = nodes.iter().flat_map(node_all_indices).collect();
            let total_bytes = group_size_bytes(items, &all_indices);
            Some(CategoryNode {
                category,
                nodes,
                all_indices,
                total_bytes,
            })
        })
        .collect()
}

/// Rebuilds the branch -> game -> category -> folder/file tree from scratch,
/// skipping removed items. Cheap enough to call after every scan/delete
/// completion, and after every change of grouping axis.
///
/// The shape is the same whatever the axis; only what the top level stands for
/// changes (see [`TopKey`]). Every game appears exactly once under its branch,
/// holding all of that branch's findings for it - the tree never scatters one
/// game's rows across a branch. Within a game, every flagged file with a
/// `group_dir` (see `worker::scan::assign_group_dirs`) is merged into one
/// `TreeNode::Folder` per `group_dir`, placed under the single display
/// category holding the majority of that folder's bytes
/// (`majority_category`) - this is what keeps a shared folder from appearing
/// in more than one category. Findings without a `group_dir` become standalone
/// `TreeNode::File` nodes in their own display category. Within a category,
/// folders precede individual files (see `build_game_categories`); within a
/// folder, member files are ordered by path (see `path_cmp`).
///
/// Two axes bend that shape rather than the levels above it:
/// [`GroupAxis::Category`] gives each branch one category node, because every
/// row in the branch already shares its category; [`GroupAxis::Flat`] gives the
/// whole tree one branch, one synthetic game ([`FLAT_GAME_ID`]) and one
/// category node of loose files, with folders dissolved. Both are still the
/// same four levels - the levels the axis has made redundant are folded away
/// when the tree is drawn, not when it is built, so the export, the totals and
/// the sort keep working unchanged.
///
/// Which top-level branch one row belongs to under `axis`.
///
/// Only the disk is derived from the file's own path; the launcher and the
/// library both read [`FindingRow::library`], which the scan and load paths
/// fill from the same `game_libraries` evidence. A row that carries no
/// attribution - or a root with no vendor behind it - lands in
/// [`TopKey::Unattributed`] rather than being dropped.
fn top_key_of(row: &FindingRow, axis: GroupAxis) -> TopKey {
    match axis {
        GroupAxis::Disk => TopKey::Disk(disk_label(&row.install_dir)),
        GroupAxis::Launcher => match row.library.as_ref().and_then(|lib| lib.vendor.as_ref()) {
            Some(vendor) => TopKey::Launcher(vendor.clone()),
            None => TopKey::Unattributed(axis),
        },
        GroupAxis::Library => match row.library.as_ref() {
            Some(lib) => TopKey::Library(lib.root.clone()),
            None => TopKey::Unattributed(axis),
        },
        GroupAxis::Category => TopKey::Category(row.display_category()),
        GroupAxis::Flat => TopKey::Flat,
    }
}

/// How one branch splits its games into [`CategoryNode`]s. See
/// [`CategorySplit`] for why two of the axes want a single node.
fn category_split_for(top: &TopKey) -> CategorySplit {
    match top {
        TopKey::Category(category) => CategorySplit::Whole(Some(*category)),
        TopKey::Flat => CategorySplit::Whole(None),
        _ => CategorySplit::PerCategory,
    }
}

pub fn build_tree(items: &[FindingItem], axis: GroupAxis) -> Vec<TopGroup> {
    let mut game_buckets: HashMap<(TopKey, i64), GameBucket> = HashMap::new();
    let flat = axis == GroupAxis::Flat;

    for (index, item) in items.iter().enumerate() {
        if item.removed {
            continue;
        }
        let top = top_key_of(&item.row, axis);
        // The flat axis puts every finding under one synthetic game. Keeping
        // the real ids here would leave one node list per game, and an
        // explicit sort orders within a node list - so "by size" would rank
        // each game's files separately and then rank the games, which is the
        // burial this axis exists to undo.
        let game_id = if flat { FLAT_GAME_ID } else { item.row.game_id };
        let bucket = game_buckets
            .entry((top, game_id))
            .or_insert_with(|| GameBucket {
                // The synthetic game is never drawn, so it is given no name
                // rather than the name of whichever finding happened to create
                // its bucket.
                game_name: if flat {
                    String::new()
                } else {
                    item.row.game_name.clone()
                },
                folders: HashMap::new(),
                orphans: Vec::new(),
            });
        match &item.row.group_dir {
            // Folders are dissolved under the flat axis, not merely
            // un-headed: a file collapsed into a folder node is not a row a
            // size sort can reach, and reaching them is the whole point.
            Some(dir) if !flat => bucket.folders.entry(dir.clone()).or_default().push(index),
            _ => bucket.orphans.push(index),
        }
    }

    let mut games_by_top: HashMap<TopKey, Vec<GameNode>> = HashMap::new();
    for ((top, game_id), bucket) in game_buckets {
        let game_name = bucket.game_name.clone();
        let categories = build_game_categories(items, bucket, category_split_for(&top));
        let all_indices: Vec<usize> = categories
            .iter()
            .flat_map(|category_node| category_node.all_indices.iter().copied())
            .collect();
        let total_bytes = group_size_bytes(items, &all_indices);
        games_by_top.entry(top).or_default().push(GameNode {
            game_id,
            game_name,
            categories,
            all_indices,
            total_bytes,
        });
    }

    let mut groups: Vec<TopGroup> = games_by_top
        .into_iter()
        .map(|(key, mut games)| {
            games.sort_by(|a, b| {
                b.total_bytes
                    .cmp(&a.total_bytes)
                    .then_with(|| a.game_name.cmp(&b.game_name))
            });
            let all_indices: Vec<usize> = games
                .iter()
                .flat_map(|game| game.all_indices.iter().copied())
                .collect();
            let total_bytes = group_size_bytes(items, &all_indices);
            TopGroup {
                key,
                games,
                all_indices,
                total_bytes,
            }
        })
        .collect();

    groups.sort_by(|a, b| cmp_top_keys(&a.key, &b.key));
    groups
}

/// Which column the findings tree is ordered by once the user has picked an
/// order of their own.
///
/// Not one variant per column drawn: the tree has four headings and only three
/// of them order it.
///
/// "Language" is missing because only file rows carry a language tag - every
/// disk, game, category and folder row leaves that cell blank. Ordering by it
/// would therefore leave four of the five levels arranged by name and reorder
/// the fifth, which is a heading that mostly does nothing and a promise the
/// screen cannot keep.
///
/// "Confidence" is missing because that column existed and was removed on
/// purpose (see `ui::tree_view::REVIEW_MARK_PX`): the percentage was the
/// detector's internal scale, and the single decision it drove is now the
/// review mark. A heading cannot be clicked into existence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    Name,
    Files,
    Size,
}

/// A user-chosen tree order: one column, one direction.
///
/// Carried as `Option<TreeSort>` everywhere, and `None` does not mean
/// "unsorted" - it means the tree's own designed order, which no combination
/// of column and direction reproduces: [`build_tree`] mixes disks by letter,
/// games by size, categories by [`CATEGORY_ORDER`], and folders before files
/// within a category. That order is how the screen communicates cleanup
/// priority, so it stays reachable rather than being a state the first click
/// destroys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeSort {
    pub column: SortColumn,
    pub descending: bool,
}

impl TreeSort {
    /// `primary` turned the way the active direction wants it.
    ///
    /// Only the primary key is flipped. Every comparison below falls back to a
    /// name tie-break, and that tie-break stays ascending in both directions so
    /// that reversing the sort never also scrambles the rows which are equal
    /// under the chosen column.
    fn directed(self, primary: std::cmp::Ordering) -> std::cmp::Ordering {
        if self.descending {
            primary.reverse()
        } else {
            primary
        }
    }
}

/// Reorders an already-built tree in place. A no-op for `None`, which is what
/// keeps [`build_tree`] the single definition of the default order: callers
/// rebuild and then call this, rather than this having to know how to undo
/// itself.
///
/// Applied at every level that has siblings to order - disks, games within a
/// disk, categories within a game, nodes within a category, and a folder's
/// member files. Levels whose rows leave the sorted column blank (no file row
/// shows a count) compare equal on it and fall through to the name tie-break,
/// so a sort never leaves part of the tree in an order nothing on screen
/// accounts for.
///
/// Under an active sort, folders and standalone files inside a category are
/// ordered together instead of folders-first. That partition is part of the
/// *default* order's meaning ("biggest cleanup opportunities lead, then where
/// things are"); keeping it under an explicit sort would make "by size" a lie
/// for precisely the case the sort exists to answer - a large loose file that
/// should outrank a small folder.
pub fn sort_tree(tree: &mut [TopGroup], items: &[FindingItem], sort: Option<TreeSort>) {
    let Some(sort) = sort else {
        return;
    };

    for top_group in tree.iter_mut() {
        for game in top_group.games.iter_mut() {
            for category_node in game.categories.iter_mut() {
                for node in category_node.nodes.iter_mut() {
                    if let TreeNode::Folder { item_indices, .. } = node {
                        item_indices.sort_by(|&a, &b| cmp_member_files(items, sort, a, b));
                    }
                }
                category_node
                    .nodes
                    .sort_by(|a, b| cmp_nodes(items, sort, a, b));
                // `all_indices` is defined as the flattening of `nodes` (see
                // `CategoryNode::all_indices`), and `export` walks the tree in
                // exactly that order - so it is rebuilt here instead of being
                // left describing the order `build_tree` produced.
                category_node.all_indices = category_node
                    .nodes
                    .iter()
                    .flat_map(node_all_indices)
                    .collect();
            }
            game.categories.sort_by(|a, b| cmp_categories(sort, a, b));
            game.all_indices = game
                .categories
                .iter()
                .flat_map(|category_node| category_node.all_indices.iter().copied())
                .collect();
        }
        top_group.games.sort_by(|a, b| cmp_games(sort, a, b));
        top_group.all_indices = top_group
            .games
            .iter()
            .flat_map(|game| game.all_indices.iter().copied())
            .collect();
    }

    tree.sort_by(|a, b| cmp_top_groups(sort, a, b));
}

/// A category's position in [`CATEGORY_ORDER`] - its place in the default
/// tree, and what stands in for its "name" when sorting (see
/// [`cmp_categories`]).
fn category_rank(category: DisplayCategory) -> usize {
    CATEGORY_ORDER
        .iter()
        .position(|&candidate| candidate == category)
        .unwrap_or(CATEGORY_ORDER.len())
}

/// [`category_rank`] for a node's category, ranking the flat axis's
/// category-less node ([`CategoryNode::category`]) last. There is only ever one
/// of those in a tree, so where it ranks is a formality - but it has to be a
/// defined one, or the sort would depend on hash-map iteration order.
fn category_node_rank(category: Option<DisplayCategory>) -> usize {
    category.map_or(CATEGORY_ORDER.len(), category_rank)
}

/// Orders two top-level branch rows.
///
/// The unattributed branch is *not* pinned last here, unlike in the default
/// order: under an explicit sort the user asked for one rule over every row,
/// and a branch that ignored it would be a heading the screen cannot account
/// for. It still sorts last among equals, since its name is empty.
fn cmp_top_groups(sort: TreeSort, a: &TopGroup, b: &TopGroup) -> std::cmp::Ordering {
    let primary = match sort.column {
        SortColumn::Name => path_cmp(&a.key.value(), &b.key.value()),
        SortColumn::Files => a.all_indices.len().cmp(&b.all_indices.len()),
        SortColumn::Size => a.total_bytes.cmp(&b.total_bytes),
    };
    sort.directed(primary)
        .then_with(|| path_cmp(&a.key.value(), &b.key.value()))
}

/// Orders two game rows within one disk.
fn cmp_games(sort: TreeSort, a: &GameNode, b: &GameNode) -> std::cmp::Ordering {
    let primary = match sort.column {
        SortColumn::Name => path_cmp(&a.game_name, &b.game_name),
        SortColumn::Files => a.all_indices.len().cmp(&b.all_indices.len()),
        SortColumn::Size => a.total_bytes.cmp(&b.total_bytes),
    };
    sort.directed(primary)
        .then_with(|| path_cmp(&a.game_name, &b.game_name))
}

/// Orders two category rows within one game.
///
/// Their "name" order is [`CATEGORY_ORDER`], not the localized label: the
/// taxonomy is fixed and six entries long, and keying it to the display string
/// would rearrange the tree whenever the interface language changes.
fn cmp_categories(sort: TreeSort, a: &CategoryNode, b: &CategoryNode) -> std::cmp::Ordering {
    let by_rank = category_node_rank(a.category).cmp(&category_node_rank(b.category));
    let primary = match sort.column {
        SortColumn::Name => by_rank,
        SortColumn::Files => a.all_indices.len().cmp(&b.all_indices.len()),
        SortColumn::Size => a.total_bytes.cmp(&b.total_bytes),
    };
    sort.directed(primary).then(by_rank)
}

/// The figure a node's row carries in the "Files" column: a folder's member
/// count, or 1 for a standalone file - whose own row leaves the cell blank,
/// which is why every file ties here and the name breaks it.
fn node_file_count(node: &TreeNode) -> usize {
    match node {
        TreeNode::Folder { item_indices, .. } => item_indices.len(),
        TreeNode::File { .. } => 1,
    }
}

/// Orders two nodes within one category - folders and standalone files
/// together, see [`sort_tree`].
fn cmp_nodes(
    items: &[FindingItem],
    sort: TreeSort,
    a: &TreeNode,
    b: &TreeNode,
) -> std::cmp::Ordering {
    let primary = match sort.column {
        SortColumn::Name => path_cmp(&node_sort_key(items, a), &node_sort_key(items, b)),
        SortColumn::Files => node_file_count(a).cmp(&node_file_count(b)),
        SortColumn::Size => node_bytes(items, a).cmp(&node_bytes(items, b)),
    };
    sort.directed(primary)
        .then_with(|| path_cmp(&node_sort_key(items, a), &node_sort_key(items, b)))
}

/// Orders two of a folder's member files. Every member row leaves the count
/// cell blank, so under that column the path is left to decide.
fn cmp_member_files(
    items: &[FindingItem],
    sort: TreeSort,
    a: usize,
    b: usize,
) -> std::cmp::Ordering {
    let (left, right) = (&items[a].row, &items[b].row);
    let primary = match sort.column {
        SortColumn::Name => path_cmp(&left.rel_path, &right.rel_path),
        SortColumn::Files => std::cmp::Ordering::Equal,
        SortColumn::Size => left.size_on_disk.cmp(&right.size_on_disk),
    };
    sort.directed(primary)
        .then_with(|| path_cmp(&left.rel_path, &right.rel_path))
}

/// Whether every / any item in `indices` is currently selected. Used to
/// drive the tri-state checkbox on category and game headers.
///
/// `any_selected` looks at every row regardless of how it got selected. A
/// row that is not bulk-selectable (imported and untrusted, or an anti-cheat
/// protected monolithic archive) can still be ticked by hand, and once it
/// is, it is really queued for deletion - so it must never be invisible to
/// the header, or the checkbox lies about there being nothing selected here.
///
/// "All selected" is judged against the bulk-selectable set when the group
/// has one: that is what a click on the header would actually select, and
/// it is what let an otherwise-complete group stay checked even with an
/// untouched blocked row sitting in it (see
/// `group_selection_never_selects_imported_untrusted_rows`). But a group
/// that is *entirely* individually-selectable-but-not-bulk-selectable - every
/// row in it still `imported_untrusted` - has no bulk-selectable rows to
/// judge against at all. Falling back to the individually-selectable set
/// there is what lets such a group read as indeterminate while partly
/// hand-ticked, and as fully checked once every row in it has been ticked by
/// hand, instead of being stuck reporting "nothing selected" or "nothing to
/// complete" forever.
///
/// An anti-cheat protected monolithic archive never lands in that fallback:
/// the same clause that excludes it from bulk selection already excludes it
/// from [`FindingRow::individually_selectable`] (which [`FindingRow::bulk_selectable`]
/// calls first), so it contributes to neither tally here - same as a row
/// with a `deletion_block_reason`.
pub fn group_selection_state(items: &[FindingItem], indices: &[usize]) -> (bool, bool) {
    let mut bulk_total = 0usize;
    let mut bulk_selected = 0usize;
    let mut selectable_total = 0usize;
    let mut selectable_selected = 0usize;
    let mut any_selected = false;

    for &index in indices {
        let item = &items[index];
        if item.selected {
            any_selected = true;
        }
        // `bulk_selectable` implies `individually_selectable`, so a
        // bulk-selectable row is counted in both tallies below - each is
        // used independently by whichever branch of `all_selected` applies.
        if item.row.bulk_selectable() {
            bulk_total += 1;
            bulk_selected += usize::from(item.selected);
        }
        if item.row.individually_selectable() {
            selectable_total += 1;
            selectable_selected += usize::from(item.selected);
        }
    }

    let all_selected = if bulk_total > 0 {
        bulk_selected == bulk_total
    } else {
        selectable_total > 0 && selectable_selected == selectable_total
    };

    (all_selected, any_selected)
}

/// Flips the selection of a whole group: selects the rest whenever anything
/// here still can be selected, and clears the group only when it cannot.
///
/// The pivot is "would selecting change anything", not "is everything
/// selected" and not "is anything selected", because those two each break a
/// different case. Pivoting on "everything" leaves the header dead in a group
/// with no bulk-selectable rows at all (every row still `imported_untrusted`,
/// or an anti-cheat protected monolithic archive on its own):
/// selecting only ever touches bulk-selectable rows (see
/// [`set_group_selection`]), so "everything selected" is unreachable and every
/// click retries the same no-op, with a hand-ticked row left impossible to
/// clear from here. Pivoting on "anything" fixes that but breaks the ordinary
/// half-ticked group, where a click would throw the user's existing ticks away
/// instead of extending them - the opposite of what a part-filled tri-state
/// checkbox means anywhere else.
///
/// Asking the mutation itself keeps both readings: an ordinary partial group
/// has more to select, so it fills up; a full one and a hand-tick-only one
/// have nothing left to select, so they clear. See [`group_selection_state`]
/// for the remaining case - nothing to select and nothing to clear - where the
/// header must be disabled rather than clickable and inert.
///
/// One combination stays visually ambiguous even so: an all-`imported_untrusted`
/// group with one row hand-ticked reports `(false, true)` from
/// [`group_selection_state`], the same indeterminate reading an ordinary
/// partial selection gets - but a click here clears rather than fills, for
/// exactly the reason above (nothing bulk-selectable to fill it with). The
/// tri-state checkbox alone cannot tell a reader which kind of indeterminate
/// group they are looking at; only the *outcome* of the click does.
pub fn toggle_group(items: &mut [FindingItem], indices: &[usize]) -> bool {
    if set_group_selection(items, indices, true) {
        return true;
    }
    set_group_selection(items, indices, false)
}

/// Sets every item in `indices` to the given selection state. Used by the
/// bulk-selection actions (select all on a disk, all of a category, ...), so
/// selecting honours [`FindingRow::bulk_selectable`]. Deselecting never does:
/// clearing a row is always allowed, whatever put it there.
pub fn set_group_selection(items: &mut [FindingItem], indices: &[usize], selected: bool) -> bool {
    let mut changed = false;
    for &index in indices {
        if (!selected || items[index].row.bulk_selectable()) && items[index].selected != selected {
            items[index].selected = selected;
            changed = true;
        }
    }
    changed
}

/// Total size in bytes of the selected, non-removed items in `indices`.
pub fn group_size_bytes(items: &[FindingItem], indices: &[usize]) -> u64 {
    indices.iter().fold(0, |total, &index| {
        total.saturating_add(items[index].row.size_on_disk)
    })
}

/// Live disk-usage snapshot produced at scan/load time (see
/// `gametrimmer_core::db::occupied_by_library`): total bytes occupied by all
/// scanned games plus the per-library breakdown. Never persisted - it is
/// recomputed from the `files` table on every scan, load, and delete (the
/// delete worker refreshes it after purging the removed files' rows), so
/// nothing here has to be kept in sync as files change and the readout never
/// lags a completed delete.
#[derive(Debug, Clone, Default)]
pub struct Occupancy {
    pub total: u64,
    pub by_library: HashMap<i64, u64>,
}

impl Occupancy {
    /// Builds an `Occupancy` from the per-library map returned by
    /// `gametrimmer_core::db::occupied_by_library`, deriving `total` as the
    /// sum of every library's bytes.
    pub fn from_by_library(by_library: HashMap<i64, u64>) -> Self {
        let total = by_library
            .values()
            .fold(0u64, |total, &bytes| total.saturating_add(bytes));
        Self { total, by_library }
    }

    /// Bytes occupied by `library_id`, or 0 if the library has no scanned
    /// files (or doesn't appear in the map at all).
    pub fn library_bytes(&self, library_id: i64) -> u64 {
        self.by_library.get(&library_id).copied().unwrap_or(0)
    }
}

/// Percentage (0.0..=100.0) of `total` occupied space that deleting
/// `selected` bytes would free. Returns 0.0 when `total` is 0 (no games
/// scanned yet), never divides by zero, and never exceeds 100.0 even if
/// `selected` somehow exceeds `total`.
pub fn freed_percent(selected: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    (selected as f64 / total as f64 * 100.0).min(100.0)
}

/// Wall-clock duration of the two phases of the most recently completed
/// scan, plus their total - see `worker::scan::run_scan` for exactly where
/// each `Instant` is captured. `None` on [`crate::app::GameTrimmerApp`] means
/// no fresh scan has completed yet (either nothing has been scanned this
/// session, or the current results were loaded from a previous scan rather
/// than produced by one - see `WorkerMsg::Done`'s `timing` field).
///
/// **These two spans overlap; they are not a partition of `total`.** The
/// file-table read runs underneath the classification rather than ahead of
/// it, so both spans start when the libraries are on disk and only their end
/// points differ. `scan + analyze` therefore exceeds `total`, and the gap
/// between `analyze`'s end and `total` is post-scan housekeeping. The log
/// line in `run_scan` states the overlap in words for the same reason this
/// comment does: a reader who adds the two numbers must not conclude the
/// arithmetic is broken.
#[derive(Debug, Clone, Copy)]
pub struct ScanTiming {
    /// Discovery + persist + reading every eligible volume's file table -
    /// the IO half. Naturally tiny on an SSD-only setup, where the MFT pass
    /// is skipped entirely; on an HDD library it is the span that used to be
    /// paid before any game was classified and is now hidden underneath
    /// `analyze`.
    pub scan: std::time::Duration,
    /// Per-game classify+write (`Verb::Analyze` in the progress bar), from
    /// the moment the pool opens to the moment the writer joins. Contains
    /// the whole of `scan` except the discovery+persist part that precedes
    /// both.
    pub analyze: std::time::Duration,
    /// The whole scan, start to finish - matches the elapsed time already
    /// folded into `format_scan_summary`'s transient status line.
    pub total: std::time::Duration,
}

/// Formats a duration compactly for display: `"Ns"` under a minute,
/// `"M:SS"` from a minute up to an hour, `"H:MM:SS"` at an hour or beyond.
/// Whole seconds only - this is a coarse, at-a-glance readout, not a
/// stopwatch.
pub fn format_duration(duration: std::time::Duration) -> String {
    let total_secs = duration.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else if minutes > 0 {
        format!("{minutes}:{seconds:02}")
    } else {
        format!("{seconds}s")
    }
}

/// Formats a byte count as a human-readable, localized size string
/// (binary units: 1024-based).
pub fn format_size(lang: crate::i18n::Lang, bytes: u64) -> String {
    let s = crate::i18n::strings(lang);
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    let value = bytes as f64;
    if value >= GB {
        format!("{:.2} {}", value / GB, s.unit_gb)
    } else if value >= MB {
        format!("{:.2} {}", value / MB, s.unit_mb)
    } else if value >= KB {
        format!("{:.2} {}", value / KB, s.unit_kb)
    } else {
        format!("{bytes} {}", s.unit_b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(
        game_id: i64,
        game_name: &str,
        source: FindingSource,
        confidence: u8,
        size: u64,
    ) -> FindingItem {
        item_at(
            game_id,
            game_name,
            "C:\\Games\\Test",
            "file.txt",
            source,
            confidence,
            size,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn item_at(
        game_id: i64,
        game_name: &str,
        install_dir: &str,
        rel_path: &str,
        source: FindingSource,
        confidence: u8,
        size: u64,
    ) -> FindingItem {
        FindingItem {
            row: FindingRow {
                file_id: 0,
                game_id,
                game_name: game_name.to_string(),
                app_id: None,
                install_dir: PathBuf::from(install_dir),
                rel_path: rel_path.to_string(),
                size,
                // Synthetic rows keep on-disk == logical, so existing
                // total/estimate assertions (which now sum on-disk) stay
                // valid; a dedicated test exercises the two diverging.
                size_on_disk: size,
                source,
                rule_desc: "test rule".to_string(),
                confidence,
                lang_tag: None,
                group_dir: None,
                deletion_block_reason: None,
                imported_untrusted: false,
                library: None,
                action: gametrimmer_core::models::FindingAction::DirectDelete,
                anti_cheat_protected: false,
                monolith_badge: None,
            },
            selected: default_selected(confidence),
            removed: false,
        }
    }

    /// Like [`item`], but as a localization finding with a `lang_tag` set —
    /// mirrors what the scan worker produces for `LangDetector` findings.
    fn loc_item(
        game_id: i64,
        game_name: &str,
        kind: LangKind,
        lang_tag: &str,
        confidence: u8,
        size: u64,
    ) -> FindingItem {
        let mut found = item(
            game_id,
            game_name,
            FindingSource::Loc(kind),
            confidence,
            size,
        );
        found.row.rule_desc = "маркер 'voices'".to_string();
        found.row.lang_tag = Some(lang_tag.to_string());
        found
    }

    fn with_group_dir(mut found: FindingItem, group_dir: &str) -> FindingItem {
        found.row.group_dir = Some(group_dir.to_string());
        found
    }

    /// Attributes a finding to a library, the way `worker::load` and
    /// `worker::scan::persistence` both do. `vendor` is `None` for a root that
    /// resolved but has no `game_libraries` row behind it.
    fn with_library(mut found: FindingItem, vendor: Option<&str>, root: &str) -> FindingItem {
        found.row.library = Some(LibraryOrigin {
            vendor: vendor.map(str::to_string),
            root: PathBuf::from(root),
        });
        found
    }

    /// An orphaned-residue finding as the scan/load path produces it: the
    /// synthetic [`ORPHAN_GAME_ID`], an empty game name, and the orphan
    /// folder's container as `install_dir` with the folder name as `rel_path`.
    fn orphan_item(container: &str, folder: &str, kind: OrphanKind, size: u64) -> FindingItem {
        let mut found = item_at(
            ORPHAN_GAME_ID,
            "",
            container,
            folder,
            FindingSource::Orphan(kind),
            orphan_confidence(kind),
            size,
        );
        found.row.rule_desc = "осиротіла тека".to_string();
        found
    }

    #[test]
    fn display_category_maps_every_source_to_its_top_level_category() {
        assert_eq!(
            display_category(FindingSource::Rule(Category::RedistFolder)),
            DisplayCategory::Redist
        );
        assert_eq!(
            display_category(FindingSource::Rule(Category::RedistFile)),
            DisplayCategory::Redist
        );
        assert_eq!(
            display_category(FindingSource::Rule(Category::DocsFolder)),
            DisplayCategory::Docs
        );
        assert_eq!(
            display_category(FindingSource::Rule(Category::DocsFile)),
            DisplayCategory::Docs
        );
        assert_eq!(
            display_category(FindingSource::Rule(Category::Bonus)),
            DisplayCategory::Bonus
        );
        assert_eq!(
            display_category(FindingSource::Rule(Category::MonolithicArchive)),
            DisplayCategory::Archives
        );
        assert_eq!(
            display_category(FindingSource::Rule(Category::DevLeftovers)),
            DisplayCategory::DevLeftovers
        );
        assert_eq!(
            display_category(FindingSource::Rule(Category::Intro)),
            DisplayCategory::Intro
        );
        for kind in [
            LangKind::Audio,
            LangKind::Text,
            LangKind::Video,
            LangKind::Font,
            LangKind::Graphic,
            LangKind::Unknown,
        ] {
            assert_eq!(
                display_category(FindingSource::Loc(kind)),
                DisplayCategory::Loc
            );
        }
        for kind in [
            OrphanKind::UnmanagedFolder,
            OrphanKind::ServiceFolder,
            OrphanKind::UnreferencedFile,
        ] {
            assert_eq!(
                display_category(FindingSource::Orphan(kind)),
                DisplayCategory::Orphan
            );
        }
    }

    #[test]
    fn bulk_selectable_excludes_blocked_and_untrusted_rows() {
        let plain = item(1, "Game", FindingSource::Rule(Category::Bonus), 90, 10);
        assert!(plain.row.bulk_selectable());

        let mut blocked = item(1, "Game", FindingSource::Rule(Category::Bonus), 90, 10);
        blocked.row.deletion_block_reason = Some("fresh scan required".to_string());
        assert!(!blocked.row.bulk_selectable());

        let mut untrusted = item(1, "Game", FindingSource::Rule(Category::Bonus), 90, 10);
        untrusted.row.imported_untrusted = true;
        assert!(!untrusted.row.bulk_selectable());

        let mut protected_monolith = item(
            1,
            "Game",
            FindingSource::Rule(Category::MonolithicArchive),
            90,
            10,
        );
        protected_monolith.row.anti_cheat_protected = true;
        protected_monolith.row.action = gametrimmer_core::models::FindingAction::SparseZero {
            format: "Wwise".to_string(),
            languages: vec!["de".to_string()],
            stream_count: 1,
            offsets: vec![(0, 1)],
            streams: vec![],
            estimated_savings: 1,
        };
        assert!(
            !protected_monolith.row.individually_selectable(),
            "the keyboard and per-row checkbox must share this rejection"
        );
        assert!(!protected_monolith.row.bulk_selectable());
        let mut unsupported_archive = protected_monolith.clone();
        unsupported_archive.row.anti_cheat_protected = false;
        assert!(
            !unsupported_archive.row.individually_selectable(),
            "an unprotected archive action is still not executable by this GUI"
        );
        assert!(!unsupported_archive.row.bulk_selectable());
    }

    /// The owner's decision (GT: narrow the anti-cheat carve-out to
    /// monolithic archives): an intro row is not deleted, it is replaced with
    /// a micro-stub, but that is still a whole-file-shaped operation as far
    /// as an anti-cheat integrity check can tell - not the container-editing
    /// a monolithic archive does. Hiding it from Select All bought no safety,
    /// only 56 hidden intro findings in Assassin's Creed Shadows with no
    /// visible reason. It now stays exactly as ordinary as a whole-file
    /// delete.
    #[test]
    fn anti_cheat_protected_intro_row_is_bulk_selectable() {
        let mut protected = item(1, "Game", FindingSource::Rule(Category::Intro), 90, 10);
        protected.row.anti_cheat_protected = true;

        assert!(
            protected.row.individually_selectable(),
            "a deliberate, single tick must still be honoured"
        );
        assert!(
            protected.row.bulk_selectable(),
            "an anti-cheat protected intro row must be picked up by select-all like any other \
             whole-file delete"
        );
    }

    /// The other half of the narrowed carve-out: a row that is only ever a
    /// whole-file delete (a language pack, here) stays perfectly ordinary in
    /// a protected game. `anti_cheat_protected` is a per-*game* verdict, so
    /// treating it alone as disqualifying (the pre-fix rule) took Select All
    /// and every group header away from every finding in every protected
    /// game - on the owner's real library, 112k+ findings across 162 games.
    #[test]
    fn anti_cheat_protected_loc_row_is_bulk_selectable() {
        let mut protected = item(1, "Game", FindingSource::Loc(LangKind::Text), 90, 10);
        protected.row.anti_cheat_protected = true;

        assert!(
            protected.row.bulk_selectable(),
            "a whole-file delete in a protected game is ordinary - anti-cheat cannot notice \
             an uninstalled language pack any differently than the user doing it by hand"
        );
    }

    #[test]
    fn source_key_preserves_the_original_granular_persistence_strings() {
        assert_eq!(
            source_key(FindingSource::Rule(Category::RedistFolder)),
            "redist_folder"
        );
        assert_eq!(
            source_key(FindingSource::Rule(Category::RedistFile)),
            "redist_file"
        );
        assert_eq!(
            source_key(FindingSource::Rule(Category::DocsFolder)),
            "docs_folder"
        );
        assert_eq!(
            source_key(FindingSource::Rule(Category::DocsFile)),
            "docs_file"
        );
        assert_eq!(source_key(FindingSource::Rule(Category::Bonus)), "bonus");
        assert_eq!(
            source_key(FindingSource::Rule(Category::MonolithicArchive)),
            "monolithic_archive"
        );
        assert_eq!(
            source_key(FindingSource::Rule(Category::DevLeftovers)),
            "dev_leftovers"
        );
        assert_eq!(source_key(FindingSource::Rule(Category::Intro)), "intro");
        assert_eq!(source_key(FindingSource::Loc(LangKind::Audio)), "loc_audio");
        assert_eq!(source_key(FindingSource::Loc(LangKind::Text)), "loc_text");
        assert_eq!(source_key(FindingSource::Loc(LangKind::Video)), "loc_video");
        assert_eq!(source_key(FindingSource::Loc(LangKind::Font)), "loc_font");
        assert_eq!(
            source_key(FindingSource::Loc(LangKind::Unknown)),
            "loc_unknown"
        );
        assert_eq!(
            source_key(FindingSource::Orphan(OrphanKind::UnmanagedFolder)),
            "orphan_folder"
        );
        assert_eq!(
            source_key(FindingSource::Orphan(OrphanKind::ServiceFolder)),
            "orphan_service"
        );
    }

    #[test]
    fn parse_source_key_round_trips_every_finding_source_variant() {
        let all_sources = [
            FindingSource::Rule(Category::RedistFolder),
            FindingSource::Rule(Category::RedistFile),
            FindingSource::Rule(Category::DocsFolder),
            FindingSource::Rule(Category::DocsFile),
            FindingSource::Rule(Category::Bonus),
            FindingSource::Rule(Category::MonolithicArchive),
            FindingSource::Rule(Category::DevLeftovers),
            FindingSource::Rule(Category::Intro),
            FindingSource::Loc(LangKind::Audio),
            FindingSource::Loc(LangKind::Text),
            FindingSource::Loc(LangKind::Video),
            FindingSource::Loc(LangKind::Font),
            FindingSource::Loc(LangKind::Unknown),
            FindingSource::Orphan(OrphanKind::UnmanagedFolder),
            FindingSource::Orphan(OrphanKind::ServiceFolder),
        ];

        for source in all_sources {
            assert_eq!(
                parse_source_key(source_key(source)),
                Some(source),
                "round-trip through source_key/parse_source_key must recover {source:?}"
            );
        }

        assert_eq!(
            parse_source_key("not_a_real_category"),
            None,
            "an unrecognized category string must parse to None, not panic"
        );
    }

    #[test]
    fn category_ui_key_is_stable_and_distinct_per_category() {
        let keys: Vec<&str> = CATEGORY_ORDER.iter().map(|&c| category_ui_key(c)).collect();
        assert_eq!(
            keys,
            vec![
                "redist",
                "dev_leftovers",
                "intro",
                "docs",
                "bonus",
                "loc",
                "archives",
                "orphan",
                "workshop",
                "shader_cache",
                "crashes",
                "saves",
                "launcher_cache"
            ]
        );
    }

    /// Two games on one disk, the alphabetically-first one being the smaller,
    /// so "by name" and "by size" cannot accidentally agree.
    fn two_games() -> Vec<FindingItem> {
        vec![
            item(
                1,
                "Alpha",
                FindingSource::Rule(Category::RedistFolder),
                90,
                100,
            ),
            item(
                2,
                "Beta",
                FindingSource::Rule(Category::RedistFolder),
                90,
                900,
            ),
        ]
    }

    fn game_names(tree: &[TopGroup]) -> Vec<String> {
        tree[0]
            .games
            .iter()
            .map(|game| game.game_name.clone())
            .collect()
    }

    fn sorted(items: &[FindingItem], column: SortColumn, descending: bool) -> Vec<TopGroup> {
        let mut tree = build_tree(items, GroupAxis::Disk);
        sort_tree(&mut tree, items, Some(TreeSort { column, descending }));
        tree
    }

    /// `None` is the tree's own order, so it must reach `build_tree`'s output
    /// untouched - this is what lets the third click on a heading give the
    /// designed order back rather than an approximation of it.
    #[test]
    fn sort_tree_leaves_the_default_order_alone() {
        let items = two_games();
        let mut tree = build_tree(&items, GroupAxis::Disk);
        let before = game_names(&tree);

        sort_tree(&mut tree, &items, None);

        assert_eq!(game_names(&tree), before);
    }

    #[test]
    fn sort_tree_orders_games_by_name_in_both_directions() {
        let items = two_games();

        assert_eq!(
            game_names(&sorted(&items, SortColumn::Name, false)),
            vec!["Alpha", "Beta"],
        );
        assert_eq!(
            game_names(&sorted(&items, SortColumn::Name, true)),
            vec!["Beta", "Alpha"],
        );
    }

    /// Ascending by size is the case the default order can never produce:
    /// `build_tree` puts the largest game first, so a test that only checked
    /// descending would pass against a `sort_tree` that did nothing at all.
    #[test]
    fn sort_tree_orders_games_by_size_in_both_directions() {
        let items = two_games();

        assert_eq!(
            game_names(&sorted(&items, SortColumn::Size, false)),
            vec!["Alpha", "Beta"],
        );
        assert_eq!(
            game_names(&sorted(&items, SortColumn::Size, true)),
            vec!["Beta", "Alpha"],
        );
    }

    /// Every member row of a folder leaves the "Files" cell blank, so ordering
    /// by that column has nothing to compare them on and the path decides.
    ///
    /// Asserted in both directions on purpose: the name tie-break stays
    /// ascending either way, so flipping a column the rows do not carry must
    /// not shuffle them for a reason nothing on screen states.
    #[test]
    fn sort_tree_leaves_a_blank_column_to_the_name_in_either_direction() {
        let items = vec![
            with_group_dir(
                item_at(
                    1,
                    "Game",
                    "C:\\Games\\Test",
                    "data\\b.bin",
                    FindingSource::Rule(Category::RedistFolder),
                    90,
                    900,
                ),
                "data",
            ),
            with_group_dir(
                item_at(
                    1,
                    "Game",
                    "C:\\Games\\Test",
                    "data\\a.bin",
                    FindingSource::Rule(Category::RedistFolder),
                    90,
                    10,
                ),
                "data",
            ),
        ];

        for descending in [false, true] {
            let tree = sorted(&items, SortColumn::Files, descending);
            let TreeNode::Folder { item_indices, .. } = &tree[0].games[0].categories[0].nodes[0]
            else {
                panic!("both findings share a group_dir, so they collapse into one folder");
            };
            assert_eq!(
                item_indices
                    .iter()
                    .map(|&i| items[i].row.rel_path.as_str())
                    .collect::<Vec<_>>(),
                vec!["data\\a.bin", "data\\b.bin"],
                "descending={descending}",
            );
        }
    }

    /// The claim that makes a size sort worth having: a large loose file
    /// outranks a small folder. The default order never interleaves the two -
    /// every folder precedes every file - so this is exactly what an explicit
    /// sort has to override.
    #[test]
    fn sort_tree_interleaves_folders_and_files_by_size() {
        let items = vec![
            with_group_dir(
                item_at(
                    1,
                    "Game",
                    "C:\\Games\\Test",
                    "small\\a.bin",
                    FindingSource::Rule(Category::RedistFolder),
                    90,
                    10,
                ),
                "small",
            ),
            item_at(
                1,
                "Game",
                "C:\\Games\\Test",
                "huge.bin",
                FindingSource::Rule(Category::RedistFolder),
                90,
                5000,
            ),
        ];

        let default_first = &build_tree(&items, GroupAxis::Disk)[0].games[0].categories[0].nodes[0];
        assert!(
            matches!(default_first, TreeNode::Folder { .. }),
            "the default order is meant to lead with folders - this test has nothing to prove otherwise",
        );

        let tree = sorted(&items, SortColumn::Size, true);
        let TreeNode::File { index } = &tree[0].games[0].categories[0].nodes[0] else {
            panic!("the largest node is a loose file, so a size sort must lead with it");
        };
        assert_eq!(items[*index].row.rel_path, "huge.bin");
    }

    #[test]
    fn sort_tree_orders_a_folders_member_files_by_size() {
        let items = vec![
            with_group_dir(
                item_at(
                    1,
                    "Game",
                    "C:\\Games\\Test",
                    "data\\a.bin",
                    FindingSource::Rule(Category::RedistFolder),
                    90,
                    10,
                ),
                "data",
            ),
            with_group_dir(
                item_at(
                    1,
                    "Game",
                    "C:\\Games\\Test",
                    "data\\b.bin",
                    FindingSource::Rule(Category::RedistFolder),
                    90,
                    900,
                ),
                "data",
            ),
        ];

        let tree = sorted(&items, SortColumn::Size, true);
        let TreeNode::Folder { item_indices, .. } = &tree[0].games[0].categories[0].nodes[0] else {
            panic!("both findings share a group_dir, so they collapse into one folder");
        };
        assert_eq!(
            item_indices
                .iter()
                .map(|&i| items[i].row.rel_path.as_str())
                .collect::<Vec<_>>(),
            vec!["data\\b.bin", "data\\a.bin"],
        );
    }

    /// `all_indices` is defined as the flattening of `nodes`, and the CSV
    /// export walks the tree in that order. A sort that reordered the nodes
    /// and left the index list describing the old order would silently make
    /// the export disagree with the screen.
    #[test]
    fn sort_tree_keeps_all_indices_in_step_with_the_nodes_it_reordered() {
        let items = vec![
            item_at(
                1,
                "Game",
                "C:\\Games\\Test",
                "small.bin",
                FindingSource::Rule(Category::RedistFolder),
                90,
                10,
            ),
            item_at(
                1,
                "Game",
                "C:\\Games\\Test",
                "huge.bin",
                FindingSource::Rule(Category::RedistFolder),
                90,
                5000,
            ),
        ];

        let tree = sorted(&items, SortColumn::Size, true);
        let category_node = &tree[0].games[0].categories[0];
        let flattened: Vec<usize> = category_node
            .nodes
            .iter()
            .flat_map(node_all_indices)
            .collect();

        assert_eq!(category_node.all_indices, flattened);
        assert_eq!(tree[0].games[0].all_indices, flattened);
        assert_eq!(tree[0].all_indices, flattened);
    }

    #[test]
    fn build_tree_groups_by_disk_then_game_then_category() {
        let items = vec![
            item(
                1,
                "Game A",
                FindingSource::Rule(Category::RedistFolder),
                90,
                100,
            ),
            item(
                1,
                "Game A",
                FindingSource::Rule(Category::RedistFile),
                95,
                50,
            ),
            item(
                2,
                "Game B",
                FindingSource::Rule(Category::RedistFolder),
                90,
                200,
            ),
        ];

        let tree = build_tree(&items, GroupAxis::Disk);

        assert_eq!(tree.len(), 1, "all games share the same disk (C:)");
        assert_eq!(tree[0].key.value(), "C:");
        assert_eq!(
            tree[0].games.len(),
            2,
            "each game appears exactly once under its disk"
        );

        let game_a = tree[0]
            .games
            .iter()
            .find(|game| game.game_name == "Game A")
            .expect("Game A node present");
        assert_eq!(
            game_a.categories.len(),
            1,
            "both redist_folder and redist_file collapse into the single Redist category"
        );
        assert_eq!(game_a.categories[0].category, Some(DisplayCategory::Redist));
        assert_eq!(
            game_a.categories[0].nodes.len(),
            2,
            "two orphan files (no group_dir set) become two separate nodes"
        );
    }

    #[test]
    fn build_tree_sorts_games_within_a_disk_by_total_bytes_descending() {
        let items = vec![
            item(
                1,
                "Small Game",
                FindingSource::Rule(Category::Bonus),
                90,
                10,
            ),
            item(
                2,
                "Big Game",
                FindingSource::Rule(Category::Bonus),
                90,
                1000,
            ),
        ];

        let tree = build_tree(&items, GroupAxis::Disk);

        assert_eq!(tree[0].games[0].game_name, "Big Game");
        assert_eq!(tree[0].games[1].game_name, "Small Game");
    }

    #[test]
    fn build_tree_separates_disks() {
        let items = vec![
            item_at(
                1,
                "Game A",
                "E:\\Games\\A",
                "file.txt",
                FindingSource::Rule(Category::Bonus),
                90,
                10,
            ),
            item_at(
                2,
                "Game B",
                "D:\\Games\\B",
                "file.txt",
                FindingSource::Rule(Category::Bonus),
                90,
                10,
            ),
        ];

        let tree = build_tree(&items, GroupAxis::Disk);

        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].key.value(), "D:", "disks are sorted alphabetically");
        assert_eq!(tree[1].key.value(), "E:");
    }

    #[test]
    fn build_tree_uses_first_component_for_non_drive_roots() {
        let items = vec![item_at(
            1,
            "Networked Game",
            "\\\\server\\share\\Games\\A",
            "file.txt",
            FindingSource::Rule(Category::Bonus),
            90,
            10,
        )];

        let tree = build_tree(&items, GroupAxis::Disk);

        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].key.value(), "\\\\server\\share");
    }

    #[test]
    fn build_tree_groups_localization_categories_together() {
        let items = vec![
            loc_item(1, "Game A", LangKind::Audio, "es", 90, 100),
            loc_item(1, "Game A", LangKind::Text, "fr", 88, 20),
            loc_item(2, "Game B", LangKind::Audio, "de", 95, 300),
        ];

        let tree = build_tree(&items, GroupAxis::Disk);

        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].games.len(), 2);
        for game in &tree[0].games {
            assert_eq!(
                game.categories.len(),
                1,
                "audio and text localization findings collapse into one Loc category"
            );
            assert_eq!(game.categories[0].category, Some(DisplayCategory::Loc));
        }
    }

    #[test]
    fn build_tree_skips_removed_items() {
        let mut items = vec![item(
            1,
            "Game A",
            FindingSource::Rule(Category::Bonus),
            90,
            10,
        )];
        items[0].removed = true;

        let tree = build_tree(&items, GroupAxis::Disk);

        assert!(tree.is_empty(), "removed items must not appear in the tree");
    }

    #[test]
    fn build_tree_collapses_a_fully_flagged_folder_into_one_node() {
        let items = vec![
            with_group_dir(
                item_at(
                    1,
                    "Game A",
                    "C:\\Games\\A",
                    "junk\\a.txt",
                    FindingSource::Rule(Category::Bonus),
                    90,
                    100,
                ),
                "junk",
            ),
            with_group_dir(
                item_at(
                    1,
                    "Game A",
                    "C:\\Games\\A",
                    "junk\\b.txt",
                    FindingSource::Rule(Category::Bonus),
                    90,
                    50,
                ),
                "junk",
            ),
        ];

        let tree = build_tree(&items, GroupAxis::Disk);

        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].games.len(), 1);
        let game = &tree[0].games[0];
        assert_eq!(game.categories.len(), 1);
        assert_eq!(
            game.categories[0].nodes.len(),
            1,
            "one folder node, not two file nodes"
        );
        match &game.categories[0].nodes[0] {
            TreeNode::Folder {
                group_dir,
                item_indices,
                total_bytes,
            } => {
                assert_eq!(group_dir, "junk");
                assert_eq!(item_indices.len(), 2);
                assert_eq!(*total_bytes, 150);
            }
            TreeNode::File { .. } => panic!("expected a folder node"),
        }
    }

    #[test]
    fn build_tree_places_a_shared_folder_in_exactly_one_category_by_byte_majority() {
        // A folder with mixed sources: 100 bytes of Docs, 10 bytes of Bonus.
        // Byte majority must put the whole folder under Docs, never split.
        let items = vec![
            with_group_dir(
                item_at(
                    1,
                    "Game A",
                    "C:\\Games\\A",
                    "extras\\manual.pdf",
                    FindingSource::Rule(Category::DocsFile),
                    85,
                    100,
                ),
                "extras",
            ),
            with_group_dir(
                item_at(
                    1,
                    "Game A",
                    "C:\\Games\\A",
                    "extras\\poster.jpg",
                    FindingSource::Rule(Category::Bonus),
                    80,
                    10,
                ),
                "extras",
            ),
        ];

        let tree = build_tree(&items, GroupAxis::Disk);

        let game = &tree[0].games[0];
        assert_eq!(
            game.categories.len(),
            1,
            "the shared folder appears in exactly one category"
        );
        assert_eq!(game.categories[0].category, Some(DisplayCategory::Docs));
        let TreeNode::Folder { item_indices, .. } = &game.categories[0].nodes[0] else {
            panic!("expected a folder node");
        };
        assert_eq!(item_indices.len(), 2, "both member findings stay together");
    }

    #[test]
    fn build_tree_majority_category_tie_breaks_by_category_order() {
        // Equal bytes on both sides (Redist vs Docs) - CATEGORY_ORDER lists
        // Redist before Docs, so Redist must win the tie.
        let items = vec![
            with_group_dir(
                item_at(
                    1,
                    "Game A",
                    "C:\\Games\\A",
                    "mixed\\setup.exe",
                    FindingSource::Rule(Category::RedistFile),
                    90,
                    50,
                ),
                "mixed",
            ),
            with_group_dir(
                item_at(
                    1,
                    "Game A",
                    "C:\\Games\\A",
                    "mixed\\readme.pdf",
                    FindingSource::Rule(Category::DocsFile),
                    85,
                    50,
                ),
                "mixed",
            ),
        ];

        let tree = build_tree(&items, GroupAxis::Disk);

        assert_eq!(
            tree[0].games[0].categories[0].category,
            Some(DisplayCategory::Redist)
        );
    }

    #[test]
    fn build_tree_files_a_weightless_group_under_a_category_it_actually_has() {
        // A read-only monolithic archive is deliberately worth zero on-disk
        // bytes: it frees nothing until in-place trimming exists, so the scan
        // zeroes `size_on_disk` to keep it out of every total while its real
        // size still shows. A whole folder of them therefore weighs nothing,
        // and the winner used to be seeded with `CATEGORY_ORDER[0]` and need a
        // strict `>` to be unseated - so Delta Force's `Content\Paks\`, 1.5 GB
        // monoliths and nothing else, was filed under "Redistributables", a
        // category with no member in that folder at all.
        //
        // The sizes below are the shape that matters: large logical, zero on
        // disk. Weighing `size` instead would hide this without fixing it, and
        // would put back into the totals the bytes the zeroing exists to keep
        // out.
        let monolith = |name: &str| {
            let mut item = with_group_dir(
                item_at(
                    1,
                    "Delta Force",
                    "F:\\SteamLibrary\\steamapps\\common\\Delta Force",
                    &format!("Game\\DeltaForce\\Content\\Paks\\{name}"),
                    FindingSource::Rule(Category::MonolithicArchive),
                    90,
                    1_632_087_572,
                ),
                "Game\\DeltaForce\\Content\\Paks",
            );
            item.row.size_on_disk = 0;
            item
        };
        let items = vec![
            monolith("pak-0-0-pakchunk1-WindowsClient.pak"),
            monolith("pak-0-0-pakchunk2-WindowsClient.pak"),
        ];

        let tree = build_tree(&items, GroupAxis::Disk);

        let categories = &tree[0].games[0].categories;
        assert_eq!(categories.len(), 1);
        assert_eq!(
            categories[0].category,
            Some(DisplayCategory::Archives),
            "a folder of monolithic archives belongs in Archives, not in whichever \
             category happens to head CATEGORY_ORDER"
        );
    }

    #[test]
    fn build_tree_orphan_files_appear_individually_in_their_own_category() {
        let items = vec![item_at(
            1,
            "Game A",
            "C:\\Games\\A",
            "loose.txt",
            FindingSource::Rule(Category::DevLeftovers),
            90,
            10,
        )];

        let tree = build_tree(&items, GroupAxis::Disk);

        let game = &tree[0].games[0];
        assert_eq!(
            game.categories[0].category,
            Some(DisplayCategory::DevLeftovers)
        );
        match &game.categories[0].nodes[0] {
            TreeNode::File { index } => assert_eq!(*index, 0),
            TreeNode::Folder { .. } => panic!("expected an orphan file node"),
        }
    }

    #[test]
    fn build_tree_sorts_orphan_files_within_a_category_by_path_not_size() {
        // Orphan files are sorted by rel_path, independent of size - "b_small"
        // is 10 bytes and "a_big" is 1000 bytes, but "a_big" still sorts
        // first because it comes first alphabetically.
        let items = vec![
            item_at(
                1,
                "Game A",
                "C:\\Games\\A",
                "b_small.txt",
                FindingSource::Rule(Category::Bonus),
                90,
                10,
            ),
            item_at(
                1,
                "Game A",
                "C:\\Games\\A",
                "a_big.txt",
                FindingSource::Rule(Category::Bonus),
                90,
                1000,
            ),
        ];

        let tree = build_tree(&items, GroupAxis::Disk);

        let TreeNode::File { index: first } = tree[0].games[0].categories[0].nodes[0] else {
            panic!("expected file nodes");
        };
        assert_eq!(
            items[first].row.rel_path, "a_big.txt",
            "files sort by path, not by size"
        );
    }

    #[test]
    fn build_tree_orders_folders_before_files_within_a_category() {
        // A small folder (30 bytes total) and a much bigger orphan file
        // (1000 bytes) share a category - folders must still lead, since
        // top-level ordering groups by node kind before by size.
        let items = vec![
            with_group_dir(
                item_at(
                    1,
                    "Game A",
                    "C:\\Games\\A",
                    "junk\\a.txt",
                    FindingSource::Rule(Category::Bonus),
                    90,
                    20,
                ),
                "junk",
            ),
            with_group_dir(
                item_at(
                    1,
                    "Game A",
                    "C:\\Games\\A",
                    "junk\\b.txt",
                    FindingSource::Rule(Category::Bonus),
                    90,
                    10,
                ),
                "junk",
            ),
            item_at(
                1,
                "Game A",
                "C:\\Games\\A",
                "huge_loose_file.bin",
                FindingSource::Rule(Category::Bonus),
                90,
                1000,
            ),
        ];

        let tree = build_tree(&items, GroupAxis::Disk);

        let nodes = &tree[0].games[0].categories[0].nodes;
        assert_eq!(nodes.len(), 2, "one folder node + one file node");
        assert!(
            matches!(nodes[0], TreeNode::Folder { .. }),
            "folder must come first even though the file is much larger"
        );
        assert!(matches!(nodes[1], TreeNode::File { .. }));
    }

    #[test]
    fn build_tree_sorts_folder_member_indices_by_path() {
        // Inserted in reverse path order (b.txt before a.txt) - build_tree
        // must reorder item_indices so they read in path order regardless of
        // scan/insertion order (the MFT scan path does not visit in path
        // order).
        let items = vec![
            with_group_dir(
                item_at(
                    1,
                    "Game A",
                    "C:\\Games\\A",
                    "junk\\b.txt",
                    FindingSource::Rule(Category::Bonus),
                    90,
                    10,
                ),
                "junk",
            ),
            with_group_dir(
                item_at(
                    1,
                    "Game A",
                    "C:\\Games\\A",
                    "junk\\a.txt",
                    FindingSource::Rule(Category::Bonus),
                    90,
                    20,
                ),
                "junk",
            ),
        ];

        let tree = build_tree(&items, GroupAxis::Disk);

        let TreeNode::Folder { item_indices, .. } = &tree[0].games[0].categories[0].nodes[0] else {
            panic!("expected a folder node");
        };
        assert_eq!(
            items[item_indices[0]].row.rel_path, "junk\\a.txt",
            "member indices are sorted by path, not insertion order"
        );
        assert_eq!(items[item_indices[1]].row.rel_path, "junk\\b.txt");
    }

    #[test]
    fn build_tree_aggregates_all_indices_and_total_bytes_at_category_and_disk_level() {
        let items = vec![
            // A collapsible folder (2 members, 150 bytes) under Bonus.
            with_group_dir(
                item_at(
                    1,
                    "Game A",
                    "C:\\Games\\A",
                    "junk\\a.txt",
                    FindingSource::Rule(Category::Bonus),
                    90,
                    100,
                ),
                "junk",
            ),
            with_group_dir(
                item_at(
                    1,
                    "Game A",
                    "C:\\Games\\A",
                    "junk\\b.txt",
                    FindingSource::Rule(Category::Bonus),
                    90,
                    50,
                ),
                "junk",
            ),
            // An orphan file (20 bytes) under Bonus, same disk.
            item_at(
                1,
                "Game A",
                "C:\\Games\\A",
                "loose.jpg",
                FindingSource::Rule(Category::Bonus),
                90,
                20,
            ),
            // An orphan file (5 bytes) under Docs, same disk - a second
            // category, so the disk-level aggregate must span categories.
            item_at(
                1,
                "Game A",
                "C:\\Games\\A",
                "readme.pdf",
                FindingSource::Rule(Category::DocsFile),
                90,
                5,
            ),
        ];

        let tree = build_tree(&items, GroupAxis::Disk);
        assert_eq!(tree.len(), 1);
        let disk = &tree[0];
        assert_eq!(disk.games.len(), 1);
        let game = &disk.games[0];

        let bonus = game
            .categories
            .iter()
            .find(|c| c.category == Some(DisplayCategory::Bonus))
            .expect("bonus category present");
        assert_eq!(
            bonus.all_indices.len(),
            3,
            "2 folder members + 1 orphan = 3 indices"
        );
        assert_eq!(bonus.total_bytes, 100 + 50 + 20);
        let expected_bonus_indices: std::collections::HashSet<usize> =
            bonus.nodes.iter().flat_map(node_all_indices).collect();
        assert_eq!(
            bonus
                .all_indices
                .iter()
                .copied()
                .collect::<std::collections::HashSet<usize>>(),
            expected_bonus_indices,
            "category all_indices must match the union of its nodes' members"
        );

        let docs = game
            .categories
            .iter()
            .find(|c| c.category == Some(DisplayCategory::Docs))
            .expect("docs category present");
        assert_eq!(docs.all_indices.len(), 1);
        assert_eq!(docs.total_bytes, 5);

        assert_eq!(
            game.all_indices.len(),
            4,
            "game-level aggregate spans every category (3 + 1)"
        );
        assert_eq!(game.total_bytes, 100 + 50 + 20 + 5);

        assert_eq!(
            disk.all_indices.len(),
            4,
            "disk-level aggregate spans every game"
        );
        assert_eq!(disk.total_bytes, 100 + 50 + 20 + 5);
        let expected_disk_indices: std::collections::HashSet<usize> = disk
            .games
            .iter()
            .flat_map(|g| g.all_indices.iter().copied())
            .collect();
        assert_eq!(
            disk.all_indices
                .iter()
                .copied()
                .collect::<std::collections::HashSet<usize>>(),
            expected_disk_indices,
            "disk all_indices must match the union of its games' members"
        );
    }

    #[test]
    fn orphan_confidence_is_below_auto_select_threshold_for_every_kind() {
        // The orphan-residue safety contract: orphaned residue is shown but never
        // auto-selected, so a game installed past the launcher can't be
        // pre-checked for deletion. Enforced purely through confidence.
        for kind in [
            OrphanKind::UnmanagedFolder,
            OrphanKind::ServiceFolder,
            OrphanKind::UnreferencedFile,
        ] {
            assert!(orphan_confidence(kind) < AUTO_SELECT_CONFIDENCE_THRESHOLD);
            assert!(!default_selected(orphan_confidence(kind)));
        }
    }

    #[test]
    fn build_tree_merges_orphans_into_one_pseudo_game_per_disk() {
        // Two leftovers in different containers on the same disk, plus a real
        // game on that disk. The orphans must collapse into a single
        // ORPHAN_GAME_ID pseudo-game node (not one per container, not mixed
        // into the real game), and a leftover on another disk stays separate.
        let items = vec![
            item_at(
                1,
                "Real Game",
                "F:\\SteamLibrary\\steamapps\\common\\Real Game",
                "file.txt",
                FindingSource::Rule(Category::Bonus),
                90,
                10,
            ),
            orphan_item(
                "F:\\SteamLibrary\\steamapps\\common",
                "LeftoverA",
                OrphanKind::UnmanagedFolder,
                100,
            ),
            orphan_item(
                "F:\\SteamLibrary\\steamapps",
                "downloading",
                OrphanKind::ServiceFolder,
                50,
            ),
            orphan_item(
                "D:\\Games\\steamapps\\common",
                "LeftoverB",
                OrphanKind::UnmanagedFolder,
                7,
            ),
        ];

        let tree = build_tree(&items, GroupAxis::Disk);

        let disk_f = tree
            .iter()
            .find(|group| group.key.value() == "F:")
            .expect("disk F present");
        let orphan_games: Vec<&GameNode> = disk_f
            .games
            .iter()
            .filter(|game| is_orphan_branch(game.game_id))
            .collect();
        assert_eq!(
            orphan_games.len(),
            1,
            "both F: leftovers must merge into one orphan pseudo-game node"
        );
        let orphan_branch = orphan_games[0];
        assert_eq!(orphan_branch.categories.len(), 1);
        assert_eq!(
            orphan_branch.categories[0].category,
            Some(DisplayCategory::Orphan)
        );
        assert_eq!(
            orphan_branch.categories[0].all_indices.len(),
            2,
            "the two F: leftovers are both under the orphan branch"
        );
        assert_eq!(orphan_branch.total_bytes, 150);
        assert!(
            disk_f
                .games
                .iter()
                .any(|game| game.game_name == "Real Game"),
            "the real game must still be its own node, not swallowed by the orphan branch"
        );

        let disk_d = tree
            .iter()
            .find(|group| group.key.value() == "D:")
            .expect("disk D present");
        assert!(
            disk_d
                .games
                .iter()
                .any(|game| is_orphan_branch(game.game_id)),
            "the D: leftover forms its own orphan branch on its own disk"
        );
    }

    #[test]
    fn default_selected_applies_confidence_threshold() {
        assert!(default_selected(85));
        assert!(default_selected(95));
        assert!(!default_selected(84));
    }

    #[test]
    fn cautious_profile_selects_only_launcher_wont_restore_categories() {
        use DisplayCategory::*;
        // Bonus / Docs / Orphan at ANY confidence - the "safe" residue.
        for category in [Bonus, Docs, Orphan] {
            assert!(profile_auto_selects(
                SelectionProfile::Cautious,
                category,
                10
            ));
            assert!(profile_auto_selects(
                SelectionProfile::Cautious,
                category,
                95
            ));
        }
        // Everything else is left unchecked, even at high confidence.
        for category in [Loc, Intro, Redist, DevLeftovers] {
            assert!(!profile_auto_selects(
                SelectionProfile::Cautious,
                category,
                95
            ));
        }
    }

    #[test]
    fn balanced_profile_adds_localization_to_cautious() {
        use DisplayCategory::*;
        // Everything Cautious selects, plus Loc at any confidence.
        for category in [Bonus, Docs, Orphan, Loc] {
            assert!(profile_auto_selects(
                SelectionProfile::Balanced,
                category,
                10
            ));
        }
        // Still leaves redistributables and dev leftovers for the user.
        assert!(!profile_auto_selects(
            SelectionProfile::Balanced,
            Redist,
            95
        ));
        assert!(!profile_auto_selects(
            SelectionProfile::Balanced,
            DevLeftovers,
            95
        ));
    }

    /// The bug this guards against: a false-positive intro match is a unique
    /// video destroyed with no upstream copy, yet `Balanced` - the default
    /// profile - used to auto-select it at *any* confidence, same as the
    /// re-downloadable categories. Not even a maximal confidence should tick
    /// it under the default profile; only `Aggressive`, an explicit user
    /// escalation, may.
    #[test]
    fn balanced_profile_never_auto_selects_intro_regardless_of_confidence() {
        use DisplayCategory::Intro;
        assert!(!profile_auto_selects(SelectionProfile::Balanced, Intro, 10));
        assert!(!profile_auto_selects(
            SelectionProfile::Balanced,
            Intro,
            100
        ));
    }

    #[test]
    fn aggressive_profile_adds_everything_at_or_above_the_floor() {
        use DisplayCategory::*;
        // Safe categories, Loc and Intro still selected regardless of confidence.
        for category in [Bonus, Docs, Orphan, Loc, Intro] {
            assert!(profile_auto_selects(
                SelectionProfile::Aggressive,
                category,
                10
            ));
        }
        // Redist / DevLeftovers now come in - but only at or above the floor (70).
        assert!(profile_auto_selects(
            SelectionProfile::Aggressive,
            Redist,
            AGGRESSIVE_CONFIDENCE_FLOOR
        ));
        assert!(profile_auto_selects(
            SelectionProfile::Aggressive,
            DevLeftovers,
            90
        ));
        assert!(!profile_auto_selects(
            SelectionProfile::Aggressive,
            Redist,
            AGGRESSIVE_CONFIDENCE_FLOOR - 1
        ));
    }

    #[test]
    fn custom_profile_is_the_plain_confidence_threshold() {
        use DisplayCategory::*;
        // Category-agnostic: matches default_selected exactly.
        for category in [Bonus, Docs, Orphan, Loc, Intro, Redist, DevLeftovers] {
            assert_eq!(
                profile_auto_selects(SelectionProfile::Custom, category, 85),
                default_selected(85)
            );
            assert_eq!(
                profile_auto_selects(SelectionProfile::Custom, category, 84),
                default_selected(84)
            );
        }
    }

    #[test]
    fn only_a_profile_never_the_confidence_path_can_select_orphans() {
        use DisplayCategory::Orphan;
        // orphan-residue safety contract, now profile-scoped: the Custom (confidence-only) path
        // still never auto-selects orphaned residue (its confidence is < 85)...
        assert!(!profile_auto_selects(
            SelectionProfile::Custom,
            Orphan,
            ORPHAN_UNMANAGED_CONFIDENCE
        ));
        assert!(!profile_auto_selects(
            SelectionProfile::Custom,
            Orphan,
            ORPHAN_SERVICE_CONFIDENCE
        ));
        // ...but a chosen non-Custom profile may, by design (user's explicit call).
        for profile in [
            SelectionProfile::Cautious,
            SelectionProfile::Balanced,
            SelectionProfile::Aggressive,
        ] {
            assert!(profile_auto_selects(
                profile,
                Orphan,
                ORPHAN_UNMANAGED_CONFIDENCE
            ));
            assert!(profile_auto_selects(
                profile,
                Orphan,
                ORPHAN_SERVICE_CONFIDENCE
            ));
        }
    }

    #[test]
    fn risk_level_is_ordered_least_risky_first() {
        assert!(RiskLevel::None < RiskLevel::Low);
        assert!(RiskLevel::Low < RiskLevel::Medium);
    }

    #[test]
    fn category_risk_follows_the_curated_table() {
        use DisplayCategory::*;
        assert_eq!(category_risk(Orphan), RiskLevel::None);
        assert_eq!(category_risk(Redist), RiskLevel::None);
        assert_eq!(category_risk(Bonus), RiskLevel::Low);
        assert_eq!(category_risk(Docs), RiskLevel::Low);
        assert_eq!(category_risk(Loc), RiskLevel::Low);
        assert_eq!(
            category_risk(Intro),
            RiskLevel::Low,
            "a false-positive intro match destroys a unique video, unlike the zero-risk categories"
        );
        assert_eq!(category_risk(DevLeftovers), RiskLevel::Medium);
    }

    #[test]
    fn plan_cards_aggregates_by_category_and_orders_by_benefit_over_risk() {
        let items = vec![
            // Two zero-risk categories: orphan (bigger) should lead redist.
            item(
                ORPHAN_GAME_ID,
                "",
                FindingSource::Orphan(OrphanKind::UnmanagedFolder),
                ORPHAN_UNMANAGED_CONFIDENCE,
                500,
            ),
            item(
                1,
                "Game A",
                FindingSource::Rule(Category::RedistFolder),
                90,
                100,
            ),
            // One low-risk category spread across two games.
            item(1, "Game A", FindingSource::Loc(LangKind::Audio), 90, 300),
            item(2, "Game B", FindingSource::Loc(LangKind::Audio), 90, 200),
            // One medium-risk category.
            item(
                1,
                "Game A",
                FindingSource::Rule(Category::DevLeftovers),
                90,
                50,
            ),
        ];

        let cards = plan_cards(&items);

        let categories: Vec<DisplayCategory> = cards.iter().map(|c| c.category).collect();
        assert_eq!(
            categories,
            vec![
                DisplayCategory::Orphan,       // None, 500
                DisplayCategory::Redist,       // None, 100
                DisplayCategory::Loc,          // Low, 500
                DisplayCategory::DevLeftovers, // Medium, 50
            ],
            "least-risky first, biggest reclaim first within a risk band"
        );

        let loc = cards
            .iter()
            .find(|c| c.category == DisplayCategory::Loc)
            .expect("a Loc card");
        assert_eq!(loc.total_size_on_disk, 500);
        assert_eq!(loc.finding_count, 2);
        assert_eq!(loc.game_count, 2, "two distinct games contribute languages");
        assert_eq!(loc.risk, RiskLevel::Low);
    }

    #[test]
    fn plan_cards_excludes_removed_items_and_empty_categories() {
        let mut items = vec![
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 90, 100),
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 90, 100),
        ];
        items[0].removed = true;

        let cards = plan_cards(&items);

        assert_eq!(
            cards.len(),
            1,
            "only categories with a live finding get a card"
        );
        assert_eq!(cards[0].category, DisplayCategory::Bonus);
        assert_eq!(cards[0].finding_count, 1, "the removed item is excluded");
        assert_eq!(cards[0].total_size_on_disk, 100);
    }

    /// plan summary. The summary row states one game count for the whole plan, so it
    /// must count distinct games across categories - summing the per-card
    /// figures would report a game contributing to three categories as three
    /// games, inflating the headline number of a tool that deletes files.
    #[test]
    fn plan_totals_counts_each_game_once_across_categories() {
        let items = vec![
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 90, 100),
            item(1, "Game A", FindingSource::Loc(LangKind::Audio), 90, 300),
            item(
                1,
                "Game A",
                FindingSource::Rule(Category::DevLeftovers),
                90,
                50,
            ),
            item(2, "Game B", FindingSource::Loc(LangKind::Audio), 90, 200),
        ];

        let totals = plan_totals(&items);

        assert_eq!(totals.finding_count, 4);
        assert_eq!(
            totals.game_count, 2,
            "Game A contributes to three categories but is one game"
        );

        let summed_over_cards: usize = plan_cards(&items).iter().map(|c| c.game_count).sum();
        assert_ne!(
            summed_over_cards, totals.game_count,
            "precondition: this test is only meaningful while the naive sum differs"
        );
    }

    #[test]
    fn plan_totals_excludes_removed_items() {
        let mut items = vec![
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 90, 100),
            item(2, "Game B", FindingSource::Rule(Category::Bonus), 90, 100),
        ];
        items[1].removed = true;

        let totals = plan_totals(&items);

        assert_eq!(totals.finding_count, 1, "the removed item is gone");
        assert_eq!(
            totals.game_count, 1,
            "a game whose only finding was removed no longer contributes"
        );
    }

    #[test]
    fn plan_totals_of_nothing_is_zero() {
        assert_eq!(plan_totals(&[]), PlanTotals::default());
    }

    #[test]
    fn ui_aggregates_share_one_consistent_live_reclaimable_snapshot() {
        let mut items = vec![
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 90, 1_000),
            item(2, "Game B", FindingSource::Loc(LangKind::Audio), 90, 500),
            item(
                3,
                "Removed",
                FindingSource::Rule(Category::DocsFile),
                90,
                900,
            ),
        ];
        items[0].row.size_on_disk = 120;
        items[0].selected = true;
        items[1].row.size_on_disk = 80;
        items[1].selected = false;
        items[2].removed = true;
        items[2].selected = true;

        let aggregates = ui_aggregates(&items);

        assert_eq!(aggregates.totals.finding_count, 2);
        assert_eq!(aggregates.totals.game_count, 2);
        assert_eq!(aggregates.selected_count, 1);
        assert_eq!(aggregates.selected_bytes_on_disk, 120);
        assert_eq!(
            aggregates
                .cards
                .iter()
                .map(|card| card.total_size_on_disk)
                .sum::<u64>(),
            200,
            "cards and the selection summary use reclaimable bytes from the same snapshot"
        );
    }

    #[test]
    fn persisted_size_aggregates_saturate_instead_of_wrapping() {
        let mut items = vec![
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 90, 1),
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 90, 1),
        ];
        for item in &mut items {
            item.selected = true;
            item.row.size_on_disk = u64::MAX;
        }

        let aggregates = ui_aggregates(&items);
        assert_eq!(aggregates.cards[0].total_size_on_disk, u64::MAX);
        assert_eq!(aggregates.selected_bytes_on_disk, u64::MAX);
        assert_eq!(group_size_bytes(&items, &[0, 1]), u64::MAX);
        assert_eq!(
            Occupancy::from_by_library(HashMap::from([(1, u64::MAX), (2, 1)])).total,
            u64::MAX
        );
    }

    #[test]
    fn toggle_group_selects_all_then_deselects_all() {
        let mut items = vec![
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 50, 10),
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 50, 10),
        ];
        items[0].selected = false;
        items[1].selected = false;
        let indices = vec![0, 1];

        toggle_group(&mut items, &indices);
        assert!(
            items.iter().all(|i| i.selected),
            "toggling an unselected group selects all"
        );

        toggle_group(&mut items, &indices);
        assert!(
            items.iter().all(|i| !i.selected),
            "toggling a fully selected group deselects all"
        );
    }

    /// The half-ticked case, which the none-to-all-to-none walk above never
    /// reaches. A part-filled tri-state checkbox means "fill the rest"
    /// everywhere else in this program, and a header that threw the user's
    /// existing ticks away instead would lose work with no way to undo it.
    #[test]
    fn toggling_a_partly_selected_group_extends_the_selection_instead_of_clearing_it() {
        let mut items = vec![
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 50, 10),
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 50, 10),
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 50, 10),
        ];
        items[0].selected = true;
        items[1].selected = false;
        items[2].selected = false;
        let indices = vec![0, 1, 2];

        assert!(
            toggle_group(&mut items, &indices),
            "the click must report a change"
        );
        assert!(
            items.iter().all(|i| i.selected),
            "a partly selected group fills up; the one row already ticked keeps its tick"
        );
    }

    /// The tree's header rows are a bulk-selection path like any other, so an
    /// `imported_untrusted` row inside a game, category or folder must survive
    /// its header being toggled. Deselecting the same group still clears it -
    /// a row the user ticked by hand is theirs to untick in bulk.
    #[test]
    fn group_selection_never_selects_imported_untrusted_rows() {
        let mut items = vec![
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 50, 10),
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 50, 10),
        ];
        items[0].selected = false;
        items[1].selected = false;
        items[1].row.imported_untrusted = true;
        let indices = vec![0, 1];

        toggle_group(&mut items, &indices);
        assert!(items[0].selected);
        assert!(!items[1].selected, "the untrusted row stays untouched");
        assert_eq!(
            group_selection_state(&items, &indices),
            (true, true),
            "blocked rows do not leave an otherwise fully-selected group indeterminate"
        );

        items[1].selected = true;
        toggle_group(&mut items, &indices);
        assert!(
            items.iter().all(|i| !i.selected),
            "the second group toggle deselects both selectable and stale blocked selections"
        );
    }

    #[test]
    fn group_selection_state_detects_partial_selection() {
        let mut items = vec![
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 90, 10),
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 50, 10),
        ];
        items[0].selected = true;
        items[1].selected = false;

        let (all, any) = group_selection_state(&items, &[0, 1]);
        assert!(!all);
        assert!(any);
    }

    /// A group where every row is `imported_untrusted` has zero
    /// bulk-selectable rows - after the anti-cheat carve-out narrowed to
    /// monolithic archives only, this is the only remaining way to reach the
    /// zero-bulk-selectable fallback in `group_selection_state` (a protected
    /// monolithic archive is excluded from `individually_selectable` too, so
    /// it drops out of both tallies rather than exercising this path - see
    /// `bulk_selectable_excludes_blocked_and_untrusted_rows`). The old
    /// bulk-selectable-only tally reported `(false, false)` here - "nothing
    /// selected" - even after a deliberate hand tick, which is what let the
    /// header lie about a file that was really queued for deletion.
    #[test]
    fn group_selection_state_is_honest_when_every_row_is_imported_untrusted() {
        let mut items = vec![
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 90, 10),
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 90, 10),
        ];
        items[0].row.imported_untrusted = true;
        items[1].row.imported_untrusted = true;
        items[0].selected = false;
        items[1].selected = false;

        assert_eq!(
            group_selection_state(&items, &[0, 1]),
            (false, false),
            "an untouched all-imported-untrusted group has nothing selected yet"
        );

        items[0].selected = true;
        assert_eq!(
            group_selection_state(&items, &[0, 1]),
            (false, true),
            "a hand tick must be visible - not-all, but never nothing-selected"
        );

        items[1].selected = true;
        assert_eq!(
            group_selection_state(&items, &[0, 1]),
            (true, true),
            "once every row that can be selected has been hand-ticked, the group is complete"
        );
    }

    /// The residual ambiguity `toggle_group`'s doc comment names but the test
    /// above never exercises: an all-imported-untrusted group with one row
    /// hand-ticked renders indeterminate exactly like an ordinary partial
    /// selection, but a click here clears the tick instead of filling the
    /// rest of the group - there is nothing bulk-selectable to fill it with,
    /// so the select pass inside `toggle_group` is a no-op and the deselect
    /// pass is the only one that ever changes anything.
    #[test]
    fn toggle_group_clears_rather_than_fills_a_partly_ticked_all_imported_untrusted_group() {
        let mut items = vec![
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 90, 10),
            item(1, "Game A", FindingSource::Rule(Category::Bonus), 90, 10),
        ];
        items[0].row.imported_untrusted = true;
        items[1].row.imported_untrusted = true;
        items[0].selected = true;
        items[1].selected = false;
        let indices = vec![0, 1];

        assert_eq!(
            group_selection_state(&items, &indices),
            (false, true),
            "one hand tick out of an all-imported-untrusted pair reads indeterminate, same as an \
             ordinary partial selection"
        );

        assert!(
            toggle_group(&mut items, &indices),
            "the click must report a change"
        );
        assert!(
            items.iter().all(|i| !i.selected),
            "nothing here is bulk-selectable, so the click clears the hand tick instead of \
             filling item 1 the way an ordinary partial group would"
        );
    }

    #[test]
    fn group_size_uses_reclaimable_on_disk_bytes_not_logical_archive_size() {
        let mut items = vec![item(
            1,
            "Game A",
            FindingSource::Rule(Category::MonolithicArchive),
            90,
            10_000,
        )];
        items[0].row.size_on_disk = 750;

        assert_eq!(group_size_bytes(&items, &[0]), 750);
    }

    #[test]
    fn format_size_picks_appropriate_unit() {
        use crate::i18n::Lang;
        assert_eq!(format_size(Lang::Uk, 512), "512 Б");
        assert_eq!(format_size(Lang::Uk, 2048), "2.00 КБ");
        assert_eq!(format_size(Lang::Uk, 5 * 1024 * 1024), "5.00 МБ");
        assert_eq!(format_size(Lang::Uk, 3 * 1024 * 1024 * 1024), "3.00 ГБ");
        assert_eq!(format_size(Lang::En, 512), "512 B");
        assert_eq!(format_size(Lang::En, 2048), "2.00 KB");
    }

    #[test]
    fn category_display_covers_every_category() {
        use crate::i18n::Lang;
        assert_eq!(
            category_display(Lang::Uk, DisplayCategory::Redist),
            "Дистрибутиви"
        );
        assert_eq!(
            category_display(Lang::Uk, DisplayCategory::Docs),
            "Документація і довідкові матеріали"
        );
        assert_eq!(
            category_display(Lang::Uk, DisplayCategory::Bonus),
            "Бонусні матеріали"
        );
        assert_eq!(
            category_display(Lang::Uk, DisplayCategory::Loc),
            "Файли локалізацій"
        );
        assert_eq!(
            category_display(Lang::Uk, DisplayCategory::Archives),
            "Монолітні архіви"
        );
        assert_eq!(
            category_display(Lang::Uk, DisplayCategory::DevLeftovers),
            "Залишки розробки"
        );
        assert_eq!(
            category_display(Lang::Uk, DisplayCategory::Orphan),
            "Осиротіле"
        );
        assert_eq!(
            category_display(Lang::En, DisplayCategory::Redist),
            "Redistributables"
        );
        assert_eq!(
            category_display(Lang::En, DisplayCategory::Archives),
            "Monolithic Archives"
        );
        assert_eq!(
            category_display(Lang::En, DisplayCategory::Orphan),
            "Orphaned"
        );
    }

    #[test]
    fn category_enabled_treats_empty_list_as_all_enabled() {
        for category in CATEGORY_ORDER {
            assert!(category_enabled(&[], category));
        }
    }

    /// Dev leftovers were shown under "Other" until they got a name of their
    /// own, and a settings file written back then still lists the category by
    /// its old key. The one thing a rename must not do is switch a category
    /// off behind the back of a user who had explicitly switched it on: the
    /// finding simply stops appearing, which reads as broken detection rather
    /// than as a setting.
    #[test]
    fn a_settings_list_written_under_the_old_key_still_enables_dev_leftovers() {
        let legacy = vec!["redist".to_string(), "other".to_string()];

        assert!(category_enabled(&legacy, DisplayCategory::DevLeftovers));
        assert!(category_enabled(&legacy, DisplayCategory::Redist));
        assert!(!category_enabled(&legacy, DisplayCategory::Docs));
    }

    #[test]
    fn category_enabled_checks_membership_by_ui_key_when_list_is_non_empty() {
        let enabled = vec!["redist".to_string(), "docs".to_string()];
        assert!(category_enabled(&enabled, DisplayCategory::Redist));
        assert!(category_enabled(&enabled, DisplayCategory::Docs));
        assert!(!category_enabled(&enabled, DisplayCategory::Bonus));
        assert!(!category_enabled(&enabled, DisplayCategory::Loc));
        assert!(!category_enabled(&enabled, DisplayCategory::DevLeftovers));
        assert!(!category_enabled(&enabled, DisplayCategory::Orphan));
    }

    /// The scan worker dedups a file with both a rules-engine finding and a
    /// localization finding by keeping the higher-confidence one. This is
    /// the model-level contract that dedup logic in `worker::scan` relies
    /// on: whichever wins becomes the row's `source`/`rule_desc`/
    /// `confidence`/`lang_tag`, never both.
    #[test]
    fn dedup_by_file_keeps_the_higher_confidence_finding() {
        fn winner(rule_confidence: u8, lang_confidence: u8) -> FindingSource {
            // Mirrors `worker::scan::combine_finding`'s tie-break: rules win ties.
            if lang_confidence > rule_confidence {
                FindingSource::Loc(LangKind::Audio)
            } else {
                FindingSource::Rule(Category::Bonus)
            }
        }

        assert_eq!(winner(70, 95), FindingSource::Loc(LangKind::Audio));
        assert_eq!(winner(95, 70), FindingSource::Rule(Category::Bonus));
        assert_eq!(
            winner(90, 90),
            FindingSource::Rule(Category::Bonus),
            "ties favor the rules engine"
        );
    }

    #[test]
    fn file_row_confidence_label_includes_lang_tag_when_present() {
        let rule_row = item(1, "Game A", FindingSource::Rule(Category::Bonus), 90, 10);
        let loc_row = loc_item(1, "Game A", LangKind::Audio, "es", 90, 10);

        let rule_label = format!(
            "{}% \u{2014} {}",
            rule_row.row.confidence, rule_row.row.rule_desc
        );
        assert_eq!(rule_label, "90% \u{2014} test rule");
        assert!(rule_row.row.lang_tag.is_none());

        let loc_label = match &loc_row.row.lang_tag {
            Some(lang) => format!(
                "{}% [{}] \u{2014} {}",
                loc_row.row.confidence, lang, loc_row.row.rule_desc
            ),
            None => unreachable!("loc_item always sets lang_tag"),
        };
        assert_eq!(loc_label, "90% [es] \u{2014} маркер 'voices'");
    }

    #[test]
    fn occupancy_from_by_library_sums_total_and_looks_up_per_library_bytes() {
        let mut by_library = HashMap::new();
        by_library.insert(1i64, 100u64);
        by_library.insert(2i64, 250u64);

        let occupancy = Occupancy::from_by_library(by_library);

        assert_eq!(occupancy.total, 350);
        assert_eq!(occupancy.library_bytes(1), 100);
        assert_eq!(occupancy.library_bytes(2), 250);
        assert_eq!(
            occupancy.library_bytes(999),
            0,
            "an absent library id must report 0 bytes, not panic"
        );
    }

    #[test]
    fn occupancy_default_is_empty() {
        let occupancy = Occupancy::default();
        assert_eq!(occupancy.total, 0);
        assert_eq!(occupancy.library_bytes(1), 0);
    }

    #[test]
    fn freed_percent_handles_zero_total_without_dividing_by_zero() {
        assert_eq!(
            freed_percent(0, 0),
            0.0,
            "the 0-games edge case must report 0%, not NaN"
        );
        assert_eq!(freed_percent(100, 0), 0.0);
    }

    #[test]
    fn freed_percent_computes_expected_fractions() {
        assert_eq!(freed_percent(0, 1000), 0.0);
        assert_eq!(freed_percent(500, 1000), 50.0);
        assert_eq!(freed_percent(1000, 1000), 100.0);
    }

    #[test]
    fn freed_percent_clamps_above_100_when_selected_exceeds_total() {
        assert_eq!(
            freed_percent(1500, 1000),
            100.0,
            "selected bytes should never be able to report more than 100%"
        );
    }

    #[test]
    fn format_duration_zero_is_zero_seconds() {
        assert_eq!(format_duration(std::time::Duration::from_secs(0)), "0s");
    }

    #[test]
    fn format_duration_sub_minute_shows_bare_seconds() {
        assert_eq!(format_duration(std::time::Duration::from_secs(3)), "3s");
        assert_eq!(format_duration(std::time::Duration::from_secs(59)), "59s");
    }

    #[test]
    fn format_duration_exactly_sixty_seconds_rolls_over_to_minutes() {
        assert_eq!(format_duration(std::time::Duration::from_secs(60)), "1:00");
    }

    #[test]
    fn format_duration_minutes_and_seconds_are_zero_padded() {
        assert_eq!(format_duration(std::time::Duration::from_secs(92)), "1:32");
        assert_eq!(
            format_duration(std::time::Duration::from_secs(605)),
            "10:05"
        );
    }

    #[test]
    fn format_duration_hours_minutes_seconds() {
        assert_eq!(
            format_duration(std::time::Duration::from_secs(3800)),
            "1:03:20"
        );
    }

    #[test]
    fn format_duration_ignores_sub_second_precision() {
        assert_eq!(
            format_duration(std::time::Duration::from_millis(3999)),
            "3s"
        );
    }

    // -- grouping axes (GT-35) --

    /// Four findings across two launchers, two libraries and two disks, so no
    /// two axes can accidentally agree on the same shape:
    ///
    /// - Steam, `C:\SteamLibrary`, disk `C:` - two games
    /// - Steam, `D:\SteamLibrary`, disk `D:` - one game (same launcher, other
    ///   library and other disk)
    /// - GOG, `C:\GOG`, disk `C:` - one game (same disk as the first two)
    fn cross_axis_items() -> Vec<FindingItem> {
        vec![
            with_library(
                item_at(
                    1,
                    "Alpha",
                    "C:\\SteamLibrary\\Alpha",
                    "a.pak",
                    FindingSource::Rule(Category::Bonus),
                    90,
                    100,
                ),
                Some("steam"),
                "C:\\SteamLibrary",
            ),
            with_library(
                item_at(
                    2,
                    "Beta",
                    "C:\\SteamLibrary\\Beta",
                    "b.pak",
                    FindingSource::Rule(Category::Bonus),
                    90,
                    200,
                ),
                Some("steam"),
                "C:\\SteamLibrary",
            ),
            with_library(
                item_at(
                    3,
                    "Gamma",
                    "D:\\SteamLibrary\\Gamma",
                    "c.pak",
                    FindingSource::Rule(Category::Bonus),
                    90,
                    300,
                ),
                Some("steam"),
                "D:\\SteamLibrary",
            ),
            with_library(
                item_at(
                    4,
                    "Delta",
                    "C:\\GOG\\Delta",
                    "d.pak",
                    FindingSource::Rule(Category::Bonus),
                    90,
                    400,
                ),
                Some("gog"),
                "C:\\GOG",
            ),
        ]
    }

    /// The branch headings a tree produces, in tree order.
    fn branch_values(tree: &[TopGroup]) -> Vec<String> {
        tree.iter()
            .map(|group| group.key.value().into_owned())
            .collect()
    }

    /// Every `findings` index the whole tree reaches, sorted - what the screen
    /// can actually show.
    fn reachable_indices(tree: &[TopGroup]) -> Vec<usize> {
        let mut indices: Vec<usize> = tree
            .iter()
            .flat_map(|group| group.all_indices.iter().copied())
            .collect();
        indices.sort_unstable();
        indices
    }

    #[test]
    fn the_launcher_axis_merges_one_launchers_libraries_into_one_branch() {
        let items = cross_axis_items();

        let tree = build_tree(&items, GroupAxis::Launcher);

        assert_eq!(
            branch_values(&tree),
            vec!["gog", "steam"],
            "two Steam libraries on two disks are still one launcher",
        );
        let steam = tree.iter().find(|g| g.key.value() == "steam").unwrap();
        assert_eq!(steam.games.len(), 3);
    }

    /// The half the launcher axis cannot answer: which of a launcher's roots a
    /// game is actually in. Asserted against the same fixture, so a
    /// `build_tree` that ignored the axis and grouped by vendor either way
    /// would fail here.
    #[test]
    fn the_library_axis_splits_one_launchers_roots_into_separate_branches() {
        let items = cross_axis_items();

        let tree = build_tree(&items, GroupAxis::Library);

        assert_eq!(
            branch_values(&tree),
            vec!["C:\\GOG", "C:\\SteamLibrary", "D:\\SteamLibrary"],
        );
    }

    /// The disk axis reads the file's own path, not the library root - which
    /// is why a library root and its games' disk can disagree without either
    /// being wrong.
    #[test]
    fn the_disk_axis_still_groups_by_the_files_own_volume() {
        let items = cross_axis_items();

        let tree = build_tree(&items, GroupAxis::Disk);

        assert_eq!(branch_values(&tree), vec!["C:", "D:"]);
    }

    /// The invariant the switcher rests on: a different cut of the same
    /// findings is *the same findings*. If any axis could drop a row, the tree
    /// would quietly shrink on a switch and read as broken detection.
    #[test]
    fn no_axis_loses_a_finding_even_when_it_cannot_attribute_it() {
        let mut items = cross_axis_items();
        // A row from a database written before the attribution existed, and a
        // root that resolved with no `game_libraries` row behind it - the two
        // shapes `FindingRow::library` documents as possible.
        items.push(item_at(
            5,
            "Epsilon",
            "E:\\Games\\Epsilon",
            "e.pak",
            FindingSource::Rule(Category::Bonus),
            90,
            500,
        ));
        items.push(with_library(
            item_at(
                6,
                "Zeta",
                "E:\\Games\\Zeta",
                "f.pak",
                FindingSource::Rule(Category::Bonus),
                90,
                600,
            ),
            None,
            "E:\\Games",
        ));
        let all: Vec<usize> = (0..items.len()).collect();

        for axis in GROUP_AXIS_ORDER {
            assert_eq!(
                reachable_indices(&build_tree(&items, axis)),
                all,
                "grouping by {axis:?} must reach every finding",
            );
        }
    }

    /// Unattributed rows get a branch of their own, and it is the last one -
    /// leftovers belong at the bottom of the tree, not sorted into the middle
    /// of it by an empty name.
    #[test]
    fn unattributed_rows_form_the_last_branch_rather_than_vanishing() {
        let mut items = cross_axis_items();
        items.push(item_at(
            5,
            "Epsilon",
            "E:\\Games\\Epsilon",
            "e.pak",
            FindingSource::Rule(Category::Bonus),
            90,
            500,
        ));

        let tree = build_tree(&items, GroupAxis::Launcher);

        assert_eq!(
            tree.last().map(|group| group.key.clone()),
            Some(TopKey::Unattributed(GroupAxis::Launcher)),
        );
        assert_eq!(tree.last().unwrap().games.len(), 1);
    }

    /// A root with no vendor behind it is unattributed on the launcher axis
    /// and perfectly placed on the library axis - the two halves of
    /// `LibraryOrigin` are independent, and one being absent must not cost the
    /// other.
    #[test]
    fn a_root_without_a_vendor_still_groups_by_library() {
        let items = vec![with_library(
            item_at(
                1,
                "Alpha",
                "E:\\Games\\Alpha",
                "a.pak",
                FindingSource::Rule(Category::Bonus),
                90,
                100,
            ),
            None,
            "E:\\Games",
        )];

        assert_eq!(
            build_tree(&items, GroupAxis::Launcher)[0].key,
            TopKey::Unattributed(GroupAxis::Launcher),
        );
        assert_eq!(
            build_tree(&items, GroupAxis::Library)[0].key,
            TopKey::Library(PathBuf::from("E:\\Games")),
        );
    }

    /// GT-35's second pitfall: the collapse keys encode the branch's value, so
    /// without the axis in them "disk E:" and "library E:" would be one key and
    /// the expand state of one axis would leak into the next.
    #[test]
    fn collapse_keys_of_two_axes_never_collide_on_the_same_value() {
        let disk = TopKey::Disk("E:".to_string());
        let library = TopKey::Library(PathBuf::from("E:"));
        let launcher = TopKey::Launcher("E:".to_string());

        let keys = [
            disk.collapse_key(),
            library.collapse_key(),
            launcher.collapse_key(),
            TopKey::Unattributed(GroupAxis::Launcher).collapse_key(),
            TopKey::Unattributed(GroupAxis::Library).collapse_key(),
        ];

        let distinct: std::collections::HashSet<&String> = keys.iter().collect();
        assert_eq!(
            distinct.len(),
            keys.len(),
            "collapse keys collide: {keys:?}"
        );
    }

    /// The orphan branch is one pseudo-game per *branch*, not per disk (see
    /// [`ORPHAN_GAME_ID`]) - so switching axes moves it with the rest of the
    /// tree instead of stranding it under a heading it no longer belongs to.
    #[test]
    fn the_orphan_branch_follows_the_active_axis() {
        let items = vec![
            with_library(
                orphan_item(
                    "C:\\SteamLibrary\\steamapps",
                    "downloading",
                    OrphanKind::ServiceFolder,
                    100,
                ),
                Some("steam"),
                "C:\\SteamLibrary",
            ),
            with_library(
                orphan_item("C:\\GOG", "Leftover", OrphanKind::UnmanagedFolder, 200),
                Some("gog"),
                "C:\\GOG",
            ),
        ];

        // Both orphans sit on C:, so the disk axis merges them into one
        // pseudo-game...
        let by_disk = build_tree(&items, GroupAxis::Disk);
        assert_eq!(by_disk.len(), 1);
        assert_eq!(by_disk[0].games.len(), 1);
        assert!(is_orphan_branch(by_disk[0].games[0].game_id));

        // ...while the launcher axis puts one under each launcher.
        let by_launcher = build_tree(&items, GroupAxis::Launcher);
        assert_eq!(branch_values(&by_launcher), vec!["gog", "steam"]);
        for group in &by_launcher {
            assert_eq!(group.games.len(), 1);
            assert!(is_orphan_branch(group.games[0].game_id));
        }
    }

    /// The category axis lifts one category out of every game into one branch -
    /// which is the cut the disk axis cannot express, since a game's
    /// localizations are scattered one game at a time under it.
    #[test]
    fn the_category_axis_gathers_one_category_across_every_game() {
        let items = vec![
            item(1, "Alpha", FindingSource::Rule(Category::Bonus), 90, 100),
            item(2, "Beta", FindingSource::Rule(Category::Bonus), 90, 200),
            item(3, "Gamma", FindingSource::Loc(LangKind::Text), 90, 300),
        ];

        let tree = build_tree(&items, GroupAxis::Category);

        assert_eq!(branch_values(&tree), vec!["bonus", "loc"]);
        let bonus = &tree[0];
        assert_eq!(bonus.key, TopKey::Category(DisplayCategory::Bonus));
        assert_eq!(bonus.games.len(), 2, "both games' bonus material, together");
        // Each branch keeps exactly one category node, and it carries the
        // branch's own category - the per-game category row folds away when
        // drawn precisely because it would say this twice.
        for group in &tree {
            for game in &group.games {
                assert_eq!(game.categories.len(), 1);
                assert_eq!(
                    game.categories[0].category,
                    Some(match &group.key {
                        TopKey::Category(category) => *category,
                        other => panic!("expected a category branch, got {other:?}"),
                    })
                );
            }
        }
    }

    /// Branches follow [`CATEGORY_ORDER`] - the cleanup priority the tree is
    /// built to communicate - not the alphabetical order of their keys, which
    /// would put "bonus" ahead of "redist".
    #[test]
    fn category_branches_follow_the_taxonomy_order_not_their_keys() {
        let items = vec![
            item(1, "Alpha", FindingSource::Loc(LangKind::Text), 90, 100),
            item(
                2,
                "Beta",
                FindingSource::Rule(Category::RedistFolder),
                90,
                200,
            ),
            item(3, "Gamma", FindingSource::Rule(Category::Bonus), 90, 300),
        ];

        let tree = build_tree(&items, GroupAxis::Category);

        assert_eq!(
            branch_values(&tree),
            vec!["redist", "bonus", "loc"],
            "CATEGORY_ORDER is redist, intro, docs, bonus, loc, other, orphan",
        );
    }

    /// The flat axis is one branch, one synthetic game and one node list. All
    /// three are what let an explicit sort order the whole result set instead
    /// of ordering inside each game or each category.
    #[test]
    fn the_flat_axis_collapses_the_whole_tree_into_one_node_list() {
        let items = cross_axis_items();

        let tree = build_tree(&items, GroupAxis::Flat);

        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].key, TopKey::Flat);
        assert_eq!(tree[0].games.len(), 1);
        assert_eq!(tree[0].games[0].game_id, FLAT_GAME_ID);
        assert!(
            tree[0].games[0].game_name.is_empty(),
            "the synthetic game borrows no real game's name",
        );
        assert_eq!(tree[0].games[0].categories.len(), 1);
        assert_eq!(tree[0].games[0].categories[0].category, None);
        assert_eq!(tree[0].games[0].categories[0].nodes.len(), items.len());
    }

    /// The card's own reason for the axis: in a hierarchy a large file stays
    /// buried under its folder node and only the folder's total competes, so
    /// "biggest first" cannot reach it. Every finding here shares one folder,
    /// which under any other axis is a single row.
    #[test]
    fn the_flat_axis_dissolves_folders_so_a_size_sort_reaches_every_file() {
        let items = vec![
            with_group_dir(
                item_at(
                    1,
                    "Alpha",
                    "C:\\Games\\Alpha",
                    "data\\small.bin",
                    FindingSource::Rule(Category::Bonus),
                    90,
                    10,
                ),
                "data",
            ),
            with_group_dir(
                item_at(
                    1,
                    "Alpha",
                    "C:\\Games\\Alpha",
                    "data\\huge.bin",
                    FindingSource::Rule(Category::Bonus),
                    90,
                    9_000,
                ),
                "data",
            ),
        ];

        // One folder node under the disk axis - the files are not rows there.
        let by_disk = build_tree(&items, GroupAxis::Disk);
        assert_eq!(by_disk[0].games[0].categories[0].nodes.len(), 1);

        let mut flat = build_tree(&items, GroupAxis::Flat);
        let nodes = &flat[0].games[0].categories[0].nodes;
        assert_eq!(nodes.len(), 2);
        assert!(
            nodes
                .iter()
                .all(|node| matches!(node, TreeNode::File { .. })),
            "the flat axis must leave no folder node for a file to hide in",
        );

        sort_tree(
            &mut flat,
            &items,
            Some(TreeSort {
                column: SortColumn::Size,
                descending: true,
            }),
        );
        let TreeNode::File { index } = flat[0].games[0].categories[0].nodes[0] else {
            panic!("the flat axis produces only file nodes");
        };
        assert_eq!(items[index].row.rel_path, "data\\huge.bin");
    }

    /// An explicit sort applies to every row, the unattributed branch
    /// included: under "by size" a heading that ignored the column would be one
    /// the screen cannot account for.
    #[test]
    fn an_explicit_sort_orders_the_unattributed_branch_with_the_rest() {
        let mut items = cross_axis_items();
        // Bigger than any attributed branch, so "largest first" has to put it
        // at the top - which the default order never would.
        items.push(item_at(
            5,
            "Epsilon",
            "E:\\Games\\Epsilon",
            "e.pak",
            FindingSource::Rule(Category::Bonus),
            90,
            9_000,
        ));

        let mut tree = build_tree(&items, GroupAxis::Launcher);
        assert_eq!(
            tree.last().map(|group| group.key.clone()),
            Some(TopKey::Unattributed(GroupAxis::Launcher)),
            "the default order pins it last",
        );

        sort_tree(
            &mut tree,
            &items,
            Some(TreeSort {
                column: SortColumn::Size,
                descending: true,
            }),
        );

        assert_eq!(
            tree.first().map(|group| group.key.clone()),
            Some(TopKey::Unattributed(GroupAxis::Launcher)),
        );
    }
}
