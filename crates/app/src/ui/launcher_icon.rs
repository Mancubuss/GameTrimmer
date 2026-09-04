//! Reads the icon of a launcher **already installed on this machine** and
//! hands it to egui as a texture, so a library row can show the launcher it
//! came from instead of two letters standing in for it.
//!
//! Nothing is shipped. No launcher icon, logo or artwork is stored in this
//! repository, in the assets directory or in the release archive; the image
//! is extracted at runtime from the user's own copy of the launcher, which is
//! what Explorer does with the same executable. A machine without the
//! launcher installed gets no image at all - see
//! [`crate::ui::vendor_icon::mark_for`] for the fallback.
//!
//! Three steps, all of them cheap once per session:
//!
//! 1. **Find the executable.** [`LauncherSource`] says where to look. The
//!    documented `App Paths` key was measured first and resolved none of the
//!    14 vendors on a real machine, so the route is the uninstall registry
//!    (`InstallLocation` plus the launcher's file name, falling back to the
//!    `DisplayIcon` the same entry records) plus Steam's own install root.
//! 2. **Extract the icon**, largest size first and scaled *down* - an
//!    upscaled 16x16 icon would look worse than the letters it replaces.
//! 3. **Cache the result**, hit or miss, for the life of the session. A
//!    repaint must never touch the registry or the shell again, and a
//!    missing launcher must not be re-probed every frame.

use std::collections::HashMap;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, HDC, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetIconInfo, PrivateExtractIconsW, HICON, ICONINFO,
};
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};
use winreg::{RegKey, HKEY};

use crate::ui::vendor_icon::{launcher_source, LauncherSource, LAUNCHER_SOURCES};

/// The size asked of Windows. 256 is the largest standard icon size, so an
/// executable that carries one is read at full resolution instead of being
/// blown up from a 32x32 entry; one that does not gets its largest available
/// image, and the downscale below undoes most of the stretch either way.
const REQUESTED_PX: i32 = 256;

/// What is kept in video memory per vendor. A library row is roughly 19
/// logical pixels tall, so 64 stays sharp up to a 300% display scale while
/// costing 16 KB per launcher.
const STORED_PX: u32 = 64;

