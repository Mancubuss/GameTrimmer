//! "Data & diagnostics": the database file - where it is, how big, and the
//! two ways to maintain it - plus the diagnostic-logging toggle.
//!
//! The database path is shown because it used to
//! be findable only by guessing where the exe lives, making "attach your
//! database" an unanswerable request in a bug report.
//!
//! # Why "Clear database" is not in a danger frame
//!
//! It was, and the frame was wrong (scan-data reset semantics). `clear_scan_data` wipes
//! findings, files, games and the operations journal and keeps libraries and
//! settings; of those, only the journal does not come back from a rescan -
//! and nothing reads it. Every `SELECT ... FROM operations` in the repository
//! is inside `#[cfg(test)]`, and restoring a deleted file goes through the
//! Recycle Bin itself (`trash::os_limited::list`), not through the journal.
//! So the price of the button is time, and time is not what red is for: red
//! has to mean "you can lose something you will not get back", or it means
//! nothing.
//!
//! That distinction stopped being academic with protected-language editing, which puts a red frame
//! around dropping English from the keep-list. A frame that cries wolf next
//! to it would devalue the one warning in this release that has to be
//! believed. The modal confirmation stays - the click is still worth a
//! deliberate second step - and only the frame goes.
//!
//! If an operations-history screen ever ships, the journal stops being dead
//! and the frame comes back, earned. The point is that it should not already
//! be spent by then.

use eframe::egui;

use crate::app::GameTrimmerApp;
use crate::i18n;
use crate::model::format_size;
use crate::ui::row_actions;

use super::SUCCESS_GREEN;

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let lang = app.lang();
    let s = i18n::strings(lang);
    let mut compact_clicked = false;
    let mut clear_clicked = false;
    let mut picked_logging_enabled = app.settings.logging_enabled;

    ui.strong(s.database_label);
    ui.add_space(4.0);
    ui.label(s.db_path_label);
    show_database_file(app, ui, s, lang);

    ui.add_space(8.0);
    if ui
        .add_enabled(!app.busy, egui::Button::new(s.btn_compact_database))
        .clicked()
    {
        compact_clicked = true;
    }
    ui.small(s.compact_hint);

    // Siblings, not a maintenance job next to a catastrophe: both act on the
    // same database file and both cost only time - see the module docs.
    ui.add_space(8.0);
    if ui
        .add_enabled(!app.busy, egui::Button::new(s.btn_clear_database))
        .clicked()
    {
        clear_clicked = true;
    }
    ui.small(s.clear_hint);

    show_maintenance_progress(app, ui, s);

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);

    super::row_heading(ui, s.logging_label, s.badge_immediately);
    ui.checkbox(&mut picked_logging_enabled, s.logging_checkbox);
    ui.small(s.logging_hint);

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);
    let bundle_clicked = show_bundle(app, ui, s);

    if compact_clicked {
        app.start_compact();
    }
    if clear_clicked {
        app.request_clear_database_confirmation();
    }
    if picked_logging_enabled != app.settings.logging_enabled {
        app.set_logging_enabled(picked_logging_enabled);
    }
    if bundle_clicked {
        app.start_bundle();
    }
}

