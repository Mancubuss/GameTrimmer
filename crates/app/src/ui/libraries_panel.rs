//! Library management section: lists every registered library (any vendor)
//! and lets the user add a folder as a manual library or remove one they
//! added. Rendered inside the top panel by [`crate::ui::top_bar`].

use eframe::egui;

use crate::app::GameTrimmerApp;
use crate::i18n;
use crate::worker::manual::MANUAL_VENDOR;

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let s = i18n::strings(app.lang());
    egui::CollapsingHeader::new(s.libraries_header)
        .default_open(false)
        .show(ui, |ui| {
            let add_clicked = ui
                .add_enabled(
                    !app.busy && !app.folder_picker_active,
                    egui::Button::new(s.btn_add_folder),
                )
                .clicked();
            if add_clicked {
                app.start_add_library();
            }
            if app.folder_picker_active {
                ui.label(s.picking_folder);
            }

            ui.add_space(4.0);

            if app.libraries.is_empty() {
                ui.label(s.no_libraries_registered);
                return;
            }

            let mut to_remove = None;
            for library in &app.libraries {
                ui.horizontal(|ui| {
                    ui.label(format!("[{}]", library.vendor));
                    // Same normalization as every path the tree shows, so the
                    // list does not mix `d:\...` with `F:\...` depending on
                    // which launcher reported the root.
                    ui.label(crate::ui::row_actions::windows_path_string(&library.path));
                    ui.label(crate::model::format_size(
                        app.lang(),
                        app.occupancy.library_bytes(library.id),
                    ));
                    if library.vendor == MANUAL_VENDOR
                        && ui
                            .add_enabled(!app.busy, egui::Button::new(s.btn_remove))
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
