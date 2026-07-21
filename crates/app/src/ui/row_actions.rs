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

/// `path` as a Windows-native string, with any forward slashes converted to
/// backslashes. Stored install dirs / relative paths occasionally use `/`, and
/// while most Windows APIs accept it, Explorer's `/select` silently ignores a
/// path that is not fully backslash-separated - so normalize before handing a
/// path to any of the shell helpers below (and when copying it to the
/// clipboard, so the user gets the canonical form).
pub fn windows_path_string(path: &Path) -> String {
    path.display().to_string().replace('/', "\\")
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
}
