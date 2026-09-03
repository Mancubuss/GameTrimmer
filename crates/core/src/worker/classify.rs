//! The classification cycle: the one place a file becomes a finding.
//!
//! Every removal path in GameTrimmer answers the same eight questions about
//! each file, in the same order - is the run cancelled, what does the rule
//! engine say, does a personal keep rule veto it, does the keep-language
//! list, is it a protected container, how do a rule match and a localization
//! match combine, is that category switched on, and does an already-flagged
//! intro's name appear elsewhere in the game.
//!
//! It used to be written out twice: once for the interactive scan, once for
//! unattended re-trim, in two crates, and the second copy skipped three of
//! the eight. That is the whole reason this module exists here rather than
//! in the application: the answer has to be one answer, and the crate that
//! runs unattended sits below the one with the window.
//!
//! What differs between callers is policy, not the questions - see
//! [`ClassifyPolicy`] once it lands. The vocabulary the answers are spoken
//! in ([`FindingSource`], [`DisplayCategory`]) lives here too, because the
//! cycle produces it and both callers persist it.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::error::{CoreError, Result as CoreResult};
use crate::langdetect::{LangDetector, LangFinding, LangKind};
use crate::orphans::OrphanKind;
use crate::perf;
use crate::rules::{Category, Finding, RuleEngine, RuleProvenance, Verdict};
use crate::safety::{SafetySnapshot, SnapshotCapture};
use crate::scanner::{FileEntry, CANCEL_POLL_INTERVAL};

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
    /// Orphaned launcher residue (see `crate::orphans`). Has no
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
/// changes that predate a saved scan.
///
/// That includes `"monolithic_archive"`: an in-place archive trimmer used to
/// write it, and no code writes it any more, but a database from that build
/// can still carry rows with it - and no `Category` variant backs it any
/// more either. Callers (see `worker::load`) must skip such rows rather than
/// fail the whole load, since the row is otherwise perfectly readable, and
/// this is what makes them skip: they never resolve to a [`FindingSource`] in
/// the first place.
pub fn parse_source_key(key: &str) -> Option<FindingSource> {
    match key {
        "redist_folder" => Some(FindingSource::Rule(Category::RedistFolder)),
        "redist_file" => Some(FindingSource::Rule(Category::RedistFile)),
        "docs_folder" => Some(FindingSource::Rule(Category::DocsFolder)),
        "docs_file" => Some(FindingSource::Rule(Category::DocsFile)),
        "bonus" => Some(FindingSource::Rule(Category::Bonus)),
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
/// `enabled_categories` setting (see `crate::settings::Settings`).
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

/// Which game is being classified: the row id its results are written under,
/// the name progress and errors call it by, where it lives, and the vendor id
/// a scoped rule is matched against.
///
/// Grouped rather than passed as four more parameters because the four always
/// travel together and the two `&str`s next to each other are exactly the pair
/// a caller can silently swap.
#[derive(Debug, Clone, Copy)]
pub struct GameIdentity<'a> {
    pub id: i64,
    pub name: &'a str,
    pub install_dir: &'a Path,
    /// `None` for a folder-scan or manual game; no scoped rule matches one.
    pub app_id: Option<&'a str>,
}

/// Classifies an already-obtained file list - from either `scan_dir`
/// (walkdir) or the MFT index pass - through both the rule engine and the
/// localization detector. The hot passes are CPU-only; only findings from an
/// imported rule or with an archive-like extension receive a bounded content
/// probe before safety evidence is captured. This work runs in parallel
/// across scan workers; only [`persist_prepared_game`] needs a `Connection`.
///
/// `cancel` is polled inside both hot passes (the localization
/// `analyze_game_cancellable` and the per-file rule-engine loop); once it is
/// observed set this returns `Err(CoreError::Other("cancelled"))` promptly
/// instead of classifying the whole (possibly enormous) file list, so a Stop
/// during a big game's analysis is honored rather than swallowed. When
/// `cancel` is never set the result is exactly the non-cancellable one.
///
/// `enabled_categories` (the persisted `enabled_categories` setting - see
/// `crate::settings`) is applied right here, before a finding
/// even enters `combined_by_index`: a file whose category is disabled is
/// treated exactly as if no rule/localization cue had matched it at all.
/// This is the single choke point for the category filter - doing it this
/// early (rather than at persistence or display) means a disabled
/// category's files never affect folder-collapsing (`assign_group_dirs`)
/// either, and the database ends up holding exactly what the setting says
/// should be scanned, not a superset filtered later.
pub fn classify_game(
    engine: &RuleEngine,
    lang_detector: &LangDetector,
    game: GameIdentity<'_>,
    entries: Vec<FileEntry>,
    enabled_categories: &[String],
    cancel: &AtomicBool,
) -> CoreResult<PreparedGame> {
    // `analyze_game` needs sibling context (the language-family heuristic),
    // so it runs once over all of this game's files rather than per-file.
    // The cancellable variant polls `cancel` inside its own hot loops, so a
    // Stop request lands promptly even mid-analysis of a huge game.
    let lang_findings: HashMap<usize, LangFinding> = lang_detector
        .analyze_game_cancellable(&entries, cancel)?
        .into_iter()
        .collect();

    // First pass: combine each entry's rule/localization findings, keeping
    // the entry's index into `entries` so `assign_group_dirs` (which needs
    // the full file list, not just the flagged ones) can be run afterwards.
    let rules_started = Instant::now();
    let mut combined_by_index: Vec<(usize, CombinedFinding)> = Vec::new();
    let mut kept = 0usize;
    // A personal keep rule outranks the same-name sweep below just as it
    // outranks every rule: a file the user vetoed by name must not come back
    // because another copy of that name was flagged elsewhere in the game.
    let mut vetoed: HashSet<usize> = HashSet::new();
    for (index, entry) in entries.iter().enumerate() {
        // The rule-engine pass is per-file regex work; on a game with
        // hundreds of thousands of files it is long enough to be worth
        // interrupting too (the same cadence the core cancel path uses).
        if index % CANCEL_POLL_INTERVAL == 0 && cancel.load(Ordering::Relaxed) {
            return Err(CoreError::Other("cancelled".to_string()));
        }
        let verdict = engine.classify(&entry.rel_path, game.app_id);
        // A personal exception stops the file here, before the localization
        // detector gets a look at it. Skipping this branch and letting the
        // veto fall through as "no rule matched" would hand a kept
        // `loc_de.pak` straight back as a localization finding - and a
        // localization finding is exactly what most exceptions are written
        // against.
        if verdict == Verdict::Kept {
            kept += 1;
            vetoed.insert(index);
            continue;
        }

        // The keep-language list is the other veto a rule must yield to,
        // and the one it used to step over: it lives inside the localization
        // detector, so it protected this file from that stage and from
        // nothing else. It applies to a rule that says it matches *content*
        // in the player's language, not to the startup screens that make up
        // the rest of the intro category - see
        // `worker::keep_language_vetoes_rule`. Dropping the verdict leaves
        // the file with no finding at all (the detector already declined it,
        // by the same predicate), so it stays on disk. It also joins
        // `vetoed`, because a copy of it elsewhere in the game must not be
        // swept back in by name.
        if let Verdict::Flagged(finding) = &verdict {
            if crate::worker::keep_language_vetoes_rule(lang_detector, finding, &entry.rel_path) {
                vetoed.insert(index);
                continue;
            }
        }

        // A protected container (see `is_protected_container`) is never
        // offered for whole-file deletion - by a rule or by the
        // localization detector. GameTrimmer can only delete a file whole,
        // and a container holds assets the user never selected, so the file
        // is skipped entirely rather than shown as a finding it cannot
        // safely act on.
        if crate::worker::is_protected_container(&entry.rel_path) {
            continue;
        }
        let lang_finding = lang_findings.get(&index);

        if let Some(combined) = combine_finding(verdict.flagged(), lang_finding) {
            if !category_enabled(enabled_categories, display_category(combined.source)) {
                continue;
            }

            combined_by_index.push((index, combined));
        }
    }

    // GT-206: a game engine plays *one* copy of a startup video out of
    // several search paths, and it is not necessarily the copy a path-shaped
    // rule reached. Every other file of this game carrying an already-flagged
    // intro's exact file name becomes an intro finding too - see
    // `same_name_siblings` for why this is done here, while classifying,
    // rather than by widening a delete batch later.
    add_same_name_intro_siblings(&entries, &mut combined_by_index, &vetoed);

    perf::add(perf::Stage::Rules, rules_started.elapsed());

    let flagged: HashSet<usize> = combined_by_index.iter().map(|(index, _)| *index).collect();
    let group_dirs = perf::timed(perf::Stage::Grouping, || {
        assign_group_dirs(&entries, &flagged)
    });

    // One cache per game: every finding here shares the same trusted root and
    // most of the same intermediate directories, which is exactly the
    // redundancy `SnapshotCapture` exists to remove.
    let safety_started = Instant::now();
    let mut capture = SnapshotCapture::new();
    let findings: Vec<PreparedFinding> = combined_by_index
        .into_iter()
        .map(|(index, combined)| {
            let entry = &entries[index];
            let safety = capture
                .capture(
                    game.install_dir,
                    &entry.rel_path,
                    entry.mft_identity.as_ref(),
                )
                .map_err(|reason| reason.to_string());
            PreparedFinding {
                entry_index: index,
                rel_path: entry.rel_path.clone(),
                size: entry.size,
                size_on_disk: entry.size_on_disk,
                source: combined.source,
                rule_id: combined.rule_id,
                confidence: combined.confidence,
                provenance: combined.provenance,
                lang_tag: combined.lang_tag,
                group_dir: group_dirs.get(&index).cloned(),
                safety,
            }
        })
        .collect();

    let anti_cheat_safe = crate::anti_cheat::AntiCheatShield::is_safe_from_relative_paths(
        entries.iter().map(|e| &e.rel_path),
    );

    perf::add(perf::Stage::Safety, safety_started.elapsed());

    Ok(PreparedGame {
        game_id: game.id,
        name: game.name.to_string(),
        app_id: game.app_id.map(str::to_string),
        install_dir: game.install_dir.to_path_buf(),
        entries,
        findings,
        kept,
        anti_cheat_protected: !anti_cheat_safe,
    })
}

/// One file's finding after reconciling the rule engine and the localization
/// detector, ready to persist and display.
#[derive(Clone)]
pub struct CombinedFinding {
    pub source: FindingSource,
    pub rule_id: String,
    pub confidence: u8,
    pub provenance: RuleProvenance,
    pub lang_tag: Option<String>,
}

/// Extends `combined` with every unclassified file of this game that carries
/// the exact file name of one of its intro findings, copying that finding's
/// description, confidence and provenance.
///
/// Why intros only: a stub written into the copy the engine never opens is
/// the one failure the user cannot see without launching the game - the app
/// reports the bytes freed and the logo still plays. Every other category
/// deletes the file outright, where a missed second copy is merely space not
/// reclaimed. See [`crate::scanner::same_name_siblings`].
///
/// Three exclusions, each of them a rule that already outranks a rules-engine
/// match and must keep outranking this:
/// - `vetoed`: a personal keep rule refuses any classification of that file.
/// - already in `combined`: the file has a verdict of its own; a second
///   finding for one file would double-count its bytes.
/// - an imported rule's match: an untrusted pack gets no reach past the
///   pattern it actually wrote, and `retrim` refuses those unattended anyway.
/// - a protected container: a whole-file delete would take everything
///   packed inside it, so it is never offered as one.
fn add_same_name_intro_siblings(
    entries: &[FileEntry],
    combined: &mut Vec<(usize, CombinedFinding)>,
    vetoed: &HashSet<usize>,
) {
    let sources: Vec<usize> = combined
        .iter()
        .filter(|(_, finding)| {
            finding.source == FindingSource::Rule(crate::rules::Category::Intro)
                && finding.provenance != RuleProvenance::ImportedUntrusted
        })
        .map(|(index, _)| *index)
        .collect();
    if sources.is_empty() {
        return;
    }

    let mut skip: HashSet<usize> = combined.iter().map(|(index, _)| *index).collect();
    skip.extend(vetoed.iter().copied());

    let pairs = crate::scanner::same_name_siblings(entries, &sources, &skip);
    if pairs.is_empty() {
        return;
    }
    let by_index: HashMap<usize, usize> = combined
        .iter()
        .enumerate()
        .map(|(position, (index, _))| (*index, position))
        .collect();
    for (sibling, source) in pairs {
        if crate::worker::is_protected_container(&entries[sibling].rel_path) {
            continue;
        }
        let Some(&position) = by_index.get(&source) else {
            continue;
        };
        // Not a clone of the source's attribution: the rule that matched
        // the source provably does not match this path (it is what the
        // depth budget excluded), so persisting its description would put a
        // pattern in `findings.rule_id` that a user auditing "why is this
        // flagged" can disprove. The confidence *is* the source's, and
        // capped there: the sweep's claim is derived from that verdict and
        // cannot outrank it, while lowering it would drop the copy below
        // `AUTO_SELECT_CONFIDENCE_THRESHOLD` and leave it unticked - which
        // is the GT-206 bug again, the logo still playing out of the copy
        // nobody stubbed.
        let mut swept = combined[position].1.clone();
        swept.rule_id = crate::scanner::SIBLING_FINDING_DESC.to_string();
        combined.push((sibling, swept));
    }
    // Findings are read back positionally by the UI tree; keeping them in
    // file order stops a swept copy from surfacing detached from its group.
    combined.sort_by_key(|(index, _)| *index);
}

/// Merges a rules-engine finding with a localization finding for the same
/// file. Categories are checked in a fixed precedence order (redist → dev
/// leftovers → bonus → docs → localization; see `Category::priority_rank`),
/// so a rule finding always beats a localization cue regardless of
/// confidence: a localized readme (`ReadMe_DE.rtf`) is documentation, and a
/// per-language file inside `Support\` is support material (also the docs
/// category) - the language split inside such folders does not change what
/// the folder is. Localization applies only to files no rule claimed.
///
/// `ui_lang` is what the reason is written in. It is resolved here, at scan
/// time, rather than when the row is drawn, because `rule_id` is persisted as
/// text: the same choice the orphan pass already makes. The cost is that
/// switching the interface language leaves already-scanned findings describing
/// themselves in the previous one until the next scan.
fn combine_finding(rule: Option<Finding>, lang: Option<&LangFinding>) -> Option<CombinedFinding> {
    match (rule, lang) {
        (Some(r), _) => Some(CombinedFinding {
            source: FindingSource::Rule(r.category),
            rule_id: r.rule_desc,
            confidence: r.confidence,
            provenance: r.provenance,
            lang_tag: None,
        }),
        (None, Some(l)) => Some(CombinedFinding {
            source: FindingSource::Loc(l.kind),
            rule_id: l.reason.to_string(),
            confidence: l.confidence,
            provenance: RuleProvenance::Builtin,
            lang_tag: Some(l.lang_tag.clone()),
        }),
        (None, None) => None,
    }
}

/// Assigns each flagged file (identified by its index into `entries`) the
/// `\`-separated path of its shallowest fully-flagged ancestor directory,
/// for UI-only tree grouping (see `model::build_tree`).
///
/// Rationale: a folder where *every* file is flagged as non-essential can be
/// shown - and deleted - as one unit instead of scattering its files across
/// whichever categories happen to match each of them individually. The
/// *shallowest* such ancestor is chosen deliberately: it is the largest unit
/// that is still wholly non-essential, so collapsing to it merges the most
/// files while remaining exactly as safe to remove as any single flagged
/// descendant. A directory must contain at least 2 files to be collapsible -
/// a single-file "folder" gains nothing from collapsing, since the file's
/// own row already represents it - and the (implicit) game root is never a
/// candidate, since there is no bounding folder above it to collapse into.
///
/// The directory chains are *borrowed* from each entry's `rel_path` wherever
/// possible (see [`dir_prefixes`]): every ancestor path is a prefix of the
/// file's own path, so counting them needs no allocation at all. Only the
/// handful of paths that survive as group keys are turned into `String`s, at
/// the end. Building them the other way - an owned `String` per directory
/// level per file - meant roughly 25 million allocations, and 25 million
/// owned-string hashes, per scan of a large library.
pub fn assign_group_dirs(
    entries: &[FileEntry],
    flagged: &HashSet<usize>,
) -> HashMap<usize, String> {
    // Directory path -> (total files under it, flagged files under it).
    let mut dir_stats: HashMap<Cow<'_, str>, (u32, u32)> = HashMap::new();

    for (index, entry) in entries.iter().enumerate() {
        let is_flagged = flagged.contains(&index);
        for dir in dir_prefixes(&entry.rel_path) {
            let stats = dir_stats.entry(dir).or_insert((0, 0));
            stats.0 += 1;
            if is_flagged {
                stats.1 += 1;
            }
        }
    }

    // The chains are recomputed here rather than kept from the loop above:
    // only the flagged files (a small fraction of a game's tree) need one,
    // and holding a chain per file was the other half of the old memory
    // cost.
    let mut group_dirs = HashMap::new();
    for &index in flagged {
        let Some(entry) = entries.get(index) else {
            continue;
        };
        // The chain is shallowest-first, so the first collapsible entry
        // found is the shallowest collapsible ancestor.
        let collapsible = dir_prefixes(&entry.rel_path).into_iter().find(|dir| {
            let (total, flagged_count) = dir_stats.get(dir).copied().unwrap_or((0, 0));
            total >= 2 && total == flagged_count
        });
        if let Some(dir) = collapsible {
            group_dirs.insert(index, dir.into_owned());
        }
    }

    group_dirs
}

/// The `\`-separated ancestor directory paths of `rel_path`, shallowest
/// first, excluding the (implicit, empty) game root and the file name
/// itself. E.g. `"a\b\c\file.txt"` -> `["a", "a\\b", "a\\b\\c"]`; a file
/// directly under the game root (no directory segments) yields an empty
/// list.
///
/// Borrowed where it can be, owned where it must be. Both producers of
/// `rel_path` - `scan_dir_cancellable`, which joins components with `\`, and
/// the MFT path (`mftscan::pathmap::scan_frn_map`), which does the same -
/// hand over paths that are already exactly `\`-separated with no empty
/// segments. For those, every ancestor is literally `&rel_path[..end]` at a
/// separator, so the whole chain costs nothing but the `Vec`.
///
/// A path that is *not* in that shape (a `/` separator, a leading or doubled
/// separator, a trailing one) has to be normalised, and a normalised prefix
/// is no longer a substring of the input - so those keep the original owned
/// build. This is a deliberate fallback rather than a simplification:
/// silently treating `a/b/c.txt` as one flat segment would change which
/// folders collapse in the UI tree, which is a behaviour change, not an
/// optimisation.
fn dir_prefixes(rel_path: &str) -> Vec<Cow<'_, str>> {
    if is_canonically_separated(rel_path) {
        return rel_path
            .match_indices('\\')
            .map(|(end, _)| Cow::Borrowed(&rel_path[..end]))
            .collect();
    }

    let segments: Vec<&str> = rel_path
        .split(['\\', '/'])
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.len() <= 1 {
        return Vec::new();
    }

    let mut prefixes = Vec::with_capacity(segments.len() - 1);
    let mut acc = String::new();
    for segment in &segments[..segments.len() - 1] {
        if !acc.is_empty() {
            acc.push('\\');
        }
        acc.push_str(segment);
        prefixes.push(Cow::Owned(acc.clone()));
    }
    prefixes
}

