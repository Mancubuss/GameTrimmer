# GT-83 spike: what a diagnostic bundle must contain, and what shape it takes

- Research date: 2026-08-13
- Branch: `master`, commit `111cce0`
- Mode: source reading and external research only. The app was not run, no file
  was written outside `docs/`, and no measurement was taken on a live machine.
  Behavioural claims about Win32 outcomes are inferences from the call sites,
  and are marked as such.
- Questions from the card: which problem classes will have to be diagnosed
  remotely, what evidence discriminates between their causes, what the minimum
  sufficient field set is, what the privacy rules are, and what the container
  looks like.
- Companion document: [telemetry-spike-2026-08-13.md](telemetry-spike-2026-08-13.md)
  (GT-22 — how, or whether, anything leaves the machine).

## Verdict

**Proceed. The analysis converges on a specific design, and it is not the one
that was withdrawn.**

Four things decide the shape:

1. **The database is already a diagnostic bundle in disguise.** `scan_runs`,
   `scan_library_evidence`, `scan_diagnostics`, `file_safety` and `operations`
   (`crates/core/src/db.rs:10-115`) were built for the deletion-safety
   contract, but they durably answer most "at which stage did it break"
   questions already. Roughly 70 % of the minimum sufficient set is extraction,
   not new instrumentation.
2. **The log is the weak half, and the gap is asymmetric in an embarrassing
   way.** `WorkerMsg::Error` — the fatal message the user actually reads on
   screen — is never written to the log, while `WorkerMsg::Warning` directly
   below it is (`crates/app/src/app.rs:1711-1722`). A user reporting "the scan
   failed with X" currently leaves behind a log whose last line is "Scan
   started".
3. **There is no global panic hook.** Release builds set
   `windows_subsystem = "windows"` (`crates/app/src/main.rs:1`), so stderr goes
   nowhere, and `catch_unwind` covers only three scan-specific sites. A panic
   in the UI thread, the writer thread, the delete path or startup leaves no
   artifact at all. This is the single highest-value change in the report.
4. **Zero telemetry is the norm in this tool's peer group, not an
   achievement.** ShareX states it absolutely; Rufus ships a feature to help
   users *evade* Microsoft's telemetry. A locally generated, user-reviewed
   file that the user hands over themselves is the expected move for this
   category — see the prior-art section and the companion document.

The withdrawn spec ("a ZIP with one CSV") fails on its own terms: the CSV
export (`crates/app/src/export.rs:104-117`) carries no scan-generation id, no
app version, no settings, no routing decisions, no block reasons and no library
attribution. It answers "what did you find", which is the one question a
remote reader can usually already guess.

## What already exists at runtime

Legend: **restart** = survives a process restart, **crash** = survives an
abrupt kill.

| Source | What it holds | Durability | Verdict for the bundle |
|---|---|---|---|
| `gametrimmer.db` — 11 tables, schema v2 | scan generations, per-library evidence, per-provider diagnostics, findings with rule provenance, per-finding safety evidence and block reason, a crash-safe deletion journal | restart + crash | **the primary source**; extract projections, never ship the file |
| `gametrimmer.log` | ~20 distinct events per session, untyped prose, local wall clock with no UTC offset, no severity, no thread, no correlation to `scan_runs.id`, append-only with no rotation | restart + crash | usable only as redacted excerpts |
| `gametrimmer.ini` | 12 keys, forgiving parser that silently falls back per field | restart + crash | **ship the raw bytes**, not the parsed struct — the difference between the two *is* the finding |
| `rules.json` / `l10n_rules.json` | effective rule packs; user edits and imported packs win permanently | restart + crash | hash and count only; see gap G11 |
| Memory-only | per-root routing reasons, per-volume probe results, `ScanTiming`, per-game `ScanStats`, `SpaceTally`, every `WorkerMsg` | **lost on exit** | the bulk of the new instrumentation |
| CSV export | 12 columns per finding | user-initiated | closest thing to a bundle today, and insufficient |
| CLI text report | the best-shaped artifact that exists | — | compiled out of release builds (`crates/app/src/cli/mod.rs:8-13`), so no user can produce it |

