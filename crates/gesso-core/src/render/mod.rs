pub mod surface;
pub mod transition;
pub mod engine;
pub mod scale;

pub use surface::Surface;
pub use transition::{Transition, WaveDir};
pub use engine::{RenderEngine, Target, OldSnapshot};
pub use scale::{scale_image, ScaleMode};

use crate::Colour;

//
// Quality
//

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeQuality {
    /// Fast sRGB blend — can show banding/stepping on dark transitions.
    Fast,
    /// Linear-light blend via LUT — perceptually smoother (NOT used for Fade anymore).
    Perceptual,
}

//
// Render Context
//

pub struct RenderCtx {
    pub quality: FadeQuality,

    // Lazily allocated LUTs (so idle doesn't permanently pin ~64KB)
    pub(crate) to_linear: Option<Box<[u16; 256]>>,
    pub(crate) to_srgb:   Option<Box<[u8; 65536]>>,

    lut_ready: bool,

    // Scratch (avoid per-frame allocs)
    pub(crate) dx2: Vec<f32>,
}

impl Default for RenderCtx {
    fn default() -> Self {
        Self {
            quality: FadeQuality::Fast,
            to_linear: None,
            to_srgb: None,
            lut_ready: false,
            dx2: Vec::new(),
        }
    }
}

impl RenderCtx {
    pub fn ensure_luts(&mut self) {
        if self.lut_ready {
            return;
        }

        let mut to_linear = Box::new([0u16; 256]);
        let mut to_srgb   = Box::new([0u8; 65536]);

        // sRGB -> linear (16-bit)
        for i in 0..256usize {
            let s = i as f32 / 255.0;
            let lin = if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            };
            to_linear[i] = (lin * 65535.0).round().clamp(0.0, 65535.0) as u16;
        }

        // linear -> sRGB (8-bit)
        for i in 0..65536usize {
            let lin = i as f32 / 65535.0;
            let s = if lin <= 0.0031308 {
                lin * 12.92
            } else {
                1.055 * lin.powf(1.0 / 2.4) - 0.055
            };
            to_srgb[i] = (s * 255.0).round().clamp(0.0, 255.0) as u8;
        }

        self.to_linear = Some(to_linear);
        self.to_srgb   = Some(to_srgb);
        self.lut_ready = true;
    }

    #[inline]
    #[allow(dead_code)]
    fn fill_dx2(&mut self, w: usize, pivot_x: f32) {
        self.dx2.resize(w, 0.0);
        for x in 0..w {
            let dx = x as f32 - pivot_x;
            self.dx2[x] = dx * dx;
        }
    }
}

//
// Entry point — `t` is already eased in engine.rs, in [0, 1].
//

pub fn render_transition(
    ctx: &mut RenderCtx,
    transition: Transition,
    old: Option<Surface<'_>>,
    old_colour: Option<Colour>,
    new: Option<Surface<'_>>,
    new_colour: Option<Colour>,
    out_width: u32,
    out_height: u32,
    out_stride: usize,
    out: &mut [u8],
    t: f32,
) {
    let t = t.clamp(0.0, 1.0);

    match transition {
        Transition::None => {
            blit_new(new, new_colour, out_width, out_height, out_stride, out);
        }

        // Drop: hard-edged expanding circle (no blending).
        Transition::Drop { .. } => {
            drop_hard(old, old_colour, new, new_colour, out_width, out_height, out_stride, out, t);
        }

        // Fade: EXACTLY like old gesso.
        Transition::Fade { .. } => {
            fade_old_style(old, old_colour, new, new_colour, out_width, out_height, out_stride, out, t);
        }

        Transition::Wave {
            dir,
            softness_px,
            amplitude_px,
            wavelength_px,
            ..
        } => match ctx.quality {
            FadeQuality::Fast => wave_fast(
                old,
                old_colour,
                new,
                new_colour,
                out_width,
                out_height,
                out_stride,
                out,
                t,
                dir,
                softness_px,
                amplitude_px,
                wavelength_px,
            ),
            FadeQuality::Perceptual => {
                ctx.ensure_luts();
                wave_lut(
                    ctx,
                    old,
                    old_colour,
                    new,
                    new_colour,
                    out_width,
                    out_height,
                    out_stride,
                    out,
                    t,
                    dir,
                    softness_px,
                    amplitude_px,
                    wavelength_px,
                )
            }
        },
    }
}