/// Whether `rel_path` is already in the shape [`dir_prefixes`] can slice
/// prefixes out of: `\` separators only, and no empty segment (no leading,
/// doubled, or trailing separator). Under those conditions splitting on `\`
/// and rejoining with `\` is the identity, so a prefix ending at any
/// separator is exactly the normalised ancestor path.
fn is_canonically_separated(rel_path: &str) -> bool {
    !rel_path.contains('/')
        && !rel_path.starts_with('\\')
        && !rel_path.ends_with('\\')
        && !rel_path.contains("\\\\")
}

/// One file's finding, already resolved (rule engine vs. localization
/// detector) but not yet persisted - the `files.id` it will reference does
/// not exist until `store_files_no_tx` has run, so the finding carries
/// `entry_index` (below) and the writer resolves the id from that. Carrying
/// `size` here (rather than re-deriving it from `entries` at persist time by
/// `rel_path`) avoids an O(files x findings) rescan per game.
pub struct PreparedFinding {
    /// Index of this finding's file into [`PreparedGame::entries`].
    ///
    /// `store_files_no_tx` returns the inserted `files.id`s in entry order,
    /// so this is all the writer needs to attach the finding to its row -
    /// where it used to select every row of the game back out and match on
    /// `rel_path`. `classify_game` has the index in hand anyway (it is what
    /// `combined_by_index` is keyed by) and simply threw it away before.
    pub entry_index: usize,
    /// The path itself, still carried alongside the index: the UI row, the
    /// safety evidence written when a snapshot could not be captured, and
    /// the orphan/finding comparisons all want it, and at one clone per
    /// *finding* (720 k) rather than per file it is not the cost that
    /// mattered.
    pub rel_path: String,
    pub size: u64,
    pub size_on_disk: u64,
    pub source: FindingSource,
    pub rule_id: String,
    pub confidence: u8,
    pub provenance: RuleProvenance,
    pub lang_tag: Option<String>,
    /// Folder-grouping key for the UI tree; see [`assign_group_dirs`].
    /// Persisted to `findings.group_dir` by `persist_prepared_game` so a
    /// later startup load can read it straight back instead of recomputing
    /// it from the whole file list (the dominant cost of the old load path).
    pub group_dir: Option<String>,
    /// Scan-time deletion evidence, or the reason it could not be captured.
    ///
    /// Captured here, on the scan pool, rather than in the writer: it costs a
    /// handful of file opens per finding, and doing it inside the writer's
    /// transaction made one thread pay for every finding in the scan while
    /// holding the database lock. The writer only inserts what it is handed.
    pub safety: std::result::Result<SafetySnapshot, String>,
}

