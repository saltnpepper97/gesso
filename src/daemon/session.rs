// Author: Dustin Pilgrim
// License: MIT

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

/// Resolve XDG_RUNTIME_DIR (required for Wayland sockets).
fn runtime_dir() -> Result<PathBuf, String> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| "XDG_RUNTIME_DIR is not set".to_string())
}

/// Probe for a connectable Wayland socket path.
///
/// Priority:
///  1) $WAYLAND_DISPLAY (if set)
///  2) any connectable "wayland-*" in $XDG_RUNTIME_DIR
fn wayland_socket_path_probe() -> Result<PathBuf, String> {
    let rt = runtime_dir()?;

    if let Ok(display) = std::env::var("WAYLAND_DISPLAY") {
        if !display.is_empty() {
            return Ok(rt.join(display));
        }
    }

    for entry in std::fs::read_dir(&rt)
        .map_err(|e| format!("failed to read {}: {e}", rt.display()))?
    {
        let entry = entry.map_err(|e| format!("failed to read entry in {}: {e}", rt.display()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if !name.starts_with("wayland-") {
            continue;
        }

        let p = entry.path();
        if UnixStream::connect(&p).is_ok() {
            return Ok(p);
        }
    }

    Err(
        "WAYLAND_DISPLAY is not set and no connectable wayland-* socket was found in XDG_RUNTIME_DIR"
            .to_string(),
    )
}

/// Ensure we appear to be running under Wayland *right now*.
///
/// Ground truth: connectable Wayland socket.
/// We only use XDG_SESSION_TYPE for better diagnostics.
pub fn ensure_wayland_alive() -> Result<(), String> {
    let sock = wayland_socket_path_probe().map_err(|probe_err| {
        let session_type =
            std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "<unset>".to_string());

        if session_type != "<unset>" && session_type != "wayland" {
            format!("not a wayland session: XDG_SESSION_TYPE={}", session_type)
        } else {
            probe_err
        }
    })?;

    UnixStream::connect(&sock)
        .map(|_| ())
        .map_err(|e| format!("failed to connect to wayland socket {}: {e}", sock.display()))
}

/// Best-effort: get a stable socket path for the watcher.
/// (We still *connect* each poll; the path itself can remain stable.)
fn wayland_socket_path() -> Result<PathBuf, String> {
    wayland_socket_path_probe()
}

/// Check logind session liveness using org.freedesktop.login1.Session.Active.
///
/// This is blocking and does NOT require systemd as PID1.
/// It only requires logind to be present and reachable over the system bus.
fn login1_session_active_blocking() -> Result<bool, String> {
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::OwnedObjectPath;

    let sys = Connection::system()
        .map_err(|e| format!("logind: could not connect to system bus: {e}"))?;

    let mgr = Proxy::new(
        &sys,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .map_err(|e| format!("logind: failed to create Manager proxy: {e}"))?;

    // PID-based resolution: works even if XDG_SESSION_* env vars are absent.
    let pid = std::process::id() as u32;
    let (session_path,): (OwnedObjectPath,) = mgr
        .call("GetSessionByPID", &(pid,))
        .map_err(|e| format!("logind: GetSessionByPID({pid}) failed: {e}"))?;

    let sess = Proxy::new(
        &sys,
        "org.freedesktop.login1",
        session_path.as_str(),
        "org.freedesktop.login1.Session",
    )
    .map_err(|e| format!("logind: failed to create Session proxy: {e}"))?;

    let active: bool = sess
        .get_property("Active")
        .map_err(|e| format!("logind: failed to read Session.Active: {e}"))?;

    Ok(active)
}

/// Spawn a background watcher that flips `shutdown_flag` when:
///  - the Wayland socket is not connectable for N consecutive polls, OR
///  - logind session becomes inactive for N consecutive polls.
///
/// Note: if logind temporarily fails, we warn but do not kill the daemon.
pub fn spawn_wayland_socket_watcher(shutdown_flag: Arc<AtomicBool>) {
    let sock = match wayland_socket_path() {
        Ok(p) => p,
        Err(e) => {
            eventline::warn!("wayland watcher disabled: {e}");
            return;
        }
    };

    std::thread::spawn(move || {
        let mut socket_failures: u32 = 0;
        let mut inactive_failures: u32 = 0;

        loop {
            std::thread::sleep(Duration::from_secs(2));

            if shutdown_flag.load(Ordering::Relaxed) {
                break;
            }

            // 1) Wayland socket liveness (compositor/socket really gone)
            if UnixStream::connect(&sock).is_err() {
                socket_failures += 1;
            } else {
                socket_failures = 0;
            }

            if socket_failures >= 3 {
                eventline::info!(
                    "wayland socket not connectable ({}); shutting down",
                    sock.display()
                );
                shutdown_flag.store(true, Ordering::Relaxed);
                break;
            }

            // 2) Session liveness (covers VT switch / session end while socket may linger)
            match login1_session_active_blocking() {
                Ok(true) => {
                    inactive_failures = 0;
                }
                Ok(false) => {
                    inactive_failures += 1;
                    if inactive_failures >= 3 {
                        eventline::info!("logind session inactive; shutting down");
                        shutdown_flag.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                Err(e) => {
                    // If logind is unavailable/transiently failing, don't kill the app.
                    // Socket-based shutdown remains the backstop.
                    eventline::warn!("logind liveness probe failed: {e}");
                    inactive_failures = 0;
                }
            }
        }
    });
}