//
// Helpers
//

#[inline]
fn colour_u32(c: Colour) -> u32 {
    (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32
}

#[inline]
fn as_u32_slice(buf: &[u8]) -> &[u32] {
    debug_assert_eq!(buf.len() % 4, 0);
    unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u32, buf.len() / 4) }
}

#[inline]
fn as_u32_slice_mut(buf: &mut [u8]) -> &mut [u32] {
    debug_assert_eq!(buf.len() % 4, 0);
    unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u32, buf.len() / 4) }
}

#[inline(always)]
fn clamp_i32(x: i32, lo: i32, hi: i32) -> i32 {
    if x < lo { lo } else if x > hi { hi } else { x }
}

#[inline(always)]
fn tau() -> f32 {
    std::f32::consts::PI * 2.0
}

#[inline(always)]
fn trailing_damp(t: f32, start: f32) -> f32 {
    if t <= start {
        return 1.0;
    }
    let u = 1.0 - (t - start) / (1.0 - start); // 1..0 linear
    u.clamp(0.0, 1.0)
}

//
// Blit
//

fn blit_new(
    new: Option<Surface<'_>>,
    new_colour: Option<Colour>,
    w: u32,
    h: u32,
    stride: usize,
    out: &mut [u8],
) {
    if let Some(col) = new_colour {
        let px = colour_u32(col);
        as_u32_slice_mut(out)[..w as usize * h as usize].fill(px);
        return;
    }

    if let Some(n) = new {
        if stride == n.stride && stride == w as usize * 4 {
            out.copy_from_slice(n.data);
        } else {
            for y in 0..h {
                let src = n.row(y);
                let dst = &mut out[y as usize * stride..][..stride];
                dst.copy_from_slice(src);
            }
        }
        return;
    }

    as_u32_slice_mut(out)[..w as usize * h as usize].fill(0);
}

fn blit_old(
    old: Option<Surface<'_>>,
    old_colour: Option<Colour>,
    w: u32,
    h: u32,
    stride: usize,
    out: &mut [u8],
) {
    if let Some(col) = old_colour {
        let px = colour_u32(col);
        as_u32_slice_mut(out)[..w as usize * h as usize].fill(px);
        return;
    }

    if let Some(o) = old {
        if stride == o.stride && stride == w as usize * 4 {
            out.copy_from_slice(o.data);
        } else {
            for y in 0..h {
                let src = o.row(y);
                let dst = &mut out[y as usize * stride..][..stride];
                dst.copy_from_slice(src);
            }
        }
    } else {
        as_u32_slice_mut(out)[..w as usize * h as usize].fill(0);
    }
}

//
// Blend (old gesso style integer blend)
//

#[inline(always)]
fn blend_u32_xrgb(a: u32, b: u32, tt: u32, inv: u32) -> u32 {
    let rb = (((a & 0x00FF00FF) * inv) + ((b & 0x00FF00FF) * tt)) >> 8;
    let g  = (((a & 0x0000FF00) * inv) + ((b & 0x0000FF00) * tt)) >> 8;
    (rb & 0x00FF00FF) | (g & 0x0000FF00)
}

//
// ─── DROP (hard edge, center circle expanding) ────────────────────────────────
//

