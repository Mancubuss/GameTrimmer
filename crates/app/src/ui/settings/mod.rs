//! The settings dialog: a left-nav list of five sections plus one right-hand
//! scroll area. Replaces the single modal with a `ScrollArea` nested inside
//! another `ScrollArea` and a collapsed "Advanced" page (audit §6.2, §6.3).
//! Every change is applied and persisted immediately by the section that owns
//! it, so "Done" only dismisses the dialog - there is no save step to forget.
//! The footer says whether the last write actually landed, because the
//! warnings list that would otherwise report a failure is behind this modal.
//!
//! One module per section, each a plain `show(app, ui)` the harness can drive
//! directly. They were ported one per commit onto this frame, in ascending
//! order of risk, so the largest landed on geometry four others had already
//! held stable. See `docs/ui-redesign-plan-2026-07-31.md` §5.
//!
//! # Why the section viewport has a fixed height
//!
//! The viewport is sized from the window alone ([`content_viewport_height`])
//! and is identical for every section, so the dialog never changes size when
//! the user switches tabs. Sizing it to the active section - egui's default,
//! since `ScrollArea` auto-shrinks to its content and `Ui::scope_dyn`
//! allocates a child's *used* rect rather than its max rect - made the modal
//! resize on every click, and a modal anchored `CENTER_CENTER` re-centers as
//! it resizes, so the content visibly jumped. Picking a `max_height` that fit
//! each section was the wrong shape of fix: a ceiling bounds the tall section
//! but does not stop the short ones shrinking the frame.
//!
//! A fixed viewport also removes the whole class of scroll bugs seen here
//! before. Only "Scanning" is long enough to need scrolling; for every other
//! section the scroll range is zero and the offset is pinned at the top by
//! construction, so it cannot be left mid-content by a previous section or by
//! egui's scroll-to-keep-focus behaviour. The cost is empty space below the
//! short sections, which is what a stable settings pane looks like.
//!
//! # Why the scroll area's content is wrapped in `ui.vertical`
//!
//! `ScrollArea::show` renders its content in the *parent* `Ui`'s layout, and
//! the row below is allocated `left_to_right`. Without the wrap every section
//! body inherits a horizontal layout, and `Ui::indent` - used by the routing
//! and delete-method hints - asserts rather than degrading. That is the panic
//! on tab switch that killed the first attempt at this dialog; it took four
//! rounds of guessing and one harness run to find. See the plan's §2A.

mod data;
mod general;
mod rules;
mod scanning;
mod selection;

use eframe::egui;

use crate::app::GameTrimmerApp;
use crate::i18n;

/// Which section of the dialog is showing. UI-only, never persisted.
///
/// `Hash` is required: the scroll area's `id_salt` is keyed on the section so
/// each keeps its own offset, rather than a long section's offset leaking
/// into a short one and rendering it already scrolled past its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsSection {
    General,
    Scanning,
    Selection,
    Rules,
    Data,
}

impl SettingsSection {
    /// Every section, in the order the left nav lists them.
    pub const ORDER: [SettingsSection; 5] = [
        SettingsSection::General,
        SettingsSection::Scanning,
        SettingsSection::Selection,
        SettingsSection::Rules,
        SettingsSection::Data,
    ];

    pub fn label(self, s: &i18n::Strings) -> &'static str {
        match self {
            SettingsSection::General => s.settings_section_general,
            SettingsSection::Scanning => s.settings_section_scanning,
            SettingsSection::Selection => s.settings_section_selection,
            SettingsSection::Rules => s.settings_section_rules,
            SettingsSection::Data => s.settings_section_data,
        }
    }
}

/// The close button's glyph. A multiplication sign rather than the letter x
/// or a box-drawing cross: it is the shape Windows uses, and it is inside the
/// bundled UI font's coverage (only the CJK/symbol fallbacks reach further -
/// see `ui::top_bar`'s note on the spinner).
const CLOSE_GLYPH: &str = "\u{2715}";

