//! UI-side data model: findings grouped for the tree view, plus selection
//! and formatting helpers. Nothing here touches the database directly.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf, Prefix};

use gametrimmer_core::langdetect::LangKind;
use gametrimmer_core::orphans::OrphanKind;
use gametrimmer_core::rules::Category;
use gametrimmer_core::settings::SelectionProfile;

/// Granular source of a finding: a rules-engine category (redist, docs,
/// bonus, ...), a localization-detector kind (audio, text, video, font,
/// unknown), or an orphaned-residue kind (GT-02: a folder inside a launcher's
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
    Docs,
    Bonus,
    Loc,
    Other,
    /// Orphaned launcher residue (GT-02) - shown under the per-disk pseudo-game
    /// branch ([`ORPHAN_GAME_ID`]), never mixed into a real game's categories.
    Orphan,
}

/// One classified file, as produced by the scan worker.
#[derive(Debug, Clone)]
pub struct FindingRow {
    pub file_id: i64,
    /// The owning game's id, or the [`ORPHAN_GAME_ID`] sentinel for orphaned
    /// residue (GT-02), which has no game behind it.
    pub game_id: i64,
    /// The owning game's display name. Empty for orphan rows - the tree renders
    /// the orphan branch with a localized label keyed off [`ORPHAN_GAME_ID`]
    /// instead (see `ui::tree_view`).
    pub game_name: String,
    pub install_dir: PathBuf,
    pub rel_path: String,
    /// Logical size (bytes) - shown as a secondary figure (tooltip) since it
    /// isn't what deleting actually reclaims.
    pub size: u64,
    /// On-disk allocated size (bytes) - the honest "space freed" figure and the
    /// one shown as primary and summed for totals/estimates (GT-05a). Falls
    /// back to `size` for rows loaded from a pre-GT-05a database (see
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
    /// exists (an "orphan" finding, shown as its own row). UI-only grouping
    /// metadata, computed by `worker::scan::assign_group_dirs` - never
    /// persisted to the database.
    pub group_dir: Option<String>,
}

