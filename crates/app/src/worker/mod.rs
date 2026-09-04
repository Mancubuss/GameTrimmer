//! Background worker: everything that touches the database or the
//! filesystem runs on a spawned `std::thread`, communicating back to the
//! UI thread through an `mpsc` channel of [`WorkerMsg`].

pub mod bundle;
pub mod clear;
pub mod compact;
pub mod delete;
pub(crate) mod descriptions;
pub mod load;
pub mod manual;
pub mod rules_io;
pub mod scan;
pub(crate) mod scan_route;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::i18n::Verb;
use crate::model::FindingRow;

const DB_FILE_NAME: &str = "gametrimmer.db";
const SETTINGS_FILE_NAME: &str = "gametrimmer.ini";
const LOG_FILE_NAME: &str = "gametrimmer.log";
pub(crate) const RULES_FILE_NAME: &str = "rules.json";
/// Localization-detector data pack (community rules).
pub const L10N_RULES_FILE_NAME: &str = "l10n_rules.json";
/// A per-game catalogue the user installed - see
/// [`gametrimmer_core::reference`].
///
/// Named for what it is rather than for what it holds today, and named the
/// same as the file a catalogue project publishes, so installing one is
/// "copy this file here" with no rename and no doubt about which file is
/// meant. The table is built to gain columns; `intros.json` would have
/// promised it never will.
pub(crate) const GAME_REFERENCE_FILE_NAME: &str = "game_reference.json";
/// This machine's own exceptions - the files its owner has said never to
/// touch, one keep rule each (see
/// [`gametrimmer_core::rules::RulePolarity::Keep`]).
///
/// A third file rather than more rules inside `rules.json`, for two reasons
/// that both come down to ownership. `rules.json` is an optional overlay a
/// user may drop in, replace or delete wholesale - it may not take their own
/// decisions with it. And the split is what keeps a shareable pack free of
/// the exceptions that are not shareable: an exception names one path in one
/// of *their* games and means nothing on anyone else's machine.
///
/// This is also the only one of the three the app still creates: it is the
/// user's own store rather than a copy of the built-in rules, so it cannot
/// go stale against them.
pub(crate) const PERSONAL_RULES_FILE_NAME: &str = "personal_rules.json";

