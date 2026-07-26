//! Corpus regression test for the localization-detection engine
//! (docs/04_implementation_plan.md §5.6).
//!
//! Reads `tests/corpus/corpus.tsv` (repo root — see
//! `tests/corpus/README.md` for provenance and format), groups rows by
//! `game_key`, runs `LangDetector::analyze_game` per group, and compares
//! against the `expected` label.
//!
//! ## 2026-07 calibration pass
//!
//! The corpus started out as a **draft heuristic**, not ground truth, with
//! ~3400 mislabeled rows found by cross-checking every disagreement against
//! the engine (see the session report for the full rule-by-rule
//! breakdown). Both sides were fixed as appropriate:
//!
//! - `tests/corpus/corpus.tsv` itself was corrected in bulk (~1030 rows)
//!   where manual review confirmed the draft label was wrong — e.g. the
//!   draft tool required no language family for bare iso2 codes
//!   (`pl_shell1.wav` mislabeled `audio`, "pl" = player not Polish; the
//!   VOX announcer's `no.wav` mislabeled `audio`, "no" the English word;
//!   `.srt`/`.vtt`/`.xml` subtitles mislabeled `video`; `.NET` satellite
//!   assemblies mislabeled `none` because the draft only checks sibling
//!   *files*, never sibling *subdirectories*, for a shared shape).
//! - The engine itself was extended in three ways, each gated by the same
//!   "never trust a bare code without corroborating structure" philosophy
//!   as the rest of the design (see `langdetect/family.rs`, `dict.rs`,
//!   `markers.rs` doc comments for the full rationale and the specific
//!   coincidental-collision cases manual review ruled out along the way):
//!   generic `<lang>-<REGION>` locale tags, a position-consistent (not
//!   just identical-shape) directory family heuristic, and separating
//!   "this is localized" marker words from words that actually describe
//!   the asset kind.
//!
//! ## 2026-07-26 manual review pass
//!
//! Every row where the corpus and the engine still disagreed — 61 the corpus
//! called `none` and the engine flagged, 2228 the corpus expected flagged and
//! the engine skipped — was dumped with its full on-disk path and reviewed by
//! hand, one row at a time. The verdict on all 2289:
//!
//! - All 61 flags were correct. Every one is a real localization file and the
//!   detected language was right; the corpus label was the wrong side. The
//!   false-positive rate is therefore now **0**, and the assertion below says
//!   exactly that rather than allowing a margin.
//! - Of the 2228 skips, **621 were not localization at all** — the draft
//!   label had latched onto a coincidental token: `cs_` for cutscene and
//!   Counter-Strike rather than Czech, `hiragana-female-chi.ogg` where "chi"
//!   is the kana ち, `POL.unity3d`, `por_caymangt4_16.zip`. The engine was
//!   right to stay silent, so those rows became `none`.
//! - The remaining **1607 are genuine misses** and stay in the corpus as the
//!   recall gap to work down. 267 of them are localized documentation
//!   (`lic_da-DK.html`, `EULA.RUS.rtf`, `SOTL_Readme_DE.TXT`) that the review
//!   also tagged with a rules-engine category; they are still localization
//!   files as far as this engine is concerned, so they still count as misses.
//! - 117 rows are localized *images*, a kind `LangKind` does not have. They
//!   are recorded as `unknown` (flag it, any kind accepted) until it does.
//!
//! Where the review supplied a kind it replaced the draft's, so the kind
//! column is now human-confirmed for every row that was ever in dispute.
//!
//! Some disagreement remains and is expected: the engine is intentionally
//! more conservative than exhaustive completeness would allow (see
//! `langdetect/mod.rs` module doc). Rather than asserting byte-for-byte
//! equality, this test:
//!
//! - Asserts a near-zero false-positive rate on `none` rows (flagging
//!   something that should never be touched is the dangerous direction) —
//!   both for plain occurrence-based flags and for family-confirmed ones.
//! - Asserts a floor on recall (of rows expecting a flag, how many the
//!   engine actually flags) and on kind accuracy (of flagged rows, how
//!   often Audio/Text/Video/Font is right) as coarse regression signals.
//! - Prints a full breakdown so a human can review drift.
//!
//! If the corpus file is missing, the test is skipped (not failed) so the
//! langdetect test suite still runs standalone before the corpus lands.

use std::collections::HashMap;
use std::path::PathBuf;

use gametrimmer_core::langdetect::{LangDetector, LangKind};
use gametrimmer_core::scanner::FileEntry;

