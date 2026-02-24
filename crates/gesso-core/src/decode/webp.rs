use std::time::Duration;

use crate::decode::{AnimFrame, DecodedImage};

pub enum WebpDecoded {
    Still(DecodedImage),
    Animated(WebpAnimation),
}

pub struct WebpAnimation {
    pub first_frame: DecodedImage,
    pub frames:      Vec<WebpFrame>,
}

pub struct WebpFrame {
    pub img:   DecodedImage,
    pub delay: Duration,
}

/// Decode a WebP file using libwebp (via the `webp` crate).
///
/// Cargo.toml: `webp = "0.3"`
pub fn decode_webp(data: &[u8]) -> Result<WebpDecoded, String> {
    match try_animated(data) {
        Ok(Some(anim)) => return Ok(WebpDecoded::Animated(anim)),
        Ok(None)       => {}
        Err(_)         => {}
    }

    // Static path.
    let decoded = webp::Decoder::new(data)
        .decode()
        .ok_or_else(|| "libwebp: failed to decode static webp".to_string())?;

    let width  = decoded.width();
    let height = decoded.height();

    // webp::WebPImage derefs to &[u8], but its pixel layout can be RGB or RGBA.
    let raw = &*decoded;

    let pixels = match raw.len() {
        n if n == width as usize * height as usize * 4 => rgba8_to_xrgb(raw),
        n if n == width as usize * height as usize * 3 => rgb8_to_xrgb(raw),
        n => {
            return Err(format!(
                "libwebp: unexpected decoded buffer size: got={n} expected={} (RGBA) or {} (RGB) for {}x{}",
                width as usize * height as usize * 4,
                width as usize * height as usize * 3,
                width,
                height
            ));
        }
    };

    Ok(WebpDecoded::Still(DecodedImage {
        width,
        height,
        stride: width as usize * 4,
        pixels,
    }))
}

/// Convert a `WebpAnimation` into the shared `Vec<AnimFrame>` type.
pub fn into_anim_frames(anim: WebpAnimation) -> Vec<AnimFrame> {
    anim.frames
        .into_iter()
        .map(|f| AnimFrame { img: f.img, delay: f.delay })
        .collect()
}

fn try_animated(data: &[u8]) -> Result<Option<WebpAnimation>, String> {
    let anim = match webp::AnimDecoder::new(data).decode() {
        Ok(a)  => a,
        Err(_) => return Ok(None),
    };

    if anim.len() <= 1 {
        // Single frame — treat as still so the static path handles it.
        return Ok(None);
    }

    let mut frames: Vec<WebpFrame> = Vec::with_capacity(anim.len());
    let mut prev_ts: i32 = 0;

    for (i, frame) in anim.into_iter().enumerate() {
        let ts_ms    = frame.get_time_ms();
        let delay_ms = if i == 0 {
            ts_ms.max(20)
        } else {
            (ts_ms - prev_ts).max(20)
        };
        prev_ts = ts_ms;

        let delay  = Duration::from_millis(delay_ms as u64);
        let width  = frame.width();
        let height = frame.height();

        // IMPORTANT:
        // webp::AnimFrame does NOT deref to pixels.
        // Use get_image() and get_layout() to handle RGB vs RGBA correctly.
        let img = frame.get_image();

        let pixels = match frame.get_layout() {
            webp::PixelLayout::Rgba => rgba8_to_xrgb(img),
            webp::PixelLayout::Rgb  => rgb8_to_xrgb(img),
            other => return Err(format!("libwebp: unsupported animated pixel layout: {other:?}")),
        };

        // Sanity: after conversion we must be XRGB8888.
        let expected = width as usize * height as usize * 4;
        if pixels.len() != expected {
            return Err(format!(
                "libwebp: frame buffer size mismatch after conversion: got={} expected={} ({}x{})",
                pixels.len(),
                expected,
                width,
                height
            ));
        }

        frames.push(WebpFrame {
            img: DecodedImage {
                width,
                height,
                stride: width as usize * 4,
                pixels,
            },
            delay,
        });
    }

    let first_frame = frames[0].img.clone();
    Ok(Some(WebpAnimation { first_frame, frames }))
}

/// RGB8 → XRGB8888 (B, G, R, 0 byte order)
#[inline]
fn rgb8_to_xrgb(rgb: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; (rgb.len() / 3) * 4];
    for (src, dst) in rgb.chunks_exact(3).zip(out.chunks_exact_mut(4)) {
        let r = src[0];
        let g = src[1];
        let b = src[2];
        dst[2] = r;
        dst[1] = g;
        dst[0] = b;
        dst[3] = 0;
    }
    out
}

/// RGBA8 → XRGB8888 (B, G, R, 0 byte order), alpha premultiplied over black.
#[inline]
fn rgba8_to_xrgb(rgba: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; (rgba.len() / 4) * 4];
    for (src, dst) in rgba.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
        let r = src[0] as u16;
        let g = src[1] as u16;
        let b = src[2] as u16;
        let a = src[3] as u16;
        dst[2] = ((r * a) / 255) as u8;
        dst[1] = ((g * a) / 255) as u8;
        dst[0] = ((b * a) / 255) as u8;
        dst[3] = 0;
    }
    out
}
