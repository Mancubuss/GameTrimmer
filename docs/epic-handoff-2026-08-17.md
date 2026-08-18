# Epic handoff — 2026-08-17

Written for whoever picks this up cold. Tree state: branch `master`, HEAD
`a9ca4fa` (`feat: close the delete-safety gaps, cover three more library
cases, and let a finding be kept for good`), with `102ef6d` (icon rework,
no functional code) directly beneath it. Tree clean, 1072 tests passing.
Everything below is committed — this document is not.

## Orientation

Three epics are live. **GT-EP7** (delete safety and tech debt) is down to
three cards, and all three are blocked on an owner decision rather than on
remaining effort. **GT-EP4** (provider and library coverage) is complete.
**GT-EP3** (rules, exceptions and localization) has its foundation card done
and five substantial cards still open — this is where the real work is.

One commit, `a9ca4fa`, closed nine cards at once (GT-105's six items,
GT-108's eight, GT-75, GT-21, plus GT-25/GT-23/GT-47/GT-38 and GT-109's
items 1–6). Its body is long and explains the *why* behind each change in
detail — read it before re-deriving any of this from the diff, it already
did that work. This document does not repeat it, only points at it and adds
what it doesn't cover: what's still open, what the code looks like after
the dust settled, and which line numbers moved.

**Correction to the prior handoff** (`docs/ep7-handoff-2026-08-16.md`,
written against `8c8b2c8`): the git log between that snapshot and now shows
three more commits closed cards that handoff listed as still open —
`fc9628f` (GT-107, providers), `5809dd3` (GT-74, UI), `4e0ec4f` (GT-134,
log level) — before the nine-card commit landed on top. So GT-EP7 is
further along than "nearly done" suggests: of the ten cards that document
tracked, only three remain.

## GT-EP7 · Безпека видалення й техборг (task 244)

### Done
GT-105 (all six items), GT-108 (all eight), GT-75, GT-74, GT-107, GT-134,
GT-25, GT-41 (resolved upstream, per the prior handoff). GT-135 (the
select-all hole that excluded `imported_untrusted` everywhere except the
toolbar) also appears fixed — `deletion_controller.rs:159`
(`select_all_never_selects_imported_untrusted_rows`) now asserts it — though
no card in the brief for this session explicitly named it closed, so verify
against the board before assuming.

### Open — each blocked on a decision, not on effort

**GT-106** (id 248) — "Deletion performance: tree fingerprint and
file_safety schema." Two independent halves:

1. *The double `tree_fingerprint` walk is not redundant — do not merge it.*
   `execute_delete_plans_observed` (`crates/core/src/ops.rs:402`) calls
   `validate_delete_plan` in its preflight loop (`ops.rs:419`, walk at
   `safety.rs:961`) as a batch precondition across every plan before any
   file moves. `open_verified_at` (`safety.rs:914`) does the walk again,
   per item, immediately before removal — and now that GT-105 landed, that
   second walk happens on the same open handle the delete then acts on
   (`safety.rs:847-923`). Caching the first result the way the old card text
   suggested would destroy the property the second walk exists for:
   freshness at the instant of deletion.
2. *What could be cheapened:* `safety::tree_fingerprint`
   (`safety.rs:482-513`) calls `identity()` per directory entry (line 491),
   i.e. one `CreateFileW` each. `fs::read_dir` on Windows already returns
   attributes/size/mtime from `FindNextFileW` at no extra cost, so the only
   component forcing the `CreateFileW` open is `file_index`
   (`FileIdentity::file_index`, `safety.rs:53`). Dropping it makes the
   fingerprint blind to "file replaced by another of identical size/mtime/
   attributes at the same path" — a **safety trade the owner must decide**,
   not a free optimisation.
3. *Schema-size claim needs re-measurement, and the slot has moved.* The old
   card's "≈ −40 MB" estimate needs recomputing against the real identity
   format (decimal `serial:index:kind:size:time:attributes`,
   `safety.rs:53-63` — unchanged fact from the prior handoff) on the owner's
   real library, which no agent here can do. **New since the prior
   handoff:** the migration slot the prior handoff called free
   (`CURRENT_SCHEMA_VERSION = 4`) is no longer free — `migrate_v5`
   (`db.rs:441-458`) shipped in `a9ca4fa` and added
   `operations.expected_tree_fingerprint` for GT-105's `partially_applied`
   verdict. `CURRENT_SCHEMA_VERSION` is now `5` (`db.rs:131`). Any GT-106
   schema change needs a new `migrate_v6`.
