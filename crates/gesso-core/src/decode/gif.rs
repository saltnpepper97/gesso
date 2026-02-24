use std::io::Cursor;
use std::time::Duration;

use gif::DisposalMethod;

use crate::Colour;
use crate::decode::DecodedImage;
use crate::mem;
use crate::render::scale::{scale_rgba_canvas_into, ScaleMode};

/// Decode just the first fully-rendered GIF frame as a still image.
pub fn decode_gif_first_frame(data: &[u8]) -> Result<DecodedImage, String> {
    let mut stream = GifFrameStream::new(data)?;
    let n = stream.width as usize * stream.height as usize * 4;
    let mut buf = vec![0u8; n];
    match stream.next_frame_into(&mut buf) {
        Some(Ok(_))  => {}
        Some(Err(e)) => return Err(e),
        None         => return Err("gif has no frames".to_string()),
    }
    Ok(DecodedImage {
        width:  stream.width,
        height: stream.height,
        stride: stream.width as usize * 4,
        pixels: buf,
    })
}

/// A streaming GIF decoder that yields composited full-canvas frames + delays.
///
/// Memory layout:
/// - ONE full-canvas RGBA compositing buffer (`canvas`)    ~8 MB at 1080p
/// - `prev_canvas` allocated lazily only for DisposalMethod::Previous
/// - NO permanent XRGB output buffer — callers supply the destination
pub struct GifFrameStream<'a> {
    pub(crate) width:  u32,
    pub(crate) height: u32,
    decoder: gif::Decoder<Cursor<&'a [u8]>>,

    /// Full-canvas RGBA compositing buffer. Persists across frames.
    canvas: Vec<u8>,

    /// Saved canvas for DisposalMethod::Previous.
    prev_canvas: Option<Vec<u8>>,

    last_disposal: DisposalMethod,
    last_rect:     (u16, u16, u16, u16),
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
        let n      = width as usize * height as usize * 4;

        Ok(Self {
            width,
            height,
            decoder,
            canvas:        vec![0u8; n],
            prev_canvas:   None,
            last_disposal: DisposalMethod::Keep,
            last_rect:     (0, 0, 0, 0),
        })
    }

    #[inline] pub fn width(&self)  -> u32 { self.width  }
    #[inline] pub fn height(&self) -> u32 { self.height }

    // ── Public decode APIs ──────────────────────────────────────────────────

    /// Advance the stream past the next frame without producing pixel output.
    /// All compositing is still performed so the canvas stays correct.
    pub fn skip_frame(&mut self) -> Option<Result<Duration, String>> {
        self.step_frame()
    }

    /// Decode and composite the next frame into `out` as XRGB8888 (1:1, no scale).
    /// `out` must be exactly `width * height * 4` bytes.
    pub fn next_frame_into(&mut self, out: &mut [u8]) -> Option<Result<Duration, String>> {
        let delay = match self.step_frame()? {
            Ok(d)  => d,
            Err(e) => return Some(Err(e)),
        };
        rgba_canvas_to_xrgb_inplace(&self.canvas, out);
        Some(Ok(delay))
    }

    /// Decode, composite, scale, and convert into `out` (XRGB8888, out_w×out_h×4).
    ///
    /// Single-pass: no intermediate allocation.  Primary hot-path for GIF playback.
    pub fn next_frame_scaled_into(
        &mut self,
        out:   &mut [u8],
        out_w: u32,
        out_h: u32,
        mode:  ScaleMode,
        bg:    Colour,
    ) -> Option<Result<Duration, String>> {
        let delay = match self.step_frame()? {
            Ok(d)  => d,
            Err(e) => return Some(Err(e)),
        };
        scale_rgba_canvas_into(
            &self.canvas, self.width, self.height,
            out, out_w, out_h,
            mode, bg,
        );
        Some(Ok(delay))
    }

    /// MUST be called before this stream is dropped when you want to reclaim RSS
    /// immediately rather than waiting for jemalloc's decay timer.
    ///
    /// Calls `madvise(MADV_DONTNEED)` on both the canvas and prev_canvas so the
    /// kernel reclaims their physical pages right now.  jemalloc still owns the
    /// virtual address range, but the physical pages are returned to the OS.
    ///
    /// Without this, a 1080p GIF leaves ~8–16 MB stranded in jemalloc's dirty cache
    /// for up to `dirty_decay_ms` milliseconds after the player is torn down.
    pub fn release_canvas(&mut self) {
        mem::pages_dontneed(&self.canvas);
        if let Some(ref pc) = self.prev_canvas {
            mem::pages_dontneed(pc);
        }
        // Zero the vecs so jemalloc can't be confused about their state,
        // and so we don't accidentally reference stale canvas data.
        self.canvas.fill(0);
        self.prev_canvas = None;
    }

    /// Drop `prev_canvas` to reclaim RSS when DisposalMethod::Previous won't occur again.
    pub fn drop_prev_canvas(&mut self) {
        if let Some(ref pc) = self.prev_canvas {
            mem::pages_dontneed(pc);
        }
        self.prev_canvas = None;
    }

    // ── Internal core ───────────────────────────────────────────────────────

    fn step_frame(&mut self) -> Option<Result<Duration, String>> {
        // ── 1. Apply previous disposal ──
        match self.last_disposal {
            DisposalMethod::Background => {
                let (left, top, fw, fh) = self.last_rect;
                let cw   = self.width  as usize;
                let ch   = self.height as usize;
                let left = left as usize;
                let top  = top  as usize;
                let fw   = fw   as usize;
                let fh   = fh   as usize;

                for y in 0..fh {
                    let cy = top + y;
                    if cy >= ch { break; }
                    let x0 = left.min(cw);
                    let x1 = (left + fw).min(cw);
                    if x1 <= x0 { continue; }
                    let s = (cy * cw + x0) * 4;
                    let e = (cy * cw + x1) * 4;
                    self.canvas[s..e].fill(0);
                }
            }
            DisposalMethod::Previous => {
                if let Some(prev) = self.prev_canvas.as_ref() {
                    self.canvas.copy_from_slice(prev);
                }
            }
            _ => {}
        }

        // ── 2. Read next raw frame ──
        let frame = match self.decoder.read_next_frame() {
            Ok(Some(f)) => f,
            Ok(None)    => return None,
            Err(e)      => return Some(Err(e.to_string())),
        };

        let d_cs  = frame.delay.max(2) as u64;
        let delay = Duration::from_millis(d_cs * 10);

        let left     = frame.left   as usize;
        let top      = frame.top    as usize;
        let fw       = frame.width  as usize;
        let fh       = frame.height as usize;
        let disposal = frame.dispose;

        // ── 3. Save canvas before compositing if this frame uses Previous ──
        if matches!(disposal, DisposalMethod::Previous) {
            let prev = self.prev_canvas
                .get_or_insert_with(|| vec![0u8; self.canvas.len()]);
            prev.copy_from_slice(&self.canvas);
        }

        self.last_disposal = disposal;
        self.last_rect     = (frame.left, frame.top, frame.width, frame.height);

        // ── 4. Composite bbox onto canvas ──
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
                let a  = src_row[si + 3];
                if a == 0 { continue; }
                let di = (cy * cw + cx) * 4;
                self.canvas[di]     = src_row[si];
                self.canvas[di + 1] = src_row[si + 1];
                self.canvas[di + 2] = src_row[si + 2];
                self.canvas[di + 3] = a;
            }
        }

        Some(Ok(delay))
    }
}

// ── Pixel format helpers ────────────────────────────────────────────────────

#[inline]
fn rgba_canvas_to_xrgb_inplace(rgba: &[u8], out: &mut [u8]) {
    debug_assert_eq!(rgba.len(), out.len());
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
