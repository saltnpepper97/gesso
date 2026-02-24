use std::io::Cursor;
use std::time::Duration;

use gif::DisposalMethod;

use crate::decode::DecodedImage;

/// Decode just the first fully-rendered GIF frame as a still image.
pub fn decode_gif_first_frame(data: &[u8]) -> Result<DecodedImage, String> {
    let mut stream = GifFrameStream::new(data)?;
    stream
        .next_frame()
        .ok_or_else(|| "gif has no frames".to_string())?
        .map(|(img, _delay)| img)
}

/// A streaming GIF decoder that yields composited full-canvas frames + delays.
///
/// Notes:
/// - Uses gif crate ColorOutput::RGBA.
/// - `frame.buffer` from the gif crate is sized to the FRAME's bounding box
///   (frame.width × frame.height × 4), NOT the full canvas. Frames must be
///   composited onto the canvas manually with proper disposal method handling.
/// - We composite RGBA onto a black canvas, then convert to XRGB8888.
/// - Delays are clamped to 20ms minimum to avoid busy-looping.
pub struct GifFrameStream<'a> {
    width:  u32,
    height: u32,
    decoder: gif::Decoder<Cursor<&'a [u8]>>,

    /// Full-canvas RGBA compositing buffer. Persists across frames.
    canvas: Vec<u8>,

    /// Saved canvas used when DisposalMethod::Previous is set on the current frame.
    /// Saved BEFORE compositing the current frame, restored before the next frame.
    prev_canvas: Vec<u8>,

    /// Disposal method declared on the most recently composited frame.
    /// Applied at the START of the next call to next_frame().
    last_disposal: DisposalMethod,

    /// Bounding rect (left, top, width, height) of the most recently composited frame.
    /// Needed when last_disposal == Background to know which region to clear.
    last_rect: (u16, u16, u16, u16),
}

impl<'a> GifFrameStream<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self, String> {
        use gif::{ColorOutput, DecodeOptions};
        let mut opts = DecodeOptions::new();
        opts.set_color_output(ColorOutput::RGBA);
        let decoder = opts
            .read_info(Cursor::new(data))
            .map_err(|e| e.to_string())?;

        let width  = decoder.width()  as u32;
        let height = decoder.height() as u32;
        // Canvas starts fully transparent-black.
        let canvas      = vec![0u8; width as usize * height as usize * 4];
        let prev_canvas = canvas.clone();

        Ok(Self {
            width,
            height,
            decoder,
            canvas,
            prev_canvas,
            // "Keep" means do nothing before the very first frame.
            last_disposal: DisposalMethod::Keep,
            last_rect: (0, 0, 0, 0),
        })
    }

    /// Returns `Some(Ok((image, delay)))` for each frame, or `None` when the stream ends.
    ///
    /// `image` is always a full-canvas XRGB8888 image with the correct compositing
    /// applied (disposal methods honoured).
    pub fn next_frame(&mut self) -> Option<Result<(DecodedImage, Duration), String>> {
        // ── Step 1: apply previous frame's disposal before overlaying new content ──
        match self.last_disposal {
            DisposalMethod::Background => {
                let (left, top, fw, fh) = self.last_rect;
                let cw = self.width  as usize;
                let ch = self.height as usize;
                for y in 0..fh as usize {
                    for x in 0..fw as usize {
                        let cx = left as usize + x;
                        let cy = top  as usize + y;
                        if cx < cw && cy < ch {
                            let i = (cy * cw + cx) * 4;
                            // Clear to transparent black (logical background).
                            self.canvas[i]     = 0;
                            self.canvas[i + 1] = 0;
                            self.canvas[i + 2] = 0;
                            self.canvas[i + 3] = 0;
                        }
                    }
                }
            }
            DisposalMethod::Previous => {
                self.canvas.copy_from_slice(&self.prev_canvas);
            }
            // Keep / Any / _ → leave canvas as-is.
            _ => {}
        }

        // ── Step 2: read the next raw frame ──
        let frame = match self.decoder.read_next_frame() {
            Ok(Some(f)) => f,
            Ok(None)    => return None,
            Err(e)      => return Some(Err(e.to_string())),
        };

        // Delay: in 1/100th-seconds. Clamp 0/1cs to 20ms minimum.
        let d_cs  = frame.delay.max(2) as u64; // 2 cs = 20 ms
        let delay = Duration::from_millis(d_cs * 10);

        let left     = frame.left   as usize;
        let top      = frame.top    as usize;
        let fw       = frame.width  as usize;
        let fh       = frame.height as usize;
        let disposal = frame.dispose;

        // ── Step 3: save canvas NOW (before compositing) if this frame's disposal is Previous ──
        if matches!(disposal, DisposalMethod::Previous) {
            self.prev_canvas.copy_from_slice(&self.canvas);
        }

        // Remember this frame's disposal + rect for the NEXT call.
        self.last_disposal = disposal;
        self.last_rect = (frame.left, frame.top, frame.width, frame.height);

        // ── Step 4: composite this frame's pixels onto the canvas ──
        //
        // frame.buffer is RGBA, sized fw×fh×4 (the frame's bounding box only —
        // NOT the full canvas). GIF transparency is binary: a == 0 → skip pixel.
        let buf      = &frame.buffer;
        let expected = fw * fh * 4;
        if buf.len() != expected {
            return Some(Err(format!(
                "gif frame buffer size mismatch: got {}, expected {} ({}×{}×4)",
                buf.len(),
                expected,
                fw,
                fh,
            )));
        }

        let cw = self.width  as usize;
        let ch = self.height as usize;

        for y in 0..fh {
            let cy = top + y;
            if cy >= ch {
                continue;
            }
            for x in 0..fw {
                let cx = left + x;
                if cx >= cw {
                    continue;
                }
                let si = (y * fw + x) * 4;
                let di = (cy * cw  + cx) * 4;

                // Binary GIF transparency: a == 0 → transparent (preserve canvas pixel).
                if buf[si + 3] == 0 {
                    continue;
                }
                self.canvas[di]     = buf[si];
                self.canvas[di + 1] = buf[si + 1];
                self.canvas[di + 2] = buf[si + 2];
                self.canvas[di + 3] = buf[si + 3];
            }
        }

        // ── Step 5: convert the full composited canvas → XRGB8888 ──
        let pixels = rgba_canvas_to_xrgb(&self.canvas);

        Some(Ok((
            DecodedImage {
                width:  self.width,
                height: self.height,
                stride: self.width as usize * 4,
                pixels,
            },
            delay,
        )))
    }
}

/// Convert an RGBA canvas (composited over implicit black background) to XRGB8888.
///
/// Layout: XRGB8888 bytes = [ B, G, R, 0 ] per pixel (little-endian 0x00RRGGBB).
/// Alpha-premultiplication over black: r' = r*a/255, g' = g*a/255, b' = b*a/255.
#[inline]
fn rgba_canvas_to_xrgb(rgba: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for (src, dst) in rgba.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
        let r = src[0] as u16;
        let g = src[1] as u16;
        let b = src[2] as u16;
        let a = src[3] as u16;
        dst[2] = ((r * a) / 255) as u8; // R channel
        dst[1] = ((g * a) / 255) as u8; // G channel
        dst[0] = ((b * a) / 255) as u8; // B channel
        dst[3] = 0;                      // X (unused)
    }
    out
}
