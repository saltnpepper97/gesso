// Author: Dustin Pilgrim
// License: MIT

use std::fs::File;
use std::os::fd::{AsFd, AsRawFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use eventline as el;
use memmap2::MmapMut;
use tempfile::tempfile;

use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_buffer::{self, WlBuffer},
        wl_callback::{self, WlCallback},
        wl_compositor::WlCompositor,
        wl_output::{self, WlOutput},
        wl_region::WlRegion,
        wl_registry,
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};

use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

use crate::spec::{Rgb, Spec};

#[derive(Debug)]
pub struct Probe {
    pub wayland_display: bool,
    pub compositor: bool,
    pub shm: bool,
    pub layer_shell: bool,
    pub outputs: usize,
}

#[derive(Default)]
pub(crate) struct ShmBuf {
    _file: Option<File>,
    mmap: Option<MmapMut>,
    _pool: Option<WlShmPool>,
    buffer: Option<WlBuffer>,
    busy: bool,
}

impl ShmBuf {
    fn is_ready(&self) -> bool {
        self.buffer.is_some() && self.mmap.is_some()
    }
}

#[derive(Default)]
pub(crate) struct DoubleBuffer {
    a: ShmBuf,
    b: ShmBuf,
    current: usize, // 0 => a, 1 => b
}

impl DoubleBuffer {
    pub(crate) fn current_mmap_mut(&mut self) -> Option<&mut MmapMut> {
        match self.current {
            0 => self.a.mmap.as_mut(),
            _ => self.b.mmap.as_mut(),
        }
    }

    pub(crate) fn current_buffer(&self) -> Option<&WlBuffer> {
        match self.current {
            0 => self.a.buffer.as_ref(),
            _ => self.b.buffer.as_ref(),
        }
    }

    pub(crate) fn swap(&mut self) {
        self.current = 1 - self.current;
    }

    /// We require both buffers ready for stable double-buffering.
    pub(crate) fn both_ready(&self) -> bool {
        self.a.is_ready() && self.b.is_ready()
    }

    pub(crate) fn current_is_busy(&self) -> bool {
        match self.current {
            0 => self.a.busy,
            _ => self.b.busy,
        }
    }

    pub(crate) fn mark_current_busy(&mut self) {
        match self.current {
            0 => self.a.busy = true,
            _ => self.b.busy = true,
        }
    }

    pub(crate) fn mark_free(&mut self, which: usize) {
        if which == 0 {
            self.a.busy = false;
        } else {
            self.b.busy = false;
        }
    }

    pub(crate) fn swap_to_free(&mut self) {
        let other = 1 - self.current;
        let other_busy = if other == 0 { self.a.busy } else { self.b.busy };
        if !other_busy {
            self.current = other;
        }
    }

    pub(crate) fn slot_mut(&mut self, which: usize) -> &mut ShmBuf {
        if which == 0 {
            &mut self.a
        } else {
            &mut self.b
        }
    }
}

#[derive(Debug, Clone)]
struct OutputInfo {
    wl: WlOutput,
    name: Option<String>,
    description: Option<String>,
}

pub(crate) struct SurfaceState {
    pub(crate) _output: WlOutput,
    pub(crate) output_name: Option<String>, // wl_output.name (or description fallback)

    pub(crate) surface: WlSurface,
    pub(crate) layer: ZwlrLayerSurfaceV1,

    pub(crate) alive: bool, // false means we destroyed the surface/layer for per-output unset

    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) configured: bool,

    pub(crate) stride: i32,
    pub(crate) size_bytes: usize,
    pub(crate) buffers: DoubleBuffer,

    pub(crate) last_colour: Rgb,
    pub(crate) has_image: bool,
    pub(crate) last_frame: Option<Arc<[u32]>>,

    // Frame callback support:
    // Some compositors / layer-shell wallpaper surfaces don't reliably deliver frame callbacks.
    // When that happens, frame_pending can get stuck and stall animations / mode switches.
    pub(crate) frame_callback_ok: bool,

    // Frame callback must be kept alive until Done arrives.
    pub(crate) frame_pending: bool,
    pub(crate) frame_cb: Option<WlCallback>,
    pub(crate) frame_tick: u32,
}

