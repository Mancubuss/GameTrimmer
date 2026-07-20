//! Pure routing logic for choosing the MFT-index scan path vs. a regular
//! `walkdir` scan, per game install root.
//!
//! Everything in this module is decided from plain data gathered by the
//! caller elsewhere (elevation status, drive-letter parsing, per-volume
//! `mftscan::is_available` results, canonicalization outcomes, and the
//! actual `mftscan::scan_roots` results) - no filesystem, registry, or
//! privilege-token access happens here, which is what makes it unit
//! testable without a real NTFS volume or Administrator rights.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gametrimmer_core::mftscan::MediaKind;
use gametrimmer_core::settings::ScanRouting;

/// One game root as input to the first routing pass, before any MFT scan
/// has been attempted.
#[derive(Debug, Clone)]
pub struct RootCheck {
    pub game_id: i64,
    pub install_dir: PathBuf,
    /// The drive letter of `install_dir`'s *nominal* path (e.g. `Some('G')`
    /// for `G:\SteamLibrary\...`), or `None` for a UNC path or anything else
    /// without a `<letter>:` prefix.
    pub volume_letter: Option<char>,
    /// `true` when canonicalizing `install_dir` resolves to a different
    /// path (a junction, symlink, mount point, or `subst` drive), or when
    /// canonicalization itself failed. Both cases get the same safe
    /// treatment: the nominal path cannot be trusted to be a plain subtree
    /// of its volume, so it must be walked directly.
    pub canonical_mismatch: bool,
}

/// Why a root is being scanned with `walkdir` instead of the MFT index.
/// Carried only for diagnostics/tests right now - callers don't currently
/// need to distinguish these beyond "not MFT".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkdirReason {
    /// The process is not running elevated, so no volume can be opened for
    /// raw MFT reads at all.
    NotElevated,
    /// `install_dir` is not on a lettered local drive (e.g. a UNC path).
    NoVolumeLetter,
    /// `mftscan::is_available` returned `false` for this root's volume.
    VolumeUnavailable,
    /// This root's volume reports no seek penalty (SSD/NVMe): a directory
    /// walk of just the library subtrees beats reading the entire volume's
    /// `$MFT` there, even on a cold cache - measured at ~40x on a real
    /// machine (mft_bench: 0.6s cold walkdir vs 26s MFT on an SSD volume).
    SsdVolume,
    /// The nominal path resolves elsewhere on disk (junction, symlink,
    /// mount point, or `subst` drive) - only a direct walk sees its real
    /// contents.
    CanonicalMismatch,
    /// The MFT scan attempt for this root returned an error (either the
    /// whole volume failed to open, or something about this specific root).
    MftFailed,
    /// The MFT scan returned zero files for this root, but the root is not
    /// actually empty on disk - treated as a scan failure rather than a
    /// genuinely empty game folder.
    MftEmptyOnNonEmptyDisk,
    /// `ScanRouting::ForceWalkdir` is in effect - the MFT path (and its
    /// volume probing) is skipped entirely for every root, regardless of
    /// elevation, volume letter, or media type.
    ForcedBySetting,
}

/// Where a root ends up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanRoute {
    Mft,
    Walkdir(WalkdirReason),
}

/// Whether the MFT path is worth taking on a volume of this media kind.
/// The MFT path is *correct* on any NTFS volume; this is purely a speed
/// call (see [`WalkdirReason::SsdVolume`]). `Unknown` keeps the MFT path:
/// it is the behavior the user explicitly opted into via elevation, and it
/// is never wrong - merely not always fastest.
pub fn mft_worthwhile(media: MediaKind) -> bool {
    match media {
        MediaKind::SeekPenalty | MediaKind::Unknown => true,
        MediaKind::NoSeekPenalty => false,
    }
}

