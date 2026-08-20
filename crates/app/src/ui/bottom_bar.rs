//! Bottom panel: persistent selection summary, bulk-selection actions and
//! the delete action.

use eframe::egui;
use gametrimmer_core::settings::SelectionProfile;

use crate::model::{self, format_duration, format_size};

use crate::app::GameTrimmerApp;
use crate::i18n;

/// The selection profiles offered in the picker, in increasing aggressiveness,
/// with `Custom` (the bare confidence threshold) last.
const PROFILE_ORDER: [SelectionProfile; 4] = [
    SelectionProfile::Cautious,
    SelectionProfile::Balanced,
    SelectionProfile::Aggressive,
    SelectionProfile::Custom,
];

/// Localized label for one profile.
fn profile_label(s: &i18n::Strings, profile: SelectionProfile) -> &'static str {
    match profile {
        SelectionProfile::Cautious => s.profile_cautious,
        SelectionProfile::Balanced => s.profile_balanced,
        SelectionProfile::Aggressive => s.profile_aggressive,
        SelectionProfile::Custom => s.profile_custom,
    }
}

/// Share of the row's width the selection summary may claim before it
/// truncates. The delete button is laid out right-to-left after it, so this
/// is what guarantees the primary action keeps its full label at any window
/// width instead of being pushed off the edge.
const SUMMARY_WIDTH_SHARE: f32 = 0.55;

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let lang = app.lang();
    let s = i18n::strings(lang);

    let aggregates = crate::ui::frame_ui_aggregates(ui.ctx(), &app.findings);
    let has_findings = aggregates.totals.finding_count > 0;
    // Before the first scan there is nothing to select and nothing to delete,
    // so the panel showed zeroed totals and a row of dead buttons under an
    // empty screen. The scan-timing readout is the one thing
    // worth keeping without findings - a scan that found nothing still took
    // time, and saying so is the difference between "done, nothing here" and
    // "did anything happen?".
    if !has_findings && app.last_scan_timing.is_none() {
        return;
    }

    egui::Panel::bottom("bottom_panel").show(ui, |ui| {
        ui.add_space(4.0);

        if has_findings {
            let selected_count = aggregates.selected_count;
            // On-disk allocation (allocated-size accounting): the honest "space that will be
            // freed" figure, matching the per-row and per-node totals and the
            // occupancy denominator (both on-disk).
            let selected_bytes = aggregates.selected_bytes_on_disk;

            let busy = app.busy.then_some(s.disabled_busy);
            let needs_selection = busy
                .or_else(|| (selected_count == 0).then_some(s.disabled_no_selection))
                // Only reachable with findings already on screen, which after
                // this release means a database from before the disclaimer -
                // see `ui::onboarding`. Cheap to state, and the alternative
                // is a Delete button that quietly does nothing.
                .or_else(|| app.blocked_by_disclaimer());
            let mut delete_clicked = false;
            let mut select_all_clicked = false;
            let mut deselect_all_clicked = false;

            // Row 1 - what is selected, and the one destructive action, alone
            // at the right edge. Everything else moved to row 2: the whole
            // cluster used to share this line, and the delete button was
            // already cut off by ~25pt at the standard 900x600 window before
            // a single finding was listed.
            ui.horizontal(|ui| {
                let summary_width = (ui.available_width() * SUMMARY_WIDTH_SHARE).max(80.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(summary_width, ui.spacing().interact_size.y),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.add(
                            egui::Label::new(i18n::selected_summary(
                                lang,
                                selected_count,
                                &format_size(lang, selected_bytes),
                            ))
                            .truncate(),
                        );
                        ui.label("\u{2139}")
                            .on_hover_text(i18n::selection_hint(lang));
                    },
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    delete_clicked =
                        crate::ui::gated_button(ui, s.btn_delete_selected, needs_selection)
                            .clicked();
                });
            });

            // Row 2 - context and bulk selection. None of it competes with
            // the delete button for width any more.
            ui.horizontal(|ui| {
                let pct = model::freed_percent(selected_bytes, app.occupancy.total);
                ui.label(i18n::occupancy_summary(
                    lang,
                    &format_size(lang, app.occupancy.total),
                    pct,
                ));

                ui.add_space(12.0);

                // Selection profile (selection profiles): a one-click policy for which
                // findings arrive pre-checked. Switching re-checks the current
                // findings in place (no re-scan).
                ui.label(s.profile_label).on_hover_text(s.profile_hint);
                let mut picked_profile = app.settings.selection_profile;
                egui::ComboBox::from_id_salt("selection_profile")
                    .selected_text(profile_label(s, picked_profile))
                    .show_ui(ui, |ui| {
                        for profile in PROFILE_ORDER {
                            ui.selectable_value(
                                &mut picked_profile,
                                profile,
                                profile_label(s, profile),
                            )
                            .on_hover_text(s.profile_hint);
                        }
                    });

                ui.add_space(12.0);

                select_all_clicked = crate::ui::gated_button(ui, s.btn_select_all, busy).clicked();
                deselect_all_clicked =
                    crate::ui::gated_button(ui, s.btn_deselect_all, needs_selection).clicked();

                if picked_profile != app.settings.selection_profile {
                    app.set_selection_profile(picked_profile);
                }
            });

            if delete_clicked {
                app.request_delete_confirmation();
            }
            if select_all_clicked {
                app.select_all();
            }
            if deselect_all_clicked {
                app.deselect_all();
            }
        }

        // Persistent last-scan timing readout (see `crate::model::ScanTiming`)
        // on its OWN row, right-aligned. The right-aligning `right_to_left`
        // layout MUST be wrapped in a `ui.horizontal` so the row's height is
        // its one line of content - a bare `with_layout` here instead takes
        // the panel's full available height and vertical-centers the label,
        // ballooning the bottom panel upward over the findings tree. Stays
        // visible until the next scan starts (unlike the transient top-bar
        // status line, whose total this deliberately duplicates - see
        // `worker::scan_route::format_scan_summary`).
        if let Some(timing) = app.last_scan_timing {
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(i18n::scan_timing_summary(
                        lang,
                        &format_duration(timing.scan),
                        &format_duration(timing.analyze),
                        &format_duration(timing.total),
                    ));
                });
            });
        }
        ui.add_space(4.0);
    });
}