pub struct Engine {
    pub(crate) _conn: Connection,
    event_queue: Option<EventQueue<Engine>>,
    pub(crate) qh: QueueHandle<Engine>,

    compositor: Option<WlCompositor>,
    pub(crate) shm: Option<WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,

    outputs: Vec<OutputInfo>,
    pub(crate) surfaces: Vec<SurfaceState>,
    current: Option<Spec>,
}

impl Engine {
    pub fn new() -> Result<Engine> {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() {
            bail!("Wayland-only: WAYLAND_DISPLAY is unset");
        }

        let conn = Connection::connect_to_env().context("connect_to_env")?;
        let (globals, event_queue) =
            registry_queue_init::<Engine>(&conn).context("registry_queue_init")?;
        let qh = event_queue.handle();

        let compositor = globals.bind::<WlCompositor, _, _>(&qh, 1..=6, ()).ok();
        let shm = globals.bind::<WlShm, _, _>(&qh, 1..=1, ()).ok();
        let layer_shell = globals
            .bind::<ZwlrLayerShellV1, _, _>(&qh, 1..=1, ())
            .ok();

        let mut outputs: Vec<OutputInfo> = Vec::new();
        for g in globals.contents().clone_list() {
            if g.interface == "wl_output" {
                let idx = outputs.len();
                let ver = g.version.min(4);
                let out: WlOutput = globals.registry().bind(g.name, ver, &qh, idx);
                outputs.push(OutputInfo {
                    wl: out,
                    name: None,
                    description: None,
                });
            }
        }

        el::info!(
            "wayland.connect display={display} compositor={compositor} shm={shm} layer_shell={layer_shell} outputs={outputs}",
            display = std::env::var_os("WAYLAND_DISPLAY").is_some(),
            compositor = compositor.is_some(),
            shm = shm.is_some(),
            layer_shell = layer_shell.is_some(),
            outputs = outputs.len()
        );

        Ok(Engine {
            _conn: conn,
            event_queue: Some(event_queue),
            qh,
            compositor,
            shm,
            layer_shell,
            outputs,
            surfaces: Vec::new(),
            current: None,
        })
    }

    pub fn probe(&self) -> Probe {
        Probe {
            wayland_display: std::env::var_os("WAYLAND_DISPLAY").is_some(),
            compositor: self.compositor.is_some(),
            shm: self.shm.is_some(),
            layer_shell: self.layer_shell.is_some(),
            outputs: self.outputs.len(),
        }
    }

    // ---- IMPORTANT: never lose the event queue even on error ----
    pub fn roundtrip(&mut self) -> Result<()> {
        let mut q = self.event_queue.take().context("event_queue missing")?;
        let res = q.roundtrip(self).context("wayland roundtrip");
        self.event_queue = Some(q);
        res.map(|_| ())
    }

    pub fn blocking_dispatch(&mut self) -> Result<()> {
        let mut q = self.event_queue.take().context("event_queue missing")?;
        let res = q.blocking_dispatch(self).context("blocking_dispatch");
        self.event_queue = Some(q);
        res.map(|_| ())
    }

    pub fn dispatch_pending(&mut self) -> Result<usize> {
        let mut q = self.event_queue.take().context("event_queue missing")?;
        let res = q.dispatch_pending(self).context("dispatch_pending");
        self.event_queue = Some(q);
        res.map(|n| n as usize)
    }

    /// Poll the Wayland socket for readability with a timeout.
    /// This prevents deadlocks caused by calling blocking_dispatch() when the compositor is silent.
    fn poll_wayland_readable(&self, timeout: Duration) -> Result<bool> {
        let fd = self._conn.backend().poll_fd().as_raw_fd();

        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };

        let timeout_ms: i32 = timeout.as_millis().min(i32::MAX as u128) as i32;

        let rc = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, timeout_ms) };
        if rc < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(e).context("poll wayland fd");
        }

        if rc == 0 {
            return Ok(false);
        }

        Ok((pfd.revents & libc::POLLIN) != 0)
    }

