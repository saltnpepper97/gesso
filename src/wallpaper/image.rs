// Author: Dustin Pilgrim
// License: MIT

use anyhow::{bail, Context, Result};
use eventline as el;
use image::RgbaImage;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::spec::{Mode, Rgb, Spec, Transition};
use crate::wallpaper::{
    paint::{paint_blend_frame_to_frame_fast, paint_wipe_frame_to_frame_fast},
    render::render_final_frame_u32,
    util::{ease_out_cubic, xrgb8888},
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

            // Ensure configuration
            engine.roundtrip()?;

            // Ensure SHM buffers for selected surfaces
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

            // ---- IMPORTANT FIX ----
            // Do NOT "validate cache" by loading frames and then load them again.
            // Load cached frames ONCE, decide validity, and reuse them.
            let mut cached_frames: Vec<Option<Arc<[u32]>>> = vec![None; engine.surfaces.len()];
            let mut cache_any = false;
            let mut cache_all = true;

            if crate::wallpaper::cache::cached_image_matches(spec).unwrap_or(false) {
                for (si, s) in engine.surfaces.iter().enumerate() {
                    if !s.configured || s.width == 0 || s.height == 0 {
                        continue;
                    }
                    if !surface_matches_output_surface(s, output) {
                        continue;
                    }

                    cache_any = true;

                    match crate::wallpaper::cache::load_last_frame(si, s.width, s.height)? {
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
                apply_cached_frames(engine, spec, &cached_frames, bg, &transition, output)?;
            } else {
                el::debug!("loading and rendering new image");
                let expanded = crate::path::expand_user_path(path)?;
                let src = load_rgba(&expanded)?;

                el::info!(
                    "loaded image dimensions={w}x{h}",
                    w = src.width(),
                    h = src.height()
                );

                match transition.kind {
                    Transition::None => {
                        el::debug!("applying immediate");
                        apply_image_immediate(engine, &src, mode, bg, output)?;
                    }
                    Transition::Fade => {
                        el::debug!("applying fade duration={ms}", ms = transition.duration);
                        fade_image(engine, &src, mode, bg, transition.duration, output)?;
                    }
                    Transition::Wipe => {
                        el::debug!("applying wipe duration={ms}", ms = transition.duration);
                        wipe_image(engine, &src, mode, bg, transition.duration, output)?;
                    }
                }

                if let Ok(key) = crate::wallpaper::cache::compute_image_key(spec) {
                    let _ = crate::wallpaper::cache::write_last_image_key(&key);
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
    cached_frames: &[Option<Arc<[u32]>>],
    bg: Rgb,
    transition: &crate::spec::TransitionSpec,
    output: Option<&str>,
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

                    for si in 0..engine.surfaces.len() {
                        if !wayland::surface_usable(engine, si) {
                            continue;
                        }
                        let matches = {
                            let s = &engine.surfaces[si];
                            surface_matches_output_surface(s, output)
                        };
                        if !matches {
                            continue;
                        }

                        let Some(frame) = cached_frames[si].as_ref() else { continue };

                        wayland::wait_for_free_buffer_idx(engine, si)?;
                        {
                            let s = &mut engine.surfaces[si];
                            wayland::paint_frame_u32(s, frame);
                            wayland::commit_surface(&qh, s, si);

                            s.last_colour = bg;
                            s.has_image = true;
                            s.last_frame = Some(Arc::clone(frame));
                        }

                        any = true;
                    }

                    if any {
                        engine._conn.flush().context("flush")?;
                        engine.dispatch_pending()?;
                    }
                }
                Transition::Fade => {
                    fade_to_cached(engine, cached_frames, bg, transition.duration, output)?
                }
                Transition::Wipe => {
                    wipe_to_cached(engine, cached_frames, bg, transition.duration, output)?
                }
            }

            if let Ok(key) = crate::wallpaper::cache::compute_image_key(spec) {
                let _ = crate::wallpaper::cache::write_last_image_key(&key);
            }

            Ok::<(), anyhow::Error>(())
        }
    )
}

