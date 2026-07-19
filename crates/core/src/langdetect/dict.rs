//! Static language dictionary: canonical language keys plus their aliases,
//! grouped by trust level (see docs/04_implementation_plan.md §5.2).
//!
//! - Level A (`level_a`): self-sufficient — full English/native names, Steam
//!   folder names (`schinese`, `koreana`, `latam`, ...), and common
//!   region-tagged forms (`pt-br`, `zh-cn`, `spanish(spain)`, ...).
//! - Level B (`level_b`): three-letter ISO 639-2/3 codes — need a positive
//!   context marker nearby to be trusted.
//! - Level C (`level_c`): two-letter ISO 639-1 codes — never trusted alone,
//!   only as part of a confirmed language family (see `family.rs`).
//!
//! The dictionary is intentionally hand-curated rather than derived from a
//! generic `xx-YY` regex: enumerating the region tags actually seen in game
//! localization folders is safer (zero false positives from things like
//! "up-to" or "re-do" matching a loose 2-3+2 letter pattern) at a modest cost
//! in recall for obscure locales.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Level {
    A,
    B,
    C,
}

pub struct LangEntry {
    pub key: &'static str,
    pub level_a: &'static [&'static str],
    pub level_b: &'static [&'static str],
    pub level_c: &'static [&'static str],
}

/// Default keep-list: canonical keys that `LangDetector::new()` never flags.
pub const KEEP_DEFAULT: &[&str] = &["uk", "en"];

