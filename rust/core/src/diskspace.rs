//! Free disk space, for work that writes optional extra files (the PAR2 prefetch).

use std::path::Path;

/// Bytes available to this process on the filesystem holding `path`, or of its nearest existing
/// ancestor (a work dir may not exist yet); `None` when it cannot be determined.
pub fn available_bytes(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    available_at(existing)
}

#[cfg(unix)]
fn available_at(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c` is a valid NUL-terminated path and `st` points to writable memory for one statvfs.
    if unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: statvfs returned 0, so it initialised `st`.
    let st = unsafe { st.assume_init() };
    // the field widths differ between platforms (u32 on macOS, u64 on Linux)
    #[allow(clippy::unnecessary_cast)]
    Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

#[cfg(windows)]
fn available_at(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut available = 0u64;
    // SAFETY: `wide` is NUL-terminated; the out pointer is valid and the unused ones may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(available)
}

#[cfg(not(any(unix, windows)))]
fn available_at(_path: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_temp_dir_has_some_space_and_a_missing_subdir_reports_its_parent() {
        let tmp = std::env::temp_dir();
        let here = available_bytes(&tmp).expect("temp dir space is known");
        assert!(here > 0);
        let missing = tmp.join("postie-diskspace-missing").join("deeper");
        assert!(available_bytes(&missing).is_some());
    }
}
