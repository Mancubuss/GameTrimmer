//! Test-only scaffolding for driving the UI through `egui_kittest`.
//!
//! Why this exists: the first attempt at the settings redesign was verified
//! by handing the user a release build and waiting for them to spot the bug
//! in a screenshot. Four rounds, four symptoms, all green on
//! `build`/`test`/`clippy` - including the round that panics on tab switch.
//! Correctness belongs in `cargo test`; only aesthetic judgement should need
//! a human. See `docs/ui-redesign-plan-2026-07-31.md`.
//!
//! Every `ui` submodule has the same shape, `show(&mut GameTrimmerApp,
//! &mut egui::Ui)`, so a harness can drive one directly through
//! [`egui_kittest::Harness::builder`]'s `build_ui_state` - no `eframe::Frame`,
//! no OS window and no GPU. That is also why the `snapshot`/`wgpu` features
//! are deliberately left off: what these tests assert is presence, geometry
//! and state, none of which need a rendered image.
//!
//! Look up widgets through [`i18n::strings`] rather than literal text. Several
//! planned changes are renames (`profile_custom` becomes "Власний", the
//! force-MFT label becomes "prefer MFT"), and an assertion keyed on a literal
//! would have to be edited in the same commit as the rename - which is exactly
//! how a test quietly stops testing anything.

use eframe::egui;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

use crate::app::GameTrimmerApp;
use crate::i18n;

/// The app's standard window size (see `main.rs`). Tests default to it so
/// they measure the layout the user actually gets, not an arbitrary viewport.
pub const STANDARD_VIEWPORT: egui::Vec2 = egui::vec2(900.0, 600.0);

/// Narrower than the standard window, for asserting that a layout reflows
/// instead of pushing its primary action off the edge.
pub const NARROW_VIEWPORT: egui::Vec2 = egui::vec2(760.0, 600.0);

/// egui_kittest defaults to 4 steps per `run`. A modal opening a scroll area
/// can legitimately need a few more before it settles; anything past this is
/// a repaint loop, which the failure from [`UiTest::run`] names as such.
const MAX_STEPS: u64 = 16;

/// One `ui::*::show` entry point. A plain `fn` pointer rather than a closure
/// so the harness stays `'static` and the app is reachable through
/// [`UiTest::app`] while the harness is alive.
pub type ShowFn = fn(&mut GameTrimmerApp, &mut egui::Ui);

/// A harness plus the temp directory holding the app's throwaway database.
///
/// Field order matters: `harness` owns the app, which opens the database on
/// every persisted setting change, so it must drop before `_dir` deletes it.
pub struct UiTest {
    harness: Harness<'static, GameTrimmerApp>,
    _dir: tempfile::TempDir,
}

impl UiTest {
    /// A harness around one render function at the standard window size.
    pub fn new(show: ShowFn) -> Self {
        Self::with_size(show, STANDARD_VIEWPORT)
    }

    /// Same, at an explicit viewport - for asserting that a layout survives a
    /// narrow window instead of clipping its primary action.
    pub fn with_size(show: ShowFn, size: egui::Vec2) -> Self {
        let dir = tempfile::tempdir().expect("create temp dir for the test database");
        let app = GameTrimmerApp::new_for_test(dir.path());
        let harness = Harness::builder()
            .with_size(size)
            .with_max_steps(MAX_STEPS)
            .build_ui_state(move |ui, app| show(app, ui), app);
        Self { harness, _dir: dir }
    }

    /// Runs frames until the UI settles. Fails with a named error rather than
    /// letting a repaint loop silently truncate at the step limit and leave
    /// the assertions below running against a half-built frame.
    pub fn run(&mut self) {
        if let Err(err) = self.harness.try_run() {
            panic!(
                "the UI never settled within {MAX_STEPS} steps - this is a repaint loop, \
                 not a slow layout: {err}"
            );
        }
    }

    pub fn app(&self) -> &GameTrimmerApp {
        self.harness.state()
    }

    pub fn app_mut(&mut self) -> &mut GameTrimmerApp {
        self.harness.state_mut()
    }