/* ---------- fade ---------- */

fn present_blend_frame(
    engine: &mut Engine,
    from_frames: &[Option<Arc<[u32]>>],
    to_frames: &[Option<Arc<[u32]>>],
    tt: u16,
    output: Option<&str>,
) -> Result<()> {
    let qh = engine.qh.clone();

    for si in 0..engine.surfaces.len() {
        if !wayland::surface_usable(engine, si) {
            continue;
        }
        let matches = {
            let s = &engine.surfaces[si];
            surface_matches_output_surface(s, output)
        };
        if !matches {
            continue;
        }

        let (Some(fromf), Some(tof)) = (from_frames[si].as_ref(), to_frames[si].as_ref()) else {
            continue;
        };

        wayland::wait_for_free_buffer_idx(engine, si)?;
        let s = &mut engine.surfaces[si];
        paint_blend_frame_to_frame_fast(s, fromf, tof, tt);
        wayland::commit_surface(&qh, s, si);
    }

    engine._conn.flush().context("flush")?;
    engine.dispatch_pending()?;
    Ok(())
}

fn fade_to_cached(
    engine: &mut Engine,
    to_frames: &[Option<Arc<[u32]>>],
    bg: Rgb,
    duration: u32,
    output: Option<&str>,
) -> Result<()> {
    el::scope!(
        "gesso.image.fade_cached",
        success = "done",
        failure = "failed",
        aborted = "aborted",
        {
            let duration = duration.max(1);
            let duration = Duration::from_millis(duration as u64);
            let frame_dt = Duration::from_secs_f32(1.0 / TARGET_FPS);

            el::info!("duration={ms} target_fps={fps}", ms = duration.as_millis() as i64, fps = TARGET_FPS);

            let mut from_frames: Vec<Option<Arc<[u32]>>> = vec![None; engine.surfaces.len()];
            let mut any = false;

            for si in 0..engine.surfaces.len() {
                let s = &engine.surfaces[si];
                if !s.configured || s.width == 0 || s.height == 0 {
                    continue;
                }
                if !surface_matches_output_surface(s, output) {
                    continue;
                }

                any = true;

                if let Some(f) = s.last_frame.as_ref() {
                    from_frames[si] = Some(Arc::clone(f));
                } else {
                    let px = xrgb8888(s.last_colour);
                    let w = s.width as usize;
                    let h = s.height as usize;
                    from_frames[si] = Some(vec![px; w * h].into());
                }
            }

            if !any {
                el::warn!("no surfaces selected");
                return Ok::<(), anyhow::Error>(());
            }

            // Present frame 0 immediately (reduces "first-frame hitch").
            present_blend_frame(engine, &from_frames, to_frames, 0, output)?;

            // Start timing AFTER first present.
            let start = Instant::now();
            let mut frames: u32 = 0;

            loop {
                let elapsed = start.elapsed();
                if elapsed >= duration {
                    break;
                }

                let t_linear = (elapsed.as_secs_f32() / duration.as_secs_f32()).min(1.0);
                let t = ease_out_cubic(t_linear);
                let tt = (t.clamp(0.0, 1.0) * 256.0).round() as u16;

                present_blend_frame(engine, &from_frames, to_frames, tt, output)?;

                frames += 1;
                let next = start + frame_dt * frames;
                let now2 = Instant::now();
                if next > now2 && next < start + duration {
                    std::thread::sleep(next - now2);
                }
            }

            // Final frame + update state
            let qh = engine.qh.clone();
            for si in 0..engine.surfaces.len() {
                if !wayland::surface_usable(engine, si) {
                    continue;
                }
                let matches = {
                    let s = &engine.surfaces[si];
                    surface_matches_output_surface(s, output)
                };
                if !matches {
                    continue;
                }

                let Some(finalf) = to_frames[si].as_ref() else { continue };

                wayland::wait_for_free_buffer_idx(engine, si)?;
                {
                    let s = &mut engine.surfaces[si];
                    wayland::paint_frame_u32(s, finalf);
                    wayland::commit_surface(&qh, s, si);

                    s.last_colour = bg;
                    s.has_image = true;
                    s.last_frame = Some(Arc::clone(finalf));
                }
            }

            engine._conn.flush().context("flush")?;
            engine.dispatch_pending()?;

            let elapsed = start.elapsed();
            el::info!("frames={frames} elapsed_ms={ms}", frames = frames, ms = elapsed.as_millis());

            Ok::<(), anyhow::Error>(())
        }
    )
}

