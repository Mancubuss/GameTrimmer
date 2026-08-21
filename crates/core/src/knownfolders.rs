//! Windows shell known folders.
//!
//! `%USERPROFILE%\Documents` is a guess, not an answer: Documents and Saved
//! Games are known folders the user can point anywhere, and on a machine
//! where Documents lives on another drive every path built from the profile
//! misses. The shell knows where they actually are, so ask it.

use std::path::PathBuf;

/// The user's Documents folder (`FOLDERID_Documents`), wherever it has been
/// redirected to.
pub fn documents_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        known_folder(&windows::Win32::UI::Shell::FOLDERID_Documents)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// The user's Saved Games folder (`FOLDERID_SavedGames`), wherever it has
/// been redirected to.
pub fn saved_games_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        known_folder(&windows::Win32::UI::Shell::FOLDERID_SavedGames)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
fn known_folder(id: &windows::core::GUID) -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};

    // SAFETY: `id` is a valid FOLDERID GUID, the token argument is the
    // documented "current user" null handle, and the returned buffer is a
    // NUL-terminated wide string owned by the shell - read once, then handed
    // straight back to `CoTaskMemFree`, which is how it must be released.
    unsafe {
        let raw = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        if raw.is_null() {
            return None;
        }
        let path = raw.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(raw.as_ptr() as *const _));
        path.filter(|path| !path.as_os_str().is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows always has these two, redirected or not. The point of the test
    /// is that the lookup returns a real directory rather than an empty
    /// string or the literal profile guess this module exists to replace.
    #[test]
    #[cfg(windows)]
    fn documents_and_saved_games_resolve_to_real_directories() {
        let documents = documents_dir().expect("Windows always has a Documents known folder");
        assert!(
            documents.is_dir(),
            "Documents resolved to {documents:?}, which is not a directory"
        );

        let saved_games = saved_games_dir().expect("Windows always has a Saved Games known folder");
        assert!(
            saved_games.is_absolute(),
            "Saved Games resolved to a relative path: {saved_games:?}"
        );
    }
}
