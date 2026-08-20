//! The one-line plan summary (plan summary), rendered inline at the top of the
//! findings-tree region (see `ui::tree_view::show`) - directly above the tree,
//! not as a separate panel.
//!
//! This replaces the six-card strip of plan-action filtering. The cards carried one piece of
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
use crate::model::{self, format_size, DisplayCategory, GroupAxis, GROUP_AXIS_ORDER};

/// Share of the row's width the summary text may claim before it starts
/// truncating. The point is that the selector can never be squeezed off the
/// row no matter how narrow the window gets - the bug this screen is being
/// rebuilt to avoid.
const SUMMARY_WIDTH_SHARE: f32 = 0.5;

/// Width the name search field asks for. Capped against the space actually
/// left on the row, so on a narrow window it shrinks instead of pushing the
/// category controls off the edge.
const SEARCH_WIDTH_PX: f32 = 220.0;

/// Glyph on the button that empties the search field. A multiplication sign
/// rather than the letter "x": it is the character this control is drawn with
/// everywhere, and it does not read as text someone typed.
pub(crate) const CLEAR_SEARCH_GLYPH: &str = "\u{d7}";

/// Renders the plan summary row directly into `ui` (the caller owns the
/// enclosing panel). A no-op when there are no findings, so nothing is drawn -
/// and no separator - on the empty startup screen.
pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let aggregates = crate::ui::frame_ui_aggregates(ui.ctx(), &app.findings);
    let cards = aggregates.cards;
    if cards.is_empty() {
        return;
    }

    let lang = app.lang();
    let s = i18n::strings(lang);
    let active_filter = app.tree_category_filter;
    let totals = aggregates.totals;

    // Deferred actions: collected while `app` is borrowed read-only for
    // rendering, applied only after the widgets are laid out.
    let mut new_filter: Option<Option<DisplayCategory>> = None;
    let mut new_axis: Option<GroupAxis> = None;
    let mut remove_category: Option<DisplayCategory> = None;
    let mut new_search: Option<String> = None;
    let active_axis = app.tree_axis;

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

        // The grouping axis (GT-35) sits beside the category filter rather
        // than in a panel of its own: both answer "which slice of the same
        // findings am I looking at", and the tree has no height to spare.
        // Neither rescans - switching either one re-cuts rows already in
        // memory.
        ui.add_space(8.0);
        ui.label(s.plan_group_label);

        let mut axis = active_axis;
        egui::ComboBox::from_id_salt("plan_group_axis")
            .selected_text(i18n::group_axis_label(lang, active_axis))
            .show_ui(ui, |ui| {
                for candidate in GROUP_AXIS_ORDER {
                    ui.selectable_value(
                        &mut axis,
                        candidate,
                        i18n::group_axis_label(lang, candidate),
                    );
                }
            });
        if axis != active_axis {
            new_axis = Some(axis);
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

        // Name search (name search) rides at the row's right edge rather than after
        // the category controls: laid out right-to-left it keeps its own width
        // as the window narrows, and the summary label to its left - which
        // already truncates - is what gives way. Searching by name and
        // filtering by category are separate axes, so both live in this one
        // row instead of costing the tree another line of height.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut query = app.tree_search.clone();
            // Laid out right-to-left, so the clear button is added first to end
            // up at the far right, past the field. Present only while there is
            // something to clear - an always-visible "x" beside an empty field
            // is one more thing to read for no gain (MT-F05).
            if !query.is_empty()
                && ui
                    .small_button(CLEAR_SEARCH_GLYPH)
                    .on_hover_text(s.btn_clear_search)
                    .clicked()
            {
                new_search = Some(String::new());
            }
            let width = ui.available_width().min(SEARCH_WIDTH_PX);
            let response = ui.add(
                egui::TextEdit::singleline(&mut query)
                    // The clear button is conditional, so its appearance after
                    // the first character shifts every following auto ID. A
                    // persistent ID keeps keyboard focus on this text edit
                    // instead of stranding it on the vanished previous ID.
                    .id_source("plan_name_search")
                    .desired_width(width)
                    .hint_text(s.search_hint),
            );
            if response.changed() {
                new_search = Some(query);
            }
        });
    });
    ui.add_space(4.0);
    // Divider between the summary row and the tree that follows in the same
    // region, so the two read as "summary above, detail below".
    ui.separator();

    if let Some(filter) = new_filter {
        app.set_category_filter(filter);
    }
    if let Some(axis) = new_axis {
        app.set_tree_axis(axis);
    }
    if let Some(category) = remove_category {
        app.request_delete_for_category(category);
    }
    if let Some(query) = new_search {
        app.set_search_query(query);
    }
}