fn drop_hard(
    old: Option<Surface<'_>>,
    old_colour: Option<Colour>,
    new: Option<Surface<'_>>,
    new_colour: Option<Colour>,
    w: u32,
    h: u32,
    stride: usize,
    out: &mut [u8],
    t: f32,
) {
    if t >= 0.999 {
        blit_new(new, new_colour, w, h, stride, out);
        return;
    }
    if t <= 0.0 {
        blit_old(old, old_colour, w, h, stride, out);
        return;
    }

    let w_usize = w as usize;

    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;

    let max_r = (cx * cx + cy * cy).sqrt();
    let r = t * max_r;
    let r2 = r * r;

    let col_px = new_colour.map(colour_u32);
    let old_px = old_colour.map(colour_u32);

    for y in 0..h {
        let dy = y as f32 - cy;
        let dy2 = dy * dy;

        let o_row = old.map(|s| as_u32_slice(s.row(y)));
        let n_row = new.map(|s| as_u32_slice(s.row(y)));

        let dst_row = &mut out[y as usize * stride..][..stride];
        let dst = as_u32_slice_mut(dst_row);

        if dy2 >= r2 {
            // all old
            if let Some(or) = o_row {
                dst.copy_from_slice(or);
            } else if let Some(opx) = old_px {
                dst.fill(opx);
            } else {
                dst.fill(0);
            }
            continue;
        }

        let half = (r2 - dy2).max(0.0).sqrt();
        let l = clamp_i32((cx - half).ceil() as i32, 0, w as i32) as usize;
        let rr = clamp_i32((cx + half).floor() as i32, 0, w as i32) as usize;

        // left old
        if l > 0 {
            if let Some(or) = o_row {
                dst[..l].copy_from_slice(&or[..l]);
            } else if let Some(opx) = old_px {
                dst[..l].fill(opx);
            } else {
                dst[..l].fill(0);
            }
        }
        // middle new
        if rr > l {
            if let Some(c) = col_px {
                dst[l..rr].fill(c);
            } else if let Some(nr) = n_row {
                dst[l..rr].copy_from_slice(&nr[l..rr]);
            } else {
                dst[l..rr].fill(0);
            }
        }
        // right old
        if rr < w_usize {
            if let Some(or) = o_row {
                dst[rr..].copy_from_slice(&or[rr..]);
            } else if let Some(opx) = old_px {
                dst[rr..].fill(opx);
            } else {
                dst[rr..].fill(0);
            }
        }
    }
}

//
// ─── FADE (old gesso behavior) ────────────────────────────────────────────────
//

