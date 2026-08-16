//! Proves - or disproves - that an `$MFT` record states the same identity
//! the filesystem reports when the file is opened.
//!
//! # Why this has to run before the optimisation it serves
//!
//! Every scan opens one handle per flagged file to record a
//! `safety::FileIdentity`, and on a big library that is 720 k `CreateFileW`
//! calls, measured at a fifth of the analyze phase. The records parsed
//! during the MFT pass already carry the same facts, so the calls look
//! removable.
//!
//! The danger is not that the substitution fails loudly. It is that a field
//! disagrees *systematically* - NTFS keeps the directory bit somewhere Win32
//! does not, say, or a size is reported differently for a compressed file -
//! and every snapshot then differs from what the delete-time check reads
//! live. The contract is fail-closed, so nothing unsafe happens: instead
//! **every deletion blocks** with `TargetChanged`, and the user finds out,
//! not the developer.
//!
//! So: compare the two on real files first, field by field, and only then
//! decide. A single mismatched field is a veto for that field, not a reason
//! to abandon the measure - the identity can be built from the MFT for the
//! fields that agree and read live for the rest, or the whole file can fall
//! back to being opened.
//!
//! # Running it
//!
//! Needs Administrator - reading `\\.\<letter>:` raw is what the MFT pass
//! does, and this replays it:
//!
//! ```text
//! cargo run -p gametrimmer-core --release --example mft_identity_check -- \
//!     --dir "F:\SteamLibrary\steamapps\common\SomeGame" --limit 5000
//! ```
//!
//! Read-only from first line to last: it opens the volume for reading,
//! opens files for metadata, and writes nothing anywhere.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use gametrimmer_core::mftscan;
use gametrimmer_core::safety::{current_identity, FileIdentity, TargetKind};
use gametrimmer_core::scanner::FileEntry;

struct Args {
    dir: PathBuf,
    limit: usize,
}

