# GT-39 design: scans scoped to a library or a game

- Research date: 2026-08-15
- Branch: `master`, commit `e0869d2` (`refactor(scan): the scan picks how to read files, not the user`)
- Mode: read-only. No code was written for GT-39; the tree changes present
  alongside this document belong to GT-61 and to the elevation fix, not here.
- Question from the card: a scan is all-or-nothing today; make it possible to
  scan one game, several games, one library, or every library on one drive —
  "persistence must update only the touched records, without overwriting the
  rest of the snapshot".
- Answer: no. See the status section directly below.

## Status: DECIDED — not building this. Kept as the record of why.

**2026-08-15, owner's decision: scoped scans are not worth their complexity.
Results are rebuilt from scratch, always.**

The reasoning, and the measurements that support it:

- A full scan on a warm cache costs about two minutes, on a utility that is
  used occasionally rather than continuously. Two minutes does not justify
  revoking an invariant that protects scan results from partial writes.
- The cold-cache case, which is the one the card was really about, is cheap
  for the same reason: this machine's large libraries sit on spinning disks
  (`F:` is a 21.83 TB ST24000VE002 HDD holding the 17.6 TB Steam library;
  `E:` and `H:` are also HDDs), and an HDD routes to the MFT index, which
  stays in the tens of seconds cold. The SSD volumes that route to a directory
  walk are the small ones, where walking is fast anyway.
- The remaining half of the card's motivation — "I do not want this library
  scanned every time" — is served by GT-61 (excluding a library from
  scanning), which shipped instead and needs none of this machinery.

So the two features that make this one unnecessary are the automatic MFT
routing (commit `e0869d2`) and library exclusion. Between them, a scoped scan
has no remaining job.

**The one condition this rests on:** the scan must run elevated. Unelevated,
`F:` falls back to a directory walk and the cold scan is minutes, not seconds
— at which point the arithmetic above changes and this decision is worth
revisiting. `Settings > Scanning` offers the restart-as-administrator button
for exactly that reason.

Everything below is the analysis as it stood before the decision. It is kept
so that a future revisit starts from the findings rather than re-deriving
them, and because the hazard in the next section is a real property of the
current storage model that anyone touching persistence needs to know about.

## Verdict on the hazard

**The data-loss concern is real, and there are three independent failure
modes, not one.** All three fire from the same change — a partial scan taking
a new generation the way a full scan does.

1. **Findings for untouched games are deleted from disk.**
   `prune_superseded_generations` (`crates/core/src/db.rs:514-540`) deletes
   every row owned by any generation the active pointer moved past — findings,
   file_safety, files, games, library evidence, diagnostics. It runs *inside*
   `activate_scan`'s transaction, immediately before the commit
   (`db.rs:497`). A generation holding one game would supersede a generation
   holding 1628, and prune the other 1627 in the same committed transaction.
2. **The UI would lose them even with a healthy database.**
   `WorkerMsg::Done { findings }` replaces the app's entire findings list
   (`crates/app/src/app.rs:1713-1728`), and the worker returns only what it
   wrote this run.
3. **Orphans would be wiped library-wide.** `persist_orphans` deletes all
   `game_id IS NULL AND scan_id = ?` rows before reinserting
   (`crates/app/src/worker/scan/orphan_analysis.rs:171-184`), and is called
   unconditionally, including with an empty list.

Verified independently of the analysis that produced them:

- `findings` has **no** `scan_id` column (`db.rs:57-65`). It is bound to a
  generation only transitively, through `findings.file_id -> files.scan_id`.
  That indirection is why every generation-scoped delete of findings is
  written as a subquery over `files`.
- The prune is **not** a separate step that a partial scan could simply skip.
  It is what keeps the database from growing without bound, so any design must
  answer where the superseded rows go — not merely whether to delete them.
- `persist_libraries`'s delete block is **already** scoped
  `WHERE library_id = ?2 AND scan_id = ?1`
  (`crates/app/src/worker/scan/persistence.rs:192-216`). This is the fact that
  makes the recommended option cheap: the correct partial-write primitive
  already exists and is already in use.

## Scope representation

A fourth field on `ScanOptions` (`crates/app/src/worker/scan.rs:75-84`), which
already means "the knobs a scan runs under, captured once at spawn time".

```rust
pub enum ScanTarget {
    Everything,
    Games(Vec<PathBuf>),
    Libraries(Vec<PathBuf>),
}
```

