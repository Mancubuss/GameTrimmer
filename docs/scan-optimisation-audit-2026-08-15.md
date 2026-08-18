# Scan optimisation audit — 2026-08-15

Where a scan's 111 seconds actually go, and what can be taken off that
number. Companion to `scan-perf-baseline-2026-08-15.md`, which established
the reference figures; this one asks what they are made of.

Everything under "measured" was timed against `temp/gametrimmer.db` — the
real 850 MB database from the reference run — on a warm cache. Everything
under "reasoned" is read off the code with the row counts in hand and is
explicitly *not* measured; treat those numbers as ranking, not as budget.

## The workload, in rows

| | |
|---|---|
| `files` | 4 914 918 |
| `findings` | 720 618 |
| `file_safety` | 720 618 |
| `games` | 1 601 |
| Database | 850 MB (207 552 pages × 4 KiB) |
| Average `rel_path` | 59 chars (≈ 290 MB of path text in `files` alone) |

The scan writes **6.4 million rows** per run and deletes 5.6 million more on
the next one. Findings — the thing the user actually sees — are 11 % of that.

## Measured

Timed on the real database, warm, `synchronous=NORMAL`, WAL, the app's own
pragmas:

| What | Cost |
|---|---|
| Insert 4.9 M `files` rows (both indexes) | 11.7 s |
| …with `idx_files_scan_id` dropped | 9.9 s |
| `UPDATE files SET scan_id` over 4.9 M rows | 4.8 s |
| `SELECT id, rel_path FROM files` → map, 4.9 M rows | 3.6 s |
| Insert 720 k `findings` | 1.7 s |
| Insert 720 k `file_safety` | 2.4 s |
| Prune one whole generation, foreign keys **on** | 13.7 s |
| Prune one whole generation, foreign keys **off** | 12.4 s |
| `pragma_foreign_key_check` over the whole database | 0.9 s |

Two of those deserve to be read twice.

**The prune is not a foreign-key problem.** The 09:57 run's 45.7 s
housekeeping invited the theory that `prune_superseded_generations` pays the
per-row child-existence check `persist_libraries` already learned to turn
off. It does not: 13.7 s against 12.4 s. The cost is the row count itself —
`DELETE FROM files` alone is 5.3 s of that, plus a 4.6 s commit. Turning the
pragma off here would buy about a second and would be a change made for the
wrong reason.

**Roughly a quarter of the analyze phase is the writer thread, and a third
of the writer is waste.** Adding the measured pieces the single writer must
do serially: 11.7 + 4.8 + 3.6 + 1.7 + 2.4 ≈ 24 s, plus ~1 s of per-game
statements, against a 68 s analyze phase. Of that, the `UPDATE` and the
read-back — 8.4 s — buy nothing that isn't already in hand.

## The cost centre

`files` holds every file of every game. It is written (12 s), immediately
rewritten (5 s), immediately read back (4 s), and deleted on the next scan
(10 s) — call it 30 seconds a run and 700 MB on disk, to store 4.2 million
rows nothing displays.

Every consumer of `files` reaches it through
`JOIN files f ON f.id = fi.file_id` — that is, only ever the flagged 11 %.
With one exception: `worker::rules_io`'s rule-import impact preview
(`rules_io.rs:306`) re-classifies the whole active inventory to answer "what
would importing these rules change?" without a rescan. That one feature is
what the other 4.2 million rows exist for.

That is the biggest single decision available, and it is a product decision,
not a tuning one — see (E) below.

## Ranked opportunities

### A. Waste in the writer — ~9 s, small diffs

**A1. `UPDATE files SET scan_id = (SELECT scan_id FROM games …)`
(`persistence.rs:328`) — measured 4.8 s.** A full second write pass over the
largest table to set a column that was known before the first insert.
`store_files_no_tx` takes the value and writes it in the `INSERT`.

**A2. The read-back map (`persistence.rs:333`) — measured 3.6 s+.** Every
row just inserted is selected back and keyed by `rel_path` into a
`HashMap<String, i64>` (4.9 M owned strings) to resolve 720 k `file_id`s.
The ids are already known: SQLite assigns them in insert order.
`store_files_no_tx` returns them positionally, `PreparedFinding` carries its
`entries` index instead of a `rel_path` copy — `classify_game` has that index
in hand already (`combined_by_index`) and throws it away.

**A3. `idx_files_scan_id` — measured 1.8 s per scan.** Added by migration 3.
Within a generation every row shares one `scan_id`, so it has no selectivity
where it is used; the load query drives `findings → files` by rowid and the
preview scans the table anyway. Confirm with `EXPLAIN QUERY PLAN` on
`load.rs:182` and `rules_io.rs:306`, then drop it.

