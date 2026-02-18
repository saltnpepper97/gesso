// Author: Dustin Pilgrim
// License: MIT

use anyhow::{Context, Result};
use eventline as el;
use std::sync::Arc;

use crate::spec::{Rgb, Transition, WipeFrom};
use crate::wallpaper::{
    animations,
    wayland::{self, Engine},
};

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

                // Output filter.
                let matches = match output {
                    None => true,
                    Some(want) => engine.surfaces[i].output_name.as_deref() == Some(want),
                };
                if !matches {
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

                let px = target.xrgb8888();
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

            let sel = animations::selected_surfaces(engine, output);

            if sel.is_empty() {
                el::warn!("no selected surfaces output={out}", out = out);
                return Ok::<(), anyhow::Error>(());
            }

            // No-op detection: skip the transition if every pixel already matches target.
            let to_px = target.xrgb8888();
            let needs = sel.iter().any(|&si| {
                let s = &engine.surfaces[si];
                if let Some(f) = s.last_frame.as_ref() {
                    f.iter().any(|&px| px != to_px)
                } else {
                    s.last_colour.xrgb8888() != to_px
                }
            });

            if !needs {
                el::info!("no-op transition");
                for &si in &sel {
                    let s = &mut engine.surfaces[si];
                    s.last_colour = target;
                    s.has_image = false;
                }
                return Ok::<(), anyhow::Error>(());
            }

            let from_frames = animations::capture_from_frames(engine, &sel);

            // Build solid TO frames (one allocation per unique size).
            let mut to_frames: Vec<Option<Arc<[u32]>>> = vec![None; engine.surfaces.len()];
            for &si in &sel {
                let s = &engine.surfaces[si];
                let w = s.width as usize;
                let h = s.height as usize;
                if w > 0 && h > 0 {
                    to_frames[si] = Some(vec![to_px; w * h].into());
                }
            }

            // Prime with the starting frame before handing off to the animator.
            match kind {
                Transition::Fade => {
                    animations::present_blend_frame(engine, &sel, &from_frames, &to_frames, 0)?
                }
                Transition::Wipe => {
                    animations::present_wipe_frame(engine, &sel, &from_frames, &to_frames, 0, wipe_from)?
                }
                Transition::None => unreachable!(),
            }

            let frames = animations::animate(engine, &sel, duration_ms, |eng, tt| match kind {
                Transition::Fade => {
                    animations::present_blend_frame(eng, &sel, &from_frames, &to_frames, tt)
                }
                Transition::Wipe => {
                    animations::present_wipe_frame(eng, &sel, &from_frames, &to_frames, tt, wipe_from)
                }
                Transition::None => unreachable!(),
            })?;

            // Finalize exact target colour on all selected surfaces.
            let qh = engine.qh.clone();
            for &si in &sel {
                let Some(finalf) = to_frames[si].as_ref() else {
                    continue;
                };

                wayland::wait_for_free_buffer_idx(engine, si)?;
                let s = &mut engine.surfaces[si];
                wayland::paint_frame_u32(s, finalf);
                wayland::commit_surface(&qh, s, si);

                s.last_colour = target;
                s.has_image = false;
                s.last_frame = Some(Arc::clone(finalf));
            }

            engine._conn.flush().context("flush")?;
            let _ = engine.dispatch_pending();

            el::info!("done kind={kind} frames={frames}", kind = kind_name(kind), frames = frames);
            Ok::<(), anyhow::Error>(())
        }
    )?;

    Ok(())
}
