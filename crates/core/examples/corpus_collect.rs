//! Read-only corpus collection tool for the localization-detection engine.
//! Discovers real Steam libraries,
//! scans each game's files in memory (no database), and extracts candidate
//! paths that contain a potential language token. Candidates are balanced
//! across libraries, given a draft `expected` label via conservative rules,
//! and written to `tests/corpus/corpus.tsv`.
//!
//! Only reads the registry, `libraryfolders.vdf`, `appmanifest_*.acf`, and
//! walks each library's game directories - performs no writes, deletes, or
//! renames anywhere on a Steam library drive. The only file this tool
//! writes is `tests/corpus/corpus.tsv` inside this repository.
//!
//! Run with: `cargo run -p gametrimmer-core --example corpus_collect`
//!
//! IMPORTANT: this tool implements its own small token/dictionary matcher
//! for corpus-collection purposes only. It intentionally does NOT use
//! `gametrimmer_core::langdetect` (that module is `todo!()` - implemented
//! by a parallel task) and must not be changed to do so.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

use gametrimmer_core::providers::steam::SteamProvider;
use gametrimmer_core::providers::{GameInstall, LibraryProvider};
use gametrimmer_core::scanner::{scan_games_parallel, FileEntry};

// ---------------------------------------------------------------------
// Dictionaries. Deliberately a simplified draft of what `langdetect::dict`
// does - this tool must not depend on the engine it collects test data for.
// ---------------------------------------------------------------------

const FULL_NAMES: &[&str] = &[
    "english",
    "french",
    "german",
    "spanish",
    "italian",
    "portuguese",
    "russian",
    "polish",
    "czech",
    "japanese",
    "korean",
    "chinese",
    "turkish",
    "arabic",
    "ukrainian",
    "dutch",
    "swedish",
    "norwegian",
    "danish",
    "finnish",
    "hungarian",
    "greek",
    "romanian",
    "brazilian",
    "bulgarian",
    "thai",
    "vietnamese",
];

const STEAM_NAMES: &[&str] = &["koreana", "schinese", "tchinese", "latam"];

const AUTONYMS: &[&str] = &[
    "deutsch",
    "francais",
    "espanol",
    "italiano",
    "polski",
    "portugues",
];

const ISO3: &[&str] = &[
    "spa", "deu", "fra", "ita", "pol", "rus", "jpn", "kor", "chi", "zho", "por", "ukr", "eng",
];

const ISO2: &[&str] = &[
    "es", "de", "fr", "it", "pl", "ru", "ja", "ko", "zh", "pt", "cs", "tr", "nl", "sv", "no", "da",
    "fi", "el", "ro", "bg", "hu", "th", "vi",
];

const AUDIO_MARKERS: &[&str] = &[
    "sound",
    "soundbanks",
    "voice",
    "vo",
    "audio",
    "speech",
    "dub",
];

const TEXT_MARKERS: &[&str] = &[
    "text",
    "strings",
    "subtitles",
    "caption",
    "loc",
    "localization",
    "lang",
    "language",
    "l10n",
];

const NEG_MARKERS: &[&str] = &[
    "art",
    "models",
    "textures",
    "units",
    "buildings",
    "history",
    "decisions",
    "events",
    "missions",
    "maps",
    "music",
    "scripts",
];

const VIDEO_MARKERS: &[&str] = &[
    "movies",
    "movie",
    "videos",
    "video",
    "cinematics",
    "cutscenes",
    "fmv",
];

const FONT_MARKERS: &[&str] = &["font", "fonts"];

const VIDEO_EXTENSIONS: &[&str] = &["bik", "bk2", "usm", "wmv", "webm", "ogv"];

const FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "fnt"];

