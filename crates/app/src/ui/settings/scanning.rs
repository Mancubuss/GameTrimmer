//! "Scanning": what gets scanned at all - the registered libraries, the
//! languages the localization detector never flags, the artifact categories
//! the scan keeps findings for, and a read-only report of how the last scan
//! read files from disk.
//!
//! Four choices keep the old dialog's behavior understandable:
//!
//! * The keep-list is chips plus a search box rather than 36 checkboxes in a
//!   wrapped block. Only the kept languages are on screen; the rest
//!   are one search away.
//! * Categories are a table with a risk column and what the default profile
//!   does with each, instead of bare checkboxes that said nothing about what
//!   turning one off would cost.
//! * The last remaining language or category renders **disabled with a
//!   reason on hover** instead of accepting the click and silently reverting
//!   it. A control that ignores you reads as broken; one that explains
//!   itself reads as a floor.
//! * File enumeration reports, it does not ask. It used to be three radio
//!   buttons; the routing is decided per volume from the device's own seek
//!   penalty, and neither override was worth its place on screen - see
//!   [`show_routing`].

use std::path::Path;

use eframe::egui;

use gametrimmer_core::langdetect::LangData;
use gametrimmer_core::providers;

use crate::app::GameTrimmerApp;
use crate::i18n;
use crate::model::{
    self, category_display, category_enabled, category_risk, category_ui_key, CATEGORY_ORDER,
};
use crate::ui::{gated_button, row_actions};
use crate::worker::manual::{LibraryRow, MANUAL_VENDOR};

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
///
/// Every library, manual or vendor-detected, can instead be *excluded* -
/// unlike Remove, this leaves the `game_libraries` row and the row on screen
/// alone, toggle off. A library that vanished from the list on exclude would
/// just be Remove wearing a different label, and Remove already exists (and
/// is correct) for manual libraries specifically because a re-scan brings a
/// vendor-detected one straight back. See
/// `gametrimmer_core::settings::Settings::excluded_libraries`.
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

    show_standalone_candidates(app, ui);
    ui.add_space(4.0);

    if app.libraries.is_empty() {
        ui.label(s.no_libraries_registered);
        return;
    }

    let mut to_remove = None;
    let mut excluded = app.settings.excluded_libraries.clone();
    for library in &app.libraries {
        ui.horizontal(|ui| {
            ui.label(format!("[{}]", library.vendor));
            ui.label(row_actions::windows_path_string(&library.path));
            ui.label(model::format_size(
                lang,
                app.occupancy.library_bytes(library.id),
            ));

            let key = providers::comparable_path(&library.path);
            let included = !excluded.iter().any(|excluded_key| excluded_key == &key);
            // The floor: excluding every library would leave the scan with
            // nothing to do (discovery already errors with
            // `no_libraries_found` on an empty set) - the last included
            // library refuses the click the same way the last keep-language
            // or category does, rather than accepting it and reverting.
            let blocked = (included && included_count(&app.libraries, &excluded) <= 1)
                .then_some(s.disabled_last_library);
            let mut checked = included;
            let response = ui.add_enabled(
                !app.busy && blocked.is_none(),
                egui::Checkbox::new(&mut checked, include_in_scan_label(s, &library.path)),
            );
            let response = match blocked {
                Some(reason) => response.on_disabled_hover_text(reason),
                None => response,
            };
            if response.changed() {
                excluded = toggled_exclusion(&excluded, &key, !checked);
            }

            if library.vendor == MANUAL_VENDOR
                && ui
                    .add_enabled(!app.busy, egui::Button::new(s.btn_remove))
                    .clicked()
            {
                to_remove = Some(library.id);
            }
        });
    }
    if excluded != app.settings.excluded_libraries {
        app.set_excluded_libraries(excluded);
    }
    if let Some(library_id) = to_remove {
        app.remove_manual_library(library_id);
    }
}

