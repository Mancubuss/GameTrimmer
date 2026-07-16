//! The settings dialog. Each change is applied and persisted immediately
//! (see `GameTrimmerApp::set_delete_method`), so "Закрити" only dismisses
//! the dialog - there is no separate save step to forget.
//!
//! Planned sections (see BACKLOG.md): keep-list languages, scanned artifact
//! categories, app language, theme.

use eframe::egui;

use gametrimmer_core::settings::DeleteMethod;

use crate::app::GameTrimmerApp;

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    if !app.show_settings {
        return;
    }

    let mut close = false;
    let mut picked_method = app.settings.delete_method;

    egui::Modal::new(egui::Id::new("gt_settings")).show(ui.ctx(), |ui| {
        ui.set_min_width(420.0);
        ui.heading("Налаштування");
        ui.add_space(8.0);

        ui.label("Спосіб видалення файлів:");
        ui.add_space(4.0);
        ui.radio_value(
            &mut picked_method,
            DeleteMethod::Permanent,
            "Остаточне видалення (найшвидше)",
        );
        ui.indent("gt_settings_permanent_hint", |ui| {
            ui.small(
                "Файли видаляються безповоротно. Якщо видалиться щось потрібне — \
                 гру завжди можна перевстановити з магазину.",
            );
        });
        ui.add_space(4.0);
        ui.radio_value(
            &mut picked_method,
            DeleteMethod::RecycleBin,
            "У Кошик Windows (повільніше)",
        );
        ui.indent("gt_settings_recycle_hint", |ui| {
            ui.small("Файли можна відновити з Кошика, доки його не очищено.");
        });

        ui.add_space(12.0);
        if ui.button("Закрити").clicked() {
            close = true;
        }
    });

    app.set_delete_method(picked_method);
    if close {
        app.show_settings = false;
    }
}
