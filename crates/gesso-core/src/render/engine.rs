// Author: Dustin Pilgrim
// License: MIT

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{Colour, Surface};
use crate::mem;
use super::{render_transition, RenderCtx, Transition};

//
// Error
//

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineError {
    UnknownOutput,
    DimensionMismatch,
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::UnknownOutput     => write!(f, "unknown output"),
            EngineError::DimensionMismatch => write!(f, "target dimensions do not match output"),
        }
    }
}

impl std::error::Error for EngineError {}

pub type Result<T> = std::result::Result<T, EngineError>;

//
// Target
//

#[derive(Debug, Clone)]
pub enum Target {
    Colour(Colour),
    Image {
        width:    u32,
        height:   u32,
        stride:   usize,
        xrgb8888: Arc<Vec<u8>>,
    },
}

impl Target {
    pub fn image(width: u32, height: u32, stride: usize, pixels: Vec<u8>) -> Self {
        Target::Image { width, height, stride, xrgb8888: Arc::new(pixels) }
    }

    fn dims(&self) -> Option<(u32, u32, usize)> {
        match self {
            Target::Colour(_) => None,
            Target::Image { width, height, stride, .. } => Some((*width, *height, *stride)),
        }
    }
}

//
// “From” snapshot (NO forced pixel allocation)
//

#[derive(Debug, Clone)]
pub enum OldSnapshot {
    /// Old content is a solid colour (including Unset as black).
    Colour(Colour),
    /// Old content is a full pixel snapshot (only used when old was an image).
    Image(Arc<Vec<u8>>),
}

impl OldSnapshot {
    #[inline]
    fn as_image(&self) -> Option<&Arc<Vec<u8>>> {
        match self {
            OldSnapshot::Image(a) => Some(a),
            _ => None,
        }
    }
}

//
// “Current” state (debug-only; NO pixel ownership)
//

#[cfg(debug_assertions)]
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
enum CurrentKind {
    Unset,
    Colour(Colour),
    Image { width: u32, height: u32, stride: usize },
}

#[cfg(debug_assertions)]
impl CurrentKind {
    fn from_target(t: &Target) -> Self {
        match t {
            Target::Colour(c) => CurrentKind::Colour(*c),
            Target::Image { width, height, stride, .. } => CurrentKind::Image {
                width: *width,
                height: *height,
                stride: *stride,
            },
        }
    }
}

//
// Per-transition easing
//

/// `1 - (1 - t)^exp`  — starts fast, ends at 0 velocity.
/// Increasing exp makes the tail linger longer.
#[inline(always)]
fn ease_out(t: f32, exp: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powf(exp)
}

/// Classic smoothstep: gentle ease-in AND ease-out.
#[inline(always)]
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Returns an eased `t` appropriate for the given transition type.
#[inline(always)]
fn ease_for_transition(transition: &Transition, t: f32) -> f32 {
    match transition {
        Transition::Drop { .. } => ease_out(t, 4.0), // punchy, long settle
        Transition::Wave { .. } => ease_out(t, 3.0), // natural wipe decel
        Transition::Fade { .. } => smoothstep(t),    // symmetric cross-dissolve
        Transition::None        => t,
    }
}

//
// Internal per-output state
//

struct ActiveTransition {
    transition: Transition,
    duration:   Duration,
    from:       OldSnapshot, // caller-supplied snapshot (may be colour-only)
    to:         Target,       // holds pixels only for the duration of the transition
    start:      Instant,
}

impl ActiveTransition {
    #[inline]
    fn progress(&self, now: Instant) -> f32 {
        let elapsed = now.duration_since(self.start).as_secs_f32();
        let dur = self.duration.as_secs_f32().max(1e-6);
        (elapsed / dur).clamp(0.0, 1.0)
    }

    #[inline]
    fn is_complete(&self, now: Instant) -> bool {
        now.duration_since(self.start) >= self.duration
    }
}

struct OutputState {
    width:   u32,
    height:  u32,
    stride:  usize,

    // Lightweight “what is currently shown” metadata (no pixels).
    // Debug-only so release builds stay warning-free and lean.
    #[cfg(debug_assertions)]
    current: CurrentKind,

