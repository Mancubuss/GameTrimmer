//! Top panel: scan/export/settings buttons and scan progress/cancel.

use eframe::egui;

use crate::app::{GameTrimmerApp, APP_TITLE};

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    egui::Panel::top("top_panel").show(ui, |ui| {
        ui.add_space(4.0);
        ui.heading(APP_TITLE);
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            let scan_clicked = ui
                .add_enabled(!app.busy, egui::Button::new("Сканувати бібліотеки"))
                .clicked();
            if scan_clicked {
                app.start_scan();
            }

            if app.busy && ui.button("Скасувати").clicked() {
                app.cancel_scan();
            }

            let export_clicked = ui
                .add_enabled(
                    !app.busy && !app.export_active && !app.findings.is_empty(),
                    egui::Button::new("Експортувати…"),
                )
                .clicked();
            if export_clicked {
                app.start_export();
            }

            ui.separator();
            if ui.button("Налаштування…").clicked() {
                app.show_settings = true;
            }
        });

        // The database lives next to the executable, so its path carries no
        // information for the user - only a failure to open it is shown.
        if let Some(db_error) = &app.db_error {
            ui.colored_label(ui.visuals().error_fg_color, db_error);
        }

        if let Some((current, total, game_name)) = app.progress.clone() {
            let fraction = if total == 0 {
                0.0
            } else {
                current as f32 / total as f32
            };
            ui.add(
                egui::ProgressBar::new(fraction)
                    .text(format!("Сканування {current}/{total}: {game_name}")),
            );
        } else if !app.status_message.is_empty() {
            ui.label(&app.status_message);
        }

        if !app.warnings.is_empty() {
            let count = app.warnings.len();
            egui::CollapsingHeader::new(format!("Попередження ({count})"))
                .default_open(false)
                .show(ui, |ui| {
                    for warning in app.warnings.clone() {
                        ui.label(warning);
                    }
                });
        }

        ui.add_space(4.0);
        crate::ui::libraries_panel::show(app, ui);
        ui.add_space(4.0);
    });
}
