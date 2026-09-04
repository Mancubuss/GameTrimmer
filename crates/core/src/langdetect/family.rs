//! The "language family" heuristic — the strongest signal in the engine.
//! Three independent shapes of evidence:
//!
//! 1. **File-name family**: sibling files in the same directory whose names
//!    differ only by the recognized language token
//!    (`Voice_english.pak` / `Voice_french.pak` / ...).
//! 2. **Folder family**: sibling subdirectories whose *entire* name is a
//!    recognized language token (`en/ de/ fr/ es/`) — every file underneath
//!    a non-keep language folder is flagged.
//! 3. **Directory family**: any files in the same directory whose *filename*
//!    carries a recognized language token, counted across the whole
//!    directory regardless of whether the rest of the filename matches
//!    (unlike mechanism 1's exact-shape requirement). This covers real
//!    per-language audio/data sets where each file's content differs, not
//!    just its language suffix — e.g. `VO_Gameplay_Charles_DE.assets.bank` /
//!    `VO_Gameplay_William_FR.assets.bank` / `VO_Gameplay_James_KO.assets.bank`
//!    share no common shape once the character-name portion is removed, so
//!    mechanism 1 misses them, but the directory as a whole is unmistakably
//!    a per-language voice-over set. Found via the corpus regression
//!    (`tests/corpus/corpus.rs`) — this is the same simplified heuristic the
//!    corpus collection tool itself uses (see `tests/corpus/README.md`).
//!
//! Every shape requires >= 3 distinct canonical languages among the
//! siblings (counting kept languages, which count as evidence but are never
//! themselves flagged) before it is trusted — except mechanism 3 restricted
//! to bare two-letter (Level C) evidence, which requires >= 5 (see
//! `MIN_FAMILY_SIZE_BARE_ONLY`) because two-letter codes collide with
//! ordinary short words far more often than full names or iso3 codes do. A
//! confirmed family is strong enough to legitimize even a bare two-letter
//! token — but a file is only ever a *member* of a family if its own
//! filename carries the language token (mechanism 3, like mechanism 1, only
//! looks at `Occurrence::is_filename` hits); sitting inside a
//! family-confirmed directory without a token of its own is never
//! sufficient.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{CoreError, Result as CoreResult};
use crate::langdetect::data::LangData;
use crate::langdetect::dict::Level;
use crate::langdetect::occurrences::Occurrence;
use crate::langdetect::reason::{LangEvidence, LangReason};
use crate::langdetect::tokens::Segment;
use crate::langdetect::UNDETERMINED_LANG;
use crate::scanner::CANCEL_POLL_INTERVAL;

const MIN_FAMILY_SIZE: usize = 3;
/// Stricter threshold for mechanism 3 (`compute_directory_occurrence_family`)
/// when *every* occurrence at a shared position is a bare two-letter
/// (Level C) code: two-letter codes collide with ordinary short words far
/// more often than full names or three-letter ISO codes do (manual corpus
/// review found a same-position, 3-distinct-language coincidence among
/// plain hiragana-romanization asset names — `english-da.png` / `-ko.png`
/// alongside `Hiragana-he.png`, none of them Danish/Korean/Hebrew content).
/// Requiring more independent coincidences before trusting a bare-code-only
/// family cuts that risk sharply while still confirming genuine sets, which
/// in practice virtually always cover many more than 3 languages.
const MIN_FAMILY_SIZE_BARE_ONLY: usize = 5;
/// Mechanism 1 analogue of [`MIN_FAMILY_SIZE_BARE_ONLY`]: identical-shape
/// siblings are much stronger evidence than same-position ones, but the
/// 2026-07-16 screenshot report still found an exact-shape, 3-distinct
/// bare-code coincidence (`ee_body01_f_de/el/he.ee` — Pathfinder character
/// bundles where de/el/he are body-variant codes, not German/Greek/Hebrew).
/// Genuine bare-code sets essentially always cover 4+ languages.
const MIN_SHAPE_FAMILY_SIZE_BARE_ONLY: usize = 4;
/// How thin the language-named share of a folder's subdirectories may get
/// before the folder reads as a storage tree rather than a set of
/// translations, as a reciprocal: 20 means "at least one in twenty" (GT-465).
///
/// Warhammer 40,000: Darktide keeps `bundle\data\`, a content-addressed store
/// of 256 subfolders named `00`..`ff`. Four of those names (`ca`, `da`, `de`,
/// `fa`) are also language codes, and the moment the dictionary learned `ca`
/// and `fa` the folder crossed the three-language bar and the whole tree
/// became a localization: 55 findings became 3,605, and 1.28 GB of game data
/// was offered for deletion. Counting how many languages a folder holds was
/// never the question; the question is what share of it they are.
///
/// The threshold is measured, not guessed. Enumerating every folder in the
/// library that a language-named subdirectory sits in: the thinnest *genuine*
/// set is Galactic Civilizations III's `Movies\` at 3 of 26 (11.5%), and the
/// densest *false* one is Dead Effect 2's `GI\level66` at 3 of 94 (3.2%).
/// Nothing at all falls between 3.2% and 11.5%, and everything below is a
/// hash tree. One in twenty sits in that gap with room on both sides.
///
/// This also subsumes the narrower guard the ticket proposed - "every code
/// here is spelled with a-f only, so the folder is hexadecimal" - and reaches
/// further: Underrail's `data\locale\creatures\` is 5 language names among
/// 1,226 creature folders, and not one of them is hex.
const MIN_LANGUAGE_FOLDER_SHARE: usize = 20;
/// Length of the bare two-letter code class (`de`, `es`, `am`) - the only
/// evidence [`shadowed_bare_codes`] ever second-guesses.
const BARE_CODE_LEN: usize = 2;
const SHAPE_PLACEHOLDER: char = '\u{1}';

