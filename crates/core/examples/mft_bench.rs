//! Benchmarks and cross-checks the MFT-based scanner (`mftscan`) against the
//! regular `walkdir`-based scanner (`scanner::scan_games_parallel`) for a
//! real game library.
//!
//! Run with:
//! `cargo run -p gametrimmer-core --release --example mft_bench -- G:\SteamLibrary`
//! (or with no argument to default to `G:\SteamLibrary`).
//!
//! READ-ONLY: only reads game manifests, directory listings, and - if
//! available - the raw NTFS volume backing the library's drive letter.
//! Performs no writes, deletes, or renames on any drive.
//!
//! The MFT-based path needs raw read access to `\\.\<letter>:`, which
//! requires Administrator privileges (or `SeBackupPrivilege`). Run this from
//! an elevated ("Run as administrator") terminal to exercise it; otherwise
//! it prints a clear message and stops after reporting the walkdir baseline
//! - it never panics on the missing privilege.

use std::env;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gametrimmer_core::error::Result;
use gametrimmer_core::providers::GameInstall;
use gametrimmer_core::scanner::FileEntry;
use gametrimmer_core::{mftscan, providers, scanner};

struct Stats {
    files: u64,
    bytes: u64,
}

fn main() {
    let library_path = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"G:\SteamLibrary"));

    println!("== mft_bench: {} ==", library_path.display());

    let Some(letter) = drive_letter(&library_path) else {
        eprintln!(
            "'{}' has no drive letter - nothing to scan.",
            library_path.display()
        );
        std::process::exit(1);
    };

    let games = discover_games(&library_path);
    if games.is_empty() {
        println!(
            "No installed games found under {} across any provider - nothing to benchmark.",
            library_path.display()
        );
        return;
    }

    println!("Found {} game(s) under this library:", games.len());
    for (id, name, dir) in &games {
        println!("  [{id}] {name} -> {}", dir.display());
    }
    println!();

    let roots: Vec<(i64, PathBuf)> = games
        .iter()
        .map(|(id, _, dir)| (*id, dir.clone()))
        .collect();

    // --- Baseline: walkdir, always runs (no special privileges needed). ---
    let walk_start = Instant::now();
    let walk_results = scanner::scan_games_parallel(&roots);
    let walk_elapsed = walk_start.elapsed();

    report("walkdir scan", &walk_results, walk_elapsed, &games);
    println!();

    // --- MFT-based scan: needs raw volume access. ---
    if !mftscan::is_available(letter) {
        println!(
            "MFT scan unavailable for volume {letter}: cannot open \\\\.\\{letter}: for raw read \
             access. This process is not elevated (or SeBackupPrivilege is not held).\n\
             This is the expected fallback path, not a bug - the real scanner would fall back \
             to the walkdir results printed above.\n\n\
             To measure the MFT-based scan, re-run this example from an Administrator terminal:\n\
             \n  cargo run -p gametrimmer-core --release --example mft_bench -- \"{}\"\n",
            library_path.display()
        );
        return;
    }

    let mft_start = Instant::now();
    let mut last_pct = u64::MAX;
    let mut progress_cb = |p: mftscan::MftProgress| {
        let pct = (p.records_done * 100)
            .checked_div(p.records_total)
            .unwrap_or(0);
        if pct != last_pct {
            eprint!("\r  reading $MFT on {}: {pct}%", p.volume);
            last_pct = pct;
        }
    };
    let mft_results = match mftscan::scan_roots(&roots, Some(&mut progress_cb), None) {
        Ok(results) => results,
        Err(err) => {
            eprintln!("\nMFT scan failed unexpectedly: {err}");
            return;
        }
    };
    eprintln!();
    let mft_elapsed = mft_start.elapsed();

    report("MFT scan", &mft_results, mft_elapsed, &games);
    println!();

    let speedup = walk_elapsed.as_secs_f64() / mft_elapsed.as_secs_f64().max(0.000_001);
    println!(
        "Speedup: {speedup:.1}x ({:.3}s walkdir vs {:.3}s MFT)",
        walk_elapsed.as_secs_f64(),
        mft_elapsed.as_secs_f64()
    );
    println!();

    print_diff(&games, &walk_results, &mft_results);
}

