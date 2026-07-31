# UI redesign — handoff for the next session (2026-07-31)

## State right now

`scan-accuracy-and-tree-search` is back at `c6c27ba`, i.e. **before any UI
redesign work**. The tree builds and every test passes (212 app + 351 core).
The only additions on the branch are the two design inputs (mockup HTML and
design prompt) plus this file — no code changes.

The first attempt at the redesign was reverted, not fixed. It is preserved as
a commit, not a stash:

| What | Where |
|---|---|
| Previous attempt (stages 1+2, unfinished) | branch `wip/ui-redesign-stage-1-2`, commit `397d878` |
| Same content, redundant backup | `stash@{0}` (safe to drop once the branch is confirmed) |
| UI/UX audit (source of the 4-stage split) | `ui-ux-audit-2026-07-29.md` (repo root) |
| Mockup, all 7 states | `docs/GameTrimmer UI Mockup (standalone).html` |
| Design prompt used to produce the mockup | `docs/design-prompt-ui-mockup-2026-07-30.md` |

Recover a single file from the previous attempt:

```bash
git checkout wip/ui-redesign-stage-1-2 -- crates/app/src/ui/settings/scanning.rs
```

Read the whole previous diff without touching the tree:

```bash
git diff c6c27ba wip/ui-redesign-stage-1-2
```

## Agreed scope

Stages **1 and 2** of the audit's §11 split. Stage 3 (sort selector,
token-filter chips, risk column, configurable columns, keyboard help,
operation history) and stage 4 (colour roles, density modes, spacing pass,
DPI verification) stay deferred — the user chose this scope explicitly and it
has not changed.

The full plan from the first attempt is still accurate as a *design* document
and worth re-reading before starting:
`C:\Users\Mancubus\.claude\plans\magical-prancing-firefly.md`.

## Why the first attempt was thrown away

Not because the design was wrong. Because of how it was verified.

Stage 2's settings dialog went through four rounds of "change the sizing code
→ run `build`/`test`/`clippy` → hand the user a release build → wait for them
to spot the bug in a screenshot". All four rounds were green on every
automated check, including the final one, which **panics when switching
tabs**. The user was doing all the actual verification, one symptom per round.

The fix for the next session is therefore a test harness, not a better guess
at a layout constant. See "Plan" below.

## egui 0.35 facts established the hard way

These cost four rounds to find and are the genuinely reusable output of the
first attempt. All verified against the vendored source in
`~/.cargo/registry/src/*/egui-0.35.0/`.

1. **`ScrollArea` defaults to `auto_shrink = true` on both axes.** So
   `.max_height(h)` is only a *ceiling* — a short section collapses the scroll
   area onto its own content height. This is why the dialog was a different
   height on every tab.
2. **`Ui::scope_dyn` allocates the child's `min_rect`, not its `max_rect`**
   (`ui.rs`, in `allocate_ui_with_layout_dyn`). A requested size is therefore
   not an achieved size — content that under-fills shrinks the parent anyway.
3. **`Separator` in a horizontal layout takes `available_size_before_wrap().y`**
   (`widgets/separator.rs:117-127`), i.e. only as tall as whatever was already
   added to that row. In a `ui.horizontal` the divider ends up the height of
   the nav labels, not of the panel.
4. **`egui::Modal` is anchored `CENTER_CENTER`**, so any height change
   re-centers the whole dialog. Combined with (1), switching tabs moved the
   content vertically — this is almost certainly the "red rectangle flashing
   for a split second" the user reported and which was never diagnosed.
5. `Ui::set_width(w)` sets min *and* max width; `set_min_height` on the modal's
   own `Ui` forces the entire dialog tall and leaves a blank gap under short
   sections (tried, rejected).
6. Scroll offset is remembered per `ScrollArea` id. A shared `id_salt` across
   tabs lets a long section's offset leak into a short one, which renders the
   short one already scrolled past its own content. Salting per section
   (`("gt_settings_scroll", section)`) fixes it and requires `Hash` on the
   section enum.
