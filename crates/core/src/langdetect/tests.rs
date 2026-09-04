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

/// GT-232: Middle-earth: Shadow of War ships eight presentation archives,
/// one per language, and the three whose locale tag is written solid were
/// the ones that went missing. `presentations_de` splits into a language
/// atom; `presentations_ptbr` splits into one opaque atom that no pass could
/// look inside.
///
/// `_en` is the control at the other end: English is keep-listed by default,
/// so it is evidence for the family and never a finding itself.
#[test]
fn shadow_of_wars_solid_locale_tags_join_their_own_family() {
    let paths = [
        "presentations_de.arch06",
        "presentations_en.arch06",
        "presentations_eses.arch06",
        "presentations_esla.arch06",
        "presentations_fr.arch06",
        "presentations_it.arch06",
        "presentations_ja.arch06",
        "presentations_ptbr.arch06",
    ];
    let findings = find_for(&paths);
    let tag_at = |idx: usize| {
        findings
            .iter()
            .find(|(hit, _)| *hit == idx)
            .map(|(_, f)| f.lang_tag.as_str())
    };

    assert_eq!(tag_at(2), Some("es"), "presentations_eses");
    assert_eq!(tag_at(3), Some("es-419"), "presentations_esla");
    assert_eq!(tag_at(7), Some("pt-br"), "presentations_ptbr");
    // The four that already worked must keep working.
    assert_eq!(tag_at(0), Some("de"));
    assert_eq!(tag_at(4), Some("fr"));
    assert_eq!(tag_at(5), Some("it"));
    assert_eq!(tag_at(6), Some("ja"));
    assert_eq!(tag_at(1), None, "English is kept, not flagged");
}

/// The other solid spellings the same rule has to reach, each in a set big
/// enough to confirm itself.
#[test]
fn every_curated_locale_tag_is_readable_without_its_separator() {
    for (glued, expected) in [
        ("esmx", "es-419"),
        ("ptpt", "pt"),
        ("zhcn", "zh-hans"),
        ("zhtw", "zh-hant"),
        ("frca", "fr"),
        ("engb", "en"),
        ("enus", "en"),
    ] {
        let paths: Vec<String> = ["de", "fr", "it", "ja", "ru", glued]
            .iter()
            .map(|tag| format!("voice_{tag}.bnk"))
            .collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let findings = find_for(&refs);
        let hit = findings.iter().find(|(idx, _)| *idx == 5).map(|(_, f)| f);
        match expected {
            // English is keep-listed: recognized, deliberately not flagged.
            "en" => assert!(hit.is_none(), "voice_{glued}.bnk is English and kept"),
            _ => assert_eq!(
                hit.map(|f| f.lang_tag.as_str()),
                Some(expected),
                "voice_{glued}.bnk"
            ),
        }
    }
}

/// The boundary of the solid-tag rule. A glued locale tag can spell an
/// ordinary word - `fa-ir` is "fair", `he-il` is "heil" - so the rule emits
/// family-gated evidence, never a self-sufficient one. Standing alone, these
/// stay words.
#[test]
fn a_glued_locale_tag_that_spells_a_word_does_not_flag_on_its_own() {
    for path in [
        "audio\\fair.bnk",
        "movies\\heil.bik",
        "sound\\crowd_fair.wav",
    ] {
        assert!(
            analyze_one(path).is_empty(),
            "{path} must stay an ordinary word"
        );
    }
}

/// GT-229: about 48 files across the library carry two plausible language
/// tokens in one name, and two runs of the same binary labelled them
/// differently. The set of flagged *paths* was always the same - only the
/// label moved - but a file shown as French in one scan and German in the
/// next is a file the user cannot trust, and the keep-list filter acts on
/// exactly that label.
///
/// The cause was a tie: two family groups claim the same file with the same
/// confidence, and whichever the hash map happened to visit first won. Hash
/// map order in Rust is seeded per instance, so this reproduces by simply
/// running the detector again.
#[test]
fn a_file_with_two_language_tokens_gets_the_same_label_every_run() {
    let paths = [
        // Two tokens, one name: a Qt translation whose locale tag can be
        // read whole or as its prefix.
        "PySide6\\translations\\qt_pt_BR.qm",
        "PySide6\\translations\\qtmultimedia_pt_BR.qm",
        "PySide6\\translations\\qt_help_pt_BR.qm",
        "PySide6\\translations\\qt_de.qm",
        "PySide6\\translations\\qt_fr.qm",
        "PySide6\\translations\\qt_es.qm",
        "PySide6\\translations\\qt_it.qm",
        "PySide6\\translations\\qt_ja.qm",
        "PySide6\\translations\\qt_ru.qm",
        "PySide6\\translations\\qt_pl.qm",
        // The classic double-token shape: a French font name carrying an
        // explicit German localization pairing.
        "Content\\Fonts\\fonts_fra_LOC_DEU.upk",
        "Content\\Fonts\\fonts_ita_LOC_FRA.upk",
        "Content\\Fonts\\fonts_spa_LOC_ITA.upk",
        "Content\\Fonts\\fonts_deu_LOC_SPA.upk",
        "Content\\Fonts\\fonts_rus_LOC_POL.upk",
    ];

    let first: Vec<(usize, String)> = {
        let mut v: Vec<(usize, String)> = find_for(&paths)
            .into_iter()
            .map(|(idx, f)| (idx, f.lang_tag))
            .collect();
        v.sort();
        v
    };

    for run in 1..200 {
        let mut again: Vec<(usize, String)> = find_for(&paths)
            .into_iter()
            .map(|(idx, f)| (idx, f.lang_tag))
            .collect();
        again.sort();
        assert_eq!(
            again, first,
            "run {run} disagreed with run 0 about which language these files are"
        );
    }
}

// --- The abbreviation a directory spells out itself (GT-467) -------------

/// Rogue Trooper Redux, `Misc\L_Text\Tannoy\`: the same eight texts
/// written twice, once spelled out and once in the studio's own two-letter
/// shorthand. `t_am` is `Tannoy_American`, not Amharic.
const TANNOY_FOLDER: &[&str] = &[
    r"Misc\L_Text\Tannoy\Tannoy_American.asr",
    r"Misc\L_Text\Tannoy\Tannoy_Chinese_S.asr",
    r"Misc\L_Text\Tannoy\Tannoy_Chinese_T.asr",
    r"Misc\L_Text\Tannoy\Tannoy_English.asr",
    r"Misc\L_Text\Tannoy\Tannoy_French.asr",
    r"Misc\L_Text\Tannoy\Tannoy_German.asr",
    r"Misc\L_Text\Tannoy\Tannoy_Italian.asr",
    r"Misc\L_Text\Tannoy\Tannoy_Spanish.asr",
    r"Misc\L_Text\Tannoy\t_am.asr",
    r"Misc\L_Text\Tannoy\t_en.asr",
    r"Misc\L_Text\Tannoy\t_fr.asr",
    r"Misc\L_Text\Tannoy\t_ge.asr",
    r"Misc\L_Text\Tannoy\t_it.asr",
    r"Misc\L_Text\Tannoy\t_sp.asr",
    r"Misc\L_Text\Tannoy\t_Zs.asr",
    r"Misc\L_Text\Tannoy\t_Zt.asr",
];

/// Delta Force, `Game\DeltaForce\Binaries\Win64\locales\` as it stands on
/// disk: twenty CEF language packs, every one of them a bare code or a
/// locale tag, and not one language spelled out in full.
const DELTA_FORCE_LOCALES: &[&str] = &[
    r"Game\DeltaForce\Binaries\Win64\locales\am.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\am.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\bn.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\bn.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\ca.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\ca.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\en-GB.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\en-GB.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\en-US.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\en-US.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\et.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\et.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\fa.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\fa.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\fil.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\fil.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\gu.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\gu.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\kn.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\kn.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\lt.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\lt.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\lv.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\lv.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\ml.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\ml.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\mr.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\mr.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\ms.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\ms.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\nb.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\nb.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\sw.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\sw.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\ta.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\ta.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\te.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\te.pak.info",
    r"Game\DeltaForce\Binaries\Win64\locales\uk.pak",
    r"Game\DeltaForce\Binaries\Win64\locales\uk.pak.info",
];

