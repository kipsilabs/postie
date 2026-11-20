//! Helpers shared by tests that must behave the same on Unix and Windows.

use std::path::Path;

/// Creates a file symlink; `false` when the platform refuses (Windows without the privilege), in which
/// case the caller skips what depends on it.
pub(crate) fn symlink_file(original: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let r = std::os::unix::fs::symlink(original, link);
    #[cfg(windows)]
    let r = std::os::windows::fs::symlink_file(original, link);
    r.is_ok()
}

/// A path string with forward slashes, to compare against `/`-literal expectations.
pub(crate) fn slash(p: impl AsRef<str>) -> String {
    p.as_ref().replace('\\', "/")
}
