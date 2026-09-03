//! Canonical examples for the context markers (`markers.rs`) and the
//! language-family heuristic (`family.rs`), plus the false-positive traps
//! and the Video/Font extension (2026-07-12 change).

use super::*;

fn fe(rel_path: &str) -> FileEntry {
    FileEntry::logical_only(rel_path, 0, None)
}

fn analyze_one(rel_path: &str) -> Vec<(usize, LangFinding)> {
    let files = vec![fe(rel_path)];
    LangDetector::new().analyze_game(&files)
}

fn find_for(paths: &[&str]) -> Vec<(usize, LangFinding)> {
    let files: Vec<FileEntry> = paths.iter().map(|p| fe(p)).collect();
    LangDetector::new().analyze_game(&files)
}

// --- Canonical examples --------------------------------------------------

#[test]
fn audio_localization_flagged_with_high_confidence() {
    // Recalibrated after the 2026-07-16 screenshot report: a lone full
    // language name in a filename is no longer trusted on an asset-kind
    // marker alone (`victory_german.webm`, `russian.spk` and dozens more
    // were game content) — real per-language audio sets are recognized by
    // their sibling family instead.
    let findings = find_for(&[
        "base\\sound\\soundbanks\\hhpc\\Spanish(Spain)_patch_1.snd",
        "base\\sound\\soundbanks\\hhpc\\French(France)_patch_1.snd",
        "base\\sound\\soundbanks\\hhpc\\German_patch_1.snd",
    ]);
    let es = findings
        .iter()
        .map(|(_, f)| f)
        .find(|f| f.lang_tag == "es")
        .expect("spanish sibling should be flagged");
    assert_eq!(es.kind, LangKind::Audio);
    assert!(es.confidence >= 90, "confidence was {}", es.confidence);
}

#[test]
fn text_localization_flagged_via_closecaption_marker() {
    let findings = analyze_one("mods\\BMS\\resource\\closecaption_spanish.dat");
    assert_eq!(findings.len(), 1, "{findings:?}");
    let (_, f) = &findings[0];
    assert_eq!(f.lang_tag, "es");
    assert_eq!(f.kind, LangKind::Text);
}