fn fade_image(
    engine: &mut Engine,
    src: &RgbaImage,
    mode: Mode,
    bg: Rgb,
    duration: u32,
    output: Option<&str>,
) -> Result<()> {
    el::scope!(
        "gesso.image.fade",
        success = "done",
        failure = "failed",
        aborted = "aborted",
        {
            let duration = duration.max(1);
            let duration = Duration::from_millis(duration as u64);
            let frame_dt = Duration::from_secs_f32(1.0 / TARGET_FPS);

            el::info!(
                "mode={mode:?} bg={r},{g},{b} duration={ms} target_fps={fps}",
                r = bg.r,
                g = bg.g,
                b = bg.b,
                ms = duration.as_millis() as i64,
                fps = TARGET_FPS
            );

            let mut from_frames: Vec<Option<Arc<[u32]>>> = vec![None; engine.surfaces.len()];
            let mut any = false;

            for si in 0..engine.surfaces.len() {
                let s = &engine.surfaces[si];
                if !s.configured || s.width == 0 || s.height == 0 {
                    continue;
                }
                if !surface_matches_output_surface(s, output) {
                    continue;
                }

                any = true;

                if let Some(f) = s.last_frame.as_ref() {
                    from_frames[si] = Some(Arc::clone(f));
                } else {
                    let px = xrgb8888(s.last_colour);
                    let w = s.width as usize;
                    let h = s.height as usize;
                    from_frames[si] = Some(vec![px; w * h].into());
                }
            }

            if !any {
                bail!("no usable outputs to fade image (selected output not found?)");
            }

            el::debug!("rendering target frames");
            let mut to_frames: Vec<Option<Arc<[u32]>>> = vec![None; engine.surfaces.len()];
            for si in 0..engine.surfaces.len() {
                let s = &engine.surfaces[si];
                if !s.configured || s.width == 0 || s.height == 0 {
                    continue;
                }
                if !surface_matches_output_surface(s, output) {
                    continue;
                }

                let w = s.width as usize;
                let h = s.height as usize;
                let frame: Arc<[u32]> = render_final_frame_u32(w, h, src, mode, bg).into();
                to_frames[si] = Some(frame);
            }

            // Present frame 0 immediately (reduces "first-frame hitch").
            present_blend_frame(engine, &from_frames, &to_frames, 0, output)?;

            let start = Instant::now();
            let mut frames: u32 = 0;

            el::debug!("starting animation");
            loop {
                let elapsed = start.elapsed();
                if elapsed >= duration {
                    break;
                }

                let t_linear = (elapsed.as_secs_f32() / duration.as_secs_f32()).min(1.0);
                let t = ease_out_cubic(t_linear);
                let tt = (t.clamp(0.0, 1.0) * 256.0).round() as u16;

                present_blend_frame(engine, &from_frames, &to_frames, tt, output)?;

                frames += 1;
                let next = start + frame_dt * frames;
                let now2 = Instant::now();
                if next > now2 && next < start + duration {
                    std::thread::sleep(next - now2);
                }
            }

            // Final present + persist cache + state
            let qh = engine.qh.clone();
            let mut any_final = false;

            for si in 0..engine.surfaces.len() {
                if !wayland::surface_usable(engine, si) {
                    continue;
                }
                let matches = {
                    let s = &engine.surfaces[si];
                    surface_matches_output_surface(s, output)
                };
                if !matches {
                    continue;
                }

                let Some(finalf) = to_frames[si].as_ref() else { continue };

                wayland::wait_for_free_buffer_idx(engine, si)?;
                {
                    let s = &mut engine.surfaces[si];
                    wayland::paint_frame_u32(s, finalf);
                    wayland::commit_surface(&qh, s, si);

                    s.last_colour = bg;
                    s.has_image = true;
                    s.last_frame = Some(Arc::clone(finalf));
                }

                let (sw, sh) = {
                    let s2 = &engine.surfaces[si];
                    (s2.width, s2.height)
                };
                let _ = crate::wallpaper::cache::store_last_frame(si, sw, sh, finalf);

                any_final = true;
            }

            if !any_final {
                bail!("no usable outputs to present fade image (selected output not found?)");
            }

            engine._conn.flush().context("flush")?;
            engine.dispatch_pending()?;

            let elapsed = start.elapsed();
            el::info!("frames={frames} elapsed_ms={ms}", frames = frames, ms = elapsed.as_millis());

            Ok::<(), anyhow::Error>(())
        }
    )
}

