//! Desktop Graphical User Interface (eframe / egui) for GameTrimmer Archive Trimmer.
//!
//! Provides a modern, responsive, and lightweight user interface for inspecting candidate
//! monolithic archives loaded directly from `gametrimmer.db`, analyzing localized stream offsets,
//! while destructive actions remain disabled pending full payload rollback.

pub mod app;
pub mod details_modal;
pub mod worker;

use eframe::egui::{self, Vec2};
use std::path::PathBuf;

pub use app::ArchiveTrimmerApp;
pub use details_modal::{ArchiveDetailsModal, DetailsModalAction};
pub use worker::{
    create_worker_channel, spawn_scan_candidates, GameScanResult, ScannedArchive, WorkerMsg,
};

/// Launches the interactive desktop graphical user interface.
pub fn run_gui(db_path: Option<PathBuf>) -> Result<(), eframe::Error> {
    let viewport = egui::ViewportBuilder::default()
        .with_title("GameTrimmer Archive Trimmer")
        .with_inner_size(Vec2::new(1150.0, 720.0))
        .with_min_inner_size(Vec2::new(850.0, 520.0));

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "GameTrimmer Archive Trimmer",
        native_options,
        Box::new(move |cc| Ok(Box::new(ArchiveTrimmerApp::new(cc, db_path)))),
    )
}
