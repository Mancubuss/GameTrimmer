//! Filesystem actions offered from a tree row's right-click context menu:
//! reveal the item in Windows Explorer, open the system "Open with..."
//! chooser, and copy the absolute path. GameTrimmer is Windows-only, so these
//! are the Windows shell helpers.
//!
//! The command wiring is split out as pure functions (`reveal_in_explorer_args`,
//! `open_with_args`, `windows_path_string`) - the part that is easy to get
//! subtly wrong, e.g. Explorer's `/select,PATH` needing to be a single token,
//! or a stored path using forward slashes that `/select` then ignores - so it
//! is unit-tested without spawning anything.

use std::path::Path;
use std::process::Command;

use windows::core::PCWSTR;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// `path` as a Windows-native string: forward slashes converted to
/// backslashes, and a leading drive letter upper-cased.
///
/// Stored install dirs / relative paths occasionally use `/`, and while most
/// Windows APIs accept it, Explorer's `/select` silently ignores a path that is
/// not fully backslash-separated - so normalize before handing a path to any of
/// the shell helpers below (and when copying it to the clipboard, so the user
/// gets the canonical form).
///
/// The drive letter is a display matter rather than a functional one (Windows
/// paths are case-insensitive), but launchers do not agree on its case -
/// Steam's `libraryfolders.vdf` can hand back `d:\portableapps\...` while every
/// other source says `F:\`. Mixed case down a list of paths reads as a bug in
/// the tool, so it is settled here, in the one place every user-visible path
/// passes through. `model::disk_label` already does the same for disk rows.
pub fn windows_path_string(path: &Path) -> String {
    let text = path.display().to_string().replace('/', "\\");

    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(drive), Some(':')) if drive.is_ascii_alphabetic() => {
            format!("{}{}", drive.to_ascii_uppercase(), &text[1..])
        }
        _ => text,
    }
}

/// Program + arguments to reveal `path` in Windows Explorer with the item
/// selected. `/select,PATH` MUST be a single argument - Explorer treats a
/// space after the comma as "no item to select" - so it is built as one token;
/// `std::process::Command` then quotes the embedded spaces when it constructs
/// the command line.
pub fn reveal_in_explorer_args(path: &Path) -> (&'static str, Vec<String>) {
    (
        "explorer.exe",
        vec![format!("/select,{}", windows_path_string(path))],
    )
}

/// Program + arguments to open `path` itself as a folder in Windows Explorer
/// (no `/select`). Used for a game's install dir, where the point is to land
/// *inside* the folder and look around - unlike `reveal_in_explorer_args`,
/// which opens the parent with the item highlighted.
pub fn open_folder_args(path: &Path) -> (&'static str, Vec<String>) {
    ("explorer.exe", vec![windows_path_string(path)])
}

/// Program + arguments for the Windows "Open with..." chooser dialog for
/// `path` (`rundll32.exe shell32.dll,OpenAs_RunDLL <path>`).
pub fn open_with_args(path: &Path) -> (&'static str, Vec<String>) {
    (
        "rundll32.exe",
        vec![
            "shell32.dll,OpenAs_RunDLL".to_string(),
            windows_path_string(path),
        ],
    )
}