Four decisions that came with it:

- **"All libraries on one drive" is not a variant.** The app already holds
  `self.libraries: Vec<LibraryRow>` with a `path` (`app.rs:660`); filtering it
  by drive letter is a UI concern. A `Drive(char)` variant would push volume
  semantics into a module that has no business with them.
- **Paths, not row ids.** `games.id` is regenerated on every scan —
  `persist_libraries` deletes and reinserts a library's rows — so an id
  captured in the UI is valid only until the next scan. `install_dir` is what
  `FindingRow` carries and what `dedupe_games_across_libraries` reconciles on;
  `game_libraries.path` is `UNIQUE` (`db.rs:30`).
- **Discovery stays unscoped.** It is cheap — provider manifests and a few
  dozen small text files (`persistence.rs:264-265`). The 15-20s startup cost
  is the delete pass, not discovery. Keeping discovery whole also keeps
  library evidence and orphan detection honest for the whole install, which
  matters because `scan_library_evidence` gates deletion (`ops.rs:209-211`).
- **Name collision:** `crate::logger::ScanScope` already exists
  (`crates/app/src/logger.rs:256`) and means the log's generation-id guard. Do
  not reuse that name.

## Persistence: the options

### A — amend the active generation in place (recommended, with a caveat)

A scoped scan never calls `begin_scan`. It writes into the current
`scan_state.active_scan_id`, replacing only the in-scope games, one
transaction per game or per `WRITE_BATCH_SIZE` batch.
`persist_prepared_game` already opens by deleting that game's findings and
replacing its files (`persistence.rs:307-317`) — it is already an idempotent
per-game replace.

- No schema change, no reader change, no migration, no `user_version` bump.
- A full scan is untouched, byte for byte, so the common case cannot regress.
- `scan_allows_deletion` (`db.rs:605-617`) keeps passing, because rows stay in
  the active generation.
- Cost: per-scan rollback is lost. A crash mid-scope leaves earlier games
  rescanned and later ones untouched. No game is ever half-described (the
  per-game transaction guarantees that), and no out-of-scope game is touched.

### B — new generation, carry untouched rows forward at activation

Re-stamp the previous generation's out-of-scope rows onto the new one inside
`activate_scan`'s transaction, before the prune. Rollback is preserved.

- Cost: re-stamping millions of `files` rows rewrites every `idx_files_scan_id`
  entry in one transaction holding the write lock, plus a WAL that size. On the
  1628-game library that is a fixed toll on *every* partial scan — cheaper than
  a full scan, but it means "recheck one game" is never fast, only faster.
- **Trap:** the intuitive version — re-stamp up front, right after
  `begin_scan` — is actively dangerous. It *moves* rows out of the old
  generation, so a later failure triggers `abort_scan`, which deletes
  `WHERE scan_id = N` and takes the carried-forward library with it. The
  carry-forward must happen inside the activation transaction or not at all.

### C — per-game generation stamps (replace the model)

Rejected. `scan_allows_deletion` is `scan_id == active_scan_id`; with no single
active id that check must be rewritten into "this file's game is the current
one for its install dir", and getting it wrong either blocks all deletion or
permits deleting against stale evidence. It also makes every read slower to
serve the uncommon case, on the path (`load_findings`, `load.rs:129-150`) that
was already optimized once for exactly this shape.

## The open question — decide before writing code

Option A writes into the active generation. Directly above the delete block it
would reuse, `persistence.rs:190-191` states the opposite as an invariant:

> Retrying preparation of the same staging generation is allowed, but rows
> from the active/previous generations are immutable.

So Option A is not "no changes needed", as it first appears. It revokes a
declared rule, and that rule is what makes the rollback reasoning tractable:
`abort_scan` deletes by `scan_id`, which is safe precisely because nothing
live is ever written under a scan id that might be aborted.

Two honest ways forward:

1. **Rewrite the invariant explicitly.** State that the active generation is
   mutable by scoped amendment only, make the two paths structurally distinct
   (a full scan constructs `ScanGenerationGuard`, a scoped scan constructs
   nothing — never an `if scoped` inside the guard), and accept the loss of
   per-scan rollback.
2. **Take Option B** and pay the re-stamp cost on every partial scan to keep
   rollback.

This is the only part of GT-39 that is a judgement call rather than a finding.

