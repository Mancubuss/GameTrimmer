//! Regex rule engine for non-essential file categories (redist, docs, bonus, ...).
//! Rules are loaded from an external `rules.json` next to the executable.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::localized::{LocalizedText, DEFAULT_LANG};
use crate::reference::{GameReference, REFERENCE_CONFIDENCE, REFERENCE_MAX_DEPTH};

/// Redist and bonus rules only apply when the match occurs within this many
/// path segments from the game root (redist installers and bonus-material
/// folders live at the root or in a first/second-level folder, not deep
/// inside asset or engine trees such as `Launcher\QtQuick\Extras`).
const MAX_SHALLOW_DEPTH: usize = 2;

/// Supported major version of `rules.json`. A file with a greater version was
/// produced by a newer GameTrimmer - refuse it rather than silently misread
/// it, exactly as [`crate::langdetect::LANG_PACK_VERSION`] already does for
/// the localization pack.
///
/// The two packs materialize from the built-ins on first use and are then
/// never overwritten (`worker::ensure_rules_path`), so a hand edit or an
/// imported community pack wins permanently. Without a version in the file
/// there is no way, after the fact, to tell which rule set actually produced
/// a finding - only a diff of the whole file.
pub const RULE_PACK_VERSION: u32 = 1;

pub const MAX_RULE_PACK_BYTES: usize = 1024 * 1024;
pub const MAX_RULES: usize = 2_000;
pub const MAX_REGEX_BYTES: usize = 512;
pub const MAX_RULE_DEPTH: usize = 32;
pub const MAX_EXTENSIONS: usize = 32;
pub const MAX_EXTENSION_BYTES: usize = 16;

/// The repo's rules.json embedded at build time - the rules every scan
/// actually runs on. A `rules.json` next to the executable is an *optional
/// overlay* folded on top of these, absent on a normal install; the app no
/// longer materializes one, because a copy on disk is a copy that goes stale
/// silently against a newer binary. See `docs/rules-packs.md`.
///
/// This is the hand-written pack only - 51 rules. The per-game catalogue that
/// used to live here as 935 literal-alternation regexes is a table now; see
/// [`crate::reference`].
pub const BUILTIN_RULES_JSON: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../rules.json"));

/// Category of a non-essential file/folder. Serialized snake_case in rules.json
/// and in the `findings.category` DB column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    RedistFolder,
    RedistFile,
    DocsFolder,
    DocsFile,
    Bonus,
    DevLeftovers,
    Intro,
    WorkshopOrphan,
    DownloadingStaging,
    ShaderCache,
    CrashDump,
    DiagnosticLogs,
    SaveBloat,
    LauncherWebCache,
    ModManagerDownloads,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleProvenance {
    #[default]
    Builtin,
    ImportedUntrusted,
}

fn is_builtin_provenance(provenance: &RuleProvenance) -> bool {
    *provenance == RuleProvenance::Builtin
}

/// Where a rule's knowledge comes from - a guess, or a lookup.
///
/// A built-in rule is a *heuristic*: `^(.*[_. -])?logos?.*\.bik$` is a
/// pattern someone wrote because startup videos tend to be named that way,
/// and for a game nobody has catalogued it is the only answer available.
/// A reference rule is an *entry*: PCGamingWiki names this game's intro
/// videos one by one, so for that game there is nothing left to guess.
///
/// This is separate from [`RuleProvenance`], which answers a different
/// question - whether the rule came from outside and should be treated with
/// suspicion. A reference rule is ours and trusted; it simply knows more.
/// Keeping them apart also keeps `provenance` exactly as the database, the
/// bundle and the UI already store it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleOrigin {
    /// A pattern that generalizes over games. The shape of every rule
    /// written before this field existed, which is why it is the default.
    #[default]
    Builtin,
    /// A file list an external catalogue gives for one named game.
    Reference,
}

impl RuleOrigin {
    /// Precedence within one category: the lowest wins. A catalogue entry
    /// for *this* game beats a heuristic aimed at games in general, however
    /// confident the heuristic is - the heuristic is guessing at an answer
    /// the entry already has.
    fn rank(self) -> u8 {
        match self {
            RuleOrigin::Reference => 0,
            RuleOrigin::Builtin => 1,
        }
    }
}

fn is_builtin_origin(origin: &RuleOrigin) -> bool {
    *origin == RuleOrigin::Builtin
}

/// Which way a rule points: does matching a file make it a deletion candidate,
/// or does it forbid every other rule from claiming it?
///
/// The engine used to know only the first kind, and the only persistent "keep
/// this" the app had was the localization keep-list - which is coarse (a whole
/// language) and says nothing about one file in one game. A user's ticks lived
/// for exactly one scan, so every re-trim re-proposed what they had already
/// consciously rejected.
///
/// A veto is deliberately *not* a new mechanism beside the rules: an exception
/// differs from a deleting rule in this field and in [`Rule::app_id`] alone, so
/// it is written in the same file format and validated by the same parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RulePolarity {
    /// Matching makes the file a deletion candidate. The behaviour every rule
    /// written before this field existed has, which is why it is the default.
    #[default]
    Delete,
    /// Matching forbids *any* rule - and the localization detector after it -
    /// from claiming the file. See [`Verdict::Kept`].
    Keep,
}

fn is_delete_polarity(polarity: &RulePolarity) -> bool {
    *polarity == RulePolarity::Delete
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::RedistFolder => "redist_folder",
            Category::RedistFile => "redist_file",
            Category::DocsFolder => "docs_folder",
            Category::DocsFile => "docs_file",
            Category::Bonus => "bonus",
            Category::DevLeftovers => "dev_leftovers",
            Category::Intro => "intro",
            Category::WorkshopOrphan => "workshop_orphan",
            Category::DownloadingStaging => "downloading_staging",
            Category::ShaderCache => "shader_cache",
            Category::CrashDump => "crash_dump",
            Category::DiagnosticLogs => "diagnostic_logs",
            Category::SaveBloat => "save_bloat",
            Category::LauncherWebCache => "launcher_web_cache",
            Category::ModManagerDownloads => "mod_manager_downloads",
        }
    }

    /// Whether rules of this category match against directory segments
    /// (as opposed to the final file name segment).
    fn matches_folder_segments(self) -> bool {
        matches!(
            self,
            Category::RedistFolder
                | Category::DocsFolder
                | Category::Bonus
                | Category::Intro
                | Category::WorkshopOrphan
                | Category::DownloadingStaging
                | Category::ShaderCache
                | Category::CrashDump
                | Category::DiagnosticLogs
                | Category::SaveBloat
                | Category::LauncherWebCache
                | Category::ModManagerDownloads
        )
    }

    /// Whether rules of this category are *also* tested against the file
    /// name, on top of the folder-segment test [`matches_folder_segments`]
    /// already runs for them. A handful of folder-segment categories mix a
    /// folder-name rule (a `CrashDumps` folder, a `Logos` folder) with a
    /// file-name rule under the very same category (a bare `*.dmp`, a bare
    /// `nvidia_logo.bik`) - without this, the file-name rule can only ever
    /// match a directory that happens to be named after a file, which is to
    /// say never. Every other folder-segment category writes folder-name
    /// patterns exclusively, so leaving them out here costs them nothing.
    fn matches_file_names(self) -> bool {
        matches!(
            self,
            Category::Intro | Category::CrashDump | Category::DiagnosticLogs
        )
    }

    /// Whether rules of this category are restricted to shallow matches
    /// (see [`MAX_SHALLOW_DEPTH`]).
    ///
    /// Intro, CrashDump and DiagnosticLogs joined this list once their rules
    /// turned out to reach an unbounded number of segments deep into an asset
    /// tree - a broad, case-insensitive prefix regex like `unreal.*\.bik` is
    /// safe next to a `Movies` folder and a false-positive match on a unique
    /// cutscene anywhere else. [`MAX_SHALLOW_DEPTH`] alone is tighter than the
    /// video/log/dump file rules in these three categories actually need
    /// (`Whiplash\GameSDK\Videos\LegalScreens.bk2`, `Saved\Crashes\*.dmp`,
    /// `Saved\Logs\player.log` all sit one segment past it), so those rules
    /// carry their own [`Rule::max_depth`] override rather than the category
    /// default - the same mechanism the redist file rules already use for a
    /// nested `Support\Software\VCRedist\` layout.
    fn is_depth_limited(self) -> bool {
        matches!(
            self,
            Category::RedistFolder
                | Category::RedistFile
                | Category::Bonus
                | Category::Intro
                | Category::CrashDump
                | Category::DiagnosticLogs
        )
    }

    /// Precedence when several rules match one file: the lowest rank wins
    /// regardless of confidence, and confidence only breaks ties within one
    /// rank. Ordered by how reliably the category is identified.
    fn priority_rank(self) -> u8 {
        match self {
            Category::RedistFolder | Category::RedistFile => 0,
            Category::Intro => 1,
            Category::DevLeftovers | Category::CrashDump | Category::DiagnosticLogs => 2,
            Category::WorkshopOrphan
            | Category::DownloadingStaging
            | Category::ShaderCache
            | Category::LauncherWebCache
            | Category::ModManagerDownloads
            | Category::SaveBloat => 3,
            Category::Bonus => 4,
            Category::DocsFolder | Category::DocsFile => 5,
        }
    }
}

