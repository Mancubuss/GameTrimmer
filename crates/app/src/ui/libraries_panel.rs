//! Library management section: lists every registered library (any vendor)
//! and lets the user add a folder as a manual library or remove one they
//! added. Rendered inside the top panel by [`crate::ui::top_bar`].

use eframe::egui;

use crate::app::GameTrimmerApp;
use crate::worker::manual::MANUAL_VENDOR;

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    egui::CollapsingHeader::new("Бібліотеки")
        .default_open(false)
        .show(ui, |ui| {
            let add_clicked = ui
                .add_enabled(
                    !app.busy && !app.folder_picker_active,
                    egui::Button::new("Додати теку..."),
                )
                .clicked();
            if add_clicked {
                app.start_add_library();
            }
            if app.folder_picker_active {
                ui.label("Вибір теки...");
            }

            ui.add_space(4.0);

            if app.libraries.is_empty() {
                ui.label("Бібліотек ще не зареєстровано.");
                return;
            }

            let mut to_remove = None;
            for library in &app.libraries {
                ui.horizontal(|ui| {
                    ui.label(format!("[{}]", library.vendor));
                    ui.label(library.path.display().to_string());
                    if library.vendor == MANUAL_VENDOR
                        && ui
                            .add_enabled(!app.busy, egui::Button::new("Прибрати"))
                            .clicked()
                    {
                        to_remove = Some(library.id);
                    }
                });
            }

            if let Some(library_id) = to_remove {
                app.remove_manual_library(library_id);
            }
        });
}
