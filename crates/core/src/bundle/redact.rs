//! The one redaction pass every byte of a diagnostic bundle goes through.
//!
//! It runs **last, over already-serialized text**, and that ordering is the
//! whole design rather than an implementation detail. Redacting field by
//! field looks equivalent and is not: `rusqlite::Error` and
//! `std::io::Error` embed full paths in their `Display` output, so an error
//! string carries the account name through a projection that never had a
//! path field to redact. A pass over the finished text has no such blind
//! spot - if the name is in the file, this sees it.
//!
//! Two substitutions, in this order:
//!
//! 1. **Library roots** become `<LIBRARY_1>`, `<LIBRARY_2>`, … The token map
//!    stays in memory and is never written to the bundle: a reader needs to
//!    know that two paths share a root, not where that root is.
//! 2. **The Windows account name** becomes `<USER>`, with no opt-out. It is
//!    matched against the live `%USERPROFILE%` rather than a regex for
//!    `Users\<something>`, so it works whatever the account is called and
//!    cannot false-positive on a game's own asset tree that happens to
//!    contain the word "Users".
//!
//! Longest-first ordering matters between the two: a library root under the
//! user profile (`C:\Users\ann\Games\Steam`) has to become `<LIBRARY_1>`
//! rather than `<USER>\Games\Steam`, or the root tokens stop being stable
//! identifiers.

use std::path::Path;

/// Replaces every trace of who and where the machine is, in text that is
/// already in its final form.
#[derive(Debug, Default)]
pub struct Redactor {
    /// `(needle, replacement)`, applied in order. Built longest-needle-first
    /// so a path contained inside another is not half-substituted.
    rules: Vec<(String, String)>,
}

impl Redactor {
    /// Builds the pass from the library roots in play and the live
    /// `%USERPROFILE%`.
    ///
    /// `user_profile` is taken as a parameter rather than read here so the
    /// tests can state a profile that does not exist on the machine running
    /// them - the alternative is a test that passes or fails depending on
    /// the developer's account name.
    pub fn new(library_roots: &[impl AsRef<Path>], user_profile: Option<&str>) -> Self {
        let mut rules: Vec<(String, String)> = library_roots
            .iter()
            .enumerate()
            .map(|(index, root)| {
                (
                    root.as_ref().to_string_lossy().into_owned(),
                    format!("<LIBRARY_{}>", index + 1),
                )
            })
            .collect();

        if let Some(profile) = user_profile.filter(|profile| !profile.is_empty()) {
            rules.push((profile.to_string(), "<USER_PROFILE>".to_string()));
            // The bare account name too, not only the profile directory: it
            // appears on its own in registry paths, in `Documents and
            // Settings`-era strings, and in a good number of error messages.
            if let Some(name) = Path::new(profile).file_name() {
                let name = name.to_string_lossy();
                if name.len() >= MIN_ACCOUNT_NAME_LEN {
                    rules.push((name.into_owned(), "<USER>".to_string()));
                }
            }
        }

        // Every needle again in its JSON-escaped form. The pass runs over
        // serialized text, and in a JSON section `D:\SteamLibrary` is
        // spelled `D:\\SteamLibrary` - so matching only the plain form
        // silently redacts nothing in exactly the sections that carry the
        // most paths. Found by a test asserting library tokenization, which
        // is also why the account name has its own bare-name rule above:
        // that one happened to catch the escaped case and hid this.
        let escaped: Vec<(String, String)> = rules
            .iter()
            .filter(|(needle, _)| needle.contains('\\'))
            .map(|(needle, replacement)| (needle.replace('\\', r"\\"), replacement.clone()))
            .collect();
        rules.extend(escaped);

        // Longest needle first: `C:\Users\ann\Games` must be consumed before
        // `C:\Users\ann`, or the first substitution leaves a fragment the
        // second one cannot recognize. This also puts each escaped form
        // ahead of its plain twin, which is the order that works.
        rules.sort_by_key(|(needle, _)| std::cmp::Reverse(needle.len()));
        Self { rules }
    }

    /// Applies the pass. Case-insensitive on the needle, because Windows
    /// paths reach us in whatever case the producing API used - `C:\Users`
    /// from one call and `c:\users` from another are the same directory and
    /// must redact identically.
    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (needle, replacement) in &self.rules {
            out = replace_ignore_case(&out, needle, replacement);
        }
        out
    }
}

/// Below this, an account name is too short to replace safely: a two-letter
/// name would rewrite unrelated substrings all over the file (an "ab" inside
/// a rule description, a hash, a language tag) and damage the evidence it is
/// meant to protect. Such a name still never appears through
/// `%USERPROFILE%` itself, which is always replaced whole.
const MIN_ACCOUNT_NAME_LEN: usize = 3;

