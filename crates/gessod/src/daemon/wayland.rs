// Author: Dustin Pilgrim
// License: MIT

use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum WaylandError {
    MissingWaylandDisplay,
    MissingRuntimeDir,
}

impl fmt::Display for WaylandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WaylandError::MissingWaylandDisplay => write!(f, "WAYLAND_DISPLAY is not set"),
            WaylandError::MissingRuntimeDir => write!(
                f,
                "XDG_RUNTIME_DIR is not set (needed to locate Wayland socket)"
            ),
        }
    }
}

impl std::error::Error for WaylandError {}

pub fn wayland_socket_path() -> Result<PathBuf, WaylandError> {
    let disp = std::env::var("WAYLAND_DISPLAY").map_err(|_| WaylandError::MissingWaylandDisplay)?;

    // If compositor gives an absolute path (rare), trust it.
    let p = PathBuf::from(&disp);
    if p.is_absolute() {
        return Ok(p);
    }

    let rt = std::env::var("XDG_RUNTIME_DIR").map_err(|_| WaylandError::MissingRuntimeDir)?;
    Ok(PathBuf::from(rt).join(disp))
}

pub fn wayland_socket_alive(sock: &std::path::Path) -> bool {
    sock.exists()
}
