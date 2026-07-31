//! "General": app language and theme.
//!
//! Both are applied and persisted the moment they change - `set_language`
//! writes the setting and every render call reads `app.lang()` fresh, and
//! `eframe::App::ui` pushes `settings.theme` into the context every frame. So
//! each row carries the "Immediately" badge and the section has no save step;
//! "Done" only dismisses the dialog.

use eframe::egui;

use gametrimmer_core::settings::{Lang, Theme};

use crate::app::GameTrimmerApp;
use crate::i18n;

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let s = i18n::strings(app.lang());
    let mut picked_lang = app.lang();
    let mut picked_theme = app.settings.theme;

    super::row_heading(ui, s.app_language_label, s.badge_immediately);
    // Persisting opens a fresh database connection, so the controls are
    // gated behind `!app.busy` rather than racing an in-flight worker's own
    // connection - the same gate the pre-rebuild dialog used.
    ui.add_enabled_ui(!app.busy, |ui| {
        ui.radio_value(&mut picked_lang, Lang::En, s.lang_name_en);
        ui.radio_value(&mut picked_lang, Lang::Uk, s.lang_name_uk);
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

    // Both setters no-op when the value is unchanged, so this runs every
    // frame without writing to the database every frame.
    if picked_lang != app.lang() {
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
        test.assert_label(s.lang_name_en);
        test.assert_label(s.lang_name_uk);

        test.assert_label(s.theme_label);
        test.assert_label(s.theme_system_label);
        test.assert_label(s.theme_light_label);
        test.assert_label(s.theme_dark_label);
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
