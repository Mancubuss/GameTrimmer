//! The settings dialog. Each change is applied and persisted immediately
//! (see `GameTrimmerApp::set_delete_method`), so "Закрити" only dismisses
//! the dialog - there is no separate save step to forget.
//!
//! Sections: deletion method, database maintenance ("Стиснути базу даних").
//! Planned (see BACKLOG.md): keep-list languages, scanned artifact
//! categories, app language, theme.

use eframe::egui;

use gametrimmer_core::settings::DeleteMethod;

use crate::app::GameTrimmerApp;

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    if !app.show_settings {
        return;
    }

    let mut close = false;
    let mut compact_clicked = false;
    let mut picked_method = app.settings.delete_method;

    egui::Modal::new(egui::Id::new("gt_settings")).show(ui.ctx(), |ui| {
        ui.set_min_width(420.0);
        ui.heading("Налаштування");
        ui.add_space(8.0);

        ui.label("Спосіб видалення файлів:");
        ui.add_space(4.0);
        // Disabled while a worker is running: changing the method persists
        // immediately through a fresh DB connection, which would race an
        // in-flight `VACUUM` (it needs exclusive access) or delete job.
        ui.add_enabled_ui(!app.busy, |ui| {
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
        });

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);

        ui.label("База даних:");
        ui.add_space(4.0);
        if ui
            .add_enabled(!app.busy, egui::Button::new("Стиснути базу даних"))
            .clicked()
        {
            compact_clicked = true;
        }
        ui.small("Звільняє місце, яке база даних більше не використовує після видалень.");

        ui.add_space(12.0);
        if ui.button("Закрити").clicked() {
            close = true;
        }
    });

    app.set_delete_method(picked_method);
    if compact_clicked {
        app.start_compact();
    }
    if close {
        app.show_settings = false;
    }
}
