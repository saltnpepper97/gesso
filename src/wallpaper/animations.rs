// Author: Dustin Pilgrim
// License: MIT

use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::spec::WipeFrom;
use crate::wallpaper::{
    paint::{ease_out_cubic, paint_blend_frame_to_frame_fast},
    wayland::{self, Engine, SurfaceState},
};

pub(crate) const TARGET_FPS: f32 = 60.0;

#[inline]
pub(crate) fn tt_from_t(t: f32) -> u16 {
    (t.clamp(0.0, 1.0) * 256.0).round() as u16
}

/// All usable surfaces that match `output` (None = all outputs).
pub(crate) fn selected_surfaces(engine: &Engine, output: Option<&str>) -> Vec<usize> {
    let mut v = Vec::with_capacity(engine.surfaces.len());
    for si in 0..engine.surfaces.len() {
        if !wayland::surface_usable(engine, si) {
            continue;
        }
        let s = &engine.surfaces[si];
        match output {
            None => v.push(si),
            Some(want) => {
                if s.output_name.as_deref() == Some(want) {
                    v.push(si);
                }
            }
        }
    }
    v
}

/// Capture FROM frames for selected surfaces without extra allocation where possible.
pub(crate) fn capture_from_frames(engine: &Engine, sel: &[usize]) -> Vec<Option<Arc<[u32]>>> {
    let mut from_frames: Vec<Option<Arc<[u32]>>> = vec![None; engine.surfaces.len()];

    for &si in sel {
        let s = &engine.surfaces[si];
        if let Some(f) = s.last_frame.as_ref() {
            from_frames[si] = Some(Arc::clone(f));
        } else {
            let px = s.last_colour.xrgb8888();
            let w = s.width as usize;
            let h = s.height as usize;
            from_frames[si] = Some(vec![px; w * h].into());
        }
    }

    from_frames
}

/// Direction-correct wipe from `fromf` to `tof`.
/// `tt` is monotonic 0..=256; do NOT reverse time.
/// Writes directly into the current SHM buffer for speed.
pub(crate) fn paint_wipe_frame_to_frame_dir(
    s: &mut SurfaceState,
    fromf: &[u32],
    tof: &[u32],
    tt: u16,
    wipe_from: WipeFrom,
) {
    let w = s.width as usize;
    let h = s.height as usize;
    if w == 0 || h == 0 {
        return;
    }

    let Some(mmap) = s.buffers.current_mmap_mut() else {
        return;
    };
    let len = mmap.len() / 4;
    let dst = unsafe { std::slice::from_raw_parts_mut(mmap.as_mut_ptr() as *mut u32, len) };

    let tt = tt.min(256) as usize;
    let cols = ((w * tt) / 256).min(w);

    let n = (w * h).min(dst.len()).min(fromf.len()).min(tof.len());
    if n < w {
        return;
    }
    let rows = n / w;

    match wipe_from {
        WipeFrom::Left => {
            for y in 0..rows {
                let off = y * w;
                let row_dst = &mut dst[off..off + w];
                let row_from = &fromf[off..off + w];
                let row_to = &tof[off..off + w];
                if cols > 0 {
                    row_dst[..cols].copy_from_slice(&row_to[..cols]);
                }
                if cols < w {
                    row_dst[cols..].copy_from_slice(&row_from[cols..]);
                }
            }
        }
        WipeFrom::Right => {
            let start = w.saturating_sub(cols);
            for y in 0..rows {
                let off = y * w;
                let row_dst = &mut dst[off..off + w];
                let row_from = &fromf[off..off + w];
                let row_to = &tof[off..off + w];
                if start > 0 {
                    row_dst[..start].copy_from_slice(&row_from[..start]);
                }
                if start < w {
                    row_dst[start..].copy_from_slice(&row_to[start..]);
                }
            }
        }
    }
}

/// Present one blended frame at position `tt` (0..=256) across all selected surfaces.
pub(crate) fn present_blend_frame(
    engine: &mut Engine,
    sel: &[usize],
    from_frames: &[Option<Arc<[u32]>>],
    to_frames: &[Option<Arc<[u32]>>],
    tt: u16,
) -> Result<()> {
    let qh = engine.qh.clone();

    for &si in sel {
        let (Some(fromf), Some(tof)) = (from_frames[si].as_ref(), to_frames[si].as_ref()) else {
            continue;
        };

        wayland::wait_for_free_buffer_idx(engine, si)?;
        let s = &mut engine.surfaces[si];
        paint_blend_frame_to_frame_fast(s, fromf, tof, tt);
        wayland::commit_surface(&qh, s, si);
    }

    engine._conn.flush().context("flush")?;
    let _ = engine.dispatch_pending();
    Ok(())
}

/// Present one wipe frame at position `tt` (0..=256) across all selected surfaces.
pub(crate) fn present_wipe_frame(
    engine: &mut Engine,
    sel: &[usize],
    from_frames: &[Option<Arc<[u32]>>],
    to_frames: &[Option<Arc<[u32]>>],
    tt: u16,
    wipe_from: WipeFrom,
) -> Result<()> {
    let qh = engine.qh.clone();

    for &si in sel {
        let (Some(fromf), Some(tof)) = (from_frames[si].as_ref(), to_frames[si].as_ref()) else {
            continue;
        };

        wayland::wait_for_free_buffer_idx(engine, si)?;
        let s = &mut engine.surfaces[si];
        paint_wipe_frame_to_frame_dir(s, fromf, tof, tt, wipe_from);
        wayland::commit_surface(&qh, s, si);
    }

    engine._conn.flush().context("flush")?;
    let _ = engine.dispatch_pending();
    Ok(())
}

/// Compositor-paced animator. Calls `present(engine, tt)` in a loop until
/// `duration_ms` elapses. Pacing is driven by buffer release + frame callbacks
/// inside `wait_for_free_buffer_idx`; a small sleep is used only as a fallback
/// when callbacks are unavailable.
///
/// Returns the number of frames presented.
pub(crate) fn animate<F>(
    engine: &mut Engine,
    sel: &[usize],
    duration_ms: u32,
    mut present: F,
) -> Result<u32>
where
    F: FnMut(&mut Engine, u16) -> Result<()>,
{
    let duration_ms = duration_ms.max(1);
    let duration = Duration::from_millis(duration_ms as u64);
    let start = Instant::now();
    let mut frames: u32 = 0;

    // Safety sleep only when compositor frame callbacks are unavailable on all surfaces,
    // to avoid a tight CPU loop on very fast or quirky compositors.
    let soft_idle = Duration::from_secs_f32(1.0 / TARGET_FPS).min(Duration::from_millis(16));

    loop {
        let elapsed = start.elapsed();
        let t_linear = (elapsed.as_secs_f32() / duration.as_secs_f32()).min(1.0);
        let t = ease_out_cubic(t_linear);
        let tt = tt_from_t(t);

        present(engine, tt)?;
        frames = frames.wrapping_add(1);

        if t_linear >= 1.0 {
            break;
        }

        let any_cb = sel.iter().any(|&si| engine.surfaces[si].frame_callback_ok);
        if !any_cb {
            std::thread::sleep(soft_idle);
        }
    }

    Ok(frames)
}
