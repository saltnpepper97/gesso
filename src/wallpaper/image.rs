// Author: Dustin Pilgrim
// License: MIT

use anyhow::{bail, Context, Result};
use eventline as el;
use image::RgbaImage;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::spec::{Mode, Rgb, Spec, Transition, WipeFrom};
use crate::wallpaper::{
    paint::{ease_out_cubic, paint_blend_frame_to_frame_fast},
    render::render_final_frame_u32,
    wayland::{self, Engine, SurfaceState},
};

const TARGET_FPS: f32 = 60.0;

#[inline]
fn surface_matches_output_surface(s: &SurfaceState, output: Option<&str>) -> bool {
    match output {
        None => true,
        Some(want) => s.output_name.as_deref() == Some(want),
    }
}

#[inline]
fn tt_from_t(t: f32) -> u16 {
    (t.clamp(0.0, 1.0) * 256.0).round() as u16
}

#[inline]
fn selected_surfaces(engine: &Engine, output: Option<&str>) -> Vec<usize> {
    let mut v = Vec::new();
    v.reserve(engine.surfaces.len());
    for si in 0..engine.surfaces.len() {
        if !wayland::surface_usable(engine, si) {
            continue;
        }
        let s = &engine.surfaces[si];
        if !surface_matches_output_surface(s, output) {
            continue;
        }
        v.push(si);
    }
    v
}

/// Clear last_frame from all surfaces to save memory at idle.
/// Frames will be reloaded from cache or regenerated on next transition.
pub fn clear_surface_frames(engine: &mut Engine) -> Result<()> {
    el::scope!(
        "gesso.image.clear_frames",
        success = "cleared",
        failure = "failed",
        aborted = "aborted",
        {
            let mut total_bytes = 0i64;
            let mut count = 0i64;

            for s in &mut engine.surfaces {
                if let Some(frame) = s.last_frame.take() {
                    let bytes = frame.len() * 4;
                    total_bytes += bytes as i64;
                    count += 1;

                    el::debug!(
                        "cleared last_frame w={} h={} bytes={}",
                        s.width,
                        s.height,
                        bytes as i64
                    );
                }
            }

            el::info!(
                "cleared surfaces={} total_bytes={} mb={:.1}",
                count,
                total_bytes,
                total_bytes as f64 / (1024.0 * 1024.0)
            );

            Ok::<(), anyhow::Error>(())
        }
    )
}

