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

    let en_p = locales_dir.join("en.json");
    assert!(en_p.exists(), "locales/en.json must exist");
    let en_raw = fs::read_to_string(&en_p).expect("read en.json");
    let en_loc: LocaleJson = serde_json::from_str(&en_raw).expect("parse en.json");
    assert_eq!(en_loc.id, "en");
    assert_eq!(en_loc.strings.len(), 254, "en.json should have 254 strings");

    let entries = fs::read_dir(locales_dir).expect("read locales dir");
    let mut checked_count = 0;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            let raw = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("failed to read {file_name}: {e}"));
            let loc: LocaleJson = serde_json::from_str(&raw)
                .unwrap_or_else(|e| panic!("failed to parse {file_name}: {e}"));

            assert!(!loc.id.is_empty(), "{file_name} missing 'id'");
            assert!(!loc.name.is_empty(), "{file_name} missing 'name'");
            assert!(
                !loc.native_name.is_empty(),
                "{file_name} missing 'native_name'"
            );
            assert_eq!(
                loc.strings.len(),
                254,
                "{file_name} has {} strings, expected 254",
                loc.strings.len()
            );

            for key in en_loc.strings.keys() {
                assert!(
                    loc.strings.contains_key(key),
                    "{file_name} is missing key '{key}' present in en.json"
                );
                assert!(
                    !loc.strings[key].is_empty(),
                    "{file_name} has empty string for key '{key}'"
                );
            }
            checked_count += 1;
        }
    }

    assert_eq!(
        checked_count, 30,
        "Expected 30 locale JSON files to be checked"
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
