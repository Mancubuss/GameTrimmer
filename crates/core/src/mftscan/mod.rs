//! Fast, Everything-style volume index scanning.
//!
//! Reads a game library's files straight out of the NTFS Master File Table
//! (MFT) in one linear pass per volume, instead of walking each game's
//! directory tree individually with `readdir`/`FindNextFile` calls. This
//! mirrors the technique the "Everything" search tool uses.
//!
//! This requires raw read access to `\\.\<letter>:`, which in turn requires
//! Administrator privileges (or `SeBackupPrivilege`) - see [`is_available`].
//! When unavailable, [`scan_roots`] returns a clear [`CoreError::Other`] for
//! the affected games so the caller can fall back to
//! [`crate::scanner::scan_games_parallel`] (a regular `walkdir` scan).
//!
//! # Module layout
//! - [`model`]: pure FRN/record data types, no I/O.
//! - [`record`]: pure, `ntfs`-crate-independent parser for a single raw
//!   FILE record's bytes (fixups, attributes, extension-record merging) -
//!   unit tested in depth with synthetic records, no real NTFS volume
//!   required.
//! - [`pathmap`]: pure path reconstruction + root filtering (unit tested
//!   with synthetic data - the part of this module worth testing in depth).
//! - [`aligned`]: sector-aligned `Read + Seek` adapter required for raw
//!   volume reads on Windows (pure logic, unit tested with a mock reader).
//! - [`volume`]: raw volume handle opening (Windows-specific).
//! - [`reader`]: streams the `$MFT`'s `$DATA` attribute sequentially and
//!   feeds it to [`record`] chunk by chunk, building a [`model::FrnMap`].
//!   Only the initial bootstrap (boot sector, locating `$MFT`'s own
//!   record) uses the `ntfs` crate; the chunk-driving control flow itself
//!   is unit tested via a synthetic chunk source.
//!
//! # Panic containment
//! The `ntfs` crate panics (rather than returning `Err`) on some malformed
//! or unusual on-disk structures. A single corrupt/unusual volume must not
//! take down a scan spanning multiple volumes, so [`scan_roots`] runs each
//! volume's scan through [`catch_scan_panic`], converting any panic into an
//! `Err` scoped to just the games on that volume - callers fall back to a
//! regular directory walk for those, exactly as they would for any other
//! scan error.

mod aligned;
mod media;
mod model;
mod pathmap;
mod reader;
mod record;
mod volume;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{CoreError, Result};
use crate::scanner::FileEntry;

pub use media::{media_kind, MediaKind};
pub use volume::{availability, is_available};

/// One progress update emitted during an MFT streaming pass, fired once per
/// chunk of records read and parsed (not once per record - a full-sized
/// `$MFT` has hundreds of thousands of those).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MftProgress {
    /// Which volume this update is for - a [`scan_roots`] call may scan
    /// several volumes in sequence.
    pub volume: char,
    /// Records processed so far on this volume, including this update.
    pub records_done: u64,
    /// Total records on this volume (the divisor for a percentage).
    pub records_total: u64,
}

