//! Lightweight localization for gametrimmer-watch daemon.
//!
//! Loads language preference from `gametrimmer.ini` and merges the corresponding
//! `locales/<lang>.json` over embedded fallback strings.

use std::collections::HashMap;
use std::path::Path;

use gametrimmer_core::settings::Lang;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchStrings {
    pub tray_tooltip_active: String,
    pub tray_tooltip_paused: String,
    pub tray_menu_open: String,
    pub tray_menu_check_now: String,
    pub tray_menu_pause: String,
    pub tray_menu_resume: String,
    pub tray_menu_exit: String,
    pub toast_updated_transition: String,
    pub toast_updated_build: String,
    pub toast_files_changed: String,
    pub toast_daemon_title: String,
}

impl Default for WatchStrings {
    fn default() -> Self {
        Self::english()
    }
}

impl WatchStrings {
    pub fn english() -> Self {
        Self {
            tray_tooltip_active: "GameTrimmer Watcher (Active)".to_string(),
            tray_tooltip_paused: "GameTrimmer Watcher (Paused)".to_string(),
            tray_menu_open: "Open GameTrimmer".to_string(),
            tray_menu_check_now: "Check now".to_string(),
            tray_menu_pause: "Pause monitoring".to_string(),
            tray_menu_resume: "Resume monitoring".to_string(),
            tray_menu_exit: "Exit".to_string(),
            toast_updated_transition:
                "{name} was updated ({old} → {new}). Click to re-trim and reclaim space."
                    .to_string(),
            toast_updated_build:
                "{name} was updated (build {new}). Click to re-trim and reclaim space.".to_string(),
            toast_files_changed: "{name} files changed. Click to re-trim and reclaim space."
                .to_string(),
            toast_daemon_title: "GameTrimmer Watcher".to_string(),
        }
    }

    pub fn ukrainian() -> Self {
        Self {
            tray_tooltip_active: "Фоновий монітор GameTrimmer (Активний)".to_string(),
            tray_tooltip_paused: "Фоновий монітор GameTrimmer (Призупинено)".to_string(),
            tray_menu_open: "Відкрити GameTrimmer".to_string(),
            tray_menu_check_now: "Перевірити зараз".to_string(),
            tray_menu_pause: "Призупинити моніторинг".to_string(),
            tray_menu_resume: "Відновити моніторинг".to_string(),
            tray_menu_exit: "Вийти".to_string(),
            toast_updated_transition:
                "{name} оновлено ({old} → {new}). Натисніть, щоб очистити рештки.".to_string(),
            toast_updated_build: "{name} оновлено (білд {new}). Натисніть, щоб очистити рештки."
                .to_string(),
            toast_files_changed: "Файли гри {name} змінилися. Натисніть, щоб очистити рештки."
                .to_string(),
            toast_daemon_title: "Фоновий монітор GameTrimmer".to_string(),
        }
    }

    pub fn load(exe_dir: &Path) -> Self {
        let ini_path = exe_dir.join("gametrimmer.ini");
        let settings = gametrimmer_core::settings::load_file(&ini_path).unwrap_or_default();

        let system_lang = detect_system_lang();
        let lang = settings.app_language.resolve(system_lang);

        let mut strings = match lang {
            Lang::Uk => Self::ukrainian(),
            _ => Self::english(),
        };

        let lang_code = lang.as_str();
        let locale_file = exe_dir.join("locales").join(format!("{lang_code}.json"));

        if locale_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&locale_file) {
                if let Ok(parsed) = serde_json::from_str::<LocaleFile>(&content) {
                    strings.merge_from_map(&parsed.strings);
                }
            }
        }

        strings
    }

    fn merge_from_map(&mut self, map: &HashMap<String, String>) {
        if let Some(v) = map.get("watch_tray_tooltip_active") {
            self.tray_tooltip_active = v.clone();
        }
        if let Some(v) = map.get("watch_tray_tooltip_paused") {
            self.tray_tooltip_paused = v.clone();
        }
        if let Some(v) = map.get("watch_tray_menu_open") {
            self.tray_menu_open = v.clone();
        }
        if let Some(v) = map.get("watch_tray_menu_check_now") {
            self.tray_menu_check_now = v.clone();
        }
        if let Some(v) = map.get("watch_tray_menu_pause") {
            self.tray_menu_pause = v.clone();
        }
        if let Some(v) = map.get("watch_tray_menu_resume") {
            self.tray_menu_resume = v.clone();
        }
        if let Some(v) = map.get("watch_tray_menu_exit") {
            self.tray_menu_exit = v.clone();
        }
        if let Some(v) = map.get("watch_toast_updated_transition") {
            self.toast_updated_transition = v.clone();
        }
        if let Some(v) = map.get("watch_toast_updated_build") {
            self.toast_updated_build = v.clone();
        }
        if let Some(v) = map.get("watch_toast_files_changed") {
            self.toast_files_changed = v.clone();
        }
        if let Some(v) = map.get("watch_toast_daemon_title") {
            self.toast_daemon_title = v.clone();
        }
    }
}

#[derive(Deserialize)]
struct LocaleFile {
    #[serde(default)]
    strings: HashMap<String, String>,
}

fn detect_system_lang() -> Lang {
    let mut count = 0u32;
    let mut length = 0u32;
    unsafe {
        if windows::Win32::Globalization::GetUserPreferredUILanguages(
            windows::Win32::Globalization::MUI_LANGUAGE_NAME,
            &mut count,
            None,
            &mut length,
        )
        .is_err()
            || length == 0
        {
            return Lang::En;
        }

        let mut buffer = vec![0u16; length as usize];
        if windows::Win32::Globalization::GetUserPreferredUILanguages(
            windows::Win32::Globalization::MUI_LANGUAGE_NAME,
            &mut count,
            Some(windows::core::PWSTR(buffer.as_mut_ptr())),
            &mut length,
        )
        .is_err()
        {
            return Lang::En;
        }

        let s = String::from_utf16_lossy(&buffer);
        for tag in s.split('\0') {
            let primary = tag.split('-').next().unwrap_or_default().to_lowercase();
            if primary == "uk" {
                return Lang::Uk;
            }
            if let Some(custom) = Lang::parse(&primary) {
                return custom;
            }
        }
    }
    Lang::En
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_strings_defaults() {
        let en = WatchStrings::english();
        assert_eq!(en.tray_menu_open, "Open GameTrimmer");
        let uk = WatchStrings::ukrainian();
        assert_eq!(uk.tray_menu_open, "Відкрити GameTrimmer");
    }

    #[test]
    fn watch_strings_merge() {
        let mut en = WatchStrings::english();
        let mut map = HashMap::new();
        map.insert(
            "watch_tray_menu_open".to_string(),
            "Ouvrir GameTrimmer".to_string(),
        );
        en.merge_from_map(&map);
        assert_eq!(en.tray_menu_open, "Ouvrir GameTrimmer");
        assert_eq!(en.tray_menu_check_now, "Check now");
    }
}
