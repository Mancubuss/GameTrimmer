# UI redesign — development plan (2026-07-31)

Supersedes the *plan* sections of
[`ui-redesign-handoff-2026-07-31.md`](ui-redesign-handoff-2026-07-31.md) and
`~/.claude/plans/magical-prancing-firefly.md`. Both remain valid as inputs:
the handoff for the egui 0.35 facts and the symptom history, the old plan for
the *design* of each change. This document replaces the **build order and the
verification model**, which is the part that failed last time.

Scope is unchanged: audit §11 **Stage 1 + Stage 2**. Stage 3 and Stage 4 stay
deferred.

---

## 1. Why this plan differs from the last one

The last attempt did not fail on design. It failed because the only way to
find out whether a layout change worked was to hand the user a release build.
Four rounds, four symptoms, one per round, all green on `build`/`test`/
`clippy` — including the round that panics on tab switch.

So the ordering rule for this pass is:

> **No UI change lands before there is a test that would have caught the last
> bug of the same kind.**

The handoff already proposed a harness. What it did not have is the
information needed to build one cheaply. That information is below, and it
changes the shape of Phase 0 substantially.

---

## 2. Facts established while writing this plan

These are new relative to the handoff and are what make the harness a
half-day job instead of a rewrite.

### 2.1. Every UI module is directly harnessable

Each module exposes the same shape:

```rust
pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui)
```

— see [`ui/mod.rs`](crates/app/src/ui/mod.rs) and every submodule. That means
a test can drive `settings_dialog::show` through
`egui_kittest::Harness::new_ui(|ui| ...)` with **no `eframe::Frame`, no
window, no GPU**. Confirmed against the 0.35 API: `Harness::new_ui`,
`get_by_label`, `query_by_label`, `run`, `try_run` all exist; only
`snapshot()` needs the `snapshot` feature and only `render()` needs `wgpu`.

Consequence: **do not enable `snapshot`, `wgpu`, `eframe`, or `x11`.** The
handoff listed all five features. Default features cover the entire "covers"
list in its own §"What the harness does": panics, widget presence, state
transitions after a simulated click. Snapshot goldens are explicitly of
limited value here (the handoff says the baseline needs user sign-off before
it means anything) and would drag in a GPU dependency plus binary artifacts.
Revisit only if Stage 4 (spacing/DPI) actually happens.

### 2.2. `crates/app` is a binary-only crate

There is no `lib.rs` — only `[[bin]]` in
[`crates/app/Cargo.toml`](crates/app/Cargo.toml). All 212 existing app tests
live in `#[cfg(test)] mod tests` blocks inside the bin (52 in `model.rs`, 44
in `worker/scan.rs`, 37 in `worker/scan_route.rs`, 18 in `cli/args.rs`, …).

Consequence: harness tests go in `#[cfg(test)]` modules **inside** the bin
too. A `crates/app/tests/` directory cannot import `crate::ui` and would need
a new lib target — not worth it. `cargo test --workspace` picks up bin tests
already, so the existing pipeline needs no change.

### 2.3. `GameTrimmerApp::new` is not test-safe — this is a prerequisite

[`app.rs:250`](crates/app/src/app.rs:250) does all of the following:

- resolves `worker::db_path()`, which is `exe_dir()/gametrimmer.db` — under
  `cargo test` that is `target/debug/deps/gametrimmer.db`;
- opens (and therefore **creates**) that SQLite database;
- calls `elevation::is_elevated()`, a real syscall;
- computes `show_elevation_prompt`, which probes volume media via
  `DeviceIoControl` for each library ([`app.rs:1360`](crates/app/src/app.rs:1360));
- **spawns a background load worker** if the database has saved findings
  ([`app.rs:345`](crates/app/src/app.rs:345)).

A harness built on this would share one mutable database file across parallel
test threads, and its behaviour would depend on whatever the previous test run
left behind. The handoff did not mention this at all. It must be fixed
*before* the first harness test, not after the first flake.

Mitigating detail: `load_libraries(None)` returns an empty vec
([`app.rs:387`](crates/app/src/app.rs:387)), and
`compute_show_elevation_prompt` over an empty library list probes nothing. So
a temp-database app naturally starts with no libraries, no elevation prompt
and no spawned worker — deterministic, as long as the path is per-test.

### 2.4. The panic is diagnosed — see §2A

Resolved on the first harness run. Full write-up below; the release fallback
was not needed.

