//! Riot Games library discovery: per-product settings files under
//! `%ProgramData%\Riot Games\Metadata\<product>.<region>\
//! <product>.<region>.product_settings.yaml`.
//!
//! Two lines are needed, so the YAML is read with a targeted line scan rather
//! than a full YAML parser (no extra dependency; the file is machine-written
//! and flat):
//!
//! ```text
//! product_install_full_path: "H:/Riot Games/VALORANT/live"
//! product_install_root: "H:/Riot Games"
//! ```
//!
//! The root matters because Riot installs games as `<root>\<game>\<channel>` -
//! that trailing `live`/`pbe` channel folder *is* the install dir. Deriving the
//! library from the install dir's parent (as every other provider does) would
//! therefore name `H:\Riot Games\VALORANT` a library, one level too deep, and
//! the vendor-folder scan's `H:\Riot Games` would not merge with it - Riot then
//! shows up twice. Riot states the real root itself, so it is used verbatim.

use std::path::{Path, PathBuf};

use crate::error::Result;

use super::{
    DiscoveredLibrary, DiscoveryDiagnostic, DiscoveryReport, DiscoveryStatus, GameInstall,
    LibraryProvider, OrphanEvidence,
};

// `GAME_ABSENT` and `degrades_evidence` live in `super` - see `steam.rs` for
// the provider that first needed the distinction and `providers::GAME_ABSENT`
// for why it does not degrade evidence on its own.
use super::{degrades_evidence, GAME_ABSENT};

const METADATA_RELATIVE_PATH: &str = r"Riot Games\Metadata";
const DEFAULT_PROGRAM_DATA: &str = r"C:\ProgramData";
const INSTALL_PATH_KEY: &str = "product_install_full_path:";
const INSTALL_ROOT_KEY: &str = "product_install_root:";

/// Metadata entries that are launcher infrastructure, not games.
///
/// Riot is not consistent about how it names these directories: the client's
/// own metadata folder is literally `Riot Client` (no region suffix, a space
/// rather than an underscore), while game folders are `<slug>.<region>` with
/// underscore-separated slugs. Comparison therefore normalizes spaces to
/// underscores - matching only `riot_client` let the client itself through as
/// a "game", so the launcher's own install showed up as a second library
/// beside the real one.
const NON_GAME_SLUGS: &[&str] = &["riot_client"];

/// A metadata directory's slug, normalized for comparison against
/// [`NON_GAME_SLUGS`]: lowercase, with spaces folded to underscores.
fn normalized_slug(slug: &str) -> String {
    slug.trim().to_ascii_lowercase().replace(' ', "_")
}

pub struct RiotProvider;

impl LibraryProvider for RiotProvider {
    fn name(&self) -> &'static str {
        "riot"
    }

    fn try_discover(&self) -> Result<Vec<DiscoveredLibrary>> {
        Ok(discover_riot().data)
    }

    fn discover(&self) -> DiscoveryReport<Vec<DiscoveredLibrary>> {
        discover_riot()
    }
}

fn diagnostic(
    stage: &'static str,
    path: Option<PathBuf>,
    message: impl std::fmt::Display,
) -> DiscoveryDiagnostic {
    DiscoveryDiagnostic {
        provider: "riot",
        stage,
        path,
        message: message.to_string(),
    }
}

