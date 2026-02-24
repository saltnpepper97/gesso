mod logging;
mod daemon;

use clap::Parser;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use eventline::info;

use gesso_ipc::{bind, default_socket_path};
use gesso_ipc::protocol as ipc;

// ── Allocator (jemalloc) ─────────────────────────────────────────────────────

use tikv_jemallocator::Jemalloc;

#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

/// Apply jemalloc tuning BEFORE any significant allocation.
///
/// Key options:
/// - `narenas:1`              — single arena; less per-arena overhead, better
///                              locality for a single-threaded render loop
/// - `background_thread:true` — dedicated purge thread (requires the
///                              `background_thread` cargo feature on tikv-jemallocator)
/// - `dirty_decay_ms:500`     — return dirty pages to OS within 500 ms of becoming free
/// - `muzzy_decay_ms:0`       — MADV_FREE muzzy pages immediately (lazy-free, zero cost
///                              if they are reused before the kernel reclaims them)
/// - `retain:false`           — actually `munmap` freed extents instead of holding
///                              virtual address space; reduces RSS for one-off large allocs
///
/// All values can be overridden at runtime by setting `MALLOC_CONF` in the environment
/// before launching gessod.
#[inline(always)]
fn configure_jemalloc() {
    if std::env::var_os("MALLOC_CONF").is_none() {
        // SAFETY: must be called before any threads are spawned and before any
        // jemalloc allocations; we are the very first thing in main().
        unsafe {
            std::env::set_var(
                "MALLOC_CONF",
                "narenas:1,background_thread:true,\
                 dirty_decay_ms:500,muzzy_decay_ms:0,\
                 retain:false",
            );
        }
    }
}

/// Compute the log file path:
/// - `$XDG_STATE_HOME/gesso/gessod.log`  (if XDG_STATE_HOME is set)
/// - `$HOME/.local/state/gesso/gessod.log`  (fallback)
fn log_file_path() -> anyhow::Result<PathBuf> {
    if let Ok(base) = std::env::var("XDG_STATE_HOME") {
        return Ok(PathBuf::from(base).join("gesso").join("gessod.log"));
    }
    let home = std::env::var("HOME")?;
    Ok(PathBuf::from(home).join(".local/state/gesso/gessod.log"))
}

/// Disable Transparent Huge Pages for this process (Linux).
///
/// THP can back anonymous regions with 2 MB pages, dramatically inflating RSS
/// when small objects happen to touch a new huge-page boundary.  Opting out is
/// safe and essentially free for a wallpaper daemon.
///
/// Uses a raw prctl(2) syscall to avoid a libc dependency.
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
            in("rdi")  PR_SET_THP_DISABLE as isize,
            in("rsi")  1isize,
            in("rdx")  0isize,
            in("r10")  0isize,
            in("r8")   0isize,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack, preserves_flags),
        );
        let _ = ret;
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {}
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
    // ── 1. Allocator config — MUST be first, before any heap use ──
    configure_jemalloc();

    // ── 2. Kernel hints ──
    #[cfg(target_os = "linux")]
    disable_thp_for_process();

    let args = Args::parse();

    // ── 3. Logging ──
    let log_path = log_file_path()?;
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("gessod: create log dir {}: {e}", parent.display()))?;
    }
    logging::init(args.verbose, &log_path)?;

    // ── 4. Socket setup ──
    let sock = match args.socket {
        Some(p) => p,
        None    => default_socket_path()?,
    };
    info!("socket: {}", sock.display());

    if let Some(parent) = sock.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("gessod: create socket dir {}: {e}", parent.display()))?;
    }
    let _ = std::fs::remove_file(&sock);

    let _instance_lock = daemon::instance_lock::InstanceLock::acquire_for_socket(&sock)
        .map_err(|e| anyhow::anyhow!("gessod: {e}"))?;

    // ── 5. IPC channels ──
    let (req_tx, req_rx)   = mpsc::channel::<ipc::Request>();
    let (resp_tx, resp_rx) = mpsc::channel::<ipc::Response>();

    // ── 6. IPC server thread ──
    // 512 KB stack is ample for simple serialisation/deserialisation work.
    // The default 8 MB wastes ~7.5 MB of RSS unnecessarily.
    let listener = bind(&sock)?;
    thread::Builder::new()
        .name("gessod-ipc".into())
        .stack_size(512 * 1024)
        .spawn(move || {
            let handler = move |req: ipc::Request| -> ipc::Response {
                if req_tx.send(req).is_err() {
                    return ipc::Response::Error { message: "daemon not running".into() };
                }
                match resp_rx.recv() {
                    Ok(r)  => r,
                    Err(_) => ipc::Response::Error { message: "daemon not running".into() },
                }
            };
            if let Err(e) = gesso_ipc::run_server(listener, handler) {
                eprintln!("ipc server error: {e}");
            }
        })
        .map_err(|e| anyhow::anyhow!("gessod: spawn ipc thread: {e}"))?;

    // ── 7. Render loop (main thread) ──
    daemon::run(req_rx, resp_tx)
}