Environment facts are thin: elevation and app version are available; Windows
build number, free disk space, locale, filesystem type and CPU count are not
probed anywhere.

## Coverage matrix

Columns: symptom → hypotheses → discriminating evidence → source (**E** exists,
**N** new) → mandatory/optional.

### (a) Launcher and library detection

| Symptom | Hypotheses | Discriminating evidence | Src | M/O |
|---|---|---|---|---|
| "My game isn't in the list" | not installed / manifest unreadable / install dir unreadable / deduped as nested / library never registered | per-provider `DiscoveryStatus` including `NotInstalled`; `scan_diagnostics` rows with `stage` (`libraryfolders-read`, `manifest-parse`, `game-path`, …); game counts before and after dedupe | E + **N** for `NotInstalled` and the dedupe counts | M |
| "Wrong launcher shown" | two providers claimed the same root and the first won / stale `manual` vendor floor / folderscan claimed it first | `game_libraries.vendor` + `path`; the per-provider `(vendor, root, count)` list *before* merge | E + **N** for the pre-merge list | M |
| "It found a folder that isn't a game" | folderscan heuristic / orphan residue misclassified | `vendor = 'folderscan'`; `category = 'orphan_folder'`; `scan_library_evidence.status = 'heuristic'` | E | M |
| "My Steam library is degraded" | one malformed `appmanifest` / unreadable `libraryfolders.vdf` / one unreadable game folder | `scan_diagnostics.stage` + `path` + `message` — the stages already separate these causes | E | M |
| Manifest-backed game silently absent | install dir genuinely missing (paused download) | needs a `stage: "game-absent"` diagnostic or a manifests-read vs games-kept count | **N** — `steam.rs:154` records nothing | O |

### (b) Scan routing, elevation, permissions

| Symptom | Hypotheses | Discriminating evidence | Src | M/O |
|---|---|---|---|---|
| "Prefer MFT changed nothing" | not elevated / SSD routed away on purpose / no drive letter / junction / volume open failed / MFT errored / MFT empty on a non-empty root / `ForceWalkdir` | `WalkdirReason` **per root**, plus the routing setting and elevation | **N** — computed at `scan.rs:684-693`, reduced to a localized count string one function later, lost on exit | M |
| "It asks for admin every start" | all volumes read as SSD / `ForceWalkdir` / no lettered libraries | per-drive `MediaKind` + routing setting | **N** — media kind probed twice, never recorded | M |
| "MFT fails on drive X" | not NTFS / ACL / filter driver / `ntfs` crate panic | the Win32 error text from the raw open; the panic payload | **N** — `is_available` collapses a descriptive error to `bool` (`mftscan/volume.rs:96-98`) | M |
| "Access denied on some folders" | per-folder DACL / cloud placeholder / file in use | the first failing path and message — it aborts the whole game scan by design | E (`scanner.rs:107-113`) | M |

### (c) Hangs, cancellation, performance

| Symptom | Hypotheses | Discriminating evidence | Src | M/O |
|---|---|---|---|---|
| "It froze at N %" | one huge game still walking / MFT streaming / writer blocked / deadlock | last progress verb + item at the moment of failure | **N** — a persisted breadcrumb | M |
| "It took 40 minutes" | HDD + walkdir / huge library / rules pass / DB commits | `ScanTiming` split, game and file counts, MFT-vs-walkdir split | E in memory, `scan_runs` timestamps durable; per-game `ScanStats` **discarded** at `persistence.rs:304` | M |
| "Stop did nothing" | cancel arrived mid-walk / after commit / run already finished | `scan_runs.state`; the three `Scan cancelled` sites currently log identical text | E + **N** to distinguish the sites | O |
| "Slower every time" | generations not pruned / WAL not checkpointed / DB bloat | `page_count`, `freelist_count`, `journal_mode`, `scan_runs` counts by state | E, queryable, never captured → **N** to snapshot | M |

### (d) Missed and false findings, rule versions

