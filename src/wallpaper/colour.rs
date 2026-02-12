// Author: Dustin Pilgrim
// License: MIT

use anyhow::{Context, Result};
use eventline as el;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::spec::{Rgb, Transition, WipeFrom};
use crate::wallpaper::{
    paint::{paint_blend_frame_to_solid_fast, paint_wipe_frame_to_solid_fast},
    util::{ease_out_cubic, xrgb8888},
    wayland::{self, Engine},
};

#[inline]
fn surface_matches_output(engine: &Engine, i: usize, output: Option<&str>) -> bool {
    let Some(name) = engine.surfaces[i].output_name.as_deref() else {
        return output.is_none();
    };
    match output {
        None => true,
        Some(want) => name == want,
    }
}

#[inline]
fn rgb_fmt(c: Rgb) -> String {
    format!("{},{},{}", c.r, c.g, c.b)
}

#[inline]
fn kind_name(kind: Transition) -> &'static str {
    match kind {
        Transition::None => "none",
        Transition::Fade => "fade",
        Transition::Wipe => "wipe",
    }
}

/* ---------- public API ---------- */

pub fn apply_solid(engine: &mut Engine, target: Rgb) -> Result<()> {
    apply_solid_on(engine, target, None)
}

pub fn fade_to(engine: &mut Engine, target: Rgb, duration_ms: u32) -> Result<()> {
    fade_to_on(engine, target, duration_ms, None)
}

/* ---------- per-output ---------- */

pub fn apply_solid_on(engine: &mut Engine, target: Rgb, output: Option<&str>) -> Result<()> {
    let out = output.unwrap_or("(all)");

    el::scope!(
        "gesso.colour.apply_solid",
        success = "applied",
        failure = "failed",
        aborted = "aborted",
        {
            el::info!("begin output={out} rgb={rgb}", out = out, rgb = rgb_fmt(target));

            let qh = engine.qh.clone();
            let mut matched = 0usize;
            let mut applied = 0usize;
            let mut skipped = 0usize;

            for i in 0..engine.surfaces.len() {
                if !wayland::surface_usable(engine, i) {
                    skipped += 1;
                    continue;
                }
                if !surface_matches_output(engine, i, output) {
                    continue;
                }
                matched += 1;

                {
                    let s = &mut engine.surfaces[i];
                    if s.buffers.current_is_busy() {
                        s.buffers.swap_to_free();
                    }
                    if s.buffers.current_is_busy() {
                        let name = s.output_name.as_deref().unwrap_or("(unknown)");
                        el::warn!(
                            "skip surface={i} output={name} reason=all_buffers_busy",
                            i = i,
                            name = name
                        );
                        skipped += 1;
                        continue;
                    }
                }

                let (w, h, name) = {
                    let s = &engine.surfaces[i];
                    (
                        s.width as usize,
                        s.height as usize,
                        s.output_name.clone().unwrap_or_else(|| "(unknown)".into()),
                    )
                };

                if w == 0 || h == 0 {
                    skipped += 1;
                    continue;
                }

                let px = xrgb8888(target);
                let frame: Arc<[u32]> = vec![px; w * h].into();

                {
                    let s = &mut engine.surfaces[i];
                    wayland::paint_frame_u32(s, &frame);
                    wayland::commit_surface(&qh, s, i);

                    s.last_colour = target;
                    s.has_image = false;
                    s.last_frame = Some(frame);
                }

                applied += 1;

                el::debug!(
                    "applied surface={i} output={name} size={w}x{h}",
                    i = i,
                    name = name,
                    w = w,
                    h = h
                );
            }

            if applied > 0 {
                // Flush once. Event pumping happens inside wait_for_free_buffer_idx during animations.
                engine._conn.flush().context("flush")?;
            }

            el::info!(
                "done output={out} matched={matched} applied={applied} skipped={skipped}",
                out = out,
                matched = matched,
                applied = applied,
                skipped = skipped
            );

            Ok::<(), anyhow::Error>(())
        }
    )?;

    Ok(())
}

pub fn fade_to_on(
    engine: &mut Engine,
    target: Rgb,
    duration_ms: u32,
    output: Option<&str>,
) -> Result<()> {
    transition_to_on(engine, target, Transition::Fade, duration_ms, output, WipeFrom::Left)
}

pub fn wipe_to_on(
    engine: &mut Engine,
    target: Rgb,
    duration_ms: u32,
    output: Option<&str>,
) -> Result<()> {
    transition_to_on(engine, target, Transition::Wipe, duration_ms, output, WipeFrom::Left)
}

pub fn wipe_to_on_from(
    engine: &mut Engine,
    target: Rgb,
    duration_ms: u32,
    output: Option<&str>,
    wipe_from: WipeFrom,
) -> Result<()> {
    transition_to_on(engine, target, Transition::Wipe, duration_ms, output, wipe_from)
}

/* ---------- single implementation: fade + wipe ---------- */

