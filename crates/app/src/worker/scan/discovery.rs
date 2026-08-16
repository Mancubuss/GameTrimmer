//! Provider discovery orchestration and its fail-closed status contract.

use super::*;

pub(super) struct DiscoveryOutcome {
    pub(super) libraries: Vec<DiscoveredLibrary>,
    pub(super) diagnostics: Vec<DiscoveryDiagnostic>,
    pub(super) degraded: bool,
}

/// Runs every provider and the persisted manual-library source. Confirmed
/// absence is harmless; degraded inventories remain visible but read-only;
/// any failed provider or panic rejects the generation before staging starts.
///
/// `excluded_libraries` is the persisted `Settings::excluded_libraries`
/// list, naming registered roots the scan does not descend into. Applied via
/// [`drop_excluded`], after the cross-provider merge and dedupe below, not
/// before: filtering earlier would fight the merge, since two providers (or a
/// provider and a manual entry) can describe the same root before it is
/// reconciled into one [`DiscoveredLibrary`].
pub(super) fn discover_libraries(
    conn: &Connection,
    lang: Lang,
    notifier: &Notifier,
    excluded_libraries: &[String],
) -> Result<DiscoveryOutcome, i18n::Reported> {
    let mut libraries = Vec::new();
    let mut diagnostics = Vec::new();
    let mut degraded = false;

    for provider in providers::all() {
        let report = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| provider.discover()))
            .unwrap_or_else(|_| {
                DiscoveryReport::failed(
                    Vec::new(),
                    DiscoveryDiagnostic {
                        provider: provider.name(),
                        stage: "panic",
                        path: None,
                        message: "provider panicked while reading launcher evidence".to_string(),
                    },
                )
            });

        // Diagnostics are debugging material: an app id and a manifest field
        // name tell whoever reads a bug report exactly what happened, and tell
        // the person at the window nothing they can act on. They go to the log
        // in full, and to `scan_diagnostics` for the diagnostic bundle.
        //
        // In full, but not all as failures. A `GAME_ABSENT` note says a game
        // the launcher knows about is not on disk - a paused download, an
        // uninstall the launcher has not caught up with. It has to be logged,
        // because it is the only answer to "why is my game not in the list",
        // and it must not be logged as ERROR, because a log where ordinary
        // states are errors stops being usable for finding the real ones. The
        // question is asked once, by `is_failure`, which is the same predicate
        // that decides whether the library's evidence degrades.
        for diagnostic in &report.diagnostics {
            let detail = match &diagnostic.path {
                Some(path) => format!(
                    "{} [{}: {}]",
                    diagnostic.message,
                    diagnostic.stage,
                    path.display()
                ),
                None => format!("{} [{}]", diagnostic.message, diagnostic.stage),
            };
            let line = i18n::provider_message(Lang::En, diagnostic.provider, detail);
            match diagnostic_level(diagnostic) {
                crate::logger::Level::Error => crate::logger::error(&line),
                crate::logger::Level::Info => crate::logger::log(&line),
            }
        }
        if report.status == DiscoveryStatus::Failed {
            return Err(i18n::Reported::new(lang, |l| {
                i18n::provider_message(
                    l,
                    provider.name(),
                    "discovery failed; the previous complete snapshot was preserved",
                )
            }));
        }

        degraded |= report.status == DiscoveryStatus::Degraded;
        diagnostics.extend(report.diagnostics.iter().cloned());
        let mut discovered = report.data;
        if report.status == DiscoveryStatus::Degraded {
            for library in &mut discovered {
                library.orphan_evidence = OrphanEvidence::Degraded;
            }
        }
        libraries.append(&mut discovered);
    }

    match manual::discover_manual_libraries(conn, lang) {
        Ok((manual_libraries, manual_warnings, manual_diagnostics)) => {
            for warning in manual_warnings {
                notifier.report_warning(warning);
            }
            degraded |= !manual_diagnostics.is_empty();
            diagnostics.extend(manual_diagnostics);
            libraries.extend(manual_libraries);
        }
        Err(error) => {
            return Err(i18n::Reported::new(lang, |l| {
                i18n::manual_libraries_read_failed(l, &error)
            }))
        }
    }

    // Providers and the manual list may report the same root. Libraries are
    // merged first, then duplicate games claimed under different roots are
    // removed using component-boundary path comparison.
    let libraries = providers::merge_libraries_by_path(libraries);
    let libraries = providers::dedupe_games_across_libraries(libraries);
    let libraries = drop_excluded(libraries, excluded_libraries);
    if libraries.is_empty() {
        return Err(i18n::Reported::new(lang, i18n::no_libraries_found));
    }

    Ok(DiscoveryOutcome {
        libraries,
        diagnostics,
        degraded,
    })
}

