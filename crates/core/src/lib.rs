pub mod atomic_file;
pub mod bundle;
pub mod db;
pub mod error;
pub mod gamestate;
pub mod hardlink;
pub mod langdetect;
pub mod localized;
pub mod mftscan;
pub mod ondisk;
pub mod ops;
pub mod orphans;
pub mod packs;
pub mod perf;
pub mod providers;
pub mod rules;
pub mod safety;
pub mod scanner;
pub mod settings;
pub mod standalone;
pub mod sysinfo;

#[cfg(test)]
mod portability_regression_tests {
    /// `eframe`'s `persistence` feature (pulled in transitively via
    /// `directories` + `ron`) would make `eframe::run_native` write window
    /// state to `%APPDATA%` on its own, outside this app's "everything next
    /// to the exe" contract - and nothing in this codebase enables it today.
    ///
    /// This is a regression guard, not a correctness test: it greps the
    /// workspace `Cargo.lock` for those two crates so that a future `eframe`
    /// upgrade that silently defaults `persistence` to on gets caught here
    /// instead of via a support report about a stray `%APPDATA%` file.
    #[test]
    fn cargo_lock_does_not_pull_in_eframe_persistence_deps() {
        let lock_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock");
        let lock_contents = std::fs::read_to_string(lock_path)
            .expect("read workspace Cargo.lock (run from within the workspace)");

        for forbidden in ["name = \"directories\"", "name = \"ron\""] {
            assert!(
                !lock_contents.contains(forbidden),
                "Cargo.lock now contains {forbidden} - this likely means eframe's \
                 `persistence` feature got enabled (directly or transitively), which \
                 would write window state to %APPDATA%. If intentional, update this \
                 test and the README's \"leaves no traces\" promise to match."
            );
        }
    }
}