fn corpus_path() -> PathBuf {
    // The corpus lives at the repo root (`tests/corpus/corpus.tsv`), not
    // under this crate — reach it via CARGO_MANIFEST_DIR so the test works
    // regardless of the process's current directory.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join("corpus.tsv")
}

struct Row {
    game_key: String,
    rel_path: String,
    expected: String,
}

fn parse_corpus(contents: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    for line in contents.lines() {
        if line.is_empty() || line.starts_with("game_key") {
            continue; // header or blank line
        }
        let mut parts = line.splitn(3, '\t');
        let (Some(game_key), Some(rel_path), Some(expected)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        rows.push(Row {
            game_key: game_key.to_string(),
            rel_path: rel_path.to_string(),
            expected: expected.trim().to_string(),
        });
    }
    rows
}

fn expected_kind(label: &str) -> Option<LangKind> {
    match label {
        "audio" => Some(LangKind::Audio),
        "text" => Some(LangKind::Text),
        "video" => Some(LangKind::Video),
        "font" => Some(LangKind::Font),
        "unknown" => None, // any kind is acceptable, as long as it's flagged
        _ => None,
    }
}

#[derive(Default, Debug)]
struct Stats {
    total: usize,
    matched: usize,
    false_positive: usize,        // expected "none", engine flagged it
    false_positive_family: usize, // ...specifically via the family heuristic
    false_negative: usize,        // expected a flag, engine did not flag
    kind_mismatch: usize,         // both flagged, kind disagrees (expected != "unknown")
    none_total: usize,
}

fn main_check() -> Stats {
    let path = corpus_path();
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "corpus test skipped: could not read {} ({e}); the corpus is prepared by a \
                 parallel task and may not exist yet",
                path.display()
            );
            return Stats::default();
        }
    };

    let rows = parse_corpus(&contents);
    if rows.is_empty() {
        eprintln!("corpus test skipped: {} has no data rows", path.display());
        return Stats::default();
    }

    // Preserve first-seen order of games; group rows by game_key.
    let mut order: Vec<String> = Vec::new();
    let mut by_game: HashMap<String, Vec<&Row>> = HashMap::new();
    for row in &rows {
        by_game.entry(row.game_key.clone()).or_insert_with(|| {
            order.push(row.game_key.clone());
            Vec::new()
        });
        by_game.get_mut(&row.game_key).unwrap().push(row);
    }

    let detector = LangDetector::new();
    let mut stats = Stats::default();

    for game_key in &order {
        let game_rows = &by_game[game_key];
        let files: Vec<FileEntry> = game_rows
            .iter()
            .map(|r| FileEntry::logical_only(r.rel_path.clone(), 0, None))
            .collect();

        let findings = detector.analyze_game(&files);
        let found: HashMap<usize, gametrimmer_core::langdetect::LangFinding> =
            findings.into_iter().collect();

        for (idx, row) in game_rows.iter().enumerate() {
            stats.total += 1;
            let is_flagged = found.contains_key(&idx);

            if row.expected == "none" {
                stats.none_total += 1;
                if is_flagged {
                    stats.false_positive += 1;
                    let f = &found[&idx];
                    if f.reason.contains("сім'я") {
                        stats.false_positive_family += 1;
                    } else if stats.false_positive - stats.false_positive_family <= 60 {
                        eprintln!(
                            "FP-nonfamily [{}] {} -> {} {:?} conf={} ({})",
                            game_key, row.rel_path, f.lang_tag, f.kind, f.confidence, f.reason
                        );
                    }
                } else {
                    stats.matched += 1;
                }
                continue;
            }

            if !is_flagged {
                stats.false_negative += 1;
                if stats.false_negative <= 80 {
                    eprintln!(
                        "FN [{}] {} (expected {})",
                        game_key, row.rel_path, row.expected
                    );
                }
                continue;
            }

            match expected_kind(&row.expected) {
                Some(want) if found[&idx].kind != want => stats.kind_mismatch += 1,
                _ => stats.matched += 1,
            }
        }
    }

    stats
}