**A4. Dead per-game deletes.** `persist_prepared_game` opens with
`DELETE FROM findings WHERE file_id IN (SELECT id FROM files WHERE game_id = ?)`
and `store_files_no_tx` with `DELETE FROM files WHERE game_id = ?`. On a
fresh generation the `games` rows were created moments earlier by
`persist_libraries`, so both match nothing, 1 601 times. Cheap individually,
free to delete.

**A5. Four per-game round trips that are per-library facts.** `scan_id`, the
library's `(vendor, path)`, and the `scan_library_evidence` status are
re-queried for every game (`persistence.rs:340-368`). All three are fixed
before the writer starts; resolve them once per library into a map.

### B. Overlap the two phases — up to ~30 s of wall clock

The scan phase (33-42 s) is one thread reading `$MFT`s while six scan
threads and the writer sit idle. Three things are serialised that need not
be:

- **Volumes are read one after another.** `run_mft_pass` loops
  `for roots in candidates_by_volume.into_values()`, and `scan_roots` loops
  again inside. Volumes are independent physical devices — on a library
  spread over three HDDs this is a 3× wall-clock loss on the phase that
  routing chose the MFT for in the first place. `scan_volume_catching_panics`
  already documents the requirement for parallelising it: each task needs its
  own `catch_unwind` inside its own closure.
- **Walkdir games wait for the MFT pass they never use.** 33 of 1 603 games
  in the reference run, but they are gated behind the whole pass regardless.
- **No game classifies until every volume is read.** A volume's entries are
  complete the moment that volume finishes; the pool could start on them
  while the next volume streams. Analyze is mostly CPU, the MFT pass is
  mostly IO — they overlap almost perfectly.

This is the largest wall-clock win available that changes no stored data and
no output. It is also the one with real concurrency risk (panic containment,
cancellation, progress reporting across two live phases), so it wants its own
change, not a corner of another one.

### C. The classify hot loop — reasoned, the largest CPU block

Analyze is 68 s; the writer accounts for ~25 s of it, leaving the six-thread
pool as the critical path for the rest. Across 4.9 M files that is on the
order of 50 µs of CPU per path to tokenize 59 characters, run 33 regexes and
build a directory chain. That is high, and the reasons are visible:

**C1. `assign_group_dirs` (`scan.rs:1249`) allocates a `String` per ancestor
per file.** `dir_prefixes` builds a fresh owned string for each level, they
go into `dir_chains: Vec<Vec<String>>`, and each is cloned again as a
`HashMap` key. At an average depth of ~5 that is ~25 million allocations and
~25 million owned-string hashes per scan. Every one of those prefixes is a
*prefix of `rel_path`* — `&rel_path[..end]` is the same bytes with no
allocation, and `HashMap<&str, (u32, u32)>` hashes the same. This is a
contained change to one function with an existing test.

**C2. `rel_path.to_lowercase()` per file in `analyze_game_cancellable`
(`langdetect/mod.rs:157`)** — 4.9 M allocations, discarded immediately, to
answer `.ends_with(".resources.dll")`. A case-insensitive suffix compare on
the last 15 bytes.

**C3. `RuleEngine::classify` (`rules.rs:346`) is 33 regexes per file.**
Per call it also allocates a `Vec<&str>` of segments and a lowercased `String`
for the extension. Two independent fixes: bucket the rules by extension once
at load time so a file only tests rules that could match it (most rules carry
an `extensions` filter), and/or fold the file-name rules into one `RegexSet`.
162 million `is_match` calls is the ceiling being paid today.

**C4. langdetect tokenizes files it then discards.** `seg_lists` and
`occ_lists` are built for *every* entry, then `.dll`/`.exe` files (a large
share of a game tree) are skipped at the decision step. The family heuristic
needs game-wide context, so this needs care — but the skip test is cheap and
knowable before the expensive part.

### D. Safety snapshots — 720 k captures, two file opens each

**D1. `identity()` (`safety.rs:190`) opens each target twice.**
`fs::symlink_metadata` first, then `CreateFileW` +
`GetFileInformationByHandle`. The handle's `BY_HANDLE_FILE_INFORMATION`
already carries `dwFileAttributes` (reparse point *and* directory bits) and
the size — everything the metadata call is consulted for. Dropping it halves
the syscalls on the code's own measured ~28 µs per capture; against 720 k
findings that is ~10 s of CPU, spread over six threads.

