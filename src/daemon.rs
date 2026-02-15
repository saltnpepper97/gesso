// Author: Dustin Pilgrim
// License: MIT

use anyhow::{Context, Result};
use std::fs;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::logrotate::{self, LogPolicy};
use crate::path::paths;
use crate::protocol::{CurrentStatus, DoctorCheck, Request, Response};
use crate::spec::Spec;
use crate::wallpaper::Engine;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

// ─────────────────────────────────────────────────────────────────────────────
// Single-instance lock (libc::flock)
// ─────────────────────────────────────────────────────────────────────────────

fn lock_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("gesso.lock")
}

/// Try to acquire a non-blocking exclusive lock.
/// Keep the returned File alive for the daemon lifetime.
/// If already locked -> Ok(None) (another daemon instance is running).
fn try_acquire_single_instance_lock(lock_path: &Path) -> Result<Option<std::fs::File>> {
    let f = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)
        .with_context(|| format!("open lock file: {}", lock_path.display()))?;

    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(Some(f))
    } else {
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EWOULDBLOCK) => Ok(None),
            _ => Err(e).with_context(|| format!("flock: {}", lock_path.display())),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Eventline init
// ─────────────────────────────────────────────────────────────────────────────

/// Initialize eventline once.
/// We keep this local so daemon.rs stays the only place that knows how runtime is bootstrapped.
fn init_eventline(log_path: &Path) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for eventline init")?;

    rt.block_on(async {
        eventline::runtime::init().await;
    });

    // Daemon policy:
    // - no console output
    // - full file logging (live + structured)
    eventline::runtime::enable_console_output(false);
    eventline::runtime::enable_console_color(false);
    eventline::runtime::enable_console_timestamp(false);
    eventline::runtime::enable_console_duration(true);

    // Single canonical log file (owned by gesso)
    eventline::runtime::enable_file_output(log_path)
        .with_context(|| format!("enable eventline file output: {}", log_path.display()))?;

    // Default verbosity (adjustable later)
    eventline::runtime::set_log_level(eventline::runtime::LogLevel::Info);

    Ok(())
}

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

                        if let Err(e) = apply_with_retry(&mut engine, spec.clone(), &p.current_path)
                        {
                            // Log and continue serving clients.
                            eventline::error!("cached apply failed spec={:?} err={:#}", spec, e);
                        }

                        Ok::<(), anyhow::Error>(())
                    }
                );
            }

            let shutdown = false;

            for conn in listener.incoming() {
                if shutdown {
                    break;
                }

                match conn {
                    Ok(mut stream) => {
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
                                break;
                            }
                            Ok(false) => {}
                            Err(e) => {
                                if is_client_disconnect(&e) {
                                    eventline::warn!(
                                        "client disconnected peer={} err={}",
                                        peer,
                                        root_io_msg(&e)
                                    );
                                } else {
                                    eventline::error!("client error peer={} err={:#}", peer, e);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eventline::error!("accept error err={}", e);
                    }
                }
            }

            // Best effort: remove socket on exit.
            let _ = fs::remove_file(&p.sock_path);
            eventline::info!("daemon exiting");

            Ok::<(), anyhow::Error>(())
        }
    )?;

    Ok(())
}

fn handle_client(stream: &mut UnixStream, current_path: &Path, engine: &mut Engine) -> Result<bool> {
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

fn build_engine() -> Result<Engine> {
    let mut engine = Engine::new()?;
    let pr = engine.probe();

    eventline::info!(
        "wayland probe wayland_display={} compositor={} shm={} layer_shell={} outputs={}",
        pr.wayland_display,
        pr.compositor,
        pr.shm,
        pr.layer_shell,
        pr.outputs
    );

    // Do a couple roundtrips to pick up initial globals/output metadata,
    // but DO NOT add fixed sleeps here (wait_for_configured handles readiness).
    for i in 0..2 {
        if let Err(e) = engine.roundtrip() {
            eventline::warn!("initial roundtrip failed attempt={} err={:#}", i, e);
        }
    }

    Ok(engine)
}

fn is_broken_pipe(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(|ioe| ioe.kind() == std::io::ErrorKind::BrokenPipe)
    })
}

fn is_client_disconnect(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        if let Some(ioe) = c.downcast_ref::<std::io::Error>() {
            use std::io::ErrorKind::*;
            return matches!(ioe.kind(), BrokenPipe | ConnectionReset | UnexpectedEof);
        }
        false
    })
}

fn root_io_msg(e: &anyhow::Error) -> String {
    for c in e.chain() {
        if let Some(ioe) = c.downcast_ref::<std::io::Error>() {
            return format!("{}", ioe);
        }
    }
    e.to_string()
}

fn apply_with_retry(engine: &mut Engine, spec: Spec, current_path: &Path) -> Result<()> {
    eventline::scope!(
        "gesso.apply",
        success = "applied",
        failure = "failed",
        aborted = "aborted",
        {
            match engine.apply(spec.clone()) {
                Ok(_) => {
                    if let Err(e) = save_current(current_path, &spec) {
                        eventline::warn!("save_current failed err={:#}", e);
                    }
                    Ok::<(), anyhow::Error>(())
                }
                Err(e) if is_broken_pipe(&e) => {
                    eventline::error!("wayland broken pipe; recreating engine err={:#}", e);
                    *engine = build_engine()?;

                    engine.apply(spec.clone())?;
                    if let Err(e2) = save_current(current_path, &spec) {
                        eventline::warn!("save_current failed err={:#}", e2);
                    }
                    Ok::<(), anyhow::Error>(())
                }
                Err(e) => Err(e),
            }
        }
    )
}

fn unset_with_retry(engine: &mut Engine, output: Option<&str>, current_path: &Path) -> Result<()> {
    eventline::scope!(
        "gesso.unset",
        success = "unset",
        failure = "failed",
        aborted = "aborted",
        {
            match engine.unset(output) {
                Ok(_) => {
                    if output.is_none() {
                        clear_current(current_path);
                    }
                    Ok::<(), anyhow::Error>(())
                }
                Err(e) if is_broken_pipe(&e) => {
                    eventline::error!("wayland broken pipe; recreating engine err={:#}", e);
                    *engine = build_engine()?;

                    engine.unset(output)?;
                    if output.is_none() {
                        clear_current(current_path);
                    }
                    Ok::<(), anyhow::Error>(())
                }
                Err(e) => Err(e),
            }
        }
    )
}

fn write_resp(stream: &mut UnixStream, resp: Response) -> Result<()> {
    let s = serde_json::to_string(&resp)?;

    // Client may disconnect early; don't treat that as daemon failure.
    if let Err(e) = stream.write_all(s.as_bytes()) {
        if matches!(
            e.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
        ) {
            return Ok(());
        }
        return Err(e.into());
    }
    if let Err(e) = stream.write_all(b"\n") {
        if matches!(
            e.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
        ) {
            return Ok(());
        }
        return Err(e.into());
    }

    if let Err(e) = stream.flush() {
        if matches!(
            e.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
        ) {
            return Ok(());
        }
        return Err(e.into());
    }

    Ok(())
}

fn save_current(path: &Path, spec: &Spec) -> Result<()> {
    let s = serde_json::to_string_pretty(spec)?;
    fs::write(path, s)?;
    Ok(())
}

fn load_current(path: &Path) -> Option<Spec> {
    let s = fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

fn clear_current(path: &Path) {
    let _ = fs::remove_file(path);
}
