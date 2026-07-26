//! Command-line argument parsing for the headless (CLI) mode (GT-10).
//!
//! Pure, allocation-light, and fully unit-tested: [`parse_invocation`] turns
//! the process arguments (already stripped of `argv[0]`) into an
//! [`Invocation`] the entry point in [`super`] acts on, and never touches the
//! filesystem, the database, or Win32. The GUI is launched only when *no* CLI
//! flag is present, so double-clicking the exe behaves exactly as before this
//! mode existed.

use std::path::PathBuf;

use gametrimmer_core::settings::SelectionProfile;

/// Whether this build accepts `--apply` at all (GT-15).
///
/// A plain `const` rather than `#[cfg]` attributes on every apply-related
/// branch: the deletion path stays compiled and type-checked (so it cannot rot
/// while it is switched off) but becomes unreachable, and re-enabling it is a
/// build flag rather than an archaeology exercise.
pub const APPLY_ENABLED: bool = cfg!(feature = "cli-apply");

/// Why `--apply` is refused in a build without the `cli-apply` feature. Says
/// what to use instead, so someone automating GameTrimmer is not left guessing
/// whether the flag was renamed.
const APPLY_DISABLED_MSG: &str = "--apply вимкнено в цій збірці: цей шлях видалення ще не \
     проганявся наживо, тож у v1 він не входить. Доступні --scan / --dry-run / --report — вони \
     нічого не видаляють";

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

/// Whether a headless run only reports what it *would* delete, or actually
/// deletes it. `--apply` is the sole path in the whole app where files are
/// removed without an explicit human click, so it is deliberately the
/// non-default: absent `--apply`, a headless run is always a dry run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Scan and report; delete nothing.
    DryRun,
    /// Scan, then delete the profile's auto-selected findings.
    Apply,
}

/// A validated headless configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadlessConfig {
    pub mode: Mode,
    /// The selection profile to apply. `None` only ever in [`Mode::DryRun`],
    /// meaning "use the persisted `selection_profile` setting"; [`Mode::Apply`]
    /// always carries an explicit profile (enforced in [`parse_invocation`]),
    /// so an automated delete can never silently inherit a surprising default.
    pub profile: Option<SelectionProfile>,
    /// Where to write the full text report, if requested (`--report <path>`).
    /// The report is also printed to the console regardless.
    pub report: Option<PathBuf>,
}

