// Author: Dustin Pilgrim
// License: MIT

use crate::error::{WlError, WlResult};
use crate::layer::{create_wallpaper_layer_surface, LayerSurface};
use crate::outputs::OutputInfo;
use crate::shm::{create_xrgb8888, ShmBuffer};
use crate::state::WlState;

use eventline::{debug, scope};
use std::collections::HashMap;
use wayland_client::QueueHandle;
use wayland_client::protocol::wl_output;

struct PerOutput {
    #[allow(dead_code)]
    output: wl_output::WlOutput,
    layer:  LayerSurface,

    // SHM buffers. We keep ONE buffer by default; the second is allocated
    // only when needed (e.g. animation / compositor still holding the first).
    //
    // KEY CHANGE:
    // Even if a buffer is "busy" forever (currently attached), we can now unmap
    // its client-side mmap at idle to reclaim RSS while keeping wl_buffer alive.
    a: Option<ShmBuffer>, // which=0
    b: Option<ShmBuffer>, // which=1
}

pub struct Presenter {
    per: HashMap<u32, PerOutput>,
}

impl Presenter {
    pub fn new(_state: &WlState, _qh: &QueueHandle<WlState>) -> WlResult<Self> {
        Ok(Self { per: HashMap::new() })
    }

    fn ensure_layer(
        &mut self,
        state:  &WlState,
        qh:     &QueueHandle<WlState>,
        out:    &OutputInfo,
        width:  u32,
        height: u32,
    ) -> WlResult<&mut PerOutput> {
        if !self.per.contains_key(&out.wl_global) {
            let (wl_out, _raw) = state
                .outputs
                .get(&out.wl_global)
                .ok_or_else(|| WlError::UnknownOutput(out.name.clone()))?;
            let wl_out = wl_out.clone();

            // Creating the layer surface commits, prompting configure.
            let layer = create_wallpaper_layer_surface(
                state, qh, &wl_out, out.wl_global, width, height,
            )?;

            self.per.insert(out.wl_global, PerOutput {
                output: wl_out,
                layer,
                a: None,
                b: None,
            });
        }

        Ok(self.per.get_mut(&out.wl_global).unwrap())
    }

    fn ensure_a(
        po: &mut PerOutput,
        state: &WlState,
        qh: &QueueHandle<WlState>,
        out: &OutputInfo,
        width: u32,
        height: u32,
    ) -> WlResult<()> {
        let needs_alloc = match &po.a {
            Some(buf) => buf.width != width || buf.height != height,
            None      => true,
        };
        if needs_alloc {
            let shm = state.require_shm()?.clone();
            po.a = Some(create_xrgb8888(&shm, qh, width, height, out.wl_global, 0)?);
        }
        Ok(())
    }

    fn ensure_b(
        po: &mut PerOutput,
        state: &WlState,
        qh: &QueueHandle<WlState>,
        out: &OutputInfo,
        width: u32,
        height: u32,
    ) -> WlResult<()> {
        let needs_alloc = match &po.b {
            Some(buf) => buf.width != width || buf.height != height,
            None      => true,
        };
        if needs_alloc {
            let shm = state.require_shm()?.clone();
            po.b = Some(create_xrgb8888(&shm, qh, width, height, out.wl_global, 1)?);
        }
        Ok(())
    }

    /// Drop / unmap SHM buffers for an output that has gone idle.
    ///
    /// IMPORTANT:
    /// - If a buffer is not busy, drop it entirely (frees wl_buffer + file + mapping).
    /// - If a buffer is busy (likely currently attached), keep wl_buffer alive but
    ///   UNMAP the client-side mmap to reclaim RSS.
    pub fn release_buffers(&mut self, out: &OutputInfo) {
        let Some(po) = self.per.get_mut(&out.wl_global) else { return };

        // Buffer A
        if let Some(buf) = po.a.as_mut() {
            if buf.busy {
                buf.unmap();
            } else {
                po.a = None;
            }
        }

        // Buffer B
        if let Some(buf) = po.b.as_mut() {
            if buf.busy {
                buf.unmap();
            } else {
                po.b = None;
            }
        }
    }