/// `(file name, language tag)` of every flagged file, sorted - the whole
/// verdict of a directory in one comparable value.
fn flagged_labels(paths: &[&str]) -> Vec<(String, String)> {
    let mut labels: Vec<(String, String)> = find_for(paths)
        .into_iter()
        .map(|(i, f)| {
            let name = paths[i].rsplit('\\').next().unwrap_or(paths[i]);
            (name.to_string(), f.lang_tag)
        })
        .collect();
    labels.sort();
    labels
}

/// GT-467: `t_am.asr` became a finding labelled Amharic the moment the
/// dictionary learned the code `am`, and Amharic is not a language this game
/// ships. The file is the American text - English - and English is on the
/// keep-list, so the one file in the set that the keep-list owed protection
/// to was the one offered for deletion.
///
/// Nothing about the set gives that away by counting: five of its six codes
/// (`en`, `fr`, `it`, and the curated `ge`/`sp`) are ones the dictionary
/// carries, so it is an ordinary bare-code family and the family gate
/// confirms it exactly as designed. What gives it away is the long set in the
/// same folder, which spells `American` out.
#[test]
fn a_spelled_out_american_keeps_its_own_abbreviation_out_of_amharic() {
    let labels = flagged_labels(TANNOY_FOLDER);
    assert!(
        !labels.iter().any(|(name, _)| name == "t_am.asr"),
        "t_am is the American text, not Amharic: {labels:?}"
    );
    // The rest of the short set is untouched - the rule only fires where the
    // spelled-out name and the code disagree.
    let short: Vec<(String, String)> = labels
        .iter()
        .filter(|(name, _)| name.starts_with("t_"))
        .cloned()
        .collect();
    assert_eq!(
        short,
        [
            // `Zs`/`Zt` are this studio's codes for the two Chinese scripts:
            // languages the dictionary cannot name, reported as undetermined
            // rather than hidden (GT-464). What this test guards is `t_am`,
            // which is still absent - a code the dictionary *can* read is
            // judged on its own merits and never reaches the unnamed path.
            ("t_Zs.asr", "und"),
            ("t_Zt.asr", "und"),
            ("t_fr.asr", "fr"),
            ("t_ge.asr", "de"),
            ("t_it.asr", "it"),
            ("t_sp.asr", "es"),
        ]
        .map(|(n, l)| (n.to_string(), l.to_string()))
        .to_vec(),
        "{labels:?}"
    );
}

/// The counterexample GT-467 must not break: the same `am` code, in a folder
/// that spells no language out at all, is genuine Amharic and stays a
/// finding. This is the folder GT-392 was written for.
#[test]
fn a_bare_am_stays_amharic_where_no_sibling_spells_american_out() {
    let labels = flagged_labels(DELTA_FORCE_LOCALES);
    assert!(
        labels.contains(&("am.pak".to_string(), "am".to_string())),
        "Delta Force ships real Amharic: {labels:?}"
    );
    assert!(
        labels.contains(&("sw.pak".to_string(), "sw".to_string())),
        "and real Swahili beside it: {labels:?}"
    );
    // English is the keep language it always was, spelled as a locale tag -
    // which is deliberately not the kind of name that shadows a bare code.
    assert!(
        !labels.iter().any(|(name, _)| name.starts_with("en-")),
        "{labels:?}"
    );
}

/// 3DMark, `bin\x64\locales\` as it stands on disk: 51 Chromium/CEF
/// language packs, `fi.pak` (Finnish) and `fil.pak` (Filipino) among them.
/// `fil` is a real dictionary entry, but only at Level B - `l10n_rules.json`
/// never lists it as a Level A, all-letters spelled-out name the way
/// `american` is for English - so it is not the kind of sibling
/// [`shadowed_bare_codes`] treats as spelling anything out, and the bare
/// `fi` beside it stays Finnish, not shadowed into losing its family.
const THREEDMARK_LOCALES: &[&str] = &[
    r"3DMark\bin\x64\locales\am.pak",
    r"3DMark\bin\x64\locales\ar.pak",
    r"3DMark\bin\x64\locales\bg.pak",
    r"3DMark\bin\x64\locales\bn.pak",
    r"3DMark\bin\x64\locales\ca.pak",
    r"3DMark\bin\x64\locales\cs.pak",
    r"3DMark\bin\x64\locales\da.pak",
    r"3DMark\bin\x64\locales\de.pak",
    r"3DMark\bin\x64\locales\el.pak",
    r"3DMark\bin\x64\locales\en-GB.pak",
    r"3DMark\bin\x64\locales\en-US.pak",
    r"3DMark\bin\x64\locales\es-419.pak",
    r"3DMark\bin\x64\locales\es.pak",
    r"3DMark\bin\x64\locales\et.pak",
    r"3DMark\bin\x64\locales\fa.pak",
    r"3DMark\bin\x64\locales\fi.pak",
    r"3DMark\bin\x64\locales\fil.pak",
    r"3DMark\bin\x64\locales\fr.pak",
    r"3DMark\bin\x64\locales\gu.pak",
    r"3DMark\bin\x64\locales\he.pak",
    r"3DMark\bin\x64\locales\hi.pak",
    r"3DMark\bin\x64\locales\hr.pak",
    r"3DMark\bin\x64\locales\hu.pak",
    r"3DMark\bin\x64\locales\id.pak",
    r"3DMark\bin\x64\locales\it.pak",
    r"3DMark\bin\x64\locales\ja.pak",
    r"3DMark\bin\x64\locales\kn.pak",
    r"3DMark\bin\x64\locales\ko.pak",
    r"3DMark\bin\x64\locales\lt.pak",
    r"3DMark\bin\x64\locales\lv.pak",
    r"3DMark\bin\x64\locales\ml.pak",
    r"3DMark\bin\x64\locales\mr.pak",
    r"3DMark\bin\x64\locales\ms.pak",
    r"3DMark\bin\x64\locales\nb.pak",
    r"3DMark\bin\x64\locales\nl.pak",
    r"3DMark\bin\x64\locales\pl.pak",
    r"3DMark\bin\x64\locales\pt-BR.pak",
    r"3DMark\bin\x64\locales\pt-PT.pak",
    r"3DMark\bin\x64\locales\ro.pak",
    r"3DMark\bin\x64\locales\ru.pak",
    r"3DMark\bin\x64\locales\sk.pak",
    r"3DMark\bin\x64\locales\sl.pak",
    r"3DMark\bin\x64\locales\sr.pak",
    r"3DMark\bin\x64\locales\sv.pak",
    r"3DMark\bin\x64\locales\sw.pak",
    r"3DMark\bin\x64\locales\ta.pak",
    r"3DMark\bin\x64\locales\te.pak",
    r"3DMark\bin\x64\locales\th.pak",
    r"3DMark\bin\x64\locales\tr.pak",
    r"3DMark\bin\x64\locales\uk.pak",
    r"3DMark\bin\x64\locales\vi.pak",
    r"3DMark\bin\x64\locales\zh-CN.pak",
    r"3DMark\bin\x64\locales\zh-TW.pak",
];

/// The counterexample a loosened Level-A gate on [`shadowed_bare_codes`]
/// would break silently: `fi.pak` has no full "finnish" name anywhere in
/// this folder to save it, so if `fil` were ever promoted to Level A (or the
/// gate stopped requiring Level A at all), `fil.pak`'s own bare token would
/// start reading as a spelled-out claim on every `fi*` code, and `fi.pak`
/// would lose the family evidence [`LangEvidence::is_family`] reports here.
/// The label itself would not visibly change in *this* folder - `locales`
/// is also a marker word, so a weaker, marker-only reading of the bare `fi`
/// token quietly steps in and still calls it Finnish - but that fallback is
/// a coincidence of this one directory name, not a guarantee: outside a
/// folder literally named `locales`/`locale`, losing family is losing the
/// finding outright, the way [`lone_iso2_without_family_is_never_flagged`]
/// shows for any other bare two-letter code. Assert on the evidence, not
/// just the label, or a regression here hides in a green test.
#[test]
fn a_bare_fi_stays_finnish_where_no_sibling_spells_filipino_out() {
    let findings = find_for(THREEDMARK_LOCALES);
    let finding_for = |basename: &str| {
        THREEDMARK_LOCALES
            .iter()
            .position(|p| p.rsplit('\\').next() == Some(basename))
            .and_then(|idx| findings.iter().find(|(i, _)| *i == idx))
            .map(|(_, f)| f)
            .unwrap_or_else(|| panic!("{basename} was not flagged: {findings:?}"))
    };

    let fi = finding_for("fi.pak");
    assert_eq!(fi.lang_tag, "fi", "3DMark ships real Finnish: {fi:?}");
    assert!(
        fi.reason.evidence.is_family(),
        "fi.pak must still be confirmed by the directory family, not merely \
         by the `locales` marker word falling back to a plain dictionary \
         lookup: {fi:?}"
    );

    let fil = finding_for("fil.pak");
    assert_eq!(fil.lang_tag, "fil", "and real Filipino beside it: {fil:?}");
}

