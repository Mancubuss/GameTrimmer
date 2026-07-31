# Contributing to GameTrimmer

There are two very different ways to help, and the first one needs no Rust at
all.

## 1. Rule packs — the easiest and most useful contribution

The analyzer is driven by two JSON files in the repository root:

- `rules.json` — category rules: path pattern → category → confidence.
- `l10n_rules.json` — the localization engine's data: language dictionary,
  marker words, default keep-list.

Both are plain data. Adding a launcher's redistributable folder, a language
alias, or a marker word is a one-line change that improves the tool for
everyone.

You can edit them in a text editor, or export the current effective set from
inside the app (**Settings → Rules → Export rules**), edit that, and open a
pull request against the repository files.

### The one rule that matters

**Zero false positives.** The engine's whole design trades recall away for
this: a file with no language token is never flagged even if it sits in a
language folder; matching works on whole tokens, never substrings, because
otherwise `read-me` and `up-to` start reading as languages. A pattern that
flags one extra needed file is worse than ten patterns that were never
written.

So every rule-pack change needs:

1. **At least two real paths it matches**, from actual games, with game names.
   Rules written against imagined paths are how false positives are born.
2. **The nearest path that must not match** — and a check that it does not.
3. A green corpus run:

   ```
   cargo test -p gametrimmer-core --test corpus
   ```

   The corpus is 15,000 hand-verified rows. Its labels are not a draft: every
   row where the corpus and the engine disagreed was reviewed by hand, file by
   file, in July 2026. If your change makes the corpus report a false
   positive, the change is wrong — not the corpus.

If you do not want to open a pull request, the **Rule pack suggestion** issue
template asks for exactly the same three things.

## 2. Code

```
cargo build
cargo test --workspace
```

Windows only — the app links Win32 directly (elevation, the MFT reader, the
Recycle Bin, registry-based launcher providers), so there is no cross-platform
build to keep green.

Before opening a pull request:

```
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

### House rules worth knowing before you write anything

- **UI changes are verified by tests, not by screenshots.** Every `ui`
  submodule has the shape `show(&mut GameTrimmerApp, &mut egui::Ui)` so it can
  be driven headlessly through `egui_kittest`. Read the module docs in
  `crates/app/src/ui/harness.rs` — they explain why: an earlier redesign was
  "verified" by handing someone a build and waiting for them to spot the bug,
  four rounds in a row, all green on `build`/`test`/`clippy`. Presence,
  geometry and state belong in `cargo test`. Only aesthetic judgement needs a
  human.
- **Look widgets up through `i18n::strings`, never by literal text.** A test
  keyed on a literal quietly stops testing anything the day the label is
  renamed.
- **Every user-facing string goes through `i18n`**, in both `en.rs` and
  `uk.rs`. No exceptions, including error messages.
- **A disabled control must explain itself.** Hovering any greyed-out action
  says why it is greyed out. A silent grey button is treated as a bug.
- **Silence is not an acceptable answer.** A launcher that was found but has
  nothing installed is shown empty rather than skipped; a scan that fell back
  from the MFT index to a folder walk says why it did. Silence reads as broken
  detection.
- **Deletion, settings persistence and scan routing need tests for the failure
  mode**, not only the happy path. Unknown stored values must fall back to a
  default rather than break loading — see `crates/core/src/settings.rs` and
  its tests for the pattern to follow.
- Code, comments and doc comments are in English. User-facing strings are in
  both English and Ukrainian.

### Commit messages

```
<type>: <description>
```

Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`, `ci`.

## Reporting instead of fixing

- **A falsely flagged file** — the most valuable report this project can get.
  Use the "Falsely flagged file" issue template.
- **A missed file** is expected behavior, not a bug: recall is deliberately
  ~81%. Report it as a rule pack suggestion if you can name the pattern.

## What will not be accepted

These are deliberate limits, not gaps:

- Anything that opens a network connection — no updates, no telemetry, no
  "anonymous statistics".
- Anything that writes to launcher folders or registry keys. Launchers are
  read-only.
- Anything that deletes without an explicit human click. (`--apply` exists,
  is compiled, and is switched off; it stays off until it can be rehearsed
  without deleting from a real library.)
- Anything that writes outside the program's own folder — no `%APPDATA%`, no
  registry, no installer.

## License

By contributing you agree that your contribution is licensed under the MIT
License, the same as the project.
