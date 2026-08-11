# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- A launcher entry that cannot be read no longer lets a live, installed game
  be mistaken for leftover residue. Provider discovery now states whether it
  read a library's inventory in full, and a library whose evidence is
  incomplete is excluded from leftover detection instead of being treated as
  proof that a folder is unmanaged.
- A library root that has become unreachable — an unplugged drive, a removed
  drive letter, an unmounted volume — is no longer read as "these files were
  already deleted". Deletion refuses the whole batch, and the findings stay.
- Repeated scans no longer grow the database without bound: a scan generation
  the results have moved past is now removed with the results it produced.

### Added

- Every deletion is now planned against filesystem evidence recorded at scan
  time and re-proven immediately before the delete: the path is rebuilt from a
  trusted root and a normalized relative path, every directory from that root
  down to the target is opened and checked for reparse points, and the target's
  volume, file identity and — for a directory — the fingerprint of its contents
  must still match. Anything unproven blocks the deletion rather than
  proceeding.
- Deletions are journaled as a durable intent before the filesystem is touched,
  so a crash mid-delete is reconciled at the next start instead of leaving the
  database disagreeing with the disk. Reconciliation only classifies; it never
  retries an operation.
- Scans are staged and published atomically. Cancelling a scan, failing one, or
  crashing during one leaves the previous complete results in place, and
  findings reach the window only after their database transaction commits.
- Imported rule packs are bounded and validated before use (pack size, rule
  count, pattern length, nesting depth), carry their origin, and their findings
  are never auto-selected by a selection profile.

### Changed

- Scan-time diagnostics — an unreadable manifest, a launcher entry without an
  install path — now go to the log and the diagnostic bundle in full detail
  instead of onto the main window, where they crowded out everything else and
  named nothing the user could act on. Failures of an action the user asked for
  (adding a folder, removing a library, exporting) appear on the status line.
- User preferences now live in a readable, atomically written
  `gametrimmer.ini` beside the executable. Existing database settings migrate
  once when the ini is absent; deleting the disposable scan database no longer
  resets language, theme, deletion policy or other preferences.
- Diagnostic logging is enabled for new installations by default, while an
  existing explicit opt-out remains respected.

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
  path is compiled but switched off in release builds.

### Around it

- Portable: one exe, no installer, no registry, nothing written outside its
  own folder.
- English and Ukrainian interface, following the Windows UI language by
  default. This reaches all the way down: why a file was flagged is reported
  by the engine as evidence rather than as a finished sentence, and a rule's
  description can be written per language in `rules.json`.
- Light, dark, or system theme.
- First-run screen explaining the method, gated on an accepted disclaimer.
  Everything it asks of the user — the three steps, the disclaimer, the tick,
  the Scan button — fits the standard window without scrolling; the background
  reading follows the button rather than delaying it.
- Rule packs are importable and exportable; imports merge rather than replace,
  and back up the previous files as `*.bak`.
- No headless mode. It is compiled, type-checked and tested in every build
  (`--features headless` restores `--scan`, `--dry-run`, `--report`,
  `--profile`), but switched off in the release: it could not delete anything,
  and it returned the shell prompt before its own output. Any argument is
  answered with that, and no argument still opens the window.
- User-controlled diagnostic log written next to the executable. The program
  opens no network connection at all.

[Unreleased]: https://github.com/Mancubuss/GameTrimmer/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/Mancubuss/GameTrimmer/releases/tag/v1.0.0
