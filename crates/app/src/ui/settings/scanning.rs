//! "Scanning": what gets scanned at all - the registered libraries, the
//! languages the localization detector never flags, the artifact categories
//! the scan keeps findings for, and how file enumeration routes between the
//! MFT index and a directory walk.
//!
//! Three things change relative to the old dialog, all from the audit:
//!
//! * The keep-list is chips plus a search box rather than 36 checkboxes in a
//!   wrapped block (§6.2). Only the kept languages are on screen; the rest
//!   are one search away.
//! * Categories are a table with a risk column and what the default profile
//!   does with each, instead of bare checkboxes that said nothing about what
//!   turning one off would cost.
//! * The last remaining language or category renders **disabled with a
//!   reason on hover** instead of accepting the click and silently reverting
//!   it. A control that ignores you reads as broken; one that explains
//!   itself reads as a floor.

use eframe::egui;

use gametrimmer_core::langdetect::LangData;
use gametrimmer_core::settings::ScanRouting;

use crate::app::GameTrimmerApp;
use crate::i18n;
use crate::model::{
    self, category_display, category_enabled, category_risk, category_ui_key, CATEGORY_ORDER,
};
use crate::ui::{gated_button, row_actions};
use crate::worker::manual::MANUAL_VENDOR;

/// Height the candidate list is allowed to take before it scrolls. Bounded
/// so a search with many hits cannot push the routing block off the section
/// viewport, which is a fixed height by design (see the module docs of
/// `ui::settings`).
const CANDIDATES_HEIGHT_PX: f32 = 120.0;

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    show_libraries(app, ui);
    separate(ui);
    show_keep_languages(app, ui);
    separate(ui);
    show_categories(app, ui);
    separate(ui);
    show_routing(app, ui);
}

fn separate(ui: &mut egui::Ui) {
    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);
}

/// The registered libraries, with the disk each one occupies. Only manually
/// added libraries can be removed - a vendor-detected one would simply come
/// back on the next scan.
fn show_libraries(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let lang = app.lang();
    let s = i18n::strings(lang);
    ui.strong(s.libraries_header);
    ui.add_space(4.0);

    if ui
        .add_enabled(
            !app.busy && !app.folder_picker_active,
            egui::Button::new(s.btn_add_folder),
        )
        .clicked()
    {
        app.start_add_library();
    }
    if app.folder_picker_active {
        ui.label(s.picking_folder);
    }
    ui.add_space(4.0);

    if app.libraries.is_empty() {
        ui.label(s.no_libraries_registered);
        return;
    }

    let mut to_remove = None;
    for library in &app.libraries {
        ui.horizontal(|ui| {
            ui.label(format!("[{}]", library.vendor));
            ui.label(row_actions::windows_path_string(&library.path));
            ui.label(model::format_size(
                lang,
                app.occupancy.library_bytes(library.id),
            ));
            if library.vendor == MANUAL_VENDOR
                && ui
                    .add_enabled(!app.busy, egui::Button::new(s.btn_remove))
                    .clicked()
            {
                to_remove = Some(library.id);
            }
        });
    }
    if let Some(library_id) = to_remove {
        app.remove_manual_library(library_id);
    }
}

/// The language every game's interface falls back to, and the one edit on
/// this screen that can leave a game unable to start - see
/// [`show_english_danger`].
const ENGLISH: &str = "en";

