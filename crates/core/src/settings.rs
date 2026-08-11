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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    /// English UI text.
    #[default]
    En,
    /// Ukrainian UI text.
    Uk,
}

impl Lang {
    /// Stable string form persisted in `gametrimmer.ini`.
    pub fn as_str(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Uk => "uk",
        }
    }

    /// Inverse of [`as_str`](Self::as_str). `None` for unknown values (e.g.
    /// written by a future version) - callers fall back to the default.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "en" => Some(Lang::En),
            "uk" => Some(Lang::Uk),
            _ => None,
        }
    }
}

/// What the user picked for the UI language: a specific one, or "whatever
/// Windows is set to".
///
/// A separate type from [`Lang`] on purpose, and the same shape [`Theme`]
/// already uses for the light/dark question. `Lang` answers "which language
/// is being rendered right now" and has to stay a plain two-valued answer -
/// every `i18n::strings` call takes one. This answers "what did the user
/// choose", which has a third state that is not a language at all. Folding
/// the two together would mean threading "...unless it is System" through
/// every call site that only ever wanted to look up a string.
///
/// Resolved once at startup rather than per call - see [`Self::resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LanguagePreference {
    /// Follow the OS UI language, falling back to [`Lang::En`] when Windows
    /// prefers something this app does not speak.
    ///
    /// The default, and the reason it is: a Windows set to Ukrainian used to
    /// get an English interface until the user found the switch. Defaulting
    /// to the machine's own language is not a preference this app is entitled
    /// to have an opinion about.
    #[default]
    System,
    /// An explicit choice, which never yields to the OS.
    Fixed(Lang),
}

impl LanguagePreference {
    /// Stable string form persisted in `gametrimmer.ini`.
    ///
    /// Shares the `app_language` key with the plain [`Lang`] values written
    /// by earlier versions: `"en"` and `"uk"` still parse, and still mean an
    /// explicit choice, so upgrading never silently overrides a language the
    /// user picked by hand.
    pub fn as_str(self) -> &'static str {
        match self {
            LanguagePreference::System => "system",
            LanguagePreference::Fixed(lang) => lang.as_str(),
        }
    }

    /// Inverse of [`as_str`](Self::as_str). `None` for unknown values (e.g.
    /// written by a future version) - callers fall back to the default.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(LanguagePreference::System),
            other => Lang::parse(other).map(LanguagePreference::Fixed),
        }
    }

    /// The language to actually render in, given what the OS reports.
    ///
    /// `system` is passed in rather than detected here: this crate has no
    /// business calling Win32, and a pure function is what lets the whole
    /// policy be tested without a machine set to the right locale.
    pub fn resolve(self, system: Lang) -> Lang {
        match self {
            LanguagePreference::System => system,
            LanguagePreference::Fixed(lang) => lang,
        }
    }
}

/// How game-library scanning chooses its file-enumeration path.
///
/// `Auto` is the default: HDD volumes use the NTFS MFT index, SSD volumes
/// use a directory walk (measured ~40x faster on SSD). Forcing a mode only
/// overrides that speed heuristic - the hard correctness gates (elevation,
/// lettered local volume, no canonical mismatch, volume available) still
/// apply to `ForceMft` exactly as they do to `Auto`; only `ForceWalkdir`
/// skips the MFT path (and its volume probing) entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScanRouting {
    /// Automatically choose between MFT index and directory walk based on
    /// volume type.
    #[default]
    Auto,
    /// Prefer the MFT index wherever it is usable, including on SSD/NVMe
    /// where a directory walk would normally be faster.
    ///
    /// Not a guarantee, despite the `Force` name: without elevation, on a
    /// volume with no drive letter, on a network path, or when the canonical
    /// path check fails, the scan still falls back to a directory walk. The
    /// user-facing label says "prefer", not "always", for that reason.
    ForceMft,
    /// Always use directory walking. Unlike [`Self::ForceMft`] this one has
    /// no fallback, so it really is unconditional.
    ForceWalkdir,
}

impl ScanRouting {
    /// Stable string form persisted in `gametrimmer.ini`.
    pub fn as_str(self) -> &'static str {
        match self {
            ScanRouting::Auto => "auto",
            ScanRouting::ForceMft => "force_mft",
            ScanRouting::ForceWalkdir => "force_walkdir",
        }
    }

    /// Inverse of [`as_str`](Self::as_str). `None` for unknown values (e.g.
    /// written by a future version) - callers fall back to the default.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(ScanRouting::Auto),
            "force_mft" => Some(ScanRouting::ForceMft),
            "force_walkdir" => Some(ScanRouting::ForceWalkdir),
            _ => None,
        }
    }
}

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
    /// How scanning chooses between the MFT index and a directory walk per
    /// game root - see [`ScanRouting`].
    pub scan_routing: ScanRouting,
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
}

const DEFAULT_LOGGING_ENABLED: bool = true;