/* ---------- wipe ---------- */

fn present_wipe_frame(
    engine: &mut Engine,
    from_frames: &[Option<Arc<[u32]>>],
    to_frames: &[Option<Arc<[u32]>>],
    tt: u16,
    output: Option<&str>,
) -> Result<()> {
    let qh = engine.qh.clone();

    for si in 0..engine.surfaces.len() {
        if !wayland::surface_usable(engine, si) {
            continue;
        }
        let matches = {
            let s = &engine.surfaces[si];
            surface_matches_output_surface(s, output)
        };
        if !matches {
            continue;
        }

        let (Some(fromf), Some(tof)) = (from_frames[si].as_ref(), to_frames[si].as_ref()) else {
            continue;
        };

        wayland::wait_for_free_buffer_idx(engine, si)?;
        let s = &mut engine.surfaces[si];
        paint_wipe_frame_to_frame_fast(s, fromf, tof, tt);
        wayland::commit_surface(&qh, s, si);
    }

    engine._conn.flush().context("flush")?;
    engine.dispatch_pending()?;
    Ok(())
}

fn wipe_to_cached(
    engine: &mut Engine,
    to_frames: &[Option<Arc<[u32]>>],
    bg: Rgb,
    duration: u32,
    output: Option<&str>,
) -> Result<()> {
    el::scope!(
        "gesso.image.wipe_cached",
        success = "done",
        failure = "failed",
        aborted = "aborted",
        {
            let duration = duration.max(1);
            let duration = Duration::from_millis(duration as u64);
            let frame_dt = Duration::from_secs_f32(1.0 / TARGET_FPS);

            el::info!("duration={ms} target_fps={fps}", ms = duration.as_millis() as i64, fps = TARGET_FPS);

            let mut from_frames: Vec<Option<Arc<[u32]>>> = vec![None; engine.surfaces.len()];
            let mut any = false;

            for si in 0..engine.surfaces.len() {
                let s = &engine.surfaces[si];
                if !s.configured || s.width == 0 || s.height == 0 {
                    continue;
                }
                if !surface_matches_output_surface(s, output) {
                    continue;
                }

                any = true;

                if let Some(f) = s.last_frame.as_ref() {
                    from_frames[si] = Some(Arc::clone(f));
                } else {
                    let px = xrgb8888(s.last_colour);
                    let w = s.width as usize;
                    let h = s.height as usize;
                    from_frames[si] = Some(vec![px; w * h].into());
                }
            }

            if !any {
                el::warn!("no surfaces selected");
                return Ok::<(), anyhow::Error>(());
            }

            // Present frame 0 immediately (reduces "first-frame hitch").
            present_wipe_frame(engine, &from_frames, to_frames, 0, output)?;

            let start = Instant::now();
            let mut frames: u32 = 0;

            loop {
                let elapsed = start.elapsed();
                if elapsed >= duration {
                    break;
                }

                let t_linear = (elapsed.as_secs_f32() / duration.as_secs_f32()).min(1.0);
                let t = ease_out_cubic(t_linear);
                let tt = (t.clamp(0.0, 1.0) * 256.0).round() as u16;

                present_wipe_frame(engine, &from_frames, to_frames, tt, output)?;

                frames += 1;
                let next = start + frame_dt * frames;
                let now2 = Instant::now();
                if next > now2 && next < start + duration {
                    std::thread::sleep(next - now2);
                }
            }

            // Final frame + update state
            let qh = engine.qh.clone();
            for si in 0..engine.surfaces.len() {
                if !wayland::surface_usable(engine, si) {
                    continue;
                }
                let matches = {
                    let s = &engine.surfaces[si];
                    surface_matches_output_surface(s, output)
                };
                if !matches {
                    continue;
                }

                let Some(finalf) = to_frames[si].as_ref() else { continue };

                wayland::wait_for_free_buffer_idx(engine, si)?;
                {
                    let s = &mut engine.surfaces[si];
                    wayland::paint_frame_u32(s, finalf);
                    wayland::commit_surface(&qh, s, si);

                    s.last_colour = bg;
                    s.has_image = true;
                    s.last_frame = Some(Arc::clone(finalf));
                }
            }

            engine._conn.flush().context("flush")?;
            engine.dispatch_pending()?;

            let elapsed = start.elapsed();
            el::info!("frames={frames} elapsed_ms={ms}", frames = frames, ms = elapsed.as_millis());

            Ok::<(), anyhow::Error>(())
        }
    )
}

