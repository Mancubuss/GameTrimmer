//! The first-run screen (first-run onboarding): what the app does, in what order, how it
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
//! # Why the action comes before the explanation
//!
//! The first version put everything it had to say above the Scan button, and
//! the result was a screen the user had to scroll before they could find out
//! there was anything to press: at the standard 900x600 window the disclaimer
//! and the button both ended up past the fold. So the order is now what the
//! screen *asks for* first - the three steps, the one-line promise that
//! scanning deletes nothing, the disclaimer, the button - and the background
//! reading (how the detectors decide, what the filters narrow, what "profile"
//! and the \u{26a0} mark mean) after it. None of the latter is needed to decide
//! whether to press Scan, and half of it only starts meaning anything once
//! there are findings on screen.
//!
//! `everything_the_first_run_asks_for_fits_in_the_window` is the assertion that
//! keeps it that way in both languages, since the wording will change again.
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

    ui.add_space(12.0);
    // The promise the rest of the app has to keep, stated before the user
    // presses anything: this button scans, it does not delete.
    ui.label(s.onboarding_safety);

    ui.add_space(14.0);
    show_disclaimer(app, ui, s);

    ui.add_space(14.0);
    // Directly under the gate it depends on, so "why is this grey" and the
    // answer are one glance apart rather than one scroll. A dead database
    // outranks the disclaimer here for the same reason it does in the top
    // panel, and the ordering has to match: ticking the box would not unblock
    // anything, so saying so first spares the user a wall they only find
    // after clearing the one they can see.
    let blocked = app
        .blocked_by_database()
        .or_else(|| app.blocked_by_disclaimer());
    if crate::ui::gated_button(ui, s.btn_scan_libraries, blocked).clicked() {
        app.start_scan();
    }

    separate(ui);

    // How the proposals are arrived at, and what narrows them. Both were
    // reachable before only by reading the README that ships beside the exe.
    //
    // Below the button rather than above it, which is the other half of the
    // shortening (the first half was cutting the text itself): what the user
    // has to *do* - read the steps, read the disclaimer, tick it, scan - now
    // ends inside the window, and the background reading follows it instead of
    // pushing it past the fold. Nothing here is needed to decide whether to
    // press Scan; the safety line above already carries the one fact that is.
    ui.strong(s.onboarding_how_heading);
    ui.add_space(4.0);
    ui.label(s.onboarding_how_body);
    ui.add_space(6.0);
    ui.label(s.onboarding_filters_body);

    separate(ui);

    // The word and the mark the main screen otherwise uses without ever
    // explaining them. Both only start mattering once there are findings on
    // screen, which is after this button has been pressed.
    ui.label(s.onboarding_selection);
    ui.add_space(4.0);
    ui.label(s.onboarding_review_mark);

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
    let already_accepted = accepted;

    crate::ui::danger_frame(ui, s.disclaimer_heading, |ui| {
        ui.label(s.disclaimer_body);
        ui.add_space(8.0);
        // Disabled once ticked, rather than left live and silently ignoring the
        // click: accepting is one-way, and a checkbox that springs back on its
        // own reads as a bug, not as a decision the app is holding to (MT-A02).
        ui.add_enabled_ui(!already_accepted, |ui| {
            ui.checkbox(&mut accepted, s.disclaimer_accept_checkbox)
                .on_disabled_hover_text(s.disclaimer_already_accepted);
        });
    });

    // One-way: see `GameTrimmerApp::accept_disclaimer` for why unticking is
    // not a state this screen offers a way back to.
    if accepted && !app.settings.disclaimer_accepted {
        app.accept_disclaimer();
    }
}