/// Direction-correct wipe from `fromf` to `tof`.
/// `tt` is monotonic 0..=256 (do NOT reverse time).
///
/// NOTE: This writes directly into the current SHM buffer for speed.
fn paint_wipe_frame_to_frame_dir(
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

    let Some(mmap) = s.buffers.current_mmap_mut() else { return };
    let len = mmap.len() / 4;
    let dst = unsafe { std::slice::from_raw_parts_mut(mmap.as_mut_ptr() as *mut u32, len) };

    let tt = (tt.min(256)) as usize;
    let cols = ((w * tt) / 256).min(w);

    let n = (w * h)
        .min(dst.len())
        .min(fromf.len())
        .min(tof.len());
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

pub fn apply_image(engine: &mut Engine, spec: &Spec) -> Result<()> {
    el::scope!(
        "gesso.image.apply",
        success = "applied",
        failure = "failed",
        aborted = "aborted",
        {
            let (path, mode, bg, transition, output) = match spec {
                Spec::Image {
                    path,
                    mode,
                    colour,
                    transition,
                    output,
                    ..
                } => (path.as_path(), *mode, *colour, *transition, output.as_deref()),
                _ => bail!("apply_image called with non-image spec"),
            };

            el::info!(
                "begin path={path} mode={mode:?} bg={r},{g},{b} transition={transition:?} output={output}",
                path = path.display().to_string(),
                r = bg.r,
                g = bg.g,
                b = bg.b,
                output = output.unwrap_or("(all)")
            );

            // Make sure configure events are pumped before buffer work.
            engine.roundtrip()?;

            // Ensure SHM buffers for selected surfaces (no per-frame allocations later).
            {
                let shm = engine.shm.as_ref().context("wl_shm missing")?.clone();
                let qh = engine.qh.clone();
                let mut buffer_count = 0;

                for (si, s) in engine.surfaces.iter_mut().enumerate() {
                    if !s.configured || s.width == 0 || s.height == 0 {
                        continue;
                    }
                    if !surface_matches_output_surface(s, output) {
                        continue;
                    }
                    wayland::ensure_buffers_for_surface_indexed(&qh, &shm, si, s)?;
                    buffer_count += 1;
                }

                el::debug!("ensured_buffers count={count}", count = buffer_count);
            }

            // Build selected surface list once and re-use everywhere.
            let sel = selected_surfaces(engine, output);
            if sel.is_empty() {
                bail!("no usable outputs to apply image (selected output not found?)");
            }

            // Try cache first.
            let mut cached_frames: Vec<Option<Arc<[u32]>>> = vec![None; engine.surfaces.len()];
            let mut cache_any = false;
            let mut cache_all = true;

            let entry_id = crate::wallpaper::cache::find_cached_entry_id(spec).unwrap_or(None);

            if let Some(entry_id) = entry_id {
                for &si in &sel {
                    let s = &engine.surfaces[si];
                    cache_any = true;

                    match crate::wallpaper::cache::load_frame(entry_id, si, s.width, s.height)? {
                        Some(frame) => cached_frames[si] = Some(frame),
                        None => {
                            cache_all = false;
                            break;
                        }
                    }
                }
            } else {
                cache_all = false;
            }

            let cache_valid = cache_any && cache_all;
            el::info!("cache_valid={valid}", valid = cache_valid);

            if cache_valid {
                el::debug!("using cached image frames (loaded once)");
                apply_cached_frames(engine, spec, &sel, &cached_frames, bg, &transition)?;
            } else {
                el::debug!("loading and rendering new image");
                let expanded = crate::path::expand_user_path(path)?;
                let src = load_rgba(&expanded)?;

                el::info!(
                    "loaded image dimensions={w}x{h}",
                    w = src.width(),
                    h = src.height()
                );

                let cache_entry_id = crate::wallpaper::cache::record_cached_image(spec)?;

                match transition.kind {
                    Transition::None => {
                        el::debug!("applying immediate");
                        apply_image_immediate(engine, &sel, &src, mode, bg, cache_entry_id)?;
                    }
                    Transition::Fade => {
                        el::debug!("applying fade duration={ms}", ms = transition.duration);
                        fade_image(
                            engine,
                            &sel,
                            &src,
                            mode,
                            bg,
                            transition.duration,
                            cache_entry_id,
                        )?;
                    }
                    Transition::Wipe => {
                        el::debug!("applying wipe duration={ms}", ms = transition.duration);
                        wipe_image(
                            engine,
                            &sel,
                            &src,
                            mode,
                            bg,
                            transition.duration,
                            transition.wipe_from,
                            cache_entry_id,
                        )?;
                    }
                }
            }

            el::info!("done");
            Ok::<(), anyhow::Error>(())
        }
    )
}

fn apply_cached_frames(
    engine: &mut Engine,
    spec: &Spec,
    sel: &[usize],
    cached_frames: &[Option<Arc<[u32]>>],
    bg: Rgb,
    transition: &crate::spec::TransitionSpec,
) -> Result<()> {
    el::scope!(
        "gesso.image.apply_cached",
        success = "applied",
        failure = "failed",
        aborted = "aborted",
        {
            el::info!(
                "using cached frames transition={transition:?}",
                transition = transition.kind
            );

            match transition.kind {
                Transition::None => {
                    let qh = engine.qh.clone();
                    let mut any = false;

                    for &si in sel {
                        let Some(frame) = cached_frames[si].as_ref() else { continue };

                        wayland::wait_for_free_buffer_idx(engine, si)?;
                        let s = &mut engine.surfaces[si];
                        wayland::paint_frame_u32(s, frame);
                        wayland::commit_surface(&qh, s, si);

                        s.last_colour = bg;
                        s.has_image = true;
                        s.last_frame = Some(Arc::clone(frame));
                        any = true;
                    }

                    if any {
                        engine._conn.flush().context("flush")?;
                        let _ = engine.dispatch_pending();
                    }
                }
                Transition::Fade => {
                    fade_to_cached(engine, sel, cached_frames, bg, transition.duration)?;
                }
                Transition::Wipe => {
                    wipe_to_cached(
                        engine,
                        sel,
                        cached_frames,
                        bg,
                        transition.duration,
                        transition.wipe_from,
                    )?;
                }
            }

            let _ = crate::wallpaper::cache::record_cached_image(spec);
            Ok::<(), anyhow::Error>(())
        }
    )
}

/* ---------- shared (snappy) presentation ---------- */

fn present_blend_frame(
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

        // This already blocks/paces using buffer release + (optionally) frame callbacks.
        wayland::wait_for_free_buffer_idx(engine, si)?;
        let s = &mut engine.surfaces[si];
        paint_blend_frame_to_frame_fast(s, fromf, tof, tt);
        wayland::commit_surface(&qh, s, si);
    }

    engine._conn.flush().context("flush")?;
    let _ = engine.dispatch_pending();
    Ok(())
}

