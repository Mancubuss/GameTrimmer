//! Command-line argument parsing for the headless (CLI) mode (headless CLI mode).
//!
//! Pure, allocation-light, and fully unit-tested: [`parse_invocation`] turns
//! the process arguments (already stripped of `argv[0]`) into an
//! [`Invocation`] the entry point in [`super`] acts on, and never touches the
//! filesystem, the database, or Win32. The GUI is launched only when *no* CLI
//! flag is present, so double-clicking the exe behaves exactly as before this
//! mode existed.

use std::path::PathBuf;

/// Whether this build accepts any CLI flag at all.
///
/// Off in the v1 release build for two reasons: the headless mode only ever
/// reports (GT-89 removed `--apply` and the selection profile it deleted
/// with - see the module doc in [`super`] for why an automated delete has no
/// way to choose files any more) and, because the exe is built with
/// `windows_subsystem = "windows"`, the shell prints its prompt again the
/// instant the process is launched - the report then arrives *under* that
/// prompt, and the operator is left pressing Enter at a console that looks
/// finished. A read-only reporter that does not hand the user back their
/// console is not worth its surface area in a first release.
///
/// A `const` rather than `#[cfg]` on the module: everything under `cli` stays
/// compiled, type-checked and unit-tested in the default build, so switching
/// it back on is a build flag rather than an archaeology exercise.
pub const HEADLESS_ENABLED: bool = cfg!(feature = "headless");

/// What a build without the `headless` feature answers to *any* argument.
///
/// Any argument, not just the headless-selecting ones: `--help` and `--version`
/// exist to describe a command-line mode, and help that documents flags the
/// build refuses is worse than no help at all.
const HEADLESS_DISABLED_MSG: &str = "GameTrimmer has no command-line mode in this build: the \
     headless run could not delete anything, and it returned the shell prompt before its own \
     output, so v1 ships without it. Start GameTrimmer with no arguments to use the app";

/// What the process should do, decided purely from its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// No CLI flag was given - launch the graphical app (the default, and what
    /// double-clicking the exe does).
    Gui,
    /// Run headless with the given configuration and exit with a status code.
    Headless(HeadlessConfig),
    /// Print usage and exit successfully (`--help`/`-h`).
    Help,
    /// Print the version and exit successfully (`--version`/`-V`).
    Version,
    /// The arguments were invalid; the string is a user-facing explanation.
    /// The entry point prints it to stderr and exits with a usage error code.
    Error(String),
}

/// A validated headless configuration.
///
/// No mode and no profile any more (GT-89): a fresh scan pre-selects nothing,
/// so headless mode has nothing left to choose and nothing left to apply -
/// `--scan` and `--dry-run` both just mean "run the report". Unattended
/// deletion returns under its own explicitly named policy (board card
/// GT-EP24, "Авто-трим: unattended повторний трим після оновлення гри"), not
/// as a resurrected `--apply`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadlessConfig {
    /// Where to write the full text report, if requested (`--report <path>`).
    /// The report is also printed to the console regardless.
    pub report: Option<PathBuf>,
}

