//! Persisted user settings, stored in `gametrimmer.ini` next to the executable.
//! Unknown keys, malformed lines and unparseable values fall back to defaults,
//! so a file written by a newer version never breaks an older one.
//!
//! The SQLite `settings` table is a legacy, read-once migration source. It is
//! consulted only when the ini file does not exist; production writes never go
//! back to the database.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension};

use crate::error::Result;

/// How the delete action disposes of files.
///
/// `Permanent` is the default: game files are always re-downloadable from
/// the store, so the fastest possible removal wins over recoverability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeleteMethod {
    /// `std::fs` removal - fastest, unrecoverable.
    #[default]
    Permanent,
    /// Move to the Windows Recycle Bin - slower, recoverable.
    RecycleBin,
}

impl DeleteMethod {
    /// Stable string form persisted in `gametrimmer.ini`.
    pub fn as_str(self) -> &'static str {
        match self {
            DeleteMethod::Permanent => "permanent",
            DeleteMethod::RecycleBin => "recycle_bin",
        }
    }

    /// Inverse of [`as_str`](Self::as_str). `None` for unknown values (e.g.
    /// written by a future version) - callers fall back to the default.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "permanent" => Some(DeleteMethod::Permanent),
            "recycle_bin" => Some(DeleteMethod::RecycleBin),
            _ => None,
        }
    }
}

/// UI language for all user-facing text.
///
/// `En` is the default: the audience for a Windows disk-cleanup tool skews
/// international, and English avoids surprising a user whose Windows locale
/// isn't Ukrainian. `Uk` stays fully supported - the strings started life in
/// Ukrainian, and it is still the primary maintainer's own language.
///
/// Custom community languages (e.g. `pl`, `de`, `fr`) are supported via [`Lang::Custom`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    /// English UI text.
    #[default]
    En,
    /// Ukrainian UI text.
    Uk,
    /// Custom community language code (up to 8 ASCII bytes).
    Custom([u8; 8]),
}

impl Lang {
    /// Stable string form persisted in `gametrimmer.ini`.
    pub fn as_str(&self) -> &str {
        match self {
            Lang::En => "en",
            Lang::Uk => "uk",
            Lang::Custom(bytes) => {
                let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                std::str::from_utf8(&bytes[..len]).unwrap_or("en")
            }
        }
    }

    /// Inverse of [`as_str`](Self::as_str). `None` for invalid or empty values.
    pub fn parse(value: &str) -> Option<Self> {
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed.len() > 8 {
            return None;
        }
        match trimmed.to_lowercase().as_str() {
            "en" => Some(Lang::En),
            "uk" => Some(Lang::Uk),
            other if other.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') => {
                let mut bytes = [0u8; 8];
                bytes[..other.len()].copy_from_slice(other.as_bytes());
                Some(Lang::Custom(bytes))
            }
            _ => None,
        }
    }
}

/// What the user picked for the UI language: a specific one, or "whatever
/// Windows is set to".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LanguagePreference {
    /// Follow the OS UI language, falling back to [`Lang::En`] when Windows
    /// prefers something this app does not speak.
    #[default]
    System,
    /// An explicit choice, which never yields to the OS.
    Fixed(Lang),
}

impl LanguagePreference {
    /// Stable string form persisted in `gametrimmer.ini`.
    pub fn as_str(&self) -> &str {
        match self {
            LanguagePreference::System => "system",
            LanguagePreference::Fixed(lang) => lang.as_str(),
        }
    }

    /// Inverse of [`as_str`](Self::as_str). `None` for unknown values.
    pub fn parse(value: &str) -> Option<Self> {
        let trimmed = value.trim();
        if trimmed.eq_ignore_ascii_case("system") {
            Some(LanguagePreference::System)
        } else {
            Lang::parse(trimmed).map(LanguagePreference::Fixed)
        }
    }

    /// The language to actually render in, given what the OS reports.
    pub fn resolve(self, system: Lang) -> Lang {
        match self {
            LanguagePreference::System => system,
            LanguagePreference::Fixed(lang) => lang,
        }
    }
}

/// The value the retired `scan_routing` setting used for "never use the MFT
/// index". It is the only one of the three old modes that carried a decision
/// worth keeping, and [`Settings::never_ask_elevation`] is where it now
/// lives - see that field for why the other two were dropped.
pub(crate) const LEGACY_ROUTING_FORCE_WALKDIR: &str = "force_walkdir";

/// UI color scheme.
///
/// `System` is the default: it follows the OS preference (light/dark),
/// which is what a well-behaved Windows app is expected to do out of the
/// box. `Light`/`Dark` let the user override that when they prefer a fixed
/// look regardless of what Windows is set to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Follow the OS light/dark preference.
    #[default]
    System,
    /// Always light, regardless of the OS setting.
    Light,
    /// Always dark, regardless of the OS setting.
    Dark,
}

impl Theme {
    /// Stable string form persisted in `gametrimmer.ini`.
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    /// Inverse of [`as_str`](Self::as_str). `None` for unknown values (e.g.
    /// written by a future version) - callers fall back to the default.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Theme::System),
            "light" => Some(Theme::Light),
            "dark" => Some(Theme::Dark),
            _ => None,
        }
    }
}

/// Which findings a scan pre-selects for deletion - the "aggressiveness" the
/// user picks instead of hand-reasoning about per-category confidence. It is a
/// pure *selection* policy applied over already-scanned findings (see
/// `gametrimmer_app::model::profile_auto_selects`), so switching profiles
/// re-selects without re-scanning. Orthogonal to
/// [`Settings::enabled_categories`], which decides what is *scanned* at all.
///
/// `Balanced` is the default: it pre-selects the residue a launcher will not
/// restore (orphaned leftovers, bonus material, documentation) plus languages
/// outside the keep-list - the everyday "reclaim the obvious" set - while
/// leaving redistributables and dev leftovers for the user to opt into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionProfile {
    /// Only what a launcher will not bring back on its own: orphaned residue,
    /// bonus material, documentation.
    Cautious,
    /// `Cautious` plus non-keep-list localization files (which only ever exist
    /// for languages already outside the keep-list).
    #[default]
    Balanced,
    /// `Balanced` plus everything else at or above the aggressive confidence
    /// floor (see `gametrimmer_app::model::AGGRESSIVE_CONFIDENCE_FLOOR`).
    Aggressive,
    /// No profile: the plain confidence threshold decides
    /// (`gametrimmer_app::model::AUTO_SELECT_CONFIDENCE_THRESHOLD`). Entered
    /// when the user hand-edits the selection, so manual choices are not
    /// clobbered by a profile policy.
    Custom,
}

impl SelectionProfile {
    /// Stable string form persisted in `gametrimmer.ini`.
    pub fn as_str(self) -> &'static str {
        match self {
            SelectionProfile::Cautious => "cautious",
            SelectionProfile::Balanced => "balanced",
            SelectionProfile::Aggressive => "aggressive",
            SelectionProfile::Custom => "custom",
        }
    }

    /// Inverse of [`as_str`](Self::as_str). `None` for unknown values (e.g.
    /// written by a future version) - callers fall back to the default.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cautious" => Some(SelectionProfile::Cautious),
            "balanced" => Some(SelectionProfile::Balanced),
            "aggressive" => Some(SelectionProfile::Aggressive),
            "custom" => Some(SelectionProfile::Custom),
            _ => None,
        }
    }
}