/// `str::replace`, ignoring ASCII case in the needle. Windows paths are
/// ASCII-cased in the parts that matter here (drive letters, `Users`), and
/// a full Unicode case fold would change the length of the haystack and
/// invalidate the byte offsets this walks.
fn replace_ignore_case(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return haystack.to_string();
    }
    let lower_haystack = haystack.to_ascii_lowercase();
    let lower_needle = needle.to_ascii_lowercase();

    let mut out = String::with_capacity(haystack.len());
    let mut cursor = 0usize;
    while let Some(found) = lower_haystack[cursor..].find(&lower_needle) {
        let start = cursor + found;
        out.push_str(&haystack[cursor..start]);
        out.push_str(replacement);
        cursor = start + needle.len();
    }
    out.push_str(&haystack[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Redactor::new`'s root list is `impl AsRef<Path>`, so an empty
    /// literal cannot infer its element type - this names it once for the
    /// tests that are only about the account name.
    const NO_ROOTS: &[&Path] = &[];

    #[test]
    fn the_account_name_never_survives_a_path() {
        let redactor = Redactor::new(NO_ROOTS, Some(r"C:\Users\anastasia"));

        let out = redactor.apply(r"failed to open C:\Users\anastasia\AppData\gametrimmer.db");

        assert!(!out.contains("anastasia"), "{out}");
        assert!(out.contains("<USER_PROFILE>"), "{out}");
    }

    /// The case this exists for: an error type that embeds a path in its
    /// `Display` output, which field-level redaction would never see.
    #[test]
    fn the_account_name_never_survives_an_error_string() {
        let redactor = Redactor::new(NO_ROOTS, Some(r"C:\Users\anastasia"));
        let err = std::io::Error::other(r"access denied for anastasia");

        let out = redactor.apply(&format!("io error: {err}"));

        assert!(!out.contains("anastasia"), "{out}");
        assert!(out.contains("<USER>"), "{out}");
    }

    #[test]
    fn library_roots_become_stable_tokens() {
        let redactor = Redactor::new(&[Path::new(r"D:\SteamLibrary"), Path::new(r"E:\GOG")], None);

        let out = redactor.apply(r"D:\SteamLibrary\common\A\movie.bik and E:\GOG\B\x.pak");

        assert_eq!(
            out,
            r"<LIBRARY_1>\common\A\movie.bik and <LIBRARY_2>\B\x.pak"
        );
    }

    /// A library inside the user profile must tokenize as the library, not
    /// as the profile - otherwise the root token stops identifying a root.
    #[test]
    fn a_library_under_the_user_profile_still_tokenizes_as_the_library() {
        let redactor = Redactor::new(
            &[Path::new(r"C:\Users\ann\Games\Steam")],
            Some(r"C:\Users\ann"),
        );

        let out = redactor.apply(r"C:\Users\ann\Games\Steam\common\A and C:\Users\ann\Desktop");

        assert!(out.starts_with(r"<LIBRARY_1>\common\A"), "{out}");
        assert!(out.contains(r"<USER_PROFILE>\Desktop"), "{out}");
        assert!(!out.contains("ann"), "{out}");
    }

    /// The pass runs over serialized text, so a path inside a JSON string
    /// arrives with its separators doubled. Matching only the plain form
    /// redacts nothing in precisely the sections that carry the most paths.
    #[test]
    fn a_json_escaped_path_redacts_too() {
        let redactor = Redactor::new(
            &[Path::new(r"D:\SteamLibrary")],
            Some(r"C:\Users\anastasia"),
        );

        let out = redactor
            .apply(r#"{"library":"D:\\SteamLibrary","home":"C:\\Users\\anastasia\\Desktop"}"#);

        assert!(!out.contains("SteamLibrary"), "{out}");
        assert!(!out.contains("anastasia"), "{out}");
        assert!(out.contains("<LIBRARY_1>"), "{out}");
        assert!(out.contains("<USER_PROFILE>"), "{out}");
    }

    /// Windows hands the same directory back in different cases depending on
    /// which API produced the string; both have to redact.
    #[test]
    fn matching_ignores_case_the_way_windows_paths_do() {
        let redactor = Redactor::new(NO_ROOTS, Some(r"C:\Users\Ann"));

        let out = redactor.apply(r"c:\users\ann\AppData");

        assert!(!out.to_ascii_lowercase().contains(r"users\ann"), "{out}");
    }

    /// A name too short to match safely must not be turned into a
    /// find-and-replace over the whole bundle.
    #[test]
    fn a_very_short_account_name_is_not_substituted_on_its_own() {
        let redactor = Redactor::new(NO_ROOTS, Some(r"C:\Users\jo"));

        let out = redactor.apply("the job finished, join the queue");

        assert_eq!(
            out, "the job finished, join the queue",
            "a 2-letter name must not rewrite unrelated words",
        );
    }
}
