// Author: Dustin Pilgrim
// License: MIT

use crate::spec::Rgb;

#[inline]
pub(crate) fn ease_out_cubic(t: f32) -> f32 {
    let t = t - 1.0;
    t * t * t + 1.0
}

#[inline]
pub(crate) fn xrgb8888(c: Rgb) -> u32 {
    ((c.r as u32) << 16) | ((c.g as u32) << 8) | (c.b as u32)
}
