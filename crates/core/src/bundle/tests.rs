use std::io::Read;
use std::path::PathBuf;

use super::*;

/// A throwaway install: a real database with one library, one game and one
/// finding, plus the ini/rules/log files that sit beside it. Returns the
/// directory so the caller keeps it alive - dropping it deletes everything.
fn fixture() -> (tempfile::TempDir, BundleInput) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db_path = dir.path().join("gametrimmer.db");
    let conn = crate::db::open(&db_path).expect("open db");

    let scan_id = crate::db::begin_scan(&conn, "complete").expect("begin scan");
    conn.execute(
        "INSERT INTO scan_state (singleton, active_scan_id) VALUES (1, ?1)
         ON CONFLICT(singleton) DO UPDATE SET active_scan_id = excluded.active_scan_id",
        [scan_id],
    )
    .expect("activate scan");
    conn.execute(
        "INSERT INTO game_libraries (vendor, path) VALUES ('steam', ?1)",
        [r"D:\SteamLibrary"],
    )
    .expect("insert library");
    conn.execute(
        "INSERT INTO games (scan_id, library_id, name, install_dir, files, bytes, scan_route)
         VALUES (?1, 1, 'Half-Life 17', ?2, 2, 4096, 'walkdir:SsdVolume')",
        rusqlite::params![scan_id, r"D:\SteamLibrary\common\HL17"],
    )
    .expect("insert game");
    conn.execute(
        "INSERT INTO files (id, scan_id, game_id, rel_path, size, size_on_disk)
         VALUES (1, ?1, 1, 'data/loc_de.pak', 1024, 4096)",
        [scan_id],
    )
    .expect("insert file");
    conn.execute(
        "INSERT INTO findings (file_id, category, rule_id, confidence, provenance)
         VALUES (1, 'loc', 'german voice pack', 90, 'builtin')",
        [],
    )
    .expect("insert finding");
    drop(conn);

    let settings_path = dir.path().join("gametrimmer.ini");
    crate::settings::save_file(&settings_path, &crate::settings::Settings::default())
        .expect("write ini");
    let rules_path = dir.path().join("rules.json");
    std::fs::write(&rules_path, crate::rules::BUILTIN_RULES_JSON).expect("write rules");
    let log_path = dir.path().join("gametrimmer.log");
    std::fs::write(&log_path, "=== session start ===\nScan started\n").expect("write log");

    let input = BundleInput {
        db_path,
        settings_path,
        rules_path,
        log_path,
        app_version: "1.0.0-test".to_string(),
        elevated: false,
        // Not this machine's real profile: an assertion about the account
        // name has to be about a name the test controls.
        user_profile: Some(r"C:\Users\testaccount".to_string()),
        options: BundleOptions::default(),
    };
    (dir, input)
}

fn build(input: &BundleInput) -> Bundle {
    super::build(input, &mut |_, _, _| true)
        .expect("build bundle")
        .expect("not cancelled")
}

fn entry_names(bytes: &[u8]) -> Vec<String> {
    let archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("open archive");
    archive.file_names().map(str::to_string).collect()
}

fn entry(bytes: &[u8], name: &str) -> String {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("open archive");
    let mut file = archive.by_name(name).expect("entry present");
    let mut out = String::new();
    file.read_to_string(&mut out).expect("read entry");
    out
}

/// The manifest's declared section list has to be exactly the archive's own
/// listing, so someone who does not trust it can check with `unzip -l`.
#[test]
fn the_manifest_declares_exactly_what_the_archive_holds() {
    let (_dir, input) = fixture();
    let bundle = build(&input);

    let manifest: serde_json::Value =
        serde_json::from_str(&entry(&bundle.bytes, "manifest.json")).expect("parse manifest");
    let declared: Vec<String> = manifest["sections_included"]
        .as_array()
        .expect("sections_included is an array")
        .iter()
        .map(|value| value.as_str().expect("section name").to_string())
        .collect();

    let mut actual = entry_names(&bundle.bytes);
    let mut declared_sorted = declared.clone();
    actual.sort();
    declared_sorted.sort();
    assert_eq!(actual, declared_sorted);
    assert_eq!(manifest["redaction_applied"], serde_json::json!(true));
    assert_eq!(
        manifest["schema_version"],
        serde_json::json!(BUNDLE_SCHEMA_VERSION)
    );
}

/// An excluded section is an absent file, never an empty one - otherwise
/// the listing stops being a truthful statement of what was included.
#[test]
fn an_excluded_section_is_absent_rather_than_empty() {
    let (_dir, mut input) = fixture();

    let without = build(&input);
    assert!(!entry_names(&without.bytes).contains(&"operations_detail.json".to_string()));

    input.options.include_operations_detail = true;
    let with = build(&input);
    assert!(entry_names(&with.bytes).contains(&"operations_detail.json".to_string()));
}