/// The result of scanning and classifying one game: no DB state, so it can
/// be produced on any thread and handed off to the single writer thread.
pub struct PreparedGame {
    pub game_id: i64,
    pub name: String,
    /// The game's vendor id, carried through classification (a rule may be
    /// scoped to it) and on into the UI row, where "never touch this" needs
    /// it to bind the exception it writes. See [`persistence::ScannedGame`].
    pub app_id: Option<String>,
    pub install_dir: PathBuf,
    pub entries: Vec<FileEntry>,
    pub findings: Vec<PreparedFinding>,
    /// How many of this game's files a personal exception vetoed.
    ///
    /// Counted here, where the verdict is seen, and summed for the whole run
    /// so the status line can say so: a user who keeps a file and rescans has
    /// to be able to tell "it is gone because you kept it" from "detection
    /// missed it".
    pub kept: usize,
    /// Whether anti-cheat protection is active on this game.
    pub anti_cheat_protected: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::langdetect::{LangEvidence, LangReason};

    fn entry(rel_path: &str) -> FileEntry {
        FileEntry::logical_only(rel_path, 1, None)
    }

    fn lang_finding_de() -> LangFinding {
        LangFinding {
            lang_tag: "de".to_string(),
            kind: LangKind::Text,
            confidence: 90,
            reason: LangReason::new(LangEvidence::Family {
                languages: 3,
                dir: Some("Docs".to_string()),
            }),
        }
    }