/// The level one provider diagnostic is logged at. A separate function only
/// so the decision can be asserted directly: driving it through the loop
/// would mean pointing the global logger at a file, and the logger's own
/// tests already have to serialize on that.
fn diagnostic_level(diagnostic: &DiscoveryDiagnostic) -> crate::logger::Level {
    if diagnostic.is_failure() {
        crate::logger::Level::Error
    } else {
        crate::logger::Level::Info
    }
}

/// Drops any library whose root is in `excluded` (a list of
/// [`providers::comparable_path`] keys - see `Settings::excluded_libraries`).
///
/// A library excluded down to nothing here is indistinguishable from one
/// that was never discovered: it falls through to the same
/// `no_libraries_found` error as an empty result from every provider. That is
/// deliberate rather than a special case worth its own message - the UI's
/// floor (the last included library cannot be excluded, see
/// `ui::settings::scanning::show_libraries`) is what is supposed to prevent
/// this in the ordinary flow; a hand-edited ini reaching it anyway should
/// fail the same way any other empty scan does.
fn drop_excluded(libraries: Vec<DiscoveredLibrary>, excluded: &[String]) -> Vec<DiscoveredLibrary> {
    if excluded.is_empty() {
        return libraries;
    }
    libraries
        .into_iter()
        .filter(|library| {
            !excluded
                .iter()
                .any(|key| key == &providers::comparable_path(&library.path))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(path: &str) -> DiscoveredLibrary {
        DiscoveredLibrary {
            vendor: "steam",
            path: PathBuf::from(path),
            games: Vec::new(),
            orphan_evidence: OrphanEvidence::Authoritative,
        }
    }

    fn diagnostic(stage: &'static str) -> DiscoveryDiagnostic {
        DiscoveryDiagnostic {
            provider: "steam",
            stage,
            path: None,
            message: "whatever the provider had to say".to_string(),
        }
    }

    /// GT-134. A paused download is not a failure, and a log that stamps
    /// ordinary states `ERROR` stops being usable for finding the real ones -
    /// which was the complaint: one scan of a healthy library produced
    /// exactly one `[ERROR]` line, and it was this.
    ///
    /// The note still has to be written. It is the only answer to "why is my
    /// game not in the list", so the fix is the level, not silence.
    #[test]
    fn an_absent_game_is_logged_but_not_as_a_failure() {
        assert_eq!(
            diagnostic_level(&diagnostic(providers::GAME_ABSENT)),
            crate::logger::Level::Info
        );
    }

    /// The other half, and the one that matters for safety: a folder the scan
    /// could not examine keeps its `ERROR`, because that is the case where a
    /// live install can drop out of the managed set.
    #[test]
    fn an_unexaminable_install_dir_is_still_logged_as_a_failure() {
        for stage in ["game-path", "manifest-read", "registry-open", "panic"] {
            assert_eq!(
                diagnostic_level(&diagnostic(stage)),
                crate::logger::Level::Error,
                "{stage} must stay a failure"
            );
        }
    }

    #[test]
    fn drop_excluded_removes_only_the_matching_root() {
        let libraries = vec![library(r"F:\SteamLibrary"), library(r"H:\itch.io")];

        let kept = drop_excluded(
            libraries,
            &[providers::comparable_path(Path::new(r"H:\itch.io"))],
        );

        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].path, PathBuf::from(r"F:\SteamLibrary"));
    }

    /// Windows paths are case-insensitive and can be written with either
    /// separator - the same requirement `comparable_path` already exists for
    /// (see `providers::merge_libraries_by_path`), reused rather than
    /// re-derived here.
    #[test]
    fn drop_excluded_compares_case_and_separator_insensitively() {
        let libraries = vec![library(r"H:\itch.io")];

        let kept = drop_excluded(
            libraries,
            &[providers::comparable_path(Path::new("h:/ITCH.IO/"))],
        );

        assert!(kept.is_empty());
    }

    #[test]
    fn drop_excluded_is_a_no_op_with_nothing_excluded() {
        let libraries = vec![library(r"F:\SteamLibrary"), library(r"H:\itch.io")];
        let kept = drop_excluded(libraries.clone(), &[]);
        assert_eq!(kept.len(), libraries.len());
    }

    /// Excluding every registered library must not error inside this
    /// function - the empty result is the caller's signal to raise
    /// `no_libraries_found`, not something `drop_excluded` decides on its own.
    #[test]
    fn drop_excluded_can_empty_the_list_entirely() {
        let libraries = vec![library(r"F:\SteamLibrary")];
        let kept = drop_excluded(
            libraries,
            &[providers::comparable_path(Path::new(r"F:\SteamLibrary"))],
        );
        assert!(kept.is_empty());
    }
}
