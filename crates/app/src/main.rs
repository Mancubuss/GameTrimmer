#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod model;
mod ui;
mod worker;

use std::sync::Arc;

use eframe::egui;

use app::{GameTrimmerApp, APP_TITLE};

const WINDOW_SIZE: [f32; 2] = [900.0, 600.0];
const SYSTEM_FONT_PATH: &str = r"C:\Windows\Fonts\segoeui.ttf";
const SYSTEM_FONT_NAME: &str = "segoe-ui";

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_TITLE)
            .with_inner_size(WINDOW_SIZE),
        ..Default::default()
    };

    eframe::run_native(
        APP_TITLE,
        options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(GameTrimmerApp::new()))
        }),
    )
}

/// Loads the system font (Segoe UI) at runtime so Cyrillic text renders
/// correctly. Falls back to the default egui fonts if the file is unreadable.
fn setup_fonts(ctx: &egui::Context) {
    let font_bytes = match std::fs::read(SYSTEM_FONT_PATH) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("Не вдалося завантажити системний шрифт {SYSTEM_FONT_PATH}: {err}");
            return;
        }
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        SYSTEM_FONT_NAME.to_owned(),
        Arc::new(egui::FontData::from_owned(font_bytes)),
    );

    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, SYSTEM_FONT_NAME.to_owned());
    }

    ctx.set_fonts(fonts);
}
