# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.0] — not yet released

First public release.

### Finding

- Scans installed games across Steam, Epic, GOG, Ubisoft, EA, Battle.net,
  Rockstar, Amazon, Riot, itch, Humble and Xbox, plus the default folders
  launchers install into, plus folders added by hand. A launcher that is found
  but has nothing installed is shown empty rather than skipped silently.
- Three independent detectors: a rule set matching file and folder names
  against known patterns (`rules.json`), a localization detector recognizing
  language files by linguistic markers (`l10n_rules.json`), and an orphan
  check comparing what launchers still have manifests for against what is
  actually on disk.
- Six categories: orphaned, redistributables, documentation, bonus material,
  localization files, other developer residue. Each can be switched off
  entirely, in which case it is never analyzed, shown or stored.
- Localization engine measured against a 15,000-row hand-verified corpus:
  zero false positives, 81.2% recall, 99.6% type accuracy.
- Two scan paths: the NTFS `$MFT` index (faster on HDD, needs administrator
  rights) and a plain directory walk (~40× faster on SSD). Chosen
  automatically per volume, overridable in settings; a scan that fell back to
  the walk reports why.

### Reviewing

- Findings tree: disk → game → category → folder → file, with ticks at any
  level propagating downwards.
- Responsive search by name and a category filter as two independent axes;
  typing keeps keyboard focus even when the clear control appears.
- Right-click any row to reveal it in Explorer, copy its path, or open a file
  with another program.
- Full keyboard navigation.
- Four selection profiles (cautious, balanced, aggressive, custom) that
  re-tick the existing findings without rescanning.
- A **⚠** on rows the engine was not confident enough to tick for you.
- CSV export of the whole tree.

### Deleting

- Permanent deletion or the Windows Recycle Bin, chosen by the user.
- Confirmation before a deletion runs: on or off. There is no size threshold —
  the one that existed compared against the batch total rather than any single
  file, which is not what its label said.
- Reports how much space was actually freed, which is not the same number as
  what was expected whenever the Recycle Bin holds part of it back.
- Nothing is ever deleted without an explicit click. The unattended `--apply`
  CLI path is compiled but switched off in release builds.

### Around it

- Portable: one exe, no installer, no registry, nothing written outside its
  own folder.
- English and Ukrainian interface, following the Windows UI language by
  default. This reaches all the way down: why a file was flagged is reported
  by the engine as evidence rather than as a finished sentence, and a rule's
  description can be written per language in `rules.json`.
- Light, dark, or system theme.
- First-run screen explaining the method, gated on an accepted disclaimer.
- Rule packs are importable and exportable; imports merge rather than replace,
  and back up the previous files as `*.bak`.
- Headless mode (`--scan`, `--dry-run`, `--report`, `--profile`), none of
  which deletes anything.
- Opt-in diagnostic log written next to the executable. The program opens no
  network connection at all.

[Unreleased]: https://github.com/Mancubuss/GameTrimmer/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/Mancubuss/GameTrimmer/releases/tag/v1.0.0
