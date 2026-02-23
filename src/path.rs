// Author: Dustin Pilgrim
// License: MIT

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub struct Paths {
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub sock_path: PathBuf,
    pub log_path: PathBuf,
    pub current_path: PathBuf,
}

pub fn paths() -> Result<Paths> {
    // Canonical state base (XDG-compliant, persistent)
    let state_base = dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .context("Could not determine state directory")?;
    let state_dir = state_base.join("gesso");

    // Runtime (sockets, ephemeral IPC)
    let runtime_base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| state_dir.clone());
    let runtime_dir = runtime_base.join("gesso");
    let sock_path = runtime_dir.join("ctl.sock");

    // SINGLE canonical log location (stateful, rotatable)
    let log_path = state_dir.join("gesso.log");

    // Current applied spec (state)
    let current_path = state_dir.join("current.json");

    Ok(Paths {
        state_dir,
        runtime_dir,
        sock_path,
        log_path,
        current_path,
    })
}

// Expand "~" / "~/" and "$HOME" / "${HOME}" in paths.
/// Does not do full shell expansion, globs, or ~user.
pub fn expand_user_path<P: AsRef<Path>>(p: P) -> Result<PathBuf> {
    let s = p.as_ref().to_string_lossy();
    let s = s.trim();

    // "~" or "~/..."
    if s == "~" || s.starts_with("~/") {
        let home = std::env::var("HOME").context("HOME is not set (needed for '~' expansion)")?;
        let rest = s.strip_prefix("~").unwrap(); // "" or "/..."
        return Ok(PathBuf::from(home).join(rest.trim_start_matches('/')));
    }

    // "$HOME/..." or "${HOME}/..."
    if let Some(rest) = s.strip_prefix("$HOME") {
        let home = std::env::var("HOME").context("HOME is not set (needed for '$HOME' expansion)")?;
        return Ok(PathBuf::from(home).join(rest.trim_start_matches('/')));
    }

    if let Some(rest) = s.strip_prefix("${HOME}") {
        let home = std::env::var("HOME").context("HOME is not set (needed for '${HOME}' expansion)")?;
        return Ok(PathBuf::from(home).join(rest.trim_start_matches('/')));
    }

    Ok(PathBuf::from(s))
}
