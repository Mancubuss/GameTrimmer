"""Harvest PCGamingWiki's "Skip intro videos" fixes into a local dataset.

Why this script exists in this shape
------------------------------------

The wiki documents how to skip a game's intro under an ``Essential
improvements`` heading, usually ``=== Skip intro videos ===``. That section is
the only first-hand evidence we have for *what a game's intro actually is*:
which files it plays, where they live, and whether the game accepts a launch
argument instead. GameTrimmer's intro rules were written without it.

Two earlier attempts failed for the same reason - one page per HTTP request:

* the MediaWiki API answers ``action=parse`` per page, so 1745 pages meant 1745
  requests, and PCGamingWiki starts returning **HTTP 429** long before that.
  A run on 2026-08-21 lost 190 of 417 pages that way, including Prey, The
  Witcher 3 and Max Payne - the wrong 190, since the biggest titles are the
  ones with the most edits;
* search paging stopped after ~400 titles because the loop ignored the API's
  own ``continue`` token.

Both are fixed here by asking for what the API is willing to hand over in bulk:
``action=query&prop=revisions`` accepts **50 titles per request**, so the whole
corpus costs ~35 requests instead of ~1745, and paging follows ``continue``
until the API stops offering one.

Output
------

``docs/pcgw_skip_intro_dataset.json``  - one record per page that has an intro
section, carrying the raw section wikitext (so a later reader can re-derive
anything this script did not think to extract), the file names, the paths, the
launch arguments, the engine, and the refcheck dates.

``docs/pcgw_skip_intro_harvest_report.md`` - counts, the most frequently named
files, the method split, and every page that was dropped, with the reason.

Standard library only. Run from the repository root:

    python scripts/harvest_pcgw_intros_full.py
"""

import json
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from collections import Counter, OrderedDict

API = "https://www.pcgamingwiki.com/w/api.php"
HEADERS = {
    "User-Agent": "GameTrimmer-Harvester/2.0 (contact: github.com/Mancubuss/GameTrimmer)"
}

# One request per second, and a real backoff when the wiki says no. The whole
# harvest is ~40 requests, so politeness costs under a minute.
REQUEST_DELAY = 1.0
MAX_RETRIES = 5
TITLES_PER_REQUEST = 50

DATASET_PATH = "docs/pcgw_skip_intro_dataset.json"
REPORT_PATH = "docs/pcgw_skip_intro_harvest_report.md"

# The searches that find the pages, deduplicated into one title list.
#
# Measured 2026-08-21: the first term is the corpus (1263 distinct pages, which
# is the whole of it - the API's `totalhits: 1745` is an estimate that paging to
# exhaustion does not confirm). The other three are the alternative wordings the
# wiki uses, and they add 16 pages between them. `insource:` was tried and
# removed: PCGamingWiki's search does not implement it and answers `totalhits: 0`.
SEARCHES = [
    '"Skip intro videos"',
    '"Skip splash screen"',
    '"Disable intro videos"',
    '"Skip intro"',
]

VIDEO_EXTENSIONS = (
    "bik",
    "bk2",
    "mp4",
    "webm",
    "ogv",
    "wmv",
    "avi",
    "usm",
    "mkv",
    "m4v",
    "mov",
    "smk",
    "roq",
    "sfd",
)

HEADING_RE = re.compile(r"^(={2,6})\s*(.+?)\s*\1\s*$", re.MULTILINE)
# A heading is an intro-skip heading when it says both "skip" and one of the
# things a game shows before its menu. "Skip intro videos" is the common one,
# but the wiki also writes "Skip intro/logo videos", "Skip startup movies",
# "Skip splash screens" and "Disable intro videos".
INTRO_HEADING_RE = re.compile(
    r"(?i)\b(skip|disable|remove|bypass)\b.*\b(intro|logo|splash|startup|movie|video|cutscene)"
    r"|\b(intro|logo|splash|startup)\b.*\b(skip|disable|remove|bypass)"
)
FILE_RE = re.compile(
    r"(?i)(?<![\w./\\-])([\w][\w \-.]{0,60}?\.(?:" + "|".join(VIDEO_EXTENSIONS) + r"))\b"
)
# `{{file|...}}` / `{{folder|...}}` are how the wiki marks a path; `<code>` and
# `{{code|...}}` carry launch arguments and config lines.
TEMPLATE_RE = re.compile(r"\{\{\s*(file|folder|path|code)\s*\|([^{}]*)\}\}", re.IGNORECASE)
CODE_TAG_RE = re.compile(r"<code>(.*?)</code>", re.IGNORECASE | re.DOTALL)
LAUNCH_ARG_RE = re.compile(r"(?<![\w-])([-+/][a-z][\w-]{2,30})\b", re.IGNORECASE)
REFCHECK_DATE_RE = re.compile(r"\{\{Refcheck[^}]*?date\s*=\s*([0-9]{4}-[0-9]{2}-[0-9]{2})", re.IGNORECASE)
ENGINE_RE = re.compile(r"\{\{\s*Infobox game/row/engine\s*\|\s*([^|}]+)", re.IGNORECASE)