/// Games installed by their own installer, past every launcher - offered as
/// folders to add by hand, never added automatically.
///
/// Behind a button rather than run on open: this sweeps the whole Windows
/// uninstall registry, which is far too much work to repeat every frame, and
/// the answer only changes when the user installs something.
///
/// It lives here, under the library list, because that is what the answer *is*
/// -- a suggestion of libraries to register. It is not a new panel: the
/// interface is already dense, and a separate screen for "here are some
/// folders" would be out of proportion to what it does.
///
/// The wording never claims these are games. This cannot tell a game from a
/// driver, and pretending otherwise in a tool that deletes files would be the
/// expensive kind of wrong - so it says where they are and lets the user
/// decide. See `gametrimmer_core::standalone`.
fn show_standalone_candidates(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let lang = app.lang();
    let s = i18n::strings(lang);

    if ui
        .add_enabled(!app.busy, egui::Button::new(s.btn_find_standalone))
        .on_hover_text(s.find_standalone_hint)
        .clicked()
    {
        let known: Vec<std::path::PathBuf> = app
            .libraries
            .iter()
            .map(|library| std::path::PathBuf::from(&library.path))
            .collect();
        app.standalone_candidates = Some(gametrimmer_core::standalone::find_candidates(&known));
    }

    let Some(candidates) = app.standalone_candidates.clone() else {
        return;
    };

    ui.add_space(4.0);
    if candidates.is_empty() {
        // Said out loud rather than left blank: silence here reads as "the
        // search is broken", not as "there is nothing outside your launchers".
        ui.label(s.no_standalone_candidates);
        return;
    }

    ui.label(s.standalone_candidates_header);
    let mut to_add = None;
    for candidate in &candidates {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!app.busy, egui::Button::new(s.btn_add_folder))
                .clicked()
            {
                to_add = Some(candidate.install_dir.clone());
            }
            let label = match &candidate.publisher {
                Some(publisher) if !publisher.trim().is_empty() => {
                    format!("{} - {}", candidate.name, publisher)
                }
                _ => candidate.name.clone(),
            };
            ui.label(label)
                .on_hover_text(candidate.install_dir.display().to_string());
        });
    }
    if let Some(path) = to_add {
        app.add_library_path(path);
        // The list is now stale in exactly one row; re-running the sweep would
        // cost another full registry pass, so drop the offer instead - the
        // added folder is in the library list above, which is the answer.
        app.standalone_candidates = None;
    }
}

/// How many of the registered libraries are not in `excluded`.
fn included_count(libraries: &[LibraryRow], excluded: &[String]) -> usize {
    libraries
        .iter()
        .filter(|library| {
            let key = providers::comparable_path(&library.path);
            !excluded.iter().any(|excluded_key| excluded_key == &key)
        })
        .count()
}

/// The per-row checkbox's own accessibility label. It carries the path
/// rather than a bare "Include in scan" repeated on every row, the same
/// technique `remove_chip_label` uses for the keep-language chips: several
/// rows sharing one plain label would make [`crate::ui::harness::UiTest`]'s
/// label lookups (and a screen reader) unable to tell them apart.
fn include_in_scan_label(s: &i18n::Strings, path: &Path) -> String {
    format!(
        "{} {}",
        s.library_include_checkbox,
        row_actions::windows_path_string(path)
    )
}

