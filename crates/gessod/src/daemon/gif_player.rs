use std::sync::Arc;
use std::time::{Duration, Instant};

use gesso_core::decode::gif::GifFrameStream;
use gesso_core::decode::{AnimDecoded, AnimFrame, DecodedImage};
use gesso_core::{scale_image, Colour, RenderEngine, ScaleMode, Target};

// ── Inner playback mode ───────────────────────────────────────────────────────

enum PlayMode {
    /// GIF: streaming decoder — one frame decoded at a time, RAM stays flat.
    ///
    /// !! FIELD ORDER MATTERS: `stream` borrows `data`'s bytes via an unsafe
    /// 'static transmute. Rust drops fields in declaration order, so `stream`
    /// MUST be listed before `data` so it is dropped first.
    Streaming {
        stream: GifFrameStream<'static>,
        data:   Arc<Vec<u8>>,
    },
    /// Animated WebP (or any pre-decoded source): index-based playback.
    Frames {
        frames: Vec<AnimFrame>,
        /// Next frame index to display. Sentinel `frames.len()` = end of loop.
        index:  usize,
    },
}

// ── GifPlayer ─────────────────────────────────────────────────────────────────

pub struct GifPlayer {
    mode:   PlayMode,

    out_w:  u32,
    out_h:  u32,
    stride: usize,
    scale:  ScaleMode,
    bg:     Colour,

    pub next_deadline: Instant,

    /// None = loop forever.  Some(n) = n full loops remaining.
    loops_left: Option<u16>,
}

impl GifPlayer {
    /// Construct from a unified `AnimDecoded`.
    ///
    /// Frame 0 is consumed/skipped during construction because the caller
    /// already displayed it via `set_now()`. `next_deadline` is set to
    /// `now + frame0_delay` so the first `tick()` shows frame 1 at the
    /// correct time.
    pub fn new(
        anim:       AnimDecoded,
        out_w:      u32,
        out_h:      u32,
        scale:      ScaleMode,
        bg:         Colour,
        loop_count: Option<u16>,
        now:        Instant,
    ) -> Result<Self, String> {
        // Both branches return (PlayMode, first_frame_delay).
        let (mode, first_delay): (PlayMode, Duration) = if let Some(data) = anim.data {
            // GIF streaming: consume frame 0 to advance past it.
            let mut stream = make_stream(&data)?;
            let delay0 = match stream.next_frame() {
                Some(Ok((_img, delay))) => delay,
                Some(Err(e))            => return Err(format!("gif frame 0 error: {e}")),
                None                    => return Err("gif has no frames".into()),
            };
            (PlayMode::Streaming { stream, data }, delay0)
        } else {
            // Pre-decoded (WebP): start at index 1, use frame 0's delay.
            if anim.frames.is_empty() {
                return Err("animated source has no frames".into());
            }
            let delay0 = anim.frames[0].delay;
            (PlayMode::Frames { frames: anim.frames, index: 1 }, delay0)
        };

        Ok(Self {
            mode,
            out_w,
            out_h,
            stride: out_w as usize * 4,
            scale,
            bg,
            next_deadline: now + first_delay,
            loops_left: loop_count,
        })
    }

    #[inline]
    pub fn next_deadline(&self) -> Instant {
        self.next_deadline
    }

    #[inline]
    fn finished(&self) -> bool {
        matches!(self.loops_left, Some(0))
    }

    fn consume_one_loop(&mut self) {
        if let Some(n) = self.loops_left.as_mut() {
            if *n > 0 { *n -= 1; }
        }
    }

    fn restart(&mut self) -> Result<(), ()> {
        match &mut self.mode {
            PlayMode::Streaming { data, stream } => {
                *stream = make_stream(data).map_err(|e| {
                    eventline::warn!("gif restart failed: {e}");
                })?;
            }
            PlayMode::Frames { index, .. } => {
                *index = 0;
            }
        }
        Ok(())
    }

    /// Decode + scale the next frame when its deadline has passed.
    ///
    /// Returns `Err(())` when a finite animation has completed all loops;
    /// the caller should remove the player from its map.
    pub fn tick(&mut self, now: Instant, eng: &mut RenderEngine, output: &str) -> Result<(), ()> {
        if self.finished() {
            return Err(());
        }
        if now < self.next_deadline {
            return Ok(());
        }

        let (img, delay): (DecodedImage, Duration) = match self.next_raw_frame() {
            FrameResult::Frame(img, delay) => (img, delay),
            FrameResult::EndOfStream => {
                self.consume_one_loop();
                if self.finished() {
                    return Err(());
                }
                if self.restart().is_err() {
                    return Err(());
                }
                match self.next_raw_frame() {
                    FrameResult::Frame(img, delay) => (img, delay),
                    _ => return Err(()),
                }
            }
            FrameResult::Error(e) => {
                eventline::warn!("animation decode error on {output}: {e}");
                return Err(());
            }
        };

        let px    = scale_image(&img, self.out_w, self.out_h, self.scale, self.bg);
        let frame = Arc::new(px);

        let _ = eng.set_now(
            output,
            Target::Image {
                width:    self.out_w,
                height:   self.out_h,
                stride:   self.stride,
                xrgb8888: frame,
            },
        );

        let delay = delay.max(Duration::from_millis(20));
        self.next_deadline = now + delay;

        Ok(())
    }

    fn next_raw_frame(&mut self) -> FrameResult {
        match &mut self.mode {
            PlayMode::Streaming { stream, .. } => match stream.next_frame() {
                Some(Ok((img, delay))) => FrameResult::Frame(img, delay),
                Some(Err(e))           => FrameResult::Error(e),
                None                   => FrameResult::EndOfStream,
            },

            PlayMode::Frames { frames, index } => {
                if *index >= frames.len() {
                    *index = 0;
                    return FrameResult::EndOfStream;
                }
                let img   = frames[*index].img.clone();
                let delay = frames[*index].delay;
                *index += 1;
                FrameResult::Frame(img, delay)
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

enum FrameResult {
    Frame(DecodedImage, Duration),
    EndOfStream,
    Error(String),
}

fn make_stream(data: &Arc<Vec<u8>>) -> Result<GifFrameStream<'static>, String> {
    // Safety:
    // - The Arc keeps the bytes alive for the lifetime of PlayMode::Streaming.
    // - Within that variant, `stream` is declared before `data` so it is
    //   always dropped before the Arc's refcount can reach zero.
    let slice: &'static [u8] =
        unsafe { std::mem::transmute::<&[u8], &'static [u8]>(&data[..]) };
    GifFrameStream::new(slice)
}
