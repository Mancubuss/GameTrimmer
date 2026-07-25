//! "Plan of action" card strip (GT-03), shown between the top bar and the
//! findings tree. Each card rolls one display category up across every disk and
//! game (see `model::plan_cards`) into a single action: how much it reclaims,
//! how risky it is, and two buttons - "Переглянути" filters the tree down to
//! that category, "Прибрати" opens the delete confirmation for the whole
//! category. The tree stays visible below as the drill-down level, so the plan
//! is a lens over the same findings, not a separate screen.

use eframe::egui;

use crate::app::GameTrimmerApp;
use crate::i18n;
use crate::model::{self, format_size, DisplayCategory};

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    // Owned snapshot, so the render closure below can read `app` immutably
    // (e.g. `app.busy`) while we collect the user's clicks into locals and
    // apply them to `app` only after the panel closes.
    let cards = model::plan_cards(&app.findings);
    if cards.is_empty() {
        return;
    }

    let lang = app.lang();
    let s = i18n::strings(lang);
    let active_filter = app.tree_category_filter;

    // Deferred actions: applied after the panel closure to keep the borrow of
    // `app` inside it read-only.
    let mut new_filter: Option<Option<DisplayCategory>> = None;
    let mut remove_category: Option<DisplayCategory> = None;

    egui::Panel::top("plan_panel").show(ui, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.strong(s.plan_heading);
            // A one-click escape from a card filter back to the full tree.
            if active_filter.is_some() && ui.button(s.plan_show_all).clicked() {
                new_filter = Some(None);
            }
        });

        ui.horizontal_wrapped(|ui| {
            for card in &cards {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.strong(model::category_display(lang, card.category));
                        ui.label(format_size(lang, card.total_size_on_disk));
                        ui.label(i18n::plan_card_summary(
                            lang,
                            card.finding_count,
                            card.game_count,
                            card.category == DisplayCategory::Orphan,
                        ));
                        ui.label(i18n::plan_risk_label(lang, card.risk));

                        ui.horizontal(|ui| {
                            // The view button doubles as the toggle: if this
                            // card's filter is already active, it clears it.
                            let is_active = active_filter == Some(card.category);
                            let view_label = if is_active {
                                s.plan_show_all
                            } else {
                                s.btn_card_view
                            };
                            if ui.button(view_label).clicked() {
                                new_filter =
                                    Some(if is_active { None } else { Some(card.category) });
                            }
                            if ui
                                .add_enabled(!app.busy, egui::Button::new(s.btn_card_remove))
                                .clicked()
                            {
                                remove_category = Some(card.category);
                            }
                        });
                    });
                });
            }
        });
        ui.add_space(4.0);
    });

    if let Some(filter) = new_filter {
        app.set_category_filter(filter);
    }
    if let Some(category) = remove_category {
        app.request_delete_for_category(category);
    }
}