/// Width the left nav column claims, fixed regardless of dialog width - the
/// section labels are short and translated text stays well under this even
/// with the +30% length headroom the audit calls for.
const NAV_WIDTH_PX: f32 = 160.0;

/// Height of the section viewport for a window `window_height` points tall.
///
/// Everything the modal draws outside the viewport - the frame's own margins,
/// the heading row, the spacing above and below the viewport, the footer
/// separator and its button row - is covered by `CHROME_PX`, deliberately
/// larger than that adds up to at default spacing. The slack keeps the modal
/// comfortably inside the window instead of overflowing a centered anchor and
/// losing its top edge off-screen.
///
/// Pure, so the window-fit invariant is testable without a live context.
fn content_viewport_height(window_height: f32) -> f32 {
    const CHROME_PX: f32 = 190.0;
    /// Below this the viewport stops shrinking and the modal is allowed to
    /// overflow instead - a window that small cannot show a usable dialog
    /// either way, and a viewport of a few points would be worse.
    const MIN_PX: f32 = 160.0;
    /// Past this, extra window height goes to the desktop rather than to an
    /// ever-taller dialog; only "Scanning" has enough content to fill it.
    const MAX_PX: f32 = 620.0;
    (window_height - CHROME_PX).clamp(MIN_PX, MAX_PX)
}

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    if !app.show_settings {
        return;
    }

    let s = i18n::strings(app.lang());
    let mut close = false;

    let settings_modal = egui::Modal::new(egui::Id::new("gt_settings")).show(ui.ctx(), |ui| {
        // The modal is drawn inside the single OS viewport, so it can never
        // be wider than the window - clamp the target width to whatever room
        // is available rather than letting a narrow window force cutoff.
        // Wider than the old single-column dialog: the nav column needs its
        // own width on top of the content area's.
        let content_rect = ui.ctx().content_rect();
        let target_width = 640.0_f32.min((content_rect.width() - 40.0).max(280.0));
        ui.set_min_width(target_width);

        // The heading row carries the close affordance every other window on
        // this desktop puts in the same corner. "Done" in the footer already
        // dismissed the dialog, but a modal with no visible way out at the
        // top-right reads as one you are stuck in - and the two Escape/
        // backdrop routes out of it are invisible by nature.
        ui.horizontal(|ui| {
            ui.heading(s.settings_heading);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(CLOSE_GLYPH).on_hover_text(s.btn_close).clicked() {
                    close = true;
                }
            });
        });
        ui.add_space(8.0);

        // Nav column, divider and section viewport share one row of exactly
        // `viewport_height`. Allocating the row (rather than `ui.horizontal`,
        // which starts one interact-height tall and grows to whatever lands
        // in it) is also what gives the divider its full height: `Separator`
        // in a horizontal layout takes the *currently available* height,
        // which without this is only as tall as the nav labels beside it.
        let viewport_height = content_viewport_height(content_rect.height());
        ui.allocate_ui_with_layout(
            egui::vec2(target_width, viewport_height),
            egui::Layout::left_to_right(egui::Align::Min),
            |ui| {
                // Belt and braces: the enclosing scope allocates this row's
                // *used* rect, so this pins the height even if a future child
                // stops filling it on its own.
                ui.set_min_height(viewport_height);

                ui.vertical(|ui| {
                    ui.set_width(NAV_WIDTH_PX);
                    for section in SettingsSection::ORDER {
                        let selected = app.settings_section == section;
                        if ui.selectable_label(selected, section.label(s)).clicked() {
                            app.settings_section = section;
                        }
                    }
                });

                ui.separator();

                // Exactly one scroll area, never nested - the audit's core
                // complaint about the old dialog.
                egui::ScrollArea::vertical()
                    .id_salt(("gt_settings_scroll", app.settings_section))
                    .auto_shrink([false; 2])
                    .max_height(viewport_height)
                    // Always visible rather than only-when-needed: keeps the
                    // content width identical across sections, and a section
                    // that overflows never clips with zero affordance the way
                    // the pre-rebuild dialog did.
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                    .show(ui, |ui| {
                        // See the module docs: without this the section body
                        // inherits the row's horizontal layout and `indent`
                        // panics.
                        ui.vertical(|ui| show_section(app, ui));
                    });
            },
        );

        // Outside the scroll area, so it stays visible and clickable no
        // matter how far the section content above has been scrolled.
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button(s.btn_done).clicked() {
                close = true;
            }
            show_save_state(app, ui, s);
        });
    });

    // Escape (when this is the top modal) or a click on the backdrop
    // dismisses the dialog, same as "Done" - closing is non-destructive
    // because every setting is already applied and persisted.
    if settings_modal.should_close() {
        close = true;
    }

    if close {
        app.show_settings = false;
        // A stale export/import or database-maintenance result must not greet
        // the user on the next open; the top-bar status line keeps the last
        // success anyway. Same for the save indicator: reopening the dialog
        // must not claim to have just saved something.
        app.rules_io_result = None;
        app.db_maint_result = None;
        app.settings_saved = false;
        app.settings_save_error = None;
    }
}

