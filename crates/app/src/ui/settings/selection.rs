//! "Selection & deletion": three switches the audit found conflated in the
//! old dialog - what a fresh scan pre-selects
//! (`default_selection_profile`), how a delete disposes of files
//! (`delete_method`), and when the confirmation is shown
//! (`confirm_behavior`).
//!
//! None of the three affects the others, and none of them touches the
//! findings currently on screen. That last point is why the profile here is
//! a *separate field* from the main-screen picker rather than the same one
//! shown twice: editing a default in a settings dialog must not silently
//! re-check the tree behind it.

use eframe::egui;

use gametrimmer_core::settings::{ConfirmBehavior, DeleteMethod, SelectionProfile};

use crate::app::GameTrimmerApp;
use crate::i18n;

/// The profiles, in ascending order of how much they select.
///
/// A local copy of the main-screen picker's order rather than a shared
/// constant: the two pickers drive different fields, and sharing one list
/// would invite a caller to reach for the wrong setter.
const PROFILE_ORDER: [SelectionProfile; 4] = [
    SelectionProfile::Cautious,
    SelectionProfile::Balanced,
    SelectionProfile::Aggressive,
    SelectionProfile::Custom,
];

fn profile_label(s: &i18n::Strings, profile: SelectionProfile) -> &'static str {
    match profile {
        SelectionProfile::Cautious => s.profile_cautious,
        SelectionProfile::Balanced => s.profile_balanced,
        SelectionProfile::Aggressive => s.profile_aggressive,
        SelectionProfile::Custom => s.profile_custom,
    }
}

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let s = i18n::strings(app.lang());
    let mut picked_profile = app.settings.default_selection_profile;
    let mut picked_method = app.settings.delete_method;
    let mut picked_confirm = app.settings.confirm_behavior;

    // Every control here persists through a fresh database connection, so
    // all three groups are gated the same way: writing one underneath a
    // running `VACUUM` (which needs exclusive access) or delete job would
    // race that worker's own connection.
    super::row_heading(ui, s.default_profile_label, s.badge_next_scan);
    ui.add_enabled_ui(!app.busy, |ui| {
        for profile in PROFILE_ORDER {
            ui.radio_value(&mut picked_profile, profile, profile_label(s, profile));
        }
    });
    ui.small(s.default_profile_hint);

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);

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

    super::row_heading(ui, s.confirm_behavior_label, s.badge_immediately);
    ui.add_enabled_ui(!app.busy, |ui| {
        ui.radio_value(
            &mut picked_confirm,
            ConfirmBehavior::Always,
            s.confirm_always_label,
        );
        ui.radio_value(
            &mut picked_confirm,
            ConfirmBehavior::OnlyAboveOneGb,
            s.confirm_only_1gb_label,
        );
        ui.radio_value(
            &mut picked_confirm,
            ConfirmBehavior::Never,
            s.confirm_never_label,
        );
    });
    ui.small(s.confirm_behavior_hint);

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);
    ui.small(s.selection_independent_switches_hint);

    if picked_profile != app.settings.default_selection_profile {
        app.set_default_selection_profile(picked_profile);
    }
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
    fn the_section_offers_all_three_switches() {
        let test = open_selection();
        let s = test.strings();

        test.assert_label(s.default_profile_label);
        for profile in PROFILE_ORDER {
            test.assert_label(profile_label(s, profile));
        }

        test.assert_label(s.delete_method_label);
        test.assert_label(s.delete_method_permanent_label);
        test.assert_label(s.delete_method_recycle_label);

        test.assert_label(s.confirm_behavior_label);
        test.assert_label(s.confirm_always_label);
        test.assert_label(s.confirm_only_1gb_label);
        test.assert_label(s.confirm_never_label);
    }

    /// Each switch says when it lands, and the three answers differ. A single
    /// badge repeated three times would be the old dialog's problem with
    /// extra ink.
    #[test]
    fn each_switch_states_its_own_timing() {
        let test = open_selection();
        let s = test.strings();

        test.assert_label(s.badge_next_scan);
        test.assert_label(s.badge_next_delete);
        test.assert_label(s.badge_immediately);
    }

    /// The section's central claim, asserted rather than left to the hint
    /// text: picking a scan default must leave the visible tree untouched.
    #[test]
    fn picking_a_scan_default_does_not_recheck_the_tree() {
        let mut test = open_selection();
        let s = test.strings();
        test.seed_findings();
        test.app_mut().mark_selection_custom();
        let before: Vec<bool> = test.app().findings.iter().map(|f| f.selected).collect();

        test.click(s.profile_cautious);

        assert_eq!(
            test.app().settings.default_selection_profile,
            SelectionProfile::Cautious,
        );
        let after: Vec<bool> = test.app().findings.iter().map(|f| f.selected).collect();
        assert_eq!(after, before, "the scan default re-checked the tree");
        assert_eq!(
            test.app().settings.selection_profile,
            SelectionProfile::Custom,
            "the live profile followed the scan default",
        );
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

        test.click(s.confirm_only_1gb_label);
        assert_eq!(
            test.app().settings.confirm_behavior,
            ConfirmBehavior::OnlyAboveOneGb,
        );

        test.click(s.confirm_never_label);
        assert_eq!(test.app().settings.confirm_behavior, ConfirmBehavior::Never);
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

        test.click(s.profile_aggressive);
        test.click(s.delete_method_recycle_label);
        test.click(s.confirm_never_label);

        assert_eq!(
            test.app().settings.default_selection_profile,
            SelectionProfile::default(),
        );
        assert_eq!(test.app().settings.delete_method, DeleteMethod::Permanent);
        assert_eq!(
            test.app().settings.confirm_behavior,
            ConfirmBehavior::Always
        );
    }
}