7. Clippy's `doc_lazy_continuation` fires on a doc-comment line starting with
   `- ` used as an em dash. Use `:` or restructure.

## Symptom history (all on the settings dialog)

| Round | Change | Symptom the user reported |
|---|---|---|
| 1 | inherited `height - 220` ceiling | Theme radios invisible, no scrollbar |
| 2 | `set_min_height` + `AlwaysVisible` + forced nav height | Scanning shows only Libraries, huge blank gap, red flash on tab switch |
| 3 | per-section `id_salt`, dropped `set_min_height` | General lost its entire "App language" section |
| 4 | fixed viewport: `allocate_ui_with_layout` + `set_min_height` on the row + `auto_shrink([false; 2])` | **Panics on tab switch** |

Round 4's approach (one fixed-height row for nav + divider + viewport,
identical for every tab) is still believed to be the *right shape* of the fix
— it addresses (1), (2) and (3) at once. It just needs to be built against a
harness that catches the panic, and the panic itself needs diagnosing.

## Unresolved: the panic

**Not diagnosed.** Known only that it happens on switching tabs in the release
build of round 4. Untested hypotheses, in rough order of suspicion:

- `set_min_height` inside the `allocate_ui_with_layout` closure combined with
  `auto_shrink([false; 2])` producing a degenerate or ever-growing rect;
- an egui `debug_assert` that is compiled out in release turning into a real
  failure downstream (`Region::sanity_check`, negative desired size);
- id collision between the per-section salted scroll area and something else
  allocated in the same frame.

Reproducing it under the harness is step 1 below and should settle this in one
run rather than by reasoning.

## Plan for the next session

0. **Harness first.** Add `egui_kittest` as a dev-dependency of
   `crates/app`. Confirmed available and version-matched:
   `egui_kittest v0.35.0` against `egui 0.35.0` (features: `snapshot`,
   `eframe`, `wgpu`, `x11`, `document-features`). Write a test that opens the
   settings dialog and visits every section, asserting no panic and that a
   known widget from each section is present. Run it against the *current*
   (pre-redesign) `settings_dialog.rs` to establish a green baseline.
1. **Reproduce the panic** by pointing the same test at
   `wip/ui-redesign-stage-1-2`. Record the stack trace here before changing
   anything.
2. **Stage 1 — recover, don't rewrite.** Bottom bar wrap, cancel gating,
   Custom-profile transition, MFT labels, modal gate, disabled hints. These
   were never implicated in any of the four bugs and can be cherry-picked from
   `397d878` file by file, each landing with the harness green.
3. **Stage 2 — rebuild in small steps**, each one keeping the harness green:
   scaffold + nav, then one section at a time. Do not hand the user a build to
   find layout bugs in; hand them a build to judge whether it *looks* right.

## What the harness does and does not cover

Covers: panics, widget presence, state transitions after a simulated click.
Does **not** cover whether the result looks good — proportions, spacing, how
much empty space under a short section is acceptable. Snapshot tests only
assert "unchanged since the baseline", and the baseline itself needs the
user's sign-off before it means anything. Aesthetic judgement stays with the
user; correctness moves into `cargo test`.

## Verification pipeline

```bash
cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
```

Baseline on `c6c27ba`: 212 app tests, 351 core tests, clippy clean. The
`binrw v0.11.3` future-incompatibility warning is pre-existing and unrelated.

## Working constraints carried over

- Code comments and documentation in English (this repo switched in `c6c27ba`).
- Do **not** take full-desktop screenshots for verification — an earlier one in
  this session captured the user's video call and private chats. If visual
  confirmation is genuinely needed, ask the user for a cropped screenshot of
  the specific window.
- The Windows-MCP `Click` tool rejects valid coordinate arrays
  (`loc: Input should be a valid list`) and could not be used to drive the app.
  This is what pushed verification onto the user in the first place, and is the
  reason the `egui_kittest` route matters.
- Release profile uses `panic = "unwind"` deliberately (`catch_unwind`
  isolation in scan code) — do not switch it to `abort`.
