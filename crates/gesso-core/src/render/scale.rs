// Author: Dustin Pilgrim
// License: MIT

use crate::{Colour, DecodedImage};
use rayon::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleMode {
    Fill,
    Fit,
    Stretch,
    Center,
    Tile,
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Scale `src` to `(dst_w, dst_h)` using `mode`, letterboxing with `bg` where needed.
/// Output is XRGB8888, stride = dst_w * 4.
///
/// Allocates a new Vec for the result.  Prefer [`scale_image_into`] when you have
/// a pre-allocated destination to avoid the per-call allocation.
pub fn scale_image(
    src: &DecodedImage,
    dst_w: u32,
    dst_h: u32,
    mode: ScaleMode,
    bg: Colour,
) -> Vec<u8> {
    let mut out = vec![0u8; dst_w as usize * dst_h as usize * 4];
    scale_image_into(src, &mut out, dst_w, dst_h, mode, bg);
    out
}

/// Scale `src` into a caller-owned XRGB8888 buffer `dst` (must be dst_w * dst_h * 4 bytes).
///
/// This is the zero-allocation variant used by the GIF and WebP players so they
/// can reuse a single Arc-managed buffer across frames.
pub fn scale_image_into(
    src: &DecodedImage,
    dst: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    mode: ScaleMode,
    bg: Colour,
) {
    fill_bg(dst, dst_w, dst_h, bg);

    if dst_w == 0 || dst_h == 0 || src.width == 0 || src.height == 0 {
        return;
    }

    // Validate source buffer to prevent panics inside blit helpers.
    let min_stride = src.width as usize * 4;
    if src.stride < min_stride {
        debug_assert!(
            false,
            "DecodedImage stride too small: stride={} min={} ({}x{})",
            src.stride, min_stride, src.width, src.height
        );
        return;
    }
    let needed = src.stride.saturating_mul(src.height as usize);
    if src.pixels.len() < needed {
        debug_assert!(
            false,
            "DecodedImage pixels too small: len={} needed={} ({}x{} stride={})",
            src.pixels.len(), needed, src.width, src.height, src.stride
        );
        return;
    }

    let stride = dst_w as usize * 4;

    match mode {
        ScaleMode::Stretch => {
            blit_scaled(dst, dst_w, dst_h, stride, src, 0, 0, dst_w, dst_h);
        }
        ScaleMode::Fill => {
            let scale = f32::max(
                dst_w as f32 / src.width  as f32,
                dst_h as f32 / src.height as f32,
            );
            let scaled_w = (src.width  as f32 * scale).round() as u32;
            let scaled_h = (src.height as f32 * scale).round() as u32;
            let off_x = ((scaled_w as i32 - dst_w as i32) / 2).max(0) as u32;
            let off_y = ((scaled_h as i32 - dst_h as i32) / 2).max(0) as u32;
            blit_scaled_crop(dst, dst_w, dst_h, stride, src, scaled_w, scaled_h, off_x, off_y);
        }
        ScaleMode::Fit => {
            let scale = f32::min(
                dst_w as f32 / src.width  as f32,
                dst_h as f32 / src.height as f32,
            );
            let scaled_w = (src.width  as f32 * scale).round() as u32;
            let scaled_h = (src.height as f32 * scale).round() as u32;
            let x = ((dst_w as i32 - scaled_w as i32) / 2).max(0) as u32;
            let y = ((dst_h as i32 - scaled_h as i32) / 2).max(0) as u32;
            blit_scaled(dst, dst_w, dst_h, stride, src, x, y, scaled_w, scaled_h);
        }
        ScaleMode::Center => {
            if src.width <= dst_w && src.height <= dst_h {
                let x = ((dst_w as i32 - src.width  as i32) / 2).max(0) as u32;
                let y = ((dst_h as i32 - src.height as i32) / 2).max(0) as u32;
                blit_exact(dst, dst_w, dst_h, stride, src, x, y);
            } else {
                let off_x = ((src.width  as i32 - dst_w as i32) / 2).max(0) as u32;
                let off_y = ((src.height as i32 - dst_h as i32) / 2).max(0) as u32;
                blit_scaled_crop(dst, dst_w, dst_h, stride, src, src.width, src.height, off_x, off_y);
            }
        }
        ScaleMode::Tile => {
            tile(dst, dst_w, dst_h, stride, src);
        }
    }
}

/// Scale an RGBA compositing canvas (R,G,B,A packed, src_w×src_h×4) into an XRGB8888
/// destination buffer (B,G,R,0 packed, dst_w×dst_h×4).
///
/// Combines format conversion (RGBA→XRGB with alpha premultiplication) and scaling in
/// a single pass, eliminating the intermediate XRGB buffer that the old GIF path held
/// permanently in memory (~8 MB at 1080p).
///
/// Called from `GifFrameStream::next_frame_scaled_into`.
pub fn scale_rgba_canvas_into(
    rgba:  &[u8],
    src_w: u32,
    src_h: u32,
    dst:   &mut [u8],
    dst_w: u32,
    dst_h: u32,
    mode:  ScaleMode,
    bg:    Colour,
) {
    // Fast path: 1:1 with no letterboxing — avoid fill_bg + all mode logic.
    if src_w == dst_w && src_h == dst_h {
        let n = src_w as usize * src_h as usize;
        if rgba.len() >= n * 4 && dst.len() >= n * 4 {
            for i in 0..n {
                let s = i * 4;
                let a = rgba[s + 3] as u16;
                dst[s]     = ((rgba[s + 2] as u16 * a) / 255) as u8; // B
                dst[s + 1] = ((rgba[s + 1] as u16 * a) / 255) as u8; // G
                dst[s + 2] = ((rgba[s]     as u16 * a) / 255) as u8; // R
                dst[s + 3] = 0;
            }
            return;
        }
    }

    fill_bg(dst, dst_w, dst_h, bg);

    if dst_w == 0 || dst_h == 0 || src_w == 0 || src_h == 0 { return; }

    let expected_src = (src_w as usize).saturating_mul(src_h as usize).saturating_mul(4);
    if rgba.len() < expected_src { return; }

    let dst_stride = dst_w as usize * 4;

    match mode {
        ScaleMode::Stretch => {
            blit_scaled_rgba(rgba, src_w, src_h, dst, dst_w, dst_h, dst_stride, 0, 0, dst_w, dst_h);
        }
        ScaleMode::Fill => {
            let scale   = f32::max(dst_w as f32 / src_w as f32, dst_h as f32 / src_h as f32);
            let sc_w    = (src_w as f32 * scale).round() as u32;
            let sc_h    = (src_h as f32 * scale).round() as u32;
            let off_x   = ((sc_w as i32 - dst_w as i32) / 2).max(0) as u32;
            let off_y   = ((sc_h as i32 - dst_h as i32) / 2).max(0) as u32;
            blit_scaled_crop_rgba(rgba, src_w, src_h, dst, dst_w, dst_h, dst_stride, sc_w, sc_h, off_x, off_y);
        }
        ScaleMode::Fit => {
            let scale = f32::min(dst_w as f32 / src_w as f32, dst_h as f32 / src_h as f32);
            let sc_w  = (src_w as f32 * scale).round() as u32;
            let sc_h  = (src_h as f32 * scale).round() as u32;
            let x     = ((dst_w as i32 - sc_w as i32) / 2).max(0) as u32;
            let y     = ((dst_h as i32 - sc_h as i32) / 2).max(0) as u32;
            blit_scaled_rgba(rgba, src_w, src_h, dst, dst_w, dst_h, dst_stride, x, y, sc_w, sc_h);
        }
        ScaleMode::Center => {
            if src_w <= dst_w && src_h <= dst_h {
                let x = ((dst_w as i32 - src_w as i32) / 2).max(0) as u32;
                let y = ((dst_h as i32 - src_h as i32) / 2).max(0) as u32;
                blit_exact_rgba(rgba, src_w, src_h, dst, dst_w, dst_h, dst_stride, x, y);
            } else {
                let off_x = ((src_w as i32 - dst_w as i32) / 2).max(0) as u32;
                let off_y = ((src_h as i32 - dst_h as i32) / 2).max(0) as u32;
                blit_scaled_crop_rgba(rgba, src_w, src_h, dst, dst_w, dst_h, dst_stride, src_w, src_h, off_x, off_y);
            }
        }
        ScaleMode::Tile => {
            tile_rgba(rgba, src_w, src_h, dst, dst_w, dst_h, dst_stride);
        }
    }
}

// ── Shared helpers ──────────────────────────────────────────────────────────

#[inline]
fn fill_bg(out: &mut [u8], w: u32, h: u32, bg: Colour) {
    let px = (bg.r as u32) << 16 | (bg.g as u32) << 8 | bg.b as u32;
    let n  = w as usize * h as usize;
    if out.len() < n.saturating_mul(4) { return; }
    let dst = unsafe { std::slice::from_raw_parts_mut(out.as_mut_ptr() as *mut u32, n) };
    dst.fill(px);
}

/// Safely fetch a full source row slice.
#[inline]
fn src_row(src: &DecodedImage, sy: u32) -> Option<&[u8]> {
    let start = (sy as usize).saturating_mul(src.stride);
    let end   = start.saturating_add(src.stride);
    src.pixels.get(start..end)
}

// ── XRGB blit helpers ───────────────────────────────────────────────────────
//
// blit_scaled and blit_scaled_crop now use bilinear filtering with an optional
// box-filter pre-pass when downscaling by ≥2× in both axes (e.g. 4K→1080p).
// The intermediate Vec is allocated only when needed and freed immediately after
// the bilinear pass — no lingering allocation.

fn blit_scaled(
    out: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    dst_stride: usize,
    src: &DecodedImage,
    dx: u32,
    dy: u32,
    sw: u32,
    sh: u32,
) {
    if sw == 0 || sh == 0 { return; }

    let clip_w = sw.min(dst_w.saturating_sub(dx));
    let clip_h = sh.min(dst_h.saturating_sub(dy));
    if clip_w == 0 || clip_h == 0 { return; }

    blit_scaled_bilinear_xrgb(
        out, dst_stride,
        &src.pixels, src.width, src.height, src.stride,
        dx, dy, clip_w, clip_h, sw, sh,
    );
}

fn blit_scaled_crop(
    out: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    dst_stride: usize,
    src: &DecodedImage,
    scaled_w: u32,
    scaled_h: u32,
    off_x: u32,
    off_y: u32,
) {
    if scaled_w == 0 || scaled_h == 0 || dst_w == 0 || dst_h == 0 { return; }

    blit_scaled_crop_bilinear_xrgb(
        out, dst_w, dst_h, dst_stride,
        &src.pixels, src.width, src.height, src.stride,
        scaled_w, scaled_h, off_x, off_y,
    );
}

// ── XRGB bilinear core ───────────────────────────────────────────────────────
//
// Key optimisation vs the naive version:
//   • X sample coords are computed once before the Y loop and stored in a
//     small Vec<XSample> (~15 KB for 1920px).  The hot loop is pure integer
//     multiply-add with direct array indexing — no f32, no bounds checks.
//   • Channels are unrolled (B/G/R written explicitly) so the compiler can
//     emit 3 independent multiply chains and auto-vectorise them.

struct XSample {
    si0: usize, // byte offset of left  sample (x0 * 4)
    si1: usize, // byte offset of right sample (x1 * 4)
    w1:  u32,   // weight toward x1 (0 ..= 256)
    w0:  u32,   // 256 - w1
}

#[inline(always)]
fn build_x_table(count: usize, src_w: u32, scaled_w: u32, off_x: u32) -> Vec<XSample> {
    let sw1      = (src_w - 1) as usize;
    let sx_scale = src_w as f32 / scaled_w as f32;
    let mut xs   = Vec::with_capacity(count);
    for ox in 0..count {
        let fx  = (ox as f32 + off_x as f32 + 0.5) * sx_scale - 0.5;
        let x0  = (fx.floor() as isize).clamp(0, sw1 as isize) as usize;
        let x1  = (x0 + 1).min(sw1);
        let w1  = ((fx - x0 as f32).clamp(0.0, 1.0) * 256.0) as u32;
        xs.push(XSample { si0: x0 * 4, si1: x1 * 4, w1, w0: 256 - w1 });
    }
    xs
}

/// Fixed-point bilinear blit for XRGB8888.
/// X table precomputed once; rows processed in parallel via rayon.
fn blit_scaled_bilinear_xrgb(
    out:        &mut [u8],
    dst_stride: usize,
    src_pix:    &[u8],
    src_w:      u32,
    src_h:      u32,
    src_stride: usize,
    dx:         u32,
    dy:         u32,
    clip_w:     u32,
    clip_h:     u32,
    scaled_w:   u32,
    scaled_h:   u32,
) {
    let sy_scale = src_h as f32 / scaled_h as f32;
    let sh1      = (src_h - 1) as usize;
    let cw       = clip_w as usize;
    let dx_off   = dx as usize * 4;

    let xs = build_x_table(cw, src_w, scaled_w, 0);

    // Each row of the destination is independent — process in parallel.
    // We slice only the rows we're writing so chunk indices map 1:1 to oy.
    let dst_start = dy as usize * dst_stride;
    let dst_end   = dst_start + clip_h as usize * dst_stride;
    let region    = &mut out[dst_start..dst_end];

    region
        .par_chunks_mut(dst_stride)
        .enumerate()
        .for_each(|(oy, row_buf)| {
            let fy   = (oy as f32 + 0.5) * sy_scale - 0.5;
            let y0   = (fy.floor() as isize).clamp(0, sh1 as isize) as usize;
            let y1   = (y0 + 1).min(sh1);
            let iwy  = ((fy - y0 as f32).clamp(0.0, 1.0) * 256.0) as u32;
            let iwy0 = 256 - iwy;

            let r0 = &src_pix[y0 * src_stride .. y0 * src_stride + src_stride];
            let r1 = &src_pix[y1 * src_stride .. y1 * src_stride + src_stride];

            let dst_row = &mut row_buf[dx_off .. dx_off + cw * 4];

            for (ox, s) in xs.iter().enumerate() {
                let di = ox * 4;
                // B
                let top = r0[s.si0]   as u32 * s.w0 + r0[s.si1]   as u32 * s.w1;
                let bot = r1[s.si0]   as u32 * s.w0 + r1[s.si1]   as u32 * s.w1;
                dst_row[di]   = ((top * iwy0 + bot * iwy + (1 << 15)) >> 16) as u8;
                // G
                let top = r0[s.si0+1] as u32 * s.w0 + r0[s.si1+1] as u32 * s.w1;
                let bot = r1[s.si0+1] as u32 * s.w0 + r1[s.si1+1] as u32 * s.w1;
                dst_row[di+1] = ((top * iwy0 + bot * iwy + (1 << 15)) >> 16) as u8;
                // R
                let top = r0[s.si0+2] as u32 * s.w0 + r0[s.si1+2] as u32 * s.w1;
                let bot = r1[s.si0+2] as u32 * s.w0 + r1[s.si1+2] as u32 * s.w1;
                dst_row[di+2] = ((top * iwy0 + bot * iwy + (1 << 15)) >> 16) as u8;
                dst_row[di+3] = 0;
            }
        });
}

/// Fixed-point bilinear blit with crop for XRGB8888.
/// Rows processed in parallel via rayon.
fn blit_scaled_crop_bilinear_xrgb(
    out:        &mut [u8],
    dst_w:      u32,
    dst_h:      u32,
    dst_stride: usize,
    src_pix:    &[u8],
    src_w:      u32,
    src_h:      u32,
    src_stride: usize,
    scaled_w:   u32,
    scaled_h:   u32,
    off_x:      u32,
    off_y:      u32,
) {
    let sy_scale = src_h as f32 / scaled_h as f32;
    let sh1      = (src_h - 1) as usize;
    let row_len  = dst_w as usize * 4;

    let vis_w = (scaled_w.saturating_sub(off_x)).min(dst_w) as usize;
    if vis_w == 0 { return; }

    // Clamp output rows to where the scaled image actually covers.
    let max_rows = scaled_h.saturating_sub(off_y).min(dst_h) as usize;
    if max_rows == 0 { return; }

    let xs = build_x_table(vis_w, src_w, scaled_w, off_x);

    out[..max_rows * dst_stride]
        .par_chunks_mut(dst_stride)
        .enumerate()
        .for_each(|(oy, dst_row)| {
            let sy_sc = oy as u32 + off_y;
            let fy   = (sy_sc as f32 + 0.5) * sy_scale - 0.5;
            let y0   = (fy.floor() as isize).clamp(0, sh1 as isize) as usize;
            let y1   = (y0 + 1).min(sh1);
            let iwy  = ((fy - y0 as f32).clamp(0.0, 1.0) * 256.0) as u32;
            let iwy0 = 256 - iwy;

            let r0 = &src_pix[y0 * src_stride .. y0 * src_stride + src_stride];
            let r1 = &src_pix[y1 * src_stride .. y1 * src_stride + src_stride];

            let row = &mut dst_row[..row_len];

            for (ox, s) in xs.iter().enumerate() {
                let di = ox * 4;
                // B
                let top = r0[s.si0]   as u32 * s.w0 + r0[s.si1]   as u32 * s.w1;
                let bot = r1[s.si0]   as u32 * s.w0 + r1[s.si1]   as u32 * s.w1;
                row[di]   = ((top * iwy0 + bot * iwy + (1 << 15)) >> 16) as u8;
                // G
                let top = r0[s.si0+1] as u32 * s.w0 + r0[s.si1+1] as u32 * s.w1;
                let bot = r1[s.si0+1] as u32 * s.w0 + r1[s.si1+1] as u32 * s.w1;
                row[di+1] = ((top * iwy0 + bot * iwy + (1 << 15)) >> 16) as u8;
                // R
                let top = r0[s.si0+2] as u32 * s.w0 + r0[s.si1+2] as u32 * s.w1;
                let bot = r1[s.si0+2] as u32 * s.w0 + r1[s.si1+2] as u32 * s.w1;
                row[di+2] = ((top * iwy0 + bot * iwy + (1 << 15)) >> 16) as u8;
                row[di+3] = 0;
            }
        });
}

fn blit_exact(
    out: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    dst_stride: usize,
    src: &DecodedImage,
    dx: u32,
    dy: u32,
) {
    let clip_w = dst_w.saturating_sub(dx).min(src.width) as usize;
    let clip_h = dst_h.saturating_sub(dy).min(src.height) as usize;
    if clip_w == 0 || clip_h == 0 { return; }

    let row_bytes = clip_w * 4;

    for sy in 0..clip_h {
        let Some(sr) = src_row(src, sy as u32) else { return; };
        let dst_off  = (dy as usize + sy) * dst_stride + dx as usize * 4;
        let Some(dr) = out.get_mut(dst_off..dst_off + row_bytes) else { return; };
        dr.copy_from_slice(&sr[..row_bytes]);
    }
}

fn tile(
    out: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    dst_stride: usize,
    src: &DecodedImage,
) {
    if dst_w == 0 || dst_h == 0 || src.width == 0 || src.height == 0 { return; }

    let row_len = dst_w as usize * 4;

    for ty in 0..dst_h {
        let sy = (ty % src.height) as u32;
        let Some(sr) = src_row(src, sy) else { return; };

        let dst_off = ty as usize * dst_stride;
        let Some(dr) = out.get_mut(dst_off..dst_off + row_len) else { return; };

        for tx in 0..dst_w as usize {
            let sx = tx % src.width as usize;
            let s  = sx * 4;
            let d  = tx * 4;
            if s + 2 >= sr.len() { break; }
            dr[d]     = sr[s];
            dr[d + 1] = sr[s + 1];
            dr[d + 2] = sr[s + 2];
            dr[d + 3] = 0;
        }
    }
}

// ── RGBA canvas blit helpers ────────────────────────────────────────────────
// Same geometry logic as the XRGB helpers above, but source is R,G,B,A and
// we premultiply alpha when writing the XRGB destination.

/// Write one RGBA source pixel as premultiplied XRGB at `dst[di..]`.
#[inline(always)]
fn put_rgba_pixel(src: &[u8], si: usize, dst: &mut [u8], di: usize) {
    let a = src[si + 3] as u16;
    dst[di]     = ((src[si + 2] as u16 * a) / 255) as u8; // B
    dst[di + 1] = ((src[si + 1] as u16 * a) / 255) as u8; // G
    dst[di + 2] = ((src[si]     as u16 * a) / 255) as u8; // R
    dst[di + 3] = 0;
}

fn blit_scaled_rgba(
    rgba:       &[u8],
    src_w:      u32,
    src_h:      u32,
    out:        &mut [u8],
    dst_w:      u32,
    dst_h:      u32,
    dst_stride: usize,
    dx: u32, dy: u32, sw: u32, sh: u32,
) {
    if sw == 0 || sh == 0 { return; }

    let clip_w = sw.min(dst_w.saturating_sub(dx));
    let clip_h = sh.min(dst_h.saturating_sub(dy));
    if clip_w == 0 || clip_h == 0 { return; }

    blit_scaled_bilinear_rgba(
        out, dst_stride,
        rgba, src_w, src_h,
        dx, dy, clip_w, clip_h, sw, sh,
    );
}

fn blit_scaled_crop_rgba(
    rgba:       &[u8],
    src_w:      u32,
    src_h:      u32,
    out:        &mut [u8],
    dst_w:      u32,
    dst_h:      u32,
    dst_stride: usize,
    scaled_w:   u32,
    scaled_h:   u32,
    off_x:      u32,
    off_y:      u32,
) {
    if scaled_w == 0 || scaled_h == 0 || dst_w == 0 || dst_h == 0 { return; }

    blit_scaled_crop_bilinear_rgba(
        out, dst_w, dst_h, dst_stride,
        rgba, src_w, src_h,
        scaled_w, scaled_h, off_x, off_y,
    );
}

// ── RGBA bilinear core ───────────────────────────────────────────────────────

/// Bilinear RGBA→XRGB blit (place at dx/dy, clip_w×clip_h).
/// X table precomputed once; inner loop is integer-only with direct indexing.
/// Bilinear RGBA→XRGB blit. Rows processed in parallel via rayon.
fn blit_scaled_bilinear_rgba(
    out:        &mut [u8],
    dst_stride: usize,
    src_pix:    &[u8],
    src_w:      u32,
    src_h:      u32,
    dx:         u32,
    dy:         u32,
    clip_w:     u32,
    clip_h:     u32,
    scaled_w:   u32,
    scaled_h:   u32,
) {
    let src_stride = src_w as usize * 4;
    let sy_scale   = src_h as f32 / scaled_h as f32;
    let sh1        = (src_h - 1) as usize;
    let cw         = clip_w as usize;
    let dx_off     = dx as usize * 4;

    let xs = build_x_table(cw, src_w, scaled_w, 0);

    let dst_start = dy as usize * dst_stride;
    let dst_end   = dst_start + clip_h as usize * dst_stride;
    let region    = &mut out[dst_start..dst_end];

    region
        .par_chunks_mut(dst_stride)
        .enumerate()
        .for_each(|(oy, row_buf)| {
            let fy   = (oy as f32 + 0.5) * sy_scale - 0.5;
            let y0   = (fy.floor() as isize).clamp(0, sh1 as isize) as usize;
            let y1   = (y0 + 1).min(sh1);
            let iwy  = ((fy - y0 as f32).clamp(0.0, 1.0) * 256.0) as u32;
            let iwy0 = 256 - iwy;

            let r0 = &src_pix[y0 * src_stride .. y0 * src_stride + src_stride];
            let r1 = &src_pix[y1 * src_stride .. y1 * src_stride + src_stride];

            let dst_row = &mut row_buf[dx_off .. dx_off + cw * 4];

            for (ox, s) in xs.iter().enumerate() {
                let di = ox * 4;
                let blend = |ch: usize| -> u32 {
                    let top = r0[s.si0+ch] as u32 * s.w0 + r0[s.si1+ch] as u32 * s.w1;
                    let bot = r1[s.si0+ch] as u32 * s.w0 + r1[s.si1+ch] as u32 * s.w1;
                    (top * iwy0 + bot * iwy + (1 << 15)) >> 16
                };
                let rv = blend(0); let gv = blend(1); let bv = blend(2); let av = blend(3);
                dst_row[di]   = ((bv * av + 127) / 255) as u8;
                dst_row[di+1] = ((gv * av + 127) / 255) as u8;
                dst_row[di+2] = ((rv * av + 127) / 255) as u8;
                dst_row[di+3] = 0;
            }
        });
}

/// Bilinear RGBA→XRGB blit with crop. Rows processed in parallel via rayon.
fn blit_scaled_crop_bilinear_rgba(
    out:        &mut [u8],
    dst_w:      u32,
    dst_h:      u32,
    dst_stride: usize,
    src_pix:    &[u8],
    src_w:      u32,
    src_h:      u32,
    scaled_w:   u32,
    scaled_h:   u32,
    off_x:      u32,
    off_y:      u32,
) {
    let src_stride = src_w as usize * 4;
    let sy_scale   = src_h as f32 / scaled_h as f32;
    let sh1        = (src_h - 1) as usize;
    let row_len    = dst_w as usize * 4;

    let vis_w   = (scaled_w.saturating_sub(off_x)).min(dst_w) as usize;
    if vis_w == 0 { return; }
    let max_rows = scaled_h.saturating_sub(off_y).min(dst_h) as usize;
    if max_rows == 0 { return; }

    let xs = build_x_table(vis_w, src_w, scaled_w, off_x);

    out[..max_rows * dst_stride]
        .par_chunks_mut(dst_stride)
        .enumerate()
        .for_each(|(oy, dst_row)| {
            let sy_sc = oy as u32 + off_y;
            let fy   = (sy_sc as f32 + 0.5) * sy_scale - 0.5;
            let y0   = (fy.floor() as isize).clamp(0, sh1 as isize) as usize;
            let y1   = (y0 + 1).min(sh1);
            let iwy  = ((fy - y0 as f32).clamp(0.0, 1.0) * 256.0) as u32;
            let iwy0 = 256 - iwy;

            let r0 = &src_pix[y0 * src_stride .. y0 * src_stride + src_stride];
            let r1 = &src_pix[y1 * src_stride .. y1 * src_stride + src_stride];

            let row = &mut dst_row[..row_len];

            for (ox, s) in xs.iter().enumerate() {
                let di = ox * 4;
                let blend = |ch: usize| -> u32 {
                    let top = r0[s.si0+ch] as u32 * s.w0 + r0[s.si1+ch] as u32 * s.w1;
                    let bot = r1[s.si0+ch] as u32 * s.w0 + r1[s.si1+ch] as u32 * s.w1;
                    (top * iwy0 + bot * iwy + (1 << 15)) >> 16
                };
                let rv = blend(0); let gv = blend(1); let bv = blend(2); let av = blend(3);
                row[di]   = ((bv * av + 127) / 255) as u8;
                row[di+1] = ((gv * av + 127) / 255) as u8;
                row[di+2] = ((rv * av + 127) / 255) as u8;
                row[di+3] = 0;
            }
        });
}
fn blit_exact_rgba(
    rgba:       &[u8],
    src_w:      u32,
    src_h:      u32,
    out:        &mut [u8],
    dst_w:      u32,
    dst_h:      u32,
    dst_stride: usize,
    dx: u32, dy: u32,
) {
    let clip_w = dst_w.saturating_sub(dx).min(src_w) as usize;
    let clip_h = dst_h.saturating_sub(dy).min(src_h) as usize;
    if clip_w == 0 || clip_h == 0 { return; }

    let src_stride = src_w as usize * 4;
    let row_bytes  = clip_w * 4;

    for sy in 0..clip_h {
        let sr0 = sy * src_stride;
        if sr0 + row_bytes > rgba.len() { break; }
        let sr = &rgba[sr0..sr0 + src_stride];

        let dst_off = (dy as usize + sy) * dst_stride + dx as usize * 4;
        let Some(dr) = out.get_mut(dst_off..dst_off + row_bytes) else { break; };

        for x in 0..clip_w {
            let si = x * 4;
            let di = x * 4;
            if si + 3 >= sr.len() { break; }
            put_rgba_pixel(sr, si, dr, di);
        }
    }
}

fn tile_rgba(
    rgba:       &[u8],
    src_w:      u32,
    src_h:      u32,
    out:        &mut [u8],
    dst_w:      u32,
    dst_h:      u32,
    dst_stride: usize,
) {
    if dst_w == 0 || dst_h == 0 || src_w == 0 || src_h == 0 { return; }

    let src_stride = src_w as usize * 4;
    let row_len    = dst_w as usize * 4;

    for ty in 0..dst_h {
        let sy  = (ty % src_h) as usize;
        let sr0 = sy * src_stride;
        if sr0 + src_stride > rgba.len() { break; }
        let sr = &rgba[sr0..sr0 + src_stride];

        let dst_off = ty as usize * dst_stride;
        let Some(dr) = out.get_mut(dst_off..dst_off + row_len) else { break; };

        for tx in 0..dst_w as usize {
            let sx = tx % src_w as usize;
            let si = sx * 4;
            let di = tx * 4;
            if si + 3 >= sr.len() { break; }
            put_rgba_pixel(sr, si, dr, di);
        }
    }
}
