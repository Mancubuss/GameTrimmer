# Security policy

## Supported versions

The latest release. This is a single-binary desktop utility with no server
side and no auto-update, so "supported" means: fixes land in the next release
and you download it yourself.

## Reporting a vulnerability

Use GitHub's private reporting:
[Report a vulnerability](https://github.com/Mancubuss/GameTrimmer/security/advisories/new).

Please do not open a public issue for anything that could be used to destroy
someone's data before a fix exists.

## What counts as a security issue here

The program has no network surface at all, so the interesting classes are
local:

- **Deleting outside what the user selected** — path traversal through a
  crafted rule pattern, a symlink or junction followed out of a game
  directory, a path canonicalization mistake.
- **A rule pack that can cause harm on import** — `rules.json` and
  `l10n_rules.json` are downloadable from the internet and importable through
  the UI. A pack that makes the app write outside its own folder, or delete
  something no rule should reach, is a vulnerability, not a bad rule.
- **Elevation misuse** — the app can relaunch itself as administrator for the
  `$MFT` scan path. Anything that lets that elevated process be steered
  (argument injection into the relaunch, an untrusted path executed with
  elevated rights) belongs here.
- **Writing outside the program folder** — the app promises to leave no traces
  beyond its own directory (plus the Recycle Bin, when chosen). A way to break
  that promise counts.

## What does not count

- **A falsely flagged file.** That is a bug and an important one, but it is
  not exploitable — nothing is deleted without an explicit click. Use the
  "Falsely flagged file" issue template.
- **SmartScreen warning on first run.** The executable is not code-signed;
  this is expected and documented in the README.
- **Requiring administrator rights for MFT scanning.** That is what raw volume
  reads cost on Windows; without elevation the app falls back to a plain
  folder walk.
