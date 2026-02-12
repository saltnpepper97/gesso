// Author: Dustin Pilgrim
// License: MIT

use crate::spec::WipeFrom;
use crate::wallpaper::wayland::SurfaceState;

#[inline(always)]
fn mmap_dst_u32<'a>(s: &'a mut SurfaceState) -> Option<&'a mut [u32]> {
    let mmap = s.buffers.current_mmap_mut()?;
    let len = mmap.len() / 4;
    Some(unsafe { std::slice::from_raw_parts_mut(mmap.as_mut_ptr() as *mut u32, len) })
}

/// Blend FROM->TO using XRGB8888 per-channel lerp.
/// `t256` in [0, 256]. 0 => from, 256 => to.
pub(crate) fn paint_blend_frame_to_frame_fast(
    s: &mut SurfaceState,
    from_frame: &[u32],
    to_frame: &[u32],
    t256: u16,
) {
    let Some(dst) = mmap_dst_u32(s) else { return };
    let n = dst.len().min(from_frame.len()).min(to_frame.len());

    if t256 >= 256 {
        dst[..n].copy_from_slice(&to_frame[..n]);
        return;
    }
    if t256 == 0 {
        dst[..n].copy_from_slice(&from_frame[..n]);
        return;
    }

    let t = t256 as u32;
    let inv = 256u32 - t;

    // Chunked loop encourages auto-vectorization and reduces loop overhead.
    let head_len = n & !7;
    let (dst_head, dst_tail) = dst[..n].split_at_mut(head_len);
    let (a_head, a_tail) = from_frame[..n].split_at(head_len);
    let (b_head, b_tail) = to_frame[..n].split_at(head_len);

    for i in (0..dst_head.len()).step_by(8) {
        dst_head[i + 0] = lerp_xrgb_u8_fast(a_head[i + 0], b_head[i + 0], t, inv);
        dst_head[i + 1] = lerp_xrgb_u8_fast(a_head[i + 1], b_head[i + 1], t, inv);
        dst_head[i + 2] = lerp_xrgb_u8_fast(a_head[i + 2], b_head[i + 2], t, inv);
        dst_head[i + 3] = lerp_xrgb_u8_fast(a_head[i + 3], b_head[i + 3], t, inv);
        dst_head[i + 4] = lerp_xrgb_u8_fast(a_head[i + 4], b_head[i + 4], t, inv);
        dst_head[i + 5] = lerp_xrgb_u8_fast(a_head[i + 5], b_head[i + 5], t, inv);
        dst_head[i + 6] = lerp_xrgb_u8_fast(a_head[i + 6], b_head[i + 6], t, inv);
        dst_head[i + 7] = lerp_xrgb_u8_fast(a_head[i + 7], b_head[i + 7], t, inv);
    }
    for i in 0..dst_tail.len() {
        dst_tail[i] = lerp_xrgb_u8_fast(a_tail[i], b_tail[i], t, inv);
    }
}

