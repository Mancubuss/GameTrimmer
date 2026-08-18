//! Smart Save Pruner and Zero-Data-Loss Shield for RPGs (GT-184).
//!
//! Features:
//! - Discovers save folders for major RPGs (Skyrim, Witcher 3, BG3, Starfield, Cyberpunk 2077, OpenMW, KCD).
//! - Strictly distinguishes manual named saves from autosaves / quicksaves.
//! - Smart Retention: keeps $N$ latest autosaves/quicksaves per campaign, identifies older excess.
//! - Zero-Data-Loss Shield: creates timestamped ZIP backup archive before any deletion.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use crate::janitor::JanitorArtifact;
use crate::rules::Category;

/// Classification of a game save file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveKind {
    /// Named/manual save created explicitly by user - NEVER auto-pruned.
    Manual,
    /// Automated or quicksave generated repeatedly - candidate for retention pruning.
    AutoOrQuick,
}

/// Metadata for an analyzed save file.
#[derive(Debug, Clone)]
pub struct SaveFileEntry {
    pub path: PathBuf,
    pub filename: String,
    pub size_bytes: u64,
    pub mtime: SystemTime,
    pub kind: SaveKind,
}

/// Definition of a supported game save location.
#[derive(Debug, Clone)]
pub struct SaveGameDef {
    pub title: &'static str,
    pub rel_path: &'static str, // Relative to UserProfile or Documents
    pub is_under_documents: bool,
    pub file_extension: &'static str,
}

pub static SUPPORTED_RPGS: &[SaveGameDef] = &[
    SaveGameDef {
        title: "The Elder Scrolls V: Skyrim Special Edition",
        rel_path: "My Games/Skyrim Special Edition/Saves",
        is_under_documents: true,
        file_extension: "ess",
    },
    SaveGameDef {
        title: "The Elder Scrolls V: Skyrim",
        rel_path: "My Games/Skyrim/Saves",
        is_under_documents: true,
        file_extension: "ess",
    },
    SaveGameDef {
        title: "Fallout 4",
        rel_path: "My Games/Fallout4/Saves",
        is_under_documents: true,
        file_extension: "fos",
    },
    SaveGameDef {
        title: "Starfield",
        rel_path: "My Games/Starfield/Saves",
        is_under_documents: true,
        file_extension: "sfs",
    },
    SaveGameDef {
        title: "The Witcher 3: Wild Hunt",
        rel_path: "The Witcher 3/gamesaves",
        is_under_documents: true,
        file_extension: "sav",
    },
    SaveGameDef {
        title: "OpenMW",
        rel_path: "My Games/OpenMW/saves",
        is_under_documents: true,
        file_extension: "omwsave",
    },
    SaveGameDef {
        title: "Cyberpunk 2077",
        rel_path: "Saved Games/CD Projekt Red/Cyberpunk 2077",
        is_under_documents: false,
        file_extension: "dat",
    },
    SaveGameDef {
        title: "Kingdom Come: Deliverance",
        rel_path: "Saved Games/kingdomcome/saves",
        is_under_documents: false,
        file_extension: "whs",
    },
    SaveGameDef {
        title: "Baldur's Gate 3",
        rel_path:
            "AppData/Local/Larian Studios/Baldur's Gate 3/PlayerProfiles/Public/Savegames/Story",
        is_under_documents: false,
        file_extension: "lsv",
    },
];

/// Classifies whether a filename is an autosave/quicksave or a manual save.
pub fn classify_save_file(filename: &str) -> SaveKind {
    let lower = filename.to_ascii_lowercase();

    // Specific patterns for quick/autosaves
    if lower.starts_with("autosave")
        || lower.starts_with("quicksave")
        || lower.starts_with("auto_")
        || lower.starts_with("quick_")
        || lower.contains("_autosave")
        || lower.contains("_quicksave")
        || lower.starts_with("checkpoint")
    {
        SaveKind::AutoOrQuick
    } else {
        SaveKind::Manual
    }
}

