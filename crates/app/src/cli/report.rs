//! Text report for a headless run (headless CLI mode): a stable, human-readable summary
//! written to `--report <path>` and echoed to the console. Pure formatting
//! over a precomputed [`ReportData`], so it is fully unit-testable without a
//! scan, a database, or Win32 - the entry point in [`super`] fills the struct
//! in and this module turns it into text.
//!
//! Category labels use the stable English keys (`model::category_ui_key`)
//! rather than the localized display names, so a report parsed by a script or
//! diffed across runs does not shift with the UI language.
//!
//! GT-89 removed selection profiles and `--apply` from headless mode: a fresh
//! scan pre-selects nothing, and headless mode has no human to tick anything,
//! so it only ever reports what a scan found. There is no "selected"
//! sub-total and no apply result any more - a report that still printed
//! "0 selected" on every run would be exactly the silence this project treats
//! as a bug (a real count vs. a permanently-zero one look identical, and only
//! one of them means "working as designed").

use crate::i18n::Lang;
use crate::model::{category_ui_key, format_size, PlanCard, RiskLevel};

/// Process exit codes for the headless mode. Distinct enough that a scheduler
/// (Task Scheduler, CI) can branch on them.
pub const EXIT_OK: u8 = 0;
/// Bad arguments (see [`super::args::parse_invocation`]).
pub const EXIT_USAGE: u8 = 1;
/// A runtime failure: no libraries found, a database/scan error, the report
/// could not be written.
pub const EXIT_RUNTIME: u8 = 2;

/// Everything a headless run computed, ready to render. Precomputing this
/// (rather than passing the raw findings) keeps [`format_report`] a pure,
/// trivially-testable string builder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportData {
    pub elevated: bool,
    /// The scanner's own one-line summary (MFT vs walkdir counts, elapsed).
    pub scan_summary: String,
    /// Per-category rollup of *every* finding (see [`crate::model::plan_cards`]),
    /// least-risky first.
    pub cards: Vec<PlanCard>,
    pub total_findings: usize,
}

/// Stable English label for a risk band (the report is not localized).
fn risk_label(risk: RiskLevel) -> &'static str {
    match risk {
        RiskLevel::None => "none",
        RiskLevel::Low => "low",
        RiskLevel::Medium => "medium",
    }
}

/// Renders a full text report. `lang` governs only the size-unit suffixes
/// (via [`format_size`]); every label is fixed English for stable, scriptable
/// output.
pub fn format_report(data: &ReportData, lang: Lang) -> String {
    let mut out = String::new();
    out.push_str("GameTrimmer headless report\n");
    out.push_str("===========================\n");
    out.push_str(&format!("elevated: {}\n", data.elevated));
    out.push_str(&format!("scan:     {}\n", data.scan_summary));
    out.push('\n');

    out.push_str(&format!(
        "Findings by category ({} total):\n",
        data.total_findings
    ));
    if data.cards.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for card in &data.cards {
            out.push_str(&format!(
                "  {:<8} {:>10}  risk={:<6} {} finding(s) in {} game(s)\n",
                category_ui_key(card.category),
                format_size(lang, card.total_size_on_disk),
                risk_label(card.risk),
                card.finding_count,
                card.game_count,
            ));
        }
    }

    out.push_str(
        "\nNothing was deleted or selected: headless mode only reports what a scan found. \
         Open the graphical app to review and tick individual findings for removal.\n",
    );

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DisplayCategory;

    fn card(category: DisplayCategory, size: u64, count: usize, games: usize) -> PlanCard {
        PlanCard {
            category,
            total_size_on_disk: size,
            finding_count: count,
            game_count: games,
            risk: crate::model::category_risk(category),
        }
    }

    fn report_data() -> ReportData {
        ReportData {
            elevated: true,
            scan_summary: "12 via MFT, 3 via walkdir, 4.2s".to_string(),
            cards: vec![
                card(DisplayCategory::Orphan, 41 * 1024 * 1024 * 1024, 5, 1),
                card(DisplayCategory::Loc, 87 * 1024 * 1024, 230, 23),
            ],
            total_findings: 235,
        }
    }

    #[test]
    fn report_names_scan_summary_and_categories() {
        let text = format_report(&report_data(), Lang::En);
        assert!(text.contains("elevated: true"), "{text}");
        assert!(text.contains("orphan"), "{text}");
        assert!(text.contains("risk=none"), "{text}");
        assert!(text.contains("235 total"), "{text}");
    }

    /// The report must never claim anything was deleted or selected: headless
    /// mode has no `--apply` any more (GT-89), so a script grepping this
    /// output for a deletion outcome should find none.
    #[test]
    fn report_states_nothing_was_deleted_or_selected() {
        let text = format_report(&report_data(), Lang::En);
        assert!(text.contains("Nothing was deleted or selected"), "{text}");
        assert!(!text.contains("Apply result"), "{text}");
        assert!(!text.contains("Selected by profile"), "{text}");
    }

    #[test]
    fn empty_findings_report_says_none() {
        let mut data = report_data();
        data.cards.clear();
        data.total_findings = 0;
        let text = format_report(&data, Lang::En);
        assert!(text.contains("(none)"), "{text}");
        assert!(text.contains("0 total"), "{text}");
    }
}
