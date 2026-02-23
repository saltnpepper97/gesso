mod logging;
mod daemon;

use clap::Parser;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use eventline::info;

use gesso_ipc::{bind, default_socket_path};
use gesso_ipc::protocol as ipc;

// ── allocator (jemalloc) ─────────────────────────────────────────────────────
//
// Requires you to have tikv-jemallocator as a dependency somewhere in the crate graph.
// (You said no toml here, so I'm not touching it.)
use tikv_jemallocator::Jemalloc;

#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

/// Apply jemalloc purge/decay defaults *from inside the binary*.
///
/// MUST run before major allocations (Args::parse, logging init, etc).
/// Your toolchain marks set_var unsafe, so we wrap it.
#[inline(always)]
fn configure_jemalloc_defaults() {
    // Allow overriding from the outside if you ever want.
    if std::env::var_os("MALLOC_CONF").is_none() {
        unsafe {
            std::env::set_var(
                "MALLOC_CONF",
                // 1 arena, background purge, fast decay -> returns RSS after frees
                "narenas:1,background_thread:true,dirty_decay_ms:1000,muzzy_decay_ms:1000",
            );
        }
    }
}

/// Compute the log file path:
/// - If XDG_STATE_HOME is set:   $XDG_STATE_HOME/gesso/gessod.log
/// - Else:                      $HOME/.local/state/gesso/gessod.log
fn log_file_path() -> anyhow::Result<PathBuf> {
    if let Ok(base) = std::env::var("XDG_STATE_HOME") {
        return Ok(PathBuf::from(base).join("gesso").join("gessod.log"));
    }
    let home = std::env::var("HOME")?;
    Ok(PathBuf::from(home).join(".local/state/gesso/gessod.log"))
}

/// Disable Transparent Huge Pages for THIS PROCESS (Linux).
///
/// If your anon regions are getting backed by THP (2MB pages), RSS jumps hard.
/// This asks the kernel to stop using THP for *this* process.
///
/// No libc: raw prctl(2) syscall via inline asm.
///
/// If unsupported/forbidden, it fails and we ignore it.
#[cfg(target_os = "linux")]
fn disable_thp_for_process() {
    const PR_SET_THP_DISABLE: usize = 41;

    #[cfg(target_arch = "x86_64")]
    const SYS_PRCTL: usize = 157;
    #[cfg(target_arch = "aarch64")]
    const SYS_PRCTL: usize = 167;

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    unsafe {
        let mut ret: isize;
        core::arch::asm!(
            "syscall",
            inlateout("rax") SYS_PRCTL as isize => ret,
            in("rdi") PR_SET_THP_DISABLE as isize,
            in("rsi") 1isize,
            in("rdx") 0isize,
            in("r10") 0isize,
            in("r8")  0isize,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack, preserves_flags),
        );
        let _ = ret; // ignore -errno
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        // unsupported arch: no-op
    }
}

#[derive(Parser)]
struct Args {
    /// Enable verbose logging (console + debug)
    #[arg(short, long)]
    verbose: bool,
    /// Override socket path
    #[arg(long)]
    socket: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    // ABSOLUTELY FIRST: configure jemalloc before anything allocates big.
    configure_jemalloc_defaults();

    // Do this as early as possible.
    #[cfg(target_os = "linux")]
    disable_thp_for_process();

    let args = Args::parse();

    // Log file under XDG_STATE_HOME (or ~/.local/state)
    let log_path = log_file_path()?;
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("gessod: create log dir {}: {e}", parent.display()))?;
    }
    logging::init(args.verbose, &log_path)?;

    let sock = match args.socket {
        Some(p) => p,
        None => default_socket_path()?,
    };
    info!("socket: {}", sock.display());

    // Ensure socket parent dir exists + remove stale socket file
    if let Some(parent) = sock.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("gessod: create socket dir {}: {e}", parent.display()))?;
    }
    let _ = std::fs::remove_file(&sock);

    // Single-instance lock lives next to the socket (same /run dir).
    // Keep it in scope for the whole process lifetime.
    let _instance_lock = daemon::instance_lock::InstanceLock::acquire_for_socket(&sock)
        .map_err(|e| anyhow::anyhow!("gessod: {e}"))?;

    // Channel: IPC thread -> render thread
    let (req_tx, req_rx) = mpsc::channel::<ipc::Request>();
    // Channel: render thread -> IPC thread
    let (resp_tx, resp_rx) = mpsc::channel::<ipc::Response>();

    // IPC server thread (blocking accept/read/write)
    let listener = bind(&sock)?;
    thread::spawn(move || {
        // One-response-per-request: handler sends req to render thread and waits for resp.
        let handler = move |req: ipc::Request| -> ipc::Response {
            if req_tx.send(req).is_err() {
                return ipc::Response::Error { message: "daemon not running".into() };
            }
            match resp_rx.recv() {
                Ok(r) => r,
                Err(_) => ipc::Response::Error { message: "daemon not running".into() },
            }
        };
        if let Err(e) = gesso_ipc::run_server(listener, handler) {
            eprintln!("ipc server error: {e}");
        }
    });

    // Render loop runs on main thread.
    daemon::run(req_rx, resp_tx)
}
