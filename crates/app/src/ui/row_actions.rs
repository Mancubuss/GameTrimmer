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
/// selected.
///
/// Two separate quoting facts have to hold at once, and satisfying one with
/// `Command`'s own escaping breaks the other:
///
/// 1. `/select,PATH` must stay one token - a space after the comma makes
///    Explorer select nothing.
/// 2. The path must be quoted *inside* that token, as `/select,"PATH"`.
///
/// This used to return an unquoted `/select,PATH` and rely on
/// `std::process::Command` to quote the embedded spaces. It does quote them -
/// but it quotes the argument *as a whole*, producing
/// `explorer.exe "/select,C:\Games\My Game\file.dll"`. Explorer does not parse
/// `/select,` from inside a quoted token, fails to read the argument at all,
/// and falls back to opening the default folder, which is why revealing a file
/// landed the user in Documents while revealing a game folder worked (there
/// the whole argument is just a path, and a quoted path is fine).
///
/// So the quotes are placed here, deliberately, and [`launch`] passes the
/// argument through verbatim.
pub fn reveal_in_explorer_args(path: &Path) -> (&'static str, Vec<String>) {
    (
        "explorer.exe",
        vec![format!("/select,\"{}\"", windows_path_string(path))],
    )
}

/// Program + arguments to open `path` itself as a folder in Windows Explorer
/// (no `/select`). Used for a game's install dir, where the point is to land
/// *inside* the folder and look around - unlike `reveal_in_explorer_args`,
/// which opens the parent with the item highlighted.
///
/// Quoted here rather than by `Command` for the reason given on
/// [`reveal_in_explorer_args`]: [`launch`] no longer escapes anything.
pub fn open_folder_args(path: &Path) -> (&'static str, Vec<String>) {
    ("explorer.exe", vec![quoted(&windows_path_string(path))])
}

/// `text` wrapped in double quotes for a Windows command line.
///
/// Every path this module hands to [`launch`] goes through here. Windows paths
/// cannot contain `"` at all, so there is nothing to escape inside - the only
/// job is keeping a path with spaces as one argument now that [`launch`] does
/// no escaping of its own.
fn quoted(text: &str) -> String {
    format!("\"{text}\"")
}

/// Program + arguments for the Windows "Open with..." chooser dialog for
/// `path` (`rundll32.exe shell32.dll,OpenAs_RunDLL <path>`).
pub fn open_with_args(path: &Path) -> (&'static str, Vec<String>) {
    (
        "rundll32.exe",
        vec![
            "shell32.dll,OpenAs_RunDLL".to_string(),
            quoted(&windows_path_string(path)),
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
/// Spawns `program` with `args` passed to the command line **verbatim**.
///
/// `Command::args` would escape each argument, and its escaping is wrong for
/// Explorer's `/select,"PATH"` - see [`reveal_in_explorer_args`] for what that
/// cost. `raw_arg` hands the string through untouched, which makes the
/// builders above responsible for their own quoting; they all route their
/// paths through `quoted` for that reason.
pub fn launch(program: &str, args: &[String]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    let mut command = Command::new(program);
    for arg in args {
        command.raw_arg(arg);
    }
    command
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
        // Exactly one argument, the path attached to `/select,`
        // (space-separating it would make Explorer select nothing) and the
        // path quoted *inside* the token rather than around the whole of it.
        assert_eq!(
            args,
            vec!["/select,\"C:\\Games\\My Game\\file.dll\"".to_string()]
        );
    }

    /// The regression this file exists for: a path with a space used to be
    /// handed to `Command` unquoted, which quoted the whole `/select,...`
    /// token. Explorer could not read the argument and opened Documents
    /// instead of the file's folder. The quotes must sit around the path
    /// only, so that `/select,` stays outside them and parseable.
    #[test]
    fn reveal_quotes_the_path_and_not_the_select_switch() {
        let p = PathBuf::from(r"F:\SteamLibrary\steamapps\common\Counter-Strike Source\cs.ttf");
        let (_program, args) = reveal_in_explorer_args(&p);
        let arg = &args[0];
        assert!(
            arg.starts_with("/select,\""),
            "`/select,` must stay outside the quotes: {arg}"
        );
        assert!(arg.ends_with('"'), "the path must be closed: {arg}");
        assert!(
            !arg.starts_with('"'),
            "quoting the whole token is what Explorer cannot parse: {arg}"
        );
    }

    #[test]
    fn reveal_normalizes_forward_slashes_for_select() {
        let p = PathBuf::from("C:/Games/file.dll");
        let (_program, args) = reveal_in_explorer_args(&p);
        assert_eq!(args, vec!["/select,\"C:\\Games\\file.dll\"".to_string()]);
    }

    /// A folder argument is a bare path, so it carries its own quotes now
    /// that `launch` no longer adds any - without them a game whose name has
    /// a space would arrive as several arguments.
    #[test]
    fn open_folder_quotes_a_path_containing_spaces() {
        let p = PathBuf::from(r"H:\SteamLibrary\steamapps\common\The Finals");
        let (program, args) = open_folder_args(&p);
        assert_eq!(program, "explorer.exe");
        assert_eq!(
            args,
            vec!["\"H:\\SteamLibrary\\steamapps\\common\\The Finals\"".to_string()]
        );
    }

    #[test]
    fn open_folder_passes_the_bare_path_without_select() {
        let p = PathBuf::from(r"D:\Games\My Game");
        let (program, args) = open_folder_args(&p);
        assert_eq!(program, "explorer.exe");
        // No `/select,` - that would open the parent with "My Game"
        // highlighted instead of opening the folder itself. Quoted, because
        // `launch` now passes arguments through verbatim.
        assert_eq!(args, vec!["\"D:\\Games\\My Game\"".to_string()]);
    }

    #[test]
    fn open_folder_normalizes_forward_slashes() {
        let p = PathBuf::from("D:/Games/My Game");
        let (_program, args) = open_folder_args(&p);
        assert_eq!(args, vec!["\"D:\\Games\\My Game\"".to_string()]);
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
                // The verb is a single token with no spaces and needs no
                // quotes; the path carries its own, since `launch` adds none.
                "shell32.dll,OpenAs_RunDLL".to_string(),
                "\"C:\\Games\\My Game\\file.dll\"".to_string(),
            ]
        );
    }

    #[test]
    fn open_file_reports_error_for_a_nonexistent_path() {
        let p = PathBuf::from(r"C:\gametrimmer_non_existent_file_zzz.xyz");
        assert!(open_file(&p).is_err());
    }
}
