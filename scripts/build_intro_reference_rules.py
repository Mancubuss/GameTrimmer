"""Turn the PCGamingWiki intro harvest into per-game rules in ``rules.json``.

Why this script exists
----------------------

GameTrimmer's intro rules are heuristics: ``^(.*[_. -])?logos?.*\\.bik$`` is a
pattern someone wrote because startup videos tend to be named that way. For a
game nobody has catalogued, guessing is the only option there is.

For the games PCGamingWiki *has* catalogued, guessing is the worse answer.
The wiki names the intro videos file by file, and 1131 of the 1509 names it
gives are invisible to the main logo heuristic - ``Prey (2017)`` ships
``ArkaneLogoAnim_Redux_1080p2997_ST-16LUFS.bk2``, where "logo" sits inside a
word with no separator in front of it.

What turns a harvested file list into a rule is the game's store id: the
harvester reads ``steam appid`` and ``gogcom id`` off the page's infobox, and
those are the same ids the Steam and GOG providers put in ``games.app_id``, so
``Rule::app_id`` binds the list to one game and to no other. A page with no
store id yields no rule - a title alone is not an identity ("Prey" is two
different games).

Conservative by construction:

* only pages whose fix is ``delete_or_replace_files`` - the wiki telling us to
  remove these files, not merely mentioning them;
* only pages that name a store id, and only that page's own ids;
* ``steam appid side`` is not used: those ids are DLC and regional SKUs that
  share the base game's folder, so a rule bound to one would claim a game the
  wiki never described.

Idempotent: every ``"origin": "reference"`` rule already in ``rules.json`` is
dropped and rebuilt, so re-running after a fresh harvest updates the pack
instead of appending to it. Hand-written rules are left untouched.

Standard library only. Run from the repository root:

    python scripts/build_intro_reference_rules.py
"""

import json
import re
import sys
from collections import OrderedDict

DATASET_PATH = "docs/pcgw_skip_intro_dataset.json"
RULES_PATH = "rules.json"

# Mirrors `rules::MAX_REGEX_BYTES`. A game whose file list does not fit gets
# several rules rather than a truncated one.
MAX_REGEX_BYTES = 512

# Mirrors `rules::MAX_RULE_DEPTH`'s use by the intro category: the built-in
# intro rules cap at 4, which is right for a broad pattern that must not reach
# into an asset tree. An exact file name in one named game is not that pattern,
# and the wiki's own paths go deeper - `Whiplash\GameSDK\Videos\LegalScreens.bk2`
# is already 4, and Unreal titles bury movies under
# `Game\Content\Movies\Startup\`.
REFERENCE_MAX_DEPTH = 8

# Above `app::model::AUTO_SELECT_CONFIDENCE_THRESHOLD` (85): a catalogue naming
# this game's intro videos one by one is the strongest evidence the app has,
# stronger than any pattern that had to generalize.
REFERENCE_CONFIDENCE = 96

# The fix method that means "these files are the intro, remove them". A page
# that only documents a launch option or a config edit names its files in
# passing, and a rule built from that would delete on the strength of a
# mention.
REQUIRED_METHOD = "delete_or_replace_files"


def escape(name):
    """Regex-escape a file name for an alternation branch."""
    return re.escape(name)


def chunk_names(names):
    """Split `names` into groups whose `^(a|b|...)$` pattern fits the limit."""
    groups, current, length = [], [], 0
    for name in names:
        piece = escape(name)
        # `^(` + branches + `|` separators + `)$`
        addition = len(piece) + (1 if current else 0)
        if current and length + addition + 4 > MAX_REGEX_BYTES:
            groups.append(current)
            current, length = [], 0
            addition = len(piece)
        current.append(piece)
        length += addition
    if current:
        groups.append(current)
    return groups


def reference_rules(record, app_id, store):
    """The rules binding one page's file list to one store id."""
    names = sorted(set(record["video_files"]))
    rules = []
    for group in chunk_names(names):
        rules.append(
            OrderedDict(
                [
                    ("category", "intro"),
                    ("pattern", "^(" + "|".join(group) + ")$"),
                    (
                        "desc",
                        {
                            "en": f"Intro video PCGamingWiki names for {record['title']}",
                            "uk": "Вступне відео, яке PCGamingWiki називає "
                            f"для гри {record['title']}",
                        },
                    ),
                    ("confidence", REFERENCE_CONFIDENCE),
                    ("app_id", app_id),
                    ("origin", "reference"),
                    ("max_depth", REFERENCE_MAX_DEPTH),
                ]
            )
        )
    if not rules:
        return []
    print(f"  {record['title']} ({store} {app_id}): {len(names)} files", file=sys.stderr)
    return rules


def build(records):
    """Every reference rule the dataset supports, and the counts to report."""
    rules = []
    linked_titles, covered_names = set(), set()
    for record in records:
        if REQUIRED_METHOD not in record["methods"] or not record["video_files"]:
            continue
        ids = [
            (record.get("steam_appid"), "steam"),
            (record.get("gog_id"), "gog"),
        ]
        bound = False
        for app_id, store in ids:
            if not app_id:
                continue
            rules.extend(reference_rules(record, app_id, store))
            bound = True
        if bound:
            linked_titles.add(record["title"])
            covered_names.update(name.lower() for name in record["video_files"])
    return rules, linked_titles, covered_names


def main():
    with open(DATASET_PATH, encoding="utf-8") as handle:
        records = json.load(handle)
    with open(RULES_PATH, encoding="utf-8") as handle:
        pack = json.load(handle)

    kept = [rule for rule in pack["rules"] if rule.get("origin") != "reference"]
    generated, linked_titles, covered_names = build(records)
    pack["rules"] = kept + generated

    with open(RULES_PATH, "w", encoding="utf-8", newline="\n") as handle:
        json.dump(pack, handle, ensure_ascii=False, indent=2)
        handle.write("\n")

    all_names = set()
    for record in records:
        all_names.update(name.lower() for name in record["video_files"])
    print(
        f"{len(records)} pages, {len(linked_titles)} linked to a store id, "
        f"{len(generated)} reference rules, "
        f"{len(covered_names)}/{len(all_names)} named files covered, "
        f"{len(kept)} hand-written rules kept"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