| Symptom | Hypotheses | Discriminating evidence | Src | M/O |
|---|---|---|---|---|
| "It flagged a file it shouldn't" | rule too broad / imported rule / localization detector / folder collapse | category, rule desc, confidence, provenance, lang tag, group dir — all persisted | E | M |
| "…but which `rules.json`?" | shipped / user-edited / imported | a hash and rule count of the effective pack | **N** — the format carries **no version at all**, unlike `l10n_rules.json` | M |
| "It missed obvious junk" | category disabled / depth limit / extension whitelist / keep-language / never enumerated | settings, rule fields, and the `files` table for "was it seen at all" | E | M |
| "Findings vanished between runs" | really deleted / unknown-category row skipped / finding with no `files` row | load-skip log lines exist; the silent `continue` does not | **N** — `persistence.rs:367-369` drops it with no record | M |

### (e) Database, migrations, locking, corruption

| Symptom | Hypotheses | Discriminating evidence | Src | M/O |
|---|---|---|---|---|
| "Database disk image is malformed" | genuine corruption / truncated file / large uncheckpointed WAL / not a SQLite file | the `SQLITE_CORRUPT` vs `SQLITE_NOTADB` classification; `integrity_check`; `-wal`/`-shm` sidecar presence and size | E for the classification logic, **N** to record it | M |
| "The app won't open my database" | schema newer than build / locked / permission | the explicit "schema version newer than supported" error; `user_version` | E — but the whole startup `db_error` is **never logged** (`app.rs:366-375`) | M |
| "Deletes are blocked / rescan required" | legacy generation / non-active generation / missing safety evidence / degraded discovery | `scan_runs.state`, `active_scan_id`, `block_reason`, `scan_library_evidence.status` — the preflight reasons map 1:1 | E, all durable | M |
| Interrupted scan left staging rows | crash / kill / power loss | `scan_runs.state = 'staging'` at next start | E | M |

### (f) Settings and portability

| Symptom | Hypotheses | Discriminating evidence | Src | M/O |
|---|---|---|---|---|
| "My setting didn't stick" | write failed / value didn't parse / key unknown to this build | raw ini bytes vs parsed settings | **N** as an artifact | M |
| "It made a second database" | exe moved or copied / two copies | the resolved exe/db/ini/log/rules paths | **N**, trivial — catches the class outright | M |
| "Portable on USB behaves oddly" | journal mode fell back to `DELETE` / per-commit syncs / removable media | effective journal mode; removability | **N**; removability not probed at all | O |

### (g) Deletion and Recycle Bin

| Symptom | Hypotheses | Discriminating evidence | Src | M/O |
|---|---|---|---|---|
| "Some files weren't deleted" | in use / permission / identity changed / tree changed / reparse point / root unreachable / stale row | `operations.outcome` + `error` + the block reason | **E and durable** | M |
| "It said 40 GB and my disk didn't change" | Recycle Bin holds it / hard links / over-quota permanent delete | `SpaceTally {expected, freed, recycled_pending}`, per-file `share`/`nuked` | **N** — memory-only today | M |
| "Crashed mid-delete" | intent written but not performed / performed but journal update lost | `status = 'pending'` plus the reconciliation classification | **E — the best-instrumented path in the codebase** | M |

### (h) UI, localization, environment

| Symptom | Hypotheses | Discriminating evidence | Src | M/O |
|---|---|---|---|---|
| "Tofu squares in game names" | fallback font missing | per-font load failures — currently `eprintln!` only, invisible in release | **N** (route through the logger) | M |
| "Blank window / crash on start" | GPU backend failure / panic before first frame / font load failure | **nothing exists** | **N — the panic hook, gap G1** | M |
| "Text is in the wrong language" | preference vs system mismatch / untranslated string | `app_language`, resolved `Lang`, the raw preferred-tag list | partially E; the tag list is discarded after matching | O |

## Minimum sufficient set

Design constraint applied: a remote reader must be able to (i) identify the
build and environment, (ii) place the failure in a phase, (iii) see the
decisions taken at that phase and their inputs, and (iv) see the durable
evidence trail for the destructive path.

