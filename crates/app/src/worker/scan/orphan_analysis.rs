//! Authoritative orphan analysis and persistence.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{params, Connection, OptionalExtension};

use gametrimmer_core::error::Result as CoreResult;
use gametrimmer_core::ondisk;
use gametrimmer_core::orphans::{self, OrphanKind};
use gametrimmer_core::providers::steam;
use gametrimmer_core::providers::{DiscoveredLibrary, OrphanEvidence};
use gametrimmer_core::scanner::scan_dir_cancellable;

use crate::i18n::{self, Lang};
use crate::model::{
    orphan_confidence, rootless_branch_id, rootless_split, source_key, FindingRow, FindingSource,
    LibraryOrigin,
};

/// One finding that belongs to no game row (orphan-residue safety, and the
/// janitor areas that live outside every install directory): its absolute
/// path, its measured size, and the classification to persist.
///
/// Orphaned residue and a janitor artifact differ in where they were found and
/// in nothing that the writer below does with them - both are stored with a
/// `NULL` `files.game_id`, a full path in `files.rel_path`, and a safety
/// snapshot captured from the parent directory. Keeping one struct is what
/// keeps that agreement from drifting into two half-equal ones.
pub(super) struct PreparedRootless {
    pub(super) full_path: PathBuf,
    /// The area whose enumeration is this finding's discovery evidence: the
    /// library root for residue, the scanned janitor directory otherwise. The
    /// delete preflight resolves it back to a `scan_library_evidence` row, so
    /// it is never blank.
    pub(super) evidence_library_path: PathBuf,
    /// Logical size (sum of the leftover's files' logical sizes).
    pub(super) size: u64,
    /// On-disk allocated size (allocated-size accounting) - the honest reclaimable figure, shown
    /// and summed as primary.
    pub(super) size_on_disk: u64,
    pub(super) source: FindingSource,
    /// English description, as stored in `findings.rule_id`; the window
    /// re-derives an orphan's sentence from its kind (see
    /// `worker::descriptions`) and shows a janitor artifact's text as written.
    pub(super) reason: String,
    pub(super) confidence: u8,
    /// The folder this row is grouped under in the tree, relative to whatever
    /// directory the finder walked (`Fumi Games\MOUSE`). `None` leaves the row
    /// loose, which is what launcher residue and one-bucket janitor areas want.
    pub(super) group_dir: Option<String>,
}

impl PreparedRootless {
    /// Orphaned launcher residue: confidence and reason both follow from the
    /// kind, so no caller gets to invent either.
    pub(super) fn orphan(
        full_path: PathBuf,
        evidence_library_path: PathBuf,
        size: u64,
        size_on_disk: u64,
        kind: OrphanKind,
    ) -> Self {
        Self {
            full_path,
            evidence_library_path,
            size,
            size_on_disk,
            source: FindingSource::Orphan(kind),
            reason: i18n::orphan_reason(Lang::En, kind).to_string(),
            confidence: orphan_confidence(kind),
            group_dir: None,
        }
    }
}

pub(super) struct OrphanCollectionIssue {
    pub(super) provider: &'static str,
    pub(super) library_path: PathBuf,
    pub(super) stage: &'static str,
    pub(super) path: PathBuf,
    pub(super) message: String,
}

#[derive(Default)]
pub(super) struct OrphanCollection {
    pub(super) orphans: Vec<PreparedRootless>,
    pub(super) issues: Vec<OrphanCollectionIssue>,
}

