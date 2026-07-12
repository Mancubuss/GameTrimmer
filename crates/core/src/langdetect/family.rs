//! The "language family" heuristic (docs/04_implementation_plan.md §5.4) —
//! the strongest signal in the engine. Three independent shapes of evidence:
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

use crate::langdetect::dict::{self, Level};
use crate::langdetect::occurrences::Occurrence;
use crate::langdetect::tokens::Segment;

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
const SHAPE_PLACEHOLDER: char = '\u{1}';

/// One family-candidate occurrence: (file index, canonical language, trust level).
type FamilyMember = (usize, &'static str, Level);

/// (parent_dir, shape-with-placeholder) -> sibling occurrences.
type ShapeGroups = HashMap<(String, String), Vec<FamilyMember>>;

/// (parent_dir, atom-sequence variant, "from start"|"from end", atom index)
/// -> sibling occurrences. See `compute_directory_occurrence_family` for
/// what "variant" distinguishes.
type PositionGroups = HashMap<(String, u8, bool, usize), Vec<FamilyMember>>;

#[derive(Debug, Clone)]
pub struct FamilyHit {
    pub canonical: &'static str,
    pub confidence: u8,
    pub reason: String,
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

/// Computes family-confirmed findings across every file of one game.
/// `seg_lists`/`occ_lists` must be index-aligned with the game's file list.
pub fn compute_family(
    seg_lists: &[Vec<Segment>],
    occ_lists: &[Vec<Occurrence>],
    keep: &HashSet<String>,
) -> HashMap<usize, FamilyHit> {
    let mut result = HashMap::new();
    compute_file_shape_family(seg_lists, occ_lists, keep, &mut result);
    compute_directory_occurrence_family(seg_lists, occ_lists, keep, &mut result);
    compute_folder_family(seg_lists, keep, &mut result);
    result
}

/// Mechanism 1: sibling files in the same directory whose name differs only
/// by the recognized language token.
fn compute_file_shape_family(
    seg_lists: &[Vec<Segment>],
    occ_lists: &[Vec<Occurrence>],
    keep: &HashSet<String>,
    result: &mut HashMap<usize, FamilyHit>,
) {
    let mut shape_groups: ShapeGroups = HashMap::new();

    for (i, segs) in seg_lists.iter().enumerate() {
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
        if distinct.len() < MIN_FAMILY_SIZE {
            continue;
        }
        for (file_idx, canonical, level) in members.iter().copied() {
            if keep.contains(canonical) {
                continue;
            }
            let reason = format!(
                "мовна сім'я з {} мов у теці '{}'",
                distinct.len(),
                display_dir(parent_dir)
            );
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
    result: &mut HashMap<usize, FamilyHit>,
) {
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
                position_groups
                    .entry((parent_dir.clone(), variant, true, atom_idx))
                    .or_default()
                    .push((i, occ.canonical, occ.level));
                position_groups
                    .entry((parent_dir.clone(), variant, false, from_end))
                    .or_default()
                    .push((i, occ.canonical, occ.level));
            }
        }
    }

    for ((parent_dir, _variant, _from_start, _idx), members) in &position_groups {
        let distinct: HashSet<&'static str> = members.iter().map(|(_, c, _)| *c).collect();
        let all_bare = members.iter().all(|(_, _, level)| *level == Level::C);
        let threshold = if all_bare {
            MIN_FAMILY_SIZE_BARE_ONLY
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
            let reason = format!(
                "мовна сім'я з {} мов у теці '{}' (спільна позиція токена)",
                distinct.len(),
                display_dir(parent_dir)
            );
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
}

/// Mechanism 2: sibling subdirectories whose entire name is a recognized
/// language token (`en/ de/ fr/ es/`).
fn compute_folder_family(
    seg_lists: &[Vec<Segment>],
    keep: &HashSet<String>,
    result: &mut HashMap<usize, FamilyHit>,
) {
    // parent_prefix -> child segment text (lowercase) -> (canonical, level)
    let mut folder_children: HashMap<String, HashMap<String, (&'static str, Level)>> =
        HashMap::new();

    for segs in seg_lists {
        if segs.len() < 2 {
            continue; // file sits at the library root, no folders at all
        }
        for j in 0..segs.len() - 1 {
            let parent_prefix = dir_key(segs, j);
            let child = &segs[j];
            if let Some((canonical, level)) = dict::lookup(&child.lower) {
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
        return;
    }

    for (i, segs) in seg_lists.iter().enumerate() {
        if segs.len() < 2 {
            continue;
        }
        for j in 0..segs.len() - 1 {
            let parent_prefix = dir_key(segs, j);
            let child_lower = segs[j].lower.clone();
            if let Some(&(canonical, confidence, count)) =
                confirmed.get(&(parent_prefix.clone(), child_lower))
            {
                let reason = format!(
                    "мовна сім'я підтек з {count} мов у теці '{}'",
                    display_dir(&parent_prefix)
                );
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::langdetect::occurrences::collect_occurrences;
    use crate::langdetect::tokens::tokenize_path;

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
        let occ_lists: Vec<_> = seg_lists.iter().map(|s| collect_occurrences(s)).collect();

        let hits = compute_family(&seg_lists, &occ_lists, &keep_default());

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
        let occ_lists: Vec<_> = seg_lists.iter().map(|s| collect_occurrences(s)).collect();

        let hits = compute_family(&seg_lists, &occ_lists, &keep_default());

        assert!(!hits.contains_key(&0), "en/ folder is kept");
        assert!(hits.contains_key(&1), "de/ folder should be flagged");
        assert!(hits.contains_key(&2), "fr/ folder should be flagged");
        assert!(hits.contains_key(&3), "es/ folder should be flagged");
    }

    #[test]
    fn no_family_below_threshold() {
        let paths = ["root\\en\\file1.txt", "root\\de\\file2.txt"];
        let seg_lists: Vec<_> = paths.iter().map(|p| tokenize_path(p)).collect();
        let occ_lists: Vec<_> = seg_lists.iter().map(|s| collect_occurrences(s)).collect();

        let hits = compute_family(&seg_lists, &occ_lists, &keep_default());
        assert!(hits.is_empty(), "only 2 siblings must not confirm a family");
    }
}
