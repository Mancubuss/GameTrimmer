//! Administrator-elevation check and UAC relaunch.
//!
//! The MFT index scan path (`gametrimmer_core::mftscan`) needs raw read
//! access to `\\.\<letter>:`, which Windows only grants to an elevated
//! process. This module answers two questions for the rest of the app:
//! is the current process already elevated, and - if not - can it relaunch
//! itself elevated via a UAC prompt.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// Returns whether the current process holds an elevated (Administrator)
/// token. Returns `false` on any failure to query it - a query failure is
/// treated exactly like "not elevated", since that is the safe assumption:
/// the caller then either offers a UAC relaunch or falls back to the slower
/// walkdir scan path, never assumes MFT access it doesn't actually have.
pub fn is_elevated() -> bool {
    // SAFETY: `process` is a pseudo-handle (always valid, never needs
    // closing) returned by `GetCurrentProcess`. `token` is only read after
    // `OpenProcessToken` reports success, at which point it holds a real,
    // uniquely-owned handle that is closed exactly once below via
    // `CloseHandle`. `elevation`'s address is passed as a same-sized output
    // buffer matching `TOKEN_ELEVATION`'s layout, as `GetTokenInformation`
    // requires for `TokenElevation` queries.
    unsafe {
        let process = GetCurrentProcess();
        let mut token = HANDLE::default();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token).is_err() {
            return false;
        }

        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned_len = 0u32;
        let info_ptr = &mut elevation as *mut TOKEN_ELEVATION as *mut core::ffi::c_void;
        let queried = GetTokenInformation(
            token,
            TokenElevation,
            Some(info_ptr),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned_len,
        )
        .is_ok();

        let _ = CloseHandle(token);

        queried && elevation.TokenIsElevated != 0
    }
}

/// Attempts to relaunch the current executable elevated, via `ShellExecuteW`
/// with the `"runas"` verb (the same UAC prompt `right click -> Run as
/// administrator` triggers).
///
/// Returns `true` if Windows accepted the relaunch request (a new elevated
/// process is starting) - the caller should then exit the current process
/// promptly so the two don't both run. Returns `false` if the user declined
/// the UAC prompt, or the relaunch could not be started for any other
/// reason (missing `current_exe`, non-UTF-8 path, `ShellExecuteW` error);
/// this is an expected, non-fatal outcome the caller must handle by simply
/// continuing unelevated, never by panicking.
pub fn relaunch_elevated() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let Some(exe_path) = exe.to_str() else {
        return false;
    };

    let operation = to_wide("runas");
    let file = to_wide(exe_path);

    // SAFETY: `operation` and `file` are null-terminated UTF-16 buffers that
    // outlive this call (they are local `Vec`s dropped only after
    // `ShellExecuteW` returns). No window handle, parameters, or working
    // directory are needed, so those are passed as null/empty.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR::from_raw(operation.as_ptr()),
            PCWSTR::from_raw(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    // `ShellExecuteW` returns a pseudo-HINSTANCE for historical reasons:
    // values greater than 32 mean success, anything else (including the
    // user declining the UAC prompt, which shows up as
    // `SE_ERR_ACCESSDENIED`) is a failure code.
    (result.0 as usize) > 32
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `is_elevated` must never panic regardless of the test runner's own
    /// elevation state - this just exercises the whole Win32 call chain
    /// without asserting a specific outcome (which would make the test
    /// depend on how `cargo test` happens to be invoked).
    #[test]
    fn is_elevated_does_not_panic() {
        let _ = is_elevated();
    }
}
