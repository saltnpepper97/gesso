use crate::error::{WlError, WlResult};

use eventline::{debug, warn};
use std::collections::HashMap;
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum,
    protocol::{
        wl_buffer,
        wl_callback,
        wl_compositor,
        wl_output,
        wl_region,
        wl_registry,
        wl_shm,
        wl_shm_pool,
        wl_surface,
    },
};

use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1,
    zwlr_layer_surface_v1,
};

// NEW: xdg-output v1 (gives DP-1 / HDMI-A-1 names)
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1,
    zxdg_output_v1,
};

#[derive(Debug, Clone)]
pub struct OutputRaw {
    pub global_name: u32,
    pub width: u32,
    pub height: u32,
    pub scale: u32,

    // NEW: compositor-provided name (e.g. "DP-1")
    pub name: Option<String>,
}

pub struct WlState {
    pub registry: wl_registry::WlRegistry,
    pub compositor: Option<wl_compositor::WlCompositor>,
    pub shm: Option<wl_shm::WlShm>,
    pub layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,

    // NEW: xdg-output manager (optional)
    pub xdg_output_manager: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
    // NEW: keep xdg_output proxies alive (per output global)
    pub xdg_outputs: HashMap<u32, zxdg_output_v1::ZxdgOutputV1>,

    // output wl_global -> last configure serial
    pub layer_configure_serial: HashMap<u32, u32>,

    pub shm_formats: Vec<u32>,

    // key: registry global name for wl_output
    pub outputs: HashMap<u32, (wl_output::WlOutput, OutputRaw)>,

    // (out_global, which) releases we observed since last drain
    pub buffer_released: Vec<(u32, usize)>,

    // Frame pacing:
    // out_global -> ready to render the next frame (wl_surface.frame callback fired)
    pub frame_ready: HashMap<u32, bool>,

    // IMPORTANT: keep wl_callback proxies alive until Done arrives.
    // If dropped early, compositor may never deliver Done and you stall/jitter.
    pub frame_callbacks: HashMap<u32, wl_callback::WlCallback>,
}

impl WlState {
    pub fn new(conn: &Connection, qh: &QueueHandle<Self>) -> WlResult<Self> {
        let display = conn.display();
        let registry = display.get_registry(qh, ());

        Ok(Self {
            registry,
            compositor: None,
            shm: None,
            layer_shell: None,

            xdg_output_manager: None,
            xdg_outputs: HashMap::new(),

            layer_configure_serial: HashMap::new(),
            shm_formats: Vec::new(),
            outputs: HashMap::new(),
            buffer_released: Vec::new(),
            frame_ready: HashMap::new(),
            frame_callbacks: HashMap::new(),
        })
    }

    pub fn require_compositor(&self) -> WlResult<&wl_compositor::WlCompositor> {
        self.compositor
            .as_ref()
            .ok_or(WlError::MissingGlobal("wl_compositor"))
    }

    pub fn require_shm(&self) -> WlResult<&wl_shm::WlShm> {
        self.shm.as_ref().ok_or(WlError::MissingGlobal("wl_shm"))
    }

    pub fn require_layer_shell(&self) -> WlResult<&zwlr_layer_shell_v1::ZwlrLayerShellV1> {
        self.layer_shell
            .as_ref()
            .ok_or(WlError::MissingGlobal("zwlr_layer_shell_v1"))
    }

