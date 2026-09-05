//! Prints the language-footprint table for one or more Battle.net game
//! folders - the acceptance check for `casc::locale_footprint` (GT-492).
//!
//! Run with: `cargo run -p gametrimmer-core --example casc_locales -- <folder>...`
//! With no arguments, scans every subfolder of `F:\Blizzard` and
//! `H:\Blizzard`. Read-only: opens `.build.info`, build configs, `.idx`
//! files, and archive blobs, and reads exactly the bytes it needs from
//! each - never writes, deletes, or renames anything.

use std::env;
use std::fs;
use std::path::PathBuf;

use gametrimmer_core::casc;

fn discover_default_folders() -> Vec<PathBuf> {
    let mut folders = Vec::new();
    for root in [r"F:\Blizzard", r"H:\Blizzard"] {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        let mut subs: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        subs.sort();
        folders.extend(subs);
    }
    folders
}

fn main() {
    // The kept locale is a flag rather than a constant so the table can be
    // asked for a locale nobody has an expected number for. Reproducing
    // figures that were known in advance proves the port agrees with its
    // reference; only asking it something unrehearsed distinguishes a
    // computation from a fit.
    let mut keep = "enUS".to_string();
    let mut args: Vec<String> = env::args().skip(1).collect();
    if let Some(pos) = args.iter().position(|a| a == "--keep") {
        if let Some(value) = args.get(pos + 1).cloned() {
            keep = value;
            args.drain(pos..=pos + 1);
        }
    }
    let keep = keep.as_str();

    let folders: Vec<PathBuf> = if args.is_empty() {
        discover_default_folders()
    } else {
        args.into_iter().map(PathBuf::from).collect()
    };

    if folders.is_empty() {
        eprintln!(
            "no game folders found (pass folders explicitly, or check F:\\Blizzard / H:\\Blizzard)"
        );
        return;
    }

    println!("kept locale: {keep}");
    println!("{:<48} {:>14}  offered locales", "folder", "removable");
    println!("{}", "-".repeat(90));

    for folder in &folders {
        let name = folder.display().to_string();
        match casc::locale_footprint(folder, keep) {
            Ok(Some(footprint)) => {
                println!(
                    "{:<48} {:>11.2} GB  [{}]",
                    name,
                    footprint.removable_bytes as f64 / 1e9,
                    footprint.offered.join(", ")
                );
            }
            Ok(None) => {
                println!("{:<48} {:>14}", name, "Ok(None)");
            }
            Err(err) => {
                println!("{:<48} {:>14}  ({err})", name, "ERROR");
            }
        }
    }
}
