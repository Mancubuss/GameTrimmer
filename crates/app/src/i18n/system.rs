//! What language Windows is set to, and how that maps onto the one this app
//! speaks while localizations are frozen (Vikunja #443 tracks unfreezing
//! them).
//!
//! # Why `GetUserPreferredUILanguages` and not the locale
//!
//! There are three plausible answers to "what language is this machine", and
//! they are not the same one:
//!
//! * `GetUserDefaultLocaleName` is the *formatting* locale - dates, decimal
//!   separators, currency. Someone running an English Windows in Ukraine has
//!   every reason to set that to `uk-UA` while wanting an English interface.
//! * `GetSystemDefaultUILanguage` is the machine's language, not the user's.
//! * `GetUserPreferredUILanguages` is the user's own ordered list of
//!   interface languages - exactly the question being asked, and ordered, so
//!   a user who lists Ukrainian first and English second gets Ukrainian while
//!   one who lists German first and English second gets English rather than a
//!   coin toss.
//!
//! # Why the matching is a separate, pure function
//!
//! [`match_preferred`] takes the tags as data. A test cannot set the host's
//! preferred UI languages, so anything asserted against the live Win32 call
//! would only be asserting what this particular developer machine is
//! configured for. The policy is the part worth pinning; the syscall is one
//! line either side of it.

use gametrimmer_core::settings::Lang;

use windows::core::PWSTR;
use windows::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};

/// The UI language to use when the user has not picked one, read from the
/// user's preferred-UI-language list.
///
/// Falls back to [`Lang::En`] when the call fails or Windows prefers nothing
/// this app speaks - the same answer the app defaulted to unconditionally
/// before this existed, so the failure mode is the old behaviour rather than
/// a broken one.
pub fn detect() -> Lang {
    match_preferred(&preferred_ui_languages())
}

/// The first preferred language this app actually speaks, or [`Lang::En`].
///
/// Matched on the tag's primary subtag - the part before the first `-` - so
/// `uk-UA`, `uk-Latn-UA` and a bare `uk` all answer the same. Case-insensitive
/// because BCP-47 is, whatever Windows happens to emit today.
///
/// Order is the point: the list is the user's own ranking, so the first
/// supported entry wins rather than the first entry being required to match.
pub fn match_preferred(tags: &[String]) -> Lang {
    tags.iter()
        .find_map(|tag| primary_subtag_language(tag))
        .unwrap_or(Lang::En)
}

/// The app language a single BCP-47 tag stands for, or `None` when it is one
/// this app does not speak.
fn primary_subtag_language(tag: &str) -> Option<Lang> {
    let primary = tag.split('-').next().unwrap_or_default().to_lowercase();
    match primary.as_str() {
        "en" => Some(Lang::En),
        _ => None,
    }
}

/// The user's preferred UI languages as BCP-47 tags, most preferred first.
///
/// Empty on any failure: an interface language is not worth surfacing an
/// error over, and every caller already has a defensible fallback.
///
/// The two-call shape is the Win32 convention - ask for the buffer size, then
/// ask again with a buffer that big. The result is a double-null-terminated
/// block of UTF-16 strings, which is why the trailing empty slice is dropped
/// rather than parsed.
fn preferred_ui_languages() -> Vec<String> {
    let mut count = 0u32;
    let mut length = 0u32;

    // SAFETY: both out-params are valid for the call's duration, and the
    // buffer is `None` on this pass - the documented way to ask only for the
    // required size, which is what `length` receives.
    if unsafe { GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut length) }
        .is_err()
        || length == 0
    {
        return Vec::new();
    }

    let mut buffer = vec![0u16; length as usize];
    // SAFETY: `buffer` holds exactly `length` UTF-16 units, which is the size
    // the call above asked for, and it outlives the call.
    if unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            Some(PWSTR(buffer.as_mut_ptr())),
            &mut length,
        )
    }
    .is_err()
    {
        return Vec::new();
    }

    split_multi_string(&buffer)
}

/// Splits a double-null-terminated UTF-16 multi-string into its parts.
///
/// Pure so the parsing is testable without Win32 - the block layout is the
/// half of this module most likely to be got subtly wrong (an off-by-one that
/// keeps the terminating empty string looks harmless until it becomes a tag
/// that matches nothing and shadows the real answer).
fn split_multi_string(buffer: &[u16]) -> Vec<String> {
    buffer
        .split(|&unit| unit == 0)
        .filter(|part| !part.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    /// The primary-subtag matcher itself, tested directly rather than through
    /// [`match_preferred`]: with English both the only supported language and
    /// the fallback, `match_preferred`'s return value can no longer tell "this
    /// tag matched" apart from "nothing matched, so it fell back" - only
    /// [`primary_subtag_language`]'s `None`/`Some` distinguishes them.
    #[test]
    fn a_supported_language_is_recognised_with_or_without_a_region() {
        assert_eq!(primary_subtag_language("en-GB"), Some(Lang::En));
        assert_eq!(primary_subtag_language("en"), Some(Lang::En));
    }

    /// BCP-47 is case-insensitive; Windows' casing is not a contract.
    #[test]
    fn matching_ignores_case() {
        assert_eq!(primary_subtag_language("EN-us"), Some(Lang::En));
    }

    /// A language whose tag merely *starts with* a supported one is a
    /// different language: `enm` is not `en`, and a prefix match rather than
    /// a subtag match would claim it. `uk` is simply unsupported while
    /// localizations are frozen (Vikunja #443 tracks unfreezing them).
    #[test]
    fn a_longer_primary_subtag_is_not_a_match() {
        assert_eq!(primary_subtag_language("enm"), None);
        assert_eq!(primary_subtag_language("uk"), None);
    }

    /// The list is the user's own ranking, so the first *supported* entry
    /// wins - the search must not stop at an earlier, unsupported one.
    #[test]
    fn the_first_supported_language_in_the_list_wins() {
        assert_eq!(match_preferred(&tags(&["de-DE", "en-US"])), Lang::En);
    }

    /// Every way the answer can be nothing useful lands on English, which is
    /// what the app did unconditionally before any of this existed.
    #[test]
    fn anything_unsupported_or_missing_falls_back_to_english() {
        assert_eq!(match_preferred(&[]), Lang::En);
        assert_eq!(match_preferred(&tags(&["de-DE", "fr-FR"])), Lang::En);
        assert_eq!(match_preferred(&tags(&[""])), Lang::En);
        assert_eq!(match_preferred(&tags(&["-"])), Lang::En);
        assert_eq!(match_preferred(&tags(&["uk-UA"])), Lang::En);
    }

    /// The Win32 block layout: strings separated by NUL, the whole thing
    /// terminated by a second NUL. The trailing empty part must not survive
    /// as a tag.
    #[test]
    fn the_multi_string_block_splits_into_its_tags() {
        let block: Vec<u16> = "uk-UA\0en-US\0\0".encode_utf16().collect();

        assert_eq!(split_multi_string(&block), tags(&["uk-UA", "en-US"]));
    }

    #[test]
    fn an_empty_multi_string_block_yields_no_tags() {
        assert!(split_multi_string(&[0, 0]).is_empty());
        assert!(split_multi_string(&[]).is_empty());
    }
}
