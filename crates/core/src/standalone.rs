//! Games installed by a developer's own installer, past every launcher.
//!
//! No provider finds these: providers read a launcher's private store, and
//! there is no launcher. The heuristic folder scan does not find them either -
//! it looks *inside* known launcher roots, and one of these can be anywhere.
//! Found on a live machine as Path of Exile 1 and 2, tens of gigabytes each,
//! visible on disk and absent from the app with no way for the user to tell
//! why. That is a blind spot in the product's main promise, not a coverage gap.
//!
//! # What this deliberately is not
//!
//! It is **not** a provider, and it never reports a game. The obvious design -
//! "every folder in the Windows uninstall registry is a game" - is a heuristic
//! that is wrong often, in a tool that deletes files, where being wrong is
//! expensive. Windows' uninstall registry lists every installed program:
//! drivers, runtimes, the browser, this app.
//!
//! So this produces *candidates to offer the user*, phrased as "these are
//! installed outside any launcher; if one is a game, add its folder". The user
//! decides, and what they accept becomes an ordinary manual library, which the
//! rest of the app already understands. The value is that the suggestion
//! carries the concrete path, so accepting it is one click instead of hunting
//! through a folder picker.
//!
//! # Why the install path needs three sources
//!
//! Verified on a live machine: Path of Exile *has* a normal uninstall entry,
//! but its `InstallLocation` is **empty**.
//!
//! The obvious fallbacks - `DisplayIcon` and `UninstallString`, which must
//! point at something real for the entry to function - were tried first and
//! measured against that same machine. **They do not work for this case.**
//! Both of Path of Exile's point into
//! `C:\ProgramData\Package Cache\{GUID}\PathOfExileInstaller.exe`: the
//! bootstrapper's cached copy of the installer, not the game. They are kept
//! because they do resolve for plenty of ordinary installers, but they are not
//! what finds the case this card was written about.
//!
//! What finds it is the second source: a vendor's own key under `SOFTWARE`
//! declaring `InstallLocation`. Path of Exile's really is
//! `HKCU\SOFTWARE\GrindingGearGames\Path of Exile\InstallLocation =
//! H:\POE\POE1\`. That is swept generically - every vendor key at depth one
//! or two that declares the value - rather than as a per-vendor list, which is
//! the part that would not have generalized. Measured on this machine, the
//! whole `HKCU\SOFTWARE` sweep yields twelve such keys, so the noise this adds
//! is small and every one of them still has to survive the filters below.

use std::path::{Path, PathBuf};

/// One program installed outside any launcher, with a resolved directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandaloneCandidate {
    /// `DisplayName` from the uninstall entry - what Windows itself calls it.
    pub name: String,
    pub install_dir: PathBuf,
    pub publisher: Option<String>,
}

/// The uninstall-registry fields this needs, lifted out of the registry so the
/// resolution rules can be tested without one.
#[derive(Debug, Clone, Default)]
pub struct UninstallEntry {
    pub display_name: Option<String>,
    pub publisher: Option<String>,
    pub install_location: Option<String>,
    pub display_icon: Option<String>,
    pub uninstall_string: Option<String>,
    /// `1` marks an entry Windows hides from Add/Remove Programs.
    pub system_component: bool,
    /// Set on entries that are updates or components of another entry.
    pub has_parent: bool,
}

