//! Trust levels for the language dictionary (see docs/04_implementation_plan.md
//! §5.2). The dictionary *data* — canonical language keys, their aliases per
//! level, the industry-word list, the default keep-list — lives in
//! `l10n_rules.json` at the repo root (parsed by `data.rs`'s
//! `LangPack::builtin`), not in Rust source; this module keeps only the
//! trust-level type the rest of the engine matches on.
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
//!
//! Notable entries worth knowing before editing `l10n_rules.json`:
//! - `en.level_b` includes `"int"` — INTernational, the English master
//!   locale in Unreal-style conventions (`*_LOC_INT.upk`, `Sounds\int\`).
//!   Mapping it to English routes such files into the keep-list instead of
//!   misreading the neighboring token as a removable language (screenshot
//!   report cases 42, 53, 61, 65).
//! - `zh-hans.level_a` lists plain `"chinese"` — it maps to Simplified as
//!   the more common variant; a neighboring "(traditional)" qualifier still
//!   resolves the file correctly at the folder-family level.
//! - `he.level_c` includes `"iw"`, the legacy ISO 639-1 code for Hebrew, but
//!   as a bare 2-letter token it is exactly the kind of ambiguous short code
//!   (studio/project prefixes like "IW_0501_intro.bik") that must never be
//!   self-sufficient — Level C, family-gated like every other two-letter
//!   code.
//! - `industry_words` (Steam-style single-word aliases such as `schinese`,
//!   `koreana`, `latam`) are the words [`super::data::LangData::is_industry_alias`]
//!   treats as localization-industry vocabulary rather than plain
//!   natural-language words — see the 2026-07-16 recalibration.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Level {
    A,
    B,
    C,
}