/// One rule from rules.json. `Serialize` keeps the round trip lossless for
/// the personal exception pack, which is read, appended to and written back
/// (see `crate::packs::add_rule`).
///
/// # Versioning of the two fields added for exceptions
///
/// [`RULE_PACK_VERSION`] deliberately stays at 1 now that `polarity` and
/// `app_id` exist. Both follow the `provenance` pattern - `default` +
/// `skip_serializing_if` - so a pack that uses neither serializes byte for
/// byte as it did before, and bumping the version would make an older build
/// refuse packs it understands perfectly well.
///
/// A pack that *does* use them is refused by an older build either way: the
/// struct is `deny_unknown_fields`, so `polarity` fails the parse there with
/// "unknown field `polarity`" rather than with "version newer than
/// supported". Both are total refusals of the whole file; only the wording
/// differs, and buying the better wording costs every unchanged pack its
/// compatibility. Should that trade ever flip (a pack format change an old
/// build would silently *misread* rather than reject), that is the version's
/// job and it must be bumped then.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// The category a matching file is filed under. Absent on a keep rule
    /// (see [`RulePolarity::Keep`]), which files nothing - it only forbids.
    /// Required on a deleting rule; [`RuleEngine::from_json_in`] says so by
    /// name rather than letting a category-less rule quietly classify
    /// everything it matches as some arbitrary default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
    /// Case-insensitive regex. Folder rules match one path segment,
    /// file rules match the file name - and a keep rule matches the whole
    /// `\`-separated path relative to the game root, because "never touch
    /// this file" names one file, not a name that may recur anywhere in the
    /// tree.
    pub pattern: String,
    /// Human-readable description, e.g. "MS Visual C++ Redist". Either one
    /// string or one per language - see [`LocalizedText`].
    pub desc: LocalizedText,
    /// 0-100. Absent on a keep rule: a veto is not a guess, and there is
    /// nothing for it to outrank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<u8>,
    /// Which way this rule points; see [`RulePolarity`].
    #[serde(default, skip_serializing_if = "is_delete_polarity")]
    pub polarity: RulePolarity,
    /// Restricts the rule to one game, by the vendor id stored in
    /// `games.app_id` (a Steam appid, a GOG product id, ...). `None` - the
    /// shape of every rule written before this field - means every game.
    ///
    /// This is what makes a personal exception personal: "never touch this
    /// file *in my game*" must not quietly protect a same-named file in the
    /// other four hundred. The same field is what a community recipe will be
    /// bound with, which is why it lives on `Rule` rather than only on the
    /// keep side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(default, skip_serializing_if = "is_builtin_provenance")]
    pub provenance: RuleProvenance,
    /// Whether this rule generalizes or looks up; see [`RuleOrigin`]. Only
    /// meaningful together with [`Rule::app_id`] - a reference entry is
    /// about one named game by definition, and the parser says so.
    #[serde(default, skip_serializing_if = "is_builtin_origin")]
    pub origin: RuleOrigin,
    /// Optional per-rule override of the category's default depth limit
    /// ([`MAX_SHALLOW_DEPTH`] for the categories [`Category::is_depth_limited`]
    /// names, unlimited for the rest).
    /// Lets a highly specific pattern (e.g. `vc_redist.*.exe`) match inside
    /// nested vendor folders like `Support\Software\VCRedist\` without
    /// loosening the shallow default that keeps generic patterns away from
    /// deep asset trees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<usize>,
    /// Optional whitelist of file extensions (lowercase, without the dot).
    /// When set, the rule only matches files whose extension is listed -
    /// used by generic folder-name rules (e.g. the bonus "extras" pattern)
    /// to demand content-type evidence (artbooks, music, video) instead of
    /// trusting the folder name alone: `Extras\artbook.pdf` is bonus
    /// material, `QtQuick\Extras\Extras.qml` is program code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
    /// Whether what this rule matches is **content in the player's language**
    /// rather than a screen the game plays on the way in.
    ///
    /// This is the line the keep-language list is drawn along, and it is
    /// per-rule because the categories do not draw it. It decides one thing,
    /// in [`crate::worker::keep_language_vetoes_rule`]: a file carrying one
    /// of the user's kept languages is off limits when the rule that claimed
    /// it says `localized_content`, and is removed otherwise. Removing the
    /// eighteen legal screens nobody in the room can read while protecting
    /// the one that actually plays is the failure this exists to prevent.
    ///
    /// **No rule in the built-in pack sets it.** The attract reel was the one
    /// that did - a five- to two-hundred-megabyte gameplay video the game
    /// loops on the idle title screen, which Bully ships as
    /// `AttractModeF/G/I/J/R/S.wmv` and Kane & Lynch as
    /// `Attract01_French.bik`. It gave the flag up because a keep-language
    /// veto answers "do I want this startup video gone" on the player's
    /// behalf and without telling them, while the reel is already offered
    /// under `app::model::REVIEW_CONFIDENCE_THRESHOLD`, so it arrives carrying
    /// the review mark - and, like everything else since GT-89, unticked. It
    /// is removed only if the player ticks it, and kept for good by a personal
    /// exception if they would rather never see it proposed again. Which
    /// startup screens go is not a decision a shipped pack should be making
    /// out of the player's sight.
    ///
    /// The field stays because the pack format is not only ours: a personal
    /// or imported rule that genuinely does name content in the player's
    /// language declares it here and gets the veto.
    ///
    /// Defaults to `false`, so every pack written before this field existed
    /// keeps its exact behaviour and no rule has to declare it. A rule that
    /// does not set it is asserting the common case: what I match is a
    /// screen.
    #[serde(default, skip_serializing_if = "is_not_localized_content")]
    pub localized_content: bool,
}

fn is_not_localized_content(localized_content: &bool) -> bool {
    !*localized_content
}

impl Rule {
    /// A personal exception: never touch `rel_path` in the game whose vendor
    /// id is `app_id`.
    ///
    /// The pattern is built here rather than in the UI so that exactly one
    /// place decides what "this file" means as a regex: the whole relative
    /// path, anchored at both ends and escaped, so a path full of regex
    /// metacharacters (`Data\[DLC]\readme (1).txt` is an ordinary Windows
    /// name) protects that one file and not a family of them.
    pub fn keep_file(app_id: &str, rel_path: &str, desc: LocalizedText) -> Self {
        Self {
            category: None,
            pattern: format!("^{}$", regex::escape(rel_path)),
            desc,
            confidence: None,
            polarity: RulePolarity::Keep,
            // A hand-written exception is the user's own decision about their
            // own machine, not an imported pack of someone else's rules - the
            // untrusted marking exists to warn about the latter.
            provenance: RuleProvenance::Builtin,
            // A veto ranks against nothing, so it has nothing to look up.
            origin: RuleOrigin::Builtin,
            // A veto outranks the keep-language list as it outranks every
            // other ranking; the parser refuses a keep rule that sets this.
            localized_content: false,
            app_id: Some(app_id.to_string()),
            max_depth: None,
            extensions: None,
        }
    }
}

/// A whole `rules.json`: the version marker and the rules it carries.
///
/// The version is the only reason this wraps the list instead of being one -
/// see [`RULE_PACK_VERSION`]. It is deliberately not an `Option`: a file
/// without the field is not "version 0", it is a file this build has no way
/// to interpret, and saying so at the parse is better than guessing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulePack {
    pub version: u32,
    pub rules: Vec<Rule>,
}

/// Parses a rules.json without compiling the regexes - the parse used by the
/// import merge (`crate::packs`), where validation happens separately through
/// [`RuleEngine::from_json`]. A pack from a newer GameTrimmer is refused
/// here, before any rule of it is looked at.
pub fn parse_rule_list(json: &str) -> Result<Vec<Rule>> {
    // `monolithic_archive` is a reserved category name: no `Category` variant
    // has ever existed for a personal or imported rule to legitimately claim
    // it (only the now-removed archive inspector produced it, directly as a
    // `Finding`, never through a parsed rule). Checked here, against the raw
    // JSON, before the typed parse below: `Category` deserializes strictly,
    // so a pack naming an unknown variant already fails that parse - this
    // exists only to give the specific reserved-name rejection a chance to
    // fire first, with a clearer message than serde's "unknown variant".
    if let Ok(serde_json::Value::Object(pack)) = serde_json::from_str(json) {
        if let Some(serde_json::Value::Array(rules)) = pack.get("rules") {
            for (index, rule) in rules.iter().enumerate() {
                if rule.get("category").and_then(serde_json::Value::as_str)
                    == Some("monolithic_archive")
                {
                    return Err(CoreError::Other(format!(
                        "rules.json: rule #{index} uses reserved category monolithic_archive"
                    )));
                }
            }
        }
    }
    let pack: RulePack = serde_json::from_str(json)?;
    if pack.version > RULE_PACK_VERSION {
        return Err(CoreError::Other(format!(
            "rules.json version {} is newer than supported {RULE_PACK_VERSION} - \
             update GameTrimmer, or use an older rules pack",
            pack.version,
        )));
    }
    Ok(pack.rules)
}

/// Serializes a rule list as a complete pack, stamped with the version this
/// build writes. Every path that produces a rules.json goes through here, so
/// a merged or restored file can never come out unversioned.
pub fn serialize_rule_list(rules: &[Rule]) -> Result<String> {
    serde_json::to_string_pretty(&RulePack {
        version: RULE_PACK_VERSION,
        rules: rules.to_vec(),
    })
    .map_err(CoreError::from)
}

pub use crate::models::Finding;

/// Everything the engine can conclude about one file.
///
/// The third state is the reason this is not an `Option<Finding>`: "no rule
/// claimed it" and "a rule forbade claiming it" have to reach the caller as
/// different answers, because the localization detector runs *after* the rule
/// engine and only on files no rule claimed (see `combine_finding` in the
/// app's scan worker). Collapsing a veto into "no match" would let a kept file
/// come straight back as a localization finding - which is exactly the file
/// the user just said never to touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// No rule matched.
    Unmatched,
    /// A keep rule matched: the file is off limits to every later stage too.
    Kept,
    /// The winning deleting rule's finding.
    Flagged(Finding),
}

impl Verdict {
    /// The deletion candidate this verdict names, if it names one. For the
    /// callers - benchmarks, corpus checks - that only ever cared about what
    /// was flagged and treat a veto exactly like a non-match.
    pub fn flagged(self) -> Option<Finding> {
        match self {
            Verdict::Flagged(finding) => Some(finding),
            Verdict::Unmatched | Verdict::Kept => None,
        }
    }
}

/// A rule with its pattern already compiled to a case-insensitive [`Regex`].
#[derive(Debug, Clone)]
struct CompiledRule {
    category: Category,
    regex: Regex,
    desc: String,
    confidence: u8,
    provenance: RuleProvenance,
    /// See [`RuleOrigin`].
    origin: RuleOrigin,
    /// See [`Rule::localized_content`].
    localized_content: bool,
    /// The effective depth limit for this rule: the rule's own `max_depth`
    /// if given, otherwise the category default (see [`Rule::max_depth`]).
    max_depth: usize,
    /// Lowercased extension whitelist, if the rule declares one
    /// (see [`Rule::extensions`]).
    extensions: Option<HashSet<String>>,
}

/// A keep rule with its pattern compiled. Carries none of the deleting rule's
/// machinery - a veto has no category to rank, no confidence to weigh, no
/// depth limit and no extension whitelist, and the parser rejects a keep rule
/// that sets any of them rather than accepting a field that would do nothing.
#[derive(Debug)]
struct CompiledKeep {
    /// Matched against the whole relative path, not a single segment.
    regex: Regex,
    app_id: Option<String>,
}

/// Whether a keep rule scoped to `scope` applies while classifying a game
/// whose vendor id is `app_id`. An unscoped veto applies to everything.
/// (Deleting rules answer the same question through
/// [`RuleEngine::scoped`], which is a lookup rather than a scan - there are
/// hundreds of them and only ever a handful of vetoes.)
fn scope_applies(scope: &Option<String>, app_id: Option<&str>) -> bool {
    match scope {
        None => true,
        Some(scope) => app_id.is_some_and(|id| id == scope),
    }
}