/// The keep-list: a chip per kept language, plus a search box over every
/// language the built-in pack knows.
fn show_keep_languages(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let s = i18n::strings(app.lang());
    super::row_heading(ui, s.keep_languages_label, s.badge_next_scan);

    let mut kept = app.settings.keep_languages.clone();
    // The floor: the detector would flag every language including the
    // user's own if the list were empty.
    let is_last = kept.len() <= 1;
    let busy = app.busy;

    ui.add_enabled_ui(!app.busy, |ui| {
        ui.horizontal_wrapped(|ui| {
            for code in kept.clone() {
                // English is not an ordinary chip - it has its own block
                // below, and leaving a second, unmarked way to remove it
                // would make that block decorative (GT-59).
                if code == ENGLISH {
                    continue;
                }
                // The button *is* the chip: its own label already names the
                // language (see `remove_chip_label`), so a label beside it
                // drew every kept language twice - GT-58.
                let blocked = is_last.then_some(s.disabled_last_keep_language);
                if gated_button(ui, &remove_chip_label(&code), blocked).clicked() {
                    kept.retain(|kept_code| kept_code != &code);
                }
            }
        });

        ui.add_space(6.0);
        ui.add(
            egui::TextEdit::singleline(&mut app.keep_language_query)
                .hint_text(s.keep_languages_add_placeholder)
                .desired_width(200.0),
        );

        let candidates = keep_language_candidates(&kept, &app.keep_language_query);
        if !candidates.is_empty() {
            egui::ScrollArea::vertical()
                .id_salt("gt_settings_keep_language_candidates")
                .max_height(CANDIDATES_HEIGHT_PX)
                .show(ui, |ui| {
                    // See the module docs of `ui::settings`: a scroll area
                    // renders in its parent's layout, and this one sits
                    // inside `add_enabled_ui`'s.
                    ui.vertical(|ui| {
                        for code in candidates {
                            if ui.button(i18n::lang_display_name(code)).clicked() {
                                kept.push(code.to_string());
                            }
                        }
                    });
                });
        }
    });
    ui.small(s.keep_languages_hint);

    ui.add_space(10.0);
    show_english_danger(ui, s, &mut kept, is_last, busy);

    if kept != app.settings.keep_languages {
        app.set_keep_languages(kept);
    }
}

/// English, alone inside a red frame directly under the chips (GT-59).
///
/// Why it is framed at all: localization findings are not split by resource
/// type yet, so dropping English from the keep-list makes the scanner propose
/// English *interface* files along with the voice-over and video that were
/// the point - and most games will not start without them. Until the spike
/// "GT-62 - splitting localization by resource type" answers whether rules
/// can tell those apart, this is the most dangerous path in the app, and it
/// used to be an ordinary cross indistinguishable from Spanish's.
///
/// Why it is framed *here* rather than in a global danger section: the rest
/// of the keep-list lives on this screen. Moving one language elsewhere would
/// mean managing languages in one place and English in another.
///
/// The block renders in both states on purpose. If it vanished once English
/// was dropped, the screen would stop showing the riskier of the two states,
/// and the only route back would be the search box - which is exactly where
/// someone who did this by accident would not think to look.
fn show_english_danger(
    ui: &mut egui::Ui,
    s: &i18n::Strings,
    kept: &mut Vec<String>,
    is_last: bool,
    busy: bool,
) {
    super::danger_frame(ui, s.danger_zone_label, |ui| {
        let is_kept = kept.iter().any(|code| code == ENGLISH);
        ui.small(if is_kept {
            s.keep_english_warning
        } else {
            s.keep_english_absent
        });
        ui.add_space(6.0);

        ui.add_enabled_ui(!busy, |ui| {
            if is_kept {
                let blocked = is_last.then_some(s.disabled_last_keep_language);
                if gated_button(ui, &remove_chip_label(ENGLISH), blocked).clicked() {
                    kept.retain(|code| code != ENGLISH);
                }
            } else if ui.button(s.btn_keep_english_again).clicked() {
                kept.push(ENGLISH.to_string());
            }
        });
    });
}

/// The chip: a remove button carrying the language name. The name is on the
/// button rather than beside it so a row of chips stays distinguishable - a
/// column of identical crosses says nothing about which one removes what, on
/// screen or to a screen reader - and so the name is drawn once (GT-58).
fn remove_chip_label(code: &str) -> String {
    format!("\u{2715} {}", i18n::lang_display_name(code))
}

/// Every built-in language not already kept, matching the typed text.
///
/// An empty query offers **nothing**, which is the whole point of the
/// rework: listing all 36 languages the moment the section opens is the
/// wall of checkboxes the audit objected to, merely rendered as buttons.
///
/// Matching runs over the display name, which is the language's own native
/// name with its code in brackets ("Français (fr)"), so typing either finds
/// it. The explicit code check covers a code from a future community pack
/// that has no native name in the table and displays bare.
///
/// Pure, so the search behaviour is testable without a frame.
fn keep_language_candidates<'a>(kept: &[String], query: &str) -> Vec<&'a str> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    LangData::builtin()
        .language_keys()
        .iter()
        .copied()
        .filter(|code| !kept.iter().any(|kept_code| kept_code == code))
        .filter(|code| {
            i18n::lang_display_name(code)
                .to_lowercase()
                .contains(&query)
                || code.to_lowercase().contains(&query)
        })
        .collect()
}