fn fade_old_style(
    old: Option<Surface<'_>>,
    old_colour: Option<Colour>,
    new: Option<Surface<'_>>,
    new_colour: Option<Colour>,
    w: u32,
    h: u32,
    stride: usize,
    out: &mut [u8],
    t: f32,
) {
    if t >= 0.999 {
        blit_new(new, new_colour, w, h, stride, out);
        return;
    }
    if t <= 0.0 {
        blit_old(old, old_colour, w, h, stride, out);
        return;
    }

    let tt = (t * 256.0).round().clamp(0.0, 256.0) as u32;
    let inv = 256 - tt;

    let new_px = new_colour.map(colour_u32);
    let old_px = old_colour.map(colour_u32);

    match (old, old_px, new, new_px) {
        (Some(o), _, Some(n), None) => {
            for y in 0..h {
                let or = as_u32_slice(o.row(y));
                let nr = as_u32_slice(n.row(y));
                let dst = as_u32_slice_mut(&mut out[y as usize * stride..][..stride]);
                for x in 0..w as usize {
                    dst[x] = blend_u32_xrgb(or[x], nr[x], tt, inv);
                }
            }
        }
        (Some(o), _, Some(_n), Some(npx)) => {
            for y in 0..h {
                let or = as_u32_slice(o.row(y));
                let dst = as_u32_slice_mut(&mut out[y as usize * stride..][..stride]);
                for x in 0..w as usize {
                    dst[x] = blend_u32_xrgb(or[x], npx, tt, inv);
                }
            }
        }

        // OLD is a solid colour (no old surface)
        (None, Some(opx), Some(n), None) => {
            for y in 0..h {
                let nr = as_u32_slice(n.row(y));
                let dst = as_u32_slice_mut(&mut out[y as usize * stride..][..stride]);
                for x in 0..w as usize {
                    dst[x] = blend_u32_xrgb(opx, nr[x], tt, inv);
                }
            }
        }
        (None, Some(opx), Some(_n), Some(npx)) => {
            for y in 0..h {
                let dst = as_u32_slice_mut(&mut out[y as usize * stride..][..stride]);
                for x in 0..w as usize {
                    dst[x] = blend_u32_xrgb(opx, npx, tt, inv);
                }
            }
        }
        (None, Some(opx), None, Some(npx)) => {
            for y in 0..h {
                let dst = as_u32_slice_mut(&mut out[y as usize * stride..][..stride]);
                for x in 0..w as usize {
                    dst[x] = blend_u32_xrgb(opx, npx, tt, inv);
                }
            }
        }
        (None, Some(opx), None, None) => {
            for y in 0..h {
                let dst = as_u32_slice_mut(&mut out[y as usize * stride..][..stride]);
                for x in 0..w as usize {
                    dst[x] = blend_u32_xrgb(opx, 0, tt, inv);
                }
            }
        }

        // OLD is absent/black (no old surface, no old colour)
        (None, None, Some(n), None) => {
            for y in 0..h {
                let nr = as_u32_slice(n.row(y));
                let dst = as_u32_slice_mut(&mut out[y as usize * stride..][..stride]);
                for x in 0..w as usize {
                    dst[x] = blend_u32_xrgb(0, nr[x], tt, inv);
                }
            }
        }
        (None, None, Some(_n), Some(npx)) => {
            for y in 0..h {
                let dst = as_u32_slice_mut(&mut out[y as usize * stride..][..stride]);
                for x in 0..w as usize {
                    dst[x] = blend_u32_xrgb(0, npx, tt, inv);
                }
            }
        }
        (Some(o), _, None, Some(npx)) => {
            for y in 0..h {
                let or = as_u32_slice(o.row(y));
                let dst = as_u32_slice_mut(&mut out[y as usize * stride..][..stride]);
                for x in 0..w as usize {
                    dst[x] = blend_u32_xrgb(or[x], npx, tt, inv);
                }
            }
        }
        (Some(o), _, None, None) => {
            for y in 0..h {
                let or = as_u32_slice(o.row(y));
                let dst = as_u32_slice_mut(&mut out[y as usize * stride..][..stride]);
                for x in 0..w as usize {
                    dst[x] = blend_u32_xrgb(or[x], 0, tt, inv);
                }
            }
        }

        // OLD is absent/black AND NEW is a solid colour (no new surface)
        (None, None, None, Some(npx)) => {
            for y in 0..h {
                let dst = as_u32_slice_mut(&mut out[y as usize * stride..][..stride]);
                for x in 0..w as usize {
                    dst[x] = blend_u32_xrgb(0, npx, tt, inv);
                }
            }
        }

        // Nothing
        (None, None, None, None) => {
            as_u32_slice_mut(out)[..w as usize * h as usize].fill(0);
        }
    }
}

//
// ─── WAVE ─────────────────────────────────────────────────────────────────────
//

fn wave_fast(
    old: Option<Surface<'_>>,
    old_colour: Option<Colour>,
    new: Option<Surface<'_>>,
    new_colour: Option<Colour>,
    w: u32,
    h: u32,
    stride: usize,
    out: &mut [u8],
    t: f32,
    dir: WaveDir,
    softness_px: u16,
    amplitude_px: u16,
    wavelength_px: u16,
) {
    wave_impl(None, old, old_colour, new, new_colour, w, h, stride, out, t, dir, softness_px, amplitude_px, wavelength_px)
}

