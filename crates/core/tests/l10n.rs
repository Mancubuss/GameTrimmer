use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(serde::Deserialize)]
struct LocaleJson {
    id: String,
    name: String,
    native_name: String,
    strings: HashMap<String, String>,
}

/// The work list handed to translators for the eventual mass-translation
/// pass. Unlike every other locale, it must carry EXACTLY the same key set
/// as `en.json` - a missing key here is a string a translator will never
/// see, and a stale key is one they will translate for nothing.
const TEMPLATE_FILE_NAME: &str = "gametrimmer.template.json";

/// Not derived from the directory listing on purpose: a file silently
/// disappearing should fail loudly rather than just shrinking this number
/// along with it.
const EXPECTED_LOCALE_FILE_COUNT: usize = 30;

#[test]
fn all_locales_json_exist_and_match_keys() {
    let locales_dir_try1 = Path::new("../../locales");
    let locales_dir_try2 = Path::new("locales");
    let locales_dir = if locales_dir_try1.exists() {
        locales_dir_try1
    } else {
        locales_dir_try2
    };

    assert!(locales_dir.exists(), "locales directory must exist");

    // `locales/en.json` is the source of truth. Only English is kept
    // current until release (project decision); the other locales are
    // updated in one pass at the end, and the cascading fallback (an
    // untranslated key degrades to English) makes that safe. A hardcoded
    // expected key count used to break in all 30 locale files at once for
    // every single new UI string, so the count is derived from en.json
    // itself instead of pinned to a number that has to be hand-updated.
    let en_p = locales_dir.join("en.json");
    assert!(en_p.exists(), "locales/en.json must exist");
    let en_raw = fs::read_to_string(&en_p).expect("read en.json");
    let en_loc: LocaleJson = serde_json::from_str(&en_raw).expect("parse en.json");
    assert_eq!(en_loc.id, "en");
    assert!(
        !en_loc.strings.is_empty(),
        "en.json must have at least one string"
    );

    let entries = fs::read_dir(locales_dir).expect("read locales dir");
    let mut checked_count = 0;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        checked_count += 1;

        let raw =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {file_name}: {e}"));
        let loc: LocaleJson = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("failed to parse {file_name}: {e}"));

        assert!(!loc.id.is_empty(), "{file_name} missing 'id'");
        assert!(!loc.name.is_empty(), "{file_name} missing 'name'");
        assert!(
            !loc.native_name.is_empty(),
            "{file_name} missing 'native_name'"
        );

        // Every key this file defines must exist in en.json. This is what
        // actually catches this class of bug: a typo'd key, or a key that
        // was renamed/removed in en.json but never cleaned up here, is a
        // failure regardless of which file it is in - including en.json
        // and the template, where it trivially holds.
        for key in loc.strings.keys() {
            assert!(
                en_loc.strings.contains_key(key),
                "{file_name} has key '{key}' that does not exist in en.json"
            );
            assert!(
                !loc.strings[key].is_empty(),
                "{file_name} has empty string for key '{key}'"
            );
        }

        if file_name == TEMPLATE_FILE_NAME {
            // The template must be a reliable work list: every en.json key
            // has to be present so no string is left untranslatable.
            for key in en_loc.strings.keys() {
                assert!(
                    loc.strings.contains_key(key),
                    "{file_name} is missing key '{key}' present in en.json"
                );
            }
        }
        // Every other locale (en.json included, trivially) is allowed to be
        // a SUBSET of en.json's keys: a missing key is not a failure here,
        // it just falls back to English at runtime. Only an unknown key
        // (checked above) is a bug worth failing the build over.
    }

    assert_eq!(
        checked_count, EXPECTED_LOCALE_FILE_COUNT,
        "Expected {EXPECTED_LOCALE_FILE_COUNT} locale JSON files to be checked, found {checked_count}"
    );
}

/// `i18n::Reported::new(lang, message)` renders `message` twice - once with
/// `Lang::En` for the log, once with the caller's own `lang` for the window
/// (see the doc comment on `Reported::new` in `crates/app/src/i18n/messages.rs`).
/// A callback shaped `|_l| ...` receives that second, real language and
/// throws it away, returning a fixed literal instead - so the window shows
/// the same text no matter what language is selected. Static JSON-diffing
/// (the test above) cannot see this: the string never reaches a locale file,
/// it is hardcoded straight into the Rust call site.
///
/// This exact bug shipped three times in `crates/app/src/worker/delete.rs`
/// (fixed alongside this test - see `i18n::save_backup_zip_failed`,
/// `i18n::intro_stub_unsupported_skip`, `i18n::intro_stub_write_failed`).
/// Grepping the app sources for the shape is cheap and, as of this test,
/// matches nothing: every `Reported::new` call site in the app actually uses
/// the language it receives. If a future call site has a genuine reason to
/// ignore it, allow-list it explicitly below with a comment explaining why,
/// rather than deleting this test.
#[test]
fn reported_messages_use_the_language_they_receive() {
    use regex::Regex;
    use walkdir::WalkDir;

    let app_src_try1 = Path::new("../app/src");
    let app_src_try2 = Path::new("crates/app/src");
    let app_src = if app_src_try1.exists() {
        app_src_try1
    } else {
        app_src_try2
    };
    assert!(app_src.exists(), "crates/app/src directory must exist");

    // Deliberately not anchored to the literal identifier `lang`: the point
    // is the discarded closure parameter (`|_l|`), not the name of whatever
    // is passed as the first argument.
    let offender_re =
        Regex::new(r"Reported::new\s*\([^,]+,\s*(move\s+)?\|\s*_l\s*\|").expect("valid regex");

    // Allow-list: call sites where `Reported::new(..., |_l| ...)` is
    // intentional, with the reason stated inline. Empty today - see the
    // doc comment above.
    let allow_listed: &[(&str, u32)] = &[];

    let mut offenders = Vec::new();
    for entry in WalkDir::new(app_src)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("rs"))
    {
        let path = entry.path();
        let content = fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        let path_str = path.to_string_lossy().replace('\\', "/");
        for (idx, line) in content.lines().enumerate() {
            if !offender_re.is_match(line) {
                continue;
            }
            let line_no = (idx + 1) as u32;
            if allow_listed
                .iter()
                .any(|(f, l)| path_str.ends_with(*f) && *l == line_no)
            {
                continue;
            }
            offenders.push(format!("{}:{line_no}: {}", path.display(), line.trim()));
        }
    }

    assert!(
        offenders.is_empty(),
        "found Reported::new(<lang>, |_l| ...) - the callback receives the \
         interface language and then ignores it, so the window shows the \
         same text no matter what language is selected. Use the received \
         language instead (see e.g. `i18n::delete_failed`,\
         `i18n::db_update_after_delete_failed`), or if a specific call site \
         has a real reason to ignore it, add it to `allow_listed` in this \
         test with a comment explaining why:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn custom_language_tag_parsing() {
    use gametrimmer_core::settings::{Lang, LanguagePreference};

    assert_eq!(Lang::parse("en"), Some(Lang::En));
    assert_eq!(Lang::parse("uk"), Some(Lang::Uk));

    let pl = Lang::parse("pl").expect("parse pl");
    assert_eq!(pl.as_str(), "pl");

    let de = Lang::parse("de").expect("parse de");
    assert_eq!(de.as_str(), "de");

    let pref = LanguagePreference::parse("pl").expect("parse pl pref");
    assert_eq!(pref.as_str(), "pl");
    assert_eq!(pref.resolve(Lang::En), pl);
}
