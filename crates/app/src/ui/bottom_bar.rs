//! Bottom panel: persistent selection summary, bulk-selection actions and
//! the delete action.

use eframe::egui;

use crate::model::{format_size, AUTO_SELECT_CONFIDENCE_THRESHOLD};

use crate::app::GameTrimmerApp;

/// Explains where the initial checkmarks come from and how to change the
/// selection in bulk - shown as a hover hint next to the summary.
fn selection_hint() -> String {
    format!(
        "Після сканування автоматично вибираються лише знахідки з упевненістю \
         не нижче {AUTO_SELECT_CONFIDENCE_THRESHOLD}%. Знахідки з нижчою упевненістю \
         позначені \u{26a0} і залишаються невибраними — перегляньте їх вручну.\n\n\
         Прапорці на рядках дерева вибирають цілий диск, гру, категорію чи теку. \
         Права кнопка миші на диску, грі або категорії відкриває дії масового вибору \
         (зокрема категорію на всьому диску).\n\n\
         Клавіатура: \u{2191}\u{2193} — курсор, PgUp/PgDn — сторінка, \
         \u{2192}/\u{2190} — розгорнути/згорнути, Space — вибрати."
    )
}

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
            ui.label("\u{2139}").on_hover_text(selection_hint());

            ui.add_space(12.0);

            let has_findings = app.findings.iter().any(|item| !item.removed);
            if ui
                .add_enabled(has_findings && !app.busy, egui::Button::new("Вибрати все"))
                .clicked()
            {
                app.select_all();
            }
            if ui
                .add_enabled(
                    selected_count > 0 && !app.busy,
                    egui::Button::new("Зняти вибір"),
                )
                .clicked()
            {
                app.deselect_all();
            }

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
