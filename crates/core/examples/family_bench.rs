//! Read-only, synthetic micro-benchmark for mechanism 3
//! (`compute_directory_occurrence_family`, the "same atom position across a
//! directory" language-family heuristic in `src/langdetect/family.rs`).
//!
//! Real game installs occasionally ship one flat directory with thousands of
//! per-language voice-over files sharing the same structural shape - e.g.
//! `VO_Gameplay_Charles000_DE.assets.bank`,
//! `VO_Gameplay_Charles000_FR.assets.bank`,
//! `VO_Gameplay_Charles000_KO.assets.bank`, ... (see the mechanism-3 doc
//! comment in `family.rs` for the real-world naming convention this
//! mirrors). Mechanism 3's "Filter 2" support check used to be an
//! O(members^2) nested scan over the position group, which is fine for a
//! handful of siblings but explosive once a group reaches thousands of
//! members. This benchmark builds such a directory in memory - purely
//! synthetic `FileEntry` values, no filesystem or database access at all -
//! and times `LangDetector::analyze_game` across a few sizes so the
//! before/after cost of that filter is directly comparable.
//!
//! Run with (PowerShell, from the repo root):
//!   cargo run -p gametrimmer-core --release --example family_bench

use std::time::Instant;

use gametrimmer_core::langdetect::LangDetector;
use gametrimmer_core::scanner::FileEntry;

/// Character names cycled through to build the "before" atoms shared by
/// several files at a time (a per-character "take" is voiced in every
/// language, exactly like a real VO set) - this gives the position group
/// plenty of members that share a distinctive, non-universal atom, so the
/// benchmark exercises the C1/C3 index paths as well as the C2 fallback,
/// not just the cheap "no shared atom at all" case.
const CHARACTERS: &[&str] = &[
    "Charles", "William", "James", "Sophie", "Anna", "Mark", "Nina", "Oleg", "Ivan", "Petro",
    "Olena", "Maria", "David", "Sarah", "Thomas",
];

/// Bare two-letter language codes cycled through as the varying language
/// token - the same convention the mechanism-3 doc comment cites
/// (`VO_Gameplay_Charles_DE` / `_FR` / `_KO`).
const CODES: &[&str] = &["de", "fr", "ko", "es", "it", "ru", "pl", "ja"];

/// Builds `count` synthetic sibling files under one directory, all sharing
/// the same atom position for the language token, and a "take" number so
/// groups of `CODES.len()` files at a time also share a distinctive
/// "character+take" atom pair.
fn synthetic_files(count: usize) -> Vec<FileEntry> {
    let group_size = CHARACTERS.len() * CODES.len();
    (0..count)
        .map(|i| {
            let character = CHARACTERS[i % CHARACTERS.len()];
            let code = CODES[i % CODES.len()];
            let take = i / group_size;
            FileEntry {
                rel_path: format!(r"Sound\VO\VO_Gameplay_{character}{take:03}_{code}.assets.bank"),
                size: 0,
                mtime: None,
            }
        })
        .collect()
}

fn main() {
    println!("== family_bench: synthetic mechanism-3 directory-family timing ==");
    println!("(in-memory only - no filesystem or database access)\n");

    let detector = LangDetector::new();

    for &count in &[1_000usize, 5_000, 20_000] {
        let files = synthetic_files(count);
        let start = Instant::now();
        let hits = detector.analyze_game(&files);
        let elapsed = start.elapsed();

        println!(
            "files={count:>6}  elapsed={elapsed:>10.3?}  findings={:>6}",
            hits.len()
        );
    }
}
