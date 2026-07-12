//! Bottom panel: persistent selection summary and the delete action.

use eframe::egui;

use crate::model::format_size;

use crate::app::GameTrimmerApp;

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    egui::Panel::bottom("bottom_panel").show(ui, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let selected_items: Vec<&crate::model::FindingItem> = app
                .findings
                .iter()
                .filter(|item| item.selected && !item.removed)
                .collect();
            let selected_count = selected_items.len();
            let selected_bytes: u64 = selected_items.iter().map(|item| item.row.size).sum();

            ui.label(format!(
                "Вибрано {selected_count} файл(ів), буде звільнено {}",
                format_size(selected_bytes)
            ));

            ui.add_space(12.0);

            let delete_clicked = ui
                .add_enabled(
                    selected_count > 0 && !app.busy,
                    egui::Button::new("Видалити вибране в Кошик"),
                )
                .clicked();
            if delete_clicked {
                app.request_delete_confirmation();
            }
        });
        ui.add_space(4.0);
    });
}
