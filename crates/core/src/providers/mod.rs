//! Discovery of game libraries across launcher vendors.

use std::path::PathBuf;

use crate::error::Result;

pub mod steam;

/// One installed game inside a library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameInstall {
    /// Display name, e.g. "DOOM The Dark Ages".
    pub name: String,
    /// Absolute path to the game's install directory.
    pub install_dir: PathBuf,
    /// Vendor-specific id (Steam appid etc.), if known.
    pub app_id: Option<String>,
}

/// A discovered game library (one root folder of one vendor).
#[derive(Debug, Clone)]
pub struct DiscoveredLibrary {
    /// Vendor tag stored in the DB, e.g. "steam".
    pub vendor: &'static str,
    /// Absolute path to the library root, e.g. `F:\SteamLibrary`.
    pub path: PathBuf,
    pub games: Vec<GameInstall>,
}

/// A source of game libraries (Steam, Epic, GOG, ...).
pub trait LibraryProvider {
    fn name(&self) -> &'static str;
    /// Discover all libraries of this vendor present on the machine.
    fn discover(&self) -> Result<Vec<DiscoveredLibrary>>;
}