    /// Localized strings for the app's current language, so assertions name
    /// widgets the same way the render code does.
    pub fn strings(&self) -> &'static i18n::Strings {
        i18n::strings(self.app().lang())
    }

    /// Whether a widget with exactly this accessibility label is on screen.
    pub fn has_label(&self, label: &str) -> bool {
        self.harness.query_by_label(label).is_some()
    }

    /// Asserts the label is present, naming what was expected on failure.
    #[track_caller]
    pub fn assert_label(&self, label: &str) {
        assert!(
            self.has_label(label),
            "expected a widget labelled {label:?} to be on screen",
        );
    }

    /// Asserts the label is absent - the other half of "this section shows
    /// only its own content".
    #[track_caller]
    pub fn assert_no_label(&self, label: &str) {
        assert!(
            !self.has_label(label),
            "did not expect a widget labelled {label:?} to be on screen",
        );
    }

    /// The on-screen rectangle of the widget with this label. Used for the
    /// geometry invariants that the four failed rounds had no way to state:
    /// "the delete button fits inside the window", "the modal does not move
    /// when the section changes".
    #[track_caller]
    pub fn rect_of(&self, label: &str) -> egui::Rect {
        self.node(label).rect()
    }

    /// Gives the app a small synthetic result set and builds its tree, for
    /// the panels that render nothing at all without findings.
    ///
    /// Two games on one disk so the tree has more than one row to move a
    /// keyboard cursor between, and everything pre-selected so the actions
    /// gated on a non-empty selection are enabled.
    pub fn seed_findings(&mut self) {
        let app = self.app_mut();
        app.findings = (0..2)
            .map(|i| crate::model::FindingItem {
                row: crate::model::FindingRow {
                    file_id: i,
                    game_id: i,
                    game_name: format!("Test Game {i}"),
                    install_dir: std::path::PathBuf::from("C:\\Games\\Test"),
                    rel_path: format!("data/loc_{i}.pak"),
                    size: 1024 * 1024,
                    size_on_disk: 1024 * 1024,
                    source: crate::model::FindingSource::Loc(
                        gametrimmer_core::langdetect::LangKind::Text,
                    ),
                    rule_desc: "test rule".to_string(),
                    confidence: 90,
                    lang_tag: Some("de".to_string()),
                    group_dir: None,
                },
                selected: true,
                removed: false,
            })
            .collect();
        app.tree = crate::model::build_tree(&app.findings);
        self.run();
    }

    /// Moves the pointer over the widget with this label and settles, so a
    /// tooltip (including `on_disabled_hover_text`) has a frame to appear in.
    #[track_caller]
    pub fn hover(&mut self, label: &str) {
        self.node(label).hover();
        self.run();
    }

    /// Sends a key press and runs until the UI settles.
    pub fn press(&mut self, key: egui::Key) {
        self.harness.key_press(key);
        self.run();
    }

    /// Clicks the widget with this label, the way a user would: scroll it
    /// into view first, then click where it ended up.
    ///
    /// The two-step dance is not optional. `Node::click` synthesizes a pointer
    /// event at the node's centre, and egui discards pointer events outside a
    /// [`egui::ScrollArea`]'s clip rect - so clicking a widget that is present
    /// in the accessibility tree but scrolled out of view does *nothing*, and
    /// the assertion afterwards passes or fails for reasons unrelated to what
    /// it claims to test. This dialog has two nested scroll areas (the audit's
    /// §6.3 complaint), so that is the common case here, not a corner case.
    ///
    /// Scrolling moves the widget, so the node has to be looked up again
    /// before the click: the first lookup's rect is stale by then.
    ///
    /// Deliberately *not* `Node::click_accesskit`, which activates a control
    /// regardless of whether it is reachable. That would paper over exactly
    /// the bug round 1 shipped - theme radios rendered off-screen with no
    /// scrollbar to reach them.
    #[track_caller]
    pub fn click(&mut self, label: &str) {
        self.node(label).scroll_to_me();
        self.run();
        self.node(label).click();
        self.run();
    }

    #[track_caller]
    fn node<'s>(&'s self, label: &'s str) -> egui_kittest::Node<'s> {
        match self.harness.query_by_label(label) {
            Some(node) => node,
            None => panic!("no widget labelled {label:?} on screen"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The harness itself has to be trustworthy before anything is asserted
    /// through it: a label that is rendered must be found, and one that is
    /// not must not be.
    #[test]
    fn finds_a_rendered_label_and_does_not_invent_a_missing_one() {
        fn show(_app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
            ui.label("gt_harness_probe");
        }

        let mut test = UiTest::new(show);
        test.run();

        test.assert_label("gt_harness_probe");
        test.assert_no_label("gt_harness_absent");
    }

    /// `rect_of` has to report real geometry - the modal-stability and
    /// button-fits-the-window assertions are worthless if it returns
    /// something degenerate.
    #[test]
    fn reports_a_non_degenerate_rect_inside_the_viewport() {
        fn show(_app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
            let _ = ui.button("gt_harness_button");
        }

        let mut test = UiTest::new(show);
        test.run();

        let rect = test.rect_of("gt_harness_button");
        assert!(
            rect.width() > 0.0 && rect.height() > 0.0,
            "empty rect: {rect:?}"
        );
        assert!(
            rect.max.x <= STANDARD_VIEWPORT.x && rect.max.y <= STANDARD_VIEWPORT.y,
            "{rect:?} escapes the {STANDARD_VIEWPORT:?} viewport",
        );
    }

    /// State has to survive between frames and be reachable while the harness
    /// is alive - this is why the harness carries the app as kittest state
    /// rather than capturing `&mut app` in the closure.
    #[test]
    fn a_click_reaches_app_state() {
        fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
            if ui.button("gt_harness_toggle").clicked() {
                app.show_settings = !app.show_settings;
            }
        }

        let mut test = UiTest::new(show);
        test.run();
        assert!(!test.app().show_settings);

        test.click("gt_harness_toggle");
        assert!(test.app().show_settings, "the click never reached the app");
    }

    /// Each harness must own its database, or tests running in parallel would
    /// interfere through persisted settings.
    #[test]
    fn each_harness_gets_its_own_database() {
        fn show(_app: &mut GameTrimmerApp, _ui: &mut egui::Ui) {}

        let one = UiTest::new(show);
        let two = UiTest::new(show);

        assert_ne!(one.app().db_path(), two.app().db_path());
        assert_eq!(one.app().db_error, None);
        assert_eq!(two.app().db_error, None);
    }
}
