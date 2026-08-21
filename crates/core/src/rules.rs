//! Regex rule engine for non-essential file categories (redist, docs, bonus, ...).
//! Rules are loaded from an external `rules.json` next to the executable.

use std::collections::HashSet;
use std::path::Path;

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::localized::{LocalizedText, DEFAULT_LANG};

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

/// The repo's rules.json embedded at build time - the seed for the external
/// file the app materializes next to the executable on first use, so users
/// always have the full effective rule set on disk to audit and edit. The
/// scanner never reads this constant directly once the file exists.
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
/// it is written in the same file format, validated by the same parser and
/// carried by the same import/export machinery.
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
/// the "Export rules"/"Import rules" flow (see
/// `crate::packs`), which rewrites the merged list back to disk.
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

/// A classification produced by the engine for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub category: Category,
    pub rule_desc: String,
    pub confidence: u8,
    pub provenance: RuleProvenance,
}

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
#[derive(Debug)]
struct CompiledRule {
    category: Category,
    regex: Regex,
    desc: String,
    confidence: u8,
    provenance: RuleProvenance,
    /// The effective depth limit for this rule: the rule's own `max_depth`
    /// if given, otherwise the category default (see [`Rule::max_depth`]).
    max_depth: usize,
    /// Lowercased extension whitelist, if the rule declares one
    /// (see [`Rule::extensions`]).
    extensions: Option<HashSet<String>>,
    /// The one game this rule applies to, if it is scoped (see
    /// [`Rule::app_id`]).
    app_id: Option<String>,
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

/// Whether a rule scoped to `scope` applies while classifying a game whose
/// vendor id is `app_id`. An unscoped rule (the shape of every built-in)
/// applies to everything, which is what keeps this free for them.
fn scope_applies(scope: &Option<String>, app_id: Option<&str>) -> bool {
    match scope {
        None => true,
        Some(scope) => app_id.is_some_and(|id| id == scope),
    }
}

#[derive(Debug)]
pub struct RuleEngine {
    rules: Vec<CompiledRule>,
    /// Split out from `rules` rather than filtered out of it per file: on a
    /// default install this is empty, so honouring the veto costs one
    /// `is_empty` per file over a 4.9-million-file scan instead of a polarity
    /// branch inside the loop that runs for every rule of every file.
    keeps: Vec<CompiledKeep>,
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
        let mut keeps = Vec::new();
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
                {
                    return Err(CoreError::Other(format!(
                        "rules.json: keep rule #{index} (pattern `{}`) sets category, confidence, \
                         max_depth or extensions; a veto ranks against nothing and matches the \
                         whole relative path, so none of them would do anything",
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

            let default_depth = if category.is_depth_limited() {
                MAX_SHALLOW_DEPTH
            } else {
                usize::MAX
            };
            rules.push(CompiledRule {
                category,
                regex,
                desc,
                confidence,
                provenance: rule.provenance,
                max_depth: rule.max_depth.unwrap_or(default_depth),
                extensions: rule.extensions.map(|list| {
                    list.into_iter()
                        .map(|ext| ext.to_ascii_lowercase())
                        .collect()
                }),
                app_id: rule.app_id,
            });
        }

        Ok(Self { rules, keeps })
    }

    /// Folds another engine's rules into this one, as the scan does with the
    /// personal exception pack on top of `rules.json`.
    ///
    /// Order matters for nothing but ties: precedence is decided by category
    /// rank and confidence, and the veto is checked before either.
    pub fn absorb(&mut self, other: RuleEngine) {
        self.rules.extend(other.rules);
        self.keeps.extend(other.keeps);
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
    ///    highest precedence wins, confidence breaking ties within one rank.
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

        let mut best: Option<Finding> = None;

        for rule in &self.rules {
            // Cheapest test first, and free for every unscoped rule (which is
            // all of them until a recipe pack arrives): an `Option` tag check
            // beside the extension check already here, in front of a regex
            // match that costs orders of magnitude more.
            if !scope_applies(&rule.app_id, app_id) {
                continue;
            }
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

            let is_better = match &best {
                Some(current) => {
                    let current_rank = current.category.priority_rank();
                    let rank = rule.category.priority_rank();
                    rank < current_rank
                        || (rank == current_rank && rule.confidence > current.confidence)
                }
                None => true,
            };
            if is_better {
                best = Some(Finding {
                    category: rule.category,
                    rule_desc: rule.desc.clone(),
                    confidence: rule.confidence,
                    provenance: rule.provenance,
                });
            }
        }

        match best {
            Some(finding) => Verdict::Flagged(finding),
            None => Verdict::Unmatched,
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
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

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
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

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
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

        let finding = engine
            .classify("base\\sound\\music\\track01.ogg", None)
            .flagged();

        assert_eq!(finding, None);
    }

    #[test]
    fn classify_ignores_redist_pattern_match_beyond_max_depth() {
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

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
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

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
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

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
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

        // Support/help folders are reference material, so they belong to
        // the docs category ("Documentation and reference material" in the
        // UI), not to bonus - and the folder claims its content wholesale,
        // whatever the file type or per-language subfolder split inside.
        let finding = engine
            .classify(r"Support\ru\voices.pak", None)
            .flagged()
            .expect("support folder content should be classified");

        assert_eq!(finding.category, Category::DocsFolder);
    }

    #[test]
    fn classify_ignores_bonus_folder_deep_inside_program_trees() {
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

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
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

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

    #[test]
    fn classify_identifies_intro_and_logo_files_and_folders() {
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

        // Logos folder with video
        let logo_finding = engine
            .classify(r"Content\Logos\publisher.mp4", None)
            .flagged()
            .expect("mp4 inside Logos folder should be classified as intro");
        assert_eq!(logo_finding.category, Category::Intro);

        // Specific boot sequence / splash in generic folders
        let boot_file = engine
            .classify(r"Data\Movies\boot_sequence.bik", None)
            .flagged()
            .expect("boot_sequence.bik should be classified as intro");
        assert_eq!(boot_file.category, Category::Intro);

        // Specific middleware logo files
        let nvidia_file = engine
            .classify(r"Engine\Binaries\nvidia_logo.bik", None)
            .flagged()
            .expect("nvidia_logo.bik should be classified as intro");
        assert_eq!(nvidia_file.category, Category::Intro);
        assert_eq!(nvidia_file.confidence, 95);

        let unreal_file = engine
            .classify(r"Movies\unreal_engine.webm", None)
            .flagged()
            .expect("unreal_engine.webm should be classified as intro");
        assert_eq!(unreal_file.category, Category::Intro);
        assert_eq!(unreal_file.confidence, 95);

        // Game-specific rule with app_id (Prey 2017)
        let prey_file = engine
            .classify(r"Whiplash\GameSDK\Videos\LegalScreens.bk2", Some("480490"))
            .flagged()
            .expect("LegalScreens.bk2 for Prey should match game-specific rule");
        assert_eq!(prey_file.category, Category::Intro);
        assert_eq!(prey_file.confidence, 95);

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
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

        // An intro logo video inside Extras folder: Intro category has priority rank 1 vs Bonus rank 3
        let finding = engine
            .classify(r"Extras\nvidia_logo.bik", None)
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
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

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
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

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
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

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
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

        let crashes = engine
            .classify(r"MyGame\Saved\Crashes\dummy.txt", None)
            .flagged()
            .expect("Unreal's own crash folder sits three segments down");
        assert_eq!(crashes.category, Category::CrashDump);

        let logos = engine
            .classify(r"MyGame\Content\Movies\Logos\publisher.bik", None)
            .flagged()
            .expect("a Logos folder three segments down is still an intro folder");
        assert_eq!(logos.category, Category::Intro);
    }

    /// GT bug: the `crashes|crashdumps|minidumps` folder rule carried
    /// confidence 95 with no depth limit, so any folder with that name
    /// anywhere in a game tree - not just the shallow `Saved\Crashes` /
    /// `CrashDumps` locations the real crash-log janitor scopes itself to
    /// (see `janitor::crashes`) - was a confidence-95 finding.
    #[test]
    fn classify_ignores_a_crash_dump_folder_buried_deep_in_the_tree() {
        let engine = RuleEngine::load(&default_rules_path()).expect("repo rules.json should load");

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

    /// The versioning decision, pinned: the two new fields are absent from a
    /// pack that does not use them, so an unchanged `rules.json` is still the
    /// v1 file every older build reads.
    #[test]
    fn a_pack_that_uses_no_exception_field_serializes_exactly_as_before() {
        let rules = parse_rule_list(BUILTIN_RULES_JSON).expect("builtin rules parse");

        let json = serialize_rule_list(&rules).expect("rules serialize");

        assert!(!json.contains("polarity"), "{json:.200}");
        assert!(!json.contains("app_id"), "{json:.200}");
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

    fn default_rules_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("rules.json")
    }
}
