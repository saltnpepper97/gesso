use std::sync::Arc;
use std::time::{Duration, Instant};

use gesso_core::decode::gif::GifFrameStream;
use gesso_core::decode::{AnimDecoded, AnimFrame};
use gesso_core::render::scale::scale_image_into;
use gesso_core::mem;
use gesso_core::{Colour, RenderEngine, ScaleMode, Target};

// ── Inner playback mode ───────────────────────────────────────────────────────

enum PlayMode {
    /// GIF: streaming decoder.
    ///
    /// !! FIELD ORDER MATTERS: `stream` borrows `data`'s bytes via an unsafe
    /// 'static transmute. Rust drops fields in declaration order, so `stream`
    /// MUST be listed before `data` so it is dropped first.
    Streaming {
        stream: GifFrameStream<'static>,
        data:   Arc<Vec<u8>>,
    },
    /// Animated WebP: index-based playback over pre-decoded frames.
    Frames {
        frames: Vec<AnimFrame>,
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

    /// Reusable XRGB output buffer (Arc so it can be handed to the RenderEngine
    /// without copying).  We hold one clone here; after the engine blits and drops
    /// its reference the strong count falls to 1, and `get_out_buf` reclaims the
    /// allocation via `Arc::try_unwrap` — no allocation on the next tick.
    out_arc: Option<Arc<Vec<u8>>>,
}

impl GifPlayer {
    pub fn new(
        anim:       AnimDecoded,
        out_w:      u32,
        out_h:      u32,
        scale:      ScaleMode,
        bg:         Colour,
        loop_count: Option<u16>,
        now:        Instant,
    ) -> Result<Self, String> {
        let (mode, first_delay): (PlayMode, Duration) = if let Some(data) = anim.data {
            let mut stream = make_stream(&data)?;
            // Skip frame 0 — caller already displayed it.  skip_frame composites the
            // canvas (so disposal logic is correct) but produces no pixel output.
            let delay0 = match stream.skip_frame() {
                Some(Ok(d))  => d,
                Some(Err(e)) => return Err(format!("gif frame 0 error: {e}")),
                None         => return Err("gif has no frames".into()),
            };
            (PlayMode::Streaming { stream, data }, delay0)
        } else {
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
            out_arc: None,
        })
    }

    #[inline]
    pub fn next_deadline(&self) -> Instant { self.next_deadline }

    // ── Teardown ────────────────────────────────────────────────────────────

    /// Explicitly release large allocations BEFORE this player is dropped.
    ///
    /// Calls `madvise(MADV_DONTNEED)` on the GIF compositing canvas and the last
    /// rendered output buffer so the kernel reclaims their physical pages
    /// immediately — without waiting for jemalloc's dirty/muzzy decay timers.
    ///
    /// Always call this when removing a player from the `gifs` map:
    ///
    /// ```ignore
    /// if let Some(mut p) = gifs.remove(&name) { p.release(); }
    /// ```
    pub fn release(&mut self) {
        // 1. Release the GIF canvas (~8 MB RGBA at 1080p).
        if let PlayMode::Streaming { stream, .. } = &mut self.mode {
            stream.release_canvas();
        }

        // 2. Release the last rendered output frame (~8 MB XRGB).
        //    If the engine still holds a reference we can't reclaim the Vec yet,
        //    but we can still advise DONTNEED so its pages are returned once freed.
        if let Some(arc) = self.out_arc.take() {
            mem::pages_dontneed(&arc);
            // Arc is dropped here; if we were the last holder the Vec is freed now.
        }

        // 3. For WebP: advise all pre-decoded frame buffers as cold.
        //    We don't DONTNEED them (they must remain valid for the player's
        //    lifetime) but we hint that access is unlikely.
        if let PlayMode::Frames { ref frames, .. } = self.mode {
            for f in frames {
                mem::pixels_cold(&f.img.pixels);
            }
        }
    }

    // ── Tick ────────────────────────────────────────────────────────────────

    #[inline]
    fn finished(&self) -> bool { matches!(self.loops_left, Some(0)) }

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
            PlayMode::Frames { index, .. } => { *index = 0; }
        }
        Ok(())
    }

    /// Reclaim the output Vec when the engine has finished reading the last frame,
    /// or allocate a fresh one.
    fn get_out_buf(&mut self) -> Vec<u8> {
        let n = self.out_w as usize * self.out_h as usize * 4;
        if let Some(arc) = self.out_arc.take() {
            match Arc::try_unwrap(arc) {
                Ok(mut v) => {
                    if v.len() != n { v.resize(n, 0); }
                    return v;
                }
                Err(still_shared) => {
                    self.out_arc = Some(still_shared);
                }
            }
        }
        vec![0u8; n]
    }

    pub fn tick(&mut self, now: Instant, eng: &mut RenderEngine, output: &str) -> Result<(), ()> {
        if self.finished()          { return Err(()); }
        if now < self.next_deadline { return Ok(()); }

        let mut out_buf = self.get_out_buf();

        let delay = match self.next_raw_frame_into(&mut out_buf) {
            FrameResult::Delay(d)  => d,
            FrameResult::EndOfStream => {
                self.consume_one_loop();
                if self.finished()        { return Err(()); }
                if self.restart().is_err() { return Err(()); }
                match self.next_raw_frame_into(&mut out_buf) {
                    FrameResult::Delay(d) => d,
                    _                     => return Err(()),
                }
            }
            FrameResult::Error(e) => {
                eventline::warn!("animation decode error on {output}: {e}");
                return Err(());
            }
        };

        let frame = Arc::new(out_buf);
        self.out_arc = Some(Arc::clone(&frame));

        let _ = eng.set_now(
            output,
            Target::Image {
                width:    self.out_w,
                height:   self.out_h,
                stride:   self.stride,
                xrgb8888: frame,
            },
        );

        self.next_deadline = now + delay.max(Duration::from_millis(20));
        Ok(())
    }

    fn next_raw_frame_into(&mut self, dst: &mut [u8]) -> FrameResult {
        match &mut self.mode {
            PlayMode::Streaming { stream, .. } => {
                match stream.next_frame_scaled_into(dst, self.out_w, self.out_h, self.scale, self.bg) {
                    Some(Ok(d))  => FrameResult::Delay(d),
                    Some(Err(e)) => FrameResult::Error(e),
                    None         => FrameResult::EndOfStream,
                }
            }
            PlayMode::Frames { frames, index } => {
                if *index >= frames.len() {
                    *index = 0;
                    return FrameResult::EndOfStream;
                }
                let delay = frames[*index].delay;
                scale_image_into(&frames[*index].img, dst, self.out_w, self.out_h, self.scale, self.bg);
                *index += 1;
                FrameResult::Delay(delay)
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

enum FrameResult {
    Delay(Duration),
    EndOfStream,
    Error(String),
}

fn make_stream(data: &Arc<Vec<u8>>) -> Result<GifFrameStream<'static>, String> {
    // Safety: Arc keeps bytes alive for PlayMode::Streaming's lifetime;
    // `stream` is declared before `data` and is always dropped first.
    let slice: &'static [u8] =
        unsafe { std::mem::transmute::<&[u8], &'static [u8]>(&data[..]) };
    GifFrameStream::new(slice)
}
