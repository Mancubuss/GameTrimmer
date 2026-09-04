//! "Selection & deletion": two independent switches - how a delete disposes
//! of files (`delete_method`) and when the confirmation is shown
//! (`confirm_behavior`).
//!
//! Neither affects the other, and neither touches the findings currently on
//! screen. What a fresh scan pre-selects used to be a third switch here;
//! GT-89 removed it along with selection profiles, because a scan now
//! pre-selects nothing at all and there is no policy left to configure.

use eframe::egui;

use gametrimmer_core::settings::{ConfirmBehavior, DeleteMethod};

use crate::app::GameTrimmerApp;
use crate::i18n;

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let s = i18n::strings(app.lang());
    let mut picked_method = app.settings.delete_method;
    let mut picked_confirm = app.settings.confirm_behavior;

    // Every control here persists through a fresh database connection, so
    // all three groups are gated the same way: writing one underneath a
    // running `VACUUM` (which needs exclusive access) or delete job would
    // race that worker's own connection.
    super::row_heading(ui, s.delete_method_label, s.badge_next_delete);
    ui.add_enabled_ui(!app.busy, |ui| {
        ui.radio_value(
            &mut picked_method,
            DeleteMethod::Permanent,
            s.delete_method_permanent_label,
        );
        ui.indent("gt_settings_permanent_hint", |ui| {
            ui.small(s.delete_method_permanent_hint);
        });
        ui.add_space(4.0);
        ui.radio_value(
            &mut picked_method,
            DeleteMethod::RecycleBin,
            s.delete_method_recycle_label,
        );
        ui.indent("gt_settings_recycle_hint", |ui| {
            ui.small(s.delete_method_recycle_hint);
        });
    });

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);

    // Two options, because the heading is already a yes/no question. The third
    // one that used to sit between them ("only above 1 GB") compared against
    // the batch total rather than any single file, which is not what its label
    // said - see `ConfirmBehavior`'s own docs for why it is gone rather than
    // reworded.
    super::row_heading(ui, s.confirm_behavior_label, s.badge_immediately);
    ui.add_enabled_ui(!app.busy, |ui| {
        ui.radio_value(
            &mut picked_confirm,
            ConfirmBehavior::Always,
            s.confirm_yes_label,
        );
        ui.radio_value(
            &mut picked_confirm,
            ConfirmBehavior::Never,
            s.confirm_no_label,
        );
    });
    ui.small(s.confirm_behavior_hint);

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);
    ui.small(s.selection_independent_switches_hint);

    if picked_method != app.settings.delete_method {
        app.set_delete_method(picked_method);
    }
    if picked_confirm != app.settings.confirm_behavior {
        app.set_confirm_behavior(picked_confirm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ui::harness::UiTest;
    use crate::ui::settings::SettingsSection;

    fn open_selection() -> UiTest {
        let mut test = UiTest::new(crate::ui::settings::show);
        test.app_mut().show_settings = true;
        test.app_mut().settings_section = SettingsSection::Selection;
        test.run();
        test
    }

    #[test]
    fn the_section_offers_both_switches() {
        let test = open_selection();
        let s = test.strings();

        test.assert_label(s.delete_method_label);
        test.assert_label(s.delete_method_permanent_label);
        test.assert_label(s.delete_method_recycle_label);

        test.assert_label(s.confirm_behavior_label);
        test.assert_label(s.confirm_yes_label);
        test.assert_label(s.confirm_no_label);
    }

    /// Each switch says when it lands, and the three answers differ. A single
    /// badge repeated three times would be the old dialog's problem with
    /// extra ink.
    #[test]
    fn each_switch_states_its_own_timing() {
        let test = open_selection();
        let s = test.strings();

        test.assert_label(s.badge_next_delete);
        test.assert_label(s.badge_immediately);
    }

    #[test]
    fn picking_a_delete_method_persists_it() {
        let mut test = open_selection();
        let s = test.strings();
        assert_eq!(test.app().settings.delete_method, DeleteMethod::Permanent);

        test.click(s.delete_method_recycle_label);

        assert_eq!(test.app().settings.delete_method, DeleteMethod::RecycleBin);
    }

    #[test]
    fn picking_a_confirmation_policy_persists_it() {
        let mut test = open_selection();
        let s = test.strings();
        assert_eq!(
            test.app().settings.confirm_behavior,
            ConfirmBehavior::Always
        );

        test.click(s.confirm_no_label);
        assert_eq!(test.app().settings.confirm_behavior, ConfirmBehavior::Never);

        test.click(s.confirm_yes_label);
        assert_eq!(
            test.app().settings.confirm_behavior,
            ConfirmBehavior::Always
        );
    }

    /// The hints under the delete method are the ones that used to panic:
    /// `Ui::indent` asserts in a horizontal layout, and this section's body
    /// inherits the nav row's unless the scroll area's content is wrapped.
    /// See the module docs of `ui::settings` and the plan's §2A.
    #[test]
    fn the_indented_hints_render_instead_of_panicking() {
        let test = open_selection();
        let s = test.strings();

        test.assert_label(s.delete_method_permanent_hint);
        test.assert_label(s.delete_method_recycle_hint);
    }

    #[test]
    fn nothing_changes_while_a_job_is_running() {
        let mut test = open_selection();
        let s = test.strings();
        test.app_mut().begin_job(false);
        test.run();

        test.click(s.delete_method_recycle_label);
        test.click(s.confirm_no_label);

        assert_eq!(test.app().settings.delete_method, DeleteMethod::Permanent);
        assert_eq!(
            test.app().settings.confirm_behavior,
            ConfirmBehavior::Always
        );
    }
}