/// Messages sent from a worker thread back to the UI thread.
#[derive(Debug)]
pub enum WorkerMsg {
    /// A plain, already-localized status line for a phase that has no
    /// granular `current/total` progress yet (e.g. the scan's library-
    /// discovery and database-preparation phases, which run before the first
    /// per-game `Progress`). Setting it clears any active progress bar, so
    /// the UI shows the spinner + this text rather than a frozen-looking gap.
    Status { text: String },
    /// Libraries discovered and persisted; scanning of individual games is
    /// about to start.
    LibrariesFound { libraries: usize, games: usize },
    /// Granular progress for a long-running operation (scanning games,
    /// deleting files, compacting the database, ...). `verb` names the
    /// operation, rendered before the `current/total` counter via
    /// `i18n::verb_label` (kept as an enum rather than a pre-localized
    /// `&'static str` so the label always reflects the *current* UI
    /// language); `detail` names the item currently being worked on (a game
    /// name for scanning, a file name for deletion). A scan has one other
    /// producer of `detail`: the volume whose file table is being read
    /// underneath the classification writes there too, on the same verb and
    /// the same game counter, because it is a second thing happening rather
    /// than a second thing to count - see `worker::scan::run_scan`.
    /// Compaction has no
    /// per-item detail - it leaves `detail` empty and reports an estimated
    /// `current`/100 percent instead (see
    /// `gametrimmer_core::db::compact_observed`); `ui::top_bar` renders that
    /// case as `"{verb} {percent}%"`.
    Progress {
        verb: Verb,
        current: usize,
        total: usize,
        detail: String,
    },
    /// The scan finished successfully with the given findings.
    Done {
        findings: Vec<FindingRow>,
        /// Human-readable summary (in the language active when the scan was
        /// started) of how the scan was carried out (MFT index vs. walkdir
        /// counts, elapsed time) - see `worker::scan_route::format_scan_summary`.
        scan_summary: String,
        /// Live disk-usage snapshot (see `gametrimmer_core::db::occupied_by_library`),
        /// aggregated straight from the `files` table rather than carried
        /// over from `findings` - the flagged-only findings list can't
        /// derive total occupied space on its own.
        occupancy: crate::model::Occupancy,
        /// How long the scan's phases took (see `crate::model::ScanTiming`
        /// and `worker::scan::run_scan`). `Some` for a fresh scan; `None`
        /// when these results were loaded from a previous scan instead (see
        /// `worker::load`) - no scan happened, so there is nothing to time.
        timing: Option<crate::model::ScanTiming>,
        /// Why roots that did not go through the MFT index were walked
        /// instead, already localized and ready to show - see
        /// `worker::scan_route::format_walkdir_breakdown`. Empty when every
        /// root took the MFT path, and when these results were loaded from a
        /// previous scan rather than produced by one.
        routing_breakdown: String,
    },
    /// A delete operation finished (possibly with some per-file failures).
    /// `occupancy` is recomputed after the deleted files' rows are purged
    /// from the database, so the UI's occupied-space/percent readout reflects
    /// the just-freed space instead of the pre-delete snapshot.
    ///
    /// `method` is the removal method this specific batch actually ran with
    /// (the per-operation choice from the confirmation modal, not the
    /// persisted default), so the post-delete summary can word itself
    /// honestly - "moved to the Recycle Bin" only when it really was.
    RemoveDone {
        outcomes: Vec<RemoveOutcome>,
        occupancy: crate::model::Occupancy,
        method: gametrimmer_core::settings::DeleteMethod,
    },
    /// One file finished being removed successfully mid-batch, so the UI can
    /// drop it from the tree immediately.
    FileRemoved { file_id: i64 },
    /// The scan was cancelled by the user before completion.
    Cancelled,
    /// Something went wrong; `msg` is an already-localized user-facing
    /// description.
    Error { msg: String },
    /// A non-fatal issue during scanning (one provider failed, or a manual
    /// library's folder is currently missing) - the scan continues.
    Warning { msg: String },
    /// The background "Add Folder..." folder picker finished. `None` means
    /// the user cancelled the dialog.
    FolderPicked { path: Option<PathBuf> },
    /// Notification from the background monitoring daemon that a game state was updated.
    #[allow(dead_code)]
    GameUpdatedIpc {
        app_id: String,
        name: String,
        new_build_id: Option<String>,
        launcher: String,
    },
    /// The background "Export..." export finished. `path` is `None`
    /// when the user cancelled the save dialog (in which case `error` is
    /// also `None`); `error` is set if the save dialog returned a path but
    /// writing the CSV failed.
    ExportDone {
        path: Option<PathBuf>,
        error: Option<String>,
    },
    /// The background "Generate diagnostic bundle" job finished.
    /// path and rror both None means the user closed the save
    /// dialog - the same "nothing happened, nothing went wrong" shape
    /// ExportDone already uses.
    BundleDone {
        path: Option<PathBuf>,
        error: Option<String>,
    },
    /// The background "Compact database" job finished.
    CompactDone {
        error: Option<String>,
        /// The reclaimable share was below the worthwhile threshold, so
        /// `VACUUM` was not run (a cheap WAL checkpoint still happened).
        skipped: bool,
    },
    /// The background "Clear database" job finished (see `worker::clear`).
    /// `error` is `None` on success, in which case the UI resets to the
    /// empty startup state (no findings, nothing scanned yet).
    ClearDone { error: Option<String> },
    /// What happened while the app was closed, for the startup banner
    /// (GT-09) - see `gametrimmer_core::gamestate::returned_since_last_scan`.
    /// Sent once, from `worker::load`, alongside (never blocking) the saved
    /// results in `Done`: this is a handful of small per-launcher reads, not
    /// a rescan, so there is no reason to make the user wait on it.
    ///
    /// `games` empty means the check ran and found nothing changed - the
    /// normal, silent case `ui::top_bar` renders as no banner at all. That is
    /// deliberately different from a check that could not run at all: a
    /// `returned_since_last_scan` error is reported through
    /// `Notifier::report_warning` instead, and *nothing* is sent for it, so a
    /// failed check can never be read as "nothing changed" - only "we don't
    /// know".
    ///
    /// Distinct from `GameUpdatedIpc` above despite both being about a game
    /// changing: that one is a live nudge from the background watch daemon
    /// while the app is already open, keyed by `app_id`, and drives a
    /// per-row tree marker. This one is a one-time startup summary read
    /// straight from the database, keyed by `game_id`, and drives a
    /// dismissible top-bar line. Neither can stand in for the other - one
    /// has no `game_id` to key a summary by, the other has no per-row detail
    /// to mark a tree with - so they stay two fields fed by two messages
    /// rather than being folded into one.
    ReturnedSinceLastScan {
        games: Vec<gametrimmer_core::gamestate::ReturnedGame>,
    },
}