**Identity and environment** — app version, elevation, the resolved paths for
exe/db/ini/log/rules, DB `user_version` vs `CURRENT_SCHEMA_VERSION`, effective
`journal_mode`, `l10n_rules.json` version plus a `rules.json` hash and rule
count, the raw ini bytes. All trivial; only the rules hash and the Windows
build number are new, the latter needing a `RtlGetVersion` probe (moderate).

**Scan run** — the `scan_runs` row, per-library evidence, per-provider
diagnostics, aggregate counts, the `ScanTiming` split (all E), plus four new
items: per-root routing decisions, per-volume probe results with the *real*
Win32 error, the fatal error text, and a last-progress breadcrumb.

**Findings** — a *sample*, not the whole set: category, rule desc, confidence,
provenance, lang tag, group dir, sizes, block reason, library attribution. All
existing.

**Deletion trail** — `operations` rows and reconciliation classifications (E),
plus `SpaceTally` and the per-file `nuked`/`share` flags (N).

Cheap optional extras: free disk space per volume, the raw preferred-language
tag list, page/freelist counts, `scan_runs` counts by state, WAL sidecar sizes.

## What the bundle must declare undiagnosable

The card asks for honest limits rather than the appearance of coverage. Seven
cases where the code structurally cannot know the answer:

1. **What exists on disk right now.** The bundle is a snapshot of a past scan;
   every safety identity is point-in-time by design. A reader cannot separate
   "the scan was wrong" from "the disk changed since".
2. **Whether a provider was skipped or genuinely absent.** `NotInstalled`
   leaves no row anywhere. Without new instrumentation the bundle can say "no
   Epic library" but not "Epic was checked and is not installed".
3. **Why a rule matched a specific path.** The engine returns the winning
   rule's description, not the competing candidates or the match position.
4. **What a walkdir failure was beyond the first.** The first error aborts the
   game scan deliberately, so a partial file list is never persisted.
5. **Whether a crash was a panic, and where.** Until the panic hook lands, the
   bundle must say "no crash record is available in this build" rather than
   presenting a truncated log as evidence of a clean exit.
6. **Whether the Recycle Bin actually holds the files.** If the bin listing
   failed, the code deliberately claims nothing; the bundle must reproduce that
   abstention rather than defaulting to "recycled".
7. **Timing correlation across artifacts.** Log lines carry local wall clock
   with no timezone while the DB carries Unix seconds. Correlating them on an
   unknown machine is guesswork until the UTC offset is captured.

## Privacy defaults

One rule dominates: **one shared redaction pass, applied last, to every
free-text field after serialization** — paths, error messages, log lines, rule
descriptions. It must match against the live `%USERPROFILE%` value rather than
a regex for `Users\anything`, so it works whatever the account name is and does
not false-positive on a game's own asset tree. This matters because
`rusqlite::Error` and `std::io::Error` embed full paths through `Display`, which
a naive "just log the error" implementation would miss.

| Category | Default |
|---|---|
| Filesystem paths | included, library-root tokenized (`<LIBRARY_1>\...\movie.bik`); the token map stays in memory, never in the file |
| Windows account name | never verbatim — always redacted, no opt-out |
| Machine GUID, volume serials | **never included** — pure cross-file correlators with no diagnostic value here |
| Installed game titles | **opt-in only**, and even then as anonymous slots (`Game 1`, `Game 2`); a library of a few dozen titles is close to a fingerprint |
| File names and sizes for reported findings | included — this is the load-bearing evidence |
| Content hashes | opt-in only |
| Full file-tree listing | opt-in, truncated |
| Timings | included; UTC only, no local offset |
| Error text | included, through the shared redaction pass |
| Settings | included (behavioural preferences; `keep_languages` is the only weak demographic signal) |
| Rule pack version, hash and provenance | included — no privacy cost, high diagnostic value |
| DB schema version and health counters | included |
| Operation journal — aggregates | included |
| Operation journal — row detail | opt-in, path-tokenized |
| Stable per-installation ID | **never** |
| Per-generation random UUID | included in the manifest |

