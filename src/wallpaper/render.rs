// Author: Dustin Pilgrim
// License: MIT

use image::{imageops::FilterType, RgbaImage};

use crate::spec::{Mode, Rgb};
use crate::wallpaper::util::xrgb8888;

/// Render an RGBA source into an XRGB8888 framebuffer (Vec<u32>) sized (dw, dh),
/// using the requested Mode and background colour for alpha compositing.
pub(crate) fn render_final_frame_u32(dw: usize, dh: usize, src: &RgbaImage, mode: Mode, bg: Rgb) -> Vec<u32> {
    let bg_px = xrgb8888(bg);
    let mut out = vec![bg_px; dw * dh];

    match mode {
        Mode::Stretch => {
            let resized = image::imageops::resize(src, dw as u32, dh as u32, FilterType::Triangle);
            blit_rgba_into_xrgb(&mut out, dw, dh, &resized, 0, 0, bg);
        }
        Mode::Fit => {
            let (rw, rh, ox, oy) = fit_rect(src.width(), src.height(), dw as u32, dh as u32);
            let resized = image::imageops::resize(src, rw, rh, FilterType::Triangle);
            blit_rgba_into_xrgb(&mut out, dw, dh, &resized, ox as i32, oy as i32, bg);
        }
        Mode::Fill => {
            let (rw, rh) = fill_size(src.width(), src.height(), dw as u32, dh as u32);
            let resized = image::imageops::resize(src, rw, rh, FilterType::Triangle);
            let cx = ((rw as i32 - dw as i32) / 2).max(0) as u32;
            let cy = ((rh as i32 - dh as i32) / 2).max(0) as u32;
            blit_rgba_crop_into_xrgb(&mut out, dw, dh, &resized, cx, cy, bg);
        }
        Mode::Center => {
            let sw = src.width() as i32;
            let sh = src.height() as i32;
            let dw_i = dw as i32;
            let dh_i = dh as i32;
            let ox = (dw_i - sw) / 2;
            let oy = (dh_i - sh) / 2;
            blit_rgba_into_xrgb(&mut out, dw, dh, src, ox, oy, bg);
        }
        Mode::Tile => tile_rgba_into_xrgb(&mut out, dw, dh, src, bg),
    }

    out
}

fn blit_rgba_into_xrgb(out: &mut [u32], out_w: usize, out_h: usize, src: &RgbaImage, ox: i32, oy: i32, bg: Rgb) {
    let sw = src.width() as i32;
    let sh = src.height() as i32;

    let x0 = ox.max(0);
    let y0 = oy.max(0);
    let x1 = (ox + sw).min(out_w as i32);
    let y1 = (oy + sh).min(out_h as i32);

    if x1 <= x0 || y1 <= y0 {
        return;
    }

    for y in y0..y1 {
        let sy = (y - oy) as u32;
        let row = (y as usize) * out_w;
        for x in x0..x1 {
            let sx = (x - ox) as u32;
            let px = src.get_pixel(sx, sy).0;
            out[row + x as usize] = composite_rgba_over_bg(px, bg);
        }
    }
}

fn blit_rgba_crop_into_xrgb(out: &mut [u32], out_w: usize, out_h: usize, src: &RgbaImage, crop_x: u32, crop_y: u32, bg: Rgb) {
    let sw = src.width();
    let sh = src.height();

    for y in 0..out_h {
        let sy = crop_y.saturating_add(y as u32);
        if sy >= sh {
            break;
        }
        let row = y * out_w;
        for x in 0..out_w {
            let sx = crop_x.saturating_add(x as u32);
            if sx >= sw {
                break;
            }
            let px = src.get_pixel(sx, sy).0;
            out[row + x] = composite_rgba_over_bg(px, bg);
        }
    }
}

fn tile_rgba_into_xrgb(out: &mut [u32], out_w: usize, out_h: usize, src: &RgbaImage, bg: Rgb) {
    let sw = src.width() as usize;
    let sh = src.height() as usize;
    if sw == 0 || sh == 0 {
        return;
    }

    for y in 0..out_h {
        let sy = y % sh;
        let row = y * out_w;
        for x in 0..out_w {
            let sx = x % sw;
            let px = src.get_pixel(sx as u32, sy as u32).0;
            out[row + x] = composite_rgba_over_bg(px, bg);
        }
    }
}

fn fit_rect(sw: u32, sh: u32, dw: u32, dh: u32) -> (u32, u32, u32, u32) {
    let swf = sw as f32;
    let shf = sh as f32;
    let dwf = dw as f32;
    let dhf = dh as f32;

    let scale = (dwf / swf).min(dhf / shf).max(0.0);
    let rw = (swf * scale).round().max(1.0) as u32;
    let rh = (shf * scale).round().max(1.0) as u32;

    let ox = (dw.saturating_sub(rw)) / 2;
    let oy = (dh.saturating_sub(rh)) / 2;

    (rw, rh, ox, oy)
}

fn fill_size(sw: u32, sh: u32, dw: u32, dh: u32) -> (u32, u32) {
    let swf = sw as f32;
    let shf = sh as f32;
    let dwf = dw as f32;
    let dhf = dh as f32;

    let scale = (dwf / swf).max(dhf / shf).max(0.0);
    let rw = (swf * scale).round().max(dw as f32).max(1.0) as u32;
    let rh = (shf * scale).round().max(dh as f32).max(1.0) as u32;

    (rw, rh)
}

fn composite_rgba_over_bg(px: [u8; 4], bg: Rgb) -> u32 {
    let r = px[0] as u32;
    let g = px[1] as u32;
    let b = px[2] as u32;
    let a = px[3] as u32;

    if a >= 255 {
        return ((r & 0xFF) << 16) | ((g & 0xFF) << 8) | (b & 0xFF);
    }
    if a == 0 {
        return xrgb8888(bg);
    }

    let br = bg.r as u32;
    let bgc = bg.g as u32;
    let bb = bg.b as u32;

    let inv = 255 - a;
    let or = (r * a + br * inv) / 255;
    let og = (g * a + bgc * inv) / 255;
    let ob = (b * a + bb * inv) / 255;

    ((or & 0xFF) << 16) | ((og & 0xFF) << 8) | (ob & 0xFF)
}