/// When the delete confirmation modal is shown before a removal runs.
///
/// One of the three independent switches in the settings dialog's "Selection
/// & deletion" section, alongside [`Settings::default_selection_profile`]
/// (what a scan pre-checks) and [`DeleteMethod`] (how a file is disposed of).
/// The old dialog blurred the three together, although none of them affects
/// the others.
///
/// [`Always`](Self::Always) is the default - skipping the confirmation is a
/// choice a user should have to make deliberately, not one an accidental
/// click can leave them in.
///
/// # Why there is no size threshold any more
///
/// A third option, "only above 1 GB", used to sit between these two. It
/// compared against the *batch* total, not any single file, which is not what
/// its label says and not what a reader assumed: 200 files of 10 MB tripped it
/// and one 900 MB file did not. A setting whose behaviour cannot be read off
/// its own label is worse than no setting, and the threshold was arbitrary
/// besides - so this is now the plain question it always was, asked or not
/// asked. A stored `only_above_1gb` no longer parses and therefore falls back
/// to [`Always`](Self::Always), which is the safe direction: nobody is silently
/// upgraded into deleting without being asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfirmBehavior {
    /// Show the confirmation modal before every deletion.
    #[default]
    Always,
    /// Never ask - the delete starts as soon as the button is clicked.
    Never,
}

impl ConfirmBehavior {
    /// Stable string form persisted in `gametrimmer.ini`.
    pub fn as_str(self) -> &'static str {
        match self {
            ConfirmBehavior::Always => "always",
            ConfirmBehavior::Never => "never",
        }
    }

    /// Inverse of [`as_str`](Self::as_str). `None` for unknown values - a
    /// setting written by a future version, or the retired `only_above_1gb`
    /// (see the type docs). Callers fall back to the default.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "always" => Some(ConfirmBehavior::Always),
            "never" => Some(ConfirmBehavior::Never),
            _ => None,
        }
    }

    /// Whether a deletion needs confirming under this policy.
    pub fn should_confirm(self) -> bool {
        match self {
            ConfirmBehavior::Always => true,
            ConfirmBehavior::Never => false,
        }
    }
}

/// Operating mode for the background monitor companion daemon (`gametrimmer-watch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum WatchMode {
    /// Interactive Toast notification with action buttons on detected updates.
    #[default]
    Interactive,
    /// Silently auto-trims detected game updates based on known rules.
    AutoTrim,
    /// Passive mode: updates badge/status in GUI without showing toasts or auto-trimming.
    Passive,
}

impl WatchMode {
    /// Stable string form persisted in `gametrimmer.ini`.
    pub fn as_str(self) -> &'static str {
        match self {
            WatchMode::Interactive => "interactive",
            WatchMode::AutoTrim => "autotrim",
            WatchMode::Passive => "passive",
        }
    }

    /// Inverse of [`as_str`](Self::as_str). `None` for unknown values - callers fall back to default.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "interactive" => Some(WatchMode::Interactive),
            "autotrim" | "auto_trim" => Some(WatchMode::AutoTrim),
            "passive" => Some(WatchMode::Passive),
            _ => None,
        }
    }
}

/// The keep-list used when the database has no stored value (or an empty
/// one): the user's own language plus English are never flagged.
pub fn default_keep_languages() -> Vec<String> {
    vec!["uk".to_string(), "en".to_string()]
}

/// Parses a comma-separated string of language codes into a trimmed,
/// lowercased, order-preserving deduplicated list. An empty result falls
/// back to [`default_keep_languages`]: an empty keep-list would let the app
/// flag every language including the user's own, which is never intended.
fn parse_keep_languages(value: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let codes: Vec<String> = value
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .filter(|s| seen.insert(s.clone()))
        .collect();
    if codes.is_empty() {
        default_keep_languages()
    } else {
        codes
    }
}

/// Inverse of [`parse_keep_languages`] for storage.
fn serialize_keep_languages(codes: &[String]) -> String {
    codes.join(",")
}

/// Parses a comma-separated string of scanned-artifact category ids into a
/// trimmed, lowercased, order-preserving deduplicated list.
///
/// Unlike [`parse_keep_languages`], an empty result is *not* replaced with a
/// default here: an empty `enabled_categories` list is itself the
/// meaningful, valid state "every category is enabled" (see
/// [`Settings::enabled_categories`]). Falling back to some hardcoded
/// non-empty list would be wrong for a setting whose whole point is to be
/// empty by default.
fn parse_enabled_categories(value: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    value
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .filter(|s| seen.insert(s.clone()))
        .collect()
}

/// Inverse of [`parse_enabled_categories`] for storage.
fn serialize_enabled_categories(ids: &[String]) -> String {
    ids.join(",")
}

/// Parses a comma-separated string of excluded-library path keys into a
/// trimmed, lowercased, order-preserving deduplicated list.
///
/// Values are expected to already be normalized via
/// [`crate::providers::comparable_path`] by the time they reach here - the
/// UI computes the key from a [`std::path::Path`] before ever calling
/// [`Settings::excluded_libraries`]'s setter, and this crate has no business
/// re-deriving path semantics from a bare string. The trim/lowercase here is
/// only a defensive pass over a hand-edited ini, mirroring
/// [`parse_enabled_categories`].
///
/// Like [`parse_enabled_categories`] and unlike [`parse_keep_languages`], an
/// empty result stays empty: no exclusions is the ordinary state, not a
/// fallback to some default set of excluded libraries.
fn parse_excluded_libraries(value: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    value
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .filter(|s| seen.insert(s.clone()))
        .collect()
}

/// Inverse of [`parse_excluded_libraries`] for storage.
fn serialize_excluded_libraries(paths: &[String]) -> String {
    paths.join(",")
}

