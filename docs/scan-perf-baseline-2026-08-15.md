# Scan performance baseline — 2026-08-15

Reference figures for a full scan+analyze run, confirmed by the owner as
"as expected". Compare every later build against this before shipping; a
total above two minutes on the same inputs is a regression to investigate,
not a fact of life.

Kept here because the log does not keep it: `gametrimmer.log` retains one
previous generation and `scan_runs` does not survive clearing the database,
so this table is the only durable history of the runs below.

## Current reference run - 2026-08-16, `8c8b2c8`, empty database

`[2026-08-16 14:09:02+03:00]`, elevated.

| | |
|---|---|
| Total | **45.8 s** |
| scan | 33.3 s (inside `analyze`) |
| analyze | 44.6 s |
| housekeeping | 0.9 s |
| prune (after the report) | 0.4 s |
| final checkpoint | ~6 s, WAL 377.9 MB -> 0 |
| `$MFT` throughput | 251 MB/s |

    Writer breakdown (7.8s): sql 5.4s (68%), commit 871ms (11%), rows 1.6s (20%)

Half the WAL of the populated case, which is what it should be: no
superseded generation to write over.

**The CPU stages drifted upward all day and it is not the code.** `tokenize`
went 36.4 -> 36.4 -> 43.9 -> 46.4 s across the afternoon's runs on
byte-identical input, with `lang_decide`, `family` and `rules` rising in the
same proportion - the signature of a machine clocking down after an hour of
back-to-back scans, not of a regression. It does not affect any conclusion
here (every one of those stages is hidden behind the read - see the audit,
Round 6), but it means the later measurements understate the gains rather
than overstating them.

### Both cases, and how far each is from its floor

    0 --------------------- 33.3 s ------ 44.6 s -- 45.8 s
    read every volume's $MFT  | pool   7.5 s |
                              | writer 7.8 s |  housekeep

The read is 73 % of the run. Pool and writer are the same size and neither
is the constraint. Floor is the read plus about eight seconds, so ~41 s;
both the empty and the populated case now sit within four of it.

## Reference run - 2026-08-16, `8c8b2c8`, populated database

`[2026-08-16 14:04:13+03:00]`, elevated, database already holding a
generation - the case a user actually runs.

| | |
|---|---|
| Total | **49.5 s** |
| scan | 35.8 s (inside `analyze`) |
| analyze | 47.8 s |
| housekeeping | 1.4 s |
| prune (after the report) | 3.5 s |
| final checkpoint (after that) | ~12 s, WAL 730 MB -> 0 |
| Workers | 16 |
| `$MFT` throughput | 217 MB/s - a *slow* run by today's spread |

    tokenize 43.9s (30%)  mft_read 23.3s (16%)  lang_decide 21.0s (14%)
    family 16.0s (11%)  rules 12.5s (8%)  occurrences 11.8s (8%)
    persist 7.4s (5%)  mft_parse 4.3s (3%)  safety 3.6s (2%)  grouping 3.4s (2%)

    Writer breakdown (7.4s): sql 5.1s (69%), commit 762ms (10%), rows 1.5s (20%)

Faster on a populated database than this morning's build managed on an empty
one. Note the machine was in its *slow* state for this run - `tokenize` 43.9 s
is the highest figure recorded all day - so the gain is understated.

### The day, one workload throughout (14 libraries, 1603 games, 720623 findings)

| build | empty database | populated |
|---|---|---|
| morning reference | 65.2 s | - |
| `7ce787d` leaf identity from the `$MFT` | 70.1 s* | - |
| `6c40470` full-width pool | 53.9 s | 70.5 s |
| `f5b40ce` prune after the report | - | 60.5 s |
| `8c8b2c8` defer WAL checkpoints | **45.8 s** | **49.5 s** |

\* slow machine state; pairs with the 70.7 s run, not the 65.2 s one.

## The rescan case - 2026-08-16, and it is the one that matters

Every reference run in this file starts from an empty database, which is
**not** what a user does. Measured for the first time since measure E
narrowed `files`:

| | empty database | populated | after `f5b40ce` |
|---|---|---|---|
| total | 53.9 s | 70.5 s | **60.5 s** |
| scan | 34.3 | 35.1 | 33.0 |
| analyze | 52.2 | 56.6 | 58.5 |
| housekeeping | 1.4 | **13.5** | **1.6** |
| of which activation | 1.0 | 13.1 | 1.3 |
| prune (after the report) | - | - | 10.3 |

The old 45.7 s figure for a rescan's housekeeping did not survive: measure E
had already taken most of it, and what was left was 13.5 s. Pulled apart on
a copy of the real database - one generation, so double for the two a rescan
holds:

