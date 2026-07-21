//! Modal dialogs: delete confirmation and the post-delete result summary.
//! No file is ever removed without the user explicitly clicking through
//! the confirmation modal here.

use eframe::egui;

use gametrimmer_core::settings::DeleteMethod;

use crate::app::GameTrimmerApp;
use crate::i18n;
use crate::model::{format_size, group_size_bytes};

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    show_elevation_prompt(app, ui);
    show_confirm_delete(app, ui);
    show_confirm_clear_database(app, ui);
    show_remove_summary(app, ui);
}

/// Startup modal offering to relaunch elevated for the faster MFT scan
/// path. Only shown once, and only when the process isn't already
/// Administrator-elevated (see `crate::elevation`).
fn show_elevation_prompt(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    if !app.show_elevation_prompt {
        return;
    }

    let s = i18n::strings(app.lang());
    let mut relaunch = false;
    let mut cont = false;

    let modal = egui::Modal::new(egui::Id::new("gt_elevation_prompt")).show(ui.ctx(), |ui| {
        ui.set_min_width(380.0);
        ui.heading(s.elevation_heading);
        ui.add_space(8.0);
        ui.label(s.elevation_body);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(s.btn_continue_without_elevation).clicked() {
                cont = true;
            }
            if ui.button(s.btn_relaunch_elevated).clicked() {
                relaunch = true;
            }
        });
    });

    // Esc / backdrop click dismisses without relaunching - the elevation is a
    // one-time offer, so dismissal maps to the non-destructive "continue
    // without elevation" path (never a relaunch), same intent as clicking that
    // button. `should_close` consumes the Escape press so it doesn't leak.
    if modal.should_close() {
        cont = true;
    }

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

    let lang = app.lang();
    let s = i18n::strings(lang);
    let count = indices.len();
    let bytes = group_size_bytes(&app.findings, &indices);

    let (question, confirm_label) = match app.settings.delete_method {
        DeleteMethod::Permanent => (
            i18n::confirm_permanent_question(lang, count, &format_size(lang, bytes)),
            s.confirm_label_permanent,
        ),
        DeleteMethod::RecycleBin => (
            i18n::confirm_recycle_question(lang, count, &format_size(lang, bytes)),
            s.confirm_label_recycle,
        ),
    };

    let mut confirmed = false;
    let mut cancelled = false;

    let modal = egui::Modal::new(egui::Id::new("gt_confirm_delete")).show(ui.ctx(), |ui| {
        ui.set_min_width(320.0);
        ui.heading(s.confirm_delete_heading);
        ui.add_space(8.0);
        ui.label(question);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(s.btn_cancel).clicked() {
                cancelled = true;
            }
            if ui.button(confirm_label).clicked() {
                confirmed = true;
            }
        });
    });

    // Esc / backdrop click dismisses this destructive confirmation as a
    // *cancel*, never a delete - dismissing a "are you sure?" prompt must
    // always map to the safe path. `should_close` consumes the Escape press.
    if modal.should_close() {
        cancelled = true;
    }

    if confirmed {
        app.confirm_delete_now();
    } else if cancelled {
        app.cancel_delete_confirmation();
    }
}

/// "Clear database" confirmation - a destructive action (permanently wipes
/// all scan results and the operations journal), so it never runs directly
/// off the settings-dialog button click. Opened by
/// `GameTrimmerApp::request_clear_database_confirmation`; shown on top of
/// the settings dialog it was triggered from, same stacking as any other
/// modal opened while another is already up (see `egui::Modal`'s own
/// "most recently shown wins" rule).
fn show_confirm_clear_database(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    if !app.confirm_clear_database {
        return;
    }

    let s = i18n::strings(app.lang());
    let mut confirmed = false;
    let mut cancelled = false;

    let modal = egui::Modal::new(egui::Id::new("gt_confirm_clear_database")).show(ui.ctx(), |ui| {
        ui.set_min_width(320.0);
        ui.heading(s.confirm_clear_heading);
        ui.add_space(8.0);
        ui.label(s.confirm_clear_body);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(s.btn_cancel).clicked() {
                cancelled = true;
            }
            if ui.button(s.btn_confirm_clear).clicked() {
                confirmed = true;
            }
        });
    });

    // Esc / backdrop click dismisses this destructive confirmation as a
    // *cancel*, never a wipe - dismissing a "are you sure?" prompt must always
    // map to the safe path. `should_close` consumes the Escape press.
    if modal.should_close() {
        cancelled = true;
    }

    if confirmed {
        app.confirm_clear_database_now();
    } else if cancelled {
        app.cancel_clear_database_confirmation();
    }
}

fn show_remove_summary(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let Some(summary) = &app.remove_summary else {
        return;
    };

    let lang = app.lang();
    let s = i18n::strings(lang);
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

    let modal = egui::Modal::new(egui::Id::new("gt_remove_summary")).show(ui.ctx(), |ui| {
        ui.set_min_width(360.0);
        ui.heading(s.remove_summary_heading);
        ui.add_space(8.0);
        let success_line = match app.settings.delete_method {
            DeleteMethod::Permanent => i18n::success_line_permanent(lang, succeeded),
            DeleteMethod::RecycleBin => i18n::success_line_recycle(lang, succeeded),
        };
        ui.label(success_line);
        ui.label(i18n::errors_count_line(lang, failed_count));

        if !failed_preview.is_empty() {
            ui.add_space(6.0);
            for line in &failed_preview {
                ui.label(line);
            }
            if failed_count > failed_preview.len() {
                ui.label(i18n::more_errors_line(
                    lang,
                    failed_count - failed_preview.len(),
                ));
            }
        }

        ui.add_space(8.0);
        if ui.button(s.btn_close).clicked() {
            close = true;
        }
    });

    // Esc / backdrop click dismisses this informational summary, same as the
    // "Закрити" button - it reports an already-completed operation, so closing
    // it has no side effects. `should_close` consumes the Escape press.
    if modal.should_close() {
        close = true;
    }

    if close {
        app.remove_summary = None;
    }
}
