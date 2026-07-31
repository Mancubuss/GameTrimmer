//! "Rules": the two analysis packs the scanner loads, each with a live
//! validity readout, plus the existing export/import pair.
//!
//! The validity line is the point of the section. A hand-edited
//! `rules.json` that no longer parses used to fail silently at scan time -
//! the app simply ran with fewer rules and said nothing (audit §6.6). Here
//! the state of each pack is visible before a scan is started, and the
//! per-pack restore is the way back out, which previously required knowing
//! where the file lived and deleting it.

use eframe::egui;

use gametrimmer_core::packs::PackKind;

use crate::app::GameTrimmerApp;
use crate::i18n;
use crate::ui::row_actions;
use crate::worker::{self, rules_io};

use super::SUCCESS_GREEN;

/// The packs, in the order the section lists them, with the file each one
/// lives in - the file name is what makes the two restore buttons tell
/// themselves apart, on screen and to a screen reader.
fn packs(s: &i18n::Strings) -> [(PackKind, &'static str, &'static str); 2] {
    [
        (
            PackKind::CategoryRules,
            s.rules_pack_category_label,
            worker::RULES_FILE_NAME,
        ),
        (
            PackKind::LangPack,
            s.rules_pack_lang_label,
            worker::L10N_RULES_FILE_NAME,
        ),
    ]
}

/// Label of the per-pack restore button. Carries the file name because both
/// packs offer the same action: two buttons reading only "Restore defaults"
/// would be indistinguishable to anyone not tracking which block they are in.
fn restore_label(s: &i18n::Strings, file_name: &str) -> String {
    format!("{} \u{2014} {file_name}", s.btn_restore_defaults)
}

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let s = i18n::strings(app.lang());
    let mut export_clicked = false;
    let mut import_clicked = false;
    let mut restore_kind: Option<PackKind> = None;

    // Both packs are rewritten by an import and read by an export, so every
    // action here is gated on the same pair of flags.
    let idle = !app.busy && !app.rules_io_active;

    for (kind, label, file_name) in packs(s) {
        show_pack(ui, s, kind, label);
        if ui
            .add_enabled(idle, egui::Button::new(restore_label(s, file_name)))
            .clicked()
        {
            restore_kind = Some(kind);
        }
        ui.add_space(10.0);
    }

    ui.separator();
    ui.add_space(8.0);

    ui.label(s.rules_label);
    ui.add_space(4.0);
    ui.add_enabled_ui(idle, |ui| {
        ui.horizontal(|ui| {
            if ui.button(s.btn_export_rules).clicked() {
                export_clicked = true;
            }
            if ui.button(s.btn_import_rules).clicked() {
                import_clicked = true;
            }
        });
    });
    ui.small(s.rules_hint);

    show_io_progress(app, ui, s);

    if export_clicked {
        app.start_rules_export();
    }
    if import_clicked {
        app.start_rules_import();
    }
    if let Some(kind) = restore_kind {
        app.restore_rules_builtin(kind);
    }
}

/// One pack: its name, whether it currently parses, and where it lives.
fn show_pack(ui: &mut egui::Ui, s: &i18n::Strings, kind: PackKind, label: &str) {
    ui.horizontal(|ui| {
        ui.strong(label);
        if rules_io::pack_is_valid(kind) {
            ui.colored_label(SUCCESS_GREEN, s.rules_valid_label);
        } else {
            ui.colored_label(ui.visuals().error_fg_color, s.rules_invalid_label);
        }
    });
    // Where to look when the answer is "does not parse" - the same reason
    // the database path is shown in "Data & diagnostics".
    if let Ok(path) = rules_io::pack_path(kind) {
        ui.small(row_actions::windows_path_string(&path));
    }
}

/// Running state and outcome of an export, import or restore. The top-bar
/// status line is behind this modal, so without this the result of an
/// import - including the added/updated counts - would never be seen.
fn show_io_progress(app: &GameTrimmerApp, ui: &mut egui::Ui, s: &i18n::Strings) {
    if app.rules_io_active {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(s.running_ellipsis);
        });
    } else if let Some(result) = &app.rules_io_result {
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

    fn open_rules() -> UiTest {
        let mut test = UiTest::new(crate::ui::settings::show);
        test.app_mut().show_settings = true;
        test.app_mut().settings_section = SettingsSection::Rules;
        test.run();
        test
    }

    #[test]
    fn the_section_lists_both_packs_and_the_import_export_pair() {
        let test = open_rules();
        let s = test.strings();

        test.assert_label(s.rules_pack_category_label);
        test.assert_label(s.rules_pack_lang_label);
        test.assert_label(s.btn_export_rules);
        test.assert_label(s.btn_import_rules);
    }

    /// Two packs, two restore buttons - and they have to be distinguishable.
    /// A shared "Restore defaults" label would be ambiguous on screen and to
    /// a screen reader alike.
    #[test]
    fn each_pack_has_its_own_named_restore_button() {
        let test = open_rules();
        let s = test.strings();

        for (_, _, file_name) in packs(s) {
            let label = restore_label(s, file_name);
            assert_eq!(
                test.count_labels(&label),
                1,
                "expected exactly one button labelled {label:?}",
            );
        }
    }

    /// The section's reason to exist: each pack says whether it parses. The
    /// packs materialize from the embedded defaults next to the test binary,
    /// so on a clean run both are valid.
    #[test]
    fn every_pack_reports_whether_it_parses() {
        let test = open_rules();
        let s = test.strings();

        assert_eq!(
            test.count_labels(s.rules_valid_label) + test.count_labels(s.rules_invalid_label),
            2,
            "each of the two packs must carry exactly one validity readout",
        );
    }

    #[test]
    fn a_running_job_disables_every_action() {
        let mut test = open_rules();
        let s = test.strings();
        test.app_mut().begin_job(false);
        test.run();

        let (_, _, file_name) = packs(s)[0];
        test.click(&restore_label(s, file_name));

        assert!(
            test.app().rules_io_result.is_none(),
            "a restore ran while a job was in flight",
        );
    }

    /// The outcome has nowhere else to appear while the modal is up.
    #[test]
    fn an_import_result_is_reported_inside_the_dialog() {
        let mut test = open_rules();
        test.app_mut().rules_io_result = Some(Ok("rules imported: 2 new".to_string()));
        test.run();
        test.assert_label("rules imported: 2 new");

        test.app_mut().rules_io_result = Some(Err("l10n_rules.json: broken".to_string()));
        test.run();
        test.assert_label("l10n_rules.json: broken");
    }
}
