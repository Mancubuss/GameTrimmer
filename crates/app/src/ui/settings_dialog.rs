//! The settings dialog. Each change is applied and persisted immediately
//! (see `GameTrimmerApp::set_delete_method`), so "Закрити" only dismisses
//! the dialog - there is no separate save step to forget.
//!
//! Sections: deletion method, database maintenance ("Стиснути базу даних"),
//! analysis rule packs (export/import - see docs/05_rules_pack_plan.md).
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
    let mut export_rules_clicked = false;
    let mut import_rules_clicked = false;
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
        ui.small(
            "Звільняє місце, яке база даних більше не використовує після видалень. \
             Виконується, лише якщо звільниться щонайменше 25% обсягу.",
        );

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);

        ui.label("Правила аналізу:");
        ui.add_space(4.0);
        // The export only reads the pack files, but the import rewrites the
        // files a scan loads at startup - both are disabled during any
        // background job to keep the flow simple, plus while a previous
        // rules dialog is still open (`rules_io_active`).
        ui.add_enabled_ui(!app.busy && !app.rules_io_active, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Експортувати правила").clicked() {
                    export_rules_clicked = true;
                }
                if ui.button("Імпортувати правила").clicked() {
                    import_rules_clicked = true;
                }
            });
        });
        ui.small(
            "Експорт зберігає rules.json і l10n_rules.json у вибрану теку — основа для \
             власних правил чи правил спільноти. Імпорт об'єднує вибрані файли з поточними \
             правилами (нові додаються, збіги оновлюються) і зберігає їх поруч із програмою; \
             попередні файли залишаються як *.bak. Зміни діятимуть з наступного сканування.",
        );
        // The top-bar status line is hidden behind this modal, so the
        // outcome must be shown right here, under the buttons.
        if app.rules_io_active {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Виконується...");
            });
        } else if let Some(result) = &app.rules_io_result {
            ui.add_space(4.0);
            match result {
                Ok(msg) => {
                    ui.colored_label(egui::Color32::from_rgb(0x4c, 0xaf, 0x50), msg);
                }
                Err(msg) => {
                    ui.colored_label(ui.visuals().error_fg_color, msg);
                }
            }
        }

        ui.add_space(12.0);
        if ui.button("Закрити").clicked() {
            close = true;
        }
    });

    app.set_delete_method(picked_method);
    if compact_clicked {
        app.start_compact();
    }
    if export_rules_clicked {
        app.start_rules_export();
    }
    if import_rules_clicked {
        app.start_rules_import();
    }
    if close {
        app.show_settings = false;
        // A stale export/import result must not greet the user on the next
        // open; the top-bar status line keeps the last success anyway.
        app.rules_io_result = None;
    }
}
