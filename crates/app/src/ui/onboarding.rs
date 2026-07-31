//! The first-run screen (GT-34): what the app does, in what order, how it
//! decides, what it will not promise - and the one thing the user has to
//! agree to before it will touch a disk.
//!
//! # Why it is not a wizard, and not a screen
//!
//! On the first launch the user met an empty window and one line of hint
//! text, and had to guess both the order of operations and what "profile"
//! meant, plus what the \u{26a0} beside some files was trying to say - in a
//! tool that deletes files. On release day every user is on their first
//! launch, so this is the only state most of the audience ever sees.
//!
//! It is still not worth a wizard. A multi-step "pick your languages, pick a
//! profile, now scan" flow asks for decisions before the user has seen a
//! single finding, which is the wrong order: the answers only mean something
//! once there is a result to apply them to. And it would add a screen to an
//! interface already called overloaded.
//!
//! So this renders *in the space the tree will occupy*, which is empty until
//! the first scan anyway, and disappears for good once the user has scanned
//! once and accepted the disclaimer. Nothing is added to the window; nothing
//! new has to be dismissed.
//!
//! # Why the disclaimer gates rather than informs
//!
//! A liability notice the user can scroll past is not one. So the acceptance
//! checkbox is the only thing standing between this screen and a scan, and
//! `GameTrimmerApp::start_scan` / `open_delete_confirmation` refuse
//! independently of which button was pressed - see
//! `GameTrimmerApp::blocked_by_disclaimer`.
//!
//! That is also why [`applies`] keys on *two* flags rather than one. A
//! database written before the disclaimer existed says `has_scanned = true`
//! and holds findings, so its owner would otherwise be sent straight to a
//! populated tree with every action silently refusing. Keying on acceptance
//! as well shows them this screen exactly once.

use eframe::egui;

use crate::app::GameTrimmerApp;
use crate::i18n;

/// Whether the first-run screen applies: the user has not yet scanned, or has
/// not yet accepted the disclaimer, and no scan is running right now (a
/// running one has its own progress to show, and an introduction to a step
/// already under way would be describing the past).
pub fn applies(app: &GameTrimmerApp) -> bool {
    !app.busy && (!app.settings.has_scanned || !app.settings.disclaimer_accepted)
}

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    // The screen is taller than the panel at the standard 900x600 window, and
    // the disclaimer is at the bottom of it. Without a scroll area the one
    // block the user *must* reach is the one clipped off the edge.
    egui::ScrollArea::vertical()
        .id_salt("gt_onboarding")
        .auto_shrink([false; 2])
        .show(ui, |ui| show_content(app, ui));
}

fn show_content(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let s = i18n::strings(app.lang());

    ui.add_space(16.0);
    ui.heading(s.onboarding_heading);
    ui.add_space(10.0);

    for step in [
        s.onboarding_step_scan,
        s.onboarding_step_review,
        s.onboarding_step_remove,
    ] {
        ui.label(step);
        ui.add_space(4.0);
    }

    separate(ui);

    // How the proposals are arrived at, and what narrows them. Both were
    // reachable before only by reading the README that ships beside the exe.
    ui.strong(s.onboarding_how_heading);
    ui.add_space(4.0);
    ui.label(s.onboarding_how_body);
    ui.add_space(6.0);
    ui.label(s.onboarding_filters_body);

    separate(ui);

    // The word and the mark the main screen otherwise uses without ever
    // explaining them.
    ui.label(s.onboarding_profile);
    ui.add_space(4.0);
    ui.label(s.onboarding_review_mark);

    ui.add_space(12.0);
    // The promise the rest of the app has to keep, stated before the user
    // presses anything: this button scans, it does not delete.
    ui.label(s.onboarding_safety);

    ui.add_space(14.0);
    show_disclaimer(app, ui, s);

    ui.add_space(14.0);
    // Directly under the gate it depends on, so "why is this grey" and the
    // answer are one glance apart rather than one scroll.
    let blocked = app.blocked_by_disclaimer();
    if crate::ui::gated_button(ui, s.btn_scan_libraries, blocked).clicked() {
        app.start_scan();
    }

    separate(ui);
    show_logging_offer(app, ui, s);

    separate(ui);
    crate::ui::credits(ui, s);
    ui.add_space(16.0);
}