    /// Drain release notifications (LIFO is fine).
    pub fn drain_releases(&mut self) -> impl Iterator<Item = (u32, usize)> + '_ {
        std::iter::from_fn(move || self.buffer_released.pop())
    }

    /// Is the compositor ready for us to render the next frame on this output?
    #[inline]
    pub fn is_frame_ready(&self, out_global: u32) -> bool {
        // default to true so first frame can render without waiting
        self.frame_ready.get(&out_global).copied().unwrap_or(true)
    }

    /// Mark that we are waiting on a frame callback for this output.
    #[inline]
    pub fn mark_frame_pending(&mut self, out_global: u32) {
        self.frame_ready.insert(out_global, false);
    }

    /// Mark that the frame callback fired and we can render again.
    #[inline]
    pub fn mark_frame_ready(&mut self, out_global: u32) {
        self.frame_ready.insert(out_global, true);
    }

    /// Remove all state for an output.
    fn drop_output_state(&mut self, out_global: u32) {
        self.outputs.remove(&out_global);
        self.layer_configure_serial.remove(&out_global);
        self.frame_ready.remove(&out_global);
        self.frame_callbacks.remove(&out_global);

        // Drop xdg-output proxy if present.
        self.xdg_outputs.remove(&out_global);

        // Also drop any queued releases for this output.
        self.buffer_released.retain(|(og, _)| *og != out_global);
    }

    // --- xdg-output wiring helpers ---

    fn ensure_xdg_for_output(&mut self, qh: &QueueHandle<Self>, out_global: u32) {
        if self.xdg_output_manager.is_none() {
            return;
        }
        if self.xdg_outputs.contains_key(&out_global) {
            return;
        }
        let Some((wl_out, _raw)) = self.outputs.get(&out_global) else {
            return;
        };
        let mgr = self.xdg_output_manager.as_ref().unwrap();
        let xdg = mgr.get_xdg_output(wl_out, qh, out_global);
        self.xdg_outputs.insert(out_global, xdg);
    }

    fn ensure_xdg_for_all_outputs(&mut self, qh: &QueueHandle<Self>) {
        if self.xdg_output_manager.is_none() {
            return;
        }
        let keys: Vec<u32> = self.outputs.keys().copied().collect();
        for og in keys {
            self.ensure_xdg_for_output(qh, og);
        }
    }
}

// --- wl_registry: bind globals ---
impl Dispatch<wl_registry::WlRegistry, ()> for WlState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global { name, interface, version } => {
                debug!("wl global discovered");

                match interface.as_str() {
                    "wl_compositor" => {
                        state.compositor = Some(registry.bind(name, version.min(4), qh, ()));
                    }
                    "wl_shm" => {
                        state.shm = Some(registry.bind(name, version.min(1), qh, ()));
                    }
                    "wl_output" => {
                        let wl_out: wl_output::WlOutput =
                            registry.bind(name, version.min(3), qh, name);

                        state.outputs.insert(
                            name,
                            (
                                wl_out,
                                OutputRaw {
                                    global_name: name,
                                    width: 1,
                                    height: 1,
                                    scale: 1,
                                    name: None,
                                },
                            ),
                        );

                        // allow first frame immediately
                        state.frame_ready.insert(name, true);

                        // If xdg-output manager already exists, create per-output xdg object.
                        state.ensure_xdg_for_output(qh, name);
                    }
                    "zwlr_layer_shell_v1" => {
                        state.layer_shell = Some(registry.bind(name, version.min(4), qh, ()));
                    }
                    // NEW: xdg-output manager
                    "zxdg_output_manager_v1" => {
                        state.xdg_output_manager =
                            Some(registry.bind(name, version.min(3), qh, ()));
                        // Outputs may already exist; create xdg objects now.
                        state.ensure_xdg_for_all_outputs(qh);
                    }
                    _ => {}
                }
            }
            wl_registry::Event::GlobalRemove { name } => {
                debug!("wl global removed");
                state.drop_output_state(name);
            }
            _ => {}
        }
    }
}

// --- wl_shm formats ---
impl Dispatch<wl_shm::WlShm, ()> for WlState {
    fn event(
        state: &mut Self,
        _proxy: &wl_shm::WlShm,
        event: wl_shm::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_shm::Event::Format { format } = event {
            let raw: u32 = match format {
                WEnum::Value(v) => v.into(),
                WEnum::Unknown(u) => u,
            };
            state.shm_formats.push(raw);
        }
    }
}