/// GT-466: the two Chinese texts of that same folder stopped being findings
/// altogether - 0.04 MB, but a whole localization the user could see on disk
/// and not in the app, while the French, German, Italian and Spanish files
/// beside them stayed.
///
/// They are not members of any name-shape family (`Tannoy_Chinese_S` carries a
/// script suffix no sibling has); they belong to the folder through the
/// shared-position mechanism, and there they hang by a thread. A member of a
/// position group has to share a *distinctive* atom with a different language,
/// and an atom carried by more than half the group is not distinctive - so
/// `Tannoy` proves nothing, and the only other atom `Tannoy_Chinese_T` has is
/// the `t` of the short set's `t_*` names. When the dictionary learned `am`,
/// `t_am.asr` joined that group as its seventh `t`, tipped `t` over the half
/// mark, and both Chinese files fell out at once: `_T` lost its support and
/// `_S`, which never had any of its own, lost the language that had been
/// carrying it.
///
/// So this test is the whole folder, line for line, and it is deliberately
/// unforgiving: sixteen files, ten findings, one label each. It is also run
/// repeatedly, because the labels of a two-family folder are exactly what
/// GT-229 made stop moving between runs.
#[test]
fn the_tannoy_folder_reads_the_same_ten_findings_every_run() {
    let expected: Vec<(String, String)> = [
        ("Tannoy_Chinese_S.asr", "zh-hans"),
        ("Tannoy_Chinese_T.asr", "zh-hans"),
        ("Tannoy_French.asr", "fr"),
        ("Tannoy_German.asr", "de"),
        ("Tannoy_Italian.asr", "it"),
        ("Tannoy_Spanish.asr", "es"),
        // GT-464: the studio's own codes for the two Chinese scripts. They
        // are languages, the dictionary cannot name them, and until this
        // card they were the only two files in the folder that produced
        // nothing at all - invisible in exactly the way a missing detector
        // is invisible. They now report as undetermined: shown, unlabelled,
        // and out of reach of bulk selection.
        ("t_Zs.asr", "und"),
        ("t_Zt.asr", "und"),
        ("t_fr.asr", "fr"),
        ("t_ge.asr", "de"),
        ("t_it.asr", "it"),
        ("t_sp.asr", "es"),
    ]
    .map(|(name, tag)| (name.to_string(), tag.to_string()))
    .to_vec();

    for run in 0..20 {
        assert_eq!(
            flagged_labels(TANNOY_FOLDER),
            expected,
            "run {run} disagreed"
        );
    }
}

/// GT-455: a script or region qualifier sitting next to the base language
/// name must survive tokenization.
///
/// Every case here is a real path from the 2026-09-04 library run, and every
/// one of them used to answer with the *base* language: the strong/CamelCase
/// split scatters `ChineseTraditional` into `chinese` + `traditional`, and
/// only `chinese` is a dictionary word - so the qualifier was dropped and the
/// file was labelled Simplified. The same shape appears with a region
/// (`es_MX` -> `es`) and with the parenthesized spelling
/// (`Portuguese(Brazil)` -> `pt`).
///
/// Why a wrong label is worse than a missing one: the keep-language list is
/// applied to the label. A player keeping Traditional Chinese looks at a
/// Traditional file marked Simplified, and their keep-list does not protect
/// it - the guard is intact and bypassed at the same time.
#[test]
fn a_script_or_region_qualifier_beside_the_language_name_is_not_dropped() {
    let labels = |paths: &[&str]| -> Vec<String> {
        let files: Vec<FileEntry> = paths.iter().map(|p| fe(p)).collect();
        let found = LangDetector::new().analyze_game(&files);
        let mut out: Vec<String> = paths
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let tag = found
                    .iter()
                    .find(|(j, _)| *j == i)
                    .map(|(_, f)| f.lang_tag.as_str())
                    .unwrap_or("-");
                format!("{p} = {tag}")
            })
            .collect();
        out.sort();
        out
    };

    // Brawlhalla: the qualifier is glued on in CamelCase.
    assert_eq!(
        labels(&[
            r"fontData\Font_ChineseTraditional.swf",
            r"fontData\Font_ChineseSimplified.swf",
        ]),
        vec![
            r"fontData\Font_ChineseSimplified.swf = zh-hans".to_string(),
            r"fontData\Font_ChineseTraditional.swf = zh-hant".to_string(),
        ],
        "a CamelCase qualifier must not be discarded"
    );

    // SteamVR: the region is separated by an underscore, and the dictionary
    // spells the tag with a hyphen.
    assert_eq!(
        labels(&[
            r"resources\localization\localization_es_MX.json",
            r"resources\localization\localization_pt_BR.json",
        ]),
        vec![
            r"resources\localization\localization_es_MX.json = es-419".to_string(),
            r"resources\localization\localization_pt_BR.json = pt-br".to_string(),
        ],
        "an underscore-separated region must reach its hyphen-spelled alias"
    );

    // DOOM: The Dark Ages: the qualifier is parenthesized, and the base
    // language also matches on its own as a prefix - so two readings compete
    // and the shorter one used to win.
    assert_eq!(
        labels(&[
            r"base\sound\soundbanks\hhpc\Portuguese(Brazil).snd",
            r"base\sound\soundbanks\hhpc\Spanish(Mexico).snd",
            r"base\sound\soundbanks\hhpc\Spanish(Spain).snd",
        ]),
        vec![
            r"base\sound\soundbanks\hhpc\Portuguese(Brazil).snd = pt-br".to_string(),
            r"base\sound\soundbanks\hhpc\Spanish(Mexico).snd = es-419".to_string(),
            r"base\sound\soundbanks\hhpc\Spanish(Spain).snd = es".to_string(),
        ],
        "a parenthesized qualifier must beat the bare language name it starts with"
    );

    // Crysis 3 Remastered: the qualifier is truncated to one letter.
    assert_eq!(
        labels(&[r"Localization\ChineseT.pak", r"Localization\ChineseS.pak",]),
        vec![
            r"Localization\ChineseS.pak = zh-hans".to_string(),
            r"Localization\ChineseT.pak = zh-hant".to_string(),
        ],
        "the truncated script suffix must separate the two Chinese scripts"
    );
}

/// The counter-example GT-455 names: the fix must not turn any short
/// neighbour into a script qualifier.
///
/// `voice_de_1.pck` / `voice_de_2.pck` are two German banks numbered 1 and 2.
/// If joining a language atom to its neighbour were accepted on shape rather
/// than on a curated alias, `de` + `1` would become a "variant" of German and
/// the pair would split into two languages that do not exist.
#[test]
fn a_neighbour_that_is_not_a_curated_variant_does_not_qualify_the_language() {
    let files: Vec<FileEntry> = [
        r"sound\voice_de_1.pck",
        r"sound\voice_de_2.pck",
        r"sound\voice_fr_1.pck",
        r"sound\voice_fr_2.pck",
    ]
    .iter()
    .map(|p| fe(p))
    .collect();

    let found = LangDetector::new().analyze_game(&files);
    let tags: HashSet<&str> = found.iter().map(|(_, f)| f.lang_tag.as_str()).collect();

    assert!(
        tags.iter().all(|t| *t == "de" || *t == "fr"),
        "a numeric neighbour must not invent a language variant, got {tags:?}"
    );
}

