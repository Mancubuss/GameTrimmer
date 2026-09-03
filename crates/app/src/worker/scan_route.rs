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
///
/// Reported back to the user in the settings dialog's "Scanning" section.
/// Routing is automatic and silent, which is exactly why the reasons have to
/// be recoverable: a scan that quietly walked every root because the process
/// is not elevated is otherwise indistinguishable from one that used the
/// index throughout.
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
/// a probe that could not answer must not silently cost the user the
/// order-of-magnitude win on a spinning disk, and the wrong guess here is
/// merely slower, never incorrect.
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
/// There is no mode parameter. Routing used to be overridable through a
/// three-way `scan_routing` setting, which offered exactly one useful
/// override and one harmful one: "always walk" was the only permanent way
/// to stop the UAC prompt (now [`gametrimmer_core::settings::Settings::never_ask_elevation`]),
/// while "prefer MFT" only bypassed the [`WalkdirReason::SsdVolume`]
/// heuristic - i.e. its sole effect was making the scan ~40x slower on an
/// SSD. Neither survived, so every root now follows the one route that is
/// both correct and fastest for its volume.
pub fn initial_route(
    elevated: bool,
    check: &RootCheck,
    volume_available: &HashMap<char, bool>,
    volume_ssd: &HashMap<char, bool>,
) -> ScanRoute {
    if !elevated {
        return ScanRoute::Walkdir(WalkdirReason::NotElevated);
    }
    let Some(letter) = check.volume_letter else {
        return ScanRoute::Walkdir(WalkdirReason::NoVolumeLetter);
    };
    if check.canonical_mismatch {
        return ScanRoute::Walkdir(WalkdirReason::CanonicalMismatch);
    }
    if volume_ssd.get(&letter).copied().unwrap_or(false) {
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
pub fn volumes_to_check(elevated: bool, checks: &[RootCheck]) -> Vec<char> {
    if !elevated {
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
/// Answers `true` only if at least one volume is not
/// [`MediaKind::NoSeekPenalty`] - i.e. [`MediaKind::SeekPenalty`] or
/// [`MediaKind::Unknown`] (a probe failure safely falls back to offering,
/// mirroring [`mft_worthwhile`]). If every library volume is a confirmed
/// SSD, elevating would only ever route to walkdir anyway (see
/// [`WalkdirReason::SsdVolume`]), so the prompt would not help. An empty
/// `volume_media` (no libraries, or none on a lettered local drive) answers
/// `false` for the same reason: there is nothing an MFT scan could speed up.
///
/// This answers only "would elevating change anything". Whether the user has
/// permanently refused to be asked is a separate question, held by
/// [`gametrimmer_core::settings::Settings::never_ask_elevation`] and checked
/// by the caller - keeping the two apart is what lets the settings screen
/// say "this machine would benefit, you have it switched off" rather than
/// conflating "pointless here" with "declined".
pub fn should_offer_elevation(volume_media: &[(char, MediaKind)]) -> bool {
    volume_media
        .iter()
        .any(|(_, media)| !matches!(media, MediaKind::NoSeekPenalty))
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

/// Every reason, in the order the breakdown lists them: most actionable
/// first. "Not elevated" is the one thing the user can change; an SSD
/// volume or a junction is not.
const REASON_ORDER: [WalkdirReason; 7] = [
    WalkdirReason::NotElevated,
    WalkdirReason::SsdVolume,
    WalkdirReason::MftFailed,
    WalkdirReason::MftEmptyOnNonEmptyDisk,
    WalkdirReason::VolumeUnavailable,
    WalkdirReason::NoVolumeLetter,
    WalkdirReason::CanonicalMismatch,
];

/// One line naming how many roots took the directory walk and why, e.g.
/// "Walked 4 of 7 roots: not elevated - 3; SSD volume - 1".
///
/// Empty when nothing was walked: a line saying "0 roots" every time the
/// MFT worked would be noise where silence already means "as configured".
pub fn format_walkdir_breakdown(
    lang: crate::i18n::Lang,
    total: usize,
    reasons: &[WalkdirReason],
) -> String {
    if reasons.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = REASON_ORDER
        .iter()
        .filter_map(|&reason| {
            let count = reasons.iter().filter(|&&r| r == reason).count();
            (count > 0).then(|| {
                format!(
                    "{} \u{2014} {count}",
                    crate::i18n::walkdir_reason_label(lang, reason)
                )
            })
        })
        .collect();
    crate::i18n::walkdir_breakdown(lang, reasons.len(), total, &parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Silence when the MFT did its job: a line reading "0 roots walked"
    /// after every scan would be noise.
    #[test]
    fn the_breakdown_is_empty_when_nothing_was_walked() {
        assert_eq!(format_walkdir_breakdown(crate::i18n::Lang::En, 7, &[]), "",);
    }

    /// The point of the line: the reasons are named, counted, and the
    /// actionable one comes first.
    #[test]
    fn the_breakdown_counts_each_reason_and_leads_with_the_actionable_one() {
        let reasons = [
            WalkdirReason::SsdVolume,
            WalkdirReason::NotElevated,
            WalkdirReason::NotElevated,
        ];

        let line = format_walkdir_breakdown(crate::i18n::Lang::En, 7, &reasons);

        assert!(line.contains("3 of 7"), "{line}");
        assert!(
            line.contains("not running as administrator \u{2014} 2"),
            "{line}"
        );
        assert!(
            line.contains("SSD, where walking is faster \u{2014} 1"),
            "{line}"
        );
        let elevation_at = line
            .find("administrator")
            .expect("elevation reason present");
        let ssd_at = line.find("SSD").expect("ssd reason present");
        assert!(
            elevation_at < ssd_at,
            "the reason the user can act on should come first: {line}",
        );
    }

    /// A reason with no roots behind it must not appear at all.
    #[test]
    fn the_breakdown_names_only_the_reasons_that_occurred() {
        let line = format_walkdir_breakdown(
            crate::i18n::Lang::En,
            2,
            &[WalkdirReason::CanonicalMismatch],
        );

        assert!(line.contains("junction"), "{line}");
        assert!(!line.contains("administrator"), "{line}");
        assert!(!line.contains("SSD"), "{line}");
    }

    /// Every variant has to be listed, or a root walked for a missing
    /// reason would vanish from a breakdown whose counts claim to add up.
    #[test]
    fn every_reason_is_in_the_listed_order() {
        for reason in [
            WalkdirReason::NotElevated,
            WalkdirReason::NoVolumeLetter,
            WalkdirReason::VolumeUnavailable,
            WalkdirReason::SsdVolume,
            WalkdirReason::CanonicalMismatch,
            WalkdirReason::MftFailed,
            WalkdirReason::MftEmptyOnNonEmptyDisk,
        ] {
            assert!(
                REASON_ORDER.contains(&reason),
                "{reason:?} is missing from REASON_ORDER",
            );
        }
    }

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
            initial_route(false, &c, &available, &HashMap::new()),
            ScanRoute::Walkdir(WalkdirReason::NotElevated)
        );
    }

    #[test]
    fn elevated_with_no_volume_letter_routes_to_walkdir() {
        let c = check(None, false);
        let available = HashMap::new();

        assert_eq!(
            initial_route(true, &c, &available, &HashMap::new()),
            ScanRoute::Walkdir(WalkdirReason::NoVolumeLetter)
        );
    }

    #[test]
    fn elevated_with_canonical_mismatch_routes_to_walkdir_even_if_volume_available() {
        let c = check(Some('G'), true);
        let mut available = HashMap::new();
        available.insert('G', true);

        assert_eq!(
            initial_route(true, &c, &available, &HashMap::new()),
            ScanRoute::Walkdir(WalkdirReason::CanonicalMismatch)
        );
    }

    #[test]
    fn elevated_with_unavailable_volume_routes_to_walkdir() {
        let c = check(Some('G'), false);
        let mut available = HashMap::new();
        available.insert('G', false);

        assert_eq!(
            initial_route(true, &c, &available, &HashMap::new()),
            ScanRoute::Walkdir(WalkdirReason::VolumeUnavailable)
        );
    }

    #[test]
    fn elevated_with_volume_missing_from_map_routes_to_walkdir() {
        let c = check(Some('G'), false);
        let available = HashMap::new(); // 'G' never queried/known

        assert_eq!(
            initial_route(true, &c, &available, &HashMap::new()),
            ScanRoute::Walkdir(WalkdirReason::VolumeUnavailable)
        );
    }

    #[test]
    fn elevated_with_available_volume_and_no_mismatch_is_mft_candidate() {
        let c = check(Some('G'), false);
        let mut available = HashMap::new();
        available.insert('G', true);

        assert_eq!(
            initial_route(true, &c, &available, &HashMap::new()),
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
            initial_route(true, &c, &available, &ssd),
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

        assert_eq!(initial_route(true, &c, &available, &ssd), ScanRoute::Mft);
    }

    /// The SSD heuristic used to be bypassable by the "prefer the MFT index"
    /// mode. Nothing bypasses it now - which is the whole point of retiring
    /// that mode, since bypassing it is a ~40x slowdown and never a speed-up.
    #[test]
    fn nothing_can_route_an_ssd_volume_back_onto_the_mft_path() {
        let c = check(Some('G'), false);
        let mut available = HashMap::new();
        available.insert('G', true);
        let mut ssd = HashMap::new();
        ssd.insert('G', true);

        assert_eq!(
            initial_route(true, &c, &available, &ssd),
            ScanRoute::Walkdir(WalkdirReason::SsdVolume),
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
        assert!(volumes_to_check(false, &checks).is_empty());
    }

    #[test]
    fn volumes_to_check_dedups_and_sorts_letters() {
        let checks = vec![
            check(Some('G'), false),
            check(Some('D'), false),
            check(Some('G'), false),
        ];
        assert_eq!(volumes_to_check(true, &checks), vec!['D', 'G']);
    }

    #[test]
    fn volumes_to_check_skips_no_letter_and_canonical_mismatch_roots() {
        let checks = vec![
            check(None, false),      // no drive letter - skip
            check(Some('C'), true),  // canonical mismatch - skip
            check(Some('G'), false), // real candidate
        ];
        assert_eq!(volumes_to_check(true, &checks), vec!['G']);
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
            format_scan_summary(Lang::En, 10, 7, 3, 2.5),
            "Scanned 10 game(s) (MFT: 7, walkdir: 3) in 2.5 sec."
        );
    }

    #[test]
    fn should_offer_elevation_hides_when_every_volume_is_ssd() {
        assert!(!should_offer_elevation(&[
            ('G', MediaKind::NoSeekPenalty),
            ('D', MediaKind::NoSeekPenalty),
        ]));
    }

    #[test]
    fn should_offer_elevation_shows_when_any_volume_has_seek_penalty() {
        assert!(should_offer_elevation(&[
            ('G', MediaKind::NoSeekPenalty),
            ('D', MediaKind::SeekPenalty),
        ]));
    }

    #[test]
    fn should_offer_elevation_shows_when_any_volume_is_unknown() {
        // Probe failure falls back to offering.
        assert!(should_offer_elevation(&[
            ('G', MediaKind::NoSeekPenalty),
            ('D', MediaKind::Unknown),
        ]));
    }

    #[test]
    fn should_offer_elevation_hides_when_no_libraries_at_all() {
        assert!(!should_offer_elevation(&[]));
    }

    #[test]
    fn format_scan_summary_rounds_elapsed_to_one_decimal() {
        use crate::i18n::Lang;
        assert_eq!(
            format_scan_summary(Lang::En, 1, 0, 1, 0.04),
            "Scanned 1 game(s) (MFT: 0, walkdir: 1) in 0.0 sec."
        );
    }
}
