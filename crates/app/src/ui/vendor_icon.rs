//! A small, drawn "vendor mark" shown next to a library row in Settings ->
//! Scanning, so a library reads by colour and shape before its `[vendor]`
//! text is read - useful once there are several libraries on screen (a live
//! run has 9).
//!
//! What is drawn is the launcher's *own* icon whenever this machine has that
//! launcher installed: the image is read at runtime out of the installed
//! executable (see [`crate::ui::launcher_icon`]), the same way Explorer draws
//! it. Nothing is shipped - no launcher logo, wordmark or artwork lives in
//! this repository or in the release archive - and the icon is drawn only
//! beside the library row it identifies, unrecoloured and unrestyled.
//!
//! When no icon can be read - the library was swept off a drive or added by
//! hand, the launcher was uninstalled but its games stayed, or the executable
//! carries no usable icon - the row falls back to our own neutral mark: a
//! rounded square, a colour derived from the vendor string, and one or two
//! letters. The fallback is never a blank space.
//!
//! Both maps live here side by side: vendor -> (initials, colour) in
//! [`vendor_mark`], and vendor -> where its launcher's executable is found in
//! [`LAUNCHER_SOURCES`].

use eframe::egui::{self, Color32, TextureHandle};

/// Hand-picked palette, not tied to any launcher's brand colour. Chosen for
/// legible white or black text (see [`text_color_for`]) rather than for
/// matching a real product.
const PALETTE: [Color32; 8] = [
    Color32::from_rgb(0x4C, 0x6E, 0xF5), // blue
    Color32::from_rgb(0x2E, 0x8B, 0x57), // green
    Color32::from_rgb(0xC4, 0x5A, 0x2B), // rust
    Color32::from_rgb(0x8A, 0x3F, 0xA0), // purple
    Color32::from_rgb(0x1F, 0x8A, 0x8A), // teal
    Color32::from_rgb(0xB0, 0x3A, 0x5B), // mauve
    Color32::from_rgb(0x6B, 0x6B, 0x2E), // olive
    Color32::from_rgb(0x3A, 0x5B, 0x8A), // navy
];

/// Maps a vendor tag to the mark drawn for it: two letters plus a fill
/// colour. The 14 launcher vendors and `"manual"` (see
/// `gametrimmer_core::providers` and `worker::manual::MANUAL_VENDOR`) get
/// hand-picked initials; anything else - an unknown or future vendor tag -
/// falls back to the first letters of the string itself, so a library never
/// renders with an empty or missing mark.
///
/// The colour is a hash of the vendor string into [`PALETTE`], so the same
/// vendor always gets the same colour and an unrecognised vendor still gets
/// *a* colour instead of a hole.
pub fn vendor_mark(vendor: &str) -> (String, Color32) {
    let known = match vendor {
        "amazon" => "Am",
        "battlenet" => "Bn",
        "ea" => "EA",
        "epic" => "Ep",
        "folderscan" => "Fs",
        "gog" => "GO",
        "humble" => "Hu",
        "itch" => "It",
        "manual" => "Mn",
        "paradox" => "Pa",
        "riot" => "Ri",
        "rockstar" => "Ro",
        "steam" => "St",
        "ubisoft" => "Ub",
        "xbox" => "Xb",
        _ => "",
    };
    let initials = if !known.is_empty() {
        known.to_string()
    } else {
        fallback_initials(vendor)
    };

    // FNV-1a: small, deterministic, no extra dependency for two bytes of hash.
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in vendor.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let color = PALETTE[(hash as usize) % PALETTE.len()];

    (initials, color)
}

/// Initials for a vendor tag not in the hand-picked table: the first one or
/// two alphanumeric characters, so an unknown vendor still renders something
/// short rather than nothing. `"?"` only for a vendor string with no
/// alphanumeric characters at all (in practice, an empty string).
fn fallback_initials(vendor: &str) -> String {
    let mut chars = vendor.chars().filter(|c| c.is_alphanumeric());
    match (chars.next(), chars.next()) {
        (Some(a), Some(b)) => format!("{}{}", a.to_ascii_uppercase(), b.to_ascii_lowercase()),
        (Some(a), None) => a.to_ascii_uppercase().to_string(),
        (None, _) => "?".to_string(),
    }
}

