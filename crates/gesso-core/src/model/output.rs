// Author: Dustin Pilgrim
// License: MIT

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputDesc {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub scale: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OutputSel {
    All,
    Named(Vec<String>),
}

impl OutputSel {
    pub fn matches(&self, name: &str) -> bool {
        match self {
            OutputSel::All => true,
            OutputSel::Named(v) => v.iter().any(|n| n == name),
        }
    }
}
