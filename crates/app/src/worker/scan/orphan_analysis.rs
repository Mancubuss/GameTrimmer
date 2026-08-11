//! Authoritative orphan analysis and persistence.

use super::*;

/// One orphaned-residue folder (orphan-residue safety) ready to persist: its absolute path,
/// total size on disk (the sum of the files under it), and why it is residue.
pub(super) struct PreparedOrphan {
    pub(super) full_path: PathBuf,
    pub(super) evidence_library_path: PathBuf,
    /// Logical size (sum of the leftover's files' logical sizes).
    pub(super) size: u64,
    /// On-disk allocated size (allocated-size accounting) - the honest reclaimable figure, shown
    /// and summed as primary.
    pub(super) size_on_disk: u64,
    pub(super) kind: OrphanKind,
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
    pub(super) orphans: Vec<PreparedOrphan>,
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
            library_orphans.push(PreparedOrphan {
                full_path: candidate.path,
                evidence_library_path: library.path.clone(),
                size,
                size_on_disk,
                kind: candidate.kind,
            });
        }
        if !incomplete {
            collection.orphans.extend(library_orphans);
        }
    }
    collection
}

/// Persists the detected orphaned residue and returns it as [`FindingRow`]s for
/// the UI. Orphan rows have no game, so they are stored with a `NULL`
/// `files.game_id` and reconstructed with the synthetic [`ORPHAN_GAME_ID`] here
/// and in `worker::load`.
///
/// The whole set of `NULL`-game rows is replaced each call: `persist_libraries`
/// only ever wipes rows tied to a game, so without this these rows would
/// accumulate across scans (a leftover deleted or a game reinstalled since the
/// last scan would otherwise linger). Passing an empty `orphans` therefore
/// doubles as "clear all orphan residue" - used when the category is disabled.
/// One transaction so a mid-write failure can't leave a half-replaced set.
pub(super) fn persist_orphans(
    conn: &mut Connection,
    orphans: &[PreparedOrphan],
    lang: Lang,
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

    let mut rows = Vec::with_capacity(orphans.len());
    {
        let mut insert_file = tx.prepare_cached(
            "INSERT INTO files (scan_id, game_id, rel_path, size, size_on_disk, mtime) \
             VALUES (?1, NULL, ?2, ?3, ?4, NULL)",
        )?;
        let mut insert_finding = tx.prepare_cached(
            "INSERT INTO findings
             (file_id, category, rule_id, confidence, lang_tag, group_dir, provenance) \
             VALUES (?1, ?2, ?3, ?4, NULL, NULL, 'builtin')",
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

        for orphan in orphans {
            let source = FindingSource::Orphan(orphan.kind);
            let confidence = orphan_confidence(orphan.kind);
            let reason = i18n::orphan_reason(lang, orphan.kind);

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
            insert_finding.execute(params![file_id, source_key(source), &reason, confidence])?;

            let (install_dir, rel_path) = orphan_install_dir_and_name(&orphan.full_path);
            let deletion_block_reason = match capture.capture(&install_dir, &rel_path) {
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
                game_id: ORPHAN_GAME_ID,
                game_name: String::new(),
                install_dir,
                rel_path,
                size: orphan.size,
                size_on_disk: orphan.size_on_disk,
                source,
                rule_desc: reason,
                confidence,
                lang_tag: None,
                group_dir: None,
                deletion_block_reason,
                imported_untrusted: false,
            });
        }
    }

    tx.commit()?;
    Ok(rows)
}