| | on the copy | x2 generations |
|---|---|---|
| validation + `pragma_foreign_key_check` | 0.71 s | ~1.5 |
| the `DELETE`s (3 x 720623 rows) | 2.24 s | ~4.5 |
| **`COMMIT`** | **3.86 s** | **~7.7** |
| | | ~13.7 against 13.1 measured |

The commit dominates, not the deleting: freeing 2.16 M rows' worth of pages
costs more to write back than to do. `f5b40ce` moved the whole thing behind
`WorkerMsg::Done`.

**6.6 s of rescan penalty remain**, all of it in `analyze` - writing a
generation into a file that already holds one. That is measure F's remaining
territory and it is being left alone on purpose: dropping the old generation
first would leave a failed or cancelled scan with nothing at all.

## Reference run - 2026-08-16, D2 + full-width pool

`[2026-08-16 13:23:03+03:00]`, release build of `6c40470`, elevated.

| | |
|---|---|
| Total | **53.9 s** |
| scan | 34.3 s (inside `analyze`) |
| analyze | 52.2 s (overlaps `scan`) |
| housekeeping | 1.4 s (of which generation activation 1.0 s) |
| Libraries / games | 14 / 1603 |
| Findings | 720623 |
| Routing | 1570 via MFT, 33 via walkdir |
| Database | empty at start |
| Workers | **16** (`available_parallelism()`) |
| `$MFT` throughput | 224 MB/s |

    tokenize 38.5s (27%)  mft_read 22.5s (16%)  lang_decide 17.7s (13%)
    persist 17.6s (13%)   family 13.8s (10%)  rules 10.7s (8%)
    occurrences 9.9s (7%)  mft_parse 3.9s (3%)  safety 2.9s (2%)
    grouping 2.7s (2%)

**Two changes, one run apart.** `7ce787d` took the finding's leaf identity
from the `$MFT` record instead of reopening the file; `6c40470` then widened
the pool, which the seeks had made impossible.

| | 65.2 s ref | 7ce787d, 6 workers | 6c40470, 16 workers |
|---|---|---|---|
| total | 65.2 | 70.1 | **53.9** |
| scan | 35.2 | 40.9 | 34.3 |
| analyze | 63.5 | 63.8 | **52.2** |
| safety | 28.2 | 10.1 | **2.9** |
| per finding | 39 us | 14 us | **4 us** |

`safety` falling *again* as the pool widened is the inverse of the 12-worker
run below, and the same explanation read backwards: what remains to open is a
few directories shared across each game's findings, small enough to sit in
the metadata cache, so threads no longer queue behind one set of heads.

**Confounded, and worth saying so.** The 13:15 run read the `$MFT` at
210 MB/s and the 13:23 run at 224 MB/s, so the machine was not in the same
state for both. The 16 s between them cannot be split between the pool width
and the machine's mood on one run each. Against the *best* previously
recorded run (64.9 s) the gain is 11 s, which is the number to quote - it is
still twice the documented run-to-run spread.

720623 safety rows, none with a `block_reason`, on both runs.

## Superseded reference run — 2026-08-16, instrumented

`[2026-08-16 08:44:17+03:00]`, release build of the stage-stopwatch work
with `SCAN_THREADS` back at 6, elevated.

| | |
|---|---|
| Total | **65.2 s** |
| scan | 35.2 s (inside `analyze`) |
| analyze | 63.5 s (overlaps `scan`) |
| housekeeping | 1.4 s (of which generation activation 1.0 s) |
| Libraries / games | 14 / 1603 |
| Findings | 720623 |
| Routing | 1570 via MFT, 33 via walkdir |
| Database | empty at start |
| Machine | 63.6 GB RAM (37.6 free), 37.1 GB file cache, 16 logical CPUs, AC |
| `$MFT` throughput | **225 MB/s** — read from the platter, not from cache |

    tokenize 29.9s (20%)  safety 28.2s (19%)  mft_read 22.4s (15%, 225 MB/s)
    persist 16.9s (11%)   lang_decide 15.1s (10%)  family 11.9s (8%)
    rules 8.8s (6%)  occurrences 8.1s (5%)  mft_parse 3.9s (3%)  grouping 2.3s (2%)

Confirms the 64.9 s reference below (same conditions, +0.3 s) and supersedes
it as the run to compare against, because this one records the conditions
instead of leaving them to be assumed.

**The `$MFT` is cold on every run**, and not for want of memory: 37 GB of
file cache, 5 GB of `$MFT`, and the volume handle is opened *with* caching
(no `FILE_FLAG_NO_BUFFERING` — see `mftscan::volume`). Nothing here is worth
tuning; it means a second scan cannot be assumed cheaper than the first, and
the 33-35 s read is a fixed cost of this library.

