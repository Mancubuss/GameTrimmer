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
const SHAPE_PLACEHOLDER: char = '\u{1}';

/// One family-candidate occurrence: (file index, canonical language, trust level).
type FamilyMember = (usize, &'static str, Level);

/// One mechanism-3 candidate: the file's remaining (non-language) atoms are
/// kept — split into those before and after the language token — so
/// coincidental same-position matches can be filtered out. See
/// `compute_directory_occurrence_family`.
struct PositionMember {
    file_idx: usize,
    canonical: &'static str,
    level: Level,
    /// Non-language atoms preceding the language token in the name.
    before_atoms: HashSet<String>,
    /// Non-language atoms following it (suffixes, extensions).
    after_atoms: HashSet<String>,
}

impl PositionMember {
    fn all_atoms(&self) -> impl Iterator<Item = &String> {
        self.before_atoms.iter().chain(self.after_atoms.iter())
    }
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
}

fn family_confidence(level: Level) -> u8 {
    match level {
        Level::A => 95,
        Level::B => 92,
        Level::C => 90,
    }
}

fn upsert(map: &mut HashMap<usize, FamilyHit>, file_idx: usize, hit: FamilyHit) {
    let replace = match map.get(&file_idx) {
        Some(existing) => hit.confidence > existing.confidence,
        None => true,
    };
    if replace {
        map.insert(file_idx, hit);
    }
}

fn dir_key(segs: &[Segment], upto: usize) -> String {
    segs[..upto]
        .iter()
        .map(|s| s.lower.as_str())
        .collect::<Vec<_>>()
        .join("\\")
}

fn display_dir(dir_key: &str) -> String {
    if dir_key.is_empty() {
        "<корінь>".to_string()
    } else {
        dir_key.to_string()
    }
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
    compute_file_shape_family(seg_lists, occ_lists, keep, cancel, &mut result)?;
    compute_directory_occurrence_family(seg_lists, occ_lists, keep, cancel, &mut result)?;
    compute_folder_family(data, seg_lists, keep, cancel, &mut result)?;
    compute_prefixed_folder_family(seg_lists, occ_lists, keep, cancel, &mut result)?;
    Ok(result)
}

/// Mechanism 1: sibling files in the same directory whose name differs only
/// by the recognized language token.
fn compute_file_shape_family(
    seg_lists: &[Vec<Segment>],
    occ_lists: &[Vec<Occurrence>],
    keep: &HashSet<String>,
    cancel: &AtomicBool,
    result: &mut HashMap<usize, FamilyHit>,
) -> CoreResult<()> {
    let mut shape_groups: ShapeGroups = HashMap::new();

    for (i, segs) in seg_lists.iter().enumerate() {
        if i % CANCEL_POLL_INTERVAL == 0 {
            check_cancel(cancel)?;
        }
        let Some(filename_seg) = segs.last() else {
            continue;
        };
        let parent_dir = dir_key(segs, segs.len() - 1);

        for occ in &occ_lists[i] {
            if !occ.is_filename {
                continue;
            }
            let shape = shape_of(&filename_seg.lower, occ.start, occ.end);
            shape_groups
                .entry((parent_dir.clone(), shape))
                .or_default()
                .push((i, occ.canonical, occ.level));
        }
    }

    for ((parent_dir, _shape), members) in &shape_groups {
        let distinct: HashSet<&'static str> = members.iter().map(|(_, c, _)| *c).collect();
        let all_bare = members.iter().all(|(_, _, level)| *level == Level::C);
        let threshold = if all_bare {
            MIN_SHAPE_FAMILY_SIZE_BARE_ONLY
        } else {
            MIN_FAMILY_SIZE
        };
        if distinct.len() < threshold {
            continue;
        }
        for (file_idx, canonical, level) in members.iter().copied() {
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
                },
            );
        }
    }
    Ok(())
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

    for (i, segs) in seg_lists.iter().enumerate() {
        if i % CANCEL_POLL_INTERVAL == 0 {
            check_cancel(cancel)?;
        }
        let Some(filename_seg) = segs.last() else {
            continue;
        };
        let parent_dir = dir_key(segs, segs.len() - 1);
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
            if !occ.is_filename {
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
                let before_atoms: HashSet<String> = atoms
                    .iter()
                    .filter(|a| a.end <= occ.start)
                    .map(|a| a.text.clone())
                    .collect();
                let after_atoms: HashSet<String> = atoms
                    .iter()
                    .filter(|a| a.start >= occ.end)
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
                            before_atoms: before_atoms.clone(),
                            after_atoms: after_atoms.clone(),
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
        // different-language member — sharing a distinctive
        // (non-universal) atom, or 3+ atoms outright, or 2+ atoms that sit
        // *before* the language token in both names (a shared naming stem:
        // `VO_Gameplay_Charles_DE` / `VO_Gameplay_William_FR`). Atoms
        // present in more than half the group (framework suffixes like
        // `_SF`, extensions) are "universal" and don't count as
        // distinctive on their own. This keeps genuine sets whose stems
        // differ per file (`FemaleVoice9_German` / `FemaleVoice9_Italian`;
        // `Spanish(Spain)_patch_1.snd` / `French(France)_patch_1.snd`)
        // while rejecting coincidental same-position matches with
        // unrelated names (`wp_consp_ar` against `uefonts_jpn` share only
        // the trailing universal `sf`+`upk`; `l01_keep_cs` against
        // `dlc_arena_crowd_cries_de`; Flatout `MountSV` vs `Statue_JA`).
        let mut atom_freq: HashMap<&str, usize> = HashMap::new();
        for m in &survivors {
            for atom in m.all_atoms() {
                *atom_freq.entry(atom.as_str()).or_default() += 1;
            }
        }

        // The naive form of "supported" above is an O(members^2) nested
        // scan with a fresh HashSet allocated per differing-canonical pair
        // - fine for a handful of siblings but explosive for the
        // thousand-file flat directories real game installs produce. The
        // three conditions below (distinctive shared atom / 3+ shared
        // atoms / 2+ shared before-atoms) are independent (OR'd), so each
        // can be answered from a small index built once over `survivors`
        // instead of a pairwise scan:
        //
        // - C1 (distinctive shared atom): for each distinctive atom
        //   (`atom_freq[a] * 2 <= survivors.len()`), the set of canonicals
        //   carrying it. A member is C1-supported iff one of its own
        //   distinctive atoms has >= 2 distinct canonicals against it -
        //   its own canonical is always one of them, so >= 2 means a
        //   different-canonical member shares it too.
        // - C3 (2+ shared before-atoms): same idea, keyed by unordered
        //   pairs of a member's own before-atoms. `|before(m) ∩
        //   before(n)| >= 2` implies m and n share some concrete pair
        //   drawn from that intersection, so indexing pairs -> canonicals
        //   finds it without ever comparing m against n directly.
        // - C2 (3+ shared atoms, multiplicity-aware): only evaluated for
        //   members not already C1/C3-supported. `max_mult[a]` bounds how
        //   many times atom `a` can appear in any single member's
        //   `all_atoms()`; if a member's own atoms can't sum to 3 even at
        //   that bound, no partner can push it over 3 shared, so it's
        //   skipped outright. Otherwise only candidates sharing >= 1 atom
        //   (via a postings index) are checked, each with the exact
        //   original expression so multiplicity semantics match
        //   precisely.
        let total = survivors.len();
        let all_atoms_sets: Vec<HashSet<&str>> = survivors
            .iter()
            .map(|m| m.all_atoms().map(|a| a.as_str()).collect())
            .collect();
        let before_sorted: Vec<Vec<&str>> = survivors
            .iter()
            .map(|m| {
                let mut v: Vec<&str> = m.before_atoms.iter().map(|s| s.as_str()).collect();
                v.sort_unstable();
                v
            })
            .collect();

        let mut distinctive_atom_canonicals: HashMap<&str, HashSet<&'static str>> = HashMap::new();
        for (si, m) in survivors.iter().enumerate() {
            for &atom in &all_atoms_sets[si] {
                if atom_freq[atom] * 2 <= total {
                    distinctive_atom_canonicals
                        .entry(atom)
                        .or_default()
                        .insert(m.canonical);
                }
            }
        }

        let mut before_pair_canonicals: HashMap<(&str, &str), HashSet<&'static str>> =
            HashMap::new();
        for (si, m) in survivors.iter().enumerate() {
            let before = &before_sorted[si];
            for i in 0..before.len() {
                for j in (i + 1)..before.len() {
                    before_pair_canonicals
                        .entry((before[i], before[j]))
                        .or_default()
                        .insert(m.canonical);
                }
            }
        }

        let mut max_mult: HashMap<&str, u8> = HashMap::new();
        let mut postings: HashMap<&str, Vec<usize>> = HashMap::new();
        for (si, m) in survivors.iter().enumerate() {
            for &atom in &all_atoms_sets[si] {
                let mult = m.before_atoms.contains(atom) as u8 + m.after_atoms.contains(atom) as u8;
                let entry = max_mult.entry(atom).or_insert(0);
                if mult > *entry {
                    *entry = mult;
                }
                postings.entry(atom).or_default().push(si);
            }
        }
        let maxpossible: Vec<u32> = all_atoms_sets
            .iter()
            .map(|atoms| atoms.iter().map(|a| u32::from(max_mult[a])).sum())
            .collect();

        let mut is_supported = vec![false; total];
        for (si, m) in survivors.iter().enumerate() {
            if si % CANCEL_POLL_INTERVAL == 0 {
                check_cancel(cancel)?;
            }
            let c1 = all_atoms_sets[si].iter().any(|a| {
                distinctive_atom_canonicals
                    .get(a)
                    .is_some_and(|s| s.len() >= 2)
            });
            if c1 {
                is_supported[si] = true;
                continue;
            }
            let before = &before_sorted[si];
            let c3 = (0..before.len()).any(|i| {
                (i + 1..before.len()).any(|j| {
                    before_pair_canonicals
                        .get(&(before[i], before[j]))
                        .is_some_and(|s| s.len() >= 2)
                })
            });
            if c3 {
                is_supported[si] = true;
                continue;
            }
            if maxpossible[si] < 3 {
                continue;
            }
            let m_all = &all_atoms_sets[si];
            let mut seen: HashSet<usize> = HashSet::new();
            let mut c2 = false;
            'candidates: for &atom in m_all.iter() {
                let Some(candidates) = postings.get(atom) else {
                    continue;
                };
                for &ni in candidates {
                    if ni == si || !seen.insert(ni) {
                        continue;
                    }
                    let n = survivors[ni];
                    if n.canonical == m.canonical {
                        continue;
                    }
                    let shared_count = n.all_atoms().filter(|a| m_all.contains(a.as_str())).count();
                    if shared_count >= 3 {
                        c2 = true;
                        break 'candidates;
                    }
                }
            }
            is_supported[si] = c2;
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
        // A survivor that failed pair-support but whose language the
        // supported members already confirmed rides along: once
        // `VO_Gameplay_*_DE/FR/KO` prove a German set exists here,
        // `R2_Stingers_DE` in the same slot is German too. Languages the
        // supported set does NOT contain stay out (`wp_consp_ar` never
        // revives via a ja/ko/de family), and over-represented constants
        // were already dropped before this point.
        let flagged: Vec<&&PositionMember> = survivors
            .iter()
            .filter(|m| distinct.contains(m.canonical))
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
