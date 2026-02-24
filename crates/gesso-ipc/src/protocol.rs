// Author: Dustin Pilgrim
// License: MIT

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OutputSel {
    All,
    Named(Vec<String>),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Mode {
    Fill,
    Fit,
    Stretch,
    Center,
    Tile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WaveDir {
    Left,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Transition {
    None,
    Drop {
        duration_ms: u32,
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
        dir: WaveDir,
        #[serde(default)]
        steps: Option<u16>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SetTarget {
    ImagePath(String),
    Colour(Rgb),
    Unset,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetRequest {
    pub outputs: OutputSel,
    pub target: SetTarget,
    pub mode: Mode,
    pub bg_colour: Option<Rgb>,
    pub transition: Transition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// List all detected outputs (name, resolution, scale).
    Outputs,
    /// Full per-output info: current target, mode, transition params.
    Info,
    Set(SetRequest),
    Unset { outputs: OutputSel },
    Stop,
    Doctor,
    Restore,
}

// ---- shared types ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub scale: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CurrentTarget {
    Unset,
    Colour(Rgb),
    ImagePath(String),
}

// ---- Info response ----

/// Full state for a single output, as returned by `gesso info`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputFullInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub scale: u32,
    pub current: CurrentTarget,
    /// Scaling mode — only meaningful when current is an image.
    pub mode: Option<Mode>,
    /// Background fill colour — only meaningful when current is an image with fit/center.
    pub bg_colour: Option<Rgb>,
    /// Last transition applied (None = instant / never set).
    pub transition: Transition,
}

// ---- Doctor response ----

/// Wayland compositor health, as returned by `gesso doctor`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub socket_ok: bool,
    pub has_compositor: bool,
    pub has_shm: bool,
    pub has_layer_shell: bool,
    pub has_xdg_output_manager: bool,
    /// Raw wl_shm pixel-format codes advertised by the compositor.
    pub shm_formats: Vec<u32>,
    pub warnings: Vec<String>,
}

// ---- Response ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Outputs(Vec<OutputInfo>),
    Info(Vec<OutputFullInfo>),
    Doctor(DoctorReport),
    Error { message: String },
}