/// Parses process arguments (excluding `argv[0]`) into an [`Invocation`].
///
/// Rules:
/// - No arguments at all -> [`Invocation::Gui`], in every build. Double-clicking
///   the exe is unaffected by any of the rest of this.
/// - In a build without the `headless` feature, *any* argument ->
///   [`Invocation::Error`] naming the absent mode (see [`HEADLESS_ENABLED`]).
/// - No recognized flag at all -> [`Invocation::Gui`].
/// - `--help`/`-h` (plus the Windows spellings `/?`, `-?`, `/h`, `/help`) and
///   `--version`/`-V` short-circuit (help wins over version).
/// - Any of `--scan`, `--dry-run`, `--report` selects headless mode. There is
///   no `--apply` and no `--profile` any more (GT-89): a fresh scan
///   pre-selects nothing, so `--scan` and `--dry-run` are now plain synonyms
///   for "run the report" - it is the only thing headless mode does.
/// - An unknown flag or a missing/invalid value yields [`Invocation::Error`]
///   rather than a best-guess.
pub fn parse_invocation(args: &[String]) -> Invocation {
    // Before any parsing, so a build without the mode answers one thing to
    // every argument rather than "unknown argument" to some and a report to
    // others. No arguments still means the GUI, in every build.
    if !HEADLESS_ENABLED {
        return if args.is_empty() {
            Invocation::Gui
        } else {
            Invocation::Error(HEADLESS_DISABLED_MSG.to_string())
        };
    }

    let mut scan = false;
    let mut dry_run = false;
    let mut help = false;
    let mut version = false;
    let mut report: Option<PathBuf> = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--scan" => scan = true,
            "--dry-run" => dry_run = true,
            // `/?` and `-?` alongside the POSIX spellings: this is a Windows
            // exe, and `gametrimmer /?` is the first thing a Windows operator
            // types. Answering "unknown argument" to the platform's own help
            // convention reads as a broken program (MT-T01).
            "--help" | "-h" | "-?" | "/?" | "/h" | "/help" => help = true,
            "--version" | "-V" => version = true,
            _ => {
                // `--report <val>` / `--report=val`; anything else is an
                // error rather than a guess.
                if let Some(value) = flag_value(arg, "--report", &mut iter) {
                    match value {
                        Ok(v) => report = Some(PathBuf::from(v)),
                        Err(e) => return Invocation::Error(e),
                    }
                } else {
                    return Invocation::Error(format!("unknown argument: {arg}"));
                }
            }
        }
    }

    // Help and version short-circuit before any headless validation.
    if help {
        return Invocation::Help;
    }
    if version {
        return Invocation::Version;
    }

    let headless = scan || dry_run || report.is_some();
    if !headless {
        return Invocation::Gui;
    }

    Invocation::Headless(HeadlessConfig { report })
}

