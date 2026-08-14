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
pub(super) fn discover_libraries(
    conn: &Connection,
    lang: Lang,
    notifier: &Notifier,
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
            crate::logger::error(&i18n::provider_failed(
                Lang::En,
                diagnostic.provider,
                detail,
            ));
        }
        if report.status == DiscoveryStatus::Failed {
            return Err(i18n::Reported::new(lang, |l| {
                i18n::provider_failed(
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
    if libraries.is_empty() {
        return Err(i18n::Reported::new(lang, i18n::no_libraries_found));
    }

    Ok(DiscoveryOutcome {
        libraries,
        diagnostics,
        degraded,
    })
}