    // If set_now() is called, we hold pixels only until the next render,
    // then drop them immediately after blitting into the compositor buffer.
    pending: Option<Target>,

    // Active animation holds pixels only for the duration of the transition.
    active:  Option<ActiveTransition>,
}

impl OutputState {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            stride: width as usize * 4,

            #[cfg(debug_assertions)]
            current: CurrentKind::Unset,

            pending: None,
            active: None,
        }
    }

    fn ensure_dims_match(&self, t: &Target) -> Result<()> {
        if let Some((w, h, s)) = t.dims() {
            if w != self.width || h != self.height || s != self.stride {
                return Err(EngineError::DimensionMismatch);
            }
        }
        Ok(())
    }
}

//
// Frame (kept for API compatibility; not used internally)
//

pub struct Frame<'a> {
    pub output:   &'a str,
    pub width:    u32,
    pub height:   u32,
    pub stride:   usize,
    pub xrgb8888: &'a [u8],
}

//
// Engine
//

pub struct RenderEngine {
    ctx:     RenderCtx,
    outputs: HashMap<String, OutputState>,
}

impl Default for RenderEngine {
    fn default() -> Self {
        Self {
            ctx:     RenderCtx::default(),
            outputs: HashMap::new(),
        }
    }
}

impl RenderEngine {
    pub fn register_output(&mut self, name: impl Into<String>, width: u32, height: u32) {
        let name = name.into();
        let replace = self
            .outputs
            .get(&name)
            .map(|st| st.width != width || st.height != height)
            .unwrap_or(true);

        if replace {
            self.outputs.insert(name, OutputState::new(width, height));
        }
    }

    /// True if we have work to do for this output (a frame must be rendered).
    /// When false, the caller should not call render_output_into(), allowing
    /// the engine to stay pixel-free while idle.
    pub fn needs_present(&self, output: &str) -> bool {
        match self.outputs.get(output) {
            None     => false,
            Some(st) => st.active.is_some() || st.pending.is_some(),
        }
    }

    /// True if this output is currently mid-transition.
    /// (Useful for gating GIF playback until transition into frame 0 finishes.)
    pub fn is_transitioning(&self, output: &str) -> bool {
        self.outputs
            .get(output)
            .map(|st| st.active.is_some())
            .unwrap_or(false)
    }

    /// Set immediately (no transition). Pixels are kept only until the next render.
    pub fn set_now(&mut self, output: &str, target: Target) -> Result<()> {
        let st = self.outputs.get_mut(output).ok_or(EngineError::UnknownOutput)?;
        st.ensure_dims_match(&target)?;

        // Drop any in-flight transition and any pending target.
        st.active = None;
        st.pending = Some(target);

        #[cfg(debug_assertions)]
        {
            st.current = CurrentKind::from_target(st.pending.as_ref().unwrap());
        }

        Ok(())
    }

    /// Start a transition where the caller provides the “from” snapshot.
    ///
    /// Key to low idle memory:
    /// - If old was a colour/unset, pass OldSnapshot::Colour (NO Vec alloc).
    /// - Only pass OldSnapshot::Image when old was truly an image.
    pub fn set_with_transition_from(
        &mut self,
        output: &str,
        from: OldSnapshot,
        target: Target,
        transition: Transition,
    ) -> Result<()> {
        let st = self.outputs.get_mut(output).ok_or(EngineError::UnknownOutput)?;
        st.ensure_dims_match(&target)?;

        if matches!(transition, Transition::None) || transition.duration_ms() == 0 {
            // Behave like set_now.
            st.active = None;
            st.pending = Some(target);

            #[cfg(debug_assertions)]
            {
                st.current = CurrentKind::from_target(st.pending.as_ref().unwrap());
            }

            return Ok(());
        }

        // Hint access patterns (best-effort).
        if let Some(img) = from.as_image() {
            mem::pixels_sequential(img);
        }
        if let Target::Image { xrgb8888, .. } = &target {
            mem::pixels_sequential(xrgb8888);
        }

        st.pending = None;

        #[cfg(debug_assertions)]
        {
            st.current = CurrentKind::from_target(&target);
        }

        st.active = Some(ActiveTransition {
            duration: Duration::from_millis(transition.duration_ms() as u64),
            transition,
            from,
            to: target,
            start: Instant::now(),
        });

        Ok(())
    }