/// The category table: what is scanned, what removing it risks, and whether
/// the default profile would pre-select it.
fn show_categories(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let lang = app.lang();
    let s = i18n::strings(lang);
    super::row_heading(ui, s.categories_label, s.badge_next_scan);

    let mut picked = app.settings.enabled_categories.clone();
    let default_profile = app.settings.default_selection_profile;

    ui.add_enabled_ui(!app.busy, |ui| {
        egui::Grid::new("gt_settings_categories_grid")
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                ui.strong(s.categories_table_header_category);
                ui.strong(s.categories_table_header_risk);
                ui.strong(s.categories_table_header_profile_behavior);
                ui.end_row();

                for category in CATEGORY_ORDER {
                    let enabled = category_enabled(&picked, category);
                    // The floor, stated the same way the keep-list states
                    // it: the last enabled category cannot be switched off,
                    // and hovering says why.
                    let blocked = (enabled && enabled_count(&picked) <= 1)
                        .then_some(s.disabled_last_category);
                    let mut checked = enabled;
                    let response = ui.add_enabled(
                        blocked.is_none(),
                        egui::Checkbox::new(&mut checked, category_display(lang, category)),
                    );
                    let response = match blocked {
                        Some(reason) => response.on_disabled_hover_text(reason),
                        None => response,
                    };
                    if response.changed() {
                        picked = toggled(&picked, category, checked);
                    }

                    ui.label(i18n::risk_level_bare_label(lang, category_risk(category)));

                    // A best-case (confidence 100) projection: `Aggressive`
                    // and `Custom` also key off each file's own confidence,
                    // so this answers "can this profile ever pre-select the
                    // category", not "always will".
                    ui.label(
                        if model::profile_auto_selects(default_profile, category, 100) {
                            s.profile_behavior_auto
                        } else {
                            s.profile_behavior_manual
                        },
                    );
                    ui.end_row();
                }
            });
    });
    ui.small(s.categories_hint);

    if picked != app.settings.enabled_categories {
        app.set_enabled_categories(picked);
    }
}

/// How many categories the stored list actually enables. An empty list is
/// the "everything is on" convention, not "nothing is" - see
/// [`gametrimmer_core::settings::Settings::enabled_categories`].
fn enabled_count(picked: &[String]) -> usize {
    if picked.is_empty() {
        CATEGORY_ORDER.len()
    } else {
        picked.len()
    }
}

/// The stored list with one category switched on or off.
///
/// Returns a new list rather than editing in place, which is what makes the
/// two conventions testable: unchecking out of the empty "everything" list
/// has to materialize the rest explicitly, and re-checking the last missing
/// one has to collapse back to empty rather than persisting a list that
/// happens to name every category.
fn toggled(picked: &[String], category: model::DisplayCategory, checked: bool) -> Vec<String> {
    let key = category_ui_key(category);
    let mut next: Vec<String> = if picked.is_empty() {
        CATEGORY_ORDER
            .iter()
            .map(|&c| category_ui_key(c).to_string())
            .collect()
    } else {
        picked.to_vec()
    };

    if checked {
        if !next.iter().any(|id| id == key) {
            next.push(key.to_string());
        }
    } else {
        next.retain(|id| id != key);
    }

    if next.len() == CATEGORY_ORDER.len() {
        next.clear();
    }
    next
}

