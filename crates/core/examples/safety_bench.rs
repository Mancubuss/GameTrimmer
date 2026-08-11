//! Read-only benchmark for the scan-time cost of the deletion safety contract.
//!
//! Every finding a scan produces gets a `capture_safety_snapshot` call before
//! it is stored, so the per-call cost is multiplied by the number of findings -
//! which on a real library is the whole point of measuring it separately from
//! the walk itself.
//!
//! Run with (PowerShell, `$env:Path` already containing cargo):
//!   cargo run -p gametrimmer-core --release --example safety_bench -- \
//!       --dir "H:\SteamLibrary\steamapps\common\SomeGame" --limit 5000
//!
//! Reports, for the files under `--dir` treated as a trusted root:
//!   * `identity` alone (one path, no chain walk)
//!   * `capture_safety_snapshot` for files at their real depth
//!   * `SnapshotCapture`, which reuses the chain across findings
//!   * `capture_safety_snapshot` for directories (which also fingerprints the
//!     whole subtree)
//!
//! Nothing is written and nothing is deleted.

use std::path::{Path, PathBuf};
use std::time::Instant;

use gametrimmer_core::safety::{capture_safety_snapshot, current_identity, SnapshotCapture};

struct Args {
    dir: PathBuf,
    limit: usize,
}

fn parse_args() -> Option<Args> {
    let mut dir = None;
    let mut limit = 2000usize;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => dir = args.next().map(PathBuf::from),
            "--limit" => limit = args.next()?.parse().ok()?,
            _ => return None,
        }
    }
    Some(Args { dir: dir?, limit })
}

/// Collects up to `limit` files and every directory under `root`, with the
/// relative path each one would be stored under.
fn collect(root: &Path, limit: usize) -> (Vec<String>, Vec<String>) {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(current) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = relative.to_string_lossy().to_string();
            match entry.file_type() {
                Ok(file_type) if file_type.is_dir() => {
                    dirs.push(relative);
                    pending.push(path);
                }
                Ok(file_type) if file_type.is_file() && files.len() < limit => {
                    files.push(relative);
                }
                _ => {}
            }
        }
        if files.len() >= limit && pending.is_empty() {
            break;
        }
    }
    (files, dirs)
}

fn report(label: &str, count: usize, elapsed: std::time::Duration) {
    if count == 0 {
        println!("{label:<38} no samples");
        return;
    }
    let per_call_us = elapsed.as_secs_f64() * 1e6 / count as f64;
    let per_second = count as f64 / elapsed.as_secs_f64();
    println!(
        "{label:<38} {count:>7} calls  {:>8.1} ms  {per_call_us:>7.1} us/call  {per_second:>9.0}/s",
        elapsed.as_secs_f64() * 1000.0
    );
}

fn main() {
    let Some(args) = parse_args() else {
        eprintln!("usage: safety_bench --dir <path> [--limit N]");
        std::process::exit(2);
    };
    let root = match args.dir.canonicalize() {
        Ok(root) => root,
        Err(err) => {
            eprintln!("cannot read {}: {err}", args.dir.display());
            std::process::exit(1);
        }
    };

    let started = Instant::now();
    let (files, dirs) = collect(&root, args.limit);
    println!(
        "root: {}\ncollected {} files and {} directories in {:.1} ms\n",
        root.display(),
        files.len(),
        dirs.len(),
        started.elapsed().as_secs_f64() * 1000.0
    );

    let started = Instant::now();
    let mut ok = 0usize;
    for relative in &files {
        if current_identity(&root.join(relative)).is_ok() {
            ok += 1;
        }
    }
    report("identity (single path)", ok, started.elapsed());

    let started = Instant::now();
    let mut ok = 0usize;
    for relative in &files {
        if capture_safety_snapshot(&root, relative).is_ok() {
            ok += 1;
        }
    }
    report("capture_safety_snapshot (file)", ok, started.elapsed());

    let started = Instant::now();
    let mut capture = SnapshotCapture::new();
    let mut ok = 0usize;
    for relative in &files {
        if capture.capture(&root, relative).is_ok() {
            ok += 1;
        }
    }
    report(
        "SnapshotCapture (file, cached chain)",
        ok,
        started.elapsed(),
    );

    let started = Instant::now();
    let mut ok = 0usize;
    for relative in dirs.iter().take(args.limit) {
        if capture_safety_snapshot(&root, relative).is_ok() {
            ok += 1;
        }
    }
    report("capture_safety_snapshot (directory)", ok, started.elapsed());
}