/// Dispatch only when readable, and never block forever.
fn dispatch_with_timeout(&mut self, timeout: Duration) -> Result<()> {
    if self.poll_wayland_readable(timeout)? {
        self.blocking_dispatch()?;
    }
    Ok(())
}

    fn ensure_surfaces(&mut self) -> Result<()> {
        if !self.surfaces.is_empty() {
            return Ok(());
        }

        // Let wl_output events arrive (name/description).
        let _ = self.roundtrip();
        let _ = self.roundtrip();

        let compositor = self
            .compositor
            .as_ref()
            .context("wl_compositor missing")?
            .clone();

        let layer_shell = self
            .layer_shell
            .as_ref()
            .context("zwlr_layer_shell_v1 missing (need layer-shell capable compositor)")?
            .clone();

        let outputs: Vec<(WlOutput, Option<String>)> = self
            .outputs
            .iter()
            .map(|oi| {
                let name = oi.name.clone().or_else(|| oi.description.clone());
                (oi.wl.clone(), name)
            })
            .collect();

        el::info!(
            "wayland.ensure_surfaces creating count={count}",
            count = outputs.len()
        );

        for (out, output_name) in outputs {
            let (surface, layer) = create_layer_surface(&compositor, &layer_shell, &self.qh, &out)?;

            let si = self.surfaces.len();
            el::info!(
                "wayland.surface.created si={si} name={name} alive={alive}",
                si = si as i64,
                name = output_name.as_deref().unwrap_or("(unknown)"),
                alive = true
            );

            self.surfaces.push(SurfaceState {
                _output: out,
                output_name,
                surface,
                layer,
                alive: true,
                width: 0,
                height: 0,
                configured: false,
                stride: 0,
                size_bytes: 0,
                buffers: DoubleBuffer::default(),
                last_colour: Rgb { r: 0, g: 0, b: 0 },
                has_image: false,
                last_frame: None,

                frame_callback_ok: false,
                frame_pending: false,
                frame_cb: None,
                frame_tick: 0,
            });
        }

        Ok(())
    }

    fn ensure_selected_surfaces_alive(&mut self, output: Option<&str>) -> Result<()> {
        let compositor = self
            .compositor
            .as_ref()
            .context("wl_compositor missing")?
            .clone();
        let layer_shell = self
            .layer_shell
            .as_ref()
            .context("zwlr_layer_shell_v1 missing")?
            .clone();

        for si in 0..self.surfaces.len() {
            if !surface_selected(self, si, output) {
                continue;
            }
            if self.surfaces[si].alive {
                continue;
            }

            let name = self.surfaces[si].output_name.as_deref().unwrap_or("(unknown)");
            el::warn!(
                "wayland.surface.resurrect si={si} name={name}",
                si = si as i64,
                name = name
            );

            let out = self.surfaces[si]._output.clone();
            let (surface, layer) = create_layer_surface(&compositor, &layer_shell, &self.qh, &out)?;

            let s = &mut self.surfaces[si];
            s.surface = surface;
            s.layer = layer;
            s.alive = true;

            // reset state; compositor will re-configure
            s.width = 0;
            s.height = 0;
            s.configured = false;
            s.stride = 0;
            s.size_bytes = 0;
            s.buffers = DoubleBuffer::default();
            s.has_image = false;
            s.last_frame = None;

            s.frame_callback_ok = false;
            s.frame_pending = false;
            s.frame_cb = None;

            el::info!("wayland.surface.resurrected si={si}", si = si as i64);
        }

        Ok(())
    }

    fn wait_for_configured(&mut self) -> Result<()> {
        let start = Instant::now();

        for attempt in 0..10 {
            self.roundtrip()?;

            let alive: Vec<&SurfaceState> = self.surfaces.iter().filter(|s| s.alive).collect();
            if alive.is_empty() {
                return Ok(());
            }

            let all_configured = alive
                .iter()
                .all(|s| s.configured && s.width > 0 && s.height > 0);

            if all_configured {
                el::info!(
                    "wayland.wait_for_configured ok elapsed_ms={ms}",
                    ms = start.elapsed().as_millis() as i64
                );
                return Ok(());
            }

            if attempt == 5 {
                let alive_count = self.surfaces.iter().filter(|s| s.alive).count();
                let configured_count = self
                    .surfaces
                    .iter()
                    .filter(|s| s.alive && s.configured && s.width > 0 && s.height > 0)
                    .count();

                el::warn!(
                    "wayland.wait_for_configured still_waiting attempt={attempt} alive={alive} configured={configured} elapsed_ms={ms}",
                    attempt = attempt as i64,
                    alive = alive_count as i64,
                    configured = configured_count as i64,
                    ms = start.elapsed().as_millis() as i64
                );
            }

            let sleep_ms = 20 + (attempt * 5);
            std::thread::sleep(Duration::from_millis(sleep_ms.min(100)));
        }

        let any_configured = self
            .surfaces
            .iter()
            .filter(|s| s.alive)
            .any(|s| s.configured && s.width > 0 && s.height > 0);

        if !any_configured && self.surfaces.iter().any(|s| s.alive) {
            let mut names = String::new();
            for s in self.surfaces.iter().filter(|s| s.alive) {
                let n = s.output_name.as_deref().unwrap_or("(unknown)");
                names.push_str(&format!("{n}(cfg={} {}x{}) ", s.configured, s.width, s.height));
            }

            el::error!(
                "wayland.wait_for_configured failed elapsed_ms={ms} surfaces={surfaces}",
                ms = start.elapsed().as_millis() as i64,
                surfaces = names
            );

            bail!("No alive surfaces configured after waiting");
        }

        Ok(())
    }

    fn ensure_buffers_for_all_surfaces(&mut self) -> Result<()> {
        let shm = self.shm.as_ref().context("wl_shm missing")?.clone();
        let qh = self.qh.clone();

        for (si, s) in self.surfaces.iter_mut().enumerate() {
            if !s.alive {
                continue;
            }
            if !s.configured || s.width == 0 || s.height == 0 {
                continue;
            }
            ensure_buffers_for_surface_indexed(&qh, &shm, si, s)?;
        }

        Ok(())
    }

    pub fn apply(&mut self, spec: Spec) -> Result<()> {
        let _ = crate::wallpaper::cache::write_last_applied(&spec);

        let target_output: Option<&str> = match &spec {
            Spec::Image { output, .. } => output.as_deref(),
            Spec::Colour { output, .. } => output.as_deref(),
        };

        el::info!(
            "wayland.apply begin kind={kind} output={output}",
            kind = match &spec {
                Spec::Image { .. } => "image",
                Spec::Colour { .. } => "colour",
            },
            output = target_output.unwrap_or("(all)")
        );

        self.ensure_surfaces()?;
        self.ensure_selected_surfaces_alive(target_output)?;
        self.wait_for_configured()?;

        match &spec {
            Spec::Colour {
                colour,
                transition,
                output,
                ..
            } => {
                self.ensure_buffers_for_all_surfaces()?;
                let out = output.as_deref();

                el::info!(
                    "wayland.apply colour output={output} transition={transition} duration={ms} rgb={r},{g},{b}",
                    output = out.unwrap_or("(all)"),
                    transition = match transition.kind {
                        crate::spec::Transition::None => "none",
                        crate::spec::Transition::Fade => "fade",
                        crate::spec::Transition::Wipe => "wipe",
                    },
                    ms = transition.duration as i64,
                    r = colour.r as i64,
                    g = colour.g as i64,
                    b = colour.b as i64
                );

                match transition.kind {
                    crate::spec::Transition::None => {
                        crate::wallpaper::colour::apply_solid_on(self, *colour, out)?
                    }
                    crate::spec::Transition::Fade => crate::wallpaper::colour::transition_to_on(
                        self,
                        *colour,
                        crate::spec::Transition::Fade,
                        transition.duration,
                        out,
                    )?,
                    crate::spec::Transition::Wipe => crate::wallpaper::colour::transition_to_on(
                        self,
                        *colour,
                        crate::spec::Transition::Wipe,
                        transition.duration,
                        out,
                    )?,
                }
            }

            Spec::Image { .. } => {
                crate::wallpaper::image::apply_image(self, &spec)?;
            }
        }

        self.current = Some(spec);
        el::info!("wayland.apply done");
        Ok(())
    }

    pub fn unset(&mut self, output: Option<&str>) -> Result<()> {
        let out_s = output.unwrap_or("(all)");

        el::scope!(
            "wayland.unset",
            success = "done",
            failure = "failed",
            aborted = "aborted",
        {
            self.ensure_surfaces()?;
            self.wait_for_configured()?;

            if let Some(want) = output {
                let found = self
                    .surfaces
                    .iter()
                    .any(|s| s.output_name.as_deref() == Some(want));
                if !found {
                    bail!("unknown output '{want}' (no wl_output.name match yet)");
                }
            }

            el::info!("wayland.unset begin output={output}", output = out_s);

            let mut any = false;

            for si in 0..self.surfaces.len() {
                if !surface_selected(self, si, output) {
                    continue;
                }

                let name = self.surfaces[si]
                    .output_name
                    .as_deref()
                    .unwrap_or("(unknown)")
                    .to_string();

                if output.is_some() {
                    let s = &mut self.surfaces[si];

                    if s.alive {
                        el::info!(
                            "wayland.unset destroy si={si} name={name}",
                            si = si as i64,
                            name = name.as_str()
                        );
                        s.layer.destroy();
                        s.surface.destroy();
                    }

                    s.alive = false;
                    s.configured = false;
                    s.width = 0;
                    s.height = 0;
                    s.stride = 0;
                    s.size_bytes = 0;
                    s.buffers = DoubleBuffer::default();
                    s.has_image = false;
                    s.last_frame = None;

                    s.frame_callback_ok = false;
                    s.frame_pending = false;
                    s.frame_cb = None;

                    any = true;
                } else {
                    let s = &mut self.surfaces[si];

                    el::info!(
                        "wayland.unset clear_state si={si} name={name}",
                        si = si as i64,
                        name = name.as_str()
                    );

                    s.has_image = false;
                    s.last_frame = None;

                    // Clear any stuck pacing state too.
                    s.frame_callback_ok = false;
                    s.frame_pending = false;
                    s.frame_cb = None;

                    any = true;
                }
            }

            self._conn.flush().context("flush")?;
            let _ = self.dispatch_pending()?; // now returns usize

            if output.is_none() {
                self.current = None;
            }

            if !any && !self.surfaces.is_empty() {
                bail!("no outputs matched for unset");
            }

            Ok::<(), anyhow::Error>(())
        })
    }

    pub fn stop(&mut self) -> Result<()> {
        el::info!("wayland.stop");
        self.current = None;
        self.surfaces.clear();
        Ok(())
    }

    pub fn current(&self) -> Option<&Spec> {
        self.current.as_ref()
    }

    pub fn running(&self) -> bool {
        self.current.is_some()
    }
}