#[derive(Debug, Default)]
pub struct RuleEngine {
    /// The rules that apply to every game. Walked for every file of every
    /// game, which is what makes their number the scan's inner loop.
    rules: Vec<CompiledRule>,
    /// The rules bound to one game, bucketed by that game's vendor id.
    ///
    /// A map rather than a tag inside `rules` for the same reason `keeps` is
    /// a separate list: the reference pack is ~950 rules and a scan walks
    /// 4.9 million files, so leaving them in the main list would cost five
    /// billion "is this your game?" comparisons to answer "no" every time.
    /// Here a game pays one hash lookup and then walks only its own handful.
    scoped: HashMap<String, Vec<CompiledRule>>,
    /// Split out from `rules` rather than filtered out of it per file: on a
    /// default install this is empty, so honouring the veto costs one
    /// `is_empty` per file over a 4.9-million-file scan instead of a polarity
    /// branch inside the loop that runs for every rule of every file.
    keeps: Vec<CompiledKeep>,
    /// The subset of `rules` that recognises a folder as bonus material by
    /// name. Duplicated here rather than filtered out of `rules` per file:
    /// there are two of them, and `classify` needs them *after* the ranking
    /// loop, where the extension whitelist that keeps a program file out of
    /// the bonus category has already excluded them from the loop itself.
    ///
    /// Built-in and unscoped only. A reference rule is about one named game's
    /// files, not about a folder that swallows whatever is under it.
    bonus_folders: Vec<CompiledRule>,
    /// What an external catalogue names for one game, as a table rather than
    /// as ~950 more compiled patterns - see [`crate::reference`].
    ///
    /// Empty unless a caller attaches one ([`Self::with_reference`]). A rule
    /// pack is validated by compiling it, and validation has no business
    /// consulting the shipped catalogue; the scan attaches it explicitly.
    reference: GameReference,
}

impl RuleEngine {
    /// Builds the engine from rules.json text, describing its findings in
    /// English. This is the form used to *validate* an incoming rule pack,
    /// where no interface language is in play.
    pub fn from_json(json: &str) -> Result<Self> {
        Self::from_json_in(json, DEFAULT_LANG)
    }

    /// Builds the engine from rules.json text, resolving every description
    /// into `lang` once, here, rather than per matched file: `classify` runs
    /// over every file of every game, and the engine is rebuilt whenever the
    /// interface language changes anyway.
    pub fn from_json_in(json: &str, lang: &str) -> Result<Self> {
        if json.len() > MAX_RULE_PACK_BYTES {
            return Err(CoreError::Other(format!(
                "rules.json exceeds the {} byte limit",
                MAX_RULE_PACK_BYTES
            )));
        }
        let raw_rules = parse_rule_list(json)?;
        if raw_rules.len() > MAX_RULES {
            return Err(CoreError::Other(format!(
                "rules.json contains {} rules; the limit is {MAX_RULES}",
                raw_rules.len()
            )));
        }

        let mut rules = Vec::with_capacity(raw_rules.len());
        let mut scoped: HashMap<String, Vec<CompiledRule>> = HashMap::new();
        let mut keeps = Vec::new();
        let mut bonus_folders = Vec::new();
        for (index, rule) in raw_rules.into_iter().enumerate() {
            if rule.pattern.len() > MAX_REGEX_BYTES {
                return Err(CoreError::Other(format!(
                    "rules.json: rule #{index} regex exceeds {MAX_REGEX_BYTES} bytes"
                )));
            }
            if rule.confidence.is_some_and(|confidence| confidence > 100) {
                return Err(CoreError::Other(format!(
                    "rules.json: rule #{index} confidence must be in 0..=100"
                )));
            }
            if rule.max_depth.is_some_and(|depth| depth > MAX_RULE_DEPTH) {
                return Err(CoreError::Other(format!(
                    "rules.json: rule #{index} max_depth exceeds {MAX_RULE_DEPTH}"
                )));
            }
            if let Some(extensions) = &rule.extensions {
                if extensions.len() > MAX_EXTENSIONS {
                    return Err(CoreError::Other(format!(
                        "rules.json: rule #{index} has more than {MAX_EXTENSIONS} extensions"
                    )));
                }
                for extension in extensions {
                    if extension.is_empty()
                        || extension.len() > MAX_EXTENSION_BYTES
                        || !extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
                    {
                        return Err(CoreError::Other(format!(
                            "rules.json: rule #{index} has invalid extension `{extension}`"
                        )));
                    }
                }
            }
            if rule.desc.is_empty() {
                return Err(CoreError::Other(format!(
                    "rules.json: rule #{index} (category {:?}, pattern `{}`) has no description; \
                     a finding the user cannot read is worse than no rule at all",
                    rule.category, rule.pattern
                )));
            }
            let desc = rule.desc.get(lang).to_string();
            let regex = RegexBuilder::new(&rule.pattern)
                .case_insensitive(true)
                .size_limit(MAX_RULE_PACK_BYTES)
                .build()
                .map_err(|err| {
                    CoreError::Other(format!(
                        "rules.json: invalid regex in rule #{index} (category {:?}, desc \"{desc}\", pattern `{}`): {err}",
                        rule.category, rule.pattern
                    ))
                })?;

            if rule.polarity == RulePolarity::Keep {
                // Every field below belongs to the ranking a veto does not
                // take part in. Accepting them silently would leave a rule
                // that looks tuned and is not - the same trap a rule with no
                // description is, and refused for the same reason.
                if rule.category.is_some()
                    || rule.confidence.is_some()
                    || rule.max_depth.is_some()
                    || rule.extensions.is_some()
                    || rule.localized_content
                    || rule.origin != RuleOrigin::Builtin
                {
                    return Err(CoreError::Other(format!(
                        "rules.json: keep rule #{index} (pattern `{}`) sets category, confidence, \
                         max_depth, extensions, localized_content or origin; a veto ranks against \
                         nothing, matches the whole relative path and already outranks the \
                         keep-language list, so none of them would do anything",
                        rule.pattern
                    )));
                }
                keeps.push(CompiledKeep {
                    regex,
                    app_id: rule.app_id,
                });
                continue;
            }

            let Some(category) = rule.category else {
                return Err(CoreError::Other(format!(
                    "rules.json: rule #{index} (desc \"{desc}\", pattern `{}`) has no category; \
                     only a keep rule may omit it",
                    rule.pattern
                )));
            };
            let Some(confidence) = rule.confidence else {
                return Err(CoreError::Other(format!(
                    "rules.json: rule #{index} (desc \"{desc}\", pattern `{}`) has no confidence; \
                     only a keep rule may omit it",
                    rule.pattern
                )));
            };

            // A reference rule that names no game is not a lookup, it is a
            // heuristic wearing a lookup's badge: it would outrank every
            // built-in pattern in its category for every game in the library.
            // Refused rather than quietly demoted - the pack author meant one
            // of the two, and only they know which.
            if rule.origin == RuleOrigin::Reference && rule.app_id.is_none() {
                return Err(CoreError::Other(format!(
                    "rules.json: rule #{index} (desc \"{desc}\", pattern `{}`) is a reference \
                     rule with no app_id; a catalogue entry is about one named game",
                    rule.pattern
                )));
            }

            let default_depth = if category.is_depth_limited() {
                MAX_SHALLOW_DEPTH
            } else {
                usize::MAX
            };
            let compiled = CompiledRule {
                category,
                regex,
                desc,
                confidence,
                provenance: rule.provenance,
                origin: rule.origin,
                localized_content: rule.localized_content,
                max_depth: rule.max_depth.unwrap_or(default_depth),
                extensions: rule.extensions.map(|list| {
                    list.into_iter()
                        .map(|ext| ext.to_ascii_lowercase())
                        .collect()
                }),
            };
            if compiled.category == Category::Bonus
                && compiled.origin == RuleOrigin::Builtin
                && rule.app_id.is_none()
            {
                bonus_folders.push(compiled.clone());
            }
            match rule.app_id {
                Some(app_id) => scoped.entry(app_id).or_default().push(compiled),
                None => rules.push(compiled),
            }
        }

        Ok(Self {
            rules,
            scoped,
            keeps,
            bonus_folders,
            reference: GameReference::default(),
        })
    }

    /// Attaches the per-game catalogue this engine consults - see
    /// [`crate::reference`].
    ///
    /// Separate from [`Self::from_json_in`] because the catalogue is not a
    /// rule pack and is not parsed from the same file: every caller that
    /// merely *validates* a pack (an import, a personal exception being
    /// added) leaves it empty, and only the scan attaches the shipped one.
    #[must_use]
    pub fn with_reference(mut self, reference: GameReference) -> Self {
        self.reference = reference;
        self
    }

    /// Folds another engine's rules into this one, as the scan does with the
    /// personal exception pack on top of `rules.json`.
    ///
    /// Order matters for nothing but ties: precedence is decided by category
    /// rank, origin and confidence, and the veto is checked before any of
    /// them.
    ///
    /// The catalogue is not folded: it is shipped data attached once by the
    /// scan, not something an absorbed pack can carry.
    pub fn absorb(&mut self, other: RuleEngine) {
        self.rules.extend(other.rules);
        for (app_id, rules) in other.scoped {
            self.scoped.entry(app_id).or_default().extend(rules);
        }
        self.keeps.extend(other.keeps);
        self.bonus_folders.extend(other.bonus_folders);
    }

    /// Loads and builds the engine from a rules.json file, describing its
    /// findings in English.
    pub fn load(path: &Path) -> Result<Self> {
        Self::load_in(path, DEFAULT_LANG)
    }