/// Scans `roots` (game id + absolute install directory) using direct MFT
/// reads. Mirrors [`crate::scanner::scan_games_parallel`]'s per-game result
/// shape so callers can swap between the two implementations transparently.
///
/// Roots are grouped by drive letter so each NTFS volume is read exactly
/// once, no matter how many games in `roots` live on it. If a volume cannot
/// be opened (not NTFS, or insufficient privileges), every game on that
/// volume gets the same descriptive `Err` in its result slot - the caller
/// can then fall back to `scan_games_parallel` for just those games.
///
/// Roots that are not on a lettered local drive (e.g. a UNC path) are
/// silently omitted from the results, since there is no volume to scan;
/// callers should fall back to a regular walk for those too.
///
/// `progress`, if given, is called once per chunk of `$MFT` records read on
/// each volume (see [`MftProgress`]). `cancel`, if given, is checked before
/// each volume is even opened, and again once per chunk while a volume is
/// being read; once set, every not-yet-scanned volume's games get an
/// immediate `Err("cancelled")` instead of being scanned.
pub fn scan_roots(
    roots: &[(i64, PathBuf)],
    mut progress: Option<&mut dyn FnMut(MftProgress)>,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<(i64, Result<Vec<FileEntry>>)>> {
    // NB: `progress.as_deref_mut()` cannot simply be called fresh on every
    // loop iteration below - `&mut dyn FnMut(..)` is invariant, so the
    // borrow checker unifies every iteration's reborrow with the single
    // lifetime from this function's signature, which then conflicts with
    // `progress` going out of scope at the end of this function. Routing
    // the reborrow through a helper function with its own explicit
    // lifetime (see `reborrow_progress`) sidesteps that inference limit.
    let mut by_volume: HashMap<char, Vec<(i64, PathBuf)>> = HashMap::new();

    for (game_id, path) in roots {
        if let Some(letter) = volume_letter(path) {
            by_volume
                .entry(letter)
                .or_default()
                .push((*game_id, path.clone()));
        }
    }

    let mut results = Vec::with_capacity(roots.len());

    for (letter, games) in by_volume {
        if let Some(flag) = cancel {
            if flag.load(Ordering::Relaxed) {
                for (game_id, _) in &games {
                    results.push((*game_id, Err(CoreError::Other("cancelled".to_string()))));
                }
                continue;
            }
        }

        let context = format!("MFT scan of volume {letter}:");
        let progress_ref = reborrow_progress(&mut progress);
        match catch_scan_panic(
            &context,
            std::panic::AssertUnwindSafe(|| scan_volume(letter, &games, progress_ref, cancel)),
        ) {
            Ok(per_game) => results.extend(per_game),
            Err(err) => {
                for (game_id, _) in &games {
                    results.push((*game_id, Err(CoreError::Other(err.to_string()))));
                }
            }
        }
    }

    Ok(results)
}

/// Runs `f`, converting an internal panic into a descriptive
/// [`CoreError::Other`] instead of letting the unwind propagate. See the
/// "Panic containment" section of the module docs for why this exists.
fn catch_scan_panic<F, R>(context: &str, f: F) -> Result<R>
where
    F: FnOnce() -> Result<R> + std::panic::UnwindSafe,
{
    match std::panic::catch_unwind(f) {
        Ok(result) => result,
        Err(payload) => Err(CoreError::Other(format!(
            "{context} panicked: {}",
            // NB: must deref explicitly to `&(dyn Any + Send)` here. Passing
            // `&payload` (a `&Box<dyn Any + Send>`) would let the compiler
            // pick the *outer* unsized coercion - `Box<dyn Any + Send>`
            // itself is `'static` and so has its own blanket `Any` impl -
            // instead of deref-coercing into the inner trait object,
            // silently making every downcast below fail.
            panic_payload_message(&*payload)
        ))),
    }
}

/// Best-effort extraction of a human-readable message from a
/// `catch_unwind` panic payload. Panic payloads are `Box<dyn Any + Send>`
/// with no guaranteed concrete type, but `panic!("...")` / `.unwrap()` /
/// `unreachable!()` (the `ntfs` crate's own panic site included) all use
/// `&str` or `String`, which covers every panic this module needs to
/// report usefully.
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// Reborrows an `Option<&mut dyn FnMut(MftProgress)>` with a fresh, shorter
/// lifetime tied to `progress` itself, rather than the original lifetime it
/// was constructed with.
///
/// This looks like it should be unnecessary - `progress.as_deref_mut()`
/// "ought" to do the same thing - but `&mut dyn Trait` is invariant, and
/// the borrow checker cannot shorten an inferred reborrow's lifetime across
/// loop iterations the way it can for concrete types. Routing the reborrow
/// through a function with an explicit `'a` signature (as opposed to
/// relying on inference at the call site) gives the borrow checker the
/// shorter lifetime it needs, so this can be called fresh on every loop
/// iteration in [`scan_roots`] (and [`reader::drive_scan`]) without the
/// reborrow being forced to live as long as the enclosing function.
pub(crate) fn reborrow_progress<'a>(
    progress: &'a mut Option<&mut dyn FnMut(MftProgress)>,
) -> Option<&'a mut dyn FnMut(MftProgress)> {
    match progress {
        Some(callback) => Some(&mut **callback),
        None => None,
    }
}

