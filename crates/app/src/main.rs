#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cli;
mod deletion_controller;
mod elevation;
mod export;
mod i18n;
mod logger;
mod model;
mod search;
mod ui;
mod worker;

use std::sync::Arc;

use eframe::egui;

use app::{GameTrimmerApp, APP_TITLE};

const WINDOW_SIZE: [f32; 2] = [900.0, 600.0];

const SYSTEM_FONT_PATH: &str = r"C:\Windows\Fonts\segoeui.ttf";
const SYSTEM_FONT_NAME: &str = "segoe-ui";

// Fallback fonts, tried in this order for any glyph Segoe UI does not cover.
// Game titles pulled from storefronts can contain CJK ideographs, Hangul,
// or symbol/dingbat characters that Segoe UI lacks, which otherwise render
// as tofu squares (`□`).
const SYMBOL_FONT_PATH: &str = r"C:\Windows\Fonts\seguisym.ttf";
const SYMBOL_FONT_NAME: &str = "segoe-ui-symbol";
/// `msyh.ttc` is a font *collection*; face `0` is the regular Microsoft
/// YaHei face, which is what we want.
const CJK_FONT_PATH: &str = r"C:\Windows\Fonts\msyh.ttc";
const CJK_FONT_NAME: &str = "microsoft-yahei";
const KOREAN_FONT_PATH: &str = r"C:\Windows\Fonts\malgun.ttf";
const KOREAN_FONT_NAME: &str = "malgun-gothic";

/// The 256x256 PNG frame of `assets/gametrimmer.ico`, used as the runtime
/// window icon (the exe-resource icon embedded by `build.rs` covers
/// Explorer/taskbar, but winit never reads it for the window itself).
const WINDOW_ICON_PNG: &[u8] = include_bytes!("../assets/gametrimmer_256.png");

fn main() -> eframe::Result {
    // Headless (CLI) mode, switched off in the v1 release build (see
    // `cli::args::HEADLESS_ENABLED`). With no argument this returns `LaunchGui`
    // in every build and the graphical app starts exactly as before; with an
    // argument the process exits here with a status code, never constructing a
    // window - either having run the headless job, or having said that this
    // build has no command-line mode.
    match cli::run_from_env() {
        cli::Outcome::Exit(code) => std::process::exit(code as i32),
        cli::Outcome::LaunchGui => {}
    }

    run_gui()
}

fn run_gui() -> eframe::Result {
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
            // No forced `set_visuals` here: the persisted `theme` setting
            // (default `System`, following the OS preference) governs the
            // theme from the very first frame via `ctx.set_theme` in
            // `GameTrimmerApp::ui` - see `app::theme_preference`.
            Ok(Box::new(GameTrimmerApp::new(cc.egui_ctx.clone())))
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
            eprintln!("Failed to decode the window icon: {err}");
            None
        }
    }
}

/// Sets up text rendering fonts: Segoe UI is the primary font (covers
/// Latin/Cyrillic/Greek, i.e. the app's own UI text) and stays first in
/// line, followed by a fallback chain for any glyph Segoe UI lacks -
/// Segoe UI Symbol (symbols/arrows/dingbats), Microsoft YaHei (Simplified
/// Chinese / CJK ideographs), then Malgun Gothic (Korean Hangul). Game
/// titles imported from storefronts can contain any of these scripts, and
/// egui otherwise renders unsupported glyphs as tofu squares (`□`).
///
/// Every font is read from `C:\Windows\Fonts` at runtime rather than
/// embedded in the binary - this keeps the executable small and portable,
/// relying on whatever font files the OS already ships. Each font is
/// loaded independently and is entirely optional: a missing or unreadable
/// file is logged and skipped without affecting the primary font or any
/// other fallback. If even Segoe UI is unavailable, egui's own built-in
/// default fonts are used instead.
fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // (font key, file path, ttc face index, prepend-to-front vs. append)
    let entries: [(&str, &str, u32, bool); 4] = [
        (SYSTEM_FONT_NAME, SYSTEM_FONT_PATH, 0, true),
        (SYMBOL_FONT_NAME, SYMBOL_FONT_PATH, 0, false),
        (CJK_FONT_NAME, CJK_FONT_PATH, 0, false),
        (KOREAN_FONT_NAME, KOREAN_FONT_PATH, 0, false),
    ];

    for (name, path, face_index, is_primary) in entries {
        if !add_font(&mut fonts, name, path, face_index) {
            continue;
        }
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let family_fonts = fonts.families.entry(family).or_default();
            if is_primary {
                family_fonts.insert(0, name.to_owned());
            } else {
                family_fonts.push(name.to_owned());
            }
        }
    }

    ctx.set_fonts(fonts);
}

/// Reads the font file at `path` and registers it in `fonts` under `name`
/// at font-collection face `face_index` (`0` for a plain `.ttf`/`.otf`
/// file, or the desired face for a `.ttc` collection). Returns `false`
/// (after logging the error) if the file could not be read, leaving
/// `fonts` untouched so the caller can skip inserting it into any family -
/// this is how each font (primary or fallback) stays optional and
/// non-fatal.
fn add_font(fonts: &mut egui::FontDefinitions, name: &str, path: &str, face_index: u32) -> bool {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("Failed to load font {name} ({path}): {err}");
            return false;
        }
    };

    fonts.font_data.insert(
        name.to_owned(),
        Arc::new(egui::FontData {
            font: bytes.into(),
            index: face_index,
            tweak: Default::default(),
        }),
    );
    true
}