/// The rule with no opt-out. Asserted over the *decompressed text of every
/// entry* rather than one section, because the pass exists precisely to
/// catch the places a field-level redaction would not think to look.
#[test]
fn the_account_name_appears_in_no_entry_of_the_archive() {
    let (_dir, mut input) = fixture();
    input.options.include_game_titles = true;
    input.options.include_operations_detail = true;
    let bundle = build(&input);

    for name in entry_names(&bundle.bytes) {
        let body = entry(&bundle.bytes, &name);
        assert!(
            !body.to_ascii_lowercase().contains("testaccount"),
            "{name} carries the account name",
        );
    }
}

/// A library of a few dozen titles is close to a fingerprint, so the
/// default ships slots. Opting in is what makes the real names appear.
#[test]
fn game_titles_are_slots_until_the_user_opts_in() {
    let (_dir, mut input) = fixture();

    let anonymous = build(&input);
    let games = entry(&anonymous.bytes, "games.json");
    assert!(!games.contains("Half-Life 17"), "{games}");
    assert!(games.contains("Game 1"), "{games}");

    input.options.include_game_titles = true;
    let named = build(&input);
    assert!(entry(&named.bytes, "games.json").contains("Half-Life 17"));
}

/// Library roots become stable tokens, and the tokens are what a reader
/// uses to see that two paths share a root.
#[test]
fn library_paths_are_tokenized_everywhere() {
    let (_dir, input) = fixture();
    let bundle = build(&input);

    let games = entry(&bundle.bytes, "games.json");
    assert!(!games.contains("SteamLibrary"), "{games}");
    assert!(games.contains("<LIBRARY_1>"), "{games}");
}

/// Cancelling must leave nothing at all - not a partial archive, and not a
/// plausible-looking file on disk. The archive exists only in memory until
/// `write`, so a cancelled build has nothing to clean up.
#[test]
fn cancelling_produces_no_bundle_at_all() {
    let (_dir, input) = fixture();

    let mut seen = 0usize;
    let outcome = super::build(&input, &mut |_, index, _| {
        seen = index;
        // Stop at the third section, well after the first has been built.
        index < 2
    })
    .expect("build should not error on cancel");

    assert!(outcome.is_none(), "a cancelled build must yield no bundle");
    assert_eq!(seen, 2);
}

/// The written file has to be a real archive that opens without
/// GameTrimmer installed - which is what `write`'s validate closure checks
/// on the bytes it reads back, not on the ones it was handed.
#[test]
fn a_written_bundle_reopens_as_an_archive() {
    let (dir, input) = fixture();
    let bundle = build(&input);
    let target = dir.path().join("bundle.zip");

    write(&target, &bundle.bytes).expect("write bundle");

    let written = std::fs::read(&target).expect("read back");
    assert_eq!(written, bundle.bytes);
    assert!(entry_names(&written).contains(&"summary.txt".to_string()));
}

/// The preview must be the file, not a description of it: whatever the
/// user read before pressing the button is what lands in the archive.
///
/// Every line but one, and the exception is the point rather than a
/// loophole: `generated at` is stamped when each rendering happens, so the
/// preview honestly carries the moment it was previewed and the archive the
/// moment it was written. Comparing those two would assert that no second
/// ticks between the click and the write - a test that passes on a fast
/// machine and fails on a slow one, which is what it did.
#[test]
fn the_preview_is_the_summary_in_the_archive_line_for_line() {
    let (_dir, input) = fixture();

    let previewed = summary(&input).expect("render preview");
    let bundle = build(&input);

    assert_eq!(
        without_generated_at(&previewed),
        without_generated_at(&entry(&bundle.bytes, "summary.txt")),
    );
    assert_eq!(
        without_generated_at(&previewed),
        without_generated_at(&bundle.summary),
    );
    // The excluded line still has to be there, or the exclusion above would
    // quietly cover its disappearance.
    assert!(previewed.contains("generated at:"), "{previewed}");
}

