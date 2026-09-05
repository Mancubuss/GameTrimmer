//! Riot Games library discovery: per-product settings files under
//! `%ProgramData%\Riot Games\Metadata\<product>.<region>\
//! <product>.<region>.product_settings.yaml`.
//!
//! A handful of flat `key: value` lines are needed, so the YAML is read with a
//! targeted line scan rather than a full YAML parser (no extra dependency;
//! the file is machine-written and, aside from one short block list, flat):
//!
//! ```text
//! auto_patching_enabled_by_player: false
//! locale_data:
//!     available_locales:
//!     - "en_US"
//!     - "de_DE"
//!     default_locale: "en_US"
//! patching_policy: "manual"
//! product_install_full_path: "H:/Riot Games/VALORANT/live"
//! product_install_root: "H:/Riot Games"
//! settings:
//!     locale: "en_US"
//! ```
//!
//! The root matters because Riot installs games as `<root>\<game>\<channel>` -
//! that trailing `live`/`pbe` channel folder *is* the install dir. Deriving the
//! library from the install dir's parent (as every other provider does) would
//! therefore name `H:\Riot Games\VALORANT` a library, one level too deep, and
//! the vendor-folder scan's `H:\Riot Games` would not merge with it - Riot then
//! shows up twice. Riot states the real root itself, so it is used verbatim.
//!
//! The same file also settles whether a trim on this product survives past
//! the next launch: Riot's patcher re-downloads whatever it thinks is missing
//! unless patching is manual, so `read_product`'s single read is extended
//! (not repeated - see [`extract_patch_info`]) to pull the patch-policy and
//! installed-locale facts alongside the two path keys above. See
//! [`AutoPatching`] for the policy tiebreak and [`RiotPatchInfo`] for the
//! shape a caller joins to a game by `app_id`.

use std::path::{Path, PathBuf};

use crate::error::Result;

use super::{
    diagnostic, DiscoveredLibrary, DiscoveryReport, DiscoveryStatus, GameInstall, LibraryProvider,
    OrphanEvidence,
};

// `GAME_ABSENT` and `degrades_evidence` live in `super` - see `steam.rs` for
// the provider that first needed the distinction and `providers::GAME_ABSENT`
// for why it does not degrade evidence on its own.
use super::{degrades_evidence, GAME_ABSENT};

const METADATA_RELATIVE_PATH: &str = r"Riot Games\Metadata";
const DEFAULT_PROGRAM_DATA: &str = r"C:\ProgramData";
const INSTALL_PATH_KEY: &str = "product_install_full_path:";
const INSTALL_ROOT_KEY: &str = "product_install_root:";
const AUTO_PATCHING_ENABLED_KEY: &str = "auto_patching_enabled_by_player:";
const PATCHING_POLICY_KEY: &str = "patching_policy:";
const AVAILABLE_LOCALES_HEADER: &str = "available_locales:";
const ACTIVE_LOCALE_KEY: &str = "locale:";

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

/// Thin projection of [`discover_riot_scan`] for the [`LibraryProvider`]
/// trait, which only has room for libraries. Still one directory walk, one
/// read per file - see `discover_riot_scan`'s doc comment.
fn discover_riot() -> DiscoveryReport<Vec<DiscoveredLibrary>> {
    let report = discover_riot_scan();
    DiscoveryReport {
        data: report.data.libraries,
        status: report.status,
        diagnostics: report.diagnostics,
    }
}

