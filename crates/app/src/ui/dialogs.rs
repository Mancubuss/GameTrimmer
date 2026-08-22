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

/// Draws `text` while reserving the space `largest` would need at the current
/// width. A caller that swaps between texts of different sizes (the delete
/// modal's per-method question) then keeps a constant overall size instead of
/// resizing on every switch. Both are laid out at the same `available_width`,
/// so the reserved size is exactly what `largest` needs; `text` is
/// top-left-aligned within it.
///
/// Both axes, not just height: reserving the height alone still let the modal
/// change *width* with the question, which moves the buttons sideways - the
/// same swallowed-click problem this exists to prevent, in the other
/// direction.
fn label_reserving_size(ui: &mut egui::Ui, text: &str, largest: &str) {
    let wrap_width = ui.available_width();
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let color = ui.visuals().text_color();
    let largest_size = ui
        .painter()
        .layout(largest.to_owned(), font_id, color, wrap_width)
        .size();
    ui.scope(|ui| {
        ui.set_min_size(largest_size);
        ui.label(text);
    });
}

/// Startup modal offering to relaunch elevated for the faster MFT scan
/// path. Shown when the process isn't already Administrator-elevated (see
/// `crate::elevation`) and elevating would actually change a route.
///
/// The "don't ask again" checkbox is the modal's own, not a settings-screen
/// option, and it is the only permanent way to refuse. Before the routing
/// modes were retired, refusing permanently meant finding "Always walk
/// folders" in Settings - a file-enumeration strategy standing in for
/// "stop asking me", which is neither where the user is looking nor what
/// they mean. Dismissing the modal without it still lasts one session only.
fn show_elevation_prompt(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    if !app.show_elevation_prompt {
        return;
    }

    let s = i18n::strings(app.lang());
    let mut relaunch = false;
    let mut cont = false;
    // Edited copy of the app's in-flight tick, written back below every frame
    // and through to the setting only on the way out - the modal body borrows
    // `app` immutably for `s`. Seeding this from the *setting* instead is how
    // the tick used to be lost: the modal is rebuilt every frame, so the box
    // was empty again by the frame the user reached "Continue".
    let mut never_ask = app.elevation_never_ask;

    let modal = egui::Modal::new(egui::Id::new("gt_elevation_prompt")).show(ui.ctx(), |ui| {
        ui.set_min_width(380.0);
        ui.heading(s.elevation_heading);
        ui.add_space(8.0);
        ui.label(s.elevation_body);
        ui.add_space(6.0);
        // Under the offer rather than beside it: "what happens if I say no"
        // and "why am I being asked at all" are the two questions this modal
        // used to leave to the README.
        ui.small(s.elevation_when_asked);
        ui.add_space(8.0);
        ui.checkbox(&mut never_ask, s.elevation_never_ask);
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

    // Keep the in-flight tick across frames, same as the delete modal keeps its
    // method and "remember" choice: the box the user ticked is never the frame
    // that reads it.
    app.elevation_never_ask = never_ask;

    if relaunch {
        // Relaunching deliberately ignores the checkbox: the user is saying
        // yes *now*, and a relaunch restarts the process, so a stored "never
        // ask" would only be read on some later launch where it would
        // contradict the answer they actually gave.
        app.relaunch_elevated();
    } else if cont {
        app.continue_without_elevation(never_ask);
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
        // reserves the *larger* question's size (the permanent wording is the
        // longer one) so switching the method never resizes the modal: without
        // that, the modal grew/shrank on each switch, and the confirm button
        // jumping to a new position could swallow the first click aimed at it.
        let question = match picked_method {
            DeleteMethod::Permanent => i18n::confirm_permanent_question(lang, count, &size_str),
            DeleteMethod::RecycleBin => i18n::confirm_recycle_question(lang, count, &size_str),
        };
        let largest_question = i18n::confirm_permanent_question(lang, count, &size_str);
        label_reserving_size(ui, &question, &largest_question);
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
                // Honest freed-vs-expected on-disk space (allocated-size accounting): "of the
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
                // Bin-bound bytes only free once the bin is emptied (allocated-size accounting);
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
    // "Close" button - it reports an already-completed operation, so closing
    // it has no side effects. `should_close` consumes the Escape press.
    if modal.should_close() {
        close = true;
    }

    if close {
        app.remove_summary = None;
    }
}

#[cfg(test)]
mod tests {
    use super::show;

    use std::path::PathBuf;

    use eframe::egui;
    use gametrimmer_core::settings::DeleteMethod;

    use crate::app::{ConfirmDelete, RemoveSummary};
    use crate::i18n;
    use crate::model::format_size;
    use crate::ui::harness::UiTest;

    /// A harness with findings behind it, since every delete prompt words
    /// itself from the batch it is about to remove.
    fn with_findings() -> UiTest {
        let mut test = UiTest::new(show);
        test.seed_findings();
        test
    }

    /// Puts the delete confirmation up over both seeded findings, the way
    /// `open_delete_confirmation` does for a "delete selected" click.
    fn confirming_delete(method: DeleteMethod) -> UiTest {
        let mut test = with_findings();
        test.app_mut().confirm_delete = Some(ConfirmDelete {
            indices: vec![0, 1],
            method,
            remember: false,
        });
        test.run();
        test
    }

    /// Bytes the seeded batch promises to free, so a test can name the same
    /// figure the modal renders instead of hard-coding a formatted string.
    fn seeded_batch_size(test: &UiTest) -> String {
        let bytes = crate::model::group_size_bytes(&test.app().findings, &[0, 1]);
        format_size(test.app().lang(), bytes)
    }

    /// Cancelling a destructive confirmation must leave the batch alone *and*
    /// start nothing - `busy` is what a spawned delete would set, so it is the
    /// half of the claim that a "the modal closed" assertion alone would miss.
    #[test]
    fn cancelling_the_delete_confirmation_starts_no_delete() {
        let mut test = confirming_delete(DeleteMethod::Permanent);
        test.assert_label(test.strings().confirm_delete_heading);

        test.click(test.strings().btn_cancel);

        assert!(test.app().confirm_delete.is_none(), "the modal stayed up");
        assert!(!test.app().busy, "cancelling started a job");
    }

    /// Escape has to map to the safe path. This is the assertion that keeps
    /// `should_close` wired to `cancelled` rather than `confirmed` - swapping
    /// the two compiles and looks identical until it deletes someone's files.
    #[test]
    fn escape_dismisses_the_delete_confirmation_as_a_cancel() {
        let mut test = confirming_delete(DeleteMethod::Permanent);

        test.press(egui::Key::Escape);

        assert!(test.app().confirm_delete.is_none(), "the modal stayed up");
        assert!(!test.app().busy, "escape started a delete");
    }

    /// The per-operation method choice has to reach both the question and the
    /// confirm button in the same frame, and survive to the next one - the
    /// modal is rebuilt every frame, so a choice that is not written back
    /// snaps to the persisted default immediately.
    #[test]
    fn picking_recycle_bin_rewords_the_question_and_the_button() {
        let mut test = confirming_delete(DeleteMethod::Permanent);
        let lang = test.app().lang();
        let size = seeded_batch_size(&test);

        test.assert_label(&i18n::confirm_permanent_question(lang, 2, &size));
        test.assert_label(test.strings().confirm_label_permanent);

        test.click(test.strings().delete_method_recycle_label);

        assert_eq!(
            test.app().confirm_delete.as_ref().map(|s| s.method),
            Some(DeleteMethod::RecycleBin),
            "the choice did not survive the frame it was made in",
        );
        test.assert_label(&i18n::confirm_recycle_question(lang, 2, &size));
        test.assert_label(test.strings().confirm_label_recycle);
        test.assert_no_label(test.strings().confirm_label_permanent);
    }

    /// "Remember my choice" is read once, on confirm - so it too has to be
    /// written back every frame or it is always false by the time it matters.
    #[test]
    fn remembering_the_method_survives_the_next_frame() {
        let mut test = confirming_delete(DeleteMethod::RecycleBin);

        test.click(test.strings().remember_delete_method);
        test.run();

        assert_eq!(
            test.app().confirm_delete.as_ref().map(|s| s.remember),
            Some(true),
            "the tick was lost between frames",
        );
    }

    /// The constant-height question block, stated as the thing it protects:
    /// the buttons must not move when the method changes, or the first click
    /// aimed at "Delete" lands on a modal that has just resized under it.
    /// Measured on Cancel, the one button in that row whose label does not
    /// change with the method.
    #[test]
    fn switching_the_method_never_moves_the_buttons() {
        let mut test = confirming_delete(DeleteMethod::Permanent);
        let before = test.rect_of(test.strings().btn_cancel);

        test.click(test.strings().delete_method_recycle_label);

        let after = test.rect_of(test.strings().btn_cancel);
        assert_eq!(
            before, after,
            "the buttons moved from {before:?} to {after:?} when the method changed",
        );
    }

    /// The database wipe is destructive, so both dismissal routes have to be
    /// the safe one - and "safe" means the findings are still there, not just
    /// that a flag flipped.
    #[test]
    fn dismissing_the_clear_database_prompt_keeps_the_findings() {
        for dismiss in ["button", "escape"] {
            let mut test = with_findings();
            test.app_mut().confirm_clear_database = true;
            test.run();
            test.assert_label(test.strings().confirm_clear_heading);
            test.assert_label(test.strings().confirm_clear_body);

            match dismiss {
                "button" => test.click(test.strings().btn_cancel),
                _ => test.press(egui::Key::Escape),
            }

            assert!(
                !test.app().confirm_clear_database,
                "{dismiss}: the modal stayed up",
            );
            assert!(!test.app().busy, "{dismiss}: dismissing started a wipe");
            assert_eq!(
                test.app().findings.len(),
                2,
                "{dismiss}: dismissing wiped the findings",
            );
        }
    }

    /// A permanent batch that lost nothing reports the shorter wording - the
    /// "of the expected Y" half only earns its place when the two diverge.
    #[test]
    fn a_clean_permanent_summary_reports_only_what_was_freed() {
        let mut test = UiTest::new(show);
        let lang = test.app().lang();
        test.app_mut().remove_summary = Some(RemoveSummary {
            succeeded: 3,
            nuked: 0,
            failed: Vec::new(),
            method: DeleteMethod::Permanent,
            expected_bytes: 3 * 1024 * 1024,
            freed_bytes: 3 * 1024 * 1024,
            recycled_pending_bytes: 0,
        });
        test.run();

        let size = format_size(lang, 3 * 1024 * 1024);
        test.assert_label(test.strings().remove_summary_heading);
        test.assert_label(&i18n::success_line_permanent(lang, 3));
        test.assert_label(&i18n::freed_summary_line(lang, &size, &size, false));
        test.assert_label(&i18n::errors_count_line(lang, 0));
    }

    /// A recycle batch has to split the recoverable files from the ones
    /// Windows silently destroyed for exceeding the bin quota, and say which
    /// bytes are free now versus after emptying the bin.
    #[test]
    fn a_recycle_summary_splits_recoverable_from_permanently_nuked() {
        let mut test = UiTest::new(show);
        let lang = test.app().lang();
        test.app_mut().remove_summary = Some(RemoveSummary {
            succeeded: 3,
            nuked: 1,
            failed: Vec::new(),
            method: DeleteMethod::RecycleBin,
            expected_bytes: 3 * 1024 * 1024,
            freed_bytes: 1024 * 1024,
            recycled_pending_bytes: 2 * 1024 * 1024,
        });
        test.run();

        // Three succeeded, one of them nuked - so two are actually recoverable.
        test.assert_label(&i18n::success_line_recycle(lang, 2));
        test.assert_label(&i18n::recycle_pending_size_line(
            lang,
            &format_size(lang, 2 * 1024 * 1024),
        ));
        test.assert_label(&i18n::success_line_nuked(lang, 1));
        test.assert_label(&i18n::freed_now_size_line(
            lang,
            &format_size(lang, 1024 * 1024),
        ));
    }

    /// One bad batch must not turn the summary into an unbounded error log:
    /// the first five are listed, the rest are counted.
    #[test]
    fn only_the_first_five_failures_are_listed_and_the_rest_counted() {
        let mut test = UiTest::new(show);
        let lang = test.app().lang();
        test.app_mut().remove_summary = Some(RemoveSummary {
            succeeded: 0,
            nuked: 0,
            failed: (0..7)
                .map(|i| {
                    (
                        PathBuf::from(format!("C:\\Games\\fail_{i}.pak")),
                        "denied".to_string(),
                    )
                })
                .collect(),
            method: DeleteMethod::Permanent,
            expected_bytes: 0,
            freed_bytes: 0,
            recycled_pending_bytes: 0,
        });
        test.run();

        for i in 0..5 {
            test.assert_label_containing(&format!("fail_{i}.pak"));
        }
        for i in 5..7 {
            test.assert_no_label_containing(&format!("fail_{i}.pak"));
        }
        test.assert_label(&i18n::errors_count_line(lang, 7));
        test.assert_label(&i18n::more_errors_line(lang, 2));
    }

    /// The summary reports an already-finished operation, so both ways out are
    /// the same way out.
    #[test]
    fn both_ways_out_of_the_summary_close_it() {
        for dismiss in ["button", "escape"] {
            let mut test = UiTest::new(show);
            test.app_mut().remove_summary = Some(RemoveSummary {
                succeeded: 1,
                nuked: 0,
                failed: Vec::new(),
                method: DeleteMethod::Permanent,
                expected_bytes: 1024,
                freed_bytes: 1024,
                recycled_pending_bytes: 0,
            });
            test.run();

            match dismiss {
                "button" => test.click(test.strings().btn_close),
                _ => test.press(egui::Key::Escape),
            }

            assert!(
                test.app().remove_summary.is_none(),
                "{dismiss}: the summary stayed up",
            );
        }
    }

    /// Dismissing the elevation offer is a one-session answer, never a
    /// relaunch and never a permanent refusal - the checkbox is the only way
    /// to say "never".
    #[test]
    fn escape_on_the_elevation_offer_continues_unelevated_for_this_session() {
        let mut test = UiTest::new(show);
        test.app_mut().show_elevation_prompt = true;
        test.run();
        test.assert_label(test.strings().elevation_heading);
        test.assert_label(test.strings().elevation_body);
        test.assert_label(test.strings().elevation_when_asked);

        test.press(egui::Key::Escape);

        assert!(!test.app().show_elevation_prompt, "the modal stayed up");
        assert!(
            !test.app().settings.never_ask_elevation,
            "a dismissal was recorded as a permanent refusal",
        );
    }

    /// The checkbox is the only permanent way to refuse, so the tick has to
    /// still be there on the frame the user reaches for "Continue" - which is
    /// never the same frame they ticked it in.
    #[test]
    fn ticking_never_ask_then_continuing_stops_the_offer_for_good() {
        let mut test = UiTest::new(show);
        test.app_mut().show_elevation_prompt = true;
        test.run();

        test.click(test.strings().elevation_never_ask);
        test.click(test.strings().btn_continue_without_elevation);

        assert!(!test.app().show_elevation_prompt, "the modal stayed up");
        assert!(
            test.app().settings.never_ask_elevation,
            "the tick was lost between the frame it was made in and the click that reads it",
        );
    }
}
