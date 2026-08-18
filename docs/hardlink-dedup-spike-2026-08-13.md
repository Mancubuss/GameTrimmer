# GT-07 spike: deduplicating identical game files with NTFS hard links

- Research date: 2026-08-13
- Branch: `master`, commit `41a8841`
- Mode: no game file was written, moved, re-attributed or linked. Link
  semantics were established on scratch files in `G:\_gt_spike`, since removed.
  The library measurement read file contents and metadata only.
- Question from the card: do hard links survive a game update and an integrity
  check, and where does deduplication not apply?

## Verdict

**Not refuted, but narrowed to the point where the original framing is the
wrong one.** Two facts decide it:

- **The payoff inside launcher-managed installs is small.** On the richest
  volume on this machine, deduplicating every confirmed duplicate that belongs
  to a launcher-managed game would reclaim **23 GB out of 2 841 GB scanned —
  0.8 %**. A Steam-only library on the second volume yields an upper bound of
  **0.5 %**.
- **The payoff outside them is four times larger and carries none of the risk.**
  Modlist and manual installs — Wabbajack-style "Stock Game" copies of Fallout 4
  and Skyrim — account for **45.8 GB, 56 % of everything found**. No launcher
  ever patches those files, which is precisely why they are safe to link.

So the honest recommendation is not "build hard-link dedup for game libraries".
It is: **report duplicates, do not link them; and if linking is ever built,
build it for modlist copies, not for launcher installs.**

## Part 1 — what a hard link actually does (measured)

Established on scratch files, 8 MiB payload, volume G:. These are mechanical
facts, not opinions, and they define the whole risk model.

| # | Operation | Result |
|---|---|---|
| 1 | `mklink /H` | both paths report the same file index, link count 2 |
| 2 | **in-place write through path A** | **path B's content changes too** |
| 3 | **truncating open through path A** | **path B is now 16 bytes; its 8 MiB asset is gone** |
| 4 | replace-by-rename into A (staging pattern) | A is a new file object, B keeps old content, both link counts drop to 1 — link broken, **saving silently lost, nothing corrupted** |
| 5 | delete A | B survives, link count 1 |
| 6 | attributes / ACLs | shared by every link — a Deny ACE set via B was visible on C |
| 7 | link limit | 1 022 further links succeeded, total 1 024, then failure — matching `CreateHardLinkW`'s documented 1 023-per-file cap plus the original |
| 8 | `Copy-Item` of two links | two independent files; **a backup or a copy destroys the saving** |
| 9 | size accounting | a tree of 1 026 links to one 8 MiB file reports **8 208 MB** to any size-summing tool |

Rows 2 and 3 are the entire danger, and row 4 is the entire mitigation. Whether
deduplication is safe for a given launcher reduces to one question with a binary
answer: **does its patcher write into the existing file, or build a new one and
replace it?**

Row 9 is a separate, immediate consequence for us: **GameTrimmer must count a
hard-linked file once.** If a user has ever deduplicated, our "found" and
"freed" figures are inflated today.

## Part 2 — how much duplication actually exists

Measured on this machine, files ≥ 1 MiB enumerated, files ≥ 8 MiB analysed.
Duplicate candidates were grouped by exact size, then by a SHA-256 of the first
and last 64 KiB, then **confirmed by a full SHA-256 of every member** — 149 GB
read for the confirmation stage on volume G:, no group left unconfirmed.

**Volume G: (SSD, 3 758 GB, 3 305 GB used) — full result**

| | value |
|---|---|
| files enumerated ≥ 1 MiB | 157 623 (2 841 GB) |
| analysed (≥ 8 MiB) | 24 485 |
| **excluded (< 8 MiB)** | **133 138 files holding 355.4 GB — not counted anywhere here** |
| confirmed duplicate groups | 1 281 |
| **confirmed reclaimable** | **81.39 GB — 2.9 % of scanned, 2.5 % of used space** |

Where that 81.39 GB lives — the decisive breakdown:

| path class | groups | GB | share |
|---|---|---|---|
| modlist / manual installs (`G:\Other`) | 1 055 | 45.82 | 56 % |
| launcher-managed install | 158 | 23.01 | 28 % |
| non-game (AI/ML virtualenvs) | 68 | 12.56 | 15 % |

