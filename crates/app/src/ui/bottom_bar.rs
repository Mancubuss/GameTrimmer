//! Bottom panel: persistent selection summary, bulk-selection actions and
//! the delete action.

use eframe::egui;
use gametrimmer_core::settings::SelectionProfile;

use crate::model::{self, format_duration, format_size, AUTO_SELECT_CONFIDENCE_THRESHOLD};

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

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let lang = app.lang();
    let s = i18n::strings(lang);
    egui::Panel::bottom("bottom_panel").show(ui, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let selected_items: Vec<&crate::model::FindingItem> = app
                .findings
                .iter()
                .filter(|item| item.selected && !item.removed)
                .collect();
            let selected_count = selected_items.len();
            // On-disk allocation (GT-05a): the honest "space that will be
            // freed" figure, matching the per-row and per-node totals and the
            // occupancy denominator (both on-disk).
            let selected_bytes: u64 = selected_items
                .iter()
                .map(|item| item.row.size_on_disk)
                .sum();

            ui.label(i18n::selected_summary(
                lang,
                selected_count,
                &format_size(lang, selected_bytes),
            ));
            ui.label("\u{2139}")
                .on_hover_text(i18n::selection_hint(lang, AUTO_SELECT_CONFIDENCE_THRESHOLD));

            ui.add_space(12.0);

            let pct = model::freed_percent(selected_bytes, app.occupancy.total);
            ui.label(i18n::occupancy_summary(
                lang,
                &format_size(lang, app.occupancy.total),
                pct,
            ));

            ui.add_space(12.0);

            // Selection profile (GT-04): a one-click policy for which findings
            // arrive pre-checked. Switching re-checks the current findings in
            // place (no re-scan) via `app.set_selection_profile`.
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
            if picked_profile != app.settings.selection_profile {
                app.set_selection_profile(picked_profile);
            }

            ui.add_space(12.0);

            let has_findings = app.findings.iter().any(|item| !item.removed);
            if ui
                .add_enabled(
                    has_findings && !app.busy,
                    egui::Button::new(s.btn_select_all),
                )
                .clicked()
            {
                app.select_all();
            }
            if ui
                .add_enabled(
                    selected_count > 0 && !app.busy,
                    egui::Button::new(s.btn_deselect_all),
                )
                .clicked()
            {
                app.deselect_all();
            }

            ui.add_space(12.0);

            let delete_clicked = ui
                .add_enabled(
                    selected_count > 0 && !app.busy,
                    egui::Button::new(s.btn_delete_selected),
                )
                .clicked();
            if delete_clicked {
                app.request_delete_confirmation();
            }
        });

        // Persistent last-scan timing readout (see `crate::model::ScanTiming`)
        // on its OWN row below the action cluster, right-aligned. Kept on a
        // separate line rather than sharing the buttons' row: on a narrow
        // window the two would otherwise overlap (the right-to-left label
        // creeping over the buttons). The right-aligning `right_to_left`
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
