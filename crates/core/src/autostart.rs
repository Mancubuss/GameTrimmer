//! Windows registry helpers for autostart (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`).

use std::path::Path;

use crate::error::Result;

pub const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const AUTOSTART_VALUE_NAME: &str = "GameTrimmerWatch";

/// Checks whether autostart is enabled for `GameTrimmerWatch` in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
pub fn is_autostart_enabled() -> Result<bool> {
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key = match hkcu.open_subkey(RUN_KEY_PATH) {
            Ok(k) => k,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err.into()),
        };

        match key.get_value::<String, _>(AUTOSTART_VALUE_NAME) {
            Ok(val) => Ok(!val.trim().is_empty()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err.into()),
        }
    }
    #[cfg(not(windows))]
    {
        Ok(false)
    }
}

/// Enables or disables autostart in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\GameTrimmerWatch`.
///
/// If `enabled` is `true`, writes the quoted path to the executable. If `exe_path` is `None`,
/// looks for `gametrimmer-watch.exe` in the same directory as the current executable,
/// falling back to `current_exe()`.
/// If `enabled` is `false`, deletes the `GameTrimmerWatch` registry value if it exists.
pub fn set_autostart(enabled: bool, exe_path: Option<&Path>) -> Result<()> {
    #[cfg(windows)]
    {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        if enabled {
            let target_path = if let Some(path) = exe_path {
                path.to_path_buf()
            } else {
                let current = std::env::current_exe()?;
                let sibling_watch = current.with_file_name("gametrimmer-watch.exe");
                if sibling_watch.exists() {
                    sibling_watch
                } else {
                    current
                }
            };

            let command = format!("\"{}\"", target_path.display());
            let (key, _) = hkcu.create_subkey(RUN_KEY_PATH)?;
            key.set_value(AUTOSTART_VALUE_NAME, &command)?;
        } else {
            let key = match hkcu.open_subkey_with_flags(RUN_KEY_PATH, KEY_SET_VALUE) {
                Ok(k) => k,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(err) => return Err(err.into()),
            };

            match key.delete_value(AUTOSTART_VALUE_NAME) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (enabled, exe_path);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn autostart_constants_are_correct() {
        assert_eq!(
            RUN_KEY_PATH,
            r"Software\Microsoft\Windows\CurrentVersion\Run"
        );
        assert_eq!(AUTOSTART_VALUE_NAME, "GameTrimmerWatch");
    }

    #[cfg(windows)]
    #[test]
    fn set_and_check_autostart_roundtrip() {
        // Save initial state to restore after test
        let initial_state = is_autostart_enabled().unwrap_or(false);

        let test_exe = PathBuf::from(r"C:\Program Files\GameTrimmer\gametrimmer-watch.exe");

        // 1. Enable autostart with explicit path
        set_autostart(true, Some(&test_exe)).expect("enable autostart");
        assert!(is_autostart_enabled().expect("check autostart enabled"));

        // 2. Disable autostart
        set_autostart(false, None).expect("disable autostart");
        assert!(!is_autostart_enabled().expect("check autostart disabled"));

        // Restore initial state if it was enabled
        if initial_state {
            let _ = set_autostart(true, None);
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn autostart_on_non_windows_is_noop() {
        assert_eq!(is_autostart_enabled().unwrap(), false);
        assert!(set_autostart(true, None).is_ok());
        assert!(set_autostart(false, None).is_ok());
    }
}
