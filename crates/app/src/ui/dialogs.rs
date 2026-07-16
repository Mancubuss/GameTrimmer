//! Modal dialogs: delete confirmation and the post-delete result summary.
//! No file is ever removed without the user explicitly clicking through
//! the confirmation modal here.

use eframe::egui;

use gametrimmer_core::settings::DeleteMethod;

use crate::app::GameTrimmerApp;
use crate::model::{format_size, group_size_bytes};

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    show_elevation_prompt(app, ui);
    show_confirm_delete(app, ui);
    show_remove_summary(app, ui);
}

/// Startup modal offering to relaunch elevated for the faster MFT scan
/// path. Only shown once, and only when the process isn't already
/// Administrator-elevated (see `crate::elevation`).
fn show_elevation_prompt(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    if !app.show_elevation_prompt {
        return;
    }

    let mut relaunch = false;
    let mut cont = false;

    egui::Modal::new(egui::Id::new("gt_elevation_prompt")).show(ui.ctx(), |ui| {
        ui.set_min_width(380.0);
        ui.heading("Пришвидшити сканування?");
        ui.add_space(8.0);
        ui.label(
            "Швидке сканування читає файлову таблицю NTFS ($MFT) напряму, як інструмент \
             Everything, і для цього потрібні права адміністратора. Без них сканування \
             працюватиме повільніше (звичайний обхід тек).",
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Продовжити без прискорення").clicked() {
                cont = true;
            }
            if ui
                .button("Перезапустити від імені адміністратора")
                .clicked()
            {
                relaunch = true;
            }
        });
    });

    if relaunch {
        app.relaunch_elevated();
    } else if cont {
        app.continue_without_elevation();
    }
}

fn show_confirm_delete(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let Some(indices) = app.confirm_delete.clone() else {
        return;
    };

    let count = indices.len();
    let bytes = group_size_bytes(&app.findings, &indices);

    let (question, confirm_label) = match app.settings.delete_method {
        DeleteMethod::Permanent => (
            format!(
                "Безповоротно видалити {count} файл(ів) ({})? Відновлення буде \
                 неможливе (гру можна перевстановити з магазину).",
                format_size(bytes)
            ),
            "Видалити безповоротно",
        ),
        DeleteMethod::RecycleBin => (
            format!(
                "Перемістити {count} файл(ів) ({}) у Кошик?",
                format_size(bytes)
            ),
            "Перемістити в Кошик",
        ),
    };

    let mut confirmed = false;
    let mut cancelled = false;

    egui::Modal::new(egui::Id::new("gt_confirm_delete")).show(ui.ctx(), |ui| {
        ui.set_min_width(320.0);
        ui.heading("Підтвердження видалення");
        ui.add_space(8.0);
        ui.label(question);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Скасувати").clicked() {
                cancelled = true;
            }
            if ui.button(confirm_label).clicked() {
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
        let success_line = match app.settings.delete_method {
            DeleteMethod::Permanent => format!("Успішно видалено: {succeeded}"),
            DeleteMethod::RecycleBin => format!("Успішно переміщено в Кошик: {succeeded}"),
        };
        ui.label(success_line);
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