    /// Render one frame into `dst`. Caller should only invoke when needs_present() is true.
    ///
    /// Returns true if a frame was written.
    pub fn render_output_into(&mut self, output: &str, dst: &mut [u8]) -> bool {
        let now = Instant::now();
        let st  = match self.outputs.get_mut(output) {
            Some(s) => s,
            None    => return false,
        };

        // Active transition: render intermediate frames until complete.
        if let Some(a) = st.active.as_ref() {
            let t_linear = a.progress(now);

            if a.is_complete(now) || t_linear >= 1.0 {
                // Finish: blit final “to” and drop all pixel ownership immediately.
                let finished = st.active.take().unwrap();

                blit_target_fast(&finished.to, st.width, st.height, st.stride, dst);

                // Mark the old snapshot reclaimable and drop (if it had pixels).
                if let OldSnapshot::Image(ref img) = finished.from {
                    mem::pixels_free(img);
                }
                // drop finished.from here

                // Also hint that `to` pixels are cold; then drop `to` immediately.
                if let Target::Image { xrgb8888, .. } = &finished.to {
                    mem::pixels_cold(xrgb8888);
                }

                #[cfg(debug_assertions)]
                {
                    st.current = CurrentKind::from_target(&finished.to);
                }

                // `finished.to` is dropped here → no idle pixel cache.
                return true;
            }

            // Eased progress.
            let mut t = ease_for_transition(&a.transition, t_linear);

            // Optional stepping: quantize eased t into N discrete steps.
            let steps = a.transition.steps();
            if steps > 0 {
                let n = steps as f32;
                t = ((t * n).floor() / n).clamp(0.0, 1.0);
            }

            render_active_into(&mut self.ctx, st.width, st.height, st.stride, a, t, dst);
            return true;
        }

        // One-shot “present once” for set_now(). After this call, drop pixels.
        if let Some(pending) = st.pending.take() {
            blit_target_fast(&pending, st.width, st.height, st.stride, dst);

            if let Target::Image { xrgb8888, .. } = &pending {
                // We no longer need this in userspace after commit.
                mem::pixels_cold(xrgb8888);
            }

            #[cfg(debug_assertions)]
            {
                st.current = CurrentKind::from_target(&pending);
            }

            // `pending` dropped here → no idle pixel cache.
            return true;
        }

        false
    }
}

//
// Helpers
//

#[inline(always)]
fn colour_u32(c: Colour) -> u32 {
    (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32
}

#[inline(always)]
fn as_u32_slice_mut(buf: &mut [u8]) -> &mut [u32] {
    debug_assert_eq!(buf.len() % 4, 0);
    unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u32, buf.len() / 4) }
}

fn blit_target_fast(target: &Target, w: u32, h: u32, _stride: usize, out: &mut [u8]) {
    match target {
        Target::Image { xrgb8888, .. } => out.copy_from_slice(xrgb8888),
        Target::Colour(c) => {
            let px = colour_u32(*c);
            let n  = w as usize * h as usize;
            as_u32_slice_mut(out)[..n].fill(px);
        }
    }
}

fn render_active_into(
    ctx:    &mut RenderCtx,
    w:      u32,
    h:      u32,
    stride: usize,
    active: &ActiveTransition,
    t:      f32,
    dst:    &mut [u8],
) {
    let (old_surf, old_colour) = match &active.from {
        OldSnapshot::Colour(c) => (None, Some(*c)),
        OldSnapshot::Image(pix) => (Some(Surface { width: w, height: h, stride, data: pix }), None),
    };

    let (new_surf, new_col) = target_to_inputs(&active.to);

    render_transition(
        ctx,
        active.transition.clone(),
        old_surf,
        old_colour,
        new_surf,
        new_col,
        w,
        h,
        stride,
        dst,
        t,
    );
}

fn target_to_inputs(t: &Target) -> (Option<Surface<'_>>, Option<Colour>) {
    match t {
        Target::Colour(c) => (None, Some(*c)),
        Target::Image { width, height, stride, xrgb8888 } => (
            Some(Surface { width: *width, height: *height, stride: *stride, data: xrgb8888 }),
            None,
        ),
    }
}
