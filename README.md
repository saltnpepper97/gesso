> [!NOTE]
> On my setup (one 2560×1440 + one 1920×1080 output), `gessod` typically idles under ~10 MiB RSS after setting wallpapers on both outputs.
> Your mileage may vary by compositor, output count, and image sizes.

# gesso

**Deterministic Wayland wallpaper daemon + CLI** with smooth transitions and low idle memory.

`gessod` owns a background surface per output (via **wlr-layer-shell**) and the `gesso` CLI sends requests over a Unix socket. It’s built to be *boring*: predictable timing, no runaway animations, and it stays light when idle.

Wayland-only. Requires a compositor that supports **layer-shell** (most wlroots-based compositors do).

---

## Demo

<video src="assets/demo.mp4" controls width="100%">
  Your browser does not support the video tag.
</video>

---

## Highlights

- **Per-output control** (by compositor output name like `DP-1`, `HDMI-A-1`)
- **Image and solid-colour** wallpapers
- Image modes: **fill / fit / stretch / center / tile**
- Transitions: **none / drop / fade / wave**
  - Optional **duration override**
  - Optional **step-quantized** transitions (`--transition-steps`)
  - `wave` supports **direction** (`--from left|right`)
- Deterministic pacing (no “runaway” frame scheduling)
- SHM rendering, buffer release when idle, and low persistent memory use

---

## Supported image formats

| Format | Support | Notes |
|---|---:|---|
| PNG | ✅ | Static images |
| JPEG | ✅ | Static images |
| WebP | ✅ | **Static only** (animated WebP not supported) |
| GIF | ✅ | **Animated GIFs play** (looping per GIF loop extension). Frames are pre-scaled per output when set. |

---

## Install

### Build from source

    git clone <your-repo-url>
    cd gesso
    cargo build --release

Binaries:

./target/release/gesso

./target/release/gessod

---

## Quick start

### 1) Start the daemon

Start it from your compositor autostart (or a user service):

    gessod

### 2) Set an image wallpaper

    gesso set ~/Pictures/wallpaper.png

### 3) Set a solid colour

    gesso colour "#0b0f14"

---

## Usage

### Image modes

gesso set ~/Pictures/wallpaper.png --mode fill
gesso set ~/Pictures/wallpaper.png --mode fit
gesso set ~/Pictures/wallpaper.png --mode stretch
gesso set ~/Pictures/wallpaper.png --mode center
gesso set ~/Pictures/wallpaper.png --mode tile

### Background colour for letterboxing (fit/center)

gesso set ~/Pictures/wallpaper.png --mode fit --colour "#101010"

### Transitions

**Fade:**

    gesso set ~/Pictures/wallpaper.png --transition fade
    gesso set ~/Pictures/wallpaper.png --transition fade --duration 600

**Drop (hard expanding circle):**

    gesso set ~/Pictures/wallpaper.png --transition drop
    gesso set ~/Pictures/wallpaper.png --transition drop --duration 750

**Wave (directional wipe):**

    gesso set ~/Pictures/wallpaper.png --transition wave
    gesso set ~/Pictures/wallpaper.png --transition wave --from right
    gesso set ~/Pictures/wallpaper.png --transition wave --from left --duration 920

**Step-quantized transitions** (discrete stepping instead of smooth):

    gesso set ~/Pictures/wallpaper.png --transition fade --transition-steps 30
    gesso colour "#1e1e2e" --transition wave --transition-steps 24

> [!TIP] 
> `--transition-steps 0` (or omitted) means smooth.

---

## Target a specific output

Targets use the compositor-provided `wl_output.name` (examples: `DP-1`, `HDMI-A-1`).

    gesso outputs
    gesso set ~/Pictures/wallpaper.png --output DP-1
    gesso colour "#111111" --output HDMI-A-1

---

## Unset

Unset all outputs:

    gesso unset

Unset one output:

    gesso unset DP-1

---

## Introspection / control

List outputs (name, geometry, scale):

    gesso outputs

Show current state per output:

    gesso info

Compositor / Wayland health checks and warnings:

    gesso doctor

Stop the daemon:

    gesso stop

---

## CLI reference

### `gesso set`

Set an image wallpaper (path, or a name resolved via `GESSO_DIRS`).

gesso set <target> [OPTIONS]

Options:

- `-m, --mode <fill|fit|stretch|center|tile>`  
  Default: `fill`

- `-c, --colour <hex>`  
  Background fill colour for `fit` / `center` (e.g. `#101010`)

- `-t, --transition <none|drop|fade|wave>`  
  Default: `none`

- `-d, --duration <ms>`  
  Overrides the transition duration in milliseconds

- `-s, --transition-steps <N>`  
  Quantize the transition into `N` steps (omit or `0` for smooth)

- `-f, --from <left|right>`  
  Only used for `--transition wave`  
  Default: `left`

- `-o, --output <NAME>`  
  Target a specific output (e.g. `DP-1`)

---

### `gesso colour`

Set a solid colour wallpaper.

gesso colour <colour> [OPTIONS]

Options:

- `-t, --transition <none|drop|fade|wave>`  
  Default: `none`

- `-d, --duration <ms>`  
  Overrides the transition duration in milliseconds

- `-s, --transition-steps <N>`  
  Quantize the transition into `N` steps (omit or `0` for smooth)

- `-f, --from <left|right>`  
  Only used for `--transition wave`  
  Default: `left`

- `-o, --output <NAME>`  
  Target a specific output

---

### `gesso unset`

Unset wallpaper on one output or all outputs.

gesso unset [OUTPUT]

If `OUTPUT` is omitted, unsets all outputs.

---

### Other commands

gesso outputs
gesso info
gesso doctor
gesso stop

---

## Environment variables

### `WAYLAND_DISPLAY`

Wayland-only: `WAYLAND_DISPLAY` must be set (your compositor normally does this).

### `GESSO_DIRS`

Optional search paths used by `gesso set <target>` when `<target>` is not an absolute/relative path.

Example:

    export GESSO_DIRS="$HOME/Pictures/Wallpapers:/usr/share/backgrounds"

### `GESSO_SOCKET`

Override the IPC socket path used to communicate between `gesso` and `gessod`.
Default is derived from `XDG_RUNTIME_DIR`.

    export GESSO_SOCKET="/run/user/1000/gesso.sock"

---

## Notes / design goals

`gesso` is designed to be “boring in the best way”:

- deterministic timing and predictable transitions
- no runaway animations
- graceful behavior when compositors are quirky
- low idle memory: render what’s needed, release buffers when quiet

Silence is a feature.
