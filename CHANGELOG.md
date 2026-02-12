# Changelog

All notable changes to this project are documented here.

## 0.2.1

### Improvements

- Added documentation to man page for `-f/--from`
- Added documentation to man page for `-V/--version`
- Added documentation comments so clap prints them in `gesso set -h` / `gesso colour -h`.
- Fixed documentation in README.md surounding `--duration` (was `--duration-ms`)
- Added documentation in README.md for `-f/--from`
- Move xrgb8888 into Rgb as Rgb::xrgb8888()
- Keep ease_out_cubic local to animation code
- Update all call sites accordingly

## 0.2.0

### New Features

- Added `--from` (`-f`) CLI option for wipe transitions, allowing animations from the left or right.
- Added a 5-entry MRU wallpaper cache.
- Replaced the previous single-image cache with a multi-entry index (maximum 5 entries).
- Store rendered frames per cache entry.
- Automatically evict the least recently used entry when capacity is exceeded.

### Improvements

- Optimized Wayland startup by warming up surfaces and buffers to reduce first-animation stutter.
- Improved image, colour, and Wayland rendering paths for smoother animations and better overall responsiveness.

## 0.1.3

### Fixes

- Use `Layer::Bottom` with `exclusive_zone = -1` to ensure wallpapers render behind panels.
- Set the default transition duration to 850ms for both image and colour transitions.

## 0.1.2

### Fixes

- `gesso stop` now correctly terminates the daemon.
- CLI now reports version via `-V`.

## 0.1.1

### Fixes

- Make Wayland wallpaper layer surfaces input-transparent so compositor root mouse bindings (e.g. labwc right-click menu) work correctly.

## 0.1.0

- Initial release.
