//! The first-run explanation (GT-34): what the app does, in what order, and
//! what its two unexplained words mean.
//!
//! # Why it is not a wizard, and not a screen
//!
//! On the first launch the user met an empty window and one line of hint
//! text, and had to guess both the order of operations and what "profile" and
//! "confidence" meant - in a tool that deletes files. On release day every
//! user is on their first launch, so this is the only state most of the
//! audience ever sees.
//!
//! It is still not worth a wizard. A multi-step "pick your languages, pick a
//! profile, now scan" flow asks for decisions before the user has seen a
//! single finding, which is the wrong order: the answers only mean something
//! once there is a result to apply them to. And it would add a screen to an
//! interface already called overloaded.
//!
//! So this renders *in the space the tree will occupy*, which is empty until
//! the first scan anyway, and disappears for good once the user has scanned
//! once. Nothing is added to the window; nothing new has to be dismissed.

use eframe::egui;

use crate::app::GameTrimmerApp;
use crate::i18n;

/// Whether the first-run explanation applies: the user has never started a
/// scan, and no scan is running right now (a running one has its own
/// progress to show, and the explanation would be describing a step already
/// under way).
pub fn applies(app: &GameTrimmerApp) -> bool {
    !app.settings.has_scanned && !app.busy
}

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
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

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(8.0);

    // The two words the main screen uses without ever defining them.
    ui.label(s.onboarding_profile);
    ui.add_space(4.0);
    ui.label(s.onboarding_confidence);

    ui.add_space(12.0);
    // The promise the rest of the app has to keep, stated before the user
    // presses anything: this button scans, it does not delete.
    ui.label(s.onboarding_safety);

    ui.add_space(14.0);
    if ui.button(s.btn_scan_libraries).clicked() {
        app.start_scan();
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

    #[test]
    fn a_fresh_window_explains_the_order_and_both_terms() {
        let test = fresh_window();
        let s = test.strings();

        assert!(!test.app().settings.has_scanned, "fresh test database");
        test.assert_label(s.onboarding_heading);
        for line in [
            s.onboarding_step_scan,
            s.onboarding_step_review,
            s.onboarding_step_remove,
            s.onboarding_profile,
            s.onboarding_confidence,
            s.onboarding_safety,
        ] {
            test.assert_label(line);
        }
    }

    /// It has to be actionable from where it is read - sending the user back
    /// to the top bar to find the same button is half the gap this closes.
    /// Asserted by the button carrying the top bar's own scan label, so it is
    /// the app's scan action rather than a decoration.
    ///
    /// Deliberately *not* clicked. `start_scan` spawns a worker that
    /// materializes the rule files next to the executable and then discovers
    /// and walks the machine's real game libraries - which a unit test has no
    /// business doing, and which no other test here does either (they drive
    /// `begin_job` instead). The wiring from this button to `start_scan` is
    /// therefore one line covered by reading, not by assertion; the state it
    /// produces is covered below.
    #[test]
    fn the_explanation_offers_the_scan_action_itself() {
        let test = fresh_window();
        let s = test.strings();

        test.assert_label(s.btn_scan_libraries);
    }

    /// And it is genuinely once: `has_scanned` is what takes it away, and
    /// what comes back is the ordinary empty-state hint. This is the
    /// assertion that keeps the introduction from becoming furniture.
    #[test]
    fn a_scanned_before_window_gets_the_ordinary_empty_state() {
        let mut test = fresh_window();
        let s = test.strings();
        test.assert_label(s.onboarding_heading);

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
}