impl FindingRow {
    /// The coarse category this row is grouped under in the tree.
    pub fn display_category(&self) -> DisplayCategory {
        display_category(self.source)
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
/// the other five is immaterial - but it must still be listed so the settings
/// dialog offers a checkbox for it and [`category_enabled`] can gate it.
pub const CATEGORY_ORDER: [DisplayCategory; 6] = [
    DisplayCategory::Redist,
    DisplayCategory::Docs,
    DisplayCategory::Bonus,
    DisplayCategory::Loc,
    DisplayCategory::Other,
    DisplayCategory::Orphan,
];

/// Synthetic `game_id` shared by every orphaned-residue finding (GT-02). Real
/// game ids are SQLite rowids (always `>= 1`), so a single reserved negative
/// sentinel can never collide with one. Because [`build_tree`] groups by
/// `(disk, game_id)`, giving every orphan on a disk the same sentinel merges
/// them all into exactly one "orphaned residue" pseudo-game node per disk,
/// beside the real games - which is the separate tree branch GT-02 calls for.
/// The rows themselves are persisted with a `NULL` `files.game_id` (there is
/// no game), and reconstructed with this sentinel at scan/load time.
pub const ORPHAN_GAME_ID: i64 = i64::MIN;

/// Whether `game_id` is the orphan-branch sentinel (see [`ORPHAN_GAME_ID`]) -
/// the tree renders such a node with a localized branch label instead of a
/// quoted game name.
pub fn is_orphan_branch(game_id: i64) -> bool {
    game_id == ORPHAN_GAME_ID
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

/// The confidence [`FindingSource::Orphan`] carries for a given kind.
pub fn orphan_confidence(kind: OrphanKind) -> u8 {
    match kind {
        OrphanKind::UnmanagedFolder => ORPHAN_UNMANAGED_CONFIDENCE,
        OrphanKind::ServiceFolder => ORPHAN_SERVICE_CONFIDENCE,
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
        FindingSource::Rule(Category::DevLeftovers) => DisplayCategory::Other,
        FindingSource::Loc(_) => DisplayCategory::Loc,
        FindingSource::Orphan(_) => DisplayCategory::Orphan,
    }
}

/// Human-readable, localized label for a category header.
pub fn category_display(lang: crate::i18n::Lang, category: DisplayCategory) -> &'static str {
    let s = crate::i18n::strings(lang);
    match category {
        DisplayCategory::Redist => s.category_redist,
        DisplayCategory::Docs => s.category_docs,
        DisplayCategory::Bonus => s.category_bonus,
        DisplayCategory::Loc => s.category_loc,
        DisplayCategory::Other => s.category_other,
        DisplayCategory::Orphan => s.category_orphan,
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
        FindingSource::Rule(Category::DevLeftovers) => "dev_leftovers",
        FindingSource::Loc(LangKind::Audio) => "loc_audio",
        FindingSource::Loc(LangKind::Text) => "loc_text",
        FindingSource::Loc(LangKind::Video) => "loc_video",
        FindingSource::Loc(LangKind::Font) => "loc_font",
        FindingSource::Loc(LangKind::Graphic) => "loc_graphic",
        FindingSource::Loc(LangKind::Unknown) => "loc_unknown",
        FindingSource::Orphan(OrphanKind::UnmanagedFolder) => "orphan_folder",
        FindingSource::Orphan(OrphanKind::ServiceFolder) => "orphan_service",
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
        "dev_leftovers" => Some(FindingSource::Rule(Category::DevLeftovers)),
        "loc_audio" => Some(FindingSource::Loc(LangKind::Audio)),
        "loc_text" => Some(FindingSource::Loc(LangKind::Text)),
        "loc_video" => Some(FindingSource::Loc(LangKind::Video)),
        "loc_font" => Some(FindingSource::Loc(LangKind::Font)),
        "loc_graphic" => Some(FindingSource::Loc(LangKind::Graphic)),
        "loc_unknown" => Some(FindingSource::Loc(LangKind::Unknown)),
        "orphan_folder" => Some(FindingSource::Orphan(OrphanKind::UnmanagedFolder)),
        "orphan_service" => Some(FindingSource::Orphan(OrphanKind::ServiceFolder)),
        _ => None,
    }
}

/// Stable short key for a display category, used for egui persistent ids
/// (collapsing header open/closed state) instead of the Ukrainian label.
pub fn category_ui_key(category: DisplayCategory) -> &'static str {
    match category {
        DisplayCategory::Redist => "redist",
        DisplayCategory::Docs => "docs",
        DisplayCategory::Bonus => "bonus",
        DisplayCategory::Loc => "loc",
        DisplayCategory::Other => "other",
        DisplayCategory::Orphan => "orphan",
    }
}

/// Whether `category` should be kept by the scan, given the persisted
/// `enabled_categories` setting (see `gametrimmer_core::settings::Settings`).
/// An empty `enabled_categories` list means every category is enabled - see
/// that field's doc comment for why an empty list isn't "nothing enabled".
pub fn category_enabled(enabled_categories: &[String], category: DisplayCategory) -> bool {
    enabled_categories.is_empty()
        || enabled_categories
            .iter()
            .any(|id| id == category_ui_key(category))
}

/// Default selection policy (docs/04 §5.5): auto-select only high-confidence
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
/// `profile` (GT-04). A pure policy over already-scanned findings, so switching
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
/// Note (GT-02): unlike the confidence-threshold path, a profile *can*
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
        SelectionProfile::Balanced => is_safe_category || category == DisplayCategory::Loc,
        SelectionProfile::Aggressive => {
            is_safe_category
                || category == DisplayCategory::Loc
                || confidence >= AGGRESSIVE_CONFIDENCE_FLOOR
        }
        SelectionProfile::Custom => default_selected(confidence),
    }
}