#[cfg(test)]
mod tests {
    use super::show;

    use crate::ui::harness::{UiTest, NARROW_VIEWPORT, STANDARD_VIEWPORT};

    /// Reported from a real run: the bar read "Scan 32s · Analysis 1:03 ·
    /// Total 1:04", which is three figures separated by dots that plainly do
    /// not add up. They are not supposed to - the file table is read
    /// underneath the classification, so the first span is contained in the
    /// second - but nothing on screen said so, and the only thing anyone can
    /// do with three dot-separated durations is try to add them.
    ///
    /// Pins the containment wording rather than the exact sentence, so it
    /// survives rephrasing but not a silent return to a flat list.
    #[test]
    fn the_timing_readout_says_the_two_spans_overlap() {
        let mut test = UiTest::with_size(show, STANDARD_VIEWPORT);
        test.seed_findings();
        test.app_mut().last_scan_timing = Some(crate::model::ScanTiming {
            scan: std::time::Duration::from_secs(32),
            analyze: std::time::Duration::from_secs(63),
            total: std::time::Duration::from_secs(64),
        });
        test.run();

        // The joining word is what stops the three figures reading as a sum;
        // `strings()` is not used because this phrasing lives in
        // `i18n::scan_timing_summary`, not in the string table.
        let joiner = match test.app_mut().lang() {
            crate::i18n::Lang::Uk => "у межах аналізу",
            _ => "within analysis",
        };
        test.assert_label_containing(joiner);
    }

