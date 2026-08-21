//! Stage stopwatches for the scan pipeline.
//!
//! The scan log reports three spans (`scan`, `analyze`, `total`) and that is
//! enough to judge a build, but not to decide what to work on next: every
//! optimisation so far was ranked by reading code, because nothing measured
//! where the analyze window actually goes. This module is that measurement.
//!
//! Timing is taken **per game** (or per MFT chunk), never per file, so the
//! instrument costs a few thousand `Instant::now()` calls per scan rather
//! than tens of millions - cheap enough to leave switched on permanently,
//! which is the point: the next round of work should not have to re-add it.
//!
//! The counters are summed across all scan worker threads, so the report is
//! CPU time, not wall clock, and its total legitimately exceeds the run's
//! `analyze` span by roughly the worker count. Read it for *proportions*
//! between stages; read the `Scan done in` line for elapsed time.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// One measured stage of the pipeline. Ordered as the work happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Pulling `$MFT` bytes off the volume (`reader::drive_scan`'s fetch).
    MftRead,
    /// Turning those bytes into FRN records (`record::parse_chunk`).
    MftParse,
    /// `langdetect::tokens::tokenize_path` over every file of a game.
    Tokenize,
    /// Dictionary lookups over the tokens.
    Occurrences,
    /// The language-family heuristic.
    Family,
    /// The per-file decision pass that follows it.
    LangDecide,
    /// `RuleEngine::classify` over every file of a game.
    Rules,
    /// `assign_group_dirs`.
    Grouping,
    /// Building each finding's `SafetySnapshot` (one `CreateFileW` per file).
    Safety,
    /// The single writer thread's inserts.
    Persist,
    /// Time [`Stage::Persist`] spent inside SQL: every `execute` the writer
    /// issues, the per-file insert loop included, but not the commit.
    PersistSql,
    /// Time [`Stage::Persist`] spent committing a batch.
    PersistCommit,
}

/// Every stage, in report order. Kept beside `Stage` so adding a variant
/// without listing it here fails the exhaustiveness check in `name`.
const ALL: [Stage; 12] = [
    Stage::MftRead,
    Stage::MftParse,
    Stage::Tokenize,
    Stage::Occurrences,
    Stage::Family,
    Stage::LangDecide,
    Stage::Rules,
    Stage::Grouping,
    Stage::Safety,
    Stage::Persist,
    Stage::PersistSql,
    Stage::PersistCommit,
];

/// Stages that measure part of another stage rather than work of their own.
/// [`report`] leaves them out: they are subsets of [`Stage::Persist`], and
/// adding them to its sum would charge the same nanoseconds twice and skew
/// every other stage's share. [`persist_breakdown`] reports them instead.
const BREAKDOWN: [Stage; 2] = [Stage::PersistSql, Stage::PersistCommit];

fn name(stage: Stage) -> &'static str {
    match stage {
        Stage::MftRead => "mft_read",
        Stage::MftParse => "mft_parse",
        Stage::Tokenize => "tokenize",
        Stage::Occurrences => "occurrences",
        Stage::Family => "family",
        Stage::LangDecide => "lang_decide",
        Stage::Rules => "rules",
        Stage::Grouping => "grouping",
        Stage::Safety => "safety",
        Stage::Persist => "persist",
        Stage::PersistSql => "persist_sql",
        Stage::PersistCommit => "persist_commit",
    }
}

static NANOS: [AtomicU64; ALL.len()] = [const { AtomicU64::new(0) }; ALL.len()];

/// Bytes pulled off the volumes during [`Stage::MftRead`]. Paired with that
/// stage's duration it gives the read's throughput, which is the only
/// honest warm/cold reading available: Windows will not say whether another
/// file's data is resident, but a `$MFT` arriving at 250 MB/s came from the
/// platter and one arriving at 3 GB/s came from memory. See
/// [`crate::sysinfo`].
static MFT_BYTES: AtomicU64 = AtomicU64::new(0);

/// Adds an already-measured span to `stage`.
pub fn add(stage: Stage, elapsed: Duration) {
    NANOS[stage as usize].fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
}

