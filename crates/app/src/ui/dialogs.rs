//! Modal dialogs: delete confirmation and the post-delete result summary.
//! No file is ever removed without the user explicitly clicking through
//! the confirmation modal here.

use eframe::egui;

use crate::app::GameTrimmerApp;
use crate::model::{format_size, group_size_bytes};

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    show_confirm_delete(app, ui);
    show_remove_summary(app, ui);
}

fn show_confirm_delete(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let Some(indices) = app.confirm_delete.clone() else {
        return;
    };

    let count = indices.len();
    let bytes = group_size_bytes(&app.findings, &indices);

    let mut confirmed = false;
    let mut cancelled = false;

    egui::Modal::new(egui::Id::new("gt_confirm_delete")).show(ui.ctx(), |ui| {
        ui.set_min_width(320.0);
        ui.heading("Підтвердження видалення");
        ui.add_space(8.0);
        ui.label(format!(
            "Перемістити {count} файл(ів) ({}) у Кошик?",
            format_size(bytes)
        ));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Скасувати").clicked() {
                cancelled = true;
            }
            if ui.button("Перемістити в Кошик").clicked() {
                confirmed = true;
            }
        });
    });

    if confirmed {
        app.confirm_delete_now();
    } else if cancelled {
        app.cancel_delete_confirmation();
    }
}

fn show_remove_summary(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let Some(summary) = &app.remove_summary else {
        return;
    };

    let succeeded = summary.succeeded;
    let failed_count = summary.failed.len();
    // Only the first few errors are shown so one bad batch doesn't flood the dialog.
    let failed_preview: Vec<String> = summary
        .failed
        .iter()
        .take(5)
        .map(|(path, err)| format!("{}: {err}", path.display()))
        .collect();

    let mut close = false;

    egui::Modal::new(egui::Id::new("gt_remove_summary")).show(ui.ctx(), |ui| {
        ui.set_min_width(360.0);
        ui.heading("Результат видалення");
        ui.add_space(8.0);
        ui.label(format!("Успішно переміщено в Кошик: {succeeded}"));
        ui.label(format!("Помилок: {failed_count}"));

        if !failed_preview.is_empty() {
            ui.add_space(6.0);
            for line in &failed_preview {
                ui.label(line);
            }
            if failed_count > failed_preview.len() {
                ui.label(format!(
                    "... і ще {} помилка(ок)",
                    failed_count - failed_preview.len()
                ));
            }
        }

        ui.add_space(8.0);
        if ui.button("Закрити").clicked() {
            close = true;
        }
    });

    if close {
        app.remove_summary = None;
    }
}