/// Strips the decoration Windows allows around a path in these fields: a
/// quoted string, an `,index` icon suffix, and trailing whitespace.
///
/// `DisplayIcon` is routinely `"C:\Game\game.exe,0"`, and `UninstallString` is
/// routinely `"C:\Game\uninstall.exe" /S` - taking either verbatim yields a
/// path that does not exist, which is how this quietly finds nothing.
fn clean_path_field(raw: &str) -> Option<PathBuf> {
    let raw = raw.trim();
    let unquoted = if let Some(rest) = raw.strip_prefix('"') {
        // Everything up to the closing quote; arguments after it are dropped.
        rest.split('"').next()?
    } else {
        // Unquoted: arguments would be space-separated, but so are many real
        // directory names, so only the icon-index suffix is stripped and the
        // rest is taken as-is. A wrong guess here fails the "does it exist"
        // check below rather than producing a bad candidate.
        raw
    };
    let without_icon_index = match unquoted.rsplit_once(',') {
        // Only when what follows is a plain number, so `C:\A,B\game.exe`
        // survives.
        Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => head,
        _ => unquoted,
    };
    let trimmed = without_icon_index.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

/// The directory an uninstall entry says its program lives in, or `None` when
/// it does not say anything usable.
///
/// `InstallLocation` first, because it is the field that means exactly this.
/// The other two are read as "the directory containing that executable" - see
/// the module docs for why an empty `InstallLocation` is common enough to
/// matter.
pub fn resolve_install_dir(entry: &UninstallEntry) -> Option<PathBuf> {
    if let Some(location) = entry.install_location.as_deref().and_then(clean_path_field) {
        return Some(location);
    }
    for field in [&entry.display_icon, &entry.uninstall_string] {
        if let Some(parent) = field
            .as_deref()
            .and_then(clean_path_field)
            .and_then(|path| path.parent().map(Path::to_path_buf))
        {
            if !parent.as_os_str().is_empty() {
                return Some(parent);
            }
        }
    }
    None
}

/// Whether this entry is worth showing at all, before its path is even looked
/// at. Cheap, obvious exclusions only - this is not trying to decide what a
/// game is.
pub fn entry_is_offerable(entry: &UninstallEntry) -> bool {
    if entry.system_component || entry.has_parent {
        return false;
    }
    entry
        .display_name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty())
}

/// Whether `dir` is somewhere it would be pointless or wrong to offer.
///
/// Two reasons to drop a directory: the app already covers it (it is inside a
/// library some launcher or the user already registered - offering it again
/// would be noise, and adding it would duplicate a library), or it is a place
/// no game is installed to and where a manual library would sweep system
/// files. Compared case-insensitively, the way Windows compares paths.
pub fn dir_is_offerable(dir: &Path, known_roots: &[PathBuf]) -> bool {
    let normalized = normalize(dir);
    if normalized.is_empty() {
        return false;
    }
    // A drive root as an "install directory" means the entry's fields were
    // decoration this could not parse. Adding one as a library would scan a
    // whole volume.
    if dir.parent().is_none() {
        return false;
    }
    for root in known_roots {
        let root = normalize(root);
        if root.is_empty() {
            continue;
        }
        // Either direction disqualifies: inside a known library, or a parent
        // of one (which would make the known library a subfolder of a new,
        // overlapping one).
        if is_within(&normalized, &root) || is_within(&root, &normalized) {
            return false;
        }
    }
    !system_locations().iter().any(|excluded| {
        let excluded = normalize(excluded);
        !excluded.is_empty() && is_within(&normalized, &excluded)
    })
}

