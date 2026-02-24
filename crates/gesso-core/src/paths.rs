// Author: Dustin Pilgrim
// License: MIT

use std::path::PathBuf;

pub fn gesso_dirs_from_env() -> Vec<PathBuf> {
    let Some(val) = std::env::var_os("GESSO_DIRS") else {
        return Vec::new();
    };

    val.to_string_lossy()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}
