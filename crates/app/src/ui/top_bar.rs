//! Top panel: scan/export/settings buttons and scan progress/cancel.

use eframe::egui;

use crate::app::{GameTrimmerApp, APP_TITLE};
use crate::i18n;

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let lang = app.lang();
    let s = i18n::strings(lang);
    egui::Panel::top("top_panel").show(ui, |ui| {
        ui.add_space(4.0);
        ui.heading(APP_TITLE);
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            let scan_clicked = ui
                .add_enabled(!app.busy, egui::Button::new(s.btn_scan_libraries))
                .clicked();
            if scan_clicked {
                app.start_scan();
            }

            if app.busy && ui.button(s.btn_cancel).clicked() {
                app.cancel_scan();
            }

            let export_clicked = ui
                .add_enabled(
                    !app.busy && !app.export_active && !app.findings.is_empty(),
                    egui::Button::new(s.btn_export),
                )
                .clicked();
            if export_clicked {
                app.start_export();
            }

            ui.separator();
            if ui.button(s.btn_settings).clicked() {
                app.show_settings = true;
            }
        });

        // The database lives next to the executable, so its path carries no
        // information for the user - only a failure to open it is shown.
        if let Some(db_error) = &app.db_error {
            ui.colored_label(ui.visuals().error_fg_color, db_error);
        }

        if let Some(progress) = app.progress.clone() {
            let fraction = if progress.total == 0 {
                0.0
            } else {
                progress.current as f32 / progress.total as f32
            };
            // Compaction has no per-item "current of total" to show (it
            // reports an estimated percentage instead, with an empty
            // `detail`) - render "{verb} {percent}%" for that case; scan and
            // delete keep the granular "{verb} {current}/{total}: {detail}".
            let text = if progress.detail.is_empty() {
                let percent = if progress.total == 100 {
                    progress.current
                } else {
                    (100 * progress.current)
                        .checked_div(progress.total)
                        .unwrap_or(0)
                };
                format!("{} {}%", i18n::verb_label(lang, progress.verb), percent)
            } else {
                format!(
                    "{} {}/{}: {}",
                    i18n::verb_label(lang, progress.verb),
                    progress.current,
                    progress.total,
                    progress.detail
                )
            };
            ui.add(egui::ProgressBar::new(fraction).text(text));
        } else if !app.status_message.is_empty() {
            if app.busy {
                // Background jobs without granular progress (delete,
                // compaction) must not look like a frozen app - a spinner
                // gives visible activity even without a progress fraction.
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(&app.status_message);
                });
            } else {
                ui.label(&app.status_message);
            }
        }

        if !app.warnings.is_empty() {
            let count = app.warnings.len();
            egui::CollapsingHeader::new(i18n::warnings_header(lang, count))
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