    /// A rule that matches every file name, so every entry handed to
    /// [`classify_game`] comes back flagged.
    fn match_all_engine() -> RuleEngine {
        RuleEngine::from_json(
            r#"{"version":1,"rules":[{"category":"docs_file","pattern":".","desc":"test rule","confidence":50}]}"#,
        )
        .expect("valid test rules.json")
    }

    #[test]
    fn assign_group_dirs_collapses_a_folder_where_every_file_is_flagged() {
        let entries = vec![entry(r"junk\a.txt"), entry(r"junk\b.txt")];
        let flagged: HashSet<usize> = [0, 1].into_iter().collect();

        let groups = assign_group_dirs(&entries, &flagged);

        assert_eq!(groups.get(&0), Some(&"junk".to_string()));
        assert_eq!(groups.get(&1), Some(&"junk".to_string()));
    }

    #[test]
    fn assign_group_dirs_does_not_collapse_a_folder_with_an_unflagged_file() {
        let entries = vec![
            entry(r"mixed\a.txt"),
            entry(r"mixed\b.txt"), // not flagged below
        ];
        let flagged: HashSet<usize> = [0].into_iter().collect();

        let groups = assign_group_dirs(&entries, &flagged);

        assert_eq!(
            groups.get(&0),
            None,
            "the folder has an unflagged member, so it must not collapse"
        );
    }

