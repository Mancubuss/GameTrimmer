//! Top panel: scan button, DB status line, and scan progress/cancel.

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

            ui.separator();
            ui.label(format!("База даних: {}", app.db_status));
        });

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

        ui.add_space(4.0);
    });
}
