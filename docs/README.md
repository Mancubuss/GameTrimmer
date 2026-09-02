# Documentation

Start with the [README](../README.md) — this folder is for people working on
GameTrimmer, not for people running it.

Everything published here is **live**: kept true as the code changes. A
document that stops being maintained leaves this repository rather than
sitting where a reader would trust it.

| Document | What it is |
|---|---|
| [manual-test-plan.md](manual-test-plan.md) | ~110 manual cases covering what the 700+ automated tests cannot: UAC, the Recycle Bin, Explorer, Excel, SmartScreen. Has a 16-case smoke subset and the release condition. |
| [portability-test-cases.md](portability-test-cases.md) | TC-01…TC-11: the program on a flash drive, on FAT32, in Program Files without rights, at 200% DPI, under long Cyrillic paths. Group V of the test plan points here. |

## Why the source comments do not cite documents

They used to — section numbers in specifications, numbered findings in an
audit. They no longer do: a pointer to a document the reader cannot open is
worse than no pointer at all, so each fact was moved into the comment that
needed it. Keep it that way. If a comment needs to explain something, explain
it there.

## Not here

- **The design documents, plans, audits, spikes, and handoffs** live in
  `docs/internal/`, which is not published (see `.gitignore`). They describe
  intentions and dated snapshots rather than the shipped program.
- **The backlog** lives on a local Kanban board, not in this repository.
- **Release notes** are in [CHANGELOG.md](../CHANGELOG.md).
- **How to contribute** is in [CONTRIBUTING.md](../CONTRIBUTING.md); reporting a
  vulnerability is in [SECURITY.md](../SECURITY.md).