fn wipe_image(
    engine: &mut Engine,
    src: &RgbaImage,
    mode: Mode,
    bg: Rgb,
    duration: u32,
    output: Option<&str>,
) -> Result<()> {
    el::scope!(
        "gesso.image.wipe",
        success = "done",
        failure = "failed",
        aborted = "aborted",
        {
            let duration = duration.max(1);
            let duration = Duration::from_millis(duration as u64);
            let frame_dt = Duration::from_secs_f32(1.0 / TARGET_FPS);

            el::info!(
                "mode={mode:?} bg={r},{g},{b} duration={ms} target_fps={fps}",
                r = bg.r,
                g = bg.g,
                b = bg.b,
                ms = duration.as_millis() as i64,
                fps = TARGET_FPS
            );

            let mut from_frames: Vec<Option<Arc<[u32]>>> = vec![None; engine.surfaces.len()];
            let mut any = false;

            for si in 0..engine.surfaces.len() {
                let s = &engine.surfaces[si];
                if !s.configured || s.width == 0 || s.height == 0 {
                    continue;
                }
                if !surface_matches_output_surface(s, output) {
                    continue;
                }

                any = true;

                if let Some(f) = s.last_frame.as_ref() {
                    from_frames[si] = Some(Arc::clone(f));
                } else {
                    let px = xrgb8888(s.last_colour);
                    let w = s.width as usize;
                    let h = s.height as usize;
                    from_frames[si] = Some(vec![px; w * h].into());
                }
            }

            if !any {
                bail!("no usable outputs to wipe image (selected output not found?)");
            }

            el::debug!("rendering target frames");
            let mut to_frames: Vec<Option<Arc<[u32]>>> = vec![None; engine.surfaces.len()];
            for si in 0..engine.surfaces.len() {
                let s = &engine.surfaces[si];
                if !s.configured || s.width == 0 || s.height == 0 {
                    continue;
                }
                if !surface_matches_output_surface(s, output) {
                    continue;
                }

                let w = s.width as usize;
                let h = s.height as usize;
                let frame: Arc<[u32]> = render_final_frame_u32(w, h, src, mode, bg).into();
                to_frames[si] = Some(frame);
            }

            // Present frame 0 immediately (reduces "first-frame hitch").
            present_wipe_frame(engine, &from_frames, &to_frames, 0, output)?;

            let start = Instant::now();
            let mut frames: u32 = 0;

            el::debug!("starting animation");
            loop {
                let elapsed = start.elapsed();
                if elapsed >= duration {
                    break;
                }

                let t_linear = (elapsed.as_secs_f32() / duration.as_secs_f32()).min(1.0);
                let t = ease_out_cubic(t_linear);
                let tt = (t.clamp(0.0, 1.0) * 256.0).round() as u16;

                present_wipe_frame(engine, &from_frames, &to_frames, tt, output)?;

                frames += 1;
                let next = start + frame_dt * frames;
                let now2 = Instant::now();
                if next > now2 && next < start + duration {
                    std::thread::sleep(next - now2);
                }
            }

            // Final present + persist cache + state
            let qh = engine.qh.clone();
            let mut any_final = false;

            for si in 0..engine.surfaces.len() {
                if !wayland::surface_usable(engine, si) {
                    continue;
                }
                let matches = {
                    let s = &engine.surfaces[si];
                    surface_matches_output_surface(s, output)
                };
                if !matches {
                    continue;
                }

                let Some(finalf) = to_frames[si].as_ref() else { continue };

                wayland::wait_for_free_buffer_idx(engine, si)?;
                {
                    let s = &mut engine.surfaces[si];
                    wayland::paint_frame_u32(s, finalf);
                    wayland::commit_surface(&qh, s, si);

                    s.last_colour = bg;
                    s.has_image = true;
                    s.last_frame = Some(Arc::clone(finalf));
                }

                let (sw, sh) = {
                    let s2 = &engine.surfaces[si];
                    (s2.width, s2.height)
                };
                let _ = crate::wallpaper::cache::store_last_frame(si, sw, sh, finalf);

                any_final = true;
            }

            if !any_final {
                bail!("no usable outputs to present wipe image (selected output not found?)");
            }

            engine._conn.flush().context("flush")?;
            engine.dispatch_pending()?;

            let elapsed = start.elapsed();
            el::info!("frames={frames} elapsed_ms={ms}", frames = frames, ms = elapsed.as_millis());

            Ok::<(), anyhow::Error>(())
        }
    )
}