/// Outcome of removing one file, matched back to its `files.id` row.
#[derive(Debug, Clone)]
pub struct RemoveOutcome {
    pub file_id: i64,
    pub path: PathBuf,
    pub error: Option<String>,
    /// True when the row was (or is about to be) purged from the DB even
    /// though the removal attempt failed - the path is already gone from
    /// disk, so the UI must treat it as removed.
    pub purged: bool,
    /// True only for a Recycle Bin removal that reported success yet did not
    /// land in the bin: Windows permanently deletes (never recycles) an item
    /// too large for the target volume's Recycle Bin quota, and `trash::delete`
    /// returns `Ok(())` for it regardless - verified by `gametrimmer_core`'s
    /// `tests/recycle_bin_quota.rs`. (Windows may well warn the user about such
    /// a delete; the app does not depend on that either way.) The UI counts
    /// these as permanent deletions so it never tells the user a gone-for-good
    /// file is "recoverable in the Recycle Bin". Always `false` for a permanent
    /// delete (nothing to reclassify) and when the bin could not be listed
    /// (we never assert a permanent delete we cannot prove).
    pub nuked: bool,
    /// The file's on-disk allocated size (allocated-size accounting), carried through from the
    /// queued [`crate::worker::delete::DeleteItem`] so the summary can sum how
    /// much space was actually freed versus expected.
    pub size_on_disk: u64,
    /// Hard-link identity read from the live file just before removal. A file
    /// named by several links keeps its allocation until the last name goes,
    /// so `size_on_disk` alone would over-report what a delete freed; see
    /// [`gametrimmer_core::hardlink`]. `None` means "assume unshared".
    pub share: Option<gametrimmer_core::hardlink::FileShare>,
}

/// Whatever a completed send has to poke so the results are looked at.
///
/// Deliberately a closure and not the `egui::Context` this used to be. The
/// context brought the whole GUI toolkit into every module that reports
/// progress - including the ones that are pure engine work and belong a
/// crate lower - and it forced two callers with no window at all (the
/// headless CLI, and every test that builds a worker) to conjure an
/// `egui::Context::default()` whose repaint requests went nowhere, purely to
/// satisfy a type.
pub(crate) type Wake = Arc<dyn Fn() + Send + Sync>;

/// A [`Wake`] that does nothing, for a caller with no event loop to wake.
pub(crate) fn no_wake() -> Wake {
    Arc::new(|| {})
}

/// Pairs a [`WorkerMsg`] sender with a wake-up so every background-thread
/// send can also rouse the UI event loop. This is the fix for progress
/// appearing to freeze while the main window is minimized: winit stops
/// calling `eframe::App::ui()` (and so `drain_messages()` never runs to
/// drain the channel) while minimized, but a repaint request forces a frame
/// regardless of window visibility. `Clone` (both fields are `Clone`) so it
/// can be handed to as many worker threads/closures as `Sender<WorkerMsg>`
/// used to be, e.g. once per rayon task in `scan::dispatch_scans`.
#[derive(Clone)]
pub(crate) struct Notifier {
    tx: Sender<WorkerMsg>,
    wake: Wake,
}

