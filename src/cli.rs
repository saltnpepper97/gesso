// Author: Dustin Pilgrim
// License: MIT

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(name = "gesso", about = "Deterministic Wayland wallpaper daemon and CLI")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Set an image wallpaper (path or name resolved via GESSO_DIRS)
    Set {
        target: String,

        #[arg(long, short = 'm', value_enum, default_value_t = ModeArg::Fill)]
        mode: ModeArg,

        /// Background fill color for fit/center letterboxing (e.g. #101010)
        #[arg(long, short = 'c')]
        colour: Option<String>,

        #[arg(long, short = 't', value_enum, default_value_t = TransitionArg::None)]
        transition: TransitionArg,

        /// Transition duration in ms
        #[arg(long, short = 'd', default_value_t = 550)]
        duration: u32,

        /// Optional output name (later; accepted now for forward-compat)
        #[arg(long, short = 'o')]
        output: Option<String>,
    },

    /// Solid colour background
    Colour {
        colour: String,

        #[arg(long, short = 't', value_enum, default_value_t = TransitionArg::None)]
        transition: TransitionArg,

        /// Transition duration in ms
        #[arg(long, short = 'd', default_value_t = 200)]
        duration: u32,

        #[arg(long, short = 'o')]
        output: Option<String>,
    },

    /// Unset wallpaper on one output (by name) or all outputs (default).
    /// If `--output` is omitted, unsets all.
    Unset {
        /// Output name to unset (if omitted, unsets all)
        #[arg(long, short = 'o')]
        output: Option<String>,
    },

    Stop,
    Status,
    Doctor,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum ModeArg {
    Fill,
    Fit,
    Stretch,
    Center,
    Tile,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum GradientDirArg {
    Vertical,
    Horizontal,
    Diag1,
    Diag2,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum TransitionArg {
    None,
    Fade,
    Wipe,
}