/// Coarse deletion-risk band shown on a [`PlanCard`] (GT-03). Deliberately a
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
/// (GT-03). See [`RiskLevel`] for why this is a hand-tuned table rather than a
/// function of confidence.
pub fn category_risk(category: DisplayCategory) -> RiskLevel {
    match category {
        // Orphaned residue: the game is already uninstalled. Redist: MSVC/DX
        // installers a game re-runs or the store re-fetches on demand.
        DisplayCategory::Orphan | DisplayCategory::Redist => RiskLevel::None,
        DisplayCategory::Bonus | DisplayCategory::Docs | DisplayCategory::Loc => RiskLevel::Low,
        // Dev leftovers (PDBs, editor junk): almost always disposable, but the
        // one category where a false positive is plausible enough to flag.
        DisplayCategory::Other => RiskLevel::Medium,
    }
}

/// One aggregated action on the "plan of action" screen (GT-03): a whole
/// display category rolled up across every disk and game, with the total space
/// it would reclaim and its curated risk band. Built by [`plan_cards`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanCard {
    pub category: DisplayCategory,
    /// Total on-disk allocation across the category's findings (GT-05a) - the
    /// honest reclaimable figure, matching the tree and bottom-bar totals.
    pub total_size_on_disk: u64,
    /// How many findings the category holds.
    pub finding_count: usize,
    /// How many distinct games contribute (the orphan branch counts as one),
    /// so the card can say "unused languages in N games".
    pub game_count: usize,
    pub risk: RiskLevel,
}

/// Rolls the current findings up into one [`PlanCard`] per non-empty display
/// category, ordered "benefit ÷ risk": least-risky first, and within a risk
/// band the biggest reclaim first. Removed items are excluded (they are gone).
/// A pure function of the findings, so it is cheap to recompute each frame and
/// unit-testable without any UI.
pub fn plan_cards(items: &[FindingItem]) -> Vec<PlanCard> {
    use std::collections::HashSet;

    // (total_on_disk, finding_count, distinct game ids) per category.
    let mut totals: HashMap<DisplayCategory, (u64, usize, HashSet<i64>)> = HashMap::new();
    for item in items {
        if item.removed {
            continue;
        }
        let entry = totals.entry(item.row.display_category()).or_default();
        entry.0 += item.row.size_on_disk;
        entry.1 += 1;
        entry.2.insert(item.row.game_id);
    }

    let mut cards: Vec<PlanCard> = totals
        .into_iter()
        .map(|(category, (size, count, games))| PlanCard {
            category,
            total_size_on_disk: size,
            finding_count: count,
            game_count: games.len(),
            risk: category_risk(category),
        })
        .collect();

    // Least-risky first (RiskLevel is ordered), then biggest reclaim first
    // within a band - so a zero-risk, high-payoff card leads the plan.
    cards.sort_by(|a, b| {
        a.risk
            .cmp(&b.risk)
            .then(b.total_size_on_disk.cmp(&a.total_size_on_disk))
    });
    cards
}