/// Maps one discovered library to its orphan-scan spec, or `None` when the
/// vendor has no container we can diff *safely*.
///
/// Only vendors with a launcher-owned container qualify. Two shapes:
///   - **Structurally exclusive** containers - a fixed folder the launcher
///     creates and fills alone: Steam's `steamapps/common` (derived from the
///     library root) and Xbox's `XboxGames` (which *is* the discovered
///     `library.path`, since `group_by_parent_dir` groups Game Pass titles
///     under their shared `XboxGames` parent).
///   - **Shared-root** containers made safe by an ownership marker: itch, whose
///     install location is user-chosen and may be shared, but whose leftovers
///     still carry a `.itch` receipt (see `orphans::itch_spec`). Here too
///     `library.path` *is* the container (the itch location = the parent of
///     each cave's install dir).
///
/// The registry-based providers (Epic, GOG, EA, Ubisoft, Battle.net, Rockstar,
/// Riot) are deliberately absent: their `library.path` is merely the parent of
/// wherever the user chose to install, not a launcher-exclusive folder, so a
/// container diff there could flag the user's own unrelated folders - the exact
/// false positive that fail-closed orphan detection forbids. Humble is likewise deferred: its download
/// location is user-chosen and it leaves no per-game ownership marker to prove a
/// folder is its residue rather than a foreign game. Tracked on the Kanban
/// separately because no authoritative install-root contract exists for them yet.
fn orphan_spec_for(library: &DiscoveredLibrary) -> Option<orphans::OrphanScanSpec> {
    if library.orphan_evidence != OrphanEvidence::Authoritative {
        return None;
    }
    // `vendor` is a `&'static str` on the provider, so these are plain string
    // compares, not heap allocations.
    match library.vendor {
        "steam" => Some(orphans::steam_spec(&library.path)),
        "xbox" => Some(orphans::xbox_spec(&library.path)),
        "itch" => Some(orphans::itch_spec(&library.path)),
        _ => None,
    }
}

/// Detects orphaned launcher residue across every discovered *supported* library
/// (see [`orphan_spec_for`]) and measures each leftover's size.
///
/// The heavy lifting is the pure diff in `gametrimmer_core::orphans` - for each
/// library, the managed install set is exactly that library's already-discovered
/// games' `install_dir`s, so anything sitting in the vendor's container that no
/// manifest points at (and, for shared-root vendors, that still carries the
/// vendor's ownership marker) is a candidate. A game installed *past* the
/// launcher is never even looked at, since only subfolders of the vendor's own
/// container are considered.
///
/// Each candidate's size is the sum of the files under it. An enumeration or
/// measurement error discards every orphan candidate from that library and is
/// returned as a scoped issue; incomplete evidence must never create a finding.
pub(super) fn collect_orphans(
    libraries: &[DiscoveredLibrary],
    cancel: &AtomicBool,
) -> OrphanCollection {
    let mut collection = OrphanCollection::default();
    for library in libraries {
        // Only vendors with a launcher-owned container we can diff safely are
        // handled; the rest (registry-based providers whose install roots are
        // arbitrary user folders) have no exclusive container and are skipped
        // by construction - see `orphan_spec_for`.
        let Some(spec) = orphan_spec_for(library) else {
            continue;
        };
        let managed =
            orphans::managed_dir_set(library.games.iter().map(|game| game.install_dir.as_path()));
        let candidates = match orphans::find_orphans(&spec, &managed) {
            Ok(candidates) => candidates,
            Err(err) => {
                collection.issues.push(OrphanCollectionIssue {
                    provider: library.vendor,
                    library_path: library.path.clone(),
                    stage: "orphan-enumeration",
                    path: spec.container,
                    message: err.to_string(),
                });
                continue;
            }
        };
        let mut library_orphans = Vec::new();
        let mut incomplete = false;
        for candidate in candidates {
            if cancel.load(Ordering::Relaxed) {
                return collection;
            }
            let (size, size_on_disk) = match scan_dir_cancellable(&candidate.path, cancel) {
                Ok(entries) => entries.iter().fold((0u64, 0u64), |(sz, on_disk), entry| {
                    (sz + entry.size, on_disk + entry.size_on_disk)
                }),
                Err(err) => {
                    if !cancel.load(Ordering::Relaxed) {
                        collection.issues.push(OrphanCollectionIssue {
                            provider: library.vendor,
                            library_path: library.path.clone(),
                            stage: "orphan-measurement",
                            path: candidate.path,
                            message: err.to_string(),
                        });
                    }
                    incomplete = true;
                    break;
                }
            };
            library_orphans.push(PreparedRootless::orphan(
                candidate.path,
                library.path.clone(),
                size,
                size_on_disk,
                candidate.kind,
            ));
        }
        if !incomplete {
            collection.orphans.extend(library_orphans);
        }
    }
    collect_steam_service_area_orphans(libraries, cancel, &mut collection);
    collection
}

