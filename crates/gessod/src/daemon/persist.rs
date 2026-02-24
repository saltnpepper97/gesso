// Author: Dustin Pilgrim
// License: MIT

use std::fs;
use std::path::{Path, PathBuf};

use gesso_core::paths::gesso_dirs_from_env;

use crate::daemon::types::{PersistedOutput, PersistedSet, PersistedState};

pub fn save_state(last_set: &std::collections::HashMap<String, PersistedSet>) -> anyhow::Result<()> {
    let path = state_file_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut outputs: Vec<PersistedOutput> = last_set
        .iter()
        .map(|(name, set)| PersistedOutput {
            name: name.clone(),
            set: set.clone(),
        })
        .collect();
    outputs.sort_by(|a, b| a.name.cmp(&b.name));

    let bytes = serde_json::to_vec_pretty(&PersistedState { outputs })?;
    fs::write(path, bytes)?;
    Ok(())
}

pub fn load_state() -> anyhow::Result<Option<PersistedState>> {
    let path = state_file_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}

pub fn state_file_path() -> anyhow::Result<PathBuf> {
    if let Ok(base) = std::env::var("XDG_STATE_HOME") {
        return Ok(PathBuf::from(base).join("gesso").join("state.json"));
    }
    let home = std::env::var("HOME")?;
    Ok(PathBuf::from(home).join(".local/state/gesso/state.json"))
}

pub fn resolve_image_path(path: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    if p.exists() {
        return Some(p.to_path_buf());
    }
    for dir in gesso_dirs_from_env() {
        let c = dir.join(path);
        if c.exists() {
            return Some(c);
        }
    }
    None
}
