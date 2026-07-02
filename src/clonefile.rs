//! Thin wrapper around the macOS `clonefile(2)` syscall.
//!
//! Cloning a directory clones the entire hierarchy in-kernel — one syscall,
//! O(metadata) time, zero data blocks copied until either side diverges.

use std::ffi::CString;
use std::io;
use std::os::raw::{c_char, c_int, c_uint};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

unsafe extern "C" {
    fn clonefile(src: *const c_char, dst: *const c_char, flags: c_uint) -> c_int;
}

/// Clone `src` to `dst` via copy-on-write. `dst` must not exist and must be
/// on the same APFS volume as `src`.
pub fn clone(src: &Path, dst: &Path) -> io::Result<()> {
    let c_src = CString::new(src.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let c_dst = CString::new(dst.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    // flags = 0: follow the path itself, clone ownership where possible.
    if unsafe { clonefile(c_src.as_ptr(), c_dst.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Human hint for the errors users will actually hit.
pub fn explain(err: &io::Error) -> Option<&'static str> {
    match err.raw_os_error() {
        // EXDEV
        Some(18) => Some(
            "source and destination are on different volumes — clonefile requires the same APFS volume (is the repo on an external drive?)",
        ),
        // ENOTSUP
        Some(45) => {
            Some("filesystem does not support cloning — the repo must live on an APFS volume")
        }
        _ => None,
    }
}