### 2.5. Two Stage-1 items are already confirmed present in the code

Not hypotheses from the audit — verified now:

- [`tree_view.rs:126-128`](crates/app/src/ui/tree_view.rs:126): `modal_open`
  checks `confirm_delete`, `remove_summary`, `show_elevation_prompt` and is
  missing `show_settings` and `confirm_clear_database`. The tree reacts to
  arrow keys behind the settings dialog today.
- [`bottom_bar.rs:36-124`](crates/app/src/ui/bottom_bar.rs:36): summary,
  hint, occupancy, profile combo, Select all, Deselect all and Delete are all
  children of one `ui.horizontal` with no wrap path. This is audit §5.1
  exactly.

---

## 2A. Panic diagnosis (resolved 2026-07-31)

**Verdict: `Ui::indent` called inside a horizontal layout.** Reproduced on the
first debug run of the harness against round 4. Evidence preserved on branch
`wip/panic-repro`, commit `7ce2c6a` (harness from `06debbc` applied on top of
`397d878`, plus the two tests below).

```
egui-0.35.0/src/ui.rs:2246
You can only indent vertical layouts, found
Layout { main_dir: LeftToRight, main_wrap: false, main_align: Center, ... }
```

### Mechanism

`settings/mod.rs` allocates one row for nav + divider + viewport with

```rust
ui.allocate_ui_with_layout(
    egui::vec2(target_width, viewport_height),
    egui::Layout::left_to_right(egui::Align::Min),
    |ui| { ... },
)
```

`ScrollArea::show` renders its content with **the parent's layout**, so every
section body inside that row runs left-to-right. `Ui::indent` asserts on
`self.layout().is_vertical()` and blows up.

Only `scanning.rs` and `selection.rs` call `indent` (three routing hints, two
delete-method hints). `General` — the default section — does not. That is
precisely why the dialog opened fine and died on the first click to Scanning
or Selection: *"panics on tab switch."*

### Two things this settles

**It is an `assert!`, not a `debug_assert!`.** It fires in release exactly as
in debug, so the handoff's hypothesis 2 ("a debug_assert compiled out in
release turning into a real failure downstream") is out, and the release
fallback run was unnecessary. Hypotheses 1 (`set_min_height` + `auto_shrink`
producing a degenerate rect) and 3 (id collision) are also out — none of the
three was right, which is the argument for reproducing rather than reasoning.

**Round 4's shape is proven, not believed.** The fix is a single
`ui.vertical(|ui| ...)` wrap around the scroll area's content. With only that
change, both repro tests pass — including

```
the_modal_does_not_move_between_sections
```

which asserts the modal's heading lands at identical coordinates across all
five sections. So the fixed-viewport approach handled handoff facts 1, 2 and
4 correctly the whole time; the layout kind was the only defect.

### Consequence for §5

`settings/` can be cherry-picked from `397d878` rather than rebuilt from
scratch, and §5.0's scaffold gate starts from a known-good geometry. The
scaffold test is still written first — it is what keeps the geometry good
through five section commits.

### New egui 0.35 fact, for the list in the handoff

> **A `ScrollArea` renders its content in the parent `Ui`'s layout.** Nesting
> one inside `allocate_ui_with_layout(.., Layout::left_to_right(..), ..)`
> silently gives every child a horizontal layout. `Ui::indent`, `Ui::separator`
> and anything else that assumes a column will misbehave or assert. Wrap the
> content in `ui.vertical` at the top of the scroll area.

---

## 3. Phase 0 — the seam and the harness

**Status: done (2026-07-31).** Commits `002c26e` (seam), `06debbc` (harness +
baseline), `7ce2c6a` on `wip/panic-repro` (diagnosis). 226 app tests, up from
212; 351 core; clippy clean. Steps below kept as the record of what was built
and why.

One thing the harness taught that is not in §2 and matters for every test
written from here on:

> **`Node::click` is a synthetic pointer event at the node's centre, and egui
> discards pointer events outside a `ScrollArea`'s clip rect.** Clicking a
> widget that is in the accessibility tree but scrolled out of view therefore
> does *nothing*, silently — two of the six baseline tests initially passed for
> that reason rather than on merit. `UiTest::click` scrolls the target into
> view, re-queries it (scrolling invalidates the rect) and only then clicks.
> Deliberately not `Node::click_accesskit`, which activates unreachable
> controls too and would hide exactly the bug round 1 shipped.

Nothing in Phase 1 or 2 starts until Phase 0 is green.

