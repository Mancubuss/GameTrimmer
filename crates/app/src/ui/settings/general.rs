//! "General": app language, theme, and the acknowledgements.
//!
//! Both are applied and persisted the moment they change - `set_language`
//! writes the setting and every render call reads `app.lang()` fresh, and
//! `eframe::App::ui` pushes `settings.theme` into the context every frame. So
//! each row carries the "Immediately" badge and the section has no save step;
//! "Done" only dismisses the dialog.

use eframe::egui;

use gametrimmer_core::settings::{Lang, LanguagePreference, Theme, WatchMode};

use crate::app::GameTrimmerApp;
use crate::i18n;

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let s = i18n::strings(app.lang());
    let mut picked_lang = app.settings.app_language;
    let mut picked_theme = app.settings.theme;
    let mut picked_watch_enabled = app.settings.watch_enabled;
    let mut picked_watch_autostart = app.settings.watch_autostart;
    let mut picked_watch_mode = app.settings.watch_mode;

    super::row_heading(ui, s.app_language_label, s.badge_immediately);
    // Persisting opens a fresh database connection, so the controls are
    // gated behind `!app.busy` rather than racing an in-flight worker's own
    // connection - the same gate the pre-rebuild dialog used.
    ui.add_enabled_ui(!app.busy, |ui| {
        let locales = i18n::available_locales();

        let sys_lang = app.system_lang();
        let sys_strings = i18n::strings(sys_lang);
        let sys_name = match sys_lang {
            Lang::En => "English",
            Lang::Custom(_) => sys_lang.as_str(),
        };
        let sys_label = format!("{} ({})", sys_strings.lang_name_system, sys_name);

        let current_text = match &picked_lang {
            LanguagePreference::System => sys_label.clone(),
            LanguagePreference::Fixed(lang) => {
                let id = lang.as_str();
                if let Some(info) = locales.iter().find(|l| l.id == id) {
                    format!("{} [{}]", info.native_name, info.id)
                } else if id == "en" {
                    "English [en]".to_string()
                } else if id == "uk" {
                    "Українська [uk]".to_string()
                } else {
                    id.to_string()
                }
            }
        };

        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("settings_app_language_selector")
                .selected_text(&current_text)
                .width(280.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut picked_lang, LanguagePreference::System, sys_label);
                    ui.separator();
                    for loc in &locales {
                        let label = format!("{} [{}]", loc.native_name, loc.id);
                        if let Some(custom_lang) = Lang::parse(&loc.id) {
                            ui.selectable_value(
                                &mut picked_lang,
                                LanguagePreference::Fixed(custom_lang),
                                label,
                            );
                        }
                    }
                });

            ui.add_space(4.0);
            if ui
                .button(format!("📁 {}", s.btn_open_folder))
                .on_hover_text("Open locales/ folder to add or edit translation files")
                .clicked()
            {
                let locales_dir = std::path::Path::new("locales");
                let _ = std::fs::create_dir_all(locales_dir);
                let _ = std::process::Command::new("explorer.exe")
                    .arg(locales_dir)
                    .spawn();
            }
        });
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

    super::row_heading(ui, s.settings_section_monitoring, s.badge_immediately);
    ui.add_enabled_ui(!app.busy, |ui| {
        ui.checkbox(&mut picked_watch_enabled, s.watch_enabled_label)
            .on_hover_text(s.watch_enabled_hint);

        ui.add_enabled_ui(picked_watch_enabled, |ui| {
            ui.indent("settings_watch_indent", |ui| {
                ui.checkbox(&mut picked_watch_autostart, s.watch_autostart_label)
                    .on_hover_text(s.watch_autostart_hint);

                ui.add_space(4.0);
                ui.label(s.watch_mode_label);
                ui.radio_value(
                    &mut picked_watch_mode,
                    WatchMode::Interactive,
                    s.watch_mode_interactive,
                )
                .on_hover_text(s.watch_mode_interactive_hint);
                // `WatchMode::AutoTrim` is persisted end to end - the daemon
                // accepts it over IPC and stores it - but nothing anywhere
                // reads it back to actually retrim anything: the wiring from
                // a detected update to `retrim_game_with_new_build` does not
                // exist yet. Offering it as a live choice would let someone
                // pick "silently deletes files for me" and see nothing
                // happen, then forget they ever turned it on - consent
                // captured for a feature that was never there to consent to.
                // Disabled rather than hidden, so the option's *existence*
                // still communicates the roadmap; greyed out is this file's
                // only honest way to say "not yet" on its own. The hint says
                // it in words too - `watch_mode_autotrim_hint` carries the
                // status in all thirty locales - and it has to be an
                // `on_disabled_hover_text`, because egui shows no ordinary
                // hover text on a disabled widget at all.
                ui.add_enabled_ui(false, |ui| {
                    ui.radio_value(
                        &mut picked_watch_mode,
                        WatchMode::AutoTrim,
                        s.watch_mode_autotrim,
                    )
                    .on_disabled_hover_text(s.watch_mode_autotrim_hint);
                });
                ui.radio_value(
                    &mut picked_watch_mode,
                    WatchMode::Passive,
                    s.watch_mode_passive,
                )
                .on_hover_text(s.watch_mode_passive_hint);

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let daemon_status_text = if app.watch_daemon_running {
                        format!("🟢 {}", s.watch_daemon_status_running)
                    } else {
                        format!("⚪ {}", s.watch_daemon_status_stopped)
                    };
                    ui.label(daemon_status_text);

                    ui.add_space(8.0);
                    if ui.button(s.btn_watch_rescan_now).clicked() {
                        let _ = app.trigger_watch_rescan();
                    }
                });
            });
        });
    });

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);

    // The acknowledgements' second home. They are read on the first-run
    // screen, which never comes back once the user has scanned - so this is
    // where anyone who wants to look again can. No badge on this block: it is
    // the one thing in the dialog that is not a setting.
    crate::ui::credits(ui, s);

    // Setters no-op when the value is unchanged, so this runs every
    // frame without writing to the database every frame.
    if picked_lang != app.settings.app_language {
        app.set_language(picked_lang);
    }
    if picked_theme != app.settings.theme {
        app.set_theme(picked_theme);
    }
    if picked_watch_enabled != app.settings.watch_enabled
        || picked_watch_autostart != app.settings.watch_autostart
        || picked_watch_mode != app.settings.watch_mode
    {
        let mut new_settings = app.settings.clone();
        new_settings.watch_enabled = picked_watch_enabled;
        new_settings.watch_autostart = picked_watch_autostart;
        new_settings.watch_mode = picked_watch_mode;
        app.set_settings(new_settings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ui::harness::UiTest;

    /// A stand-in for "some language other than English", for tests that
    /// exist to prove a language switch actually took effect rather than to
    /// prove any particular translation. Localizations are frozen (Vikunja
    /// #443 tracks unfreezing them), so English is the only compiled-in
    /// table; a `Custom` tag is what proves `app.lang()` really moved instead
    /// of coincidentally starting out equal to the default.
    const OTHER_LANG: Lang = Lang::Custom(*b"xx\0\0\0\0\0\0");

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
        test.assert_label(s.theme_label);
        test.assert_label(s.theme_system_label);
        test.assert_label(s.theme_light_label);
        test.assert_label(s.theme_dark_label);
    }

    /// Two rows of this section offer a "follow the OS" option, and they
    /// started out sharing one string.
    #[test]
    fn no_two_controls_in_the_section_share_a_label() {
        let test = open_general();
        let s = test.strings();

        for label in [
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

    /// persisted onboarding completion's tail: the first-run screen retires itself for good after one
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
    /// and here the honest answer is "now" for all three rows - so all three have to
    /// carry it. One badge would read as if the other setting were deferred.
    #[test]
    fn every_row_says_it_applies_immediately() {
        let test = open_general();
        assert_eq!(test.count_labels(test.strings().badge_immediately), 3);
    }

    #[test]
    fn the_section_offers_background_monitoring_controls() {
        let test = open_general();
        let s = test.strings();

        test.assert_label(s.settings_section_monitoring);
        test.assert_label(s.watch_enabled_label);
    }

    #[test]
    fn background_monitoring_toggles_and_modes_apply_without_a_save_step() {
        let mut test = open_general();
        let s = test.strings();
        assert!(test.app().settings.watch_enabled);
        assert!(!test.app().settings.watch_autostart);
        assert_eq!(test.app().settings.watch_mode, WatchMode::Interactive);

        // AutoTrim is offered but disabled - see the section's own comment -
        // so clicking its label must not move the setting. Covered on its
        // own in `the_autotrim_mode_is_offered_but_not_selectable`; this
        // test only needs to confirm it does not disturb the modes that do
        // work.
        test.click(s.watch_mode_passive);
        assert_eq!(test.app().settings.watch_mode, WatchMode::Passive);

        test.click(s.watch_autostart_label);
        assert!(test.app().settings.watch_autostart);

        test.click(s.watch_enabled_label);
        assert!(!test.app().settings.watch_enabled);
    }

    /// The option that does nothing. `WatchMode::AutoTrim` is a real,
    /// persisted enum variant and reads as a normal choice next to the other
    /// two - the only thing telling a user it is not live yet is whether the
    /// control accepts a click at all. Interactive stays selected is the
    /// part that would silently regress if the control were re-enabled
    /// before the feature actually exists.
    #[test]
    fn the_autotrim_mode_is_offered_but_not_selectable() {
        let mut test = open_general();
        let s = test.strings();
        assert_eq!(test.app().settings.watch_mode, WatchMode::Interactive);

        test.assert_label(s.watch_mode_autotrim);
        test.click(s.watch_mode_autotrim);

        assert_eq!(
            test.app().settings.watch_mode,
            WatchMode::Interactive,
            "a disabled option must not be selectable by clicking its label",
        );
    }

    #[test]
    fn picking_a_language_applies_it_without_a_save_step() {
        let mut test = open_general();
        assert_eq!(test.app().lang(), Lang::En, "unexpected starting language");

        test.app_mut()
            .set_language(LanguagePreference::Fixed(OTHER_LANG));
        test.run();

        assert_eq!(test.app().lang(), OTHER_LANG);
    }

    /// system-theme default. Out of the box the app follows Windows rather than insisting on
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

        test.app_mut().set_system_language_for_test(OTHER_LANG);
        test.run();

        assert_eq!(test.app().lang(), OTHER_LANG);
    }

    /// The other half of the promise: an explicit choice is not a suggestion.
    /// A user who picked English keeps English when the system reports
    /// something else.
    #[test]
    fn an_explicit_choice_does_not_yield_to_the_system_language() {
        let mut test = open_general();
        test.app_mut().set_system_language_for_test(OTHER_LANG);
        test.run();
        assert_eq!(test.app().lang(), OTHER_LANG, "following the system");

        test.app_mut()
            .set_language(LanguagePreference::Fixed(Lang::En));
        test.run();

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
        test.app_mut().set_system_language_for_test(OTHER_LANG);
        // A frame first: the dialog is still drawn in English until one runs.
        test.run();

        test.app_mut()
            .set_language(LanguagePreference::Fixed(Lang::En));
        test.run();
        assert_eq!(test.app().lang(), Lang::En);

        test.app_mut().set_language(LanguagePreference::System);
        test.run();

        assert_eq!(test.app().settings.app_language, LanguagePreference::System,);
        assert_eq!(test.app().lang(), OTHER_LANG);
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

        assert_eq!(test.app().settings.theme, Theme::System);
        assert_eq!(test.app().lang(), Lang::En);
    }
}
