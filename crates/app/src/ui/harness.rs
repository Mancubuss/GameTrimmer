//! Test-only scaffolding for driving the UI through `egui_kittest`.
//!
//! Why this exists: the first attempt at the settings redesign was verified
//! by handing the user a release build and waiting for them to spot the bug
//! in a screenshot. Four rounds, four symptoms, all green on
//! `build`/`test`/`clippy` - including the round that panics on tab switch.
//! Correctness belongs in `cargo test`; only aesthetic judgement should need
//! a human.
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

/// Frames [`UiTest::run_animated`] draws. Enough for a layout to reach its
/// final size, which is all these tests read; the animation itself is not
/// under assertion.
const ANIMATED_STEPS: usize = 4;

/// The `(vendor, library root)` pair each seeded game is attributed to, by
/// game index - two launchers and two library roots sharing one disk, so the
/// three grouping axes produce three different trees over the same findings.
pub const SEEDED_LIBRARIES: [(&str, &str); 2] = [("steam", "C:\\SteamLibrary"), ("gog", "C:\\GOG")];

/// One `ui::*::show` entry point. A plain `fn` pointer rather than a closure
/// so the harness stays `'static` and the app is reachable through
/// [`UiTest::app`] while the harness is alive.
pub type ShowFn = fn(&mut GameTrimmerApp, &mut egui::Ui);

