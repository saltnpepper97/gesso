use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eventline::{info, scope};

use gesso_core::RenderEngine;
use gesso_ipc::protocol as ipc;
use gesso_wl::WlBackend;

use crate::daemon::ipc::handle_request;
use crate::daemon::persist::load_state;
use crate::daemon::restore::apply_persisted_state;
use crate::daemon::types::PersistedSet;
use crate::daemon::wayland::{wayland_socket_alive, wayland_socket_path};

#[cfg(target_os = "linux")]
fn memory_pressure_pulse() {
    const PULSE_MIB: usize = 24;
    const PAGE: usize = 4096;

    let mut v = vec![0u8; PULSE_MIB * 1024 * 1024];

    for i in (0..v.len()).step_by(PAGE) {
        unsafe {
            core::ptr::write_volatile(v.as_mut_ptr().add(i), 1);
        }
    }

    drop(v);
}

#[cfg(not(target_os = "linux"))]
fn memory_pressure_pulse() {}

/// Wait (briefly) for compositor-provided output names (DP-1 / HDMI-A-1).
/// This is REQUIRED because we intentionally refuse to expose OUT-* fallbacks.
fn wait_for_named_outputs(wl: &mut WlBackend) -> anyhow::Result<Vec<gesso_wl::OutputInfo>> {
    // If names are already available, return immediately.
    let mut outs = wl.outputs();
    if !outs.is_empty() {
        return Ok(outs);
    }

    // Block a little while for xdg-output events to arrive.
    // This keeps CLI behavior deterministic: `gesso outputs` / `gesso unset` works right away.
    let start = Instant::now();
    let timeout = Duration::from_millis(1500);

    while start.elapsed() < timeout {
        // Blocking dispatch will receive xdg-output name events if the compositor is sending them.
        let _ = wl.blocking_dispatch();

        outs = wl.outputs();
        if !outs.is_empty() {
            return Ok(outs);
        }
    }

    // Still none: compositor may not support xdg-output manager, or names never arrived.
    Ok(outs)
}

pub fn run(rx: mpsc::Receiver<ipc::Request>, tx: mpsc::Sender<ipc::Response>) -> anyhow::Result<()> {
    scope!("gessod.run", {
        info!("starting gessod");

        // Resolve the compositor's Wayland socket and exit when it vanishes.
        let wl_sock = match wayland_socket_path() {
            Ok(p) => p,
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
                "no named outputs yet (DP-1/HDMI-A-1). `gesso doctor` may show missing xdg-output manager."
            );
        }

        let mut eng = RenderEngine::default();
        for o in &outputs {
            eng.register_output(&o.name, o.width, o.height);
        }

        let mut active: HashSet<String> = HashSet::new();
        let mut current: HashMap<String, ipc::CurrentTarget> = HashMap::new();
        let mut last_set: HashMap<String, PersistedSet> = HashMap::new();

        for o in &outputs {
            current.insert(o.name.clone(), ipc::CurrentTarget::Unset);
        }

        if let Ok(Some(persist)) = load_state() {
            info!("restoring persisted state");
            apply_persisted_state(
                &mut eng,
                &mut active,
                &mut current,
                &mut last_set,
                &outputs,
                persist,
            )?;
        }

        let mut quitting = false;

        let mut was_busy = false;
        let mut last_pulse = Instant::now() - Duration::from_secs(60);
        const PULSE_COOLDOWN: Duration = Duration::from_millis(800);

        loop {
            if !wayland_socket_alive(&wl_sock) {
                info!("wayland socket vanished ({}); exiting gessod", wl_sock.display());
                break;
            }

            // Process wayland events (non-blocking)
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
                    eventline::warn!(
                        "reconnected but still no named outputs (DP-1/HDMI-A-1)."
                    );
                }

                for o in &outputs {
                    eng.register_output(&o.name, o.width, o.height);
                    current.entry(o.name.clone()).or_insert(ipc::CurrentTarget::Unset);
                }
            } else {
                outputs = wl.outputs();
                // If names disappear transiently, try to wait again (briefly) rather than breaking selection.
                if outputs.is_empty() {
                    outputs = wait_for_named_outputs(&mut wl)?;
                }

                for o in &outputs {
                    eng.register_output(&o.name, o.width, o.height);
                    current.entry(o.name.clone()).or_insert(ipc::CurrentTarget::Unset);
                }
            }

            // Drain IPC
            while let Ok(req) = rx.try_recv() {
                let resp = match req {
                    ipc::Request::Restore => match load_state() {
                        Ok(Some(persist)) => {
                            if let Err(e) = apply_persisted_state(
                                &mut eng,
                                &mut active,
                                &mut current,
                                &mut last_set,
                                &outputs,
                                persist,
                            ) {
                                ipc::Response::Error { message: format!("restore failed: {e}") }
                            } else {
                                ipc::Response::Ok
                            }
                        }
                        _ => ipc::Response::Error { message: "no persisted state".into() },
                    },
                    other => handle_request(
                        &mut eng,
                        &mut wl,
                        &outputs,
                        &mut active,
                        &mut current,
                        &mut last_set,
                        other,
                        &mut quitting,
                    ),
                };

                let _ = tx.send(resp);
            }

            if quitting {
                break;
            }

            // Present if any output needs it
            let any_needs_present = outputs
                .iter()
                .filter(|o| active.contains(&o.name))
                .any(|o| eng.needs_present(&o.name));

            if any_needs_present {
                was_busy = true;

                let mut any_presented = false;

                for o in &outputs {
                    if !active.contains(&o.name) || !eng.needs_present(&o.name) {
                        continue;
                    }

                    let presented = wl.present_rendered(&o.name, o.width, o.height, |dst| {
                        eng.render_output_into(&o.name, dst);
                        Ok(())
                    })?;

                    if presented {
                        any_presented = true;
                    }
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

            // Idle: release SHM buffers for all quiet active outputs.
            for o in &outputs {
                if active.contains(&o.name) {
                    wl.release_buffers(&o.name);
                }
            }

            if was_busy && last_pulse.elapsed() >= PULSE_COOLDOWN {
                memory_pressure_pulse();
                last_pulse = Instant::now();
            }
            was_busy = false;

            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(req) => {
                    let resp = match req {
                        ipc::Request::Restore => match load_state() {
                            Ok(Some(persist)) => {
                                if let Err(e) = apply_persisted_state(
                                    &mut eng,
                                    &mut active,
                                    &mut current,
                                    &mut last_set,
                                    &outputs,
                                    persist,
                                ) {
                                    ipc::Response::Error { message: format!("restore failed: {e}") }
                                } else {
                                    ipc::Response::Ok
                                }
                            }
                            _ => ipc::Response::Error { message: "no persisted state".into() },
                        },
                        other => handle_request(
                            &mut eng,
                            &mut wl,
                            &outputs,
                            &mut active,
                            &mut current,
                            &mut last_set,
                            other,
                            &mut quitting,
                        ),
                    };

                    let _ = tx.send(resp);
                    was_busy = true;
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