/// GT-23: orphan detection for Steam's two remaining service areas -
/// `steamapps/workshop` (Workshop content of an uninstalled game) and
/// `depotcache` (cached depot manifests) - each with its own per-item
/// "is anything still referencing this?" check, since a blind sweep of
/// either risks deleting live data (a still-subscribed Workshop item, or a
/// manifest an installed game still needs).
///
/// Not folded into [`orphan_spec_for`]/the loop above: both areas need
/// evidence a single `DiscoveredLibrary`'s already-known `games` can't
/// supply - Workshop needs a *separate* small state file per appid
/// (`appworkshop_<appid>.acf`), and depotcache needs evidence from *every*
/// Steam library, not just its own (see `steam::installed_depots_for_library`
/// for the real-machine evidence that forced that). So this runs once, after
/// the generic per-library loop, over every already-discovered library.
fn collect_steam_service_area_orphans(
    libraries: &[DiscoveredLibrary],
    cancel: &AtomicBool,
    collection: &mut OrphanCollection,
) {
    // Only libraries whose manifest inventory is provably complete get this
    // treatment - same gate `orphan_spec_for` applies to the primary pass.
    // A degraded library's `games` list may be missing entries, which would
    // make an installed game's Workshop subscription or depot dependency
    // look orphaned.
    let steam_libraries: Vec<&DiscoveredLibrary> = libraries
        .iter()
        .filter(|library| {
            library.vendor == "steam" && library.orphan_evidence == OrphanEvidence::Authoritative
        })
        .collect();

    collect_workshop_orphans(&steam_libraries, cancel, collection);
    collect_depotcache_orphans(&steam_libraries, cancel, collection);
}

/// Reads and parses one appid's `appworkshop_<appid>.acf`, returning the set
/// of published-file ids Steam currently lists as installed - or `None` when
/// the file is missing, unreadable, or malformed. Both collapse to the same
/// `None` deliberately: GT-23's rule is that missing and unreadable evidence
/// both degrade to "do not flag", so the caller does not need (and must not
/// be tempted to add) different handling for the two.
fn read_workshop_live_items(state_path: &Path) -> Option<HashSet<String>> {
    let contents = std::fs::read_to_string(state_path).ok()?;
    steam::parse_workshop_installed_items(&contents)
}

/// Detects orphaned Workshop item folders across every evidence-authoritative
/// Steam library: for each appid under `steamapps/workshop/content/`, reads
/// that appid's own `appworkshop_<appid>.acf` and flags exactly the item
/// folders it does not list as installed (see
/// `orphans::workshop_item_orphans`).
///
/// An appid whose state file is missing, unreadable, or malformed is skipped
/// entirely (fail closed) - no items under it are flagged - without recording
/// an issue: this is a supplementary, already-low-confidence detection pass,
/// not the primary discovery evidence that `orphan_evidence` guards, so a gap
/// here is not surfaced as a scan-wide diagnostic.
fn collect_workshop_orphans(
    steam_libraries: &[&DiscoveredLibrary],
    cancel: &AtomicBool,
    collection: &mut OrphanCollection,
) {
    for library in steam_libraries {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let content_dir = orphans::steam_workshop_content_dir(&library.path);
        let appid_dirs = match orphans::list_subdirs(&content_dir) {
            Ok(dirs) => dirs,
            Err(err) => {
                collection.issues.push(OrphanCollectionIssue {
                    provider: "steam",
                    library_path: library.path.clone(),
                    stage: "workshop-enumeration",
                    path: content_dir,
                    message: err.to_string(),
                });
                continue;
            }
        };

        for appid_dir in appid_dirs {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let Some(appid) = appid_dir.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(live_items) =
                read_workshop_live_items(&orphans::steam_workshop_state_path(&library.path, appid))
            else {
                continue;
            };

            let candidates = match orphans::workshop_item_orphans(&appid_dir, &live_items) {
                Ok(candidates) => candidates,
                Err(err) => {
                    collection.issues.push(OrphanCollectionIssue {
                        provider: "steam",
                        library_path: library.path.clone(),
                        stage: "workshop-item-enumeration",
                        path: appid_dir,
                        message: err.to_string(),
                    });
                    continue;
                }
            };

            push_measured_orphans(collection, "steam", &library.path, candidates, cancel);
        }
    }
}