**D2. The MFT already knows.** 1 570 of 1 603 games route through the MFT,
whose records carry the file reference number, `$STANDARD_INFORMATION`
timestamps, attributes and sizes — the whole of `FileIdentity` except the
volume serial, which is per-volume. For MFT-routed games the scan-time
snapshot could be built with **zero** filesystem calls. The delete-time
validation re-walks the chain uncached regardless (`validate_delete_plan`),
so the safety contract is untouched; what changes is only where the
point-in-time record comes from. Bigger change, biggest single saving in
this section.

### E. Stop writing 4.2 million rows nobody reads — ~30 s/run, ~700 MB

The structural one. Restricting `files` to flagged files (720 k rows) turns
12 s of insert into under 2 s, 10 s of prune into under 2 s, and an 850 MB
database into something like 150 MB — which also makes every other number on
this page smaller, including the WAL and the checkpoint.

The cost is the rule-import impact preview, the only consumer of the full
inventory. Three ways out, in increasing order of ambition:

1. **Keep the inventory, change its shape.** One row per game holding the
   path+size list as a compressed blob: 1 601 inserts instead of 4.9 M, and
   ~290 MB of path text compresses hard. The preview decompresses per game
   and classifies exactly as it does now.
2. **Narrow the preview.** Answer it against the flagged set plus a bounded
   sample, and say so in the dialog.
3. **Drop the full-fidelity preview** and require a rescan to see the impact.

(1) keeps the feature at roughly its current usefulness and is the honest
recommendation; (3) is not, unless the preview turns out to be unused.

### F. Housekeeping ordering

The baseline doc's conclusion holds and is now measured: the previous
generation is deleted *after* the new one is fully written, so the database
carries two full generations and a WAL of the same order through the most
expensive part of the run. Dropping the superseded generation before staging
the new one costs the same ~12 s but pays it against a half-size file, and
halves the peak WAL. Independent of, and additive with, (E).

## Not recommended

- **`synchronous=OFF`.** Buys little on top of WAL + `NORMAL` and trades a
  corruptible database for it.
- **Parallel database writers.** SQLite has one writer per file; more
  connections buys contention. The writer's problem is how much it is asked
  to write, which is (A) and (E).
- **Turning foreign keys off around the prune.** Measured at ~1.3 s, and it
  would be done on a theory the measurement refutes.
- **Trimming `file_safety`'s redundant text** (82 MB of `trusted_root` +
  `rel_path` + `evidence_library_path`, of which `rel_path` duplicates
  `files.rel_path` exactly). It is deliberately self-contained evidence; the
  2.4 s it costs is not worth reopening that contract for.

## Suggested order

1. **A1-A5** — ~9 s, contained diffs, no behaviour change. Do these first
   and re-measure before anything else, so the later numbers are honest.
2. **C1, C2** — allocation removal in two functions, both covered by
   existing tests.
3. **F** — reorder the prune. One change, no new logic.
4. **D1** — one syscall removed from a hot path.
5. **B** — phase overlap. Own change, own testing.
6. **C3, D2, E** — each is a design change with a real decision behind it.

A run that lands 1-4 should come in around 85-90 s on the reference
workload. B brings that toward 60. E is what makes the number stop being
about the database at all.

---

# What actually happened — results, 2026-08-15/16

Written after implementing the first two rounds. Read this before trusting
the rankings above: two of them were wrong, and the reasons are worth more
than the numbers.

## Round 1 — A1, A2, C1, C2, D1 (commits 351cae6, f132c25, f4e3433)

    before  111.1 s (scan 33.4, analyze 70.6, housekeeping 7.1)
    after    95.1 s (scan 33.3, analyze 55.8, housekeeping 6.0)

**−16.0 s**, all of it in analyze. Held constant: same 720623 findings, same
routing, same empty-database start, both warm.

Isolated, A1+A2 measure 8.4 s; in place they returned closer to 15 s. **An
isolated query timing is a floor for this pipeline, not an estimate** — under
load there is a growing WAL and per-batch commits for it to slow down. Scale
later estimates accordingly.

## Round 2 — A3 + measure B (uncommitted at time of writing)

    before  95.1 s (scan 33.3, analyze 55.8, housekeeping 6.0)
    after   93.1 s (scan 34.8 / analyze 86.1 overlapping, housekeeping 6.6)

**−2.0 s, and all of it was A3.** Measure B — overlapping the file-table read
with classification — returned **nothing** on this machine.

### Why B returned nothing, and it is not a bug

The library's layout, from the active generation:

