// Author: Dustin Pilgrim
// License: MIT

use gesso_core::{Transition as CoreTransition, WaveDir as CoreWaveDir};
use gesso_ipc::protocol as ipc;

use crate::daemon::types::PersistedTransition;

pub const DEFAULT_WAVE_SOFTNESS_PX: u16 = 32;
pub const DEFAULT_WAVE_AMPLITUDE_PX: u16 = 18;
pub const DEFAULT_WAVE_WAVELENGTH_PX: u16 = 180;

pub const DEFAULT_DROP_SOFTNESS_PX: u16 = 0;
pub const DEFAULT_DROP_SEED: u32 = 0;

pub const DEFAULT_FADE_MS: u32 = 350;

fn steps_u16_from_opt(s: Option<u16>) -> u16 {
    s.unwrap_or(0)
}

pub fn persisted_transition_from_ipc(t: ipc::Transition) -> PersistedTransition {
    match t {
        ipc::Transition::None => PersistedTransition::None,

        ipc::Transition::Drop { duration_ms, steps } => PersistedTransition::Drop {
            duration_ms,
            softness_px: Some(DEFAULT_DROP_SOFTNESS_PX),
            seed: Some(DEFAULT_DROP_SEED),
            steps,
        },

        ipc::Transition::Fade { duration_ms, steps } => PersistedTransition::Fade { duration_ms, steps },

        ipc::Transition::Wave { duration_ms, dir, steps } => PersistedTransition::Wave {
            duration_ms,
            wave_from: dir,
            softness_px: Some(DEFAULT_WAVE_SOFTNESS_PX),
            amplitude_px: Some(DEFAULT_WAVE_AMPLITUDE_PX),
            wavelength_px: Some(DEFAULT_WAVE_WAVELENGTH_PX),
            steps,
        },
    }
}

pub fn ipc_transition_from_persisted(t: &PersistedTransition) -> ipc::Transition {
    match t {
        PersistedTransition::None => ipc::Transition::None,

        PersistedTransition::Drop { duration_ms, steps, .. } => {
            ipc::Transition::Drop { duration_ms: *duration_ms, steps: *steps }
        }

        PersistedTransition::Fade { duration_ms, steps } => {
            ipc::Transition::Fade { duration_ms: *duration_ms, steps: *steps }
        }

        PersistedTransition::Wave { duration_ms, wave_from, steps, .. } => ipc::Transition::Wave {
            duration_ms: *duration_ms,
            dir: wave_from.clone(),
            steps: *steps,
        },
    }
}

pub fn to_core_transition_persisted(t: &PersistedTransition) -> CoreTransition {
    match t {
        PersistedTransition::None => CoreTransition::None,

        PersistedTransition::Drop { duration_ms, softness_px, seed, steps } => CoreTransition::Drop {
            duration_ms: *duration_ms,
            softness_px: softness_px.unwrap_or(DEFAULT_DROP_SOFTNESS_PX),
            seed: seed.unwrap_or(DEFAULT_DROP_SEED),
            steps: steps_u16_from_opt(*steps),
        },

        PersistedTransition::Fade { duration_ms, steps } => CoreTransition::Fade {
            duration_ms: *duration_ms,
            steps: steps_u16_from_opt(*steps),
        },

        PersistedTransition::Wave {
            duration_ms,
            wave_from,
            softness_px,
            amplitude_px,
            wavelength_px,
            steps,
        } => CoreTransition::Wave {
            duration_ms: *duration_ms,
            dir: match wave_from {
                ipc::WaveDir::Left => CoreWaveDir::Left,
                ipc::WaveDir::Right => CoreWaveDir::Right,
            },
            softness_px: softness_px.unwrap_or(DEFAULT_WAVE_SOFTNESS_PX),
            amplitude_px: amplitude_px.unwrap_or(DEFAULT_WAVE_AMPLITUDE_PX),
            wavelength_px: wavelength_px.unwrap_or(DEFAULT_WAVE_WAVELENGTH_PX),
            steps: steps_u16_from_opt(*steps),
        },
    }
}

pub fn to_core_transition(t: ipc::Transition) -> CoreTransition {
    match t {
        ipc::Transition::None => CoreTransition::None,

        ipc::Transition::Drop { duration_ms, steps } => CoreTransition::Drop {
            duration_ms,
            softness_px: DEFAULT_DROP_SOFTNESS_PX,
            seed: DEFAULT_DROP_SEED,
            steps: steps_u16_from_opt(steps),
        },

        ipc::Transition::Fade { duration_ms, steps } => CoreTransition::Fade {
            duration_ms: if duration_ms == 0 { DEFAULT_FADE_MS } else { duration_ms },
            steps: steps_u16_from_opt(steps),
        },

        ipc::Transition::Wave { duration_ms, dir, steps } => CoreTransition::Wave {
            duration_ms,
            dir: match dir {
                ipc::WaveDir::Left => CoreWaveDir::Left,
                ipc::WaveDir::Right => CoreWaveDir::Right,
            },
            softness_px: DEFAULT_WAVE_SOFTNESS_PX,
            amplitude_px: DEFAULT_WAVE_AMPLITUDE_PX,
            wavelength_px: DEFAULT_WAVE_WAVELENGTH_PX,
            steps: steps_u16_from_opt(steps),
        },
    }
}