/// Scans one NTFS volume for all games in `games`, all of which must be on
/// drive `letter`.
fn scan_volume(
    letter: char,
    games: &[(i64, PathBuf)],
    progress: Option<&mut dyn FnMut(MftProgress)>,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<(i64, Result<Vec<FileEntry>>)>> {
    let mut volume_file = volume::open_volume(letter)?;
    let frn_map = reader::build_frn_map(&mut volume_file, letter, progress, cancel)?;

    let scan_roots: Vec<pathmap::ScanRoot> = games
        .iter()
        .filter_map(|(game_id, path)| {
            root_rel_to_volume(path, letter).map(|root_rel| pathmap::ScanRoot {
                game_id: *game_id,
                root_rel,
            })
        })
        .collect();

    let by_game = pathmap::scan_frn_map(&frn_map, &scan_roots);
    Ok(by_game
        .into_iter()
        .map(|(game_id, entries)| (game_id, Ok(entries)))
        .collect())
}

/// Extracts the drive letter of an absolute Windows path, e.g. `'G'` from
/// `G:\SteamLibrary\Game`. Returns `None` for anything without a `<letter>:`
/// prefix (UNC paths, relative paths, ...).
///
/// Public so callers (the app crate's scan-routing logic) can group game
/// roots by volume themselves - e.g. to call [`is_available`] once per
/// volume before deciding whether to hand a batch of roots to
/// [`scan_roots`] at all.
pub fn volume_letter(path: &Path) -> Option<char> {
    let s = path.to_str()?;
    let mut chars = s.chars();
    let letter = chars.next()?;
    if letter.is_ascii_alphabetic() && chars.next() == Some(':') {
        Some(letter.to_ascii_uppercase())
    } else {
        None
    }
}

/// Strips the `<letter>:` prefix and any leading separator from `path`,
/// returning the remainder with `\` separators. Returns `None` if `path` is
/// not on `letter`.
fn root_rel_to_volume(path: &Path, letter: char) -> Option<String> {
    let s = path.to_str()?;
    if volume_letter(path)? != letter {
        return None;
    }
    // Safe to byte-slice at index 2: `volume_letter` already confirmed the
    // first two characters are an ASCII letter followed by ':', both
    // single-byte in UTF-8.
    let rest = s[2..].trim_start_matches(['\\', '/']);
    Some(rest.replace('/', "\\"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_letter_extracts_uppercase_drive_letter() {
        assert_eq!(volume_letter(Path::new(r"g:\SteamLibrary")), Some('G'));
        assert_eq!(volume_letter(Path::new(r"C:\Games")), Some('C'));
    }

    #[test]
    fn volume_letter_none_for_unc_or_relative_paths() {
        assert_eq!(volume_letter(Path::new(r"\\server\share\Games")), None);
        assert_eq!(volume_letter(Path::new("relative/path")), None);
    }

    #[test]
    fn root_rel_to_volume_strips_drive_and_normalizes_separators() {
        assert_eq!(
            root_rel_to_volume(Path::new(r"G:\SteamLibrary\HalfLife"), 'G'),
            Some("SteamLibrary\\HalfLife".to_string())
        );
    }

    #[test]
    fn root_rel_to_volume_none_for_wrong_drive() {
        assert_eq!(
            root_rel_to_volume(Path::new(r"G:\SteamLibrary\HalfLife"), 'D'),
            None
        );
    }

    #[test]
    fn catch_scan_panic_passes_through_ok() {
        let result: Result<i32> = catch_scan_panic("ctx", || Ok(42));
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn catch_scan_panic_passes_through_err_unchanged() {
        let result: Result<i32> = catch_scan_panic("ctx", || {
            Err(CoreError::Other("underlying failure".to_string()))
        });
        let err = result.unwrap_err();
        assert_eq!(err.to_string(), "underlying failure");
    }

    #[test]
    fn catch_scan_panic_converts_panic_to_err() {
        // Silence the default panic hook for just this test so `cargo test`
        // output stays clean - the panic below is expected and handled.
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let result: Result<()> = catch_scan_panic("MFT scan of volume X:", || {
            panic!("internal error: entered unreachable code: boom");
        });

        std::panic::set_hook(previous_hook);

        let err = result.unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("MFT scan of volume X:"),
            "message should include the context: {message}"
        );
        assert!(
            message.contains("boom"),
            "message should include the panic payload: {message}"
        );
    }

    #[test]
    fn catch_scan_panic_handles_non_string_payload() {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let result: Result<()> = catch_scan_panic("ctx", || {
            std::panic::panic_any(42i32);
        });

        std::panic::set_hook(previous_hook);

        let err = result.unwrap_err();
        assert!(err.to_string().contains("non-string panic payload"));
    }
}