#[test]
fn negative_marker_art_units_blocks_flagging() {
    let findings = analyze_one("Art\\Units\\King Spanish\\unit.flc");
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn negative_marker_decisions_blocks_flagging() {
    let findings = analyze_one("decisions\\SpanishNation.txt");
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn negative_marker_history_blocks_flagging() {
    let findings = analyze_one("history\\wars\\SecondAngloSpanishWar.txt");
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn filename_language_family_flags_non_keep_members_only() {
    let findings = find_for(&[
        "sound\\Voice_english.pak",
        "sound\\Voice_french.pak",
        "sound\\Voice_german.pak",
        "sound\\Voice_polish.pak",
    ]);
    let tags: HashSet<String> = findings.iter().map(|(_, f)| f.lang_tag.clone()).collect();
    assert_eq!(
        tags,
        ["fr", "de", "pl"].into_iter().map(String::from).collect(),
        "english must stay kept: {findings:?}"
    );
    for (_, f) in &findings {
        assert!(matches!(f.kind, LangKind::Audio | LangKind::Unknown));
    }
}

#[test]
fn lone_iso2_without_family_is_never_flagged() {
    let findings = analyze_one("data\\it\\credits.txt");
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn sibling_language_folders_flag_non_keep_folders_only() {
    let findings = find_for(&[
        "root\\en\\file1.txt",
        "root\\de\\file2.txt",
        "root\\fr\\file3.txt",
        "root\\es\\file4.txt",
    ]);
    let tags: HashSet<String> = findings.iter().map(|(_, f)| f.lang_tag.clone()).collect();
    assert_eq!(
        tags,
        ["de", "fr", "es"].into_iter().map(String::from).collect(),
        "en/ folder must stay kept: {findings:?}"
    );
}

#[test]
fn iso3_with_localization_marker_flags_polish() {
    let findings = analyze_one("localization\\pol\\quests.json");
    assert_eq!(findings.len(), 1, "{findings:?}");
    let (_, f) = &findings[0];
    assert_eq!(f.lang_tag, "pl");
}

#[test]
fn bare_no_token_in_compound_word_is_not_flagged() {
    let findings = analyze_one("movies\\intro_no_subtitles.bik");
    assert!(findings.is_empty(), "{findings:?}");
}

// --- Video / Font extension (2026-07-12 requirement change) --------------

#[test]
fn video_marker_folder_flags_video_kind() {
    let findings = analyze_one("movies\\german\\intro.bik");
    assert_eq!(findings.len(), 1, "{findings:?}");
    let (_, f) = &findings[0];
    assert_eq!(f.lang_tag, "de");
    assert_eq!(f.kind, LangKind::Video);
}

#[test]
fn keep_language_video_is_not_flagged() {
    let findings = analyze_one("data\\movies\\intro_english.usm");
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn font_marker_with_cjk_steam_name_flags_font_kind() {
    let findings = analyze_one("fonts\\font_schinese.ttf");
    assert_eq!(findings.len(), 1, "{findings:?}");
    let (_, f) = &findings[0];
    assert_eq!(f.lang_tag, "zh-hans");
    assert_eq!(f.kind, LangKind::Font);
}

#[test]
fn video_extension_family_infers_video_kind_without_word_marker() {
    let findings = find_for(&[
        "cine\\intro_en.bik",
        "cine\\intro_de.bik",
        "cine\\intro_fr.bik",
        "cine\\intro_pl.bik",
    ]);
    let tags: HashSet<String> = findings.iter().map(|(_, f)| f.lang_tag.clone()).collect();
    assert_eq!(
        tags,
        ["de", "fr", "pl"].into_iter().map(String::from).collect(),
        "english must stay kept: {findings:?}"
    );
    for (_, f) in &findings {
        assert_eq!(f.kind, LangKind::Video, "{f:?}");
    }
}

// --- Keep-list customization ----------------------------------------------

#[test]
fn custom_keep_list_suppresses_a_different_language() {
    let detector = LangDetector::with_keep_list(&["es".to_string()]);
    let files = vec![fe(
        "base\\sound\\soundbanks\\hhpc\\Spanish(Spain)_patch_1.snd",
    )];
    let findings = detector.analyze_game(&files);
    assert!(findings.is_empty(), "es is now kept: {findings:?}");
}

// --- Satellite assembly case-insensitivity (C2 allocation removal) -------

#[test]
fn ends_with_ignore_ascii_case_handles_mixed_case_and_short_haystacks() {
    // Mixed case must match, same as the old `to_lowercase()` did.
    assert!(ends_with_ignore_ascii_case(
        "de\\Foo.Resources.DLL",
        b".resources.dll"
    ));
    // A haystack shorter than the suffix must not panic on the slice.
    assert!(!ends_with_ignore_ascii_case("a", b".resources.dll"));
    assert!(!ends_with_ignore_ascii_case("", b".resources.dll"));
    assert!(!ends_with_ignore_ascii_case("Foo.dll", b".resources.dll"));
}

#[test]
fn satellite_assembly_mixed_case_extension_is_not_treated_as_executable() {
    // A .NET satellite assembly named with mixed case must still be
    // exempted from the "executable code is never a localization" rule,
    // and flagged like any other localization file in a language folder.
    let findings = analyze_one("localization\\pol\\Quests.Resources.DLL");
    assert_eq!(findings.len(), 1, "{findings:?}");
    let (_, f) = &findings[0];
    assert_eq!(f.lang_tag, "pl");
}

#[test]
fn plain_dll_in_language_folder_stays_excluded_as_executable() {
    // Sanity check that a non-satellite .dll is still skipped, proving the
    // satellite-assembly check above is doing real work.
    let findings = analyze_one("localization\\pol\\quests.dll");
    assert!(findings.is_empty(), "{findings:?}");
}

/// GT-392: Delta Force ships a CEF `locales/` folder in three places, 66
/// files / 45.1 MB in total, and the detector found none of them - not
/// because the shape was hard, but because seventeen of the codes were not
/// in the dictionary at all.
///
/// The set is the check: the folder holds one file per language, so if any
/// one code is unknown it simply goes missing while its neighbours are
/// found. `uk` and `en-US` are the control - they were always known and are
/// keep-listed by default, so they must stay unflagged for a different
/// reason.
#[test]
fn delta_forces_locales_folder_is_found_in_every_language_it_ships() {
    let dir = "Launcher\\service\\locales";
    let codes = [
        "am", "bn", "ca", "de", "es", "et", "fa", "fil", "fr", "gu", "hi", "hr", "id", "it", "ja",
        "kn", "ko", "lt", "lv", "ml", "mr", "ms", "nb", "nl", "pl", "pt-BR", "pt-PT", "ro", "ru",
        "sk", "sl", "sv", "sw", "ta", "te", "th", "tr", "vi", "zh-CN", "zh-TW",
    ];
    let paths: Vec<String> = codes
        .iter()
        .map(|code| format!("{dir}\\{code}.pak"))
        .collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let findings = find_for(&refs);

    let missing: Vec<&str> = codes
        .iter()
        .enumerate()
        .filter(|(idx, _)| !findings.iter().any(|(hit, _)| hit == idx))
        .map(|(_, code)| *code)
        .collect();
    assert!(
        missing.is_empty(),
        "these locales stayed invisible: {missing:?}"
    );

    // The seventeen GT-392 added, spelled as the launcher spells them.
    for (code, expected) in [
        ("am", "am"),
        ("bn", "bn"),
        ("ca", "ca"),
        ("et", "et"),
        ("fa", "fa"),
        ("fil", "fil"),
        ("gu", "gu"),
        ("kn", "kn"),
        ("lt", "lt"),
        ("lv", "lv"),
        ("ml", "ml"),
        ("mr", "mr"),
        ("ms", "ms"),
        ("nb", "no"),
        ("sw", "sw"),
        ("ta", "ta"),
        ("te", "te"),
    ] {
        let idx = codes.iter().position(|c| *c == code).expect("code listed");
        let (_, finding) = findings
            .iter()
            .find(|(hit, _)| *hit == idx)
            .unwrap_or_else(|| panic!("{code}.pak was not flagged"));
        assert_eq!(finding.lang_tag, expected, "{code}.pak");
    }
}

/// The other half of GT-392: a wider dictionary must not turn short words
/// into languages. Every code added is two or three letters, which is the
/// exact class the family gate exists for (8b03b91).
#[test]
fn the_long_tail_codes_do_not_flag_on_their_own() {
    for path in [
        // "ms" is milliseconds far more often than it is Malay.
        "Config\\timeout_ms.ini",
        // "ca" is a certificate authority, "lt"/"gt" are comparisons.
        "Binaries\\Win64\\ca.crt",
        "Shaders\\compare_lt.hlsl",
        // "sl" ships as an NVIDIA Streamline DLL and is deliberately never
        // read as Slovenian - .dll/.exe are ignored outright.
        "Binaries\\Win64\\sl.dlss.dll",
        // A rifle called "ar 2" is not Arabic (8b03b91), and a game's own
        // initials are not a language.
        "data\\items\\ar 2 lady (luckystrike).item.bytes",
    ] {
        assert!(
            analyze_one(path).is_empty(),
            "{path} must not read as a language"
        );
    }
}
