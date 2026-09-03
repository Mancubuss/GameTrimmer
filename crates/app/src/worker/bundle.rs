//! The "Generate diagnostic bundle" job: ask where to save, collect and
//! compress the whole archive in memory, then write it in one atomic call.
//!
//! The save dialog runs first, before any collection, for one reason: a
//! user who changes their mind at the file picker should cost nothing, and
//! `rfd` blocks its thread until they answer either way. Cancelling there
//! is reported the same way `ExportDone` reports it - both `path` and
//! `error` `None`, meaning "nothing happened and nothing went wrong".
//!
//! Cancellation during assembly is checked between sections rather than
//! inside them, and it is safe at any point precisely because the archive
//! only exists as a `Vec<u8>` until the single
//! `gametrimmer_core::bundle::write` call at the end. There is no window in
//! which a partial file is on disk looking like a real one.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;

use gametrimmer_core::bundle::{self, BundleInput};

use crate::i18n::{self, Lang, Verb};

use super::{Notifier, Wake, WorkerMsg};

/// Spawns the whole job: save dialog, assembly, atomic write.
pub fn spawn_bundle(
    input: BundleInput,
    cancel: Arc<AtomicBool>,
    tx: Sender<WorkerMsg>,
    lang: Lang,
    wake: Wake,
) -> JoinHandle<()> {
    let notifier = Notifier::new(tx, wake);
    std::thread::spawn(move || run_bundle(input, &cancel, &notifier, lang))
}

fn run_bundle(input: BundleInput, cancel: &AtomicBool, notifier: &Notifier, lang: Lang) {
    // A plain suggested name, not one carrying the generation id: that id
    // is minted inside `bundle::build`, which has not run yet, and a second
    // id invented here would be a different number from the one the
    // manifest holds - exactly the confusion an id is supposed to prevent.
    // The manifest's `generation_id` is the one to quote in a report.
    let Some(target) = rfd::FileDialog::new()
        .set_title(i18n::strings(lang).bundle_save_title)
        .set_file_name(BUNDLE_FILE_NAME)
        .add_filter("ZIP", &["zip"])
        .save_file()
    else {
        // The user closed the picker. Not an error, and not a result.
        notifier.send(WorkerMsg::BundleDone {
            path: None,
            error: None,
        });
        return;
    };

    let mut sections_done = 0usize;
    let built = bundle::build(&input, &mut |section, index, total| {
        sections_done = index;
        notifier.send(WorkerMsg::Progress {
            verb: Verb::Bundle,
            current: index,
            total,
            detail: section.to_string(),
        });
        !cancel.load(Ordering::Relaxed)
    });

    let bundle = match built {
        Ok(Some(bundle)) => bundle,
        // Cancelled between sections: nothing was written, so this is
        // reported as a plain cancellation rather than a failure.
        Ok(None) => {
            notifier.send(WorkerMsg::Cancelled);
            return;
        }
        Err(err) => {
            notifier.send(WorkerMsg::BundleDone {
                path: None,
                error: Some(i18n::bundle_failed(lang, err)),
            });
            return;
        }
    };

    match bundle::write(&target, &bundle.bytes) {
        Ok(()) => notifier.send(WorkerMsg::BundleDone {
            path: Some(target),
            error: None,
        }),
        Err(err) => notifier.send(WorkerMsg::BundleDone {
            path: None,
            error: Some(i18n::bundle_failed(lang, err)),
        }),
    }
}

/// What the save dialog suggests. The user renames it or does not; nothing
/// downstream reads the name.
const BUNDLE_FILE_NAME: &str = "gametrimmer-diagnostics.zip";

/// The paths and options a bundle needs, resolved from the app's own
/// "everything next to the exe" rule.
///
/// `%USERPROFILE%` is read by name and only for the redaction pass, which
/// needs its value in order to remove it. Nothing here iterates
/// `env::vars()` - those values carry account, machine and domain names,
/// and in a corporate setting internal UNC paths.
pub fn input_from_paths(
    db_path: PathBuf,
    options: gametrimmer_core::bundle::BundleOptions,
    elevated: bool,
) -> std::io::Result<BundleInput> {
    Ok(BundleInput {
        db_path,
        settings_path: super::settings_path()?,
        rules_path: super::ensure_rules_path()?,
        log_path: super::log_path()?,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        elevated,
        user_profile: std::env::var("USERPROFILE").ok(),
        options,
    })
}
