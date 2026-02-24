// Author: Dustin Pilgrim
// License: MIT

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eventline::{info, scope};

use gesso_core::RenderEngine;
use gesso_ipc::protocol as ipc;
use gesso_wl::WlBackend;

use crate::daemon::gif_player::GifPlayer;
use crate::daemon::ipc::handle_request;
use crate::daemon::persist::load_state;
use crate::daemon::restore::apply_persisted_state;
use crate::daemon::types::PersistedSet;
use crate::daemon::wayland::{wayland_socket_alive, wayland_socket_path};

/// Wait (briefly) for compositor-provided output names (DP-1 / HDMI-A-1).
fn wait_for_named_outputs(wl: &mut WlBackend) -> anyhow::Result<Vec<gesso_wl::OutputInfo>> {
    let mut outs = wl.outputs();
    if !outs.is_empty() {
        return Ok(outs);
    }
    let start   = Instant::now();
    let timeout = Duration::from_millis(1500);
    while start.elapsed() < timeout {
        let _ = wl.blocking_dispatch();
        outs = wl.outputs();
        if !outs.is_empty() {
            return Ok(outs);
        }
    }
    Ok(outs)
}

pub fn run(rx: mpsc::Receiver<ipc::Request>, tx: mpsc::Sender<ipc::Response>) -> anyhow::Result<()> {
    scope!("gessod.run", {
        info!("starting gessod");

        let wl_sock = match wayland_socket_path() {
            Ok(p)  => p,
            Err(e) => {
                eventline::error!("wayland not usable: {e}");
                return Ok(());
            }
        };

        let mut wl = WlBackend::connect()?;
        wl.roundtrip()?;

        let mut outputs = wait_for_named_outputs(&mut wl)?;
        if outputs.is_empty() {
            eventline::warn!(
                "no named outputs yet. `gesso doctor` may show missing xdg-output manager."
            );
        }

        let mut eng = RenderEngine::default();
        for o in &outputs {
            eng.register_output(&o.name, o.width, o.height);
        }

        let mut active:   HashSet<String>                     = HashSet::new();
        let mut current:  HashMap<String, ipc::CurrentTarget> = HashMap::new();
        let mut last_set: HashMap<String, PersistedSet>        = HashMap::new();
        let mut gifs:     HashMap<String, GifPlayer>           = HashMap::new();

        for o in &outputs {
            current.insert(o.name.clone(), ipc::CurrentTarget::Unset);
        }

        if let Ok(Some(persist)) = load_state() {
            info!("restoring persisted state");
            apply_persisted_state(
                &mut eng, &mut active, &mut current, &mut last_set, &mut gifs,
                &outputs, persist,
            )?;
        }

        let mut quitting = false;

        loop {
            if !wayland_socket_alive(&wl_sock) {
                info!("wayland socket vanished ({}); exiting gessod", wl_sock.display());
                break;
            }

            // ── Wayland dispatch ──────────────────────────────────────────────
            if let Err(e) = wl.dispatch() {
                eventline::warn!("wl.dispatch failed: {e:#}; reconnecting");

                if !wayland_socket_alive(&wl_sock) {
                    info!("wayland socket vanished ({}); exiting gessod", wl_sock.display());
                    break;
                }

                wl = WlBackend::connect()?;
                wl.roundtrip()?;
                outputs = wait_for_named_outputs(&mut wl)?;
                if outputs.is_empty() {
                    eventline::warn!("reconnected but still no named outputs.");
                }
                for o in &outputs {
                    eng.register_output(&o.name, o.width, o.height);
                    current.entry(o.name.clone()).or_insert(ipc::CurrentTarget::Unset);
                }
            } else {
                outputs = wl.outputs();
                if outputs.is_empty() {
                    outputs = wait_for_named_outputs(&mut wl)?;
                }
                for o in &outputs {
                    eng.register_output(&o.name, o.width, o.height);
                    current.entry(o.name.clone()).or_insert(ipc::CurrentTarget::Unset);
                }
            }

            // ── Tick animation players ────────────────────────────────────────
            {
                let now = Instant::now();
                let mut finished: Vec<String> = Vec::new();

                for o in &outputs {
                    if !active.contains(&o.name)     { continue; }
                    if eng.is_transitioning(&o.name) { continue; }
                    if let Some(p) = gifs.get_mut(&o.name) {
                        if p.tick(now, &mut eng, &o.name).is_err() {
                            finished.push(o.name.clone());
                        }
                    }
                }

                for name in finished {
                    gifs.remove(&name);
                }
            }

            // ── Drain IPC ─────────────────────────────────────────────────────
            while let Ok(req) = rx.try_recv() {
                let resp = match req {
                    ipc::Request::Restore => handle_restore(
                        &mut eng, &mut active, &mut current, &mut last_set,
                        &mut gifs, &outputs,
                    ),
                    other => handle_request(
                        &mut eng, &mut wl, &outputs,
                        &mut active, &mut current, &mut last_set, &mut gifs,
                        other, &mut quitting,
                    ),
                };
                let _ = tx.send(resp);
            }

            if quitting { break; }

            // ── Present ───────────────────────────────────────────────────────
            let any_needs_present = outputs
                .iter()
                .filter(|o| active.contains(&o.name))
                .any(|o| eng.needs_present(&o.name));

            if any_needs_present {
                let mut any_presented = false;

                for o in &outputs {
                    if !active.contains(&o.name) || !eng.needs_present(&o.name) {
                        continue;
                    }
                    let presented = wl.present_rendered(&o.name, o.width, o.height, |dst| {
                        eng.render_output_into(&o.name, dst);
                        Ok(())
                    })?;
                    if presented { any_presented = true; }
                }

                for o in &outputs {
                    if active.contains(&o.name) && !eng.needs_present(&o.name) {
                        wl.release_buffers(&o.name);
                    }
                }

                if !any_presented {
                    if let Err(e) = wl.blocking_dispatch() {
                        eventline::warn!("wl.blocking_dispatch failed: {e:#}; reconnecting");
                        if !wayland_socket_alive(&wl_sock) {
                            info!("wayland socket vanished ({}); exiting gessod", wl_sock.display());
                            break;
                        }
                        wl = WlBackend::connect()?;
                        wl.roundtrip()?;
                        let _ = wait_for_named_outputs(&mut wl)?;
                    }
                }

                continue;
            }

            // ── Idle ──────────────────────────────────────────────────────────
            for o in &outputs {
                if active.contains(&o.name) {
                    wl.release_buffers(&o.name);
                }
            }

            // Sleep until next IPC or animation deadline.
            let now = Instant::now();
            let mut timeout = Duration::from_millis(250);
            for o in &outputs {
                if !active.contains(&o.name) { continue; }
                if let Some(p) = gifs.get(&o.name) {
                    let dt = p.next_deadline().saturating_duration_since(now);
                    if dt < timeout { timeout = dt; }
                }
            }

            match rx.recv_timeout(timeout) {
                Ok(req) => {
                    let resp = match req {
                        ipc::Request::Restore => handle_restore(
                            &mut eng, &mut active, &mut current, &mut last_set,
                            &mut gifs, &outputs,
                        ),
                        other => handle_request(
                            &mut eng, &mut wl, &outputs,
                            &mut active, &mut current, &mut last_set, &mut gifs,
                            other, &mut quitting,
                        ),
                    };
                    let _ = tx.send(resp);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if !wayland_socket_alive(&wl_sock) {
                        info!("wayland socket vanished ({}); exiting gessod", wl_sock.display());
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        Ok(())
    })
}

fn handle_restore(
    eng:      &mut RenderEngine,
    active:   &mut HashSet<String>,
    current:  &mut HashMap<String, ipc::CurrentTarget>,
    last_set: &mut HashMap<String, PersistedSet>,
    gifs:     &mut HashMap<String, GifPlayer>,
    outputs:  &[gesso_wl::OutputInfo],
) -> ipc::Response {
    match load_state() {
        Ok(Some(persist)) => {
            match apply_persisted_state(eng, active, current, last_set, gifs, outputs, persist) {
                Ok(())  => ipc::Response::Ok,
                Err(e)  => ipc::Response::Error { message: format!("restore failed: {e}") },
            }
        }
        _ => ipc::Response::Error { message: "no persisted state".into() },
    }
}
