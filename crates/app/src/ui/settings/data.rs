//! "Data & diagnostics": the database file - where it is, how big, and the
//! two ways to maintain it - plus the diagnostic-logging toggle.
//!
//! The database path is shown at all, which is the audit's §6.5: it used to
//! be findable only by guessing where the exe lives, making "attach your
//! database" an unanswerable request in a bug report.
//!
//! # Why "Clear database" is not in a danger frame
//!
//! It was, and the frame was wrong (GT-60). `clear_scan_data` wipes
//! findings, files, games and the operations journal and keeps libraries and
//! settings; of those, only the journal does not come back from a rescan -
//! and nothing reads it. Every `SELECT ... FROM operations` in the repository
//! is inside `#[cfg(test)]`, and restoring a deleted file goes through the
//! Recycle Bin itself (`trash::os_limited::list`), not through the journal.
//! So the price of the button is time, and time is not what red is for: red
//! has to mean "you can lose something you will not get back", or it means
//! nothing.
//!
//! That distinction stopped being academic with GT-59, which puts a red frame
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

    /// GT-60, and the reverse of what this section asserted before it: the
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
