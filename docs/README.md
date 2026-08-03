# Documentation

Start with the [README](../README.md) — this folder is for people working on
GameTrimmer, not for people running it.

Everything here is either **live** (kept true as the code changes) or
**archived** (a dated snapshot, left as written). Nothing in between: a
document that is no longer maintained moves to `archive/` rather than quietly
going stale where a reader would trust it.

## Live

| Document | What it is |
|---|---|
| [manual-test-plan.md](manual-test-plan.md) | ~110 manual cases covering what the 700+ automated tests cannot: UAC, the Recycle Bin, Explorer, Excel, SmartScreen. Has a 16-case smoke subset and the release condition. |
| [portability-test-cases.md](portability-test-cases.md) | TC-01…TC-11: the program on a flash drive, on FAT32, in Program Files without rights, at 200% DPI, under long Cyrillic paths. Group V of the test plan points here. |
| [04_implementation_plan.md](04_implementation_plan.md) | The detection algorithm as specified. Source comments across `langdetect/` cite its sections by number — §5.2 for the dictionary, §5.4 for the language-family heuristic — so it is a live reference, not a historical plan. |
| [05_rules_pack_plan.md](05_rules_pack_plan.md) | The rule-pack format and the import/export merge, cited by `core::packs`. |
| [portability-audit.md](portability-audit.md) | The portability findings the code is built against; `core::db`, `core::lib`, `worker` and the README all cite its numbered findings. Its dated sections carry notes where reality has since moved on. |
| [ui-redesign-plan-2026-07-31.md](ui-redesign-plan-2026-07-31.md) | Cited by `ui::harness` and `ui::settings` for the layout decisions they hold stable. |

## Archive

`archive/` holds the project's history: the original concept and
specification, the discussion notes it grew from, the user stories, three
audits (codebase, UI/UX, and the mockup brief), launcher research, the
superseded backlog, and the standalone HTML mockup.

They are kept because they explain *why* things are the way they are, and they
are dated because they were true on the day they were written. Do not read them
as instructions — where an archived document and the code disagree, the code is
right.

## Not here

- **The backlog** lives on a local Kanban board, not in this repository.
- **Release notes** are in [CHANGELOG.md](../CHANGELOG.md).
- **How to contribute** is in [CONTRIBUTING.md](../CONTRIBUTING.md); reporting a
  vulnerability is in [SECURITY.md](../SECURITY.md).