fn parse_args() -> Option<Args> {
    let mut dir = None;
    let mut limit = 5000usize;
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

/// One field's verdict across every file compared. Counted rather than
/// collected: a systematic disagreement shows up as a count equal to the
/// sample size, and an occasional one as a handful, and those two mean
/// entirely different things.
#[derive(Default)]
struct FieldTally {
    matched: usize,
    differed: usize,
    /// A few concrete disagreements, kept for the report - a count alone
    /// never says *how* two values differ, and the how is the whole
    /// diagnosis.
    examples: Vec<String>,
}

impl FieldTally {
    fn record(&mut self, agrees: bool, describe: impl FnOnce() -> String) {
        if agrees {
            self.matched += 1;
        } else {
            self.differed += 1;
            if self.examples.len() < 3 {
                self.examples.push(describe());
            }
        }
    }
}

fn compare(
    tallies: &mut BTreeMap<&'static str, FieldTally>,
    field: &'static str,
    agrees: bool,
    describe: impl FnOnce() -> String,
) {
    tallies.entry(field).or_default().record(agrees, describe);
}

/// Reads one game directory's files the way a scan does, via the MFT.
///
/// Every failure is reported. `scan_roots` reports per-volume failures
/// *inside* its result - one `Err` per game rather than one for the call -
/// and the first version of this function collected the `Ok`s and dropped
/// the rest, so a volume this process was not allowed to open produced
/// "0 files" and no reason at all.
fn read_entries(dir: &Path) -> Vec<FileEntry> {
    let Some(letter) = mftscan::volume_letter(dir) else {
        eprintln!("{} has no drive letter to scan", dir.display());
        return Vec::new();
    };
    if let Err(err) = mftscan::availability(letter) {
        eprintln!("volume {letter}: cannot be read: {err}");
        return Vec::new();
    }

    let roots = vec![(1i64, dir.to_path_buf())];
    let results = match mftscan::scan_roots(&roots, None, None) {
        Ok(results) => results,
        Err(err) => {
            eprintln!("MFT scan failed: {err}");
            return Vec::new();
        }
    };

    let mut entries = Vec::new();
    for (_, result) in results {
        match result {
            Ok(found) => entries.extend(found),
            Err(err) => eprintln!("volume scan failed: {err}"),
        }
    }
    if entries.is_empty() {
        eprintln!(
            "the volume was read but nothing under {} matched - check the path",
            dir.display()
        );
    }
    entries
}

fn main() {
    let Some(args) = parse_args() else {
        eprintln!("usage: mft_identity_check --dir <game directory> [--limit N]");
        std::process::exit(2);
    };

    println!("Reading {} through the MFT ...", args.dir.display());
    let started = Instant::now();
    let entries = read_entries(&args.dir);
    println!("{} files in {:.1?}\n", entries.len(), started.elapsed());

    if entries.is_empty() {
        eprintln!("nothing to compare");
        std::process::exit(1);
    }

    let mut tallies: BTreeMap<&'static str, FieldTally> = BTreeMap::new();
    let mut without_identity = 0usize;
    let mut unopenable = 0usize;
    let mut compared = 0usize;
    let mut live_time = std::time::Duration::ZERO;

    for entry in entries.iter().take(args.limit) {
        let Some(mft) = entry.mft_identity else {
            without_identity += 1;
            continue;
        };
        let path = args.dir.join(&entry.rel_path);

        let at = Instant::now();
        let live: FileIdentity = match current_identity(&path) {
            Ok(identity) => identity,
            Err(_) => {
                unopenable += 1;
                continue;
            }
        };
        live_time += at.elapsed();
        compared += 1;

        let name = || entry.rel_path.clone();
        compare(
            &mut tallies,
            "volume_serial",
            mft.volume_serial == live.volume_serial,
            || {
                format!(
                    "{}: mft {} live {}",
                    name(),
                    mft.volume_serial,
                    live.volume_serial
                )
            },
        );
        compare(
            &mut tallies,
            "file_index",
            mft.file_index == live.file_index,
            || {
                format!(
                    "{}: mft {:#x} live {:#x}",
                    name(),
                    mft.file_index,
                    live.file_index
                )
            },
        );
        compare(&mut tallies, "size", mft.size == live.size, || {
            format!("{}: mft {} live {}", name(), mft.size, live.size)
        });
        compare(
            &mut tallies,
            "last_write_time",
            mft.last_write_time == live.last_write_time,
            || {
                format!(
                    "{}: mft {} live {} (delta {})",
                    name(),
                    mft.last_write_time,
                    live.last_write_time,
                    live.last_write_time as i128 - mft.last_write_time as i128,
                )
            },
        );
        compare(
            &mut tallies,
            "kind",
            (live.kind == TargetKind::Directory) == mft.is_directory,
            || {
                format!(
                    "{}: mft dir={} live {:?}",
                    name(),
                    mft.is_directory,
                    live.kind
                )
            },
        );
        // The one field known to be defined differently on each side, which
        // is exactly why it is measured rather than assumed: NTFS does not
        // keep the directory bit in $STANDARD_INFORMATION, so the raw
        // comparison is expected to fail while the bit that matters - is
        // this a reparse point - may still agree.
        compare(
            &mut tallies,
            "attributes (raw)",
            mft.nt_attributes == live.attributes,
            || {
                format!(
                    "{}: mft {:#x} live {:#x} (differing bits {:#x})",
                    name(),
                    mft.nt_attributes,
                    live.attributes,
                    mft.nt_attributes ^ live.attributes,
                )
            },
        );
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        compare(
            &mut tallies,
            "attributes (reparse bit)",
            (mft.nt_attributes & FILE_ATTRIBUTE_REPARSE_POINT)
                == (live.attributes & FILE_ATTRIBUTE_REPARSE_POINT),
            || {
                format!(
                    "{}: mft {:#x} live {:#x}",
                    name(),
                    mft.nt_attributes,
                    live.attributes,
                )
            },
        );
    }

    println!("compared {compared} files");
    if without_identity > 0 {
        println!("{without_identity} carried no MFT identity (would still be opened)");
    }
    if unopenable > 0 {
        println!("{unopenable} could not be opened live (nothing to compare against)");
    }
    if compared > 0 {
        println!(
            "live identity cost {:.1?} total, {:.0} us each\n",
            live_time,
            live_time.as_secs_f64() * 1e6 / compared as f64,
        );
    }

    println!("{:<26} {:>9} {:>9}", "field", "agreed", "differed");
    for (field, tally) in &tallies {
        println!("{:<26} {:>9} {:>9}", field, tally.matched, tally.differed);
    }

    let disagreeing: Vec<_> = tallies.iter().filter(|(_, t)| t.differed > 0).collect();
    if disagreeing.is_empty() {
        println!("\nEvery field agreed on every file.");
        return;
    }

    println!("\nDisagreements:");
    for (field, tally) in disagreeing {
        let verdict = if tally.matched == 0 {
            "systematic - this field cannot come from the MFT as it stands"
        } else {
            "occasional - look at what these files have in common"
        };
        println!("  {field}: {verdict}");
        for example in &tally.examples {
            println!("      {example}");
        }
    }
}