impl Default for Settings {
    fn default() -> Self {
        Self {
            delete_method: DeleteMethod::default(),
            app_language: LanguagePreference::default(),
            keep_languages: default_keep_languages(),
            scan_routing: ScanRouting::default(),
            theme: Theme::default(),
            enabled_categories: Vec::new(),
            selection_profile: SelectionProfile::default(),
            default_selection_profile: SelectionProfile::default(),
            confirm_behavior: ConfirmBehavior::default(),
            logging_enabled: DEFAULT_LOGGING_ENABLED,
            has_scanned: false,
            disclaimer_accepted: false,
        }
    }
}

const DELETE_METHOD_KEY: &str = "delete_method";
const APP_LANGUAGE_KEY: &str = "app_language";
const KEEP_LANGUAGES_KEY: &str = "keep_languages";
const SCAN_ROUTING_KEY: &str = "scan_routing";
const THEME_KEY: &str = "theme";
const ENABLED_CATEGORIES_KEY: &str = "enabled_categories";
const SELECTION_PROFILE_KEY: &str = "selection_profile";
const DEFAULT_SELECTION_PROFILE_KEY: &str = "default_selection_profile";
const CONFIRM_BEHAVIOR_KEY: &str = "confirm_behavior";
const LOGGING_ENABLED_KEY: &str = "logging_enabled";
const HAS_SCANNED_KEY: &str = "has_scanned";
const DISCLAIMER_ACCEPTED_KEY: &str = "disclaimer_accepted";

const SETTINGS_KEYS: [&str; 12] = [
    DELETE_METHOD_KEY,
    APP_LANGUAGE_KEY,
    KEEP_LANGUAGES_KEY,
    SCAN_ROUTING_KEY,
    THEME_KEY,
    ENABLED_CATEGORIES_KEY,
    SELECTION_PROFILE_KEY,
    DEFAULT_SELECTION_PROFILE_KEY,
    CONFIRM_BEHAVIOR_KEY,
    LOGGING_ENABLED_KEY,
    HAS_SCANNED_KEY,
    DISCLAIMER_ACCEPTED_KEY,
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
        scan_routing: value(SCAN_ROUTING_KEY)
            .and_then(ScanRouting::parse)
            .unwrap_or_default(),
        theme: value(THEME_KEY).and_then(Theme::parse).unwrap_or_default(),
        enabled_categories: value(ENABLED_CATEGORIES_KEY)
            .map(parse_enabled_categories)
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
    }
}

fn settings_values(settings: &Settings) -> [(&'static str, String); 12] {
    [
        (DELETE_METHOD_KEY, settings.delete_method.as_str().into()),
        (APP_LANGUAGE_KEY, settings.app_language.as_str().into()),
        (
            KEEP_LANGUAGES_KEY,
            serialize_keep_languages(&settings.keep_languages),
        ),
        (SCAN_ROUTING_KEY, settings.scan_routing.as_str().into()),
        (THEME_KEY, settings.theme.as_str().into()),
        (
            ENABLED_CATEGORIES_KEY,
            serialize_enabled_categories(&settings.enabled_categories),
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
/// All twelve writes go in one transaction. Outside one, each `INSERT` is its
/// own implicit transaction and costs a WAL sync of its own - twelve syncs per
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
            "INSERT INTO settings (key, value) VALUES ('app_language', 'fr')",
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
        assert_eq!(Lang::parse("nonsense"), None);
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

    #[test]
    fn defaults_to_auto_scan_routing_on_empty_database() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        let settings = load(&conn).expect("load settings");
        assert_eq!(settings.scan_routing, ScanRouting::Auto);
    }

    #[test]
    fn scan_routing_round_trips_through_as_str_parse() {
        for routing in [
            ScanRouting::Auto,
            ScanRouting::ForceMft,
            ScanRouting::ForceWalkdir,
        ] {
            assert_eq!(ScanRouting::parse(routing.as_str()), Some(routing));
        }
        assert_eq!(ScanRouting::parse("nonsense"), None);
    }

    #[test]
    fn save_then_load_round_trips_every_scan_routing_variant() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        for routing in [
            ScanRouting::Auto,
            ScanRouting::ForceMft,
            ScanRouting::ForceWalkdir,
        ] {
            let settings = Settings {
                scan_routing: routing,
                ..Settings::default()
            };
            save(&conn, &settings).expect("save settings");
            let loaded = load(&conn).expect("load settings");
            assert_eq!(loaded.scan_routing, routing);
        }
    }

    #[test]
    fn unknown_scan_routing_value_falls_back_to_auto() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('scan_routing', 'random')",
            [],
        )
        .expect("insert unknown value");
        let settings = load(&conn).expect("load settings");
        assert_eq!(
            settings.scan_routing,
            ScanRouting::Auto,
            "a value written by a future version must not break loading"
        );
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
        for scan_routing in [
            ScanRouting::Auto,
            ScanRouting::ForceMft,
            ScanRouting::ForceWalkdir,
        ] {
            cases.push(Settings {
                scan_routing,
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
        cases.push(Settings {
            keep_languages: vec!["uk".into(), "en".into(), "ja".into()],
            enabled_categories: vec!["docs".into(), "redist".into()],
            logging_enabled: true,
            has_scanned: true,
            disclaimer_accepted: true,
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
              app_language=fr\n\
              keep_languages=\n\
              scan_routing=teleport\n\
              theme=sepia\n\
              enabled_categories=\n\
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
}