/// Canonical language family for every dictionary entry above, plus bare
/// "en"/"uk" (needed for locale-tag prefixes and the keep-list rule; note
/// "en"/"uk" are deliberately excluded from ISO2 matching per the task
/// spec, but still need a canonical mapping for the keep-list check).
fn canonical_lang(word: &str) -> Option<&'static str> {
    let pairs: &[(&str, &str)] = &[
        ("english", "en"),
        ("eng", "en"),
        ("en", "en"),
        ("ukrainian", "uk"),
        ("ukr", "uk"),
        ("uk", "uk"),
        ("french", "fr"),
        ("francais", "fr"),
        ("fra", "fr"),
        ("fr", "fr"),
        ("german", "de"),
        ("deutsch", "de"),
        ("deu", "de"),
        ("de", "de"),
        ("spanish", "es"),
        ("espanol", "es"),
        ("spa", "es"),
        ("es", "es"),
        ("latam", "es"),
        ("italian", "it"),
        ("italiano", "it"),
        ("ita", "it"),
        ("it", "it"),
        ("portuguese", "pt"),
        ("portugues", "pt"),
        ("por", "pt"),
        ("pt", "pt"),
        ("brazilian", "pt"),
        ("russian", "ru"),
        ("rus", "ru"),
        ("ru", "ru"),
        ("polish", "pl"),
        ("polski", "pl"),
        ("pol", "pl"),
        ("pl", "pl"),
        ("czech", "cs"),
        ("cs", "cs"),
        ("japanese", "ja"),
        ("jpn", "ja"),
        ("ja", "ja"),
        ("korean", "ko"),
        ("koreana", "ko"),
        ("kor", "ko"),
        ("ko", "ko"),
        ("chinese", "zh"),
        ("schinese", "zh"),
        ("tchinese", "zh"),
        ("chi", "zh"),
        ("zho", "zh"),
        ("zh", "zh"),
        ("turkish", "tr"),
        ("tur", "tr"),
        ("tr", "tr"),
        ("arabic", "ar"),
        ("ar", "ar"),
        ("dutch", "nl"),
        ("nl", "nl"),
        ("swedish", "sv"),
        ("sv", "sv"),
        ("norwegian", "no"),
        ("no", "no"),
        ("danish", "da"),
        ("da", "da"),
        ("finnish", "fi"),
        ("fi", "fi"),
        ("hungarian", "hu"),
        ("hu", "hu"),
        ("greek", "el"),
        ("el", "el"),
        ("romanian", "ro"),
        ("ro", "ro"),
        ("bulgarian", "bg"),
        ("bg", "bg"),
        ("thai", "th"),
        ("th", "th"),
        ("vietnamese", "vi"),
        ("vi", "vi"),
    ];
    pairs
        .iter()
        .find(|(k, _)| *k == word)
        .map(|(_, canon)| *canon)
}

// ---------------------------------------------------------------------
// Tokenization
// ---------------------------------------------------------------------

const DELIMITERS: &[char] = &['_', '-', '.', '(', ')', '[', ']', ' '];

