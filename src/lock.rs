//! Locks on files, by `flock`: one holder at a time, let go as the file
//! is dropped, and by the system when the holder's process ends, however
//! it ends.

use std::fs::File;
use std::io::{Error, ErrorKind, Result};
use std::os::fd::AsRawFd;
use std::path::Path;

/// The file at `path`, made when missing, locked `how` (`flock`'s
/// operation).
fn flock(path: &Path, how: libc::c_int) -> Result<File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    // SAFETY: flock on a file this function opened.
    if unsafe { libc::flock(file.as_raw_fd(), how) } != 0 {
        return Err(Error::last_os_error());
    }
    Ok(file)
}

/// The lock on `path`, waiting while another holds it.
#[cfg(feature = "benchmark")]
pub(crate) fn lock(path: &Path) -> Result<File> {
    flock(path, libc::LOCK_EX)
}

/// The lock on `path` when no one holds it; `None` while another does.
pub(crate) fn try_lock(path: &Path) -> Result<Option<File>> {
    match flock(path, libc::LOCK_EX | libc::LOCK_NB) {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.kind() == ErrorKind::WouldBlock => Ok(None),
        Err(e) => Err(e),
    }
}
