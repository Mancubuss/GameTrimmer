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

impl AntiCheatEngine {
    /// The user-facing reason trimming is held back for this engine.
    fn warning(&self) -> &'static str {
        match self {
            AntiCheatEngine::EasyAntiCheat => {
                "Easy Anti-Cheat protected title. Archive modification may trigger integrity violations."
            }
            AntiCheatEngine::BattlEye => {
                "BattlEye protected title. Archive modification may cause game launch rejection."
            }
            AntiCheatEngine::RiotVanguard => {
                "Riot Vanguard protected title. Client modification strictly prohibited."
            }
            AntiCheatEngine::ValveAntiCheat => {
                "Valve Anti-Cheat module detected. Archive modification is blocked."
            }
            AntiCheatEngine::DenuvoAntiCheat => "Denuvo Anti-Tamper / Anti-Cheat detected.",
            AntiCheatEngine::Ricochet => "Activision Ricochet driver detected.",
            AntiCheatEngine::GameGuard => "nProtect GameGuard detected.",
            AntiCheatEngine::TencentACE => "Tencent Anti-Cheat Expert detected.",
            AntiCheatEngine::PunkBuster => "PunkBuster detected.",
            AntiCheatEngine::Custom(_) => "Anti-cheat software detected.",
        }
    }
}

/// Folder names that mean an anti-cheat, matched against one *path segment*
/// and never as a substring of the whole path.
///
/// Whole-path substring matching is what made a single-player game look
/// protected: "Ricochet" and "Vanguard" are ordinary English words, and
/// `cfg\skills\hero\bountyhunter\ricochet.cfg` is a Bounty Hunter skill
/// config, not Activision's driver. A `Ricochet\` *folder* still matches.
///
/// These three tables are the one declared policy. Both detectors read them -
/// the on-disk walk in [`check_game_safety`] and the in-memory path check in
/// [`AntiCheatShield::is_safe_from_relative_paths`] - so the two verdicts
/// cannot drift apart the way two hand-maintained chains of `||` did. Split by
/// how a row is matched rather than by engine because that is what lets a
/// segment skip the rules that cannot apply to it.
static DIR_SIGNATURES: &[(AntiCheatEngine, &str)] = {
    use AntiCheatEngine as E;
    &[
        (E::EasyAntiCheat, "easyanticheat"),
        (E::EasyAntiCheat, "easyanticheat_eos"),
        (E::BattlEye, "battleye"),
        (E::BattlEye, "be"),
        // The folder rule the disk walk never had; until now the preflight had
        // to take the union of both detectors to cover it (GT-443).
        (E::RiotVanguard, "vanguard"),
        (E::DenuvoAntiCheat, "denuvo"),
        (E::Ricochet, "ricochet"),
        (E::GameGuard, "gameguard"),
        (E::TencentACE, "anticheatexpert"),
        (E::TencentACE, "sguard"),
        (E::PunkBuster, "punkbuster"),
        (E::PunkBuster, "pb"),
    ]
};

/// Exact file names that mean an anti-cheat.
static FILE_SIGNATURES: &[(AntiCheatEngine, &str)] = {
    use AntiCheatEngine as E;
    &[
        (E::BattlEye, "beservice.exe"),
        (E::BattlEye, "beservice_x64.exe"),
        (E::BattlEye, "bedaisy.sys"),
        (E::BattlEye, "beclient.dll"),
        (E::BattlEye, "beclient_x64.dll"),
        (E::RiotVanguard, "vgk.sys"),
        (E::RiotVanguard, "vgtray.exe"),
        (E::RiotVanguard, "vgc.exe"),
        // Valve module names only. Broad Steam-runtime names such as
        // steamservice.dll are present in non-VAC titles too.
        (E::ValveAntiCheat, "vac.dll"),
        (E::ValveAntiCheat, "vac2.dll"),
        (E::ValveAntiCheat, "vacmodule.dll"),
        (E::ValveAntiCheat, "vacmodule2.dll"),
        (E::ValveAntiCheat, "vac_module.dll"),
        (E::DenuvoAntiCheat, "dbdata.dll"),
        (E::Ricochet, "randgrid.sys"),
        (E::GameGuard, "gamemon.des"),
        (E::TencentACE, "sguard64.dll"),
        (E::PunkBuster, "pbsvc.exe"),
        (E::PunkBuster, "pbcl.dll"),
    ]
};

/// File name prefixes that mean an anti-cheat: these engines ship a family of
/// modules under one stem (`EasyAntiCheat_x64.dll`, `EasyAntiCheat_EOS.sys`,
/// `denuvo64.dll`).
static FILE_PREFIX_SIGNATURES: &[(AntiCheatEngine, &str)] = {
    use AntiCheatEngine as E;
    &[
        (E::EasyAntiCheat, "easyanticheat"),
        (E::DenuvoAntiCheat, "denuvo"),
        (E::TencentACE, "anticheatexpert"),
    ]
};