/// Everything except the wall-clock stamp - see the caller for why that one
/// line cannot be compared.
fn without_generated_at(summary: &str) -> String {
    summary
        .lines()
        .filter(|line| !line.trim_start().starts_with("generated at:"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The preview has to change when a toggle does, or it is not a preview of
/// what will be written.
#[test]
fn the_preview_states_what_the_toggles_decided() {
    let (_dir, mut input) = fixture();

    let default_preview = summary(&input).expect("render");
    assert!(
        default_preview.contains("Game 1, Game 2"),
        "{default_preview}"
    );
    assert!(!default_preview.contains("operations_detail.json"));

    input.options.include_game_titles = true;
    input.options.include_operations_detail = true;
    let opted_in = summary(&input).expect("render");
    assert!(
        opted_in.contains("INCLUDED because you chose to"),
        "{opted_in}"
    );
    assert!(opted_in.contains("operations_detail.json"), "{opted_in}");
}

/// Identity of the rule pack, not its contents: the question is whether
/// the rule that fired was stock, and shipping the file back to whoever
/// already has it answers nothing.
#[test]
fn the_rule_pack_is_reported_by_identity_not_by_content() {
    let (_dir, input) = fixture();
    let bundle = build(&input);

    let rules: serde_json::Value =
        serde_json::from_str(&entry(&bundle.bytes, "rules.json")).expect("parse rules section");

    assert_eq!(rules["matches_builtin"], serde_json::json!(true));
    assert!(rules["crc32"].is_string());
    assert!(
        rules["rule_count"].as_u64().is_some_and(|count| count > 0),
        "{rules}",
    );
    assert!(
        !entry(&bundle.bytes, "rules.json").contains("\"pattern\""),
        "the pack's own rules must not be shipped back",
    );
}

/// A missing log is a normal state (the user switched logging off) and has
/// to read as that, not as a failure to build the bundle.
#[test]
fn a_missing_log_is_reported_rather_than_fatal() {
    let (_dir, mut input) = fixture();
    input.log_path = PathBuf::from(r"Z:\nothing\here\gametrimmer.log");

    let bundle = build(&input);
    let errors = entry(&bundle.bytes, "errors.txt");

    assert!(errors.contains("no log file at"), "{errors}");
    assert!(errors.contains("logging may be switched off"), "{errors}");
}

/// The environment is never enumerated - a bundle must carry named values
/// only. This pins the absence of an `env::vars()` sweep the cheap way:
/// nothing in the archive mentions a variable nobody asked for.
#[test]
fn no_section_enumerates_the_environment() {
    let (_dir, input) = fixture();
    let bundle = build(&input);

    for name in entry_names(&bundle.bytes) {
        let body = entry(&bundle.bytes, &name);
        for forbidden in ["COMPUTERNAME", "USERDOMAIN", "LOGONSERVER"] {
            assert!(!body.contains(forbidden), "{name} carries {forbidden}");
        }
    }
}

/// `db_health` is the section a corruption report is read from, so it has
/// to carry the classification rather than only the raw counters.
#[test]
fn db_health_carries_the_integrity_verdict_and_both_schema_versions() {
    let (_dir, input) = fixture();
    let bundle = build(&input);

    let health: serde_json::Value =
        serde_json::from_str(&entry(&bundle.bytes, "db_health.json")).expect("parse health");

    assert_eq!(health["integrity_check"], serde_json::json!("ok"));
    assert_eq!(
        health["current_schema_version"],
        serde_json::json!(crate::db::CURRENT_SCHEMA_VERSION)
    );
    assert_eq!(health["user_version"], health["current_schema_version"]);
}

/// The ini's raw text and this build's reading of it, together. A setting
/// that "didn't stick" is exactly the gap between the two, and either half
/// alone hides it.
#[test]
fn settings_ship_both_the_raw_ini_and_the_parsed_view() {
    let (_dir, input) = fixture();
    let bundle = build(&input);

    let settings: serde_json::Value =
        serde_json::from_str(&entry(&bundle.bytes, "settings.json")).expect("parse settings");

    assert!(settings["raw"]
        .as_str()
        .expect("raw ini text")
        .contains("[settings]"));
    assert!(settings["parsed"]["logging_enabled"].is_string());
    assert!(settings["read_error"].is_null());
}

/// Sanity on the shape the whole format argument rests on: a realistic
/// bundle stays small enough to attach to an issue without hosting it.
#[test]
fn the_default_bundle_is_small_enough_to_attach() {
    let (_dir, input) = fixture();
    let bundle = build(&input);

    assert!(
        bundle.bytes.len() < 256 * 1024,
        "a one-game bundle should be tiny, got {} bytes",
        bundle.bytes.len(),
    );
    assert_eq!(&bundle.bytes[..2], b"PK", "not a zip");
}

/// Every entry has to be Deflate or Store: Explorer's built-in handler
/// reads those two and nothing else, and a zip a recipient cannot
/// double-click is the wrong failure for a support file.
#[test]
fn every_entry_uses_a_method_explorer_can_open() {
    let (_dir, input) = fixture();
    let bundle = build(&input);

    let mut archive = zip::ZipArchive::new(Cursor::new(&bundle.bytes)).expect("open archive");
    for index in 0..archive.len() {
        let file = archive.by_index(index).expect("entry");
        assert!(
            matches!(
                file.compression(),
                zip::CompressionMethod::Deflated | zip::CompressionMethod::Stored
            ),
            "{} uses {:?}",
            file.name(),
            file.compression(),
        );
    }
}

/// Two bundles from the same install must not be linkable as the same
/// install: the id is per-generation, never stable. A stable one would let
/// two files posted months apart be tied to the same person, for a benefit
/// nothing here needs.
#[test]
fn each_generation_gets_its_own_id() {
    let (_dir, input) = fixture();

    let id_of = |bundle: &Bundle| -> String {
        let manifest: serde_json::Value =
            serde_json::from_str(&entry(&bundle.bytes, "manifest.json")).expect("parse manifest");
        manifest["generation_id"]
            .as_str()
            .expect("generation_id")
            .to_string()
    };

    assert_ne!(id_of(&build(&input)), id_of(&build(&input)));
}
