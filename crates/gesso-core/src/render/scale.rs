// Author: Dustin Pilgrim
// License: MIT

use crate::{Colour, DecodedImage};

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

// ── XRGB blit helpers (unchanged from original) ─────────────────────────────

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

    for oy in 0..clip_h {
        let sy = (oy as u64 * src.height as u64 / sh as u64) as u32;
        let Some(sr) = src_row(src, sy) else { return; };

        let dst_off = (dy + oy) as usize * dst_stride + dx as usize * 4;
        let row_len = clip_w as usize * 4;
        let Some(dst_row) = out.get_mut(dst_off..dst_off + row_len) else { return; };

        for ox in 0..clip_w as usize {
            let sx = (ox as u64 * src.width as u64 / sw as u64) as usize;
            let s  = sx * 4;
            let d  = ox * 4;
            if s + 2 >= sr.len() { break; }
            dst_row[d]     = sr[s];
            dst_row[d + 1] = sr[s + 1];
            dst_row[d + 2] = sr[s + 2];
            dst_row[d + 3] = 0;
        }
    }
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

    let row_len = dst_w as usize * 4;

    for oy in 0..dst_h {
        let scaled_y = oy.saturating_add(off_y);
        if scaled_y >= scaled_h { break; }

        let sy = (scaled_y as u64 * src.height as u64 / scaled_h as u64) as u32;
        let Some(sr) = src_row(src, sy) else { return; };

        let dst_off = oy as usize * dst_stride;
        let Some(dst_row) = out.get_mut(dst_off..dst_off + row_len) else { return; };

        for ox in 0..dst_w as usize {
            let scaled_x = (ox as u32).saturating_add(off_x);
            if scaled_x >= scaled_w { break; }

            let sx = (scaled_x as u64 * src.width as u64 / scaled_w as u64) as usize;
            let s  = sx * 4;
            let d  = ox * 4;
            if s + 2 >= sr.len() { break; }
            dst_row[d]     = sr[s];
            dst_row[d + 1] = sr[s + 1];
            dst_row[d + 2] = sr[s + 2];
            dst_row[d + 3] = 0;
        }
    }
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

    let clip_w     = sw.min(dst_w.saturating_sub(dx));
    let clip_h     = sh.min(dst_h.saturating_sub(dy));
    if clip_w == 0 || clip_h == 0 { return; }

    let src_stride = src_w as usize * 4;

    for oy in 0..clip_h {
        let sy  = (oy as u64 * src_h as u64 / sh as u64) as usize;
        let sr0 = sy * src_stride;
        if sr0 + src_stride > rgba.len() { break; }
        let sr = &rgba[sr0..sr0 + src_stride];

        let dst_off = (dy + oy) as usize * dst_stride + dx as usize * 4;
        let row_len = clip_w as usize * 4;
        let Some(dr) = out.get_mut(dst_off..dst_off + row_len) else { break; };

        for ox in 0..clip_w as usize {
            let sx = (ox as u64 * src_w as u64 / sw as u64) as usize;
            let si = sx * 4;
            let di = ox * 4;
            if si + 3 >= sr.len() { break; }
            put_rgba_pixel(sr, si, dr, di);
        }
    }
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

    let src_stride = src_w as usize * 4;
    let row_len    = dst_w as usize * 4;

    for oy in 0..dst_h {
        let sy_sc = oy.saturating_add(off_y);
        if sy_sc >= scaled_h { break; }

        let sy  = (sy_sc as u64 * src_h as u64 / scaled_h as u64) as usize;
        let sr0 = sy * src_stride;
        if sr0 + src_stride > rgba.len() { break; }
        let sr = &rgba[sr0..sr0 + src_stride];

        let dst_off = oy as usize * dst_stride;
        let Some(dr) = out.get_mut(dst_off..dst_off + row_len) else { break; };

        for ox in 0..dst_w as usize {
            let sx_sc = (ox as u32).saturating_add(off_x);
            if sx_sc >= scaled_w { break; }

            let sx = (sx_sc as u64 * src_w as u64 / scaled_w as u64) as usize;
            let si = sx * 4;
            let di = ox * 4;
            if si + 3 >= sr.len() { break; }
            put_rgba_pixel(sr, si, dr, di);
        }
    }
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
