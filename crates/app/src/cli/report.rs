//! Text report for a headless run (GT-10): a stable, human-readable summary
//! written to `--report <path>` and echoed to the console. Pure formatting
//! over a precomputed [`ReportData`], so it is fully unit-testable without a
//! scan, a database, or Win32 - the entry point in [`super`] fills the struct
//! in and this module turns it into text.
//!
//! Category labels use the stable English keys (`model::category_ui_key`)
//! rather than the localized display names, so a report parsed by a script or
//! diffed across runs does not shift with the UI language.

use gametrimmer_core::settings::{DeleteMethod, SelectionProfile};

use super::args::Mode;
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
/// An `--apply` run completed but some files could not be removed.
pub const EXIT_PARTIAL: u8 = 3;

/// The reclaim outcome of an `--apply` run, mirrored from
/// [`crate::worker::delete::SpaceTally`] plus per-file success/failure counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyReport {
    pub method: DeleteMethod,
    /// Sum of on-disk sizes of everything that was queued for removal.
    pub expected: u64,
    /// Freed immediately (permanent deletes + over-quota recycles).
    pub freed: u64,
    /// Moved to the Recycle Bin - frees only once the bin is emptied.
    pub recycled_pending: u64,
    pub succeeded: usize,
    pub failed: usize,
}

/// Everything a headless run computed, ready to render. Precomputing this
/// (rather than passing the raw findings) keeps [`format_report`] a pure,
/// trivially-testable string builder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportData {
    pub mode: Mode,
    /// The profile that actually decided the selection (already resolved from
    /// settings when the CLI left it implicit).
    pub profile: SelectionProfile,
    pub elevated: bool,
    /// The scanner's own one-line summary (MFT vs walkdir counts, elapsed).
    pub scan_summary: String,
    /// Per-category rollup of *every* finding (see [`crate::model::plan_cards`]),
    /// least-risky first.
    pub cards: Vec<PlanCard>,
    pub total_findings: usize,
    /// How many findings the profile pre-selected for removal.
    pub selected_count: usize,
    /// On-disk total of the pre-selected findings.
    pub selected_size_on_disk: u64,
    /// Present only for [`Mode::Apply`], after the delete ran.
    pub apply: Option<ApplyReport>,
}

impl ReportData {
    /// The process exit code this outcome maps to: partial success when an
    /// apply left files behind, OK otherwise. (Usage and runtime errors are
    /// returned by the entry point before a `ReportData` is ever built.)
    pub fn exit_code(&self) -> u8 {
        match &self.apply {
            Some(apply) if apply.failed > 0 => EXIT_PARTIAL,
            _ => EXIT_OK,
        }
    }
}

/// Stable English label for a risk band (the report is not localized).
fn risk_label(risk: RiskLevel) -> &'static str {
    match risk {
        RiskLevel::None => "none",
        RiskLevel::Low => "low",
        RiskLevel::Medium => "medium",
    }
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::DryRun => "dry-run (no files deleted)",
        Mode::Apply => "apply (files deleted)",
    }
}

fn method_label(method: DeleteMethod) -> &'static str {
    match method {
        DeleteMethod::Permanent => "permanent delete",
        DeleteMethod::RecycleBin => "recycle bin",
    }
}