/// Replays the detector over real library paths and reports what it finds,
/// broken down by language and by the evidence that produced each finding.
///
/// A full rescan needs administrator rights (the MFT reader) and about a
/// minute and a half, which makes it a poor instrument for the inner loop of
/// a detector change - and an impossible one when nobody is at the keyboard
/// to answer the elevation prompt. This replays the paths of an existing
/// scan instead: same detector, same real names, no privileges, seconds.
///
/// Ignored by default because it needs data that is not in the repository.
/// Export it from the scan database and point the variable at it:
///
/// ```text
/// sqlite3 -noheader copy-of-gametrimmer.db "
///   SELECT g.name || char(9) || fi.rel_path
///   FROM files fi JOIN games g ON g.id = fi.game_id
///   ORDER BY g.name, fi.rel_path;" > paths.tsv
/// GT460_PATHS=paths.tsv cargo test -p gametrimmer-core --lib -- \
///   --ignored --nocapture harness_replay_library
/// ```
///
/// What it cannot see: the scan database stores only files that produced a
/// finding, so a replay carries the family members but not the silent files
/// around them. Counts therefore track a rescan closely without being it -
/// use it to compare a change against itself, not to replace the measurement.
#[test]
#[ignore]
fn harness_replay_library() {
    use std::collections::BTreeMap;
    let path = std::env::var("GT460_PATHS").expect("set GT460_PATHS to the exported tsv");
    let raw = std::fs::read_to_string(&path).expect("read tsv");

    let mut by_game: BTreeMap<String, Vec<(String, u64)>> = BTreeMap::new();
    for line in raw.lines() {
        let mut cols = line.split('\t');
        let (Some(game), Some(rel)) = (cols.next(), cols.next()) else {
            continue;
        };
        let size = cols.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        by_game
            .entry(game.to_string())
            .or_default()
            .push((rel.to_string(), size));
    }

    // With `GT471_DUMP` set, every finding is also printed as
    // `game <tab> directory <tab> tag <tab> bytes`, so a threshold can be
    // swept in a script instead of a rebuild per candidate value.
    let dump = std::env::var_os("GT471_DUMP").is_some();

    let detector = LangDetector::new();
    let mut grand = 0usize;
    let mut grand_bytes = 0u64;
    let mut per_ev_all: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
    for (game, entries) in &by_game {
        let paths: Vec<&String> = entries.iter().map(|(p, _)| p).collect();
        let files: Vec<FileEntry> = paths.iter().map(|p| fe(p)).collect();
        let found = detector.analyze_game(&files);
        grand += found.len();
        let mut per_tag: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, f) in &found {
            *per_tag.entry(f.lang_tag.as_str()).or_default() += 1;
        }
        let top: Vec<String> = {
            let mut v: Vec<_> = per_tag.into_iter().collect();
            v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            v.into_iter()
                .take(6)
                .map(|(t, n)| format!("{t}:{n}"))
                .collect()
        };
        let mut per_ev: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
        for (i, f) in &found {
            let name = match &f.reason.evidence {
                LangEvidence::LocPair { .. } => "LocPair",
                LangEvidence::TokenWithMarker { .. } => "TokenWithMarker",
                LangEvidence::BareToken { .. } => "BareToken",
                LangEvidence::Family { .. } => "Family",
                LangEvidence::FamilyAtSharedPosition { .. } => "FamilyAtSharedPosition",
                LangEvidence::SubfolderFamily { .. } => "SubfolderFamily",
                LangEvidence::SubfolderFamilyWithPrefix { .. } => "SubfolderFamilyWithPrefix",
            };
            let bytes = entries[*i].1;
            if dump {
                let rel = &entries[*i].0;
                let dir = rel.rfind('\\').map_or("", |cut| &rel[..cut]);
                // Whether a *directory* on the path names this language
                // (`Localization\FRA\`, `fonts\rus\`) as opposed to the file
                // name alone carrying the token.
                let segs = tokenize_path(rel);
                let anchored = collect_occurrences(&LangData::builtin(), &segs)
                    .iter()
                    .any(|o| !o.is_filename && o.canonical == f.lang_tag);
                let token = match &f.reason.evidence {
                    LangEvidence::LocPair { token }
                    | LangEvidence::TokenWithMarker { token, .. }
                    | LangEvidence::BareToken { token } => token.as_str(),
                    _ => "",
                };
                let level = match LangData::builtin().lookup(token) {
                    Some((_, dict::Level::A)) => "A",
                    Some((_, dict::Level::B)) => "B",
                    Some((_, dict::Level::C)) => "C",
                    None => "-",
                };
                println!(
                    "DUMP\t{game}\t{dir}\t{}\t{bytes}\t{name}\t{}\t{token}\t{level}",
                    f.lang_tag,
                    if anchored { "dir" } else { "file" }
                );
            }
            let slot = per_ev.entry(name).or_default();
            slot.0 += 1;
            slot.1 += bytes;
            let all = per_ev_all.entry(name).or_default();
            all.0 += 1;
            all.1 += bytes;
            grand_bytes += bytes;
        }
        let ev: Vec<String> = per_ev
            .iter()
            .map(|(k, (n, b))| format!("{k}={n}/{:.2}GB", *b as f64 / 1_073_741_824.0))
            .collect();
        println!(
            "REPLAY {game}\tfiles={}\tfindings={}\t{}\t| {}",
            paths.len(),
            found.len(),
            top.join(" "),
            ev.join(" ")
        );
    }
    println!(
        "REPLAY TOTAL findings={grand} bytes={:.2}GB",
        grand_bytes as f64 / 1_073_741_824.0
    );
    for (k, (n, b)) in &per_ev_all {
        println!(
            "REPLAY BY-EVIDENCE {k}\t{n}\t{:.2}GB",
            *b as f64 / 1_073_741_824.0
        );
    }
}

// --- GT-465: a few language names among hundreds of siblings ------------

/// Warhammer 40,000: Darktide stores its content by hash, in 256 folders
/// named `00`..`ff`. Four of those names - `ca`, `da`, `de`, `fa` - are also
/// language codes, and the day the dictionary learned `ca` and `fa` the
/// folder crossed the three-language bar and the whole store became a
/// localization: 55 findings turned into 3,605 and 1.28 GB of game data was
/// offered for deletion.
///
/// The hexadecimal alphabet is what makes this inevitable rather than
/// unlucky - a dense fixed alphabet will always spell a few language codes
/// eventually - so the guard is about *share*, not about hex.
#[test]
fn four_language_names_among_two_hundred_and_fifty_six_are_a_hash_tree() {
    let paths: Vec<String> = (0..256)
        .map(|n| format!("bundle\\data\\{n:02x}\\{n:02x}36aa7024366ba7.stream"))
        .collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();

    let found = find_for(&refs);

    assert!(
        found.is_empty(),
        "a content-addressed store is not a language set, got {:?}",
        found
            .iter()
            .map(|(i, f)| (refs[*i], &f.lang_tag))
            .collect::<Vec<_>>()
    );
}

/// The counter-example the ticket requires: an ordinary set of a handful of
/// language subdirectories must still be recognized, or the fix has switched
/// the mechanism off instead of narrowing it. Bare two-letter codes under a
/// folder that says nothing, so the answer rests on the folder family alone.
#[test]
fn a_handful_of_language_folders_is_still_a_family() {
    let found = find_for(&[
        "data\\en\\text.dat",
        "data\\de\\text.dat",
        "data\\fr\\text.dat",
        "data\\uk\\text.dat",
        "data\\pl\\text.dat",
    ]);

    let tags: HashSet<&str> = found.iter().map(|(_, f)| f.lang_tag.as_str()).collect();
    assert_eq!(
        tags,
        ["de", "fr", "pl"].into_iter().collect(),
        "en and uk are kept, the rest is a family: {found:?}"
    );
}

/// The tightest genuine case the library holds, and the one that fixes where
/// the bar may sit: Galactic Civilizations III keeps three language folders
/// among twenty-six. Three in twenty-six is the thinnest real set measured
/// across every folder family in the library, and it has to survive - so the
/// share may not be asked more strictly than about one in nine.
#[test]
fn three_language_folders_among_twenty_six_still_count() {
    let mut paths: Vec<String> = ["fr", "de", "ru"]
        .iter()
        .map(|lang| format!("data\\{lang}\\clip.dat"))
        .collect();
    for n in 0..23 {
        paths.push(format!("data\\cutscene{n:02}\\clip.dat"));
    }
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();

    let found = find_for(&refs);

    let tags: HashSet<&str> = found.iter().map(|(_, f)| f.lang_tag.as_str()).collect();
    assert_eq!(
        tags,
        ["fr", "de", "ru"].into_iter().collect(),
        "three in twenty-six is a real localization: {found:?}"
    );
}

