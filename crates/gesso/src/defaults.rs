// Author: Dustin Pilgrim
// License: MIT

use gesso_ipc::protocol as ipc;
use crate::cli::{TransitionArg, WaveFromArg};

// ---- sane defaults ----
const IMG_DROP_MS: u32 = 2300;
const IMG_FADE_MS: u32 = 950;
const IMG_WAVE_MS: u32 = 1750;
const COL_DROP_MS: u32 = 1700;
const COL_FADE_MS: u32 = 1100;
const COL_WAVE_MS: u32 = 1700;

fn mode_nudge_ms(mode: ipc::Mode) -> i32 {
    match mode {
        ipc::Mode::Fill    =>    0,
        ipc::Mode::Fit     =>  -60,
        ipc::Mode::Center  =>  -60,
        ipc::Mode::Stretch =>  -40,
        ipc::Mode::Tile    => -120,
    }
}

fn clamp_ms(base: u32, nudge: i32) -> u32 {
    let v = base as i32 + nudge;
    v.max(160) as u32
}

fn norm_steps(s: Option<u16>) -> Option<u16> {
    match s {
        None | Some(0) => None,
        Some(v) => Some(v),
    }
}

fn wave_dir(from: WaveFromArg) -> ipc::WaveDir {
    match from {
        WaveFromArg::Left  => ipc::WaveDir::Left,
        WaveFromArg::Right => ipc::WaveDir::Right,
    }
}

pub fn build_transition_image(
    t:        TransitionArg,
    duration: Option<u32>,
    from:     WaveFromArg,
    steps:    Option<u16>,
    mode:     ipc::Mode,
) -> ipc::Transition {
    let nudge = mode_nudge_ms(mode);
    let steps = norm_steps(steps);
    match t {
        TransitionArg::None => ipc::Transition::None,
        TransitionArg::Drop => ipc::Transition::Drop {
            duration_ms: duration.unwrap_or(clamp_ms(IMG_DROP_MS, nudge)),
            steps,
        },
        TransitionArg::Fade => ipc::Transition::Fade {
            duration_ms: duration.unwrap_or(clamp_ms(IMG_FADE_MS, nudge)),
            steps,
        },
        TransitionArg::Wave => ipc::Transition::Wave {
            duration_ms: duration.unwrap_or(clamp_ms(IMG_WAVE_MS, nudge)),
            dir: wave_dir(from),
            steps,
        },
    }
}

pub fn build_transition_colour(
    t:        TransitionArg,
    duration: Option<u32>,
    from:     WaveFromArg,
    steps:    Option<u16>,
) -> ipc::Transition {
    let steps = norm_steps(steps);
    match t {
        TransitionArg::None => ipc::Transition::None,
        TransitionArg::Drop => ipc::Transition::Drop {
            duration_ms: duration.unwrap_or(COL_DROP_MS),
            steps,
        },
        TransitionArg::Fade => ipc::Transition::Fade {
            duration_ms: duration.unwrap_or(COL_FADE_MS),
            steps,
        },
        TransitionArg::Wave => ipc::Transition::Wave {
            duration_ms: duration.unwrap_or(COL_WAVE_MS),
            dir: wave_dir(from),
            steps,
        },
    }
}