fn normalize(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// Whether `path` is `root` or sits under it. Compares whole segments, so
/// `C:\Games2` is not "within" `C:\Games`.
fn is_within(path: &str, root: &str) -> bool {
    path == root
        || (path.len() > root.len()
            && path.starts_with(root)
            && path.as_bytes()[root.len()] == b'\\')
}

/// Where ordinary applications install, plus the places a manual library would
/// be actively harmful.
///
/// This is the filter that makes the feature usable rather than a wall of
/// noise. Swept unfiltered, this machine's registry offers around two hundred
/// entries - 7-Zip, Android Studio, Citrix, the browser - and three of them are
/// games. Nobody reads that list.
///
/// The rule is deliberately about *location*, not about what a program is: this
/// cannot tell a game from a driver and does not try. What it can observe is
/// that a game installed past every launcher is, almost by construction,
/// somewhere the user chose - `H:\POE\POE1`, not `C:\Program Files`. Ordinary
/// applications take the default, and the default is one of these.
///
/// The cost is stated plainly rather than hidden: a game that *did* install to
/// `Program Files` is not offered. That is a miss, and the folder picker is
/// still there for it. The alternative - offering everything - makes the
/// feature useless for the case it exists for, which is a worse failure than a
/// miss the user has another route around.
fn system_locations() -> Vec<PathBuf> {
    let mut locations: Vec<PathBuf> = ["SystemRoot", "ProgramData", "ProgramFiles"]
        .iter()
        .filter_map(|var| std::env::var_os(var).map(PathBuf::from))
        .collect();
    // `ProgramFiles(x86)` is not a valid Rust identifier for the array above
    // and is absent on 32-bit Windows, so it is read separately.
    if let Some(x86) = std::env::var_os("ProgramFiles(x86)") {
        locations.push(PathBuf::from(x86));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let local = PathBuf::from(local);
        // Where per-user installers and package managers put applications.
        locations.push(local.join("Programs"));
        locations.push(local.join("Microsoft").join("WinGet"));
        // Burn/WiX bootstrappers keep a copy of the installer here and point
        // `DisplayIcon`/`UninstallString` at it, so entries resolve to
        // `Package Cache\{GUID}` - a cache, never an install. Observed on this
        // machine for PowerToys, Python and the Paradox launcher, and it is
        // also why Path of Exile's own uninstall entry is a dead end (see the
        // module docs). `ProgramData\Package Cache` is already covered by the
        // `ProgramData` entry above.
        locations.push(local.join("Package Cache"));
    }
    locations
}

/// Turns raw uninstall entries into the candidates worth offering.
///
/// Split from the registry read so the whole decision is testable: the
/// registry sweep supplies entries, this decides.
pub fn candidates_from_entries(
    entries: impl IntoIterator<Item = UninstallEntry>,
    known_roots: &[PathBuf],
    directory_holds_files: impl Fn(&Path) -> bool,
) -> Vec<StandaloneCandidate> {
    let mut found: Vec<StandaloneCandidate> = Vec::new();
    for entry in entries {
        if !entry_is_offerable(&entry) {
            continue;
        }
        let Some(install_dir) = resolve_install_dir(&entry) else {
            continue;
        };
        if !dir_is_offerable(&install_dir, known_roots) {
            continue;
        }
        if !directory_holds_files(&install_dir) {
            continue;
        }
        // Several entries can share one directory (a game plus its bundled
        // redistributable). One offer per directory - the user is choosing a
        // folder, not a program.
        if found
            .iter()
            .any(|existing| normalize(&existing.install_dir) == normalize(&install_dir))
        {
            continue;
        }
        found.push(StandaloneCandidate {
            name: entry.display_name.unwrap_or_default().trim().to_string(),
            install_dir,
            publisher: entry.publisher,
        });
    }
    found.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    found
}

/// Reads the Windows uninstall registry - both the 64- and 32-bit views, under
/// both the machine and the current user - and returns the candidates worth
/// offering.
///
/// A key that cannot be read is skipped rather than failing the sweep: this is
/// a convenience that suggests folders, and one unreadable vendor key must not
/// cost the user every other suggestion.
#[cfg(windows)]
pub fn find_candidates(known_roots: &[PathBuf]) -> Vec<StandaloneCandidate> {
    use winreg::enums::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
    };
    use winreg::RegKey;

    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";

    let mut entries = Vec::new();
    for (root, flags) in [
        (HKEY_LOCAL_MACHINE, KEY_READ | KEY_WOW64_64KEY),
        (HKEY_LOCAL_MACHINE, KEY_READ | KEY_WOW64_32KEY),
        (HKEY_CURRENT_USER, KEY_READ | KEY_WOW64_64KEY),
    ] {
        let Ok(uninstall) = RegKey::predef(root).open_subkey_with_flags(UNINSTALL, flags) else {
            continue;
        };
        for name in uninstall.enum_keys().flatten() {
            let Ok(key) = uninstall.open_subkey_with_flags(&name, flags) else {
                continue;
            };
            let string = |value: &str| key.get_value::<String, _>(value).ok();
            entries.push(UninstallEntry {
                display_name: string("DisplayName"),
                publisher: string("Publisher"),
                install_location: string("InstallLocation"),
                display_icon: string("DisplayIcon"),
                uninstall_string: string("UninstallString"),
                system_component: key.get_value::<u32, _>("SystemComponent").unwrap_or(0) == 1,
                has_parent: string("ParentKeyName").is_some()
                    || string("ParentDisplayName").is_some(),
            });
        }
    }

    // Second source: vendor keys that declare their own `InstallLocation`.
    // See the module docs - this is the one that finds a game whose uninstall
    // entry points only at a cached bootstrapper.
    for (root, label) in [(HKEY_CURRENT_USER, "HKCU"), (HKEY_LOCAL_MACHINE, "HKLM")] {
        let _ = label;
        let Ok(software) = RegKey::predef(root)
            .open_subkey_with_flags("SOFTWARE", KEY_READ | KEY_WOW64_64KEY)
        else {
            continue;
        };
        for vendor_name in software.enum_keys().flatten() {
            let Ok(vendor) = software.open_subkey_with_flags(&vendor_name, KEY_READ) else {
                continue;
            };
            // Depth one: the vendor key itself declares it.
            if let Ok(location) = vendor.get_value::<String, _>("InstallLocation") {
                entries.push(UninstallEntry {
                    display_name: Some(vendor_name.clone()),
                    publisher: Some(vendor_name.clone()),
                    install_location: Some(location),
                    ..Default::default()
                });
            }
            // Depth two: one key per product under the vendor, which is where
            // Path of Exile puts it. Deeper is not swept - past this the keys
            // are configuration, not installation.
            for product_name in vendor.enum_keys().flatten() {
                let Ok(product) = vendor.open_subkey_with_flags(&product_name, KEY_READ) else {
                    continue;
                };
                if let Ok(location) = product.get_value::<String, _>("InstallLocation") {
                    entries.push(UninstallEntry {
                        display_name: Some(product_name),
                        publisher: Some(vendor_name.clone()),
                        install_location: Some(location),
                        ..Default::default()
                    });
                }
            }
        }
    }

    candidates_from_entries(entries, known_roots, |dir| {
        // The same probe folder discovery uses, in its non-fail-open form: an
        // unreadable directory is not offered rather than offered blindly.
        crate::providers::try_holds_installed_files(dir).unwrap_or(false)
    })
}