/// Blend FROM->SOLID using XRGB8888 per-channel lerp.
/// `t256` in [0, 256]. 0 => from, 256 => solid.
pub(crate) fn paint_blend_frame_to_solid_fast(
    s: &mut SurfaceState,
    from_frame: &[u32],
    to_px: u32,
    t256: u16,
) {
    let Some(dst) = mmap_dst_u32(s) else { return };
    let n = dst.len().min(from_frame.len());

    if t256 >= 256 {
        dst[..n].fill(to_px);
        return;
    }
    if t256 == 0 {
        dst[..n].copy_from_slice(&from_frame[..n]);
        return;
    }

    let t = t256 as u32;
    let inv = 256u32 - t;

    let b_rb = to_px & 0x00FF00FF;
    let b_g = to_px & 0x0000FF00;

    #[inline(always)]
    fn lerp_to_solid(a: u32, b_rb: u32, b_g: u32, t: u32, inv: u32) -> u32 {
        let a_rb = a & 0x00FF00FF;
        let a_g = a & 0x0000FF00;
        let rb = ((a_rb * inv + b_rb * t) >> 8) & 0x00FF00FF;
        let g = ((a_g * inv + b_g * t) >> 8) & 0x0000FF00;
        rb | g
    }

    let head_len = n & !7;
    let (dst_head, dst_tail) = dst[..n].split_at_mut(head_len);
    let (a_head, a_tail) = from_frame[..n].split_at(head_len);

    for i in (0..dst_head.len()).step_by(8) {
        dst_head[i + 0] = lerp_to_solid(a_head[i + 0], b_rb, b_g, t, inv);
        dst_head[i + 1] = lerp_to_solid(a_head[i + 1], b_rb, b_g, t, inv);
        dst_head[i + 2] = lerp_to_solid(a_head[i + 2], b_rb, b_g, t, inv);
        dst_head[i + 3] = lerp_to_solid(a_head[i + 3], b_rb, b_g, t, inv);
        dst_head[i + 4] = lerp_to_solid(a_head[i + 4], b_rb, b_g, t, inv);
        dst_head[i + 5] = lerp_to_solid(a_head[i + 5], b_rb, b_g, t, inv);
        dst_head[i + 6] = lerp_to_solid(a_head[i + 6], b_rb, b_g, t, inv);
        dst_head[i + 7] = lerp_to_solid(a_head[i + 7], b_rb, b_g, t, inv);
    }
    for i in 0..dst_tail.len() {
        dst_tail[i] = lerp_to_solid(a_tail[i], b_rb, b_g, t, inv);
    }
}

/// Directional wipe FROM->SOLID using row fill/copy.
/// `t256` in [0, 256]. 0 => all FROM, 256 => all SOLID.
pub(crate) fn paint_wipe_frame_to_solid_fast(
    s: &mut SurfaceState,
    from_frame: &[u32],
    to_px: u32,
    t256: u16,
    wipe_from: WipeFrom,
) {
    // read geometry BEFORE borrowing s mutably for dst
    let w = s.width as usize;
    let h = s.height as usize;
    if w == 0 || h == 0 {
        return;
    }

    let Some(dst) = mmap_dst_u32(s) else { return };

    let frame_px = w.saturating_mul(h);
    let n = dst.len().min(from_frame.len()).min(frame_px);
    if n < w {
        return;
    }
    let rows = n / w;

    if t256 >= 256 {
        dst[..n].fill(to_px);
        return;
    }
    if t256 == 0 {
        dst[..n].copy_from_slice(&from_frame[..n]);
        return;
    }

    let cols = (((t256.min(256)) as usize) * w / 256).min(w);

    match wipe_from {
        WipeFrom::Left => {
            for y in 0..rows {
                let off = y * w;
                let row_dst = &mut dst[off..off + w];
                let row_from = &from_frame[off..off + w];

                row_dst[..cols].fill(to_px);
                row_dst[cols..].copy_from_slice(&row_from[cols..]);
            }
        }
        WipeFrom::Right => {
            let start = w.saturating_sub(cols);
            for y in 0..rows {
                let off = y * w;
                let row_dst = &mut dst[off..off + w];
                let row_from = &from_frame[off..off + w];

                row_dst[..start].copy_from_slice(&row_from[..start]);
                row_dst[start..].fill(to_px);
            }
        }
    }
}

#[inline(always)]
fn lerp_xrgb_u8_fast(a: u32, b: u32, t: u32, inv: u32) -> u32 {
    let a_rb = a & 0x00FF00FF;
    let b_rb = b & 0x00FF00FF;
    let a_g = a & 0x0000FF00;
    let b_g = b & 0x0000FF00;

    let rb = ((a_rb * inv + b_rb * t) >> 8) & 0x00FF00FF;
    let g = ((a_g * inv + b_g * t) >> 8) & 0x0000FF00;

    rb | g
}

#[inline]
pub(crate) fn ease_out_cubic(t: f32) -> f32 {
    let t = t - 1.0;
    t * t * t + 1.0
}
