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
    ///   gesso set wall.jpg -t drop
    ///   gesso set wall.jpg -t fade -d 350
    ///   gesso set wall.jpg -t wave -f right
    ///   gesso set wall.jpg -t fade -s 24
    Set {
        target: String,

        #[arg(long, short = 'm', value_enum, default_value_t = ModeArg::Fill)]
        mode: ModeArg,

        /// Background fill colour for fit/center (e.g. "#101010")
        #[arg(long, short = 'c')]
        colour: Option<String>,

        /// Transition type (default: none)
        ///
        /// none: instant
        /// drop: hard circle expands from center (default ~750ms, mode-adjusted)
        /// fade: crossfade (default ~360ms, mode-adjusted)
        /// wave: directional wipe (default ~920ms, mode-adjusted, see --from)
        #[arg(long, short = 't', value_enum, default_value_t = TransitionArg::None)]
        transition: TransitionArg,

        /// Override transition duration in ms.
        #[arg(long, short = 'd')]
        duration: Option<u32>,

        /// Quantize the transition into N discrete steps (e.g. 24, 60).
        /// 0/omitted = smooth.
        #[arg(long = "transition-steps", short = 's')]
        transition_steps: Option<u16>,

        /// Wipe direction (only used when --transition wave).
        #[arg(long = "from", short = 'f', value_enum, default_value_t = WaveFromArg::Left)]
        from: WaveFromArg,

        /// Target a specific output by wl_output.name (e.g. DP-1, HDMI-A-1).
        #[arg(long, short = 'o')]
        output: Option<String>,
    },

    /// Set a solid colour background
    ///
    /// Examples:
    ///   gesso colour "#1e1e2e"
    ///   gesso colour "#1e1e2e" -t drop
    ///   gesso colour "#1e1e2e" -t wave -f left -d 200
    ///   gesso colour "#1e1e2e" -t fade -s 30
    Colour {
        colour: String,

        /// Transition type (default: none)
        ///
        /// none: instant
        /// drop: hard circle expands from center (default ~620ms)
        /// fade: crossfade (default ~300ms)
        /// wave: directional wipe (default ~820ms, see --from)
        #[arg(long, short = 't', value_enum, default_value_t = TransitionArg::None)]
        transition: TransitionArg,

        /// Override transition duration in ms.
        #[arg(long, short = 'd')]
        duration: Option<u32>,

        /// Quantize the transition into N discrete steps (e.g. 24, 60).
        /// 0/omitted = smooth.
        #[arg(long = "transition-steps", short = 's')]
        transition_steps: Option<u16>,

        /// Wipe direction (only used when --transition wave).
        #[arg(long = "from", short = 'f', value_enum, default_value_t = WaveFromArg::Left)]
        from: WaveFromArg,

        #[arg(long, short = 'o')]
        output: Option<String>,
    },

    /// Unset wallpaper on one output (by name) or all outputs (default).
    ///
    /// Examples:
    ///   gesso unset          # all
    ///   gesso unset DP-1     # just DP-1
    Unset {
        /// Output name (positional). If omitted, unsets all outputs.
        #[arg(value_name = "OUTPUT")]
        output: Option<String>,
    },

    /// List all detected outputs with resolution and scale.
    Outputs,

    /// Show current wallpaper, mode, and transition for each output.
    ///
    /// One key=value pair per line, prefixed with the output name — easy to grep:
    ///   gesso info | grep DP-1
    Info,

    /// Show Wayland compositor health: globals, shm formats, and warnings.
    Doctor,

    Stop,
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
pub enum TransitionArg {
    None,
    Drop,
    Fade,
    Wave,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum WaveFromArg {
    Left,
    Right,
}
