// Author: Dustin Pilgrim
// License: MIT

use anyhow::{bail, Result};
use eventline as el;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    Fill,
    Fit,
    Stretch,
    Center,
    Tile,
}

impl From<crate::cli::ModeArg> for Mode {
    fn from(m: crate::cli::ModeArg) -> Self {
        match m {
            crate::cli::ModeArg::Fill => Mode::Fill,
            crate::cli::ModeArg::Fit => Mode::Fit,
            crate::cli::ModeArg::Stretch => Mode::Stretch,
            crate::cli::ModeArg::Center => Mode::Center,
            crate::cli::ModeArg::Tile => Mode::Tile,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Transition {
    None,
    Fade,
    Wipe,
}

impl From<crate::cli::TransitionArg> for Transition {
    fn from(t: crate::cli::TransitionArg) -> Self {
        match t {
            crate::cli::TransitionArg::None => Transition::None,
            crate::cli::TransitionArg::Fade => Transition::Fade,
            crate::cli::TransitionArg::Wipe => Transition::Wipe,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WipeFrom {
    Left,
    Right,
}

impl From<crate::cli::WipeFromArg> for WipeFrom {
    fn from(w: crate::cli::WipeFromArg) -> Self {
        match w {
            crate::cli::WipeFromArg::Left => WipeFrom::Left,
            crate::cli::WipeFromArg::Right => WipeFrom::Right,
        }
    }
}

impl Default for WipeFrom {
    fn default() -> Self {
        WipeFrom::Left
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TransitionSpec {
    pub kind: Transition,
    pub duration: u32,

    #[serde(default)]
    pub wipe_from: WipeFrom,
}

impl Default for TransitionSpec {
    fn default() -> Self {
        TransitionSpec {
            kind: Transition::None,
            duration: 200,
            wipe_from: WipeFrom::Left,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    /// Parse color from hex string (with or without # prefix)
    /// Examples: "#FF5733", "FF5733"
    pub fn parse(s: &str) -> Result<Rgb> {
        el::scope!(
            "gesso.spec.rgb.parse",
            success = "parsed",
            failure = "failed",
            aborted = "aborted",
            {
                let s = s.trim();
                let hex = s.strip_prefix('#').unwrap_or(s);

                if hex.len() != 6 {
                    bail!("Invalid colour '{s}': expected #RRGGBB");
                }

                let r = u8::from_str_radix(&hex[0..2], 16)?;
                let g = u8::from_str_radix(&hex[2..4], 16)?;
                let b = u8::from_str_radix(&hex[4..6], 16)?;

                el::debug!(
                    "parsed input={input} hex={hex} rgb={r},{g},{b}",
                    input = s,
                    hex = hex,
                    r = r,
                    g = g,
                    b = b
                );

                Ok::<Rgb, anyhow::Error>(Rgb { r, g, b })
            }
        )
    }

    #[inline]
    pub fn xrgb8888(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }
    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Spec {
    Image {
        path: PathBuf,
        mode: Mode,
        // background fill for letterbox/center
        colour: Rgb,
        output: Option<String>,
        transition: TransitionSpec,
    },
    Colour {
        colour: Rgb,
        output: Option<String>,
        transition: TransitionSpec,
    },
}
