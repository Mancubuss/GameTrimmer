//! "Data & diagnostics": the database file (where it is, how big, compaction)
//! and the danger zone, plus the diagnostic-logging toggle.
//!
//! Two changes over the old dialog, both from the audit:
//!
//! * The database path is shown at all. It used to be findable only by
//!   guessing where the exe lives, which made "attach your database" an
//!   unanswerable request in a bug report (§6.5).
//! * "Clear database" is inside a red frame under its own heading instead of
//!   sitting inline beside "Compact database". The two were one tab-stop
//!   apart and looked identical, though one is a recoverable maintenance job
//!   and the other wipes every scan result on the machine.

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
    show_maintenance_progress(app, ui, s);

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);

    super::row_heading(ui, s.logging_label, s.badge_immediately);
    ui.checkbox(&mut picked_logging_enabled, s.logging_checkbox);
    ui.small(s.logging_hint);

    ui.add_space(16.0);
    show_danger_zone(app, ui, s, &mut clear_clicked);

    if compact_clicked {
        app.start_compact();
    }
    if clear_clicked {
        app.request_clear_database_confirmation();
    }
    if picked_logging_enabled != app.settings.logging_enabled {
        app.set_logging_enabled(picked_logging_enabled);
    }
}

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

/// The one place in Settings that uses red - see the module docs. The click
/// still only opens the confirmation modal; nothing is destroyed from here.
fn show_danger_zone(
    app: &GameTrimmerApp,
    ui: &mut egui::Ui,
    s: &i18n::Strings,
    clear_clicked: &mut bool,
) {
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.5, ui.visuals().error_fg_color))
        .inner_margin(egui::Margin::same(10))
        .corner_radius(4)
        .show(ui, |ui| {
            ui.colored_label(ui.visuals().error_fg_color, s.danger_zone_label);
            ui.add_space(6.0);
            if ui
                .add_enabled(!app.busy, egui::Button::new(s.btn_clear_database))
                .clicked()
            {
                *clear_clicked = true;
            }
            ui.small(s.clear_hint);
        });
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

    /// The audit's §6.5 complaint: the wipe used to be a plain button beside
    /// a recoverable one. It must be inside the labelled danger zone.
    #[test]
    fn the_wipe_sits_under_its_own_danger_heading() {
        let test = open_data();
        let s = test.strings();

        test.assert_label(s.danger_zone_label);

        let heading = test.rect_of(s.danger_zone_label);
        let button = test.rect_of(s.btn_clear_database);
        assert!(
            button.min.y >= heading.min.y,
            "the wipe button ({button:?}) is not under its heading ({heading:?})",
        );
        // And not beside the recoverable action it used to be confused with.
        let compact = test.rect_of(s.btn_compact_database);
        assert!(
            button.min.y > compact.max.y,
            "the wipe still shares a row with Compact: {button:?} vs {compact:?}",
        );
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