4. The card's claim that `file_safety.rel_path` duplicates `files.rel_path`
   is only true for game-bound findings. `file_safety` (`db.rs:109-120`) and
   `files` (`db.rs:47-55`) both carry `rel_path`, but orphan rows populate
   `file_safety.evidence_library_path` instead of going through a game, so
   the two columns hold different values there — it's two pieces of
   information, not a redundant copy, for that subset of rows.

**GT-109** (id 251) item 7 — the AUD-09 peak-RSS/backpressure benchmark.
Blocked structurally: `crates/app/Cargo.toml` declares only `[[bin]]`
(lines 6-8), no `[lib]`, and the bounded channel under test —
`std::sync::mpsc::sync_channel::<GameOutcome>(2 * scan_threads())` — lives
at `crates/app/src/worker/scan.rs:445`, private to the binary crate. An
external Criterion-style benchmark has nothing to link against. Two options
for the owner:
- Add a `[lib]` target to the app crate. Touches every `mod` declaration in
  `main.rs`, and is the more general fix if other benchmarks or a future
  integration-test crate will want the same access.
- Measure from inside a `#[cfg(test)]` harness in `scan.rs` using
  `GetProcessMemoryInfo`. Cheaper to write, but measures the test process's
  RSS, not the shipped binary's.

**GT-42** (id 56) — "folder collapsing needs refining." Still blocked on the
owner supplying concrete examples of folders that grouped wrongly. The rule
moved: it's `assign_group_dirs` (`crates/app/src/worker/scan.rs:1678-1717`,
was `1599-1638` when the prior handoff was written — collapse a directory
when every file under it is flagged, `total >= 2 && total == flagged_count`,
picking the shallowest such ancestor). Pinned by
`assign_group_dirs_collapses_a_folder_where_every_file_is_flagged` and four
sibling tests at `scan.rs:3797-3900`. Moving the rule without concrete wrong
examples from the owner is guesswork, same as before.

## GT-EP4 · Provider and library coverage — COMPLETE

All five cards closed: GT-23, GT-24, GT-25, GT-38, GT-47.

GT-24's spike is `docs/spike-registry-launcher-roots-2026-08-17.md` — read
it before writing any follow-up card, it has the live-vs-documented
evidence grading per launcher. Summary of what it found, with **no cards
opened yet for any of it**:

- **Ubisoft** (`uplay_install.manifest` + `uplay_install.state` at the root
  of every install) and **EA** (`__Installer\installerdata.xml`) both have
  in-folder ownership markers **live-verified on this machine** (2/2
  samples each). Extending orphan detection to them is the same shape
  `itch_spec` already uses in `orphans.rs` — a small, mechanical
  `OrphanScanSpec` constructor per launcher plus a match arm in
  `orphan_spec_for` (`crates/app/src/worker/scan/orphan_analysis.rs`) — but
  EA's marker is currently read as best-effort/optional display metadata in
  `ea.rs`, so its universality needs more than 2 samples before it gates an
  actual delete decision.
