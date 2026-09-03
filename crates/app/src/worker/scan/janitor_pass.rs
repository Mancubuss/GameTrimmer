//! The janitor pass: the artifacts that sit *outside* every game install
//! directory.
//!
//! The rules engine classifies files under a game's install directory, and
//! `orphan_analysis` diffs a launcher's own managed container against its
//! manifests. Neither can see a Windows Error Reporting dump in
//! `%LOCALAPPDATA%\CrashDumps`, a GPU driver's stale shader cache, a
//! launcher's CEF web cache, or the hundredth autosave of an RPG in
//! `Documents` - which is where `gametrimmer_core::janitor` looks, and why its
//! findings need a pass of their own.
//!
//! What this deliberately does **not** call:
//!
//! - `janitor::workshop` and `janitor::staging::scan_steam_downloading`. Both
//!   areas are already detected, with stricter evidence, by the orphan pass
//!   (`steamapps/downloading` is a Steam service folder; a Workshop item is
//!   checked against that appid's own `appworkshop_<appid>.acf`). Running the
//!   janitor's version too would offer the same bytes twice, under a second
//!   confidence, with the weaker of the two safety checks.
//! - `janitor::crashes::scan_game_engine_crashes`. `Saved\Crashes`,
//!   `Saved\Logs`, `*.dmp` and `player.log` inside a game are `rules.json`
//!   territory (categories `crash_dump` and `diagnostic_logs`) and are found
//!   by the ordinary per-game classification.
//!
//! Every artifact here is persisted by `orphan_analysis::persist_rootless`
//! alongside the orphan rows: one writer, because each call replaces the whole
//! set of `NULL`-game rows.

use std::collections::{HashMap, HashSet};

use gametrimmer_core::janitor::{self, JanitorArtifact, JanitorConfig};

use super::orphan_analysis::{measure_single_file, PreparedRootless};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use gametrimmer_core::providers::{DiscoveredLibrary, OrphanEvidence};
use gametrimmer_core::scanner::scan_dir_cancellable;

use crate::model::{category_enabled, DisplayCategory, FindingSource};

/// Confidence for an artifact the janitor considers safe by default (a stale
/// cache file, a crash dump, a launcher's web cache).
///
/// Below `crate::model::AUTO_SELECT_CONFIDENCE_THRESHOLD` on purpose, for the
/// same reason the orphan confidences are: these live outside every game
/// directory, so nothing here is ever pre-ticked by the bare-confidence path.
const JANITOR_SAFE_CONFIDENCE: u8 = 80;

/// Confidence for an artifact the janitor wants a human to look at - today
/// exactly the save-game pruner, whose deletions are irreplaceable player
/// progress even with the backup ZIP behind them.
const JANITOR_REVIEW_CONFIDENCE: u8 = 60;

pub(super) struct JanitorCollection {
    pub(super) findings: Vec<PreparedRootless>,
    /// Every directory an artifact was read from, deduplicated. Recorded into
    /// `scan_library_evidence` by the caller: the delete preflight resolves a
    /// `NULL`-game row's `file_safety.evidence_library_path` back to one of
    /// these, and a row with no such evidence can never be deleted.
    pub(super) evidence_roots: Vec<PathBuf>,
}

impl JanitorCollection {
    fn empty() -> Self {
        Self {
            findings: Vec::new(),
            evidence_roots: Vec::new(),
        }
    }
}