/* ---------- helpers ---------- */

fn create_layer_surface(
    compositor: &WlCompositor,
    layer_shell: &ZwlrLayerShellV1,
    qh: &QueueHandle<Engine>,
    out: &WlOutput,
) -> Result<(WlSurface, ZwlrLayerSurfaceV1)> {
    let surface = compositor.create_surface(qh, ());

    // Default input region is the full surface; that steals pointer clicks from the compositor/root.
    let empty_region = compositor.create_region(qh, ());
    surface.set_input_region(Some(&empty_region));
    // drop(empty_region) is fine; Wayland keeps the object alive as needed.

    let layer = layer_shell.get_layer_surface(
        &surface,
        Some(out),
        Layer::Background,
        "gesso".into(),
        qh,
        (),
    );

    layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
    layer.set_size(0, 0);

    // For wallpapers you typically want 0 (don’t reserve space).
    // -1 can be interpreted as "exclusive sized to surface" in some compositors and can be weird.
    layer.set_exclusive_zone(0);

    layer.set_keyboard_interactivity(KeyboardInteractivity::None);

    surface.commit();
    Ok((surface, layer))
}

/* ---------- Selection helpers ---------- */

pub(crate) fn surface_usable(engine: &Engine, i: usize) -> bool {
    let s = &engine.surfaces[i];
    s.alive && s.configured && s.width != 0 && s.height != 0
}