// --- GT-468: a slot that mostly holds something else is not a language set

/// Learn Japanese To Survive ships the hiragana syllabary as voice clips.
/// Nine of its seventy syllables collide with a language code - `da`, `de`,
/// `ge`, `ko`, `no`, `ro`, `ru`, `ta`, `te` - and the shape family saw nine
/// languages varying in one slot and confirmed them, because the sixty-one
/// syllables that produce no dictionary match never join the group at all.
/// The slot has to be judged by everything standing in it.
///
/// The list is the folder as it ships, not a sample: the whole point is what
/// the engine could not see.
#[test]
fn a_slot_that_mostly_holds_syllables_is_not_a_language_set() {
    const SYLLABLES: &str = "a ba be bi bo bu chi da de do e fu ga ge gi go gu ha he hi ho i \
         ji-2 ji ka ke ki ko ku ma me mi mo mu n na ne ni no nu o pa pe pi po pu ra re ri ro ru \
         sa se shi so su ta te to tsu u wa wo ya yo yu za ze zo zu-2 zu";
    let paths: Vec<String> = SYLLABLES
        .split_whitespace()
        .map(|s| format!("www\\audio\\se\\hiragana-female-{s}.ogg"))
        .collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();

    let found = find_for(&refs);

    assert!(
        found.is_empty(),
        "hiragana is not a set of languages, got {:?}",
        found
            .iter()
            .map(|(i, f)| (refs[*i], &f.lang_tag))
            .collect::<Vec<_>>()
    );
}

/// Lambda Wars ships a browser-usage table keyed by *country* as a build
/// dependency: `AD.json`, `AE.json`, ... 241 of them. Country codes and
/// language codes are drawn from the same two-letter alphabet, so about forty
/// of the files match the dictionary - and the other two hundred, which are
/// the answer, were invisible.
#[test]
fn a_slot_of_country_codes_is_not_a_language_set() {
    const COUNTRIES: &str = "AD AE AF AG AI AL alt-af alt-an alt-as alt-eu alt-na alt-oc alt-sa \
         alt-ww AM AO AR AS AT AU AW AX AZ BA BB BD BE BF BG BH BI BJ BM BN BO BR BS BT BW BY BZ \
         CA CD CF CG CH CI CK CL CM CN CO CR CU CV CX CY CZ DE DJ DK DM DO DZ EC EE EG ER ES ET \
         FI FJ FK FM FO FR GA GB GD GE GF GG GH GI GL GM GN GP GQ GR GT GU GW GY HK HN HR HT HU \
         ID IE IL IM IN IQ IR IS IT JE JM JO JP KE KG KH KI KM KN KP KR KW KY KZ LA LB LC LI LK \
         LR LS LT LU LV LY MA MC MD ME MG MH MK ML MM MN MO MP MQ MR MS MT MU MV MW MX MY MZ NA \
         NC NE NF NG NI NL NO NP NR NU NZ OM PA PE PF PG PH PK PL PM PN PR PS PT PW PY QA RE RO \
         RS RU RW SA SB SC SD SE SG SH SI SK SL SM SN SO SR ST SV SY SZ TC TD TG TH TJ TK TL TM \
         TN TO TR TT TV TW TZ UA UG US UY UZ VA VC VE VG VI VN VU WF WS YE YT ZA ZM ZW";
    let paths: Vec<String> = COUNTRIES
        .split_whitespace()
        .map(|c| {
            format!(
                "lambdawars\\ui\\menu_next\\node_modules\\caniuse-db\\region-usage-json\\{c}.json"
            )
        })
        .collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();

    let found = find_for(&refs);

    assert!(
        found.is_empty(),
        "a table of countries is not a localization, got {:?}",
        found
            .iter()
            .map(|(i, f)| (refs[*i], &f.lang_tag))
            .collect::<Vec<_>>()
    );
}

/// The counter-example, and the reason the bar is "more than half" rather
/// than "all": a real per-language set keeps its neighbours.
/// `Voice_english` / `Voice_french` / `Voice_german` / `Voice_polish` sit
/// beside `intro_logo` and `intro_test`, and four languages against two
/// strays is still a family. Bare two-letter codes are used deliberately:
/// they are never trusted on their own, so this set stands or falls on the
/// family and on nothing else.
#[test]
fn a_real_set_survives_a_few_neighbours_in_its_slot() {
    let refs = [
        "cine\\intro_en.bik",
        "cine\\intro_de.bik",
        "cine\\intro_fr.bik",
        "cine\\intro_pl.bik",
        "cine\\intro_it.bik",
        "cine\\intro_logo.bik",
        "cine\\intro_test.bik",
    ];

    let found = find_for(&refs);

    let tags: HashSet<&str> = found.iter().map(|(_, f)| f.lang_tag.as_str()).collect();
    assert_eq!(
        tags,
        ["de", "fr", "pl", "it"].into_iter().collect(),
        "a real four-language set must survive two strays: {found:?}"
    );
}

// --- GT-469: a set must not hang on which side of half an atom falls -----

/// The Tannoy folder held its two Chinese files by accident. `tannoy` is the
/// stem eight of its members share and the plainest sign they belong
/// together, but the shared-position family wrote off any atom carried by
/// more than half the group - so the spelled-out set was left resting on
/// `Tannoy_Chinese_T` happening to share the letter `t` with the short
/// `t_fr` / `t_ge` set.
///
/// That is a verdict decided by arithmetic on the file count: two more `t_*`
/// files tipped `t` over half as well and both Chinese files vanished
/// silently; a third tipped `tannoy` back under half and they returned.
/// Whatever else the folder holds, the answer about these two files must not
/// move.
#[test]
fn the_chinese_pair_holds_however_many_short_names_join_the_folder() {
    let extras = [
        vec![],
        vec![r"Misc\L_Text\Tannoy\t_pl.asr"],
        vec![
            r"Misc\L_Text\Tannoy\t_pl.asr",
            r"Misc\L_Text\Tannoy\t_ru.asr",
        ],
        vec![
            r"Misc\L_Text\Tannoy\t_pl.asr",
            r"Misc\L_Text\Tannoy\t_ru.asr",
            r"Misc\L_Text\Tannoy\t_ja.asr",
        ],
    ];

    for extra in extras {
        let paths: Vec<&str> = TANNOY_FOLDER.iter().copied().chain(extra.clone()).collect();
        let found = flagged_labels(&paths);
        let chinese: Vec<&String> = found
            .iter()
            .filter(|(name, _)| name.starts_with("Tannoy_Chinese"))
            .map(|(name, _)| name)
            .collect();
        assert_eq!(
            chinese.len(),
            2,
            "both Chinese texts must survive {} extra short-set files, got {found:?}",
            extra.len()
        );
    }
}

/// The counter-example that keeps the rule above from becoming "anything the
/// neighbours share is evidence". Wreckfest's `data\menu\textures` holds
/// eleven genuine `startup_screen_3_<lang>_<size>_raw.bmap` screens beside
/// ten `*_bg_*` backgrounds - and `raw` is on every single file in the
/// folder. Supported on `raw` alone the backgrounds would join the set as
/// Bulgarian, and the folder's label distribution would then tip far enough
/// for GT-471 to throw all of it out: eleven real languages lost to one
/// meaningless suffix.
#[test]
fn the_suffix_every_file_carries_supports_nothing() {
    let mut paths: Vec<String> = Vec::new();
    for part in [
        "angle_meter",
        "gearbox",
        "mainmenu",
        "pc_button",
        "result_player",
        "serverbrowser",
        "settings",
        "wf_levelnumber",
        "result_reward_avatar",
        "wf_element_tournament",
    ] {
        paths.push(format!("data\\menu\\textures\\{part}_bg_400x48_raw.bmap"));
    }
    for lang in [
        "de", "es", "fi", "fr", "it", "ja", "ko", "pl", "pt", "ru", "zh",
    ] {
        paths.push(format!(
            "data\\menu\\textures\\startup_screen_3_{lang}_1920x1080_raw.bmap"
        ));
        paths.push(format!(
            "data\\menu\\textures\\startup_screen_3_{lang}_3840x2160_raw.bmap"
        ));
    }
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();

    let found = find_for(&refs);

    assert!(
        !found.iter().any(|(i, _)| refs[*i].contains("_bg_")),
        "a background is not Bulgarian: {:?}",
        found
            .iter()
            .filter(|(i, _)| refs[*i].contains("_bg_"))
            .map(|(i, f)| (refs[*i], &f.lang_tag))
            .collect::<Vec<_>>()
    );
    assert!(
        found
            .iter()
            .filter(|(i, _)| refs[*i].contains("startup_screen"))
            .count()
            >= 18,
        "and the startup screens stay findings: {found:?}"
    );
}