/// Same scan as `discover_riot`, but also carrying the patch-policy and
/// locale facts read from each product's settings file. Public so a future
/// caller that needs both the libraries and the patch info can get them from
/// one walk instead of two.
pub fn discover_riot_scan() -> DiscoveryReport<RiotScan> {
    let metadata_dir = program_data_dir().join(METADATA_RELATIVE_PATH);
    let entries = match std::fs::read_dir(&metadata_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return DiscoveryReport::not_installed(RiotScan {
                libraries: Vec::new(),
                patch_info: Vec::new(),
            })
        }
        Err(err) => {
            return DiscoveryReport::failed(
                RiotScan {
                    libraries: Vec::new(),
                    patch_info: Vec::new(),
                },
                diagnostic("riot", "metadata-enumeration", Some(metadata_dir), err),
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
                    "riot",
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
                diagnostics.push(diagnostic(
                    "riot",
                    "metadata-entry-type",
                    Some(product_dir),
                    err,
                ));
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
                Ok(false) => diagnostics.push(diagnostic("riot", 
                    GAME_ABSENT,
                    Some(product.game.install_dir),
                    "product metadata present, install directory absent (uninstall that left the metadata behind)",
                )),
                Err(err) => {
                    diagnostics.push(diagnostic("riot", "game-path", Some(product.game.install_dir), err))
                }
            },
            Ok(None) => {}
            Err(err) => {
                diagnostics.push(diagnostic("riot", "product-settings-read", Some(product_dir), err))
            }
        }
    }
    // Patch info is cloned out of `products` *before* `group_by_declared_root`
    // consumes it - a second directory walk or a second `read_to_string` per
    // file would cost the same file read twice for the one scan. Cloning a
    // few small strings is not that; see [`RiotScan`].
    let patch_info: Vec<RiotPatchInfo> = products.iter().map(|p| p.patch_info.clone()).collect();
    let mut libraries = group_by_declared_root(products);
    if degrades_evidence(&diagnostics) {
        for library in &mut libraries {
            library.orphan_evidence = OrphanEvidence::Degraded;
        }
        DiscoveryReport::degraded(
            RiotScan {
                libraries,
                patch_info,
            },
            diagnostics,
        )
    } else {
        // Complete, but not necessarily silent: a `GAME_ABSENT` note still
        // travels so it reaches the log and `scan_diagnostics`.
        DiscoveryReport {
            data: RiotScan {
                libraries,
                patch_info,
            },
            status: DiscoveryStatus::Complete,
            diagnostics,
        }
    }
}

/// Everything one metadata-directory scan produces: the libraries the
/// [`LibraryProvider`] trait needs, and the per-product patch/locale facts a
/// caller can join to a game by `app_id`. Both come from the same walk and
/// the same read of each settings file - a caller that wants both must call
/// [`discover_riot_scan`] once rather than pairing `discover()` with a
/// second, separate read of these files.
pub struct RiotScan {
    pub libraries: Vec<DiscoveredLibrary>,
    pub patch_info: Vec<RiotPatchInfo>,
}

/// One product's metadata: the game, plus the library root Riot itself declared
/// for it (absent only if the settings file omitted the key), plus the
/// patch-policy and locale facts read from the same settings file.
#[derive(Debug)]
struct ProductEntry {
    game: GameInstall,
    root: Option<PathBuf>,
    patch_info: RiotPatchInfo,
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
    // Computed before `dir_name` is moved into `app_id` below - same string,
    // used as the join key for both.
    let patch_info = extract_patch_info(&contents, &dir_name);

    Ok(Some(ProductEntry {
        game: GameInstall {
            name: display_name_for(slug, &path),
            install_dir: path,
            app_id: Some(dir_name),
        },
        root: extract_path(&contents, INSTALL_ROOT_KEY).map(PathBuf::from),
        patch_info,
    }))
}

/// Extracts one `key: "value"` line's raw value, quotes and surrounding
/// whitespace stripped - `None` if the key is absent or the value empty.
/// Nesting is irrelevant here: every line is trimmed before comparison, so a
/// key several levels deep in the YAML (e.g. `settings.locale`) is found the
/// same way as a top-level one. Shared by [`extract_path`] (which additionally
/// normalizes slashes for path values) and the patch/locale extractors below,
/// which want the string untouched.
fn extract_raw<'a>(yaml: &'a str, key: &str) -> Option<&'a str> {
    let raw = yaml
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(key))?
        .trim()
        .trim_matches('"');
    (!raw.is_empty()).then_some(raw)
}

