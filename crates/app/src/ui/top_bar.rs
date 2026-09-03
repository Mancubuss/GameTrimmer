//! Top panel: scan/export/settings buttons and scan progress/cancel.

use eframe::egui;

use crate::app::GameTrimmerApp;
use crate::i18n;

/// One frame of a textual spinner for the given animation-clock time (seconds),
/// cycling through `| / - \` at a fixed 8 frames per second.
///
/// Kept ASCII on purpose: braille/box spinner glyphs are not guaranteed to be
/// present in the bundled UI font (only the CJK/symbol fallbacks added for game
/// names cover exotic ranges), so an ASCII frame is the safe choice that always
/// renders. Pure and side-effect free so it can be unit-tested.
fn spinner_frame(time: f64) -> char {
    const FRAMES: [char; 4] = ['|', '/', '-', '\\'];
    const SPINNER_FPS: f64 = 8.0;
    let idx = (time * SPINNER_FPS) as i64;
    FRAMES[idx.rem_euclid(FRAMES.len() as i64) as usize]
}

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let lang = app.lang();
    let s = i18n::strings(lang);
    egui::Panel::top("top_panel").show(ui, |ui| {
        // No app-name heading here: the window's own title bar already says
        // "GameTrimmer" a few points above, and a second copy spent a whole
        // row of the top panel restating it to a user who is looking at it.
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            // The running job first: it is the blocker the user has to wait
            // out, and it outranks a gate they can clear in one click. A dead
            // database ranks next, above the disclaimer: unlike the
            // disclaimer there is no click that clears it - opening it failed
            // at startup and nothing this window can do reopens it - so it is
            // even less "one click away" than the gate it outranks, and
            // naming it beats sending the user to accept a disclaimer that
            // would not unblock anything anyway.
            let scan_blocked = app
                .busy
                .then_some(s.disabled_busy)
                .or_else(|| app.blocked_by_database())
                .or_else(|| app.blocked_by_disclaimer());
            if crate::ui::gated_button(ui, s.btn_scan_libraries, scan_blocked).clicked() {
                app.start_scan();
            }

            // Only for a job that actually stops when asked - `cancel_scan`
            // sets the scan's token and nothing else reads it, so during a
            // delete, compaction, database clear or rules import this button
            // used to look actionable and do nothing.
            if app.can_cancel() && ui.button(s.btn_cancel).clicked() {
                app.cancel_scan();
            }

            let export_blocked = if app.busy {
                Some(s.disabled_busy)
            } else if app.export_active {
                Some(s.disabled_export_running)
            } else if app.findings.is_empty() {
                Some(s.disabled_no_findings)
            } else {
                None
            };
            if crate::ui::gated_button(ui, s.btn_export, export_blocked).clicked() {
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

        if let Some(phase_state) = &app.scan_phase_state {
            ui.add_space(2.0);
            let overall_pct = (phase_state.overall_fraction * 100.0) as u32;
            let overall_text = format!("{}: {}%", s.scan_overall_title, overall_pct);
            ui.add(egui::ProgressBar::new(phase_state.overall_fraction).text(overall_text));

            if let Some(p1) = &phase_state.phase1 {
                let frac = if p1.total > 0 {
                    p1.current as f32 / p1.total as f32
                } else {
                    0.0
                };
                let label = if !p1.detail.is_empty() {
                    format!(
                        "{}: {}",
                        i18n::scan_phase_1_label(lang, p1.current, p1.total),
                        p1.detail
                    )
                } else {
                    i18n::scan_phase_1_label(lang, p1.current, p1.total)
                };
                ui.add(egui::ProgressBar::new(frac).text(label));
            }

            if let Some(p2) = &phase_state.phase2 {
                let frac = if p2.total > 0 {
                    p2.current as f32 / p2.total as f32
                } else {
                    0.0
                };
                let label = if !p2.detail.is_empty() {
                    format!(
                        "{}: {}",
                        i18n::scan_phase_2_label(lang, p2.current, p2.total),
                        p2.detail
                    )
                } else {
                    i18n::scan_phase_2_label(lang, p2.current, p2.total)
                };
                ui.add(egui::ProgressBar::new(frac).text(label));
            }
        } else if let Some(progress) = app.progress.clone() {
            let fraction = if progress.total == 0 {
                0.0
            } else {
                progress.current as f32 / progress.total as f32
            };

            // Track, on egui's animation clock, how long the progress line has
            // shown the same item. When one item (a large game being analyzed)
            // holds it unchanged past a short threshold, the app can look
            // frozen even though work continues - so a running-dots suffix is
            // appended after the item's name to signal it's still alive. During
            // the bulk of a scan, sibling games finishing in parallel keep the
            // line changing every fraction of a second, so this only kicks in
            // at the tail, when a single big game is the last one analyzing.
            let now = ui.input(|i| i.time);
            if progress.detail != app.last_progress_detail {
                app.last_progress_detail = progress.detail.clone();
                app.last_progress_detail_at = now;
            }
            let stalled_for = now - app.last_progress_detail_at;

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
                // Threshold ~= the point a still line starts reading as "stuck".
                const SPINNER_AFTER_SECS: f64 = 1.0;
                let spinner =
                    if progress.verb == i18n::Verb::Analyze && stalled_for >= SPINNER_AFTER_SECS {
                        // A single glyph spinning in place - fixed width, reads as
                        // motion, rather than a line that keeps growing dots.
                        format!(" {}", spinner_frame(now))
                    } else {
                        String::new()
                    };
                format!(
                    "{} {}/{}: {}{}",
                    i18n::verb_label(lang, progress.verb),
                    progress.current,
                    progress.total,
                    progress.detail,
                    spinner
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

        ui.add_space(4.0);
    });
}

#[cfg(test)]
mod tests {
    use super::{show, spinner_frame};

    use crate::app::{GameTrimmerApp, ProgressState};
    use crate::i18n;
    use crate::ui::harness::UiTest;
    use crate::worker::WorkerMsg;

    /// The window's own title bar already says "GameTrimmer"; a heading here
    /// restated it one row below, spending a line of the most crowded panel
    /// in the app on something the user is already looking at.
    #[test]
    fn the_panel_does_not_repeat_the_window_title() {
        let mut test = UiTest::new(show);
        test.run();

        test.assert_no_label(crate::app::APP_TITLE);
    }

    /// Scan-time diagnostics name app ids and manifest fields. They belong in
    /// the log, where a bug report can carry them - not on the window, where
    /// they pushed everything else down after every single scan and told the
    /// user nothing they could act on.
    #[test]
    fn scan_diagnostics_do_not_reach_the_window() {
        let mut test = UiTest::new(show);
        test.app_mut().apply_message(WorkerMsg::Warning {
            msg: "Provider \"ubisoft\": 1081 has no usable InstallDir [game-entry]".to_string(),
        });
        test.run();

        test.assert_no_label_containing("1081");
        test.assert_no_label_containing("game-entry");
    }

    /// The counterpart, and the risk in removing that list: a failed export is
    /// the answer to a button the user just pressed. It has to land on the
    /// status line, or the click looks like it worked.
    #[test]
    fn a_failed_export_still_reaches_the_window() {
        let mut test = UiTest::new(show);
        test.app_mut().apply_message(WorkerMsg::ExportDone {
            path: None,
            error: Some("disk is full".to_string()),
        });
        test.run();

        test.assert_label_containing("disk is full");
    }

    /// GT-74, half A. Before the fix, this same sentence could reach the
    /// window twice: once as `db_error` (drawn permanently, below), and again
    /// as `status_message` once a scan the button should have refused to
    /// start reached the worker's own `db::open` failure. `count_labels_containing`
    /// rather than `assert_label_containing` is what makes this a regression
    /// test rather than a presence check - it fails the moment a second copy
    /// shows up anywhere on screen, including one arriving via
    /// `status_message`.
    #[test]
    fn a_dead_database_explains_itself_exactly_once() {
        let mut test = UiTest::new(show);
        test.app_mut().db_error = Some("gt_probe_db_dead".to_string());
        test.run();

        assert_eq!(
            test.count_labels_containing("gt_probe_db_dead"),
            1,
            "the database error must appear exactly once, not duplicated",
        );
    }

    /// GT-74, half B. A dead database has to grey out the scan button, not
    /// just add a permanent label above it - otherwise the button still
    /// invites the click that reaches the worker and produces half A's
    /// duplicate. Also pins the gate ordering in `scan_blocked`: the
    /// disclaimer is unaccepted here too (the default test fixture), but the
    /// database is the reason reported, because accepting the disclaimer
    /// would not unblock anything while the database stays dead.
    #[test]
    fn scan_button_is_disabled_and_blames_the_database() {
        let mut test = UiTest::new(show);
        test.app_mut().db_error = Some("gt_probe_db_dead".to_string());
        test.run();
        let s = test.strings();

        test.hover(s.btn_scan_libraries);

        assert!(
            test.has_label(s.disabled_database),
            "hovering the disabled scan button did not blame the database",
        );
    }

    /// Idle: nothing to cancel, so no button.
    #[test]
    fn no_cancel_button_while_idle() {
        let mut test = UiTest::new(show);
        test.run();

        let s = test.strings();
        test.assert_no_label(s.btn_cancel);
        assert!(!test.app().can_cancel());
    }

    /// A scan is the one job `cancel_scan` can actually stop.
    #[test]
    fn cancel_is_offered_during_a_scan() {
        let mut test = UiTest::new(show);
        test.app_mut().begin_job(true);
        test.run();

        let s = test.strings();
        test.assert_label(s.btn_cancel);
    }

    /// The regression this fix is really about: `start_scan` clears
    /// `progress`, and the scan's first phase can run 15-20s before the first
    /// `Progress` message arrives. Gating on `progress.verb` instead of on
    /// the job would hide Cancel for exactly that stretch.
    #[test]
    fn cancel_is_offered_before_the_first_progress_message() {
        let mut test = UiTest::new(show);
        test.app_mut().begin_job(true);
        test.app_mut().progress = None;
        test.run();

        let s = test.strings();
        test.assert_label(s.btn_cancel);
    }

    /// Delete, compaction, clear, import, export: busy, but nothing reads the
    /// cancel token, so the button must not be there to be clicked.
    #[test]
    fn no_cancel_button_for_a_job_that_cannot_be_stopped() {
        for verb in [i18n::Verb::Delete, i18n::Verb::Compact, i18n::Verb::Clear] {
            let mut test = UiTest::new(show);
            test.app_mut().begin_job(false);
            test.app_mut().progress = Some(ProgressState {
                verb,
                current: 1,
                total: 10,
                detail: String::new(),
            });
            test.run();

            let s = test.strings();
            assert!(
                !test.has_label(s.btn_cancel),
                "Cancel was offered during {verb:?}, which cannot be cancelled",
            );
        }
    }

    /// `begin_job`/`end_job` are the only writers of `busy`, which is what
    /// stops the two flags from drifting apart. Pin the pairing.
    #[test]
    fn ending_a_job_clears_both_busy_and_cancellability() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut app = GameTrimmerApp::new_for_test(dir.path());

        app.begin_job(true);
        assert!(app.busy && app.can_cancel());

        app.end_job();
        assert!(!app.busy, "end_job should clear busy");
        assert!(!app.can_cancel(), "an ended job is not cancellable");
    }

    /// A non-cancellable job must never report itself as cancellable just
    /// because something is running.
    #[test]
    fn a_non_cancellable_job_is_busy_but_not_cancellable() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut app = GameTrimmerApp::new_for_test(dir.path());

        app.begin_job(false);

        assert!(app.busy);
        assert!(!app.can_cancel());
    }

    /// The file table is now read *underneath* the classification rather
    /// than before it, so both would be reporting at once. They share one
    /// bar: it counts games (the work that finishes last) and the volume
    /// read is only the detail line. Pinned here because the alternative -
    /// a second verb with a records-read fraction of its own - makes the bar
    /// jump between two unrelated totals for the first half of a scan.
    #[test]
    fn the_file_table_read_shows_as_detail_on_the_game_counting_bar() {
        let mut test = UiTest::new(show);
        let detail = i18n::reading_mft_detail(test.app().lang(), 'D', 42);
        test.app_mut().begin_job(true);
        test.app_mut().apply_message(WorkerMsg::Progress {
            verb: i18n::Verb::Analyze,
            current: 7,
            total: 1603,
            detail,
        });
        test.run();

        // The counter is games, not records: 7 of 1603, while the detail
        // says what the disk is doing.
        test.assert_label_containing("7/1603");
        test.assert_label_containing("42%");
    }

    /// The same bar, same verb, a moment later - the only thing that changes
    /// when the reading finishes is the detail line. A verb switch here
    /// would relabel a bar mid-scan for no change in what it counts.
    #[test]
    fn a_classified_game_keeps_the_same_bar_the_volume_read_used() {
        let mut test = UiTest::new(show);
        test.app_mut().begin_job(true);
        test.app_mut().apply_message(WorkerMsg::Progress {
            verb: i18n::Verb::Analyze,
            current: 7,
            total: 1603,
            detail: "Test Game".to_string(),
        });
        test.run();

        let verb = i18n::verb_label(test.app().lang(), i18n::Verb::Analyze);
        test.assert_label_containing(&format!("{verb} 7/1603: Test Game"));
    }

    // At 8 fps each frame lasts 0.125s; sample the middle of each slot.
    fn frame_at_slot(slot: i64) -> char {
        spinner_frame(slot as f64 * 0.125 + 0.06)
    }

    #[test]
    fn spinner_cycles_through_all_four_frames_in_order() {
        let seq: Vec<char> = (0..4).map(frame_at_slot).collect();
        assert_eq!(seq, vec!['|', '/', '-', '\\']);
    }

    #[test]
    fn spinner_wraps_around_after_four_frames() {
        // The fifth slot must land back on the first frame (cycle length 4).
        assert_eq!(frame_at_slot(4), frame_at_slot(0));
        assert_eq!(frame_at_slot(5), frame_at_slot(1));
    }

    #[test]
    fn spinner_frames_are_always_ascii() {
        // Non-ASCII glyphs risk rendering as tofu in the bundled font; guard it.
        for i in 0..64 {
            let c = spinner_frame(i as f64 * 0.037);
            assert!(c.is_ascii(), "frame {c:?} at step {i} is not ASCII");
        }
    }
}
