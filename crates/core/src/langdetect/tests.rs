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
