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

/// Draws `text` while reserving the vertical space `tallest` would need at the
/// current width. A caller that swaps between texts of different heights (the
/// delete modal's per-method question) then keeps a constant overall height
/// instead of resizing on every switch. Both are laid out at the same
/// `available_width`, so the reserved height is exactly what `tallest` needs;
/// `text` is top-aligned within it.
fn label_reserving_height(ui: &mut egui::Ui, text: &str, tallest: &str) {
    let wrap_width = ui.available_width();
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let color = ui.visuals().text_color();
    let tallest_height = ui
        .painter()
        .layout(tallest.to_owned(), font_id, color, wrap_width)
        .size()
        .y;
    ui.scope(|ui| {
        ui.set_min_height(tallest_height);
        ui.label(text);
    });
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
    let Some(state) = app.confirm_delete.clone() else {
        return;
    };

    let lang = app.lang();
    let s = i18n::strings(lang);
    let count = state.indices.len();
    let bytes = group_size_bytes(&app.findings, &state.indices);

    // Edited copies of the modal's own state; written back to `app` once, at
    // the end, so the borrow of `app.findings` above stays put.
    let mut picked_method = state.method;
    let mut picked_remember = state.remember;

    let mut confirmed = false;
    let mut cancelled = false;

    let size_str = format_size(lang, bytes);
    let modal = egui::Modal::new(egui::Id::new("gt_confirm_delete")).show(ui.ctx(), |ui| {
        ui.set_min_width(380.0);
        ui.heading(s.confirm_delete_heading);
        ui.add_space(8.0);

        // The question is phrased per method, recomputed from the radio value
        // below (not the persisted setting) so picking "Recycle Bin" here
        // immediately rewords the prompt. Rendered into a block that always
        // reserves the *taller* question's height (the permanent wording is the
        // longer, multi-line one) so switching the method never resizes the
        // modal: without that, the modal grew/shrank on each switch, and the
        // confirm button jumping to a new position could swallow the first
        // click aimed at it.
        let question = match picked_method {
            DeleteMethod::Permanent => i18n::confirm_permanent_question(lang, count, &size_str),
            DeleteMethod::RecycleBin => i18n::confirm_recycle_question(lang, count, &size_str),
        };
        let tallest_question = i18n::confirm_permanent_question(lang, count, &size_str);
        label_reserving_height(ui, &question, &tallest_question);
        ui.add_space(8.0);

        ui.radio_value(
            &mut picked_method,
            DeleteMethod::Permanent,
            s.delete_method_permanent_label,
        );
        ui.radio_value(
            &mut picked_method,
            DeleteMethod::RecycleBin,
            s.delete_method_recycle_label,
        );
        ui.add_space(4.0);
        ui.checkbox(&mut picked_remember, s.remember_delete_method);

        ui.add_space(8.0);
        // Derived from the just-updated radio value so the button reflects the
        // current choice the same frame the radio changes, not one frame later.
        let confirm_label = match picked_method {
            DeleteMethod::Permanent => s.confirm_label_permanent,
            DeleteMethod::RecycleBin => s.confirm_label_recycle,
        };
        ui.horizontal(|ui| {
            if ui.button(s.btn_cancel).clicked() {
                cancelled = true;
            }
            if ui.button(confirm_label).clicked() {
                confirmed = true;
            }
        });
    });

    // Keep the in-flight choice across frames: the modal is rebuilt every
    // frame, so an edit that isn't written back would snap straight back to
    // the persisted setting on the next one.
    if let Some(pending) = app.confirm_delete.as_mut() {
        pending.method = picked_method;
        pending.remember = picked_remember;
    }

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
    let nuked = summary.nuked;
    let expected_bytes = summary.expected_bytes;
    let freed_bytes = summary.freed_bytes;
    let recycled_pending_bytes = summary.recycled_pending_bytes;
    let failed_count = summary.failed.len();
    // The method this batch actually ran with - never `app.settings.delete_method`,
    // which can differ from the per-operation choice when the user picked a
    // one-off method without ticking "remember".
    let method = summary.method;
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
        match method {
            DeleteMethod::Permanent => {
                ui.label(i18n::success_line_permanent(lang, succeeded));
                // Honest freed-vs-expected on-disk space (GT-05a): "of the
                // expected Y" only when they diverge - i.e. some files failed;
                // otherwise the shorter "Freed X" reads cleaner.
                ui.label(i18n::freed_summary_line(
                    lang,
                    &format_size(lang, freed_bytes),
                    &format_size(lang, expected_bytes),
                    freed_bytes != expected_bytes,
                ));
            }
            DeleteMethod::RecycleBin => {
                // Only the files that really landed in the bin are recoverable;
                // report those honestly as "space frees after you empty it".
                let recycled = succeeded - nuked;
                ui.label(i18n::success_line_recycle(lang, recycled));
                // Bin-bound bytes only free once the bin is emptied (GT-05a);
                // spell out the amount so the pre-delete estimate is reconciled.
                if recycled_pending_bytes > 0 {
                    ui.label(i18n::recycle_pending_size_line(
                        lang,
                        &format_size(lang, recycled_pending_bytes),
                    ));
                }
                // Windows permanently deletes items too large for the volume's
                // Recycle Bin quota, and `trash` reports that as success - so
                // call those out as permanent, never recoverable (see
                // `worker::RemoveOutcome::nuked`). Those bytes are freed now.
                if nuked > 0 {
                    ui.label(i18n::success_line_nuked(lang, nuked));
                    ui.label(i18n::freed_now_size_line(
                        lang,
                        &format_size(lang, freed_bytes),
                    ));
                }
            }
        }
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
