//! "Selection & deletion": three independent switches - what a fresh scan pre-selects
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

/// What the profile actually ticks, in one line.
///
/// The picker offered four bare names and nothing else - the only description
/// of them lived in a tooltip on the main screen's picker, which drives a
/// *different* field (see the module docs). A settings screen whose subject
/// is what gets deleted has to say on screen what each option deletes.
///
/// The wording deliberately avoids the confidence percentages the old text
/// quoted ("everything at 70% or higher"): the number is the detector's
/// internal scale, it is no longer shown anywhere in the tree, and it was
/// never something a user could weigh a decision against.
fn profile_hint(s: &i18n::Strings, profile: SelectionProfile) -> &'static str {
    match profile {
        SelectionProfile::Cautious => s.profile_cautious_hint,
        SelectionProfile::Balanced => s.profile_balanced_hint,
        SelectionProfile::Aggressive => s.profile_aggressive_hint,
        SelectionProfile::Custom => s.profile_custom_hint,
    }
}

/// Scroll-area id for a profile's hint line. Stable per profile, because
/// `Ui::indent` needs an id and a duplicate would collide across rows.
fn profile_hint_id(profile: SelectionProfile) -> &'static str {
    match profile {
        SelectionProfile::Cautious => "gt_settings_profile_cautious_hint",
        SelectionProfile::Balanced => "gt_settings_profile_balanced_hint",
        SelectionProfile::Aggressive => "gt_settings_profile_aggressive_hint",
        SelectionProfile::Custom => "gt_settings_profile_custom_hint",
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
            // Indented under its own radio, the way the routing modes and the
            // delete methods already describe themselves - one shared shape
            // for "this option, and what it does".
            ui.indent(profile_hint_id(profile), |ui| {
                ui.small(profile_hint(s, profile));
            });
            ui.add_space(4.0);
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
        test.assert_label(s.confirm_yes_label);
        test.assert_label(s.confirm_no_label);
    }

    /// Four bare names - "Cautious", "Balanced", "Aggressive", "Custom" -
    /// told the user nothing about what any of them ticks, on the one screen
    /// whose subject is what gets deleted. Every profile now describes itself
    /// where it is chosen, and the four descriptions have to differ or the
    /// picker is still four indistinguishable options with more ink.
    #[test]
    fn every_profile_says_what_it_ticks() {
        let test = open_selection();
        let s = test.strings();

        let mut seen = Vec::new();
        for profile in PROFILE_ORDER {
            let hint = profile_hint(s, profile);
            test.assert_label(hint);
            assert!(
                !seen.contains(&hint),
                "{profile:?} reuses another profile's description: {hint:?}",
            );
            seen.push(hint);
        }
    }

    /// The descriptions are indented under their radios via `Ui::indent`, the
    /// construct that panics in a horizontal layout and killed an earlier
    /// attempt at this dialog (see the module docs of `ui::settings`).
    #[test]
    fn the_indented_profile_hints_render_instead_of_panicking() {
        let mut test = open_selection();
        let s = test.strings();

        // Also under a running job: the block renders inside `add_enabled_ui`,
        // which is a different layout scope from the idle path above.
        test.app_mut().begin_job(false);
        test.run();

        for profile in PROFILE_ORDER {
            test.assert_label(profile_hint(s, profile));
        }
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

        test.click(s.profile_aggressive);
        test.click(s.delete_method_recycle_label);
        test.click(s.confirm_no_label);

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