/// Steam AppIDs of VAC-secured titles.
///
/// VAC lives in the Steam client, not in the game folder: Counter-Strike 2
/// ships no file any walk of its directory could recognise, so without this
/// list the most obviously protected game in a library scans as unprotected.
/// Kept deliberately short - every entry is a title checked to be VAC-secured
/// rather than protected by a file-detectable engine such as EAC or BattlEye.
static VAC_STEAM_APP_IDS: &[&str] = &[
    "10",   // Counter-Strike
    "240",  // Counter-Strike: Source
    "320",  // Half-Life 2: Deathmatch
    "440",  // Team Fortress 2
    "500",  // Left 4 Dead
    "550",  // Left 4 Dead 2
    "570",  // Dota 2
    "730",  // Counter-Strike 2 (formerly CS:GO)
    "4000", // Garry's Mod
];

/// ASCII-case-insensitive prefix test that does not allocate.
fn starts_with_ignore_ascii_case(haystack: &str, prefix: &str) -> bool {
    haystack.len() >= prefix.len()
        && haystack.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

/// Matches one path segment against the signature tables.
///
/// `is_dir` keeps a folder rule from firing on a file that merely shares the
/// word, a file rule from firing on a folder, and - because it picks the table
/// first - lets each segment skip every rule that cannot apply to it.
/// Comparison is case-insensitive in place: no lowercased copy of the segment
/// is allocated, because this runs once per segment of every scanned path in
/// the library.
fn match_segment(segment: &str, is_dir: bool) -> Option<&'static AntiCheatEngine> {
    let exact = if is_dir {
        DIR_SIGNATURES
    } else {
        FILE_SIGNATURES
    };
    if let Some((engine, _)) = exact
        .iter()
        .find(|(_, name)| segment.eq_ignore_ascii_case(name))
    {
        return Some(engine);
    }
    if is_dir {
        return None;
    }
    FILE_PREFIX_SIGNATURES
        .iter()
        .find(|(_, prefix)| starts_with_ignore_ascii_case(segment, prefix))
        .map(|(engine, _)| engine)
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
    // One bucket per engine actually hit, in the order the walk meets them.
    let mut hits: Vec<(&'static AntiCheatEngine, Vec<PathBuf>)> = Vec::new();

    // Walk the complete declared game root. A depth cap can miss a protected
    // module while later archive discovery descends further into the tree.
    for entry in WalkDir::new(game_dir).follow_links(false) {
        let entry = entry.map_err(|error| SafetyError::Traversal(error.to_string()))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(engine) = match_segment(name, entry.file_type().is_dir()) else {
            continue;
        };

        let relative = path.strip_prefix(game_dir).unwrap_or(path).to_path_buf();
        match hits.iter_mut().find(|(hit, _)| *hit == engine) {
            Some((_, files)) => files.push(relative),
            None => hits.push((engine, vec![relative])),
        }
    }

    let findings: Vec<AntiCheatFinding> = hits
        .into_iter()
        .map(|(engine, detected_files)| AntiCheatFinding {
            engine: engine.clone(),
            detected_files,
            warning: engine.warning().to_string(),
        })
        .collect();

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
    ///
    /// Reads the same [`SIGNATURES`] table as the on-disk walk. Every segment
    /// but the last names a directory; the last one names the file.
    pub fn is_safe_from_relative_paths(
        rel_paths: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> bool {
        for path in rel_paths {
            let mut segments = path.as_ref().split(['\\', '/']).peekable();
            while let Some(segment) = segments.next() {
                let is_dir = segments.peek().is_some();
                if !segment.is_empty() && match_segment(segment, is_dir).is_some() {
                    return false;
                }
            }
        }
        true
    }

    /// Returns `true` when this Steam title is VAC-secured.
    ///
    /// The install directory is required because `app_id` is vendor-specific:
    /// a GOG, itch or Ubisoft id can be the same digits as a Steam AppID,
    /// while a Steam game always lives under a `steamapps` library folder.
    pub fn is_vac_protected(app_id: Option<&str>, install_dir: &Path) -> bool {
        let Some(app_id) = app_id else {
            return false;
        };
        VAC_STEAM_APP_IDS.contains(&app_id)
            && install_dir.components().any(|component| {
                component
                    .as_os_str()
                    .to_str()
                    .is_some_and(|segment| segment.eq_ignore_ascii_case("steamapps"))
            })
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

    /// Materializes relative paths as real files (with their parent folders)
    /// under a fresh directory, so the on-disk walk sees exactly what the
    /// in-memory check is given.
    fn materialize(rel_paths: &[&str]) -> tempfile::TempDir {
        let dir = tempdir().expect("tempdir");
        for rel in rel_paths {
            let full = dir.path().join(rel.replace('\\', "/"));
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).expect("create parents");
            }
            fs::write(&full, b"x").expect("write");
        }
        dir
    }

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

    /// GT-222 regression: The Incredible Adventures of Van Helsing ships a
    /// Bounty Hunter skill named Ricochet. A single-player game must not be
    /// shielded because one of its config files is called after an English
    /// word that also names an anti-cheat.
    #[test]
    fn test_skill_config_named_ricochet_is_not_anti_cheat() {
        let paths = [
            "cfg\\skills\\hero\\bountyhunter\\ricochet.cfg",
            "cfg\\skills\\hero\\bountyhunter\\ricochet_lvl2.cfg",
            "data\\vanguard_armour.tex",
        ];
        assert!(AntiCheatShield::is_safe_from_relative_paths(paths));

        let dir = materialize(&paths);
        assert!(AntiCheatShield::is_safe(dir.path()));
    }

    /// The same word as a *directory* is the real signature and still fires.
    #[test]
    fn test_ricochet_directory_is_anti_cheat() {
        let paths = ["Game\\Ricochet\\anticheat.dll"];
        assert!(!AntiCheatShield::is_safe_from_relative_paths(paths));

        let dir = materialize(&paths);
        let report = check_game_safety(dir.path(), false).expect("check safety");
        assert_eq!(report.findings[0].engine, AntiCheatEngine::Ricochet);
    }

    /// The folder rule the disk walk was missing entirely before GT-222.
    #[test]
    fn test_vanguard_directory_is_anti_cheat_on_disk_too() {
        let paths = ["Riot\\Vanguard\\readme.txt"];
        assert!(!AntiCheatShield::is_safe_from_relative_paths(paths));

        let dir = materialize(&paths);
        let report = check_game_safety(dir.path(), false).expect("check safety");
        assert_eq!(report.findings[0].engine, AntiCheatEngine::RiotVanguard);
    }

    /// The two detectors read one table, so they must never disagree.
    #[test]
    fn test_both_detectors_agree_on_the_same_paths() {
        let cases: &[&[&str]] = &[
            &["bin\\Game.exe", "content\\paks\\pakchunk0.pak"],
            &["cfg\\skills\\hero\\bountyhunter\\ricochet.cfg"],
            &["Game\\Ricochet\\anticheat.dll"],
            &["Riot\\Vanguard\\readme.txt"],
            &["EasyAntiCheat\\EasyAntiCheat_x64.dll"],
            &["be\\BEClient_x64.dll"],
            &["pb\\pbsvc.exe"],
            &["AntiCheatExpert\\SGuard64.dll"],
            &["bin\\denuvo64.dll"],
            &["GameGuard\\GameMon.des"],
            &["maybe\\pbs\\vanguards.txt", "art\\ricochet_icon.png"],
        ];

        for paths in cases {
            let dir = materialize(paths);
            let from_disk = AntiCheatShield::is_safe(dir.path());
            let from_memory = AntiCheatShield::is_safe_from_relative_paths(*paths);
            assert_eq!(
                from_disk, from_memory,
                "detectors disagree on {paths:?}: disk={from_disk}, memory={from_memory}"
            );
        }
    }

    /// Counter-Strike 2 carries no anti-cheat file at all - VAC lives in the
    /// Steam client - so only the AppID list can shield it.
    #[test]
    fn test_vac_app_id_shields_a_game_with_no_signature_files() {
        let install_dir =
            Path::new("F:\\SteamLibrary\\steamapps\\common\\Counter-Strike Global Offensive");
        assert!(AntiCheatShield::is_safe_from_relative_paths([
            "game\\csgo\\pak01_dir.vpk",
            "game\\bin\\win64\\cs2.exe",
        ]));
        assert!(AntiCheatShield::is_vac_protected(Some("730"), install_dir));
    }

    #[test]
    fn test_vac_app_id_list_ignores_other_launchers_and_unknown_ids() {
        // Same digits, but a GOG install directory - not a Steam AppID.
        assert!(!AntiCheatShield::is_vac_protected(
            Some("730"),
            Path::new("F:\\GOG Games\\Some Game")
        ));
        // A Steam game that is simply not on the list.
        assert!(!AntiCheatShield::is_vac_protected(
            Some("292030"),
            Path::new("F:\\SteamLibrary\\steamapps\\common\\The Witcher 3")
        ));
        // A folder-scan or manual game has no id at all.
        assert!(!AntiCheatShield::is_vac_protected(
            None,
            Path::new("F:\\SteamLibrary\\steamapps\\common\\Whatever")
        ));
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
