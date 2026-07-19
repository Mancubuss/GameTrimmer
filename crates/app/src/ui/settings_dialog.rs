//! The settings dialog. Each change is applied and persisted immediately
//! (see `GameTrimmerApp::set_delete_method`), so "Закрити" only dismisses
//! the dialog - there is no separate save step to forget.
//!
//! Sections: deletion method, keep-list languages (never flagged by the
//! localization detector - see `gametrimmer_core::settings::keep_languages`),
//! database maintenance ("Стиснути базу даних"), analysis rule packs
//! (export/import - see docs/05_rules_pack_plan.md), app language, theme,
//! and scanned artifact categories.

use eframe::egui;

use gametrimmer_core::langdetect::LangData;
use gametrimmer_core::settings::{DeleteMethod, Lang, Theme};

use crate::app::GameTrimmerApp;
use crate::i18n;
use crate::model::{category_display, category_enabled, category_ui_key, CATEGORY_ORDER};

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    if !app.show_settings {
        return;
    }

    let s = i18n::strings(app.lang());
    let mut close = false;
    let mut compact_clicked = false;
    let mut export_rules_clicked = false;
    let mut import_rules_clicked = false;
    let mut picked_method = app.settings.delete_method;
    let mut picked_keep_languages = app.settings.keep_languages.clone();
    let mut picked_lang = app.lang();
    let mut picked_theme = app.settings.theme;
    // Kept in the same "empty means every category enabled" representation
    // used for storage (see `Settings::enabled_categories`) - the checkbox
    // loop below reads/writes it through `category_enabled`, and the block
    // is normalized back to empty once every category ends up checked, so
    // the dialog never persists a functionally-identical-but-different
    // value on every frame it happens to be open.
    let mut picked_categories = app.settings.enabled_categories.clone();

    egui::Modal::new(egui::Id::new("gt_settings")).show(ui.ctx(), |ui| {
        ui.set_min_width(420.0);
        ui.heading(s.settings_heading);
        ui.add_space(8.0);

        ui.label(s.delete_method_label);
        ui.add_space(4.0);
        // Disabled while a worker is running: changing the method persists
        // immediately through a fresh DB connection, which would race an
        // in-flight `VACUUM` (it needs exclusive access) or delete job.
        ui.add_enabled_ui(!app.busy, |ui| {
            ui.radio_value(
                &mut picked_method,
                DeleteMethod::Permanent,
                s.delete_method_permanent_label,
            );
            ui.indent("gt_settings_permanent_hint", |ui| {
                ui.small(s.delete_method_permanent_hint);
            });
            ui.add_space(4.0);
            ui.radio_value(
                &mut picked_method,
                DeleteMethod::RecycleBin,
                s.delete_method_recycle_label,
            );
            ui.indent("gt_settings_recycle_hint", |ui| {
                ui.small(s.delete_method_recycle_hint);
            });
        });

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);

        ui.label(s.app_language_label);
        ui.add_space(4.0);
        // Persists immediately, same as the delete method above - a fresh
        // DB connection write, so gated behind `!app.busy` for the same
        // reason.
        ui.add_enabled_ui(!app.busy, |ui| {
            ui.radio_value(&mut picked_lang, Lang::En, s.lang_name_en);
            ui.radio_value(&mut picked_lang, Lang::Uk, s.lang_name_uk);
        });

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);

        ui.label(s.theme_label);
        ui.add_space(4.0);
        ui.add_enabled_ui(!app.busy, |ui| {
            ui.radio_value(&mut picked_theme, Theme::System, s.theme_system_label);
            ui.radio_value(&mut picked_theme, Theme::Light, s.theme_light_label);
            ui.radio_value(&mut picked_theme, Theme::Dark, s.theme_dark_label);
        });

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);

        ui.label(s.keep_languages_label);
        ui.add_space(4.0);
        // Disabled while a worker is running: the same reasoning as the
        // delete-method radio buttons above - this persists immediately
        // through a fresh DB connection.
        ui.add_enabled_ui(!app.busy, |ui| {
            egui::ScrollArea::vertical()
                .max_height(160.0)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for &code in LangData::builtin().language_keys() {
                            let mut checked = picked_keep_languages.iter().any(|c| c == code);
                            let label = i18n::lang_display_name(code);
                            if ui.checkbox(&mut checked, label).changed() {
                                if checked {
                                    if !picked_keep_languages.iter().any(|c| c == code) {
                                        picked_keep_languages.push(code.to_string());
                                    }
                                } else if picked_keep_languages.len() > 1 {
                                    // At least one language must always stay
                                    // checked - an empty keep-list would let
                                    // the detector flag the user's own
                                    // language. Unchecking the last one is
                                    // simply ignored (the checkbox snaps
                                    // back on the next frame).
                                    picked_keep_languages.retain(|c| c != code);
                                }
                            }
                        }
                    });
                });
        });
        ui.small(s.keep_languages_hint);

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);

        ui.label(s.categories_label);
        ui.add_space(4.0);
        // Disabled while a worker is running: the same reasoning as the
        // other persisted-immediately controls above.
        ui.add_enabled_ui(!app.busy, |ui| {
            for category in CATEGORY_ORDER {
                let key = category_ui_key(category);
                let mut checked = category_enabled(&picked_categories, category);
                let label = category_display(app.lang(), category);
                if ui.checkbox(&mut checked, label).changed() {
                    if checked {
                        if !picked_categories.iter().any(|id| id == key) {
                            picked_categories.push(key.to_string());
                        }
                    } else if picked_categories.is_empty() {
                        // Was "every category enabled" (the empty-list
                        // convention) - unchecking one now means everything
                        // *except* it, so materialize the rest explicitly.
                        picked_categories = CATEGORY_ORDER
                            .iter()
                            .filter(|&&c| c != category)
                            .map(|&c| category_ui_key(c).to_string())
                            .collect();
                    } else if picked_categories.len() > 1 {
                        // At least one category must always stay checked -
                        // an empty list would mean "every category enabled"
                        // again, silently undoing every uncheck the user
                        // just made. Unchecking the last one is simply
                        // ignored (the checkbox snaps back on the next
                        // frame), same pattern as the keep-languages list.
                        picked_categories.retain(|id| id != key);
                    }
                }
            }
        });
        // Once every category ends up checked again, collapse back to the
        // canonical empty-list storage form instead of persisting an
        // explicit list that happens to name every category - keeps
        // `Settings::enabled_categories` stable and avoids re-saving on
        // every frame the dialog is open.
        if picked_categories.len() == CATEGORY_ORDER.len() {
            picked_categories.clear();
        }
        ui.small(s.categories_hint);

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);

        ui.label(s.database_label);
        ui.add_space(4.0);
        if ui
            .add_enabled(!app.busy, egui::Button::new(s.btn_compact_database))
            .clicked()
        {
            compact_clicked = true;
        }
        ui.small(s.compact_hint);

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);

        ui.label(s.rules_label);
        ui.add_space(4.0);
        // The export only reads the pack files, but the import rewrites the
        // files a scan loads at startup - both are disabled during any
        // background job to keep the flow simple, plus while a previous
        // rules dialog is still open (`rules_io_active`).
        ui.add_enabled_ui(!app.busy && !app.rules_io_active, |ui| {
            ui.horizontal(|ui| {
                if ui.button(s.btn_export_rules).clicked() {
                    export_rules_clicked = true;
                }
                if ui.button(s.btn_import_rules).clicked() {
                    import_rules_clicked = true;
                }
            });
        });
        ui.small(s.rules_hint);
        // The top-bar status line is hidden behind this modal, so the
        // outcome must be shown right here, under the buttons.
        if app.rules_io_active {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(s.running_ellipsis);
            });
        } else if let Some(result) = &app.rules_io_result {
            ui.add_space(4.0);
            match result {
                Ok(msg) => {
                    ui.colored_label(egui::Color32::from_rgb(0x4c, 0xaf, 0x50), msg);
                }
                Err(msg) => {
                    ui.colored_label(ui.visuals().error_fg_color, msg);
                }
            }
        }

        ui.add_space(12.0);
        if ui.button(s.btn_close).clicked() {
            close = true;
        }
    });

    app.set_delete_method(picked_method);
    app.set_language(picked_lang);
    app.set_theme(picked_theme);
    app.set_keep_languages(picked_keep_languages);
    app.set_enabled_categories(picked_categories);
    if compact_clicked {
        app.start_compact();
    }
    if export_rules_clicked {
        app.start_rules_export();
    }
    if import_rules_clicked {
        app.start_rules_import();
    }
    if close {
        app.show_settings = false;
        // A stale export/import result must not greet the user on the next
        // open; the top-bar status line keeps the last success anyway.
        app.rules_io_result = None;
    }
}