| Volume | Games | Files | Route | Physical disk |
|---|---|---|---|---|
| F: | 1501 | 4 573 997 | MFT | #3, 24 TB HDD |
| H: | 69 | 172 918 | MFT | #1, 8 TB HDD |
| G: | 30 | 168 408 | walkdir | SSD |
| D: | 3 | 7 102 | walkdir | SSD |

**One volume holds 94 % of the games and 93 % of the files.** MFT path
reconstruction cannot yield a single path until that volume's whole `$MFT`
has been parsed — the parent-FRN chain needs the complete map, and a parent
record may appear anywhere later in the stream. So the pool gets nothing from
F: until F: is finished, which is nearly the whole read. Only H: and the two
walkdir volumes overlapped: ~6 % of the work.

Overlap pays only where there are **both** several volumes **and** substantial
per-volume analysis. That is a narrower band than the ranking above assumed.

### The same evidence deflates parallel volume reads on this machine

Reading F: and H: concurrently saves the smaller of the two, and H: is 3.5 %
of the files. The scan phase here is bounded by one disk, and no amount of
cross-device parallelism changes that.

This does **not** retire the idea. The owner's correction stands and is the
reason it stays on the list: MFT read cost is proportional to a volume's
*total file count*, not to the games on it, so someone with three disks and
thirty games pays three full reads while their analyze phase is trivial —
the exact inverse of this machine. It does mean the measure cannot be
validated on this library, and must be justified and tested synthetically.

**The general lesson, which cost two rounds to learn: this library is an
outlier and was known to be one. Do not rank an optimisation by what it does
here without asking what it does on a small library spread over many disks.**

## Revised opportunity list

Ranked for *this* machine; the second table is for everyone else.

| | Measure | Estimate | Confidence |
|---|---|---|---|
| 1 | **(E) stop writing 4.2 M `files` rows** — analyze is still writer-bound (~51 s) and ~85 % of the writer's work is inventory only the rule-import preview reads | ~20-25 s | measured parts (insert 9.9 s, prune 6.3 s, read-back 3.6 s) |
| 2 | **split reading from parsing inside one volume** — `reader::drive_scan` fetches a chunk then parses it in the same loop iteration, so the disk idles during parse and the CPU idles during read. One reader thread feeding a bounded channel overlaps them | ~10 s | estimate only: ~8192 records/chunk at ~2 us against ~32 ms to read 8 MiB. **Instrument read-vs-parse per volume before building anything** | 
| 3 | **(F) drop the superseded generation before staging the new one** — hits *rescans*, the daily path, not first scans | ~10 s on rescans | partly measured (prune 12.4 s) |
| 4 | `canonical_mismatch` calls `dunce::canonicalize` once per game — 1603 filesystem round trips on an HDD before the MFT pass starts | 1-2 s | unmeasured, cheap to check |
| 5 | reuse the 8 MiB chunk buffer instead of `vec![0u8; n]` per chunk | <1 s | trivial |

For other layouts, not this one:

| | Measure | Who it helps |
|---|---|---|
| 6 | parallel volume reads, grouped by **physical device** (`IOCTL_STORAGE_GET_DEVICE_NUMBER`, not drive letter) | many disks, evenly spread games |
| 7 | phase overlap *(done)* | several volumes **and** substantial analysis |

Still gated behind (E) — all of it hides behind the writer today, so all of it
returns ~0 until the writer stops dominating (see GT-112):

| | Measure |
|---|---|
| 8 | `RuleEngine::classify` runs 33 regexes per file; bucket rules by extension or use a `RegexSet` |
| 9 | `file_extension` in `tokens.rs` lowercases per file — the same allocation C2 removed, in a different function |
| 10 | langdetect tokenizes `.dll`/`.exe` entries it then discards |
| 11 | **D2**: for the 1570 MFT-routed games the identity is already in the MFT record — 720 k safety snapshots could cost zero syscalls |

Closed by measurement, do not re-propose: foreign keys off around the prune
(1.3 s), `synchronous=OFF`, a second SQLite writer, trimming `file_safety`'s
duplicated text (2.4 s, and it is deliberately self-contained evidence).

## Round 3 — measure E (commits a24dd5b, 747146b, 3547c88)

    before  93.1 s (scan 34.8 / analyze 86.1 overlapping, housekeeping 6.6)   850 MB
    after   64.9 s (scan 32.9 / analyze 63.1 overlapping, housekeeping 1.5)   379 MB