By relationship: across-launcher 30.79 GB, across-game 26.19 GB, within-game
24.41 GB. By type: `.ba2` 19.6, `.bsa` 15.5, `.dds` 15.5, `.dll` 12.8, `.bk2` 7.9 GB.

The largest single opportunities are all the same story — one modlist keeping a
private copy of the base game:

```
1 419.6 MB x3  G:\Other\LoreOut\Stock Game\Data\DLCNukaWorld - Main.ba2
1 724.2 MB x2  G:\Other\LoreRim\Stock Game\Data\Skyrim - Voices_en0.bsa
1 467.4 MB x2  G:\Other\NOLVUS\...\STOCK GAME\Data\Skyrim - Sounds.bsa
   10.7 MB x105 ...\Racial Skin Variance - SPID\...\femalehands_10_msn.dds
```

The `.dll` line is worth naming honestly: 12.56 GB of it is duplicated PyTorch
libraries inside AI toolchain virtualenvs, which are not games and not
GameTrimmer's business. Counting them would flatter the feature.

**Volume F: (HDD, 22 TB, 147 GB free) — partial**

Only the Steam library finished enumerating before the budget ran out
(76 297 files, 1 734.6 GB). Files ≥ 16 MiB, cheap-key stage only, **no full-hash
confirmation** — so this is an upper bound, not a measurement:

| | value |
|---|---|
| duplicate groups (cheap key) | 139 |
| **upper bound reclaimable** | **8.79 GB — 0.5 % of the library** |

F:\Epic, F:\EA, F:\GOG, F:\Ubisoft and F:\Blizzard were **not measured at all**.
That gap is stated rather than papered over; on current evidence it would not
change the conclusion, since F:\SteamLibrary is the cleanest available sample of
a pure launcher-managed library and it is the emptiest of duplication.

## Part 3 — which launchers may be linked

From documentation and open-source clients; no live patch was run (see gaps).

| Launcher | Patch write pattern | Evidence | Link-safe? |
|---|---|---|---|
| **Steam** | stage-and-replace: builds the new file alongside, then swaps it in | vendor doc | **yes** |
| **itch.io / butler** | wharf spec: reconstructs into a staging folder, then merges; untouched files are never opened for write | vendor doc + source | **yes** |
| **GOG Galaxy** | `.tmp` write then `os.rename`, in a protocol-compliant open client | community-technical | likely yes |
| **Battle.net** | Blizzard: "patches are installed into another folder, and once complete, the new patched files replace the old files" | vendor doc | likely yes for loose files; CASC containers make per-file linking mostly moot |
| **Epic** | its own `EInstallMode` documents `DestructiveInstall` — "allowing immediate changes to be made to existing files" — as a supported mode; nothing says the shipped launcher avoids it, and the open-source `legendary` client writes in place | vendor API doc | **no — documented risk, not mere uncertainty** |
| **EA app** | closed, no reimplementation exists | none | undetermined → unsafe |
| **Ubisoft Connect** | no technical documentation | none | undetermined → unsafe |
| **Amazon Games** | only an unfinished third-party client, delta patching "untested" | none | undetermined → unsafe |

Local corroboration for Steam: `steamapps\downloading` and `steamapps\temp`
exist on both libraries on this machine (648 and 663 entries on F:), which is
the staging directory the documented model requires. Epic's `.egstore` folders
exist per game but hold manifests, and their presence says nothing about how
the patcher writes.

## Part 4 — the constraints that bite later

- **1 023 links per file** (`CreateHardLinkW`), confirmed by measurement.
  Irrelevant for game assets, relevant if we ever link small shared runtimes.
- **Copies do not preserve links.** No robocopy flag exists for hard links, and
  Explorer/xcopy materialise independent files. A user who backs up and restores
  a deduplicated library gets the full size back, with no warning.
- **Anti-cheat: no evidence either way.** EAC, BattlEye and Vanguard all appear
  to validate file *content*, and a hard link changes no byte — but no vendor
  documents link-count behaviour, and absence of evidence is not safety. Any
  rollout must be tested against at least one protected title first.