/// Parses process arguments (excluding `argv[0]`) into an [`Invocation`].
///
/// Rules:
/// - No recognized flag at all -> [`Invocation::Gui`].
/// - `--help`/`-h` and `--version`/`-V` short-circuit (help wins over version).
/// - Any of `--scan`, `--apply`, `--dry-run`, `--profile`, `--report` selects
///   headless mode.
/// - `--apply` requires an explicit `--profile <name>` and conflicts with
///   `--dry-run`.
/// - An unknown flag, a missing/invalid value, or a conflicting combination
///   yields [`Invocation::Error`] rather than a best-guess.
pub fn parse_invocation(args: &[String]) -> Invocation {
    let mut scan = false;
    let mut apply = false;
    let mut dry_run = false;
    let mut help = false;
    let mut version = false;
    let mut profile_raw: Option<String> = None;
    let mut report: Option<PathBuf> = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--scan" => scan = true,
            "--apply" => apply = true,
            "--dry-run" => dry_run = true,
            "--help" | "-h" => help = true,
            "--version" | "-V" => version = true,
            _ => {
                // `--profile <val>` / `--profile=val` and the same for
                // `--report`; anything else is an error rather than a guess.
                if let Some(value) = flag_value(arg, "--profile", &mut iter) {
                    match value {
                        Ok(v) => profile_raw = Some(v),
                        Err(e) => return Invocation::Error(e),
                    }
                } else if let Some(value) = flag_value(arg, "--report", &mut iter) {
                    match value {
                        Ok(v) => report = Some(PathBuf::from(v)),
                        Err(e) => return Invocation::Error(e),
                    }
                } else {
                    return Invocation::Error(format!("невідомий аргумент: {arg}"));
                }
            }
        }
    }

    // Help and version short-circuit before any headless validation, so
    // `--apply --help` still just prints help.
    if help {
        return Invocation::Help;
    }
    if version {
        return Invocation::Version;
    }

    // Checked after the help/version short-circuit above, so `--apply --help`
    // still prints help - and before every other headless validation, so the
    // reason reported is "this build has no --apply" rather than a complaint
    // about a missing --profile for a flag that is not going to run anyway.
    //
    // Refused outright rather than silently downgraded to a dry run: someone
    // who typed --apply expects files to go, and a run that quietly deletes
    // nothing would read as success.
    if apply && !APPLY_ENABLED {
        return Invocation::Error(APPLY_DISABLED_MSG.to_string());
    }

    let headless = scan || apply || dry_run || profile_raw.is_some() || report.is_some();
    if !headless {
        return Invocation::Gui;
    }

    if apply && dry_run {
        return Invocation::Error(
            "--apply і --dry-run взаємовиключні: --apply видаляє, --dry-run лише звітує"
                .to_string(),
        );
    }

    let profile = match profile_raw {
        Some(raw) => match SelectionProfile::parse(&raw) {
            Some(p) => Some(p),
            None => {
                return Invocation::Error(format!(
                    "невідомий профіль \"{raw}\": очікується cautious | balanced | aggressive | custom"
                ));
            }
        },
        None => None,
    };

    if apply && profile.is_none() {
        return Invocation::Error(
            "--apply вимагає явного --profile <назва> (запобіжник: автоматичне видалення \
             не успадковує профіль за замовчуванням)"
                .to_string(),
        );
    }

    let mode = if apply { Mode::Apply } else { Mode::DryRun };
    Invocation::Headless(HeadlessConfig {
        mode,
        profile,
        report,
    })
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
            None => Some(Err(format!("{name} потребує значення"))),
        };
    }
    if let Some(rest) = arg.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
        if rest.is_empty() {
            return Some(Err(format!("{name} потребує значення")));
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

    #[test]
    fn scan_alone_is_dry_run_with_no_explicit_profile() {
        assert_eq!(
            parse_invocation(&args(&["--scan"])),
            Invocation::Headless(HeadlessConfig {
                mode: Mode::DryRun,
                profile: None,
                report: None,
            })
        );
    }

    #[test]
    fn dry_run_flag_is_headless_dry_run() {
        assert_eq!(
            parse_invocation(&args(&["--dry-run"])),
            Invocation::Headless(HeadlessConfig {
                mode: Mode::DryRun,
                profile: None,
                report: None,
            })
        );
    }

    /// GT-15: in a build without the `cli-apply` feature - which is what v1
    /// ships - the flag is refused outright. Never downgraded to a dry run:
    /// someone who typed `--apply` expects files to go, and a run that quietly
    /// deleted nothing would read as success.
    #[cfg(not(feature = "cli-apply"))]
    #[test]
    fn apply_is_refused_in_a_build_without_the_feature() {
        for argv in [
            vec!["--apply"],
            vec!["--apply", "--profile", "aggressive"],
            vec!["--apply", "--dry-run", "--profile", "cautious"],
        ] {
            match parse_invocation(&args(&argv)) {
                Invocation::Error(msg) => assert!(
                    msg.contains("вимкнено в цій збірці"),
                    "expected the disabled-build reason for {argv:?}, got: {msg}"
                ),
                other => panic!("expected a refusal for {argv:?}, got {other:?}"),
            }
        }
    }

    #[cfg(feature = "cli-apply")]
    #[test]
    fn apply_requires_explicit_profile() {
        match parse_invocation(&args(&["--apply"])) {
            Invocation::Error(msg) => assert!(msg.contains("--profile"), "got: {msg}"),
            other => panic!("expected usage error, got {other:?}"),
        }
    }

    #[cfg(feature = "cli-apply")]
    #[test]
    fn apply_with_profile_is_apply_mode() {
        assert_eq!(
            parse_invocation(&args(&["--apply", "--profile", "aggressive"])),
            Invocation::Headless(HeadlessConfig {
                mode: Mode::Apply,
                profile: Some(SelectionProfile::Aggressive),
                report: None,
            })
        );
    }

    #[cfg(feature = "cli-apply")]
    #[test]
    fn apply_and_dry_run_conflict() {
        match parse_invocation(&args(&["--apply", "--dry-run", "--profile", "cautious"])) {
            Invocation::Error(msg) => assert!(msg.contains("взаємовиключн"), "got: {msg}"),
            other => panic!("expected conflict error, got {other:?}"),
        }
    }

    #[test]
    fn profile_accepts_equals_form() {
        assert_eq!(
            parse_invocation(&args(&["--scan", "--profile=balanced"])),
            Invocation::Headless(HeadlessConfig {
                mode: Mode::DryRun,
                profile: Some(SelectionProfile::Balanced),
                report: None,
            })
        );
    }

    #[test]
    fn report_captures_path_in_both_forms() {
        let space = parse_invocation(&args(&["--scan", "--report", "out.txt"]));
        let equals = parse_invocation(&args(&["--scan", "--report=out.txt"]));
        let expected = Invocation::Headless(HeadlessConfig {
            mode: Mode::DryRun,
            profile: None,
            report: Some(PathBuf::from("out.txt")),
        });
        assert_eq!(space, expected);
        assert_eq!(equals, expected);
    }

    #[test]
    fn report_alone_is_headless_dry_run() {
        // Even with no --scan/--dry-run, asking for a report means "run headless".
        assert_eq!(
            parse_invocation(&args(&["--report", "r.txt"])),
            Invocation::Headless(HeadlessConfig {
                mode: Mode::DryRun,
                profile: None,
                report: Some(PathBuf::from("r.txt")),
            })
        );
    }

    #[test]
    fn unknown_flag_is_error() {
        match parse_invocation(&args(&["--frobnicate"])) {
            Invocation::Error(msg) => assert!(msg.contains("невідомий аргумент"), "got: {msg}"),
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn invalid_profile_name_is_error() {
        match parse_invocation(&args(&["--scan", "--profile", "reckless"])) {
            Invocation::Error(msg) => assert!(msg.contains("reckless"), "got: {msg}"),
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn profile_missing_value_at_end_is_error() {
        match parse_invocation(&args(&["--scan", "--profile"])) {
            Invocation::Error(msg) => assert!(msg.contains("потребує значення"), "got: {msg}"),
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn empty_equals_value_is_error() {
        match parse_invocation(&args(&["--report="])) {
            Invocation::Error(msg) => assert!(msg.contains("потребує значення"), "got: {msg}"),
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn help_wins_over_everything() {
        assert_eq!(
            parse_invocation(&args(&["--apply", "--help"])),
            Invocation::Help
        );
        assert_eq!(parse_invocation(&args(&["-h"])), Invocation::Help);
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

    #[test]
    #[cfg(feature = "cli-apply")]
    fn apply_accepts_custom_profile_explicitly() {
        // `custom` is a valid explicit choice (the plain confidence path); only
        // *omitting* the profile under --apply is rejected.
        assert_eq!(
            parse_invocation(&args(&["--apply", "--profile", "custom"])),
            Invocation::Headless(HeadlessConfig {
                mode: Mode::Apply,
                profile: Some(SelectionProfile::Custom),
                report: None,
            })
        );
    }
}