/// Homefront: The Revolution writes Brazilian Portuguese as two words, and
/// once the family above started confirming that folder by its shared
/// `xml`/`dialog` stems the family's own label - plain `pt`, read off the
/// second word - outranked the `brazilian` token that had been getting it
/// right. A wrong label is worse than a missing one: the keep-list is applied
/// to the label, so a player keeping Brazilian Portuguese would have had this
/// file offered for deletion (the GT-455 argument).
#[test]
fn brazilian_portuguese_written_as_two_words_is_not_plain_portuguese() {
    let mut paths = vec![r"localization\brazilian_portuguese_xml.pak".to_string()];
    for lang in [
        "czech", "french", "german", "italian", "japanese", "polish", "russian", "spanish",
    ] {
        paths.push(format!("localization\\{lang}_xml.pak"));
        paths.push(format!("localization\\{lang}_dialog.pak"));
    }
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();

    let found = find_for(&refs);
    let brazilian = found
        .iter()
        .find(|(i, _)| refs[*i].contains("brazilian"))
        .map(|(_, f)| f.lang_tag.as_str());

    assert_eq!(
        brazilian,
        Some("pt-br"),
        "`brazilian_portuguese` is pt-br, not pt: {found:?}"
    );
}

// --- GT-471: a label set whose shape says "naming scheme", not "translation".

/// Bungie writes `sr` into every package name in `packages\`. It is a
/// two-letter code with an asset word beside it, which is exactly the shape
/// the marker rule trusts - and there is no second language anywhere in the
/// folder, which is exactly the shape a language pack never has. 41.42 GB of
/// game packages hung on it.
#[test]
fn one_bare_code_repeated_through_a_folder_is_a_naming_scheme() {
    let paths: Vec<String> = (0..8)
        .map(|n| format!("packages\\w64_sr_audio_02a{n}_0.pkg"))
        .chain((0..6).map(|n| format!("packages\\w64_sr_video_035{n}_1.pkg")))
        .collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();

    let found = find_for(&refs);

    assert!(
        found.is_empty(),
        "a lone two-letter code covering a whole folder is not a language, got {:?}",
        found.iter().map(|(_, f)| &f.lang_tag).collect::<Vec<_>>()
    );
}

/// The counter-example that keeps the rule above from being "one language is
/// never enough": Swelter is a Russian-only Half-Life 2 mod, and its voice
/// packs say `russian` in full. Spelled-out names are self-sufficient
/// evidence, so saturation says nothing about them.
#[test]
fn a_single_language_named_in_full_survives_a_folder_of_its_own() {
    let paths: Vec<String> = (0..8)
        .map(|n| format!("hl2\\hl2_sound_vo_russian_00{n}.vpk"))
        .collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();

    let found = find_for(&refs);

    assert_eq!(
        found.len(),
        8,
        "a Russian-only mod still ships a removable Russian voice pack"
    );
    assert!(found.iter().all(|(_, f)| f.lang_tag == "ru"));
}

/// Red Faction: Armageddon names its cutscenes `..._CS_...` for *cutscene*,
/// and the shared-position family reads nineteen of them as Czech - 1.89 GB
/// of video. The genuine `voices_*.vpp_pc` family sits in the same folder,
/// two files per language, so the set reads `cs:21` against `:2`: a head ten
/// times its own third. The whole set goes, the twenty genuine rows with it -
/// the known and accepted cost of judging a folder rather than a file.
#[test]
fn a_label_that_dwarfs_the_rest_takes_the_whole_folder_with_it() {
    // The folder as the game actually ships it - the cutscene names have to
    // be the real ones, because the family that misreads them is built from
    // how they differ from each other.
    let refs = [
        "build\\pc\\cache\\M01_CS_00.bik",
        "build\\pc\\cache\\M02_CS_01_02_MO_EWalk.bik",
        "build\\pc\\cache\\M03_CS_03_MO_ExoExit.bik",
        "build\\pc\\cache\\M06_CS_05.bik",
        "build\\pc\\cache\\M08_CS_10.bik",
        "build\\pc\\cache\\M09_CS_11.bik",
        "build\\pc\\cache\\M12_CS_13.bik",
        "build\\pc\\cache\\M13_CS_14_MO_WBarge.bik",
        "build\\pc\\cache\\M14_CS_15.bik",
        "build\\pc\\cache\\M14_MO_WFall_CS_17.bik",
        "build\\pc\\cache\\M14_MO_WSeal_CS_16.bik",
        "build\\pc\\cache\\M17_CS_18.bik",
        "build\\pc\\cache\\M17_MO_TheEnd_CS_19.bik",
        "build\\pc\\cache\\Q01_CS_04.bik",
        "build\\pc\\cache\\Q02_CS_04_5.bik",
        "build\\pc\\cache\\Q04_CS_08_08_5.bik",
        "build\\pc\\cache\\Q06_CS_09_MO_Thwart.bik",
        "build\\pc\\cache\\Q07_CS_12.bik",
        "build\\pc\\cache\\Q08_CS_12_5.bik",
        "build\\pc\\cache\\legal_hd-demo.bik",
        "build\\pc\\cache\\legal_hd.bik",
        "build\\pc\\cache\\legal_hd_PS3-demo.bik",
        "build\\pc\\cache\\legal_hd_PS3.bik",
        "build\\pc\\cache\\legal_hd_PS3_no_esrb.bik",
        "build\\pc\\cache\\legal_hd_no_esrb.bik",
        "build\\pc\\cache\\logo_SPIKE.bik",
        "build\\pc\\cache\\logo_SyfyGAMES.bik",
        "build\\pc\\cache\\logo_THQ-V-RFA.bik",
        "build\\pc\\cache\\voiceboot_AR.vpp_pc",
        "build\\pc\\cache\\voiceboot_CZ.vpp_pc",
        "build\\pc\\cache\\voiceboot_DE.vpp_pc",
        "build\\pc\\cache\\voiceboot_ES.vpp_pc",
        "build\\pc\\cache\\voiceboot_FR.vpp_pc",
        "build\\pc\\cache\\voiceboot_IT.vpp_pc",
        "build\\pc\\cache\\voiceboot_JP.vpp_pc",
        "build\\pc\\cache\\voiceboot_KO.vpp_pc",
        "build\\pc\\cache\\voiceboot_PL.vpp_pc",
        "build\\pc\\cache\\voiceboot_RU.vpp_pc",
        "build\\pc\\cache\\voices_AR.vpp_pc",
        "build\\pc\\cache\\voices_CZ.vpp_pc",
        "build\\pc\\cache\\voices_DE.vpp_pc",
        "build\\pc\\cache\\voices_ES.vpp_pc",
        "build\\pc\\cache\\voices_FR.vpp_pc",
        "build\\pc\\cache\\voices_IT.vpp_pc",
        "build\\pc\\cache\\voices_JP.vpp_pc",
        "build\\pc\\cache\\voices_KO.vpp_pc",
        "build\\pc\\cache\\voices_PL.vpp_pc",
        "build\\pc\\cache\\voices_RU.vpp_pc",
    ];

    let found = find_for(&refs);

    assert!(
        !found.iter().any(|(i, _)| refs[*i].ends_with(".bik")),
        "the cutscenes are not Czech, got {:?}",
        found
            .iter()
            .filter(|(i, _)| refs[*i].ends_with(".bik"))
            .map(|(i, f)| (refs[*i], &f.lang_tag))
            .collect::<Vec<_>>()
    );
}