**−28.2 s**, the largest single win of the three rounds, and it came from
writing less rather than writing faster. `files` went from 4 914 918 rows to
720 618. The owner's call on the trade-off was option (в) — remove the
rule-import impact preview outright — not (а): "порівняння показувати,
звісно, цікаво, але не ціною тримання такої купи лахміття в базі".

Cumulative: **111.1 s → 64.9 s**, database 850 MB → 379 MB.

### The regression this shipped, and why the tests did not catch it

`occupied_by_library` summed `files.size_on_disk`. That was correct while
`files` meant "every file"; the moment it meant "flagged files", the same
query started reporting what the *findings* occupy. A 23 TB library showed
744 GB, and the "selected frees N %" line beneath it read 99.9 %, being a
number divided by itself. Found by the owner looking at the window, after a
run reported 927 green tests.

The same mistake existed in the diagnostic bundle's `COUNT(*) FROM files`.
That one *was* found and fixed while making the change — and finding it did
not prompt a search for others of the same shape, which is the actual error.

**When a table's meaning changes, grep every aggregate over it, not just the
one that comes to mind.** `JOIN files ... ON f.id = fi.file_id` consumers are
safe by construction; `COUNT(*)`/`SUM(...)` over `files` are not, and there
were two.

Both are now pinned by tests written against the specific defect rather than
against the feature in general (`occupied_by_library_counts_unflagged_files_too`,
`only_flagged_files_get_a_row_while_the_game_totals_cover_everything`), and
two older fixtures were rewritten because they set up `files` rows without
per-game totals — they encoded the pre-change data model and so could not
have failed.

A second, smaller one from the same round: the bottom bar still printed
"Scan 32s · Analysis 1:03 · Total 1:04" after the phases began to overlap.
Also caught by eye. Pinned in the UI harness now.

### What is left, re-ranked after round 3

The writer is no longer the dominant cost, which promotes everything that was
previously hidden behind it. GT-112's "optimising classification is pointless
while this holds" no longer holds.

| | Measure | Estimate | Confidence |
|---|---|---|---|
| 1 | **split reading from parsing inside one volume** — `reader::drive_scan` fetches a chunk then parses it in the same loop iteration; disk idles during parse, CPU during read. One reader thread, bounded channel | ~10 s | estimate; **instrument read-vs-parse per volume first** |
| 2 | **(F) drop the superseded generation before staging the new one** — hits rescans, the daily path. Cheaper than it was (a quarter of the rows) but still ordered wrong | a few s on rescans | partly measured |
| 3 | `RuleEngine::classify` runs 33 regexes per file; bucket by extension or use a `RegexSet` | now visible | reasoned |
| 4 | `file_extension` in `tokens.rs` lowercases per file | now visible | reasoned |
| 5 | langdetect tokenizes `.dll`/`.exe` entries it discards | now visible | reasoned |
| 6 | **D2**: identity for the 1570 MFT-routed games is already in the MFT record — 720 k safety snapshots could cost zero syscalls | now visible | reasoned |
| 7 | `canonical_mismatch` — 1603 `dunce::canonicalize` calls before the MFT pass | 1-2 s | unmeasured |
| 8 | reuse the 8 MiB chunk buffer | <1 s | trivial |
| 9 | parallel volume reads by physical device — **for other layouts**, untestable here | — | see above |

Items 3-6 are worth measuring before building: they were ranked as "hidden
behind the writer" when the writer was 93 % of the phase, and nobody has
measured them since it stopped being.

## Round 4 — the measurement itself (2026-08-16)

Items 3-6 were ranked by reading code. Before building any of them, the
pipeline got stage stopwatches (`gametrimmer_core::perf`): ten spans, timed
once per game or once per 8 MiB `$MFT` chunk, never per file, so the
instrument costs a few thousand clock reads per scan and can stay switched on
for good. Every later round starts from data instead of from a re-reading of
the same code.

Baseline run, same workload as every run in the baseline document
(14 libraries, 1603 games, 720623 findings, empty database, warm cache),
**70.7 s total**, six workers:

    tokenize     40.2 s (22 %)      lang_decide  19.4 s (11 %)
    safety       37.7 s (20 %)      family       14.9 s ( 8 %)
    persist      22.1 s (12 %)      rules        11.4 s ( 6 %)
    mft_read     21.0 s (11 %)      occurrences  10.4 s ( 6 %)
                                    mft_parse     3.8 s ( 2 %)
                                    grouping      3.1 s ( 2 %)

Thread time summed over workers (183.9 s against a 68.4 s analyze window),
so read it as proportions.

### What the ranking got wrong

