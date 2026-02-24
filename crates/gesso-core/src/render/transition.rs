// Author: Dustin Pilgrim
// License: MIT

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaveDir {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "PascalCase")]
pub enum Transition {
    None,

    /// Drop: hard circle expands from screen centre, revealing new image.
    /// (No blending / no feather in the renderer; this is a crisp cut.)
    Drop {
        duration_ms: u32,
        /// Kept for forward-compat, currently ignored by the hard renderer.
        softness_px: u16,
        /// Reserved.
        seed: u32,
        /// Quantize progress into N steps. 0 = smooth.
        #[serde(default)]
        steps: u16,
    },

    /// Fade: crossfade old->new.
    Fade {
        duration_ms: u32,
        /// Quantize progress into N steps. 0 = smooth.
        #[serde(default)]
        steps: u16,
    },

    /// Directional wavefront wipe.
    Wave {
        duration_ms: u32,
        dir: WaveDir,
        softness_px: u16,
        amplitude_px: u16,
        wavelength_px: u16,
        /// Quantize progress into N steps. 0 = smooth.
        #[serde(default)]
        steps: u16,
    },
}

impl Transition {
    #[inline]
    pub fn duration_ms(&self) -> u32 {
        match *self {
            Transition::None => 0,
            Transition::Drop { duration_ms, .. } => duration_ms,
            Transition::Fade { duration_ms, .. } => duration_ms,
            Transition::Wave { duration_ms, .. } => duration_ms,
        }
    }

    #[inline]
    pub fn steps(&self) -> u16 {
        match *self {
            Transition::None => 0,
            Transition::Drop { steps, .. } => steps,
            Transition::Fade { steps, .. } => steps,
            Transition::Wave { steps, .. } => steps,
        }
    }
}