/// Success green for a finished job or a healthy file, matching the colour
/// the old dialog used. Not one of `visuals()`'s own colours: this has to
/// read as "done", not as "clickable", in both themes.
const SUCCESS_GREEN: egui::Color32 = egui::Color32::from_rgb(0x4c, 0xaf, 0x50);

/// The footer's report on the last settings write.
///
/// A failure stays until something saves; a success stays until the next
/// change replaces it or the dialog closes. Neither fades on a timer: an
/// indicator that erases itself while the user is still reading it is worse
/// than one that simply describes the most recent write, and a self-driving
/// fade would keep the whole dialog repainting for no other reason.
///
/// The asymmetry that does matter is which one is loud. The dialog persists
/// every change as it is made, so the interesting case is the one where
/// that did not work - and the warnings list it also goes into sits on the
/// main window, behind this very modal.
fn show_save_state(app: &GameTrimmerApp, ui: &mut egui::Ui, s: &i18n::Strings) {
    if let Some(error) = &app.settings_save_error {
        ui.colored_label(ui.visuals().error_fg_color, error);
    } else if app.settings_saved {
        ui.colored_label(SUCCESS_GREEN, s.label_saved);
    }
}

/// A setting's label with its "when does this take effect" badge beside it.
///
/// The badge is what the audit (§6.4) asked for: the old dialog applied some
/// switches the instant they changed and others only on the next scan, and
/// gave the user no way to tell which was which. Shared by every section so
/// the badge always sits in the same place relative to its control.
fn row_heading(ui: &mut egui::Ui, label: &str, badge: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.small(badge);
    });
    ui.add_space(4.0);
}

/// Re-exported so sections keep saying `super::danger_frame`. The frame
/// itself moved up to `crate::ui` once the first-run screen needed one too -
/// see its docs for why there is exactly one.
use crate::ui::danger_frame;

