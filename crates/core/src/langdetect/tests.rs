//! Canonical examples from docs/04_implementation_plan.md §5.3/§5.4 plus the
//! false-positive traps and the Video/Font extension (2026-07-12 change).

use super::*;

fn fe(rel_path: &str) -> FileEntry {
    FileEntry {
        rel_path: rel_path.to_string(),
        size: 0,
        mtime: None,
    }
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