/// The three places Windows records installed programs. Read in this order;
/// the first entry whose `DisplayName` matches wins.
fn uninstall_roots() -> [(HKEY, &'static str); 3] {
    [
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
        (
            HKEY_CURRENT_USER,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
    ]
}

/// Per-session cache of launcher icons: at most one registry sweep and one
/// icon extraction per vendor, whatever the frame rate.
///
/// A miss is cached as a `None` entry, which is the point: three live cases
/// have no icon to find (a swept or hand-added library, an uninstalled
/// launcher whose games remain, an executable with no usable icon) and none
/// of them may be re-probed on the next repaint.
#[derive(Default)]
pub struct LauncherIcons {
    cache: HashMap<String, Option<TextureHandle>>,
}

impl LauncherIcons {
    /// The launcher icon for `vendor`, or `None` when there is none to show.
    pub fn texture(&mut self, ctx: &egui::Context, vendor: &str) -> Option<TextureHandle> {
        self.texture_with(ctx, vendor, load_icon_for_vendor)
    }

    /// [`Self::texture`] with the source injected, so the caching rule can be
    /// tested without a registry or a shell.
    pub fn texture_with(
        &mut self,
        ctx: &egui::Context,
        vendor: &str,
        load: impl FnOnce(&str) -> Option<ColorImage>,
    ) -> Option<TextureHandle> {
        if let Some(cached) = self.cache.get(vendor) {
            return cached.clone();
        }
        let texture = load(vendor).map(|image| {
            ctx.load_texture(
                format!("launcher_icon_{vendor}"),
                image,
                TextureOptions::LINEAR,
            )
        });
        self.cache.insert(vendor.to_string(), texture.clone());
        texture
    }
}

/// Finds `vendor`'s launcher and reads its icon. `None` at either step means
/// the row keeps its lettered mark.
fn load_icon_for_vendor(vendor: &str) -> Option<ColorImage> {
    let exe = launcher_exe(vendor)?;
    extract_icon(&exe)
}

/// Where `vendor`'s launcher executable (or, for a launcher that hides it in
/// a version-numbered directory, its recorded icon file) actually is on this
/// machine.
fn launcher_exe(vendor: &str) -> Option<PathBuf> {
    match launcher_source(vendor) {
        LauncherSource::SteamRoot => {
            let exe = gametrimmer_core::providers::steam::find_steam_root()?.join("steam.exe");
            exe.is_file().then_some(exe)
        }
        LauncherSource::Uninstall { .. } => uninstall_paths().get(vendor).cloned(),
        LauncherSource::None => None,
    }
}

/// One sweep of the uninstall registry for every vendor at once, done at most
/// once per process. Sweeping per vendor would walk the same few hundred keys
/// up to fourteen times over.
fn uninstall_paths() -> &'static HashMap<String, PathBuf> {
    static PATHS: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();
    PATHS.get_or_init(sweep_uninstall_registry)
}

fn sweep_uninstall_registry() -> HashMap<String, PathBuf> {
    let mut found: HashMap<String, PathBuf> = HashMap::new();
    for (hive, subkey) in uninstall_roots() {
        let Ok(root) = RegKey::predef(hive).open_subkey_with_flags(subkey, KEY_READ) else {
            continue;
        };
        for name in root.enum_keys().flatten() {
            let Ok(entry) = root.open_subkey_with_flags(&name, KEY_READ) else {
                continue;
            };
            let Ok(display_name) = entry.get_value::<String, _>("DisplayName") else {
                continue;
            };
            for (vendor, source) in LAUNCHER_SOURCES {
                let LauncherSource::Uninstall {
                    display_name: wanted,
                    exe,
                } = source
                else {
                    continue;
                };
                // Prefix, not equality: installers append a version ("Humble
                // App 1.1.8+411") or a stray space ("Riot Client ").
                if found.contains_key(vendor)
                    || !display_name
                        .to_lowercase()
                        .starts_with(&wanted.to_lowercase())
                {
                    continue;
                }
                if let Some(path) = launcher_path_from_entry(&entry, exe) {
                    found.insert(vendor.to_string(), path);
                }
            }
        }
    }
    found
}

/// The launcher's own file for one uninstall entry: the recorded install
/// location joined with the launcher's file name, or - for an entry whose
/// install location is empty or version-numbered - the icon file the entry
/// itself points at.
///
/// That order matters: some entries point `DisplayIcon` at their *uninstaller*
/// (GOG Galaxy and Amazon Games both do), whose icon is not the launcher's.
fn launcher_path_from_entry(entry: &RegKey, exe: &str) -> Option<PathBuf> {
    if let Ok(location) = entry.get_value::<String, _>("InstallLocation") {
        let candidate = to_windows_path(&location).join(to_windows_path(exe));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let icon = entry.get_value::<String, _>("DisplayIcon").ok()?;
    let candidate = to_windows_path(&parse_icon_spec(&icon));
    candidate.is_file().then_some(candidate)
}

/// Registry paths are not consistently backslashed - Riot records its install
/// location as `C:/Riot Games/Riot Client`.
fn to_windows_path(raw: &str) -> PathBuf {
    PathBuf::from(raw.replace('/', "\\"))
}

/// A `DisplayIcon` value is an icon *spec*, not a path: the file may be
/// quoted and is often followed by a resource index (`"C:\\...\\app.exe",0`).
fn parse_icon_spec(spec: &str) -> String {
    let trimmed = spec.trim();
    if let Some(rest) = trimmed.strip_prefix('"') {
        return rest.split('"').next().unwrap_or_default().to_string();
    }
    match trimmed.rfind(',') {
        // Only a trailing `,<index>` is a separator; a comma inside a
        // directory name is part of the path.
        Some(at) if trimmed[at + 1..].trim().parse::<i32>().is_ok() => trimmed[..at].to_string(),
        _ => trimmed.to_string(),
    }
}

/// Reads the largest icon `exe` carries and scales it down to [`STORED_PX`].
fn extract_icon(exe: &Path) -> Option<ColorImage> {
    // `PrivateExtractIconsW` takes a fixed MAX_PATH buffer, so a launcher
    // installed deeper than that keeps the lettered mark rather than
    // truncating a path into an unrelated file.
    let mut wide = [0u16; 260];
    let encoded: Vec<u16> = exe.as_os_str().encode_wide().collect();
    if encoded.len() >= wide.len() {
        return None;
    }
    wide[..encoded.len()].copy_from_slice(&encoded);

    let mut icons = [HICON::default(); 1];
    // SAFETY: `wide` is a NUL-terminated buffer of exactly MAX_PATH wide
    // chars, and `icons` is a one-element slice the call fills with at most
    // one handle; both outlive the call.
    let extracted = unsafe {
        PrivateExtractIconsW(
            &wide,
            0,
            REQUESTED_PX,
            REQUESTED_PX,
            Some(&mut icons),
            None,
            0,
        )
    };
    if extracted != 1 || icons[0].is_invalid() {
        return None;
    }

    let image = icon_to_image(icons[0]);
    // SAFETY: `icons[0]` came from `PrivateExtractIconsW`, is owned by us,
    // and is not used after this point - including on the failure paths
    // inside `icon_to_image`, which never take ownership of it.
    unsafe {
        let _ = DestroyIcon(icons[0]);
    }
    image.map(downscale)
}

/// Pixels of one icon, with both bitmaps `GetIconInfo` hands us released on
/// every path out.
fn icon_to_image(icon: HICON) -> Option<ColorImage> {
    let mut info = ICONINFO::default();
    // SAFETY: `icon` is a live handle and `info` is a properly aligned,
    // zeroed ICONINFO. On success the call hands us ownership of the two
    // bitmaps it names, which is why nothing may return before the cleanup
    // below.
    unsafe { GetIconInfo(icon, &mut info) }.ok()?;

    let pixels = icon_pixels(&info);

    // SAFETY: both handles were created by the `GetIconInfo` above, are
    // owned by us, and are not used again. `hbmColor` is null for a
    // monochrome icon, hence the guard.
    unsafe {
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
        }
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        }
    }
    pixels
}

