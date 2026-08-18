# GT-28 spike: compressing a game install instead of deleting from it

- Research date: 2026-08-13
- Branch: `master`, commit `41a8841`
- Mode: no game file was written, moved or re-attributed. Every measurement was
  taken on scratch copies in `G:\_gt_wof` / `C:\_gt_spike`, all removed afterwards.
- Question from the card: *why* do compressed games stop updating, and does a
  provably safe subset exist?

## Verdict

**Refuted. Close the card, as agreed for read-only placeholders.**

The design document's premise — "compression is the only risk-free saving,
because Verify passes: content and hash are unchanged" — is wrong twice over,
and the lived experience behind the card is now explained rather than merely
believed.

Three findings, in order of how decisively each one kills the direction:

1. **On this machine the classic mechanism does not exist at all.** Every game
   volume is formatted with 64 KiB clusters, and NTFS compression (LZNT1,
   `compact /c`) refuses to run above 4 KiB. Verified below.
2. **The mechanism that does work reverts itself.** WOF / Compact-OS
   compression — the kind CompactGUI applies — is transparent for reads only.
   The first write decompresses the *whole file*, permanently. Microsoft
   documents this; we reproduced it.
3. **Both kinds disable DirectStorage's BypassIO fast path**, per Microsoft's
   own driver documentation. So even a hypothetically stable compression costs
   load time on exactly the modern titles users care about.

## Two different things are called "compression"

Conflating them is why the source document reached the wrong conclusion.

| | NTFS compression (LZNT1) | WOF / Compact OS (XPRESS, LZX) |
|---|---|---|
| Applied by | `compact /c` | `compact /c /exe:LZX`, CompactGUI |
| Transparent for | reads **and** writes | reads only |
| On write | stays compressed | **whole file silently decompressed** |
| Cluster-size limit | **≤ 4 KiB only** | none — file-level |
| Blocks BypassIO/DirectStorage | yes (NTFS refuses) | yes (`wof.sys` vetoes) |

## Finding 1 — LZNT1 is unavailable on this machine's game drives

`Win32_Volume.BlockSize` on the development machine:

| Volume | Label | Cluster | LZNT1 possible |
|---|---|---|---|
| C: | SSD: Windows | 4 096 | yes |
| E: | HDD: Archive | 8 192 | no |
| F: | HDD: Games | 65 536 | **no** |
| G: | SSD: Games | 65 536 | **no** |
| H: | HDD: Backup | 65 536 | no |

Running `compact /c /f` on a 64 MiB scratch file, same file, two volumes:

```
G: (65536 B clusters)  ->  the cluster size of the volume is larger than 4096 bytes.
                           0 files within 1 directories were compressed.
                           attributes after: Archive, NotContentIndexed
C: (4096 B clusters)   ->  zeros.bin  67108864 : 0 = 1,0 to 1 [OK]
                           attributes after: Archive, Compressed
```

The attribute is not merely ineffective on the 64 KiB volume — it is never set.
Since 64 KiB is a common choice for large game drives, any feature built on
LZNT1 would be silently unavailable for a large share of users, with the
diagnosis buried in a `compact` exit code.

## Finding 2 — WOF compression undoes itself on the first write

Microsoft, *Using Compact OS to optimize storage footprint*:

> The `/EXE:<compression algorithm>` option is optimized for executables or
> read-only files […] If files compressed with this option are ever opened for
> write, they will automatically be decompressed. The installer of these custom
> program files is responsible for detecting the files were compressed […] and
> must re-compress them after overwriting them.

No launcher does that, because no launcher knows WOF exists. Raymond Chen
confirms the behaviour in plainer words and adds the important caveat that it
"is not contractual" — it is observed behaviour, not an API guarantee, so it
cannot even be relied on to keep behaving this way.

Reproduced locally on a 330 MB `.esm` compressed with `/exe:LZX`, then appended
with 1 MB:

| | logical | on disk | state |
|---|---|---|---|
| before append | 330.78 MB | 211.16 MB | 1 file compressed |
| after +1 MB | 331.82 MB | 331.82 MB | 0 compressed, 1 not compressed |

One megabyte of writing destroyed 120 MB of saving. The same append against an
LZNT1-compressed copy kept its ratio, as documented.

