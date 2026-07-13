//! Pure path-reconstruction and root-filtering logic for MFT scanning. No
//! I/O and no `ntfs`/`windows` types here - this is the part that is
//! meaningfully unit-testable without a real NTFS volume.

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

/// Follows a record's *primary* (first) alias upward to the volume root,
/// returning the ancestor path relative to the volume root (no leading or
/// trailing `\`). Returns `None` if the chain is broken (a referenced parent
/// is missing from `map`) or cyclic.
fn ancestor_path(
    frn: u64,
    map: &FrnMap,
    memo: &mut HashMap<u64, Option<String>>,
) -> Option<String> {
    if frn == ROOT_FRN {
        return Some(String::new());
    }
    if let Some(cached) = memo.get(&frn) {
        return cached.clone();
    }

    // Cycle guard: if we recurse back into `frn` before this call returns,
    // the lookup above will find this placeholder and report "no path"
    // rather than looping forever.
    memo.insert(frn, None);

    let result = map.get(&frn).and_then(|record| {
        let primary = record.aliases.first()?;
        let parent_path = ancestor_path(primary.parent_frn, map, memo)?;
        Some(if parent_path.is_empty() {
            primary.name.clone()
        } else {
            format!("{parent_path}\\{}", primary.name)
        })
    });

    memo.insert(frn, result.clone());
    result
}

/// Reconstructs every full path (relative to the volume root) that `frn` is
/// known by - one per `$FILE_NAME` alias, so a hard-linked file yields one
/// path per link.
fn full_paths_for(frn: u64, map: &FrnMap, memo: &mut HashMap<u64, Option<String>>) -> Vec<String> {
    let Some(record) = map.get(&frn) else {
        return Vec::new();
    };

    record
        .aliases
        .iter()
        .filter_map(|alias| {
            let parent_path = ancestor_path(alias.parent_frn, map, memo)?;
            Some(if parent_path.is_empty() {
                alias.name.clone()
            } else {
                format!("{parent_path}\\{}", alias.name)
            })
        })
        .collect()
}

/// Returns the part of `full_path` below `root_rel`, or `None` if `full_path`
/// is not strictly inside `root_rel`. Comparison is done component-by-component
/// with a Unicode-aware case fold (Windows paths are case-insensitive), and
/// the returned suffix keeps the original casing from `full_path`.
fn strip_root(full_path: &str, root_rel: &str) -> Option<String> {
    if root_rel.is_empty() {
        return Some(full_path.to_string());
    }

    let full_components: Vec<&str> = full_path.split('\\').collect();
    let root_components: Vec<&str> = root_rel.split('\\').collect();

    if full_components.len() <= root_components.len() {
        return None; // full_path must be strictly deeper than root_rel
    }

    let matches = root_components
        .iter()
        .zip(full_components.iter())
        .all(|(r, f)| r.to_lowercase() == f.to_lowercase());

    if !matches {
        return None;
    }

    Some(full_components[root_components.len()..].join("\\"))
}

/// Reconstructs full paths for every file record in `map`, then filters them
/// down to the given `roots`, producing one `FileEntry` list per game id.
/// Directories are never emitted (only used as ancestors). Games with no
/// matching files get an empty `Vec`, not an error.
pub fn scan_frn_map(map: &FrnMap, roots: &[ScanRoot]) -> Vec<(i64, Vec<FileEntry>)> {
    let mut memo: HashMap<u64, Option<String>> = HashMap::new();
    let mut results: HashMap<i64, Vec<FileEntry>> =
        roots.iter().map(|r| (r.game_id, Vec::new())).collect();

    for (&frn, record) in map {
        if record.is_directory {
            continue;
        }

        for full_path in full_paths_for(frn, map, &mut memo) {
            for root in roots {
                if let Some(rel_path) = strip_root(&full_path, &root.root_rel) {
                    if let Some(bucket) = results.get_mut(&root.game_id) {
                        bucket.push(FileEntry {
                            rel_path,
                            size: record.size,
                            mtime: record.mtime,
                        });
                    }
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
            mtime: None,
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
            mtime,
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
    fn reconstructs_nested_full_path() {
        let map = sample_map();
        let mut memo = HashMap::new();
        let paths = full_paths_for(104, &map, &mut memo);
        assert_eq!(paths, vec!["SteamLibrary\\HalfLife\\data\\save1.sav"]);
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
        // Give hl.exe (104's sibling, frn 102) a second hard-linked name inside Portal.
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