/// First routing pass, before any MFT scan is attempted: decides whether a
/// root is even a *candidate* for the MFT path. Only roots that come back
/// [`ScanRoute::Mft`] here are worth passing into `mftscan::scan_roots` at
/// all; the rest already have a definitive `walkdir` reason and don't need
/// the (comparatively expensive) MFT pass to touch them.
///
/// `volume_ssd` is consulted before `volume_available`: an SSD volume is
/// routed to walkdir without ever being probed for raw-open availability,
/// so its absence from `volume_available` is expected and must not read as
/// "unavailable".
///
/// `mode` (the persisted `scan_routing` setting) only ever narrows or widens
/// which routes are *considered* - it never bypasses a correctness gate:
/// - `ScanRouting::ForceWalkdir` short-circuits every root to
///   [`WalkdirReason::ForcedBySetting`] before any other check runs - no
///   elevation, volume-letter, or media-type check is even consulted.
/// - `ScanRouting::ForceMft` skips only the [`WalkdirReason::SsdVolume`]
///   speed heuristic; `NotElevated`, `NoVolumeLetter`, `CanonicalMismatch`,
///   and `VolumeUnavailable` all still apply exactly as under `Auto`.
pub fn initial_route(
    elevated: bool,
    mode: ScanRouting,
    check: &RootCheck,
    volume_available: &HashMap<char, bool>,
    volume_ssd: &HashMap<char, bool>,
) -> ScanRoute {
    if mode == ScanRouting::ForceWalkdir {
        return ScanRoute::Walkdir(WalkdirReason::ForcedBySetting);
    }
    if !elevated {
        return ScanRoute::Walkdir(WalkdirReason::NotElevated);
    }
    let Some(letter) = check.volume_letter else {
        return ScanRoute::Walkdir(WalkdirReason::NoVolumeLetter);
    };
    if check.canonical_mismatch {
        return ScanRoute::Walkdir(WalkdirReason::CanonicalMismatch);
    }
    if mode != ScanRouting::ForceMft && volume_ssd.get(&letter).copied().unwrap_or(false) {
        return ScanRoute::Walkdir(WalkdirReason::SsdVolume);
    }
    if volume_available.get(&letter).copied().unwrap_or(false) {
        ScanRoute::Mft
    } else {
        ScanRoute::Walkdir(WalkdirReason::VolumeUnavailable)
    }
}

/// Distinct volume letters worth querying `mftscan::is_available` for: those
/// with at least one root that is elevated-eligible, has a drive letter, and
/// has no canonicalization mismatch. Roots already decided otherwise (not
/// elevated, no drive letter, canonical mismatch) don't need their volume
/// probed at all, so this can save an `is_available` call (itself a raw
/// volume open) per volume that turns out not to matter.
///
/// `ScanRouting::ForceWalkdir` always returns empty: every root is routed to
/// walkdir by [`initial_route`] before volume type ever matters, so no
/// volume needs probing at all under that mode.
pub fn volumes_to_check(elevated: bool, mode: ScanRouting, checks: &[RootCheck]) -> Vec<char> {
    if !elevated || mode == ScanRouting::ForceWalkdir {
        return Vec::new();
    }
    let mut letters: Vec<char> = checks
        .iter()
        .filter(|c| !c.canonical_mismatch)
        .filter_map(|c| c.volume_letter)
        .collect();
    letters.sort_unstable();
    letters.dedup();
    letters
}

/// Second routing pass, after `mftscan::scan_roots` has actually been tried
/// for a candidate root: folds in the scan's own success/failure and the
/// "empty result but non-empty on disk" rule. `nonempty_on_disk` is only
/// meaningful (and only needs to have been computed by the caller) when
/// `entries_empty` is `true`.
pub fn finalize_mft_result(mft_ok: bool, entries_empty: bool, nonempty_on_disk: bool) -> ScanRoute {
    if !mft_ok {
        return ScanRoute::Walkdir(WalkdirReason::MftFailed);
    }
    if entries_empty && nonempty_on_disk {
        return ScanRoute::Walkdir(WalkdirReason::MftEmptyOnNonEmptyDisk);
    }
    ScanRoute::Mft
}