def request(params):
    """One API call, with backoff. Returns the decoded JSON or raises."""
    params = dict(params)
    params.setdefault("format", "json")
    params.setdefault("formatversion", "2")
    url = API + "?" + urllib.parse.urlencode(params)
    delay = REQUEST_DELAY
    last_error = None
    for attempt in range(MAX_RETRIES):
        try:
            req = urllib.request.Request(url, headers=HEADERS)
            with urllib.request.urlopen(req, timeout=60) as resp:
                return json.loads(resp.read().decode("utf-8"))
        except urllib.error.HTTPError as err:
            last_error = err
            if err.code in (429, 502, 503, 504):
                # `Retry-After` is the server telling us exactly how long; a
                # guess is only for when it does not.
                wait = float(err.headers.get("Retry-After") or delay * (attempt + 2))
                print(f"  HTTP {err.code}, waiting {wait:.0f}s", flush=True)
                time.sleep(wait)
                continue
            raise
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as err:
            last_error = err
            time.sleep(delay * (attempt + 2))
    raise RuntimeError(f"giving up after {MAX_RETRIES} attempts: {last_error}")


def search_titles():
    """Every page title the searches return, deduplicated, order preserved."""
    titles = OrderedDict()
    for term in SEARCHES:
        offset = 0
        reported_total = None
        while True:
            data = request(
                {
                    "action": "query",
                    "list": "search",
                    "srsearch": term,
                    "srwhat": "text",
                    "srlimit": 50,
                    "sroffset": offset,
                    "srnamespace": 0,
                }
            )
            query = data.get("query", {})
            if reported_total is None:
                reported_total = query.get("searchinfo", {}).get("totalhits")
                print(f"search {term!r}: {reported_total} hits reported", flush=True)
            for hit in query.get("search", []):
                titles.setdefault(hit["title"], term)
            # The API's own continuation, not an assumed step: a search that
            # returns fewer than srlimit results is not necessarily the last.
            cont = data.get("continue", {})
            if "sroffset" not in cont:
                break
            offset = cont["sroffset"]
            time.sleep(REQUEST_DELAY)
        print(f"  collected {len(titles)} unique titles so far", flush=True)
        time.sleep(REQUEST_DELAY)
    return list(titles)


def fetch_wikitext(titles):
    """`{title: wikitext}` for every title, fetched 50 at a time.

    Redirects are followed, and both the requested title and the target end up
    in the map so a caller can look up either.
    """
    pages = {}
    missing = []
    for start in range(0, len(titles), TITLES_PER_REQUEST):
        chunk = titles[start : start + TITLES_PER_REQUEST]
        data = request(
            {
                "action": "query",
                "prop": "revisions",
                "rvprop": "content",
                "rvslots": "main",
                "titles": "|".join(chunk),
                "redirects": 1,
            }
        )
        query = data.get("query", {})
        aliases = {}
        for entry in query.get("normalized", []) + query.get("redirects", []):
            aliases[entry["from"]] = entry["to"]
        for page in query.get("pages", []):
            if page.get("missing"):
                missing.append(page.get("title", "?"))
                continue
            revisions = page.get("revisions") or []
            if not revisions:
                missing.append(page.get("title", "?"))
                continue
            pages[page["title"]] = revisions[0]["slots"]["main"]["content"]
        for source, target in aliases.items():
            if target in pages:
                pages[source] = pages[target]
        print(
            f"fetched {min(start + TITLES_PER_REQUEST, len(titles))}/{len(titles)} titles",
            flush=True,
        )
        time.sleep(REQUEST_DELAY)
    return pages, missing


def intro_sections(wikitext):
    """Every intro-skip section of a page: `(heading, body)` pairs.

    A section ends at the next heading of the same or a higher level, which is
    what keeps "Skip intro videos" from swallowing the rest of the article.
    """
    found = []
    headings = list(HEADING_RE.finditer(wikitext))
    for index, match in enumerate(headings):
        title = match.group(2)
        if not INTRO_HEADING_RE.search(title):
            continue
        level = len(match.group(1))
        end = len(wikitext)
        for later in headings[index + 1 :]:
            if len(later.group(1)) <= level:
                end = later.start()
                break
        found.append((title, wikitext[match.end() : end].strip()))
    return found


def clean_template_values(body):
    """`{{file|Movies/logo.bik}}` -> `Movies/logo.bik`, per template kind."""
    values = {"file": [], "folder": [], "code": []}
    for kind, raw in TEMPLATE_RE.findall(body):
        kind = kind.lower()
        bucket = "folder" if kind in ("folder", "path") else ("file" if kind == "file" else "code")
        for part in raw.split("|"):
            part = re.sub(r"\{\{[^{}]*\}\}", "", part).strip()
            if part:
                values[bucket].append(part)
    return values


