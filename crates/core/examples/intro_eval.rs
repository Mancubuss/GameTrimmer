//! Diagnostic dump of every `Category::Intro` hit the repo's `rules.json`
//! produces over a real file list - including the GT-206 same-name sibling
//! sweep, so the number printed here is the number of files the app would
//! actually stub.
//!
//! Two inputs:
//!
//! * **corpus** (default) - `tests/corpus/corpus.tsv`. Cheap and in-repo, but
//!   it is a *localization sampling* corpus capped at 45 rows per game, so it
//!   cannot measure how far an intro rule reaches. Kept only as a smoke test.
//! * **library** - a TSV of a real installed library, `game<TAB>size<TAB>rel_path`,
//!   which is the only honest safety evidence for an intro rule change. Build
//!   one with `find` over the library roots.
//!
//! A second argument is the keep-language list, comma separated. It matters:
//! an intro rule's claim is vetoed when the file carries one of the user's
//! kept languages (`worker::keep_language_vetoes_rule`), so the answer depends
//! on a setting and any number quoted without one is meaningless.
//!
//! ```text
//! cargo run -p gametrimmer-core --example intro_eval > corpus.txt
//! cargo run -p gametrimmer-core --example intro_eval -- library.tsv > lib.txt
//! cargo run -p gametrimmer-core --example intro_eval -- library.tsv en,de > lib_de.txt
//! ```
//!
//! An intro false positive destroys a unique video with nothing to
//! re-download it from, so every newly caught path has to be read one by one:
//! run this before and after touching the intro rules and diff the output.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gametrimmer_core::langdetect::LangDetector;
use gametrimmer_core::reference::GameReference;
use gametrimmer_core::rules::{Category, RuleEngine};
use gametrimmer_core::scanner::{same_name_siblings, FileEntry};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// One row of either input: which game it belongs to, its size and its path.
struct Row {
    game: String,
    entry: FileEntry,
}

fn read_corpus() -> Vec<Row> {
    let corpus = repo_root().join("tests").join("corpus").join("corpus.tsv");
    let contents = std::fs::read_to_string(&corpus)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", corpus.display()));
    contents
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with("game_key"))
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let (game, rel_path, _) = (parts.next()?, parts.next()?, parts.next()?);
            Some(Row {
                game: game.to_string(),
                // The corpus carries no sizes; 0 for every row makes the
                // sibling sweep's size guard a no-op rather than a lie.
                entry: FileEntry::logical_only(rel_path, 0, None),
            })
        })
        .collect()
}

/// `game<TAB>size<TAB>rel_path`, `/` or `\` separators.
fn read_library(path: &str) -> Vec<Row> {
    let contents =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("could not read {path}: {e}"));
    contents
        .lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let (game, size, rel_path) = (parts.next()?, parts.next()?, parts.next()?);
            Some(Row {
                game: game.to_string(),
                entry: FileEntry::logical_only(
                    rel_path.replace('/', "\\"),
                    size.parse().unwrap_or(0),
                    None,
                ),
            })
        })
        .collect()
}

fn main() {
    let arg = std::env::args().nth(1);
    let rows = match arg.as_deref() {
        None | Some("corpus") => read_corpus(),
        Some(path) => read_library(path),
    };
    // The catalogue is half the intro answer and lives outside rules.json
    // (see core::reference), so an eval without it measures the heuristics
    // alone and silently reports worse coverage than the app has.
    let engine = RuleEngine::load(&repo_root().join("rules.json"))
        .expect("repo rules.json should load")
        .with_reference(GameReference::builtin().expect("built-in catalogue should parse"));
    let detector = match std::env::args().nth(2) {
        Some(keep) => LangDetector::with_keep_list(
            &keep
                .split(',')
                .map(|tag| tag.trim().to_string())
                .collect::<Vec<_>>(),
        ),
        None => LangDetector::new(),
    };
    let mut vetoed = 0usize;

    // Group by game: both the sibling sweep and a real scan work one game at
    // a time, and a sweep across games would be meaningless.
    let mut by_game: Vec<(String, Vec<FileEntry>)> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for row in rows {
        let slot = *seen.entry(row.game.clone()).or_insert_with(|| {
            by_game.push((row.game.clone(), Vec::new()));
            by_game.len() - 1
        });
        by_game[slot].1.push(row.entry);
    }

    let mut total = 0usize;
    let mut rule_hits = 0usize;
    let mut swept = 0usize;
    for (game, entries) in &by_game {
        total += entries.len();
        let mut sources = Vec::new();
        let mut descs: HashMap<usize, (u8, String)> = HashMap::new();
        let mut claimed: HashSet<usize> = HashSet::new();
        for (index, entry) in entries.iter().enumerate() {
            let Some(finding) = engine.classify(&entry.rel_path, None).flagged() else {
                continue;
            };
            claimed.insert(index);
            if finding.category != Category::Intro {
                continue;
            }
            // Exactly the guard both classification paths apply.
            if gametrimmer_core::worker::keep_language_vetoes_rule(
                &detector,
                &finding,
                &entry.rel_path,
            ) {
                vetoed += 1;
                eprintln!("veto\t{game}\t{}", entry.rel_path);
                continue;
            }
            sources.push(index);
            descs.insert(index, (finding.confidence, finding.rule_desc));
        }
        for index in &sources {
            rule_hits += 1;
            let (confidence, desc) = &descs[index];
            println!(
                "{game}\t{}\t{}\trule\t{confidence}\t{desc}",
                entries[*index].size, entries[*index].rel_path
            );
        }
        for (sibling, source) in same_name_siblings(entries, &sources, &claimed) {
            swept += 1;
            let (confidence, _) = &descs[&source];
            println!(
                "{game}\t{}\t{}\tsweep\t{confidence}\tcopy of {}",
                entries[sibling].size, entries[sibling].rel_path, entries[source].rel_path
            );
        }
    }
    eprintln!(
        "intro: {} total ({rule_hits} by rule + {swept} swept siblings), \
         {vetoed} vetoed by the keep-language list / {total} rows / {} games",
        rule_hits + swept,
        by_game.len()
    );
}