#[test]
fn corpus_regression() {
    let stats = main_check();
    if stats.total == 0 {
        return; // corpus missing/empty — already logged, nothing to assert
    }

    let fp_rate = stats.false_positive as f64 / stats.none_total.max(1) as f64;
    let agreement_rate = stats.matched as f64 / stats.total as f64;

    let expect_flag_total = stats.total - stats.none_total;
    let flagged_total = expect_flag_total.saturating_sub(stats.false_negative);
    let recall = flagged_total as f64 / expect_flag_total.max(1) as f64;
    // Of rows the engine DID flag, how often was Audio/Text/Video/Font right
    // (rows with a draft `unknown` label accept any kind, so they're not
    // counted against kind accuracy either way — see `expected_kind`).
    let kind_checked_total = flagged_total;
    let kind_correct = kind_checked_total.saturating_sub(stats.kind_mismatch);
    let kind_accuracy = kind_correct as f64 / kind_checked_total.max(1) as f64;

    eprintln!(
        "corpus: {} rows, {} matched ({:.1}%), {} false-positive ({:.1}% of {} 'none' rows, of \
         which {} via confirmed language family), {} false-negative, {} kind-mismatch, \
         recall {:.1}%, kind accuracy {:.1}%",
        stats.total,
        stats.matched,
        agreement_rate * 100.0,
        stats.false_positive,
        fp_rate * 100.0,
        stats.none_total,
        stats.false_positive_family,
        stats.false_negative,
        stats.kind_mismatch,
        recall * 100.0,
        kind_accuracy * 100.0,
    );

    // Safety-critical direction: flagging a file that must never be touched.
    // Every `none` row the engine ever disagreed with has now been reviewed by
    // hand (see the 2026-07-26 pass in the module doc), so a false positive is
    // no longer "the draft label might be wrong" — it means the engine flagged
    // a file a human looked at and said is not localization. That deserves a
    // hard zero rather than a percentage bar with slack in it.
    //
    // If this fires, do NOT relax the bar: read the `FP-nonfamily` lines the
    // run prints, open the files, and either narrow the rule or — if review
    // shows the corpus label is the wrong side — correct that row and say so
    // in the commit.
    assert_eq!(
        stats.false_positive, 0,
        "engine flagged {} of {} 'none' rows ({} via the family heuristic); every one of these \
         was manually confirmed to not be localization",
        stats.false_positive, stats.none_total, stats.false_positive_family
    );

    // Recall: of rows expecting a flag, how many the engine actually flags.
    // 80.8% after the 2026-07-26 review; the jump from 75.0% came entirely
    // from deleting 621 labels manual review found were never localization,
    // not from touching the engine. The remaining 1607 misses are real.
    //
    // NOTE: 85% was the original target but is not achievable without
    // weakening a safety invariant (verified during the calibration pass —
    // see the session report). The remaining gap is concentrated in
    // patterns the engine deliberately refuses on safety grounds: bare
    // two-letter codes whose only supporting folder is a `<prefix>_<lang>`
    // suffix rather than an exact language name (`sds_de\speech\*.sds` —
    // the file itself carries no token, and "a file without a language
    // token is never a candidate even in a language folder" is an
    // intentional, tested invariant); locale identifiers embedded inside a
    // longer compound token (`actions_zh_tw.json`, `fonts_jpn_LOC_ITA.upk`)
    // that would need substring-level rather than whole-token matching to
    // catch, at real risk of the "up-to"/"read-me" false-positive class the
    // whole-token design exists to prevent; and Apple-style `<lang>.lproj`
    // folders, a real but narrow convention not yet covered. 79% leaves
    // headroom for corpus churn while still catching any real regression.
    assert!(
        recall >= 0.79,
        "recall too low: {:.1}% ({} / {} rows expecting a flag)",
        recall * 100.0,
        flagged_total,
        expect_flag_total
    );

    // Kind accuracy: of flagged rows with a specific expected kind, how
    // often Audio/Text/Video/Font is right. One documented residual
    // weakness keeps this under 100%: a same-segment "closest marker wins"
    // tie can pick a generic scene/DLC-id token (`fmv_####`) in a filename
    // over the file's own enclosing `audio\` folder when both is_filename
    // markers exist — see the `audio -> Video` cluster in the session
    // report. Left as-is rather than papered over by relabeling the corpus.
    assert!(
        kind_accuracy >= 0.98,
        "kind accuracy too low: {:.1}% ({} / {} flagged rows)",
        kind_accuracy * 100.0,
        kind_correct,
        kind_checked_total
    );

    // Coarse overall regression signal. 88.5% after the 2026-07-26 review.
    assert!(
        agreement_rate > 0.87,
        "overall agreement with corpus too low: {:.1}%",
        agreement_rate * 100.0
    );
}