/// Extracts the value of a `--name value` / `--name=value` flag.
///
/// Returns `None` when `arg` is not this flag at all (so the caller can try
/// the next flag), `Some(Ok(value))` on success, and `Some(Err(msg))` when the
/// flag matched but its value is missing (`--name` at the end of the argument
/// list, or an empty `--name=`).
fn flag_value<'a>(
    arg: &str,
    name: &str,
    iter: &mut impl Iterator<Item = &'a String>,
) -> Option<Result<String, String>> {
    if arg == name {
        return match iter.next() {
            Some(value) => Some(Ok(value.clone())),
            None => Some(Err(format!("{name} needs a value"))),
        };
    }
    if let Some(rest) = arg.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
        if rest.is_empty() {
            return Some(Err(format!("{name} needs a value")));
        }
        return Some(Ok(rest.to_string()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_args_launches_gui() {
        assert_eq!(parse_invocation(&args(&[])), Invocation::Gui);
    }

    /// The v1 release build. Every argument gets the same answer, naming the
    /// absent mode - not "unknown argument", which would read as a typo in a
    /// flag that used to work, and not a usage block listing flags this build
    /// refuses.
    #[cfg(not(feature = "headless"))]
    mod without_the_feature {
        use super::*;

        #[test]
        fn every_flag_is_refused_with_the_same_reason() {
            for argv in [
                vec!["--scan"],
                vec!["--dry-run"],
                vec!["--report", "out.txt"],
                vec!["--help"],
                vec!["/?"],
                vec!["--version"],
                vec!["--frobnicate"],
            ] {
                match parse_invocation(&args(&argv)) {
                    Invocation::Error(msg) => assert!(
                        msg.contains("no command-line mode in this build"),
                        "expected the disabled-mode reason for {argv:?}, got: {msg}"
                    ),
                    other => panic!("expected a refusal for {argv:?}, got {other:?}"),
                }
            }
        }

        /// The one thing switching the mode off must not touch: double-clicking
        /// the exe passes no arguments and has to launch the app.
        #[test]
        fn a_bare_launch_still_opens_the_window() {
            assert_eq!(parse_invocation(&args(&[])), Invocation::Gui);
        }
    }

    /// Everything the headless mode does when it is switched on. Compiled and
    /// run only in a `--features headless` build - in the default one every
    /// argument is refused before parsing begins, so these would all be
    /// asserting against that refusal instead of against the parser.
    #[cfg(feature = "headless")]
    mod enabled {
        use super::*;

        #[test]
        fn scan_alone_is_headless() {
            assert_eq!(
                parse_invocation(&args(&["--scan"])),
                Invocation::Headless(HeadlessConfig { report: None })
            );
        }

        /// `--dry-run` used to be the opposite of `--apply`; with `--apply`
        /// gone (GT-89) it is just another spelling of "run the report".
        #[test]
        fn dry_run_flag_is_headless_too() {
            assert_eq!(
                parse_invocation(&args(&["--dry-run"])),
                Invocation::Headless(HeadlessConfig { report: None })
            );
        }

        #[test]
        fn report_captures_path_in_both_forms() {
            let space = parse_invocation(&args(&["--scan", "--report", "out.txt"]));
            let equals = parse_invocation(&args(&["--scan", "--report=out.txt"]));
            let expected = Invocation::Headless(HeadlessConfig {
                report: Some(PathBuf::from("out.txt")),
            });
            assert_eq!(space, expected);
            assert_eq!(equals, expected);
        }

        #[test]
        fn report_alone_is_headless() {
            // Even with no --scan/--dry-run, asking for a report means "run headless".
            assert_eq!(
                parse_invocation(&args(&["--report", "r.txt"])),
                Invocation::Headless(HeadlessConfig {
                    report: Some(PathBuf::from("r.txt")),
                })
            );
        }

        #[test]
        fn unknown_flag_is_error() {
            match parse_invocation(&args(&["--frobnicate"])) {
                Invocation::Error(msg) => assert!(msg.contains("unknown argument"), "got: {msg}"),
                other => panic!("expected error, got {other:?}"),
            }
        }

        #[test]
        fn report_missing_value_at_end_is_error() {
            match parse_invocation(&args(&["--scan", "--report"])) {
                Invocation::Error(msg) => assert!(msg.contains("needs a value"), "got: {msg}"),
                other => panic!("expected error, got {other:?}"),
            }
        }

        #[test]
        fn empty_equals_value_is_error() {
            match parse_invocation(&args(&["--report="])) {
                Invocation::Error(msg) => assert!(msg.contains("needs a value"), "got: {msg}"),
                other => panic!("expected error, got {other:?}"),
            }
        }

        #[test]
        fn help_wins_over_everything() {
            assert_eq!(
                parse_invocation(&args(&["--scan", "--help"])),
                Invocation::Help
            );
            assert_eq!(parse_invocation(&args(&["-h"])), Invocation::Help);
        }

        /// The Windows help conventions must reach the same place as `--help`
        /// (MT-T01) - `/?` in particular, which is what an operator types first on
        /// this platform.
        #[test]
        fn windows_help_conventions_are_accepted_too() {
            for flag in ["/?", "-?", "/h", "/help"] {
                assert_eq!(
                    parse_invocation(&args(&[flag])),
                    Invocation::Help,
                    "flag: {flag}"
                );
            }
        }

        #[test]
        fn version_flag() {
            assert_eq!(parse_invocation(&args(&["--version"])), Invocation::Version);
            assert_eq!(parse_invocation(&args(&["-V"])), Invocation::Version);
        }

        #[test]
        fn help_beats_version_when_both_present() {
            assert_eq!(
                parse_invocation(&args(&["--version", "--help"])),
                Invocation::Help
            );
        }
    }
}