/// Case-insensitive path comparison used to detect junctions, symlinks,
/// mount points, and `subst` drives: if canonicalizing a root's nominal path
/// resolves to something other than the nominal path itself (compared this
/// way), the root cannot be trusted to be a plain subtree of its volume, and
/// must be walked directly instead of resolved through the MFT index.
pub fn paths_case_insensitively_equal(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/// Decides whether the startup modal offering a UAC relaunch is worth
/// showing at all: only when elevating would actually change which scan path
/// gets used for at least one game library. Pure - the caller (`app.rs`)
/// gathers `volume_media` by resolving each library's drive letter and
/// probing it with `mftscan::media_kind` (both cheap, unelevated-friendly
/// operations - see that module's docs).
///
/// `volume_media` holds one entry per *distinct* drive letter among the
/// user's game libraries (not one per library) - duplicates don't change the
/// outcome, so the caller is free to dedup or not.
///
/// - [`ScanRouting::ForceWalkdir`]: never show the prompt - the MFT path is
///   skipped entirely regardless of elevation (mirrors [`initial_route`]'s
///   own short-circuit), so elevating would not help.
/// - [`ScanRouting::ForceMft`]: show the prompt whenever at least one library
///   has a drive letter at all - the user explicitly opted into MFT even on
///   SSDs, so unlike `Auto`, media kind is irrelevant here.
/// - [`ScanRouting::Auto`]: show the prompt only if at least one volume is
///   not [`MediaKind::NoSeekPenalty`] - i.e. [`MediaKind::SeekPenalty`] or
///   [`MediaKind::Unknown`] (a probe failure safely falls back to the old
///   "always offer" behavior, mirroring [`mft_worthwhile`]). If every
///   library volume is a confirmed SSD, elevating would only ever route to
///   walkdir anyway (see [`WalkdirReason::SsdVolume`]), so the prompt would
///   not help.
///
/// An empty `volume_media` (no libraries, or none on a lettered local drive)
/// always answers `false` under every mode - there is nothing an MFT scan
/// could speed up.
pub fn should_offer_elevation(mode: ScanRouting, volume_media: &[(char, MediaKind)]) -> bool {
    match mode {
        ScanRouting::ForceWalkdir => false,
        ScanRouting::ForceMft => !volume_media.is_empty(),
        ScanRouting::Auto => volume_media
            .iter()
            .any(|(_, media)| !matches!(media, MediaKind::NoSeekPenalty)),
    }
}

/// Builds the "(MFT: X, walkdir: Y)" scan-method breakdown shown in the
/// final status line after a scan completes. A thin re-export of
/// `i18n::format_scan_summary` kept under this name since callers reach for
/// it alongside the rest of the scan-routing logic.
pub fn format_scan_summary(
    lang: crate::i18n::Lang,
    total: usize,
    mft: usize,
    walkdir: usize,
    elapsed_secs: f64,
) -> String {
    crate::i18n::format_scan_summary(lang, total, mft, walkdir, elapsed_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(letter: Option<char>, canonical_mismatch: bool) -> RootCheck {
        RootCheck {
            game_id: 1,
            install_dir: PathBuf::from(r"G:\SteamLibrary\Game"),
            volume_letter: letter,
            canonical_mismatch,
        }
    }

    #[test]
    fn not_elevated_always_routes_to_walkdir() {
        let c = check(Some('G'), false);
        let mut available = HashMap::new();
        available.insert('G', true);

        assert_eq!(
            initial_route(false, ScanRouting::Auto, &c, &available, &HashMap::new()),
            ScanRoute::Walkdir(WalkdirReason::NotElevated)
        );
    }

    #[test]
    fn elevated_with_no_volume_letter_routes_to_walkdir() {
        let c = check(None, false);
        let available = HashMap::new();

        assert_eq!(
            initial_route(true, ScanRouting::Auto, &c, &available, &HashMap::new()),
            ScanRoute::Walkdir(WalkdirReason::NoVolumeLetter)
        );
    }

    #[test]
    fn elevated_with_canonical_mismatch_routes_to_walkdir_even_if_volume_available() {
        let c = check(Some('G'), true);
        let mut available = HashMap::new();
        available.insert('G', true);

        assert_eq!(
            initial_route(true, ScanRouting::Auto, &c, &available, &HashMap::new()),
            ScanRoute::Walkdir(WalkdirReason::CanonicalMismatch)
        );
    }

    #[test]
    fn elevated_with_unavailable_volume_routes_to_walkdir() {
        let c = check(Some('G'), false);
        let mut available = HashMap::new();
        available.insert('G', false);

        assert_eq!(
            initial_route(true, ScanRouting::Auto, &c, &available, &HashMap::new()),
            ScanRoute::Walkdir(WalkdirReason::VolumeUnavailable)
        );
    }

    #[test]
    fn elevated_with_volume_missing_from_map_routes_to_walkdir() {
        let c = check(Some('G'), false);
        let available = HashMap::new(); // 'G' never queried/known

        assert_eq!(
            initial_route(true, ScanRouting::Auto, &c, &available, &HashMap::new()),
            ScanRoute::Walkdir(WalkdirReason::VolumeUnavailable)
        );
    }

    #[test]
    fn elevated_with_available_volume_and_no_mismatch_is_mft_candidate() {
        let c = check(Some('G'), false);
        let mut available = HashMap::new();
        available.insert('G', true);

        assert_eq!(
            initial_route(true, ScanRouting::Auto, &c, &available, &HashMap::new()),
            ScanRoute::Mft
        );
    }

    #[test]
    fn elevated_ssd_volume_routes_to_walkdir_without_availability_probe() {
        let c = check(Some('G'), false);
        // 'G' is deliberately absent from `available`: an SSD volume must
        // be routed away before availability is ever consulted.
        let available = HashMap::new();
        let mut ssd = HashMap::new();
        ssd.insert('G', true);

        assert_eq!(
            initial_route(true, ScanRouting::Auto, &c, &available, &ssd),
            ScanRoute::Walkdir(WalkdirReason::SsdVolume)
        );
    }

    #[test]
    fn elevated_non_ssd_volume_still_follows_availability() {
        let c = check(Some('G'), false);
        let mut available = HashMap::new();
        available.insert('G', true);
        let mut ssd = HashMap::new();
        ssd.insert('G', false);

        assert_eq!(
            initial_route(true, ScanRouting::Auto, &c, &available, &ssd),
            ScanRoute::Mft
        );
    }

    #[test]
    fn force_walkdir_short_circuits_before_any_other_check() {
        // Deliberately set up a root that would otherwise be a perfect MFT
        // candidate (elevated, lettered, no mismatch, volume available, not
        // SSD) - ForceWalkdir must still win.
        let c = check(Some('G'), false);
        let mut available = HashMap::new();
        available.insert('G', true);
        let mut ssd = HashMap::new();
        ssd.insert('G', false);

        assert_eq!(
            initial_route(true, ScanRouting::ForceWalkdir, &c, &available, &ssd),
            ScanRoute::Walkdir(WalkdirReason::ForcedBySetting)
        );
    }

    #[test]
    fn force_walkdir_wins_even_when_not_elevated() {
        let c = check(Some('G'), false);
        assert_eq!(
            initial_route(
                false,
                ScanRouting::ForceWalkdir,
                &c,
                &HashMap::new(),
                &HashMap::new()
            ),
            ScanRoute::Walkdir(WalkdirReason::ForcedBySetting)
        );
    }

    #[test]
    fn force_mft_ignores_ssd_volume_and_stays_on_mft() {
        let c = check(Some('G'), false);
        let mut available = HashMap::new();
        available.insert('G', true);
        let mut ssd = HashMap::new();
        ssd.insert('G', true); // SSD - Auto would route this to walkdir

        assert_eq!(
            initial_route(true, ScanRouting::ForceMft, &c, &available, &ssd),
            ScanRoute::Mft,
            "ForceMft must bypass only the SSD speed heuristic"
        );
    }

    #[test]
    fn force_mft_still_respects_not_elevated() {
        let c = check(Some('G'), false);
        let mut available = HashMap::new();
        available.insert('G', true);

        assert_eq!(
            initial_route(
                false,
                ScanRouting::ForceMft,
                &c,
                &available,
                &HashMap::new()
            ),
            ScanRoute::Walkdir(WalkdirReason::NotElevated),
            "ForceMft must not bypass the elevation correctness gate"
        );
    }

    #[test]
    fn force_mft_still_respects_no_volume_letter() {
        let c = check(None, false);

        assert_eq!(
            initial_route(
                true,
                ScanRouting::ForceMft,
                &c,
                &HashMap::new(),
                &HashMap::new()
            ),
            ScanRoute::Walkdir(WalkdirReason::NoVolumeLetter),
            "ForceMft must not bypass the volume-letter correctness gate"
        );
    }

    #[test]
    fn force_mft_still_respects_canonical_mismatch() {
        let c = check(Some('G'), true);
        let mut available = HashMap::new();
        available.insert('G', true);

        assert_eq!(
            initial_route(true, ScanRouting::ForceMft, &c, &available, &HashMap::new()),
            ScanRoute::Walkdir(WalkdirReason::CanonicalMismatch),
            "ForceMft must not bypass the canonical-mismatch correctness gate"
        );
    }

    #[test]
    fn force_mft_still_respects_volume_unavailable() {
        let c = check(Some('G'), false);
        let mut available = HashMap::new();
        available.insert('G', false);
        let mut ssd = HashMap::new();
        ssd.insert('G', true);

        assert_eq!(
            initial_route(true, ScanRouting::ForceMft, &c, &available, &ssd),
            ScanRoute::Walkdir(WalkdirReason::VolumeUnavailable),
            "ForceMft must not bypass the volume-available correctness gate"
        );
    }

    #[test]
    fn mft_worthwhile_only_rejects_no_seek_penalty() {
        assert!(mft_worthwhile(MediaKind::SeekPenalty));
        assert!(mft_worthwhile(MediaKind::Unknown));
        assert!(!mft_worthwhile(MediaKind::NoSeekPenalty));
    }

    #[test]
    fn volumes_to_check_is_empty_when_not_elevated() {
        let checks = vec![check(Some('G'), false), check(Some('D'), false)];
        assert!(volumes_to_check(false, ScanRouting::Auto, &checks).is_empty());
    }

    #[test]
    fn volumes_to_check_dedups_and_sorts_letters() {
        let checks = vec![
            check(Some('G'), false),
            check(Some('D'), false),
            check(Some('G'), false),
        ];
        assert_eq!(
            volumes_to_check(true, ScanRouting::Auto, &checks),
            vec!['D', 'G']
        );
    }

    #[test]
    fn volumes_to_check_skips_no_letter_and_canonical_mismatch_roots() {
        let checks = vec![
            check(None, false),      // no drive letter - skip
            check(Some('C'), true),  // canonical mismatch - skip
            check(Some('G'), false), // real candidate
        ];
        assert_eq!(
            volumes_to_check(true, ScanRouting::Auto, &checks),
            vec!['G']
        );
    }

    #[test]
    fn volumes_to_check_is_empty_for_force_walkdir_even_when_elevated_with_candidates() {
        let checks = vec![check(Some('G'), false), check(Some('D'), false)];
        assert!(
            volumes_to_check(true, ScanRouting::ForceWalkdir, &checks).is_empty(),
            "ForceWalkdir must never probe any volume"
        );
    }

    #[test]
    fn volumes_to_check_still_probes_for_force_mft() {
        let checks = vec![check(Some('G'), false)];
        assert_eq!(
            volumes_to_check(true, ScanRouting::ForceMft, &checks),
            vec!['G']
        );
    }

    #[test]
    fn finalize_mft_result_error_routes_to_walkdir_regardless_of_emptiness() {
        assert_eq!(
            finalize_mft_result(false, true, true),
            ScanRoute::Walkdir(WalkdirReason::MftFailed)
        );
        assert_eq!(
            finalize_mft_result(false, false, false),
            ScanRoute::Walkdir(WalkdirReason::MftFailed)
        );
    }

    #[test]
    fn finalize_mft_result_empty_but_nonempty_on_disk_routes_to_walkdir() {
        assert_eq!(
            finalize_mft_result(true, true, true),
            ScanRoute::Walkdir(WalkdirReason::MftEmptyOnNonEmptyDisk)
        );
    }

    #[test]
    fn finalize_mft_result_genuinely_empty_root_stays_on_mft() {
        assert_eq!(finalize_mft_result(true, true, false), ScanRoute::Mft);
    }

    #[test]
    fn finalize_mft_result_nonempty_result_stays_on_mft() {
        assert_eq!(finalize_mft_result(true, false, false), ScanRoute::Mft);
        // `nonempty_on_disk` is meaningless once entries aren't empty, but
        // must not be able to flip the outcome either way.
        assert_eq!(finalize_mft_result(true, false, true), ScanRoute::Mft);
    }

    #[test]
    fn paths_case_insensitively_equal_ignores_case() {
        assert!(paths_case_insensitively_equal(
            Path::new(r"G:\SteamLibrary\Game"),
            Path::new(r"g:\steamlibrary\game"),
        ));
    }

    #[test]
    fn paths_case_insensitively_equal_detects_real_differences() {
        assert!(!paths_case_insensitively_equal(
            Path::new(r"G:\SteamLibrary\Game"),
            Path::new(r"D:\Elsewhere\Game"),
        ));
    }

    #[test]
    fn format_scan_summary_matches_expected_shape() {
        use crate::i18n::Lang;
        assert_eq!(
            format_scan_summary(Lang::Uk, 10, 7, 3, 2.5),
            "Проскановано 10 ігор (MFT: 7, обхід тек: 3) за 2.5 с."
        );
        assert_eq!(
            format_scan_summary(Lang::En, 10, 7, 3, 2.5),
            "Scanned 10 game(s) (MFT: 7, walkdir: 3) in 2.5 sec."
        );
    }

    #[test]
    fn should_offer_elevation_never_shows_for_force_walkdir() {
        // Deliberately include a HDD volume - ForceWalkdir must still say no,
        // since the MFT path is skipped entirely regardless of media kind.
        assert!(!should_offer_elevation(
            ScanRouting::ForceWalkdir,
            &[('G', MediaKind::SeekPenalty)]
        ));
    }

    #[test]
    fn should_offer_elevation_force_walkdir_ignores_empty_input_too() {
        assert!(!should_offer_elevation(ScanRouting::ForceWalkdir, &[]));
    }

    #[test]
    fn should_offer_elevation_force_mft_shows_for_any_lettered_volume_regardless_of_media() {
        // SSD volume: Auto would say no, but ForceMft means the user opted
        // into MFT even on SSDs, so media kind must not matter here.
        assert!(should_offer_elevation(
            ScanRouting::ForceMft,
            &[('G', MediaKind::NoSeekPenalty)]
        ));
        assert!(should_offer_elevation(
            ScanRouting::ForceMft,
            &[('G', MediaKind::SeekPenalty)]
        ));
        assert!(should_offer_elevation(
            ScanRouting::ForceMft,
            &[('G', MediaKind::Unknown)]
        ));
    }

    #[test]
    fn should_offer_elevation_force_mft_hides_when_no_volumes_at_all() {
        assert!(!should_offer_elevation(ScanRouting::ForceMft, &[]));
    }

    #[test]
    fn should_offer_elevation_auto_hides_when_every_volume_is_ssd() {
        assert!(!should_offer_elevation(
            ScanRouting::Auto,
            &[
                ('G', MediaKind::NoSeekPenalty),
                ('D', MediaKind::NoSeekPenalty),
            ]
        ));
    }

    #[test]
    fn should_offer_elevation_auto_shows_when_any_volume_has_seek_penalty() {
        assert!(should_offer_elevation(
            ScanRouting::Auto,
            &[
                ('G', MediaKind::NoSeekPenalty),
                ('D', MediaKind::SeekPenalty),
            ]
        ));
    }

    #[test]
    fn should_offer_elevation_auto_shows_when_any_volume_is_unknown() {
        // Probe failure falls back to the old "always offer" behavior.
        assert!(should_offer_elevation(
            ScanRouting::Auto,
            &[('G', MediaKind::NoSeekPenalty), ('D', MediaKind::Unknown),]
        ));
    }

    #[test]
    fn should_offer_elevation_auto_hides_when_no_libraries_at_all() {
        assert!(!should_offer_elevation(ScanRouting::Auto, &[]));
    }

    #[test]
    fn format_scan_summary_rounds_elapsed_to_one_decimal() {
        use crate::i18n::Lang;
        assert_eq!(
            format_scan_summary(Lang::Uk, 1, 0, 1, 0.04),
            "Проскановано 1 ігор (MFT: 0, обхід тек: 1) за 0.0 с."
        );
    }
}
