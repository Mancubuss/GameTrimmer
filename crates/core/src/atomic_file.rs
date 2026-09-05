//! Durable sibling-file replacement with validation and a recovery backup.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Error, ErrorKind, Result, Write};
use std::path::{Path, PathBuf};

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

type StagedReplacement = (PathBuf, PathBuf, PathBuf, bool);

fn cleanup_temps(staged: &[StagedReplacement]) {
    for (_, temp, _, _) in staged {
        let _ = fs::remove_file(temp);
    }
}

#[cfg(windows)]
fn replace(from: &Path, to: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        MoveFileExW(
            PCWSTR(from.as_ptr()),
            PCWSTR(to.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    // `windows::core::Error` already captured this call's own `GetLastError`
    // as an `HRESULT` (`Error::from_win32` folds a Win32 code into one via
    // `(code & 0xFFFF) | 0x80070000` - the standard `HRESULT_CODE` encoding,
    // reversed below). `Error::last_os_error()` instead re-reads the
    // thread-local last-error slot *now*, after this call already returned -
    // any other fallible call executed by anything on this thread in between
    // (including inside `windows`/`std` internals) can have overwritten it,
    // so the previous code risked reporting the wrong failure entirely.
    .map_err(|error| Error::from_raw_os_error((error.code().0 as u32 & 0xFFFF) as i32))
}

#[cfg(not(windows))]
fn replace(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to)
}

/// Atomically replaces a set of files as one logical batch. Every payload is
/// staged, flushed and validated before any target changes. If a later replace
/// or reopen validation fails, already-replaced targets are restored from
/// their recovery backups.
pub fn atomic_write_batch_with_backup(
    files: &[(&Path, &[u8])],
    validate: impl Fn(&Path, &[u8]) -> Result<()>,
) -> Result<()> {
    for (index, (target, _)) in files.iter().enumerate() {
        if files[index + 1..].iter().any(|(other, _)| other == target) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "duplicate atomic target",
            ));
        }
    }
    let mut staged: Vec<StagedReplacement> = Vec::with_capacity(files.len());
    for (target, bytes) in files {
        if let Err(error) = validate(target, bytes) {
            cleanup_temps(&staged);
            return Err(error);
        }
        let temp = sibling(target, ".replace-tmp");
        let backup = sibling(target, ".bak");
        let _ = fs::remove_file(&temp);
        if let Err(error) = write_synced(&temp, bytes) {
            let _ = fs::remove_file(&temp);
            cleanup_temps(&staged);
            return Err(error);
        }
        let existed = target.is_file();
        if existed {
            let backup_result = fs::copy(target, &backup)
                .and_then(|_| OpenOptions::new().write(true).open(&backup)?.sync_all());
            if let Err(error) = backup_result {
                let _ = fs::remove_file(&temp);
                cleanup_temps(&staged);
                return Err(error);
            }
        }
        staged.push((target.to_path_buf(), temp, backup, existed));
    }

    let mut replaced = 0usize;
    let commit_result = (|| {
        for (target, temp, _, _) in &staged {
            replace(temp, target)?;
            // From this point the target was mutated and must participate in
            // rollback even if reopen validation fails below.
            replaced += 1;
            let reopened = fs::read(target)?;
            validate(target, &reopened)?;
        }
        Ok(())
    })();

    if let Err(commit_error) = commit_result {
        // Every step here used to be `let _ = ...`: a failed rollback was
        // indistinguishable from a successful one, so a caller who saw only
        // `commit_error` would believe the target still held its original
        // content when a copy, sync, or replace mid-rollback had actually
        // left it holding the NEW content instead. Collecting failures
        // instead of discarding them turns that into a reported condition.
        let mut rollback_failures: Vec<String> = Vec::new();
        for (target, _, backup, existed) in staged.iter().take(replaced).rev() {
            let restore_result = if *existed {
                let rollback = sibling(target, ".rollback-tmp");
                fs::copy(backup, &rollback)
                    .and_then(|_| OpenOptions::new().write(true).open(&rollback)?.sync_all())
                    .and_then(|_| replace(&rollback, target))
            } else {
                // Nothing existed at this target before the batch started,
                // so removing what the batch wrote *is* the rollback here.
                // Already gone counts as success - there is nothing left to
                // roll back.
                match fs::remove_file(target) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error),
                }
            };
            if let Err(error) = restore_result {
                rollback_failures.push(format!("{}: {error}", target.display()));
            }
        }
        cleanup_temps(&staged);
        if rollback_failures.is_empty() {
            return Err(commit_error);
        }
        return Err(Error::new(
            commit_error.kind(),
            format!(
                "{commit_error} (additionally, rollback failed and left the new content in \
                 place for: {})",
                rollback_failures.join(", ")
            ),
        ));
    }
    // Every target committed and reopened clean, so none of the backups
    // just made will ever be needed for recovery. Leaving them behind is
    // exactly what turns a portable build's rules.json/settings directory
    // into an ever-growing pile of `.bak` copies next to the exe. This is
    // best-effort and silent on failure: the commit already succeeded and
    // must be reported as success either way, and this module has no
    // logging channel of its own to surface a removal failure through (the
    // app crate's logger sits above it) - a leftover `.bak` from a failed
    // cleanup is a minor, self-correcting nuisance (the next successful
    // write's own cleanup removes it), not a correctness problem worth
    // failing an otherwise-successful write over.
    for (_, _, backup, existed) in &staged {
        if *existed {
            let _ = fs::remove_file(backup);
        }
    }
    Ok(())
}