**Localization detection is 46 % of all CPU** — tokenize + occurrences +
family + lang_decide = 84.9 s of 183.9 — and it appears in the round-3 table
only as item 5, a footnote about wasted work on `.dll` files. The single
biggest span is `tokenize_path`, which builds an owned `String` per token per
segment per file, 4.9 M times.

**Item 1's ceiling is 3.8 s, not ~10 s.** Parsing is 3.8 s against 21.0 s of
reading, so a reader/parser split can hide at most the smaller of the two.
The 3.8 s are real wall clock - the MFT-routed games cannot start
classification until F: is read to the end - but the measure is a sixth of
what it was sold as.

**Items 3 and 4 are worth ~6 %.** The 33 regexes and the per-file
`to_lowercase` measure 11.4 s of thread time, under 2 s of wall clock at six
workers. They were ranked third and fourth; they belong last.

**Item 6 (D2) is confirmed and is now the biggest single item** - see below,
where it stopped being a CPU question at all.

### The 12-worker experiment: refuted, and instructive

`SCAN_THREADS` was 6, chosen when the workers walked directories and the
work was IO-bound. With 1570 of 1603 games arriving as ready-made file lists
that premise looked stale, so the pool was widened to `available_parallelism()`
(12 on this 16-thread machine) and measured.

    6 workers    70.7 s      safety  37.7 s
    12 workers  136.8 s      safety 617.0 s

**Nearly twice as slow.** Every genuinely CPU-bound span measured the same at
either width (tokenize 40.2 / 40.2, rules 11.4 / 11.3, family 14.9 / 14.6) -
which is the tell. All of the loss is in `safety`: 52 us per `CreateFileW` at
six workers, 857 us at twelve. It is not CPU work at all. It is 720 k
random-access metadata opens against the same mechanical volume whose `$MFT`
was just read sequentially, and twelve queues into one set of heads is a seek
storm.

Reverted. The constant is not stale-because-IO-bound after all; it is small
because a fifth of the analyze phase is still disk seeks, and it should not
be re-tested until those seeks are gone.

### What is left, re-ranked after round 4

| | Measure | Measured cost | Note |
|---|---|---|---|
| 1 | **D2 — build safety identity from the MFT records already parsed**, for the 1570 MFT-routed games | 37.7 s thread time, ~6 s wall | and it is what pins the pool at six; removing it unblocks item 4 |
| 2 | **`tokenize_path` allocates per token** — owned `String` per piece per segment per file | 40.2 s (22 %) | borrow from the path, lowercase only when a lookup needs it |
| 3 | **skip `.dll`/`.exe` before tokenizing, not after** (old item 5) | part of the 40.2 s | must not change `family`'s sibling evidence — the corpus is the check |
| 4 | **re-test the pool width** once safety no longer seeks | unknown | only meaningful after item 1 |
| 5 | (F) drop the superseded generation before staging the new one | rescans only | unchanged from round 3 |
| 6 | split MFT reading from parsing | ≤3.8 s | ceiling now measured, was estimated at ~10 s |
| 7 | `RuleEngine::classify`'s 33 regexes; `file_extension`'s lowercase | 11.4 s (6 %) | was ranked 3rd and 4th |
| 8 | `canonical_mismatch`, chunk-buffer reuse, parallel volume reads | — | unchanged from round 3 |

The persistent lesson, third round running: **a stage ranked by reading code
has been wrong about its size every single time it was finally measured** -
the writer in round 2, the four classification items here, and my own
12-worker hypothesis in the same afternoon.

## Round 5 — D2's verification gate (2026-08-16)

Before replacing the per-file `CreateFileW` with data from the `$MFT`, the
two had to be proven to agree. `examples/mft_identity_check.rs` compares
them field by field on real files; `crates/core/src/mftscan/` now keeps the
sequence number, the raw NT timestamp and the attribute bits it used to
discard, and `FileEntry` carries an `Option<MftIdentity>` built from them.

**Two runs, and the difference between them is the lesson.**

| Sample | Files | Result |
|---|---|---|
| Far Cry 6 | 113 | every field agreed on every file |
| whole `steamapps\common` | 50 000 | one file disagreed |

    Green Hell\...\mono-2.0-bdwgc.dll: mft 0x42020 live 0x2020 (diff 0x40000)

Bit `0x40000` is NTFS's own "has extended attributes" marker, which Win32
does not report. `FileIdentity` is compared whole at delete time, so that
file - and roughly fourteen per scan of this library - would have blocked
with `TargetChanged` permanently and inexplicably. Masked in
`NTFS_INTERNAL_ATTRIBUTES`, together with the directory and index-view
markers that directories would have carried had any reached this code.