/// Inverse of [`bool_as_str`]. `None` for unknown values (e.g. written by a
/// future version) - callers fall back to the field's default.
fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Stable string form persisted in `gametrimmer.ini` for a plain `bool`
/// setting.
fn bool_as_str(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

/// All persisted settings, with defaults for anything missing from the
/// ini file. Grows one field per setting as the settings dialog gains
/// options (deletion method, keep-list languages, categories, app language,
/// theme, ...).
///
/// Not `Copy`: `keep_languages` is a `Vec<String>`. Call sites that used to
/// rely on `Copy` now clone explicitly where needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub delete_method: DeleteMethod,
    /// What the user picked for the UI language - see [`LanguagePreference`].
    /// Not a [`Lang`]: the stored answer has a third state ("follow Windows")
    /// that the rendered answer does not.
    pub app_language: LanguagePreference,
    /// Language keys (normalized: trimmed, lowercased, deduplicated) that
    /// the localization detector never flags for deletion. Always
    /// non-empty - see [`default_keep_languages`].
    pub keep_languages: Vec<String>,
    /// Whether the startup modal offering a UAC relaunch stays suppressed
    /// across restarts.
    ///
    /// This is what is left of the retired three-way `scan_routing` setting,
    /// and it is deliberately not a routing mode. Routing itself has one
    /// behavior now: the MFT index wherever it is both usable and faster, a
    /// directory walk everywhere else, decided per volume from the device's
    /// own seek penalty (see `mftscan::media_kind`). The two overrides that
    /// setting used to offer did not survive the question "who would want
    /// this":
    ///
    /// * "Prefer the MFT index" only ever bypassed the SSD speed heuristic,
    ///   so its single effect was making the scan ~40x slower on an SSD.
    /// * "Always walk folders" was the only *permanent* way to stop being
    ///   asked for Administrator rights on every launch - dismissing the
    ///   modal lasts one session. That is the decision worth keeping, and
    ///   this field is it, stated as what the user actually wanted rather
    ///   than as a file-enumeration strategy.
    ///
    /// An ini written before this field existed migrates on load: the old
    /// `scan_routing=force_walkdir` becomes `true`, both other modes
    /// `false`. See [`settings_from_values`].
    pub never_ask_elevation: bool,
    pub theme: Theme,
    /// Scanned-artifact category ids (normalized: trimmed, lowercased,
    /// deduplicated) the scan worker keeps findings for. **Empty means every
    /// category is enabled** - this is the default, and is also what an
    /// upgrade from a database written before this setting existed sees.
    /// An empty list is deliberately never treated as "nothing enabled":
    /// that would make every scan silently come back with zero findings,
    /// which is worse than any category being stuck on. Callers (the
    /// settings dialog) are responsible for never letting the *last*
    /// checked category be unchecked - see `ui::settings::scanning`.
    pub enabled_categories: Vec<String>,
    /// The profile the **currently displayed** findings follow. Orthogonal to
    /// [`Self::enabled_categories`]: that decides what is scanned, this
    /// decides what is checked among what was scanned. The main-screen picker
    /// reads and writes it, re-applying it to the tree on the spot, and any
    /// hand-edited checkbox drops it to [`SelectionProfile::Custom`] so the
    /// label stops claiming a policy the selection no longer follows.
    /// Registered library roots (see `game_libraries` in the database) the
    /// scan does not descend into. Keyed by path, normalized through
    /// [`crate::providers::comparable_path`] - not by `game_libraries.id`,
    /// which is not stable across a database rebuild.
    ///
    /// This is *exclude*, not *remove*: the row in `game_libraries` is left
    /// alone, so the library stays visible in Settings with its toggle off
    /// and survives a re-scan without coming back into the scanned set. A
    /// vendor-detected library that instead disappeared from the list on
    /// exclude would simply be Remove wearing a different label - Remove
    /// already exists for manual libraries and works that way on purpose
    /// (see `ui::settings::scanning::show_libraries`).
    ///
    /// Empty means nothing is excluded - the ordinary state, and the default.
    /// Unlike [`Self::enabled_categories`], there is no inverted "empty means
    /// all" convention here: every registered library is scanned unless its
    /// key is in this list, which is the plain reading of the field name.
    /// Callers (the settings dialog) are responsible for never letting the
    /// last included library be excluded - discovery already errors with
    /// `no_libraries_found` on an empty scan set, so excluding everything
    /// would leave a scan with nothing to do.
    pub excluded_libraries: Vec<String>,
    pub selection_profile: SelectionProfile,
    /// The profile a **fresh scan** pre-selects with.
    ///
    /// Deliberately separate from [`Self::selection_profile`]: the settings
    /// dialog edits only this one, so changing it can never silently
    /// re-check the tree the user is looking at. It takes effect the next
    /// time a scan finishes, which is also when the live profile is reset to
    /// match it.
    pub default_selection_profile: SelectionProfile,
    /// When the delete confirmation modal is shown - see [`ConfirmBehavior`].
    pub confirm_behavior: ConfirmBehavior,
    /// Whether the app writes a `gametrimmer.log` file next to the
    /// executable with diagnostics (errors and scan lifecycle events) - see
    /// the `logger` module in the `app` crate. Enabled by default so a useful
    /// diagnostic already exists when an unexpected scan result or failure
    /// needs investigating; users can switch it off at any time.
    pub logging_enabled: bool,
    /// Whether the user has ever started a scan. The only thing it drives is
    /// the first-run explanation, which occupies the empty tree area
    /// until then and never comes back afterwards.
    ///
    /// Set when a scan *starts*, not when one finishes: the explanation has
    /// done its job the moment the user acts on it, and a scan that finds
    /// nothing or fails would otherwise put the whole introduction back on
    /// screen as if the click had never happened.
    pub has_scanned: bool,
    /// Whether the user has accepted the liability disclaimer shown on the
    /// first-run screen. Until they have, the app starts no scan and deletes
    /// nothing - a disclaimer that can be scrolled past is not one.
    ///
    /// Separate from [`Self::has_scanned`] rather than folded into it: a
    /// database written by an earlier version has `has_scanned = true` and no
    /// acceptance on record, and that user has to be shown the disclaimer
    /// once, not locked out of a tool they were already using.
    pub disclaimer_accepted: bool,
    /// Whether background update monitoring is enabled.
    pub watch_enabled: bool,
    /// Whether background monitoring launches automatically on Windows boot.
    pub watch_autostart: bool,
    /// Operating mode for the background monitor companion daemon.
    pub watch_mode: WatchMode,
}

const DEFAULT_LOGGING_ENABLED: bool = true;
const DEFAULT_WATCH_ENABLED: bool = true;
const DEFAULT_WATCH_AUTOSTART: bool = false;

impl Default for Settings {
    fn default() -> Self {
        Self {
            delete_method: DeleteMethod::default(),
            app_language: LanguagePreference::default(),
            keep_languages: default_keep_languages(),
            never_ask_elevation: false,
            theme: Theme::default(),
            enabled_categories: Vec::new(),
            excluded_libraries: Vec::new(),
            selection_profile: SelectionProfile::default(),
            default_selection_profile: SelectionProfile::default(),
            confirm_behavior: ConfirmBehavior::default(),
            logging_enabled: DEFAULT_LOGGING_ENABLED,
            has_scanned: false,
            disclaimer_accepted: false,
            watch_enabled: DEFAULT_WATCH_ENABLED,
            watch_autostart: DEFAULT_WATCH_AUTOSTART,
            watch_mode: WatchMode::default(),
        }
    }
}

const DELETE_METHOD_KEY: &str = "delete_method";
const APP_LANGUAGE_KEY: &str = "app_language";
const KEEP_LANGUAGES_KEY: &str = "keep_languages";
/// Read-only leftover of the retired routing setting. Still listed in
/// [`SETTINGS_KEYS`] so [`parse_ini`] does not discard the line before
/// [`settings_from_values`] can migrate it into
/// [`Settings::never_ask_elevation`]; never written back, so the key decays
/// out of the ini on the first save.
const LEGACY_SCAN_ROUTING_KEY: &str = "scan_routing";
const NEVER_ASK_ELEVATION_KEY: &str = "never_ask_elevation";
const THEME_KEY: &str = "theme";
const ENABLED_CATEGORIES_KEY: &str = "enabled_categories";
const EXCLUDED_LIBRARIES_KEY: &str = "excluded_libraries";
const SELECTION_PROFILE_KEY: &str = "selection_profile";
const DEFAULT_SELECTION_PROFILE_KEY: &str = "default_selection_profile";
const CONFIRM_BEHAVIOR_KEY: &str = "confirm_behavior";
const LOGGING_ENABLED_KEY: &str = "logging_enabled";
const HAS_SCANNED_KEY: &str = "has_scanned";
const DISCLAIMER_ACCEPTED_KEY: &str = "disclaimer_accepted";
const WATCH_ENABLED_KEY: &str = "watch_enabled";
const WATCH_AUTOSTART_KEY: &str = "watch_autostart";
const WATCH_MODE_KEY: &str = "watch_mode";

const SETTINGS_KEYS: [&str; 17] = [
    DELETE_METHOD_KEY,
    APP_LANGUAGE_KEY,
    KEEP_LANGUAGES_KEY,
    LEGACY_SCAN_ROUTING_KEY,
    NEVER_ASK_ELEVATION_KEY,
    THEME_KEY,
    ENABLED_CATEGORIES_KEY,
    EXCLUDED_LIBRARIES_KEY,
    SELECTION_PROFILE_KEY,
    DEFAULT_SELECTION_PROFILE_KEY,
    CONFIRM_BEHAVIOR_KEY,
    LOGGING_ENABLED_KEY,
    HAS_SCANNED_KEY,
    DISCLAIMER_ACCEPTED_KEY,
    WATCH_ENABLED_KEY,
    WATCH_AUTOSTART_KEY,
    WATCH_MODE_KEY,
];