CompactGUI's own wiki names the consequence — "compression decay": the folder
"will slowly but surely grow in size until it is back to its original size."
Its Background Watcher re-compresses after updates, which is a treadmill, not a
fix, and its issue tracker records that treadmill racing a live Steam update
into actual corruption (#591: ~15 files stuck flagged-but-not-compressed, fixed
only after 15 sequential integrity passes; #551: Helldivers 2 reinstalled).

So the card's phrasing — "games stop updating" — resolves into three distinct
observed failures: savings evaporate silently (decay), some titles hang while
decompressing themselves at launch (Guild Wars 2, Ghost Recon Wildlands, LOTRO
are named in CompactGUI's own megathread), and a compression pass racing a
patch can corrupt files outright.

## Finding 3 — both kinds disable DirectStorage

Microsoft, *BypassIO for Filter Drivers*: "You can't enable NTFS compression on
a BypassIO active file", and the same page's worked example shows `wof.sys`
itself refusing BypassIO. Two different mechanisms, same outcome. CompactGUI's
README states the rule bluntly: do not use it on DirectStorage titles.

An indicative local read test (317 MB video, warm cache, so treat as a ceiling
on the ratio rather than a real-world figure): 3 703 MB/s uncompressed versus
1 032 MB/s WOF-compressed.

## Finding 4 — fragmentation, the long-tail failure

KB967351 documents that a file's on-disk layout is described by a bounded
number of extent records, and that "compressed files are more likely to reach
the limit […] decompressing and compressing a file increases fragmentation
significantly", with the wall typically between 40 GB and 90 GB for a very
fragmented file. Hitting it fails writes with `ERROR_FILE_SYSTEM_LIMITATION`
regardless of free space. Compress→patch→decompress→recompress churn on large
game archives is precisely the workload that walks toward that limit. This is a
documented failure *mode*; no field report we found proves it as the root cause
of a specific game's failure, and it is presented here as risk, not as diagnosis.

## What compresses, when it works

Measured on scratch copies of real assets from this machine's libraries; LZNT1
figures taken on C:, WOF on G:.

| Category | Sample | LZNT1 saving | WOF/LZX saving |
|---|---|---|---|
| shader package `.pak` | 157.9 MB | 56.5 % | 84.6 % |
| executable `.exe` | 186.3 MB | 33.6 % | 60.7 % |
| game database `.esm` | 315.5 MB | 26.2 % | 36.1 % |
| texture archive `.vpk` | 203.5 MB | 22.6 % | 44.2 % |
| library `.dll` | 74.2 MB | 18.9 % | 22.0 % |
| packed media `.pck` | 203.3 MB | 0.1 % | 0.2 % |
| video `.bk2` / `.mp4` | 407.6 MB | ~0.3 % | 0.4–2.2 % |
| audio `.wav` | 5.8 MB | 0.4 % | 1.5 % |

Caveat on these numbers: nine files, five categories, one machine. `.dds`,
`.wem`, `.ubulk` and `.locres` had no samples taken and are unmeasured. The
headline "43 % overall" that falls out of this sample is carried almost
entirely by one shader pack and should not be quoted as a general figure.

The shape is nonetheless clear and matches expectation: compression pays on
code, shaders and uncompressed databases, and pays nothing on the video, audio
and packed archives that dominate install size — which is exactly the content a
trimming tool is looking at.

## Answering the card's questions

**Why does updating break?** Not because Verify fails on content — content and
hash are genuinely unchanged. It breaks because WOF compression is a property
*outside* the file's content that no launcher preserves: the first write throws
it away (decay), a re-compression pass racing a patch corrupts files, and some
games decompress their own install at launch and hang. LZNT1, which does not
have this problem, is unavailable on 64 KiB volumes and still costs DirectStorage.

**Is there a provably safe subset?** Only a subset that cannot be identified in
advance. Files that are never rewritten are safe by definition, but nothing —
no launcher manifest, no OS facility — tells us before a patch which files that
patch will touch. "Safe by launcher" is unsupportable: no primary source
distinguishes launchers here, because the failure is about which files get
rewritten, not who rewrites them. The only honestly safe target is content
belonging to a title that will never be patched again, which the user knows and
the tool does not.

Anti-cheat interaction was investigated and **nothing was found** from any
vendor. The occasional claim that anti-cheat flags compressed files is
uncorroborated and should not be repeated in our documentation.

## Consequences for GameTrimmer

1. **Close GT-28 as refuted.** Do not build compression, and do not ship a
   "compress instead of delete" option.
2. **One small, real deliverable falls out of this**, worth its own card:
   `ondisk.rs` should be trusted on compressed installs. Measured on a
   WOF-compressed 256 MB scratch file, `GetCompressedFileSizeW` returned
   0.75 MB against 256 MB logical, matching `compact`'s own 341:1 ratio — so
   the existing `on_disk_size` already reports honest physical sizes for users
   who compressed their games with CompactGUI, and our "freed space" arithmetic
   is not inflated for them. Single sample; worth a unit test pinning it.
3. **Say something when we detect it.** A user whose install is WOF-compressed
   is on a treadmill they may not know about. Detecting the state read-only and
   naming it is in scope for a tool that already explains what it finds; acting
   on it is not.

## Sources

- [Using Compact OS to optimize storage footprint — Microsoft](https://learn.microsoft.com/en-us/windows/iot/iot-enterprise/optimize/compactos)
- [BypassIO for Filter Drivers — Microsoft](https://learn.microsoft.com/en-us/windows-hardware/drivers/ifs/bypassio)
- [KB967351 — A heavily fragmented file in an NTFS volume may not grow beyond a certain size](https://support.microsoft.com/en-us/help/967351/a-heavily-fragmented-file-in-an-ntfs-volume-may-not-grow-beyond-a-cert)
- [What is WofCompressedData? — Raymond Chen, The Old New Thing](https://devblogs.microsoft.com/oldnewthing/20190618-00/?p=102597)
- [compact command reference — Microsoft](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/compact)
- [CompactGUI wiki, "Important Information"](https://github.com/IridiumIO/CompactGUI/wiki/Important-Information)
- [CompactGUI #101 — known-bad titles megathread](https://github.com/IridiumIO/CompactGUI/issues/101)
- [CompactGUI #423 — DirectStorage warning](https://github.com/IridiumIO/CompactGUI/issues/423)
- [CompactGUI #551 — Helldivers 2 decompression failures](https://github.com/IridiumIO/CompactGUI/issues/551)
- [CompactGUI #591 — Dota 2 update race, corruption](https://github.com/IridiumIO/CompactGUI/issues/591)
- Local measurements on the development machine: `Win32_Volume.BlockSize` for
  C:/E:/F:/G:/H:; `compact /c /f` on G: and C:; `/exe:LZX` append test;
  `GetCompressedFileSizeW` against a WOF-backed file.
