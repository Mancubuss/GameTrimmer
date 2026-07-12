//! Diagnostic dump for the langdetect corpus regression (see
//! `crates/core/tests/corpus.rs` and `tests/corpus/README.md`).
//!
//! For every corpus row, prints one TSV line with expected vs. actual
//! (language/kind/confidence/reason, or "NOTFLAGGED"), plus a `category`
//! column classifying the disagreement. This is a throwaway analysis tool
//! (not part of the test suite) — run it and redirect stdout to a scratch
//! file for offline analysis:
//!
//! `cargo run -p gametrimmer-core --example corpus_eval > out.tsv`

use std::collections::HashMap;
use std::path::PathBuf;

use gametrimmer_core::langdetect::LangDetector;
use gametrimmer_core::scanner::FileEntry;

fn corpus_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join("corpus.tsv")
}

struct Row {
    game_key: String,
    rel_path: String,
    expected: String,
}

fn parse_corpus(contents: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    for line in contents.lines() {
        if line.is_empty() || line.starts_with("game_key") {
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let (Some(game_key), Some(rel_path), Some(expected)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        rows.push(Row {
            game_key: game_key.to_string(),
            rel_path: rel_path.to_string(),
            expected: expected.trim().to_string(),
        });
    }
    rows
}

fn main() {
    let path = corpus_path();
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    let rows = parse_corpus(&contents);

    let mut order: Vec<String> = Vec::new();
    let mut by_game: HashMap<String, Vec<&Row>> = HashMap::new();
    for row in &rows {
        by_game.entry(row.game_key.clone()).or_insert_with(|| {
            order.push(row.game_key.clone());
            Vec::new()
        });
        by_game.get_mut(&row.game_key).unwrap().push(row);
    }

    let detector = LangDetector::new();

    println!("game_key\trel_path\texpected\tcategory\tactual_lang\tactual_kind\tactual_confidence\tactual_reason");

    for game_key in &order {
        let game_rows = &by_game[game_key];
        let files: Vec<FileEntry> = game_rows
            .iter()
            .map(|r| FileEntry {
                rel_path: r.rel_path.clone(),
                size: 0,
                mtime: None,
            })
            .collect();

        let findings = detector.analyze_game(&files);
        let found: HashMap<usize, gametrimmer_core::langdetect::LangFinding> =
            findings.into_iter().collect();

        for (idx, row) in game_rows.iter().enumerate() {
            let is_flagged = found.contains_key(&idx);
            let (actual_lang, actual_kind, actual_conf, actual_reason) = match found.get(&idx) {
                Some(f) => (
                    f.lang_tag.clone(),
                    format!("{:?}", f.kind),
                    f.confidence.to_string(),
                    f.reason.clone(),
                ),
                None => (
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                ),
            };

            let category = if row.expected == "none" {
                if !is_flagged {
                    "MATCH"
                } else if found[&idx].reason.contains("сім'я") {
                    "FP_FAMILY"
                } else {
                    "FP_NONFAMILY"
                }
            } else if !is_flagged {
                "FN"
            } else {
                let want = match row.expected.as_str() {
                    "audio" => Some("Audio"),
                    "text" => Some("Text"),
                    "video" => Some("Video"),
                    "font" => Some("Font"),
                    _ => None, // "unknown" — any kind acceptable
                };
                match want {
                    Some(w) if actual_kind != w => "KIND_MISMATCH",
                    _ => "MATCH",
                }
            };

            println!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                game_key,
                row.rel_path,
                row.expected,
                category,
                actual_lang,
                actual_kind,
                actual_conf,
                actual_reason
            );
        }
    }
}
