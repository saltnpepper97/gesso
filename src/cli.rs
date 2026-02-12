// Author: Dustin Pilgrim
// License: MIT

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "gesso",
    about = "Deterministic Wayland wallpaper daemon and CLI",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Set an image wallpaper (path or name resolved via GESSO_DIRS)
    ///
    /// Examples:
    ///   gesso set wall.jpg
    ///   gesso set wall.jpg -t fade -d 550
    ///   gesso set wall.jpg -t wipe -f right -d 550
    Set {
        /// Image target (path or name resolved via GESSO_DIRS)
        target: String,

        /// Scaling mode for the image.
        ///
        /// fill:   fill output, crop as needed (default)
        /// fit:    fit entire image, letterbox
        /// stretch:stretch to output
        /// center: center without scaling, letterbox
        /// tile:   tile image
        #[arg(long, short = 'm', value_enum, default_value_t = ModeArg::Fill)]
        mode: ModeArg,

        /// Background fill colour used for letterboxing with fit/center (e.g. "#101010").
        #[arg(long, short = 'c')]
        colour: Option<String>,

        /// Transition type (default: none).
        ///
        /// none:  instant switch
        /// fade:  alpha blend between old/new
        /// wipe:  horizontal wipe (see --from)
        #[arg(long, short = 't', value_enum, default_value_t = TransitionArg::None)]
        transition: TransitionArg,

        /// Transition duration in milliseconds (default: 850).
        #[arg(long, short = 'd', default_value_t = 850)]
        duration: u32,

        /// Wipe direction (only used when --transition wipe).
        ///
        /// left:  new wallpaper enters from the left (default)
        /// right: new wallpaper enters from the right
        #[arg(long = "from", short = 'f', value_enum, default_value_t = WipeFromArg::Left)]
        from: WipeFromArg,

        /// Target a specific output by wl_output.name (e.g. DP-1, HDMI-A-1).
        ///
        /// Note: accepted now for forward-compat.
        #[arg(long, short = 'o')]
        output: Option<String>,
    },

    /// Set a solid colour background
    ///
    /// Examples:
    ///   gesso colour "#1e1e2e"
    ///   gesso colour "#1e1e2e" -t fade -d 200
    ///   gesso colour "#1e1e2e" -t wipe -f left -d 200
    Colour {
        /// Colour in hex form (e.g. "#1e1e2e")
        colour: String,

        /// Transition type (default: none).
        ///
        /// none:  instant switch
        /// fade:  alpha blend between old/new
        /// wipe:  horizontal wipe (see --from)
        #[arg(long, short = 't', value_enum, default_value_t = TransitionArg::None)]
        transition: TransitionArg,

        /// Transition duration in milliseconds (default: 850).
        #[arg(long, short = 'd', default_value_t = 850)]
        duration: u32,

        /// Wipe direction (only used when --transition wipe).
        ///
        /// left:  new wallpaper enters from the left (default)
        /// right: new wallpaper enters from the right
        #[arg(long = "from", short = 'f', value_enum, default_value_t = WipeFromArg::Left)]
        from: WipeFromArg,

        /// Target a specific output by wl_output.name (e.g. DP-1, HDMI-A-1).
        #[arg(long, short = 'o')]
        output: Option<String>,
    },

    /// Unset wallpaper on one output (by name) or all outputs (default).
    Unset {
        /// Output name to unset (if omitted, unsets all)
        #[arg(long, short = 'o')]
        output: Option<String>,
    },

    /// Stop the wallpaper daemon.
    Stop,

    /// Show current wallpaper state.
    Status,

    /// Run environment and compositor diagnostics.
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

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum WipeFromArg {
    Left,
    Right,
}