/* ---------- immediate ---------- */

fn apply_image_immediate(
    engine: &mut Engine,
    src: &RgbaImage,
    mode: Mode,
    bg: Rgb,
    output: Option<&str>,
) -> Result<()> {
    el::scope!(
        "gesso.image.immediate",
        success = "applied",
        failure = "failed",
        aborted = "aborted",
        {
            el::info!(
                "mode={mode:?} bg={r},{g},{b} output={output}",
                r = bg.r,
                g = bg.g,
                b = bg.b,
                output = output.unwrap_or("(all)")
            );

            let qh = engine.qh.clone();
            let mut any = false;

            for si in 0..engine.surfaces.len() {
                if !wayland::surface_usable(engine, si) {
                    continue;
                }
                let matches = {
                    let s = &engine.surfaces[si];
                    surface_matches_output_surface(s, output)
                };
                if !matches {
                    continue;
                }

                wayland::wait_for_free_buffer_idx(engine, si)?;

                let (dw, dh) = {
                    let s = &engine.surfaces[si];
                    (s.width as usize, s.height as usize)
                };

                let frame: Arc<[u32]> = render_final_frame_u32(dw, dh, src, mode, bg).into();

                {
                    let s = &mut engine.surfaces[si];
                    wayland::paint_frame_u32(s, &frame);
                    wayland::commit_surface(&qh, s, si);

                    s.last_colour = bg;
                    s.has_image = true;
                    s.last_frame = Some(Arc::clone(&frame));
                }

                let (sw, sh) = {
                    let s = &engine.surfaces[si];
                    (s.width, s.height)
                };
                let _ = crate::wallpaper::cache::store_last_frame(si, sw, sh, &frame);

                any = true;
            }

            if !any {
                bail!("no usable outputs to render image (selected output not found?)");
            }

            engine._conn.flush().context("flush")?;
            engine.dispatch_pending()?;

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

            let img = image::open(path).with_context(|| format!("decode image: {}", path.display()))?;
            let rgba = img.to_rgba8();

            el::info!("loaded dimensions={w}x{h}", w = rgba.width(), h = rgba.height());

            Ok::<RgbaImage, anyhow::Error>(rgba)
        }
    )
}