**Run-to-run spread is larger than most optimisations.** The 08:09 and 08:44
runs are the same code path at the same worker count, and every CPU stage
differs by about a quarter — tokenize 40.2 vs 29.9, safety 37.7 vs 28.2,
rules 11.4 vs 8.8 — for 70.7 s against 65.2 s. Nothing in the app explains
it; something else on the machine was taking cycles. A measure worth
shipping has to beat that noise, which means **one run is not a result** for
anything under ~5 s.

## Instrumented runs — 2026-08-16

Two runs on the same workload as every entry below, both with the stage
stopwatches added (`gametrimmer_core::perf`, logged on the line after
`Scan done in`). The instrument times once per game and once per `$MFT`
chunk, so it cannot account for a difference of seconds.

| Run | Workers | Total | safety | tokenize | persist | mft_read |
|---|---|---|---|---|---|---|
| 08:09 | 6 | **70.7 s** | 37.7 s | 40.2 s | 22.1 s | 21.0 s |
| 08:23 | 12 | 136.8 s | **617.0 s** | 40.2 s | 22.5 s | 19.2 s |

Stage figures are thread time summed over the pool, so they exceed the wall
clock; compare them against each other, not against `total`.

The 12-worker run is the refutation of "the pool is stale, the work is CPU
now": every CPU span is unchanged and the whole loss is `safety` - 720 k
`CreateFileW` calls going from 52 us to 857 us each as twelve queues hit one
mechanical volume. See the audit, "Round 4".

The 70.7 s run sits 5.8 s above the 64.9 s reference and 5.5 s above the
65.2 s one, at identical inputs and the same worker count. Nothing in the
instrument explains it - see "run-to-run spread" above.

## Previous reference run

`[2026-08-16 07:35:25+03:00]`, release build of `747146b` (measure E: `files`
narrowed to the flagged subset, rule-import preview removed), elevated.

| | |
|---|---|
| Total | **64.9 s** |
| scan | 32.9 s (inside `analyze`) |
| analyze | 63.1 s (overlaps `scan`) |
| housekeeping | 1.5 s (of which generation activation 1.0 s) |
| Libraries / games | 14 / 1603 |
| Findings | 720623 |
| Routing | 1570 via MFT, 33 via walkdir |
| Database | **379 MB** (was 850 MB) |
| OS file cache | warm |

**−28.2 s against the 93.1 s run.** `files` went from 4914918 rows to 720618,
so the single writer thread - the serialisation point of the whole analyze
phase - has roughly a seventh of the rows to insert. Housekeeping is a
quarter of what it was for the same reason: the next scan has a quarter of
the rows to delete.

Occupancy is now summed from `games.bytes_on_disk` rather than from `files`;
the run above reports 25584 GB, which is the library. A run reporting a total
close to what the *findings* occupy (roughly 750 GB here) means that
regression is back - see the commit message on `747146b`.

The whole arc, same workload throughout - 14 libraries, 1603 games, 720623
findings, empty database, warm cache:

| Build | Total | Note |
|---|---|---|
| `e0869d2` + timing work | 111.1 s | the starting point |
| `f4e3433` | 95.1 s | A1, A2, C1, C2, D1 |
| `d1c786e` | 93.1 s | A3 + phase overlap (overlap contributed nothing here) |
| `747146b` | **64.9 s** | measure E |

## Previous reference run

`[2026-08-15 23:49:44+03:00]`, release build of `f4e3433` + uncommitted A3
(drop `idx_files_scan_id`) and measure B (phase overlap), running elevated.

| | |
|---|---|
| Total | **93.1 s** |
| scan | 34.8 s | 
| analyze | 86.1 s (overlaps `scan` — see below) |
| housekeeping | 6.6 s (of which generation activation 3.8 s) |
| Libraries / games | 14 / 1603 |
| Findings | 720623 |
| Routing | 1570 via MFT, 33 via walkdir |
| Database | empty at start (`scan 1`) |
| OS file cache | warm |

**−2.0 s against the 11:33 run, and all of it was A3.** Measure B returned
nothing here, for a structural reason worth remembering before ranking any
future scan-phase work:

| Volume | Games | Files | Route |
|---|---|---|---|
| F: | 1501 | 4 573 997 | MFT |
| H: | 69 | 172 918 | MFT |
| G: | 30 | 168 408 | walkdir |
| D: | 3 | 7 102 | walkdir |

