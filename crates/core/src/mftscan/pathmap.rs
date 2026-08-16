//! Pure path-reconstruction and root-filtering logic for MFT scanning. No
//! I/O and no `ntfs`/`windows` types here - this is the part that is
//! meaningfully unit-testable without a real NTFS volume.
//!
//! # Why matching happens in FRN space, not string space
//! A previous implementation reconstructed the full path *string* of every
//! file on the volume and compared it against every root with a
//! per-component case-folded prefix check. That is
//! `O(files x roots x path components)` with allocations inside the inner
//! loop - measured on a real machine it turned a seconds-long `$MFT` read
//! into a 26-second scan on a 24-root volume, and into an hour-plus on a
//! 1000+-root volume. This implementation instead:
//!
//! 1. resolves each root directory to its File Record Number once (walking
//!    *down* a `(parent FRN, folded name) -> FRN` directory index), then
//! 2. classifies every directory FRN by walking *up* the parent chain with
//!    memoization - pure `u64` lookups, no strings - and only
//! 3. builds relative-path strings for files that actually fall under some
//!    root.
//!
//! Total work is `O(records)` with small constants, independent of the
//! number of roots.

use std::collections::HashMap;

use crate::scanner::FileEntry;

use super::model::{FrnMap, ROOT_FRN};

/// One directory to collect files under, expressed as a path relative to
/// the volume root using `\` separators (e.g. `SteamLibrary\HalfLife`), with
/// no drive letter and no leading/trailing separator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRoot {
    pub game_id: i64,
    pub root_rel: String,
}

/// The case fold used for every name comparison - Windows paths are
/// case-insensitive, and this matches the fold the previous string-based
/// implementation applied per component.
fn fold(name: &str) -> String {
    name.to_lowercase()
}

/// Which roots a directory belongs to, and the directory's path relative to
/// each of those roots (empty string for the root directory itself). Almost
/// always 0 or 1 entries; more only when roots are nested or duplicated.
type DirClassification = Vec<(usize, String)>;

/// Ensures `memo` contains the classification for directory `frn`,
/// recursing up the (primary-alias) parent chain. A missing parent record
/// or a parent cycle yields an empty classification - such directories
/// simply cannot be resolved to any root, mirroring how a broken/cyclic
/// chain dropped the file in the string-based implementation.
fn ensure_classified(
    frn: u64,
    map: &FrnMap,
    roots_at_frn: &HashMap<u64, Vec<usize>>,
    memo: &mut HashMap<u64, DirClassification>,
) {
    if memo.contains_key(&frn) {
        return;
    }

    // Cycle guard: if the parent chain leads back to `frn`, the recursive
    // call finds this placeholder and stops instead of looping forever.
    memo.insert(frn, Vec::new());

    let mut entries: DirClassification = Vec::new();

    if frn != ROOT_FRN {
        if let Some(record) = map.get(&frn) {
            if let Some(primary) = record.aliases.first() {
                ensure_classified(primary.parent_frn, map, roots_at_frn, memo);
                if let Some(parent_entries) = memo.get(&primary.parent_frn) {
                    for (root_idx, parent_rel) in parent_entries {
                        let rel = if parent_rel.is_empty() {
                            primary.name.clone()
                        } else {
                            format!("{parent_rel}\\{}", primary.name)
                        };
                        entries.push((*root_idx, rel));
                    }
                }
            }
        }
    }

    // A root anchored exactly at this directory contributes itself with an
    // empty relative prefix - in addition to (not instead of) any enclosing
    // roots inherited from the parent chain above, so nested roots each see
    // the file.
    if let Some(indices) = roots_at_frn.get(&frn) {
        for &root_idx in indices {
            entries.push((root_idx, String::new()));
        }
    }

    memo.insert(frn, entries);
}