/// Collects the janitor artifacts for the categories the user left enabled.
///
/// The per-category gate is applied here rather than after collection because
/// each of these scanners walks real directories: a disabled category should
/// cost nothing, not be found and then discarded.
pub(super) fn collect_janitor(
    libraries: &[DiscoveredLibrary],
    enabled_categories: &[String],
    config: &JanitorConfig,
    cancel: &AtomicBool,
) -> JanitorCollection {
    let mut artifacts: Vec<JanitorArtifact> = Vec::new();

    if category_enabled(enabled_categories, DisplayCategory::Crashes) {
        // The WER folder catches every dump on the machine, not just games'
        // (GT-230), so a dump is only offered once it is attributed to a
        // known game executable. Building that attribution means walking
        // every game's install directory, which is only worth doing when the
        // WER folder actually has candidate dumps waiting - checking that
        // first costs one `read_dir` instead of a full library walk on every
        // scan of a library that has never crashed a game.
        let wer_candidates = janitor::crashes::wer_crash_dump_candidate_exes();
        if !wer_candidates.is_empty() {
            let known_games = known_game_executables(libraries, &wer_candidates, cancel);
            artifacts.extend(janitor::crashes::scan_windows_wer_crash_dumps(&known_games));
        }
        artifacts.extend(janitor::crashes::scan_unity_logs());
    }
    if cancel.load(Ordering::Relaxed) {
        return JanitorCollection::empty();
    }

    if category_enabled(enabled_categories, DisplayCategory::ShaderCache) {
        artifacts.extend(janitor::shadercache::scan_system_shader_caches(
            config.shader_stale_days,
        ));
        let installed = installed_app_ids(libraries);
        for library in libraries {
            // Only Steam keeps a `steamapps/shadercache`, and only a library
            // whose manifest inventory is provably complete can say an appid
            // is *not* installed - the same gate the orphan pass applies
            // before calling anything abandoned.
            if library.vendor != "steam" || library.orphan_evidence != OrphanEvidence::Authoritative
            {
                continue;
            }
            artifacts.extend(janitor::shadercache::scan_steam_shader_cache(
                &library.path,
                &installed,
            ));
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return JanitorCollection::empty();
    }

    if category_enabled(enabled_categories, DisplayCategory::LauncherCache) {
        artifacts.extend(janitor::launchers::scan_launcher_web_caches());
        artifacts.extend(janitor::launchers::scan_mod_manager_downloads());
    }
    if cancel.load(Ordering::Relaxed) {
        return JanitorCollection::empty();
    }

    if category_enabled(enabled_categories, DisplayCategory::Saves) {
        artifacts.extend(janitor::saves::scan_save_locations(
            config.save_retention_count,
        ));
    }

    prepare(artifacts, libraries, cancel)
}

/// The vendor ids of every discovered game, as the janitor's "is this appid
/// still installed?" checks want them.
fn installed_app_ids(libraries: &[DiscoveredLibrary]) -> HashSet<String> {
    libraries
        .iter()
        .flat_map(|library| library.games.iter())
        .filter_map(|game| game.app_id.clone())
        .collect()
}

/// How deep under a game's install directory to look for its executables
/// when attributing a WER crash dump (GT-230). `Binaries\Win64\game.exe`
/// sits three segments below the install root, and that is the ceiling: a
/// game that buries its executable deeper is not matched, and an unmatched
/// dump is dropped rather than offered, which is the safe direction.
const MAX_EXE_SEARCH_DEPTH: usize = 3;

/// Builds the `lowercased exe name -> game title` index the WER crash pass
/// needs to attribute a dump to a game.
///
/// Only resolves the executables named in `candidates`: walking every game's
/// install directory is not free on a library of thousands of games, so this
/// stops as soon as every candidate has been found (or cancellation fires).
fn known_game_executables(
    libraries: &[DiscoveredLibrary],
    candidates: &HashSet<String>,
    cancel: &AtomicBool,
) -> HashMap<String, String> {
    let mut index = HashMap::new();
    for library in libraries {
        for game in &library.games {
            if cancel.load(Ordering::Relaxed) || index.len() >= candidates.len() {
                return index;
            }
            collect_exe_names(
                &game.install_dir,
                0,
                &game.name,
                candidates,
                &mut index,
                cancel,
            );
        }
    }
    index
}

/// Recursive, depth-bounded, cancellable walk collecting `*.exe` base names
/// under `dir` that appear in `candidates`, keyed lowercased with `game_title`
/// as the value.
fn collect_exe_names(
    dir: &Path,
    depth: usize,
    game_title: &str,
    candidates: &HashSet<String>,
    index: &mut HashMap<String, String>,
    cancel: &AtomicBool,
) {
    if depth > MAX_EXE_SEARCH_DEPTH
        || index.len() >= candidates.len()
        || cancel.load(Ordering::Relaxed)
    {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if index.len() >= candidates.len() || cancel.load(Ordering::Relaxed) {
            return;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_exe_names(&path, depth + 1, game_title, candidates, index, cancel);
        } else if file_type.is_file() {
            let is_exe = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
            if !is_exe {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let lower = name.to_ascii_lowercase();
            if candidates.contains(&lower) {
                index.entry(lower).or_insert_with(|| game_title.to_string());
            }
        }
    }
}

/// Measures each artifact and turns it into a persistable finding.
///
/// The size the janitor reported is not carried over: it is a logical sum, and
/// every other finding in the app is shown and totalled by its allocated size
/// on disk. Re-measuring here produces both figures the way the orphan pass
/// does. An artifact whose measurement fails is dropped rather than persisted
/// with a guessed size - incomplete evidence must never create a finding.
fn prepare(
    artifacts: Vec<JanitorArtifact>,
    libraries: &[DiscoveredLibrary],
    cancel: &AtomicBool,
) -> JanitorCollection {
    let library_roots: HashSet<PathBuf> = libraries
        .iter()
        .map(|library| library.path.clone())
        .collect();
    let mut findings = Vec::with_capacity(artifacts.len());
    let mut evidence_roots: Vec<PathBuf> = Vec::new();

    for artifact in artifacts {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        // The directory the artifact was read from is its discovery evidence.
        // A path with no parent (a bare drive root) is not something the
        // janitor ever reports, and would leave the row with no evidence to
        // record, so it is skipped rather than persisted unusable.
        let Some(evidence_root) = artifact.path.parent().map(Path::to_path_buf) else {
            continue;
        };
        // A janitor area must never claim to be a discovered library root:
        // `scan_library_evidence` is keyed by path, so writing one would
        // overwrite that library's real discovery status with the janitor's.
        if library_roots.contains(&evidence_root) {
            continue;
        }

        let measured = if artifact.path.is_dir() {
            scan_dir_cancellable(&artifact.path, cancel)
                .map(|entries| {
                    entries.iter().fold((0u64, 0u64), |(size, on_disk), entry| {
                        (size + entry.size, on_disk + entry.size_on_disk)
                    })
                })
                .map_err(|err| err.to_string())
        } else {
            measure_single_file(&artifact.path)
        };
        let Ok((size, size_on_disk)) = measured else {
            crate::logger::error(&format!(
                "Janitor artifact skipped, it could not be measured: {}",
                artifact.path.display()
            ));
            continue;
        };
        if size == 0 {
            continue;
        }

        if !evidence_roots.contains(&evidence_root) {
            evidence_roots.push(evidence_root.clone());
        }
        findings.push(PreparedRootless {
            full_path: artifact.path,
            evidence_library_path: evidence_root,
            size,
            size_on_disk,
            source: FindingSource::Rule(artifact.category),
            reason: artifact.description,
            confidence: if artifact.requires_backup || !artifact.is_safe_default {
                JANITOR_REVIEW_CONFIDENCE
            } else {
                JANITOR_SAFE_CONFIDENCE
            },
            group_dir: artifact.group_dir,
        });
    }

    JanitorCollection {
        findings,
        evidence_roots,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gametrimmer_core::providers::GameInstall;
    use gametrimmer_core::rules::Category;
    use tempfile::tempdir;

    fn artifact(path: PathBuf, category: Category, requires_backup: bool) -> JanitorArtifact {
        JanitorArtifact {
            path,
            category,
            size_bytes: 0,
            description: "Test artifact".to_string(),
            is_safe_default: !requires_backup,
            requires_backup,
            app_id: None,
            game_title: None,
            group_dir: None,
        }
    }

    #[test]
    fn prepared_artifacts_are_measured_and_carry_their_own_directory_as_evidence() {
        let temp = tempdir().expect("tempdir");
        let dump = temp.path().join("crash.dmp");
        std::fs::write(&dump, vec![0u8; 4096]).expect("write dump");

        let cancel = AtomicBool::new(false);
        let collection = prepare(
            vec![artifact(dump.clone(), Category::CrashDump, false)],
            &[],
            &cancel,
        );

        assert_eq!(collection.findings.len(), 1);
        let finding = &collection.findings[0];
        assert_eq!(finding.full_path, dump);
        assert_eq!(finding.size, 4096);
        assert_eq!(finding.source, FindingSource::Rule(Category::CrashDump));
        assert_eq!(finding.confidence, JANITOR_SAFE_CONFIDENCE);
        assert_eq!(
            collection.evidence_roots,
            vec![temp.path().to_path_buf()],
            "the directory the artifact was read from is its discovery evidence"
        );
    }

    #[test]
    fn an_artifact_that_no_longer_exists_is_dropped_rather_than_guessed() {
        let temp = tempdir().expect("tempdir");
        let cancel = AtomicBool::new(false);

        let collection = prepare(
            vec![artifact(
                temp.path().join("vanished.dmp"),
                Category::CrashDump,
                false,
            )],
            &[],
            &cancel,
        );

        assert!(collection.findings.is_empty());
        assert!(collection.evidence_roots.is_empty());
    }

    #[test]
    fn a_save_artifact_stays_below_the_auto_select_threshold() {
        let temp = tempdir().expect("tempdir");
        let save = temp.path().join("quicksave.sav");
        std::fs::write(&save, vec![0u8; 128]).expect("write save");

        let cancel = AtomicBool::new(false);
        let collection = prepare(
            vec![artifact(save, Category::SaveBloat, true)],
            &[],
            &cancel,
        );

        assert_eq!(collection.findings.len(), 1);
        assert!(
            collection.findings[0].confidence < crate::model::AUTO_SELECT_CONFIDENCE_THRESHOLD,
            "a save is never pre-ticked by confidence alone"
        );
    }

    #[test]
    fn known_game_executables_finds_an_exe_within_the_bounded_depth() {
        let temp = tempdir().expect("tempdir");
        let install_dir = temp.path().join("Portal 2");
        let bin_dir = install_dir.join("bin").join("win64");
        std::fs::create_dir_all(&bin_dir).expect("create bin dir");
        std::fs::write(bin_dir.join("portal2.exe"), b"exe").expect("write exe");

        let library = DiscoveredLibrary {
            vendor: "steam",
            path: temp.path().to_path_buf(),
            games: vec![GameInstall {
                name: "Portal 2".to_string(),
                install_dir,
                app_id: Some("620".to_string()),
            }],
            orphan_evidence: OrphanEvidence::Authoritative,
        };

        let mut candidates = HashSet::new();
        candidates.insert("portal2.exe".to_string());
        let cancel = AtomicBool::new(false);

        let index = known_game_executables(&[library], &candidates, &cancel);

        assert_eq!(index.get("portal2.exe"), Some(&"Portal 2".to_string()));
    }

    #[test]
    fn known_game_executables_ignores_an_exe_that_is_not_a_candidate() {
        let temp = tempdir().expect("tempdir");
        let install_dir = temp.path().join("Some Game");
        std::fs::create_dir_all(&install_dir).expect("create install dir");
        std::fs::write(install_dir.join("somegame.exe"), b"exe").expect("write exe");

        let library = DiscoveredLibrary {
            vendor: "steam",
            path: temp.path().to_path_buf(),
            games: vec![GameInstall {
                name: "Some Game".to_string(),
                install_dir,
                app_id: None,
            }],
            orphan_evidence: OrphanEvidence::Authoritative,
        };

        // Candidate set names an unrelated executable, so nothing resolves.
        let mut candidates = HashSet::new();
        candidates.insert("link.exe".to_string());
        let cancel = AtomicBool::new(false);

        let index = known_game_executables(&[library], &candidates, &cancel);

        assert!(index.is_empty());
    }
}
