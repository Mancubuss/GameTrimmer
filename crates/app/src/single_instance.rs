//! Single-instance guard - GT-75: "a second copy of the exe next to the
//! first is unrestricted".
//!
//! GameTrimmer is portable: `worker::db_path` derives `gametrimmer.db` from
//! the running exe's own directory (`worker::exe_dir`), with nothing else
//! keying the database to a particular process. Copy the exe into the same
//! folder and launch it while the first copy is scanning, and both windows
//! open the same database with no warning. WAL keeps that from corrupting
//! anything, but a long scan in one copy blocks writes in the other,
//! findings drift apart between the two windows, and two copies can each
//! offer to delete the same file from two different snapshots of state -
//! not acceptable for a tool whose job is deleting files.
//!
//! # Why a named mutex and not a lock file
//!
//! A lock file (write a marker on start, check for it, delete it on a clean
//! exit) does not survive a crash the way this guard needs it to: the crash
//! is exactly the case where the marker never gets deleted, and the next
//! launch - the *first* launch after the crash, running alone - would find
//! a stale file and refuse to start. A named kernel mutex has the opposite
//! failure mode, which is the one this app wants: Windows closes every
//! handle a process held the instant it terminates, crash or not, so the
//! mutex this guard creates disappears with the process regardless of how
//! it went down, and it leaves nothing on disk to clean up afterwards
//! either. The guard is deliberately *not* app-wide (a single mutex name
//! shared by every launch anywhere): two portable copies of GameTrimmer in
//! two different folders are two independent installs with two different
//! databases, and both must keep working side by side - only two launches
//! from the *same* directory are the same install.

use std::io;
use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, MessageBoxW, SetForegroundWindow, MB_ICONWARNING, MB_OK, MB_TOPMOST,
};

use crate::app::APP_TITLE;
use crate::i18n::{self, Lang};
use crate::path_normalize;

/// Holds the named mutex for as long as this instance runs. The caller
/// binds the value returned by [`acquire`] to a variable that lives for the
/// rest of `main` (see `main.rs`) - dropping it early would release the
/// guard while the process is still running.
pub struct Guard(HANDLE);

// `HANDLE` is a plain kernel handle value (not a pointer into this
// process's own memory that aliasing rules could care about), and `Guard`'s
// only operation on it - `CloseHandle` in `Drop` - is safe to perform from
// any thread. Needed because `main` may end up holding this across whatever
// thread `eframe::run_native` ends up running the event loop on.
unsafe impl Send for Guard {}
unsafe impl Sync for Guard {}

impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a HANDLE this `Guard` uniquely owns - it was
        // returned by `CreateMutexW` in `acquire` and never duplicated or
        // handed to anything else - so closing it here is neither a
        // double-close nor a use-after-close affecting another owner.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Why [`acquire`] did not return a live [`Guard`].
pub enum AcquireError {
    /// Another copy of GameTrimmer already holds the mutex for this exact
    /// directory - the case this whole module exists to catch.
    AlreadyRunning,
    /// The directory could not be normalized (see `path_normalize`), or the
    /// `CreateMutexW` call itself failed for a reason other than "already
    /// exists" (e.g. the process is out of handles). Neither means another
    /// copy is running; the caller treats this the same way the rest of the
    /// app treats a non-essential diagnostic failing - log it and let the
    /// process start unprotected rather than blocking a legitimate solo
    /// launch over a Win32 hiccup (see `elevation::is_elevated`'s "query
    /// failure reads as not-elevated" for the same policy elsewhere).
    CouldNotVerify(io::Error),
}

