// Author: Dustin Pilgrim
// License: MIT

use anyhow::{Context, Result};
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

pub fn lock_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("gesso.lock")
}

/// Try to acquire a non-blocking exclusive lock.
/// Keep the returned File alive for the daemon lifetime.
/// If already locked -> Ok(None) (another daemon instance is running).
pub fn try_acquire_single_instance_lock(lock_path: &Path) -> Result<Option<std::fs::File>> {
    let f = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)
        .with_context(|| format!("open lock file: {}", lock_path.display()))?;

    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(Some(f))
    } else {
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EWOULDBLOCK) => Ok(None),
            _ => Err(e).with_context(|| format!("flock: {}", lock_path.display())),
        }
    }
}