pub(crate) fn surface_selected(engine: &Engine, i: usize, output: Option<&str>) -> bool {
    let Some(want) = output else { return true };
    engine.surfaces[i].output_name.as_deref() == Some(want)
}

/* ---------- Shared helpers used by colour.rs + image.rs ---------- */

pub(crate) fn wait_for_free_buffer_idx(engine: &mut Engine, i: usize) -> Result<()> {
    // Key rule: DO NOT let requests hang forever.
    // We avoid libc poll() by pumping dispatch_pending() in bounded time.

    const WARN_AFTER: Duration = Duration::from_millis(250);
    const DISABLE_FRAME_CB_AFTER: Duration = Duration::from_millis(200);
    const HARD_BAIL_AFTER: Duration = Duration::from_millis(1500);

    let start = Instant::now();
    let mut warned = false;

    loop {
        let elapsed = start.elapsed();

        // Hard bail: do not let ANY request hang forever.
        if elapsed >= HARD_BAIL_AFTER {
            let name = engine.surfaces[i].output_name.as_deref().unwrap_or("(unknown)");
            el::error!(
                "wayland.wait_for_free_buffer hard_bail si={si} name={name} elapsed_ms={ms} frame_pending={fp} frame_cb_ok={ok} busy={busy}",
                si = i as i64,
                name = name,
                ms = elapsed.as_millis() as i64,
                fp = engine.surfaces[i].frame_pending,
                ok = engine.surfaces[i].frame_callback_ok,
                busy = engine.surfaces[i].buffers.current_is_busy()
            );

            // Clear stuck pacing state so we can keep serving requests.
            {
                let s = &mut engine.surfaces[i];
                s.frame_pending = false;
                s.frame_cb = None;
                s.frame_callback_ok = false;
            }

            // Return Ok so callers proceed. Worst case: we skip perfect pacing,
            // but we DO NOT freeze the daemon/client.
            return Ok(());
        }

        // Prefer a free buffer if possible.
        {
            let s = &mut engine.surfaces[i];
            if s.buffers.current_is_busy() {
                s.buffers.swap_to_free();
            }

            let ready = if s.frame_callback_ok {
                !s.buffers.current_is_busy() && !s.frame_pending
            } else {
                !s.buffers.current_is_busy()
            };

            if ready {
                if warned {
                    el::info!(
                        "wayland.wait_for_free_buffer ok si={si} elapsed_ms={ms}",
                        si = i as i64,
                        ms = elapsed.as_millis() as i64
                    );
                }
                return Ok(());
            }
        }

        // Disable frame-callback pacing if it looks stuck.
        if elapsed >= DISABLE_FRAME_CB_AFTER {
            let disabled = {
                let s = &mut engine.surfaces[i];
                if s.frame_callback_ok && s.frame_pending {
                    s.frame_callback_ok = false;
                    s.frame_pending = false;
                    s.frame_cb = None;
                    true
                } else {
                    false
                }
            };

            if disabled {
                let name = engine.surfaces[i].output_name.as_deref().unwrap_or("(unknown)");
                el::warn!(
                    "wayland.frame_callback.disabled si={si} name={name} elapsed_ms={ms}",
                    si = i as i64,
                    name = name,
                    ms = elapsed.as_millis() as i64
                );
            }
        }

        if !warned && elapsed >= WARN_AFTER {
            warned = true;
            let name = engine.surfaces[i].output_name.as_deref().unwrap_or("(unknown)");
            el::warn!(
                "wayland.wait_for_free_buffer blocked si={si} name={name} frame_pending={fp} frame_cb_ok={ok} busy={busy}",
                si = i as i64,
                name = name,
                fp = engine.surfaces[i].frame_pending,
                ok = engine.surfaces[i].frame_callback_ok,
                busy = engine.surfaces[i].buffers.current_is_busy()
            );
        }

        // Pump events without blocking forever.        
        engine._conn.flush().context("flush")?;
        engine.dispatch_with_timeout(Duration::from_millis(16))?;
        std::thread::sleep(Duration::from_millis(1));
    }
}

