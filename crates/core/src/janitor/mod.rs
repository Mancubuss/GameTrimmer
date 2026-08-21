//! Next-Gen Game & Engine Artifact Janitor module for GameTrimmer (GT-EP15).
//!
//! Provides deep scanning, analysis, and safe cleanup for:
//! - Steam/EGS Workshop orphaned items and unsubscribed mods.
//! - Incomplete download chunks and staging depots (`steamapps/downloading`, `.egstore/Pending`).
//! - GPU shader caches (NVIDIA DXCache/GLCache, AMD DxCache, Windows D3DSCache, Steam pipeline caches).
//! - Crash dumps (Windows WER `.dmp`, Unreal Engine `Saved/Crashes`) and runaway log files (Unity `Player.log`).
//! - Smart Save Pruner with retention policy (keeping $N$ latest quicksaves/autosaves) and Zero-Data-Loss ZIP backups.
//! - Launcher CEF/Chromium web caches and mod-manager installer archives.

pub mod crashes;
pub mod launchers;
pub mod saves;
pub mod shadercache;
pub mod staging;
pub mod workshop;

use crate::rules::Category;
use std::path::PathBuf;

/// Information about a discovered janitor artifact candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JanitorArtifact {
    pub path: PathBuf,
    pub category: Category,
    pub size_bytes: u64,
    pub description: String,
    pub is_safe_default: bool,
    pub requires_backup: bool,
    pub app_id: Option<String>,
    pub game_title: Option<String>,
    /// The folder that names the game this artifact belongs to, relative to
    /// the directory that was scanned (`Fumi Games\MOUSE`), or the scanned
    /// directory's own name when it belongs to one game whole.
    ///
    /// The app groups a listing under it instead of drawing a flat list of
    /// files: a save area is hundreds of files across dozens of games, and
    /// answering "which of these do I want gone" needs them under the game
    /// that wrote them. `None` leaves the artifact ungrouped, which is the
    /// right answer for the areas that are one bucket anyway (crash dumps,
    /// a launcher's web cache).
    pub group_dir: Option<String>,
}

/// Overall configuration for the Janitor scanner.
#[derive(Debug, Clone)]
pub struct JanitorConfig {
    /// Age threshold in days for GPU shader cache files to be considered stale (default: 30).
    pub shader_stale_days: u32,
    /// Number of newest autosaves/quicksaves to retain per character/game in Smart Save Pruner (default: 10).
    pub save_retention_count: usize,
    /// Backup directory for save games before pruning.
    pub save_backup_dir: Option<PathBuf>,
}

impl Default for JanitorConfig {
    fn default() -> Self {
        Self {
            shader_stale_days: 30,
            save_retention_count: 10,
            save_backup_dir: None,
        }
    }
}