#[cfg(not(windows))]
pub fn find_candidates(_known_roots: &[PathBuf]) -> Vec<StandaloneCandidate> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> UninstallEntry {
        UninstallEntry {
            display_name: Some(name.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn install_location_wins_when_it_is_present() {
        let entry = UninstallEntry {
            install_location: Some(r"C:\Games\Thing".into()),
            display_icon: Some(r"C:\Elsewhere\thing.exe,0".into()),
            ..named("Thing")
        };
        assert_eq!(
            resolve_install_dir(&entry),
            Some(PathBuf::from(r"C:\Games\Thing"))
        );
    }

    /// The case this exists for, in the shape it was actually found in: a real
    /// uninstall entry whose `InstallLocation` is empty, with the path only
    /// reachable through the other fields.
    #[test]
    fn an_empty_install_location_falls_back_to_the_icon_directory() {
        let entry = UninstallEntry {
            install_location: Some(String::new()),
            display_icon: Some(r"H:\POE\POE1\PathOfExile.exe,0".into()),
            ..named("Path of Exile")
        };
        assert_eq!(
            resolve_install_dir(&entry),
            Some(PathBuf::from(r"H:\POE\POE1"))
        );
    }

    #[test]
    fn a_quoted_uninstall_string_with_arguments_still_yields_its_directory() {
        let entry = UninstallEntry {
            uninstall_string: Some(r#""H:\POE\POE2\uninstall.exe" /S --quiet"#.into()),
            ..named("Path of Exile 2")
        };
        assert_eq!(
            resolve_install_dir(&entry),
            Some(PathBuf::from(r"H:\POE\POE2"))
        );
    }

    /// A comma is legal in a directory name, so only a trailing *number* may
    /// be treated as an icon index.
    #[test]
    fn a_comma_in_a_real_directory_name_is_not_read_as_an_icon_index() {
        let entry = UninstallEntry {
            display_icon: Some(r"C:\Games\Sam, Max\game.exe".into()),
            ..named("Sam and Max")
        };
        assert_eq!(
            resolve_install_dir(&entry),
            Some(PathBuf::from(r"C:\Games\Sam, Max"))
        );
    }

    #[test]
    fn entries_windows_itself_hides_are_not_offered() {
        assert!(!entry_is_offerable(&UninstallEntry {
            system_component: true,
            ..named("Some Runtime")
        }));
        assert!(!entry_is_offerable(&UninstallEntry {
            has_parent: true,
            ..named("Security Update")
        }));
        assert!(!entry_is_offerable(&UninstallEntry::default()));
        assert!(entry_is_offerable(&named("A Real Program")));
    }

    #[test]
    fn a_directory_inside_a_known_library_is_not_offered_again() {
        let known = vec![PathBuf::from(r"F:\SteamLibrary")];
        assert!(!dir_is_offerable(
            Path::new(r"F:\SteamLibrary\steamapps\common\Game"),
            &known
        ));
        // Case and slash direction must not let the same folder through.
        assert!(!dir_is_offerable(
            Path::new(r"f:/steamlibrary/steamapps/common/Game"),
            &known
        ));
        // A sibling that merely shares a prefix is a different folder.
        assert!(dir_is_offerable(Path::new(r"F:\SteamLibrary2\Game"), &known));
    }

    /// Offering a parent of a registered library would make that library a
    /// subfolder of a new overlapping one.
    #[test]
    fn a_parent_of_a_known_library_is_not_offered() {
        let known = vec![PathBuf::from(r"F:\Games\SteamLibrary")];
        assert!(!dir_is_offerable(Path::new(r"F:\Games"), &known));
    }

    #[test]
    fn a_drive_root_is_never_offered() {
        assert!(!dir_is_offerable(Path::new(r"C:\"), &[]));
    }

    /// The location filter, checked against the real environment rather than
    /// hardcoded paths - `Program Files` is not `C:\Program Files` on every
    /// machine.
    #[test]
    fn ordinary_application_locations_are_not_offered() {
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            let inside = PathBuf::from(&program_files).join("Some App");
            assert!(
                !dir_is_offerable(&inside, &[]),
                "an app in Program Files is not what this looks for"
            );
        }
        // A folder the user chose is exactly what it does look for.
        assert!(dir_is_offerable(Path::new(r"H:\POE\POE1"), &[]));
    }

    #[test]
    fn one_offer_per_directory_even_with_several_entries_in_it() {
        let entries = vec![
            UninstallEntry {
                install_location: Some(r"H:\POE\POE1".into()),
                ..named("Path of Exile")
            },
            UninstallEntry {
                install_location: Some(r"H:\poe\poe1".into()),
                ..named("Path of Exile Redistributable")
            },
        ];
        let found = candidates_from_entries(entries, &[], |_| true);
        assert_eq!(found.len(), 1, "one folder is one offer: {found:?}");
        assert_eq!(found[0].name, "Path of Exile");
    }

    #[test]
    fn a_directory_with_nothing_in_it_is_not_offered() {
        let entries = vec![UninstallEntry {
            install_location: Some(r"H:\POE\POE1".into()),
            ..named("Path of Exile")
        }];
        assert!(candidates_from_entries(entries, &[], |_| false).is_empty());
    }

    /// Manual probe: what does the sweep actually find on *this* machine?
    ///
    /// `#[ignore]`d because the answer depends on what is installed, so it can
    /// assert nothing - but the rules above were written against a real
    /// registry and this is how that was checked, and how it can be checked
    /// again when a case turns up that this misses.
    ///
    /// ```text
    /// cargo test -p gametrimmer-core --lib standalone -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "depends on what is installed on this machine; prints, asserts nothing"]
    fn probe_what_this_machine_offers() {
        for candidate in find_candidates(&[]) {
            println!(
                "{}  [{}]  {}",
                candidate.name,
                candidate.publisher.as_deref().unwrap_or("-"),
                candidate.install_dir.display()
            );
        }
    }

    #[test]
    fn an_entry_that_names_no_directory_at_all_is_skipped() {
        let entries = vec![named("Something With No Path")];
        assert!(candidates_from_entries(entries, &[], |_| true).is_empty());
    }
}