Everything else agreed on all 50 000: `file_index` (so `sequence << 48 | frn`
is the composition Win32 reports), `size`, `last_write_time`, `kind`,
`volume_serial`, and the reparse bit the contract actually gates on.

**The cost being removed is worse than the profile said.** The stage
breakdown measured `safety` at 39 us per finding. The same call cost
1 420 us over one game and **17 798 us** across the library - the same
`CreateFileW`, 450x apart, and the only variable is how the files are laid
out relative to each other. 28.2 s is this library's lucky case, not a
ceiling.

### What is left of D2

The plumbing and the proof are in. What remains is the switch itself:
`classify_game` builds each finding's snapshot through
`SnapshotCapture::capture`, which reads the leaf live. It should take the
leaf identity from `FileEntry.mft_identity` when there is one and open the
file when there is not (every walkdir game, and any record missing a field).

**Unchanged, deliberately**: the trusted root, every intermediate directory,
directory tree fingerprints, and the whole of `validate_delete_plan`. Those
are where `RootMissing`, the long-path refusal and junction detection live,
none of which an `$MFT` record reproduces, and the delete-time check must
stay a live read or the TOCTOU guard it exists to be is gone.

## Round 6 - the constraint moved, and most of the backlog died with it (2026-08-16)

D2 landed (`7ce787d`) and the pool widened behind it (`6c40470`): 65.2 s ->
53.9 s. The interesting part is not the 11 s, it is where the run now spends
its time, because it invalidates most of the ranking above.

Take the 13:23 stage line apart. Total pool CPU is 140.2 s; `mft_read`
(22.5) and `mft_parse` (3.9) belong to the reading, `persist` (17.6) runs on
the writer thread. What is left - tokenize, lang_decide, family, rules,
occurrences, safety, grouping - is **96.2 s of classification CPU over 16
workers, or 6.0 s of wall clock.**

The critical path is therefore:

    0 -------------------------- 34.3 s --------- 52.2 s -- 53.9 s
    read every volume's $MFT      | classify 1501 games: 6.0 s |
                                  | write 720623 rows: 17.6 s  | housekeep

F: holds 93 % of the bytes and 94 % of the games, and MFT path
reconstruction yields nothing until that whole table is parsed, so nearly
the entire read is a serial prefix. After it, sixteen workers finish
classifying in about six seconds and then **wait on the single writer for
another eighteen**.

### What this kills

- **`tokenize` allocations.** The largest stage in the log, 38.5 s, and the
  top of the previous backlog. It is now hidden under a writer that takes
  three times as long. Removing all of it would save nothing.
- **Skipping `.dll`/`.exe` before tokenizing** - same reason.
- **Rules regexes, `file_extension` lowercase, `canonical_mismatch`** - same
  reason, and they were 6 % even when the pool mattered.
- **GPU offload of any classification stage.** The whole of classification is
  6.0 s of wall clock behind an 17.6 s writer, so the ceiling on offloading
  *all* of it is zero. Independently of that it is the wrong workload: 4.5 M
  variable-length paths, branchy dictionary lookups, data-dependent control
  flow, and a PCIe round trip that would likely cost more than the six
  seconds it was meant to remove.

### What is actually left

1. **The writer, 17.6 s.** Now the entire exposed tail. `WRITE_BATCH_SIZE`
   is 24; one transaction per 24 games against 720 k rows. Bigger batches,
   statement reuse, and the `synchronous`/WAL settings are all untested here.
   This is the only item whose gain lands on the wall clock one-for-one.
2. **The read, 34.3 s at 224 MB/s.** That is sequential-transfer speed for
   this disk, and 5 GB of `$MFT` has to cross it. Not a software problem
   unless overlapped/unbuffered IO can beat the cache-through path - worth
   one experiment, not a project.
3. **Dropping the superseded generation before staging the new one**
   (measure F). Housekeeping is 1.4 s on an empty database; it was 45.7 s on
   a full one. Untested and still worth testing, because every real rescan
   is the full-database case.

The floor, if the writer became free, is the read plus a few seconds:
roughly 38 s. There is about 15 s of headroom left in this pipeline and one
credible way to reach it.

## Round 7 - measure F, measured (2026-08-16)

F was ranked on a 45.7 s figure from the era when `files` held 4.9 M rows.
The rescan case had never been re-measured since measure E narrowed it, and
measuring it cost one run and no code: 70.5 s against 53.9 s on an empty
database, with 12.1 s of the 16.6 s difference in `activate_scan`.