fn wave_lut(
    ctx: &RenderCtx,
    old: Option<Surface<'_>>,
    old_colour: Option<Colour>,
    new: Option<Surface<'_>>,
    new_colour: Option<Colour>,
    w: u32,
    h: u32,
    stride: usize,
    out: &mut [u8],
    t: f32,
    dir: WaveDir,
    softness_px: u16,
    amplitude_px: u16,
    wavelength_px: u16,
) {
    wave_impl(Some(ctx), old, old_colour, new, new_colour, w, h, stride, out, t, dir, softness_px, amplitude_px, wavelength_px)
}

#[inline(always)]
fn blend_lut(ctx: &RenderCtx, a: u32, b: u32, tt: u32, inv: u32) -> u32 {
    let tl = ctx.to_linear.as_ref().expect("LUTs not initialized");
    let ts = ctx.to_srgb.as_ref().expect("LUTs not initialized");

    let ar = ((a >> 16) & 0xFF) as usize;
    let ag = ((a >> 8) & 0xFF) as usize;
    let ab = (a & 0xFF) as usize;
    let br = ((b >> 16) & 0xFF) as usize;
    let bg = ((b >> 8) & 0xFF) as usize;
    let bb = (b & 0xFF) as usize;

    let r = ((tl[ar] as u32 * inv) + (tl[br] as u32 * tt)) >> 8;
    let g = ((tl[ag] as u32 * inv) + (tl[bg] as u32 * tt)) >> 8;
    let b = ((tl[ab] as u32 * inv) + (tl[bb] as u32 * tt)) >> 8;

    (ts[r as usize] as u32) << 16
        | (ts[g as usize] as u32) << 8
        | (ts[b as usize] as u32)
}

