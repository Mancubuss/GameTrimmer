# GameTrimmer

[![CI](https://github.com/Mancubuss/GameTrimmer/actions/workflows/ci.yml/badge.svg)](https://github.com/Mancubuss/GameTrimmer/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platform: Windows x64](https://img.shields.io/badge/platform-Windows%2010%2F11%20x64-lightgrey)
![No network access](https://img.shields.io/badge/network-none-brightgreen)

**English** · [Українська](README.uk.md)

A portable Windows utility that finds files inside your installed games that
you personally will never need, and lets you delete them — deliberately, one
by one or a whole category at a time.

Games keep a lot on disk that you will never use: installers for libraries
that are already installed, voice-over in thirteen languages when you hear
one, PDF manuals and concept art, leftovers of games the launcher has already
forgotten. That is what GameTrimmer looks for.

**Nothing is deleted without your confirmation.** The app scans, shows what it
found as a tree, and waits.

<!-- SCREENSHOTS GO HERE — replace this comment with:
     ![Findings tree](docs/screenshots/tree.png)
     ![Settings](docs/screenshots/settings.png)
     ![Deletion summary](docs/screenshots/summary.png)
     A GUI tool without screenshots converts nothing. -->

## What GameTrimmer does NOT do

This matters more than the feature list, so it comes first:

- **It does not delete or uninstall games.** The game stays playable; only what
  you ticked disappears.
- **It does not touch saves, settings or profiles.** They are outside what the
  engine considers at all.
- **It does not change launcher state.** Steam, Epic, GOG, Ubisoft, EA and the
  rest are read **read-only** — nothing is ever written to their registry keys
  or folders.
- **It does not go online.** No updates, no telemetry, no "anonymous
  statistics". The program opens no network connection at all.
- **It does not install itself and leaves no traces.** All of its state lives
  next to the executable: disposable scan data in `gametrimmer.db`, user
  settings in the readable `gametrimmer.ini`, `rules.json`, `l10n_rules.json`,
  plus the default-on local diagnostic log `gametrimmer.log` (which you can
  switch off) and `*.bak` files after a rules import. Nothing outside its own
  folder (except the Windows Recycle Bin, if
  that is the deletion method you chose). Delete the folder and nothing is
  left. Deleting only `gametrimmer.db*` resets scan data without resetting your
  settings.
- **It does not promise a game will reinstall without consequences after a
  trim.** If the launcher runs a file verification, it will re-download what
  you removed.

## Quick start

1. Download the zip from the releases page and unpack it into a **writable
   folder** — anywhere, including a USB stick. Not into `Program Files`
   without administrator rights: the program stores results in a SQLite
   database next to itself and cannot work without write access (it will say
   so clearly).
2. Run `gametrimmer.exe`.
3. Press **Scan libraries**. Libraries are detected automatically; you can add
   your own folders by hand.
4. Review the findings tree, untick what you want to keep, and press delete.

Steam, Epic, GOG, Ubisoft, EA, Battle.net, Rockstar, Amazon, Riot, itch,
Humble and Xbox are recognized, plus the folders launchers install games into
by default. A launcher that is found but has nothing installed is shown empty
rather than skipped silently: silence about it would read as broken detection.

Scanning through `$MFT` (the faster path on HDDs) requires administrator
rights — the app will offer to relaunch. Without them it simply walks the
folders the ordinary way.

## What it finds

The names are the same ones you see in the findings tree:

| Category | What it is | Risk of loss |
|---|---|---|
| Orphaned | Leftovers of games the launcher no longer knows about | none — the game is already gone |
| Redistributables | vcredist, DirectX, PhysX and similar installers | none — trivially restored |
| Documentation and reference material | Manuals, licenses, help files | low — re-downloadable from the store |
| Bonus material | Soundtracks, wallpapers, concept art, artbooks | low — same |
| Localization files | Voice-over and text for languages outside your keep-list | low — same |
| Other | Developer residue: debug symbols, logs, test data | medium — worth a look before bulk deletion |

The language keep-list is configurable; languages on it are never flagged.

## Profiles and the ⚠ mark

A profile decides what arrives **already ticked** after a scan. It does not
change what was found — only the initial ticks — and it switches without
rescanning. Each profile is described in plain words in the settings, right
under its own switch.

- **Cautious** — only what the launcher will not bring back on its own:
  leftovers of deleted games, bonus material, documentation.
- **Balanced** — the same plus localizations outside the keep-list.
- **Aggressive** — the same plus everything else the engine is fairly
  confident about, including redistributables and installer residue.
- **Custom** — not by category, but purely by the engine's confidence in a
  specific file: only the most reliable findings get ticked.

Internally the engine computes a confidence score (0–100), but the tree does
not show it. That is its own internal scale, and it does not match the risk of
deleting: orphaned residue has low confidence and zero risk — the game is
already gone. Instead of a number, the row carries a **⚠**: the engine did not
tick this file for you, so look at it before deleting. The number itself
stayed where it has context — in the row's tooltip (next to the rule that
produced it) and in the CSV export.

## Reviewing what was found

The tree goes top down: disk → game → category → folder → file. A tick can be
placed at any level and propagates to everything below it.

- **Search by name.** The field above the tree keeps only the branches that
  contain a match. You can search for a game, a folder or a file; finding a
  game by name opens all of its contents. Search does not change what is
  ticked, and it resets after a new scan.
- **Category filter** — a separate axis from search; both work together.
- **Right-click any row** — reveal in Explorer, copy path, and for a file also
  "Open with". A disk row opens the root of the disk, a game row its install
  directory. This is how you check a questionable finding with your own eyes
  without leaving the app.
- **From the keyboard:** ↑/↓ — row, PageUp/PageDown and Home/End — page and
  end of list, →/← — expand and collapse a branch, Space or Enter — toggle the
  tick.

## Honest about the localization engine

The hardest part is deciding that a file belongs to a language you do not play
in. There is a deliberate trade-off here: **better to miss something than to
delete something needed.**

Metrics against the project's own 15,000-row corpus
(`cargo test -p gametrimmer-core --test corpus`):

- **zero false positives** — not one of the 6,637 files the corpus marks as
  "do not touch" was ever proposed for deletion;
- **81.2% recall** — roughly a fifth of genuinely redundant language files are
  deliberately left alone;
- **99.6% type accuracy** (audio / text / video / font / graphics) among what
  was flagged.

These numbers are not the engine grading itself: every row where the corpus
and the engine disagreed (2,289 out of 15,000) was reviewed by hand in July
2026, file by file. That review showed that all 61 "false positives" were in
fact correct and the corpus was wrong; and that 621 "misses" were not
localizations at all — the draft labeller had latched onto a coincidence
(`cs_` as *cutscene* rather than Czech; `chi` as the Japanese syllable *chi*
rather than Chinese).

Why recall is not higher is not an unfinished job but a line we do not cross.
A file with no language token is never flagged, even if it sits in a language
folder. Matching works on whole tokens, not substrings, because otherwise
`read-me` or `up-to` start reading as languages. The `<lang>.lproj` layout is
not covered yet. Pushing recall to 85% would require weakening those
invariants — and we do not.

Localized **graphics** have their own type, but the engine never flags a file
for "looking like an image" — on the contrary, the words `art`, `textures`,
`meshes` block a match: textures are the bulk of a game's bytes and almost
never language-dependent. The "graphics" type only refines *what* was found
once a file has already been flagged on linguistic grounds.

If you see something missed — that is expected. If you see something **falsely
flagged** — that is a bug, and worth reporting.

## How files are deleted

Two methods, switched in the settings:

- **Permanent deletion** (default) — fastest, unrecoverable.
- **To the Recycle Bin** — slower and bounded by the bin's quota, but
  reversible. Removable media may have no Recycle Bin at all; behavior then
  depends on the system.

After a deletion the app reports **how much space was actually freed**, not
how much was expected — two different numbers whenever the Recycle Bin holds
part of it back.

## Headless (CLI)

**Not part of version 1.0.** Launched with any argument, GameTrimmer says so
and exits; launched with none, it opens the window as usual.

Two things kept it out. It could not delete: `--apply` is the only path in the
program where files disappear without a click, it has never been exercised on
live data, and it stays switched off. And because the exe is built as a Windows
GUI application, the shell prints its prompt again the moment the process
starts — the report then arrives underneath that prompt, at a console that
looks like it has already finished. A read-only reporter that does not hand you
back your console is not worth shipping.

The code is compiled, type-checked and unit-tested in every build, so it is not
going anywhere: `--features headless` restores the mode (`--scan`, `--dry-run`,
`--report <path>`, `--profile <name>`), and `--features cli-apply` additionally
restores `--apply` for those who understand the risk. It goes back on by
default once it works end to end.

## Portability and known limits

- One exe, no installers, nothing written to `%APPDATA%` or the registry.
- A writable folder is a hard requirement, not a preference.
- Windows 10/11, x64. The manifest declares per-monitor DPI v2 and long paths.
- The executable is not code-signed, so SmartScreen will warn on first run.
- Some scenarios have been reasoned through and covered by automated tests,
  but not yet run on real hardware: an exFAT/FAT32 stick, unpacking into
  `Program Files` without rights, 100/150/200% display scaling, and the
  Recycle Bin on removable media. The manual steps for them are in
  [`docs/portability-test-cases.md`](docs/portability-test-cases.md). If you
  hit a problem in any of these, please report it.

## Rule packs

The analyzer is driven by two JSON files in the repository root:

- [`rules.json`](rules.json) — category rules (redistributables,
  documentation, bonus material and so on): path pattern → category →
  confidence. A rule's `desc` is what the tooltip and the CSV export show, so
  it can be written per language — `{"en": "...", "uk": "..."}` — or as a
  single string when the text is a product name and needs no translation.
  Unknown interface languages fall back to `en`.
- [`l10n_rules.json`](l10n_rules.json) — the localization engine's data: the
  language dictionary (canonical keys and their aliases by confidence tier),
  marker words (audio/text/video/fonts, negative markers) and the default
  language keep-list.

Both files are a canonical part of the repository (not generated artifacts)
and are embedded into the exe at build time. They are simultaneously program
source and standalone downloadable artifacts: they can be fetched straight
from GitHub, independently of the program itself (raw, branch `master`):

```
https://raw.githubusercontent.com/Mancubuss/GameTrimmer/master/rules.json
https://raw.githubusercontent.com/Mancubuss/GameTrimmer/master/l10n_rules.json
```

### Importing a rule pack

1. Download the file you want (your own rule set, a community-extended
   language dictionary, and so on) — the format is detected from the JSON
   structure, so the file name does not matter.
2. In the app, open **Settings → Rules → Import rules** and pick the
   downloaded file (several at once is fine).
3. The app merges the imported pack with the current one: new
   rules/languages/words are added, matches (same category+pattern, same
   language key) are updated with the pack's data — **nothing is removed**.
   Before writing, the current file is copied to `*.bak` automatically.
4. Changes take effect from the next scan.

**Settings → Rules → Export rules** saves the current effective set of both
files into a folder of your choice — a convenient starting point for your own
edits, or for proposing changes to the community set.

Contributions to the rule packs are very welcome — see
[CONTRIBUTING.md](CONTRIBUTING.md).

## For developers

Rust + egui (eframe), rusqlite, walkdir + rayon. Workspace:

- `crates/core` — scanning, rules, cache, launcher providers;
- `crates/app` — GUI and CLI.

```
cargo build --release
cargo test --workspace
```

The portable zip is built by `scripts/package-portable.ps1`.

[`docs/`](docs/README.md) has the manual test plan and the portability cases.
Design documents are not published: source comments explain themselves rather
than citing a specification by section.

## History

This is the fourth attempt: a Python prototype came first, then C++/WinAPI,
then C++20 with Qt 6.8, and finally this one. The earlier three are not
published — what was worth keeping from them was carried over and rewritten.

## Acknowledgements

- **Anthropic** — for Claude Code, with which this program was written.
- **Andrej Karpathy** — for the inspiration.
- **The author of [TikiOne Steam Cleaner](https://github.com/jonathanlermitage/tikione-steam-cleaner)**
  — for the idea and for the first list of redistributables that `rules.json`
  grew out of.

The same three lines are visible inside the program itself: on the first-run
screen and in **Settings → General**.

## License

MIT License.

TikiOne Steam Cleaner is also distributed under MIT, and MIT asks that its
text accompany copies. It lives in
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) — together with an account
of exactly what was taken from it (a list of targets, not rule text) — and
ships in the portable zip next to `LICENSE`.