impl Notifier {
    pub(crate) fn new(tx: Sender<WorkerMsg>, wake: Wake) -> Self {
        Self { tx, wake }
    }

    /// A notifier whose messages nobody is waiting to be woken for: a test
    /// that reads the channel itself. The headless CLI reaches the same
    /// place through [`no_wake`], which is what the job entrypoints take.
    #[cfg(test)]
    pub(crate) fn silent(tx: Sender<WorkerMsg>) -> Self {
        Self::new(tx, no_wake())
    }

    /// Sends `msg` and immediately requests a repaint. A closed receiver (the
    /// UI already dropped, e.g. during shutdown) is not an error the worker
    /// thread can act on - same as the plain `Sender::send` calls this
    /// replaces, the result is discarded.
    pub(crate) fn send(&self, msg: WorkerMsg) {
        let _ = self.tx.send(msg);
        (self.wake)();
    }

    /// Reports a fatal failure: English to the diagnostic log, the interface
    /// language to the window.
    ///
    /// The logging belongs here and not in the app's `WorkerMsg::Error` arm
    /// because this is the last point where the message still exists in both
    /// languages - past the channel it is one finished string, and which
    /// language that string is in would depend on a setting.
    ///
    /// Every worker reports through these two rather than through
    /// `send(WorkerMsg::Error { .. })` directly. When the logging lived in
    /// the app arm, "every worker" happened for free; now it is a rule, and
    /// a worker that sends the variant by hand writes nothing to the log.
    pub(crate) fn report_error(&self, report: crate::i18n::Reported) {
        crate::logger::error(&report.log);
        self.send(WorkerMsg::Error { msg: report.shown });
    }

    /// Reports a non-fatal diagnostic. Same split as [`Self::report_error`];
    /// the level is the only difference, since a warning is by definition
    /// something the work carried on past.
    pub(crate) fn report_warning(&self, report: crate::i18n::Reported) {
        crate::logger::log(&report.log);
        self.send(WorkerMsg::Warning { msg: report.shown });
    }
}

/// Computes the live disk-usage snapshot (see
/// [`crate::model::Occupancy`]) from the `files` table, or an empty snapshot
/// on any query error. The single place the three producers (scan, load,
/// delete) build their `Done`/`RemoveDone` occupancy, so the non-fatal
/// fallback and the log message stay identical across all of them. An
/// aggregation failure must never hide otherwise-good results, so it degrades
/// to "0 bytes / 0%" rather than propagating.
pub(crate) fn occupancy_or_default(conn: &rusqlite::Connection) -> crate::model::Occupancy {
    match gametrimmer_core::db::occupied_by_library(conn) {
        Ok(by_library) => crate::model::Occupancy::from_by_library(by_library),
        Err(err) => {
            crate::logger::error(&format!(
                "Failed to compute occupied space per library: {err}"
            ));
            crate::model::Occupancy::default()
        }
    }
}

/// The directory every data file lives in: next to the executable.
///
/// `pub(crate)` rather than private: `main::single_instance_guard` (GT-75)
/// needs the same directory the database, settings and log all key off of,
/// resolved the exact same way, so the guard is checking the very directory
/// whose file it is trying to protect rather than a path that could drift
/// from it.
pub(crate) fn exe_dir() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| io::Error::other("failed to resolve the executable's directory"))?;
    Ok(dir.to_path_buf())
}

/// Resolves the database path: `gametrimmer.db` next to the executable.
pub fn db_path() -> io::Result<PathBuf> {
    Ok(exe_dir()?.join(DB_FILE_NAME))
}

/// Resolves the settings path: `gametrimmer.ini` next to the executable.
pub fn settings_path() -> io::Result<PathBuf> {
    Ok(exe_dir()?.join(SETTINGS_FILE_NAME))
}

/// Resolves the diagnostic log path: `gametrimmer.log` next to the
/// executable - see `crate::logger`.
pub fn log_path() -> io::Result<PathBuf> {
    Ok(exe_dir()?.join(LOG_FILE_NAME))
}