fn discover_riot() -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let metadata_dir = program_data_dir().join(METADATA_RELATIVE_PATH);
    let entries = match std::fs::read_dir(&metadata_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return DiscoveryReport::not_installed(Vec::new())
        }
        Err(err) => {
            return DiscoveryReport::failed(
                Vec::new(),
                diagnostic("metadata-enumeration", Some(metadata_dir), err),
            )
        }
    };
    let mut products = Vec::new();
    let mut diagnostics = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                diagnostics.push(diagnostic(
                    "metadata-entry",
                    Some(metadata_dir.clone()),
                    err,
                ));
                continue;
            }
        };
        let product_dir = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) => {
                diagnostics.push(diagnostic("metadata-entry-type", Some(product_dir), err));
                continue;
            }
        };
        if !file_type.is_dir() {
            continue;
        }
        match read_product(&product_dir) {
            // A product whose install dir is simply not there is normal (an
            // uninstall that left the metadata directory behind), and a
            // folder that does not exist cannot be mistaken for orphan
            // residue either. A folder that merely could not be examined is
            // the dangerous case: it stays on disk, drops out of `products`,
            // and anything diffing this library against its managed set
            // would then call it residue. Diagnose it - see `try_is_dir`.
            Ok(Some(product)) => match super::try_is_dir(&product.game.install_dir) {
                Ok(true) => products.push(product),
                // Recorded, but explicitly not degrading - see `GAME_ABSENT`.
                Ok(false) => diagnostics.push(diagnostic(
                    GAME_ABSENT,
                    Some(product.game.install_dir),
                    "product metadata present, install directory absent (uninstall that left the metadata behind)",
                )),
                Err(err) => {
                    diagnostics.push(diagnostic("game-path", Some(product.game.install_dir), err))
                }
            },
            Ok(None) => {}
            Err(err) => {
                diagnostics.push(diagnostic("product-settings-read", Some(product_dir), err))
            }
        }
    }
    let mut libraries = group_by_declared_root(products);
    if degrades_evidence(&diagnostics) {
        for library in &mut libraries {
            library.orphan_evidence = OrphanEvidence::Degraded;
        }
        DiscoveryReport::degraded(libraries, diagnostics)
    } else {
        // Complete, but not necessarily silent: a `GAME_ABSENT` note still
        // travels so it reaches the log and `scan_diagnostics`.
        DiscoveryReport {
            data: libraries,
            status: DiscoveryStatus::Complete,
            diagnostics,
        }
    }
}

/// One product's metadata: the game, plus the library root Riot itself declared
/// for it (absent only if the settings file omitted the key).
#[derive(Debug)]
struct ProductEntry {
    game: GameInstall,
    root: Option<PathBuf>,
}

/// Groups products into libraries by the root Riot declared, falling back to
/// the install dir's parent when a settings file has no `product_install_root`
/// (never seen in practice - the fallback exists so a missing key degrades to
/// the old behaviour rather than dropping the game).
fn group_by_declared_root(products: Vec<ProductEntry>) -> Vec<DiscoveredLibrary> {
    let mut libraries: Vec<DiscoveredLibrary> = Vec::new();

    for product in products {
        let root = match product.root {
            Some(root) => root,
            None => match product.game.install_dir.parent() {
                Some(parent) => parent.to_path_buf(),
                None => continue,
            },
        };

        let existing = libraries.iter_mut().find(|library| {
            library.path.to_string_lossy().to_lowercase() == root.to_string_lossy().to_lowercase()
        });

        match existing {
            Some(library) => library.games.push(product.game),
            None => libraries.push(DiscoveredLibrary {
                vendor: "riot",
                path: root,
                games: vec![product.game],
                orphan_evidence: OrphanEvidence::Authoritative,
            }),
        }
    }

    libraries
}