    #[test]
    fn assign_group_dirs_does_not_collapse_a_single_file_folder() {
        let entries = vec![entry(r"lonely\only.txt")];
        let flagged: HashSet<usize> = [0].into_iter().collect();

        let groups = assign_group_dirs(&entries, &flagged);

        assert_eq!(
            groups.get(&0),
            None,
            "a folder with only one file gains nothing from collapsing"
        );
    }

    /// The grouping itself must not care which separator wrote the path:
    /// two files in the same folder, spelled differently, still collapse
    /// into one group.
    #[test]
    fn assign_group_dirs_groups_across_mixed_separators() {
        let entries = vec![entry("junk/a.txt"), entry(r"junk\b.txt")];
        let flagged: HashSet<usize> = [0, 1].into_iter().collect();

        let groups = assign_group_dirs(&entries, &flagged);

        assert_eq!(groups.get(&0), Some(&"junk".to_string()));
        assert_eq!(groups.get(&1), Some(&"junk".to_string()));
    }

    #[test]
    fn assign_group_dirs_leaves_unflagged_files_out_of_the_result() {
        let entries = vec![entry(r"junk\a.txt"), entry(r"junk\b.txt")];
        let flagged: HashSet<usize> = [0].into_iter().collect();

        let groups = assign_group_dirs(&entries, &flagged);

        assert_eq!(
            groups.len(),
            0,
            "\"junk\" has only 1 of 2 files flagged, so it can't collapse, \
             and the one flagged file has no other collapsible ancestor"
        );
    }