On the identifier question: a *stable* install ID would let two files posted
months apart be linked as the same person, for a benefit nothing in the card
requires. A *per-generation* UUID solves the only real case — the user
generating a "before" and "after" pair in one troubleshooting session — without
being durable.

Two Windows specifics worth stating explicitly. First, never iterate
`env::vars()`: env values carry account, machine and domain names, and in a
corporate setting internal UNC paths. Name any variable that is diagnostically
useful and redact its value like any other string. Second, the Steam and Epic
providers parse manifests that contain Steam64 IDs and Epic account GUIDs; the
bundle must carry only what the provider *derived* (library path, game name,
app/build id — already columns in `games`), never the manifest's own account
fields. That is a narrowing of the projection, not a redaction pass.

## Container format

Seven real support-bundle designs were read for this spike. The recurring
pattern is consistent: the container is an archive, not an opaque blob; a small
sidecar declares the format version; integrity, where present, is a detached
checksum; redaction is name-based against a known field list; and a
human-readable summary is paired with the structured data.

Recommended layout — a zip, always, because a bare JSON file cannot hold a
human-readable summary and a machine-readable payload as two independently
openable artifacts:

```
gametrimmer-diag-<uuid8>.zip
├── manifest.json           # schema version, generation UUID, app version, section list
├── summary.txt             # what a reader opens first; always present, never excludable
├── settings.json
├── rules.json              # pack hash/version + provenance counts
├── db_health.json
├── findings.json           # path-tokenized
├── operations_summary.json
├── operations_detail.json  # opt-in
└── errors.json             # redacted excerpts tied to the reported problem
```

Excluded sections are **absent files, not empty ones**, so the manifest's
declared section list is always exactly the zip's own listing, checkable with
`unzip -l`.

**Manifest fields:** `schema_version`, `generated_at` (UTC), `generation_id`,
`app_version`, `sections_included`, and a fixed `redaction_applied: true` that
documents the pass ran — the same role Home Assistant's `**REDACTED**` marker
plays.

**Version policy**, modelled on Android's `bugreport-format` and consistent
with what this codebase already does: bump only on a breaking field change;
adding an optional field is not breaking, exactly as the ini parser already
treats its own keys. A reader meeting an unknown newer version behaves like
`ensure_supported_schema_version` in `db.rs:217` — refuse to *interpret*
unrecognized structured fields, but never refuse to show `summary.txt`, which
has no schema to be incompatible with.

**No checksum.** sosreport ships a detached `.sha256` because a support
engineer runs a fixed pipeline against the archive. Here the same user who
reviewed the file attaches it by hand; zip's own per-entry CRC32 already
catches copy corruption, and a cryptographic checksum would be maintenance cost
with no consumer.

**Truncation is declared, not silent.** Any opt-in detail list caps at a fixed
row count with `"truncated": true, "total_rows": N, "included_rows": 500` in
the payload itself.

**Expected size:** tens to low hundreds of KB for the always-included sections;
a few MB if full findings and operation detail are opted in. Small enough to
attach to a forum post without hosting — which is itself a design constraint,
since the companion document recommends exactly that delivery route.

## Container and compression level, measured

The format question was settled on real data rather than by preference.
Payloads were built from this machine's actual release database (1 598 games,
4 924 011 files, 720 165 findings, schema v2) using the section layout above,
path-tokenized and account-redacted exactly as the real generator would, and
then compressed with every codec that could plausibly be reached from Rust or
from a shipped tool. Timings are the best of three warm runs on a 16-core
machine; the external tools were re-timed independently from PowerShell to rule
out measurement error.

**Ratios, on the realistic profiles** (raw → compressed, share of raw):