/// The counter-example for the dominance rule. Forza Horizon 5 ships its
/// radio DJ in seven languages, two files each - the flattest set in the
/// library, and the one that proves the threshold is not simply "any
/// unevenness". A stray extra file of one language must not tip it over.
#[test]
fn an_even_set_of_languages_survives_being_judged_as_a_set() {
    let mut paths: Vec<String> = Vec::new();
    for lang in ["BR", "DE", "ES", "FR", "IT", "JP", "KO"] {
        paths.push(format!("media\\Audio\\FMODBanks\\VO_DJ_02_{lang}.bank"));
        paths.push(format!(
            "media\\Audio\\FMODBanks\\VO_DJ_02_{lang}.assets.bank"
        ));
    }
    paths.push("media\\Audio\\FMODBanks\\VO_DJ_03_DE.bank".to_string());
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();

    let found = find_for(&refs);

    let langs: HashSet<&str> = found.iter().map(|(_, f)| f.lang_tag.as_str()).collect();
    assert!(
        langs.len() >= 6,
        "an even seven-language set must survive, kept only {langs:?}"
    );
}

// --- GT-460: a code that means something other than a language ----------

/// War Thunder names its ship models for the navy that sailed them.
/// `content\base\res\ships\` holds 640 of them - `usa` 105, `uk` 105, `ussr`
/// 101, `jap` 95, `ger` 91, `it` 70, `fr` 69 - and five of those seven read
/// as languages. Every guard the engine has says yes: the slot is mostly
/// languages, the members support each other on `battleship` and `class` the
/// way a translated set supports itself on a shared stem, and the label
/// counts are as even as any genuine set in the library. 367 findings and
/// 3.75 GB of ship models, the largest false cluster in the library by size.
///
/// Nothing about the *shape* of this folder separates it from a translation.
/// What separates it is that no language is called `usa` or `ussr` - a
/// nation is content, not localization.
#[test]
fn ships_named_for_the_navy_that_sailed_them_are_not_translations() {
    // Real names off the disk, and all of the same width: a name built
    // differently is not an occupant of the same slot, so the fixture has to
    // keep the nations side by side the way the folder does.
    let refs = [
        r"content\base\res\ships\fr_battleship_dunkerque.grp",
        r"content\base\res\ships\fr_battleship_strasbourg.grp",
        r"content\base\res\ships\fr_cruiser_emile.grp",
        r"content\base\res\ships\ger_battleship_bismarck.grp",
        r"content\base\res\ships\ger_battleship_scharnhorst.grp",
        r"content\base\res\ships\ger_cruiser_nurnberg.grp",
        r"content\base\res\ships\it_battleship_cavour.grp",
        r"content\base\res\ships\it_cruiser_trento.grp",
        r"content\base\res\ships\jap_aircraftcarrier_shokaku.grp",
        r"content\base\res\ships\jap_battleship_yamato.grp",
        r"content\base\res\ships\uk_battleship_hood.grp",
        r"content\base\res\ships\uk_cruiser_belfast.grp",
        r"content\base\res\ships\usa_admirable_class.grp",
        r"content\base\res\ships\usa_aircraftcarrier_enterprise.grp",
        r"content\base\res\ships\ussr_battleship_marat.grp",
        r"content\base\res\ships\ussr_cruiser_kirov.grp",
        // The one name that does twin across the nations, and so proved a
        // "family" existed here in the first place.
        r"content\base\res\ships\fr_pships_weaponry.grp",
        r"content\base\res\ships\ger_pships_weaponry.grp",
        r"content\base\res\ships\it_pships_weaponry.grp",
        r"content\base\res\ships\usa_pships_weaponry.grp",
        r"content\base\res\ships\ussr_pships_weaponry.grp",
    ];

    let found = find_for(&refs);

    assert!(
        found.is_empty(),
        "a navy is not a language, got {:?}",
        found
            .iter()
            .map(|(i, f)| (refs[*i], &f.lang_tag))
            .collect::<Vec<_>>()
    );
}

/// The counter-example, and the reason a country name alone may not convict a
/// slot: Anno 1701 spells American English `usa`. Seven languages and one
/// country name, and the country name is plainly the eighth language - unlike
/// War Thunder's roster, where `jap` stands beside `usa` and the dictionary
/// can read neither as a language.
#[test]
fn a_country_name_among_languages_is_still_a_language() {
    let refs = [
        r"data\loca\selectlanguage_cze.ini",
        r"data\loca\selectlanguage_fra.ini",
        r"data\loca\selectlanguage_ger.ini",
        r"data\loca\selectlanguage_hun.ini",
        r"data\loca\selectlanguage_ita.ini",
        r"data\loca\selectlanguage_pol.ini",
        r"data\loca\selectlanguage_spa.ini",
        r"data\loca\selectlanguage_usa.ini",
    ];

    let found = find_for(&refs);

    assert!(
        found.len() >= 7,
        "a full language list with `usa` in it is a language list, got {:?}",
        found
            .iter()
            .map(|(i, f)| (refs[*i], &f.lang_tag))
            .collect::<Vec<_>>()
    );
}

/// Underrail keeps its text in `data\locale\`, and the word *locale* on the
/// path is what makes every file under it a language file. It is not a
/// translation tree: it is the game's own text database, one folder per
/// creature and one file per field. `id.xnb` is the identifier field, `th`
/// another field, and the five `id_<part>.xnb` beside them are parts of the
/// same one - 6,476 findings and 1.16 GB of it, the largest single false
/// cluster left in the library.
///
/// Two guards had to miss for this to pass. The saturation test asks whether
/// every finding rests on nothing better than a bare two-letter code, and it
/// asked the dictionary alone - which does not carry `id_ap`, so five of the
/// seven files read as stronger evidence than they are. And a folder with two
/// labels was exempt from being judged at all, however lopsided the two were.
#[test]
fn a_field_name_repeated_through_every_folder_of_a_database_is_not_a_language() {
    let mut paths: Vec<String> = Vec::new();
    for creature in ["ag1", "ag2", "arch", "at", "az"] {
        for field in ["id", "id_ap", "id_ep", "id_ra", "id_rs", "id_sm", "th"] {
            paths.push(format!("data\\locale\\creatures\\{creature}\\{field}.xnb"));
        }
    }
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();

    let found = find_for(&refs);

    assert!(
        found.is_empty(),
        "a field name is not Indonesian, got {:?}",
        found
            .iter()
            .map(|(i, f)| (refs[*i], &f.lang_tag))
            .collect::<Vec<_>>()
    );
}

/// The counter-example that keeps the rule above from reading "two languages
/// are never enough". A game translated into two languages ships them evenly,
/// and evenness is what the dominance ratio measures - so the same folder
/// shape, minus the steep head, must keep every finding.
#[test]
fn a_two_language_folder_survives_being_judged_as_a_set() {
    let mut paths: Vec<String> = Vec::new();
    for n in 0..6 {
        paths.push(format!("data\\locale\\sounds_de_{n}.pck"));
        paths.push(format!("data\\locale\\sounds_fr_{n}.pck"));
    }
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();

    let found = find_for(&refs);

    let langs: HashSet<&str> = found.iter().map(|(_, f)| f.lang_tag.as_str()).collect();
    assert_eq!(
        found.len(),
        12,
        "an even two-language set must survive whole, got {langs:?}"
    );

    // And the same folder with the second language reduced to a single file
    // is the Underrail shape again: six against one, on nothing but bare
    // codes. This half is what proves the check above can go red - without
    // it, "twelve findings survived" is equally consistent with the guard
    // never running.
    let mut lopsided: Vec<String> = (0..6)
        .map(|n| format!("data\\locale\\sounds_de_{n}.pck"))
        .collect();
    lopsided.push("data\\locale\\sounds_fr_0.pck".to_string());
    let refs: Vec<&str> = lopsided.iter().map(String::as_str).collect();

    assert!(
        find_for(&refs).is_empty(),
        "six of one label against one of another is a naming scheme"
    );
}

