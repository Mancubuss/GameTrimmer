#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod elevation;
mod export;
mod model;
mod ui;
mod worker;

use std::sync::Arc;

use eframe::egui;

use app::{GameTrimmerApp, APP_TITLE};

const WINDOW_SIZE: [f32; 2] = [900.0, 600.0];
const SYSTEM_FONT_PATH: &str = r"C:\Windows\Fonts\segoeui.ttf";
const SYSTEM_FONT_NAME: &str = "segoe-ui";
/// The 256x256 PNG frame of `assets/gametrimmer.ico`, used as the runtime
/// window icon (the exe-resource icon embedded by `build.rs` covers
/// Explorer/taskbar, but winit never reads it for the window itself).
const WINDOW_ICON_PNG: &[u8] = include_bytes!("../assets/gametrimmer_256.png");

fn main() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(APP_TITLE)
        .with_inner_size(WINDOW_SIZE);
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
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

/// Decodes the embedded window icon. `None` (falling back to the default
/// window icon) if the embedded PNG is somehow undecodable - never a panic.
fn window_icon() -> Option<egui::IconData> {
    match image::load_from_memory(WINDOW_ICON_PNG) {
        Ok(decoded) => {
            let rgba = decoded.to_rgba8();
            let (width, height) = rgba.dimensions();
            Some(egui::IconData {
                rgba: rgba.into_raw(),
                width,
                height,
            })
        }
        Err(err) => {
            eprintln!("Не вдалося декодувати іконку вікна: {err}");
            None
        }
    }
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
