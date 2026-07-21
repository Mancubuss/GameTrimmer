//! Top panel: scan/export/settings buttons and scan progress/cancel.

use eframe::egui;

use crate::app::{GameTrimmerApp, APP_TITLE};
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

#[cfg(test)]
mod tests {
    use super::spinner_frame;

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
