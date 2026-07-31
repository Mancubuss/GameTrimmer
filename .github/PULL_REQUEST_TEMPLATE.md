<!-- Rule-pack-only changes (rules.json / l10n_rules.json): fill in the first
     two sections and delete the rest. -->

## What this changes

<!-- One or two sentences. -->

## Why

<!-- The problem, not the diff. -->

## For rule pack changes

- [ ] At least two **real** paths this new pattern matches (with game names)
- [ ] The nearest path that must **not** match, and a check that it does not
- [ ] `cargo test -p gametrimmer-core --test corpus` still reports **zero
      false positives**

<!-- Paste the paths here. -->

## For code changes

- [ ] `cargo fmt --all`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] UI changes are covered by `ui::harness` tests, not by "looks right on my
      machine" — see the module docs in `crates/app/src/ui/harness.rs`
- [ ] Anything touching deletion, settings persistence, or scan routing has a
      test for the failure mode, not just the happy path

## Manual check

<!-- Which cases from docs/manual-test-plan.md you ran, if any. -->