/// A harness plus the temp directory holding the app's throwaway database and ini.
///
/// Field order matters: `harness` owns the app, which rewrites the ini on
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
        let dir = tempfile::tempdir().expect("create temp dir for portable test files");
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

    /// Runs a fixed number of frames instead of waiting for the UI to settle.
    ///
    /// For states that never settle *by design*: a spinner asks for a repaint
    /// every frame, so [`Self::run`] would report it as a repaint loop - and
    /// rightly so, since that check is what catches the accidental ones. An
    /// animated state gets its own entry point rather than loosening `run`
    /// for every test.
    pub fn run_animated(&mut self) {
        self.harness.run_steps(ANIMATED_STEPS);
    }

    /// Draws exactly one frame and reports how long it took.
    ///
    /// One frame, not [`Self::run`]'s settle loop: a budget stated over "until
    /// it settles" measures how many frames egui asked for as much as it
    /// measures the cost of each, so a layout that needs one extra frame reads
    /// as a slowdown it is not. What the user waits on is a single repaint.
    pub fn time_frame(&mut self) -> std::time::Duration {
        let started = std::time::Instant::now();
        self.harness.run_steps(1);
        started.elapsed()
    }

    /// The fastest of `runs` single frames drawn in the current state.
    ///
    /// The minimum rather than the mean: this is a shared, noisy developer
    /// machine, and every source of noise here (a scheduler preemption, a
    /// background build, a stray GC in some other process) only ever makes a
    /// frame *slower*. The floor is the closest thing to the cost of the code
    /// itself, and a budget compared against it fails only when the code got
    /// slower - not when the machine was busy.
    pub fn fastest_frame(&mut self, runs: usize) -> std::time::Duration {
        (0..runs.max(1))
            .map(|_| self.time_frame())
            .min()
            .expect("at least one run")
    }

    pub fn app(&self) -> &GameTrimmerApp {
        self.harness.state()
    }

    /// How many times this harness has actually read the rules packs from
    /// disk - see `ui::settings::rules::pack_disk_reads`.
    pub fn pack_disk_reads(&self) -> usize {
        crate::ui::settings::rules::pack_disk_reads(&self.harness.ctx)
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
    ///
    /// Counts rather than queries for a single node: a repeated label is
    /// normal in this UI (every immediate setting carries the same "applies
    /// now" badge), and `query_by_label` *panics* when more than one matches.
    /// A presence check has no business caring how many there are.
    pub fn has_label(&self, label: &str) -> bool {
        self.count_labels(label) > 0
    }

    /// How many widgets carry exactly this label - for the cases where the
    /// repetition is the claim, such as "every row states when it applies".
    pub fn count_labels(&self, label: &str) -> usize {
        self.harness.query_all_by_label(label).count()
    }

    /// How many widgets carry this text anywhere in their label - for the
    /// claims about a name being drawn once when the widget that draws it
    /// surrounds it with something else ("\u{2715} Ukrainian (uk)").
    pub fn count_labels_containing(&self, text: &str) -> usize {
        self.harness.query_all_by_label_contains(text).count()
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

    /// Asserts some widget's label contains this text - for messages built by
    /// `format!`, where the test cares about the part that carries the
    /// information rather than the whole rendered sentence.
    #[track_caller]
    pub fn assert_label_containing(&self, text: &str) {
        assert!(
            self.count_labels_containing(text) > 0,
            "expected a widget whose label contains {text:?} to be on screen",
        );
    }

    /// The other half: nothing on screen mentions this text at all.
    #[track_caller]
    pub fn assert_no_label_containing(&self, text: &str) {
        assert!(
            self.count_labels_containing(text) == 0,
            "did not expect any widget label to contain {text:?}",
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
    ///
    /// The two games come from *different* launchers and libraries on that one
    /// shared disk (see [`SEEDED_LIBRARIES`]). That is what makes the grouping
    /// axes tell each other apart here: grouping by disk yields one branch and
    /// grouping by launcher or library yields two, so a switcher that silently
    /// did nothing cannot pass.
    pub fn seed_findings(&mut self) {
        self.seed_corpus(2, 1);
    }

    /// A corpus of `games` games with `files_per_game` findings each, for the
    /// measurements that are about *volume* rather than about which row says
    /// what: a frame's cost is dominated by how many findings are behind it,
    /// and two of them measure nothing.
    ///
    /// Same row shape as [`Self::seed_findings`], which is the two-game case
    /// of it - so a timing test and a labelling test disagree about size and
    /// about nothing else.
    pub fn seed_many_findings(&mut self, games: usize, files_per_game: usize) {
        self.seed_corpus(games, files_per_game);
    }

    fn seed_corpus(&mut self, games: usize, files_per_game: usize) {
        let app = self.app_mut();
        // Findings on screen mean a user who is past the first-run screen:
        // they scanned once, and the disclaimer that gates scanning is
        // accepted. Without both, `ui::onboarding` takes over the central
        // panel and every tree assertion below is about the wrong screen.
        app.accept_disclaimer();
        app.mark_scan_started();
        app.findings = (0..(games * files_per_game) as i64)
            .map(|i| {
                let game = i / files_per_game as i64;
                crate::model::FindingItem {
                    row: crate::model::FindingRow {
                        file_id: i,
                        game_id: game,
                        game_name: format!("Test Game {game}"),
                        // These two stand for ordinary launcher-known games, which
                        // is what the seeded libraries say they are. Leaving this
                        // `None` would make both of them the *unclaimed* kind -
                        // folder-scan or hand-added - and the tree marks those with
                        // a trailing diamond (GT-38), which would quietly change
                        // every row label these tests look up by name.
                        app_id: Some(format!("{}", 100 + game)),
                        install_dir: std::path::PathBuf::from("C:\\Games\\Test"),
                        library: Some(crate::model::LibraryOrigin {
                            vendor: Some(
                                SEEDED_LIBRARIES[game as usize % SEEDED_LIBRARIES.len()]
                                    .0
                                    .to_string(),
                            ),
                            root: std::path::PathBuf::from(
                                SEEDED_LIBRARIES[game as usize % SEEDED_LIBRARIES.len()].1,
                            ),
                        }),
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
                        deletion_block_reason: None,
                        imported_untrusted: false,
                        action: gametrimmer_core::models::FindingAction::DirectDelete,
                        anti_cheat_protected: false,
                        monolith_badge: None,
                    },
                    selected: true,
                    removed: false,
                }
            })
            .collect();
        app.rebuild_tree();
        // What `WorkerMsg::Done` does after swapping in a real result set:
        // fold the search corpus over the new findings. Assigning `findings`
        // here without it would leave the name search looking at an empty
        // corpus, so every search assertion would pass or fail for the wrong
        // reason.
        app.clear_search();
        self.run();
    }

    /// Moves the pointer over the widget with this label and settles, so a
    /// tooltip (including `on_disabled_hover_text`) has a frame to appear in.
    ///
    /// Scrolls the widget into view first, for the same reason [`Self::click`]
    /// does: egui discards pointer events outside a scroll area's clip rect,
    /// so hovering a widget that is in the accessibility tree but below the
    /// fold produces no tooltip and a failure that blames the tooltip.
    #[track_caller]
    pub fn hover(&mut self, label: &str) {
        self.node(label).scroll_to_me();
        self.run();
        self.node(label).hover();
        self.run();
    }

    /// Sends a key press and runs until the UI settles.
    pub fn press(&mut self, key: egui::Key) {
        self.harness.key_press(key);
        self.run();
    }

    /// Gives keyboard focus to the only widget with this accessibility role.
    /// Useful for controls such as an unlabelled text edit whose visible text
    /// is a placeholder rather than an accessibility label.
    #[track_caller]
    pub fn focus_only_role(&mut self, role: egui::accesskit::Role) {
        {
            let mut nodes = self.harness.query_all_by_role(role);
            let node = nodes
                .next()
                .unwrap_or_else(|| panic!("no widget with role {role:?} on screen"));
            assert!(
                nodes.next().is_none(),
                "more than one widget with role {role:?} is on screen",
            );
            node.focus();
        }
        self.run();
    }

    /// Sends text to whichever widget currently owns keyboard focus.
    pub fn type_text(&mut self, text: &str) {
        self.harness.event(egui::Event::Text(text.to_owned()));
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
    /// it claims to test. This dialog has two nested scroll areas, so that is
    /// the common case here, not a corner case.
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

    /// Clicks at an absolute position instead of on a named widget - for the
    /// claims that are about *space* rather than about a control: "the empty
    /// part of this row is still the row" (whole-row interaction).
    ///
    /// Mirrors what [`egui_kittest::Node::click`] does at a node's centre:
    /// move the pointer there first, then press and release. egui resolves a
    /// click against the position it last saw, so the move is not optional.
    pub fn click_at(&mut self, pos: egui::Pos2) {
        self.harness.event(egui::Event::PointerMoved(pos));
        for pressed in [true, false] {
            self.harness.event(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            });
        }
        self.run();
    }

    /// Clicks the `n`-th checkbox on screen, counting in render order.
    ///
    /// Checkboxes here are deliberately label-less (the row's name is next to
    /// them, and repeating it would read twice to a screen reader), so they
    /// cannot be reached by [`Self::click`]. Looked up by accessibility role
    /// and clicked by position, which is also what makes it a real test of
    /// hit-testing rather than of the accessibility tree.
    pub fn nth_checkbox_rect(&self, n: usize) -> egui::Rect {
        self.harness
            .query_all_by_role(egui::accesskit::Role::CheckBox)
            .nth(n)
            .unwrap_or_else(|| panic!("fewer than {} checkboxes on screen", n + 1))
            .rect()
    }

    /// How many checkboxes are on screen. Every tree row carries exactly one,
    /// so folding a group takes its children's away - which is how a test
    /// tells "the group is still open" apart from "that click collapsed it".
    pub fn checkbox_count(&self) -> usize {
        self.harness
            .query_all_by_role(egui::accesskit::Role::CheckBox)
            .count()
    }

    /// Hovers the `n`-th checkbox on screen (see [`Self::nth_checkbox_rect`]),
    /// so its `on_disabled_hover_text` tooltip has a frame to appear in.
    /// Position-based like [`Self::nth_checkbox_rect`] rather than
    /// label-based like [`Self::hover`]: these checkboxes are deliberately
    /// label-less (see that method's doc comment), so they cannot be reached
    /// by accessibility label at all.
    #[track_caller]
    pub fn hover_nth_checkbox(&mut self, n: usize) {
        {
            let node = self
                .harness
                .query_all_by_role(egui::accesskit::Role::CheckBox)
                .nth(n)
                .unwrap_or_else(|| panic!("fewer than {} checkboxes on screen", n + 1));
            node.hover();
        }
        self.run();
    }

    /// How many combo boxes currently display exactly this selection.
    ///
    /// A combo box carries its selected text as an accessibility *value*, not
    /// as a label, so [`Self::has_label`] never finds it - which is why the
    /// category filter on the summary row went untested until the grouping
    /// switcher landed beside it.
    pub fn count_combo_values(&self, value: &str) -> usize {
        self.combo_nodes(value).count()
    }

    /// Asserts some combo box is showing this selection.
    #[track_caller]
    pub fn assert_combo_value(&self, value: &str) {
        assert!(
            self.count_combo_values(value) > 0,
            "expected a combo box showing {value:?} to be on screen",
        );
    }

    /// The other half: no combo box is showing this selection.
    #[track_caller]
    pub fn assert_no_combo_value(&self, value: &str) {
        assert_eq!(
            self.count_combo_values(value),
            0,
            "did not expect a combo box showing {value:?}",
        );
    }

    /// Opens the combo box currently showing `value`, so its entries can be
    /// clicked by label the way a user picks one.
    #[track_caller]
    pub fn open_combo(&mut self, value: &str) {
        {
            let mut nodes = self.combo_nodes(value);
            let node = nodes
                .next()
                .unwrap_or_else(|| panic!("no combo box showing {value:?} on screen"));
            assert!(
                nodes.next().is_none(),
                "more than one combo box is showing {value:?}",
            );
            node.click();
        }
        self.run();
    }

    fn combo_nodes<'s>(&'s self, value: &'s str) -> impl Iterator<Item = egui_kittest::Node<'s>> {
        self.harness
            .query_all_by_role(egui::accesskit::Role::ComboBox)
            .filter(move |node| node.value().as_deref() == Some(value))
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

    /// Each harness must own both portable state files, or tests running in
    /// parallel would interfere through scan data or persisted settings.
    #[test]
    fn each_harness_gets_its_own_database_and_ini() {
        fn show(_app: &mut GameTrimmerApp, _ui: &mut egui::Ui) {}

        let one = UiTest::new(show);
        let two = UiTest::new(show);

        assert_ne!(one.app().db_path(), two.app().db_path());
        assert_ne!(one.app().settings_path(), two.app().settings_path());
        assert_eq!(one.app().db_error, None);
        assert_eq!(two.app().db_error, None);
    }
}
