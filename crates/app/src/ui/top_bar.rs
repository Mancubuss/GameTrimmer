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

        // The startup "what came back" banner (GT-09,
        // `WorkerMsg::ReturnedSinceLastScan`). One line inside the panel
        // that is already here, not a new panel - the owner's standing rule
        // is that this window is already crowded, and a fourth panel for one
        // dismissible sentence would be exactly the kind of feature this UI
        // cannot keep absorbing. Silent (no line at all) whenever nothing
        // changed or the banner has been dismissed this session - a user who
        // has nothing to be told about must see nothing, not an empty or
        // zeroed-out line that reads as broken detection.
        if !app.returned_banner_dismissed && !app.returned_games.is_empty() {
            let bytes: u64 = app.returned_games.iter().map(|game| game.bytes).sum();
            let text = i18n::returned_games_banner(lang, app.returned_games.len(), bytes);
            ui.horizontal(|ui| {
                ui.label(text);
                if ui.button("\u{2715}").on_hover_text(s.btn_close).clicked() {
                    app.dismiss_returned_games_banner();
                }
            });
        }

        if let Some(progress) = app.progress.clone() {
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

    /// GT-389. The three-bar panel used to feed one game-start message and
    /// one game-finish message into the same `overall_fraction`, one mapped
    /// to 0-50% and the other to 50-100% - and since the pool classifies
    /// games in parallel, the two interleaved and the combined bar visibly
    /// jumped backward and forward between the two ranges. There is one
    /// producer now: the bar's fraction is `current / total` off the same
    /// monotonic game counter every time, so a later update replaces the
    /// earlier one instead of sitting alongside it. Nothing clamps the
    /// value: what removed the jumping was removing the second producer, so
    /// this pins the single counter reaching the bar rather than a guard
    /// against going backwards.
    #[test]
    fn the_scan_bar_reads_one_counter_and_each_update_replaces_the_last() {
        let mut test = UiTest::new(show);
        test.app_mut().begin_job(true);
        test.app_mut().apply_message(WorkerMsg::Progress {
            verb: i18n::Verb::Analyze,
            current: 40,
            total: 100,
            detail: "Game A".to_string(),
        });
        test.run();
        test.assert_label_containing("40/100");

        test.app_mut().apply_message(WorkerMsg::Progress {
            verb: i18n::Verb::Analyze,
            current: 41,
            total: 100,
            detail: "Game B".to_string(),
        });
        test.run();
        test.assert_label_containing("41/100");
        test.assert_no_label_containing("40/100");
    }

    /// GT-389, state 1. Library discovery has no per-item denominator yet -
    /// `WorkerMsg::Status` is what a scan sends for it, and its handler
    /// clears `progress` so the bar has nothing to divide by. Before this
    /// fix that was moot: the three-bar panel primed itself on the very
    /// first game and never let this branch run again for the rest of the
    /// scan. Now it renders through the plain status branch: a spinner next
    /// to the status text, with no numbered progress bar underneath it.
    #[test]
    fn discovery_renders_as_an_indeterminate_spinner() {
        let mut test = UiTest::new(show);
        test.app_mut().begin_job(true);
        let text = i18n::strings(test.app().lang())
            .detecting_libraries
            .to_string();
        test.app_mut().apply_message(WorkerMsg::Status { text });
        // Busy + a status message draws `ui.spinner()`, which asks for a
        // repaint every frame by design - `run` would report that as a
        // repaint loop, so this state settles through `run_animated` instead.
        test.run_animated();

        let s = test.strings();
        test.assert_label(s.detecting_libraries);
        // No game/file counter exists yet during discovery.
        test.assert_no_label_containing("/");
    }

    /// GT-389, state 3. Once the writer joins, orphan/janitor detection,
    /// generation activation, the WAL checkpoint and the summary/occupancy
    /// queries all run before `Done` - measured at ~3s in a real scan - and
    /// used to leave the last analysis bar frozen on screen for that whole
    /// stretch, which reads as a hang the moment the counter stops moving.
    /// `run_scan` now sends a `Status` here too, the same way it already
    /// does for discovery; its handler clears the stale bar so only the
    /// tail's own caption is left.
    #[test]
    fn the_housekeeping_tail_shows_its_own_caption() {
        let mut test = UiTest::new(show);
        test.app_mut().begin_job(true);
        test.app_mut().apply_message(WorkerMsg::Progress {
            verb: i18n::Verb::Analyze,
            current: 100,
            total: 100,
            detail: "Last Game".to_string(),
        });
        let text = i18n::strings(test.app().lang()).finishing_scan.to_string();
        test.app_mut().apply_message(WorkerMsg::Status { text });
        // Same spinner as the discovery state - see the comment in
        // `discovery_renders_as_an_indeterminate_spinner`.
        test.run_animated();

        let s = test.strings();
        test.assert_label(s.finishing_scan);
        test.assert_no_label_containing("Last Game");
    }

    /// GT-389. The anti-freeze spinner (below) used to be unreachable during
    /// a scan at all: the three-bar panel's `else` branch, the only place it
    /// lives, never ran again once the panel primed itself on the first
    /// game. Pin the detail as already-seen far enough in the past that any
    /// plausible animation-clock "now" clears the 1s stall threshold, and
    /// check that the previously-exact line grows a trailing glyph.
    #[test]
    fn a_stalled_detail_shows_the_anti_freeze_spinner() {
        let mut test = UiTest::new(show);
        let verb = i18n::verb_label(test.app().lang(), i18n::Verb::Analyze);
        let base_text = format!("{verb} 41/42: ARK: Survival Evolved");

        test.app_mut().begin_job(true);
        test.app_mut().apply_message(WorkerMsg::Progress {
            verb: i18n::Verb::Analyze,
            current: 41,
            total: 42,
            detail: "ARK: Survival Evolved".to_string(),
        });
        test.run();
        // Fresh detail, not yet stalled: the line is exactly the base text.
        test.assert_label(&base_text);

        test.app_mut().last_progress_detail = "ARK: Survival Evolved".to_string();
        test.app_mut().last_progress_detail_at = -1_000.0;
        test.run();

        // Stalled: a spinner glyph is appended, so the line is no longer
        // exactly the base text, though it still starts with it.
        test.assert_no_label(&base_text);
        test.assert_label_containing(&base_text);
    }

    /// GT-09. A game whose build id moved *and* whose deletion is back
    /// (non-zero bytes) must state both the count and the size, formatted
    /// through `i18n::returned_games_banner` - looked up via that function
    /// rather than a literal string, per this module's own doc comment on
    /// why (renames must not silently stop being tested).
    #[test]
    fn the_returned_games_banner_shows_the_count_and_size_when_something_came_back() {
        let mut test = UiTest::new(show);
        let bytes = 3 * 1024 * 1024 * 1024;
        test.app_mut()
            .apply_message(WorkerMsg::ReturnedSinceLastScan {
                games: vec![gametrimmer_core::gamestate::ReturnedGame {
                    game_id: 1,
                    name: "Test Game".to_string(),
                    files: 4,
                    bytes,
                }],
            });
        test.run();

        let expected = i18n::returned_games_banner(test.app().lang(), 1, bytes);
        test.assert_label_containing(&expected);
    }

    /// The common case (and the owner's own database today): games updated
    /// but nothing a previous trim deleted is back, because nothing was ever
    /// deleted. "0 B is back" would read as a bug, so the line must state
    /// only the count.
    #[test]
    fn the_returned_games_banner_omits_the_size_at_zero_bytes() {
        let mut test = UiTest::new(show);
        test.app_mut()
            .apply_message(WorkerMsg::ReturnedSinceLastScan {
                games: vec![gametrimmer_core::gamestate::ReturnedGame {
                    game_id: 1,
                    name: "Test Game".to_string(),
                    files: 0,
                    bytes: 0,
                }],
            });
        test.run();

        // Exact, not "containing": the non-zero wording *starts with* the
        // zero wording, so a `contains` check would pass even if the size
        // clause were appended.
        //
        // What this pins is the wiring - that the bar draws exactly what the
        // message function returns for this input - not the wording, because
        // both sides read the same function and move together when it is
        // edited. The wording is pinned separately, and deliberately, by
        // `i18n::messages`'
        // `returned_games_banner_drops_the_size_clause_at_zero_bytes`; break
        // the zero-bytes branch and that one goes red while this one stays
        // green, which is the division of labour intended here.
        let expected = i18n::returned_games_banner(test.app().lang(), 1, 0);
        test.assert_label(&expected);
    }

    /// Silence is the correct normal state: an empty list means the check
    /// ran and found nothing changed, which must not print an empty or
    /// zeroed-out line - that would read as broken detection, not as "all
    /// clear".
    #[test]
    fn the_returned_games_banner_is_silent_when_nothing_changed() {
        let mut test = UiTest::new(show);
        test.app_mut()
            .apply_message(WorkerMsg::ReturnedSinceLastScan { games: Vec::new() });
        test.run();

        // Asserted against what the banner *would* say for an empty list, not
        // against a literal phrase: a reworded sentence must break this test
        // rather than make it pass by accident.
        let would_say = i18n::returned_games_banner(test.app().lang(), 0, 0);
        test.assert_no_label(&would_say);
    }

    /// Dismissing hides the banner for the rest of the session without
    /// discarding the data behind it - only its visibility changes (see
    /// `GameTrimmerApp::dismiss_returned_games_banner`).
    #[test]
    fn dismissing_the_returned_games_banner_hides_it_but_keeps_the_data() {
        let mut test = UiTest::new(show);
        test.app_mut()
            .apply_message(WorkerMsg::ReturnedSinceLastScan {
                games: vec![gametrimmer_core::gamestate::ReturnedGame {
                    game_id: 1,
                    name: "Test Game".to_string(),
                    files: 1,
                    bytes: 1024,
                }],
            });
        test.run();
        let expected = i18n::returned_games_banner(test.app().lang(), 1, 1024);
        test.assert_label_containing(&expected);

        test.click("\u{2715}");

        test.assert_no_label(&expected);
        assert_eq!(
            test.app().returned_games.len(),
            1,
            "dismissing the banner must not discard what it reported",
        );
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