fn present_wipe_frame(
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

/// Capture FROM frames for selected surfaces without doing extra work.
fn capture_from_frames(engine: &Engine, sel: &[usize]) -> Vec<Option<Arc<[u32]>>> {
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

/// Render TO frames once, with reuse across identical output sizes.
/// (Common on multi-monitor setups with duplicate resolutions.)
fn render_to_frames(
    engine: &Engine,
    sel: &[usize],
    src: &RgbaImage,
    mode: Mode,
    bg: Rgb,
) -> Vec<Option<Arc<[u32]>>> {
    let mut to_frames: Vec<Option<Arc<[u32]>>> = vec![None; engine.surfaces.len()];
    let mut reuse: HashMap<(u32, u32), Arc<[u32]>> = HashMap::new();

    for &si in sel {
        let s = &engine.surfaces[si];
        let (w, h) = (s.width, s.height);

        if let Some(frame) = reuse.get(&(w, h)) {
            to_frames[si] = Some(Arc::clone(frame));
            continue;
        }

        let frame: Arc<[u32]> = render_final_frame_u32(w as usize, h as usize, src, mode, bg).into();
        reuse.insert((w, h), Arc::clone(&frame));
        to_frames[si] = Some(frame);
    }

    to_frames
}

/// Snappy animator: no fixed sleeps; pacing comes from wait_for_free_buffer_idx (buffer release + frame callbacks).
fn animate<F>(
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

    // A small safety sleep only when duration is long and callbacks are disabled,
    // to avoid a tight loop on very fast compositors / weird callback behavior.
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

        // If callbacks are disabled everywhere, the wait_for_free_buffer_idx path
        // may not naturally pace; give the CPU a tiny breather.
        let any_cb = sel.iter().any(|&si| engine.surfaces[si].frame_callback_ok);
        if !any_cb {
            std::thread::sleep(soft_idle);
        }
    }

    Ok(frames)
}

/* ---------- fade ---------- */

fn fade_to_cached(
    engine: &mut Engine,
    sel: &[usize],
    to_frames: &[Option<Arc<[u32]>>],
    bg: Rgb,
    duration: u32,
) -> Result<()> {
    el::scope!(
        "gesso.image.fade_cached",
        success = "done",
        failure = "failed",
        aborted = "aborted",
        {
            if sel.is_empty() {
                el::warn!("no surfaces selected");
                return Ok::<(), anyhow::Error>(());
            }

            let from_frames = capture_from_frames(engine, sel);

            // Present starting frame (tt=0) once.
            present_blend_frame(engine, sel, &from_frames, to_frames, 0)?;

            let frames = animate(engine, sel, duration, |eng, tt| {
                present_blend_frame(eng, sel, &from_frames, to_frames, tt)
            })?;

            // Finalize exact TO + state.
            let qh = engine.qh.clone();
            for &si in sel {
                let Some(finalf) = to_frames[si].as_ref() else { continue };

                wayland::wait_for_free_buffer_idx(engine, si)?;
                let s = &mut engine.surfaces[si];
                wayland::paint_frame_u32(s, finalf);
                wayland::commit_surface(&qh, s, si);

                s.last_colour = bg;
                s.has_image = true;
                s.last_frame = Some(Arc::clone(finalf));
            }

            engine._conn.flush().context("flush")?;
            let _ = engine.dispatch_pending();

            el::info!("frames={frames}", frames = frames as i64);
            Ok::<(), anyhow::Error>(())
        }
    )
}

fn fade_image(
    engine: &mut Engine,
    sel: &[usize],
    src: &RgbaImage,
    mode: Mode,
    bg: Rgb,
    duration: u32,
    cache_entry_id: u64,
) -> Result<()> {
    el::scope!(
        "gesso.image.fade",
        success = "done",
        failure = "failed",
        aborted = "aborted",
        {
            el::info!(
                "mode={mode:?} bg={r},{g},{b} duration_ms={ms} target_fps={fps}",
                r = bg.r,
                g = bg.g,
                b = bg.b,
                ms = duration as i64,
                fps = TARGET_FPS
            );

            if sel.is_empty() {
                bail!("no usable outputs to fade image (selected output not found?)");
            }

            let from_frames = capture_from_frames(engine, sel);

            el::debug!("rendering target frames");
            let to_frames = render_to_frames(engine, sel, src, mode, bg);

            // Present starting frame once.
            present_blend_frame(engine, sel, &from_frames, &to_frames, 0)?;

            el::debug!("starting animation");
            let frames = animate(engine, sel, duration, |eng, tt| {
                present_blend_frame(eng, sel, &from_frames, &to_frames, tt)
            })?;

            // Finalize exact TO + state + cache.
            let qh = engine.qh.clone();
            let mut any_final = false;

            for &si in sel {
                let Some(finalf) = to_frames[si].as_ref() else { continue };

                wayland::wait_for_free_buffer_idx(engine, si)?;
                let s = &mut engine.surfaces[si];
                wayland::paint_frame_u32(s, finalf);
                wayland::commit_surface(&qh, s, si);

                s.last_colour = bg;
                s.has_image = true;
                s.last_frame = Some(Arc::clone(finalf));

                let (sw, sh) = {
                    let s2 = &engine.surfaces[si];
                    (s2.width, s2.height)
                };
                let _ = crate::wallpaper::cache::store_frame(cache_entry_id, si, sw, sh, finalf);

                any_final = true;
            }

            if !any_final {
                bail!("no usable outputs to present fade image (selected output not found?)");
            }

            engine._conn.flush().context("flush")?;
            let _ = engine.dispatch_pending();

            el::info!("frames={frames}", frames = frames as i64);
            Ok::<(), anyhow::Error>(())
        }
    )
}

/* ---------- wipe ---------- */

fn wipe_to_cached(
    engine: &mut Engine,
    sel: &[usize],
    to_frames: &[Option<Arc<[u32]>>],
    bg: Rgb,
    duration: u32,
    wipe_from: WipeFrom,
) -> Result<()> {
    el::scope!(
        "gesso.image.wipe_cached",
        success = "done",
        failure = "failed",
        aborted = "aborted",
        {
            if sel.is_empty() {
                el::warn!("no surfaces selected");
                return Ok::<(), anyhow::Error>(());
            }

            let from_frames = capture_from_frames(engine, sel);

            // Present starting frame once.
            present_wipe_frame(engine, sel, &from_frames, to_frames, 0, wipe_from)?;

            let frames = animate(engine, sel, duration, |eng, tt| {
                present_wipe_frame(eng, sel, &from_frames, to_frames, tt, wipe_from)
            })?;

            // Finalize exact TO + state.
            let qh = engine.qh.clone();
            for &si in sel {
                let Some(finalf) = to_frames[si].as_ref() else { continue };

                wayland::wait_for_free_buffer_idx(engine, si)?;
                let s = &mut engine.surfaces[si];
                wayland::paint_frame_u32(s, finalf);
                wayland::commit_surface(&qh, s, si);

                s.last_colour = bg;
                s.has_image = true;
                s.last_frame = Some(Arc::clone(finalf));
            }

            engine._conn.flush().context("flush")?;
            let _ = engine.dispatch_pending();

            el::info!("frames={frames}", frames = frames as i64);
            Ok::<(), anyhow::Error>(())
        }
    )
}

fn wipe_image(
    engine: &mut Engine,
    sel: &[usize],
    src: &RgbaImage,
    mode: Mode,
    bg: Rgb,
    duration: u32,
    wipe_from: WipeFrom,
    cache_entry_id: u64,
) -> Result<()> {
    el::scope!(
        "gesso.image.wipe",
        success = "done",
        failure = "failed",
        aborted = "aborted",
        {
            el::info!(
                "mode={mode:?} bg={r},{g},{b} duration_ms={ms} target_fps={fps}",
                r = bg.r,
                g = bg.g,
                b = bg.b,
                ms = duration as i64,
                fps = TARGET_FPS
            );

            if sel.is_empty() {
                bail!("no usable outputs to wipe image (selected output not found?)");
            }

            let from_frames = capture_from_frames(engine, sel);

            el::debug!("rendering target frames");
            let to_frames = render_to_frames(engine, sel, src, mode, bg);

            // Present starting frame once.
            present_wipe_frame(engine, sel, &from_frames, &to_frames, 0, wipe_from)?;

            el::debug!("starting animation");
            let frames = animate(engine, sel, duration, |eng, tt| {
                present_wipe_frame(eng, sel, &from_frames, &to_frames, tt, wipe_from)
            })?;

            // Finalize exact TO + state + cache.
            let qh = engine.qh.clone();
            let mut any_final = false;

            for &si in sel {
                let Some(finalf) = to_frames[si].as_ref() else { continue };

                wayland::wait_for_free_buffer_idx(engine, si)?;
                let s = &mut engine.surfaces[si];
                wayland::paint_frame_u32(s, finalf);
                wayland::commit_surface(&qh, s, si);

                s.last_colour = bg;
                s.has_image = true;
                s.last_frame = Some(Arc::clone(finalf));

                let (sw, sh) = {
                    let s2 = &engine.surfaces[si];
                    (s2.width, s2.height)
                };
                let _ = crate::wallpaper::cache::store_frame(cache_entry_id, si, sw, sh, finalf);

                any_final = true;
            }

            if !any_final {
                bail!("no usable outputs to present wipe image (selected output not found?)");
            }

            engine._conn.flush().context("flush")?;
            let _ = engine.dispatch_pending();

            el::info!("frames={frames}", frames = frames as i64);
            Ok::<(), anyhow::Error>(())
        }
    )
}

/* ---------- immediate ---------- */

fn apply_image_immediate(
    engine: &mut Engine,
    sel: &[usize],
    src: &RgbaImage,
    mode: Mode,
    bg: Rgb,
    cache_entry_id: u64,
) -> Result<()> {
    el::scope!(
        "gesso.image.immediate",
        success = "applied",
        failure = "failed",
        aborted = "aborted",
        {
            el::info!(
                "mode={mode:?} bg={r},{g},{b}",
                r = bg.r,
                g = bg.g,
                b = bg.b
            );

            if sel.is_empty() {
                bail!("no usable outputs to render image (selected output not found?)");
            }

            let qh = engine.qh.clone();

            // Render once per unique (w,h) and reuse.
            let mut reuse: HashMap<(u32, u32), Arc<[u32]>> = HashMap::new();

            for &si in sel {
                wayland::wait_for_free_buffer_idx(engine, si)?;

                let (w, h) = {
                    let s = &engine.surfaces[si];
                    (s.width, s.height)
                };

                let frame = if let Some(f) = reuse.get(&(w, h)) {
                    Arc::clone(f)
                } else {
                    let f: Arc<[u32]> =
                        render_final_frame_u32(w as usize, h as usize, src, mode, bg).into();
                    reuse.insert((w, h), Arc::clone(&f));
                    f
                };

                let s = &mut engine.surfaces[si];
                wayland::paint_frame_u32(s, &frame);
                wayland::commit_surface(&qh, s, si);

                s.last_colour = bg;
                s.has_image = true;
                s.last_frame = Some(Arc::clone(&frame));

                let _ = crate::wallpaper::cache::store_frame(cache_entry_id, si, w, h, &frame);
            }

            engine._conn.flush().context("flush")?;
            let _ = engine.dispatch_pending();

            el::info!("surfaces_updated");
            Ok::<(), anyhow::Error>(())
        }
    )
}

fn load_rgba(path: &Path) -> Result<RgbaImage> {
    el::scope!(
        "gesso.image.load_rgba",
        success = "loaded",
        failure = "failed",
        aborted = "aborted",
        {
            el::debug!("loading path={path}", path = path.display().to_string());

            let img = image::open(path)
                .with_context(|| format!("decode image: {}", path.display()))?;
            let rgba = img.to_rgba8();

            el::info!("loaded dimensions={w}x{h}", w = rgba.width(), h = rgba.height());
            Ok::<RgbaImage, anyhow::Error>(rgba)
        }
    )
}
