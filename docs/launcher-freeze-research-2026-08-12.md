# Freezing a trim: what each launcher actually allows

- Research date: 2026-08-12
- Branch: `master`, commit `6a208f0`
- Mode: read-only. Local launcher metadata was read, never written.
- Question: can GameTrimmer stop a launcher from re-downloading the files a trim removed?

## Summary

A true freeze — "these files never come back, whatever the user does" — is not
available on any launcher without lying to that launcher about what is
installed. But the problem behind the request is narrower than it looks, and
most of it is solvable read-only:

1. **Restoration is almost never automatic.** On every launcher examined, the
   full content check that restores deleted files is *user-initiated*
   (Verify / Repair / Scan and Repair). Ordinary patches are delta-based and do
   not restore files the patch does not touch.
2. **Every launcher exposes a local version key**, so "your trim was undone"
   is detectable offline, read-only, for the price of reading a few small
   files. This is the high-value, zero-risk half of the feature.
3. **Three launchers have a native per-language mechanism** (Steam depots,
   Battle.net language packs, Riot locales) that is strictly better than
   deleting: the launcher itself stops considering that content installed, so
   it survives even a full verify.

The recommended shape is therefore **detect and re-apply**, not prevent, with
native levers recommended where they exist.

## When a launcher writes into a game folder

The model is the same everywhere:

| Trigger | Restores deleted files? | User-initiated? |
|---|---|---|
| Delta patch / update | Only files the patch itself touches | no |
| Verify / Repair / Scan and Repair | **Yes, all of them** | yes |
| Interrupted update, crash during patch, incomplete-install flag | Usually yes (implicit validation) | no |
| First install / move / repair | Yes | yes |

The consequence: a trim survives indefinitely for a user who never verifies and
whose games do not patch those specific files. That is most users most of the
time — which is why the behaviour reads as random rather than as a rule.

## Candidate mechanisms

| Mechanism | Survives patch | Survives verify | Cost |
|---|---|---|---|
| Detect + re-apply the trim | n/a | n/a | one single-game scan; **no writes outside our own folder** |
| Native per-language lever | ✓ | ✓ | only where the launcher offers it; needs a content→language map |
| Sparse / zero-byte placeholder | better than deletion | ✗ | the game reads zeros instead of hitting a clean "file not found" → crash risk **higher than plain deletion** |
| Deny-write ACL on path | ✓ | ✓ | launcher update fails hard (Steam: "missing file privileges"); leaves state outside our folder |
| Editing the launcher's manifest | ✓ | ✓ | launcher rewrites it at will; a wrong depot entry re-downloads the whole game |

Placeholders deserve an explicit warning: they look like the elegant answer and
are not. `ondisk.rs` already measures sparse files correctly via
`GetCompressedFileSizeW`, so the plumbing would fit — but replacing an asset
with zeros converts a well-handled missing-file path into an
undefined-content path. Deleting is the safer destructive option.

## Per-launcher findings

Rows marked **(local)** were verified against real metadata on the development
machine; the rest are from documentation and are marked as needing
confirmation against a live install.

| Launcher | Version key for change detection | Where | Native per-language / optional lever |
|---|---|---|---|
| Steam **(local)** | `buildid`, `TargetBuildID` | `steamapps/appmanifest_*.acf` | per-language depots |
| Epic **(local)** | `AppVersionString`, `bNeedsValidation`, `bIsIncompleteInstall` | `%ProgramData%\Epic\EpicGamesLauncher\Data\Manifests\*.item` | `InstallTags` (selective install) |
| Amazon **(local)** | `InstallVersion` + `LastKnownLatestVersion` | `GameInstallInfo.sqlite`, table `DbSet` | none found |
| itch **(local)** | `caves.build_id`, `builds.version` | `%APPDATA%\itch\db\butler.db` | none needed (plain folders) |
| Humble **(local)** | `downloadedVersion` + `latestBuildVersion` | `%APPDATA%\Humble App\config.json` | none needed (plain folders) |
| Riot **(local)** | `patching_policy`, `locale_data` | `%ProgramData%\Riot Games\Metadata\*.product_settings.yaml` | explicit locale list; `patching_policy: "manual"` |
| Ubisoft **(local)** | per-install `language` value | `HKLM\SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs\<id>` | per-install language |
| Rockstar **(local)** | none found | registry `InstallFolder` only | none found |
| Xbox / MS Store **(local)** | n/a | `.GamingRoot` | **out of scope** — see below |
| GOG Galaxy | `version` in `goggame-<id>.info` | game folder | `language` field; offline installers never auto-update |
| EA app | not found in registry alone | `HKLM\...\EA Desktop\InstalledGames` | none found |
| Battle.net | not examined | Agent / product database | per-language packs in the launcher UI |

### Steam

The richest surface, and the only one where a lever needs real work.

`appmanifest_*.acf` carries more than the build id we already record. Two
fields matter and are currently unread:

- **`FullValidateAfterNextUpdate`** — observed as `"1"` on a live install. When
  set, Steam runs a *full* validation after the next update, which will undo a
  trim regardless of what the patch touched. Reading this read-only turns
  "your trim may not survive" from a guess into a fact we can state before the
  user deletes anything.
- **`AutoUpdateBehavior`**, **`ScheduledAutoUpdate`** — tell us how soon an
  update is likely, i.e. how durable a trim is for this game.