/// Attempts to become the sole running instance for `dir` (the portable
/// install directory - see `worker::exe_dir`).
pub fn acquire(dir: &Path) -> Result<Guard, AcquireError> {
    let normalized = path_normalize::normalize_dir(dir).map_err(AcquireError::CouldNotVerify)?;
    let name = to_wide(&mutex_name(&normalized));

    // SAFETY: `name` is a null-terminated UTF-16 buffer alive for the whole
    // call. `None` security attributes gives the default, non-inheritable
    // ACL - this handle is never meant to be inherited by a child process.
    // `binitialowner: false` is deliberate: this guard never calls
    // `WaitForSingleObject` and never takes ownership of the mutex in the
    // synchronization sense - only the *existence* of the named kernel
    // object is used as the single-instance signal (via
    // `ERROR_ALREADY_EXISTS` below), so nothing here ever blocks or
    // contends with anything.
    let handle = unsafe { CreateMutexW(None, false, PCWSTR::from_raw(name.as_ptr())) }
        .map_err(|err| AcquireError::CouldNotVerify(io::Error::other(err)))?;

    // `CreateMutexW` returns a valid, non-null handle in both cases - a
    // fresh mutex it just created, or a handle to one that already existed,
    // opened by another process holding the same name - because Windows
    // does not treat "opened the existing object" as a call failure. The
    // only way to tell the two apart is `GetLastError` immediately after a
    // successful call, which is exactly the standard single-instance idiom
    // this follows.
    //
    // SAFETY: no preconditions beyond having just made a Win32 call on this
    // thread, which `CreateMutexW` above did; `GetLastError` reads the
    // calling thread's own last-error slot.
    let already_running = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;

    if already_running {
        // SAFETY: `handle` is a HANDLE this call uniquely owns and has not
        // shared with anything yet. It is a handle to the *other* process's
        // mutex, kept alive by whatever handle that other process holds,
        // not by this one - closing it here only gives up this process's
        // own reference, it does not touch the other instance's guard.
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Err(AcquireError::AlreadyRunning);
    }

    Ok(Guard(handle))
}

/// Derives a Win32 mutex name from an already-normalized directory path.
///
/// Kernel object names cannot contain a backslash, and are capped at
/// `MAX_PATH` (260) characters - a `\\?\`-prefixed verbatim path (long
/// paths, some UNC targets) can exceed that on its own before even reaching
/// the object-name limit. Hashing the path sidesteps both problems at once:
/// the digest is a fixed-length, backslash-free string regardless of how
/// long or how prefixed the source path is. Two different directories
/// landing on the same 64-bit digest is not a realistic risk for the number
/// of directories one desktop could ever hold - this only needs to
/// distinguish however many folders one user has actually put a copy of
/// GameTrimmer in, not defend against an adversary choosing paths to
/// collide.
///
/// `Local\` is written explicitly rather than left to the default
/// namespace: it documents the intent (session-local, never promoted to
/// `Global\` and shared across Remote Desktop sessions or services) instead
/// of depending on a reader already knowing what the unprefixed default is.
fn mutex_name(normalized_dir: &Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    normalized_dir.to_string_lossy().hash(&mut hasher);
    format!("Local\\GameTrimmer-SingleInstance-{:016x}", hasher.finish())
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Tells the user a copy is already running: brings that copy's window to
/// the front if it can be found, then explains what happened with a native
/// message box.
///
/// A plain Win32 dialog rather than an `egui` one, because there is no
/// `egui::Context` yet - this runs before `eframe::run_native` is even
/// called, from `main`, which is the only point that knows the launch is
/// about to be redundant before any window exists.
pub fn notify_already_running(lang: Lang) {
    let raised = raise_existing_window();
    let body = i18n::already_running_body(lang, raised);

    let title = to_wide(i18n::strings(lang).already_running_title);
    let body = to_wide(&body);
    // SAFETY: both buffers are null-terminated UTF-16 and alive for the
    // call. `None` owner window is correct - there is no window of ours to
    // own this dialog, which is the whole reason it exists. `MB_TOPMOST`
    // matters here specifically: the point of this dialog is that the
    // launch was *not* silent, and a message box that opens behind the
    // already-running window would read exactly like the silence GT-75 is
    // about.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR::from_raw(body.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            MB_OK | MB_ICONWARNING | MB_TOPMOST,
        );
    }
}

/// Finds the running instance's main window by title and brings it to the
/// front. Returns whether that succeeded, so [`notify_already_running`] can
/// word its message accurately instead of promising a raise that may not
/// have happened.
///
/// Matched by title, not window class: `eframe`/`winit` register their own
/// class name internally and do not expose it to application code, so title
/// is the only handle available - and `APP_TITLE` ("GameTrimmer") is
/// distinctive enough that a false match on some unrelated window is not a
/// realistic concern on a normal desktop.
fn raise_existing_window() -> bool {
    let title = to_wide(APP_TITLE);
    // SAFETY: `title` is a null-terminated UTF-16 buffer alive for the call.
    // A `None`/null class name matches on title alone, which is what's
    // available here (see above).
    let Ok(hwnd) = (unsafe { FindWindowW(None, PCWSTR::from_raw(title.as_ptr())) }) else {
        return false;
    };
    // SAFETY: `hwnd` was just returned by `FindWindowW` as a live window
    // handle. `SetForegroundWindow` has no unsafe precondition beyond a
    // syntactically valid `HWND` - passing a stale one (the window closing
    // in the gap between the two calls) just fails harmlessly and returns
    // `FALSE`.
    unsafe { SetForegroundWindow(hwnd) }.as_bool()
}