/// The excluded-library list with one library's key added or removed.
/// Returns a new list rather than editing in place - mirrors `toggled` for
/// categories, minus that function's "empty means everything" convention:
/// `excluded_libraries` has no inverted meaning, so there is nothing to
/// materialize or collapse.
fn toggled_exclusion(excluded: &[String], key: &str, exclude: bool) -> Vec<String> {
    let mut next = excluded.to_vec();
    if exclude {
        if !next.iter().any(|excluded_key| excluded_key == key) {
            next.push(key.to_string());
        }
    } else {
        next.retain(|excluded_key| excluded_key != key);
    }
    next
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
                // would make that block decorative (protected-language editing).
                if code == ENGLISH {
                    continue;
                }
                // The button *is* the chip: its own label already names the
                // language (see `remove_chip_label`), so a label beside it
                // drew every kept language twice - single-label language chips.
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

/// English, alone inside a red frame directly under the chips (protected-language editing).
///
/// Why it is framed at all: localization findings are not split by resource
/// type yet, so dropping English from the keep-list makes the scanner propose
/// English *interface* files along with the voice-over and video that were
/// the point - and most games will not start without them. Until the spike
/// "resource-type localization split - splitting localization by resource type" answers whether rules
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
/// screen or to a screen reader - and so the name is drawn once (single-label language chips).
fn remove_chip_label(code: &str) -> String {
    format!("\u{2715} {}", i18n::lang_display_name(code))
}

/// Every built-in language not already kept, matching the typed text.
///
/// An empty query offers **nothing**, which is the whole point of the
/// rework: listing all 36 languages the moment the section opens is the
/// wall of checkboxes, merely rendered as buttons.
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

/// How the scanner enumerated files last time. Read-only by design: there
/// is nothing to choose here any more.
///
/// This block used to be three radio buttons. They did not survive the
/// question "who would want this". "Prefer the MFT index" bypassed only the
/// SSD speed heuristic, so its single effect was a ~40x slower scan on an
/// SSD; "Always walk folders" was really a way to stop the UAC prompt, and
/// now says so in the prompt itself (`ui::dialogs::show_elevation_prompt`).
/// What is left is the half that was actually informative: which route each
/// root took, and why - without it, a scan that quietly walked everything
/// because the app is not elevated looks identical to one that used the
/// index.
fn show_routing(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let s = i18n::strings(app.lang());

    ui.strong(s.scan_method_label);
    ui.add_space(4.0);
    ui.small(s.scan_method_hint);

    if !app.last_routing_breakdown.is_empty() {
        ui.add_space(6.0);
        ui.small(&app.last_routing_breakdown);
    }

    // The way back. Elevation is otherwise offered only by the startup modal
    // (`ui::dialogs::show_elevation_prompt`), and that modal is exactly what
    // "Don't ask again" switches off - which left the checkbox a one-way
    // door: the setting it writes is not editable here, so the only route
    // back was editing the ini by hand. The button is not gated on that
    // setting, because it is equally the way back from having dismissed the
    // modal for the session, and because "you turned this off" is a worse
    // thing for the screen to say than simply offering the action.
    if !app.elevated {
        ui.add_space(8.0);
        if ui
            .add_enabled(!app.busy, egui::Button::new(s.btn_relaunch_elevated))
            .clicked()
        {
            app.relaunch_elevated();
        }
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

    /// Registers two libraries directly on `app.libraries`, the same field
    /// `refresh_libraries` populates from the database - a UI test has no
    /// database, so this is the harness-level equivalent of having scanned
    /// once with a Steam library and a manually added one already known.
    fn seed_two_libraries(test: &mut UiTest) {
        test.app_mut().libraries = vec![
            LibraryRow {
                id: 1,
                vendor: "steam".to_string(),
                path: std::path::PathBuf::from(r"F:\SteamLibrary"),
            },
            LibraryRow {
                id: 2,
                vendor: MANUAL_VENDOR.to_string(),
                path: std::path::PathBuf::from(r"H:\itch.io"),
            },
        ];
        test.run();
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
        test.assert_label(s.scan_method_label);
    }

    /// The central claim of exclude-vs-remove: unchecking a library's toggle
    /// persists the exclusion and the row stays exactly where it was -
    /// unlike `remove_manual_library`, nothing disappears from the list.
    #[test]
    fn excluding_a_library_persists_and_the_row_stays_visible() {
        let mut test = open_scanning();
        let s = test.strings();
        seed_two_libraries(&mut test);

        let itch_path = Path::new(r"H:\itch.io");
        let label = include_in_scan_label(s, itch_path);
        test.click(&label);

        assert_eq!(
            test.app().settings.excluded_libraries,
            vec![providers::comparable_path(itch_path)],
        );
        // Still on screen, and the checkbox is still reachable by the same
        // label - excluding is not removing.
        test.assert_label(&label);
        test.assert_label(&row_actions::windows_path_string(itch_path));
    }

    /// Replaces the old dialog's silent-revert behaviour with the same floor
    /// the keep-list and category table already use: the last included
    /// library refuses the click and says why on hover, rather than
    /// accepting it and leaving the scan with nothing to do.
    #[test]
    fn the_last_included_library_cannot_be_excluded_and_explains_itself() {
        let mut test = open_scanning();
        let s = test.strings();
        seed_two_libraries(&mut test);

        let steam_path = Path::new(r"F:\SteamLibrary");
        let itch_path = Path::new(r"H:\itch.io");
        test.app_mut().settings.excluded_libraries = vec![providers::comparable_path(steam_path)];
        test.run();

        let label = include_in_scan_label(s, itch_path);
        test.click(&label);

        assert_eq!(
            test.app().settings.excluded_libraries,
            vec![providers::comparable_path(steam_path)],
            "the only included library must not have been excluded",
        );
        test.hover(&label);
        test.assert_label(s.disabled_last_library);
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

    /// single-label language chips: the chip drew its language twice - once as a plain label, then
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
    /// section opens would be the same wall of checkboxes,
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

    /// protected-language editing: English used to be an ordinary chip with an ordinary cross,
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

    /// Elevation has to be reachable from here, because the startup modal is
    /// the only other place that offers it and its "Don't ask again"
    /// checkbox turns that place off permanently. Without this button the
    /// checkbox is a one-way door: it writes a setting that no screen can
    /// edit, leaving the ini as the only route back.
    #[test]
    fn an_unelevated_session_can_still_reach_the_restart_offer() {
        let mut test = open_scanning();
        let s = test.strings();

        test.app_mut().elevated = false;
        test.app_mut().settings.never_ask_elevation = true;
        test.run();

        test.assert_label(s.btn_relaunch_elevated);
    }

    /// And it must not be on screen when it would do nothing: an already
    /// elevated process has nothing to relaunch into.
    #[test]
    fn the_restart_offer_is_absent_once_already_elevated() {
        let mut test = open_scanning();
        let s = test.strings();

        test.app_mut().elevated = true;
        test.run();

        test.assert_no_label(s.btn_relaunch_elevated);
    }

    /// The block explains what happened; it must not offer a choice about
    /// routing, because there is none. A stray routing control here would be
    /// a promise the scanner does not keep - the route is decided per volume
    /// from the device's own seek penalty, not from anything on this screen.
    #[test]
    fn the_scan_method_block_only_reports_and_never_offers_a_setting() {
        let test = open_scanning();
        let s = test.strings();

        test.assert_label(s.scan_method_label);
        test.assert_label(s.scan_method_hint);

        for gone in [
            "Auto",
            "Авто",
            "MFT index",
            "Always walk",
            "Завжди обходити",
        ] {
            assert!(
                !test.has_label(gone),
                "the retired routing control {gone:?} is still on screen",
            );
        }
    }
}