### 0.1. Test-safe construction seam

Split [`app.rs:250`](crates/app/src/app.rs:250):

```rust
pub fn new(ctx: egui::Context) -> Self {
    Self::new_with(ctx, worker::db_path().ok(), true)
}

fn new_with(ctx: egui::Context, db_path: Option<PathBuf>, autoload: bool) -> Self
```

`autoload = false` skips the saved-findings worker spawn. Production
behaviour is byte-identical; the seam is additive. Tests build with an
explicit path inside a `tempfile::TempDir` (`tempfile` is already a
dev-dependency), so each test owns its database and nothing lands in
`target/`.

Do **not** pass `db_path: None` in tests: that path sets `db_error` and the
dialog would then render an error banner the harness has to know about. A real
temp database keeps the widget tree identical to production.

**Acceptance:** a test constructs two apps concurrently without touching a
shared file; `cargo test` leaves no `.db` in `target/debug/deps/`.

### 0.2. Add the dependency

```toml
[dev-dependencies]
egui_kittest = "0.35.0"   # default features only
tempfile = "3.27.0"
```

Verified fetchable (`egui_kittest v0.35.0`, matching `egui 0.35.0`). It is not
in the local registry yet, so this step needs network once.

**Acceptance:** `cargo test --workspace` still reports 212 app + 351 core
tests passing, and `cargo clippy --workspace --all-targets -- -D warnings` is
still clean with the new dev-dependency present.

### 0.3. Harness helper module

New `crates/app/src/ui/harness.rs`, `#[cfg(test)]`-gated and declared as
`#[cfg(test)] mod harness;` in [`ui/mod.rs`](crates/app/src/ui/mod.rs). It
provides:

- `fn test_app() -> (TempDir, GameTrimmerApp)` — temp db, `autoload = false`,
  keeping the `TempDir` alive for the caller;
- `fn run_ui(app, f)` — wraps `Harness::new_ui`, calls `try_run`, and turns
  `ExceededMaxStepsError` into a named failure instead of a hang;
- `fn assert_no_panic_across(sections)` — the driver used repeatedly below;
- label lookup that goes through `i18n::strings(lang)` rather than literal
  text, so a copy change does not silently turn an assertion into a no-op.

That last point matters: several Stage-1 items *are* renames (`profile_custom`
→ "Власний", force-MFT → "prefer MFT"). Tests keyed on hardcoded strings would
have to be edited in the same commit as the rename, which defeats them.

### 0.4. Baseline test against the **current** dialog

Against today's [`settings_dialog.rs`](crates/app/src/ui/settings_dialog.rs),
before any redesign:

1. open the dialog (`app.show_settings = true`), run, assert no panic;
2. assert one known widget per current section is present (delete method,
   app language, theme, keep-list, categories, and the Advanced header);
3. expand Advanced, run, assert the routing radios appear;
4. toggle the last remaining language off and assert it is still on next
   frame — this pins the current *silent revert* behaviour (audit §6.5) so
   that Stage 2's change to a disabled-with-tooltip control is a deliberate,
   visible test edit rather than an accident.

**Acceptance:** green on `c6c27ba` as-is. If it is not green, the harness is
wrong, not the dialog.

### 0.5. Reproduce the panic

A file-level checkout does not work: round 4's `settings/` depends on i18n
keys, `Settings` fields and `app.rs` state that only exist on that branch. Use
a worktree and carry the harness over instead:

```bash
git worktree add -b wip/panic-repro <scratch> wip/ui-redesign-stage-1-2
```

then cherry-pick the seam commit and copy `ui/harness.rs` in.

**Done — see §2A.** `Ui::indent` inside the horizontal layout that
`allocate_ui_with_layout` establishes and `ScrollArea` propagates. One
`ui.vertical` wrap fixes it, and round 4's geometry is proven correct.

---

## 4. Phase 1 — Stage 1, recover file by file

Source: `wip/ui-redesign-stage-1-2` (`397d878`). None of these seven were
implicated in any of the four bugs, so they are cherry-picks, not rewrites.
One commit each, harness green before the next.

