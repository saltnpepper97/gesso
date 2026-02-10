// Author: Dustin Pilgrim
// License: MIT

use eventline as el;
use crate::wallpaper::wayland::SurfaceState;

/// Blend FROM->TO using XRGB8888 per-channel lerp.
/// `t256` in [0, 256]. 0 => from, 256 => to.
pub(crate) fn paint_blend_frame_to_frame_fast(
    s: &mut SurfaceState,
    from_frame: &[u32],
    to_frame: &[u32],
    t256: u16,
) {
    let Some(mmap) = s.buffers.current_mmap_mut() else { 
        el::warn!("paint_blend: no mmap available");
        return;
    };
    
    let len = mmap.len() / 4;
    let dst = unsafe { std::slice::from_raw_parts_mut(mmap.as_mut_ptr() as *mut u32, len) };
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
    
    for i in 0..n {
        dst[i] = lerp_xrgb_u8_fast(from_frame[i], to_frame[i], t, inv);
    }
}

/// Left->right wipe. No blending: pixels are either FROM or TO.
/// `t256` in [0, 256]. 0 => all FROM, 256 => all TO.
pub(crate) fn paint_wipe_frame_to_frame_fast(
    s: &mut SurfaceState,
    from_frame: &[u32],
    to_frame: &[u32],
    t256: u16,
) {
    let Some(mmap) = s.buffers.current_mmap_mut() else {
        el::warn!("paint_wipe: no mmap available");
        return;
    };
    
    let len = mmap.len() / 4;
    let dst = unsafe { std::slice::from_raw_parts_mut(mmap.as_mut_ptr() as *mut u32, len) };
    let w = s.width as usize;
    let h = s.height as usize;
    
    if w == 0 || h == 0 {
        el::warn!("paint_wipe: zero dimensions w={w} h={h}", w = w, h = h);
        return;
    }
    
    let n = dst.len().min(from_frame.len()).min(to_frame.len());
    let frame_px = w.saturating_mul(h);
    let n2 = n.min(frame_px);
    
    if n2 == 0 {
        return;
    }
    
    if t256 >= 256 {
        dst[..n2].copy_from_slice(&to_frame[..n2]);
        return;
    }
    if t256 == 0 {
        dst[..n2].copy_from_slice(&from_frame[..n2]);
        return;
    }
    
    let edge = ((t256 as usize) * w) / 256;
    
    for y in 0..h {
        let row0 = y * w;
        if row0 >= n2 {
            break;
        }
        let row1 = (row0 + w).min(n2);
        let e = (row0 + edge).min(row1);
        
        if row0 < e {
            dst[row0..e].copy_from_slice(&to_frame[row0..e]);
        }
        if e < row1 {
            dst[e..row1].copy_from_slice(&from_frame[e..row1]);
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