const INI_HEADER: &str = "; GameTrimmer user settings. Unknown keys are ignored.\n[settings]\n";

fn settings_from_values(values: &HashMap<String, String>) -> Settings {
    let value = |key: &str| values.get(key).map(String::as_str);
    Settings {
        delete_method: value(DELETE_METHOD_KEY)
            .and_then(DeleteMethod::parse)
            .unwrap_or_default(),
        app_language: value(APP_LANGUAGE_KEY)
            .and_then(LanguagePreference::parse)
            .unwrap_or_default(),
        keep_languages: value(KEEP_LANGUAGES_KEY)
            .map(parse_keep_languages)
            .unwrap_or_else(default_keep_languages),
        // An explicit value always wins; only a settings file predating this
        // field falls through to the retired routing mode it replaced. The
        // migration is one-way and lossy on purpose: `force_walkdir` was the
        // only mode carrying a decision ("stop asking me for Administrator
        // rights"), and the other two said nothing this field can express.
        never_ask_elevation: value(NEVER_ASK_ELEVATION_KEY)
            .and_then(parse_bool)
            .unwrap_or_else(|| {
                value(LEGACY_SCAN_ROUTING_KEY) == Some(LEGACY_ROUTING_FORCE_WALKDIR)
            }),
        theme: value(THEME_KEY).and_then(Theme::parse).unwrap_or_default(),
        enabled_categories: value(ENABLED_CATEGORIES_KEY)
            .map(parse_enabled_categories)
            .unwrap_or_default(),
        excluded_libraries: value(EXCLUDED_LIBRARIES_KEY)
            .map(parse_excluded_libraries)
            .unwrap_or_default(),
        selection_profile: value(SELECTION_PROFILE_KEY)
            .and_then(SelectionProfile::parse)
            .unwrap_or_default(),
        default_selection_profile: value(DEFAULT_SELECTION_PROFILE_KEY)
            .and_then(SelectionProfile::parse)
            .unwrap_or_default(),
        confirm_behavior: value(CONFIRM_BEHAVIOR_KEY)
            .and_then(ConfirmBehavior::parse)
            .unwrap_or_default(),
        logging_enabled: value(LOGGING_ENABLED_KEY)
            .and_then(parse_bool)
            .unwrap_or(DEFAULT_LOGGING_ENABLED),
        has_scanned: value(HAS_SCANNED_KEY)
            .and_then(parse_bool)
            .unwrap_or_default(),
        disclaimer_accepted: value(DISCLAIMER_ACCEPTED_KEY)
            .and_then(parse_bool)
            .unwrap_or_default(),
        watch_enabled: value(WATCH_ENABLED_KEY)
            .and_then(parse_bool)
            .unwrap_or(DEFAULT_WATCH_ENABLED),
        watch_autostart: value(WATCH_AUTOSTART_KEY)
            .and_then(parse_bool)
            .unwrap_or(DEFAULT_WATCH_AUTOSTART),
        watch_mode: value(WATCH_MODE_KEY)
            .and_then(WatchMode::parse)
            .unwrap_or_default(),
    }
}

/// The ini's key/value pairs, which is also how the diagnostic bundle
/// reports the parsed settings: `Settings` is not serde-backed, and this is
/// already the canonical text form of every field, so a `Serialize` derive
/// would be a second description of the same thing to keep in sync.
pub fn settings_values(settings: &Settings) -> [(&'static str, String); 16] {
    [
        (DELETE_METHOD_KEY, settings.delete_method.as_str().into()),
        (APP_LANGUAGE_KEY, settings.app_language.as_str().into()),
        (
            KEEP_LANGUAGES_KEY,
            serialize_keep_languages(&settings.keep_languages),
        ),
        (
            NEVER_ASK_ELEVATION_KEY,
            bool_as_str(settings.never_ask_elevation).into(),
        ),
        (THEME_KEY, settings.theme.as_str().into()),
        (
            ENABLED_CATEGORIES_KEY,
            serialize_enabled_categories(&settings.enabled_categories),
        ),
        (
            EXCLUDED_LIBRARIES_KEY,
            serialize_excluded_libraries(&settings.excluded_libraries),
        ),
        (
            SELECTION_PROFILE_KEY,
            settings.selection_profile.as_str().into(),
        ),
        (
            DEFAULT_SELECTION_PROFILE_KEY,
            settings.default_selection_profile.as_str().into(),
        ),
        (
            CONFIRM_BEHAVIOR_KEY,
            settings.confirm_behavior.as_str().into(),
        ),
        (
            LOGGING_ENABLED_KEY,
            bool_as_str(settings.logging_enabled).into(),
        ),
        (HAS_SCANNED_KEY, bool_as_str(settings.has_scanned).into()),
        (
            DISCLAIMER_ACCEPTED_KEY,
            bool_as_str(settings.disclaimer_accepted).into(),
        ),
        (
            WATCH_ENABLED_KEY,
            bool_as_str(settings.watch_enabled).into(),
        ),
        (
            WATCH_AUTOSTART_KEY,
            bool_as_str(settings.watch_autostart).into(),
        ),
        (WATCH_MODE_KEY, settings.watch_mode.as_str().into()),
    ]
}

/// Reads one raw value from the `settings` table.
fn read_value(conn: &Connection, key: &str) -> Result<Option<String>> {
    let value = conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
            row.get::<_, String>(0)
        })
        .optional()?;
    Ok(value)
}

/// Upserts one raw value into the `settings` table.
fn write_value(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}

/// Loads settings from the legacy database table. Production calls this only
/// when `gametrimmer.ini` is absent and immediately migrates the result.
pub fn load(conn: &Connection) -> Result<Settings> {
    let mut values = HashMap::new();
    for key in SETTINGS_KEYS {
        if let Some(value) = read_value(conn, key)? {
            values.insert(key.to_string(), value);
        }
    }
    Ok(settings_from_values(&values))
}

/// Persists every settings field.
///
/// All thirteen writes go in one transaction. Outside one, each `INSERT` is its
/// own implicit transaction and costs a WAL sync of its own - thirteen syncs per
/// flipped radio button, which on a USB flash drive is the difference between
/// instant and a visible pause (MT-I01, MT-N01). One transaction also makes the
/// write atomic: a pull mid-save can no longer leave half the settings updated.
pub fn save(conn: &Connection, settings: &Settings) -> Result<()> {
    // `unchecked_transaction` rather than `Connection::transaction`, which needs
    // `&mut Connection`: every caller here holds a shared borrow, and there is
    // no nested transaction to conflict with - this function only ever runs on
    // a freshly opened connection.
    let tx = conn.unchecked_transaction()?;
    write_values(&tx, settings)?;
    Ok(tx.commit()?)
}

fn write_values(conn: &Connection, settings: &Settings) -> Result<()> {
    for (key, value) in settings_values(settings) {
        write_value(conn, key, &value)?;
    }
    Ok(())
}

/// Loads `gametrimmer.ini`. The parser is deliberately forgiving: comments,
/// blank lines, unknown sections/keys and malformed lines are ignored, while
/// invalid known values fall back field-by-field through [`Settings::default`].
/// Invalid UTF-8 bytes are replaced rather than turning a damaged preference
/// file into a startup blocker.
pub fn load_file(path: &Path) -> Result<Settings> {
    let bytes = fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(settings_from_values(&parse_ini(&text)))
}