| # | Change | Files | Test that must exist |
|---|---|---|---|
| 1 | Adaptive bottom bar — wrap the action cluster below a width threshold; move the profile picker up to the plan panel | `bottom_bar.rs`, `plan_panel.rs` | Run the panel at 760 px and at 1200 px; assert the Delete button node exists and its rect is inside the available width in both |
| 2 | Cancel only for cancellable jobs — gate on `ProgressState.verb`, not bare `busy` | `top_bar.rs` | For each `Verb`, assert Cancel present ⇔ `Scan`/`Analyze` |
| 3 | Manual edit flips the profile to `Custom` | `app.rs`, `tree_view.rs` | Simulate a row checkbox click; assert `settings.selection_profile == Custom` |
| 4 | Rename `profile_custom` → «Власний»; force-MFT → "prefer MFT where available" | `i18n/uk.rs`, `i18n/en.rs` | Existing i18n parity test extended to the new keys |
| 5 | Centralized modal gate — add `show_settings` + `confirm_clear_database` | `tree_view.rs:126` | With the settings dialog open, send `ArrowDown`; assert `tree_cursor` unchanged |
| 6 | `on_disabled_hover_text` on every `!busy` / `has_findings` / `selected_count > 0` gate | `bottom_bar.rs`, `top_bar.rs` | Assert each disabled button carries non-empty hover text |
| 7 | Bottom bar hidden before the first scan (audit §5.3) | `bottom_bar.rs`, `app.rs` | With `findings` empty and no prior scan, assert the Delete button is absent |

Item 1's width assertion is the one that would have caught the original
"delete button clipped at 900 px" complaint. Item 5 is a two-line fix for a
confirmed live bug and could reasonably go first.

Note on item 7: this is audit §5.3, listed under audit Stage 1's spirit but
not in the old plan's Stage-1 list. It is cheap and it is the difference
between an empty screen that looks broken and one that looks intentional.
Drop it if it turns out to entangle with the plan panel.

---

## 5. Phase 2 — Stage 2, rebuild the settings dialog

Design is unchanged from the old plan §"Stage 2" — five sections in
`crates/app/src/ui/settings/` (`mod.rs`, `general.rs`, `scanning.rs`,
`selection.rs`, `rules.rs`, `data.rs`), left nav, one scroll area, visible
save state. Read that document for the per-section content; it is accurate
and grounded in the real data model (two JSON packs, not the mockup's three
YAML files; an added `default_selection_profile` field rather than
overloading the live one).

What changes is **how it is built**.

### 2.0. Scaffold first, sections second

Land `mod.rs` with: the modal frame, the nav list, the `SettingsSection` enum
(needs `Hash` — see handoff fact 6), the footer, and **five placeholder
sections each rendering a single distinct label**. No real content.

Then write the test that all four previous rounds lacked:

```
for every section, in every order, twice:
    click the nav entry
    run
    assert: no panic
    assert: that section's marker label is present
    assert: no other section's marker label is present
    assert: the modal's outer rect is identical to the first section's
```

That last assertion is the whole ballgame. It fails on handoff facts 1, 2 and
4 simultaneously — `auto_shrink` collapsing a short section, `scope_dyn`
allocating `min_rect`, and `Modal`'s `CENTER_CENTER` anchor re-centering on
every height change. Rounds 1–4 each fixed one of those and shipped the
others.

Fixed-viewport approach (round 4's shape) is **verified correct** — §2A ran
this exact assertion against `397d878` plus a one-line fix and it passed for
all five sections:

- one row of fixed height for nav + divider + content viewport, allocated with
  `allocate_ui_with_layout`;
- `ui.vertical` wrapping the scroll area's content — the §2A fix, without
  which every section body inherits the row's horizontal layout;
- `auto_shrink([false; 2])` on the content scroll area;
- per-section `id_salt` (`("gt_settings_scroll", section)`) so a long
  section's scroll offset cannot leak into a short one (handoff fact 6);
- `set_min_height(viewport_height)` **inside** the allocation closure — kept,
  since it is not what caused the panic. `Separator` gets its full height from
  the allocated row (handoff fact 3);
- no `set_min_height` on the modal's own `Ui` (handoff fact 5, tried and
  rejected).

**Gate:** the scaffold test must be green with placeholder content before any
real section is written. If the geometry cannot be made stable with five
one-label sections, it will not become stable with five real ones.

### 2.1 – 2.5. One section per commit

Order by risk, cheapest first:

1. **`general.rs`** — language, theme. Already settings-backed, no new state.
2. **`data.rs`** — db path/size/schema, compact, danger-zone clear. Mostly a
   move of existing widgets.
3. **`selection.rs`** — delete method (move), new `default_selection_profile`,
   new `confirm_behavior`. Two new persisted fields following the exact
   `DeleteMethod` pattern; unit-test the parse/serialize pair and the
   `request_delete_confirmation` gating as plain logic, not through the UI.