/// Records bytes read from a volume's `$MFT`.
pub fn add_mft_bytes(bytes: u64) {
    MFT_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

/// Runs `body`, charging its wall time to `stage`. Returns whatever `body`
/// returned, including `Err` - a cancelled stage still cost what it cost.
pub fn timed<T>(stage: Stage, body: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let out = body();
    add(stage, started.elapsed());
    out
}

/// Zeroes every counter. Called at the start of a scan so the report
/// describes that run rather than every run since the app started.
pub fn reset() {
    for counter in &NANOS {
        counter.store(0, Ordering::Relaxed);
    }
    MFT_BYTES.store(0, Ordering::Relaxed);
}

/// Reads one stage's accumulated CPU time.
pub fn elapsed(stage: Stage) -> Duration {
    Duration::from_nanos(NANOS[stage as usize].load(Ordering::Relaxed))
}

/// Megabytes per second achieved while reading the `$MFT`s, or `None` when
/// nothing was read (an unelevated run, or one routed entirely to walkdir).
pub fn mft_throughput() -> Option<f64> {
    let seconds = elapsed(Stage::MftRead).as_secs_f64();
    let bytes = MFT_BYTES.load(Ordering::Relaxed);
    (bytes > 0 && seconds > 0.0).then(|| bytes as f64 / seconds / 1024.0 / 1024.0)
}

/// One log line: every stage with a non-zero total, largest first, with its
/// share of the measured sum. Stages that never ran are omitted rather than
/// printed as zeros.
pub fn report() -> String {
    let mut rows: Vec<(Stage, Duration)> = ALL
        .iter()
        .filter(|stage| !BREAKDOWN.contains(stage))
        .map(|&stage| (stage, elapsed(stage)))
        .filter(|(_, spent)| !spent.is_zero())
        .collect();
    rows.sort_by_key(|(_, spent)| std::cmp::Reverse(*spent));

    let sum: Duration = rows.iter().map(|(_, spent)| *spent).sum();
    let parts: Vec<String> = rows
        .iter()
        .map(|(stage, spent)| {
            let share = if sum.is_zero() {
                0
            } else {
                (spent.as_secs_f64() / sum.as_secs_f64() * 100.0).round() as u64
            };
            let rate = match *stage {
                // The warm/cold reading - see `MFT_BYTES`.
                Stage::MftRead => mft_throughput()
                    .map(|mb| format!(", {mb:.0} MB/s"))
                    .unwrap_or_default(),
                _ => String::new(),
            };
            format!("{} {:.1?} ({share}%{rate})", name(*stage), spent)
        })
        .collect();

    format!(
        "Stage CPU time (summed over workers, {:.1?}): {}",
        sum,
        parts.join(", ")
    )
}

/// Splits the writer's total into the three things it is made of, so a
/// number that has moved between 17.6 s and 26.3 s across runs can be
/// attributed instead of guessed at.
///
/// `row building` is what is left after the SQL and the commit: constructing
/// one `FindingRow` per finding (seven clones apiece, a `PathBuf` and a
/// `LibraryOrigin` among them) and encoding two `FileIdentity` values into
/// strings with `format!`. A Python replay of the same inserts against a copy
/// of the real database took 11.4 s single-threaded, so the gap between that
/// and what this thread charges is either this work or contention with the
/// scan pool - and those need telling apart before either is worth fixing.
///
/// Returns `None` when the writer never ran.
pub fn persist_breakdown() -> Option<String> {
    let total = elapsed(Stage::Persist);
    if total.is_zero() {
        return None;
    }
    let sql = elapsed(Stage::PersistSql);
    let commit = elapsed(Stage::PersistCommit);
    let rest = total.saturating_sub(sql + commit);
    let share = |part: Duration| (part.as_secs_f64() / total.as_secs_f64() * 100.0).round() as u64;
    Some(format!(
        "Writer breakdown ({:.1?} total): sql {:.1?} ({}%), commit {:.1?} ({}%),          row building {:.1?} ({}%)",
        total,
        sql,
        share(sql),
        commit,
        share(commit),
        rest,
        share(rest)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The counters are process-global, so this is the one test that may
    /// touch them; it resets first and asserts on proportions rather than
    /// absolute times.
    #[test]
    fn timed_charges_the_stage_it_names_and_report_lists_it() {
        reset();
        timed(Stage::Rules, || {
            std::thread::sleep(Duration::from_millis(5))
        });

        assert!(elapsed(Stage::Rules) >= Duration::from_millis(5));
        assert!(elapsed(Stage::Safety).is_zero());

        let line = report();
        assert!(line.contains("rules"), "{line}");
        assert!(
            !line.contains("safety"),
            "a stage that never ran should not be printed: {line}"
        );
    }
}