pub fn transition_to_on(
    engine: &mut Engine,
    target: Rgb,
    kind: Transition,
    duration_ms: u32,
    output: Option<&str>,
    wipe_from: WipeFrom,
) -> Result<()> {
    if kind == Transition::None {
        return apply_solid_on(engine, target, output);
    }

    let out = output.unwrap_or("(all)");
    let duration_ms = duration_ms.max(16);
    let duration = Duration::from_millis(duration_ms as u64);

    el::scope!(
        "gesso.colour.transition",
        success = "done",
        failure = "failed",
        aborted = "aborted",
        {
            el::info!(
                "begin kind={kind} output={out} rgb={rgb} duration_ms={ms} wipe_from={wf:?}",
                kind = kind_name(kind),
                out = out,
                rgb = rgb_fmt(target),
                ms = duration_ms,
                wf = wipe_from
            );

            // Capture "from" frames.
            let mut from_frames: Vec<Option<Arc<[u32]>>> = vec![None; engine.surfaces.len()];
            let mut any_selected = 0usize;

            for i in 0..engine.surfaces.len() {
                let s = &engine.surfaces[i];
                if !wayland::surface_usable(engine, i) {
                    continue;
                }
                if !surface_matches_output(engine, i, output) {
                    continue;
                }
                any_selected += 1;

                if let Some(f) = s.last_frame.as_ref() {
                    from_frames[i] = Some(Arc::clone(f));
                } else {
                    let px = xrgb8888(s.last_colour);
                    from_frames[i] = Some(vec![px; (s.width * s.height) as usize].into());
                }
            }

            if any_selected == 0 {
                el::warn!("no selected surfaces output={out}", out = out);
                return Ok::<(), anyhow::Error>(());
            }

            // No-op detection WITHOUT allocating "to frames".
            let to_px = xrgb8888(target);
            let mut needs = false;

            for i in 0..engine.surfaces.len() {
                if !wayland::surface_usable(engine, i) {
                    continue;
                }
                if !surface_matches_output(engine, i, output) {
                    continue;
                }

                if let Some(fromf) = from_frames[i].as_ref() {
                    // Cheap mismatch scan. If any pixel differs, we need a transition.
                    if fromf.iter().any(|&px| px != to_px) {
                        needs = true;
                        break;
                    }
                } else {
                    let s = &engine.surfaces[i];
                    if xrgb8888(s.last_colour) != to_px {
                        needs = true;
                        break;
                    }
                }
            }

            if !needs {
                el::info!("no-op transition");
                for i in 0..engine.surfaces.len() {
                    if surface_matches_output(engine, i, output) {
                        let s = &mut engine.surfaces[i];
                        s.last_colour = target;
                        s.has_image = false;
                    }
                }
                return Ok::<(), anyhow::Error>(());
            }

            let qh = engine.qh.clone();
            let start = Instant::now();
            let mut frames: u32 = 0;

            // Render until duration elapsed.
            // Pacing is compositor-driven via frame callbacks + buffer release in wayland::wait_for_free_buffer_idx.
            loop {
                let now = Instant::now();
                let elapsed = now.saturating_duration_since(start);
                if elapsed >= duration {
                    break;
                }

                let raw = (elapsed.as_secs_f32() / duration.as_secs_f32()).min(1.0);
                let t = ease_out_cubic(raw);
                let tt = (t * 256.0).round() as u16; // monotonic 0..256

                for i in 0..engine.surfaces.len() {
                    if !wayland::surface_usable(engine, i) {
                        continue;
                    }
                    if !surface_matches_output(engine, i, output) {
                        continue;
                    }

                    let Some(fromf) = from_frames[i].as_ref() else { continue };

                    // This call now does the *blocking cadence*:
                    // - wait for free buffer (release)
                    // - if callbacks work, also wait for frame_done
                    wayland::wait_for_free_buffer_idx(engine, i)?;

                    let s = &mut engine.surfaces[i];
                    match kind {
                        Transition::Wipe => paint_wipe_frame_to_solid_fast(s, fromf, to_px, tt, wipe_from),
                        Transition::Fade => paint_blend_frame_to_solid_fast(s, fromf, to_px, tt),
                        Transition::None => unreachable!(),
                    }

                    wayland::commit_surface(&qh, s, i);
                }

                engine._conn.flush().context("flush")?;
                frames += 1;
            }

            // Finalize to exact target (one allocation per surface at end only).
            for i in 0..engine.surfaces.len() {
                if !wayland::surface_usable(engine, i) {
                    continue;
                }
                if !surface_matches_output(engine, i, output) {
                    continue;
                }

                wayland::wait_for_free_buffer_idx(engine, i)?;
                let s = &mut engine.surfaces[i];

                let w = s.width as usize;
                let h = s.height as usize;
                if w == 0 || h == 0 {
                    continue;
                }

                let finalf: Arc<[u32]> = vec![to_px; w * h].into();
                wayland::paint_frame_u32(s, &finalf);
                wayland::commit_surface(&qh, s, i);

                s.last_colour = target;
                s.has_image = false;
                s.last_frame = Some(finalf);
            }

            engine._conn.flush().context("flush")?;
            // One dispatch at the end is OK (helps deliver final release/done quickly).
            let _ = engine.dispatch_pending();

            el::info!("done kind={kind} frames={frames}", kind = kind_name(kind), frames = frames);
            Ok::<(), anyhow::Error>(())
        }
    )?;

    Ok(())
}