**One volume holds 94 % of the games.** MFT path reconstruction cannot yield
a single path until that volume's whole `$MFT` is parsed, so the pool gets
nothing from F: until F: is done — which is nearly the whole read. Only 6 %
of the work had anything to overlap with. F: and H: are separate physical
disks (#3 and #1), so parallel volume reads would work here mechanically and
still gain almost nothing, because F: is 93 % of the bytes.

Neither measure is thereby wrong; both are simply un-testable on this
library. See `scan-optimisation-audit-2026-08-15.md`, "What actually
happened".

## Previous reference run

`[2026-08-15 11:33:55+03:00]`, release build of `f4e3433` (GT-131's first
measure: A1, A2, C1, C2, D1), running elevated.

| | |
|---|---|
| Total | **95.1 s** |
| scan | 33.3 s |
| analyze | 55.8 s |
| housekeeping | 6.0 s (of which generation activation 2.6 s) |
| Libraries / games | 14 / 1603 |
| Findings | 720623 |
| Routing | 1570 via MFT, 33 via walkdir |
| Database | empty at start (`scan 1`) |
| OS file cache | warm |

Held constant against the previous reference below: same workload to the
finding, same routing, same empty-database start, both warm. The whole
16.0 s is in `analyze` (14.8 s) and `housekeeping` (1.1 s); `scan` is
unchanged at 33.3 s, which is right - that measure touched nothing in it.

Worth keeping in mind when judging the next one: the two redundant writer
passes measure 8.4 s in isolation on this database, and removing them
returned closer to 15 s in place. An isolated query timing is a floor for
this pipeline, not an estimate.

## Previous reference run

`[2026-08-15 10:32:53+03:00]`, release build of `e0869d2` + uncommitted
GT-61/timing work, running elevated.

| | |
|---|---|
| Total | **111.1 s** |
| scan | 33.4 s |
| analyze | 70.6 s |
| housekeeping | 7.1 s (of which generation activation 2.9 s) |
| Libraries / games | 14 / 1603 |
| Findings | 720623 (+16 orphans) |
| Routing | 1570 via MFT, 33 via walkdir |
| Database | empty at start (`scan 1`) |
| OS file cache | warm |

Secondary reference, same build, one library excluded (GT-61):
13 libraries / 551 games, 96206 findings — **52.0 s**
(scan 30.2, analyze 20.0, housekeeping 1.9).

## What the earlier runs showed

| Run | Database at start | Cache | scan | analyze | housekeeping | Total |
|---|---|---|---|---|---|---|
| 00:26 | empty | cold | 68.9 | 277.1 | 6.8 | 352.8 s |
| 09:57 | holds previous generation | warm | 54.5 | 167.2 | 45.7 | 267.4 s |
| 10:32 | empty | warm | 33.4 | 70.6 | 7.1 | **111.1 s** |

Identical workload in all three: 1603 games, 720623 findings.

The 09:57 and 10:32 runs hold workload and cache roughly constant and differ
only in whether the database already held a generation. That run costs
2.4x the analyze time and 6x the housekeeping. Both are explainable:

- **analyze** writes a second generation into a file that already carries
  one, against larger tables and a growing WAL.
- **housekeeping** is dominated by `activate_scan`, which validates the new
  generation, runs `pragma_foreign_key_check` across the *whole* database,
  and deletes the superseded generation's ~720k rows in one transaction.

This is the cost the "rebuild everything from scratch" decision pays on
every rescan — see `scoped-scan-design-2026-08-15.md`. It does not change
that decision: 111 s is the floor and it is comfortable. It does mean the
worthwhile optimisation is not the scan itself but dropping the previous
generation *before* the new one is written, rather than after.

## Reading a run from measure B onwards

Measure B (audit item B, "overlap the two phases") changed what two of these
numbers mean, so a run from before it and a run from after it are not
column-for-column comparable. The `Scan done in` line now says as much in
words; this is how to line the two up.

Before: every eligible volume's `$MFT` was read to completion first
(`scan`), and only then was anything classified (`analyze`). The two
partitioned the run, so `scan + analyze + housekeeping = total`.

After: the reading runs *underneath* the classification. Both spans start
when the libraries have been written; `scan` ends when the last volume has
been read, `analyze` when the writer thread joins. They overlap, and their
sum now exceeds the total by however much of the reading was hidden.

| Figure | Comparable across the change? |
|---|---|
| `total` | **Yes, directly.** Still the number to judge a build by. |
| `scan` | **Yes, directly.** Same measurement — discovery + library persist + reading every eligible volume. A change here is still a change in IO cost. |
| `analyze` | No. It now contains the reading it overlaps. Compare it against the old `scan + analyze` less the discovery+persist part, or simply read it as "the whole overlapped window" and judge by `total`. |
| `housekeeping` | Yes. Still everything after the writer joins, and now measured directly instead of inferred by subtraction. |

Against the 11:33 reference (scan 33.3, analyze 55.8, total 95.1), the same
workload perfectly overlapped should report roughly `scan 33`,
`analyze 56-60`, `total 62-66` — the same two phase costs, minus the 33 s
that no longer waits its turn. A `total` still near 95 with a `scan` near 33
means the overlap is not happening (the reading finishes before the pool has
anything to do with it), not that the machine got slower.

## Method

Read the `Scan done in` line from `gametrimmer.log` beside the executable.
Compare warm against warm, and note whether the database was empty — a
comparison that ignores either condition will find a regression that is not
there, or miss one that is.