    /// Render directly into an SHM buffer and commit to the compositor.
    ///
    /// Returns `Ok(true)` if a frame was committed, `Ok(false)` if skipped
    /// because the compositor isn't ready yet.
    pub fn render_present_xrgb8888(
        &mut self,
        state:  &mut WlState,
        qh:     &QueueHandle<WlState>,
        out:    &OutputInfo,
        width:  u32,
        height: u32,
        stride: usize,
        mut render: impl FnMut(&mut [u8]) -> WlResult<()>,
    ) -> WlResult<bool> {
        scope!("gesso-wl.present", {
            // Drain buffer releases so busy flags are current, and discard
            // released pages immediately (best-effort).
            for (og, which) in state.drain_releases() {
                if let Some(po) = self.per.get_mut(&og) {
                    match which {
                        0 => {
                            if let Some(buf) = po.a.as_mut() {
                                buf.busy = false;
                                shm_buf_dontneed(buf);
                            }
                        }
                        1 => {
                            if let Some(buf) = po.b.as_mut() {
                                buf.busy = false;
                                shm_buf_dontneed(buf);
                            }
                        }
                        _ => {}
                    }
                }
            }

            if stride != width as usize * 4 {
                return Err(WlError::Protocol("stride must equal width*4".into()));
            }
            let expected = width as usize * 4 * height as usize;

            // Ensure layer surface exists (triggers configure).
            let po = self.ensure_layer(state, qh, out, width, height)?;

            // Layer shell requires configure+ack before first attach.
            if !state.layer_configure_serial.contains_key(&out.wl_global) {
                return Ok(false);
            }

            // Compositor hasn't signalled it's ready for the next frame yet.
            if !state.is_frame_ready(out.wl_global) {
                return Ok(false);
            }

            // Ensure we have at least buffer A.
            Self::ensure_a(po, state, qh, out, width, height)?;

            // Choose a free buffer.
            //
            // Strategy:
            // 1) If A is free → use it.
            // 2) Else if B exists and is free → use it.
            // 3) Else allocate B (lazy) and use it if free.
            // 4) Else both are busy → skip.
            let (which, buf): (u32, &mut ShmBuffer) = {
                let a_free = po.a.as_ref().map(|b| !b.busy).unwrap_or(false);
                if a_free {
                    (0, po.a.as_mut().unwrap())
                } else {
                    let b_free = po.b.as_ref().map(|b| !b.busy).unwrap_or(false);
                    if b_free {
                        (1, po.b.as_mut().unwrap())
                    } else {
                        // Lazily allocate B only when needed.
                        Self::ensure_b(po, state, qh, out, width, height)?;
                        let b_free2 = po.b.as_ref().map(|b| !b.busy).unwrap_or(false);
                        if b_free2 {
                            (1, po.b.as_mut().unwrap())
                        } else {
                            return Ok(false);
                        }
                    }
                }
            };

            // Ensure mmap exists before writing, even if we previously unmapped at idle.
            let dst = buf.map_slice_mut(expected)?;
            render(dst)?;

            debug!("commit");

            // Register frame callback BEFORE commit.
            let cb = po.layer.surface.frame(qh, out.wl_global);
            state.frame_callbacks.insert(out.wl_global, cb);
            state.mark_frame_pending(out.wl_global);

            po.layer.surface.attach(Some(&buf.wl_buffer), 0, 0);
            po.layer.surface.damage_buffer(0, 0, width as i32, height as i32);
            po.layer.surface.commit();

            match which {
                0 => { if let Some(b) = po.a.as_mut() { b.busy = true; } }
                1 => { if let Some(b) = po.b.as_mut() { b.busy = true; } }
                _ => {}
            }

            Ok(true)
        })
    }

    /// Compatibility shim: copies a slice into SHM. Still compositor-paced.
    pub fn present_xrgb8888(
        &mut self,
        state:  &mut WlState,
        qh:     &QueueHandle<WlState>,
        out:    &OutputInfo,
        width:  u32,
        height: u32,
        stride: usize,
        data:   &[u8],
    ) -> WlResult<bool> {
        let expected = width as usize * 4 * height as usize;
        if data.len() != expected {
            return Err(WlError::BufferSizeMismatch { expected, got: data.len() });
        }
        self.render_present_xrgb8888(state, qh, out, width, height, stride, |dst| {
            dst.copy_from_slice(data);
            Ok(())
        })
    }

    pub fn unset(&mut self, out: &OutputInfo) {
        scope!("gesso-wl.unset", {
            self.per.remove(&out.wl_global);
        });
    }
}

// ── SHM memory pressure helper ───────────────────────────────────────────────

#[inline]
fn shm_buf_dontneed(buf: &ShmBuffer) {
    #[cfg(target_os = "linux")]
    {
        use rustix::mm::{Advice, madvise};

        let Some(mmap) = buf.mmap.as_ref() else {
            return; // unmapped already
        };

        let data: &[u8] = &mmap[..];
        if data.is_empty() {
            return;
        }

        let page = rustix::param::page_size();

        let addr  = data.as_ptr() as usize;
        let start = (addr + page - 1) & !(page - 1);
        let end   = (addr + data.len()) & !(page - 1);

        if end <= start {
            return;
        }

        let _ = unsafe {
            madvise(
                start as *mut std::ffi::c_void,
                end - start,
                Advice::LinuxDontNeed,
            )
        };
    }
    #[cfg(not(target_os = "linux"))]
    let _ = buf;
}