/// The default-on diagnostic logging choice, and the reason it is useful.
///
/// It is the same setting as the one in "Data & diagnostics" rather than a
/// second flag, so a user who leaves it on here finds it ticked there, while
/// one who opts out finds it off there too.
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
    use gametrimmer_core::settings::Lang;

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
            s.onboarding_selection,
            s.onboarding_review_mark,
            s.onboarding_safety,
        ] {
            test.assert_label(line);
        }
    }

    /// GT-74: this screen carries the app's *other* scan button, and a gate
    /// added to the top panel alone leaves it looking live. `start_scan`
    /// refuses either way, but a button that accepts the click and does
    /// nothing is the failure this ticket is about, one screen over.
    #[test]
    fn a_dead_database_greys_out_the_first_run_scan_button_too() {
        let mut test = fresh_window();
        test.app_mut().db_error = Some("gt_probe_db_dead".to_string());
        test.run();
        let s = test.strings();

        test.hover(s.btn_scan_libraries);

        assert!(
            test.has_label(s.disabled_database),
            "the first-run scan button did not blame the database",
        );
    }

    /// The complaint this screen was shortened for: it was long enough that the
    /// disclaimer the user has to accept, and the button that becomes live once
    /// they do, both sat below the fold of the standard window - so the first
    /// thing a first-run user had to work out was that there was more screen.
    ///
    /// Measured on this layout at 900x600: the Scan button used to end at
    /// y=633, and now ends at y=347. The assertion is the weaker claim that
    /// everything the screen *asks for* is on it, which is what has to keep
    /// holding as the wording changes again.
    /// English only while localizations are frozen for development (Vikunja
    /// #443 tracks unfreezing them) - it used to run both languages, because
    /// the fold is a claim about rendered height and the Ukrainian strings
    /// were the longer set, so an English-only assertion could pass on a
    /// layout that overflowed for half the audience. Re-add the longer
    /// language to this loop once localizations unfreeze.
    #[test]
    fn everything_the_first_run_asks_for_fits_in_the_window() {
        let language = Lang::En;
        let mut test = fresh_window();
        test.app_mut()
            .set_language(gametrimmer_core::settings::LanguagePreference::Fixed(
                language,
            ));
        test.run();

        let s = i18n::strings(language);
        let fold = crate::ui::harness::STANDARD_VIEWPORT.y;

        for label in [
            s.onboarding_heading,
            s.onboarding_step_scan,
            s.onboarding_safety,
            s.disclaimer_heading,
            s.disclaimer_body,
            s.disclaimer_accept_checkbox,
            s.btn_scan_libraries,
        ] {
            let bottom = test.rect_of(label).max.y;
            assert!(
                bottom <= fold,
                "in {language:?} the first-run screen pushes something it asks for past \
                 the fold (ends at y={bottom}, window is {fold} tall): {label}",
            );
        }
    }

    /// The other half of the shortening: the background reading was moved from
    /// above the Scan button to below it. Asserted by geometry rather than by
    /// presence, because presence is what the layout had before too - the
    /// question is only where.
    #[test]
    fn the_background_reading_follows_the_action_rather_than_delaying_it() {
        let test = fresh_window();
        let s = test.strings();
        let button = test.rect_of(s.btn_scan_libraries).max.y;

        for label in [
            s.onboarding_how_heading,
            s.onboarding_how_body,
            s.onboarding_filters_body,
            s.onboarding_selection,
            s.onboarding_review_mark,
        ] {
            let top = test.rect_of(label).min.y;
            assert!(
                top > button,
                "background reading sits above the Scan button (y={top} vs {button}): {label}",
            );
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

    /// Accepting is one-way, so the tick is disabled afterwards and says why.
    /// It used to stay live and simply ignore the click, which looks like the
    /// app failing to register it rather than holding to a decision (MT-A02).
    #[test]
    fn the_accepted_tick_is_disabled_and_explains_itself() {
        let mut test = fresh_window();
        let s = test.strings();

        test.click(s.disclaimer_accept_checkbox);
        // Still the first-run screen for this frame - the tick is on it, and
        // the claim is about how it looks the moment after acceptance.
        assert!(test.app().settings.disclaimer_accepted);

        test.hover(s.disclaimer_accept_checkbox);
        test.assert_label(s.disclaimer_already_accepted);
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
    fn the_default_on_logging_choice_drives_the_real_setting() {
        let mut test = fresh_window();
        let s = test.strings();
        assert!(test.app().settings.logging_enabled);
        test.assert_label(s.onboarding_logging_body);

        test.click(s.logging_checkbox);

        assert!(!test.app().settings.logging_enabled);
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