def classify(body):
    """Which kinds of fix this section describes. A page may offer several."""
    low = body.lower()
    methods = []
    if re.search(r"(?i)(command line|launch option|launch parameter|-\w+\b.*argument)", body):
        methods.append("launch_option")
    if re.search(r"(?i)\b(delete|remove|rename|replace|empty|0[ -]?byte|zero[ -]?byte|blank)\b", low):
        methods.append("delete_or_replace_files")
    if re.search(r"(?i)(\.ini\b|\.cfg\b|\.xml\b|config file|settings file|registry)", low):
        methods.append("config_edit")
    if re.search(r"(?i)\b(mod|patch|tool|injector)\b", low) and not methods:
        methods.append("community_mod")
    return methods or ["other"]


def extract(title, wikitext):
    sections = intro_sections(wikitext)
    if not sections:
        return None

    files, folders, code, args, methods = [], [], [], [], []
    bodies = []
    for heading, body in sections:
        bodies.append(f"=== {heading} ===\n{body}")
        templates = clean_template_values(body)
        files.extend(templates["file"])
        folders.extend(templates["folder"])
        code.extend(templates["code"])
        code.extend(fragment.strip() for fragment in CODE_TAG_RE.findall(body))
        files.extend(FILE_RE.findall(body))
        methods.extend(classify(body))

    joined_code = " ".join(code)
    args.extend(LAUNCH_ARG_RE.findall(joined_code))

    def dedup(values, lower=False):
        seen = OrderedDict()
        for value in values:
            value = value.strip().strip("'\"`")
            if not value:
                continue
            seen.setdefault(value.lower() if lower else value, value if not lower else value.lower())
        return list(seen.values())

    # A `{{file|...}}` value can itself be a path; keep whichever it is, but
    # report the bare video names separately - those are what a rule matches.
    # A name has to have something before the dot: `{{file|*.bik}}` and a bare
    # `.bik` are the wiki describing a whole folder, not naming a file, and a
    # rule built from one of those would match every video in the game.
    video_files = dedup(
        [
            name
            for name in files
            if name.lower().endswith(VIDEO_EXTENSIONS)
            and re.match(r"^[\w][\w \-.]*\.", name)
        ],
        lower=True,
    )
    path_like = dedup([value for value in files + folders if "/" in value or "\\" in value])

    return {
        "title": title,
        "url": "https://www.pcgamingwiki.com/wiki/"
        + urllib.parse.quote(title.replace(" ", "_"), safe="_():,'!-"),
        "methods": sorted(set(methods)),
        "video_files": video_files,
        "paths": path_like,
        "launch_arguments": dedup(args, lower=True),
        "code_fragments": dedup(code)[:20],
        "engine": dedup(ENGINE_RE.findall(wikitext))[:4],
        "refcheck_dates": sorted(set(REFCHECK_DATE_RE.findall("\n".join(bodies)))),
        "section_text": "\n\n".join(bodies)[:6000],
    }


def write_report(titles, pages, missing, records, no_section):
    file_counter = Counter()
    for record in records:
        file_counter.update(record["video_files"])
    method_counter = Counter()
    for record in records:
        method_counter.update(record["methods"])
    arg_counter = Counter()
    for record in records:
        arg_counter.update(record["launch_arguments"])

    lines = [
        "# PCGamingWiki intro-skip harvest",
        "",
        f"- Titles found by search: **{len(titles)}**",
        f"- Pages fetched: **{len(pages)}**",
        f"- Pages with an intro-skip section: **{len(records)}**",
        f"- Pages fetched but without such a section: **{len(no_section)}**",
        f"- Titles the API had no page for: **{len(missing)}**",
        "",
        "## Fix methods (a page may document several)",
        "",
        "| Method | Pages |",
        "|---|---|",
    ]
    for method, count in method_counter.most_common():
        lines.append(f"| `{method}` | {count} |")

    lines += ["", "## Most frequently named video files", "", "| File | Pages |", "|---|---|"]
    for name, count in file_counter.most_common(60):
        lines.append(f"| `{name}` | {count} |")

    lines += ["", "## Most frequent launch arguments", "", "| Argument | Pages |", "|---|---|"]
    for arg, count in arg_counter.most_common(40):
        lines.append(f"| `{arg}` | {count} |")

    if missing:
        lines += ["", "## Titles with no page", ""]
        lines += [f"- {title}" for title in sorted(missing)[:100]]

    with open(REPORT_PATH, "w", encoding="utf-8") as handle:
        handle.write("\n".join(lines) + "\n")


def main():
    titles = search_titles()
    if not titles:
        print("no titles found - refusing to overwrite the dataset", file=sys.stderr)
        return 1
    print(f"{len(titles)} unique titles to fetch", flush=True)

    pages, missing = fetch_wikitext(titles)
    print(f"{len(pages)} pages fetched, {len(missing)} titles had no page", flush=True)

    records, no_section = [], []
    for title in titles:
        wikitext = pages.get(title)
        if wikitext is None:
            continue
        record = extract(title, wikitext)
        if record is None:
            no_section.append(title)
        else:
            records.append(record)

    print(f"{len(records)} pages carry an intro-skip section", flush=True)

    with open(DATASET_PATH, "w", encoding="utf-8") as handle:
        json.dump(records, handle, ensure_ascii=False, indent=2)
    write_report(titles, pages, missing, records, no_section)
    print(f"wrote {DATASET_PATH} and {REPORT_PATH}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
