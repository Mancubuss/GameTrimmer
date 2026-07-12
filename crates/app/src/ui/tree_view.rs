//! Central panel: the two-level (category -> game) findings tree with
//! per-file checkboxes.

use eframe::egui;
use eframe::egui::collapsing_header::CollapsingState;

use crate::app::GameTrimmerApp;
use crate::model::{
    self, category_display, format_size, group_selection_state, group_size_bytes, toggle_group,
    CategoryGroup, DisplayCategory, GameGroup,
};

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    egui::CentralPanel::default().show(ui, |ui| {
        if app.tree.is_empty() {
            ui.add_space(16.0);
            ui.label(if app.busy {
                "Сканування триває..."
            } else {
                "Немає знахідок. Натисніть \u{ab}Сканувати бібліотеки\u{bb}, щоб почати."
            });
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            // Cloning the tree lets us mutate `app.findings` freely inside
            // the closures below without fighting the borrow checker; the
            // tree itself (indices, names) never changes mid-frame.
            let tree = app.tree.clone();
            for category_group in &tree {
                show_category(app, ui, category_group);
            }
        });
    });
}

fn show_category(app: &mut GameTrimmerApp, ui: &mut egui::Ui, group: &CategoryGroup) {
    let all_indices: Vec<usize> = group
        .games
        .iter()
        .flat_map(|g| g.item_indices.iter().copied())
        .collect();
    let (all_selected, any_selected) = group_selection_state(&app.findings, &all_indices);
    let total_bytes = group_size_bytes(&app.findings, &all_indices);

    let id = ui.make_persistent_id(("gt_category", model::category_key(group.category)));
    let state = CollapsingState::load_with_default_open(ui.ctx(), id, true);

    state
        .show_header(ui, |ui| {
            let mut checked = all_selected;
            let response = ui.add(
                egui::Checkbox::new(&mut checked, "").indeterminate(any_selected && !all_selected),
            );
            if response.clicked() {
                toggle_group(&mut app.findings, &all_indices);
            }
            ui.label(format!(
                "{} ({} файл(ів), {})",
                category_display(group.category),
                all_indices.len(),
                format_size(total_bytes)
            ));
        })
        .body(|ui| {
            for game_group in &group.games {
                show_game(app, ui, group.category, game_group);
            }
        });
}

fn show_game(
    app: &mut GameTrimmerApp,
    ui: &mut egui::Ui,
    category: DisplayCategory,
    group: &GameGroup,
) {
    let (all_selected, any_selected) = group_selection_state(&app.findings, &group.item_indices);
    let total_bytes = group_size_bytes(&app.findings, &group.item_indices);

    let id = ui.make_persistent_id(("gt_game", model::category_key(category), group.game_id));
    let state = CollapsingState::load_with_default_open(ui.ctx(), id, false);

    state
        .show_header(ui, |ui| {
            let mut checked = all_selected;
            let response = ui.add(
                egui::Checkbox::new(&mut checked, "").indeterminate(any_selected && !all_selected),
            );
            if response.clicked() {
                toggle_group(&mut app.findings, &group.item_indices);
            }
            ui.label(format!(
                "{} ({} файл(ів), {})",
                group.game_name,
                group.item_indices.len(),
                format_size(total_bytes)
            ));
        })
        .body(|ui| {
            egui::Grid::new(("gt_grid", model::category_key(category), group.game_id))
                .num_columns(4)
                .striped(true)
                .show(ui, |ui| {
                    for &index in &group.item_indices {
                        show_file_row(app, ui, index);
                    }
                });
        });
}

fn show_file_row(app: &mut GameTrimmerApp, ui: &mut egui::Ui, index: usize) {
    let item = &mut app.findings[index];
    ui.checkbox(&mut item.selected, "");
    ui.label(&item.row.rel_path);
    ui.label(format_size(item.row.size));
    let confidence_label = match &item.row.lang_tag {
        Some(lang) => format!(
            "{}% [{}] \u{2014} {}",
            item.row.confidence, lang, item.row.rule_desc
        ),
        None => format!("{}% \u{2014} {}", item.row.confidence, item.row.rule_desc),
    };
    ui.label(confidence_label);
    ui.end_row();
}