| Profile | raw | zip deflate 6 | zip deflate 9 | 7z LZMA2 mx5 | zstd 19 | bzip2 9 |
|---|---|---|---|---|---|---|
| default, 500 findings | 650 KiB | 83 KB · 12.4 % | 80 KB · 12.1 % | 64 KB · 9.7 % | 67 KB · 10.0 % | 60 KB · 9.0 % |
| 5 000 findings | 2.9 MiB | 191 KB · 6.4 % | 178 KB · 5.9 % | 144 KB · 4.8 % | 135 KB · 4.5 % | 121 KB · 4.1 % |
| 50 000 findings | 26 MiB | 1.29 MB · 4.8 % | 1.19 MB · 4.4 % | 942 KB · 3.5 % | 763 KB · 2.8 % | 659 KB · 2.4 % |
| **every finding** | **300 MiB** | 16.0 MB · 5.3 % | 14.8 MB · 4.9 % | 12.0 MB · 4.0 % | — | — |

The default profile's 650 KiB of raw input is mostly the per-game aggregate
section for 1 598 games, not the 500 sampled findings — which is why its ratio
is the worst of the four: it has the least repetition to exploit.

**Cost of the ratio**, on the 300 MiB upper bound: deflate 6 takes 2.0 s,
deflate 9 takes 4.6 s for 7.7 % less, 7z mx5 takes 5.6 s for 25 % less, and 7z
mx1 takes 0.25 s for a size between the two deflate levels. The data is so
redundant — JSON with the same thirteen keys on every row — that LZMA2's match
finder skips through it at over a gigabyte per second at mx1, which is why the
fast preset is unusually competitive here.

Four findings decide it:

1. **Solid archiving buys nothing.** A solid tar stream and a per-entry zip land
   within 0.01 % of each other at every level (deflate 6: 190 790 vs 190 797
   bytes), because one section dominates the payload. The main structural
   argument for 7z does not apply to this data.
2. **The whole argument is worth ~19 KB at the size that actually occurs.** The
   default bundle is 83 KB with deflate and 64 KB with LZMA2. That difference is
   invisible on a file attached to an issue, and it stays under 400 KB even at
   ten times the sampled detail.
3. **Only deflate is openable everywhere.** Windows Explorer's built-in zip
   handler reads Store and Deflate; a zip whose entries use bzip2 or LZMA looks
   corrupt to a recipient who double-clicks it, which is exactly the wrong
   failure for a support file. 7z needs 7-Zip installed on Windows 10, and zstd
   needs a tool everywhere. The recipient's cost matters as much as ours.
4. **Only deflate is free in the dependency tree.** `zip` + `flate2` are already
   resolvable; LZMA2 from Rust means `sevenz-rust`, and zstd means C bindings
   through `zstd-sys`. Both are real weight for a saving nothing needs.

**Decision: a `.zip` with Deflate at level 6 — the `zip` crate's default — and
no configurable level.** Level 9 is available for 7 % at 2.4× the time and is
not worth a setting; a setting here would only invite the question of which one
to pick. The compression is not the slow part in any case: collecting and
redacting dominates for every realistic bundle.

One consequence for the truncation cap. At 500 findings the bundle is 83 KB
and instant. Letting the user opt into *everything* produces 16 MB and about
20 seconds of work, mostly in generation rather than compression — still under
GitHub's 25 MB attachment limit, but no longer a file anyone reviews before
sending. **Cap the opt-in detail at 50 000 rows** (1.3 MB, well under a second
of compression), declare the truncation in the payload, and do not offer an
unbounded mode.

A last note on where the size actually goes: the payload is ~20× more
compressible than typical text because every row repeats its keys. If size ever
becomes a real constraint, changing the section layout — NDJSON with a shared
header, or columnar arrays — would beat any change of codec by a wide margin.
It is not a constraint today.

## Generation mechanics

`crates/core/src/atomic_file.rs` applies with one adaptation. Its
`atomic_write_with_backup` already gives the property the card needs — stage to
a `.replace-tmp` sibling, `sync_all`, atomic `MoveFileExW` with
`MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`, reopen and re-validate,
roll back from `.bak` on failure — so a cancelled or failed run never leaves a
plausible-looking partial file. The adaptation is that it takes a complete
`&[u8]`: build the whole zip in memory via `ZipWriter<Cursor<Vec<u8>>>` and
hand it over in one call. That keeps the existing guarantee unchanged and fits
comfortably in memory at the sizes above. The `validate` closure should reopen
the bytes as a zip and check that `manifest.json` parses — the same read-back
discipline `settings.rs` already uses for the ini.