// --- wl_output events ---
impl Dispatch<wl_output::WlOutput, u32> for WlState {
    fn event(
        state: &mut Self,
        _proxy: &wl_output::WlOutput,
        event: wl_output::Event,
        global_name: &u32,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let Some((_o, raw)) = state.outputs.get_mut(global_name) {
            match event {
                wl_output::Event::Mode { width, height, .. } => {
                    raw.width = (width as u32).max(1);
                    raw.height = (height as u32).max(1);
                }
                wl_output::Event::Scale { factor } => {
                    raw.scale = (factor as u32).max(1);
                }
                _ => {}
            }
        } else {
            warn!("wl_output event for unknown output");
        }
    }
}

// --- xdg-output manager (no events needed) ---
impl Dispatch<zxdg_output_manager_v1::ZxdgOutputManagerV1, ()> for WlState {
    fn event(
        _: &mut Self,
        _: &zxdg_output_manager_v1::ZxdgOutputManagerV1,
        _: zxdg_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {}
}

// --- xdg-output per-output events (where DP-1 arrives) ---
impl Dispatch<zxdg_output_v1::ZxdgOutputV1, u32> for WlState {
    fn event(
        state: &mut Self,
        _proxy: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        out_global: &u32,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let Some((_o, raw)) = state.outputs.get_mut(out_global) {
            match event {
                zxdg_output_v1::Event::Name { name } => {
                    raw.name = Some(name);
                }
                zxdg_output_v1::Event::Description { .. } => {}
                zxdg_output_v1::Event::LogicalSize { .. } => {}
                zxdg_output_v1::Event::LogicalPosition { .. } => {}
                zxdg_output_v1::Event::Done => {}
                _ => {}
            }
        }
    }
}

// --- REQUIRED no-op dispatch impls for created objects ---

impl Dispatch<wl_surface::WlSurface, ()> for WlState {
    fn event(
        _: &mut Self,
        _: &wl_surface::WlSurface,
        _: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {}
}

impl Dispatch<wl_region::WlRegion, ()> for WlState {
    fn event(
        _: &mut Self,
        _: &wl_region::WlRegion,
        _: wl_region::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {}
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for WlState {
    fn event(
        _: &mut Self,
        _: &wl_shm_pool::WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {}
}

// IMPORTANT: wl_buffer release pacing
impl Dispatch<wl_buffer::WlBuffer, (u32, usize)> for WlState {
    fn event(
        state: &mut Self,
        _proxy: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        data: &(u32, usize),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            state.buffer_released.push(*data);
        }
    }
}

// Frame callback pacing (wl_surface.frame)
// IMPORTANT: we keep callback proxies alive in state.frame_callbacks until Done.
impl Dispatch<wl_callback::WlCallback, u32> for WlState {
    fn event(
        state: &mut Self,
        _proxy: &wl_callback::WlCallback,
        event: wl_callback::Event,
        out_global: &u32,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.mark_frame_ready(*out_global);

            // Drop the stored callback proxy now that Done fired.
            state.frame_callbacks.remove(out_global);
        }
    }
}

impl Dispatch<zwlr_layer_shell_v1::ZwlrLayerShellV1, ()> for WlState {
    fn event(
        _: &mut Self,
        _: &zwlr_layer_shell_v1::ZwlrLayerShellV1,
        _: zwlr_layer_shell_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {}
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, u32> for WlState {
    fn event(
        state: &mut Self,
        proxy: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        out_global: &u32,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                proxy.ack_configure(serial);
                state.layer_configure_serial.insert(*out_global, serial);

                // After configure, allow a frame immediately.
                state.mark_frame_ready(*out_global);
            }
            zwlr_layer_surface_v1::Event::Closed => {
                warn!("layer surface closed");
                state.layer_configure_serial.remove(out_global);
                state.frame_ready.remove(out_global);
                state.frame_callbacks.remove(out_global);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for WlState {
    fn event(
        _: &mut Self,
        _: &wl_compositor::WlCompositor,
        _: wl_compositor::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {}
}