pub(crate) fn commit_surface(
    qh: &wayland_client::QueueHandle<Engine>,
    s: &mut SurfaceState,
    surface_index: usize,
) {
    // Always request a frame callback if one is not already pending.
    // We only *pace* on callbacks when frame_callback_ok == true.
    if !s.frame_pending {
        let cb = s.surface.frame(qh, surface_index);
        s.frame_cb = Some(cb);
        s.frame_pending = true;
    }

    if let Some(buf) = s.buffers.current_buffer() {
        s.surface.attach(Some(buf), 0, 0);
        s.surface.damage_buffer(0, 0, s.width as i32, s.height as i32);
        s.surface.commit();

        s.buffers.mark_current_busy();
        s.buffers.swap();
    } else {
        el::warn!(
            "wayland.commit_surface missing_buffer si={si} name={name}",
            si = surface_index as i64,
            name = s.output_name.as_deref().unwrap_or("(unknown)")
        );
    }
}

pub(crate) fn paint_frame_u32(s: &mut SurfaceState, frame: &[u32]) {
    let Some(mmap) = s.buffers.current_mmap_mut() else { return };
    let len = mmap.len() / 4;
    let dst = unsafe { std::slice::from_raw_parts_mut(mmap.as_mut_ptr() as *mut u32, len) };
    let n = dst.len().min(frame.len());
    dst[..n].copy_from_slice(&frame[..n]);
}