/// Detects orphaned `depotcache/*.manifest` files across every
/// evidence-authoritative Steam library.
///
/// The proof-of-need set has to be global (see
/// `steam::installed_depots_for_library`'s doc comment for the real-machine
/// evidence): built once, from every Steam library's installed depots, before
/// any single library's cache files are checked. If reading any library's
/// installed depots fails, the whole set is untrustworthy - an undercount
/// there could flag a manifest some other library's game still needs - so
/// depotcache detection is skipped for the entire scan rather than proceeding
/// with partial evidence.
fn collect_depotcache_orphans(
    steam_libraries: &[&DiscoveredLibrary],
    cancel: &AtomicBool,
    collection: &mut OrphanCollection,
) {
    if steam_libraries.is_empty() {
        return;
    }

    let mut needed_files: HashSet<String> = HashSet::new();
    for library in steam_libraries {
        match steam::installed_depots_for_library(&library.path) {
            Ok(pairs) => needed_files.extend(pairs.into_iter().map(|(depot_id, manifest_id)| {
                format!("{depot_id}_{manifest_id}.manifest").to_lowercase()
            })),
            Err(err) => {
                collection.issues.push(OrphanCollectionIssue {
                    provider: "steam",
                    library_path: library.path.clone(),
                    stage: "depotcache-evidence",
                    path: library.path.join("steamapps"),
                    message: err.to_string(),
                });
                return;
            }
        }
    }

    for library in steam_libraries {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let depotcache_dir = orphans::steam_depotcache_dir(&library.path);
        let candidates = match orphans::depotcache_orphans(&depotcache_dir, &needed_files) {
            Ok(candidates) => candidates,
            Err(err) => {
                collection.issues.push(OrphanCollectionIssue {
                    provider: "steam",
                    library_path: library.path.clone(),
                    stage: "depotcache-enumeration",
                    path: depotcache_dir,
                    message: err.to_string(),
                });
                continue;
            }
        };
        push_measured_orphans(collection, "steam", &library.path, candidates, cancel);
    }
}

/// Measures every candidate and appends it to `collection` as a
/// [`PreparedOrphan`]. A directory candidate (Workshop item folders) is
/// walked and summed like the primary per-library loop above;
/// [`OrphanKind::UnreferencedFile`] candidates (single depot-cache manifests)
/// are measured directly, since `scan_dir_cancellable` requires a directory
/// and would simply error on a file.
///
/// Unlike the primary loop, one candidate's measurement failure here only
/// discards that candidate, not the whole library's batch: these are already
/// a best-effort supplementary pass over a low-confidence category, so there
/// is no reason to hide otherwise-good results because one item vanished
/// mid-scan.
fn push_measured_orphans(
    collection: &mut OrphanCollection,
    provider: &'static str,
    evidence_library_path: &Path,
    candidates: Vec<orphans::OrphanCandidate>,
    cancel: &AtomicBool,
) {
    for candidate in candidates {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let measured = if candidate.kind == OrphanKind::UnreferencedFile {
            measure_single_file(&candidate.path)
        } else {
            scan_dir_cancellable(&candidate.path, cancel)
                .map(|entries| {
                    entries.iter().fold((0u64, 0u64), |(sz, on_disk), entry| {
                        (sz + entry.size, on_disk + entry.size_on_disk)
                    })
                })
                .map_err(|err| err.to_string())
        };
        match measured {
            Ok((size, size_on_disk)) => collection.orphans.push(PreparedRootless::orphan(
                candidate.path,
                evidence_library_path.to_path_buf(),
                size,
                size_on_disk,
                candidate.kind,
            )),
            Err(message) => {
                if !cancel.load(Ordering::Relaxed) {
                    collection.issues.push(OrphanCollectionIssue {
                        provider,
                        library_path: evidence_library_path.to_path_buf(),
                        stage: "orphan-measurement",
                        path: candidate.path,
                        message,
                    });
                }
            }
        }
    }
}

