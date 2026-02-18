// Author: Dustin Pilgrim
// License: MIT

use anyhow::{bail, Context, Result};
use eventline as el;
use image::RgbaImage;
use std::collections::HashMap;
use std::sync::Arc;

use crate::spec::{Mode, Rgb, Spec, Transition, WipeFrom};
use crate::wallpaper::{
    animations,
    render::render_final_frame_u32,
    wayland::{self, Engine},
};

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
                    let matches = match output {
                        None => true,
                        Some(want) => s.output_name.as_deref() == Some(want),
                    };
                    if !matches {
                        continue;
                    }
                    wayland::ensure_buffers_for_surface_indexed(&qh, &shm, si, s)?;
                    buffer_count += 1;
                }

                el::debug!("ensured_buffers count={count}", count = buffer_count);
            }

            // Build selected surface list once and re-use everywhere.
            let sel = animations::selected_surfaces(engine, output);
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
                        let Some(frame) = cached_frames[si].as_ref() else {
                            continue;
                        };

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

/// Render TO frames once per unique (w, h), reusing across identical output sizes.
/// Common on multi-monitor setups with duplicate resolutions.
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

        let frame: Arc<[u32]> =
            render_final_frame_u32(w as usize, h as usize, src, mode, bg).into();
        reuse.insert((w, h), Arc::clone(&frame));
        to_frames[si] = Some(frame);
    }

    to_frames
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

            let from_frames = animations::capture_from_frames(engine, sel);

            animations::present_blend_frame(engine, sel, &from_frames, to_frames, 0)?;

            let frames = animations::animate(engine, sel, duration, |eng, tt| {
                animations::present_blend_frame(eng, sel, &from_frames, to_frames, tt)
            })?;

            // Finalize exact TO + state.
            let qh = engine.qh.clone();
            for &si in sel {
                let Some(finalf) = to_frames[si].as_ref() else {
                    continue;
                };

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
                fps = animations::TARGET_FPS
            );

            if sel.is_empty() {
                bail!("no usable outputs to fade image (selected output not found?)");
            }

            let from_frames = animations::capture_from_frames(engine, sel);

            el::debug!("rendering target frames");
            let to_frames = render_to_frames(engine, sel, src, mode, bg);

            animations::present_blend_frame(engine, sel, &from_frames, &to_frames, 0)?;

            el::debug!("starting animationsation");
            let frames = animations::animate(engine, sel, duration, |eng, tt| {
                animations::present_blend_frame(eng, sel, &from_frames, &to_frames, tt)
            })?;

            // Finalize exact TO + state + cache.
            let qh = engine.qh.clone();
            let mut any_final = false;

            for &si in sel {
                let Some(finalf) = to_frames[si].as_ref() else {
                    continue;
                };

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

            let from_frames = animations::capture_from_frames(engine, sel);

            animations::present_wipe_frame(engine, sel, &from_frames, to_frames, 0, wipe_from)?;

            let frames = animations::animate(engine, sel, duration, |eng, tt| {
                animations::present_wipe_frame(eng, sel, &from_frames, to_frames, tt, wipe_from)
            })?;

            // Finalize exact TO + state.
            let qh = engine.qh.clone();
            for &si in sel {
                let Some(finalf) = to_frames[si].as_ref() else {
                    continue;
                };

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
                fps = animations::TARGET_FPS
            );

            if sel.is_empty() {
                bail!("no usable outputs to wipe image (selected output not found?)");
            }

            let from_frames = animations::capture_from_frames(engine, sel);

            el::debug!("rendering target frames");
            let to_frames = render_to_frames(engine, sel, src, mode, bg);

            animations::present_wipe_frame(engine, sel, &from_frames, &to_frames, 0, wipe_from)?;

            el::debug!("starting animationsation");
            let frames = animations::animate(engine, sel, duration, |eng, tt| {
                animations::present_wipe_frame(eng, sel, &from_frames, &to_frames, tt, wipe_from)
            })?;

            // Finalize exact TO + state + cache.
            let qh = engine.qh.clone();
            let mut any_final = false;

            for &si in sel {
                let Some(finalf) = to_frames[si].as_ref() else {
                    continue;
                };

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

fn load_rgba(path: &std::path::Path) -> Result<RgbaImage> {
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