Collection, redaction and zip assembly run on a worker thread following the
existing `crates/app/src/worker/` pattern, reporting per-section progress, with
cancellation checked between sections rather than inside them.

**Preview.** The rule that the user sees exactly what the file contains is best
satisfied by rendering the actual generated `summary.txt` in a pane before any
bytes touch disk — not a generic checkbox list. The opt-in toggles sit
alongside it and regenerate it live, so what the user reads is always what they
are about to write. This is a stronger version of what Docker and Home
Assistant *tell* users to do manually after the fact.

## Prior art

| Product | Format | Preview / redaction | Delivery |
|---|---|---|---|
| Home Assistant | per-integration JSON | **strongest found** — framework-level `async_redact_data()`, redacted values show as `**REDACTED**`; docs still tell users to eyeball it | user downloads, attaches to a GitHub issue |
| Android bugreport | zip with `version.txt` + `main-entry.txt` | — | — |
| Red Hat sosreport | `.tar.xz` + detached `.sha256` | scrubs secrets, **but not hostnames or usernames by default** — a separate `sos clean` pass is required | uploaded to a support case |
| Docker Desktop | zip, `{user-id}/{timestamp}` diagnostic ID | docs tell the user to extract and read it before uploading | uploaded, user quotes the ID |
| Syncthing | zip, gated behind an explicit "enable debugging" step | config redacted of usernames, passwords, API keys — device/folder IDs remain and are themselves sensitive | manual |
| JetBrains IDEs | zip | none described; handed to the user via "Show in Explorer" | manual attach |
| DXDiag / MSInfo32 | `.txt` / `.nfo` | none | manual attach — the Windows-native ritual every user already knows |
| OBS Studio | plain-text log upload | **negative example** — no confirmation, no retention statement, no deletion path; filed as a privacy issue | one click, immediate |

Two lessons transfer directly. sosreport shows that strong secret-scrubbing
still ships usernames by default — which is why the account-name pass here is
unconditional rather than a separate cleaning mode. OBS shows that unremarkable
data is still a failure when it is too easy to transmit before looking at it,
which is precisely what the card's fixed rules already prevent.

## Consequences for GameTrimmer

1. **GT-83 gets its implementation spec from this document**: `.zip` container
   at Deflate 6, manifest and summary as above, the minimum sufficient set, the
   privacy defaults table, live preview before write, atomic write via the
   existing helper, opt-in detail capped at 50 000 rows.
2. **The action lives in Settings → "Data & diagnostics"**, beside the database
   path, and not in a panel of its own. That section exists because the path
   "used to be findable only by guessing where the exe lives, making 'attach
   your database' an unanswerable request in a bug report"
   (`crates/app/src/ui/settings/data.rs:1-7`) — the bundle button is the real
   answer to that request, and it retires the workaround the section was built
   around. The logging toggle already sits there too, so the whole diagnostic
   surface stays in one place.
3. **The panic hook is a separate card and should land first.** A
   `std::panic::set_hook` writing payload, location, thread and backtrace to
   the log is the highest-value single change found, and the bundle is worth
   less without it.
4. **Twelve further instrumentation gaps are worth their own cards**, ordered
   by decisiveness lost per unit of work:
   - **G2** `WorkerMsg::Error` is never logged, while `Warning` beside it is (trivial).
   - **G3** per-root routing reasons are computed, aggregated, localized and lost.
   - **G4** `is_available` discards the Win32 error that explains *why* a volume failed; `media_kind` does the same.
   - **G5** a finding whose `rel_path` has no `files` row is dropped by a bare `continue` with no trace.
   - **G6** a Steam manifest whose install dir is absent is skipped silently — the exact path "my game isn't listed" lands on.
   - **G7** per-game `ScanStats` is computed and thrown away.
   - **G8** the startup `db_error` is never logged.
   - **G9** the log has no rotation and no size bound.
   - **G10** log lines carry no severity, no thread, no scan-run correlation, and mixed languages.
   - **G11** `rules.json` carries no version and no integrity marker, unlike `l10n_rules.json`.
   - **G12** the log file path is never shown in the UI, though the database path is — the same problem, already solved once.
   - **G13** (doc rot) `FindingRow::group_dir`'s doc says it is never persisted, which contradicts the code that writes and reads it.