    #[test]
    fn assign_group_dirs_never_collapses_the_game_root() {
        // Two flagged files directly at the root: there is no directory
        // string representing the root itself for them to collapse into.
        let entries = vec![entry("a.txt"), entry("b.txt")];
        let flagged: HashSet<usize> = [0, 1].into_iter().collect();

        let groups = assign_group_dirs(&entries, &flagged);

        assert!(groups.is_empty(), "root-level files are always orphans");
    }

    #[test]
    fn assign_group_dirs_picks_the_shallowest_collapsible_ancestor() {
        // Both "top" and "top\\nested" are fully flagged and have >= 2
        // files; "top" is shallower and must win.
        let entries = vec![
            entry(r"top\nested\a.txt"),
            entry(r"top\nested\b.txt"),
            entry(r"top\c.txt"),
        ];
        let flagged: HashSet<usize> = [0, 1, 2].into_iter().collect();

        let groups = assign_group_dirs(&entries, &flagged);

        assert_eq!(groups.get(&0), Some(&"top".to_string()));
        assert_eq!(groups.get(&1), Some(&"top".to_string()));
        assert_eq!(groups.get(&2), Some(&"top".to_string()));
    }

    #[test]
    fn dir_prefixes_is_empty_for_a_file_directly_under_the_game_root() {
        assert!(dir_prefixes("readme.txt").is_empty());
    }