/// Extracts one `key: "value"` line from the settings YAML - a flat file whose
/// path values use forward slashes.
fn extract_path(yaml: &str, key: &str) -> Option<String> {
    let raw = extract_raw(yaml, key)?;
    let normalized = raw.replace('/', "\\");
    let trimmed = normalized.trim_end_matches('\\');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Extracts one `key: true`/`key: false` line. Anything else on that line
/// (missing key, or a value that is neither literal) comes back as `None` -
/// this must never guess, since callers ([`auto_patching_status`]) treat
/// `None` as "unresolved" rather than picking a default.
fn extract_bool(yaml: &str, key: &str) -> Option<bool> {
    match extract_raw(yaml, key)? {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Extracts the `available_locales:` block list:
/// ```text
/// available_locales:
/// - "en_US"
/// - "de_DE"
/// ```
/// Returns an empty `Vec` when the header is absent or has no `- "..."` lines
/// under it - never fabricated, since an empty result here means "Riot did
/// not say", not "no locales installed".
fn extract_locale_list(yaml: &str) -> Vec<String> {
    let mut lines = yaml.lines().map(str::trim);
    if lines
        .by_ref()
        .find(|line| *line == AVAILABLE_LOCALES_HEADER)
        .is_none()
    {
        return Vec::new();
    }
    lines
        .take_while(|line| line.starts_with('-'))
        .filter_map(|line| {
            let value = line.trim_start_matches('-').trim().trim_matches('"');
            (!value.is_empty()).then(|| value.to_string())
        })
        .collect()
}

/// Whether Riot will re-download patches for a product without the player
/// asking - and therefore whether a file trim on it survives past the next
/// launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoPatching {
    /// Riot will fetch and apply updates on its own; a trim will not survive.
    Automatic,
    /// The player (or Riot, for this product) patches by hand; a trim
    /// survives until the next manual patch.
    Manual,
    /// The file did not say clearly enough to answer either way. Must never
    /// be reported as `Manual` - see the module doc for why a wrong "it will
    /// survive" is the dangerous mistake here, not a merely inconvenient one.
    Unknown,
}

/// Resolves [`AutoPatching`] from the two fields Riot writes, which can
/// disagree:
///
/// - `auto_patching_enabled_by_player` is a plain boolean answering exactly
///   the question this function asks, with no vocabulary to misread.
/// - `patching_policy` is a free-form string; the only value seen in
///   practice, and the only one this code trusts, is `"manual"`.
///
/// The boolean wins whenever it parses, disagreement included: it is Riot's
/// most direct, least ambiguous statement of what will actually happen, and
/// trusting an opaque string over an explicit boolean for no reason would be
/// backwards. `patching_policy` is consulted only as a fallback when the
/// boolean is missing or unparseable, and only for that one recognized
/// value - an unrecognized string, or both keys absent, resolves to
/// `Unknown` rather than a guessed `Manual` or `Automatic`.
fn auto_patching_status(yaml: &str) -> AutoPatching {
    if let Some(enabled) = extract_bool(yaml, AUTO_PATCHING_ENABLED_KEY) {
        return if enabled {
            AutoPatching::Automatic
        } else {
            AutoPatching::Manual
        };
    }
    match extract_raw(yaml, PATCHING_POLICY_KEY) {
        Some("manual") => AutoPatching::Manual,
        _ => AutoPatching::Unknown,
    }
}

/// Patch-policy and locale facts read from one product's settings file,
/// keyed by `app_id` (the metadata directory name, e.g. `valorant.live`) so a
/// caller can join it to the matching [`GameInstall`].
#[derive(Debug, Clone)]
pub struct RiotPatchInfo {
    pub app_id: String,
    pub auto_patching: AutoPatching,
    /// Locales Riot has installed for this product (`locale_data.available_locales`).
    /// Empty when the file does not say - never fabricated.
    pub available_locales: Vec<String>,
    /// The locale currently active (`settings.locale`), if the file says.
    pub active_locale: Option<String>,
}

/// Builds a [`RiotPatchInfo`] from the same `contents` string `read_product`
/// already read - no second file read for any of these fields.
fn extract_patch_info(yaml: &str, app_id: &str) -> RiotPatchInfo {
    RiotPatchInfo {
        app_id: app_id.to_string(),
        auto_patching: auto_patching_status(yaml),
        available_locales: extract_locale_list(yaml),
        active_locale: extract_raw(yaml, ACTIVE_LOCALE_KEY).map(str::to_string),
    }
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

    /// Verbatim shape of a real `valorant.live.product_settings.yaml`
    /// (captured 2026-09-05, `auto_patching_enabled_by_player: false` /
    /// `patching_policy: "manual"` agreeing - both `teamfighttactics.live`
    /// and `.pbe` match this shape too).
    const VALORANT_YAML: &str = concat!(
        "auto_patching_enabled_by_player: false\n",
        "channel: live\n",
        "locale_data:\n",
        "    available_locales:\n",
        "    - \"en_US\"\n",
        "    - \"de_DE\"\n",
        "    - \"ja_JP\"\n",
        "    default_locale: \"en_US\"\n",
        "patching_policy: \"manual\"\n",
        "product_install_full_path: \"H:/Riot Games/VALORANT/live\"\n",
        "product_install_root: \"H:/Riot Games\"\n",
        "settings:\n",
        "    locale: \"en_US\"\n",
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
            // These grouping tests don't care about patch info.
            patch_info: RiotPatchInfo {
                app_id: name.to_string(),
                auto_patching: AutoPatching::Unknown,
                available_locales: Vec::new(),
                active_locale: None,
            },
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

    #[test]
    fn auto_patching_status_reads_manual_from_the_real_valorant_shape() {
        assert_eq!(auto_patching_status(VALORANT_YAML), AutoPatching::Manual);
    }

    #[test]
    fn auto_patching_status_detects_automatic_from_the_player_toggle() {
        let yaml = "auto_patching_enabled_by_player: true\n";
        assert_eq!(auto_patching_status(yaml), AutoPatching::Automatic);
    }

    /// The disagreement case: the boolean is Riot's most direct statement of
    /// what will happen, so it wins over a `patching_policy` string that
    /// says the opposite - see `auto_patching_status`'s doc comment.
    #[test]
    fn auto_patching_status_lets_the_player_toggle_override_a_disagreeing_policy() {
        let yaml = "auto_patching_enabled_by_player: true\npatching_policy: \"manual\"\n";
        assert_eq!(
            auto_patching_status(yaml),
            AutoPatching::Automatic,
            "the boolean says automatic; the opaque policy string must not win"
        );
    }

    /// Missing entirely must never default to `Manual` - a wrong "it will
    /// survive" is the dangerous mistake, not a merely inconvenient one.
    #[test]
    fn auto_patching_status_is_unknown_when_both_keys_are_absent() {
        assert_eq!(
            auto_patching_status("channel: live\n"),
            AutoPatching::Unknown
        );
    }

    /// An unrecognized `patching_policy` value (no boolean present to
    /// resolve it) must also stay `Unknown`, never guessed as `Manual`.
    #[test]
    fn auto_patching_status_is_unknown_for_an_unrecognized_policy_value() {
        assert_eq!(
            auto_patching_status("patching_policy: \"scheduled\"\n"),
            AutoPatching::Unknown
        );
    }

    #[test]
    fn extract_locale_list_reads_the_real_valorant_shape() {
        assert_eq!(
            extract_locale_list(VALORANT_YAML),
            vec!["en_US", "de_DE", "ja_JP"]
        );
    }

    #[test]
    fn extract_locale_list_is_empty_when_locale_data_is_absent() {
        assert!(extract_locale_list("channel: live\n").is_empty());
    }

    #[test]
    fn extract_locale_list_is_empty_when_the_header_has_no_items() {
        assert!(extract_locale_list("available_locales:\ndefault_locale: \"en_US\"\n").is_empty());
    }

    #[test]
    fn extract_patch_info_reads_the_active_locale_from_settings() {
        let info = extract_patch_info(VALORANT_YAML, "valorant.live");
        assert_eq!(info.app_id, "valorant.live");
        assert_eq!(info.active_locale.as_deref(), Some("en_US"));
        assert_eq!(info.auto_patching, AutoPatching::Manual);
        assert_eq!(info.available_locales, vec!["en_US", "de_DE", "ja_JP"]);
    }

    #[test]
    fn extract_patch_info_leaves_active_locale_none_when_settings_is_absent() {
        let info = extract_patch_info("channel: live\n", "riftbound.live");
        assert_eq!(info.active_locale, None);
    }
}
