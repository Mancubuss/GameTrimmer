//! The one-line plan summary (GT-12), rendered inline at the top of the
//! findings-tree region (see `ui::tree_view::show`) - directly above the tree,
//! not as a separate panel.
//!
//! This replaces the six-card strip of GT-03. The cards carried one piece of
//! real value - drill down into a single display category - and that fits in a
//! row: "Found X in N games · Show: [all categories]". They also cost the only
//! known UI bug in the app: laid out with `horizontal_wrapped`, a card that
//! *almost* fit the remaining width was squeezed rather than wrapped, down to a
//! single character wide and a thousand pixels tall, which pushed the findings
//! tree off screen entirely at ordinary window widths.
//!
//! `model::plan_cards` is deliberately still here and still used: it feeds the
//! per-category roll-up of the CLI report (`cli::report`), and it supplies this
//! row's category list. Only the strip is gone, not the aggregation.

use eframe::egui;

use crate::app::GameTrimmerApp;
use crate::i18n;
use crate::model::{self, format_size, DisplayCategory};

/// Share of the row's width the summary text may claim before it starts
/// truncating. The point is that the selector can never be squeezed off the
/// row no matter how narrow the window gets - the bug this screen is being
/// rebuilt to avoid.
const SUMMARY_WIDTH_SHARE: f32 = 0.5;

/// Renders the plan summary row directly into `ui` (the caller owns the
/// enclosing panel). A no-op when there are no findings, so nothing is drawn -
/// and no separator - on the empty startup screen.
pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let cards = model::plan_cards(&app.findings);
    if cards.is_empty() {
        return;
    }

    let lang = app.lang();
    let s = i18n::strings(lang);
    let active_filter = app.tree_category_filter;
    let totals = model::plan_totals(&app.findings);

    // Deferred actions: collected while `app` is borrowed read-only for
    // rendering, applied only after the widgets are laid out.
    let mut new_filter: Option<Option<DisplayCategory>> = None;
    let mut remove_category: Option<DisplayCategory> = None;

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let summary = i18n::plan_totals_summary(lang, totals.finding_count, totals.game_count);
        let summary_width = (ui.available_width() * SUMMARY_WIDTH_SHARE).max(80.0);
        ui.allocate_ui_with_layout(
            egui::vec2(summary_width, ui.spacing().interact_size.y),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add(egui::Label::new(summary).truncate());
            },
        );

        ui.add_space(8.0);
        ui.label(s.plan_filter_label);

        let mut picked = active_filter;
        egui::ComboBox::from_id_salt("plan_category_filter")
            .selected_text(match active_filter {
                Some(category) => model::category_display(lang, category),
                None => s.plan_filter_all,
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut picked, None, s.plan_filter_all);
                for card in &cards {
                    // Each entry carries the category's reclaimable size: it is
                    // the number that made the cards worth reading, and it fits
                    // here without costing the tree any height.
                    ui.selectable_value(
                        &mut picked,
                        Some(card.category),
                        format!(
                            "{} - {}",
                            model::category_display(lang, card.category),
                            format_size(lang, card.total_size_on_disk)
                        ),
                    );
                }
            });
        if picked != active_filter {
            new_filter = Some(picked);
        }

        // Whole-category removal stays one click away, but only while a
        // category is actually selected - so the control can never be read as
        // "delete everything", and the risk band of what is about to go is
        // stated right next to the button that would do it.
        if let Some(category) = active_filter {
            ui.add_space(8.0);
            if ui
                .add_enabled(!app.busy, egui::Button::new(s.btn_remove_category))
                .clicked()
            {
                remove_category = Some(category);
            }
            ui.small(i18n::plan_risk_label(lang, model::category_risk(category)));
        }
    });
    ui.add_space(4.0);
    // Divider between the summary row and the tree that follows in the same
    // region, so the two read as "summary above, detail below".
    ui.separator();

    if let Some(filter) = new_filter {
        app.set_category_filter(filter);
    }
    if let Some(category) = remove_category {
        app.request_delete_for_category(category);
    }
}