/// Loads the ini when it exists. Otherwise reads the legacy SQLite table (if
/// a connection is available), writes the result to the ini atomically, and
/// returns it. The database is never written by this migration.
pub fn load_file_or_migrate(path: &Path, legacy: Option<&Connection>) -> Result<Settings> {
    if path.exists() {
        return load_file(path);
    }

    let settings = match legacy {
        Some(conn) => load(conn)?,
        None => Settings::default(),
    };
    save_file(path, &settings)?;
    Ok(settings)
}

/// Persists every setting to `gametrimmer.ini` via a sibling temporary file
/// and an atomic replace. The complete snapshot is written on every change so
/// the file has one source of truth and never needs merge semantics.
pub fn save_file(path: &Path, settings: &Settings) -> Result<()> {
    let mut body = String::from(INI_HEADER);
    for (key, value) in settings_values(settings) {
        body.push_str(key);
        body.push('=');
        body.push_str(&value);
        body.push('\n');
    }

    crate::atomic_file::atomic_write_with_backup(path, body.as_bytes(), |_path, bytes| {
        let text = std::str::from_utf8(bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        if !text.contains("[settings]") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "settings section missing after write",
            ));
        }
        Ok(())
    })?;
    Ok(())
}

fn parse_ini(text: &str) -> HashMap<String, String> {
    let known: std::collections::HashSet<&str> = SETTINGS_KEYS.into_iter().collect();
    let mut values = HashMap::new();
    let mut in_settings = false;

    for (index, line) in text.lines().enumerate() {
        let line = if index == 0 {
            line.trim_start_matches('\u{feff}')
        } else {
            line
        };
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_settings = line[1..line.len() - 1]
                .trim()
                .eq_ignore_ascii_case("settings");
            continue;
        }
        if !in_settings {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if known.contains(key) {
            values.insert(key.to_string(), value.trim().to_string());
        }
    }
    values
}

