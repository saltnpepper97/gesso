// Author: Dustin Pilgrim
// License: MIT

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

mod png;
mod jpeg;
pub mod gif;
pub mod webp;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported format (png, jpeg, gif, webp supported)")]
    Unsupported,
    #[error("png decode failed: {0}")]
    Png(String),
    #[error("jpeg decode failed: {0}")]
    Jpeg(String),
    #[error("gif decode failed: {0}")]
    Gif(String),
    #[error("webp decode failed: {0}")]
    WebP(String),
}

#[derive(Clone)]
pub struct DecodedImage {
    pub width:  u32,
    pub height: u32,
    pub stride: usize,   // width * 4
    pub pixels: Vec<u8>, // XRGB8888: B,G,R,0
}

/// A single animation frame shared between GIF and animated WebP.
pub struct AnimFrame {
    pub img:   DecodedImage,
    pub delay: Duration,
}

/// Result of decoding any supported image.
pub enum Decoded {
    Still(DecodedImage),
    Animated(AnimDecoded),
}

pub struct AnimDecoded {
    /// Raw source bytes for the streaming GIF decoder.
    /// `None` for animated WebP (frames are pre-decoded eagerly).
    pub data:        Option<Arc<Vec<u8>>>,
    /// First frame, ready to display immediately.
    pub first_frame: DecodedImage,
    /// All frames — populated for WebP; empty for GIF (streaming decoder used).
    pub frames:      Vec<AnimFrame>,
    /// None = loop forever.  Some(n) = play n times.
    pub loop_count:  Option<u16>,
}

// ── Magic-byte detectors ──────────────────────────────────────────────────────

fn is_png(buf: &[u8]) -> bool {
    buf.len() >= 8 && buf[..8] == [137, 80, 78, 71, 13, 10, 26, 10]
}

fn is_jpeg(buf: &[u8]) -> bool {
    buf.len() >= 2 && buf[0] == 0xFF && buf[1] == 0xD8
}

fn is_gif(buf: &[u8]) -> bool {
    buf.len() >= 6 && (&buf[..6] == b"GIF87a" || &buf[..6] == b"GIF89a")
}

/// WebP: RIFF container with "WEBP" at offset 8.
fn is_webp(buf: &[u8]) -> bool {
    buf.len() >= 12 && &buf[..4] == b"RIFF" && &buf[8..12] == b"WEBP"
}

// ── Public API ────────────────────────────────────────────────────────────────

pub fn decode(path: &Path) -> Result<Decoded, DecodeError> {
    let data = std::fs::read(path)?;

    if is_png(&data) {
        return png::decode_png(&data)
            .map(Decoded::Still)
            .map_err(DecodeError::Png);
    }

    if is_jpeg(&data) {
        return jpeg::decode_jpeg(&data)
            .map(Decoded::Still)
            .map_err(DecodeError::Jpeg);
    }

    if is_gif(&data) {
        let data  = Arc::new(data);
        let first = gif::decode_gif_first_frame(&data).map_err(DecodeError::Gif)?;
        return Ok(Decoded::Animated(AnimDecoded {
            data:        Some(data),
            first_frame: first,
            frames:      Vec::new(), // GIF uses streaming decoder, no pre-decoded frames
            loop_count:  None,
        }));
    }

    if is_webp(&data) {
        return decode_webp_inner(data).map_err(DecodeError::WebP);
    }

    Err(DecodeError::Unsupported)
}

fn decode_webp_inner(data: Vec<u8>) -> Result<Decoded, String> {
    match webp::decode_webp(&data)? {
        webp::WebpDecoded::Still(img) => Ok(Decoded::Still(img)),
        webp::WebpDecoded::Animated(anim) => {
            let first_frame = anim.first_frame.clone();
            let frames      = webp::into_anim_frames(anim);
            Ok(Decoded::Animated(AnimDecoded {
                data:        None,
                first_frame,
                frames,
                loop_count:  None,
            }))
        }
    }
}

/// Decode and return a single still image.
/// For animated formats, returns only the first frame.
pub fn decode_image(path: &Path) -> Result<DecodedImage, DecodeError> {
    match decode(path)? {
        Decoded::Still(img)  => Ok(img),
        Decoded::Animated(a) => Ok(a.first_frame),
    }
}