/// The diagnostic bundle: two opt-ins, the preview, and the button.
///
/// The preview is the point of the whole layout. This section exists
/// because "attach your database" used to be an unanswerable request (see
/// the module docs), and answering it with a file the user cannot see
/// inside would trade one opaque artifact for another. So the actual
/// generated `summary.txt` is rendered here, above the button, and the
/// toggles regenerate it - what the user reads is what gets written,
/// rather than a checkbox list describing it.
///
/// Returns whether the generate button was clicked, following this
/// module's rule that a button never calls into the app during the render
/// pass.
fn show_bundle(app: &mut GameTrimmerApp, ui: &mut egui::Ui, s: &i18n::Strings) -> bool {
    super::row_heading(ui, s.bundle_label, s.badge_immediately);
    ui.small(s.bundle_hint);
    ui.add_space(6.0);

    let mut options = app.bundle_options;
    ui.checkbox(&mut options.include_game_titles, s.bundle_titles_checkbox);
    ui.checkbox(
        &mut options.include_operations_detail,
        s.bundle_operations_checkbox,
    );
    if options != app.bundle_options {
        app.bundle_options = options;
    }
    // After applying the toggles, so the preview below is always the one
    // for the options currently shown rather than the previous frame's.
    app.refresh_bundle_preview();

    ui.add_space(8.0);
    ui.label(s.bundle_preview_label);
    if let Some((_, preview)) = &app.bundle_preview {
        // Bounded height and *wrapped* text, never extended: a monospace
        // line long enough to widen the pane would widen the dialog, and
        // `the_modal_does_not_move_or_resize_between_sections` is the test
        // that catches exactly that - the settings dialog must not jump as
        // the user moves between sections.
        egui::ScrollArea::vertical()
            .max_height(PREVIEW_HEIGHT)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(preview).monospace())
                        .wrap_mode(egui::TextWrapMode::Wrap),
                );
            });
    }

    ui.add_space(8.0);
    let clicked = ui
        .add_enabled(!app.busy, egui::Button::new(s.btn_generate_bundle))
        .clicked();

    if app.bundle_active {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(s.running_ellipsis);
        });
    } else if let Some(result) = &app.bundle_result {
        ui.add_space(4.0);
        match result {
            Ok(message) => {
                ui.colored_label(SUCCESS_GREEN, message);
            }
            Err(message) => {
                ui.colored_label(ui.visuals().error_fg_color, message);
            }
        }
    }

    clicked
}

/// Tall enough to show the summary's identity and counts without scrolling
/// (the part a user actually reads before deciding) while keeping the
/// button on screen at the standard window size.
const PREVIEW_HEIGHT: f32 = 180.0;

/// The database path, its size, and the two ways to act on it.
///
/// Nothing renders when the path could not be resolved - the dialog would be
/// offering to copy and open nothing. That case already has a visible error:
/// `db_error` is reported on the main window's status line.
fn show_database_file(
    app: &GameTrimmerApp,
    ui: &mut egui::Ui,
    s: &i18n::Strings,
    lang: i18n::Lang,
) {
    let Some(path) = app.db_path() else {
        return;
    };
    let display_path = row_actions::windows_path_string(path);

    // Truncated rather than wrapped: a deep path would otherwise push the
    // buttons below it out of the viewport, and the full text is one click
    // away on "Copy".
    ui.add(egui::Label::new(&display_path).truncate());

    ui.horizontal(|ui| {
        if ui.button(s.btn_copy).clicked() {
            ui.ctx().copy_text(display_path.clone());
        }
        if ui.button(s.btn_open_folder).clicked() {
            if let Some(parent) = path.parent() {
                let (program, args) = row_actions::open_folder_args(parent);
                if let Err(err) = row_actions::launch(program, &args) {
                    crate::logger::log(&format!("Failed to open Explorer: {err}"));
                }
            }
        }
        // Read straight from the filesystem rather than cached: compaction
        // changes it, and a stale number here is what makes a user run
        // "Compact" twice wondering whether it did anything.
        if let Ok(meta) = std::fs::metadata(path) {
            ui.add_space(8.0);
            ui.label(format_size(lang, meta.len()));
        }
    });
}