/// Where a vendor's launcher executable is found on this machine, so its own
/// icon can be read out of it at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherSource {
    /// Steam records its own install root in the registry and the provider
    /// already reads it - see `gametrimmer_core::providers::steam`. Steam is
    /// the one launcher that need not appear in the uninstall registry at
    /// all: a portable install has no uninstall entry, and the dev machine's
    /// Steam is exactly that.
    SteamRoot,
    /// Found through the Windows uninstall registry: the entry whose
    /// `DisplayName` starts with `display_name` records where the launcher
    /// was installed, and `exe` is the executable's path relative to that
    /// directory (a bare file name for most; Epic buries its launcher a few
    /// levels down).
    Uninstall {
        display_name: &'static str,
        exe: &'static str,
    },
    /// Installed under a version-numbered subfolder of a known parent
    /// directory (resolved under Program Files), for a launcher whose
    /// uninstall entry cannot be trusted to name the executable: Paradox
    /// Launcher registers an entry but leaves `InstallLocation` empty and
    /// records its installer bundle as `DisplayIcon`; EA Desktop registers
    /// no uninstall entry at all. `parent` is relative to a Program Files
    /// root; `inner` is the subpath below the version folder itself, empty
    /// when the exe sits directly in it.
    VersionedInstall {
        parent: &'static str,
        inner: &'static str,
        exe: &'static str,
    },
    /// Installed as a Microsoft Store package. Such an app has no uninstall
    /// entry and no registry path at all, and its install directory carries
    /// the package version in its own name, so any hard-coded path would die
    /// on the launcher's first update. `family` is the package *family* name,
    /// which never changes across versions, and Windows is asked where that
    /// family is installed; `exe` is the executable's path inside the package
    /// directory.
    StorePackage {
        family: &'static str,
        exe: &'static str,
    },
    /// No launcher executable to read an icon from, so the row keeps the
    /// lettered mark. Each entry below says why.
    None,
}

/// Vendor -> where to find its launcher, for every vendor this app can label
/// a library with: the 14 in `gametrimmer_core::providers` plus `"manual"`.
///
/// The routes are measured on a real machine (2026-09-04), not guessed. The
/// documented `App Paths` registry key was probed first and resolved *zero*
/// of the 14 - not Steam, not GOG, not Epic - so it is not consulted at all;
/// the uninstall registry, which the standalone-games sweep already reads,
/// resolved ten. Two more (EA, Paradox) resolve through
/// [`LauncherSource::VersionedInstall`] - walking Program Files for the
/// version-numbered folder the launcher actually installed into, since
/// neither one's uninstall entry can be trusted to name the executable. The
/// last one (Xbox) is a Store package and appears in no registry route at
/// all, so it resolves through [`LauncherSource::StorePackage`].
pub const LAUNCHER_SOURCES: [(&str, LauncherSource); 15] = [
    (
        "amazon",
        LauncherSource::Uninstall {
            display_name: "Amazon Games",
            exe: "Amazon Games.exe",
        },
    ),
    (
        "battlenet",
        LauncherSource::Uninstall {
            display_name: "Battle.net",
            exe: "Battle.net.exe",
        },
    ),
    // The EA app installs under a version-numbered directory and registers
    // no uninstall entry of its own, so it is found the same way as
    // Paradox below: by walking Program Files for the version folder.
    (
        "ea",
        LauncherSource::VersionedInstall {
            parent: r"Electronic Arts\EA Desktop",
            inner: "EA Desktop",
            exe: "EADesktop.exe",
        },
    ),
    (
        "epic",
        LauncherSource::Uninstall {
            display_name: "Epic Games Launcher",
            exe: "Launcher\\Portal\\Binaries\\Win32\\EpicGamesLauncher.exe",
        },
    ),
    // Swept off a drive: no launcher exists by definition.
    ("folderscan", LauncherSource::None),
    (
        "gog",
        LauncherSource::Uninstall {
            display_name: "GOG GALAXY",
            exe: "GalaxyClient.exe",
        },
    ),
    (
        "humble",
        LauncherSource::Uninstall {
            display_name: "Humble App",
            exe: "Humble App.exe",
        },
    ),
    (
        "itch",
        LauncherSource::Uninstall {
            display_name: "itch",
            // itch keeps its executable in a version-numbered directory, so
            // the install location alone cannot name it; the icon comes from
            // the `DisplayIcon` file the same entry records.
            exe: "itch.exe",
        },
    ),
    // Added by hand: no launcher exists by definition.
    ("manual", LauncherSource::None),
    // Paradox does register an uninstall entry ("Paradox Launcher v2"), but
    // both the HKLM and HKCU copies leave `InstallLocation` empty and record
    // their installer bundle (under `Package Cache`) as `DisplayIcon` - not
    // the launcher. Read the same way as EA above instead of trusting either.
    (
        "paradox",
        LauncherSource::VersionedInstall {
            parent: r"Paradox Interactive\launcher",
            inner: "",
            exe: "Paradox Launcher.exe",
        },
    ),
    (
        "riot",
        LauncherSource::Uninstall {
            display_name: "Riot Client",
            exe: "RiotClientServices.exe",
        },
    ),
    (
        "rockstar",
        LauncherSource::Uninstall {
            display_name: "Rockstar Games Launcher",
            exe: "Launcher.exe",
        },
    ),
    ("steam", LauncherSource::SteamRoot),
    (
        "ubisoft",
        LauncherSource::Uninstall {
            display_name: "Ubisoft Connect",
            exe: "UbisoftConnect.exe",
        },
    ),
    // The Xbox app is a Store package: it has no uninstall entry and no
    // registry path to its executable, so neither route above can reach it.
    // Windows is asked for the package directory instead - the app itself
    // carries a 256px icon, and the same extraction as everyone else reads
    // it.
    (
        "xbox",
        LauncherSource::StorePackage {
            family: "Microsoft.GamingApp_8wekyb3d8bbwe",
            exe: "XboxPcApp.exe",
        },
    ),
];

