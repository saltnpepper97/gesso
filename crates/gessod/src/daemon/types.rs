// Author: Dustin Pilgrim
// License: MIT

//
// Persisted State
//

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedState {
    pub outputs: Vec<PersistedOutput>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedOutput {
    pub name: String,
    pub set: PersistedSet,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedSet {
    pub target: PersistedTarget,
    pub mode: Option<gesso_ipc::protocol::Mode>,
    pub bg_colour: Option<gesso_ipc::protocol::Rgb>,
    pub transition: PersistedTransition,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "PascalCase")]
pub enum PersistedTarget {
    Unset,
    Colour { r: u8, g: u8, b: u8 },
    ImagePath { path: String },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "PascalCase")]
pub enum PersistedTransition {
    None,

    Drop {
        duration_ms: u32,
        softness_px: Option<u16>,
        seed: Option<u32>,
        #[serde(default)]
        steps: Option<u16>,
    },

    Fade {
        duration_ms: u32,
        #[serde(default)]
        steps: Option<u16>,
    },

    Wave {
        duration_ms: u32,
        wave_from: gesso_ipc::protocol::WaveDir,
        softness_px: Option<u16>,
        amplitude_px: Option<u16>,
        wavelength_px: Option<u16>,
        #[serde(default)]
        steps: Option<u16>,
    },
}
