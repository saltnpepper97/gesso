// Author: Dustin Pilgrim
// License: MIT

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum LockError {
    NoParent(PathBuf),
    AlreadyRunning(PathBuf),
    Io(std::io::Error),
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockError::NoParent(p) => write!(f, "socket path has no parent dir: {}", p.display()),
            LockError::AlreadyRunning(p) => {
                write!(f, "gessod already running (lock held at {})", p.display())
            }
            LockError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for LockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LockError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for LockError {
    fn from(e: std::io::Error) -> Self {
        LockError::Io(e)
    }
}

pub struct InstanceLock {
    path: PathBuf,
    file: std::fs::File,
}

impl InstanceLock {
    pub fn acquire_for_socket(sock_path: &Path) -> Result<Self, LockError> {
        let dir = sock_path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| LockError::NoParent(sock_path.to_path_buf()))?;

        let lock_path = dir.join("gesso.lock");

        // If stale lock exists (process dead), clean it up and retry once.
        if lock_path.exists() && is_lock_stale(&lock_path) {
            let _ = fs::remove_file(&lock_path);
        }

        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(LockError::AlreadyRunning(lock_path));
            }
            Err(e) => return Err(LockError::Io(e)),
        };

        // Write PID for debugging / stale detection.
        let pid = std::process::id();
        let _ = writeln!(file, "pid={pid}");

        Ok(Self { path: lock_path, file })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // Best-effort cleanup.
        let _ = self.file.flush();
        let _ = fs::remove_file(&self.path);
    }
}

fn is_lock_stale(lock_path: &Path) -> bool {
    // Read pid=1234, check /proc/1234 exists.
    let mut s = String::new();
    if std::fs::File::open(lock_path).and_then(|mut f| f.read_to_string(&mut s)).is_err() {
        return false;
    }

    let pid = s
        .lines()
        .find_map(|l| l.strip_prefix("pid="))
        .and_then(|v| v.trim().parse::<u32>().ok());

    let Some(pid) = pid else { return false; };

    !Path::new("/proc").join(pid.to_string()).exists()
}