/// One family-candidate occurrence: (file index, canonical language, trust level).
/// (file index, canonical key, trust level, byte offset of the matched
/// token within the file name) - the offset is not used to group, only to
/// settle a tie between two groups claiming the same file (see [`upsert`]).
type FamilyMember = (usize, &'static str, Level, usize);

/// One mechanism-3 candidate: the file's remaining (non-language) atoms are
/// kept so coincidental same-position matches can be filtered out. See
/// `compute_directory_occurrence_family`.
struct PositionMember {
    file_idx: usize,
    canonical: &'static str,
    level: Level,
    /// Byte offset of the matched token within the file name - carried only
    /// so [`upsert`] can settle a tie reproducibly.
    token_start: usize,
    /// Every non-language atom of the name, as this variant splits it —
    /// extension atoms included, since a directory holding both `.txt` and
    /// `.dat` members says something real by which of the two a file uses.
    atoms: HashSet<String>,
    /// The subset of `atoms` that says how the file was *named*: everything
    /// in the stem, plus anything preceding the language token. What this
    /// leaves out is the tail of a dotted extension — `.item.bytes`,
    /// `.bundle.bin` — which describes the format, is shared by every file
    /// of that format in the directory, and so tells nothing about whether
    /// two names belong to the same set.
    naming_atoms: HashSet<String>,
}

/// (parent_dir, shape-with-placeholder) -> sibling occurrences.
type ShapeGroups = HashMap<(String, String), Vec<FamilyMember>>;

/// (parent_dir, atom-sequence variant, "from start"|"from end", atom index)
/// -> sibling occurrences. See `compute_directory_occurrence_family` for
/// what "variant" distinguishes.
type PositionGroups = HashMap<(String, u8, bool, usize), Vec<PositionMember>>;

#[derive(Debug, Clone)]
pub struct FamilyHit {
    pub canonical: &'static str,
    pub confidence: u8,
    pub reason: LangReason,
    /// Where the language token that produced this hit starts inside the file
    /// name. Never part of the decision to flag - only of [`upsert`]'s
    /// tie-break. Folder-based mechanisms have no such token and pass 0.
    token_start: usize,
    /// How many distinct languages the family that produced this hit covers.
    /// Also tie-break only, and the last one: when two families agree on the
    /// language they disagree only about which evidence to *show*, and the
    /// wider family is the better thing to show.
    family_size: usize,
}

fn family_confidence(level: Level) -> u8 {
    match level {
        Level::A => 95,
        Level::B => 92,
        Level::C => 90,
    }
}

/// Records `hit` for `file_idx`, keeping the strongest claim when several
/// families claim the same file.
///
/// The tie-break is the whole point (GT-229). Some 48 files in the library
/// carry two plausible language tokens in one name -
/// `fonts_fra_LOC_DEU.upk`, `qt_help_pt_BR.qm` - and two families claim them
/// with identical confidence. "Whoever got here first" then meant "whichever
/// bucket the hash map happened to visit first", which Rust seeds per map
/// instance: the same binary over the same files labelled the same file
/// French in one run and German in the next. The set of flagged *paths* was
/// stable; only the label moved, and the label is what the keep-list filters
/// on and what the user reads.
///
/// So the order is stated instead of stumbled into: higher confidence wins;
/// then the token further along the name, because a language written as a
/// suffix is the file's language and one written earlier is part of what the
/// file is called (`fonts_fra_LOC_DEU` is the German cut of the `fonts_fra`
/// asset); then the lexicographically smaller canonical key, which decides
/// nothing in particular but decides it the same way every time; then the
/// wider family, which is what settles the case where the two claims agree
/// on the language and differ only in the evidence they would print
/// ("family of 9 languages" against "family of 7").
fn upsert(map: &mut HashMap<usize, FamilyHit>, file_idx: usize, hit: FamilyHit) {
    let replace = match map.get(&file_idx) {
        Some(existing) => {
            (
                hit.confidence,
                hit.token_start,
                std::cmp::Reverse(hit.canonical),
                hit.family_size,
            ) > (
                existing.confidence,
                existing.token_start,
                std::cmp::Reverse(existing.canonical),
                existing.family_size,
            )
        }
        None => true,
    };
    if replace {
        map.insert(file_idx, hit);
    }
}

/// Orders two claims on the same *folder* the way [`upsert`] orders claims
/// on the same file. A folder family names no token inside a file name, so
/// the position term is dropped and only confidence, the canonical and the
/// family size remain.
///
/// One folder can belong to two name-shape families at once (GT-472).
/// HUMANKIND's `AssetBundles\pt-BR-Localization\` joins the eleven-folder
/// set shaped `<tag>-Localization`, where the token is the whole locale tag
/// `pt-br`; and a second, ten-folder set where the *entire* folder name
/// matched as a locale tag with trailing parts, leaving `pt` — the same
/// coarse reading that collapses `zh-CN` and `zh-TW` into one Chinese and so
/// counts one language fewer. Both wrote their verdict into the same cell,
/// the later write won, and Rust deliberately varies map iteration order per
/// process: six consecutive runs of the same binary over the same game
/// labelled that folder `pt` once and `pt-br` five times, and `zh-TW` flipped
/// between the two Chinese scripts with it. The set of paths never moved —
/// only the label, which is the one thing the user's keep-list is applied to.
///
/// Confidence settles both cases on its own and in the right direction: a
/// tag read whole (`pt-br`, `zh-tw`) is a Level A dictionary hit, while the
/// coarse reading falls back to the bare prefix and its Level C. Where no
/// finer reading exists — `de-DE-Localization`, both readings say German —
/// there is nothing to choose between and the answer is unchanged.
fn claim_rank(claim: &(&'static str, u8, usize)) -> (u8, std::cmp::Reverse<&'static str>, usize) {
    (claim.1, std::cmp::Reverse(claim.0), claim.2)
}

/// Bare two-letter codes that a language name spelled out in full in the
/// same directory contradicts, as `(file index, occurrence start)` pairs the
/// two filename mechanisms must skip.
///
/// Rogue Trooper Redux writes the same eight texts twice in one folder:
/// `Tannoy_American.asr` ... `Tannoy_Spanish.asr` spelled out, and
/// `t_am.asr` ... `t_sp.asr` abbreviated to the studio's own two letters.
/// Five of those six short tokens agree with the dictionary anyway - `en`,
/// `fr`, `it`, plus the curated `ge` and `sp` - so the set reads as an
/// ordinary bare-code family and the family gate confirms it, exactly as
/// designed. The sixth does not agree: `am` here is `American`, which is
/// English, which is on the keep-list - and the file was being offered for
/// deletion as Amharic, a language the game does not ship (GT-467).
/// Counting how many of a set's tokens are "real" codes cannot separate
/// this from a genuine set; the long set beside it can, because a directory
/// that spells `American` out in full has already said what its `am` stands
/// for.
///
/// So a bare code loses its family evidence when an all-letters language
/// name in the same directory begins with those two letters and resolves to
/// a different language. Where the two agree - `sp` beside `Spanish`, `ge`
/// beside `German` - nothing happens. A directory that spells nothing out is
/// untouched: Delta Force's `locales` folder holds `am.pak` among nineteen
/// other bare codes and locale tags with no `American` anywhere, and it
/// stays Amharic. Locale tags are deliberately outside the spelled-out
/// pool, since `pt-br` beginning with `pt` says nothing about a sibling
/// `pt`, and so are folder tokens, whose findings never pass through here
/// at all.
fn shadowed_bare_codes(
    seg_lists: &[Vec<Segment>],
    occ_lists: &[Vec<Occurrence>],
    cancel: &AtomicBool,
) -> CoreResult<HashSet<(usize, usize)>> {
    fn spelled_out(occ: &Occurrence) -> bool {
        occ.is_filename
            && occ.level == Level::A
            && occ.matched.len() > BARE_CODE_LEN
            && occ.matched.bytes().all(|b| b.is_ascii_alphabetic())
    }
    fn bare_code(occ: &Occurrence) -> bool {
        occ.is_filename && occ.level == Level::C && occ.matched.len() == BARE_CODE_LEN
    }

    // parent dir -> first two letters of a spelled-out name -> what those
    // names mean there (a directory may spell out two languages sharing a
    // first pair, and then no reading is the obvious one).
    let mut spelled: HashMap<String, HashMap<&str, HashSet<&'static str>>> = HashMap::new();
    for (i, segs) in seg_lists.iter().enumerate() {
        if i % CANCEL_POLL_INTERVAL == 0 {
            check_cancel(cancel)?;
        }
        if segs.is_empty() || !occ_lists[i].iter().any(spelled_out) {
            continue;
        }
        let dir = spelled.entry(dir_key(segs, segs.len() - 1)).or_default();
        for occ in occ_lists[i].iter().filter(|o| spelled_out(o)) {
            dir.entry(&occ.matched[..BARE_CODE_LEN])
                .or_default()
                .insert(occ.canonical);
        }
    }

    let mut shadowed = HashSet::new();
    if spelled.is_empty() {
        return Ok(shadowed);
    }
    for (i, segs) in seg_lists.iter().enumerate() {
        if i % CANCEL_POLL_INTERVAL == 0 {
            check_cancel(cancel)?;
        }
        if segs.is_empty() || !occ_lists[i].iter().any(bare_code) {
            continue;
        }
        let Some(dir) = spelled.get(&dir_key(segs, segs.len() - 1)) else {
            continue;
        };
        for occ in occ_lists[i].iter().filter(|o| bare_code(o)) {
            if dir
                .get(occ.matched.as_str())
                .is_some_and(|claims| claims.iter().any(|c| *c != occ.canonical))
            {
                shadowed.insert((i, occ.start));
            }
        }
    }
    Ok(shadowed)
}

fn dir_key(segs: &[Segment], upto: usize) -> String {
    segs[..upto]
        .iter()
        .map(|s| s.lower.as_str())
        .collect::<Vec<_>>()
        .join("\\")
}

/// The directory a family was found in, or `None` for the game's own root -
/// whoever renders the reason words that, not this module.
fn display_dir(dir_key: &str) -> Option<String> {
    (!dir_key.is_empty()).then(|| dir_key.to_string())
}

fn shape_of(filename_lower: &str, start: usize, end: usize) -> String {
    let mut shaped = String::with_capacity(filename_lower.len());
    shaped.push_str(&filename_lower[..start]);
    shaped.push(SHAPE_PLACEHOLDER);
    shaped.push_str(&filename_lower[end..]);
    shaped
}

/// Returns `Err(CoreError::Other("cancelled"))` if `cancel` is set, else
/// `Ok(())`. The hot loops below poll this every [`CANCEL_POLL_INTERVAL`]
/// iterations so a Stop request during the analysis of a huge game (ARK's
/// thousand-file flat directories) is honored promptly instead of only after
/// the whole family computation runs to completion. The "cancelled" sentinel
/// matches [`crate::scanner::collect_cancellable`] so the app's writer treats
/// it as a normal user action, not a scan failure.
#[inline]
fn check_cancel(cancel: &AtomicBool) -> CoreResult<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(CoreError::Other("cancelled".to_string()))
    } else {
        Ok(())
    }
}

