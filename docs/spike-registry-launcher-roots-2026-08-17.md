# Spike: default roots of registry-based launchers (GT-24)

## What the spike asked

GameTrimmer detects orphaned installs via a **container diff**
(`gametrimmer_core::orphans::find_orphans`): every immediate subfolder of a
launcher's managed container that has no matching manifest entry is reported
as residue. `orphan_spec_for` (`crates/app/src/worker/scan/orphan_analysis.rs`)
currently wires this up for Steam, Xbox and itch only. It deliberately skips
the seven registry-based providers — Epic, GOG, EA, Ubisoft, Battle.net,
Rockstar, Riot — plus Humble, because their discovered `library.path` is just
the parent of wherever the user chose to install, not a folder the launcher
owns exclusively. Diffing there risks flagging the user's own unrelated
folders, which the codebase's fail-closed stance on orphan-residue safety forbids.

Two questions, per launcher:

1. Does it have a **known default root** — a path the installer picks by
   default that, unmodified, is guaranteed to hold only that launcher's games?
2. Does it leave a **structural ownership marker** inside each game folder —
   the way itch drops `.itch` — that proves "this folder belongs to launcher
   X" without consulting the launcher's own manifest?

## Method and what evidence was actually available

Read first, as instructed: all seven provider modules
(`crates/core/src/providers/{epic,gog,ea,ubisoft,battlenet,rockstar,riot}.rs`),
`humble.rs` and `itch.rs` for the working precedent, and `orphans.rs` /
`orphan_analysis.rs` for exactly what the container diff requires to stay safe.