The native lever is per-language depots. Steam depots carry a language
attribute; content only downloads for users running Steam in that language
([Steamworks docs](https://partner.steamgames.com/doc/store/application/depots)).
Where a game's localization is a separate depot, setting the game's language in
Steam makes Steam itself uninstall the others — permanently, surviving verify,
with no file surgery. That is strictly better than deleting those files.

The depot→language map is **available offline**: `Steam/appcache/appinfo.vdf`
(10.8 MB on the dev machine). Its header magic is `0x07564429`, i.e. **binary
VDF v29, which stores keys in a shared string table at the end of the file**
rather than inline — confirmed by probing: `depots`, `language`, `optional`,
`baselanguages`, `dlcappid` and `sharedinstall` each occur exactly once in the
file. Reading it means writing a v29 parser with string-table support. That is
bounded, offline, read-only work — but it is real work, and the format is
undocumented and has changed versions before, so it needs a version guard and
a graceful "cannot read this, say nothing" path.

### Epic

`AppVersionString` is a clean equivalent of Steam's build id, and Epic also
publishes its own `bNeedsValidation` and `bIsIncompleteInstall` flags — i.e.
Epic tells us directly when it is about to restore everything. `InstallTags`
is Epic's selective-install mechanism (the field exists on real manifests, and
is empty for titles that do not use it); worth a look as a native lever, but
only some titles populate it.

### GOG Galaxy

The most freeze-friendly launcher. Verify/Repair is manual only, and GOG's
offline installers mean a user can opt out of updates entirely. `goggame-<id>.info`
is plain JSON carrying a version and a `language` field. Needs confirmation
against a live Galaxy install — GOG is not installed on the dev machine.

### Riot

Unusual: `patching_policy: "manual"` and `auto_patching_enabled_by_player: false`
were both observed on a live install, alongside `locale_data` with
`available_locales` and the active `locale`. So Riot exposes both a patching
policy and a locale list we can read. Riot's patcher is otherwise aggressive,
so reading the policy before recommending a trim matters more here than
elsewhere.

### Xbox / Microsoft Store — out of scope

`C:\Program Files\WindowsApps` is owned by `NT SERVICE\TrustedInstaller`
(verified locally). GameTrimmer cannot write there, and anything changed is
reverted by the store. No freeze mechanism applies. Recommend documenting this
as a known limitation rather than attempting anything.

### Correction on EA and Ubisoft

An early secondary source suggested the EA app repairs missing files
automatically. Follow-up research does not support that: both the EA app and
Ubisoft Connect require the user to start Scan and Repair / Verify Files
manually. This moves both from "hopeless" to "same as everyone else" and is
the reason they are not excluded below.

## What is already built

`gamestate::changed_games` in `crates/core/src/gamestate.rs` is complete and
fully unit-tested: it compares stored build ids against current manifest state
and reports `Updated` / `Uninstalled`, conservatively refusing to claim a
change it cannot evidence. `build_id` is written on every scan by
`worker/scan/persistence.rs`.

**It has no production caller.** `crates/app/src/worker/scan.rs:1815` says so
in as many words: *"record now, show later. Nothing in the UI reads `build_id`
yet."*

So the highest-value piece of this feature is wiring, not construction.

## Recommendation

**MVP (agreed scope): detection only.** Wire `changed_games` to the UI so a
game whose build id moved says so. No writes to any launcher, no README change,
no new panel — the existing per-game row is where this belongs.

**Next, still read-only:** read `FullValidateAfterNextUpdate` and the Epic /
Riot equivalents, so the app can warn *before* a trim that this particular game
will not keep it.

**Then:** re-apply. Store the trim as a recipe and offer one click to redo it
on a game that came back. From the user's side this is the freeze they asked
for, at zero risk.

**Only after that, and only behind explicit per-game consent** (the agreed
position on the invariant): native levers first — recommend Steam's language
setting or Battle.net's language packs rather than deleting. Direct writes into
launcher folders (ACLs, manifest edits) stay last and stay optional; they
require replacing the README's "nothing is ever written to their registry keys
or folders" with an honest description and a guaranteed unfreeze.

## Sources

- [Depots — Steamworks Documentation](https://partner.steamgames.com/doc/store/application/depots)
- [A Complete Guide to Repairing Game Files on Different Launchers — MakeUseOf](https://www.makeuseof.com/how-to-verify-game-file-integrity-different-launchers/)
- [Verifying game files in Ubisoft Connect PC — Ubisoft](https://www.ubisoft.com/en-us/help/connectivity-and-performance/article/verifying-game-files-in-ubisoft-connect-pc/000060529)
- [My game data is corrupt. How can I repair my game? — GOG Support](https://support.gog.com/hc/en-us/articles/360003930017-My-game-data-is-corrupt-How-can-I-repair-my-game)
- [GOG offline installers — gog-galaxy-dev-docs](https://github.com/gogcom/gog-galaxy-dev-docs/blob/master/docs/offline-installers.md)
- [How to manually edit your appmanifest file — Steam Community guide](https://steamcommunity.com/sharedfiles/filedetails/?id=3517757180)
- [Game detection (Epic manifests) — Nexus Mods Vortex wiki](https://github.com/Nexus-Mods/Vortex/wiki/MODDINGWIKI-Developers-General-Game-detection)
- Local metadata on the development machine: Steam `appmanifest_228980.acf` and
  `appcache/appinfo.vdf`; Epic `Data\Manifests\*.item`; Riot
  `teamfighttactics.live.product_settings.yaml`; Amazon `GameInstallInfo.sqlite`;
  itch `butler.db`; Humble `config.json`; `WindowsApps` ACL owner.
