//! "General": app language, theme, and the acknowledgements.
//!
//! Both are applied and persisted the moment they change - `set_language`
//! writes the setting and every render call reads `app.lang()` fresh, and
//! `eframe::App::ui` pushes `settings.theme` into the context every frame. So
//! each row carries the "Immediately" badge and the section has no save step;
//! "Done" only dismisses the dialog.

use eframe::egui;

use gametrimmer_core::settings::{Lang, LanguagePreference, Theme};

use crate::app::GameTrimmerApp;
use crate::i18n;

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let s = i18n::strings(app.lang());
    let mut picked_lang = app.settings.app_language;
    let mut picked_theme = app.settings.theme;

    super::row_heading(ui, s.app_language_label, s.badge_immediately);
    // Persisting opens a fresh database connection, so the controls are
    // gated behind `!app.busy` rather than racing an in-flight worker's own
    // connection - the same gate the pre-rebuild dialog used.
    //
    // Three options rather than two, and in the same order as the theme row
    // below: "follow the OS" first, then the explicit overrides. The two rows
    // now ask the same shape of question, so they should not answer it in two
    // different layouts.
    ui.add_enabled_ui(!app.busy, |ui| {
        ui.radio_value(
            &mut picked_lang,
            LanguagePreference::System,
            s.lang_name_system,
        );
        ui.radio_value(
            &mut picked_lang,
            LanguagePreference::Fixed(Lang::En),
            s.lang_name_en,
        );
        ui.radio_value(
            &mut picked_lang,
            LanguagePreference::Fixed(Lang::Uk),
            s.lang_name_uk,
        );
    });

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);

    super::row_heading(ui, s.theme_label, s.badge_immediately);
    ui.add_enabled_ui(!app.busy, |ui| {
        ui.radio_value(&mut picked_theme, Theme::System, s.theme_system_label);
        ui.radio_value(&mut picked_theme, Theme::Light, s.theme_light_label);
        ui.radio_value(&mut picked_theme, Theme::Dark, s.theme_dark_label);
    });

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);

    // The acknowledgements' second home. They are read on the first-run
    // screen, which never comes back once the user has scanned - so this is
    // where anyone who wants to look again can. No badge on this block: it is
    // the one thing in the dialog that is not a setting.
    crate::ui::credits(ui, s);

    // Both setters no-op when the value is unchanged, so this runs every
    // frame without writing to the database every frame.
    if picked_lang != app.settings.app_language {
        app.set_language(picked_lang);
    }
    if picked_theme != app.settings.theme {
        app.set_theme(picked_theme);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ui::harness::UiTest;

    /// Drives the whole dialog, not `show` in isolation: a section that
    /// renders correctly but is not reachable from the nav is not shipped.
    fn open_general() -> UiTest {
        let mut test = UiTest::new(crate::ui::settings::show);
        test.app_mut().show_settings = true;
        test.app_mut().settings_section = crate::ui::settings::SettingsSection::General;
        test.run();
        test
    }

    #[test]
    fn the_section_offers_every_language_and_theme() {
        let test = open_general();
        let s = test.strings();

        test.assert_label(s.app_language_label);
        test.assert_label(s.lang_name_system);
        test.assert_label(s.lang_name_en);
        test.assert_label(s.lang_name_uk);

        test.assert_label(s.theme_label);
        test.assert_label(s.theme_system_label);
        test.assert_label(s.theme_light_label);
        test.assert_label(s.theme_dark_label);
    }

    /// Two rows of this section now offer a "follow the OS" option, and they
    /// started out sharing one string. Two radio buttons with the same label
    /// on one screen cannot be told apart by name - not by a reader of the
    /// screen, not by a screen reader, and not by the harness, which is how
    /// this was caught.
    #[test]
    fn no_two_controls_in_the_section_share_a_label() {
        let test = open_general();
        let s = test.strings();

        for label in [
            s.lang_name_system,
            s.lang_name_en,
            s.lang_name_uk,
            s.theme_system_label,
            s.theme_light_label,
            s.theme_dark_label,
        ] {
            assert_eq!(
                test.count_labels(label),
                1,
                "{label:?} labels more than one control in this section",
            );
        }
    }

    /// GT-68's tail: the first-run screen retires itself for good after one
    /// scan, so without a second home the acknowledgements become unreachable
    /// the moment the user does the thing the app exists for.
    ///
    /// Line by line rather than by the heading: a heading over an empty block
    /// is exactly the failure a presence check on the heading would miss.
    #[test]
    fn the_section_carries_the_acknowledgements() {
        let test = open_general();
        let s = test.strings();

        test.assert_label(s.credits_heading);
        for line in [s.credits_anthropic, s.credits_karpathy, s.credits_tikione] {
            test.assert_label(line);
        }
    }

    /// The badge is the section's only statement about *when* a change lands,
    /// and here the honest answer is "now" for both rows - so both have to
    /// carry it. One badge would read as if the other setting were deferred.
    #[test]
    fn every_row_says_it_applies_immediately() {
        let test = open_general();
        assert_eq!(test.count_labels(test.strings().badge_immediately), 2);
    }

    #[test]
    fn picking_a_language_applies_it_without_a_save_step() {
        let mut test = open_general();
        assert_eq!(test.app().lang(), Lang::En, "unexpected starting language");

        test.click(i18n::strings(Lang::En).lang_name_uk);

        assert_eq!(test.app().lang(), Lang::Uk);
        // And the dialog around it followed: the label lookup below uses the
        // Ukrainian strings, so this fails if the switch needed a reopen.
        test.assert_label(i18n::strings(Lang::Uk).theme_label);
    }

    /// GT-71. Out of the box the app follows Windows rather than insisting on
    /// English - and the picker says which of the three it is on, instead of
    /// showing a language the user never chose.
    #[test]
    fn a_fresh_app_follows_the_system_language() {
        let test = open_general();

        assert_eq!(
            test.app().settings.app_language,
            LanguagePreference::System,
            "a fresh app should defer to Windows, not pin a language",
        );
    }

    /// The resolution itself, through the app rather than through
    /// `LanguagePreference::resolve` alone: what the window renders in has to
    /// follow the machine while the preference is System.
    ///
    /// `new_for_test` pins the detected language so the suite does not depend
    /// on the developer's own Windows; this is one of the tests that is about
    /// that value, so it sets it.
    #[test]
    fn the_system_preference_renders_in_the_machines_language() {
        let mut test = open_general();
        assert_eq!(test.app().lang(), Lang::En, "the pinned test default");

        test.app_mut().set_system_language_for_test(Lang::Uk);
        test.run();

        assert_eq!(test.app().lang(), Lang::Uk);
        // And the dialog around it followed, rather than needing a reopen.
        test.assert_label(i18n::strings(Lang::Uk).theme_label);
    }

    /// The other half of the promise: an explicit choice is not a suggestion.
    /// A user who picked English keeps English on a Ukrainian Windows.
    #[test]
    fn an_explicit_choice_does_not_yield_to_the_system_language() {
        let mut test = open_general();
        test.app_mut().set_system_language_for_test(Lang::Uk);
        test.run();
        assert_eq!(test.app().lang(), Lang::Uk, "following the system");

        test.click(i18n::strings(Lang::Uk).lang_name_en);

        assert_eq!(
            test.app().settings.app_language,
            LanguagePreference::Fixed(Lang::En),
        );
        assert_eq!(test.app().lang(), Lang::En);
    }

    /// And back again: "System" is a state the user can return to, not a
    /// default they lose the moment they touch the picker.
    #[test]
    fn the_system_option_can_be_chosen_again_after_an_explicit_one() {
        let mut test = open_general();
        test.app_mut().set_system_language_for_test(Lang::Uk);
        // A frame first: the dialog is still drawn in English until one runs,
        // and the click below looks up a Ukrainian label.
        test.run();

        test.click(i18n::strings(Lang::Uk).lang_name_en);
        assert_eq!(test.app().lang(), Lang::En);

        test.click(i18n::strings(Lang::En).lang_name_system);

        assert_eq!(test.app().settings.app_language, LanguagePreference::System,);
        assert_eq!(test.app().lang(), Lang::Uk);
    }

    #[test]
    fn picking_a_theme_applies_it_without_a_save_step() {
        let mut test = open_general();
        let s = test.strings();
        assert_eq!(test.app().settings.theme, Theme::System);

        test.click(s.theme_dark_label);
        assert_eq!(test.app().settings.theme, Theme::Dark);

        test.click(s.theme_light_label);
        assert_eq!(test.app().settings.theme, Theme::Light);
    }

    /// Persisting opens its own database connection; doing that underneath a
    /// running worker is the race the `!app.busy` gate exists to prevent.
    #[test]
    fn the_controls_do_nothing_while_a_job_is_running() {
        let mut test = open_general();
        let s = test.strings();
        test.app_mut().begin_job(false);
        test.run();

        test.click(s.theme_dark_label);
        test.click(s.lang_name_uk);

        assert_eq!(test.app().settings.theme, Theme::System);
        assert_eq!(test.app().lang(), Lang::En);
    }
}