/// Where `rules.json` (category rules) would sit if someone put one there.
///
/// The app never writes it. The rules a scan runs on are the ones compiled
/// into this binary, so updating GameTrimmer updates its rules - which is
/// the whole point: a copy written out once on first run stayed frozen at
/// that day's rule set forever, and no later release could reach it.
pub fn rules_path() -> io::Result<PathBuf> {
    Ok(exe_dir()?.join(RULES_FILE_NAME))
}

/// Where `l10n_rules.json` (the localization detector's data pack) would sit
/// - same contract as [`rules_path`].
pub fn l10n_rules_path() -> io::Result<PathBuf> {
    Ok(exe_dir()?.join(L10N_RULES_FILE_NAME))
}

/// The overlay pack of `kind`, if one is actually lying next to the
/// executable.
///
/// This is the whole opt-in mechanism, and it is deliberately just a file
/// test: a pack is in effect because it is there, the way `winapp2.ini` is.
/// `None` means "run on the built-ins alone", which is the normal case and
/// not a failure - an executable directory that cannot even be resolved
/// says the same thing.
/// The per-game catalogue a user installed, if one is actually lying next to
/// the executable.
///
/// Same opt-in as [`overlay_pack_path`] and deliberately not routed through
/// `PackKind`: a catalogue is not a rule pack, does not merge like one, and
/// has no personal or importable variant to distinguish. One file, one
/// question, one answer.
pub fn installed_reference_path() -> Option<PathBuf> {
    let path = exe_dir().ok()?.join(GAME_REFERENCE_FILE_NAME);
    path.is_file().then_some(path)
}

pub fn overlay_pack_path(kind: gametrimmer_core::packs::PackKind) -> Option<PathBuf> {
    let path = match kind {
        gametrimmer_core::packs::PackKind::CategoryRules => rules_path(),
        gametrimmer_core::packs::PackKind::LangPack => l10n_rules_path(),
    }
    .ok()?;
    path.is_file().then_some(path)
}

/// Folds a category-rules overlay into `engine`, if there is one.
///
/// `None` means no overlay is installed, which is the normal case and does
/// nothing. An overlay that does not parse is returned as an error for the
/// caller to report: it must not kill or degrade the scan, but it must be
/// said out loud, because an ignored overlay looks exactly like an overlay
/// that is wrong about the library.
pub fn absorb_rules_overlay(
    engine: &mut gametrimmer_core::rules::RuleEngine,
    overlay: Option<&Path>,
) -> Result<(), gametrimmer_core::error::CoreError> {
    let Some(path) = overlay else {
        return Ok(());
    };
    let loaded = gametrimmer_core::rules::RuleEngine::load(path).map_err(|err| {
        gametrimmer_core::error::CoreError::Other(format!("{}: {err}", path.display()))
    })?;
    engine.absorb(loaded);
    Ok(())
}

/// The compiled language tables a scan runs on: the built-in ones, extended
/// by `overlay` if there is one.
///
/// Returns the tables it could build plus whatever went wrong, rather than a
/// `Result`, because those are not alternatives here: a broken overlay still
/// leaves a usable detector, and the caller has to both keep scanning and
/// report the file.
pub fn lang_data_with_overlay(
    overlay: Option<&Path>,
) -> (
    std::sync::Arc<gametrimmer_core::langdetect::LangData>,
    Option<gametrimmer_core::error::CoreError>,
) {
    use gametrimmer_core::langdetect::{LangData, LangPack};

    let Some(path) = overlay else {
        return (LangData::builtin(), None);
    };
    match std::fs::read_to_string(path)
        .map_err(gametrimmer_core::error::CoreError::from)
        .and_then(|text| LangPack::from_json(&text))
        .map_err(|err| {
            gametrimmer_core::error::CoreError::Other(format!("{}: {err}", path.display()))
        }) {
        Ok(pack) => (
            std::sync::Arc::new(LangData::compile(&LangPack::merge(
                LangPack::builtin(),
                pack,
            ))),
            None,
        ),
        Err(err) => (LangData::builtin(), Some(err)),
    }
}

