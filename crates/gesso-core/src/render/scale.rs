use crate::{Colour, DecodedImage};

#[derive(Debug, Clone, Copy)]
pub enum ScaleMode {
    Fill,
    Fit,
    Stretch,
    Center,
    Tile,
}

/// Scale `src` to `(dst_w, dst_h)` using `mode`, letterboxing with `bg` where needed.
/// Output is XRGB8888, stride = dst_w * 4.
pub fn scale_image(
    src: &DecodedImage,
    dst_w: u32,
    dst_h: u32,
    mode: ScaleMode,
    bg: Colour,
) -> Vec<u8> {
    let stride = dst_w as usize * 4;
    let mut out = vec![0u8; stride * dst_h as usize];
    fill_bg(&mut out, dst_w, dst_h, bg);

    // Trivial outs.
    if dst_w == 0 || dst_h == 0 || src.width == 0 || src.height == 0 {
        return out;
    }

    // Validate source buffer so helper slicing can’t panic.
    // We assume pixels are XRGB8888 (4 bytes per pixel), and `stride` is bytes per row.
    let min_stride = src.width as usize * 4;
    if src.stride < min_stride {
        debug_assert!(
            false,
            "DecodedImage stride too small: stride={} min={} ({}x{})",
            src.stride,
            min_stride,
            src.width,
            src.height
        );
        return out;
    }
    let needed = src.stride.saturating_mul(src.height as usize);
    if src.pixels.len() < needed {
        debug_assert!(
            false,
            "DecodedImage pixels too small: len={} needed={} ({}x{} stride={})",
            src.pixels.len(),
            needed,
            src.width,
            src.height,
            src.stride
        );
        return out;
    }

    match mode {
        ScaleMode::Stretch => {
            blit_scaled(&mut out, dst_w, dst_h, stride, src, 0, 0, dst_w, dst_h);
        }
        ScaleMode::Fill => {
            let scale = f32::max(
                dst_w as f32 / src.width as f32,
                dst_h as f32 / src.height as f32,
            );
            let scaled_w = (src.width as f32 * scale).round() as u32;
            let scaled_h = (src.height as f32 * scale).round() as u32;
            let off_x = ((scaled_w as i32 - dst_w as i32) / 2).max(0) as u32;
            let off_y = ((scaled_h as i32 - dst_h as i32) / 2).max(0) as u32;

            blit_scaled_crop(
                &mut out,
                dst_w,
                dst_h,
                stride,
                src,
                scaled_w,
                scaled_h,
                off_x,
                off_y,
            );
        }
        ScaleMode::Fit => {
            let scale = f32::min(
                dst_w as f32 / src.width as f32,
                dst_h as f32 / src.height as f32,
            );
            let scaled_w = (src.width as f32 * scale).round() as u32;
            let scaled_h = (src.height as f32 * scale).round() as u32;
            let x = ((dst_w as i32 - scaled_w as i32) / 2).max(0) as u32;
            let y = ((dst_h as i32 - scaled_h as i32) / 2).max(0) as u32;

            blit_scaled(&mut out, dst_w, dst_h, stride, src, x, y, scaled_w, scaled_h);
        }
        ScaleMode::Center => {
            if src.width <= dst_w && src.height <= dst_h {
                let x = ((dst_w as i32 - src.width as i32) / 2).max(0) as u32;
                let y = ((dst_h as i32 - src.height as i32) / 2).max(0) as u32;

                blit_exact(&mut out, dst_w, dst_h, stride, src, x, y);
            } else {
                let off_x = ((src.width as i32 - dst_w as i32) / 2).max(0) as u32;
                let off_y = ((src.height as i32 - dst_h as i32) / 2).max(0) as u32;

                blit_scaled_crop(
                    &mut out,
                    dst_w,
                    dst_h,
                    stride,
                    src,
                    src.width,
                    src.height,
                    off_x,
                    off_y,
                );
            }
        }
        ScaleMode::Tile => {
            tile(&mut out, dst_w, dst_h, stride, src);
        }
    }

    out
}

// --- helpers ---

#[inline]
fn fill_bg(out: &mut [u8], w: u32, h: u32, bg: Colour) {
    let px = (bg.r as u32) << 16 | (bg.g as u32) << 8 | bg.b as u32;
    let n = w as usize * h as usize;

    // out is always sized to w*h*4, so this is safe as long as n*4 == out.len().
    // Keep it simple: if the math doesn't line up (shouldn't happen), just return.
    if out.len() < n.saturating_mul(4) {
        return;
    }

    let dst = unsafe { std::slice::from_raw_parts_mut(out.as_mut_ptr() as *mut u32, n) };
    dst.fill(px);
}