/// Splits one path segment (a folder or file name) on delimiters, then
/// further splits each resulting piece on CamelCase boundaries
/// (`SpanishNation` -> `Spanish`, `Nation`).
fn tokenize_segment(segment: &str) -> Vec<String> {
    let mut raw_tokens = Vec::new();
    let mut current = String::new();
    for c in segment.chars() {
        if DELIMITERS.contains(&c) {
            if !current.is_empty() {
                raw_tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        raw_tokens.push(current);
    }

    raw_tokens
        .iter()
        .flat_map(|t| split_camel_case(t))
        .collect()
}

fn split_camel_case(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut result = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && c.is_uppercase() && chars[i - 1].is_lowercase() {
            result.push(std::mem::take(&mut current));
        }
        current.push(c);
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

/// All lowercase tokens across every segment of a `\`/`/`-separated
/// relative path (folders + file name, extension included as its own
/// token since `.` is a delimiter).
fn all_path_tokens(rel_path: &str) -> Vec<String> {
    rel_path
        .split(['\\', '/'])
        .flat_map(tokenize_segment)
        .map(|t| t.to_lowercase())
        .collect()
}

// ---------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum MatchType {
    FullName,
    SteamName,
    Autonym,
    LocaleTag,
    Iso3,
    Iso2,
}

impl MatchType {
    fn label(self) -> &'static str {
        match self {
            MatchType::FullName => "full_name",
            MatchType::SteamName => "steam_name",
            MatchType::Autonym => "autonym",
            MatchType::LocaleTag => "locale_tag",
            MatchType::Iso3 => "iso3",
            MatchType::Iso2 => "iso2",
        }
    }
}

struct Candidate {
    rel_path: String,
    token: String,
    match_type: MatchType,
    canonical: Option<&'static str>,
    path_tokens: HashSet<String>,
    /// Whether the matched language token sits in the file name itself
    /// (last path segment), rather than only in a parent folder name.
    /// Required by the video/font rules, which only trigger on a language
    /// token + extension/marker combo in the file name.
    token_in_filename: bool,
}

/// Lowercase extension (without the dot) of a path's last segment, if any.
fn extension_of(rel_path: &str) -> Option<String> {
    let last = rel_path.rsplit(['\\', '/']).next().unwrap_or(rel_path);
    last.rsplit_once('.')
        .map(|(_, ext)| ext.to_lowercase())
        .filter(|ext| !ext.is_empty())
}

/// Locale-tag pattern: 2-3 letters + `-`/`_` + 2 letters, as a standalone
/// word (e.g. `en-us`, `pt_BR`, `zh-CN`). Only accepted as a match if the
/// prefix resolves to a known language (filters out incidental matches
/// like `non-eu`, `read-me`).
fn find_locale_tag(segment: &str, locale_re: &Regex) -> Option<(String, &'static str)> {
    for caps in locale_re.captures_iter(segment) {
        let whole = caps.get(0)?.as_str().to_string();
        let prefix = caps.get(1)?.as_str().to_lowercase();
        if let Some(canon) = canonical_lang(&prefix) {
            return Some((whole, canon));
        }
    }
    None
}

/// Finds the single highest-priority language-token match for a file's
/// relative path, per the priority order in the task spec:
/// full_name > steam_name > autonym > locale_tag > iso3 > iso2.
fn detect_match(rel_path: &str, locale_re: &Regex) -> Option<Candidate> {
    let tokens = all_path_tokens(rel_path);
    let token_set: HashSet<String> = tokens.iter().cloned().collect();

    let last_seg = rel_path.rsplit(['\\', '/']).next().unwrap_or(rel_path);
    let filename_tokens: HashSet<String> = tokenize_segment(last_seg)
        .into_iter()
        .map(|t| t.to_lowercase())
        .collect();

    if let Some(t) = tokens.iter().find(|t| FULL_NAMES.contains(&t.as_str())) {
        return Some(Candidate {
            rel_path: rel_path.to_string(),
            token: t.clone(),
            match_type: MatchType::FullName,
            canonical: canonical_lang(t),
            token_in_filename: filename_tokens.contains(t),
            path_tokens: token_set,
        });
    }
    if let Some(t) = tokens.iter().find(|t| STEAM_NAMES.contains(&t.as_str())) {
        return Some(Candidate {
            rel_path: rel_path.to_string(),
            token: t.clone(),
            match_type: MatchType::SteamName,
            canonical: canonical_lang(t),
            token_in_filename: filename_tokens.contains(t),
            path_tokens: token_set,
        });
    }
    if let Some(t) = tokens.iter().find(|t| AUTONYMS.contains(&t.as_str())) {
        return Some(Candidate {
            rel_path: rel_path.to_string(),
            token: t.clone(),
            match_type: MatchType::Autonym,
            canonical: canonical_lang(t),
            token_in_filename: filename_tokens.contains(t),
            path_tokens: token_set,
        });
    }
    // Locale tag needs the raw (un-tokenized) segments, since `-`/`_` is
    // part of the pattern itself.
    for segment in rel_path.split(['\\', '/']) {
        if let Some((whole, canon)) = find_locale_tag(segment, locale_re) {
            return Some(Candidate {
                rel_path: rel_path.to_string(),
                token: whole,
                match_type: MatchType::LocaleTag,
                canonical: Some(canon),
                token_in_filename: segment == last_seg,
                path_tokens: token_set,
            });
        }
    }
    if let Some(t) = tokens.iter().find(|t| ISO3.contains(&t.as_str())) {
        return Some(Candidate {
            rel_path: rel_path.to_string(),
            token: t.clone(),
            match_type: MatchType::Iso3,
            canonical: canonical_lang(t),
            token_in_filename: filename_tokens.contains(t),
            path_tokens: token_set,
        });
    }
    if let Some(t) = tokens.iter().find(|t| ISO2.contains(&t.as_str())) {
        return Some(Candidate {
            rel_path: rel_path.to_string(),
            token: t.clone(),
            match_type: MatchType::Iso2,
            canonical: canonical_lang(t),
            token_in_filename: filename_tokens.contains(t),
            path_tokens: token_set,
        });
    }

    None
}

// ---------------------------------------------------------------------
// Labeling (draft, conservative - see tests/corpus/README.md)
// ---------------------------------------------------------------------

fn parent_dir(rel_path: &str) -> String {
    match rel_path.rfind(['\\', '/']) {
        Some(pos) => rel_path[..pos].to_lowercase(),
        None => String::new(),
    }
}

/// For every candidate in one game, flags whether it sits in a directory
/// where >=3 sibling candidates resolve to different canonical languages
/// (the "language family" heuristic, §5.4 of the plan).
fn compute_family_flags(candidates: &[Candidate]) -> Vec<bool> {
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, c) in candidates.iter().enumerate() {
        groups.entry(parent_dir(&c.rel_path)).or_default().push(i);
    }

    let mut flags = vec![false; candidates.len()];
    for idxs in groups.values() {
        let langs: HashSet<&str> = idxs
            .iter()
            .filter_map(|&i| candidates[i].canonical)
            .collect();
        if langs.len() >= 3 {
            for &i in idxs {
                flags[i] = true;
            }
        }
    }
    flags
}