/// Computes family-confirmed findings across every file of one game.
/// `seg_lists`/`occ_lists` must be index-aligned with the game's file list.
///
/// `cancel` is polled inside the hot loops (see [`check_cancel`]); when it is
/// observed set this returns `Err(CoreError::Other("cancelled"))` without
/// finishing the remaining mechanisms. When `cancel` is never set the output
/// is exactly what the previous non-cancellable version produced.
pub fn compute_family(
    data: &LangData,
    seg_lists: &[Vec<Segment>],
    occ_lists: &[Vec<Occurrence>],
    keep: &HashSet<String>,
    cancel: &AtomicBool,
) -> CoreResult<HashMap<usize, FamilyHit>> {
    let mut result = HashMap::new();
    let shadowed = shadowed_bare_codes(seg_lists, occ_lists, cancel)?;
    compute_file_shape_family(
        data,
        seg_lists,
        occ_lists,
        keep,
        &shadowed,
        cancel,
        &mut result,
    )?;
    compute_directory_occurrence_family(
        seg_lists,
        occ_lists,
        keep,
        &shadowed,
        cancel,
        &mut result,
    )?;
    compute_folder_family(data, seg_lists, keep, cancel, &mut result)?;
    compute_prefixed_folder_family(seg_lists, occ_lists, keep, cancel, &mut result)?;
    Ok(result)
}

/// Mechanism 1: sibling files in the same directory whose name differs only
/// by the recognized language token.
fn compute_file_shape_family(
    data: &LangData,
    seg_lists: &[Vec<Segment>],
    occ_lists: &[Vec<Occurrence>],
    keep: &HashSet<String>,
    shadowed: &HashSet<(usize, usize)>,
    cancel: &AtomicBool,
    result: &mut HashMap<usize, FamilyHit>,
) -> CoreResult<()> {
    let mut shape_groups: ShapeGroups = HashMap::new();
    // Every file of the game by its directory, so a candidate group can be
    // asked what *else* stands in its slot - see `slot_is_mostly_languages`.
    let mut by_dir: HashMap<String, Vec<usize>> = HashMap::new();

    for (i, segs) in seg_lists.iter().enumerate() {
        if i % CANCEL_POLL_INTERVAL == 0 {
            check_cancel(cancel)?;
        }
        let Some(filename_seg) = segs.last() else {
            continue;
        };
        let parent_dir = dir_key(segs, segs.len() - 1);
        by_dir.entry(parent_dir.clone()).or_default().push(i);

        for occ in &occ_lists[i] {
            if !occ.is_filename || shadowed.contains(&(i, occ.start)) {
                continue;
            }
            let shape = shape_of(&filename_seg.lower, occ.start, occ.end);
            shape_groups
                .entry((parent_dir.clone(), shape))
                .or_default()
                .push((i, occ.canonical, occ.level, occ.start));
        }
    }

    for ((parent_dir, shape), members) in &shape_groups {
        let distinct: HashSet<&'static str> = members.iter().map(|(_, c, _, _)| *c).collect();
        let all_bare = members.iter().all(|(_, _, level, _)| *level == Level::C);
        let threshold = if all_bare {
            MIN_SHAPE_FAMILY_SIZE_BARE_ONLY
        } else {
            MIN_FAMILY_SIZE
        };
        if distinct.len() < threshold {
            continue;
        }
        if !slot_is_mostly_languages(seg_lists, by_dir.get(parent_dir), shape, members) {
            continue;
        }
        claim_unnamed_slot_fillers(
            data,
            seg_lists,
            by_dir.get(parent_dir),
            parent_dir,
            shape,
            members,
            distinct.len(),
            result,
        );
        for (file_idx, canonical, level, token_start) in members.iter().copied() {
            if keep.contains(canonical) {
                continue;
            }
            let reason = LangReason::new(LangEvidence::Family {
                languages: distinct.len(),
                dir: display_dir(parent_dir),
            });
            upsert(
                result,
                file_idx,
                FamilyHit {
                    canonical,
                    confidence: family_confidence(level),
                    reason,
                    token_start,
                    family_size: distinct.len(),
                },
            );
        }
    }
    Ok(())
}

/// Whether the slot this family varies actually holds languages, asked of
/// *every* file in the directory rather than only of the ones the dictionary
/// recognized (GT-468).
///
/// The shape family sees `da`, `de`, `ge`, `ko`, `no`, `ro`, `ru`, `ta`, `te`
/// in `Learn Japanese To Survive`'s `www\audio\se\` and reads nine languages
/// varying in one slot. What it does not see is the thirty-odd files beside
/// them - `ka`, `ke`, `ki`, `ku`, `ma`, `me`, ... - which produce no
/// occurrence and so never join the group at all. The slot holds the Japanese
/// syllabary, and the nine that look like language codes are a minority in
/// it. Same shape, same answer: Lambda Wars ships a browser-usage table keyed
/// by *country* inside `caniuse-db\region-usage-json\`, and ARK ships ICU's
/// locale tables in `Content\Localization\ICU\icudt53l\`.
///
/// So the set is confirmed only when the recognized values are more than half
/// of everything standing in that slot. A real language folder passes
/// trivially - Delta Force's twenty CEF packs are twenty languages and
/// nothing else - and a real set keeps passing with a few stray neighbours,
/// which is what "more than half" buys over "all".
///
/// The occupants are read by matching the shape's literal prefix and suffix
/// against the directory's file names, which is the same thing `shape_of`
/// built them from. Only groups that already cleared the family-size
/// threshold are asked, so the scan is over the few directories that produced
/// a candidate rather than over the game.
/// Confidence carried by a finding whose language could not be named
/// (GT-464). Deliberately below every real family score, for two reasons at
/// once: it loses any tie against a claim that *can* name the language, and
/// it sits under the app's `REVIEW_CONFIDENCE_THRESHOLD`, so the row arrives
/// already marked as worth a look.
const UNNAMED_CONFIDENCE: u8 = 60;

