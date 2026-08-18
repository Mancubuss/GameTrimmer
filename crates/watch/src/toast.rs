//! Windows Notification & Toast/Balloon helper.
//!
//! Provides clean formatting and Win32 balloon/toast notifications via `Shell_NotifyIconW`.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_INFO, NIIF_INFO, NIIF_LARGE_ICON, NIM_MODIFY, NOTIFYICONDATAW,
};

use crate::i18n::WatchStrings;

/// Formats a clean title and message for a game update notification using localized strings.
pub fn format_game_updated_toast(
    strings: &WatchStrings,
    name: &str,
    launcher: &str,
    old_build: Option<&str>,
    new_build: Option<&str>,
) -> (String, String) {
    let launcher_title = match launcher.to_lowercase().as_str() {
        "steam" => "Steam",
        "epic" => "Epic Games",
        "gog" => "GOG Galaxy",
        _ => "GameTrimmer",
    };

    let title = format!("GameTrimmer • {launcher_title}");

    let body = match (old_build, new_build) {
        (Some(old_b), Some(new_b)) if old_b != new_b => {
            strings
                .toast_updated_transition
                .replace("{name}", name)
                .replace("{old}", old_b)
                .replace("{new}", new_b)
        }
        (_, Some(new_b)) => {
            strings
                .toast_updated_build
                .replace("{name}", name)
                .replace("{new}", new_b)
        }
        _ => strings.toast_files_changed.replace("{name}", name),
    };

    (title, body)
}

/// Formats a toast message when re-trim completes or daemon state changes.
pub fn format_daemon_status_toast(strings: &WatchStrings, status: &str) -> (String, String) {
    (strings.toast_daemon_title.clone(), status.to_string())
}

/// Shows a Win32 Balloon notification through the tray icon's HWND.
pub fn show_tray_balloon(hwnd: HWND, icon_id: u32, title: &str, body: &str) -> bool {
    let mut nid = NOTIFYICONDATAW::default();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = icon_id;
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = NIIF_INFO | NIIF_LARGE_ICON;

    let title_wide = to_wide_capped(title, 64);
    nid.szInfoTitle[..title_wide.len()].copy_from_slice(&title_wide);

    let body_wide = to_wide_capped(body, 256);
    nid.szInfo[..body_wide.len()].copy_from_slice(&body_wide);

    unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) }.as_bool()
}

fn to_wide_capped(s: &str, max_len: usize) -> Vec<u16> {
    let mut wide: Vec<u16> = s.encode_utf16().take(max_len - 1).collect();
    wide.push(0);
    wide
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_game_updated_toast_build_transition() {
        let strings = WatchStrings::english();
        let (title, body) =
            format_game_updated_toast(&strings, "Cyberpunk 2077", "steam", Some("100"), Some("200"));
        assert_eq!(title, "GameTrimmer • Steam");
        assert!(body.contains("Cyberpunk 2077 was updated (100 → 200)"));
    }

    #[test]
    fn format_game_updated_toast_single_build() {
        let strings = WatchStrings::english();
        let (title, body) =
            format_game_updated_toast(&strings, "Fortnite", "epic", None, Some("v29.40"));
        assert_eq!(title, "GameTrimmer • Epic Games");
        assert!(body.contains("Fortnite was updated (build v29.40)"));
    }

    #[test]
    fn format_game_updated_toast_ukrainian() {
        let strings = WatchStrings::ukrainian();
        let (title, body) =
            format_game_updated_toast(&strings, "Cyberpunk 2077", "steam", Some("100"), Some("200"));
        assert_eq!(title, "GameTrimmer • Steam");
        assert!(body.contains("Cyberpunk 2077 оновлено (100 → 200)"));
    }
}
