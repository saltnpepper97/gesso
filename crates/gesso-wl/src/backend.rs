// Author: Dustin Pilgrim
// License: MIT

use crate::error::{WlError, WlResult};
use crate::health::HealthReport;
use crate::outputs::{Outputs, OutputInfo};
use crate::present::Presenter;
use crate::state::WlState;

use eventline::{debug, info, scope, warn};
use wayland_client::{Connection, EventQueue};

#[derive(Debug, Clone)]
pub struct PresentSpec<'a> {
    pub output: &'a str,
    pub width: u32,
    pub height: u32,
    pub stride: usize,      // must be width*4
    pub xrgb8888: &'a [u8], // B,G,R,0
}

pub struct WlBackend {
    conn: Connection,
    queue: EventQueue<WlState>,
    state: WlState,
    outputs: Outputs,
    presenter: Presenter,
}

impl WlBackend {
    pub fn connect() -> WlResult<Self> {
        scope!("gesso-wl.connect", {
            info!("connecting to wayland");

            let conn = Connection::connect_to_env()
                .map_err(|e| WlError::Connect(e.to_string()))?;

            let mut queue = conn.new_event_queue();
            let qh = queue.handle();

            let mut state = WlState::new(&conn, &qh)?;
            queue
                .roundtrip(&mut state)
                .map_err(|e| WlError::Protocol(e.to_string()))?;

            debug!("wayland globals bound");

            let outputs = Outputs::from_state(&state);
            if outputs.list().is_empty() {
                warn!("no outputs discovered (yet)");
            }

            let presenter = Presenter::new(&state, &qh)?;

            Ok(Self { conn, queue, state, outputs, presenter })
        })
    }

    /// Non-blocking dispatch. Call this at the top of every loop iteration.
    pub fn dispatch(&mut self) -> WlResult<()> {
        self.conn
            .flush()
            .map_err(|e| WlError::Protocol(e.to_string()))?;
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(|e| WlError::Protocol(e.to_string()))?;
        self.outputs = Outputs::from_state(&self.state);
        Ok(())
    }

    /// Blocking dispatch — only for startup / configure wait.
    pub fn blocking_dispatch(&mut self) -> WlResult<()> {
        self.conn
            .flush()
            .map_err(|e| WlError::Protocol(e.to_string()))?;
        self.queue
            .blocking_dispatch(&mut self.state)
            .map_err(|e| WlError::Protocol(e.to_string()))?;
        self.outputs = Outputs::from_state(&self.state);
        Ok(())
    }

    pub fn roundtrip(&mut self) -> WlResult<()> {
        self.queue
            .roundtrip(&mut self.state)
            .map_err(|e| WlError::Protocol(e.to_string()))?;
        self.outputs = Outputs::from_state(&self.state);
        Ok(())
    }

    pub fn outputs(&self) -> Vec<OutputInfo> {
        self.outputs.list()
    }

    /// Render directly into shm via a closure. The compositor's frame callback
    /// paces delivery naturally — call this every loop iteration.
    ///
    /// Returns:
    ///   `Ok(true)`  — frame committed to compositor.
    ///   `Ok(false)` — compositor not ready yet; skip this tick, try again next.
    ///   `Err(_)`    — real error (bad output name, protocol fault, etc.).
    pub fn present_rendered(
        &mut self,
        output: &str,
        width: u32,
        height: u32,
        mut render: impl FnMut(&mut [u8]) -> WlResult<()>,
    ) -> WlResult<bool> {
        let out_name = output.to_string();
        let qh = self.queue.handle();
        let stride = width as usize * 4;

        let result = {
            let out = self
                .outputs
                .by_name(&out_name)
                .ok_or_else(|| WlError::UnknownOutput(out_name.clone()))?;

            self.presenter.render_present_xrgb8888(
                &mut self.state,
                &qh,
                out,
                width,
                height,
                stride,
                |dst| render(dst),
            )
        };

        match result {
            Ok(presented) => Ok(presented),

            Err(WlError::Protocol(ref msg))
                if msg.contains("not configured yet") =>
            {
                self.roundtrip()?;

                let out = self
                    .outputs
                    .by_name(&out_name)
                    .ok_or_else(|| WlError::UnknownOutput(out_name.clone()))?;

                match self.presenter.render_present_xrgb8888(
                    &mut self.state,
                    &qh,
                    out,
                    width,
                    height,
                    stride,
                    |dst| render(dst),
                ) {
                    Ok(presented) => Ok(presented),
                    Err(WlError::Protocol(_)) => Ok(false),
                    Err(e) => Err(e),
                }
            }

            Err(e) => Err(e),
        }
    }

    /// Compatibility shim: copies a pre-rendered slice into shm.
    pub fn present(&mut self, spec: PresentSpec<'_>) -> WlResult<bool> {
        let out_name = spec.output.to_string();
        let qh = self.queue.handle();

        let result = {
            let out = self
                .outputs
                .by_name(&out_name)
                .ok_or_else(|| WlError::UnknownOutput(out_name.clone()))?;

            self.presenter.present_xrgb8888(
                &mut self.state,
                &qh,
                out,
                spec.width,
                spec.height,
                spec.stride,
                spec.xrgb8888,
            )
        };

        match result {
            Ok(presented) => Ok(presented),

            Err(WlError::Protocol(ref msg))
                if msg.contains("not configured yet") =>
            {
                self.roundtrip()?;

                let out = self
                    .outputs
                    .by_name(&out_name)
                    .ok_or_else(|| WlError::UnknownOutput(out_name.clone()))?;

                match self.presenter.present_xrgb8888(
                    &mut self.state,
                    &qh,
                    out,
                    spec.width,
                    spec.height,
                    spec.stride,
                    spec.xrgb8888,
                ) {
                    Ok(presented) => Ok(presented),
                    Err(WlError::Protocol(_)) => Ok(false),
                    Err(e) => Err(e),
                }
            }

            Err(e) => Err(e),
        }
    }

    pub fn unset(&mut self, output: &str) -> WlResult<()> {
        let out = self
            .outputs
            .by_name(output)
            .ok_or_else(|| WlError::UnknownOutput(output.to_string()))?;
        self.presenter.unset(out);
        Ok(())
    }

    /// Drop the SHM pixel buffers for an idle output, reclaiming RSS.
    ///
    /// The layer surface is kept alive so the next present doesn't need to
    /// wait for a compositor configure round-trip. Only the two pixel mmaps
    /// (~16 MB per 1080p output) are freed. `Presenter` will reallocate them
    /// transparently on the next render call.
    ///
    /// Safe to call every idle tick — it is a no-op if both buffers are
    /// already freed or still held by the compositor.
    pub fn release_buffers(&mut self, output: &str) {
        if let Some(out) = self.outputs.by_name(output) {
            self.presenter.release_buffers(out);
        }
    }

    pub fn health(&self) -> HealthReport {
        HealthReport::from_state(&self.state)
    }
}
