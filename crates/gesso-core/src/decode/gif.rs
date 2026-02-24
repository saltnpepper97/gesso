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
/// Key properties:
/// - Keeps ONE full-canvas RGBA compositing buffer (`canvas`)
/// - Allocates `prev_canvas` lazily only if DisposalMethod::Previous occurs
/// - Reuses an XRGB output buffer (`xrgb`) so there is NO per-frame allocation
/// - Returns DecodedImage that *borrows* from internal `xrgb` via Vec clone at the end
///   (caller-owned). For the daemon we’ll avoid this by exposing a slice API later.
pub struct GifFrameStream<'a> {
    width:  u32,
    height: u32,
    decoder: gif::Decoder<Cursor<&'a [u8]>>,

    /// Full-canvas RGBA compositing buffer. Persists across frames.
    canvas: Vec<u8>,

    /// Saved canvas used when DisposalMethod::Previous is set.
    /// Saved BEFORE compositing the current frame, restored before the next frame.
    prev_canvas: Option<Vec<u8>>,

    /// Reused full-canvas XRGB output buffer.
    xrgb: Vec<u8>,

    /// Disposal method declared on the most recently composited frame.
    /// Applied at the START of the next call to next_frame().
    last_disposal: DisposalMethod,

    /// Bounding rect (left, top, width, height) of the most recently composited frame.
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

        let n = width as usize * height as usize * 4;

        Ok(Self {
            width,
            height,
            decoder,
            canvas: vec![0u8; n],
            prev_canvas: None,
            xrgb: vec![0u8; n],
            last_disposal: DisposalMethod::Keep,
            last_rect: (0, 0, 0, 0),
        })
    }

    /// Returns `Some(Ok((image, delay)))` for each frame, or `None` when the stream ends.
    ///
    /// `image` is always a full-canvas XRGB8888 image with correct compositing.
    pub fn next_frame(&mut self) -> Option<Result<(DecodedImage, Duration), String>> {
        // ── Step 1: apply previous frame's disposal ──
        match self.last_disposal {
            DisposalMethod::Background => {
                let (left, top, fw, fh) = self.last_rect;

                let cw = self.width as usize;
                let ch = self.height as usize;

                let left = left as usize;
                let top  = top as usize;
                let fw   = fw as usize;
                let fh   = fh as usize;

                for y in 0..fh {
                    let cy = top + y;
                    if cy >= ch { break; }

                    let x0 = left.min(cw);
                    let x1 = (left + fw).min(cw);
                    if x1 <= x0 { continue; }

                    let start = (cy * cw + x0) * 4;
                    let end   = (cy * cw + x1) * 4;
                    self.canvas[start..end].fill(0);
                }
            }
            DisposalMethod::Previous => {
                if let Some(prev) = self.prev_canvas.as_ref() {
                    self.canvas.copy_from_slice(prev);
                }
            }
            _ => {}
        }

        // ── Step 2: read the next raw frame ──
        let frame = match self.decoder.read_next_frame() {
            Ok(Some(f)) => f,
            Ok(None)    => return None,
            Err(e)      => return Some(Err(e.to_string())),
        };

        // Delay: in 1/100th-seconds. Clamp to 20ms minimum.
        let d_cs  = frame.delay.max(2) as u64; // 2 cs = 20 ms
        let delay = Duration::from_millis(d_cs * 10);

        let left     = frame.left   as usize;
        let top      = frame.top    as usize;
        let fw       = frame.width  as usize;
        let fh       = frame.height as usize;
        let disposal = frame.dispose;

        // ── Step 3: save canvas NOW (before compositing) if this frame's disposal is Previous ──
        if matches!(disposal, DisposalMethod::Previous) {
            let prev = self.prev_canvas.get_or_insert_with(|| vec![0u8; self.canvas.len()]);
            prev.copy_from_slice(&self.canvas);
        }

        self.last_disposal = disposal;
        self.last_rect = (frame.left, frame.top, frame.width, frame.height);

        // ── Step 4: composite this frame bbox onto canvas ──
        let buf      = &frame.buffer;
        let expected = fw * fh * 4;
        if buf.len() != expected {
            return Some(Err(format!(
                "gif frame buffer size mismatch: got {}, expected {} ({}×{}×4)",
                buf.len(), expected, fw, fh,
            )));
        }

        let cw = self.width  as usize;
        let ch = self.height as usize;

        for y in 0..fh {
            let cy = top + y;
            if cy >= ch { continue; }

            let src_row = &buf[y * fw * 4..(y + 1) * fw * 4];

            for x in 0..fw {
                let cx = left + x;
                if cx >= cw { continue; }

                let si = x * 4;
                let a = src_row[si + 3];

                // Binary transparency
                if a == 0 { continue; }

                let di = (cy * cw + cx) * 4;
                self.canvas[di]     = src_row[si];
                self.canvas[di + 1] = src_row[si + 1];
                self.canvas[di + 2] = src_row[si + 2];
                self.canvas[di + 3] = a;
            }
        }

        // ── Step 5: convert canvas -> XRGB (IN PLACE, no alloc) ──
        rgba_canvas_to_xrgb_inplace(&self.canvas, &mut self.xrgb);

        // Return a DecodedImage that owns pixels.
        // NOTE: this still clones for API compatibility with your existing GifPlayer;
        // the big win is we removed the per-frame alloc inside decode. Next step is
        // returning a slice to avoid this clone in the daemon path.
        let pixels = self.xrgb.clone();

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

    /// Aggressive: drop prev_canvas if you want lower steady-state RSS.
    pub fn drop_prev_canvas(&mut self) {
        self.prev_canvas = None;
    }
}

/// Convert an RGBA canvas to XRGB8888, writing into `out` (must be same len).
#[inline]
fn rgba_canvas_to_xrgb_inplace(rgba: &[u8], out: &mut [u8]) {
    if out.len() != rgba.len() { return; }
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
}