/// Safely fetch a full source row slice `[start..start+stride]`.
#[inline]
fn src_row(src: &DecodedImage, sy: u32) -> Option<&[u8]> {
    let start = (sy as usize).saturating_mul(src.stride);
    let end = start.saturating_add(src.stride);
    src.pixels.get(start..end)
}

/// Blit `src` scaled to `(sw, sh)` at destination offset `(dx, dy)`.
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
    if sw == 0 || sh == 0 {
        return;
    }

    let clip_w = sw.min(dst_w.saturating_sub(dx));
    let clip_h = sh.min(dst_h.saturating_sub(dy));
    if clip_w == 0 || clip_h == 0 {
        return;
    }

    for oy in 0..clip_h {
        let sy = (oy as u64 * src.height as u64 / sh as u64) as u32;
        let Some(src_row) = src_row(src, sy) else { return; };

        let dst_off = (dy + oy) as usize * dst_stride + dx as usize * 4;
        let row_len = clip_w as usize * 4;
        let Some(dst_row) = out.get_mut(dst_off..dst_off + row_len) else { return; };

        for ox in 0..clip_w as usize {
            let sx = (ox as u64 * src.width as u64 / sw as u64) as usize;

            // src pixels are tightly packed in a row (at least width*4) by our validation.
            let s = sx * 4;
            let d = ox * 4;

            // If a bogus `src.width` ever slips through, avoid panic.
            if s + 2 >= src_row.len() {
                break;
            }

            dst_row[d] = src_row[s];
            dst_row[d + 1] = src_row[s + 1];
            dst_row[d + 2] = src_row[s + 2];
            dst_row[d + 3] = 0;
        }
    }
}

/// Blit `src` pre-scaled to `(scaled_w, scaled_h)`, outputting only
/// the window `[off_x..off_x+dst_w, off_y..off_y+dst_h]`.
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
    if scaled_w == 0 || scaled_h == 0 || dst_w == 0 || dst_h == 0 {
        return;
    }

    let row_len = dst_w as usize * 4;

    for oy in 0..dst_h {
        let scaled_y = oy.saturating_add(off_y);
        if scaled_y >= scaled_h {
            break;
        }

        let sy = (scaled_y as u64 * src.height as u64 / scaled_h as u64) as u32;
        let Some(src_row) = src_row(src, sy) else { return; };

        let dst_off = oy as usize * dst_stride;
        let Some(dst_row) = out.get_mut(dst_off..dst_off + row_len) else { return; };

        for ox in 0..dst_w as usize {
            let scaled_x = (ox as u32).saturating_add(off_x);
            if scaled_x >= scaled_w {
                break;
            }

            let sx = (scaled_x as u64 * src.width as u64 / scaled_w as u64) as usize;
            let s = sx * 4;
            let d = ox * 4;

            if s + 2 >= src_row.len() {
                break;
            }

            dst_row[d] = src_row[s];
            dst_row[d + 1] = src_row[s + 1];
            dst_row[d + 2] = src_row[s + 2];
            dst_row[d + 3] = 0;
        }
    }
}

/// Blit `src` 1:1 at `(dx, dy)`, clipping to dst bounds.
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
    if clip_w == 0 || clip_h == 0 {
        return;
    }

    let row_bytes = clip_w * 4;

    for sy in 0..clip_h {
        let Some(src_row) = src_row(src, sy as u32) else { return; };

        let dst_off = (dy as usize + sy) * dst_stride + dx as usize * 4;
        let Some(dst_row) = out.get_mut(dst_off..dst_off + row_bytes) else { return; };

        // Copy only the visible width (not full src.stride).
        dst_row.copy_from_slice(&src_row[..row_bytes]);
    }
}

fn tile(out: &mut [u8], dst_w: u32, dst_h: u32, dst_stride: usize, src: &DecodedImage) {
    if dst_w == 0 || dst_h == 0 || src.width == 0 || src.height == 0 {
        return;
    }

    let row_len = dst_w as usize * 4;

    for ty in 0..dst_h {
        let sy = (ty % src.height) as u32;
        let Some(src_row) = src_row(src, sy) else { return; };

        let dst_off = ty as usize * dst_stride;
        let Some(dst_row) = out.get_mut(dst_off..dst_off + row_len) else { return; };

        for tx in 0..dst_w as usize {
            let sx = tx % src.width as usize;
            let s = sx * 4;
            let d = tx * 4;

            if s + 2 >= src_row.len() {
                break;
            }

            dst_row[d] = src_row[s];
            dst_row[d + 1] = src_row[s + 1];
            dst_row[d + 2] = src_row[s + 2];
            dst_row[d + 3] = 0;
        }
    }
}