/// Discovers save directory for a supported game on current Windows system.
pub fn resolve_save_dir(def: &SaveGameDef) -> Option<PathBuf> {
    if def.is_under_documents {
        if let Ok(user_profile) = std::env::var("USERPROFILE") {
            let docs = PathBuf::from(user_profile)
                .join("Documents")
                .join(def.rel_path);
            if docs.is_dir() {
                return Some(docs);
            }
            // Some Windows setups place OneDrive\Documents or custom Documents
            if let Ok(onedrive) = std::env::var("OneDrive") {
                let onedrive_docs = PathBuf::from(onedrive).join("Documents").join(def.rel_path);
                if onedrive_docs.is_dir() {
                    return Some(onedrive_docs);
                }
            }
        }
    } else if let Ok(user_profile) = std::env::var("USERPROFILE") {
        let p = PathBuf::from(user_profile).join(def.rel_path);
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

/// Analyzes save files in a folder and determines pruning candidates.
pub fn analyze_save_dir(
    save_dir: &Path,
    file_ext: &str,
    retain_count: usize,
) -> (Vec<SaveFileEntry>, Vec<SaveFileEntry>) {
    let mut auto_saves = Vec::new();
    let mut manual_saves = Vec::new();

    if let Ok(entries) = std::fs::read_dir(save_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or_default();
                if ext.eq_ignore_ascii_case(file_ext) || file_ext == "*" {
                    let filename = entry.file_name().to_string_lossy().to_string();
                    if let Ok(meta) = entry.metadata() {
                        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                        let kind = classify_save_file(&filename);
                        let save_entry = SaveFileEntry {
                            path,
                            filename,
                            size_bytes: meta.len(),
                            mtime,
                            kind,
                        };

                        match kind {
                            SaveKind::AutoOrQuick => auto_saves.push(save_entry),
                            SaveKind::Manual => manual_saves.push(save_entry),
                        }
                    }
                }
            }
        }
    }

    // Sort auto/quicksaves by modification time descending (newest first)
    auto_saves.sort_by_key(|b| std::cmp::Reverse(b.mtime));

    let mut kept = manual_saves;
    let mut excess = Vec::new();

    for (idx, save) in auto_saves.into_iter().enumerate() {
        if idx < retain_count {
            kept.push(save);
        } else {
            excess.push(save);
        }
    }

    (kept, excess)
}

/// Scans all supported RPG save folders and returns excess autosave artifacts.
pub fn scan_all_rpg_save_bloat(retain_count: usize) -> Vec<JanitorArtifact> {
    let mut artifacts = Vec::new();

    for def in SUPPORTED_RPGS {
        if let Some(dir) = resolve_save_dir(def) {
            let (_kept, excess) = analyze_save_dir(&dir, def.file_extension, retain_count);
            for entry in excess {
                artifacts.push(JanitorArtifact {
                    path: entry.path,
                    category: Category::SaveBloat,
                    size_bytes: entry.size_bytes,
                    description: format!(
                        "Excess autosave/quicksave in {} ({})",
                        def.title, entry.filename
                    ),
                    is_safe_default: false, // Explicit user confirmation required
                    requires_backup: true,  // Zero-Data-Loss Shield
                    app_id: None,
                    game_title: Some(def.title.to_string()),
                });
            }
        }
    }

    artifacts
}

/// Zero-Data-Loss Shield: Creates a ZIP backup of save files before deletion.
pub fn create_save_backup_zip(
    save_files: &[PathBuf],
    target_backup_dir: &Path,
    game_prefix: &str,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(target_backup_dir)?;

    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let safe_prefix = game_prefix.replace([' ', ':', '\\', '/', '<', '>', '"', '|', '?', '*'], "_");
    let zip_filename = format!("save_backup_{safe_prefix}_{timestamp}.zip");
    let zip_path = target_backup_dir.join(zip_filename);

    let file = File::create(&zip_path)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for save_path in save_files {
        if save_path.is_file() {
            let fname = save_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("save.dat");
            zip.start_file(fname, options)?;
            let mut f = File::open(save_path)?;
            let mut buffer = Vec::new();
            f.read_to_end(&mut buffer)?;
            zip.write_all(&buffer)?;
        }
    }

    zip.finish()?;
    Ok(zip_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_classify_saves() {
        assert_eq!(classify_save_file("autosave1.ess"), SaveKind::AutoOrQuick);
        assert_eq!(classify_save_file("quicksave.ess"), SaveKind::AutoOrQuick);
        assert_eq!(
            classify_save_file("AutoSave_001.sav"),
            SaveKind::AutoOrQuick
        );
        assert_eq!(classify_save_file("Save_001.ess"), SaveKind::Manual);
        assert_eq!(
            classify_save_file("Character_Hardcore_Ending.sav"),
            SaveKind::Manual
        );
    }

    #[test]
    fn test_save_backup_zip() {
        let temp = tempdir().unwrap();
        let save1 = temp.path().join("autosave1.ess");
        let save2 = temp.path().join("quicksave.ess");
        std::fs::write(&save1, b"SAVE_DATA_1").unwrap();
        std::fs::write(&save2, b"SAVE_DATA_2").unwrap();

        let backup_dir = temp.path().join("backups");
        let zip_path = create_save_backup_zip(&[save1, save2], &backup_dir, "Skyrim_SE").unwrap();

        assert!(zip_path.exists());
        assert!(zip_path.metadata().unwrap().len() > 0);
    }
}