    #[test]
    fn dir_prefixes_lists_ancestors_shallowest_first_excluding_root_and_file_name() {
        assert_eq!(
            dir_prefixes(r"a\b\c\file.txt"),
            vec!["a".to_string(), r"a\b".to_string(), r"a\b\c".to_string()]
        );
    }

    /// The chains are sliced straight out of `rel_path` when it is already
    /// `\`-separated, which no path either scan producer emits can violate -
    /// but a `/`, a doubled separator or a leading one would make a borrowed
    /// prefix mean something different from the normalised ancestor. Those
    /// take the owned path, and must still normalise exactly as before.
    #[test]
    fn dir_prefixes_normalizes_separators_it_cannot_slice_through() {
        assert_eq!(
            dir_prefixes("a/b/c/file.txt"),
            vec!["a".to_string(), r"a\b".to_string(), r"a\b\c".to_string()],
            "forward slashes must normalize to the same chain as backslashes"
        );
        assert_eq!(
            dir_prefixes(r"a\\b\file.txt"),
            vec!["a".to_string(), r"a\b".to_string()],
            "a doubled separator is one separator, not an empty directory"
        );
        assert_eq!(
            dir_prefixes(r"\a\file.txt"),
            vec!["a".to_string()],
            "a leading separator does not create a nameless root directory"
        );
    }

    #[test]
    fn combine_finding_prefers_any_rule_category_over_localization() {
        // The localization cue is MORE confident (90 vs 85), but category
        // precedence is fixed: a localized readme is documentation first.
        let rule = Finding {
            category: Category::DocsFile,
            rule_desc: "Файл документації (PDF/RTF)".to_string(),
            confidence: 85,
            provenance: RuleProvenance::Builtin,
            localized_content: false,
        };

        let combined = combine_finding(Some(rule), Some(&lang_finding_de()))
            .expect("a rule match must produce a finding");

        assert!(matches!(
            combined.source,
            FindingSource::Rule(Category::DocsFile)
        ));
        assert_eq!(combined.lang_tag, None);
    }

    #[test]
    fn combine_finding_uses_localization_only_when_no_rule_matches() {
        let combined = combine_finding(None, Some(&lang_finding_de()))
            .expect("a localization finding alone must survive");

        assert!(matches!(
            combined.source,
            FindingSource::Loc(LangKind::Text)
        ));
        assert_eq!(combined.lang_tag.as_deref(), Some("de"));
    }