fn program_data_dir() -> PathBuf {
    std::env::var("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_PROGRAM_DATA))
}

/// Reads one `Metadata\<product>.<region>` directory into a [`ProductEntry`],
/// if it describes an installed game.
///
/// Returns `Ok(None)` for a directory that is not a game at all
/// ([`NON_GAME_SLUGS`]) and for a settings file that was read fine but
/// declares no install path - both mean "nothing to report", not a failure.
/// `Err` is reserved for cases where the settings file could not be read at
/// all, which is a real gap in the inventory.
fn read_product(product_dir: &Path) -> std::result::Result<Option<ProductEntry>, String> {
    let dir_name = product_dir
        .file_name()
        .ok_or_else(|| "metadata directory has no name".to_string())?
        .to_string_lossy()
        .into_owned();
    let slug = dir_name.split('.').next().unwrap_or(&dir_name);

    let normalized = normalized_slug(slug);
    if NON_GAME_SLUGS
        .iter()
        .any(|excluded| *excluded == normalized)
    {
        return Ok(None);
    }

    let settings_path = product_dir.join(format!("{dir_name}.product_settings.yaml"));
    let contents = std::fs::read_to_string(settings_path).map_err(|err| err.to_string())?;
    // A settings file that reads fine but has no `product_install_full_path`
    // is not a failure - it is Riot saying nothing is installed for this
    // product. `teamfighttactics.live` and `.pbe` are part of League and
    // never installed on their own, but still get a metadata directory and a
    // settings file. Treating that the same as an unreadable file would flip
    // every Riot library to `Degraded` (see the caller) over a product that
    // was never going to be there, so this joins the `NON_GAME_SLUGS` path:
    // `Ok(None)`, skipped silently, no diagnostic.
    let Some(install_path) = extract_path(&contents, INSTALL_PATH_KEY) else {
        return Ok(None);
    };
    let path = PathBuf::from(install_path);

    Ok(Some(ProductEntry {
        game: GameInstall {
            name: display_name_for(slug, &path),
            install_dir: path,
            app_id: Some(dir_name),
        },
        root: extract_path(&contents, INSTALL_ROOT_KEY).map(PathBuf::from),
    }))
}

/// Extracts one `key: "value"` line from the settings YAML - a flat file whose
/// path values use forward slashes.
fn extract_path(yaml: &str, key: &str) -> Option<String> {
    let raw = yaml
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(key))?
        .trim()
        .trim_matches('"');

    let normalized = raw.replace('/', "\\");
    let trimmed = normalized.trim_end_matches('\\');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Maps Riot's internal product slugs to display names, falling back to the
/// install directory's last path component (which Riot names after the game).
fn display_name_for(slug: &str, install_dir: &Path) -> String {
    match slug.to_ascii_lowercase().as_str() {
        "valorant" => "VALORANT".to_string(),
        "league_of_legends" => "League of Legends".to_string(),
        "bacon" => "Legends of Runeterra".to_string(),
        _ => install_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| slug.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim shape of a real `valorant.live.product_settings.yaml`.
    const VALORANT_YAML: &str = concat!(
        "channel: live\n",
        "product_install_full_path: \"H:/Riot Games/VALORANT/live\"\n",
        "product_install_root: \"H:/Riot Games\"\n",
    );

    #[test]
    fn extract_path_reads_quoted_forward_slash_path() {
        assert_eq!(
            extract_path(VALORANT_YAML, INSTALL_PATH_KEY).as_deref(),
            Some(r"H:\Riot Games\VALORANT\live")
        );
    }

    /// The line the provider used to ignore, which is why Riot appeared twice.
    #[test]
    fn extract_path_reads_the_declared_install_root() {
        assert_eq!(
            extract_path(VALORANT_YAML, INSTALL_ROOT_KEY).as_deref(),
            Some(r"H:\Riot Games")
        );
    }

    #[test]
    fn extract_path_reads_unquoted_value() {
        let yaml = "product_install_full_path: C:/Riot Games/League of Legends\n";
        assert_eq!(
            extract_path(yaml, INSTALL_PATH_KEY).as_deref(),
            Some(r"C:\Riot Games\League of Legends")
        );
    }

    #[test]
    fn extract_path_returns_none_when_key_absent() {
        assert!(extract_path("channel: live\n", INSTALL_PATH_KEY).is_none());
        assert!(extract_path(VALORANT_YAML, "no_such_key:").is_none());
    }

    #[test]
    fn extract_path_returns_none_for_empty_value() {
        assert!(extract_path("product_install_full_path: \"\"\n", INSTALL_PATH_KEY).is_none());
    }

    fn product(name: &str, install_dir: &str, root: Option<&str>) -> ProductEntry {
        ProductEntry {
            game: GameInstall {
                name: name.to_string(),
                install_dir: PathBuf::from(install_dir),
                app_id: None,
            },
            root: root.map(PathBuf::from),
        }
    }

    /// The bug in one assertion: grouping by the install dir's parent gives
    /// `H:\Riot Games\VALORANT`, which the vendor-folder scan's `H:\Riot Games`
    /// then cannot merge with, so Riot is listed twice.
    #[test]
    fn group_by_declared_root_uses_the_root_not_the_channel_folders_parent() {
        let libraries = group_by_declared_root(vec![product(
            "VALORANT",
            r"H:\Riot Games\VALORANT\live",
            Some(r"H:\Riot Games"),
        )]);

        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].path, PathBuf::from(r"H:\Riot Games"));
        assert_eq!(libraries[0].games.len(), 1);
    }

    #[test]
    fn group_by_declared_root_puts_several_games_of_one_root_together() {
        let libraries = group_by_declared_root(vec![
            product(
                "VALORANT",
                r"H:\Riot Games\VALORANT\live",
                Some(r"H:\Riot Games"),
            ),
            product(
                "League of Legends",
                r"h:\riot games\League of Legends\live",
                Some(r"h:\riot games"),
            ),
        ]);

        assert_eq!(libraries.len(), 1, "same root, different case");
        assert_eq!(libraries[0].games.len(), 2);
    }

    #[test]
    fn group_by_declared_root_splits_distinct_roots() {
        let libraries = group_by_declared_root(vec![
            product("A", r"H:\Riot Games\A\live", Some(r"H:\Riot Games")),
            product("B", r"F:\Riot\B\live", Some(r"F:\Riot")),
        ]);

        assert_eq!(libraries.len(), 2);
    }

    /// No declared root: fall back to the old behaviour rather than lose the
    /// game entirely.
    #[test]
    fn group_by_declared_root_falls_back_to_the_install_dirs_parent() {
        let libraries = group_by_declared_root(vec![product(
            "VALORANT",
            r"H:\Riot Games\VALORANT\live",
            None,
        )]);

        assert_eq!(libraries[0].path, PathBuf::from(r"H:\Riot Games\VALORANT"));
    }

    #[test]
    fn display_name_for_maps_known_slugs() {
        let dir = PathBuf::from(r"F:\Riot Games\VALORANT\live");
        assert_eq!(display_name_for("valorant", &dir), "VALORANT");
        assert_eq!(
            display_name_for("league_of_legends", &dir),
            "League of Legends"
        );
        assert_eq!(display_name_for("bacon", &dir), "Legends of Runeterra");
    }

    /// The shape actually on disk: `%ProgramData%\Riot Games\Metadata\
    /// Riot Client` - a space, no region suffix - which an exact match against
    /// `riot_client` misses, registering the launcher itself as a game.
    #[test]
    fn normalized_slug_folds_the_riot_client_folder_onto_its_slug() {
        assert_eq!(normalized_slug("Riot Client"), "riot_client");
        assert_eq!(normalized_slug("riot_client"), "riot_client");
        assert_eq!(normalized_slug("RIOT CLIENT"), "riot_client");
    }

    #[test]
    fn normalized_slug_leaves_game_slugs_alone() {
        assert_eq!(normalized_slug("valorant"), "valorant");
        assert_eq!(normalized_slug("league_of_legends"), "league_of_legends");
    }

    #[test]
    fn display_name_for_falls_back_to_install_dir_leaf() {
        let dir = PathBuf::from(r"F:\Riot Games\2XKO");
        assert_eq!(display_name_for("2xko", &dir), "2XKO");
    }

    /// `teamfighttactics.live` and `.pbe` are part of League and never
    /// installed on their own, but Riot still writes them a metadata
    /// directory and a settings file with no `product_install_full_path`.
    /// That must read as "nothing here", the same as an excluded slug - not
    /// as a failure that would flip the whole Riot library to `Degraded`.
    #[test]
    fn read_product_returns_ok_none_when_settings_file_has_no_install_path() {
        let root = tempfile::tempdir().unwrap();
        let product_dir = root.path().join("teamfighttactics.live");
        std::fs::create_dir(&product_dir).unwrap();
        std::fs::write(
            product_dir.join("teamfighttactics.live.product_settings.yaml"),
            "channel: live\n",
        )
        .unwrap();

        let result = read_product(&product_dir);
        assert!(
            matches!(result, Ok(None)),
            "a readable settings file with no install path is not a failure: {result:?}"
        );
    }

    /// The counterpart: a settings file that could not be read at all is a
    /// real gap in the inventory - a game may be missing and there is no way
    /// to tell - so it must surface as `Err`, not be swallowed like the
    /// "nothing installed" case above. Standing in for "unreadable" with
    /// "missing" since forcing a genuinely unreadable file is awkward on
    /// Windows; both take the same `read_to_string` error path.
    #[test]
    fn read_product_errs_when_the_settings_file_is_missing() {
        let root = tempfile::tempdir().unwrap();
        let product_dir = root.path().join("valorant.live");
        std::fs::create_dir(&product_dir).unwrap();
        // No `.product_settings.yaml` written.

        let result = read_product(&product_dir);
        assert!(
            result.is_err(),
            "an unreadable settings file must not be mistaken for 'nothing installed': {result:?}"
        );
    }
}
