// Author: Dustin Pilgrim
// License: MIT

use anyhow::{Context, Result};
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::Path;

use crate::protocol::{CurrentStatus, DoctorCheck, Request, Response};
use crate::spec::Spec;
use crate::wallpaper::Engine;

use super::engine::{apply_with_retry, unset_with_retry};
use super::utils::write_resp;
use super::state::clear_current;

pub fn handle_client(stream: &mut UnixStream, current_path: &Path, engine: &mut Engine) -> Result<bool> {
    // Read exactly one JSON line request, then drop the reader before writing.
    let req: Request = {
        let mut line = String::new();
        let n = {
            let mut reader = BufReader::new(&mut *stream);
            reader.read_line(&mut line).context("read request line")?
        };

        // EOF: client connected but sent nothing (or closed immediately). Not an error.
        if n == 0 {
            return Ok(false);
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(false);
        }

        serde_json::from_str(trimmed).context("parse request json")?
    };

    match req {
        Request::Apply { spec } => {
            eventline::scope!(
                "gesso.request.apply",
                success = "ok",
                failure = "failed",
                aborted = "aborted",
                {
                    eventline::info!("apply request spec={:?}", spec);

                    match apply_with_retry(engine, spec.clone(), current_path) {
                        Ok(_) => {
                            eventline::info!("apply success spec={:?}", spec);
                            write_resp(stream, Response::Ok)?;
                        }
                        Err(e) => {
                            eventline::error!("apply failed spec={:?} err={:#}", spec, e);
                            write_resp(stream, Response::Error { message: e.to_string() })?;
                        }
                    }

                    Ok::<(), anyhow::Error>(())
                }
            )?;
        }

        Request::Unset { output } => {
            let out = output.clone().unwrap_or_else(|| "(all)".into());

            eventline::scope!(
                "gesso.request.unset",
                success = "ok",
                failure = "failed",
                aborted = "aborted",
                {
                    eventline::info!("unset request output={}", out);

                    match unset_with_retry(engine, output.as_deref(), current_path) {
                        Ok(_) => {
                            eventline::info!("unset success output={}", out);
                            write_resp(stream, Response::Ok)?;
                        }
                        Err(e) => {
                            eventline::error!("unset failed output={} err={:#}", out, e);
                            write_resp(stream, Response::Error { message: e.to_string() })?;
                        }
                    }

                    Ok::<(), anyhow::Error>(())
                }
            )?;
        }

        Request::Stop => {
            eventline::scope!(
                "gesso.request.stop",
                success = "stopped",
                failure = "failed",
                aborted = "aborted",
                {
                    eventline::info!("stop request");

                    // Best effort: stop wallpaper + clear state.
                    let _ = engine.stop();
                    clear_current(current_path);

                    // Reply first so client doesn't see connection reset.
                    write_resp(stream, Response::Ok)?;

                    Ok::<(), anyhow::Error>(())
                }
            )?;

            return Ok(true);
        }

        Request::Status => {
            let cur = engine.current().cloned();
            let running = engine.running();

            eventline::debug!("status request running={} current={:?}", running, cur);

            let payload = cur.map(|spec| CurrentStatus {
                spec,
                running,
                note: if running { "running".into() } else { "not running".into() },
            });

            write_resp(stream, Response::Status { current: payload })?;
        }

        Request::Doctor => {
            eventline::scope!(
                "gesso.request.doctor",
                success = "ok",
                failure = "failed",
                aborted = "aborted",
                {
                    let pr = engine.probe();
                    eventline::info!(
                        "probe wayland_display={} compositor={} shm={} layer_shell={} outputs={}",
                        pr.wayland_display,
                        pr.compositor,
                        pr.shm,
                        pr.layer_shell,
                        pr.outputs
                    );

                    let mut checks = Vec::new();
                    checks.push(DoctorCheck {
                        name: "WAYLAND_DISPLAY set".into(),
                        ok: pr.wayland_display,
                        detail: "Wayland-only".into(),
                    });
                    checks.push(DoctorCheck {
                        name: "wl_compositor".into(),
                        ok: pr.compositor,
                        detail: "required".into(),
                    });
                    checks.push(DoctorCheck {
                        name: "wl_shm".into(),
                        ok: pr.shm,
                        detail: "required (v1 renderer)".into(),
                    });
                    checks.push(DoctorCheck {
                        name: "zwlr_layer_shell_v1".into(),
                        ok: pr.layer_shell,
                        detail: "required for wallpaper layer surfaces".into(),
                    });
                    checks.push(DoctorCheck {
                        name: "wl_output count".into(),
                        ok: pr.outputs > 0,
                        detail: format!("seen: {}", pr.outputs),
                    });

                    write_resp(stream, Response::Doctor { checks })?;
                    Ok::<(), anyhow::Error>(())
                }
            )?;
        }
    }

    Ok(false)
}

// keep Spec imported in this file because Request/Status paths mention it in logs/debug
#[allow(dead_code)]
fn _keep_spec_imported(_: &Spec) {}
