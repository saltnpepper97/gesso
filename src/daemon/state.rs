// Author: Dustin Pilgrim
// License: MIT

use anyhow::Result;
use std::fs;
use std::path::Path;

use crate::spec::Spec;

pub fn save_current(path: &Path, spec: &Spec) -> Result<()> {
    let s = serde_json::to_string_pretty(spec)?;
    fs::write(path, s)?;
    Ok(())
}

pub fn load_current(path: &Path) -> Option<Spec> {
    let s = fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

pub fn clear_current(path: &Path) {
    let _ = fs::remove_file(path);
}