- **Epic** (`.egstore\`) and **GOG** (`goggame-<id>.info`) are
  documentation-only — same shape as Ubisoft/EA, but nobody has confirmed
  the marker on a live install yet.
- **Riot**, **Battle.net** and **Rockstar** are recorded as not detectable
  with the current marker scheme (Riot: live-verified no marker exists;
  Battle.net: no shared container to diff even setting the marker question
  aside; Rockstar: no live install available to check, no marker found in
  research). The spike's recommendation: record these on the board as
  "researched, no path forward" so nobody re-investigates the dead end —
  they should **not** get follow-up cards.
- **Paradox** also qualifies now: `launcher-settings.json` carries `gameId`,
  `exePath`, `gameDataPath` inside every install folder
  (`crates/core/src/providers/paradox.rs:376`), on top of the
  `%APPDATA%\Paradox Interactive\games` container itself being
  launcher-owned rather than user-chosen.

Sketch for whoever writes these cards: one card per launcher (they're
independent and small once the marker is confirmed), each needing the
`OrphanScanSpec` constructor, the `orphan_spec_for` wire-up, and the
itch-style three-way corpus test (managed install spared, leftover flagged,
foreign folder in the same shared root spared).

## GT-EP3 · Rules, exceptions and localization — the real remaining work

### Done
**GT-21** (personal exceptions) — the foundation everything else in this
epic builds on. `Verdict` (`crates/core/src/rules.rs:311-318`) is
`Unmatched | Kept | Flagged(Finding)`, not a rank, specifically so a keep
rule's veto outranks the localization detector stage too (`classify`,
`rules.rs:567`, runs before `combine_finding` in the scan worker hands
unclaimed files to language detection). `RulePolarity`
(`rules.rs:83-93`) is `Delete` (default) or `Keep`; scope is `Rule::app_id`
(`Option<String>`, `rules.rs:346-348`), resolved by `scope_applies`
(`rules.rs:365-369`) against the game's own `app_id` — `None` scope means
unscoped (every built-in rule). Personal exceptions persist to
`personal_rules.json`, materialized empty on first run by
`ensure_personal_rules_path` (`crates/app/src/worker/mod.rs:352-355`).

### Open

- **GT-06** (id 14) "Community recipes" — explicit phase 2 of GT-21,
  estimate L. The mechanism already exists; a recipe differs from a
  personal exception by exactly two things: a `build_id` (expiry) and a
  distribution channel. The hard part is trust, not code — a stale or
  malicious recipe breaks someone's install. The card requires: a signed or
  moderated default source, a diff shown before applying, binding to
  buildid, no auto-apply.
- **GT-19** (id 100) "Launcher client cache and logs" — a new rule category
  for the launcher's own working files (logs, dumps, htmlcache, httpcache,
  shadercache, Workshop temporaries), **off by default and outside
  auto-select at every profile**, because it touches the launcher's working
  state rather than surplus game files. New evidence from this session: a
  Paradox install's `.cpatch` folder holds 118 MB *after the download
  finishes* — live-verified at `H:\Paradox\surviving_mars`
  (`crates/core/src/providers/paradox.rs`, module doc lines 35-46,
  `has_content_besides_cpatch`, `paradox.rs:293`) — a concrete first target
  for this category.
- **GT-20** (id 101) "Language statistics per library" — estimate S,
  aggregate existing localization findings by language. The card argues for
  a compact addition near the keep-list in settings rather than a new
  panel, because the interface is already overloaded (see the project's
  standing note on this).
- **GT-62** (id 164) "Spike: splitting localization by resource type" —
  estimate L, called the single biggest unclaimed win in the backlog:
  English voice-over and video are tens of GB per game, currently locked
  behind GT-59's red warning frame because findings aren't split by
  resource type. Two prior spike attempts died on the API spend limit; the
  furthest one got as far as identifying Mafia III as the decisive test
  case — 5 language packs of ~3.9 GB each, with voice-over stored in files
  named `subtitles*.pck`, i.e. the filename actively lies about the
  resource type. That lead is recorded on the board card, not in this repo
  — re-read the card before restarting the spike. Evidence base in-repo:
  `crates/core/src/langdetect/` (`data.rs`, `dict.rs`, `family.rs`,
  `markers.rs`, `occurrences.rs`, `reason.rs`, `tokens.rs`), the
  `l10n_rules.json` data pack (materialized at runtime by
  `ensure_l10n_rules_path`, `worker/mod.rs:338-343`), and the hand-verified
  corpus at `tests/corpus/corpus.tsv` reachable from
  `crates/core/tests/corpus.rs` and `crates/core/examples/corpus_collect.rs`.
  The corpus is hand-verified (see the project's standing note on this) —
  an engine/corpus mismatch is an engine bug, not a corpus error to
  discount.
- **GT-40** (id 54) "Localization corpus: 31 kind mismatches" — needs an
  owner decision on whether to touch recall at all (the figure and mismatch
  count are on the board card, not reproduced here — re-read it, the corpus
  format and calibration history are documented in
  `crates/core/tests/corpus.rs:1-40`). The card asks to review it together
  with the keep-list UI rather than in isolation.

## Decisions waiting on the owner

Nothing below can move without one of these:

1. **GT-106, half 1** — is trading `identity()`'s `file_index` component out
   of `tree_fingerprint` (to drop the per-entry `CreateFileW`) an acceptable
   safety trade? It makes the fingerprint blind to same-size/same-mtime/
   same-attributes file substitution at an unchanged path.
2. **GT-106, half 2** — re-measure schema savings against the real
   `safety-capture` numbers on the owner's library, now that leaf identity
   comes from the MFT record (`7ce787d`) and `migrate_v5` already used the
   schema slot the prior handoff called free.
3. **GT-109 item 7** — add `[lib]` to `crates/app/Cargo.toml` (touches every
   `mod` line in `main.rs`), or accept an in-process `#[cfg(test)]`
   benchmark that measures the test binary rather than the shipped one?
4. **GT-42** — concrete examples of folders `assign_group_dirs` grouped
   wrongly. Nothing can be safely changed without them; the rule is pinned
   by five passing tests today.
5. **GT-20 / GT-40** — should localization corpus recall (currently
   imperfect per the board card) be touched at all before or alongside the
   keep-list UI work, or deferred until GT-62 changes what "a localization
   finding" even means?
6. **Epic/GOG follow-up cards** — nobody has a live install to confirm
   `.egstore\` / `goggame-<id>.info` firsthand. Needs either the owner's own
   install or someone else's machine before a card can be opened with
   confidence.

## Traps and things already tried

- **`DisplayIcon` / `UninstallString` are a dead end for standalone games.**
  Path of Exile 1 and 2 (tens of GB, `crates/core/src/standalone.rs`) have
  real uninstall entries with empty `InstallLocation`, and the obvious
  fallback — read `DisplayIcon`/`UninstallString` instead — was built first
  and measured: both point into
  `C:\ProgramData\Package Cache\{GUID}\PathOfExileInstaller.exe`, the
  bootstrapper's own copy of the installer (`standalone.rs:27-41`), so they
  find nothing for the case the module exists to solve. What works is a
  generic sweep of vendor registry keys under `SOFTWARE` that declare
  `InstallLocation` directly (`standalone.rs:326-356`) — unfiltered it
  offered around two hundred entries, three of them games, so a location
  filter is load-bearing, not cosmetic.
- **The `.bak` cleanup broke `restore_builtin_at` before it shipped.**
  `atomic_write_with_backup`'s own `.bak` is scaffolding, removed once a
  write commits — but `restore_builtin_at`
  (`crates/app/src/worker/rules_io.rs:102-141`) *deliberately* parks the
  displaced pack at the same `.bak` path as its actual product, not a
  temporary. Fixed by writing that backup explicitly, by hand, after the
  replacement (`rules_io.rs:116-138`), independent of how the atomic writer
  manages its own temporaries. Anyone touching `atomic_file.rs`'s backup
  handling again should read `rules_io.rs:104-115`'s comment first — it's
  exactly this trap, already documented in place.
- **The three-way discovery-policy disagreement was unreachable but real.**
  Before this session, three places independently decided whether discovery
  evidence permits a deletion — the load query's SQL, the scan's
  persistence path, and the delete preflight — and disagreed on an
  unrecognised status: two allowed it, one refused. Unreachable while only
  three statuses are ever written by today's code, which is a fact about
  the current writer, not the policy. Unified into one function,
  `safety::discovery_block_reason`, at the strictest of the three. Worth
  remembering if a new discovery status is ever added: check that function,
  not the three old call sites, they no longer exist independently.
- **`.cpatch` persistence, corrected.** The research going into GT-25
  assumed `.cpatch` disappears once a Paradox download finishes. It
  doesn't — verified live at `H:\Paradox\surviving_mars`, still holding
  118 MB post-download. The load-bearing check became "does the folder
  contain anything *besides* `.cpatch`", not "is `.cpatch` absent"
  (`paradox.rs:251-293`, tests at `paradox.rs:554-571`).
- **`depotcache` is a sibling of `steamapps`, not nested inside it** —
  corrected during GT-23. And a depot manifest needed by a game on one
  drive was found cached in a *different* library's `depotcache/`
  (`D:\...\Steam\depotcache`), so the "is this manifest still needed" check
  has to be computed across every library, not per-library
  (`crates/core/src/providers/steam.rs:511-519`).
- **`.bak`/restore interaction is the same trap as above, called out
  separately because it's easy to hit again**: any future change to how
  atomic writes manage their recovery copy should re-check
  `restore_builtin_at`'s reliance on that same path before assuming the
  temporary can be cleaned up more aggressively.

## Board note

The project board is a local Vikunja instance, GameTrimmer project id 5.
Every card named above carries a detailed Ukrainian progress note added
during this session — read the card before starting work on it rather than
trusting only this document, especially for GT-40's recall figures and
GT-62's Mafia III lead, which live on the cards and are only summarized
here.