/// Ensures `dir/file_name` exists, seeding it with `builtin` on first use.
/// An existing file is never touched.
fn ensure_data_file_in(dir: &Path, file_name: &str, builtin: &str) -> io::Result<PathBuf> {
    let path = dir.join(file_name);
    if !path.is_file() {
        std::fs::write(&path, builtin)?;
    }
    Ok(path)
}

/// Ensures `personal_rules.json` exists next to the executable and returns
/// its path, materializing an empty pack on first run.
///
/// Seeded empty rather than left absent so the file is there to be found,
/// audited and hand-edited before the first exception is added - the same
/// transparency contract as the other two packs, and the answer to "where do
/// my exceptions live?" being a path that exists.
pub fn ensure_personal_rules_path() -> io::Result<PathBuf> {
    let empty = gametrimmer_core::rules::serialize_rule_list(&[]).map_err(io::Error::other)?;
    ensure_data_file_in(&exe_dir()?, PERSONAL_RULES_FILE_NAME, &empty)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the overlay: a file next to the executable has to
    /// actually change what a scan decides. Written as a keep rule because a
    /// veto is the one verdict nothing else can produce, so a green result
    /// cannot be a built-in rule happening to agree.
    #[test]
    fn a_rules_overlay_next_to_the_exe_changes_the_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let overlay = dir.path().join(RULES_FILE_NAME);
        let path = r"_CommonRedist\DirectX\dxsetup.exe";

        let mut engine = gametrimmer_core::rules::RuleEngine::from_json(
            gametrimmer_core::rules::BUILTIN_RULES_JSON,
        )
        .expect("the built-in rules compile");
        // The counter-example, and it has to come first: without it a keep
        // that "worked" could just be a path the built-ins never claimed.
        assert!(
            matches!(
                engine.classify(path, Some("620")),
                gametrimmer_core::rules::Verdict::Flagged(_)
            ),
            "the built-in rules must flag this path, or the veto below proves nothing",
        );

        std::fs::write(
            &overlay,
            gametrimmer_core::rules::serialize_rule_list(&[
                gametrimmer_core::rules::Rule::keep_file("620", path, "Overlay probe".into()),
            ])
            .expect("the overlay serializes"),
        )
        .expect("write the overlay");

        absorb_rules_overlay(&mut engine, Some(&overlay)).expect("the overlay loads");

        assert_eq!(
            engine.classify(path, Some("620")),
            gametrimmer_core::rules::Verdict::Kept,
            "the overlay next to the executable was not applied",
        );
    }

    /// A broken overlay is reported, not fatal - and the built-ins stay.
    #[test]
    fn a_broken_overlay_is_reported_and_leaves_the_builtins_standing() {
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join(RULES_FILE_NAME);
        std::fs::write(&rules, "{ not json").unwrap();
        let mut engine = gametrimmer_core::rules::RuleEngine::from_json(
            gametrimmer_core::rules::BUILTIN_RULES_JSON,
        )
        .expect("the built-in rules compile");

        assert!(absorb_rules_overlay(&mut engine, Some(&rules)).is_err());
        assert!(
            matches!(
                engine.classify(r"_CommonRedist\DirectX\dxsetup.exe", Some("620")),
                gametrimmer_core::rules::Verdict::Flagged(_)
            ),
            "a broken overlay took the built-in rules down with it",
        );

        let lang = dir.path().join(L10N_RULES_FILE_NAME);
        std::fs::write(&lang, r#"{"version": 99, "languages": []}"#).unwrap();
        let (data, err) = lang_data_with_overlay(Some(&lang));
        assert!(err.is_some(), "a pack from a newer build was accepted");
        assert_eq!(
            data.language_keys().len(),
            gametrimmer_core::langdetect::LangData::builtin()
                .language_keys()
                .len(),
            "a broken overlay took the built-in language tables down with it",
        );
    }

    /// The language half of the same claim: an overlay teaches the detector a
    /// code the built-in tables do not have, and keeps every one they do.
    #[test]
    fn a_language_overlay_next_to_the_exe_adds_to_the_builtin_tables() {
        let dir = tempfile::tempdir().unwrap();
        let overlay = dir.path().join(L10N_RULES_FILE_NAME);
        let builtin = gametrimmer_core::langdetect::LangData::builtin();
        assert!(
            !builtin.language_keys().contains(&"tlh"),
            "pick a key the built-in tables do not already have",
        );

        std::fs::write(
            &overlay,
            r#"{"version": 1, "languages": [{"key": "tlh", "level_a": ["klingon"]}],
                "industry_words": [], "keep_default": [],
                "markers": {"negative": [], "overridable_negative": [], "audio": [],
                "text": [], "video": [], "font": [], "loc_generic": [], "loc_specific": [],
                "video_extensions": [], "font_extensions": [], "text_extensions": []}}"#,
        )
        .expect("write the overlay");

        let (data, err) = lang_data_with_overlay(Some(&overlay));

        assert!(err.is_none(), "{err:?}");
        assert!(
            data.language_keys().contains(&"tlh"),
            "the overlay next to the executable was not merged in",
        );
        assert_eq!(
            data.language_keys().len(),
            builtin.language_keys().len() + 1,
            "an overlay must extend the built-in tables, not replace them",
        );
    }

    /// The empty pack the file is seeded with has to be a pack the scan can
    /// then load - a first run would otherwise warn about its own default.
    #[test]
    fn the_seeded_personal_pack_is_a_valid_empty_rule_pack() {
        let empty = gametrimmer_core::rules::serialize_rule_list(&[]).expect("serialize");

        let engine = gametrimmer_core::rules::RuleEngine::from_json(&empty)
            .expect("the seeded personal pack must compile");

        assert_eq!(
            engine.classify(r"data\loc_de.pak", Some("620")),
            gametrimmer_core::rules::Verdict::Unmatched,
            "an empty exception pack must veto nothing",
        );
    }

    /// GT-127's regression guard, written because the regression happened.
    ///
    /// Moving the logging out of the app's `WorkerMsg::Error` arm and into
    /// [`Notifier::report_error`] silently un-logged every worker that sent
    /// the variant itself - seventeen sites across delete, load and clear
    /// that had been covered for free while the app arm did the logging.
    /// Nothing failed: the messages still reached the window, and only the
    /// log went quiet, which is the failure mode this whole epic exists to
    /// remove.
    ///
    /// A source check rather than a behavioural one, because the thing to
    /// prevent is a *new call site* taking the shortcut, and no runtime
    /// assertion can see a site nobody ran.
    ///
    /// GT-394 widened it. The guard only ever looked for `WorkerMsg::Error`
    /// and `WorkerMsg::Warning`, so a worker that carried its failure in a
    /// completion variant instead - `CompactDone { error: Some(..) }`,
    /// `ClearDone { error }` - walked straight past it, and both of those
    /// were silent for exactly as long as the guard existed. Any message
    /// carrying an `error` field is now held to the same rule, and because
    /// such a send is spread over several lines the check reads the text
    /// around each `WorkerMsg::` rather than one line at a time.
    #[test]
    fn no_worker_sends_a_report_without_logging_it() {
        /// How much text after a `WorkerMsg::` still counts as part of that
        /// send. Long enough to reach the fields of a multi-line struct
        /// variant, short enough not to spill into the next statement.
        const SEND_WINDOW: usize = 300;
        /// How far above a send to look for its `logger::error`. Covers the
        /// match arm that built the message, not the whole function.
        const LOG_LOOKBACK: usize = 400;
        /// How much of an `app.rs` match arm counts as that arm's handling.
        const HANDLER_WINDOW: usize = 900;

        /// Whether a `logger::error` sits in the text just before `offset`.
        fn logged_before(source: &str, offset: usize, lookback: usize) -> bool {
            let preceding = &source[..offset];
            let start = preceding
                .char_indices()
                .rev()
                .take(lookback)
                .last()
                .map_or(0, |(index, _)| index);
            preceding[start..].contains("logger::error")
        }

        let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let worker_dir = crate_root.join("src/worker");
        let app_source =
            std::fs::read_to_string(crate_root.join("src/app.rs")).expect("read the app source");
        let mut offenders = Vec::new();

        let mut pending = vec![worker_dir.clone()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).expect("read the worker source directory") {
                let path = entry.expect("read a worker source entry").path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().is_some_and(|ext| ext == "rs")
                    // `mod.rs` is where the two legitimate sends live.
                    && path != worker_dir.join("mod.rs")
                {
                    let source = std::fs::read_to_string(&path).expect("read a worker source file");
                    for (number, line) in source.lines().enumerate() {
                        if line.contains("send(WorkerMsg::Error")
                            || line.contains("send(WorkerMsg::Warning")
                        {
                            offenders.push(format!("{}:{}", path.display(), number + 1));
                        }
                    }
                    for (offset, _) in source.match_indices("WorkerMsg::") {
                        let message: String = source[offset..].chars().take(SEND_WINDOW).collect();
                        // `error: Some(..)` and the `{ error }` shorthand are
                        // the two shapes a failure travels in. `error: None`
                        // is a success and is left alone.
                        let carries_failure =
                            message.contains("error: Some(") || message.contains("{ error }");
                        if !carries_failure {
                            continue;
                        }
                        if logged_before(&source, offset, LOG_LOOKBACK) {
                            continue;
                        }
                        // A worker may leave the logging to the handler that
                        // receives the message - `BundleDone` does exactly
                        // that - so a variant logged in its `app.rs` arm is
                        // compliant too. What nothing may do is go unlogged
                        // in both places.
                        let variant: String = message
                            .trim_start_matches("WorkerMsg::")
                            .chars()
                            .take_while(char::is_ascii_alphanumeric)
                            .collect();
                        let handled = app_source
                            .match_indices(&format!("WorkerMsg::{variant}"))
                            .any(|(arm, _)| {
                                app_source[arm..]
                                    .chars()
                                    .take(HANDLER_WINDOW)
                                    .collect::<String>()
                                    .contains("logger::error")
                            });
                        if !handled {
                            offenders.push(format!(
                                "{}:{} ({variant})",
                                path.display(),
                                source[..offset].lines().count()
                            ));
                        }
                    }
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "these sites bypass Notifier::report_error/report_warning and so write \
             nothing to the diagnostic log:\n{}",
            offenders.join("\n"),
        );
    }

    /// `db_path()` and `settings_path()` (and, by the same `exe_dir()`
    /// construction, `ensure_rules_path()`/`ensure_l10n_rules_path()`) must
    /// resolve identically no matter what the
    /// process's current working directory happens to be at launch:
    /// double-clicking the exe from Explorer and running it from `cmd.exe`
    /// in an unrelated folder must give the same data paths, or a portable
    /// install would grow a second database depending on how it was started.
    ///
    /// This changes the process-global CWD, which would be unsafe if any
    /// other code path read it - nothing in the workspace calls
    /// `std::env::current_dir()` outside this test, so no other test can
    /// observe or be perturbed by this change. The original CWD is restored
    /// before returning either way.
    #[test]
    fn portable_data_paths_are_independent_of_current_working_directory() {
        let original_cwd = std::env::current_dir().expect("read original cwd");
        let paths_before = (
            db_path().expect("db_path with original cwd"),
            settings_path().expect("settings_path with original cwd"),
        );

        // `std::env::temp_dir()` is guaranteed to differ from the test
        // binary's own directory (the only thing `exe_dir()` should track).
        let alt_dir = std::env::temp_dir();
        let cwd_change = std::env::set_current_dir(&alt_dir);

        let result = cwd_change.map(|()| (db_path(), settings_path()));

        // Always restore, even if the assertion below panics.
        let restore = std::env::set_current_dir(&original_cwd);

        let (db_after, settings_after) = result.expect("change cwd for test");
        let paths_after = (
            db_after.expect("db_path with alternate cwd"),
            settings_after.expect("settings_path with alternate cwd"),
        );
        restore.expect("restore original cwd");

        assert_eq!(
            paths_before, paths_after,
            "portable data paths must not depend on the process's current working directory"
        );
    }
}