pub static LANGS: &[LangEntry] = &[
    LangEntry {
        key: "en",
        level_a: &[
            "english", "en-us", "en-gb", "en-uk", "en-au", "en-ca", "en-ie", "en-nz", "en-za",
        ],
        // "int" = INTernational, the English master locale in Unreal-style
        // conventions (`*_LOC_INT.upk`, `UnrealEd.INT.xaml`, `Sounds\int\`).
        // Mapping it to English routes such files into the keep-list instead
        // of misreading the neighboring token as a removable language
        // (screenshot report cases 42, 53, 61, 65).
        level_b: &["eng", "int"],
        level_c: &["en"],
    },
    LangEntry {
        key: "uk",
        level_a: &["ukrainian", "ukrainska", "українська", "uk-ua", "ua"],
        level_b: &["ukr"],
        level_c: &["uk"],
    },
    LangEntry {
        key: "fr",
        level_a: &[
            "french",
            "francais",
            "français",
            "french(france)",
            "fr-fr",
            "fr-be",
            "fr-ch",
        ],
        level_b: &["fra", "fre"],
        level_c: &["fr"],
    },
    LangEntry {
        key: "de",
        level_a: &["german", "deutsch", "de-de", "de-at", "de-ch"],
        level_b: &["deu", "ger"],
        level_c: &["de", "ge"],
    },
    LangEntry {
        key: "es",
        level_a: &["spanish", "espanol", "español", "spanish(spain)", "es-es"],
        level_b: &["spa"],
        level_c: &["es", "sp"],
    },
    LangEntry {
        key: "es-419",
        level_a: &[
            "latam",
            "spanish(latinamerica)",
            "spanish(mexico)",
            "es-mx",
            "es-419",
            "es-ar",
            "es-co",
            "es-cl",
        ],
        level_b: &[],
        level_c: &[],
    },
    LangEntry {
        key: "pt",
        level_a: &["portuguese", "portugues", "português", "pt-pt"],
        level_b: &["por"],
        level_c: &["pt"],
    },
    LangEntry {
        key: "pt-br",
        level_a: &["brazilian", "portuguese(brazil)", "pt-br"],
        level_b: &["bra"],
        level_c: &["br"],
    },
    LangEntry {
        key: "it",
        level_a: &["italian", "italiano", "it-it"],
        level_b: &["ita"],
        level_c: &["it"],
    },
    LangEntry {
        key: "ru",
        level_a: &["russian", "russkiy", "русский", "ru-ru"],
        level_b: &["rus"],
        level_c: &["ru"],
    },
    LangEntry {
        key: "pl",
        level_a: &["polish", "polski", "pl-pl"],
        level_b: &["pol"],
        level_c: &["pl"],
    },
    LangEntry {
        key: "cs",
        level_a: &["czech", "cestina", "čeština", "cs-cz"],
        level_b: &["ces", "cze", "czch"],
        level_c: &["cs", "cz"],
    },
    LangEntry {
        key: "ja",
        level_a: &["japanese", "nihongo", "ja-jp"],
        level_b: &["jpn"],
        level_c: &["ja", "jp"],
    },
    LangEntry {
        key: "ko",
        level_a: &["korean", "koreana", "hangugeo", "ko-kr"],
        level_b: &["kor"],
        level_c: &["ko", "kr"],
    },
    LangEntry {
        key: "zh-hans",
        level_a: &[
            // Plain "chinese" maps to Simplified as the more common
            // variant; a neighboring "(traditional)" qualifier still
            // resolves the file correctly at the folder-family level.
            "chinese",
            "schinese",
            "simplifiedchinese",
            "chinesesimplified",
            "zh-cn",
            "zh-hans",
            "zh-sg",
        ],
        level_b: &["chn"],
        level_c: &["zh"],
    },
    LangEntry {
        key: "zh-hant",
        level_a: &[
            "tchinese",
            "traditionalchinese",
            "chinesetraditional",
            "zh-tw",
            "zh-hant",
            "zh-hk",
            "zh-mo",
        ],
        level_b: &[],
        level_c: &[],
    },
    LangEntry {
        key: "tr",
        level_a: &["turkish", "turkce", "türkçe", "tr-tr"],
        level_b: &["tur"],
        level_c: &["tr"],
    },
    LangEntry {
        key: "ar",
        level_a: &["arabic", "alarabiya", "ar-sa", "ar-eg", "ar-ae"],
        level_b: &["ara"],
        level_c: &["ar"],
    },
    LangEntry {
        key: "th",
        level_a: &["thai", "phasathai", "th-th"],
        level_b: &["tha"],
        level_c: &["th"],
    },
    LangEntry {
        key: "vi",
        level_a: &["vietnamese", "tiengviet", "vi-vn"],
        level_b: &["vie"],
        level_c: &["vi"],
    },
    LangEntry {
        key: "hu",
        level_a: &["hungarian", "magyar", "hu-hu"],
        level_b: &["hun"],
        level_c: &["hu"],
    },
    LangEntry {
        key: "nl",
        level_a: &["dutch", "nederlands", "nl-nl", "nl-be"],
        level_b: &["nld", "dut"],
        level_c: &["nl", "du"],
    },
    LangEntry {
        key: "da",
        level_a: &["danish", "dansk", "da-dk"],
        level_b: &["dan"],
        level_c: &["da"],
    },
    LangEntry {
        key: "no",
        level_a: &["norwegian", "norsk", "nb-no", "nn-no", "no-no"],
        level_b: &["nor", "nob", "nno"],
        level_c: &["no"],
    },
    LangEntry {
        key: "sv",
        level_a: &["swedish", "svenska", "sv-se"],
        level_b: &["swe"],
        level_c: &["sv"],
    },
    LangEntry {
        key: "fi",
        level_a: &["finnish", "suomi", "fi-fi"],
        level_b: &["fin"],
        level_c: &["fi"],
    },
    LangEntry {
        key: "el",
        level_a: &["greek", "ellinika", "el-gr"],
        level_b: &["ell", "gre"],
        level_c: &["el"],
    },
    LangEntry {
        key: "ro",
        level_a: &["romanian", "romana", "română", "ro-ro"],
        level_b: &["ron", "rum"],
        level_c: &["ro"],
    },
    LangEntry {
        key: "bg",
        level_a: &["bulgarian", "balgarski", "bg-bg"],
        level_b: &["bul"],
        level_c: &["bg"],
    },
    LangEntry {
        key: "hr",
        level_a: &["croatian", "hrvatski", "hr-hr"],
        level_b: &["hrv"],
        level_c: &["hr"],
    },
    LangEntry {
        key: "sr",
        level_a: &["serbian", "srpski", "sr-rs"],
        level_b: &["srp"],
        level_c: &["sr"],
    },
    LangEntry {
        key: "sk",
        level_a: &["slovak", "slovencina", "slovenčina", "sk-sk"],
        level_b: &["slk", "slo"],
        level_c: &["sk"],
    },
    LangEntry {
        key: "sl",
        level_a: &["slovenian", "slovenscina", "slovenščina", "sl-si"],
        level_b: &["slv"],
        level_c: &["sl"],
    },
    LangEntry {
        key: "id",
        level_a: &["indonesian", "bahasaindonesia", "id-id"],
        level_b: &["ind"],
        level_c: &["id"],
    },
    LangEntry {
        key: "hi",
        level_a: &["hindi", "hi-in"],
        level_b: &["hin"],
        level_c: &["hi"],
    },
    LangEntry {
        key: "he",
        level_a: &["hebrew", "ivrit", "he-il"],
        level_b: &["heb"],
        // "iw" is the legacy ISO 639-1 code for Hebrew, but as a bare
        // 2-letter token it is exactly the kind of ambiguous short code
        // (studio/project prefixes like "IW_0501_intro.bik") that must
        // never be self-sufficient — Level C, family-gated like every
        // other two-letter code.
        level_c: &["he", "iw"],
    },
];

/// Steam-style single-word aliases that exist only as localization-industry
/// vocabulary (see [`is_industry_alias`]).
pub(super) const INDUSTRY_WORDS: &[&str] = &[
    "schinese",
    "tchinese",
    "koreana",
    "latam",
    "simplifiedchinese",
    "traditionalchinese",
    "chinesesimplified",
    "chinesetraditional",
];