/// Draft label rules, applied in priority order. `en`/`uk` keep-list and
/// negative markers always win; then the four positive asset categories
/// (audio/video/text/font); then the iso2/iso3-without-context fallback;
/// anything else left over is "unknown" pending human review.
///
/// `movies`/`video` are intentionally NOT negative markers (unlike
/// `music`/`textures`, which stay negative) - see tests/corpus/README.md.
fn label_for(cand: &Candidate, family: bool) -> &'static str {
    if matches!(cand.canonical, Some("en") | Some("uk")) {
        return "none";
    }
    if cand
        .path_tokens
        .iter()
        .any(|t| NEG_MARKERS.contains(&t.as_str()))
    {
        return "none";
    }
    if cand
        .path_tokens
        .iter()
        .any(|t| AUDIO_MARKERS.contains(&t.as_str()))
    {
        return "audio";
    }
    let ext = extension_of(&cand.rel_path);
    let has_video_ext = ext
        .as_deref()
        .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e));
    if cand
        .path_tokens
        .iter()
        .any(|t| VIDEO_MARKERS.contains(&t.as_str()))
        || (has_video_ext && cand.token_in_filename)
    {
        return "video";
    }
    if cand
        .path_tokens
        .iter()
        .any(|t| TEXT_MARKERS.contains(&t.as_str()))
    {
        return "text";
    }
    let has_font_ext = ext.as_deref().is_some_and(|e| FONT_EXTENSIONS.contains(&e));
    if cand
        .path_tokens
        .iter()
        .any(|t| FONT_MARKERS.contains(&t.as_str()))
        || (has_font_ext && cand.token_in_filename)
    {
        return "font";
    }
    match cand.match_type {
        MatchType::Iso2 | MatchType::Iso3 => {
            if family {
                "unknown"
            } else {
                "none"
            }
        }
        _ => "unknown",
    }
}

