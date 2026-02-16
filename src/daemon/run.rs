// Author: Dustin Pilgrim
// License: MIT

use anyhow::{Context, Result};
use std::fs;
use std::os::unix::net::UnixListener;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::logrotate::{self, LogPolicy};
use crate::path::paths;
use super::session;
use crate::wallpaper::Engine;

use super::client::handle_client;
use super::engine::build_engine;
use super::lock::{lock_path, try_acquire_single_instance_lock};
use super::logging::init_eventline;
use super::state::load_current;

pub fn run_daemon() -> Result<()> {
    let p = paths()?;

    fs::create_dir_all(&p.state_dir).context("create state dir")?;
    fs::create_dir_all(&p.runtime_dir).context("create runtime dir")?;

    // ─────────────────────────────────────────────────────────────────────────
    // SINGLE INSTANCE ENFORCEMENT
    // ─────────────────────────────────────────────────────────────────────────
    // Acquire lock BEFORE touching the socket file so we never delete a live daemon's socket.
    let lock_file_path = lock_path(&p.runtime_dir);
    let _lock = match try_acquire_single_instance_lock(&lock_file_path)? {
        Some(f) => f, // keep alive for lifetime
        None => {
            // eventline console is disabled, so print directly.
            eprintln!("gesso: another instance is already running.");
            return Ok(());
        }
    };
    // ─────────────────────────────────────────────────────────────────────────

    // Rotate/prepare the SINGLE canonical log file *before* eventline opens it.
    let had_existing = logrotate::prepare_log_file(&p.log_path, LogPolicy::default())
        .with_context(|| format!("prepare_log_file: {}", p.log_path.display()))?;

    // If the log already existed and wasn't rotated, insert ONE literal blank line
    // between daemon runs. This is intentionally raw, not an eventline record.
    if had_existing {
        logrotate::write_raw_blank_line(&p.log_path)
            .with_context(|| format!("write blank line: {}", p.log_path.display()))?;
    }

    init_eventline(&p.log_path)?;

    // Write a run header using eventline (eventline is the ONLY logging).
    eventline::info!("{}", logrotate::run_header());

    // Refuse to start outside an active Wayland session.
    if let Err(e) = session::ensure_wayland_alive() {
        eventline::error!("not starting: {e}");
        return Ok(());
    }

    // Shared shutdown flag for watcher + accept loop.
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    session::spawn_wayland_socket_watcher(shutdown_flag.clone());

    eventline::scope!(
        "gesso.daemon",
        success = "exiting",
        failure = "crashed",
        aborted = "aborted",
        {
            eventline::info!(
                "startup sock={} current={} runtime_dir={} state_dir={} log={}",
                p.sock_path.display(),
                p.current_path.display(),
                p.runtime_dir.display(),
                p.state_dir.display(),
                p.log_path.display(),
            );

            // Remove stale socket file (safe: we hold the lock)
            if p.sock_path.exists() {
                let _ = fs::remove_file(&p.sock_path);
            }

            let listener = UnixListener::bind(&p.sock_path).context("bind ctl.sock")?;
            let _ = fs::set_permissions(&p.sock_path, fs::Permissions::from_mode(0o600));

            // Make accept loop stoppable (so the watcher can trigger shutdown).
            listener
                .set_nonblocking(true)
                .context("set_nonblocking on ctl.sock")?;

            // Build engine
            let mut engine: Engine = eventline::scope!(
                "gesso.wayland.build_engine",
                success = "ready",
                failure = "failed",
                aborted = "aborted",
                {
                    let e = build_engine()?;
                    Ok::<Engine, anyhow::Error>(e)
                }
            )?;

            // Warm up Wayland: configure layer surfaces, allocate SHM buffers,
            // and fault-in mmap pages so the first real animation isn't choppy.
            let _ = eventline::scope!(
                "gesso.wayland.warmup",
                success = "ready",
                failure = "skipped",
                aborted = "aborted",
                {
                    if let Err(e) = engine.warmup() {
                        eventline::warn!("warmup failed err={:#}", e);
                    }
                    Ok::<(), anyhow::Error>(())
                }
            );

            // Try to restore cached wallpaper
            if let Some(spec) = load_current(&p.current_path) {
                let _ = eventline::scope!(
                    "gesso.daemon.restore_cached",
                    success = "done",
                    failure = "failed",
                    aborted = "aborted",
                    {
                        eventline::info!("restoring cached spec={:?}", spec);

                        if let Err(e) =
                            super::engine::apply_with_retry(&mut engine, spec.clone(), &p.current_path)
                        {
                            // Log and continue serving clients.
                            eventline::error!("cached apply failed spec={:?} err={:#}", spec, e);
                        }

                        Ok::<(), anyhow::Error>(())
                    }
                );
            }

            loop {
                if shutdown_flag.load(Ordering::Relaxed) {
                    eventline::info!("session dead; exiting daemon loop");
                    break;
                }

                match listener.accept() {
                    Ok((mut stream, _addr)) => {
                        let peer = stream
                            .peer_addr()
                            .ok()
                            .map(|a| {
                                if let Some(p) = a.as_pathname() {
                                    p.display().to_string()
                                } else {
                                    format!("{a:?}")
                                }
                            })
                            .unwrap_or_else(|| "unknown".into());

                        // Allow long-running apply operations.
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(120)));

                        let res: Result<bool> = eventline::scope!(
                            "gesso.daemon.client",
                            success = "done",
                            failure = "error",
                            aborted = "aborted",
                            {
                                eventline::debug!("client connected peer={}", peer);
                                let should_exit =
                                    handle_client(&mut stream, &p.current_path, &mut engine)?;
                                Ok::<bool, anyhow::Error>(should_exit)
                            }
                        );

                        match res {
                            Ok(true) => {
                                eventline::info!("shutdown requested; exiting daemon loop");
                                shutdown_flag.store(true, Ordering::Relaxed);
                                break;
                            }
                            Ok(false) => {}
                            Err(e) => {
                                if super::utils::is_client_disconnect(&e) {
                                    eventline::warn!(
                                        "client disconnected peer={} err={}",
                                        peer,
                                        super::utils::root_io_msg(&e)
                                    );
                                } else {
                                    eventline::error!("client error peer={} err={:#}", peer, e);
                                }
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // Nothing to accept; keep loop responsive to watcher shutdown.
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(e) => {
                        eventline::error!("accept error err={}", e);
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }

            // Best effort: stop wallpaper when we exit due to session death.
            let _ = engine.stop();

            let _ = fs::remove_file(&p.sock_path);
            eventline::info!("daemon exiting");

            Ok::<(), anyhow::Error>(())
        }
    )?;

    Ok(())
}
