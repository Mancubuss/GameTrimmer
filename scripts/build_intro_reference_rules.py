"""Turn the PCGamingWiki intro harvest into ``game_reference.json``.

Why this script exists
----------------------

GameTrimmer's intro rules are heuristics: ``^(.*[_. -])?logos?.*\\.bik$`` is a
pattern someone wrote because startup videos tend to be named that way. For a
game nobody has catalogued, guessing is the only option there is.

For the games PCGamingWiki *has* catalogued, guessing is the worse answer.
The wiki names the intro videos file by file, and most of the names it gives
are invisible to the main logo heuristic - ``Prey (2017)`` ships
``ArkaneLogoAnim_Redux_1080p2997_ST-16LUFS.bk2``, where "logo" sits inside a
word with no separator in front of it.

What turns a harvested file list into an entry is the game's store id: the
harvester reads ``steam appid`` and ``gogcom id`` off the page's infobox, and
those are the same ids the Steam and GOG providers put in ``games.app_id``, so
the entry binds the list to one game and to no other. A page with no store id
yields no entry - a title alone is not an identity ("Prey" is two different
games).

Why a table and not rules
-------------------------

This script used to write these lists into ``rules.json`` as one regex per
game (``"pattern": "^(intro_ea\\\\.bik|legal\\\\.bik)$"``). All 935 of those
patterns were literal alternations - not one contained a regex metacharacter -
and the engine paid 156 ms per scan compiling them, plus 327 KB of ``rules.json``
mostly spent on 935 copies of the same sentence in two languages. The catalogue
is a table, so it is written as one; ``rules.json`` goes back to holding only
the rules a person writes by hand. The description is generated from the title
by ``core::reference::intro_desc`` and is not stored here.

Conservative by construction:

* only pages whose fix is ``delete_or_replace_files`` - the wiki telling us to
  remove these files, not merely mentioning them;
* only pages that name a store id, and only that page's own ids;
* ``steam appid side`` is not used: those ids are DLC and regional SKUs that
  share the base game's folder, so an entry bound to one would claim a game
  the wiki never described.

Idempotent: the catalogue is rebuilt from scratch on every run, and any
leftover ``"origin": "reference"`` rule still sitting in ``rules.json`` from
the old shape is dropped. Hand-written rules are left untouched.

Rebuilt from scratch is exactly why the hand-written half lives in its own
file. ``game_reference_local.json`` holds the games the wiki has no page for,
and the startup videos it did not list; this script must never open it, in
either direction. The two are merged at load time by ``core::reference``.

Standard library only. Run from the repository root:

    python scripts/build_intro_reference_rules.py
"""

import json
import sys

DATASET_PATH = "docs/pcgw_skip_intro_dataset.json"
RULES_PATH = "rules.json"
REFERENCE_PATH = "game_reference.json"

# Named here only so it is obvious that it is *not* used below. The
# hand-written half of the catalogue is nobody's output; a regeneration that
# touched it would throw away the entries that exist precisely because the
# harvest cannot produce them.
NEVER_WRITTEN_BY_THIS_SCRIPT = "game_reference_local.json"

# Mirrors `core::reference::GAME_REFERENCE_VERSION`.
GAME_REFERENCE_VERSION = 1

# The fix method that means "these files are the intro, remove them". A page
# that only documents a launch option or a config edit names its files in
# passing, and an entry built from that would delete on the strength of a
# mention.
REQUIRED_METHOD = "delete_or_replace_files"


def build(records):
    """The catalogue the dataset supports, and the counts to report.

    One entry per store id. A game sold on both Steam and GOG produces two
    entries with the same title and file list, because the two installations
    carry two different ids and either may be the one on disk.
    """
    by_id = {}
    linked_titles, covered_names = set(), set()
    for record in records:
        if REQUIRED_METHOD not in record["methods"] or not record["video_files"]:
            continue
        names = sorted(set(record["video_files"]))
        bound = False
        for app_id in (record.get("steam_appid"), record.get("gog_id")):
            if not app_id:
                continue
            existing = by_id.get(app_id)
            if existing is None:
                by_id[app_id] = {
                    "app_id": app_id,
                    "title": record["title"],
                    "intro_files": names,
                }
            else:
                # Two wiki pages naming one store id. The engine refuses a
                # duplicate rather than letting one list vanish, so merge here
                # and say so - a silent union would hide a harvest bug, and a
                # crash on a real game would hide the catalogue.
                merged = sorted(set(existing["intro_files"]) | set(names))
                print(
                    f"  merged: {app_id} claimed by "
                    f"{existing['title']!r} and {record['title']!r} "
                    f"({len(existing['intro_files'])} + {len(names)} "
                    f"-> {len(merged)} files)",
                    file=sys.stderr,
                )
                existing["intro_files"] = merged
            bound = True
        if bound:
            linked_titles.add(record["title"])
            covered_names.update(name.lower() for name in record["video_files"])

    games = sorted(by_id.values(), key=lambda entry: (entry["title"], entry["app_id"]))
    return games, linked_titles, covered_names


def check_ascii(games):
    """The matcher folds case as ASCII; a name it cannot fold is a hard stop.

    Refusing here rather than in Rust means a bad harvest fails the person
    running the generator, who can look at the page, instead of failing a
    user's scan.
    """
    offenders = [
        (entry["title"], name)
        for entry in games
        for name in entry["intro_files"]
        if not name.isascii()
    ]
    if offenders:
        for title, name in offenders:
            print(f"  non-ASCII file name in {title!r}: {name!r}", file=sys.stderr)
        raise SystemExit(
            f"{len(offenders)} non-ASCII file names; core::reference refuses these - "
            "teach the matcher to fold them, or drop the pages"
        )


def main():
    with open(DATASET_PATH, encoding="utf-8") as handle:
        records = json.load(handle)
    with open(RULES_PATH, encoding="utf-8") as handle:
        pack = json.load(handle)

    games, linked_titles, covered_names = build(records)
    check_ascii(games)

    # Any `origin: reference` rule left in rules.json is from the shape this
    # script replaced. Dropped here so one run migrates a pack that still has
    # them, and so a re-run on an already-migrated pack changes nothing.
    kept = [rule for rule in pack["rules"] if rule.get("origin") != "reference"]
    dropped = len(pack["rules"]) - len(kept)
    if dropped:
        pack["rules"] = kept
        with open(RULES_PATH, "w", encoding="utf-8", newline="\n") as handle:
            json.dump(pack, handle, ensure_ascii=False, indent=2)
            handle.write("\n")

    with open(REFERENCE_PATH, "w", encoding="utf-8", newline="\n") as handle:
        json.dump(
            {"version": GAME_REFERENCE_VERSION, "games": games},
            handle,
            ensure_ascii=False,
            indent=2,
        )
        handle.write("\n")

    all_names = set()
    for record in records:
        all_names.update(name.lower() for name in record["video_files"])
    print(
        f"{len(records)} pages, {len(linked_titles)} linked to a store id, "
        f"{len(games)} catalogue entries, "
        f"{len(covered_names)}/{len(all_names)} named files covered, "
        f"{len(kept)} hand-written rules kept"
        + (f", {dropped} old reference rules dropped" if dropped else "")
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