fn wave_impl(
    ctx: Option<&RenderCtx>,
    old: Option<Surface<'_>>,
    old_colour: Option<Colour>,
    new: Option<Surface<'_>>,
    new_colour: Option<Colour>,
    w: u32,
    h: u32,
    stride: usize,
    out: &mut [u8],
    t: f32,
    dir: WaveDir,
    softness_px: u16,
    amplitude_px: u16,
    wavelength_px: u16,
) {
    if t >= 0.999 {
        blit_new(new, new_colour, w, h, stride, out);
        return;
    }
    if t <= 0.0 {
        blit_old(old, old_colour, w, h, stride, out);
        return;
    }

    let w_usize = w as usize;
    let wl = wavelength_px.max(1) as f32;
    let new_px = new_colour.map(colour_u32);
    let old_px = old_colour.map(colour_u32);

    let base = t * (w as f32);
    let damp = trailing_damp(t, 0.75);

    let amp = amplitude_px as f32 * damp;
    let effective_soft = ((softness_px as f32) * damp).round() as i32;

    for y in 0..h {
        let dst = as_u32_slice_mut(&mut out[y as usize * stride..][..stride]);

        let o_row = old.map(|s| as_u32_slice(s.row(y)));
        let n_row = new.map(|s| as_u32_slice(s.row(y)));

        let phase = (y as f32 / wl) * tau() + t * tau();
        let wobble = phase.sin() * amp;

        let cut_f = base + wobble;
        let cut = clamp_i32(cut_f.round() as i32, 0, w as i32);

        if effective_soft <= 0 {
            let cols = cut as usize;
            match dir {
                WaveDir::Left => {
                    // new left, old right
                    if cols > 0 {
                        if let Some(c) = new_px {
                            dst[..cols].fill(c);
                        } else if let Some(nr) = n_row {
                            dst[..cols].copy_from_slice(&nr[..cols]);
                        } else {
                            dst[..cols].fill(0);
                        }
                    }
                    if cols < w_usize {
                        if let Some(or) = o_row {
                            dst[cols..].copy_from_slice(&or[cols..]);
                        } else if let Some(opx) = old_px {
                            dst[cols..].fill(opx);
                        } else {
                            dst[cols..].fill(0);
                        }
                    }
                }
                WaveDir::Right => {
                    let start = w_usize.saturating_sub(cols);
                    if start > 0 {
                        if let Some(or) = o_row {
                            dst[..start].copy_from_slice(&or[..start]);
                        } else if let Some(opx) = old_px {
                            dst[..start].fill(opx);
                        } else {
                            dst[..start].fill(0);
                        }
                    }
                    if start < w_usize {
                        if let Some(c) = new_px {
                            dst[start..].fill(c);
                        } else if let Some(nr) = n_row {
                            dst[start..].copy_from_slice(&nr[start..]);
                        } else {
                            dst[start..].fill(0);
                        }
                    }
                }
            }
            continue;
        }

        let left = clamp_i32(cut - effective_soft, 0, w as i32) as usize;
        let right = clamp_i32(cut + effective_soft, 0, w as i32) as usize;

        #[inline(always)]
        fn alpha_0_to_256(i: usize, len: usize) -> u32 {
            if len <= 1 { 256 } else { ((i * 256) / (len - 1)).min(256) as u32 }
        }

        match dir {
            WaveDir::Left => {
                // new before band
                if left > 0 {
                    if let Some(c) = new_px {
                        dst[..left].fill(c);
                    } else if let Some(nr) = n_row {
                        dst[..left].copy_from_slice(&nr[..left]);
                    } else {
                        dst[..left].fill(0);
                    }
                }
                // old after band
                if right < w_usize {
                    if let Some(or) = o_row {
                        dst[right..].copy_from_slice(&or[right..]);
                    } else if let Some(opx) = old_px {
                        dst[right..].fill(opx);
                    } else {
                        dst[right..].fill(0);
                    }
                }

                let band_len = right.saturating_sub(left);
                for i in 0..band_len {
                    let x = left + i;

                    let a = 256 - alpha_0_to_256(i, band_len);
                    let inv = 256 - a;

                    let o = if let Some(or) = o_row {
                        or[x]
                    } else {
                        old_px.unwrap_or(0)
                    };

                    let npx = new_px.unwrap_or_else(|| n_row.map(|r| r[x]).unwrap_or(0));

                    dst[x] = if let Some(c) = ctx {
                        blend_lut(c, o, npx, a, inv)
                    } else {
                        blend_u32_xrgb(o, npx, a, inv)
                    };
                }
            }

            WaveDir::Right => {
                let old_end = w_usize.saturating_sub(right);
                let new_start = w_usize.saturating_sub(left);

                if old_end > 0 {
                    if let Some(or) = o_row {
                        dst[..old_end].copy_from_slice(&or[..old_end]);
                    } else if let Some(opx) = old_px {
                        dst[..old_end].fill(opx);
                    } else {
                        dst[..old_end].fill(0);
                    }
                }
                if new_start < w_usize {
                    if let Some(c) = new_px {
                        dst[new_start..].fill(c);
                    } else if let Some(nr) = n_row {
                        dst[new_start..].copy_from_slice(&nr[new_start..]);
                    } else {
                        dst[new_start..].fill(0);
                    }
                }

                let band_len = new_start.saturating_sub(old_end);
                for i in 0..band_len {
                    let x = old_end + i;
                    let a = alpha_0_to_256(i, band_len);
                    let inv = 256 - a;

                    let o = if let Some(or) = o_row {
                        or[x]
                    } else {
                        old_px.unwrap_or(0)
                    };

                    let npx = new_px.unwrap_or_else(|| n_row.map(|r| r[x]).unwrap_or(0));

                    dst[x] = if let Some(c) = ctx {
                        blend_lut(c, o, npx, a, inv)
                    } else {
                        blend_u32_xrgb(o, npx, a, inv)
                    };
                }
            }
        }
    }
}