## Guardrails, if Option A is chosen

- Full scan constructs the guard; scoped scan constructs nothing. Not a flag.
- Scope `persist_orphans` to the in-scope library roots, or skip it and leave
  existing orphan rows alone.
- Refuse to run when there is no active generation (`active_scan_id` is `None`
  or `0`, the legacy read-only snapshot). "Nothing to amend" means "run a full
  scan", not "amend generation 0".
- After a scoped scan, re-read the whole snapshot with `load::load_findings`
  and send *that* in `WorkerMsg::Done`. It keeps the invariant that `Done`
  always carries a complete snapshot, which every consumer in
  `app.rs:1686-1735` assumes.

## Recommended first slice

`ScanTarget::{Everything, Libraries}`. Defer `Games`.

Library-scoped is a materially smaller problem:

- `persist_libraries`'s delete is already `WHERE library_id = ? AND scan_id = ?`;
  handing it a filtered list is the whole persistence change.
- `scan_library_evidence` is keyed `(scan_id, library_path)` and upserts
  (`db.rs:363-369`), so a library-scoped run refreshes exactly its own evidence.
- `persist_orphans` is inherently library-level (`collect_orphans` walks
  library roots). Under a game-scoped run there is no coherent orphan question
  at all, and the right answer — skip detection — is a behaviour difference the
  UI must explain.
- The MFT cost model degrades gracefully: a whole library on one volume is
  still worth an `$MFT` read; one game is not.

This lands two of the card's four cases, and the two that describe the actual
pain (a library changed, or one drive was reorganized). It defers individual
games, the MFT root-count threshold, and any UI for picking games out of a
1628-row tree — none of which are blocked by it, since the generation
question is answered once for all four.

## Blast radius

**Must change:** `worker/scan.rs` (`ScanOptions.target`, discovery filtering,
conditional guard, orphan block, `Done` payload), `worker/scan/persistence.rs`
(narrow the delete for `Games`), `worker/scan/orphan_analysis.rs:163-185`,
`worker/scan/generation.rs` (document why it is full-scan only), `app.rs`
(build the target, `spawn_scan` call site), `cli/mod.rs` (passes `Everything`),
the UI surface the action lands on, and i18n.

**Reads generations as global — review, all safe under Option A** precisely
because it never creates a second live generation: `worker/load.rs:190` and
`:111-117`; `db.rs:868` (`occupied_by_library`); **`db.rs:605-617`
(`scan_allows_deletion` — the load-bearing one**: any design leaving untouched
findings under a non-active `scan_id` silently makes all of them undeletable,
surfacing as `'legacy snapshot is read-only'`); `ops.rs:197-312`;
`bundle/sections.rs` (eight sites); `worker/rules_io.rs:307`;
`worker/delete.rs:228-234`; `db.rs:544-568` (`validate_scan_generation` — not
called on the scoped path under A, so the finding-has-safety-evidence
invariant is enforced only per game; worth an explicit end-of-run check).

**Verified unaffected:** `export.rs` (serializes the in-memory tree, no SQL),
`ui/tree_view.rs` and `model.rs` (build from `Vec<FindingItem>`),
`gamestate.rs`, `db.rs:823-841` (`clear_scan_data`), `db.rs:621-633`
(`cleanup_abandoned_scans` — no staging generation exists to abandon under A).

## Separate hazard in the same card

`run_mft_pass` (`scan.rs:833-972`) reads a whole volume's `$MFT` per volume
with any candidate root, and routing has no notion of how many roots it serves
(`scan_route::initial_route`, `volumes_to_check`). Amortized over 1628 games
that is the point of the path; for a one-game scan on an HDD it reads the
entire volume index to serve one directory. A scoped scan should route to
walkdir below some root count — and that needs a new `WalkdirReason` variant so
`format_walkdir_breakdown` can explain it, rather than a silent branch.

## Test names for the slice

```
a_scan_scoped_to_one_library_leaves_every_other_librarys_findings_in_place
a_scoped_scan_that_fails_partway_leaves_the_libraries_it_had_not_reached_untouched
a_scoped_scan_never_marks_the_active_generation_superseded
a_scoped_scan_refuses_to_run_against_the_legacy_generation
orphans_outside_the_scanned_library_survive_a_scoped_scan
a_full_scan_still_replaces_the_whole_snapshot_and_prunes_the_previous_one
```
