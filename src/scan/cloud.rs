//! Platform quirks that a plain `stat` cannot see.

use std::path::Path;

/// Directories a cloud provider backs with a FileProvider extension: iCloud
/// Drive, Dropbox, OneDrive, Tresorit, Google Drive. They live on the boot
/// volume and report the boot volume's `st_dev`, so the usual mount check does
/// not notice them — but `readdir` inside one can block on the network for
/// minutes. Apple marks every domain root with this xattr.
#[cfg(target_os = "macos")]
pub fn is_cloud_root(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    const NAME: &[u8] = b"com.apple.file-provider-domain-id\0";
    const XATTR_NOFOLLOW: libc::c_int = 0x0001;

    let Ok(cpath) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // Size query only: no buffer, no copy. Returns the value length, or -1 with
    // ENOATTR when the attribute is absent.
    let rc = unsafe {
        libc::getxattr(
            cpath.as_ptr(),
            NAME.as_ptr() as *const libc::c_char,
            std::ptr::null_mut(),
            0,
            0,
            XATTR_NOFOLLOW,
        )
    };
    rc >= 0
}

/// Nothing to do elsewhere. On Linux the cloud clients that can stall a walk
/// — rclone, gcsfuse, an NFS or SMB share — are real mounts with their own
/// device numbers, so the ordinary filesystem-boundary check already stops at
/// them. Dropbox and friends sync into plain local directories, which are
/// exactly as cheap to walk as anything else.
#[cfg(not(target_os = "macos"))]
pub fn is_cloud_root(_path: &Path) -> bool {
    false
}