fn separate(ui: &mut egui::Ui) {
    ui.add_space(12.0);
    ui.separator();
    ui.add_space(10.0);
}

/// The disclaimer, in the app's one danger treatment, with the checkbox that
/// unblocks scanning and deleting.
///
/// Rendered in both states on purpose - the accepted one is what an upgrading
/// user sees for the one frame between ticking it and the screen retiring,
/// and a block that vanished mid-click would read as a glitch rather than as
/// an acknowledgement.
fn show_disclaimer(app: &mut GameTrimmerApp, ui: &mut egui::Ui, s: &i18n::Strings) {
    let mut accepted = app.settings.disclaimer_accepted;

    crate::ui::danger_frame(ui, s.disclaimer_heading, |ui| {
        ui.label(s.disclaimer_body);
        ui.add_space(8.0);
        ui.checkbox(&mut accepted, s.disclaimer_accept_checkbox);
    });

    // One-way: see `GameTrimmerApp::accept_disclaimer` for why unticking is
    // not a state this screen offers a way back to.
    if accepted && !app.settings.disclaimer_accepted {
        app.accept_disclaimer();
    }
}

/// The offer to turn diagnostic logging on, and the reason it is worth it.
///
/// It is the same setting as the one in "Data & diagnostics" rather than a
/// second flag, so a user who says yes here finds it already ticked there -
/// and one who says no is not asked again by a screen that never returns.
fn show_logging_offer(app: &mut GameTrimmerApp, ui: &mut egui::Ui, s: &i18n::Strings) {
    let mut enabled = app.settings.logging_enabled;

    ui.strong(s.logging_label);
    ui.add_space(4.0);
    ui.label(s.onboarding_logging_body);
    ui.add_space(8.0);
    ui.checkbox(&mut enabled, s.logging_checkbox);

    if enabled != app.settings.logging_enabled {
        app.set_logging_enabled(enabled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ui::harness::UiTest;

    /// Drives the whole central panel, not this module alone: what is under
    /// test is that the explanation *reaches the screen* in the empty state,
    /// which is a claim about `tree_view`'s branch as much as about this.
    fn fresh_window() -> UiTest {
        let mut test = UiTest::new(crate::ui::tree_view::show);
        test.run();
        test
    }

    /// Everything a first launch has to say, in one assertion, so a block
    /// silently dropped from the layout fails here rather than in a build the
    /// user is asked to judge.
    #[test]
    fn a_fresh_window_explains_the_order_the_method_and_both_terms() {
        let test = fresh_window();
        let s = test.strings();

        assert!(!test.app().settings.has_scanned, "fresh test database");
        test.assert_label(s.onboarding_heading);
        for line in [
            s.onboarding_step_scan,
            s.onboarding_step_review,
            s.onboarding_step_remove,
            s.onboarding_how_heading,
            s.onboarding_how_body,
            s.onboarding_filters_body,
            s.onboarding_profile,
            s.onboarding_review_mark,
            s.onboarding_safety,
        ] {
            test.assert_label(line);
        }
    }

    /// It has to be actionable from where it is read - sending the user back
    /// to the top bar to find the same button is half the gap this closes.
    /// Asserted by the button carrying the top bar's own scan label, so it is
    /// the app's scan action rather than a decoration.
    #[test]
    fn the_explanation_offers_the_scan_action_itself() {
        let test = fresh_window();
        let s = test.strings();

        test.assert_label(s.btn_scan_libraries);
    }

    /// And it is genuinely once: scanning plus acceptance is what takes it
    /// away, and what comes back is the ordinary empty-state hint. This is
    /// the assertion that keeps the introduction from becoming furniture.
    #[test]
    fn a_scanned_before_window_gets_the_ordinary_empty_state() {
        let mut test = fresh_window();
        let s = test.strings();
        test.assert_label(s.onboarding_heading);

        test.app_mut().accept_disclaimer();
        test.app_mut().mark_scan_started();
        test.run();

        assert!(!applies(test.app()));
        test.assert_no_label(s.onboarding_heading);
        test.assert_label(s.no_findings_hint);
    }

    /// The flag has to outlive the process, or every launch is a first
    /// launch. Read back through a second app on the same directory rather
    /// than from the struct that just wrote it.
    #[test]
    fn the_first_run_is_remembered_across_launches() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut app = GameTrimmerApp::new_for_test(dir.path());
        assert!(!app.settings.has_scanned);

        app.mark_scan_started();

        let reopened = GameTrimmerApp::new_for_test(dir.path());
        assert!(
            reopened.settings.has_scanned,
            "the first run was forgotten on restart",
        );
    }

    /// The control on `applies`: while a scan is running the panel has
    /// progress to report, and an introduction to a step already under way
    /// would be describing the past.
    #[test]
    fn it_yields_to_a_running_scan() {
        let mut test = fresh_window();
        let s = test.strings();
        test.app_mut().begin_job(true);
        test.run();

        test.assert_no_label(s.onboarding_heading);
        test.assert_label(s.scanning_in_progress);
    }

    /// The disclaimer is on screen, in the app's danger treatment, with the
    /// checkbox that clears it.
    #[test]
    fn the_first_run_states_the_disclaimer_in_the_danger_treatment() {
        let test = fresh_window();
        let s = test.strings();

        test.assert_label(s.disclaimer_heading);
        test.assert_label(s.disclaimer_body);
        test.assert_label(s.disclaimer_accept_checkbox);
    }

    /// The whole point of the gate. Before acceptance the scan button is
    /// disabled and says why; after it, it is live.
    #[test]
    fn scanning_is_blocked_until_the_disclaimer_is_accepted() {
        let mut test = fresh_window();
        let s = test.strings();
        assert!(!test.app().settings.disclaimer_accepted);

        test.hover(s.btn_scan_libraries);
        test.assert_label(s.disabled_disclaimer);

        test.click(s.disclaimer_accept_checkbox);

        assert!(test.app().settings.disclaimer_accepted);
        assert!(test.app().blocked_by_disclaimer().is_none());
    }

    /// Acceptance has to outlive the process too, or the gate asks again on
    /// every launch and stops meaning anything.
    #[test]
    fn acceptance_is_remembered_across_launches() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut app = GameTrimmerApp::new_for_test(dir.path());
        assert!(!app.settings.disclaimer_accepted);

        app.accept_disclaimer();

        let reopened = GameTrimmerApp::new_for_test(dir.path());
        assert!(
            reopened.settings.disclaimer_accepted,
            "the acceptance was forgotten on restart",
        );
    }

    /// The upgrade case this screen's `applies` was widened for: a database
    /// that has scanned before but predates the disclaimer. Its owner must be
    /// shown the screen rather than dropped onto a tree whose every action
    /// silently refuses.
    #[test]
    fn a_database_from_before_the_disclaimer_still_gets_the_screen() {
        let mut test = fresh_window();
        let s = test.strings();
        test.app_mut().mark_scan_started();
        test.run();

        assert!(test.app().settings.has_scanned);
        assert!(!test.app().settings.disclaimer_accepted);
        test.assert_label(s.disclaimer_heading);
    }

    /// The logging offer is the same setting as the one in the settings
    /// dialog, not a second flag that agrees with it by accident.
    #[test]
    fn the_logging_offer_drives_the_real_setting() {
        let mut test = fresh_window();
        let s = test.strings();
        assert!(!test.app().settings.logging_enabled);
        test.assert_label(s.onboarding_logging_body);

        test.click(s.logging_checkbox);

        assert!(test.app().settings.logging_enabled);
    }

    /// The acknowledgements, each naming who it thanks. Asserted line by line
    /// rather than by the heading alone: a heading over an empty block is
    /// exactly the failure a presence check on the heading would miss.
    #[test]
    fn the_first_run_credits_what_it_was_built_on() {
        let test = fresh_window();
        let s = test.strings();

        test.assert_label(s.credits_heading);
        for line in [s.credits_anthropic, s.credits_karpathy, s.credits_tikione] {
            test.assert_label(line);
        }
    }
}