fn icon_pixels(info: &ICONINFO) -> Option<ColorImage> {
    if info.hbmColor.is_invalid() {
        // A monochrome (1bpp) icon has no colour bitmap at all. Rare enough
        // that the lettered mark is a better answer than a black square.
        return None;
    }
    let (width, height) = bitmap_size(HGDIOBJ(info.hbmColor.0))?;
    let color = read_dib(HGDIOBJ(info.hbmColor.0), width, height)?;
    let mask = read_dib(HGDIOBJ(info.hbmMask.0), width, height);
    let rgba = bgra_to_rgba(&color, mask.as_deref());
    Some(ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        &rgba,
    ))
}

fn bitmap_size(bitmap: HGDIOBJ) -> Option<(i32, i32)> {
    let mut header = BITMAP::default();
    // SAFETY: `bitmap` is a live GDI bitmap and the buffer handed over is a
    // properly aligned BITMAP of exactly the size declared.
    let written = unsafe {
        GetObjectW(
            bitmap,
            std::mem::size_of::<BITMAP>() as i32,
            Some(std::ptr::from_mut(&mut header).cast()),
        )
    };
    if written == 0 || header.bmWidth <= 0 || header.bmHeight <= 0 {
        return None;
    }
    Some((header.bmWidth, header.bmHeight))
}