/// Opens `path` with its default Windows associated application via
/// `ShellExecuteW` (e.g. default media player for video files, text editor
/// for config/text files).
pub fn open_file(path: &Path) -> Result<(), String> {
    let win_path = windows_path_string(path);
    let operation = to_wide("open");
    let file = to_wide(&win_path);

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

    let code = result.0 as usize;
    if code > 32 {
        Ok(())
    } else {
        Err(format!(
            "ShellExecuteW failed with code {code} for path {win_path}"
        ))
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Launches a fire-and-forget GUI helper, detached from this process. On
/// success the child keeps running on its own; on failure (the program could
/// not be started at all) an error string is returned for the caller to log -
/// the process handle is intentionally dropped, never waited on.
pub fn launch(program: &str, args: &[String]) -> Result<(), String> {
    Command::new(program)
        .args(args)
        .spawn()
        .map(|_child| ())
        .map_err(|err| format!("{program}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn windows_path_string_converts_forward_slashes() {
        let p = PathBuf::from("D:/Games/My Game/redist/vc.dll");
        assert_eq!(
            windows_path_string(&p),
            "D:\\Games\\My Game\\redist\\vc.dll"
        );
    }

    /// Steam's `libraryfolders.vdf` really does report a lowercase drive on
    /// this machine; every other provider reports an uppercase one, and the
    /// library list showed both.
    #[test]
    fn windows_path_string_upper_cases_the_drive_letter() {
        assert_eq!(
            windows_path_string(&PathBuf::from(r"d:\portableapps\portable\games\steam")),
            r"D:\portableapps\portable\games\steam"
        );
    }

    /// A UNC path has no drive letter to touch.
    #[test]
    fn windows_path_string_leaves_a_unc_path_alone() {
        assert_eq!(
            windows_path_string(&PathBuf::from(r"\\server\share\Games\Game")),
            r"\\server\share\Games\Game"
        );
    }

    #[test]
    fn windows_path_string_leaves_backslash_paths_unchanged() {
        let p = PathBuf::from(r"C:\Games\My Game\docs\manual.pdf");
        assert_eq!(windows_path_string(&p), r"C:\Games\My Game\docs\manual.pdf");
    }

    #[test]
    fn reveal_uses_explorer_with_single_select_token() {
        let p = PathBuf::from(r"C:\Games\My Game\file.dll");
        let (program, args) = reveal_in_explorer_args(&p);
        assert_eq!(program, "explorer.exe");
        // Exactly one argument, and it keeps the path attached to `/select,`
        // (space-separating it would make Explorer select nothing).
        assert_eq!(args, vec![r"/select,C:\Games\My Game\file.dll".to_string()]);
    }

    #[test]
    fn reveal_normalizes_forward_slashes_for_select() {
        let p = PathBuf::from("C:/Games/file.dll");
        let (_program, args) = reveal_in_explorer_args(&p);
        assert_eq!(args, vec![r"/select,C:\Games\file.dll".to_string()]);
    }

    #[test]
    fn open_folder_passes_the_bare_path_without_select() {
        let p = PathBuf::from(r"D:\Games\My Game");
        let (program, args) = open_folder_args(&p);
        assert_eq!(program, "explorer.exe");
        // No `/select,` - that would open the parent with "My Game"
        // highlighted instead of opening the folder itself.
        assert_eq!(args, vec![r"D:\Games\My Game".to_string()]);
    }

    #[test]
    fn open_folder_normalizes_forward_slashes() {
        let p = PathBuf::from("D:/Games/My Game");
        let (_program, args) = open_folder_args(&p);
        assert_eq!(args, vec![r"D:\Games\My Game".to_string()]);
    }

    #[test]
    fn launch_reports_error_for_a_missing_program() {
        // A program that cannot exist on PATH: spawn fails immediately with
        // NotFound and no window ever appears, so this is safe to run anywhere.
        let err = launch("gametrimmer-no-such-binary-zzz", &[]).unwrap_err();
        assert!(
            err.starts_with("gametrimmer-no-such-binary-zzz: "),
            "error should be prefixed with the program name, got {err:?}"
        );
    }

    #[test]
    fn open_with_invokes_shell32_openas_verb() {
        let p = PathBuf::from(r"C:\Games\My Game\file.dll");
        let (program, args) = open_with_args(&p);
        assert_eq!(program, "rundll32.exe");
        assert_eq!(
            args,
            vec![
                "shell32.dll,OpenAs_RunDLL".to_string(),
                r"C:\Games\My Game\file.dll".to_string(),
            ]
        );
    }

    #[test]
    fn open_file_reports_error_for_a_nonexistent_path() {
        let p = PathBuf::from(r"C:\gametrimmer_non_existent_file_zzz.xyz");
        assert!(open_file(&p).is_err());
    }
}