/* ---------- Buffer management ---------- */

pub(crate) fn ensure_buffers_for_surface_indexed(
    qh: &QueueHandle<Engine>,
    shm: &WlShm,
    surface_index: usize,
    s: &mut SurfaceState,
) -> Result<()> {
    let width = s.width as usize;
    let height = s.height as usize;
    let stride = (width * 4) as i32;
    let size_bytes = (stride as usize) * height;

    let needs_recreate = !s.buffers.both_ready() || s.size_bytes != size_bytes || s.stride != stride;
    if !needs_recreate {
        return Ok(());
    }

    el::info!(
        "wayland.buffers.recreate si={si} w={w} h={h} stride={stride} bytes={bytes}",
        si = surface_index as i64,
        w = s.width as i64,
        h = s.height as i64,
        stride = stride as i64,
        bytes = size_bytes as i64
    );

    // Clear pacing state too.
    s.frame_pending = false;
    s.frame_cb = None;
    s.frame_callback_ok = false;

    s.buffers = DoubleBuffer::default();

    create_one_buffer(qh, shm, surface_index, s, 0, size_bytes, stride)?;
    create_one_buffer(qh, shm, surface_index, s, 1, size_bytes, stride)?;

    s.stride = stride;
    s.size_bytes = size_bytes;

    Ok(())
}

fn create_one_buffer(
    qh: &QueueHandle<Engine>,
    shm: &WlShm,
    surface_index: usize,
    s: &mut SurfaceState,
    which: usize,
    size_bytes: usize,
    stride: i32,
) -> Result<()> {
    let file = tempfile().context("tempfile for shm")?;
    file.set_len(size_bytes as u64).context("set_len shm file")?;
    let mmap = unsafe { MmapMut::map_mut(&file).context("mmap shm")? };

    let pool = shm.create_pool(file.as_fd(), size_bytes as i32, qh, ());
    let buffer = pool.create_buffer(
        0,
        s.width as i32,
        s.height as i32,
        stride,
        wl_shm::Format::Xrgb8888,
        qh,
        (surface_index, which),
    );

    let target = s.buffers.slot_mut(which);
    target._file = Some(file);
    target.mmap = Some(mmap);
    target._pool = Some(pool);
    target.buffer = Some(buffer);
    target.busy = false;

    el::debug!(
        "wayland.buffer.created si={si} which={which} bytes={bytes}",
        si = surface_index as i64,
        which = which as i64,
        bytes = size_bytes as i64
    );

    Ok(())
}