/// GT-464: the file that fills a confirmed set's slot with a token the
/// dictionary cannot read.
///
/// A directory holding `sounds_fre.pck sounds_ger.pck sounds_ita.pck
/// sounds_spa.pck` and one `sounds_jap.pck` shows the user four rows. The
/// fifth file is not silent because the engine judged it and said no — it is
/// silent because `jap` is not in the dictionary (`jpn` is), and a file with
/// no recognized token produces nothing at all. From the outside those two
/// are the same picture, which is the whole complaint behind
/// "show found-but-empty rather than nothing".
///
/// The evidence here is entirely about the *neighbours*, never about the
/// file: it must stand in a set this mechanism already confirmed, fill that
/// set's exact name shape, and fill it with a single atom of the same width
/// the set's own tokens use. That last condition is what keeps the rule from
/// becoming the "ride along without evidence" path closed by `8b03b91`:
/// `sounds_master.pck` beside three-letter language codes is seven letters
/// wide and never qualifies.
///
/// What it cannot do is tell a language it has never heard of from an
/// ordinary word of the same length — `sounds_sfx.pck` sits in the same slot
/// as `sounds_jap.pck` and is not a language. That is why the answer is
/// "undetermined" rather than a guess, why the row carries no label, and why
/// the caller keeps it out of bulk selection: the user is being shown a
/// question, not an answer.
#[allow(clippy::too_many_arguments)]
fn claim_unnamed_slot_fillers(
    data: &LangData,
    seg_lists: &[Vec<Segment>],
    occupants: Option<&Vec<usize>>,
    parent_dir: &str,
    shape: &str,
    members: &[FamilyMember],
    languages: usize,
    result: &mut HashMap<usize, FamilyHit>,
) {
    let Some(occupants) = occupants else {
        return;
    };
    let Some(cut) = shape.find(SHAPE_PLACEHOLDER) else {
        return;
    };
    let prefix = &shape[..cut];
    let suffix = &shape[cut + SHAPE_PLACEHOLDER.len_utf8()..];

    // The set has to agree on how wide its own slot is. A set spelling some
    // of its languages `de` and others `german` describes no width at all,
    // and anything at all would fit the gap between its literals.
    let mut width: Option<usize> = None;
    for (file_idx, ..) in members.iter().copied() {
        let Some(name) = seg_lists[file_idx].last() else {
            return;
        };
        let len = name.lower.len() + SHAPE_PLACEHOLDER.len_utf8() - shape.len();
        match width {
            Some(seen) if seen != len => return,
            _ => width = Some(len),
        }
    }
    let Some(width) = width else {
        return;
    };

    let claimed: HashSet<usize> = members.iter().map(|(i, ..)| *i).collect();
    for file_idx in occupants.iter().copied() {
        if claimed.contains(&file_idx) || result.contains_key(&file_idx) {
            continue;
        }
        let Some(name) = seg_lists[file_idx].last() else {
            continue;
        };
        let low = name.lower.as_str();
        if low.len() != prefix.len() + width + suffix.len() {
            continue;
        }
        if !low.starts_with(prefix) || !low.ends_with(suffix) {
            continue;
        }
        let filler = &low[prefix.len()..low.len() - suffix.len()];
        // One atom, not a fragment of several: a gap holding `en_us` or
        // `v2.1` is not this set's slot being filled, it is a different name
        // that happens to be the same length.
        if !filler.bytes().all(|b| b.is_ascii_alphanumeric()) {
            continue;
        }
        // A token the dictionary *can* read is an ordinary finding and has
        // already been judged on its own merits; a word the engine knows to
        // mean something else is not a language going unnamed.
        if data.lookup(filler).is_some() || is_marker_word(data, filler) {
            continue;
        }
        upsert(
            result,
            file_idx,
            FamilyHit {
                canonical: UNDETERMINED_LANG,
                confidence: UNNAMED_CONFIDENCE,
                reason: LangReason::new(LangEvidence::Family {
                    languages,
                    dir: display_dir(parent_dir),
                }),
                token_start: 0,
                family_size: languages,
            },
        );
    }
}

/// True if the engine already knows this word to mean something other than a
/// language — an asset tree, a content type, or localization vocabulary.
fn is_marker_word(data: &LangData, word: &str) -> bool {
    [
        &data.negative,
        &data.overridable_negative,
        &data.audio,
        &data.text,
        &data.video,
        &data.font,
        &data.loc_generic,
        &data.loc_specific,
        &data.video_extensions,
        &data.font_extensions,
        &data.text_extensions,
        &data.audio_extensions,
        &data.graphic_extensions,
    ]
    .iter()
    .any(|set| set.contains(word))
}

fn slot_is_mostly_languages(
    seg_lists: &[Vec<Segment>],
    dir_files: Option<&Vec<usize>>,
    shape: &str,
    members: &[FamilyMember],
) -> bool {
    let Some((prefix, suffix)) = shape.split_once(SHAPE_PLACEHOLDER) else {
        return true;
    };
    let Some(dir_files) = dir_files else {
        return true;
    };
    let member_files: HashSet<usize> = members.iter().map(|(i, _, _, _)| *i).collect();

    let mut occupants: HashSet<&str> = HashSet::new();
    let mut recognized: HashSet<&str> = HashSet::new();
    for i in dir_files.iter().copied() {
        let Some(name) = seg_lists[i].last().map(|seg| seg.lower.as_str()) else {
            continue;
        };
        if name.len() <= prefix.len() + suffix.len()
            || !name.starts_with(prefix)
            || !name.ends_with(suffix)
        {
            continue;
        }
        let value = &name[prefix.len()..name.len() - suffix.len()];
        occupants.insert(value);
        if member_files.contains(&i) {
            recognized.insert(value);
        }
    }
    recognized.len() * 2 > occupants.len()
}

/// The atom standing in one positional slot of `file`, or `None` when the
/// file has no atom there. The slot is named the way
/// `compute_directory_occurrence_family` names it: which atom sequence
/// (stem-only or whole filename), counted from which end, at which index.
/// Returns `None` when the file's name is not `width` atoms long, because a
/// name of a different length is not laid out like the family's members and
/// its atom at this index is not the same slot - see
/// [`position_slot_is_mostly_languages`].
fn slot_atom(
    seg_lists: &[Vec<Segment>],
    file: usize,
    slot: (u8, bool, usize),
    width: usize,
) -> Option<&str> {
    let (variant, from_start, idx) = slot;
    let filename_seg = seg_lists[file].last()?;
    let stem_end = filename_seg
        .lower
        .find('.')
        .unwrap_or(filename_seg.lower.len());
    let atoms: Vec<&_> = filename_seg
        .atoms
        .iter()
        .filter(|a| variant == 1 || a.end <= stem_end)
        .collect();
    if atoms.len() != width {
        return None;
    }
    let position = if from_start {
        idx
    } else {
        atoms.len().checked_sub(idx + 1)?
    };
    atoms.get(position).map(|a| a.text.as_str())
}

/// How many atoms a file's name has in one of the two variants.
fn atom_width(seg_lists: &[Vec<Segment>], file: usize, variant: u8) -> Option<usize> {
    let filename_seg = seg_lists[file].last()?;
    let stem_end = filename_seg
        .lower
        .find('.')
        .unwrap_or(filename_seg.lower.len());
    Some(
        filename_seg
            .atoms
            .iter()
            .filter(|a| variant == 1 || a.end <= stem_end)
            .count(),
    )
}

/// [`slot_is_mostly_languages`] for the positional family (GT-468). Same
/// question, different way of naming the slot: here it is an atom index
/// rather than a blanked-out shape.
///
/// Learn Japanese To Survive needs both. Its syllable clips are
/// `hiragana-female-<syllable>.ogg`, so `hiragana` and `female` are on every
/// file and the pair of them vouches for the whole group through the
/// two-shared-naming-atoms rule - the shape check never gets to speak.
///
/// Only names of the *same length* are asked. A position means nothing across
/// names built differently, and counting them as occupants convicts real
/// sets: Wolfenstein: Youngblood keeps `italian.pack` and `russian.pack`
/// beside twenty-eight `patch_<n>_<language>.pack`, and judging the bare pair
/// against the whole folder lost 0.91 GB of genuine language packs.
fn position_slot_is_mostly_languages(
    seg_lists: &[Vec<Segment>],
    dir_files: Option<&Vec<usize>>,
    slot: (u8, bool, usize),
    members: &[PositionMember],
) -> bool {
    let Some(dir_files) = dir_files else {
        return true;
    };
    // Every member counts here, supported or not. The question is what the
    // dictionary recognizes in this slot, not which members another rule
    // vouched for - and a rider that joins the family later is recognized
    // just the same.
    let member_files: HashSet<usize> = members.iter().map(|m| m.file_idx).collect();
    let Some(width) = members
        .first()
        .and_then(|m| atom_width(seg_lists, m.file_idx, slot.0))
    else {
        return true;
    };

    let mut occupants: HashSet<&str> = HashSet::new();
    let mut recognized: HashSet<&str> = HashSet::new();
    for i in dir_files.iter().copied() {
        let Some(value) = slot_atom(seg_lists, i, slot, width) else {
            continue;
        };
        occupants.insert(value);
        if member_files.contains(&i) {
            recognized.insert(value);
        }
    }
    recognized.len() * 2 > occupants.len()
}

