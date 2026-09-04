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
/// Below `crate::model::REVIEW_CONFIDENCE_THRESHOLD` on purpose, for the same
/// reason the orphan confidences are: these live outside every game directory,
/// so each one carries the review mark and asks to be looked at before it is
/// ticked. Nothing is pre-ticked anywhere since GT-89; what this figure still
/// decides is whether the row admits to being a judgement call.
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
        // known game executable. Reading the folder's names first means a
        // machine with no dumps waiting pays one `read_dir` and builds no
        // index at all.
        let wer_candidates = janitor::crashes::wer_crash_dump_candidate_exes();
        if !wer_candidates.is_empty() {
            let known_games = known_game_executables(libraries, &wer_candidates);
            artifacts.extend(janitor::crashes::scan_windows_wer_crash_dumps(
                &known_games,
                cancel,
            ));
        }
        artifacts.extend(janitor::crashes::scan_unity_logs(cancel));
    }
    if cancel.load(Ordering::Relaxed) {
        return JanitorCollection::empty();
    }

    if category_enabled(enabled_categories, DisplayCategory::ShaderCache) {
        artifacts.extend(janitor::shadercache::scan_system_shader_caches(
            config.shader_stale_days,
            cancel,
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
                cancel,
            ));
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return JanitorCollection::empty();
    }

    if category_enabled(enabled_categories, DisplayCategory::LauncherCache) {
        artifacts.extend(janitor::launchers::scan_launcher_web_caches(cancel));
        artifacts.extend(janitor::launchers::scan_mod_manager_downloads(cancel));
    }
    if cancel.load(Ordering::Relaxed) {
        return JanitorCollection::empty();
    }

    if category_enabled(enabled_categories, DisplayCategory::Saves) {
        artifacts.extend(janitor::saves::scan_save_locations(
            config.save_retention_count,
            cancel,
        ));
    }
    if cancel.load(Ordering::Relaxed) {
        return JanitorCollection::empty();
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

/// Reduces a name to the letters and digits in it, lowercased, so an
/// executable and the game it belongs to can be compared across the
/// punctuation and spacing that separate them: `Portal2.exe` against
/// `Portal 2`, `Fallout76.exe` against `Fallout 76`.
fn comparable_name(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Builds the `lowercased exe name -> game title` index the WER crash pass
/// needs to attribute a dump to a game.
///
/// Matching is by name, against both the game's title and the folder it is
/// installed in, and touches no disk at all. The first version of this read
/// the executables off disk instead - correct, and unaffordable: with the
/// candidates in the WER folder belonging to no game (which is the ordinary
/// case, since a game's own dumps land inside its install directory), the
/// early exit never fires and every game in the library gets walked three
/// levels deep. That measured 327.6 s of housekeeping against 2.8 s before
/// it, on a library of 1637 games.
///
/// The trade is false negatives: a game whose executable is named nothing
/// like the game (`RDR2.exe`) is not matched, and its dump is dropped rather
/// than offered. That is the direction this card chose to err in.
fn known_game_executables(
    libraries: &[DiscoveredLibrary],
    candidates: &HashSet<String>,
) -> HashMap<String, String> {
    let wanted: HashMap<String, &str> = candidates
        .iter()
        .map(|exe| (comparable_name(exe.trim_end_matches(".exe")), exe.as_str()))
        .collect();
    let mut index = HashMap::new();
    for library in libraries {
        for game in &library.games {
            let folder = game
                .install_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            for name in [game.name.as_str(), folder] {
                if let Some(exe) = wanted.get(&comparable_name(name)) {
                    index
                        .entry((*exe).to_string())
                        .or_insert_with(|| game.name.clone());
                }
            }
        }
    }
    index
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
            collection.findings[0].confidence < crate::model::REVIEW_CONFIDENCE_THRESHOLD,
            "a save has to carry the review mark: since GT-89 nothing is pre-ticked at all, so the mark is the only thing left that can say to look at this one first"
        );
    }

    /// One library, so every test below can say what it holds in one line.
    fn library_of(games: &[(&str, &str)]) -> DiscoveredLibrary {
        DiscoveredLibrary {
            vendor: "steam",
            path: PathBuf::from(r"F:\SteamLibrary"),
            games: games
                .iter()
                .map(|(name, folder)| GameInstall {
                    name: (*name).to_string(),
                    install_dir: PathBuf::from(r"F:\SteamLibrary\common").join(folder),
                    app_id: None,
                })
                .collect(),
            orphan_evidence: OrphanEvidence::Authoritative,
        }
    }

    #[test]
    fn known_game_executables_matches_a_dump_to_the_game_it_is_named_after() {
        let library = library_of(&[("Portal 2", "Portal 2")]);
        let candidates = HashSet::from(["portal2.exe".to_string()]);

        let index = known_game_executables(&[library], &candidates);

        assert_eq!(index.get("portal2.exe"), Some(&"Portal 2".to_string()));
    }

    #[test]
    fn known_game_executables_matches_on_the_install_folder_when_the_title_differs() {
        // Steam's display name and the folder it installs into are not
        // always the same string, and either is evidence enough.
        let library = library_of(&[("Fallout 76", "Fallout76")]);
        let candidates = HashSet::from(["fallout76.exe".to_string()]);

        let index = known_game_executables(&[library], &candidates);

        assert_eq!(index.get("fallout76.exe"), Some(&"Fallout 76".to_string()));
    }

    #[test]
    fn known_game_executables_leaves_an_executable_that_is_no_game_unresolved() {
        // The ten false positives GT-230 was opened for: a linker, a music
        // tagger, a KDE Connect daemon, Ubisoft Connect.
        let library = library_of(&[("Portal 2", "Portal 2")]);
        let candidates = HashSet::from([
            "link.exe".to_string(),
            "picard.exe".to_string(),
            "kdeconnectd.exe".to_string(),
            "upc.exe".to_string(),
        ]);

        let index = known_game_executables(&[library], &candidates);

        assert!(index.is_empty());
    }
}
