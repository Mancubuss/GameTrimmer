//! Manual probe: what does [`RecycleBin`] actually do with an item bigger
//! than the target volume's Recycle Bin quota?
//!
//! Windows permanently deletes (does not recycle) an item that cannot fit into
//! the volume's bin. `trash::delete` runs `IFileOperation` with
//! `FOF_NO_UI | FOF_ALLOWUNDO | FOF_WANTNUKEWARNING` - so the question this
//! probe answers is narrow: does the call return an error (item survives) or
//! `Ok(())` for an item that is actually gone and NOT in the bin? The answer
//! decides whether the app may honestly tell the user "moved to the Recycle
//! Bin, recoverable".
//!
//! This probe does NOT establish whether Windows shows the user a
//! notification/warning for such a delete - real-world use suggests it often
//! does. The app's honesty therefore rests on the return-value + bin-membership
//! facts this probe checks, never on an assumption that the delete is silent.
//!
//! `#[ignore]`d because it needs a real volume, a couple of GB of free space
//! and several seconds. Run it explicitly:
//!
//! ```text
//! $env:GT_RECYCLE_PROBE_DIR = "J:\gt-recycle-probe"   # on the target volume
//! $env:GT_RECYCLE_PROBE_MB  = "2500"                  # > that volume's quota
//! cargo test -p gametrimmer-core --test recycle_bin_quota -- --ignored --nocapture
//! ```
//!
//! The volume's quota is `MaxCapacity` (in MiB) under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{volume-guid}`.
//! Pick a volume whose bin is empty: an item that *does* fit is recycled by
//! evicting older items, which would destroy real user data.

use std::path::PathBuf;

use gametrimmer_core::ops::{
    execute_delete_plans_observed, prepare_delete_plans, DeleteAttendance, FsOutcome,
};
use gametrimmer_core::settings::DeleteMethod;

/// Writes `size_mb` MiB of zeroes to `path` in 1 MiB chunks.
fn write_filler(path: &std::path::Path, size_mb: u64) {
    use std::io::Write;

    let chunk = vec![0u8; 1024 * 1024];
    let file = std::fs::File::create(path).expect("create filler file");
    let mut writer = std::io::BufWriter::new(file);
    for _ in 0..size_mb {
        writer.write_all(&chunk).expect("write filler chunk");
    }
    writer.flush().expect("flush filler file");
}

/// True if the Recycle Bin currently holds an entry whose original path is
/// `path` - i.e. the item really is recoverable, not nuked.
fn is_in_recycle_bin(path: &std::path::Path) -> bool {
    let items = match trash::os_limited::list() {
        Ok(items) => items,
        // Listing failing is not the same as "not in the bin"; treat it as
        // unknown-but-not-proof and let the caller's assertion speak.
        Err(err) => {
            println!("WARN: could not list the Recycle Bin: {err}");
            return false;
        }
    };
    items.iter().any(|item| {
        item.original_path()
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .eq_ignore_ascii_case(path.to_string_lossy().trim_start_matches(r"\\?\"))
    })
}

fn recycle_through_authoritative_pipeline(
    root: &std::path::Path,
    rel_path: &str,
) -> gametrimmer_core::error::Result<FsOutcome> {
    let mut conn = gametrimmer_core::db::open_in_memory()?;
    let scan_id = gametrimmer_core::db::begin_scan(&conn, "complete")?;
    conn.execute(
        "INSERT INTO game_libraries (vendor, path) VALUES ('test', ?1)",
        [root.to_string_lossy()],
    )?;
    let library_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO games (scan_id, library_id, name, install_dir, app_id)
         VALUES (?1, ?2, 'Recycle quota probe', ?3, 'probe')",
        rusqlite::params![scan_id, library_id, root.to_string_lossy()],
    )?;
    let game_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO files (scan_id, game_id, rel_path, size) VALUES (?1, ?2, ?3, 1)",
        rusqlite::params![scan_id, game_id, rel_path],
    )?;
    let file_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO findings (file_id, category, confidence) VALUES (?1, 'docs_file', 90)",
        [file_id],
    )?;
    let snapshot = gametrimmer_core::safety::capture_safety_snapshot(root, rel_path)
        .map_err(|error| gametrimmer_core::error::CoreError::Other(error.to_string()))?;
    conn.execute(
        "INSERT INTO file_safety
         (file_id, scan_id, trusted_root, rel_path, root_identity,
          target_identity, target_kind, tree_fingerprint)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            file_id,
            scan_id,
            snapshot.trusted_root.to_string_lossy(),
            snapshot.rel_path.to_string_lossy(),
            snapshot.root_identity.encode(),
            snapshot.target_identity.encode(),
            snapshot.target_identity.kind.as_str(),
            snapshot.tree_fingerprint,
        ],
    )?;
    gametrimmer_core::db::record_scan_library_evidence(&conn, scan_id, root, "test", "complete")?;
    gametrimmer_core::db::activate_scan(&mut conn, scan_id)?;
    let plans = prepare_delete_plans(
        &conn,
        &[file_id],
        DeleteMethod::RecycleBin,
        DeleteAttendance::Interactive,
    )?;
    let outcomes = execute_delete_plans_observed(
        &mut conn,
        DeleteMethod::RecycleBin,
        &plans,
        |_, _, _| {},
        |_, _| {},
    )?;
    Ok(outcomes[0].status)
}

#[test]
#[ignore = "needs a real volume with a small Recycle Bin quota; see module docs"]
fn item_larger_than_the_bin_quota_is_never_reported_as_a_successful_recycle() {
    let Ok(dir) = std::env::var("GT_RECYCLE_PROBE_DIR") else {
        panic!("set GT_RECYCLE_PROBE_DIR to a directory on the target volume");
    };
    let size_mb: u64 = std::env::var("GT_RECYCLE_PROBE_MB")
        .unwrap_or_else(|_| "2500".to_string())
        .parse()
        .expect("GT_RECYCLE_PROBE_MB must be a number of MiB");

    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("create probe dir");
    let target = dir.join("oversized.bin");
    println!("writing {size_mb} MiB to {}", target.display());
    write_filler(&target, size_mb);

    let result = recycle_through_authoritative_pipeline(&dir, "oversized.bin");
    let still_on_disk = target.exists();
    let in_bin = is_in_recycle_bin(&target);

    println!("remove() -> {result:?}");
    println!("still on disk: {still_on_disk}");
    println!("found in Recycle Bin: {in_bin}");

    // Clean up whatever survived before asserting, so a failing run doesn't
    // leave gigabytes behind.
    let _ = std::fs::remove_file(&target);
    let _ = std::fs::remove_dir(&dir);

    // The safety-critical invariant: reporting success must mean the item is
    // recoverable. An error (item left on disk) is an acceptable outcome too -
    // the app surfaces it as a per-file failure. Success reported for a file
    // that is actually gone (permanently deleted, not in the bin) is not.
    if matches!(result, Ok(FsOutcome::Removed)) {
        assert!(
            in_bin,
            "`trash::delete` reported success for an item over the bin quota, \
             but it is not in the Recycle Bin - the app's \"moved to the \
             Recycle Bin\" wording would be a lie"
        );
    } else {
        assert!(
            still_on_disk,
            "`trash::delete` reported an error but the item is gone from disk - \
             the app would keep a finding for a file that no longer exists"
        );
    }
}