/// Logical + on-disk size of a single file - the [`scan_dir_cancellable`]
/// equivalent for a candidate that is a file rather than a directory.
pub(super) fn measure_single_file(path: &Path) -> Result<(u64, u64), String> {
    let metadata = std::fs::metadata(path).map_err(|err| err.to_string())?;
    let logical = metadata.len();
    let cluster = ondisk::cluster_size(path);
    let size_on_disk = ondisk::on_disk_size(path, logical, cluster);
    Ok((logical, size_on_disk))
}

/// Persists every finding that has no game behind it - orphaned residue and
/// janitor artifacts alike - and returns them as [`FindingRow`]s for the UI.
/// Such rows are stored with a `NULL` `files.game_id` and reconstructed with
/// the synthetic [`ORPHAN_GAME_ID`] here and in `worker::load`.
///
/// The whole set of `NULL`-game rows is replaced each call: `persist_libraries`
/// only ever wipes rows tied to a game, so without this these rows would
/// accumulate across scans (a leftover deleted or a game reinstalled since the
/// last scan would otherwise linger). Passing an empty slice therefore doubles
/// as "clear all rootless findings" - used when every such category is
/// disabled. One transaction so a mid-write failure can't leave a
/// half-replaced set.
///
/// Both kinds go through this one call for that same reason: two writers, each
/// clearing `NULL`-game rows, would delete each other's work.
pub(super) fn persist_rootless(
    conn: &mut Connection,
    findings: &[PreparedRootless],
    scan_id: i64,
) -> CoreResult<Vec<FindingRow>> {
    let tx = conn.transaction()?;

    // `findings` first - it references `files.id`.
    tx.execute(
        "DELETE FROM file_safety WHERE file_id IN
         (SELECT id FROM files WHERE game_id IS NULL AND scan_id = ?1)",
        [scan_id],
    )?;
    tx.execute(
        "DELETE FROM findings WHERE file_id IN
         (SELECT id FROM files WHERE game_id IS NULL AND scan_id = ?1)",
        [scan_id],
    )?;
    tx.execute(
        "DELETE FROM files WHERE game_id IS NULL AND scan_id = ?1",
        [scan_id],
    )?;

    let mut rows = Vec::with_capacity(findings.len());
    {
        let mut insert_file = tx.prepare_cached(
            "INSERT INTO files (scan_id, game_id, rel_path, size, size_on_disk, mtime) \
             VALUES (?1, NULL, ?2, ?3, ?4, NULL)",
        )?;
        let mut insert_finding = tx.prepare_cached(
            "INSERT INTO findings
             (file_id, category, rule_id, confidence, lang_tag, group_dir, provenance) \
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, 'builtin')",
        )?;
        let mut insert_safety = tx.prepare_cached(
            "INSERT OR REPLACE INTO file_safety
             (file_id, scan_id, evidence_library_path, trusted_root, rel_path, root_identity,
              target_identity, target_kind, tree_fingerprint, block_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;

        // Orphans in the same library share their parent directories, so the
        // chain above each one is worth proving once rather than per orphan.
        let mut capture = gametrimmer_core::safety::SnapshotCapture::new();
        // Library attribution, resolved the way `worker::load` resolves it for
        // an orphan row: by the recorded library root, not by the in-memory
        // `DiscoveredLibrary` the scan happened to hold. Taking the same route
        // is what keeps a fresh scan and a later load from disagreeing. Cached
        // per root, since every orphan of one library shares it.
        let mut vendor_by_root: HashMap<PathBuf, Option<String>> = HashMap::new();

        for orphan in findings {
            let source = orphan.source;
            let confidence = orphan.confidence;
            // Stored in English like every other description in the database;
            // `worker::descriptions` rebuilds the localized sentence from an
            // orphan's kind when the row is drawn, and shows a janitor
            // artifact's own text unchanged.
            let reason = orphan.reason.clone();

            // Stored full-path-in-`rel_path`: an orphan has no game row to hold
            // its `install_dir`, so the row must be self-contained. `load`
            // splits it back into the same `(install_dir, rel_path)` pair.
            let full_path_str = orphan.full_path.to_string_lossy().to_string();
            insert_file.execute(params![
                scan_id,
                full_path_str,
                orphan.size as i64,
                orphan.size_on_disk as i64
            ])?;
            let file_id = tx.last_insert_rowid();
            // The split has to happen before the insert: a group the path does
            // not actually contain is dropped, and the column must record what
            // the row was really built with, not what was asked for.
            let (install_dir, rel_path, group_dir) =
                rootless_split(&orphan.full_path, orphan.group_dir.as_deref());
            insert_finding.execute(params![
                file_id,
                source_key(source),
                &reason,
                confidence,
                group_dir.as_deref()
            ])?;

            let vendor = match vendor_by_root.get(&orphan.evidence_library_path) {
                Some(vendor) => vendor.clone(),
                None => {
                    let vendor = tx
                        .query_row(
                            "SELECT vendor FROM game_libraries WHERE path = ?1",
                            params![orphan.evidence_library_path.to_string_lossy()],
                            |row| row.get::<_, String>(0),
                        )
                        .optional()?;
                    vendor_by_root.insert(orphan.evidence_library_path.clone(), vendor.clone());
                    vendor
                }
            };

            // An orphan is a leftover *directory*, found by comparing launcher
            // manifests against what is on disk rather than by walking the
            // `$MFT`, so there is no record here to quote and the capture
            // opens it live.
            let deletion_block_reason = match capture.capture(&install_dir, &rel_path, None) {
                Ok(snapshot) => {
                    insert_safety.execute(params![
                        file_id,
                        scan_id,
                        orphan.evidence_library_path.to_string_lossy(),
                        snapshot.trusted_root.to_string_lossy(),
                        snapshot.rel_path.to_string_lossy(),
                        snapshot.root_identity.encode(),
                        snapshot.target_identity.encode(),
                        snapshot.target_identity.kind.as_str(),
                        snapshot.tree_fingerprint,
                        None::<String>,
                    ])?;
                    None
                }
                Err(block) => {
                    let block = block.to_string();
                    insert_safety.execute(params![
                        file_id,
                        scan_id,
                        orphan.evidence_library_path.to_string_lossy(),
                        install_dir.to_string_lossy(),
                        rel_path,
                        None::<String>,
                        None::<String>,
                        None::<String>,
                        None::<String>,
                        &block,
                    ])?;
                    Some(block)
                }
            };
            rows.push(FindingRow {
                file_id,
                game_id: rootless_branch_id(source),
                game_name: String::new(),
                app_id: None,
                install_dir,
                rel_path,
                size: orphan.size,
                size_on_disk: orphan.size_on_disk,
                source,
                rule_desc: reason,
                confidence,
                lang_tag: None,
                group_dir,
                deletion_block_reason,
                imported_untrusted: false,
                library: Some(LibraryOrigin {
                    vendor,
                    root: orphan.evidence_library_path.clone(),
                }),
                anti_cheat_protected: false,
            });
        }
    }

    tx.commit()?;
    Ok(rows)
}