/// Prints total and per-game file count/size for one scan pass.
fn report(
    label: &str,
    results: &[(i64, Result<Vec<FileEntry>>)],
    elapsed: Duration,
    games: &[(i64, String, PathBuf)],
) {
    let totals = summarize(results);
    println!(
        "{label}: {} file(s), {} byte(s), in {:.3}s",
        totals.files,
        totals.bytes,
        elapsed.as_secs_f64()
    );
    for (id, name, _) in games {
        let stats = per_game_stats(results, *id);
        println!(
            "  [{id}] {name}: {} file(s), {} byte(s)",
            stats.files, stats.bytes
        );
    }
}

/// Prints any per-game count/size mismatch between the two scans.
fn print_diff(
    games: &[(i64, String, PathBuf)],
    walk_results: &[(i64, Result<Vec<FileEntry>>)],
    mft_results: &[(i64, Result<Vec<FileEntry>>)],
) {
    println!("Per-game differences (walkdir vs MFT):");
    let mut any_diff = false;

    for (id, name, _) in games {
        let w = per_game_stats(walk_results, *id);
        let m = per_game_stats(mft_results, *id);
        if w.files != m.files || w.bytes != m.bytes {
            any_diff = true;
            println!(
                "  [{id}] {name}: walkdir {} file(s)/{} byte(s) vs MFT {} file(s)/{} byte(s) \
                 (diff {} file(s), {} byte(s) - likely hard links, junctions, or a concurrent \
                 write; see report for details)",
                w.files,
                w.bytes,
                m.files,
                m.bytes,
                m.files as i64 - w.files as i64,
                m.bytes as i64 - w.bytes as i64,
            );
        }
    }

    if !any_diff {
        println!("  (none - both scans agree exactly)");
    }
}

fn summarize(results: &[(i64, Result<Vec<FileEntry>>)]) -> Stats {
    let mut stats = Stats { files: 0, bytes: 0 };
    for (_, result) in results {
        if let Ok(entries) = result {
            stats.files += entries.len() as u64;
            stats.bytes += entries.iter().map(|e| e.size).sum::<u64>();
        }
    }
    stats
}

fn per_game_stats(results: &[(i64, Result<Vec<FileEntry>>)], game_id: i64) -> Stats {
    match results.iter().find(|(id, _)| *id == game_id) {
        Some((_, Ok(entries))) => Stats {
            files: entries.len() as u64,
            bytes: entries.iter().map(|e| e.size).sum(),
        },
        Some((_, Err(err))) => {
            eprintln!("  [{game_id}] scan error: {err}");
            Stats { files: 0, bytes: 0 }
        }
        None => Stats { files: 0, bytes: 0 },
    }
}

/// Discovers every installed game (across all vendors) whose library root
/// matches `library_path` (case-insensitive), assigning each a synthetic,
/// stable id for this run only (not a real database game id).
fn discover_games(library_path: &Path) -> Vec<(i64, String, PathBuf)> {
    let target = library_path.to_string_lossy().to_lowercase();

    let mut games: Vec<GameInstall> = Vec::new();
    for provider in providers::all() {
        let Ok(libraries) = provider.discover() else {
            continue;
        };
        for library in libraries {
            if library.path.to_string_lossy().to_lowercase() == target {
                games.extend(library.games);
            }
        }
    }

    games
        .into_iter()
        .enumerate()
        .map(|(i, game)| (i as i64 + 1, game.name, game.install_dir))
        .collect()
}

/// Extracts the drive letter of an absolute Windows path, e.g. `'G'` from
/// `G:\SteamLibrary`.
fn drive_letter(path: &Path) -> Option<char> {
    let s = path.to_str()?;
    let mut chars = s.chars();
    let letter = chars.next()?;
    if letter.is_ascii_alphabetic() && chars.next() == Some(':') {
        Some(letter.to_ascii_uppercase())
    } else {
        None
    }
}