/// Renders a full text report. `lang` governs only the size-unit suffixes
/// (via [`format_size`]); every label is fixed English for stable, scriptable
/// output.
pub fn format_report(data: &ReportData, lang: Lang) -> String {
    let mut out = String::new();
    out.push_str("GameTrimmer headless report\n");
    out.push_str("===========================\n");
    out.push_str(&format!("mode:     {}\n", mode_label(data.mode)));
    out.push_str(&format!("profile:  {}\n", data.profile.as_str()));
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
    out.push('\n');

    out.push_str(&format!(
        "Selected by profile \"{}\": {} finding(s), {}\n",
        data.profile.as_str(),
        data.selected_count,
        format_size(lang, data.selected_size_on_disk),
    ));

    match &data.apply {
        None => {
            out.push_str(
                "\nDry run: nothing was deleted. Re-run with --apply --profile <name> to remove \
                 the selected findings.\n",
            );
        }
        Some(apply) => {
            out.push('\n');
            out.push_str("Apply result:\n");
            out.push_str(&format!(
                "  method:          {}\n",
                method_label(apply.method)
            ));
            out.push_str(&format!("  removed:         {} file(s)\n", apply.succeeded));
            if apply.failed > 0 {
                out.push_str(&format!("  failed:          {} file(s)\n", apply.failed));
            }
            out.push_str(&format!(
                "  freed now:       {} (of {} expected)\n",
                format_size(lang, apply.freed),
                format_size(lang, apply.expected),
            ));
            if apply.recycled_pending > 0 {
                out.push_str(&format!(
                    "  in recycle bin:  {} (frees when the bin is emptied)\n",
                    format_size(lang, apply.recycled_pending),
                ));
            }
        }
    }

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

    fn dry_run_data() -> ReportData {
        ReportData {
            mode: Mode::DryRun,
            profile: SelectionProfile::Balanced,
            elevated: true,
            scan_summary: "12 via MFT, 3 via walkdir, 4.2s".to_string(),
            cards: vec![
                card(DisplayCategory::Orphan, 41 * 1024 * 1024 * 1024, 5, 1),
                card(DisplayCategory::Loc, 87 * 1024 * 1024, 230, 23),
            ],
            total_findings: 235,
            selected_count: 235,
            selected_size_on_disk: 41 * 1024 * 1024 * 1024 + 87 * 1024 * 1024,
            apply: None,
        }
    }

    #[test]
    fn dry_run_report_names_mode_profile_and_selection() {
        let text = format_report(&dry_run_data(), Lang::En);
        assert!(text.contains("mode:     dry-run"), "{text}");
        assert!(text.contains("profile:  balanced"), "{text}");
        assert!(text.contains("orphan"), "{text}");
        assert!(text.contains("risk=none"), "{text}");
        assert!(
            text.contains("Selected by profile \"balanced\": 235 finding(s)"),
            "{text}"
        );
        // A dry run must make it explicit that nothing was removed.
        assert!(text.contains("nothing was deleted"), "{text}");
        assert!(!text.contains("Apply result"), "{text}");
    }

    #[test]
    fn dry_run_exit_code_is_ok() {
        assert_eq!(dry_run_data().exit_code(), EXIT_OK);
    }

    #[test]
    fn apply_report_shows_freed_versus_expected() {
        let mut data = dry_run_data();
        data.mode = Mode::Apply;
        data.apply = Some(ApplyReport {
            method: DeleteMethod::Permanent,
            expected: 10 * 1024 * 1024,
            freed: 10 * 1024 * 1024,
            recycled_pending: 0,
            succeeded: 235,
            failed: 0,
        });
        let text = format_report(&data, Lang::En);
        assert!(text.contains("Apply result"), "{text}");
        assert!(text.contains("method:          permanent delete"), "{text}");
        assert!(text.contains("removed:         235 file(s)"), "{text}");
        assert!(text.contains("freed now:"), "{text}");
        assert!(
            !text.contains("failed:"),
            "no failures line when none failed: {text}"
        );
        assert_eq!(data.exit_code(), EXIT_OK);
    }

    #[test]
    fn apply_with_failures_reports_partial_and_exit_code() {
        let mut data = dry_run_data();
        data.mode = Mode::Apply;
        data.apply = Some(ApplyReport {
            method: DeleteMethod::Permanent,
            expected: 10 * 1024 * 1024,
            freed: 6 * 1024 * 1024,
            recycled_pending: 0,
            succeeded: 200,
            failed: 35,
        });
        let text = format_report(&data, Lang::En);
        assert!(text.contains("failed:          35 file(s)"), "{text}");
        assert_eq!(data.exit_code(), EXIT_PARTIAL);
    }

    #[test]
    fn recycle_apply_reports_pending_space() {
        let mut data = dry_run_data();
        data.mode = Mode::Apply;
        data.apply = Some(ApplyReport {
            method: DeleteMethod::RecycleBin,
            expected: 8 * 1024 * 1024,
            freed: 0,
            recycled_pending: 8 * 1024 * 1024,
            succeeded: 235,
            failed: 0,
        });
        let text = format_report(&data, Lang::En);
        assert!(text.contains("method:          recycle bin"), "{text}");
        assert!(text.contains("in recycle bin:"), "{text}");
    }

    #[test]
    fn empty_findings_report_says_none() {
        let mut data = dry_run_data();
        data.cards.clear();
        data.total_findings = 0;
        data.selected_count = 0;
        data.selected_size_on_disk = 0;
        let text = format_report(&data, Lang::En);
        assert!(text.contains("(none)"), "{text}");
    }
}