// ---------------------------------------------------------------------
// Per-game collection
// ---------------------------------------------------------------------

struct GameCandidates {
    library_path: PathBuf,
    game_name: String,
    game_key: String,
    candidates: Vec<Candidate>,
}

fn make_game_key(game: &GameInstall) -> String {
    match &game.app_id {
        Some(id) => format!("steam:{id}"),
        None => {
            let sanitized: String = game
                .name
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '_' })
                .collect();
            format!("steam:noid:{sanitized}")
        }
    }
}

fn drive_of(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

/// Global index encoding (library_idx, game_idx) into a single i64 for
/// `scan_games_parallel`, which only takes a flat `(id, PathBuf)` list.
fn encode_idx(lib_idx: usize, game_idx: usize) -> i64 {
    ((lib_idx as i64) << 32) | (game_idx as i64)
}

fn decode_idx(idx: i64) -> (usize, usize) {
    ((idx >> 32) as usize, (idx & 0xFFFF_FFFF) as usize)
}

const TOTAL_ROW_CAP: usize = 15_000;
const PER_GAME_CAP: usize = 150;
const F_TOP_N: usize = 40;
const F_RICH_THRESHOLD: usize = 50;

/// Evenly samples up to `want` items from `items` (stride selection),
/// preserving order. Head-truncation would keep only the first game(s)
/// scanned; stride sampling keeps every game and folder family
/// represented in the capped corpus.
fn stride_sample<T>(items: Vec<T>, want: usize) -> Vec<T> {
    if items.len() <= want || want == 0 {
        return items;
    }
    let len = items.len();
    let step = len as f64 / want as f64;
    let keep: HashSet<usize> = (0..want).map(|i| ((i as f64) * step) as usize).collect();
    items
        .into_iter()
        .enumerate()
        .filter_map(|(i, item)| keep.contains(&i).then_some(item))
        .collect()
}

/// One row of the generated corpus, plus extra fields (kept only for the
/// human-review sample printed at the end - not written to corpus.tsv,
/// which is strictly `game_key\trel_path\texpected`).
struct Row {
    game_key: String,
    rel_path: String,
    expected: &'static str,
    token: String,
    match_type_label: &'static str,
}

/// Picks up to `want` rows with the given `expected` label, spread evenly
/// across all matches (rather than just the first N) for a more
/// representative human-review sample.
fn pick_examples<'a>(rows: &'a [Row], label: &str, want: usize) -> Vec<&'a Row> {
    let matches: Vec<&Row> = rows.iter().filter(|r| r.expected == label).collect();
    if matches.len() <= want || want == 0 {
        return matches;
    }
    let step = matches.len() as f64 / want as f64;
    (0..want)
        .map(|i| matches[((i as f64) * step) as usize])
        .collect()
}

