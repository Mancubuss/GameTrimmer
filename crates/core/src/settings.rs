//! Persisted user settings, stored in the `settings` key-value table of the
//! main database. Unknown keys and unparseable values fall back to defaults,
//! so a database written by a newer version never breaks an older one.

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
    /// Stable string form persisted into the `settings` table.
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
    /// Stable string form persisted into the `settings` table.
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
    /// Stable string form persisted into the `settings` table.
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

/// All persisted settings, with defaults for anything missing from the
/// database. Grows one field per setting as the settings dialog gains
/// options (deletion method, keep-list languages, categories, app language,
/// theme, ...).
///
/// Not `Copy`: `keep_languages` is a `Vec<String>`. Call sites that used to
/// rely on `Copy` now clone explicitly where needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub delete_method: DeleteMethod,
    pub app_language: Lang,
    /// Language keys (normalized: trimmed, lowercased, deduplicated) that
    /// the localization detector never flags for deletion. Always
    /// non-empty - see [`default_keep_languages`].
    pub keep_languages: Vec<String>,
    pub theme: Theme,
    /// Scanned-artifact category ids (normalized: trimmed, lowercased,
    /// deduplicated) the scan worker keeps findings for. **Empty means every
    /// category is enabled** - this is the default, and is also what an
    /// upgrade from a database written before this setting existed sees.
    /// An empty list is deliberately never treated as "nothing enabled":
    /// that would make every scan silently come back with zero findings,
    /// which is worse than any category being stuck on. Callers (the
    /// settings dialog) are responsible for never letting the *last*
    /// checked category be unchecked - see `ui::settings_dialog`.
    pub enabled_categories: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            delete_method: DeleteMethod::default(),
            app_language: Lang::default(),
            keep_languages: default_keep_languages(),
            theme: Theme::default(),
            enabled_categories: Vec::new(),
        }
    }
}

const DELETE_METHOD_KEY: &str = "delete_method";
const APP_LANGUAGE_KEY: &str = "app_language";
const KEEP_LANGUAGES_KEY: &str = "keep_languages";
const THEME_KEY: &str = "theme";
const ENABLED_CATEGORIES_KEY: &str = "enabled_categories";

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

/// Loads settings from the database. Missing rows and unrecognized values
/// both yield the field's default rather than an error.
pub fn load(conn: &Connection) -> Result<Settings> {
    let delete_method = read_value(conn, DELETE_METHOD_KEY)?
        .and_then(|value| DeleteMethod::parse(&value))
        .unwrap_or_default();
    let app_language = read_value(conn, APP_LANGUAGE_KEY)?
        .and_then(|value| Lang::parse(&value))
        .unwrap_or_default();
    let keep_languages = read_value(conn, KEEP_LANGUAGES_KEY)?
        .map(|value| parse_keep_languages(&value))
        .unwrap_or_else(default_keep_languages);
    let theme = read_value(conn, THEME_KEY)?
        .and_then(|value| Theme::parse(&value))
        .unwrap_or_default();
    let enabled_categories = read_value(conn, ENABLED_CATEGORIES_KEY)?
        .map(|value| parse_enabled_categories(&value))
        .unwrap_or_default();
    Ok(Settings {
        delete_method,
        app_language,
        keep_languages,
        theme,
        enabled_categories,
    })
}

/// Persists every settings field.
pub fn save(conn: &Connection, settings: &Settings) -> Result<()> {
    write_value(conn, DELETE_METHOD_KEY, settings.delete_method.as_str())?;
    write_value(conn, APP_LANGUAGE_KEY, settings.app_language.as_str())?;
    write_value(
        conn,
        KEEP_LANGUAGES_KEY,
        &serialize_keep_languages(&settings.keep_languages),
    )?;
    write_value(conn, THEME_KEY, settings.theme.as_str())?;
    write_value(
        conn,
        ENABLED_CATEGORIES_KEY,
        &serialize_enabled_categories(&settings.enabled_categories),
    )
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

    #[test]
    fn defaults_to_english_on_empty_database() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");
        let settings = load(&conn).expect("load settings");
        assert_eq!(settings.app_language, Lang::En);
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
    fn save_then_load_round_trips_every_language() {
        let conn = crate::db::open_in_memory().expect("open in-memory db");

        for lang in [Lang::En, Lang::Uk] {
            let settings = Settings {
                app_language: lang,
                ..Settings::default()
            };
            save(&conn, &settings).expect("save settings");
            let loaded = load(&conn).expect("load settings");
            assert_eq!(loaded.app_language, lang);
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
            Lang::En,
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
}