/// Mechanism 3: files in the same directory whose filename carries a
/// recognized language token *at the same atom position* (counted from the
/// start, or from the end, of the filename's strong-delimiter-split atoms)
/// — a generalization of mechanism 1 (which additionally requires the
/// entire remaining shape to be identical) that still requires the
/// language token to sit at a *consistent structural slot* shared by every
/// family member, not merely "anywhere in the same directory".
///
/// This exists to catch real per-language sets whose non-language part
/// differs per file — e.g. `VO_Gameplay_Charles_DE.assets.bank` /
/// `VO_Gameplay_William_FR.assets.bank` / `VO_Gameplay_James_KO.assets.bank`
/// share no common shape once the character name is removed (mechanism 1
/// misses them), but the language code is consistently the *last* atom
/// before the extension in every member.
///
/// The position requirement is what keeps this safe: an early, unbounded
/// version of this mechanism (grouping by directory alone, ignoring
/// position) was found via manual corpus review to flag things like
/// `SP\...\scripted_voice_SK_gd2_vo_english.spk` (`SK` is a level/mission
/// code, not Slovak — every other file in that directory is plain English
/// VO with unrelated mission-code substrings), `GTAIV\...\title_NO_site_e2.wtd`
/// (`no` is the English word, not Norwegian — sitting at a different atom
/// position than the genuine `title_offline_e1_es.wtd`-style siblings), and
/// `manual_00_ID_eng.arc` (the literal, non-varying `ID` substring in every
/// manual filename, not Indonesian — the real varying language token in
/// that family is the *last* atom, one position further along). Requiring
/// same-position occurrences among >= `MIN_FAMILY_SIZE` distinct languages
/// rejects all of these while still confirming the genuine cases above.
fn compute_directory_occurrence_family(
    seg_lists: &[Vec<Segment>],
    occ_lists: &[Vec<Occurrence>],
    keep: &HashSet<String>,
    shadowed: &HashSet<(usize, usize)>,
    cancel: &AtomicBool,
    result: &mut HashMap<usize, FamilyHit>,
) -> CoreResult<()> {
    // (parent_dir, variant, "from start"|"from end", atom index) -> members.
    // `variant` distinguishes two atom sequences per file, since real
    // conventions disagree on whether the language token lives inside or
    // outside the "extension":
    //   0 = the *stem* only (atoms before the first `.`) — needed for
    //       `VO_Gameplay_Charles_DE.assets.bank` vs `R2_Stingers_DE.bank`,
    //       which only share a consistent position once the variable-length
    //       `.bank`/`.assets.bank` tail is excluded.
    //   1 = the *whole filename* including "extension" atoms — needed for
    //       conventions that use the language as the file extension itself
    //       (`Bk_<hash>.FRA` / `Bk_<hash>.DEU` / `Bk_<hash>.RUS`), which the
    //       stem-only variant would exclude entirely.
    let mut position_groups: PositionGroups = HashMap::new();
    let mut by_dir: HashMap<String, Vec<usize>> = HashMap::new();

    for (i, segs) in seg_lists.iter().enumerate() {
        if i % CANCEL_POLL_INTERVAL == 0 {
            check_cancel(cancel)?;
        }
        let Some(filename_seg) = segs.last() else {
            continue;
        };
        let parent_dir = dir_key(segs, segs.len() - 1);
        by_dir.entry(parent_dir.clone()).or_default().push(i);
        let stem_end = filename_seg
            .lower
            .find('.')
            .unwrap_or(filename_seg.lower.len());
        let stem_atoms: Vec<_> = filename_seg
            .atoms
            .iter()
            .filter(|a| a.end <= stem_end)
            .collect();
        let whole_atoms: Vec<_> = filename_seg.atoms.iter().collect();

        for occ in &occ_lists[i] {
            if !occ.is_filename || shadowed.contains(&(i, occ.start)) {
                continue;
            }
            // Only atom-aligned occurrences carry a stable position (a
            // weak-piece match like a curated `pt-br` locale tag spans
            // multiple atoms and doesn't need this mechanism — it's
            // already Level A/B self-sufficient or handled by mechanism 1).
            for (variant, atoms) in [(0u8, &stem_atoms), (1u8, &whole_atoms)] {
                let Some(atom_idx) = atoms
                    .iter()
                    .position(|a| a.start == occ.start && a.end == occ.end)
                else {
                    continue;
                };
                let from_end = atoms.len() - 1 - atom_idx;
                let own_atoms: HashSet<String> = atoms
                    .iter()
                    .filter(|a| a.end <= occ.start || a.start >= occ.end)
                    .map(|a| a.text.clone())
                    .collect();
                let naming_atoms: HashSet<String> = atoms
                    .iter()
                    .filter(|a| a.end <= occ.start || (a.start >= occ.end && a.end <= stem_end))
                    .map(|a| a.text.clone())
                    .collect();
                for (from_start, idx) in [(true, atom_idx), (false, from_end)] {
                    position_groups
                        .entry((parent_dir.clone(), variant, from_start, idx))
                        .or_default()
                        .push(PositionMember {
                            file_idx: i,
                            canonical: occ.canonical,
                            level: occ.level,
                            token_start: occ.start,
                            atoms: own_atoms.clone(),
                            naming_atoms: naming_atoms.clone(),
                        });
                }
            }
        }
    }

    for (group_idx, ((parent_dir, _variant, _from_start, _idx), members)) in
        position_groups.iter().enumerate()
    {
        if group_idx % CANCEL_POLL_INTERVAL == 0 {
            check_cancel(cancel)?;
        }
        // Filter 1 (2026-07-16 report): a "language" that accounts for more
        // than half the group is a constant naming convention, not the
        // varying slot of a per-language set — `*_bg_*` background textures
        // sharing a trailing `_raw` atom with genuine `startup_screen_3_de`
        // siblings (Wreckfest), or `_hi` highlight bitmaps (DA: Origins).
        let mut per_lang: HashMap<&'static str, usize> = HashMap::new();
        for m in members {
            *per_lang.entry(m.canonical).or_default() += 1;
        }
        let survivors: Vec<&PositionMember> = members
            .iter()
            .filter(|m| per_lang[m.canonical] * 2 <= members.len())
            .collect();

        // Filter 2 (2026-07-16 report): a member must be *supported* by a
        // different-language member — sharing a distinctive (non-universal)
        // atom, or 2+ *naming* atoms (a shared stem `VO_Gameplay_Charles_DE`
        // / `VO_Gameplay_William_FR`, or a shared suffix
        // `Spanish(Spain)_patch_1.snd` / `German_patch_1.snd`). Atoms
        // present in more than half the group (framework suffixes like
        // `_SF`) are "universal" and don't count as distinctive on their
        // own. This keeps genuine sets whose stems differ per file
        // (`FemaleVoice9_German` / `FemaleVoice9_Italian`) while rejecting
        // coincidental same-position matches with unrelated names
        // (`wp_consp_ar` against `uefonts_jpn`; `l01_keep_cs` against
        // `dlc_arena_crowd_cries_de`; Flatout `MountSV` vs `Statue_JA`).
        //
        // The second condition used to be "3+ shared atoms" counted over
        // the whole filename, or 2+ shared atoms restricted to those
        // *before* the language token. The first half of that was a
        // loophole rather than a bar (GT-224): it was only ever reached for
        // members C1 had already rejected, and C1 rejects a member
        // precisely when no *other* language carries any of its distinctive
        // atoms — so by construction every atom such a member can still
        // share with a different-language partner is a universal one. "3+
        // shared atoms" therefore read "3+ shared *uninformative* atoms",
        // which a directory of `<name>.item.bytes` files hands out for
        // free: Shadowrun Dragonfall's `ar 2 lady (luckystrike)`, `sr 3
        // eiger's rifle` and `russian grenade 3 (frag)` share `item`,
        // `bytes` and a literal `3`, and on that alone were confirmed a
        // three-language family — "ar" being an assault rifle and "sr" a
        // sniper rifle. Counting `naming_atoms` instead drops the dotted
        // format tail and asks the two names to agree on two words their
        // author actually chose, which those three never do and
        // `_patch_1` sets always do.
        let mut atom_freq: HashMap<&str, usize> = HashMap::new();
        for m in &survivors {
            for atom in &m.atoms {
                *atom_freq.entry(atom.as_str()).or_default() += 1;
            }
        }

        // The naive form of "supported" above is an O(members^2) nested
        // scan with a fresh HashSet allocated per differing-canonical pair
        // - fine for a handful of siblings but explosive for the
        // thousand-file flat directories real game installs produce. The
        // two conditions below are independent (OR'd), so each can be
        // answered from a small index built once over `survivors` instead
        // of a pairwise scan:
        //
        // - C1 (distinctive shared atom): for each distinctive atom
        //   (`atom_freq[a] * 2 <= survivors.len()`), the set of canonicals
        //   carrying it. A member is C1-supported iff one of its own
        //   distinctive atoms has >= 2 distinct canonicals against it -
        //   its own canonical is always one of them, so >= 2 means a
        //   different-canonical member shares it too.
        // - C2 (2+ shared naming atoms): same idea, keyed by unordered
        //   pairs of a member's own naming atoms. `|naming(m) ∩ naming(n)|
        //   >= 2` implies m and n share some concrete pair drawn from that
        //   intersection, so indexing pairs -> canonicals finds it without
        //   ever comparing m against n directly.
        let total = survivors.len();
        let naming_sorted: Vec<Vec<&str>> = survivors
            .iter()
            .map(|m| {
                let mut v: Vec<&str> = m.naming_atoms.iter().map(|s| s.as_str()).collect();
                v.sort_unstable();
                v
            })
            .collect();

        let mut distinctive_atom_canonicals: HashMap<&str, HashSet<&'static str>> = HashMap::new();
        for m in &survivors {
            for atom in &m.atoms {
                if atom_freq[atom.as_str()] * 2 <= total {
                    distinctive_atom_canonicals
                        .entry(atom.as_str())
                        .or_default()
                        .insert(m.canonical);
                }
            }
        }

        // C1b (GT-469): an atom the whole neighbourhood shares is the
        // *strongest* sign a file belongs to the set, and the frequency cut
        // above reads it as the weakest - a stem carried by more than half
        // the group is dismissed as a naming convention and stops supporting
        // anything. Rogue Trooper's `Misc\L_Text\Tannoy\` holds the same
        // eight texts twice, spelled out (`Tannoy_French`) and in the
        // studio's shorthand (`t_fr`). `tannoy` covers eight of the group's
        // thirteen members, so it is written off, and the spelled-out set
        // then hangs on whether `Tannoy_Chinese_T` happens to share the
        // letter `t` with the short set. Two more `t_*` files tip `t` over
        // half as well and both Chinese files vanish; a third tips `tannoy`
        // back under and they return. A verdict that flickers with the file
        // count is not a verdict.
        //
        // So the cut moves from "more than half" to "literally all", and the
        // question becomes *linguistic spread*. An atom carried by
        // `MIN_FAMILY_SIZE` different languages in the same slot is what a
        // per-language naming stem looks like; a framework suffix shared by a
        // texture and a subtitle covers one or two.
        //
        // The atom every single neighbour carries stays worthless, because
        // that is what a format tail is, and letting it through costs real
        // findings: Wreckfest's `data\menu\textures` holds eleven genuine
        // `startup_screen_3_<lang>_1920x1080_raw.bmap` screens beside ten
        // `*_bg_*` backgrounds, and `raw` is in every one of the thirty-two.
        // Supported on `raw` alone, the ten backgrounds join the set as
        // Bulgarian, the folder's label distribution tips, and GT-471 then
        // throws the whole folder out - eleven real languages lost to one
        // meaningless suffix.
        //
        // Nor is a number a word, however many languages carry it: Shadowrun
        // Dragonfall's `ar 2 lady (luckystrike)`, `sr 3 eiger's rifle` and
        // `russian grenade 3 (frag)` agree on a literal `3` and on nothing
        // else, and "ar" is an assault rifle (GT-224).
        //
        // Nor is a one- or two-letter atom, which is the very shape a
        // language code has - which is why it collides with one in the first
        // place. XCOM: Chimera Squad's `wp_consp_ar_SF.upk` is a weapon, and
        // the `_SF` engine suffix it shares with the rest of the folder is
        // not a word about language any more than `_BW` (Burial at Sea) is in
        // BioShock Infinite. Three characters is the shortest thing a studio
        // writes when it means a word rather than a code.
        const MIN_STEM_ATOM_LEN: usize = 3;

        let mut naming_atom_freq: HashMap<&str, usize> = HashMap::new();
        let mut naming_atom_canonicals: HashMap<&str, HashSet<&'static str>> = HashMap::new();
        for m in &survivors {
            for atom in &m.naming_atoms {
                if atom.len() < MIN_STEM_ATOM_LEN || !atom.chars().any(|c| c.is_alphabetic()) {
                    continue;
                }
                *naming_atom_freq.entry(atom.as_str()).or_default() += 1;
                naming_atom_canonicals
                    .entry(atom.as_str())
                    .or_default()
                    .insert(m.canonical);
            }
        }
        naming_atom_canonicals.retain(|atom, _| naming_atom_freq[atom] < total);

        let mut naming_pair_canonicals: HashMap<(&str, &str), HashSet<&'static str>> =
            HashMap::new();
        for (si, m) in survivors.iter().enumerate() {
            let naming = &naming_sorted[si];
            for i in 0..naming.len() {
                for j in (i + 1)..naming.len() {
                    naming_pair_canonicals
                        .entry((naming[i], naming[j]))
                        .or_default()
                        .insert(m.canonical);
                }
            }
        }

        let mut is_supported = vec![false; total];
        for (si, m) in survivors.iter().enumerate() {
            if si % CANCEL_POLL_INTERVAL == 0 {
                check_cancel(cancel)?;
            }
            let c1 = m.atoms.iter().any(|a| {
                distinctive_atom_canonicals
                    .get(a.as_str())
                    .is_some_and(|s| s.len() >= 2)
            });
            if c1 {
                is_supported[si] = true;
                continue;
            }
            let c1b = m.naming_atoms.iter().any(|a| {
                naming_atom_canonicals
                    .get(a.as_str())
                    .is_some_and(|s| s.len() >= MIN_FAMILY_SIZE)
            });
            if c1b {
                is_supported[si] = true;
                continue;
            }
            let naming = &naming_sorted[si];
            is_supported[si] = (0..naming.len()).any(|i| {
                (i + 1..naming.len()).any(|j| {
                    naming_pair_canonicals
                        .get(&(naming[i], naming[j]))
                        .is_some_and(|s| s.len() >= 2)
                })
            });
        }

        let supported: Vec<&&PositionMember> = survivors
            .iter()
            .enumerate()
            .filter(|(si, _)| is_supported[*si])
            .map(|(_, m)| m)
            .collect();

        let distinct: HashSet<&'static str> = supported.iter().map(|m| m.canonical).collect();
        let all_bare = supported.iter().all(|m| m.level == Level::C);
        let threshold = if all_bare {
            MIN_FAMILY_SIZE_BARE_ONLY
        } else {
            MIN_FAMILY_SIZE
        };
        if distinct.len() < threshold {
            continue;
        }
        if !position_slot_is_mostly_languages(
            seg_lists,
            by_dir.get(parent_dir),
            (*_variant, *_from_start, *_idx),
            members,
        ) {
            continue;
        }
        // A survivor that failed pair-support but whose language the
        // supported members already confirmed rides along: once
        // `VO_Gameplay_*_DE/FR/KO` prove a German set exists here,
        // `R2_Stingers_DE` in the same slot is German too. Languages the
        // supported set does NOT contain stay out (`wp_consp_ar` never
        // revives via a ja/ko/de family), and over-represented constants
        // were already dropped before this point.
        //
        // What a *bare-code* rider must still bring is a name of its own.
        // `R2_Stingers` is a file that happens to be German; `cs.ttf` is a
        // file called "cs", and Counter-Strike's own UI font was landing in
        // the Czech localization on nothing more than that (GT-224) - it
        // occupies the same "last atom of the stem" slot as the genuine
        // `cstrike_czech.txt` / `gameui_german.txt` set while sharing not
        // one word with it. Riding along is the one route into this
        // mechanism that asks a file for no evidence at all, so a name that
        // is nothing but a two-letter code may not take it: those collide
        // with ordinary short words, which is the same reason
        // `MIN_FAMILY_SIZE_BARE_ONLY` exists. A stem that is nothing but a
        // full name or an iso3 code still rides - `russian.pack` beside
        // `patch_1_russian.pack`, Path of Exile's
        // `russian.datc64_1.bundle.bin` - because "russian" is not a word a
        // file falls into by accident.
        let flagged: Vec<&&PositionMember> = survivors
            .iter()
            .filter(|m| {
                distinct.contains(m.canonical)
                    && (m.level != Level::C || !m.naming_atoms.is_empty())
            })
            .collect();
        for member in &flagged {
            if keep.contains(member.canonical) {
                continue;
            }
            let reason = LangReason::new(LangEvidence::FamilyAtSharedPosition {
                languages: distinct.len(),
                dir: display_dir(parent_dir),
            });
            upsert(
                result,
                member.file_idx,
                FamilyHit {
                    canonical: member.canonical,
                    confidence: family_confidence(member.level),
                    reason,
                    token_start: member.token_start,
                    family_size: distinct.len(),
                },
            );
        }
    }
    Ok(())
}