/// Reconstructs relative paths for every file record in `map` that falls
/// under one of the given `roots`, producing one `FileEntry` list per game
/// id. Directories are never emitted (only used as ancestors). Games with
/// no matching files get an empty `Vec`, not an error. Several roots may
/// name the same directory (two store entries sharing one install dir) -
/// each gets its own full copy of the files.
pub fn scan_frn_map(map: &FrnMap, roots: &[ScanRoot]) -> Vec<(i64, Vec<FileEntry>)> {
    // 1. Directory child index: (parent FRN, folded name) -> child dir FRN.
    //    Built over directory records only, so its size is the directory
    //    count, not the file count.
    let mut dir_children: HashMap<(u64, String), u64> = HashMap::new();
    for (&frn, record) in map {
        if !record.is_directory {
            continue;
        }
        for alias in &record.aliases {
            dir_children.insert((alias.parent_frn, fold(&alias.name)), frn);
        }
    }

    // 2. Resolve each root's relative path to its directory FRN by walking
    //    down the index component by component. Roots that don't resolve
    //    (deleted or renamed since discovery) simply match nothing, which is
    //    the string implementation's behavior too.
    let mut roots_at_frn: HashMap<u64, Vec<usize>> = HashMap::new();
    for (root_idx, root) in roots.iter().enumerate() {
        let mut frn = ROOT_FRN;
        let mut resolved = true;
        if !root.root_rel.is_empty() {
            for component in root.root_rel.split('\\') {
                match dir_children.get(&(frn, fold(component))) {
                    Some(&child) => frn = child,
                    None => {
                        resolved = false;
                        break;
                    }
                }
            }
        }
        if resolved {
            roots_at_frn.entry(frn).or_default().push(root_idx);
        }
    }

    // 3+4. Classify each file's parent directory (memoized, FRN space) and
    //      emit entries only for files that belong to some root.
    let mut memo: HashMap<u64, DirClassification> = HashMap::new();
    let mut results: HashMap<i64, Vec<FileEntry>> =
        roots.iter().map(|r| (r.game_id, Vec::new())).collect();

    for record in map.values() {
        if record.is_directory {
            continue;
        }

        for alias in &record.aliases {
            ensure_classified(alias.parent_frn, map, &roots_at_frn, &mut memo);
            let Some(parent_entries) = memo.get(&alias.parent_frn) else {
                continue;
            };
            for (root_idx, dir_rel) in parent_entries {
                let rel_path = if dir_rel.is_empty() {
                    alias.name.clone()
                } else {
                    format!("{dir_rel}\\{}", alias.name)
                };
                if let Some(bucket) = results.get_mut(&roots[*root_idx].game_id) {
                    bucket.push(FileEntry {
                        rel_path,
                        size: record.size,
                        size_on_disk: record.alloc_size,
                        mtime: record.mtime,
                    });
                }
            }
        }
    }

    roots
        .iter()
        .map(|r| (r.game_id, results.remove(&r.game_id).unwrap_or_default()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mftscan::model::{MftRecord, NameAlias};

    fn dir(parent: u64, name: &str) -> MftRecord {
        MftRecord {
            is_directory: true,
            size: 0,
            alloc_size: 0,
            mtime: None,
            mtime_nt: None,
            sequence: 1,
            nt_attributes: None,
            aliases: vec![NameAlias {
                parent_frn: parent,
                name: name.to_string(),
            }],
        }
    }

    fn file(parent: u64, name: &str, size: u64, mtime: Option<i64>) -> MftRecord {
        MftRecord {
            is_directory: false,
            size,
            // Synthetic path-mapping tests don't care about on-disk vs
            // logical; keep them equal so `size_on_disk` assertions (where a
            // test makes them) stay predictable.
            alloc_size: size,
            mtime,
            mtime_nt: mtime.map(|secs| secs as u64),
            sequence: 1,
            nt_attributes: None,
            aliases: vec![NameAlias {
                parent_frn: parent,
                name: name.to_string(),
            }],
        }
    }

    /// Builds:
    /// 5 (root)
    /// └─ 100 SteamLibrary (dir)
    ///    ├─ 101 HalfLife (dir)
    ///    │  ├─ 102 hl.exe (file)
    ///    │  └─ 103 data (dir)
    ///    │     └─ 104 save1.sav (file)
    ///    └─ 105 Portal (dir)
    ///       └─ 106 portal.exe (file)
    fn sample_map() -> FrnMap {
        let mut map = FrnMap::new();
        map.insert(100, dir(ROOT_FRN, "SteamLibrary"));
        map.insert(101, dir(100, "HalfLife"));
        map.insert(102, file(101, "hl.exe", 500, Some(1_000)));
        map.insert(103, dir(101, "data"));
        map.insert(104, file(103, "save1.sav", 42, Some(2_000)));
        map.insert(105, dir(100, "Portal"));
        map.insert(106, file(105, "portal.exe", 900, Some(3_000)));
        map
    }

    #[test]
    fn filters_files_under_matching_root_only() {
        let map = sample_map();
        let roots = vec![ScanRoot {
            game_id: 1,
            root_rel: "SteamLibrary\\HalfLife".to_string(),
        }];

        let results = scan_frn_map(&map, &roots);
        assert_eq!(results.len(), 1);
        let (game_id, entries) = &results[0];
        assert_eq!(*game_id, 1);

        let mut rel_paths: Vec<&str> = entries.iter().map(|e| e.rel_path.as_str()).collect();
        rel_paths.sort();
        assert_eq!(rel_paths, vec!["data\\save1.sav", "hl.exe"]);

        // Portal's file must not leak into HalfLife's bucket.
        assert!(!rel_paths.contains(&"portal.exe"));
    }

    #[test]
    fn separate_roots_each_get_only_their_own_files() {
        let map = sample_map();
        let roots = vec![
            ScanRoot {
                game_id: 1,
                root_rel: "SteamLibrary\\HalfLife".to_string(),
            },
            ScanRoot {
                game_id: 2,
                root_rel: "SteamLibrary\\Portal".to_string(),
            },
        ];

        let results = scan_frn_map(&map, &roots);
        let by_id: HashMap<i64, Vec<FileEntry>> = results.into_iter().collect();

        let hl = &by_id[&1];
        let mut hl_paths: Vec<&str> = hl.iter().map(|e| e.rel_path.as_str()).collect();
        hl_paths.sort();
        assert_eq!(hl_paths, vec!["data\\save1.sav", "hl.exe"]);

        let portal = &by_id[&2];
        let portal_paths: Vec<&str> = portal.iter().map(|e| e.rel_path.as_str()).collect();
        assert_eq!(portal_paths, vec!["portal.exe"]);
    }

    /// Two store entries can share one install directory (e.g. the Half-Life
    /// 2 VR episodes all live in `Half-Life 2 VR`) - every game id sharing a
    /// root directory must get its own full copy of the files.
    #[test]
    fn duplicate_roots_on_the_same_dir_each_get_all_files() {
        let map = sample_map();
        let roots = vec![
            ScanRoot {
                game_id: 1,
                root_rel: "SteamLibrary\\HalfLife".to_string(),
            },
            ScanRoot {
                game_id: 2,
                root_rel: "SteamLibrary\\HalfLife".to_string(),
            },
        ];

        let results = scan_frn_map(&map, &roots);
        let by_id: HashMap<i64, Vec<FileEntry>> = results.into_iter().collect();

        assert_eq!(by_id[&1].len(), 2);
        assert_eq!(by_id[&2].len(), 2);
    }

    /// A root nested inside another root: files under the inner root belong
    /// to both games, with paths relative to each root respectively.
    #[test]
    fn nested_roots_both_see_inner_files() {
        let map = sample_map();
        let roots = vec![
            ScanRoot {
                game_id: 1,
                root_rel: "SteamLibrary\\HalfLife".to_string(),
            },
            ScanRoot {
                game_id: 2,
                root_rel: "SteamLibrary\\HalfLife\\data".to_string(),
            },
        ];

        let results = scan_frn_map(&map, &roots);
        let by_id: HashMap<i64, Vec<FileEntry>> = results.into_iter().collect();

        assert!(by_id[&1].iter().any(|e| e.rel_path == "data\\save1.sav"));
        assert!(by_id[&2].iter().any(|e| e.rel_path == "save1.sav"));
    }

    #[test]
    fn root_with_no_files_yields_empty_vec_not_missing_entry() {
        let map = sample_map();
        let roots = vec![ScanRoot {
            game_id: 42,
            root_rel: "SteamLibrary\\DoesNotExist".to_string(),
        }];

        let results = scan_frn_map(&map, &roots);
        assert_eq!(results, vec![(42, Vec::new())]);
    }

    #[test]
    fn root_matching_is_case_insensitive_but_preserves_original_case() {
        let map = sample_map();
        let roots = vec![ScanRoot {
            game_id: 1,
            root_rel: "steamlibrary\\halflife".to_string(),
        }];

        let results = scan_frn_map(&map, &roots);
        let (_, entries) = &results[0];
        let mut rel_paths: Vec<&str> = entries.iter().map(|e| e.rel_path.as_str()).collect();
        rel_paths.sort();
        // Original casing ("hl.exe", not lowercased) must be preserved.
        assert_eq!(rel_paths, vec!["data\\save1.sav", "hl.exe"]);
    }

    #[test]
    fn hard_linked_file_yields_one_path_per_alias() {
        let mut map = sample_map();
        // Give hl.exe (frn 102) a second hard-linked name inside Portal.
        map.get_mut(&102).unwrap().aliases.push(NameAlias {
            parent_frn: 105,
            name: "hl_link.exe".to_string(),
        });

        let roots = vec![
            ScanRoot {
                game_id: 1,
                root_rel: "SteamLibrary\\HalfLife".to_string(),
            },
            ScanRoot {
                game_id: 2,
                root_rel: "SteamLibrary\\Portal".to_string(),
            },
        ];

        let results = scan_frn_map(&map, &roots);
        let by_id: HashMap<i64, Vec<FileEntry>> = results.into_iter().collect();

        assert!(by_id[&1].iter().any(|e| e.rel_path == "hl.exe"));
        assert!(by_id[&2].iter().any(|e| e.rel_path == "hl_link.exe"));
    }

    #[test]
    fn broken_ancestor_chain_is_skipped_not_panicking() {
        let mut map = FrnMap::new();
        // 200's parent (999) does not exist in the map.
        map.insert(200, file(999, "orphan.txt", 10, None));

        let roots = vec![ScanRoot {
            game_id: 1,
            root_rel: String::new(),
        }];

        // Should not panic; the orphan simply cannot be resolved to a path.
        let results = scan_frn_map(&map, &roots);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn cyclic_parent_chain_terminates_without_panicking() {
        let mut map = FrnMap::new();
        // 300 <-> 301 form a cycle, never reaching ROOT_FRN.
        map.insert(300, dir(301, "a"));
        map.insert(301, file(300, "b.txt", 5, None));

        let roots = vec![ScanRoot {
            game_id: 1,
            root_rel: String::new(),
        }];

        let results = scan_frn_map(&map, &roots);
        // No panic, and the cyclic file cannot be resolved so it's dropped.
        assert_eq!(results, vec![(1, Vec::new())]);
    }

    #[test]
    fn empty_root_rel_matches_whole_volume() {
        let map = sample_map();
        let roots = vec![ScanRoot {
            game_id: 1,
            root_rel: String::new(),
        }];

        let results = scan_frn_map(&map, &roots);
        let (_, entries) = &results[0];
        assert_eq!(entries.len(), 3, "all 3 files on the volume should match");
    }
}