    /// The bug this panel was rebuilt for. Measured before the
    /// fix at the standard 900x600 window: the delete button ran to x=924.8
    /// with no findings at all, and to x=954.0 with results - 25pt and 54pt
    /// past the right edge. The primary, destructive action was clipped.
    ///
    /// Both window widths and both content states, because the empty case was
    /// already broken and the narrow case is where it gets worse.
    #[test]
    fn the_delete_button_stays_inside_the_window() {
        for (name, size) in [("standard", STANDARD_VIEWPORT), ("narrow", NARROW_VIEWPORT)] {
            for seeded in [false, true] {
                let mut test = UiTest::with_size(show, size);
                if seeded {
                    test.seed_findings();
                } else {
                    // Nothing to delete: the panel is not drawn at all, which
                    // is the other half of this fix (§5.3).
                    test.run();
                    test.assert_no_label(test.strings().btn_delete_selected);
                    continue;
                }

                let rect = test.rect_of(test.strings().btn_delete_selected);
                assert!(
                    rect.max.x <= size.x,
                    "{name} window ({}pt): delete button reaches x={} and is clipped",
                    size.x,
                    rect.max.x,
                );
                assert!(
                    rect.min.x >= 0.0 && rect.max.y <= size.y,
                    "{name} window: delete button {rect:?} escapes the viewport",
                );
            }
        }
    }

    /// §5.3: before any scan the panel offered zeroed totals and a row of
    /// dead buttons under an empty screen.
    #[test]
    fn the_panel_is_absent_until_there_is_something_to_act_on() {
        let mut test = UiTest::new(show);
        test.run();
        let s = test.strings();

        test.assert_no_label(s.btn_delete_selected);
        test.assert_no_label(s.btn_select_all);
        test.assert_no_label(s.profile_label);

        test.seed_findings();

        test.assert_label(s.btn_delete_selected);
        test.assert_label(s.btn_select_all);
        test.assert_label(s.profile_label);
    }

    /// A greyed-out button with no explanation reads as broken. Every gated
    /// action in this panel must name its blocker on hover.
    ///
    /// Asserted through the tooltip actually appearing after a hover, not by
    /// inspecting the call site - `on_disabled_hover_text` silently does
    /// nothing if the response it is attached to was never disabled.
    #[test]
    fn every_disabled_action_explains_itself_on_hover() {
        let mut test = UiTest::new(show);
        test.seed_findings();
        for item in &mut test.app_mut().findings {
            item.selected = false;
        }
        test.run();
        let s = test.strings();

        // Findings exist but none are checked, so the two selection-driven
        // actions are the ones that must explain themselves.
        for button in [s.btn_deselect_all, s.btn_delete_selected] {
            test.hover(button);
            assert!(
                test.has_label(s.disabled_no_selection),
                "hovering the disabled {button:?} did not say why",
            );
        }
    }

    /// A running job outranks the per-action preconditions: it is the thing
    /// the user has to wait out, so it is what the hint should say.
    #[test]
    fn a_running_job_is_the_reason_reported_while_it_lasts() {
        let mut test = UiTest::new(show);
        test.seed_findings();
        test.app_mut().begin_job(false);
        test.run();
        let s = test.strings();

        test.hover(s.btn_delete_selected);

        assert!(
            test.has_label(s.disabled_busy),
            "a busy app should blame the running job, not the selection",
        );
    }

    /// The control: with findings selected and nothing running, the actions
    /// are enabled and no explanation is shown. Without this the assertions
    /// above would also pass if every button were permanently disabled.
    #[test]
    fn no_explanation_is_shown_once_the_action_is_available() {
        let mut test = UiTest::new(show);
        test.seed_findings();
        let s = test.strings();

        test.hover(s.btn_delete_selected);

        assert!(!test.has_label(s.disabled_no_selection));
        assert!(!test.has_label(s.disabled_busy));
    }
}