/// Running state and outcome of a compact/clear job.
///
/// The top-bar status line and progress bar are behind this modal, so
/// without this the job finishes invisibly and the user clicks again.
fn show_maintenance_progress(app: &GameTrimmerApp, ui: &mut egui::Ui, s: &i18n::Strings) {
    if app.db_maint_active {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(s.running_ellipsis);
        });
    } else if let Some(result) = &app.db_maint_result {
        ui.add_space(4.0);
        match result {
            Ok(message) => {
                ui.colored_label(SUCCESS_GREEN, message);
            }
            Err(message) => {
                ui.colored_label(ui.visuals().error_fg_color, message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ui::harness::UiTest;
    use crate::ui::settings::SettingsSection;

    fn open_data() -> UiTest {
        let mut test = UiTest::new(crate::ui::settings::show);
        test.app_mut().show_settings = true;
        test.app_mut().settings_section = SettingsSection::Data;
        test.run();
        test
    }

    /// The bundle lives in this section rather than in a panel of its own,
    /// because this section exists precisely because "attach your database"
    /// was an unanswerable request - and the bundle is the real answer to it.
    #[test]
    fn the_bundle_action_lives_beside_the_database_path() {
        let test = open_data();
        let s = test.strings();

        test.assert_label(s.bundle_label);
        test.assert_label(s.btn_generate_bundle);
        test.assert_label(s.bundle_titles_checkbox);
        test.assert_label(s.bundle_operations_checkbox);
        // Same section, not a new one: the database path is still here.
        test.assert_label(s.db_path_label);
    }

    /// The rule the whole layout is built around: the user sees the actual
    /// generated text before any byte is written, not a checkbox list
    /// describing it.
    #[test]
    fn the_preview_shows_the_real_summary_before_anything_is_written() {
        let test = open_data();

        let (_, preview) = test
            .app()
            .bundle_preview
            .as_ref()
            .expect("the preview renders as soon as the section opens");
        assert!(
            preview.contains("GameTrimmer diagnostic bundle"),
            "{preview}"
        );
        assert!(preview.contains("Privacy"), "{preview}");
        // The preview is rendered, so its heading is on screen too.
        test.assert_label(test.strings().bundle_preview_label);
    }

    /// A toggle has to change the preview, or it is not a preview of what
    /// will be written. Asserted through the rendered text rather than the
    /// flag, since the flag agreeing with itself proves nothing.
    #[test]
    fn opting_in_to_game_titles_changes_what_the_preview_promises() {
        let mut test = open_data();
        let s = test.strings();

        let before = test.app().bundle_preview.clone().expect("preview").1;
        assert!(before.contains("Game 1, Game 2"), "{before}");

        test.click(s.bundle_titles_checkbox);

        let after = test.app().bundle_preview.clone().expect("preview").1;
        assert_ne!(before, after, "the preview must follow the toggle");
        assert!(after.contains("INCLUDED because you chose to"), "{after}");
    }

    /// Both opt-ins default to off. They are the two sections that carry
    /// the most about the user rather than about the program, so the
    /// default has to be the quiet one.
    #[test]
    fn both_opt_ins_start_switched_off() {
        let test = open_data();

        assert_eq!(
            test.app().bundle_options,
            gametrimmer_core::bundle::BundleOptions::default(),
        );
        assert!(!test.app().bundle_options.include_game_titles);
        assert!(!test.app().bundle_options.include_operations_detail);
    }

    /// The button is a save action, not a send action, and the hint has to
    /// say so - the one thing a privacy-sensitive user checks first.
    #[test]
    fn the_hint_states_that_nothing_is_transmitted() {
        let test = open_data();

        test.assert_label(test.strings().bundle_hint);
        assert!(
            test.strings().bundle_hint.contains("no network")
                || test.strings().bundle_hint.contains("немає мережевого"),
            "the hint must state that nothing leaves the machine",
        );
    }

    #[test]
    fn the_section_shows_the_database_file_and_what_can_be_done_with_it() {
        let test = open_data();
        let s = test.strings();

        test.assert_label(s.db_path_label);
        test.assert_label(s.btn_copy);
        test.assert_label(s.btn_open_folder);
        test.assert_label(s.btn_compact_database);
    }

    /// The path itself, not just its label - a bug report needs the string,
    /// and the harness app's database lives in its own temp directory.
    #[test]
    fn the_actual_path_is_on_screen() {
        let test = open_data();
        let path = test.app().db_path().expect("test app has a database path");
        let shown = row_actions::windows_path_string(path);

        test.assert_label(&shown);
    }

    /// scan-data reset semantics, and the reverse of what this section asserted before it: the
    /// clear button belongs with "Compact", in ordinary maintenance, because
    /// what it costs is a rescan and nothing else. Red is spent on the one
    /// warning in this release that has to be believed - the English block in
    /// "Scanning" - and a frame that cries wolf beside it devalues it.
    #[test]
    fn clearing_the_database_is_ordinary_maintenance_not_a_danger_zone() {
        let test = open_data();
        let s = test.strings();

        test.assert_no_label(s.danger_zone_label);

        // In the database block with its sibling, above the section's next
        // heading - not parked at the bottom in a frame of its own.
        let compact = test.rect_of(s.btn_compact_database);
        let clear = test.rect_of(s.btn_clear_database);
        let next_block = test.rect_of(s.logging_label);
        assert!(
            clear.min.y > compact.min.y && clear.max.y < next_block.min.y,
            "the wipe ({clear:?}) is not between Compact ({compact:?}) and \
             the logging row ({next_block:?})",
        );
    }

    /// The hint used to promise permanence the button does not deliver: the
    /// only thing it removes that a rescan cannot rebuild is the operations
    /// journal, which nothing reads. Saying "permanently" there is the same
    /// overstatement as the frame was.
    #[test]
    fn the_hint_prices_the_wipe_in_time_rather_than_in_loss() {
        for lang in [i18n::Lang::En, i18n::Lang::Uk] {
            let hint = i18n::strings(lang).clear_hint.to_lowercase();
            for overstatement in ["permanent", "безповоротн"] {
                assert!(
                    !hint.contains(overstatement),
                    "{lang:?} clear_hint still claims {overstatement:?}: {hint:?}",
                );
            }
        }
    }

    /// Clicking never wipes anything on its own - the confirmation modal is
    /// the only path to the destructive worker.
    #[test]
    fn clearing_the_database_only_opens_the_confirmation() {
        let mut test = open_data();
        let s = test.strings();
        assert!(!test.app().confirm_clear_database);

        test.click(s.btn_clear_database);

        assert!(test.app().confirm_clear_database);
        assert!(
            !test.app().db_maint_active,
            "the click started the wipe instead of asking",
        );
    }

    /// Both database actions open their own connection, so neither may fire
    /// while a worker is holding one.
    #[test]
    fn a_running_job_disables_both_database_actions() {
        let mut test = open_data();
        let s = test.strings();
        test.app_mut().begin_job(false);
        test.run();

        test.click(s.btn_compact_database);
        test.click(s.btn_clear_database);

        assert!(!test.app().confirm_clear_database);
    }

    /// The status line that would normally report this is behind the modal.
    #[test]
    fn a_finished_job_reports_its_outcome_inside_the_dialog() {
        let mut test = open_data();
        test.app_mut().db_maint_result = Some(Ok("compacted: 4 MB freed".to_string()));
        test.run();
        test.assert_label("compacted: 4 MB freed");

        test.app_mut().db_maint_result = Some(Err("could not open the database".to_string()));
        test.run();
        test.assert_label("could not open the database");
    }

    #[test]
    fn a_running_job_is_visible_inside_the_dialog() {
        let mut test = open_data();
        let s = test.strings();
        test.app_mut().db_maint_active = true;
        // The spinner repaints every frame on purpose - see `run_animated`.
        test.run_animated();

        test.assert_label(s.running_ellipsis);
    }

    #[test]
    fn the_logging_toggle_applies_immediately() {
        let mut test = open_data();
        let s = test.strings();
        let before = test.app().settings.logging_enabled;

        test.click(s.logging_checkbox);

        assert_eq!(test.app().settings.logging_enabled, !before);
    }
}
