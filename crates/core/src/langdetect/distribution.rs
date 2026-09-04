//! Rejects whole label sets whose *shape* says "naming scheme", not
//! "translation" — the last pass before findings leave the detector.
//!
//! The mechanisms in `family.rs`, `markers.rs` and `occurrences.rs` all
//! answer the same question one file at a time: does this name carry a
//! language token in a trustworthy context? Some naming schemes pass that
//! test honestly and are still not localization, because the token means
//! something else in that game — `sr` in Bungie's package names, `pl` for
//! *player* in Dragon's Dogma, `kr` for *Kingdom Rush*, `br` for *battle
//! royale* in Call of Duty. No amount of per-file context settles those; the
//! answer is only visible in the company the label keeps.
//!
//! A translated game is translated whole, so a real label set inside one
//! folder is roughly even: Forza's `pt-br:217 fr:216 de:215 it:171 ko:163`,
//! XCOM 2's `fr:10609 it:10594 de:10591 es:10024`, Metro Exodus's one file
//! per language. A naming scheme has one steep head and a long accidental
//! tail, because only one of its labels is a real word in the scheme:
//! Call of Duty's `ar:1020 tr:146 es:145 ...` (assault rifle), Red Faction's
//! `cs:21` against `:2`. So the measure is the largest label against the
//! *third* largest — the second is too easy for a two-language game to trip.
//!
//! Two shapes are rejected, measured over the library on 2026-09-04:
//!
//! - **A steep head** (`MIN_DOMINANCE`, 13 folders, 14.24 GB): Call of Duty
//!   8.47 GB, LET IT DIE 3.55, Red Faction 1.90, plus ARK's ICU tables, a
//!   bundled JRE and a bundled Python's `encodings`. The tightest genuine
//!   set in the library sits at 2.75x, so 4.0x keeps a full ratio of margin.
//! - **A saturated folder** (`MIN_SATURATED_FILES`, 8 folders, 41.45 GB):
//!   one language, on nothing better than a two-letter code with an asset
//!   word beside it, over five files or more. A real language pack has
//!   sibling languages — that is what makes it a pack; a lone bare code
//!   repeated through a folder is a constant in the file-name grammar.
//!   Destiny 2's `packages\` (121 files, 41.42 GB) is the whole prize here.
//!
//! Both are asked only of findings the *file name* claimed. When a directory
//! on the path names the language (`Localization\FRA\`, `fonts\rus\`,
//! `audio\speech\sp\`), the folder itself is the evidence and its shape says
//! nothing: Deadfall Adventures' `Localization\FRA` reads `fr:201 id:8 ar:4`
//! — a 50x head that is entirely correct, and the tail is the error. Judging
//! those folders by distribution would delete the right answer and keep the
//! wrong one.

use std::collections::HashMap;

use crate::langdetect::data::LangData;
use crate::langdetect::dict::Level;
use crate::langdetect::occurrences::Occurrence;
use crate::langdetect::reason::LangEvidence;
use crate::langdetect::LangFinding;
use crate::scanner::FileEntry;

/// How many times the commonest label must outrun the third commonest before
/// the set reads as a naming scheme. The library's most lopsided *genuine*
/// folder is 2.75x and its least lopsided rejected one is 4.0x.
const MIN_DOMINANCE: f32 = 4.0;

/// How many files one lone two-letter code must cover before it reads as a
/// constant in the naming scheme rather than a small language pack.
const MIN_SATURATED_FILES: usize = 5;

/// Drops the findings whose label set is technical rather than linguistic.
///
/// Called once per game, over the findings only — a few hundred entries
/// against the millions of files behind them.
pub fn drop_technical_code_sets(
    data: &LangData,
    files: &[FileEntry],
    occurrences: &[Vec<Occurrence>],
    findings: &mut Vec<(usize, LangFinding)>,
) {
    let mut by_dir: HashMap<&str, Vec<usize>> = HashMap::new();
    for (slot, (file_index, finding)) in findings.iter().enumerate() {
        if language_is_named_by_a_directory(&occurrences[*file_index], &finding.lang_tag) {
            continue;
        }
        let rel = files[*file_index].rel_path.as_str();
        let dir = rel.rfind('\\').map_or("", |cut| &rel[..cut]);
        by_dir.entry(dir).or_default().push(slot);
    }

    let mut rejected = vec![false; findings.len()];
    for slots in by_dir.values() {
        let mut per_label: HashMap<&str, usize> = HashMap::new();
        for slot in slots {
            *per_label
                .entry(findings[*slot].1.lang_tag.as_str())
                .or_default() += 1;
        }
        if !is_technical(data, &per_label, slots, findings) {
            continue;
        }
        for slot in slots {
            rejected[*slot] = true;
        }
    }

    let mut slot = 0;
    findings.retain(|_| {
        let keep = !rejected[slot];
        slot += 1;
        keep
    });
}

fn is_technical(
    data: &LangData,
    per_label: &HashMap<&str, usize>,
    slots: &[usize],
    findings: &[(usize, LangFinding)],
) -> bool {
    let mut counts: Vec<usize> = per_label.values().copied().collect();
    counts.sort_unstable_by(|a, b| b.cmp(a));
    match counts.len() {
        1 => {
            slots.len() >= MIN_SATURATED_FILES
                && slots
                    .iter()
                    .all(|slot| rests_on_a_bare_code(data, &findings[*slot].1))
        }
        0 | 2 => false,
        _ => counts[0] as f32 >= counts[2] as f32 * MIN_DOMINANCE,
    }
}

/// Whether a directory on the path *is* this language, rather than the file
/// name merely carrying its token.
fn language_is_named_by_a_directory(occurrences: &[Occurrence], lang_tag: &str) -> bool {
    occurrences
        .iter()
        .any(|occ| !occ.is_filename && occ.canonical == lang_tag)
}

/// Whether the finding rests on nothing better than a two-letter code with an
/// asset word beside it — the weakest thing the engine acts on, and the only
/// evidence a saturated folder is allowed to be rejected for.
fn rests_on_a_bare_code(data: &LangData, finding: &LangFinding) -> bool {
    let token = match &finding.reason.evidence {
        LangEvidence::TokenWithMarker { token, .. } | LangEvidence::BareToken { token } => token,
        _ => return false,
    };
    matches!(data.lookup(token), Some((_, Level::C)))
}