/// The active section's body. One module each, ported one per commit (plan
/// §5.1-5.5) onto a frame the geometry test below already held stable.
fn show_section(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    match app.settings_section {
        SettingsSection::General => general::show(app, ui),
        SettingsSection::Scanning => scanning::show(app, ui),
        SettingsSection::Selection => selection::show(app, ui),
        SettingsSection::Rules => rules::show(app, ui),
        SettingsSection::Data => data::show(app, ui),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ui::harness::UiTest;

    /// A label that appears only when this section's body is on screen -
    /// deliberately not the nav entry's own text, which is there either way
    /// and so cannot tell "the entry exists" from "the body rendered".
    fn body_marker(section: SettingsSection, s: &i18n::Strings) -> &'static str {
        match section {
            SettingsSection::General => s.app_language_label,
            SettingsSection::Scanning => s.libraries_header,
            SettingsSection::Selection => s.default_profile_label,
            SettingsSection::Rules => s.rules_pack_category_label,
            SettingsSection::Data => s.db_path_label,
        }
    }

    fn open_dialog() -> UiTest {
        let mut test = UiTest::new(show);
        test.app_mut().show_settings = true;
        test.run();
        test
    }

    /// **The gate for this whole rebuild.**
    ///
    /// Four consecutive attempts at this dialog shipped a version where
    /// switching sections moved or resized it, each fixing one cause and
    /// leaving the others: `auto_shrink` collapsing a short section onto its
    /// content, `scope_dyn` allocating a child's used rect rather than its
    /// max rect, and `Modal`'s `CENTER_CENTER` anchor re-centering on every
    /// height change. One assertion covers all three.
    ///
    /// Every section, in order, twice - the second pass catches state that
    /// only settles after a section has been visited once (a remembered
    /// scroll offset, a lazily created id).
    #[test]
    fn the_modal_does_not_move_or_resize_between_sections() {
        let mut test = open_dialog();
        let s = test.strings();
        let first = test.rect_of(s.settings_heading);

        for round in 0..2 {
            for section in SettingsSection::ORDER {
                test.click(section.label(s));

                let now = test.rect_of(s.settings_heading);
                assert_eq!(
                    (now.min.x, now.min.y, now.width(), now.height()),
                    (first.min.x, first.min.y, first.width(), first.height()),
                    "the dialog moved on {section:?} (round {round}): {first:?} -> {now:?}",
                );
            }
        }
    }

    /// Switching sections must not panic, and the footer must survive it.
    /// This is the assertion that would have caught the `Ui::indent` panic
    /// the first attempt shipped (plan §2A).
    #[test]
    fn every_section_renders_without_panicking() {
        let mut test = open_dialog();
        let s = test.strings();

        for section in SettingsSection::ORDER {
            test.click(section.label(s));
            test.assert_label(s.settings_heading);
            test.assert_label(s.btn_done);

            // The active section's body rendered, and no other section's did:
            // the nav is a switch, not an accordion.
            test.assert_label(body_marker(section, s));
            for other in SettingsSection::ORDER {
                if other != section {
                    test.assert_no_label(body_marker(other, s));
                }
            }
        }
    }

    /// The nav is a switch, not an accordion: every entry stays reachable in
    /// one click from any other, and clicking one activates exactly it.
    #[test]
    fn every_section_is_one_click_away_from_every_other() {
        let mut test = open_dialog();
        let s = test.strings();

        for from in SettingsSection::ORDER {
            for to in SettingsSection::ORDER {
                test.click(from.label(s));
                test.click(to.label(s));

                assert_eq!(
                    test.app().settings_section,
                    to,
                    "could not reach {to:?} directly from {from:?}",
                );
                for entry in SettingsSection::ORDER {
                    test.assert_label(entry.label(s));
                }
            }
        }
    }

    #[test]
    fn nothing_renders_while_the_dialog_is_closed() {
        let mut test = UiTest::new(show);
        test.run();

        let s = test.strings();
        test.assert_no_label(s.settings_heading);
        test.assert_no_label(s.btn_done);
    }

    #[test]
    fn done_dismisses_the_dialog() {
        let mut test = open_dialog();
        let s = test.strings();

        test.click(s.btn_done);

        assert!(!test.app().show_settings);
    }

    /// The close button in the corner every window on this desktop puts one
    /// in. Asserted through the same state "Done" is asserted through, so the
    /// two are provably the same exit rather than two similar-looking ones.
    #[test]
    fn the_corner_close_button_dismisses_the_dialog() {
        let mut test = open_dialog();

        test.click(CLOSE_GLYPH);

        assert!(!test.app().show_settings);
    }

    /// It has to be in the *corner*, not merely on screen: above the nav
    /// column, and to the right of the heading it shares a row with.
    #[test]
    fn the_close_button_sits_in_the_top_right_corner() {
        let test = open_dialog();
        let s = test.strings();

        let close = test.rect_of(CLOSE_GLYPH);
        let heading = test.rect_of(s.settings_heading);
        let first_section = test.rect_of(SettingsSection::ORDER[0].label(s));

        assert!(
            close.min.x > heading.max.x,
            "the close button ({close:?}) is not right of the heading ({heading:?})",
        );
        assert!(
            close.max.y < first_section.min.y,
            "the close button ({close:?}) is not above the nav ({first_section:?})",
        );
    }

    /// Closing from the corner has to clear the same transient state "Done"
    /// clears, or a stale save indicator greets the user on the next open.
    #[test]
    fn the_corner_close_button_clears_the_transient_state_too() {
        let mut test = open_dialog();
        let s = test.strings();
        test.click(s.settings_section_data);
        test.click(s.logging_checkbox);
        test.assert_label(s.label_saved);

        test.click(CLOSE_GLYPH);
        test.app_mut().show_settings = true;
        test.run();

        test.assert_no_label(s.label_saved);
    }

    /// A write that failed has to be visible where the change was made. It
    /// also lands in the warnings list, but that is on the main window,
    /// behind this modal.
    #[test]
    fn a_failed_save_is_reported_in_the_dialog() {
        let mut test = open_dialog();
        test.app_mut().settings_save_error = Some("gt_save_failed_probe".to_string());
        test.run();

        test.assert_label("gt_save_failed_probe");
    }

    /// Changing a setting says so. Before this the dialog looked identical
    /// whether the write reached the database or not.
    #[test]
    fn a_successful_change_acknowledges_itself() {
        let mut test = open_dialog();
        let s = test.strings();
        test.assert_no_label(s.label_saved);

        test.click(s.settings_section_data);
        test.click(s.logging_checkbox);

        test.assert_label(s.label_saved);
        assert!(
            test.app().settings_save_error.is_none(),
            "the temp database should accept the write",
        );
    }

    /// Reopening the dialog must not claim to have just saved something.
    #[test]
    fn the_save_indicator_does_not_survive_a_reopen() {
        let mut test = open_dialog();
        let s = test.strings();
        test.click(s.settings_section_data);
        test.click(s.logging_checkbox);
        test.assert_label(s.label_saved);

        test.click(s.btn_done);
        test.app_mut().show_settings = true;
        test.run();

        test.assert_no_label(s.label_saved);
    }

    /// Mirrors `CHROME_PX` - the budget the viewport leaves for everything
    /// the modal draws around it.
    const CHROME_PX: f32 = 190.0;

    /// The whole point of the chrome budget: at any window height that can
    /// host the dialog at all, viewport + chrome must fit inside it, or a
    /// centered modal loses its top edge off-screen.
    #[test]
    fn viewport_leaves_room_for_the_dialog_chrome() {
        for window_height in [350, 500, 720, 900, 1080, 1440, 2160] {
            let window_height = window_height as f32;
            let total = content_viewport_height(window_height) + CHROME_PX;
            assert!(
                total <= window_height,
                "window {window_height}: viewport + chrome = {total} overflows",
            );
        }
    }

    /// A 4K-tall settings dialog is not the goal - past the cap the extra
    /// height goes to the desktop.
    #[test]
    fn viewport_stops_growing_on_very_tall_windows() {
        assert_eq!(
            content_viewport_height(1440.0),
            content_viewport_height(2160.0),
        );
    }

    /// Below the floor the modal is allowed to overflow instead; a viewport
    /// of a few points would show nothing at all.
    #[test]
    fn viewport_never_collapses_on_a_tiny_window() {
        assert!(content_viewport_height(200.0) >= 160.0);
        assert!(content_viewport_height(0.0) >= 160.0);
    }

    #[test]
    fn viewport_never_shrinks_as_the_window_grows() {
        let mut previous = 0.0_f32;
        for window_height in (0..2400).step_by(37) {
            let height = content_viewport_height(window_height as f32);
            assert!(
                height >= previous,
                "window {window_height}: {height} < previous {previous}",
            );
            previous = height;
        }
    }
}
