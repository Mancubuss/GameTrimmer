//! Normalizes a filesystem path so that different spellings of the same
//! directory - different case, a trailing separator, an old 8.3 short name,
//! forward vs. backward slashes - produce byte-identical output.
//!
//! Exists for GT-75: the single-instance guard (`single_instance`) derives a
//! Win32 mutex name from the portable install directory, and Windows mutex
//! names are case-sensitive. Without this, launching the same portable
//! folder as `E:\Games\GameTrimmer` and `e:\games\gametrimmer\` (or via an
//! 8.3 alias like `E:\GAMETR~1`) would silently mint two different mutex
//! names, and the whole guard would do nothing for exactly the case it
//! exists to catch. It is deliberately a standalone module rather than
//! folded into `single_instance` - path normalization is a general
//! filesystem question, useful anywhere two spellings of one directory need
//! to compare equal, not something specific to single-instance detection.

use std::io;
use std::path::{Path, PathBuf};

/// Resolves `path` to its canonical on-disk form.
///
/// Built on `dunce::canonicalize` (already a dependency here - see
/// `worker::scan::canonical_mismatch`, which uses it for the same
/// case-and-junction-insensitive comparison over install directories)
/// rather than `std::fs::canonicalize` directly. Both do the same
/// underlying Windows resolution - `GetFinalPathNameByHandleW`, which
/// consults the *live* filesystem rather than just rewriting the string, so
/// it: resolves case to whatever the actual directory entry was created
/// with (two differently-cased spellings of the same directory always
/// resolve to the identical string); expands an 8.3 short name
/// (`PROGRA~1`) to its long form; normalizes slash direction; and drops a
/// trailing separator. `std::fs::canonicalize` additionally prefixes the
/// result with the `\\?\` verbatim marker even for an ordinary drive path;
/// `dunce::canonicalize` strips that back off whenever the plain form
/// round-trips safely, so a caller comparing two canonicalizations of the
/// same directory - one reached through a verbatim-prefixed spelling, one
/// through a plain one - still gets matching output. Paths that genuinely
/// need the verbatim form to be expressed (UNC shares, paths beyond the
/// traditional ~260-character limit) keep whatever prefix `dunce` decides
/// is required; it never fails to strip one and thereby produce two
/// different "normal" forms for the same target, which is the property this
/// function actually needs - it does not need every input to end up prefix-
/// free, only for one target to always normalize to one string.
///
/// Fails if `path` cannot be resolved - it does not exist, or the process
/// lacks permission to query it. There is no meaningful "normalized form"
/// of a path that cannot be looked up, so callers get the `io::Error`
/// rather than a guessed-at fallback.
pub fn normalize_dir(path: &Path) -> io::Result<PathBuf> {
    dunce::canonicalize(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    /// Two spellings that differ only in case must normalize identically -
    /// the exact failure mode GT-75 was filed over: a case-sensitive Win32
    /// mutex name derived straight from an un-normalized path would treat
    /// `C:\Games\GameTrimmer` and `C:\GAMES\GAMETRIMMER` as two different
    /// directories and let two copies run side by side undetected.
    #[test]
    fn case_differences_normalize_identically() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let lower = dir.path().join("case_probe");
        fs::create_dir(&lower).expect("create subdir");

        let uppercase_spelling = PathBuf::from(lower.to_string_lossy().to_uppercase());

        let normalized_lower = normalize_dir(&lower).expect("normalize lowercase spelling");
        let normalized_upper =
            normalize_dir(&uppercase_spelling).expect("normalize uppercase spelling");
        assert_eq!(normalized_lower, normalized_upper);
    }

    /// A trailing separator is not part of a directory's identity.
    #[test]
    fn trailing_separator_normalizes_identically() {
        let dir = tempfile::tempdir().expect("create temp dir");

        let without_slash = dir.path().to_path_buf();
        let with_slash = PathBuf::from(format!("{}\\", dir.path().display()));

        let normalized_without = normalize_dir(&without_slash).expect("normalize bare path");
        let normalized_with =
            normalize_dir(&with_slash).expect("normalize trailing-separator path");
        assert_eq!(normalized_without, normalized_with);
    }

    /// An 8.3 short-name alias (`GAMETR~1`) must resolve to the same
    /// directory as its long form. Short-name generation is a per-volume
    /// Windows setting (`fsutil 8dot3name`) that is on by default but can be
    /// switched off; if this machine's temp volume has it disabled,
    /// `GetShortPathNameW` hands back the long name unchanged and there is
    /// nothing meaningful left to assert, so the test logs and returns
    /// rather than failing on an environment it cannot control.
    #[test]
    fn eight_dot_three_short_name_normalizes_identically() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let long_name_dir = dir.path().join("a-rather-long-directory-name-for-8dot3");
        fs::create_dir(&long_name_dir).expect("create subdir");

        let Some(short) = short_path_name(&long_name_dir) else {
            eprintln!(
                "skipping: could not query an 8.3 short name for {}",
                long_name_dir.display()
            );
            return;
        };
        if short == long_name_dir {
            eprintln!(
                "skipping: 8.3 short-name generation appears disabled on this volume \
                 (fsutil 8dot3name) - {} has no distinct short alias",
                long_name_dir.display()
            );
            return;
        }

        let normalized_long = normalize_dir(&long_name_dir).expect("normalize long spelling");
        let normalized_short = normalize_dir(&short).expect("normalize 8.3 spelling");
        assert_eq!(normalized_long, normalized_short);
    }

    /// `GetShortPathNameW` wrapped just for this test - the app itself never
    /// needs to *produce* an 8.3 name, only to normalize away one a user's
    /// launch happened to be spelled with. Returns `None` on any failure
    /// (rather than propagating an error) since the caller treats "could not
    /// get a short name" and "no short name exists" the same way: skip.
    fn short_path_name(path: &Path) -> Option<PathBuf> {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::GetShortPathNameW;

        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        // SAFETY: `wide` is a null-terminated UTF-16 buffer alive for both
        // calls below. The first call passes no output buffer to ask only
        // for the required length (the documented way to size the second
        // call); the second call's buffer is sized to exactly that length
        // and stays valid for the whole call.
        let needed = unsafe { GetShortPathNameW(PCWSTR::from_raw(wide.as_ptr()), None) };
        if needed == 0 {
            return None;
        }

        let mut buffer = vec![0u16; needed as usize];
        // SAFETY: see above; `buffer` holds exactly `needed` UTF-16 units.
        let written =
            unsafe { GetShortPathNameW(PCWSTR::from_raw(wide.as_ptr()), Some(&mut buffer)) };
        if written == 0 {
            return None;
        }
        buffer.truncate(written as usize);
        Some(PathBuf::from(std::ffi::OsString::from_wide(&buffer)))
    }
}