/// Where `vendor`'s launcher lives, or [`LauncherSource::None`] for a vendor
/// with no launcher to read (including any future vendor tag not yet in
/// [`LAUNCHER_SOURCES`], which simply keeps the lettered mark).
pub fn launcher_source(vendor: &str) -> LauncherSource {
    LAUNCHER_SOURCES
        .iter()
        .find(|(name, _)| *name == vendor)
        .map_or(LauncherSource::None, |(_, source)| *source)
}

/// What a row draws for its vendor.
pub enum Mark {
    /// The launcher's own icon, read at runtime from the installed launcher.
    Icon(TextureHandle),
    /// Our neutral mark: initials on a coloured square.
    Letters(String, Color32),
}

/// Picks between the two. Kept separate from [`show`] so the rule - an icon
/// when one was read, the lettered mark whenever one was not - is testable
/// without a registry, a shell or a window.
pub fn mark_for(icon: Option<TextureHandle>, vendor: &str) -> Mark {
    match icon {
        Some(texture) => Mark::Icon(texture),
        None => {
            let (initials, color) = vendor_mark(vendor);
            Mark::Letters(initials, color)
        }
    }
}

/// Black or white, whichever contrasts more with `bg` - independent of the
/// app's light/dark theme, since the mark's fill colour comes from the fixed
/// [`PALETTE`] rather than from `ui.visuals()`.
fn text_color_for(bg: Color32) -> Color32 {
    let luminance =
        0.299 * f32::from(bg.r()) + 0.587 * f32::from(bg.g()) + 0.114 * f32::from(bg.b());
    if luminance > 140.0 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

/// Draws the mark for `vendor` at the current cursor, sized to the row's
/// text height, and returns the allocated response (hover text repeats the
/// full vendor tag, since the mark itself may only carry one or two letters).
///
/// `icons` is the per-session cache: the launcher's icon is extracted at most
/// once per vendor, and a vendor with no icon is remembered as such so a
/// missing launcher is not re-probed on every repaint.
pub fn show(
    ui: &mut egui::Ui,
    icons: &mut crate::ui::launcher_icon::LauncherIcons,
    vendor: &str,
) -> egui::Response {
    let size = ui.text_style_height(&egui::TextStyle::Body);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        match mark_for(icons.texture(ui.ctx(), vendor), vendor) {
            // Drawn at full white tint, i.e. exactly the pixels Windows
            // handed us: the launcher's image is never recoloured or
            // restyled.
            Mark::Icon(texture) => {
                ui.painter().image(
                    texture.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            Mark::Letters(initials, color) => {
                let painter = ui.painter();
                painter.rect_filled(rect, size * 0.2, color);
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    initials,
                    egui::FontId::proportional(size * 0.55),
                    text_color_for(color),
                );
            }
        }
    }
    response.on_hover_text(vendor.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANONICAL_VENDORS: [&str; 14] = [
        "amazon",
        "battlenet",
        "ea",
        "epic",
        "folderscan",
        "gog",
        "humble",
        "itch",
        "paradox",
        "riot",
        "rockstar",
        "steam",
        "ubisoft",
        "xbox",
    ];

    #[test]
    fn every_canonical_vendor_and_manual_gets_a_non_empty_mark() {
        for vendor in CANONICAL_VENDORS.iter().chain(["manual"].iter()) {
            let (initials, _color) = vendor_mark(vendor);
            assert!(
                !initials.is_empty(),
                "vendor {vendor:?} produced an empty mark"
            );
        }
    }

    #[test]
    fn unknown_vendor_falls_back_instead_of_panicking_or_being_empty() {
        let (initials, _color) = vendor_mark("some_future_launcher");
        assert!(
            !initials.is_empty(),
            "unknown vendor produced an empty mark"
        );
        assert_eq!(initials, "So");
    }

    #[test]
    fn empty_vendor_string_still_produces_a_mark() {
        let (initials, _color) = vendor_mark("");
        assert!(!initials.is_empty());
    }

    #[test]
    fn every_canonical_vendor_and_manual_has_a_launcher_source() {
        for vendor in CANONICAL_VENDORS.iter().chain(["manual"].iter()) {
            let listed = LAUNCHER_SOURCES
                .iter()
                .filter(|(name, _)| name == vendor)
                .count();
            assert_eq!(
                listed, 1,
                "vendor {vendor:?} appears {listed} times in LAUNCHER_SOURCES, expected exactly 1"
            );
        }
        assert_eq!(
            LAUNCHER_SOURCES.len(),
            CANONICAL_VENDORS.len() + 1,
            "LAUNCHER_SOURCES lists vendors beyond the canonical 14 plus \"manual\""
        );
    }

    #[test]
    fn no_two_vendors_claim_the_same_launcher_executable() {
        let mut seen: Vec<(&str, &str)> = Vec::new();
        for (vendor, source) in LAUNCHER_SOURCES.iter() {
            if let LauncherSource::Uninstall { display_name, exe } = source {
                if let Some((other, _)) = seen.iter().find(|(_, e)| e == exe) {
                    panic!("vendors {other:?} and {vendor:?} both claim the executable {exe:?}");
                }
                if let Some((other, _)) = seen.iter().find(|(_, d)| d == display_name) {
                    panic!(
                        "vendors {other:?} and {vendor:?} both claim the uninstall entry {display_name:?}"
                    );
                }
                seen.push((vendor, exe));
                seen.push((vendor, display_name));
            }
        }
    }

    #[test]
    fn a_vendor_whose_launcher_lookup_failed_falls_back_to_the_lettered_mark() {
        // The failure is injected, not read off this machine: no icon, for
        // any of the three live reasons (no launcher, uninstalled launcher,
        // executable without a usable icon).
        for vendor in CANONICAL_VENDORS.iter().chain(["manual"].iter()) {
            match mark_for(None, vendor) {
                Mark::Letters(initials, _) => assert!(
                    !initials.is_empty(),
                    "vendor {vendor:?} fell back to an empty mark"
                ),
                Mark::Icon(_) => panic!("vendor {vendor:?} claimed an icon it does not have"),
            }
        }
    }

    #[test]
    fn an_extracted_icon_replaces_the_letters() {
        let ctx = egui::Context::default();
        let texture = ctx.load_texture(
            "test",
            egui::ColorImage::filled([2, 2], Color32::RED),
            egui::TextureOptions::LINEAR,
        );
        match mark_for(Some(texture), "steam") {
            Mark::Icon(_) => {}
            Mark::Letters(initials, _) => {
                panic!("an extracted icon still drew the letters {initials:?}")
            }
        }
    }

    #[test]
    fn mapping_is_deterministic() {
        for vendor in CANONICAL_VENDORS
            .iter()
            .chain(["manual", "unknown_thing"].iter())
        {
            let first = vendor_mark(vendor);
            let second = vendor_mark(vendor);
            assert_eq!(first, second, "vendor {vendor:?} was not deterministic");
        }
    }
}