    /// `classify_game`'s `enabled_categories` filter is the single choke
    /// point for the "scanned artifact categories" setting - a disabled
    /// category's files must never reach `combined_by_index` at all, so
    /// they neither show up in the returned findings nor influence
    /// `assign_group_dirs` folder-collapsing.
    #[test]
    fn classify_game_drops_findings_whose_category_is_disabled() {
        let engine = match_all_engine(); // every file classifies as docs_file
        let lang_detector = LangDetector::new();
        let entries = vec![entry("readme.txt"), entry("manual.pdf")];

        let never_cancel = AtomicBool::new(false);
        let prepared_all_enabled = classify_game(
            &engine,
            &lang_detector,
            GameIdentity {
                id: 1,
                name: "Test Game",
                install_dir: Path::new("C:/Games/Test"),
                app_id: None,
            },
            entries.clone(),
            &[], // empty = every category enabled
            &never_cancel,
        )
        .expect("uncancelled classify_game should succeed");
        assert_eq!(
            prepared_all_enabled.findings.len(),
            2,
            "with no categories disabled, both files should be classified"
        );

        let prepared_docs_disabled = classify_game(
            &engine,
            &lang_detector,
            GameIdentity {
                id: 1,
                name: "Test Game",
                install_dir: Path::new("C:/Games/Test"),
                app_id: None,
            },
            entries,
            &["redist".to_string()], // "docs" is not in the enabled list
            &never_cancel,
        )
        .expect("uncancelled classify_game should succeed");
        assert!(
            prepared_docs_disabled.findings.is_empty(),
            "disabling \"docs\" must drop every docs_file finding, not just filter it later"
        );
    }

    /// Sibling case: when the finding's category *is* in the enabled list,
    /// it must still come through unaffected.
    #[test]
    fn classify_game_keeps_findings_whose_category_is_enabled() {
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();
        let entries = vec![entry("readme.txt")];

        let prepared = classify_game(
            &engine,
            &lang_detector,
            GameIdentity {
                id: 1,
                name: "Test Game",
                install_dir: Path::new("C:/Games/Test"),
                app_id: None,
            },
            entries,
            &["docs".to_string()],
            &AtomicBool::new(false),
        )
        .expect("uncancelled classify_game should succeed");
        assert_eq!(prepared.findings.len(), 1);
    }

    /// `classify_game` itself must honor a pre-set cancel flag - this is the
    /// MFT branch's guarantee (it skips the walk and calls `classify_game`
    /// directly), and the reason the "Analysis" phase of a huge game (ARK) can
    /// now be stopped. With the flag already set, the first `collect_cancellable`
    /// checkpoint inside `analyze_game_cancellable` fires before any real work.
    #[test]
    fn classify_game_returns_cancelled_when_flag_pre_set() {
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();

        let entries = vec![
            FileEntry::logical_only("a.txt", 1, None),
            FileEntry::logical_only("b\\c.txt", 1, None),
        ];
        let cancel = AtomicBool::new(true);

        let result = classify_game(
            &engine,
            &lang_detector,
            GameIdentity {
                id: 1,
                name: "Test Game",
                install_dir: Path::new("C:/Games/Test"),
                app_id: None,
            },
            entries,
            &[],
            &cancel,
        );

        match result {
            Ok(_) => panic!("a pre-cancelled classify_game must return Err"),
            Err(err) => assert!(
                err.to_string().contains("cancelled"),
                "error message should mention cancellation, got: {err}"
            ),
        }
    }

    #[test]
    fn test_classify_game_detects_anti_cheat_from_relative_paths_in_memory() {
        let engine = match_all_engine();
        let lang_detector = LangDetector::new();
        let never_cancel = AtomicBool::new(false);

        // Safe game entries
        let safe_entries = vec![entry("bin/Game.exe"), entry("Data/Audio/Voices.pck")];
        let prepared_safe = classify_game(
            &engine,
            &lang_detector,
            GameIdentity {
                id: 1,
                name: "Safe Game",
                install_dir: Path::new("C:/Games/Safe"),
                app_id: None,
            },
            safe_entries,
            &[],
            &never_cancel,
        )
        .expect("classify safe game");
        assert!(!prepared_safe.anti_cheat_protected);

        // EAC game entries (in memory only, non-existent disk path)
        let eac_entries = vec![
            entry("bin/Game.exe"),
            entry("EasyAntiCheat/easyanticheat_x64.dll"),
        ];
        let prepared_eac = classify_game(
            &engine,
            &lang_detector,
            GameIdentity {
                id: 2,
                name: "EAC Game",
                install_dir: Path::new("C:/Games/NonExistentEAC"),
                app_id: None,
            },
            eac_entries,
            &[],
            &never_cancel,
        )
        .expect("classify EAC game");
        assert!(prepared_eac.anti_cheat_protected);
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
        // "monolithic_archive" is not a synthetic unknown string: it is what
        // the removed in-place archive trimmer wrote, and a database from
        // that build can still carry rows with it. No `Category` variant
        // backs it any more, so it must parse to `None` just like any other
        // unrecognized category - which is what makes `worker::load` skip
        // such a row instead of showing it as an ordinary (and unsafe)
        // whole-file delete.
        assert_eq!(parse_source_key("monolithic_archive"), None);
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
}