/// How the scanner enumerates files. The MFT index is far faster on a
/// spinning disk and pointless on an SSD, which is what `Auto` decides.
fn show_routing(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let s = i18n::strings(app.lang());
    let mut picked = app.settings.scan_routing;

    super::row_heading(ui, s.scan_routing_label, s.badge_next_scan);
    ui.add_enabled_ui(!app.busy, |ui| {
        for (routing, label, hint, id) in [
            (
                ScanRouting::Auto,
                s.scan_routing_auto_label,
                s.scan_routing_auto_hint,
                "gt_settings_routing_auto_hint",
            ),
            (
                ScanRouting::ForceMft,
                s.scan_routing_force_mft_label,
                s.scan_routing_force_mft_hint,
                "gt_settings_routing_mft_hint",
            ),
            (
                ScanRouting::ForceWalkdir,
                s.scan_routing_force_walkdir_label,
                s.scan_routing_force_walkdir_hint,
                "gt_settings_routing_walkdir_hint",
            ),
        ] {
            ui.radio_value(&mut picked, routing, label);
            ui.indent(id, |ui| {
                ui.small(hint);
            });
            ui.add_space(4.0);
        }
    });

    // What actually happened last time. "Prefer the MFT index" is a
    // preference, not a promise - without this a user who turned it on and
    // saw no speed-up had no way to learn that the app is not elevated, or
    // that their library sits on an SSD where walking is the faster route
    // anyway (audit §5.2).
    if !app.last_routing_breakdown.is_empty() {
        ui.add_space(6.0);
        ui.small(&app.last_routing_breakdown);
    }

    if picked != app.settings.scan_routing {
        app.set_scan_routing(picked);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ui::harness::UiTest;
    use crate::ui::settings::SettingsSection;

    fn open_scanning() -> UiTest {
        let mut test = UiTest::new(crate::ui::settings::show);
        test.app_mut().show_settings = true;
        test.app_mut().settings_section = SettingsSection::Scanning;
        test.run();
        test
    }

    /// Round 2 of the failed attempt shipped a "Scanning" tab that showed
    /// only the library list and a large blank gap below it. Every block has
    /// to be present.
    #[test]
    fn the_section_shows_all_four_blocks() {
        let test = open_scanning();
        let s = test.strings();

        test.assert_label(s.libraries_header);
        test.assert_label(s.keep_languages_label);
        test.assert_label(s.categories_label);
        test.assert_label(s.scan_routing_label);
    }

    #[test]
    fn the_category_table_has_a_row_per_category_with_risk_and_profile_behaviour() {
        let test = open_scanning();
        let s = test.strings();
        let lang = test.app().lang();

        test.assert_label(s.categories_table_header_category);
        test.assert_label(s.categories_table_header_risk);
        test.assert_label(s.categories_table_header_profile_behavior);

        for category in CATEGORY_ORDER {
            test.assert_label(category_display(lang, category));
        }
        assert_eq!(
            test.count_labels(s.profile_behavior_auto)
                + test.count_labels(s.profile_behavior_manual),
            CATEGORY_ORDER.len(),
            "every category row must say what the default profile does with it",
        );
    }

    /// The keep-list shows what is kept, not all 36 languages at once.
    ///
    /// Kept languages are looked up by their chip (the remove button, which
    /// carries the name) and offered ones by the bare name their candidate
    /// button uses, so the two states cannot be confused for one another.
    #[test]
    fn only_the_kept_languages_are_on_screen_until_a_search() {
        let mut test = open_scanning();
        let kept = test.app().settings.keep_languages.clone();
        assert!(kept.len() >= 2, "the default keep-list has more than one");

        for code in &kept {
            test.assert_label(&remove_chip_label(code));
        }
        let offered = LangData::builtin()
            .language_keys()
            .iter()
            .filter(|code| test.has_label(&i18n::lang_display_name(code)))
            .count();
        assert_eq!(
            offered, 0,
            "languages are offered for adding before anything was searched for",
        );

        test.app_mut().keep_language_query = "fran".to_string();
        test.run();
        test.assert_label(&i18n::lang_display_name("fr"));
    }

    /// GT-58: the chip drew its language twice - once as a plain label, then
    /// again inside the remove button whose label already names it, so the
    /// screen read "Ukrainian (uk)  \u{2715} Ukrainian (uk)".
    #[test]
    fn a_kept_language_is_named_once_per_chip() {
        let mut test = open_scanning();
        test.app_mut()
            .set_keep_languages(vec!["en".to_string(), "uk".to_string()]);
        test.run();

        for code in ["en", "uk"] {
            assert_eq!(
                test.count_labels_containing(&i18n::lang_display_name(code)),
                1,
                "the {code} chip names its language more than once",
            );
        }
    }

    /// Languages are listed under their own native name with the code in
    /// brackets, so both "Fran" and "fr" have to find French. And an already
    /// kept language must never be offered a second time.
    #[test]
    fn the_search_matches_native_names_and_codes_and_hides_what_is_kept() {
        let kept = vec!["en".to_string(), "de".to_string()];

        let by_name = keep_language_candidates(&kept, "fran");
        assert!(by_name.contains(&"fr"), "{by_name:?}");

        let by_code = keep_language_candidates(&kept, "fr");
        assert!(by_code.contains(&"fr"), "{by_code:?}");

        for candidates in [by_name, by_code] {
            assert!(
                !candidates.contains(&"de") && !candidates.contains(&"en"),
                "an already-kept language is offered again: {candidates:?}",
            );
        }
    }

    /// The rework's actual claim. Offering all 36 languages the moment the
    /// section opens would be the wall of checkboxes the audit objected to,
    /// merely rendered as buttons.
    #[test]
    fn an_empty_search_offers_nothing() {
        assert!(keep_language_candidates(&["en".to_string()], "").is_empty());
        assert!(keep_language_candidates(&["en".to_string()], "   ").is_empty());
    }

    /// Replaces the old dialog's behaviour, which accepted the click on the
    /// last language and silently put the checkbox back (plan §0.4). The
    /// control is now disabled and says why on hover.
    #[test]
    fn the_last_keep_language_cannot_be_removed_and_explains_itself() {
        let mut test = open_scanning();
        let s = test.strings();
        test.app_mut()
            .set_keep_languages(vec!["en".to_string(), "de".to_string()]);
        test.run();

        test.click(&remove_chip_label("de"));
        assert_eq!(test.app().settings.keep_languages, vec!["en".to_string()]);

        // One language left: the remove button is still there, still says
        // what it is, and now refuses with a reason.
        test.click(&remove_chip_label("en"));
        assert_eq!(test.app().settings.keep_languages, vec!["en".to_string()]);
        test.hover(&remove_chip_label("en"));
        test.assert_label(s.disabled_last_keep_language);
    }

    /// GT-59: English used to be an ordinary chip with an ordinary cross,
    /// beside Spanish and German, though removing it is the one edit here
    /// that can leave a game unable to start. The only way to take it off
    /// has to be the framed one.
    #[test]
    fn english_can_be_dropped_only_from_inside_the_danger_frame() {
        let mut test = open_scanning();
        let s = test.strings();
        test.app_mut()
            .set_keep_languages(vec!["en".to_string(), "de".to_string()]);
        test.run();

        test.assert_label(s.danger_zone_label);
        assert_eq!(
            test.count_labels(&remove_chip_label("en")),
            1,
            "English is offered in more than one place",
        );

        // Geometry, because "inside the frame" is a claim about where the
        // button is, not about which strings are on screen: English sits
        // below the danger heading and every ordinary chip above it.
        let heading = test.rect_of(s.danger_zone_label);
        let english = test.rect_of(&remove_chip_label("en"));
        let ordinary = test.rect_of(&remove_chip_label("de"));
        assert!(
            english.min.y > heading.min.y,
            "the English control is not under the danger heading: {english:?} vs {heading:?}",
        );
        assert!(
            ordinary.max.y < heading.min.y,
            "an ordinary chip ({ordinary:?}) is inside the danger frame ({heading:?})",
        );
    }

    /// The wording is the whole point of the block. The danger is deferred -
    /// nothing is deleted by the click - so a warning that claims otherwise
    /// is one the user can catch lying, and then the frame is worth nothing.
    #[test]
    fn the_warning_describes_the_deferred_danger() {
        let mut test = open_scanning();
        let s = test.strings();
        test.app_mut()
            .set_keep_languages(vec!["en".to_string(), "de".to_string()]);
        test.run();

        test.assert_label(s.keep_english_warning);
    }

    /// Both states render. If the block vanished with English, the screen
    /// would stop showing the riskier of the two, and the way back would be
    /// the search box - where someone who did this by accident will not look.
    #[test]
    fn the_block_still_says_so_once_english_is_gone_and_offers_the_way_back() {
        let mut test = open_scanning();
        let s = test.strings();
        test.app_mut()
            .set_keep_languages(vec!["en".to_string(), "de".to_string()]);
        test.run();

        test.click(&remove_chip_label("en"));
        assert_eq!(test.app().settings.keep_languages, vec!["de".to_string()]);
        test.assert_label(s.keep_english_absent);

        test.click(s.btn_keep_english_again);
        assert!(
            test.app()
                .settings
                .keep_languages
                .iter()
                .any(|code| code == "en"),
            "the way back did not put English on the keep-list",
        );
    }

    /// The floor still applies inside the frame: English alone on the list is
    /// still the last language, and the block has to refuse with the same
    /// reason the ordinary chips give rather than silently accept the click.
    #[test]
    fn the_framed_control_still_honours_the_last_language_floor() {
        let mut test = open_scanning();
        let s = test.strings();
        test.app_mut().set_keep_languages(vec!["en".to_string()]);
        test.run();

        test.click(&remove_chip_label("en"));

        assert_eq!(test.app().settings.keep_languages, vec!["en".to_string()]);
        test.hover(&remove_chip_label("en"));
        test.assert_label(s.disabled_last_keep_language);
    }

    /// The empty-list convention, both directions. Unchecking one category
    /// out of "everything enabled" must name the rest; re-checking the last
    /// missing one must collapse back to empty rather than persist a list
    /// naming every category.
    #[test]
    fn the_category_list_round_trips_through_the_everything_convention() {
        let first = CATEGORY_ORDER[0];

        let after_uncheck = toggled(&[], first, false);
        assert_eq!(after_uncheck.len(), CATEGORY_ORDER.len() - 1);
        assert!(!category_enabled(&after_uncheck, first));

        let after_recheck = toggled(&after_uncheck, first, true);
        assert!(
            after_recheck.is_empty(),
            "re-enabling everything must collapse to the empty form: {after_recheck:?}",
        );
        assert!(category_enabled(&after_recheck, first));
    }

    #[test]
    fn the_last_enabled_category_cannot_be_switched_off() {
        let mut test = open_scanning();
        let s = test.strings();
        let lang = test.app().lang();
        let last = CATEGORY_ORDER[0];
        test.app_mut()
            .set_enabled_categories(vec![category_ui_key(last).to_string()]);
        test.run();

        test.click(category_display(lang, last));

        assert_eq!(
            test.app().settings.enabled_categories,
            vec![category_ui_key(last).to_string()],
        );
        test.hover(category_display(lang, last));
        test.assert_label(s.disabled_last_category);
    }

    #[test]
    fn picking_a_routing_mode_persists_it() {
        let mut test = open_scanning();
        let s = test.strings();
        assert_eq!(test.app().settings.scan_routing, ScanRouting::Auto);

        test.click(s.scan_routing_force_walkdir_label);

        assert_eq!(test.app().settings.scan_routing, ScanRouting::ForceWalkdir);
    }

    /// Nothing to report is reported as nothing: a "0 roots walked" line
    /// after every clean scan would be noise.
    #[test]
    fn the_routing_diagnostics_appear_only_once_there_is_something_to_say() {
        let mut test = open_scanning();
        assert!(test.app().last_routing_breakdown.is_empty());
        test.assert_no_label("gt_routing_probe");

        test.app_mut().last_routing_breakdown = "gt_routing_probe".to_string();
        test.run();

        test.assert_label("gt_routing_probe");
    }

    /// The routing hints are `Ui::indent` calls - the construct that
    /// panicked in the first attempt when a section body inherited the nav
    /// row's horizontal layout (plan §2A). This is the tallest section, so
    /// they are also the most likely to need scrolling to reach.
    #[test]
    fn the_indented_routing_hints_render_instead_of_panicking() {
        let test = open_scanning();
        let s = test.strings();

        test.assert_label(s.scan_routing_auto_hint);
        test.assert_label(s.scan_routing_force_mft_hint);
        test.assert_label(s.scan_routing_force_walkdir_hint);
    }
}
