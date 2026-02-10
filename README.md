# gesso

Deterministic Wayland wallpaper daemon and controller with smooth, low-latency transitions.

`gesso` renders wallpapers using **wlr-layer-shell**, creating a dedicated background surface per output. It supports image and solid-colour wallpapers with carefully paced fade and wipe animations designed to stay smooth even under compositor quirks.

Wayland-only. Requires a compositor with layer-shell support (most wlroots-based compositors).

---

## Features

- Image wallpapers with multiple modes (fill, fit, stretch, center, tile)
- Solid colour wallpapers
- Smooth fade and wipe transitions
- Per-output targeting
- Deterministic timing (no runaway frame pacing)
- SHM double-buffering with frame-callback fallback
- Cached last frames for instant re-apply
- Minimal dependencies, daemon-friendly design

---

## Installation

### Build from source

    git clone <your-repo-url>
    cd gesso
    cargo build --release

Binary location:

    ./target/release/gesso

---

## Usage

### Starting the daemon

In your wayland compositors autostart section start gesso daemon with:

```gessod ```

### Set an image wallpaper

    gesso set ~/Pictures/wallpaper.png

### Image modes

    gesso set ~/Pictures/wallpaper.png --mode fill
    gesso set ~/Pictures/wallpaper.png --mode fit
    gesso set ~/Pictures/wallpaper.png --mode stretch
    gesso set ~/Pictures/wallpaper.png --mode center
    gesso set ~/Pictures/wallpaper.png --mode tile

### Background colour (for fit / center letterboxing)

    gesso set ~/Pictures/wallpaper.png --mode fit --colour "#101010"

### Transitions

Fade:

    gesso set ~/Pictures/wallpaper.png --transition fade --duration-ms 600

Wipe:

    gesso set ~/Pictures/wallpaper.png --transition wipe --duration-ms 750

### Solid colour

    gesso colour "#0b0f14"

With transition:

    gesso colour "#0b0f14" --transition fade --duration-ms 200
    gesso colour "#0b0f14" --transition wipe --duration-ms 260

### Target a specific output

Uses the compositor’s `wl_output.name` (e.g. `DP-1`, `HDMI-A-1`).

    gesso set ~/Pictures/wallpaper.png --output DP-1
    gesso colour "#111111" --output DP-1

---

## Unset wallpaper

Unset all outputs:

    gesso unset

Unset one output:

    gesso unset --output DP-1

---

## CLI Reference

### gesso set

Set an image wallpaper (path or name resolved via `GESSO_DIRS`).

    gesso set <target> [OPTIONS]

Options:

- `-m, --mode <fill|fit|stretch|center|tile>`  
  Default: `fill`

- `-c, --colour <hex>`  
  Background fill colour for fit/center letterboxing (e.g. `#101010`)

- `-t, --transition <none|fade|wipe>`  
  Default: `none`

- `-d, --duration-ms <ms>`  
  Default: `550`

- `-o, --output <name>`  
  Target a specific output

---

### gesso colour

Set a solid colour wallpaper.

    gesso colour <colour> [OPTIONS]

Options:

- `-t, --transition <none|fade|wipe>`  
  Default: `none`

- `-d, --duration-ms <ms>`  
  Default: `200`

- `-o, --output <name>`  
  Target a specific output

---

### gesso unset

Unset wallpaper on one output or all outputs.

    gesso unset [--output <name>]

---

### Other commands

    gesso status
    gesso doctor
    gesso stop

---

## Environment

### WAYLAND_DISPLAY

Must be set (Wayland-only).

### GESSO_DIRS

Optional search paths for `gesso set <target>` when `<target>` is not an absolute or relative path.

Example:

    export GESSO_DIRS="$HOME/Pictures/Wallpapers:/usr/share/backgrounds"

---

## Philosophy

`gesso` is designed to be boring in the best way:

- deterministic timing
- no runaway animations
- no daemon lockups
- graceful degradation when compositors misbehave

Silence is a feature.