pub fn atomic_write_with_backup(
    target: &Path,
    bytes: &[u8],
    validate: impl Fn(&Path, &[u8]) -> Result<()>,
) -> Result<()> {
    atomic_write_batch_with_backup(&[(target, bytes)], validate)
}

/// Commits an already-written and flushed replacement file. The caller picks
/// the recovery-backup path so compound formats (SQLite plus sidecars) can use
/// one coherent recovery namespace.
pub fn replace_staged_with_backup(
    staged: &Path,
    target: &Path,
    backup: &Path,
    validate: impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    validate(staged)?;
    let existed = target.is_file();
    if existed {
        fs::copy(target, backup)?;
        OpenOptions::new().write(true).open(backup)?.sync_all()?;
    }
    replace(staged, target)?;
    if let Err(error) = validate(target) {
        if existed {
            let rollback = sibling(target, ".rollback-tmp");
            fs::copy(backup, &rollback)?;
            OpenOptions::new().write(true).open(&rollback)?.sync_all()?;
            replace(&rollback, target)?;
        } else {
            let _ = fs::remove_file(target);
        }
        return Err(error);
    }
    // See the matching comment in `atomic_write_batch_with_backup`: the
    // commit already succeeded, so a stale backup left behind by a failed
    // removal here is a self-correcting nuisance, not a reason to fail an
    // otherwise-successful write, and this module has no logging channel to
    // report the removal failure through anyway.
    if existed {
        let _ = fs::remove_file(backup);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn a_late_reopen_validation_failure_restores_the_entire_batch() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.json");
        let second = dir.path().join("second.json");
        fs::write(&first, b"old-first").unwrap();
        fs::write(&second, b"old-second").unwrap();
        let calls = Cell::new(0usize);

        let result = atomic_write_batch_with_backup(
            &[(&first, b"new-first"), (&second, b"new-second")],
            |_path, _bytes| {
                let call = calls.get() + 1;
                calls.set(call);
                if call == 4 {
                    Err(Error::new(ErrorKind::InvalidData, "fault injection"))
                } else {
                    Ok(())
                }
            },
        );

        assert_eq!(result.unwrap_err().kind(), ErrorKind::InvalidData);
        assert_eq!(fs::read(&first).unwrap(), b"old-first");
        assert_eq!(fs::read(&second).unwrap(), b"old-second");
        assert!(!sibling(&first, ".replace-tmp").exists());
        assert!(!sibling(&second, ".replace-tmp").exists());
    }

    #[test]
    fn duplicate_targets_are_rejected_before_any_write() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("rules.json");
        fs::write(&target, b"old").unwrap();

        let result = atomic_write_batch_with_backup(
            &[(&target, b"one"), (&target, b"two")],
            |_path, _bytes| Ok(()),
        );

        assert_eq!(result.unwrap_err().kind(), ErrorKind::InvalidInput);
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert!(!sibling(&target, ".replace-tmp").exists());
    }

    /// The two existing tests above only ever exercise the failure paths,
    /// which is exactly where a `.bak` earns its keep. A successful write
    /// has nothing left to recover from, so the backup it made along the way
    /// must not linger - see the comment above the cleanup loop in
    /// `atomic_write_batch_with_backup` for why a portable build cares.
    #[test]
    fn a_successful_batch_write_removes_the_backup() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("rules.json");
        fs::write(&target, b"old").unwrap();

        atomic_write_with_backup(&target, b"new", |_path, _bytes| Ok(())).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert!(
            !sibling(&target, ".bak").exists(),
            "a successful commit must not leave a stale .bak behind"
        );
    }

    /// Same defect, same fix, in `replace_staged_with_backup` - its callers
    /// (SQLite plus sidecars) pick their own backup path instead of using
    /// `sibling`, so this exercises that function directly.
    #[test]
    fn a_successful_staged_replace_removes_the_backup() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("gametrimmer.db");
        let staged = dir.path().join("gametrimmer.db.staged");
        let backup = dir.path().join("gametrimmer.db.bak");
        fs::write(&target, b"old").unwrap();
        fs::write(&staged, b"new").unwrap();

        replace_staged_with_backup(&staged, &target, &backup, |_path| Ok(())).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert!(
            !backup.exists(),
            "a successful commit must not leave a stale backup behind"
        );
    }

    /// Rollback used to run entirely under `let _ = ...`: a failed copy,
    /// sync, or replace mid-rollback vanished silently and the caller would
    /// see only the original commit error, as if the target still held its
    /// original content - when it actually now holds the NEW content. This
    /// deletes the backup from inside the validation closure, right before
    /// the injected failure that triggers rollback, so the rollback's own
    /// `fs::copy` has nothing to read and fails in turn.
    #[test]
    fn a_rollback_failure_is_reported_not_swallowed() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("rules.json");
        fs::write(&target, b"old").unwrap();
        let calls = Cell::new(0usize);

        let result = atomic_write_with_backup(&target, b"new", |path, _bytes| {
            let call = calls.get() + 1;
            calls.set(call);
            if call == 2 {
                // The reopen-validate call: fail it, but first remove the
                // backup rollback is about to need.
                fs::remove_file(sibling(path, ".bak")).unwrap();
                Err(Error::new(ErrorKind::InvalidData, "fault injection"))
            } else {
                Ok(())
            }
        });

        let error = result.unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("fault injection"),
            "the original commit error must still be visible: {message}"
        );
        assert!(
            message.to_lowercase().contains("rollback"),
            "the rollback failure must be reported too: {message}"
        );
        // The exact situation the caller must be told about: rollback did
        // not restore the original, so the target holds the NEW content.
        assert_eq!(fs::read(&target).unwrap(), b"new");
    }
}