/// A language folder is its own evidence, and its *tail* is the error, not
/// its head: Deadfall Adventures' `Localization\FRA` reads `fr:201` against
/// `id:8 ar:4`, a fiftyfold head that is entirely correct. Judging such a
/// folder by shape would delete the right answer.
#[test]
fn a_folder_that_names_its_language_is_not_judged_by_shape() {
    let mut paths: Vec<String> = (0..12)
        .map(|n| format!("ADVGame\\Localization\\FRA\\ADVGame_{n}.upk"))
        .collect();
    paths.push("ADVGame\\Localization\\FRA\\id_strings.upk".to_string());
    paths.push("ADVGame\\Localization\\FRA\\ar_strings.upk".to_string());
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();

    let found = find_for(&refs);

    assert!(
        found.iter().filter(|(_, f)| f.lang_tag == "fr").count() >= 12,
        "the French folder is French however lopsided its labels look, got {:?}",
        found.iter().map(|(_, f)| &f.lang_tag).collect::<Vec<_>>()
    );
}

/// HUMANKIND, `AssetBundles\`: eleven asset bundles named
/// `<locale>-Localization`, two files each.
const HUMANKIND_BUNDLES: &[&str] = &[
    r"AssetBundles\de-DE-Localization\de-DE-Localization",
    r"AssetBundles\de-DE-Localization\de-de-localization.assetbundle",
    r"AssetBundles\es-ES-Localization\es-ES-Localization",
    r"AssetBundles\es-ES-Localization\es-es-localization.assetbundle",
    r"AssetBundles\fr-FR-Localization\fr-FR-Localization",
    r"AssetBundles\fr-FR-Localization\fr-fr-localization.assetbundle",
    r"AssetBundles\it-IT-Localization\it-IT-Localization",
    r"AssetBundles\it-IT-Localization\it-it-localization.assetbundle",
    r"AssetBundles\ko-KR-Localization\ko-KR-Localization",
    r"AssetBundles\ko-KR-Localization\ko-kr-localization.assetbundle",
    r"AssetBundles\pl-PL-Localization\pl-PL-Localization",
    r"AssetBundles\pl-PL-Localization\pl-pl-localization.assetbundle",
    r"AssetBundles\pt-BR-Localization\pt-BR-Localization",
    r"AssetBundles\pt-BR-Localization\pt-br-localization.assetbundle",
    r"AssetBundles\ru-RU-Localization\ru-RU-Localization",
    r"AssetBundles\ru-RU-Localization\ru-ru-localization.assetbundle",
    r"AssetBundles\tr-TR-Localization\tr-TR-Localization",
    r"AssetBundles\tr-TR-Localization\tr-tr-localization.assetbundle",
    r"AssetBundles\zh-CN-Localization\zh-CN-Localization",
    r"AssetBundles\zh-CN-Localization\zh-cn-localization.assetbundle",
    r"AssetBundles\zh-TW-Localization\zh-TW-Localization",
    r"AssetBundles\zh-TW-Localization\zh-tw-localization.assetbundle",
];

/// GT-472: the same file was labelled a different language from run to run.
///
/// Each of these folders belongs to two name-shape families at once - the
/// eleven-folder set whose token is the whole locale tag, and a ten-folder
/// set where the entire folder name matched as a tag with trailing parts and
/// only the bare prefix survived. Both wrote into the same cell, the later
/// write won, and map iteration order is deliberately different in every
/// process: six consecutive runs over the real game gave `pt` once and
/// `pt-br` five times, with Traditional Chinese flipping to Simplified
/// alongside it.
///
/// Which is why this test runs the analysis forty times rather than once: a
/// single run was green five times in six and proved nothing. The two
/// interesting rows are `pt-BR` and `zh-TW`, the only two folders here whose
/// coarse and fine readings disagree.
///
/// The counter-example is in the same list and is not optional:
/// `de-DE-Localization` has no finer reading to prefer, and must still come
/// back as plain German. Preferring the more specific answer must not invent
/// one where none exists.
#[test]
fn a_locale_folder_reads_the_same_language_in_every_run() {
    let expected: Vec<(String, String)> = [
        ("de-DE-Localization", "de"),
        ("de-de-localization.assetbundle", "de"),
        ("es-ES-Localization", "es"),
        ("es-es-localization.assetbundle", "es"),
        ("fr-FR-Localization", "fr"),
        ("fr-fr-localization.assetbundle", "fr"),
        ("it-IT-Localization", "it"),
        ("it-it-localization.assetbundle", "it"),
        ("ko-KR-Localization", "ko"),
        ("ko-kr-localization.assetbundle", "ko"),
        ("pl-PL-Localization", "pl"),
        ("pl-pl-localization.assetbundle", "pl"),
        ("pt-BR-Localization", "pt-br"),
        ("pt-br-localization.assetbundle", "pt-br"),
        ("ru-RU-Localization", "ru"),
        ("ru-ru-localization.assetbundle", "ru"),
        ("tr-TR-Localization", "tr"),
        ("tr-tr-localization.assetbundle", "tr"),
        ("zh-CN-Localization", "zh-hans"),
        ("zh-TW-Localization", "zh-hant"),
        ("zh-cn-localization.assetbundle", "zh-hans"),
        ("zh-tw-localization.assetbundle", "zh-hant"),
    ]
    .map(|(name, tag)| (name.to_string(), tag.to_string()))
    .to_vec();

    for run in 0..40 {
        assert_eq!(
            flagged_labels(HUMANKIND_BUNDLES),
            expected,
            "run {run} disagreed"
        );
    }
}

/// GT-464: a file that fills a confirmed set's slot with a name the
/// dictionary cannot read is reported, not hidden.
///
/// `sounds_jap.pck` is Japanese - the dictionary carries `jpn` and not `jap`
/// - and before this card it produced nothing at all. Four rows appeared and
/// the fifth file did not, which from the outside is the same picture as a
/// detector that missed the folder entirely. Across the library that silence
/// covered 4,206 files, most of them real languages spelled in a notation
/// the pack does not list (`esn`, `ptb`, `cht`, `zht`, `mex`).
///
/// The two guards are the point of the test, because without them this is
/// the "ride along with no evidence of your own" path that `8b03b91` closed:
///
/// - **width**: `sounds_master.pck` fits between the same literals, but the
///   set spells its languages in three letters and `master` is six. A set
///   that does not agree on the width of its own slot claims nothing at all.
/// - **vocabulary**: `sounds_dub.pck` is three letters in the right place,
///   and `dub` is a word the engine already knows to mean dubbing. A word
///   with a known non-language meaning is not a language going unnamed.
///
/// What no rule can do is separate an unknown language from an ordinary word
/// of the same length - `sfx` sits in this slot in fifteen games. That is
/// exactly why the answer is "undetermined" and not a guess, and why the app
/// keeps such rows out of bulk selection.
#[test]
fn a_slot_filled_with_an_unreadable_name_is_reported_as_undetermined() {
    let paths = [
        r"sound\sounds_fre.pck",
        r"sound\sounds_ger.pck",
        r"sound\sounds_ita.pck",
        r"sound\sounds_spa.pck",
        r"sound\sounds_jap.pck",
        r"sound\sounds_master.pck",
        r"sound\sounds_dub.pck",
    ];

    let labels = flagged_labels(&paths);

    assert!(
        labels.contains(&("sounds_jap.pck".to_string(), "und".to_string())),
        "the unreadable name should be reported as undetermined, got {labels:?}"
    );
    for hidden in ["sounds_master.pck", "sounds_dub.pck"] {
        assert!(
            !labels.iter().any(|(name, _)| name == hidden),
            "{hidden} must not ride along with the set, got {labels:?}"
        );
    }
}

/// The counter-example the ticket requires: a game with no confirmed set of
/// languages must not start reporting "undetermined" on short codes that
/// merely look like one.
///
/// War Thunder's `res\ships\` names its hulls by nation, not by language
/// (`ger_`, `it_`), and Call of Duty's `mp_br_*` / `cp_sv_*` are map and
/// campaign prefixes. Neither directory ever confirms a language family, so
/// there is no slot to fill and nothing to report - the same conclusion the
/// engine already reaches for them today, reached for the same reason.
#[test]
fn a_folder_with_no_confirmed_set_reports_nothing_undetermined() {
    let paths = [
        r"res\ships\ger_bismarck.blk",
        r"res\ships\it_littorio.blk",
        r"res\ships\mp_br_favela.blk",
        r"res\ships\cp_sv_hunted.blk",
        r"res\ships\usa_iowa.blk",
    ];

    let labels = flagged_labels(&paths);

    assert!(
        !labels.iter().any(|(_, tag)| tag == "und"),
        "a folder that never confirmed a language set has no slot to fill, got {labels:?}"
    );
}