/* ---------- Dispatch ---------- */

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Engine {
    fn event(
        _state: &mut Engine,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Engine>,
    ) {
        // ignore
    }
}

impl Dispatch<WlOutput, usize> for Engine {
    fn event(
        state: &mut Engine,
        _proxy: &WlOutput,
        event: wl_output::Event,
        data: &usize,
        _conn: &Connection,
        _qh: &QueueHandle<Engine>,
    ) {
        let idx = *data;
        if idx >= state.outputs.len() {
            return;
        }

        match event {
            wl_output::Event::Name { name } => {
                let was = state.outputs[idx].name.is_some();
                state.outputs[idx].name = Some(name.clone());
                if !was {
                    el::info!(
                        "wayland.output.name idx={idx} name={name}",
                        idx = idx as i64,
                        name = name
                    );
                }
            }
            wl_output::Event::Description { description } => {
                let was = state.outputs[idx].description.is_some();
                state.outputs[idx].description = Some(description.clone());
                if !was {
                    el::info!(
                        "wayland.output.description idx={idx} description={desc}",
                        idx = idx as i64,
                        desc = description
                    );
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Engine {
    fn event(
        state: &mut Engine,
        proxy: &ZwlrLayerSurfaceV1,
        event: <ZwlrLayerSurfaceV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Engine>,
    ) {
        use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Event as E;

        match event {
            E::Configure { serial, width, height } => {
                if let Some(s) = state
                    .surfaces
                    .iter_mut()
                    .find(|s| s.alive && s.layer == *proxy)
                {
                    s.width = width;
                    s.height = height;
                    s.configured = true;

                    el::info!(
                        "wayland.surface.configure name={name} w={w} h={h}",
                        name = s.output_name.as_deref().unwrap_or("(unknown)"),
                        w = width as i64,
                        h = height as i64
                    );

                    s.layer.ack_configure(serial);
                    s.surface.commit();
                }
            }
            E::Closed => {
                if let Some(s) = state
                    .surfaces
                    .iter_mut()
                    .find(|s| s.alive && s.layer == *proxy)
                {
                    s.configured = false;

                    // Closed surfaces should not retain stuck pacing state.
                    s.frame_pending = false;
                    s.frame_cb = None;
                    s.frame_callback_ok = false;

                    el::warn!(
                        "wayland.surface.closed name={name}",
                        name = s.output_name.as_deref().unwrap_or("(unknown)")
                    );
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlBuffer, (usize, usize)> for Engine {
    fn event(
        state: &mut Engine,
        _proxy: &WlBuffer,
        event: wl_buffer::Event,
        data: &(usize, usize),
        _conn: &Connection,
        _qh: &QueueHandle<Engine>,
    ) {
        if let wl_buffer::Event::Release = event {
            let (si, which) = *data;
            if let Some(s) = state.surfaces.get_mut(si) {
                s.buffers.mark_free(which);
            }
        }
    }
}

impl Dispatch<WlCallback, usize> for Engine {
    fn event(
        state: &mut Engine,
        _proxy: &WlCallback,
        event: wl_callback::Event,
        data: &usize,
        _conn: &Connection,
        _qh: &QueueHandle<Engine>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            if let Some(s) = state.surfaces.get_mut(*data) {
                s.frame_pending = false;
                s.frame_cb = None; // drop only after Done
                s.frame_tick = s.frame_tick.wrapping_add(1);

                // If we got a callback, consider callbacks working again.
                s.frame_callback_ok = true;
            }
        }
    }
}

wayland_client::delegate_noop!(Engine: ignore WlCompositor);
wayland_client::delegate_noop!(Engine: ignore WlShm);
wayland_client::delegate_noop!(Engine: ignore ZwlrLayerShellV1);
wayland_client::delegate_noop!(Engine: ignore WlSurface);
wayland_client::delegate_noop!(Engine: ignore WlShmPool);
wayland_client::delegate_noop!(Engine: ignore WlRegion);