    /// Loads and builds the engine from a rules.json file, describing its
    /// findings in `lang` - see [`from_json_in`](Self::from_json_in).
    pub fn load_in(path: &Path, lang: &str) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::from_json_in(&text, lang)
    }

    /// Classifies one file by its path relative to the game root
    /// (`\`-separated, as produced by the scanner), in the game whose vendor
    /// id is `app_id` (`None` for a game no launcher gave one - a folder-scan
    /// or manual library).
    ///
    /// Precedence, outermost first:
    ///
    /// 1. **A keep rule vetoes everything.** This is the rank below every
    ///    category rank that [`Category::priority_rank`] describes as "lowest
    ///    rank wins regardless of confidence" - taken to its conclusion: a
    ///    veto is not a better classification, it is the refusal of any. It
    ///    also outranks the stage *after* this one, which no `Category` can
    ///    express, and that is why it is a [`Verdict`] rather than a rank.
    /// 2. Among the deleting rules that match, the one whose category has the
    ///    highest precedence wins ([`Category::priority_rank`]).
    /// 3. Within one category, a rule that *looked the answer up* beats one
    ///    that *guessed* it ([`RuleOrigin::rank`]), whatever the guess's
    ///    confidence. A catalogue naming this game's intro videos one by one
    ///    is not competing with the heuristic - it is what the heuristic is
    ///    an approximation of.
    /// 4. Confidence breaks the remaining ties.
    /// 5. Finally, a winner that *guessed* is re-labelled `bonus` if its path
    ///    runs through a folder recognised by name as bonus material - see
    ///    [`Self::absorbed_by_bonus_folder`]. This step re-labels, it never
    ///    flags, and it never touches a vetoed file or a reference rule's
    ///    verdict.
    ///
    /// A rule scoped to a game (see [`Rule::app_id`]) takes part in neither
    /// step while any other game is being classified.
    pub fn classify(&self, rel_path: &str, app_id: Option<&str>) -> Verdict {
        // Before anything is matched, and before the path is even split: a
        // vetoed file is not classified at all. Empty on a default install,
        // so this costs one length check per file.
        for keep in &self.keeps {
            if scope_applies(&keep.app_id, app_id) && keep.regex.is_match(rel_path) {
                return Verdict::Kept;
            }
        }

        // A protected container is never a whole-file deletion target -
        // GameTrimmer has no way to remove just the parts of a container
        // that are safe to lose. An ordinary or imported rule may
        // use any display category it wants; letting it claim `voices.pck`
        // as `docs_file` would otherwise smuggle the container into the
        // whole-file delete path.
        if crate::worker::is_protected_container(rel_path) {
            return Verdict::Unmatched;
        }

        let segments: Vec<&str> = rel_path
            .split(['\\', '/'])
            .filter(|segment| !segment.is_empty())
            .collect();

        let (file_name, folder_segments) = match segments.split_last() {
            Some((file_name, folders)) => (*file_name, folders),
            None => return Verdict::Unmatched,
        };
        let file_ext = file_name
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase());

        // The winner and the key it won with: category rank, then origin
        // rank, then confidence (negated so that, like the two ranks in front
        // of it, smaller is better and one tuple comparison decides).
        let mut best: Option<((u8, u8, i16), Finding)> = None;

        // The catalogue enters the ranking as one more candidate rather than
        // short-circuiting it, so precedence keeps meaning exactly what
        // `RuleOrigin::rank` says: an entry beats a *heuristic* in its own
        // category, and still loses to a higher-ranked category (a redist
        // installer that happens to share a name is a redist installer). This
        // is a lookup, so a file in a game nobody catalogued pays one hash
        // miss for it.
        let file_depth = folder_segments.len() + 1;
        if let Some(desc) = app_id
            .filter(|_| file_depth <= REFERENCE_MAX_DEPTH)
            .and_then(|id| self.reference.intro_desc_for(id, file_name))
        {
            best = Some((
                (
                    Category::Intro.priority_rank(),
                    RuleOrigin::Reference.rank(),
                    -(REFERENCE_CONFIDENCE as i16),
                ),
                Finding {
                    category: Category::Intro,
                    rule_desc: desc.to_string(),
                    confidence: REFERENCE_CONFIDENCE,
                    // Ours and trusted, exactly as the reference rules it
                    // replaces were - `provenance` marks a pack that came
                    // from outside, which this did not.
                    provenance: RuleProvenance::Builtin,
                    // A startup screen, not content in the player's language.
                    // Same assertion every reference rule made by omitting
                    // the field; see `Rule::localized_content`.
                    localized_content: false,
                },
            ));
        }

        // One hash lookup for the whole scoped pack, instead of asking every
        // one of its rules whether it is about this game.
        let scoped = app_id
            .and_then(|id| self.scoped.get(id))
            .map_or(&[][..], Vec::as_slice);

        for rule in self.rules.iter().chain(scoped) {
            if let Some(allowed) = &rule.extensions {
                let ext_listed = file_ext.as_deref().is_some_and(|ext| allowed.contains(ext));
                if !ext_listed {
                    continue;
                }
            }

            let is_match = if rule.category.matches_folder_segments() {
                let folder_match = folder_segments.iter().enumerate().any(|(i, segment)| {
                    let depth = i + 1;
                    depth <= rule.max_depth && rule.regex.is_match(segment)
                });
                let file_match = if rule.category.matches_file_names() {
                    let file_depth = folder_segments.len() + 1;
                    file_depth <= rule.max_depth && rule.regex.is_match(file_name)
                } else {
                    false
                };
                folder_match || file_match
            } else {
                let file_depth = folder_segments.len() + 1;
                file_depth <= rule.max_depth && rule.regex.is_match(file_name)
            };

            if !is_match {
                continue;
            }

            let key = (
                rule.category.priority_rank(),
                rule.origin.rank(),
                -(rule.confidence as i16),
            );
            if best.as_ref().is_none_or(|(best_key, _)| key < *best_key) {
                best = Some((
                    key,
                    Finding {
                        category: rule.category,
                        rule_desc: rule.desc.clone(),
                        confidence: rule.confidence,
                        provenance: rule.provenance,
                        localized_content: rule.localized_content,
                    },
                ));
            }
        }

        match best {
            Some((key, finding)) => {
                Verdict::Flagged(self.absorbed_by_bonus_folder(key, finding, folder_segments))
            }
            None => Verdict::Unmatched,
        }
    }

    /// Re-labels a finding whose path runs through a folder recognised by
    /// name as bonus material.
    ///
    /// A `Blood and Wine extras` folder is one pile to the player - the
    /// artbook, the comic, the soundtrack and the `Thumbs.db` left beside
    /// them - and answering "development leftovers, documentation, bonus
    /// material" for the three files in it is an answer nobody asked for.
    /// The folder wins, and the per-file rule only supplies the fact that
    /// there was something to classify at all.
    ///
    /// Three things this deliberately does not do:
    ///
    /// * It never *creates* a finding. A file no rule flagged (a `.dll` under
    ///   `Extras`, which the bonus rule's extension whitelist excludes on
    ///   purpose) stays unflagged, so mistaking a folder for a bonus folder
    ///   costs a wrong label and never a wrongly deletable folder.
    /// * It never overrules a reference rule ([`RuleOrigin::Reference`]),
    ///   which knows this game by name, with a folder name that guesses.
    /// * It leaves [`Category::Intro`] alone. Every other category here is a
    ///   plain deletion, so moving a file between them changes a label; an
    ///   intro is *replaced by a stub* instead (see the stub contract in
    ///   `crate::retrim`), so re-labelling one would quietly swap a stub for
    ///   an outright deletion. That is a change of mechanism, not of
    ///   category, and this step only decides categories.
    /// * It never runs on a vetoed file: [`Self::classify`] returns
    ///   [`Verdict::Kept`] before any of this, and `localized_content` is
    ///   carried over rather than taken from the bonus rule so that the
    ///   language veto in [`crate::worker::keep_language_vetoes_rule`] still
    ///   sees the file it was meant to see.
    fn absorbed_by_bonus_folder(
        &self,
        key: (u8, u8, i16),
        finding: Finding,
        folder_segments: &[&str],
    ) -> Finding {
        let (_, origin_rank, _) = key;
        if finding.category == Category::Bonus
            || finding.category == Category::Intro
            || origin_rank == RuleOrigin::Reference.rank()
        {
            return finding;
        }
        let Some(rule) = self.bonus_folders.iter().find(|rule| {
            folder_segments.iter().enumerate().any(|(index, segment)| {
                let depth = index + 1;
                depth <= rule.max_depth && rule.regex.is_match(segment)
            })
        }) else {
            return finding;
        };
        Finding {
            category: Category::Bonus,
            rule_desc: rule.desc.clone(),
            confidence: rule.confidence,
            provenance: rule.provenance,
            localized_content: finding.localized_content,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wraps a bare rule array in the pack envelope the format requires, so
    /// a test about rule *semantics* does not restate the wrapper each time.
    /// The envelope itself is covered directly by the tests further down.
    fn pack(rules: &str) -> String {
        format!(r#"{{"version":{RULE_PACK_VERSION},"rules":{rules}}}"#)
    }

    fn sample_json() -> String {
        pack(
            r#"[
            {"category": "redist_folder", "pattern": "^_?commonredist(s)?$", "desc": "Common redist folder", "confidence": 90},
            {"category": "redist_file", "pattern": "^vcredist.*\\.exe$", "desc": "MS Visual C++ Redist", "confidence": 95}
        ]"#,
        )
    }

    #[test]
    fn from_json_parses_minimal_rule_set() {
        let engine = RuleEngine::from_json(&sample_json()).expect("valid rules should parse");
        assert_eq!(engine.rules.len(), 2);
        assert_eq!(engine.rules[0].category, Category::RedistFolder);
        assert_eq!(engine.rules[1].confidence, 95);
    }

    #[test]
    fn classify_finds_common_redist_folder_and_file_with_highest_confidence() {
        let engine = RuleEngine::from_json(&sample_json()).unwrap();

        let finding = engine
            .classify("_CommonRedist\\vcredist_x64.exe", None)
            .flagged()
            .expect("should match both the folder and the file rule");

        // The file rule (95) beats the folder rule (90).
        assert_eq!(finding.confidence, 95);
        assert_eq!(finding.category, Category::RedistFile);
    }

    #[test]
    fn classify_matches_docs_folder_and_file() {
        let engine = shipped_engine();

        let finding = engine
            .classify("manual\\game_manual.pdf", None)
            .flagged()
            .expect("manual folder + pdf file should be classified as docs");

        // The dedicated readme/eula/manual rule (88) outranks the generic
        // PDF/RTF rule (85) within the same docs category.
        assert_eq!(finding.confidence, 88);
        assert!(matches!(
            finding.category,
            Category::DocsFolder | Category::DocsFile
        ));
    }

    #[test]
    fn classify_matches_legal_text_folders_without_an_extension() {
        // Found by the 2026-07-26 corpus review: these were the only four
        // rows it tagged as documentation that no rule reached. The files
        // themselves are named after the language and carry no extension
        // (`TermsOfService\de`), so only a folder rule with no extension
        // filter can see them.
        let engine = shipped_engine();

        for path in [
            "Siren\\Content\\Data\\TermsOfService\\de",
            "Siren\\Content\\Data\\PrivacyPolicy\\pt",
        ] {
            let finding = engine
                .classify(path, None)
                .flagged()
                .unwrap_or_else(|| panic!("{path} should be classified as documentation"));
            assert_eq!(finding.category, Category::DocsFolder);
        }
    }

    #[test]
    fn classify_returns_none_for_unremarkable_game_content() {
        let engine = shipped_engine();

        let finding = engine
            .classify("base\\sound\\music\\track01.ogg", None)
            .flagged();

        assert_eq!(finding, None);
    }

    #[test]
    fn classify_ignores_redist_pattern_match_beyond_max_depth() {
        let engine = shipped_engine();

        // "installations" contains the substring "install" but sits four
        // segments deep, well past MAX_REDIST_DEPTH, and is real game content.
        let finding = engine
            .classify("data\\assets\\textures\\installations\\wall.dds", None)
            .flagged();

        assert_eq!(finding, None);
    }

    #[test]
    fn classify_respects_a_per_rule_max_depth_override() {
        let json = r#"[
            {"category": "redist_file", "pattern": "^vc_?redist.*\\.exe$", "desc": "MS Visual C++ Redist", "confidence": 95, "max_depth": 4}
        ]"#;
        let engine = RuleEngine::from_json(&pack(json)).unwrap();

        // Depth 4 (3 folders + file) is beyond the category default of 2,
        // but within this rule's own override.
        let finding = engine
            .classify(r"Support\Software\VCRedist\vc_redist.x64.exe", None)
            .flagged()
            .expect("max_depth override should allow the deeper match");
        assert_eq!(finding.category, Category::RedistFile);

        // Depth 5 is beyond even the override.
        assert_eq!(
            engine
                .classify(r"a\b\c\d\vc_redist.x64.exe", None)
                .flagged(),
            None,
            "the override is still a limit, not an unlimited pass"
        );
    }

    #[test]
    fn repo_rules_classify_nested_vc_redist_as_redist_not_docs() {
        let engine = shipped_engine();

        // Real-world layout (Assassin's Creed Mirage): the redist installer
        // lives 3 folders deep under "Support", which alone matches a
        // low-confidence support/docs folder rule - the specific redist
        // file rule must win by category precedence.
        let finding = engine
            .classify(r"Support\Software\VCRedist\vc_redist.x64.exe", None)
            .flagged()
            .expect("vc_redist installer should be classified");

        assert_eq!(finding.category, Category::RedistFile);
    }

    #[test]
    fn classify_prefers_higher_priority_category_over_higher_confidence() {
        let engine = shipped_engine();

        // The docs rule for *.pdf (85) is more confident than the bonus
        // folder rule (80), but an artbook inside an extras folder is bonus
        // material - category precedence must beat raw confidence.
        let finding = engine
            .classify(r"Extras\artbook.pdf", None)
            .flagged()
            .expect("an artbook inside Extras should be classified");

        assert_eq!(finding.category, Category::Bonus);
    }

    #[test]
    fn classify_puts_support_folder_content_into_docs_not_bonus() {
        let engine = shipped_engine();

        // Support/help folders are reference material, but an archive-shaped
        // file is a protected container (see
        // `crate::worker::is_protected_container`) and must not become a
        // whole-file rule finding merely because of its parent folder.
        assert_eq!(
            engine.classify(r"Support\ru\voices.pak", None),
            Verdict::Unmatched
        );
        let finding = engine
            .classify(r"Support\ru\voices.dat", None)
            .flagged()
            .expect("support folder content should be classified");

        assert_eq!(finding.category, Category::DocsFolder);
    }

    #[test]
    fn classify_ignores_bonus_folder_deep_inside_program_trees() {
        let engine = shipped_engine();

        // Real-world layout (XCOM 2 launcher): `Extras` here is a Qt QML
        // module three segments deep, not a bonus-content folder. The
        // shallow depth limit must keep the bonus rule away from it even
        // for media-typed files.
        assert_eq!(
            engine
                .classify(r"Launcher\QtQuick\Extras\poster.png", None)
                .flagged(),
            None
        );
    }

    #[test]
    fn classify_bonus_folder_requires_media_or_document_content() {
        let engine = shipped_engine();

        let media = engine
            .classify(r"Extras\track01.mp3", None)
            .flagged()
            .expect("music inside Extras should be bonus material");
        assert_eq!(media.category, Category::Bonus);

        assert_eq!(
            engine.classify(r"Extras\plugin.dll", None).flagged(),
            None,
            "a program file is not bonus material even inside an extras folder"
        );
    }

    /// The Witcher 3's `Blood and Wine extras` is one pile of bonus material
    /// to the player. Before the folder absorbed its contents, the same pile
    /// came back as three answers: the `Thumbs.db` as development leftovers,
    /// the comic as documentation, and only the artbook as bonus material.
    #[test]
    fn a_bonus_folder_absorbs_the_leftovers_and_comics_beside_its_artbook() {
        let engine = shipped_engine();

        for path in [
            r"Blood and Wine extras\ARTBOOK\artbook.pdf",
            r"Blood and Wine extras\COMICS\comic.cbr",
            r"Blood and Wine extras\Thumbs.db",
            r"Hearts of Stone extras\WALLPAPERS\Thumbs.db",
        ] {
            let finding = engine
                .classify(path, None)
                .flagged()
                .unwrap_or_else(|| panic!("{path} should be classified"));
            assert_eq!(finding.category, Category::Bonus, "{path}");
        }
    }

    /// The price of the folder winning is paid in labels only. A file no rule
    /// claimed stays unclaimed, so mistaking a folder for a bonus folder can
    /// never turn its contents into deletion candidates wholesale.
    #[test]
    fn a_bonus_folder_does_not_flag_what_no_rule_claimed() {
        let engine = shipped_engine();

        assert_eq!(
            engine
                .classify(r"Blood and Wine extras\bin\plugin.dll", None)
                .flagged(),
            None
        );
    }

    /// Absorption re-labels a finding; it must not disarm the veto that runs
    /// after it. `localized_content` is what
    /// `crate::worker::keep_language_vetoes_rule` reads to keep a file in a
    /// language the user asked to keep, so it is carried over rather than
    /// taken from the bonus rule.
    #[test]
    fn a_bonus_folder_keeps_the_language_veto_armed() {
        let mut engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json loads");
        engine.absorb(
            RuleEngine::from_json(&pack(
                r#"[{"category": "dev_leftovers", "pattern": "^voice_ru\\.db$", "desc": "Russian voice bank", "confidence": 90, "localized_content": true}]"#,
            ))
            .expect("localized pack compiles"),
        );

        let finding = engine
            .classify(r"Blood and Wine extras\voice_ru.db", None)
            .flagged()
            .expect("the localized file should be classified");

        assert_eq!(finding.category, Category::Bonus);
        assert!(
            finding.localized_content,
            "the language veto reads this flag and must still see it"
        );
    }

    /// A reference rule knows this game by name; a folder name guesses. The
    /// guess does not get to overwrite the lookup.
    #[test]
    fn a_bonus_folder_does_not_overrule_a_reference_rule() {
        let mut engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json loads");
        engine.absorb(
            RuleEngine::from_json(&pack(
                r#"[{"category": "intro", "pattern": "^studio_logo\\.bik$", "desc": "PCGamingWiki entry", "confidence": 70, "app_id": "292030", "origin": "reference"}]"#,
            ))
            .expect("reference pack compiles"),
        );

        let finding = engine
            .classify(r"Blood and Wine extras\studio_logo.bik", Some("292030"))
            .flagged()
            .expect("the catalogued intro should be classified");

        assert_eq!(finding.category, Category::Intro);
    }

    #[test]
    fn classify_identifies_intro_and_logo_files_and_folders() {
        let engine = shipped_engine();

        // Logos folder with video
        let logo_finding = engine
            .classify(r"Content\Logos\publisher.mp4", None)
            .flagged()
            .expect("mp4 inside Logos folder should be classified as intro");
        assert_eq!(logo_finding.category, Category::Intro);

        // Specific boot sequence / splash in generic folders
        let boot_file = engine
            .classify(r"Data\Movies\boot_sequence.mp4", None)
            .flagged()
            .expect("boot_sequence.mp4 should be classified as intro");
        assert_eq!(boot_file.category, Category::Intro);

        // Specific middleware logo files
        let nvidia_file = engine
            .classify(r"Engine\Binaries\nvidia_logo.mp4", None)
            .flagged()
            .expect("nvidia_logo.mp4 should be classified as intro");
        assert_eq!(nvidia_file.category, Category::Intro);
        assert_eq!(nvidia_file.confidence, 95);

        let unreal_file = engine
            .classify(r"Movies\unreal_engine.webm", None)
            .flagged()
            .expect("unreal_engine.webm should be classified as intro");
        assert_eq!(unreal_file.category, Category::Intro);
        assert_eq!(unreal_file.confidence, 95);

        // Game-specific rule with app_id (Prey 2017) - Bink 2 reaches the
        // intro rules the same as any other container now that its
        // header-derived stub is verified live; see GT-204.
        let prey_file = engine
            .classify(r"Whiplash\GameSDK\Videos\LegalScreens.bk2", Some("480490"))
            .flagged()
            .expect("LegalScreens.bk2 should be classified as intro");
        assert_eq!(prey_file.category, Category::Intro);

        // Crucial safety checks: credits and story cinematics are NOT intro videos
        let credits_file = engine.classify(r"Movies\credits.bk2", None).flagged();
        assert!(credits_file.is_none() || credits_file.unwrap().category != Category::Intro);

        let opening_story = engine
            .classify(r"Movies\opening_cinematic.mp4", None)
            .flagged();
        assert!(opening_story.is_none() || opening_story.unwrap().category != Category::Intro);
    }

    #[test]
    fn intro_category_outranks_bonus_video_rules() {
        let engine = shipped_engine();

        // An intro logo video inside Extras folder: Intro category has priority rank 1 vs Bonus rank 3
        let finding = engine
            .classify(r"Extras\nvidia_logo.mp4", None)
            .flagged()
            .expect("nvidia_logo inside Extras should be classified as intro rather than bonus");
        assert_eq!(finding.category, Category::Intro);
    }

    /// GT bug: an intro rule's broad, case-insensitive prefix regex
    /// (`unreal.*\.bik`, `.*_logos?\.mp4`, ...) used to have no depth limit
    /// at all, so it could reach a unique cutscene or gameplay asset buried
    /// deep in the tree. Depth-limiting the category (with a per-rule
    /// override for the realistic, somewhat deeper video locations - see
    /// `Category::is_depth_limited`) must keep it out of a tree this deep.
    #[test]
    fn classify_ignores_intro_looking_file_buried_deep_in_the_tree() {
        let engine = shipped_engine();

        assert_eq!(
            engine
                .classify(
                    r"Game\Content\Assets\Cinematics\Deep\Nested\nvidia_logo.bik",
                    None,
                )
                .flagged(),
            None,
            "an intro-looking file six segments deep is real content, not a startup video"
        );
    }

    /// GT bug: `crash_dump` and `diagnostic_logs` are folder-segment
    /// categories (see `Category::matches_folder_segments`), and the file
    /// branch used to be hardcoded to `Category::Intro` alone - so their
    /// `*.dmp` and `player.log` FILE rules were tested only against
    /// directory names and could never match a real file.
    #[test]
    fn classify_matches_a_real_crash_dump_and_diagnostic_log_file() {
        let engine = shipped_engine();

        let dump = engine
            .classify(r"Saved\Crashes\report.dmp", None)
            .flagged()
            .expect(".dmp file should be classified as a crash dump");
        assert_eq!(dump.category, Category::CrashDump);
        assert_eq!(dump.confidence, 95);

        let log = engine
            .classify(r"Saved\Logs\player.log", None)
            .flagged()
            .expect("player.log should be classified as a diagnostic log");
        assert_eq!(log.category, Category::DiagnosticLogs);
        assert_eq!(log.confidence, 90);
    }

    /// The other half of the same bug, the other direction: the crash-dump
    /// folder rule must keep matching folder names as before - the file-name
    /// branch is additive, not a replacement.
    #[test]
    fn classify_still_matches_a_crash_dump_folder_by_name() {
        let engine = shipped_engine();

        let finding = engine
            .classify(r"Saved\Crashes\dummy.txt", None)
            .flagged()
            .expect("a file inside a Crashes folder should be classified via the folder rule");
        assert_eq!(finding.category, Category::CrashDump);
    }

    /// The depth limit has to clear the layout Unreal actually ships:
    /// `<Game>\<Project>\Saved\Crashes` puts the folder three segments
    /// down, one past the shared shallow default, and `Content\Movies\Logos`
    /// does the same for intro. Capping these two folder rules at the default
    /// would have silenced both without anything noticing.
    #[test]
    fn classify_still_matches_the_folder_layouts_games_actually_ship() {
        let engine = shipped_engine();

        let crashes = engine
            .classify(r"MyGame\Saved\Crashes\dummy.txt", None)
            .flagged()
            .expect("Unreal's own crash folder sits three segments down");
        assert_eq!(crashes.category, Category::CrashDump);

        let logos = engine
            .classify(r"MyGame\Content\Movies\Logos\publisher.mp4", None)
            .flagged()
            .expect("a Logos folder three segments down is still an intro folder");
        assert_eq!(logos.category, Category::Intro);
    }

    /// GT-204, resolved for Bink 1 and still open for Bink 2.
    ///
    /// `classify` returns `Unmatched` for every archive-shaped path, so a
    /// monolithic container is never offered as a whole-file deletion
    /// target. `bik` was on that list and should not have been: a Bink 1
    /// file is a video, not a container of separable language streams.
    /// Claiming it only blocked the intro rules - seven of the eight match
    /// `.bik` - from ever seeing the files they exist for.
    ///
    /// `bk2` is still claimed, deliberately. The stub for it has never been
    /// through a decoder, and it cannot be: ffmpeg rejects genuine, working
    /// Bink 2 files, so it can neither confirm nor condemn ours. That one is
    /// settled by trying a stub in a real game, not by a test.
    #[test]
    fn bink_1_and_bink_2_are_both_intro_videos() {
        let engine = shipped_engine();

        // Bink 2's header-derived stub was verified live in a real game
        // (Scars Above, variant B) - see GT-204 - so it now reaches the
        // intro rules the same as Bink 1, instead of being claimed by the
        // archive inspector.
        for rel_path in [
            r"Data\Movies\boot_sequence.bik",
            r"Engine\Binaries\nvidia_logo.bik",
            r"Whiplash\GameSDK\Videos\LegalScreens.bk2",
        ] {
            assert_eq!(
                engine
                    .classify(rel_path, None)
                    .flagged()
                    .unwrap_or_else(|| panic!("{rel_path} must reach the intro rules"))
                    .category,
                Category::Intro,
            );
        }
    }

    /// GT bug: the `crashes|crashdumps|minidumps` folder rule carried
    /// confidence 95 with no depth limit, so any folder with that name
    /// anywhere in a game tree - not just the shallow `Saved\Crashes` /
    /// `CrashDumps` locations the real crash-log janitor scopes itself to
    /// (see `janitor::crashes`) - was a confidence-95 finding.
    #[test]
    fn classify_ignores_a_crash_dump_folder_buried_deep_in_the_tree() {
        let engine = shipped_engine();

        assert_eq!(
            engine
                .classify(r"Data\Mods\Community\Assets\Crashes\dummy.txt", None)
                .flagged(),
            None,
            "a folder named Crashes five segments deep is not the game's own crash log folder"
        );
    }

    #[test]
    fn from_json_parses_extension_whitelist_case_insensitively() {
        let json = r#"[
            {"category": "bonus", "pattern": "^extras$", "desc": "Bonus", "confidence": 80, "extensions": ["PDF"]}
        ]"#;
        let engine = RuleEngine::from_json(&pack(json)).unwrap();

        assert!(engine
            .classify(r"Extras\Artbook.PDF", None)
            .flagged()
            .is_some());
        assert_eq!(engine.classify(r"Extras\readme.txt", None).flagged(), None);
        assert_eq!(
            engine.classify(r"Extras\noextension", None).flagged(),
            None,
            "a file without an extension never passes a whitelist"
        );
    }

    /// The shipped rules were written in Ukrainian, and an English
    /// interface showed them untranslated in its row tooltips and CSV export.
    /// A new rule added Ukrainian-only would bring the bug straight back, so
    /// the data file is checked rather than only the machinery that reads it.
    #[test]
    fn every_builtin_rule_describes_itself_in_english() {
        let rules = parse_rule_list(BUILTIN_RULES_JSON).expect("builtin rules parse");
        assert!(!rules.is_empty(), "the builtin rule list must not be empty");

        for rule in &rules {
            let english = rule.desc.get(DEFAULT_LANG);
            assert!(
                !english.trim().is_empty(),
                "rule `{}` has no English description",
                rule.pattern
            );
            assert!(
                !english
                    .chars()
                    .any(|ch| ('\u{0400}'..='\u{04FF}').contains(&ch)),
                "rule `{}` describes itself in Cyrillic where English is expected: {english:?}",
                rule.pattern
            );
        }
    }

    /// The Ukrainian side must survive the translation, or the fix would have
    /// been "delete the Ukrainian" rather than "add the English".
    #[test]
    fn the_builtin_rules_keep_their_ukrainian_descriptions() {
        let rules = parse_rule_list(BUILTIN_RULES_JSON).expect("builtin rules parse");
        let translated = rules
            .iter()
            .filter(|rule| rule.desc.get("uk") != rule.desc.get(DEFAULT_LANG))
            .count();

        // Every rule but the one whose description is a bare product name
        // ("AMD Dual-Core Optimizer"), which is deliberately language-neutral
        // and stored as a single string.
        assert_eq!(
            translated,
            rules.len() - 1,
            "expected all but the language-neutral rule to carry a Ukrainian variant"
        );
    }

    #[test]
    fn from_json_reports_which_rule_has_the_invalid_regex() {
        let bad_json = r#"[
            {"category": "bonus", "pattern": "^(unterminated", "desc": "Broken rule", "confidence": 80}
        ]"#;

        let err = RuleEngine::from_json(&pack(bad_json))
            .expect_err("unterminated group should fail to compile");
        let message = err.to_string();
        assert!(
            message.contains("rule #0"),
            "error should name the offending rule: {message}"
        );
        assert!(
            message.contains("Broken rule"),
            "error should include the rule's desc: {message}"
        );
    }

    #[test]
    fn builtin_rules_json_parses_and_compiles() {
        let engine = RuleEngine::from_json(BUILTIN_RULES_JSON)
            .expect("embedded builtin rules must always compile");
        assert!(!engine.rules.is_empty());
    }

    /// The point of the version: a pack written by a newer build is refused
    /// with a message that says so, instead of being read as far as this
    /// build happens to understand it.
    #[test]
    fn a_pack_from_a_newer_build_is_refused() {
        let newer = format!(
            r#"{{"version":{},"rules":[]}}"#,
            RULE_PACK_VERSION.saturating_add(1)
        );

        let err = parse_rule_list(&newer).expect_err("a newer pack must not be read");

        let message = err.to_string();
        assert!(message.contains("newer than supported"), "{message}");
        assert!(
            message.contains(&RULE_PACK_VERSION.to_string()),
            "the message should name the supported version: {message}",
        );
    }

    /// The other half: a file with no version is not "version 0" to be read
    /// leniently - nothing about it says which rules format it holds.
    #[test]
    fn a_pack_without_a_version_is_refused_rather_than_assumed() {
        let bare_array = r#"[{"category":"bonus","pattern":"x","desc":"x","confidence":80}]"#;

        assert!(parse_rule_list(bare_array).is_err());
        assert!(parse_rule_list(r#"{"rules":[]}"#).is_err());
    }

    /// Every path that writes a rules.json goes through `serialize_rule_list`
    /// (the import merge, the restore-to-builtin), so an unversioned file can
    /// never be produced by the app itself.
    #[test]
    fn a_serialized_pack_carries_the_version_and_reparses() {
        let rules = parse_rule_list(BUILTIN_RULES_JSON).expect("builtin rules parse");

        let json = serialize_rule_list(&rules).expect("rules serialize");

        assert!(
            json.contains(&format!("\"version\": {RULE_PACK_VERSION}")),
            "the written pack must declare its version: {}",
            &json[..json.len().min(120)],
        );
        assert_eq!(
            parse_rule_list(&json).expect("written pack reparses").len(),
            rules.len(),
        );
    }

    /// The embedded pack is the seed for every user's file; if it fell behind
    /// the constant, every fresh install would materialize a file this build
    /// then refuses or misreads.
    #[test]
    fn the_builtin_pack_declares_the_supported_version() {
        let pack: RulePack =
            serde_json::from_str(BUILTIN_RULES_JSON).expect("builtin rules parse as a pack");

        assert_eq!(pack.version, RULE_PACK_VERSION);
    }

    #[test]
    fn strict_schema_and_numeric_bounds_are_enforced() {
        let unknown =
            r#"[{"category":"bonus","pattern":"x","desc":"x","confidence":80,"surprise":true}]"#;
        assert!(RuleEngine::from_json(&pack(unknown)).is_err());

        let confidence = r#"[{"category":"bonus","pattern":"x","desc":"x","confidence":101}]"#;
        assert!(RuleEngine::from_json(&pack(confidence)).is_err());

        let depth =
            r#"[{"category":"bonus","pattern":"x","desc":"x","confidence":80,"max_depth":33}]"#;
        assert!(RuleEngine::from_json(&pack(depth)).is_err());

        let extension = r#"[{"category":"bonus","pattern":"x","desc":"x","confidence":80,"extensions":["thisextensionistoolong"]}]"#;
        assert!(RuleEngine::from_json(&pack(extension)).is_err());
    }

    #[test]
    fn regex_rule_and_file_count_limits_are_enforced() {
        let pattern = "x".repeat(MAX_REGEX_BYTES + 1);
        let oversized_regex =
            format!(r#"[{{"category":"bonus","pattern":"{pattern}","desc":"x","confidence":80}}]"#);
        assert!(RuleEngine::from_json(&pack(&oversized_regex)).is_err());

        let rule = r#"{"category":"bonus","pattern":"x","desc":"x","confidence":80}"#;
        let too_many = format!(
            "[{}]",
            std::iter::repeat_n(rule, MAX_RULES + 1)
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(RuleEngine::from_json(&pack(&too_many)).is_err());

        let oversized_file = " ".repeat(MAX_RULE_PACK_BYTES + 1);
        assert!(RuleEngine::from_json(&oversized_file).is_err());
    }

    #[test]
    fn load_reads_and_compiles_the_real_repo_rules_json() {
        let engine = RuleEngine::load(&default_rules_path())
            .expect("repo rules.json should parse and compile");
        assert!(!engine.rules.is_empty());
    }

    /// A pack holding the builtin rules plus `extra`, which is how the scan
    /// actually runs once a personal exception exists.
    fn engine_with_keep(extra: &str) -> RuleEngine {
        let mut engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json loads");
        engine.absorb(RuleEngine::from_json(&pack(extra)).expect("keep pack compiles"));
        engine
    }

    /// The point of the whole card: a file the user has kept stops being
    /// offered, however confidently a rule would otherwise claim it.
    #[test]
    fn a_keep_rule_vetoes_the_rule_that_would_have_flagged_the_file() {
        let path = r"Support\Software\VCRedist\vc_redist.x64.exe";
        let plain = RuleEngine::load(&default_rules_path()).unwrap();
        assert!(
            matches!(plain.classify(path, Some("620")), Verdict::Flagged(_)),
            "precondition: this path is a finding without the exception",
        );

        let engine = engine_with_keep(&format!(
            r#"[{{"polarity":"keep","app_id":"620","pattern":"{}","desc":"Kept by me"}}]"#,
            regex::escape(path).replace('\\', "\\\\"),
        ));

        assert_eq!(engine.classify(path, Some("620")), Verdict::Kept);
    }

    /// A veto is not a non-match: the localization detector runs on files no
    /// rule claimed, so the two answers have to stay distinguishable or a kept
    /// file walks straight back in through the other door.
    #[test]
    fn a_veto_is_reported_apart_from_a_plain_non_match() {
        let engine = engine_with_keep(
            r#"[{"polarity":"keep","app_id":"620","pattern":"^data\\\\loc_de\\.pak$","desc":"Kept"}]"#,
        );

        assert_eq!(
            engine.classify(r"data\loc_de.pak", Some("620")),
            Verdict::Kept
        );
        assert_eq!(
            engine.classify(r"data\loc_fr.pak", Some("620")),
            Verdict::Unmatched,
        );
    }

    /// "Never touch this file *in my game*" - the same relative path in the
    /// other four hundred games must be untouched by one game's exception.
    #[test]
    fn a_keep_rule_scoped_to_one_game_does_not_affect_another() {
        let path = r"Extras\artbook.pdf";
        let engine = engine_with_keep(
            r#"[{"polarity":"keep","app_id":"620","pattern":"^Extras\\\\artbook\\.pdf$","desc":"Kept"}]"#,
        );

        assert_eq!(engine.classify(path, Some("620")), Verdict::Kept);
        assert!(
            matches!(engine.classify(path, Some("730")), Verdict::Flagged(_)),
            "another game's identical path must still be classified",
        );
        assert!(
            matches!(engine.classify(path, None), Verdict::Flagged(_)),
            "a game with no vendor id matches no scoped rule",
        );
    }

    /// The scope is a property of `Rule`, not of the keep side alone - a
    /// community recipe will bind a *deleting* rule to one game the same way,
    /// and a field that only worked on one polarity would be a trap.
    #[test]
    fn a_scope_restricts_a_deleting_rule_to_its_game_too() {
        let json = r#"[
            {"category": "bonus", "pattern": "^extras$", "desc": "Bonus", "confidence": 80,
             "app_id": "620"}
        ]"#;
        let engine = RuleEngine::from_json(&pack(json)).unwrap();

        assert!(matches!(
            engine.classify(r"Extras\track.mp3", Some("620")),
            Verdict::Flagged(_)
        ));
        assert_eq!(
            engine.classify(r"Extras\track.mp3", Some("730")),
            Verdict::Unmatched,
        );
    }

    /// `keep_file` is the only place that turns a path into a pattern, and a
    /// Windows file name is full of characters a regex reads as syntax.
    #[test]
    fn keep_file_matches_the_one_path_it_names_and_nothing_else() {
        let rule = Rule::keep_file("620", r"Data\[DLC]\readme (1).txt", "Kept".into());
        let json = serialize_rule_list(&[rule]).expect("the exception serializes");
        let engine = RuleEngine::from_json(&json).expect("the exception compiles");

        assert_eq!(
            engine.classify(r"Data\[DLC]\readme (1).txt", Some("620")),
            Verdict::Kept,
        );
        assert_eq!(
            engine.classify(r"Data\xDLCx\readme (1)atxt", Some("620")),
            Verdict::Unmatched,
            "the metacharacters must have been escaped, not interpreted",
        );
        assert_eq!(
            engine.classify(r"Data\[DLC]\readme (1).txt.bak", Some("620")),
            Verdict::Unmatched,
            "the pattern is anchored: a longer path is a different file",
        );
    }

    /// A personal pack has to come back off disk as what was written to it -
    /// this is the "survives a reload" half of surviving a re-scan.
    #[test]
    fn an_exception_pack_round_trips_through_the_file_format() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("personal_rules.json");
        let written = serialize_rule_list(&[Rule::keep_file(
            "620",
            r"Support\ru\voices.pak",
            "Kept by me".into(),
        )])
        .expect("serialize");
        std::fs::write(&path, &written).expect("write the pack");

        let reloaded = parse_rule_list(&std::fs::read_to_string(&path).unwrap())
            .expect("the written pack reparses");

        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].polarity, RulePolarity::Keep);
        assert_eq!(reloaded[0].app_id.as_deref(), Some("620"));
        assert_eq!(reloaded[0].category, None);
        assert_eq!(reloaded[0].confidence, None);
        assert_eq!(
            RuleEngine::load(&path)
                .expect("the reloaded pack compiles")
                .classify(r"Support\ru\voices.pak", Some("620")),
            Verdict::Kept,
        );
    }

    /// The versioning decision, pinned: the optional fields are absent from a
    /// pack that does not use them, so such a pack is still the v1 file every
    /// older build reads.
    ///
    /// Written against a pack of plain rules rather than `BUILTIN_RULES_JSON`,
    /// which stopped being one: the shipped pack now carries per-game
    /// reference rules and so names `app_id` and `origin` on purpose.
    #[test]
    fn a_pack_that_uses_no_optional_field_serializes_exactly_as_before() {
        let rules = parse_rule_list(&sample_json()).expect("plain rules parse");

        let json = serialize_rule_list(&rules).expect("rules serialize");

        assert!(!json.contains("polarity"), "{json:.200}");
        assert!(!json.contains("app_id"), "{json:.200}");
        assert!(!json.contains("origin"), "{json:.200}");
        assert!(json.contains(&format!("\"version\": {RULE_PACK_VERSION}")));
    }

    /// Fields that would silently do nothing are refused rather than ignored:
    /// a veto ranks against nothing and matches the whole relative path.
    #[test]
    fn a_keep_rule_may_not_carry_the_fields_a_veto_ignores() {
        for extra in [
            r#""category":"bonus""#,
            r#""confidence":80"#,
            r#""max_depth":3"#,
            r#""extensions":["pdf"]"#,
            r#""localized_content":true"#,
        ] {
            let json = pack(&format!(
                r#"[{{"polarity":"keep","pattern":"^x$","desc":"Kept",{extra}}}]"#
            ));
            let err = RuleEngine::from_json(&json)
                .expect_err("a keep rule carrying {extra} must be refused");
            assert!(err.to_string().contains("keep rule #0"), "{err}");
        }
    }

    /// The other direction: only a keep rule may omit them.
    #[test]
    fn a_deleting_rule_still_has_to_declare_its_category_and_confidence() {
        let no_category = r#"[{"pattern":"^x$","desc":"x","confidence":80}]"#;
        let err = RuleEngine::from_json(&pack(no_category)).expect_err("category is required");
        assert!(err.to_string().contains("no category"), "{err}");

        let no_confidence = r#"[{"category":"bonus","pattern":"^x$","desc":"x"}]"#;
        let err = RuleEngine::from_json(&pack(no_confidence)).expect_err("confidence is required");
        assert!(err.to_string().contains("no confidence"), "{err}");
    }

    #[test]
    fn ordinary_rules_cannot_emit_the_reserved_monolithic_archive_category() {
        let json = pack(
            r#"[{"category":"monolithic_archive","pattern":".*\\.pck$","desc":"unsafe archive rule","confidence":90}]"#,
        );
        let error = RuleEngine::from_json(&json).expect_err("reserved category must be rejected");
        assert!(error
            .to_string()
            .contains("reserved category monolithic_archive"));
    }

    #[test]
    fn ordinary_rule_category_cannot_smuggle_a_monolithic_candidate() {
        let json = pack(
            r#"[{"category":"docs_file","pattern":"^voices\\.pck$","desc":"smuggled archive","confidence":99,"provenance":"imported_untrusted"}]"#,
        );
        let engine = RuleEngine::from_json(&json).expect("syntactically valid imported rule");
        assert_eq!(engine.classify("voices.pck", None), Verdict::Unmatched);
    }

    #[test]
    fn reference_pack_reaches_an_intro_video_no_heuristic_can_see() {
        let engine = shipped_engine();

        // PCGamingWiki names this file for Alice: Madness Returns, Steam
        // appid 19680. No built-in pattern reaches it: the publisher rule
        // wants the name to *be* `ea.bik`, and "intro_ea" is neither that nor
        // anything with "logo" in it. 915 of the wiki's 1509 named files are
        // out of reach this way.
        let path = r"Alice2\Movies\intro_ea.bik";

        let finding = engine
            .classify(path, Some("19680"))
            .flagged()
            .expect("the catalogue names this file for this game");
        assert_eq!(finding.category, Category::Intro);
        assert_eq!(finding.confidence, REFERENCE_CONFIDENCE);

        // The counter-example that tells a working test from a vacuous one:
        // the same file in any other game is nobody's business.
        assert_eq!(engine.classify(path, Some("220")), Verdict::Unmatched);
        assert_eq!(engine.classify(path, None), Verdict::Unmatched);
    }

    #[test]
    fn reference_pack_takes_over_a_file_the_heuristic_was_already_guessing_at() {
        let engine = shipped_engine();

        // Prey (2017), Steam appid 480490. The studio-logo heuristic does
        // reach this one (`arkane` + `logo`, 95) - so what the catalogue
        // changes here is not whether the file is found but who answers for
        // it, and the answer stops being a guess.
        let path = r"GameSDK\Videos\ArkaneLogoAnim_Redux_1080p2997_ST-16LUFS.bk2";

        let guessed = engine
            .classify(path, Some("999999999"))
            .flagged()
            .expect("the studio-logo heuristic claims it in any game");
        assert_eq!(guessed.confidence, 95);

        let looked_up = engine
            .classify(path, Some("480490"))
            .flagged()
            .expect("the catalogue names it for this game");
        assert_eq!(looked_up.confidence, REFERENCE_CONFIDENCE);
        assert!(
            looked_up.rule_desc.contains("PCGamingWiki"),
            "{looked_up:?}"
        );
    }

    #[test]
    fn a_game_outside_the_catalogue_is_classified_exactly_as_before() {
        let engine = shipped_engine();

        // 999999999 is no game's appid, so only the heuristics can speak - and
        // they must say precisely what they say for an unidentified game.
        for path in [
            r"Movies\ue4_logo.mp4",
            r"Movies\nvidia.bik",
            r"Movies\LegalScreens.bk2",
            r"base\sound\music\track01.ogg",
            r"Movies\credits.bk2",
        ] {
            assert_eq!(
                engine.classify(path, Some("999999999")),
                engine.classify(path, None),
                "{path} must not change because a catalogue exists for other games"
            );
        }
    }

    /// The move out of `rules.json` had one hard requirement: **no file may
    /// change its verdict**. Two named games cannot show that - a wrong
    /// lowercase, a lost depth limit or a dropped entry would sail past them -
    /// so this rebuilds the *old* shape from the catalogue, rule for rule as
    /// `build_intro_reference_rules.py` used to emit it, and compares the two
    /// engines over every name the catalogue holds.
    ///
    /// Counter-examples ride along in the same loop, because agreement on
    /// matches alone would also be reported by two engines that flag
    /// everything: each name is asked again in a game the catalogue does not
    /// know, and once with no game at all.
    #[test]
    fn every_catalogued_file_gets_the_verdict_the_old_rule_shape_gave_it() {
        use crate::reference::REFERENCE_MAX_DEPTH;

        let catalogue: serde_json::Value =
            serde_json::from_str(crate::reference::BUILTIN_GAME_REFERENCE_JSON).unwrap();
        let entries = catalogue["games"].as_array().unwrap();

        // The old shape: one deleting rule per game, pattern `^(a|b|...)$`.
        let as_rules: Vec<serde_json::Value> = entries
            .iter()
            .filter(|entry| !entry["intro_files"].as_array().unwrap().is_empty())
            .map(|entry| {
                let names: Vec<String> = entry["intro_files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|name| regex::escape(name.as_str().unwrap()))
                    .collect();
                serde_json::json!({
                    "category": "intro",
                    "pattern": format!("^({})$", names.join("|")),
                    "desc": crate::reference::intro_desc(entry["title"].as_str().unwrap(), "en"),
                    "confidence": REFERENCE_CONFIDENCE,
                    "app_id": entry["app_id"],
                    "origin": "reference",
                    "max_depth": REFERENCE_MAX_DEPTH,
                })
            })
            .collect();

        let hand_written = std::fs::read_to_string(default_rules_path()).unwrap();
        let mut old_pack: serde_json::Value = serde_json::from_str(&hand_written).unwrap();
        old_pack["rules"]
            .as_array_mut()
            .unwrap()
            .extend(as_rules.clone());
        // The old pattern list is what used to blow past MAX_REGEX_BYTES and
        // force the generator to split a game across several rules; the limit
        // is irrelevant to the table, so this reconstruction is exempted from
        // it by chunking nothing and raising nothing - any game whose rebuilt
        // pattern is too long is simply skipped, and reported.
        let over_limit: Vec<&serde_json::Value> = as_rules
            .iter()
            .filter(|rule| rule["pattern"].as_str().unwrap().len() > MAX_REGEX_BYTES)
            .collect();
        old_pack["rules"]
            .as_array_mut()
            .unwrap()
            .retain(|rule| rule["pattern"].as_str().map(str::len).unwrap_or(0) <= MAX_REGEX_BYTES);

        let old = RuleEngine::from_json(&old_pack.to_string()).expect("the old shape compiles");
        let new = shipped_engine();

        let mut compared = 0usize;
        for entry in entries {
            let app_id = entry["app_id"].as_str().unwrap();
            if over_limit
                .iter()
                .any(|rule| rule["app_id"].as_str() == Some(app_id))
            {
                continue;
            }
            for name in entry["intro_files"].as_array().unwrap() {
                let name = name.as_str().unwrap();
                // The harvest lowercases every name it stores, and the files
                // on disk are `ArkaneLogoAnim_Redux...bk2`. Asking only in
                // the catalogue's own spelling would compare two engines on
                // the one case that cannot tell a folding matcher from a
                // byte-for-byte one - which is exactly what this test did
                // until a deliberately case-sensitive matcher passed it.
                for name in [name.to_string(), name.to_uppercase(), title_case(name)] {
                    for prefix in ["", r"Movies\", r"Game\Content\Movies\Startup\"] {
                        let path = format!("{prefix}{name}");
                        assert_eq!(
                            old.classify(&path, Some(app_id)),
                            new.classify(&path, Some(app_id)),
                            "{path} in {app_id} changed verdict",
                        );
                        // The counter-examples: neither engine may claim this
                        // file for a game that is not this one.
                        assert_eq!(
                            old.classify(&path, Some("999999999")),
                            new.classify(&path, Some("999999999")),
                            "{path} in an uncatalogued game changed verdict",
                        );
                        assert_eq!(
                            old.classify(&path, None),
                            new.classify(&path, None),
                            "{path} with no game changed verdict",
                        );
                        compared += 1;
                    }
                }
            }
        }

        assert!(
            compared > 9_000,
            "only {compared} paths compared - the catalogue did not load",
        );
        assert!(
            over_limit.len() < 20,
            "{} games could not be expressed as one old-shape rule; too many to \
             call this comparison complete",
            over_limit.len(),
        );
    }

    #[test]
    fn the_shipped_pack_stays_within_the_engine_limits() {
        // Both limits are checked at load, and a pack that trips one is
        // refused wholesale - so the failure to catch is this file growing
        // past them, not a user's import being blamed for it.
        let json = std::fs::read_to_string(default_rules_path()).unwrap();
        let rules = parse_rule_list(&json).unwrap();
        assert!(
            json.len() <= MAX_RULE_PACK_BYTES,
            "rules.json is {} bytes, over the {MAX_RULE_PACK_BYTES} limit",
            json.len()
        );
        assert!(rules.len() <= MAX_RULES);

        // And the headroom the catalogue's move out of this file bought, which
        // is what the move was *for*: the budget is there for community
        // recipes, not for a generator to refill. Carrying the catalogue as
        // rules put this file at 407 KB / 986 rules - a fifth of the space and
        // half the count gone, on data nobody edits by hand. A regenerated
        // pack that writes entries back in here trips this long before it
        // trips the limits above.
        assert!(
            json.len() <= MAX_RULE_PACK_BYTES / 10 && rules.len() <= MAX_RULES / 10,
            "rules.json is back to {} bytes / {} rules - the per-game catalogue \
             belongs in game_reference.json, not here",
            json.len(),
            rules.len(),
        );
        assert!(
            rules.iter().all(|rule| rule.origin == RuleOrigin::Builtin),
            "a reference-origin rule is back in rules.json; the catalogue is a table now",
        );
    }

    fn default_rules_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("rules.json")
    }

    /// The engine a scan actually runs on: the shipped rules *plus* the
    /// shipped catalogue. Both halves are needed to reproduce a real verdict -
    /// building from `rules.json` alone measures the heuristics on their own,
    /// which is a question no user ever asks.
    fn shipped_engine() -> RuleEngine {
        RuleEngine::load(&default_rules_path())
            .expect("repo rules.json should load")
            .with_reference(GameReference::builtin().expect("the built-in catalogue parses"))
    }

    /// `intro_ea.bik` -> `Intro_ea.bik`: a third spelling, so the comparison
    /// covers a mixed case as well as the two uniform ones.
    fn title_case(name: &str) -> String {
        let mut chars = name.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().chain(chars).collect(),
            None => String::new(),
        }
    }

    /// A heuristic that is *more* confident than the catalogue entry beside
    /// it, so that a passing test can only mean origin decided the winner.
    fn origin_pack() -> String {
        pack(
            r#"[
            {"category": "intro", "pattern": "^logo\\.bik$", "desc": "Guessed: startup logo", "confidence": 99},
            {"category": "intro", "pattern": "^(logo|weird_name)\\.bik$", "desc": "PCGamingWiki entry", "confidence": 70, "app_id": "480490", "origin": "reference"}
        ]"#,
        )
    }

    #[test]
    fn reference_rule_outranks_a_more_confident_heuristic_in_its_own_game() {
        let engine = RuleEngine::from_json(&origin_pack()).unwrap();

        let finding = engine
            .classify(r"Movies\logo.bik", Some("480490"))
            .flagged()
            .expect("both rules match, one of them has to win");

        assert_eq!(finding.rule_desc, "PCGamingWiki entry");
        assert_eq!(finding.confidence, 70);
    }

    #[test]
    fn reference_rule_leaves_every_other_game_to_the_heuristic() {
        let engine = RuleEngine::from_json(&origin_pack()).unwrap();

        // The same file in a game the catalogue does not cover.
        let finding = engine
            .classify(r"Movies\logo.bik", Some("220"))
            .flagged()
            .expect("the heuristic still applies where the entry does not");
        assert_eq!(finding.rule_desc, "Guessed: startup logo");

        // And the name only the entry knows is not claimed there at all.
        assert_eq!(
            engine.classify(r"Movies\weird_name.bik", Some("220")),
            Verdict::Unmatched
        );
        assert_eq!(
            engine.classify(r"Movies\weird_name.bik", None),
            Verdict::Unmatched
        );
    }

    #[test]
    fn reference_rule_without_an_app_id_is_refused() {
        let json = pack(
            r#"[
            {"category": "intro", "pattern": "^logo\\.bik$", "desc": "Unbound entry", "confidence": 96, "origin": "reference"}
        ]"#,
        );

        let err = RuleEngine::from_json(&json).expect_err("a lookup must name what it looked up");
        assert!(err.to_string().contains("app_id"), "{err}");
    }
}
