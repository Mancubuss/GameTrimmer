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
    .map_err(|_| Error::last_os_error())
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
        for (target, _, backup, existed) in staged.iter().take(replaced).rev() {
            if *existed {
                let rollback = sibling(target, ".rollback-tmp");
                if fs::copy(backup, &rollback).is_ok() {
                    let _ = OpenOptions::new()
                        .write(true)
                        .open(&rollback)
                        .and_then(|file| file.sync_all());
                    let _ = replace(&rollback, target);
                }
            } else {
                let _ = fs::remove_file(target);
            }
        }
        cleanup_temps(&staged);
        return Err(commit_error);
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
}
