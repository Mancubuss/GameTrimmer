//! "Plan of action" card strip (GT-03), rendered inline at the top of the
//! findings-tree region (see `ui::tree_view::show`) - directly above the tree,
//! not as a separate panel. Each card rolls one display category up across
//! every disk and game (see `model::plan_cards`) into a single action: how much
//! it reclaims, how risky it is, and two buttons - "Переглянути" filters the
//! tree below down to that category, "Прибрати" opens the delete confirmation
//! for the whole category. The tree is always visible beneath, so the plan is a
//! lens over the same findings, never a screen that hides them.

use eframe::egui;

use crate::app::GameTrimmerApp;
use crate::i18n;
use crate::model::{self, format_size, DisplayCategory};

/// Renders the plan-card strip directly into `ui` (the caller owns the
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

    // Deferred actions: collected while `app` is borrowed read-only for
    // rendering, applied only after the widgets are laid out.
    let mut new_filter: Option<Option<DisplayCategory>> = None;
    let mut remove_category: Option<DisplayCategory> = None;

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.strong(s.plan_heading);
        // A one-click escape from a card filter back to the full tree. Shown
        // only while a filter is active, right beside the heading so there is a
        // single, obvious "back to everything" control.
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
                        // "Переглянути" is a toggle that filters the tree below
                        // to this category; the active card stays visibly
                        // pressed (`selected`) so it is obvious which filter is
                        // on. Clicking it again - or the heading's "Показати
                        // все" - clears the filter. The label no longer flips to
                        // "Показати все", which used to read as a second,
                        // duplicate button.
                        let is_active = active_filter == Some(card.category);
                        if ui
                            .add(egui::Button::new(s.btn_card_view).selected(is_active))
                            .clicked()
                        {
                            new_filter = Some(if is_active { None } else { Some(card.category) });
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
    // Divider between the plan cards and the tree that follows in the same
    // region, so the two read as "summary above, detail below".
    ui.separator();

    if let Some(filter) = new_filter {
        app.set_category_filter(filter);
    }
    if let Some(category) = remove_category {
        app.request_delete_for_category(category);
    }
}