#[cfg(test)]
mod tests {
    use super::show;

    use gametrimmer_core::settings::LanguagePreference;

    use crate::i18n::{self, Lang};
    use crate::model::{GroupAxis, GROUP_AXIS_ORDER};
    use crate::ui::harness::{UiTest, NARROW_VIEWPORT, STANDARD_VIEWPORT};

    fn seeded() -> UiTest {
        let mut test = UiTest::new(show);
        test.seed_findings();
        test
    }

    /// The switcher has to say which cut is active before it is touched -
    /// otherwise the tree's headings are the only clue, and "Disk C:" looks
    /// much the same whichever axis produced it.
    #[test]
    fn the_switcher_names_the_active_axis() {
        let mut test = seeded();
        let lang = test.app().lang();

        test.assert_label(test.strings().plan_group_label);
        test.assert_combo_value(i18n::group_axis_label(lang, GroupAxis::Disk));

        test.app_mut().set_tree_axis(GroupAxis::Library);
        test.run();

        test.assert_no_combo_value(i18n::group_axis_label(lang, GroupAxis::Disk));
        test.assert_combo_value(i18n::group_axis_label(lang, GroupAxis::Library));
    }

    /// The control is wired to the app, not just drawn: picking an entry has to
    /// reach `set_tree_axis`. Without this the panel could render a switcher
    /// that looks right and regroups nothing.
    #[test]
    fn picking_an_entry_regroups_the_tree() {
        let mut test = seeded();
        let lang = test.app().lang();
        assert_eq!(test.app().tree_axis, GroupAxis::Disk);

        // Open the combo, then choose the launcher entry from the popup.
        test.open_combo(i18n::group_axis_label(lang, GroupAxis::Disk));
        test.click(i18n::group_axis_label(lang, GroupAxis::Launcher));

        assert_eq!(test.app().tree_axis, GroupAxis::Launcher);
    }

    /// This row already carries a summary, a category filter and a search
    /// field, and GT-35 adds a fourth control to it. Both window widths and
    /// both languages, for the reason the bottom bar is measured the same way:
    /// the Ukrainian strings are the longer set, and a control pushed past the
    /// right edge is exactly the bug the six-card strip was rebuilt to avoid.
    #[test]
    fn the_summary_row_holds_every_control_at_both_widths() {
        for (name, size) in [("standard", STANDARD_VIEWPORT), ("narrow", NARROW_VIEWPORT)] {
            for language in [Lang::En, Lang::Uk] {
                for axis in GROUP_AXIS_ORDER {
                    let mut test = UiTest::with_size(show, size);
                    test.app_mut()
                        .set_language(LanguagePreference::Fixed(language));
                    test.seed_findings();
                    test.app_mut().set_tree_axis(axis);
                    test.run();

                    let s = i18n::strings(language);
                    for label in [s.plan_filter_label, s.plan_group_label] {
                        let rect = test.rect_of(label);
                        assert!(
                            rect.min.x >= 0.0 && rect.max.x <= size.x,
                            "{name} window, {language:?}, {axis:?}: {label:?} sits at \
                             {rect:?} and is clipped by the {}pt viewport",
                            size.x,
                        );
                    }
                }
            }
        }
    }
}