Pulling that apart on a copy showed the cost was **the commit, not the
deleting** - ~7.7 s of writing back pages freed by 2.16 M rows against
~4.5 s of issuing the deletes. And none of it is work anyone waits for.

So the fix was not F. `f5b40ce` left the generation lifecycle exactly where
it was and simply stopped making the scan wait: activation moves the
pointer, `prune_superseded` runs after the results are reported. **70.5 s ->
60.5 s**, housekeeping 13.5 -> 1.6.

F itself - dropping the superseded generation *before* staging the new one -
would have taken the remaining 6.6 s as well, because the writer would work
against half-sized tables. It is declined, not deferred: it trades a scan
that fails or is cancelled halfway from "the previous results survive" to
"there is nothing at all". Six seconds do not buy that.

### Where the pipeline stands

| | empty database | populated |
|---|---|---|
| now | 53.9 s | 60.5 s |
| floor (read + a few seconds) | ~38 s | ~38 s |

What is left, in order:

1. **The writer.** `persist` measured 17.6 / 20.5 / 26.3 s across three runs -
   the widest spread of any stage, on a single thread, and the entire exposed
   tail after the `$MFT` read finishes. `WRITE_BATCH_SIZE` is 24. Untouched
   and now the only item whose gain lands one-for-one on the wall clock.
2. **The read**, 33-35 s at 224-252 MB/s. Sequential-transfer speed for this
   disk. One experiment with unbuffered/overlapped IO, not a project.
3. Nothing else. Every classification stage is hidden behind the writer -
   see Round 6.

## Round 8 - the writer, and the probe that answered the wrong question (2026-08-16)

`persist` was the last exposed stage: 17.6 / 20.5 / 26.3 s across three runs
of identical work. Three candidates, none verified - the row building (seven
clones and two `format!`s per finding), contention with sixteen workers, or
SQLite itself.

**A Python probe replayed the same inserts against a copy of the real
database** to test the SQLite-side levers before touching any code:

| variant | |
|---|---|
| current: cache 20 MB, batch 24, FK on | 11.4 s |
| cache 256 MB | 11.1 s |
| batch 200 | 11.2 s |
| FK off | 9.9 s |
| one commit, FK off | 7.1 s |

Read at the time as "cache and batch size do nothing, so it must be the Rust
side". **That was the wrong reading of a right measurement.** Batch 24 and
batch 200 are identical because checkpoints fire on WAL *size*, not on commit
count; the single-commit row is the one that mattered, and it says the cost
is checkpointing.

Splitting the writer three ways in the app settled it:

    persist 25.1s = sql 4.6s (18%) + commit 19.1s (76%) + row building 1.3s (5%)

Sixty-seven commits at ~285 ms. `wal_autocheckpoint` defaults to 1000 frames
(~4 MB) and one batch of this scan writes about that, so every batch folded
4 MB of WAL back across a 760 MB file on a mechanical disk - rewriting pages
the next batch would dirty again. `8c8b2c8` defers it to the checkpoint the
scan already ran at the end.

| | before | after |
|---|---|---|
| total (populated database) | 62.3 s | **49.5 s** |
| persist | 25.1 | 7.4 |
| of which commit | 19.1 | 0.76 |
| prune (same connection) | 9.9 | 3.5 |

`sql` and `row building` did not move, which is how one knows nothing else
was touched. The trade is a 730 MB WAL, truncated afterwards, and ~12 s of
checkpointing that happens after the results are on screen.

### Where the pipeline stands now

    0 ------------------------- 35.8 s ------- 47.8 s -- 49.5 s
    read every volume's $MFT     | classify 7.0 s |
                                 | write     7.4 s |  housekeep

The writer and the pool are now the same size, and neither is the
constraint: **the `$MFT` read is 72 % of the run.** The floor is that read
plus about seven seconds, so roughly 41 s against today's 49.5 - and the
remaining eight seconds are split between two stages rather than sitting in
one.

Nothing on the old backlog reaches them. Every classification stage is still
hidden (Round 6), and the read is at the disk's sequential-transfer speed.
The next real gain, if one is wanted, is a faster disk or an experiment with
unbuffered/overlapped volume reads - not another pass over this code.

### The day, end to end

| | empty database | populated |
|---|---|---|
| this morning | 65.2 s | (never measured) |
| tonight | 53.9 s* | **49.5 s** |

\* not re-measured after `8c8b2c8`; the empty-database case has no superseded
generation to write over, so it gains less.