4. **`rules.rs`** — two real packs, validity line, existing export/import,
   new `restore_builtin`. Test `restore_builtin` at the worker level.
5. **`scanning.rs`** — the largest: library list moved out of
   `libraries_panel.rs`, keep-language chips + searchable combo replacing 36
   checkboxes, the categories table, the MFT radios, the per-`WalkdirReason`
   diagnostics line. This is where round 2's "Scanning shows only Libraries,
   huge blank gap" happened. It goes last, on a scaffold already proven
   stable by four other sections.

After each: re-run the full section-cycling test from 2.0, plus a
section-specific presence test. The keep-language rework additionally must
flip the §0.4 last-checkbox assertion from "silently reverts" to "renders
disabled with an explanatory tooltip" — a deliberate, reviewed test edit.

### 2.6. Save state

Transient "Збережено" next to the section heading, inline error if a setter's
persistence fails. Check first whether `persist_settings()` is fallible today;
if it is infallible, ship the success indicator and leave the error branch out
rather than fabricating error handling for a path that cannot occur.

---

## 6. What the harness still does not cover

Unchanged from the handoff, and worth restating because it is the boundary of
this plan:

Covers — panics, widget presence, widget absence, geometry invariants
(identical modal rect across sections, primary button inside the viewport),
state transitions after a simulated click, keyboard events reaching or not
reaching the tree.

Does not cover — whether it looks good. Proportions, how much empty space
under a short section reads as acceptable, whether the danger zone's red is
too loud. That stays with the user.

The contract for this pass: **the user is handed a build to judge whether it
looks right, never to find out whether it works.** If a build goes out and
comes back with a functional bug, the correct response is to add the test that
missed it before fixing the bug.

---

## 7. Verification pipeline

```bash
cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
```

Baseline on `c6c27ba`: 212 app tests, 351 core tests, clippy clean. The
`binrw v0.11.3` future-incompatibility warning is pre-existing and unrelated.

Run after every numbered item above, not per phase.

---

## 8. Risks

| Risk | Status / mitigation |
|---|---|
| ~~§0.5 does not reproduce the panic~~ | **Closed.** Reproduced and fixed on the first debug run — §2A |
| ~~The fixed-viewport shape cannot hold a stable rect~~ | **Closed.** §2A proved it holds across all five sections |
| ~~`new_with` seam changes production startup by accident~~ | **Closed.** `new()` passes exactly `worker::db_path().ok()` and `true`; 216 pre-existing app tests unchanged |
| A test passes because a click silently missed a clipped widget | Real, and already hit twice — see the note in §3. `UiTest::click` scrolls first; never reach for `click_accesskit` to make a red test green |
| kittest label queries depend on AccessKit output that egui does not emit for some widget | Fall back to asserting on app *state* after a simulated click rather than on node presence; note the gap in this file |
| Cherry-picking round 4 also carries its unreviewed content changes | Section commits are per-file and each lands with its own test; treat `397d878` as a source of diffs to read, not a source of truth |
| Scope creep into Stage 3/4 | Anything not in §4 or §5 goes to the board, not into this branch |

---

## 9. Exit criteria

Phase 0 done when the baseline test is green on the unmodified dialog and the
panic has a written verdict. **Met 2026-07-31** — see the status note in §3
and the verdict in §2A.

Phase 1 done when all seven items are on the branch, each with its test, and
the audit §12 "Головне вікно" checklist items covered by Stage 1 pass.

Phase 2 done when: no nested scroll areas remain; every section is one click
away; the modal rect is provably identical across sections; selected languages
are visible without scrolling; the last mandatory checkbox explains itself
instead of jumping; save state is visible; the danger zone is visually
separated; and the tree ignores keyboard input behind every modal.

Then, and only then, a build goes to the user — for the aesthetic call.

---

## 10. Working constraints carried over

- Code comments and documentation in English.
- **No full-desktop screenshots.** An earlier session captured the user's
  video call and private chats. If visual confirmation is needed, ask for a
  cropped screenshot of the specific window.
- The Windows-MCP `Click` tool rejects valid coordinate arrays
  (`loc: Input should be a valid list`) and cannot drive the app. This is what
  pushed verification onto the user, and it is why the kittest route matters.
- Release profile keeps `panic = "unwind"` — `catch_unwind` isolation in the
  scan code depends on it.
