//! A small, drawn "vendor mark" shown next to a library row in Settings ->
//! Scanning, so a library reads by colour and shape before its `[vendor]`
//! text is read - useful once there are several libraries on screen (a live
//! run has 9).
//!
//! These are our own neutral marks, not launcher logos: a rounded square,
//! a colour derived from the vendor string, and one or two letters. Real
//! launcher logos, wordmarks and brand colours are third-party trademarks
//! the owner decided (2026-09-04) not to source or imitate; this ships the
//! plain version first and leaves real icons as a later, separate decision.
//!
//! The vendor -> (initials, colour) mapping lives in one function,
//! [`vendor_mark`], so swapping in real logos later only touches this file.

use eframe::egui::{self, Color32};

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
/// full vendor tag, since the mark itself only carries one or two letters).
pub fn show(ui: &mut egui::Ui, vendor: &str) -> egui::Response {
    let size = ui.text_style_height(&egui::TextStyle::Body);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let (initials, color) = vendor_mark(vendor);
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