fn main() {
    let locale_re = Regex::new(r"(?i)\b([a-zA-Z]{2,3})[-_]([a-zA-Z]{2})\b").expect("valid regex");

    let report = SteamProvider.discover();
    if report.status == gametrimmer_core::providers::DiscoveryStatus::Failed {
        eprintln!("Steam discovery failed: {:#?}", report.diagnostics);
        std::process::exit(1);
    }
    let libraries = report.data;

    if libraries.is_empty() {
        println!("No Steam libraries found.");
        return;
    }

    println!("Discovered {} Steam librar(y/ies):", libraries.len());
    for lib in &libraries {
        println!("  {} — {} game(s)", lib.path.display(), lib.games.len());
    }

    let mut dirs: Vec<(i64, PathBuf)> = Vec::new();
    for (lib_idx, lib) in libraries.iter().enumerate() {
        for (game_idx, game) in lib.games.iter().enumerate() {
            dirs.push((encode_idx(lib_idx, game_idx), game.install_dir.clone()));
        }
    }

    println!(
        "\nScanning {} game folder(s), read-only, in memory (no DB)...",
        dirs.len()
    );
    let scan_results = scan_games_parallel(&dirs);

    let mut files_by_idx: HashMap<i64, Vec<FileEntry>> = HashMap::new();
    let mut scan_errors = 0usize;
    for (idx, res) in scan_results {
        match res {
            Ok(entries) => {
                files_by_idx.insert(idx, entries);
            }
            Err(err) => {
                scan_errors += 1;
                let (lib_idx, game_idx) = decode_idx(idx);
                eprintln!(
                    "  scan error: {} / {}: {err}",
                    libraries[lib_idx].path.display(),
                    libraries[lib_idx].games[game_idx].name
                );
            }
        }
    }

    let mut all_games: Vec<GameCandidates> = Vec::new();
    for (lib_idx, lib) in libraries.iter().enumerate() {
        for (game_idx, game) in lib.games.iter().enumerate() {
            let idx = encode_idx(lib_idx, game_idx);
            let Some(entries) = files_by_idx.get(&idx) else {
                continue;
            };
            let candidates: Vec<Candidate> = entries
                .iter()
                .filter_map(|e| detect_match(&e.rel_path, &locale_re))
                .collect();

            all_games.push(GameCandidates {
                library_path: lib.path.clone(),
                game_name: game.name.clone(),
                game_key: make_game_key(game),
                candidates,
            });
        }
    }

    // ---- Overall stats (pre-selection, full scan) ----
    let total_candidates: usize = all_games.iter().map(|g| g.candidates.len()).sum();
    let mut by_type: HashMap<&'static str, usize> = HashMap::new();
    for g in &all_games {
        for c in &g.candidates {
            *by_type.entry(c.match_type.label()).or_insert(0) += 1;
        }
    }

    println!("\n=== Повна статистика збору (до балансування) ===");
    println!(
        "Ігор просканованих: {}, помилок сканування: {scan_errors}",
        all_games.len()
    );
    println!("Кандидатів усього: {total_candidates}");
    println!("За типом збігу:");
    let mut type_pairs: Vec<(&str, usize)> = by_type.into_iter().collect();
    type_pairs.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (t, n) in &type_pairs {
        println!("  {t}: {n}");
    }

    let mut by_count: Vec<&GameCandidates> = all_games.iter().collect();
    by_count.sort_by_key(|g| std::cmp::Reverse(g.candidates.len()));
    println!("\nТоп-15 ігор за кількістю кандидатів:");
    for g in by_count.iter().take(15) {
        println!(
            "  {:>5}  {} ({})",
            g.candidates.len(),
            g.game_name,
            g.library_path.display()
        );
    }

    // ---- Selection / balancing ----
    let mut selected: Vec<&GameCandidates> = Vec::new();
    let mut f_games_with_candidates: Vec<&GameCandidates> = Vec::new();

    for g in &all_games {
        if g.candidates.is_empty() {
            continue;
        }
        if drive_of(&g.library_path) == "F:" {
            f_games_with_candidates.push(g);
        } else {
            selected.push(g);
        }
    }

    let mut f_alpha = f_games_with_candidates.clone();
    f_alpha.sort_by_key(|g| g.game_name.to_lowercase());
    let f_top_n: HashSet<&str> = f_alpha
        .iter()
        .take(F_TOP_N)
        .map(|g| g.game_key.as_str())
        .collect();
    let f_rich: HashSet<&str> = f_games_with_candidates
        .iter()
        .filter(|g| g.candidates.len() >= F_RICH_THRESHOLD)
        .map(|g| g.game_key.as_str())
        .collect();

    let mut f_selected_count = 0usize;
    for g in &f_games_with_candidates {
        if f_top_n.contains(g.game_key.as_str()) || f_rich.contains(g.game_key.as_str()) {
            selected.push(g);
            f_selected_count += 1;
        }
    }

    println!(
        "\nБалансування F:\\ — обрано {f_selected_count} з {} ігор із кандидатами \
         (перші {F_TOP_N} за алфавітом + мовно-багаті з ≥{F_RICH_THRESHOLD} кандидатами).",
        f_games_with_candidates.len()
    );

    // ---- Build corpus rows ----
    // Two-level sampling instead of head-truncation: cap each game at
    // PER_GAME_CAP rows (evenly sampled), then evenly sample the union
    // down to TOTAL_ROW_CAP. Head-truncation would fill the whole cap
    // with the first couple of games (single games routinely carry
    // 50k-150k candidates).
    let mut rows: Vec<Row> = Vec::new();
    let mut rows_before_caps = 0usize;
    for g in &selected {
        let flags = compute_family_flags(&g.candidates);
        let game_rows: Vec<Row> = g
            .candidates
            .iter()
            .zip(flags)
            .map(|(c, family)| Row {
                game_key: g.game_key.clone(),
                rel_path: c.rel_path.clone(),
                expected: label_for(c, family),
                token: c.token.clone(),
                match_type_label: c.match_type.label(),
            })
            .collect();
        rows_before_caps += game_rows.len();
        rows.extend(stride_sample(game_rows, PER_GAME_CAP));
    }

    let rows_after_game_cap = rows.len();
    let truncated = rows.len() > TOTAL_ROW_CAP;
    let rows = stride_sample(rows, TOTAL_ROW_CAP);
    println!(
        "\nВибірка: {rows_before_caps} кандидатів у обраних іграх → {rows_after_game_cap} \
         після ліміту {PER_GAME_CAP}/гру → {} після загального ліміту {TOTAL_ROW_CAP}.",
        rows.len()
    );

    let mut label_counts: HashMap<&str, usize> = HashMap::new();
    for r in &rows {
        *label_counts.entry(r.expected).or_insert(0) += 1;
    }

    println!("\n=== corpus.tsv (після балансування) ===");
    println!(
        "Рядків: {}{}",
        rows.len(),
        if truncated {
            " (рівномірна вибірка за загальним лімітом)"
        } else {
            ""
        }
    );
    let mut label_pairs: Vec<(&str, usize)> = label_counts.into_iter().collect();
    label_pairs.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (label, n) in &label_pairs {
        let pct = 100.0 * (*n as f64) / (rows.len().max(1) as f64);
        println!("  {label}: {n} ({pct:.1}%)");
    }

    // ---- Write tests/corpus/corpus.tsv ----
    let out_path = repo_corpus_path();
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).expect("create tests/corpus directory");
    }
    let mut out = String::from("game_key\trel_path\texpected\n");
    for r in &rows {
        out.push_str(&r.game_key);
        out.push('\t');
        out.push_str(&r.rel_path);
        out.push('\t');
        out.push_str(r.expected);
        out.push('\n');
    }
    fs::write(&out_path, out).expect("write tests/corpus/corpus.tsv");
    println!("\nЗаписано {} рядків у {}", rows.len(), out_path.display());

    // ---- Human-review sample (30 rows) ----
    let has_video = rows.iter().any(|r| r.expected == "video");
    let has_font = rows.iter().any(|r| r.expected == "font");
    let plan: &[(&str, usize)] = if has_video || has_font {
        &[
            ("audio", 8),
            ("text", 4),
            ("video", 4),
            ("font", 2),
            ("unknown", 4),
            ("none", 8),
        ]
    } else {
        &[("audio", 10), ("text", 5), ("unknown", 5), ("none", 10)]
    };

    println!("\n=== Вибірка для перевірки людиною (до 30 рядків) ===");
    if !has_video && !has_font {
        println!(
            "(video/font кандидатів у корпусі не знайдено - лишено стару пропорцію 10/5/5/10)"
        );
    }
    for (label, want) in plan {
        let examples = pick_examples(&rows, label, *want);
        println!("\n-- {label} ({} з {want} запланованих) --", examples.len());
        for r in examples {
            println!(
                "  [{}] {}\t{}\t{}  (token={:?})",
                r.match_type_label, r.game_key, r.rel_path, r.expected, r.token
            );
        }
    }
}

fn repo_corpus_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join("corpus.tsv")
}
