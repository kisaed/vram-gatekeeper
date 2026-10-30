//! Free disk space (spec §15.3).
//!
//! Responsibility: report how many bytes are available to the current user on
//! the volume holding a folder, so a download can refuse early instead of
//! filling the disk.
//! Not responsible for: deciding how much is enough.

use std::path::Path;

use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::core::PCWSTR;

use super::wide;

/// Bytes free to the calling user on the volume containing `dir`, or `None`
/// if it cannot be determined (the caller then does not block).
pub fn free_bytes(dir: &Path) -> Option<u64> {
    // The API wants an existing directory; walk up to one that exists.
    let mut probe = dir;
    while !probe.exists() {
        probe = probe.parent()?;
    }
    let wide = wide(&probe.as_os_str().to_string_lossy());
    let mut available = 0u64;
    // SAFETY: `wide` is NUL-terminated and outlives the call; the first out
    // pointer is valid and the other two are optional (null).
    // https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-getdiskfreespaceexw
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&raw mut available), None, None) }.ok()?;
    Some(available)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_space_for_an_existing_and_a_future_folder() {
        let tmp = std::env::temp_dir();
        let existing = free_bytes(&tmp).expect("free space of the temp drive");
        assert!(existing > 0);
        let future = tmp.join("does").join("not").join("exist-yet");
        assert_eq!(free_bytes(&future), Some(existing), "walks up to an existing parent on the same volume");
    }
}
