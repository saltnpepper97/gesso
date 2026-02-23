use std::sync::Arc;

use gesso_core::{decode_image, scale_image, Colour, ScaleMode};
use gesso_ipc::protocol as ipc;

use crate::daemon::persist::resolve_image_path;
use crate::daemon::types::{PersistedSet, PersistedTarget};

/// New “pixels snapshot” function used by transitions.
/// Returns an xrgb8888 buffer sized exactly to the output (width*height*4).
pub fn snapshot_pixels_for_output(
    out: &gesso_wl::OutputInfo,
    prev: Option<&PersistedSet>,
) -> Arc<Vec<u8>> {
    let w = out.width;
    let h = out.height;
    let stride = w as usize * 4;
    let nbytes = stride * h as usize;

    let Some(ps) = prev else {
        return Arc::new(vec![0u8; nbytes]);
    };

    match &ps.target {
        PersistedTarget::Unset => Arc::new(vec![0u8; nbytes]),

        PersistedTarget::Colour { r, g, b } => {
            let px = ((*r as u32) << 16) | ((*g as u32) << 8) | (*b as u32);
            let mut bytes = vec![0u8; nbytes];
            let words = unsafe {
                std::slice::from_raw_parts_mut(bytes.as_mut_ptr() as *mut u32, bytes.len() / 4)
            };
            words.fill(px);
            Arc::new(bytes)
        }

        PersistedTarget::ImagePath { path } => {
            let Some(resolved) = resolve_image_path(path) else {
                return Arc::new(vec![0u8; nbytes]);
            };

            let decoded = match decode_image(&resolved) {
                Ok(d) => d,
                Err(_) => return Arc::new(vec![0u8; nbytes]),
            };

            let mode = ps.mode.unwrap_or(ipc::Mode::Fill);
            let bg = ps.bg_colour.unwrap_or(ipc::Rgb { r: 0, g: 0, b: 0 });

            let pixels = scale_image(
                &decoded,
                w,
                h,
                to_scale_mode(mode),
                Colour { r: bg.r, g: bg.g, b: bg.b },
            );

            Arc::new(pixels)
        }
    }
}

/// Back-compat: if older code imports `snapshot_for_output`, keep it working.
/// (Some of your compiler output suggests this name existed previously.)
pub fn snapshot_for_output(
    out: &gesso_wl::OutputInfo,
    prev: Option<&PersistedSet>,
) -> Arc<Vec<u8>> {
    snapshot_pixels_for_output(out, prev)
}

fn to_scale_mode(m: ipc::Mode) -> ScaleMode {
    match m {
        ipc::Mode::Fill => ScaleMode::Fill,
        ipc::Mode::Fit => ScaleMode::Fit,
        ipc::Mode::Stretch => ScaleMode::Stretch,
        ipc::Mode::Center => ScaleMode::Center,
        ipc::Mode::Tile => ScaleMode::Tile,
    }
}
