//! Anti-Cheat & Multi-Player Shield.
//!
//! Scans game directories for known anti-cheat systems (EasyAntiCheat, BattlEye,
//! Riot Vanguard, Denuvo, Valve Anti-Cheat, Ricochet, Tencent ACE, PunkBuster, GameGuard).
//!
//! If anti-cheat software is detected, modifying or zeroing monolithic archives
//! is blocked by default to prevent anti-cheat integrity mismatches or multiplayer bans.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;
use walkdir::WalkDir;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AntiCheatEngine {
    EasyAntiCheat,
    BattlEye,
    RiotVanguard,
    ValveAntiCheat,
    DenuvoAntiCheat,
    Ricochet,
    GameGuard,
    TencentACE,
    PunkBuster,
    Custom(String),
}

impl std::fmt::Display for AntiCheatEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AntiCheatEngine::EasyAntiCheat => write!(f, "Easy Anti-Cheat (EAC)"),
            AntiCheatEngine::BattlEye => write!(f, "BattlEye"),
            AntiCheatEngine::RiotVanguard => write!(f, "Riot Vanguard"),
            AntiCheatEngine::ValveAntiCheat => write!(f, "Valve Anti-Cheat (VAC)"),
            AntiCheatEngine::DenuvoAntiCheat => write!(f, "Denuvo Anti-Cheat / Anti-Tamper"),
            AntiCheatEngine::Ricochet => write!(f, "Activision Ricochet"),
            AntiCheatEngine::GameGuard => write!(f, "nProtect GameGuard"),
            AntiCheatEngine::TencentACE => write!(f, "Tencent Anti-Cheat Expert (ACE)"),
            AntiCheatEngine::PunkBuster => write!(f, "Even Balance PunkBuster"),
            AntiCheatEngine::Custom(name) => write!(f, "Anti-Cheat ({name})"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AntiCheatFinding {
    pub engine: AntiCheatEngine,
    pub detected_files: Vec<PathBuf>,
    pub warning: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SafetyReport {
    pub is_safe: bool,
    pub findings: Vec<AntiCheatFinding>,
}

#[derive(Error, Debug)]
pub enum SafetyError {
    #[error("Anti-cheat detected in game directory: {engine} (Files: {detected_files:?}). Trimming blocked to protect multiplayer account.")]
    AntiCheatDetected {
        engine: AntiCheatEngine,
        detected_files: Vec<PathBuf>,
    },
    #[error("I/O error during anti-cheat scan: {0}")]
    Io(#[from] std::io::Error),
    #[error("Directory traversal failed during anti-cheat scan: {0}")]
    Traversal(String),
}

/// Scans `game_dir` for known anti-cheat systems.
///
/// Returns `Ok(SafetyReport)` containing any detected anti-cheat systems.
/// If `enforce_safety` is true and any anti-cheat is found, returns `Err(SafetyError::AntiCheatDetected)`.
pub fn check_game_safety(
    game_dir: &Path,
    enforce_safety: bool,
) -> Result<SafetyReport, SafetyError> {
    let mut findings = Vec::new();

    let mut eac_files = Vec::new();
    let mut be_files = Vec::new();
    let mut vanguard_files = Vec::new();
    let mut vac_files = Vec::new();
    let mut denuvo_files = Vec::new();
    let mut ricochet_files = Vec::new();
    let mut gameguard_files = Vec::new();
    let mut ace_files = Vec::new();
    let mut pb_files = Vec::new();

    // Walk the complete declared game root. A depth cap can miss a protected
    // module while later archive discovery descends further into the tree.
    for entry in WalkDir::new(game_dir).follow_links(false).into_iter() {
        let entry = entry.map_err(|error| SafetyError::Traversal(error.to_string()))?;
        let path = entry.path();
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase();
        let relative = path.strip_prefix(game_dir).unwrap_or(path);

        // EasyAntiCheat checks
        if file_name.contains("easyanticheat")
            || file_name == "easyanticheat.sys"
            || file_name == "easyanticheat_eos.sys"
            || file_name == "easyanticheat_setup.exe"
            || file_name.starts_with("easyanticheat_")
            || (entry.file_type().is_dir()
                && (file_name == "easyanticheat" || file_name == "easyanticheat_eos"))
        {
            eac_files.push(relative.to_path_buf());
        }

        // BattlEye checks
        if file_name == "beservice.exe"
            || file_name == "beservice_x64.exe"
            || file_name == "bedaisy.sys"
            || file_name == "beclient_x64.dll"
            || file_name == "beclient.dll"
            || (entry.file_type().is_dir() && (file_name == "battleye" || file_name == "be"))
        {
            be_files.push(relative.to_path_buf());
        }

        // Vanguard checks
        if file_name == "vgk.sys" || file_name == "vgtray.exe" || file_name == "vgc.exe" {
            vanguard_files.push(relative.to_path_buf());
        }

        // Valve Anti-Cheat module names. Avoid broad Steam-runtime matches such
        // as steamservice.dll, which is present in non-VAC titles too.
        if matches!(
            file_name.as_str(),
            "vac.dll" | "vac2.dll" | "vacmodule.dll" | "vac_module.dll" | "vacmodule2.dll"
        ) {
            vac_files.push(relative.to_path_buf());
        }

        // Denuvo checks
        if file_name == "dbdata.dll" || file_name.starts_with("denuvo") {
            denuvo_files.push(relative.to_path_buf());
        }

        // Ricochet checks
        if file_name == "randgrid.sys" {
            ricochet_files.push(relative.to_path_buf());
        }

        // GameGuard checks
        if file_name == "gamemon.des" || (entry.file_type().is_dir() && file_name == "gameguard") {
            gameguard_files.push(relative.to_path_buf());
        }

        // Tencent ACE checks
        if file_name.contains("anticheatexpert")
            || file_name == "sguard64.dll"
            || (entry.file_type().is_dir()
                && (file_name == "anticheatexpert" || file_name == "sguard"))
        {
            ace_files.push(relative.to_path_buf());
        }

        // PunkBuster checks
        if file_name == "pbsvc.exe"
            || file_name == "pbcl.dll"
            || (entry.file_type().is_dir() && file_name == "pb")
        {
            pb_files.push(relative.to_path_buf());
        }
    }

    if !eac_files.is_empty() {
        findings.push(AntiCheatFinding {
            engine: AntiCheatEngine::EasyAntiCheat,
            detected_files: eac_files,
            warning: "Easy Anti-Cheat protected title. Archive modification may trigger integrity violations.".to_string(),
        });
    }

    if !be_files.is_empty() {
        findings.push(AntiCheatFinding {
            engine: AntiCheatEngine::BattlEye,
            detected_files: be_files,
            warning:
                "BattlEye protected title. Archive modification may cause game launch rejection."
                    .to_string(),
        });
    }

    if !vanguard_files.is_empty() {
        findings.push(AntiCheatFinding {
            engine: AntiCheatEngine::RiotVanguard,
            detected_files: vanguard_files,
            warning: "Riot Vanguard protected title. Client modification strictly prohibited."
                .to_string(),
        });
    }

    if !vac_files.is_empty() {
        findings.push(AntiCheatFinding {
            engine: AntiCheatEngine::ValveAntiCheat,
            detected_files: vac_files,
            warning: "Valve Anti-Cheat module detected. Archive modification is blocked."
                .to_string(),
        });
    }

    if !denuvo_files.is_empty() {
        findings.push(AntiCheatFinding {
            engine: AntiCheatEngine::DenuvoAntiCheat,
            detected_files: denuvo_files,
            warning: "Denuvo Anti-Tamper / Anti-Cheat detected.".to_string(),
        });
    }

    if !ricochet_files.is_empty() {
        findings.push(AntiCheatFinding {
            engine: AntiCheatEngine::Ricochet,
            detected_files: ricochet_files,
            warning: "Activision Ricochet driver detected.".to_string(),
        });
    }

    if !gameguard_files.is_empty() {
        findings.push(AntiCheatFinding {
            engine: AntiCheatEngine::GameGuard,
            detected_files: gameguard_files,
            warning: "nProtect GameGuard detected.".to_string(),
        });
    }

    if !ace_files.is_empty() {
        findings.push(AntiCheatFinding {
            engine: AntiCheatEngine::TencentACE,
            detected_files: ace_files,
            warning: "Tencent Anti-Cheat Expert detected.".to_string(),
        });
    }

    if !pb_files.is_empty() {
        findings.push(AntiCheatFinding {
            engine: AntiCheatEngine::PunkBuster,
            detected_files: pb_files,
            warning: "PunkBuster detected.".to_string(),
        });
    }

    let is_safe = findings.is_empty();

    if enforce_safety && !is_safe {
        if let Some(first) = findings.first() {
            return Err(SafetyError::AntiCheatDetected {
                engine: first.engine.clone(),
                detected_files: first.detected_files.clone(),
            });
        }
    }

    Ok(SafetyReport { is_safe, findings })
}

/// Anti-cheat detection shield helper.
pub struct AntiCheatShield;

impl AntiCheatShield {
    /// Checks a list of relative paths purely in memory without disk I/O.
    /// Returns `true` if no anti-cheat software signatures are detected.
    pub fn is_safe_from_relative_paths(
        rel_paths: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> bool {
        for p in rel_paths {
            let s = p.as_ref();
            let lower = s.to_ascii_lowercase();

            // Direct substring matches
            if lower.contains("easyanticheat")
                || lower.contains("beservice")
                || lower.contains("bedaisy.sys")
                || lower.contains("beclient")
                || lower.contains("battleye")
                || lower.contains("vgk.sys")
                || lower.contains("vgtray.exe")
                || lower.contains("vgc.exe")
                || lower.contains("vanguard")
                || lower.contains("vacmodule.dll")
                || lower.contains("vac_module.dll")
                || lower.contains("vacmodule2.dll")
                || lower.contains("denuvo")
                || lower.contains("dbdata.dll")
                || lower.contains("randgrid.sys")
                || lower.contains("ricochet")
                || lower.contains("gameguard")
                || lower.contains("gamemon.des")
                || lower.contains("anticheatexpert")
                || lower.contains("sguard64.dll")
                || lower.contains("punkbuster")
                || lower.contains("pbsvc.exe")
                || lower.contains("pbcl.dll")
            {
                return false;
            }

            // Segment checks for short names (to avoid false positive substrings like "maybe" for "be")
            for segment in lower.split(['\\', '/']) {
                if segment == "be"
                    || segment == "pb"
                    || segment == "sguard"
                    || segment == "vac.dll"
                    || segment == "vac2.dll"
                {
                    return false;
                }
            }
        }
        true
    }

    /// Checks a game directory for anti-cheat software.
    pub fn check_directory(game_dir: &Path, enforce: bool) -> Result<SafetyReport, SafetyError> {
        check_game_safety(game_dir, enforce)
    }

    /// Returns `true` only when a complete anti-cheat scan succeeds and finds no signatures.
    pub fn is_safe(game_dir: &Path) -> bool {
        check_game_safety(game_dir, false)
            .map(|report| report.is_safe)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_safe_directory_returns_is_safe_true() {
        let dir = tempdir().expect("tempdir");
        let fake_game = dir.path().join("Game.exe");
        fs::write(&fake_game, b"fake exe").expect("write");

        let report = check_game_safety(dir.path(), false).expect("check safety");
        assert!(report.is_safe);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn test_eac_detection_blocks_when_enforced() {
        let dir = tempdir().expect("tempdir");
        let eac_dir = dir.path().join("EasyAntiCheat");
        fs::create_dir_all(&eac_dir).expect("create EAC dir");
        fs::write(eac_dir.join("easyanticheat_x64.dll"), b"eac binary").expect("write dll");

        let report = check_game_safety(dir.path(), false).expect("check safety non-enforced");
        assert!(!report.is_safe);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].engine, AntiCheatEngine::EasyAntiCheat);

        let enforced_res = check_game_safety(dir.path(), true);
        assert!(enforced_res.is_err());
        match enforced_res.unwrap_err() {
            SafetyError::AntiCheatDetected { engine, .. } => {
                assert_eq!(engine, AntiCheatEngine::EasyAntiCheat);
            }
            _ => panic!("Expected AntiCheatDetected error"),
        }
    }

    #[test]
    fn test_battleye_detection() {
        let dir = tempdir().expect("tempdir");
        let be_file = dir.path().join("BEService.exe");
        fs::write(&be_file, b"battleye service").expect("write");

        let report = check_game_safety(dir.path(), false).expect("check safety");
        assert!(!report.is_safe);
        assert_eq!(report.findings[0].engine, AntiCheatEngine::BattlEye);
    }

    #[test]
    fn test_vac_detection() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("vac_module.dll"), b"vac module").expect("write");

        let report = check_game_safety(dir.path(), false).expect("check safety");
        assert!(!report.is_safe);
        assert_eq!(report.findings[0].engine, AntiCheatEngine::ValveAntiCheat);
    }

    #[test]
    fn test_missing_directory_fails_closed() {
        let dir = tempdir().expect("tempdir");
        let missing = dir.path().join("missing-game");
        assert!(check_game_safety(&missing, false).is_err());
        assert!(!AntiCheatShield::is_safe(&missing));
    }

    #[test]
    fn test_is_safe_from_relative_paths() {
        assert!(AntiCheatShield::is_safe_from_relative_paths([
            "bin/Game.exe",
            "Data/Audio/Voices.pck",
            "maybe/file.txt",
            "content/paks/pakchunk0.pak",
        ]));

        assert!(!AntiCheatShield::is_safe_from_relative_paths([
            "EasyAntiCheat/easyanticheat_x64.dll",
        ]));

        assert!(!AntiCheatShield::is_safe_from_relative_paths([
            "BEService.exe",
        ]));

        assert!(!AntiCheatShield::is_safe_from_relative_paths([
            "be/BEClient.dll",
        ]));

        assert!(!AntiCheatShield::is_safe_from_relative_paths([
            "bin/denuvo64.dll",
        ]));

        assert!(!AntiCheatShield::is_safe_from_relative_paths(["vgc.exe",]));

        assert!(!AntiCheatShield::is_safe_from_relative_paths([
            "GameGuard/gamemon.des",
        ]));

        assert!(!AntiCheatShield::is_safe_from_relative_paths(
            ["pbsvc.exe",]
        ));

        assert!(!AntiCheatShield::is_safe_from_relative_paths([
            "AntiCheatExpert/sguard64.dll",
        ]));

        assert!(!AntiCheatShield::is_safe_from_relative_paths([
            "bin/vacmodule.dll",
        ]));
    }

    #[test]
    fn test_shallow_is_safe_directory() {
        let dir = tempdir().expect("tempdir");
        let fake_game = dir.path().join("Game.exe");
        fs::write(&fake_game, b"fake exe").expect("write");
        assert!(AntiCheatShield::is_safe(dir.path()));

        let be_file = dir.path().join("BEService.exe");
        fs::write(&be_file, b"battleye service").expect("write");
        assert!(!AntiCheatShield::is_safe(dir.path()));
    }
}