- **DirectStorage** says nothing about hard links anywhere in its docs. Reads as
  indifference, not as clearance.
- **VSS / File History / defrag / Storage Sense**: no vendor statement about
  hard links found. Mechanically they should be fine — one MFT record, many
  directory entries — but this is inference and stays undetermined.
- **Windows Server Data Deduplication is server-only** and unsupported on
  Windows 11 in any edition, so the built-in route does not exist for our users.

## Answering the card's questions

**Do links survive an update and an integrity check on 3+ real games?** Not
tested — and this is the honest gap in this spike. No live patch was applied to
a real install, because that requires modifying real game data. What replaced
that test is stronger for the launchers where it applies: the outcome is fully
determined by the patcher's write pattern, and for Steam and itch that pattern
is documented by the vendor. For Epic, EA, Ubisoft and Amazon the question
remains genuinely open, and Epic's own API documentation makes in-place
overwriting a supported mode — so for those four, a live test is still required
before any link is created.

**Which file types must never be deduplicated?** Beyond the card's own limits
(never configs, saves or logs; same volume only):

- anything a launcher may rewrite — i.e. everything inside an Epic, EA app,
  Ubisoft Connect or Amazon install until each is verified;
- anything under an anti-cheat-protected title, pending a live test;
- files small enough that a 64 KiB cluster makes the saving illusory — every
  game volume on this machine uses 64 KiB clusters, so linking a 10 KiB file
  saves one cluster at best;
- anything the user may copy or back up as a folder, since the saving silently
  does not survive the copy.

## Recommendation

1. **Do not build hard-link deduplication for launcher-managed installs.**
   0.8 % of scanned bytes on the best volume, 0.5 % upper bound on a clean Steam
   library, against a failure whose blast radius is a *different* game's files.
   The arithmetic does not support the risk.
2. **Build detection, not linking.** Reporting confirmed duplicate sets
   read-only costs one full hash of size-matched candidates — 149 GB read and
   three minutes on the volume measured here — and fits what GameTrimmer already
   does: explain what it found and let the owner decide. This is where the card
   should land.
3. **Fix the size accounting regardless.** A hard-linked file must be counted
   once. Today a deduplicated library would inflate both our "found" and our
   "freed" numbers, which is the same class of dishonesty the project has been
   removing everywhere else.
4. **If linking is ever built, scope it to modlist "Stock Game" copies** — 56 %
   of the payoff, no launcher patching those paths, and an audience that already
   understands what it is doing. Treat that as a new card, not as this one.

## Sources

- [Uploading to Steam (SteamPipe) — Steamworks](https://partner.steamgames.com/doc/sdk/uploading)
- [wharf: apply algorithm — itch.io](https://itch.io/docs/wharf/algorithms/apply.html)
- [BuildPatchServices `EInstallMode` — Unreal Engine API](https://docs.unrealengine.com/4.26/en-US/API/Runtime/BuildPatchServices/BuildPatchServices__EInstallMode/)
- [Insufficient Disk Space (patch staging) — Blizzard Support](https://support.blizzard.com/en/article/162452)
- [heroic-gogdl — GOG protocol client, `task_executor.py`](https://github.com/Heroic-Games-Launcher/heroic-gogdl)
- [legendary — unofficial Epic client, writes in place](https://github.com/derrod/legendary)
- [CreateHardLinkW — Microsoft](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-createhardlinkw)
- [robocopy reference — Microsoft](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/robocopy)
- [Data Deduplication overview — Microsoft](https://learn.microsoft.com/en-us/windows-server/storage/data-deduplication/overview)
- [DirectStorage developer guidance — Microsoft](https://github.com/microsoft/DirectStorage/blob/main/Docs/DeveloperGuidance.md)
- [Using Easy Anti-Cheat — Epic Developer docs](https://dev.epicgames.com/docs/game-services/anti-cheat/using-anti-cheat)
- Local measurements on the development machine: link semantics probe on G:;
  duplicate census over `G:\SteamLibrary`, `G:\Epic`, `G:\Other`, `G:\VR`,
  `G:\AI` and `F:\SteamLibrary`; `steamapps\downloading` and `.egstore`
  presence.