This machine (Mancubus's dev box) has **Origin/EA, Ubisoft Connect, Rockstar
Games Launcher, and the Riot Client** with real installs. It does **not**
have Epic, GOG, or Battle.net installed. Evidence is graded accordingly:

- **Live-verified** = read directly off this machine's registry and/or disk
  during this spike.
- **Documented** = from vendor support pages, community wikis, or third-party
  tooling repos found via web search; never presented as verified.

### Live findings

- **EA** (`Origin Games` HKLM key present, two uninstall-registry entries with
  `Publisher` starting `Electronic Arts`): install locations are
  `H:\EA\Battlefield 3` and `F:\SteamLibrary\steamapps\common\Dragon Age
  Inquisition` — the second one **physically inside Steam's own
  `steamapps\common`**, alongside `steam_appid.txt` and `installScript.vdf`.
  This is live, first-hand proof that EA's install root is not just
  "user-changeable" but can already overlap another launcher's exclusive
  container on this exact machine. Both folders contain
  `__Installer\installerdata.xml` at the root (already read as best-effort
  metadata by `ea.rs::refine_name_from_installer_xml`) — present in both
  samples.
- **Ubisoft Connect** (`HKLM\...\Ubisoft\Launcher\Installs`): games live at
  `F:\Ubisoft\*` (a custom drive, not a Program Files default). Checked two
  installs (Brawlhalla, For Honor): both root folders contain
  `uplay_install.manifest`, `uplay_install.state`, and `uplay_download\`.
  Same three names, same location, in both samples.
- **Riot Client** (`%ProgramData%\Riot Games\Metadata`): VALORANT's
  `product_settings.yaml` declares `product_install_root: "H:/Riot Games"` —
  a custom drive, not the documented `C:\Riot Games` default, so the user
  changed it. Checked the install folder tree (`H:\Riot Games\VALORANT\live`)
  and the declared root itself: no Riot-specific file anywhere — only
  `Manifest_DebugFiles_Win64.txt` / `Manifest_NonUFSFiles_Win64.txt` /
  `Manifest_UFSFiles_Win64.txt`, which are generic Unreal Engine build
  manifests any UE game ships, not a Riot ownership marker.
- **Rockstar Games Launcher** (`HKLM\...\Rockstar Games`): only the
  `Launcher` and `Social Club` subkeys exist with real `InstallFolder`
  values; a `Steam` subkey exists but its `InstallFolder` is empty (GTA V is
  Steam-installed, not natively Rockstar-installed here). No live Rockstar
  game folder was available to inspect — verdict below is documentation-only.
- **Epic, GOG, Battle.net**: registry keys/uninstall entries absent —
  nothing installed on this machine. Verdicts are documentation-only.

## Per-launcher table

| Launcher | Default root | Root confidence | Structural marker | Marker confidence | Container-diff verdict |
|---|---|---|---|---|---|
| Epic | `C:\Program Files\Epic Games\<Game>` (a per-user *changeable default*, not fixed) | Documented (Epic support pages) | `.egstore\` folder inside every install dir (installer/manifest state Epic itself reads to recognize the install) | Documented (Epic support/community sources); not seen on this machine | **Unsafe today, extendable** if the marker holds up — see below |
| GOG (Galaxy) | `C:\GOG Games\<Game>` (documented default; Galaxy lets the user repoint it, and legacy GOG installers used other defaults) | Documented (GOG support forum) | `goggame-<id>.info` (+ `.ico`) file at the root of every install dir; deleting it is GOG's own documented way to make Galaxy "forget" a game | Documented (GOG forum, third-party Galaxy plugins reading this file) | **Unsafe today, extendable** |
| EA (Origin / EA app) | None — installer offers a folder, but this machine shows a live install placed *inside another launcher's own container* | **Live-verified: no exclusive or even non-overlapping root** | `__Installer\installerdata.xml` present at the root of both live-verified installs | **Live-verified in 2/2 samples** — but `ea.rs`'s own comment treats it as optional/best-effort, not guaranteed universal | **Unsafe today, extendable if the marker is confirmed reliable across more samples** |
| Ubisoft Connect | `C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games` (documented default); this machine has it redirected to `F:\Ubisoft` | Documented default; **live-verified as user-overridden** | `uplay_install.manifest` + `uplay_install.state` (+ `uplay_download\`) at the root of every install dir | **Live-verified in 2/2 samples** (Brawlhalla, For Honor) | **Unsafe today, extendable** |
| Battle.net | Blizzard gives each game its *own* top-level default folder (e.g. `C:\Program Files (x86)\Diablo IV`) — there is no single shared default container to begin with | Documented; structurally there is no common root even in principle | `.build.info` file at the root of every install dir — the file the game engine itself requires to start | Documented (community sources; Diablo III's own error message references it) | **Unsafe today** for the "shared container" scheme; the missing-common-root shape is a separate, harder problem (see follow-up) |
| Rockstar Games Launcher | `C:\Program Files\Rockstar Games\<Game>` (documented default) | Documented; **no live install to check** | Not established | Not researched — no vendor/community source located confirming a marker, and no live sample | **Unresolved** — treat as unsafe/undetectable until a marker is found |
| Riot Client | `C:\Riot Games` documented default; `product_install_root` is explicitly declared per product in `%ProgramData%\Riot Games\Metadata\...\product_settings.yaml` — this machine has it redirected to `H:\Riot Games` | Documented default; **live-verified declared-root mechanism, live-verified as user-overridden** | **None found** — checked both the per-game folder and the declared root itself; only generic Unreal Engine manifest files, which any UE game (not just Riot's) would also carry | **Live-verified absent** | **Fundamentally undetectable** with the current marker-based scheme |

A cross-cutting note that applies to every row above except Riot: **the
"default root" question turns out to be a soft signal, not the deciding
one.** `orphans::OrphanScanSpec.ownership_markers` (see its doc comment in
`orphans.rs`) already lets a container be *user-chosen and shared* as long as
a marker proves per-folder ownership — that's exactly how itch is handled,
and `library.path` for every one of these seven providers is already
discovered dynamically via `group_by_parent_dir` (or, for Riot, the declared
`product_install_root`), not a hardcoded default. So a launcher does not need
a *fixed* default root to be extendable — it needs a marker. Riot is the one
launcher here where a "root" is explicitly available yet extension is still
blocked, because the marker is what's actually missing.

## The Paradox note

GT-25 confirmed, against a live install on this machine, that Paradox
Interactive's launcher answers **both** questions positively: its
`gameLibraryPaths` config resolves to `%APPDATA%\Paradox Interactive\games` —
a container inside the launcher's *own* AppData tree, which end users do not
normally redirect, making it structurally close to Steam/Xbox rather than to
this batch — and every installed game additionally carries
`launcher-settings.json` (`{"gameId", "exePath", "gameDataPath",
"isFallbackSettingsFile"}`) as a belt-and-braces marker. Paradox is the
existing proof that a registry/config-based launcher *can* clear the bar this
spike is checking for the other seven; it just isn't one of them.

## Verdict

**Extendable candidates (marker exists, itch-style safety is architecturally
available):**
- **Epic** — `.egstore\` marker, documented only, needs live confirmation.
- **GOG** — `goggame-<id>.info` marker, documented only, needs live
  confirmation.
- **EA** — `__Installer\installerdata.xml`, live-verified 2/2 on this
  machine, but the existing provider code already treats it as
  best-effort/optional, so its universality across the installed base needs
  more samples before it can gate a delete-adjacent decision.
- **Ubisoft** — `uplay_install.manifest` / `uplay_install.state`,
  live-verified 2/2 on this machine, strongest evidence of the batch.

**Fundamentally undetectable with the container-diff + marker scheme:**
- **Riot** — live-verified no marker exists in the install folder or at the
  declared root. The explicit `product_install_root` metadata solves the
  "where is the container" problem but not the "is this folder ours"
  problem, which is the one that actually gates safety.
- **Battle.net** — even setting the marker question aside, there usually
  isn't a *shared* container to diff in the first place: each game gets its
  own top-level default folder, so the itch/Ubisoft-style "one folder, many
  subfolders, diff against manifests" shape doesn't apply by default. A
  `.build.info` marker exists per-game, but the surrounding problem is
  different enough (per-game root, not a shared container) that it likely
  needs its own detection shape rather than reusing `OrphanScanSpec`
  verbatim.
- **Rockstar** — no evidence either way; no marker found in research, no live
  install on this machine to check. Must stay out of scope until someone
  with a real Rockstar install can check.

**Humble** is unchanged by this spike (out of scope — GT-24 was scoped to the
seven registry-based launchers) and stays recorded as undetectable per the
existing code comment: user-chosen `downloadLocation`, no known per-game
marker.

## Sketch of the follow-up ticket(s)

For the four extendable candidates, a follow-up card (or one per launcher —
they're independent and small) would need to:

1. **Confirm the marker on a live install**, for Epic and GOG specifically
   (no machine with either was available here) — same shape as the Paradox
   spike did: install one real game, inspect the folder, quote the exact
   marker file/folder name and its stability across an uninstall-reinstall
   cycle.
2. **Widen the EA sample** beyond 2 installs before trusting
   `__Installer\installerdata.xml` as a hard safety gate — it's currently
   used as soft display-name metadata precisely because it's allowed to be
   absent; confirm whether that absence is rare enough (or detectable enough)
   to safely fall back to "no marker match, don't flag" rather than silently
   trusting an absent-but-actually-owned folder.
3. **Add an `OrphanScanSpec` constructor per launcher** in `orphans.rs`,
   mirroring `itch_spec`: `container = library.path` (already discovered),
   `ownership_markers = vec![PathBuf::from("<marker>")]`. This is a small,
   mechanical addition once the marker is confirmed — the diff engine
   (`find_orphans`, `has_ownership_marker`) needs no changes, it already
   supports non-empty `ownership_markers` generically.
4. **Wire it into `orphan_spec_for`** (`orphan_analysis.rs`) by adding the
   vendor string to the match arm.
5. **Extend the corpus/tests** the way `itch_spec`'s tests do: a live game
   (managed, carries the marker) is spared, a leftover (carries the marker,
   no manifest) is flagged, and a foreign/manual folder in the same shared
   root (no marker) is spared — the itch three-way test in `orphans.rs` is
   the template to copy per launcher.

Riot, Battle.net and Rockstar do **not** get a follow-up card from this
spike — they should be recorded on the board as "researched, no path
forward" so nobody re-investigates the same dead end.