5. **Dependencies:** `serde_json`, `regex`, `rusqlite` and `thiserror` are
   already direct dependencies. `flate2`, `crc32fast`, `uuid` and `chrono` are
   already in `Cargo.lock` transitively and cost nothing to promote. One
   genuinely new crate is needed — `zip` (8.6.0 stable, verified on crates.io),
   which pulls in the first two anyway. `sha2`/`blake3` are **not** recommended
   until opt-in content hashing is actually built.
6. **Nothing here requires a network.** See
   [telemetry-spike-2026-08-13.md](telemetry-spike-2026-08-13.md).

## Measurement reproducibility

The payload generator and the benchmark are throwaway scripts, not part of the
build. They read the release database read-only and wrote nothing into the
repository. The numbers above can be reproduced by regenerating payloads from
any database with a comparable row count; ratios will move with the data's
redundancy, but the ordering of the codecs and the openability argument will
not.

## Sources

- [Home Assistant — Diagnostics](https://www.home-assistant.io/integrations/diagnostics/) and [developer docs](https://developers.home-assistant.io/docs/core/integration/diagnostics/)
- [Android — bugreport-format.md (AOSP)](https://android.googlesource.com/platform/frameworks/native/+/39d5eeb582/cmds/dumpstate/bugreport-format.md)
- [Red Hat — Generating an sos report](https://docs.redhat.com/en/documentation/red_hat_enterprise_linux/9/html/getting_the_most_from_your_support_experience/generating-an-sos-report-for-technical-support_generating-sos-reports-for-technical-support) and [KB 3592](https://access.redhat.com/solutions/3592)
- [Docker Desktop — Troubleshoot](https://docs.docker.com/desktop/troubleshoot-and-support/troubleshoot/)
- [Syncthing support bundle](https://docs.kastelo.net/syncthing/support-bundle/)
- [GitHub Enterprise — Providing data to support](https://docs.github.com/en/enterprise-server@3.0/support/contacting-github-support/providing-data-to-github-support)
- [JetBrains — Locating IDE log files](https://intellij-support.jetbrains.com/hc/en-us/articles/207241085-Locating-IDE-log-files)
- [OBS Studio — issue #3434, log upload privacy](https://github.com/obsproject/obs-studio/issues/3434)
- [macOS sysdiagnose explainer](https://eclecticlight.co/2026/06/27/explainer-sysdiagnose-and-logarchives/) (secondary; Apple publishes no structural spec)
- [crates.io — zip](https://crates.io/api/v1/crates/zip)
- Source reading at commit `111cce0`: `db.rs`, `ops.rs`, `safety.rs`, `settings.rs`,
  `atomic_file.rs`, `rules.rs`, `scanner.rs`, `providers/*`, `mftscan/*`,
  `logger.rs`, `export.rs`, `app.rs`, `worker/*`, `cli/report.rs`.

## Explicit uncertainties

- The app was not exercised. Behavioural claims about Win32 outcomes (for
  example what `is_available` returns on a ReFS volume) are inferences from the
  call sites, not observations.
- "`WorkerMsg::Error` is never logged" rests on the single `match` in
  `apply_message` plus the CLI's own handling; a consumer outside those two
  would change it.
- Only the production halves of the large files were read; test modules were
  assumed to produce no runtime evidence.
- The bundle-format survey covers seven products; Apple and Docker do not
  publish internal structural specs, so those two rows are behavioural
  observations from their own documentation rather than format specifications.
