//! Per-game state between scans (game-state tracking): telling what changed since the last
//! scan *without* rescanning.
//!
//! The expensive part of a scan is enumerating files; detecting that a game
//! changed is not. Steam bumps a game's `buildid` on every content update (and
//! a `Verify` that re-downloads files), so comparing the `buildid` recorded at
//! the last scan against the one in the manifest right now answers "did this
//! game come back?" for the price of reading a few dozen small text files - see
//! `providers::steam::manifest_states`.
//!
//! This module holds the *pure* half of that comparison, so the policy is
//! unit-tested without a database, a Steam install, or a scan.

use std::collections::HashMap;

/// What the last scan recorded about one game, read back from `games`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredGameState {
    pub game_id: i64,
    pub name: String,
    /// Vendor id (`games.app_id`). `None` for games discovered by folder scan,
    /// which therefore can never be matched to a manifest - see
    /// [`changed_games`].
    pub app_id: Option<String>,
    /// The `buildid` recorded at the last scan, or `None` for a game scanned
    /// before this tracking existed (or by a provider that has no build id).
    pub build_id: Option<String>,
}

/// Why a game is reported as changed since the last scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// The build id moved: the launcher updated (or re-verified) the game, so
    /// files removed by a previous trim are likely back.
    Updated,
    /// The game is gone from its launcher's manifests entirely - uninstalled
    /// since the last scan, so its recorded findings are stale.
    Uninstalled,
}

/// One game whose on-disk state no longer matches what the last scan recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedGame {
    pub game_id: i64,
    pub name: String,
    pub kind: ChangeKind,
    /// The build id the last scan recorded (`None` if it recorded none).
    pub previous_build_id: Option<String>,
    /// The build id in the manifest right now; `None` for
    /// [`ChangeKind::Uninstalled`].
    pub current_build_id: Option<String>,
}

/// Compares what the last scan recorded against the launcher's manifests right
/// now, returning only the games that actually changed.
///
/// `current` maps appid -> current `buildid` (see
/// `providers::steam::manifest_states`). Deliberately conservative - a game is
/// reported **only** when the evidence is unambiguous:
///
/// - **Updated**: both build ids are known and differ. A game whose stored
///   build id is `None` (scanned before this tracking existed, or by a provider
///   with no build id) is never reported as updated: "we don't know" must not
///   render as "it changed", or the very first run after upgrading would flag
///   the entire library.
/// - **Uninstalled**: the game had an `app_id` *and* a recorded build id - i.e.
///   it demonstrably came from a manifest - yet no manifest carries that appid
///   now. Requiring the recorded build id is what keeps a game whose manifests
///   simply were not readable this time (Steam not installed, drive offline,
///   `current` empty) from being declared uninstalled en masse; such a game has
///   no stored build id only in the legacy case, which is excluded anyway.
///
/// Games with no `app_id` at all (folder-scan discoveries) are never reported:
/// there is no key to match them against a manifest.
pub fn changed_games(
    stored: &[StoredGameState],
    current: &HashMap<String, String>,
) -> Vec<ChangedGame> {
    stored
        .iter()
        .filter_map(|game| {
            let app_id = game.app_id.as_deref()?;
            // Never claim a change we cannot evidence (see doc comment).
            let previous = game.build_id.as_deref()?;

            match current.get(app_id) {
                Some(now) if now != previous => Some(ChangedGame {
                    game_id: game.game_id,
                    name: game.name.clone(),
                    kind: ChangeKind::Updated,
                    previous_build_id: Some(previous.to_string()),
                    current_build_id: Some(now.clone()),
                }),
                Some(_) => None,
                None => Some(ChangedGame {
                    game_id: game.game_id,
                    name: game.name.clone(),
                    kind: ChangeKind::Uninstalled,
                    previous_build_id: Some(previous.to_string()),
                    current_build_id: None,
                }),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(
        game_id: i64,
        name: &str,
        app_id: Option<&str>,
        build: Option<&str>,
    ) -> StoredGameState {
        StoredGameState {
            game_id,
            name: name.to_string(),
            app_id: app_id.map(str::to_string),
            build_id: build.map(str::to_string),
        }
    }

    fn current(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn reports_a_game_whose_build_id_moved() {
        let stored_games = [stored(1, "Portal 2", Some("620"), Some("100"))];
        let changed = changed_games(&stored_games, &current(&[("620", "201")]));

        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].game_id, 1);
        assert_eq!(changed[0].kind, ChangeKind::Updated);
        assert_eq!(changed[0].previous_build_id.as_deref(), Some("100"));
        assert_eq!(changed[0].current_build_id.as_deref(), Some("201"));
    }

    #[test]
    fn ignores_an_unchanged_game() {
        let stored_games = [stored(1, "Portal 2", Some("620"), Some("100"))];
        assert!(changed_games(&stored_games, &current(&[("620", "100")])).is_empty());
    }

    #[test]
    fn never_reports_a_game_with_no_recorded_build_id() {
        // The upgrade case: rows written before build-id tracking existed. Not
        // knowing the old build must never render as "everything changed".
        let stored_games = [stored(1, "Portal 2", Some("620"), None)];
        assert!(
            changed_games(&stored_games, &current(&[("620", "999")])).is_empty(),
            "an unknown previous build id is not evidence of a change"
        );
    }

    #[test]
    fn never_reports_a_game_without_an_app_id() {
        // Folder-scan discoveries have no manifest key to match against.
        let stored_games = [stored(1, "Repack Game", None, Some("100"))];
        assert!(changed_games(&stored_games, &current(&[("620", "100")])).is_empty());
    }

    #[test]
    fn reports_a_game_missing_from_the_manifests_as_uninstalled() {
        let stored_games = [stored(7, "Deleted Game", Some("440"), Some("55"))];
        let changed = changed_games(&stored_games, &current(&[("620", "100")]));

        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].kind, ChangeKind::Uninstalled);
        assert_eq!(changed[0].current_build_id, None);
    }

    #[test]
    fn legacy_rows_stay_silent_even_when_manifests_are_unreadable() {
        // Steam not installed / drive offline: `current` is empty. Rows without
        // a recorded build id must not turn into a wall of "uninstalled".
        let stored_games = [
            stored(1, "Legacy A", Some("620"), None),
            stored(2, "Legacy B", Some("440"), None),
        ];
        assert!(changed_games(&stored_games, &HashMap::new()).is_empty());
    }

    #[test]
    fn mixed_library_reports_only_the_games_that_moved() {
        let stored_games = [
            stored(1, "Updated", Some("1"), Some("a")),
            stored(2, "Same", Some("2"), Some("b")),
            stored(3, "Legacy", Some("3"), None),
            stored(4, "FolderScan", None, Some("d")),
        ];
        let changed = changed_games(
            &stored_games,
            &current(&[("1", "a2"), ("2", "b"), ("3", "c"), ("4", "d")]),
        );

        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].name, "Updated");
    }

    #[test]
    fn empty_inputs_yield_no_changes() {
        assert!(changed_games(&[], &HashMap::new()).is_empty());
    }
}