/// The whole-plan roll-up behind the one-line summary above the tree
/// (GT-12). Built by [`plan_totals`].
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
pub fn plan_totals(items: &[FindingItem]) -> PlanTotals {
    use std::collections::HashSet;

    let live = items.iter().filter(|item| !item.removed);
    let mut games: HashSet<i64> = HashSet::new();
    let mut finding_count = 0usize;

    for item in live {
        finding_count += 1;
        games.insert(item.row.game_id);
    }

    PlanTotals {
        finding_count,
        game_count: games.len(),
    }
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
    pub category: DisplayCategory,
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

/// One physical disk's games, largest first.
#[derive(Debug, Clone)]
pub struct DiskGroup {
    pub disk: String,
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
fn disk_label(install_dir: &Path) -> String {
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
        *bytes_by_category.entry(row.display_category()).or_insert(0) += row.size_on_disk;
    }

    let mut best = CATEGORY_ORDER[0];
    let mut best_bytes = bytes_by_category.get(&best).copied().unwrap_or(0);
    for &category in &CATEGORY_ORDER[1..] {
        let bytes = bytes_by_category.get(&category).copied().unwrap_or(0);
        if bytes > best_bytes {
            best = category;
            best_bytes = bytes;
        }
    }
    best
}

/// Total bytes represented by a tree node - a folder's precomputed total, or
/// a single file's size - used to sort nodes within a category. On-disk size,
/// to match the figure shown and summed everywhere else (GT-05a).
fn node_bytes(items: &[FindingItem], node: &TreeNode) -> u64 {
    match node {
        TreeNode::Folder { total_bytes, .. } => *total_bytes,
        TreeNode::File { index } => items[*index].row.size_on_disk,
    }
}

/// All flat `findings` indices held under one node - a folder's whole
/// member list, or a single orphan file's index. Used to build
/// `CategoryNode::all_indices`/`DiskGroup::all_indices` once in
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

/// Builds one game's category list (display order; within each category,
/// folders first - largest total first - then individual files by path).
fn build_game_categories(items: &[FindingItem], bucket: GameBucket) -> Vec<CategoryNode> {
    let mut nodes_by_category: HashMap<DisplayCategory, Vec<TreeNode>> = HashMap::new();

    for (group_dir, mut indices) in bucket.folders {
        // Member files are collected in scan order (insertion order into the
        // group_dir bucket), which for the MFT scan path is not path order -
        // sort explicitly so the folder's children always read top-to-bottom
        // by path regardless of how the scan visited them.
        indices.sort_by(|&a, &b| path_cmp(&items[a].row.rel_path, &items[b].row.rel_path));
        let total_bytes = group_size_bytes(items, &indices);
        let category = majority_category(items, &indices);
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
        nodes_by_category
            .entry(items[index].row.display_category())
            .or_default()
            .push(TreeNode::File { index });
    }

    CATEGORY_ORDER
        .iter()
        .filter_map(|&category| {
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

/// Rebuilds the disk -> game -> category -> folder/file tree from scratch,
/// skipping removed items. Cheap enough to call after every scan/delete
/// completion.
///
/// Every game appears exactly once under its disk, holding all of its
/// findings - the tree never scatters one game's rows across the disk.
/// Within a game, every flagged file with a `group_dir` (see
/// `worker::scan::assign_group_dirs`) is merged into one `TreeNode::Folder`
/// per `group_dir`, placed under the single display category holding the
/// majority of that folder's bytes (`majority_category`) - this is what
/// keeps a shared folder from appearing in more than one category. Findings
/// without a `group_dir` become standalone `TreeNode::File` nodes in their
/// own display category. Within a category, folders precede individual
/// files (see `build_game_categories`); within a folder, member files are
/// ordered by path (see `path_cmp`).
/// Order-sensitive fingerprint of which findings are currently checked.
///
/// Used to notice that the user edited the selection without having to hook
/// every place that can edit it. The tree mutates `findings` through a
/// disjoint borrow in a dozen places - per-file checkbox, tri-state group
/// checkboxes at four levels, keyboard toggle, four context-menu actions -
/// none of which has `&mut GameTrimmerApp` to call a setter on. Enumerating
/// them is exactly the kind of hand-maintained list that goes stale (see
/// `GameTrimmerApp::any_modal_open` for the same lesson), so `ui::tree_view`
/// compares this before and after its own rendering pass instead.
///
/// FNV-1a over one byte per finding: one cheap pass, and the tree already
/// walks every finding each frame to flatten its visible rows.
///
/// `removed` is folded in as its own bit so a mid-delete `FileRemoved` is not
/// mistaken for a hand-edit - though in practice those arrive in
/// `drain_messages`, outside the window this is compared across.
pub fn selection_fingerprint(items: &[FindingItem]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    items.iter().fold(FNV_OFFSET, |hash, item| {
        let bits = u64::from(item.selected) | (u64::from(item.removed) << 1);
        (hash ^ bits).wrapping_mul(FNV_PRIME)
    })
}

pub fn build_tree(items: &[FindingItem]) -> Vec<DiskGroup> {
    let mut game_buckets: HashMap<(String, i64), GameBucket> = HashMap::new();

    for (index, item) in items.iter().enumerate() {
        if item.removed {
            continue;
        }
        let disk = disk_label(&item.row.install_dir);
        let bucket = game_buckets
            .entry((disk, item.row.game_id))
            .or_insert_with(|| GameBucket {
                game_name: item.row.game_name.clone(),
                folders: HashMap::new(),
                orphans: Vec::new(),
            });
        match &item.row.group_dir {
            Some(dir) => bucket.folders.entry(dir.clone()).or_default().push(index),
            None => bucket.orphans.push(index),
        }
    }

    let mut games_by_disk: HashMap<String, Vec<GameNode>> = HashMap::new();
    for ((disk, game_id), bucket) in game_buckets {
        let game_name = bucket.game_name.clone();
        let categories = build_game_categories(items, bucket);
        let all_indices: Vec<usize> = categories
            .iter()
            .flat_map(|category_node| category_node.all_indices.iter().copied())
            .collect();
        let total_bytes = group_size_bytes(items, &all_indices);
        games_by_disk.entry(disk).or_default().push(GameNode {
            game_id,
            game_name,
            categories,
            all_indices,
            total_bytes,
        });
    }

    let mut disks: Vec<DiskGroup> = games_by_disk
        .into_iter()
        .map(|(disk, mut games)| {
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
            DiskGroup {
                disk,
                games,
                all_indices,
                total_bytes,
            }
        })
        .collect();

    disks.sort_by(|a, b| a.disk.cmp(&b.disk));
    disks
}

/// Whether every / any item in `indices` is currently selected. Used to
/// drive the tri-state checkbox on category and game headers.
pub fn group_selection_state(items: &[FindingItem], indices: &[usize]) -> (bool, bool) {
    if indices.is_empty() {
        return (false, false);
    }
    let selected_count = indices.iter().filter(|&&i| items[i].selected).count();
    (selected_count == indices.len(), selected_count > 0)
}

/// Flips the selection of a whole group: selects all if not all are
/// currently selected, otherwise deselects all.
pub fn toggle_group(items: &mut [FindingItem], indices: &[usize]) {
    let (all_selected, _) = group_selection_state(items, indices);
    set_group_selection(items, indices, !all_selected);
}

/// Sets every item in `indices` to the given selection state. Used by the
/// bulk-selection actions (select all on a disk, all of a category, ...).
pub fn set_group_selection(items: &mut [FindingItem], indices: &[usize], selected: bool) {
    for &index in indices {
        items[index].selected = selected;
    }
}

/// Total size in bytes of the selected, non-removed items in `indices`.
pub fn group_size_bytes(items: &[FindingItem], indices: &[usize]) -> u64 {
    indices.iter().map(|&i| items[i].row.size).sum()
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
        let total = by_library.values().sum();
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
#[derive(Debug, Clone, Copy)]
pub struct ScanTiming {
    /// Discovery + persist + the MFT index pre-pass (`Verb::Scan` in the
    /// progress bar). Naturally tiny on an SSD-only setup, where the MFT
    /// pass is skipped entirely - that is the honest phase split, not a bug.
    pub scan: std::time::Duration,
    /// Per-game scan+classify+write (`Verb::Analyze` in the progress bar).
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
    fn display_category_maps_every_source_to_the_five_top_level_categories() {
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
            display_category(FindingSource::Rule(Category::DevLeftovers)),
            DisplayCategory::Other
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
        for kind in [OrphanKind::UnmanagedFolder, OrphanKind::ServiceFolder] {
            assert_eq!(
                display_category(FindingSource::Orphan(kind)),
                DisplayCategory::Orphan
            );
        }
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
            source_key(FindingSource::Rule(Category::DevLeftovers)),
            "dev_leftovers"
        );
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
            FindingSource::Rule(Category::DevLeftovers),
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
            vec!["redist", "docs", "bonus", "loc", "other", "orphan"]
        );
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

        let tree = build_tree(&items);

        assert_eq!(tree.len(), 1, "all games share the same disk (C:)");
        assert_eq!(tree[0].disk, "C:");
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
        assert_eq!(game_a.categories[0].category, DisplayCategory::Redist);
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

        let tree = build_tree(&items);

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

        let tree = build_tree(&items);

        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].disk, "D:", "disks are sorted alphabetically");
        assert_eq!(tree[1].disk, "E:");
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

        let tree = build_tree(&items);

        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].disk, "\\\\server\\share");
    }

    #[test]
    fn build_tree_groups_localization_categories_together() {
        let items = vec![
            loc_item(1, "Game A", LangKind::Audio, "es", 90, 100),
            loc_item(1, "Game A", LangKind::Text, "fr", 88, 20),
            loc_item(2, "Game B", LangKind::Audio, "de", 95, 300),
        ];

        let tree = build_tree(&items);

        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].games.len(), 2);
        for game in &tree[0].games {
            assert_eq!(
                game.categories.len(),
                1,
                "audio and text localization findings collapse into one Loc category"
            );
            assert_eq!(game.categories[0].category, DisplayCategory::Loc);
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

        let tree = build_tree(&items);

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

        let tree = build_tree(&items);

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

        let tree = build_tree(&items);

        let game = &tree[0].games[0];
        assert_eq!(
            game.categories.len(),
            1,
            "the shared folder appears in exactly one category"
        );
        assert_eq!(game.categories[0].category, DisplayCategory::Docs);
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

        let tree = build_tree(&items);

        assert_eq!(
            tree[0].games[0].categories[0].category,
            DisplayCategory::Redist
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

        let tree = build_tree(&items);

        let game = &tree[0].games[0];
        assert_eq!(game.categories[0].category, DisplayCategory::Other);
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

        let tree = build_tree(&items);

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

        let tree = build_tree(&items);

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

        let tree = build_tree(&items);

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

        let tree = build_tree(&items);
        assert_eq!(tree.len(), 1);
        let disk = &tree[0];
        assert_eq!(disk.games.len(), 1);
        let game = &disk.games[0];

        let bonus = game
            .categories
            .iter()
            .find(|c| c.category == DisplayCategory::Bonus)
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
            .find(|c| c.category == DisplayCategory::Docs)
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
    fn orphan_confidence_is_below_auto_select_threshold_for_both_kinds() {
        // The GT-02 safety contract: orphaned residue is shown but never
        // auto-selected, so a game installed past the launcher can't be
        // pre-checked for deletion. Enforced purely through confidence.
        assert!(orphan_confidence(OrphanKind::UnmanagedFolder) < AUTO_SELECT_CONFIDENCE_THRESHOLD);
        assert!(orphan_confidence(OrphanKind::ServiceFolder) < AUTO_SELECT_CONFIDENCE_THRESHOLD);
        assert!(!default_selected(orphan_confidence(
            OrphanKind::UnmanagedFolder
        )));
        assert!(!default_selected(orphan_confidence(
            OrphanKind::ServiceFolder
        )));
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

        let tree = build_tree(&items);

        let disk_f = tree
            .iter()
            .find(|disk| disk.disk == "F:")
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
            DisplayCategory::Orphan
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
            .find(|disk| disk.disk == "D:")
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
        for category in [Loc, Redist, Other] {
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
        assert!(!profile_auto_selects(SelectionProfile::Balanced, Other, 95));
    }

    #[test]
    fn aggressive_profile_adds_everything_at_or_above_the_floor() {
        use DisplayCategory::*;
        // Safe categories and Loc still selected regardless of confidence.
        for category in [Bonus, Docs, Orphan, Loc] {
            assert!(profile_auto_selects(
                SelectionProfile::Aggressive,
                category,
                10
            ));
        }
        // Redist / Other now come in - but only at or above the floor (70).
        assert!(profile_auto_selects(
            SelectionProfile::Aggressive,
            Redist,
            AGGRESSIVE_CONFIDENCE_FLOOR
        ));
        assert!(profile_auto_selects(
            SelectionProfile::Aggressive,
            Other,
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
        for category in [Bonus, Docs, Orphan, Loc, Redist, Other] {
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
        // GT-02 contract, now profile-scoped: the Custom (confidence-only) path
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
        assert_eq!(category_risk(Other), RiskLevel::Medium);
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
                DisplayCategory::Orphan, // None, 500
                DisplayCategory::Redist, // None, 100
                DisplayCategory::Loc,    // Low, 500
                DisplayCategory::Other,  // Medium, 50
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

    /// GT-12. The summary row states one game count for the whole plan, so it
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
        assert_eq!(category_display(Lang::Uk, DisplayCategory::Other), "Інше");
        assert_eq!(
            category_display(Lang::Uk, DisplayCategory::Orphan),
            "Осиротіле"
        );
        assert_eq!(
            category_display(Lang::En, DisplayCategory::Redist),
            "Redistributables"
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

    #[test]
    fn category_enabled_checks_membership_by_ui_key_when_list_is_non_empty() {
        let enabled = vec!["redist".to_string(), "docs".to_string()];
        assert!(category_enabled(&enabled, DisplayCategory::Redist));
        assert!(category_enabled(&enabled, DisplayCategory::Docs));
        assert!(!category_enabled(&enabled, DisplayCategory::Bonus));
        assert!(!category_enabled(&enabled, DisplayCategory::Loc));
        assert!(!category_enabled(&enabled, DisplayCategory::Other));
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

    /// The fingerprint exists to tell a hand-edit from everything else, so it
    /// has to move when a checkbox moves and stay put when nothing does.
    #[test]
    fn selection_fingerprint_tracks_checkbox_state() {
        let mut items = vec![
            item(1, "A", FindingSource::Rule(Category::RedistFolder), 90, 10),
            item(2, "B", FindingSource::Rule(Category::RedistFolder), 90, 20),
        ];
        let baseline = selection_fingerprint(&items);

        assert_eq!(selection_fingerprint(&items), baseline, "pure function");

        items[0].selected = !items[0].selected;
        let after_toggle = selection_fingerprint(&items);
        assert_ne!(after_toggle, baseline, "a toggled checkbox must show up");

        items[0].selected = !items[0].selected;
        assert_eq!(
            selection_fingerprint(&items),
            baseline,
            "toggling back must return to the same fingerprint",
        );
    }

    /// Deletion marks findings `removed`, which must not read as a hand-edit
    /// of the selection - it is folded in as its own bit rather than sharing
    /// one with `selected`.
    #[test]
    fn selection_fingerprint_separates_removal_from_deselection() {
        let mut removed = vec![item(
            1,
            "A",
            FindingSource::Rule(Category::RedistFolder),
            90,
            10,
        )];
        removed[0].selected = true;
        removed[0].removed = true;

        let mut deselected = vec![item(
            1,
            "A",
            FindingSource::Rule(Category::RedistFolder),
            90,
            10,
        )];
        deselected[0].selected = false;
        deselected[0].removed = false;

        assert_ne!(
            selection_fingerprint(&removed),
            selection_fingerprint(&deselected),
            "a removed-but-checked finding is not the same as an unchecked one",
        );
    }
}
