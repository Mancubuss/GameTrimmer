//! `archive_trimmer` (`gametrimmer-archive`) - Monolithic Game Archive Inspector.
//!
//! Provides conservative inspection and localized data analysis. Destructive
//! mutation is disabled until full payload rollback and independent validation
//! exist for each supported container format:
//! - Audiokinetic Wwise PCK / SoundBank (`.pck`, `.bnk`)
//! - Unreal Engine 4 & 5 PAK (`.pak`)
//! - Electron ASAR (`.asar`)
//! - RAD Game Tools Bink Video (`.bik`, `.bk2`)
//! - Capcom RE Engine PAK (`re_chunk_*.pak` / `KPKA`)
//! - Unity AssetBundle / UnityFS (`.bundle`, `.unity3d`, `*.assets`)

pub mod anti_cheat;
#[cfg(feature = "cli")]
pub mod cli;
#[cfg(feature = "gui")]
pub mod db_reader;
pub mod formats;
#[cfg(feature = "gui")]
pub mod gui;
#[cfg(feature = "gui")]
pub mod logger;
#[cfg(test)]
mod safety;
pub mod scanner;
pub mod sparse;

// Convenient top-level re-exports
pub use anti_cheat::{
    check_game_safety, AntiCheatEngine, AntiCheatFinding, AntiCheatShield, SafetyError,
    SafetyReport,
};
#[cfg(feature = "gui")]
pub use db_reader::{
    find_default_db_path, read_games_with_candidates, CandidateFile, DbError, GameArchiveCandidates,
};
pub use formats::{
    is_external_single_language_file, ArchiveAnalysis, ArchiveError, ArchiveHandler, ArchiveType,
    FormatDetector, TrimOptions, TrimResult, TrimmableChunk,
};
#[cfg(feature = "gui")]
pub use gui::{run_gui, ArchiveTrimmerApp};
pub use scanner::{
    batch_trim_game, scan_game_directory, BatchTrimReport, GameScanReport, ScanError,
};
pub use sparse::{
    cluster_align_range, get_cluster_size, get_on_disk_size, is_sparse, query_allocated_ranges,
    SparseError,
};