/// Mechanism 2: sibling subdirectories whose entire name is a recognized
/// language token (`en/ de/ fr/ es/`).
fn compute_folder_family(
    data: &LangData,
    seg_lists: &[Vec<Segment>],
    keep: &HashSet<String>,
    cancel: &AtomicBool,
    result: &mut HashMap<usize, FamilyHit>,
) -> CoreResult<()> {
    // parent_prefix -> child segment text (lowercase) -> (canonical, level)
    let mut folder_children: HashMap<String, HashMap<String, (&'static str, Level)>> =
        HashMap::new();
    // parent_prefix -> *every* child folder name, language or not. The
    // denominator - see `MIN_LANGUAGE_FOLDER_SHARE`.
    let mut all_children: HashMap<String, HashSet<String>> = HashMap::new();

    for (i, segs) in seg_lists.iter().enumerate() {
        if i % CANCEL_POLL_INTERVAL == 0 {
            check_cancel(cancel)?;
        }
        if segs.len() < 2 {
            continue; // file sits at the library root, no folders at all
        }
        for j in 0..segs.len() - 1 {
            let parent_prefix = dir_key(segs, j);
            let child = &segs[j];
            all_children
                .entry(parent_prefix.clone())
                .or_default()
                .insert(child.lower.clone());
            if let Some((canonical, level)) = data.lookup(&child.lower) {
                folder_children
                    .entry(parent_prefix)
                    .or_default()
                    .insert(child.lower.clone(), (canonical, level));
            }
        }
    }

    let mut confirmed: HashMap<(String, String), (&'static str, u8, usize)> = HashMap::new();
    for (parent_prefix, children) in &folder_children {
        let distinct: HashSet<&'static str> = children.values().map(|(c, _)| *c).collect();
        if distinct.len() < MIN_FAMILY_SIZE {
            continue;
        }
        // A handful of language names lost among hundreds of siblings is a
        // storage tree that happens to spell a few of them, not a set of
        // translations - GT-465.
        let siblings = all_children.get(parent_prefix).map_or(0, HashSet::len);
        if children.len() * MIN_LANGUAGE_FOLDER_SHARE < siblings {
            continue;
        }
        for (child_lower, value) in children {
            let (canonical, level) = *value;
            if keep.contains(canonical) {
                continue;
            }
            confirmed.insert(
                (parent_prefix.clone(), child_lower.clone()),
                (canonical, family_confidence(level), distinct.len()),
            );
        }
    }

    if confirmed.is_empty() {
        return Ok(());
    }

    for (i, segs) in seg_lists.iter().enumerate() {
        if i % CANCEL_POLL_INTERVAL == 0 {
            check_cancel(cancel)?;
        }
        if segs.len() < 2 {
            continue;
        }
        for j in 0..segs.len() - 1 {
            let parent_prefix = dir_key(segs, j);
            let child_lower = segs[j].lower.clone();
            if let Some(&(canonical, confidence, count)) =
                confirmed.get(&(parent_prefix.clone(), child_lower))
            {
                let reason = LangReason::new(LangEvidence::SubfolderFamily {
                    languages: count,
                    dir: display_dir(&parent_prefix),
                });
                upsert(
                    result,
                    i,
                    FamilyHit {
                        canonical,
                        confidence,
                        reason,
                        // A folder family names no token inside the file
                        // name, so it has no position to offer and loses a
                        // tie to a family that does.
                        token_start: 0,
                        family_size: count,
                    },
                );
            }
        }
    }
    Ok(())
}

/// Mechanism 4 (2026-07-16 report, Mafia II miss): sibling subdirectories
/// that share an identical name shape around a varying language token —
/// `sds_de\ sds_fr\ sds_jp\ sds_pl\ ...`. Mechanism 2 requires the *entire*
/// folder name to be a language token, so prefixed conventions like Mafia
/// II's `sds_<lang>` were invisible even though the set structure is
/// unmistakable. Like mechanism 2, a confirmed prefixed folder family flags
/// every file underneath a non-keep member folder (files inside carry no
/// language token of their own).
fn compute_prefixed_folder_family(
    seg_lists: &[Vec<Segment>],
    occ_lists: &[Vec<Occurrence>],
    keep: &HashSet<String>,
    cancel: &AtomicBool,
    result: &mut HashMap<usize, FamilyHit>,
) -> CoreResult<()> {
    // (parent_prefix, folder-name shape) -> child folder lower ->
    // (canonical, level).
    type ShapeChildren = HashMap<(String, String), HashMap<String, (&'static str, Level)>>;
    let mut shape_children: ShapeChildren = HashMap::new();

    for (i, segs) in seg_lists.iter().enumerate() {
        if i % CANCEL_POLL_INTERVAL == 0 {
            check_cancel(cancel)?;
        }
        for occ in &occ_lists[i] {
            if occ.is_filename || occ.whole_segment {
                continue; // whole-segment folders are mechanism 2's job
            }
            let seg = &segs[occ.seg_index];
            let parent_prefix = dir_key(segs, seg.index);
            let shape = shape_of(&seg.lower, occ.start, occ.end);
            shape_children
                .entry((parent_prefix, shape))
                .or_default()
                .insert(seg.lower.clone(), (occ.canonical, occ.level));
        }
    }

    let mut confirmed: HashMap<(String, String), (&'static str, u8, usize)> = HashMap::new();
    for ((parent_prefix, _shape), children) in &shape_children {
        let distinct: HashSet<&'static str> = children.values().map(|(c, _)| *c).collect();
        let all_bare = children.values().all(|(_, level)| *level == Level::C);
        let threshold = if all_bare {
            MIN_FAMILY_SIZE_BARE_ONLY
        } else {
            MIN_FAMILY_SIZE
        };
        if distinct.len() < threshold {
            continue;
        }
        for (child_lower, &(canonical, level)) in children {
            if keep.contains(canonical) {
                continue;
            }
            let claim = (canonical, family_confidence(level), distinct.len());
            confirmed
                .entry((parent_prefix.clone(), child_lower.clone()))
                .and_modify(|held| {
                    if claim_rank(&claim) > claim_rank(held) {
                        *held = claim;
                    }
                })
                .or_insert(claim);
        }
    }

    if confirmed.is_empty() {
        return Ok(());
    }

    for (i, segs) in seg_lists.iter().enumerate() {
        if i % CANCEL_POLL_INTERVAL == 0 {
            check_cancel(cancel)?;
        }
        if segs.len() < 2 {
            continue;
        }
        for j in 0..segs.len() - 1 {
            let parent_prefix = dir_key(segs, j);
            if let Some(&(canonical, confidence, count)) =
                confirmed.get(&(parent_prefix.clone(), segs[j].lower.clone()))
            {
                let reason = LangReason::new(LangEvidence::SubfolderFamilyWithPrefix {
                    languages: count,
                    dir: display_dir(&parent_prefix),
                });
                upsert(
                    result,
                    i,
                    FamilyHit {
                        canonical,
                        confidence,
                        reason,
                        // A folder family names no token inside the file
                        // name, so it has no position to offer and loses a
                        // tie to a family that does.
                        token_start: 0,
                        family_size: count,
                    },
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::langdetect::occurrences::collect_occurrences;
    use crate::langdetect::tokens::tokenize_path;

    /// The tests never exercise cancellation — a fresh, never-set flag.
    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn data() -> std::sync::Arc<LangData> {
        LangData::builtin()
    }

    fn keep_default() -> HashSet<String> {
        ["uk".to_string(), "en".to_string()].into_iter().collect()
    }

    #[test]
    fn file_shape_family_confirmed_with_three_siblings() {
        let paths = [
            "sound\\Voice_english.pak",
            "sound\\Voice_french.pak",
            "sound\\Voice_german.pak",
            "sound\\Voice_polish.pak",
        ];
        let seg_lists: Vec<_> = paths.iter().map(|p| tokenize_path(p)).collect();
        let occ_lists: Vec<_> = seg_lists
            .iter()
            .map(|s| collect_occurrences(&data(), s))
            .collect();

        let hits = compute_family(
            &data(),
            &seg_lists,
            &occ_lists,
            &keep_default(),
            &no_cancel(),
        )
        .unwrap();

        assert!(!hits.contains_key(&0), "english is kept");
        assert!(hits.contains_key(&1), "french should be flagged");
        assert!(hits.contains_key(&2), "german should be flagged");
        assert!(hits.contains_key(&3), "polish should be flagged");
    }

    #[test]
    fn folder_family_confirmed_with_three_language_subfolders() {
        let paths = [
            "root\\en\\file1.txt",
            "root\\de\\file2.txt",
            "root\\fr\\file3.txt",
            "root\\es\\file4.txt",
        ];
        let seg_lists: Vec<_> = paths.iter().map(|p| tokenize_path(p)).collect();
        let occ_lists: Vec<_> = seg_lists
            .iter()
            .map(|s| collect_occurrences(&data(), s))
            .collect();

        let hits = compute_family(
            &data(),
            &seg_lists,
            &occ_lists,
            &keep_default(),
            &no_cancel(),
        )
        .unwrap();

        assert!(!hits.contains_key(&0), "en/ folder is kept");
        assert!(hits.contains_key(&1), "de/ folder should be flagged");
        assert!(hits.contains_key(&2), "fr/ folder should be flagged");
        assert!(hits.contains_key(&3), "es/ folder should be flagged");
    }

    /// GT-224: `cstrike\\resource\\cs.ttf` is Counter-Strike's own UI font,
    /// not a Czech localization - "cs" is the game's own abbreviation. It
    /// occupies the same "last atom of the stem" slot as the genuine
    /// `cstrike_czech.txt` / `gameui_german.txt` set, but shares no naming
    /// word with any of them.
    #[test]
    fn bare_code_with_no_naming_of_its_own_is_not_a_family_member() {
        let mut paths = vec!["cstrike\\resource\\cs.ttf".to_string()];
        for lang in [
            "czech", "danish", "dutch", "french", "german", "greek", "italian", "japanese",
            "korean", "polish", "russian", "spanish", "swedish", "turkish",
        ] {
            paths.push(format!("cstrike\\resource\\cstrike_{lang}.txt"));
            paths.push(format!("cstrike\\resource\\gameui_{lang}.txt"));
        }
        let seg_lists: Vec<_> = paths.iter().map(|p| tokenize_path(p)).collect();
        let occ_lists: Vec<_> = seg_lists
            .iter()
            .map(|s| collect_occurrences(&data(), s))
            .collect();

        let hits = compute_family(
            &data(),
            &seg_lists,
            &occ_lists,
            &keep_default(),
            &no_cancel(),
        )
        .unwrap();

        assert!(!hits.contains_key(&0), "cs.ttf is the game font, not Czech");
        assert!(hits.contains_key(&1), "cstrike_czech.txt is a real member");
    }

    /// GT-224: Shadowrun Dragonfall item files. "ar" is an assault rifle and
    /// "sr" a sniper rifle; all these names share is the `.item.bytes`
    /// extension every file in the directory carries, plus a literal `3`.
    #[test]
    fn shared_extension_atoms_alone_do_not_support_a_family() {
        let paths = [
            "data\\items\\ar 2 lady (luckystrike).item.bytes",
            "data\\items\\ar 3 lady (luckystrike).item.bytes",
            "data\\items\\russian grenade 3 (frag).item.bytes",
            "data\\items\\sr 3 eiger's rifle.item.bytes",
        ];
        let seg_lists: Vec<_> = paths.iter().map(|p| tokenize_path(p)).collect();
        let occ_lists: Vec<_> = seg_lists
            .iter()
            .map(|s| collect_occurrences(&data(), s))
            .collect();

        let hits = compute_family(
            &data(),
            &seg_lists,
            &occ_lists,
            &keep_default(),
            &no_cancel(),
        )
        .unwrap();

        assert!(
            hits.is_empty(),
            "item names are not a language family: {hits:?}"
        );
    }

    /// The naming-suffix set mechanism 1 misses (the paren qualifier breaks
    /// the shape) and C2 must keep: two shared stem words, `patch` and `1`.
    #[test]
    fn shared_naming_suffix_still_confirms_a_family() {
        let paths = [
            "sound\\Spanish(Spain)_patch_1.snd",
            "sound\\French(France)_patch_1.snd",
            "sound\\German_patch_1.snd",
        ];
        let seg_lists: Vec<_> = paths.iter().map(|p| tokenize_path(p)).collect();
        let occ_lists: Vec<_> = seg_lists
            .iter()
            .map(|s| collect_occurrences(&data(), s))
            .collect();

        let hits = compute_family(
            &data(),
            &seg_lists,
            &occ_lists,
            &keep_default(),
            &no_cancel(),
        )
        .unwrap();

        assert_eq!(hits.len(), 3, "{hits:?}");
    }

    #[test]
    fn no_family_below_threshold() {
        let paths = ["root\\en\\file1.txt", "root\\de\\file2.txt"];
        let seg_lists: Vec<_> = paths.iter().map(|p| tokenize_path(p)).collect();
        let occ_lists: Vec<_> = seg_lists
            .iter()
            .map(|s| collect_occurrences(&data(), s))
            .collect();

        let hits = compute_family(
            &data(),
            &seg_lists,
            &occ_lists,
            &keep_default(),
            &no_cancel(),
        )
        .unwrap();
        assert!(hits.is_empty(), "only 2 siblings must not confirm a family");
    }
}