#[cfg(test)]
fn temporary_path(path: &Path) -> PathBuf {
    let mut path = path.as_os_str().to_owned();
    path.push(".replace-tmp");
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_permanent_delete_on_empty_database() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        let settings = load(&conn).expect("load settings");
        assert_eq!(settings.delete_method, DeleteMethod::Permanent);
    }

    /// The default is "follow Windows", not English. What that resolves to
    /// is a separate question, answered by `resolve` and tested below - this
    /// one is about what an empty database means.
    #[test]
    fn defaults_to_following_the_system_language_on_empty_database() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        let settings = load(&conn).expect("load settings");
        assert_eq!(settings.app_language, LanguagePreference::System);
    }

    #[test]
    fn save_then_load_round_trips_every_method() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");

        for method in [DeleteMethod::RecycleBin, DeleteMethod::Permanent] {
            let settings = Settings {
                delete_method: method,
                ..Settings::default()
            };
            save(&conn, &settings).expect("save settings");
            let loaded = load(&conn).expect("load settings");
            assert_eq!(loaded.delete_method, method);
        }
    }

    #[test]
    fn save_then_load_round_trips_every_language_preference() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");

        for preference in [
            LanguagePreference::System,
            LanguagePreference::Fixed(Lang::En),
            LanguagePreference::Fixed(Lang::Uk),
        ] {
            let settings = Settings {
                app_language: preference,
                ..Settings::default()
            };
            save(&conn, &settings).expect("save settings");
            let loaded = load(&conn).expect("load settings");
            assert_eq!(loaded.app_language, preference);
        }
    }

    /// The compatibility clause. Versions before the System option wrote a
    /// bare "en"/"uk" under the same key, and that has to keep meaning an
    /// explicit choice - otherwise upgrading would quietly hand a user who
    /// picked English a Ukrainian interface because Windows says so.
    #[test]
    fn a_language_stored_by_an_older_version_stays_an_explicit_choice() {
        for (stored, expected) in [("en", Lang::En), ("uk", Lang::Uk)] {
            let conn = crate::db::open_in_memory().expect("open in-memory db");
            write_value(&conn, APP_LANGUAGE_KEY, stored).expect("write legacy value");

            let loaded = load(&conn).expect("load settings");

            assert_eq!(loaded.app_language, LanguagePreference::Fixed(expected));
            // The other language as the "system" answer, so a preference that
            // yielded to the OS would be visible rather than coincidentally
            // equal.
            let other = if expected == Lang::En {
                Lang::Uk
            } else {
                Lang::En
            };
            assert_eq!(loaded.app_language.resolve(other), expected);
        }
    }

    /// The whole policy, as plain logic: System defers to whatever the OS
    /// reported, a fixed choice never does.
    #[test]
    fn only_the_system_preference_yields_to_the_operating_system() {
        for system in [Lang::En, Lang::Uk] {
            assert_eq!(LanguagePreference::System.resolve(system), system);
            for fixed in [Lang::En, Lang::Uk] {
                assert_eq!(LanguagePreference::Fixed(fixed).resolve(system), fixed);
            }
        }
    }

    #[test]
    fn unknown_stored_value_falls_back_to_default() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('delete_method', 'quarantine')",
            [],
        )
        .expect("insert unknown value");

        let settings = load(&conn).expect("load settings");
        assert_eq!(
            settings.delete_method,
            DeleteMethod::Permanent,
            "a value written by a future version must not break loading"
        );
    }

    #[test]
    fn unknown_stored_language_falls_back_to_default() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('app_language', 'invalid language code')",
            [],
        )
        .expect("insert unknown value");

        let settings = load(&conn).expect("load settings");
        assert_eq!(
            settings.app_language,
            LanguagePreference::System,
            "a value written by a future version must not break loading"
        );
    }

    #[test]
    fn delete_method_round_trips_through_as_str_parse() {
        for method in [DeleteMethod::Permanent, DeleteMethod::RecycleBin] {
            assert_eq!(DeleteMethod::parse(method.as_str()), Some(method));
        }
        assert_eq!(DeleteMethod::parse("nonsense"), None);
    }

    #[test]
    fn lang_round_trips_through_as_str_parse() {
        for lang in [Lang::En, Lang::Uk] {
            assert_eq!(Lang::parse(lang.as_str()), Some(lang));
        }
        let custom = Lang::parse("pl").expect("parse custom pl");
        assert_eq!(custom.as_str(), "pl");
        assert_eq!(Lang::parse("invalid language with spaces"), None);
    }

    #[test]
    fn defaults_keep_languages_on_empty_database() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        let settings = load(&conn).expect("load settings");
        assert_eq!(settings.keep_languages, default_keep_languages());
    }

    #[test]
    fn save_then_load_round_trips_keep_languages() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        let settings = Settings {
            keep_languages: vec!["uk".to_string(), "en".to_string(), "pl".to_string()],
            ..Settings::default()
        };
        save(&conn, &settings).expect("save settings");
        let loaded = load(&conn).expect("load settings");
        assert_eq!(loaded.keep_languages, settings.keep_languages);
    }

    #[test]
    fn empty_string_keep_languages_falls_back_to_default() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('keep_languages', '')",
            [],
        )
        .expect("insert empty string");
        let settings = load(&conn).expect("load settings");
        assert_eq!(
            settings.keep_languages,
            default_keep_languages(),
            "an empty keep-list would flag the user's own language"
        );
    }

    #[test]
    fn keep_languages_normalizes_dedup_trims_lowercases() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('keep_languages', ' UK, en , EN, Uk, uk, fr ')",
            [],
        )
        .expect("insert messy keep-languages");
        let settings = load(&conn).expect("load settings");
        assert_eq!(
            settings.keep_languages,
            vec!["uk".to_string(), "en".to_string(), "fr".to_string()],
            "keep-languages should be deduplicated, trimmed, and lowercased"
        );
    }

    /// The prompt is offered by default: suppressing it is a decision the
    /// user has to have made, and a fresh install has made no decisions.
    #[test]
    fn defaults_to_asking_about_elevation_on_empty_database() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        let settings = load(&conn).expect("load settings");
        assert!(!settings.never_ask_elevation);
    }

    #[test]
    fn save_then_load_round_trips_both_elevation_answers() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        for never_ask in [true, false] {
            let settings = Settings {
                never_ask_elevation: never_ask,
                ..Settings::default()
            };
            save(&conn, &settings).expect("save settings");
            let loaded = load(&conn).expect("load settings");
            assert_eq!(loaded.never_ask_elevation, never_ask);
        }
    }

    #[test]
    fn unknown_elevation_value_falls_back_to_asking() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('never_ask_elevation', 'random')",
            [],
        )
        .expect("insert unknown value");
        let settings = load(&conn).expect("load settings");
        assert!(
            !settings.never_ask_elevation,
            "a value written by a future version must not break loading"
        );
    }

    /// The retired `scan_routing` setting had exactly one mode carrying a
    /// decision worth keeping: `force_walkdir` also meant "never offer me
    /// the UAC relaunch". Dropping the setting without carrying that over
    /// would start asking again, every launch, with no way to refuse -
    /// the modal's own dismissal lasts one session (`app::continue_without_elevation`).
    #[test]
    fn the_retired_force_walkdir_mode_migrates_into_never_asking() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('scan_routing', 'force_walkdir')",
            [],
        )
        .expect("insert legacy value");

        let settings = load(&conn).expect("load settings");

        assert!(
            settings.never_ask_elevation,
            "a user who had turned the MFT path off is being asked for admin again",
        );
    }

    /// The other two modes said nothing about elevation, so they must not
    /// silently suppress the prompt.
    #[test]
    fn the_other_retired_routing_modes_migrate_into_still_asking() {
        for legacy in ["auto", "force_mft"] {
            let conn = crate::db::open_in_memory().expect("open in-memory db");
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('scan_routing', ?1)",
                [legacy],
            )
            .expect("insert legacy value");

            let settings = load(&conn).expect("load settings");

            assert!(
                !settings.never_ask_elevation,
                "{legacy:?} silently suppressed the elevation prompt",
            );
        }
    }

    /// Migration is a fallback, not an override: once the new field has been
    /// written, a stale `scan_routing` line left in the file must not be able
    /// to flip it back.
    #[test]
    fn an_explicit_elevation_answer_outranks_the_legacy_routing_key() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('scan_routing', 'force_walkdir')",
            [],
        )
        .expect("insert legacy value");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('never_ask_elevation', 'false')",
            [],
        )
        .expect("insert explicit value");

        let settings = load(&conn).expect("load settings");

        assert!(!settings.never_ask_elevation);
    }

    #[test]
    fn defaults_to_system_theme_on_empty_database() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        let settings = load(&conn).expect("load settings");
        assert_eq!(settings.theme, Theme::System);
    }

    #[test]
    fn save_then_load_round_trips_every_theme() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");

        for theme in [Theme::System, Theme::Light, Theme::Dark] {
            let settings = Settings {
                theme,
                ..Settings::default()
            };
            save(&conn, &settings).expect("save settings");
            let loaded = load(&conn).expect("load settings");
            assert_eq!(loaded.theme, theme);
        }
    }

    #[test]
    fn unknown_stored_theme_falls_back_to_default() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('theme', 'sepia')",
            [],
        )
        .expect("insert unknown value");

        let settings = load(&conn).expect("load settings");
        assert_eq!(
            settings.theme,
            Theme::System,
            "a value written by a future version must not break loading"
        );
    }

    #[test]
    fn theme_round_trips_through_as_str_parse() {
        for theme in [Theme::System, Theme::Light, Theme::Dark] {
            assert_eq!(Theme::parse(theme.as_str()), Some(theme));
        }
        assert_eq!(Theme::parse("nonsense"), None);
    }

    #[test]
    fn defaults_to_all_categories_enabled_on_empty_database() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        let settings = load(&conn).expect("load settings");
        assert!(
            settings.enabled_categories.is_empty(),
            "an empty list means every category is enabled - see the field's doc comment"
        );
    }

    #[test]
    fn save_then_load_round_trips_enabled_categories() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        let settings = Settings {
            enabled_categories: vec!["redist".to_string(), "docs".to_string()],
            ..Settings::default()
        };
        save(&conn, &settings).expect("save settings");
        let loaded = load(&conn).expect("load settings");
        assert_eq!(loaded.enabled_categories, settings.enabled_categories);
    }

    #[test]
    fn empty_string_enabled_categories_stays_empty_meaning_all_enabled() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('enabled_categories', '')",
            [],
        )
        .expect("insert empty string");
        let settings = load(&conn).expect("load settings");
        assert!(
            settings.enabled_categories.is_empty(),
            "unlike keep_languages, an empty enabled_categories must stay empty, \
             not fall back to some hardcoded default list - empty means \"all\""
        );
    }

    #[test]
    fn enabled_categories_normalizes_dedup_trims_lowercases() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('enabled_categories', ' REDIST, docs , DOCS, redist ')",
            [],
        )
        .expect("insert messy enabled-categories");
        let settings = load(&conn).expect("load settings");
        assert_eq!(
            settings.enabled_categories,
            vec!["redist".to_string(), "docs".to_string()],
            "enabled-categories should be deduplicated, trimmed, and lowercased"
        );
    }

    #[test]
    fn defaults_to_no_excluded_libraries_on_empty_database() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        let settings = load(&conn).expect("load settings");
        assert!(
            settings.excluded_libraries.is_empty(),
            "nothing is excluded until the user says otherwise"
        );
    }

    #[test]
    fn save_then_load_round_trips_excluded_libraries() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        let settings = Settings {
            excluded_libraries: vec![r"h:\itch.io".to_string(), r"f:\steamlibrary".to_string()],
            ..Settings::default()
        };
        save(&conn, &settings).expect("save settings");
        let loaded = load(&conn).expect("load settings");
        assert_eq!(loaded.excluded_libraries, settings.excluded_libraries);
    }

    #[test]
    fn empty_string_excluded_libraries_stays_empty() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('excluded_libraries', '')",
            [],
        )
        .expect("insert empty string");
        let settings = load(&conn).expect("load settings");
        assert!(
            settings.excluded_libraries.is_empty(),
            "an empty excluded_libraries must stay empty, not fall back to some default list"
        );
    }

    #[test]
    fn excluded_libraries_normalizes_dedup_trims_lowercases() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            r"INSERT INTO settings (key, value) VALUES ('excluded_libraries', ' H:\ITCH.IO, f:\steamlibrary , F:\SteamLibrary, h:\itch.io ')",
            [],
        )
        .expect("insert messy excluded-libraries");
        let settings = load(&conn).expect("load settings");
        assert_eq!(
            settings.excluded_libraries,
            vec![r"h:\itch.io".to_string(), r"f:\steamlibrary".to_string()],
            "excluded-libraries should be deduplicated, trimmed, and lowercased"
        );
    }

    #[test]
    fn defaults_to_balanced_profile_on_empty_database() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        let settings = load(&conn).expect("load settings");
        assert_eq!(
            settings.selection_profile,
            SelectionProfile::Balanced,
            "Balanced is the default aggressiveness profile"
        );
    }

    #[test]
    fn save_then_load_round_trips_every_selection_profile() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        for profile in [
            SelectionProfile::Cautious,
            SelectionProfile::Balanced,
            SelectionProfile::Aggressive,
            SelectionProfile::Custom,
        ] {
            let settings = Settings {
                selection_profile: profile,
                ..Settings::default()
            };
            save(&conn, &settings).expect("save settings");
            let loaded = load(&conn).expect("load settings");
            assert_eq!(loaded.selection_profile, profile);
        }
    }

    #[test]
    fn selection_profile_round_trips_through_as_str_parse() {
        for profile in [
            SelectionProfile::Cautious,
            SelectionProfile::Balanced,
            SelectionProfile::Aggressive,
            SelectionProfile::Custom,
        ] {
            assert_eq!(SelectionProfile::parse(profile.as_str()), Some(profile));
        }
        assert_eq!(SelectionProfile::parse("nonsense"), None);
    }

    #[test]
    fn unknown_stored_selection_profile_falls_back_to_balanced() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('selection_profile', 'reckless')",
            [],
        )
        .expect("insert unknown value");
        let settings = load(&conn).expect("load settings");
        assert_eq!(
            settings.selection_profile,
            SelectionProfile::Balanced,
            "a value written by a future version must not break loading"
        );
    }

    #[test]
    fn save_then_load_round_trips_every_default_selection_profile() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        for profile in [
            SelectionProfile::Cautious,
            SelectionProfile::Balanced,
            SelectionProfile::Aggressive,
            SelectionProfile::Custom,
        ] {
            let settings = Settings {
                default_selection_profile: profile,
                ..Settings::default()
            };
            save(&conn, &settings).expect("save settings");
            let loaded = load(&conn).expect("load settings");
            assert_eq!(loaded.default_selection_profile, profile);
        }
    }

    /// The whole reason the field exists: editing the scan default in
    /// Settings must not disturb the profile the visible tree is following.
    #[test]
    fn the_scan_default_is_stored_apart_from_the_live_profile() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        let settings = Settings {
            selection_profile: SelectionProfile::Custom,
            default_selection_profile: SelectionProfile::Aggressive,
            ..Settings::default()
        };
        save(&conn, &settings).expect("save settings");

        let loaded = load(&conn).expect("load settings");
        assert_eq!(loaded.selection_profile, SelectionProfile::Custom);
        assert_eq!(
            loaded.default_selection_profile,
            SelectionProfile::Aggressive
        );
    }

    #[test]
    fn save_then_load_round_trips_every_confirm_behavior() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        for behavior in [ConfirmBehavior::Always, ConfirmBehavior::Never] {
            let settings = Settings {
                confirm_behavior: behavior,
                ..Settings::default()
            };
            save(&conn, &settings).expect("save settings");
            let loaded = load(&conn).expect("load settings");
            assert_eq!(loaded.confirm_behavior, behavior);
        }
    }

    #[test]
    fn confirm_behavior_round_trips_through_as_str_and_parse() {
        for behavior in [ConfirmBehavior::Always, ConfirmBehavior::Never] {
            assert_eq!(ConfirmBehavior::parse(behavior.as_str()), Some(behavior));
        }
        assert_eq!(ConfirmBehavior::parse("nonsense"), None);
    }

    /// A value written by a future version must not break loading, and the
    /// fallback has to be the *safe* end of this particular setting.
    #[test]
    fn an_unreadable_stored_confirm_behavior_falls_back_to_always_asking() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('confirm_behavior', 'sometimes')",
            [],
        )
        .expect("insert unknown value");

        let settings = load(&conn).expect("load settings");
        assert_eq!(settings.confirm_behavior, ConfirmBehavior::Always);
    }

    #[test]
    fn should_confirm_is_the_setting_itself() {
        assert!(ConfirmBehavior::Always.should_confirm());
        assert!(!ConfirmBehavior::Never.should_confirm());
    }

    /// The retired size threshold, which used to mean "ask only above 1 GB of
    /// batch total". A database that still holds it must land on *asking* -
    /// dropping a setting is not licence to start deleting without a prompt on
    /// someone's machine.
    #[test]
    fn the_retired_size_threshold_falls_back_to_always_asking() {
        assert_eq!(ConfirmBehavior::parse("only_above_1gb"), None);

        let conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('confirm_behavior', 'only_above_1gb')",
            [],
        )
        .expect("insert retired value");

        let settings = load(&conn).expect("load settings");
        assert_eq!(settings.confirm_behavior, ConfirmBehavior::Always);
    }

    #[test]
    fn defaults_to_enabled_logging_on_empty_database() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        let settings = load(&conn).expect("load settings");
        assert!(
            settings.logging_enabled,
            "diagnostic logging should be available from the first run"
        );
    }

    #[test]
    fn save_then_load_round_trips_logging_enabled() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");

        for enabled in [true, false] {
            let settings = Settings {
                logging_enabled: enabled,
                ..Settings::default()
            };
            save(&conn, &settings).expect("save settings");
            let loaded = load(&conn).expect("load settings");
            assert_eq!(loaded.logging_enabled, enabled);
        }
    }

    /// The first-run onboarding flag. The default matters as much as the round-trip: a
    /// database written before this key existed has to read as "never
    /// scanned" so an upgrading user is not shown a first-run introduction,
    /// nor - the other direction - denied it on a genuinely fresh install.
    #[test]
    fn save_then_load_round_trips_has_scanned_and_defaults_to_false() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        assert!(!load(&conn).expect("load settings").has_scanned);

        for scanned in [true, false] {
            let settings = Settings {
                has_scanned: scanned,
                ..Settings::default()
            };
            save(&conn, &settings).expect("save settings");
            let loaded = load(&conn).expect("load settings");
            assert_eq!(loaded.has_scanned, scanned);
        }
    }

    /// The gate in front of every scan and every deletion. The default is the
    /// load-bearing half: a database written before this key existed must
    /// read as "not accepted", so an upgrading user is asked once rather than
    /// silently treated as having agreed to something never shown to them.
    #[test]
    fn save_then_load_round_trips_disclaimer_acceptance_and_defaults_to_false() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        assert!(!load(&conn).expect("load settings").disclaimer_accepted);

        for accepted in [true, false] {
            let settings = Settings {
                disclaimer_accepted: accepted,
                ..Settings::default()
            };
            save(&conn, &settings).expect("save settings");
            let loaded = load(&conn).expect("load settings");
            assert_eq!(loaded.disclaimer_accepted, accepted);
        }
    }

    /// An older database has `has_scanned` set and no acceptance on record.
    /// The two must not be conflated: that user has to see the disclaimer
    /// once, and must not be locked out of a tool they were already using.
    #[test]
    fn a_database_from_before_the_disclaimer_reads_as_scanned_but_not_accepted() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        write_value(&conn, HAS_SCANNED_KEY, bool_as_str(true)).expect("write legacy key");

        let loaded = load(&conn).expect("load settings");

        assert!(loaded.has_scanned);
        assert!(!loaded.disclaimer_accepted);
    }

    #[test]
    fn unknown_stored_logging_value_falls_back_to_default() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('logging_enabled', 'maybe')",
            [],
        )
        .expect("insert unknown value");

        let settings = load(&conn).expect("load settings");
        assert!(
            settings.logging_enabled,
            "an invalid value should fall back to the current default"
        );
    }

    #[test]
    fn every_setting_and_variant_round_trips_through_ini() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("gametrimmer.ini");
        let mut cases = vec![Settings::default()];

        for method in [DeleteMethod::Permanent, DeleteMethod::RecycleBin] {
            cases.push(Settings {
                delete_method: method,
                ..Settings::default()
            });
        }
        for app_language in [
            LanguagePreference::System,
            LanguagePreference::Fixed(Lang::En),
            LanguagePreference::Fixed(Lang::Uk),
        ] {
            cases.push(Settings {
                app_language,
                ..Settings::default()
            });
        }
        for never_ask_elevation in [true, false] {
            cases.push(Settings {
                never_ask_elevation,
                ..Settings::default()
            });
        }
        for theme in [Theme::System, Theme::Light, Theme::Dark] {
            cases.push(Settings {
                theme,
                ..Settings::default()
            });
        }
        for selection_profile in [
            SelectionProfile::Cautious,
            SelectionProfile::Balanced,
            SelectionProfile::Aggressive,
            SelectionProfile::Custom,
        ] {
            cases.push(Settings {
                selection_profile,
                ..Settings::default()
            });
            cases.push(Settings {
                default_selection_profile: selection_profile,
                ..Settings::default()
            });
        }
        for confirm_behavior in [ConfirmBehavior::Always, ConfirmBehavior::Never] {
            cases.push(Settings {
                confirm_behavior,
                ..Settings::default()
            });
        }
        for watch_enabled in [true, false] {
            cases.push(Settings {
                watch_enabled,
                ..Settings::default()
            });
        }
        for watch_autostart in [true, false] {
            cases.push(Settings {
                watch_autostart,
                ..Settings::default()
            });
        }
        for watch_mode in [
            WatchMode::Interactive,
            WatchMode::AutoTrim,
            WatchMode::Passive,
        ] {
            cases.push(Settings {
                watch_mode,
                ..Settings::default()
            });
        }
        cases.push(Settings {
            keep_languages: vec!["uk".into(), "en".into(), "ja".into()],
            enabled_categories: vec!["docs".into(), "redist".into()],
            excluded_libraries: vec![r"h:\itch.io".into()],
            logging_enabled: true,
            has_scanned: true,
            disclaimer_accepted: true,
            watch_enabled: true,
            watch_autostart: true,
            watch_mode: WatchMode::AutoTrim,
            ..Settings::default()
        });

        for expected in cases {
            save_file(&path, &expected).expect("save ini");
            assert_eq!(load_file(&path).expect("load ini"), expected);
            assert!(
                !temporary_path(&path).exists(),
                "an atomic save must not leave its temporary sibling behind"
            );
        }
    }

    #[test]
    fn malformed_ini_falls_back_field_by_field_without_blocking_load() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("gametrimmer.ini");
        std::fs::write(
            &path,
            b"\xFF\xFE; damaged bytes are tolerated\n\
              [other]\n\
              theme=dark\n\
              [settings]\n\
              malformed line\n\
              future_key=future_value\n\
              delete_method=quarantine\n\
              app_language=invalid language with spaces\n\
              keep_languages=\n\
              scan_routing=teleport\n\
              never_ask_elevation=maybe\n\
              theme=sepia\n\
              enabled_categories=\n\
              excluded_libraries=\n\
              selection_profile=reckless\n\
              default_selection_profile=reckless\n\
              confirm_behavior=only_above_1gb\n\
              logging_enabled=maybe\n\
              has_scanned=maybe\n\
              disclaimer_accepted=maybe\n",
        )
        .expect("write malformed ini");

        let loaded = load_file(&path).expect("a damaged ini must remain non-fatal");
        assert_eq!(loaded, Settings::default());
        assert!(loaded.enabled_categories.is_empty());
        assert!(loaded.excluded_libraries.is_empty());
        assert_eq!(loaded.keep_languages, default_keep_languages());
    }

    #[test]
    fn missing_ini_migrates_legacy_database_once_then_ini_wins() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("gametrimmer.ini");
        let conn = crate::db::open_in_memory().expect("open legacy database");
        let legacy = Settings {
            delete_method: DeleteMethod::RecycleBin,
            app_language: LanguagePreference::Fixed(Lang::Uk),
            theme: Theme::Dark,
            logging_enabled: true,
            has_scanned: true,
            disclaimer_accepted: true,
            ..Settings::default()
        };
        save(&conn, &legacy).expect("seed legacy settings table");

        let migrated = load_file_or_migrate(&path, Some(&conn)).expect("migrate to ini");
        assert_eq!(migrated, legacy);
        assert_eq!(load_file(&path).expect("read migrated ini"), legacy);

        let changed_database = Settings {
            theme: Theme::Light,
            ..Settings::default()
        };
        save(&conn, &changed_database).expect("change legacy table after migration");
        assert_eq!(
            load_file_or_migrate(&path, Some(&conn)).expect("load existing ini"),
            legacy,
            "once the ini exists, the legacy database must never override it"
        );
    }

    #[test]
    fn a_fresh_install_materializes_default_ini_without_a_database() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("gametrimmer.ini");

        let loaded = load_file_or_migrate(&path, None).expect("create default ini");

        assert_eq!(loaded, Settings::default());
        assert_eq!(load_file(&path).expect("reload default ini"), loaded);
    }

    #[test]
    fn watch_mode_as_str_and_parse() {
        assert_eq!(WatchMode::Interactive.as_str(), "interactive");
        assert_eq!(WatchMode::AutoTrim.as_str(), "autotrim");
        assert_eq!(WatchMode::Passive.as_str(), "passive");

        assert_eq!(WatchMode::parse("interactive"), Some(WatchMode::Interactive));
        assert_eq!(WatchMode::parse("INTERACTIVE"), Some(WatchMode::Interactive));
        assert_eq!(WatchMode::parse("autotrim"), Some(WatchMode::AutoTrim));
        assert_eq!(WatchMode::parse("auto_trim"), Some(WatchMode::AutoTrim));
        assert_eq!(WatchMode::parse("AUTOTRIM"), Some(WatchMode::AutoTrim));
        assert_eq!(WatchMode::parse("passive"), Some(WatchMode::Passive));
        assert_eq!(WatchMode::parse("PASSIVE"), Some(WatchMode::Passive));
        assert_eq!(WatchMode::parse("  interactive  "), Some(WatchMode::Interactive));
        assert_eq!(WatchMode::parse("unknown_mode"), None);
        assert_eq!(WatchMode::parse(""), None);
    }

    #[test]
    fn defaults_for_watch_settings_on_empty_database() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        let settings = load(&conn).expect("load settings");
        assert_eq!(settings.watch_enabled, true);
        assert_eq!(settings.watch_autostart, false);
        assert_eq!(settings.watch_mode, WatchMode::Interactive);
    }

    #[test]
    fn save_then_load_round_trips_watch_settings() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");

        for enabled in [true, false] {
            for autostart in [true, false] {
                for mode in [WatchMode::Interactive, WatchMode::AutoTrim, WatchMode::Passive] {
                    let settings = Settings {
                        watch_enabled: enabled,
                        watch_autostart: autostart,
                        watch_mode: mode,
                        ..Settings::default()
                    };
                    save(&conn, &settings).expect("save settings");
                    let loaded = load(&conn).expect("load settings");
                    assert_eq!(loaded.watch_enabled, enabled);
                    assert_eq!(loaded.watch_autostart, autostart);
                    assert_eq!(loaded.watch_mode, mode);
                }
            }
        }
    }
}