/// Reads a GDI bitmap as a 32bpp top-down BGRA buffer.
fn read_dib(bitmap: HGDIOBJ, width: i32, height: i32) -> Option<Vec<u8>> {
    if bitmap.is_invalid() {
        return None;
    }
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Negative: rows come back top-down, the order egui wants.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buffer = vec![0u8; (width as usize) * (height as usize) * 4];

    // SAFETY: a screen DC, released below on every path.
    let dc: HDC = unsafe { GetDC(None) };
    if dc.is_invalid() {
        return None;
    }
    // SAFETY: `buffer` is exactly `width * height * 4` bytes, which is what
    // the 32bpp BI_RGB header above describes, and `info` is a live
    // BITMAPINFO for the duration of the call.
    let rows = unsafe {
        GetDIBits(
            dc,
            windows::Win32::Graphics::Gdi::HBITMAP(bitmap.0),
            0,
            height as u32,
            Some(buffer.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    // SAFETY: `dc` came from the `GetDC` above and is not used again.
    unsafe {
        ReleaseDC(None, dc);
    }
    (rows == height).then_some(buffer)
}

/// Windows hands back BGRA; egui wants RGBA.
///
/// `mask` is the icon's AND mask read the same way, and is consulted only in
/// the one case that needs it: an older icon whose colour bitmap has an
/// **all-zero alpha channel** and carries its transparency in the mask
/// instead. Read literally, such an icon is fully transparent and renders as
/// an invisible hole. In the mask, white means "let the background through",
/// so anything darker becomes opaque. With no mask to read, the icon is
/// treated as opaque - a visible square beats an invisible one.
fn bgra_to_rgba(color: &[u8], mask: Option<&[u8]>) -> Vec<u8> {
    let has_alpha = color.chunks_exact(4).any(|pixel| pixel[3] != 0);
    color
        .chunks_exact(4)
        .enumerate()
        .flat_map(|(index, pixel)| {
            let alpha = if has_alpha {
                pixel[3]
            } else {
                match mask.and_then(|mask| mask.get(index * 4..index * 4 + 3)) {
                    Some(mask_pixel) if mask_pixel.iter().all(|&channel| channel == 0xFF) => 0,
                    _ => 0xFF,
                }
            };
            [pixel[2], pixel[1], pixel[0], alpha]
        })
        .collect()
}

/// Scales an extracted icon down to [`STORED_PX`]. Only ever down: an image
/// smaller than that is left alone rather than blown up.
fn downscale(image: ColorImage) -> ColorImage {
    let [width, height] = [image.width() as u32, image.height() as u32];
    if width <= STORED_PX && height <= STORED_PX {
        return image;
    }
    let flat: Vec<u8> = image
        .pixels
        .iter()
        .flat_map(|pixel| pixel.to_srgba_unmultiplied())
        .collect();
    let Some(source) = image::RgbaImage::from_raw(width, height, flat) else {
        return image;
    };
    let resized = image::imageops::resize(
        &source,
        STORED_PX,
        STORED_PX,
        image::imageops::FilterType::Lanczos3,
    );
    ColorImage::from_rgba_unmultiplied([STORED_PX as usize, STORED_PX as usize], resized.as_raw())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// One opaque BGRA pixel: blue 0x10, green 0x20, red 0x30, alpha 0xFF.
    const OPAQUE_BGRA: [u8; 4] = [0x10, 0x20, 0x30, 0xFF];

    #[test]
    fn bgra_bytes_come_back_as_rgba() {
        let rgba = bgra_to_rgba(&OPAQUE_BGRA, None);
        assert_eq!(
            rgba,
            vec![0x30, 0x20, 0x10, 0xFF],
            "blue and red were not swapped"
        );
    }

    #[test]
    fn an_all_zero_alpha_icon_takes_its_alpha_from_the_mask() {
        // Two pixels, no alpha anywhere - the older-icon case.
        let color = [0x10, 0x20, 0x30, 0x00, 0x40, 0x50, 0x60, 0x00];
        // Mask: first pixel white (transparent), second black (opaque).
        let mask = [0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00];
        let rgba = bgra_to_rgba(&color, Some(&mask));
        assert_eq!(rgba[3], 0x00, "a masked-out pixel was made opaque");
        assert_eq!(
            rgba[7], 0xFF,
            "an icon carrying its alpha in the mask rendered as an invisible hole"
        );
    }

    #[test]
    fn an_all_zero_alpha_icon_without_a_mask_is_opaque() {
        let color = [0x10, 0x20, 0x30, 0x00];
        let rgba = bgra_to_rgba(&color, None);
        assert_eq!(
            rgba[3], 0xFF,
            "an icon with no alpha and no mask rendered as an invisible hole"
        );
    }

    #[test]
    fn a_real_alpha_channel_is_kept_even_when_some_pixels_are_transparent() {
        let color = [0x10, 0x20, 0x30, 0x00, 0x40, 0x50, 0x60, 0x80];
        let mask = [0x00; 8];
        let rgba = bgra_to_rgba(&color, Some(&mask));
        assert_eq!(
            rgba[3], 0x00,
            "the mask overrode a colour bitmap that had real alpha"
        );
        assert_eq!(rgba[7], 0x80);
    }

    #[test]
    fn an_icon_spec_loses_its_quotes_and_resource_index() {
        assert_eq!(
            parse_icon_spec("C:\\Programs\\Humble App\\Humble App.exe,0"),
            "C:\\Programs\\Humble App\\Humble App.exe"
        );
        assert_eq!(
            parse_icon_spec("\"C:\\Rockstar\\Launcher.exe\""),
            "C:\\Rockstar\\Launcher.exe"
        );
        assert_eq!(
            parse_icon_spec("C:\\Games, Inc\\app.ico"),
            "C:\\Games, Inc\\app.ico",
            "a comma inside a directory name was mistaken for a resource index"
        );
    }

    #[test]
    fn the_cache_asks_its_source_once_for_a_hit() {
        let ctx = egui::Context::default();
        let mut icons = LauncherIcons::default();
        let calls = Cell::new(0);
        let load = |_: &str| {
            calls.set(calls.get() + 1);
            Some(ColorImage::filled([2, 2], egui::Color32::RED))
        };

        assert!(icons.texture_with(&ctx, "steam", load).is_some());
        assert!(icons.texture_with(&ctx, "steam", load).is_some());
        assert!(icons.texture_with(&ctx, "steam", load).is_some());
        assert_eq!(calls.get(), 1, "the icon was extracted more than once");
    }

    #[test]
    fn the_cache_remembers_a_miss_too() {
        let ctx = egui::Context::default();
        let mut icons = LauncherIcons::default();
        let calls = Cell::new(0);
        let load = |_: &str| {
            calls.set(calls.get() + 1);
            None
        };

        assert!(icons.texture_with(&ctx, "manual", load).is_none());
        assert!(icons.texture_with(&ctx, "manual", load).is_none());
        assert_eq!(
            calls.get(),
            1,
            "a launcher that is not installed was probed again on the next frame"
        );
    }
}
