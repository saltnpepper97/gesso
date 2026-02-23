use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use crate::frame;
use crate::protocol::{Request, Response};

const DIR_NAME: &str = "gesso";
const SOCK_NAME: &str = "gesso.sock";

/// Returns:
///   $XDG_RUNTIME_DIR/gesso/gesso.sock
/// Fallback:
///   /tmp/gesso-<uid>/gesso.sock
pub fn default_socket_path() -> std::io::Result<PathBuf> {
    let base = if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(dir)
    } else {
        let uid = unsafe { libc::geteuid() };
        PathBuf::from(format!("/tmp/gesso-{}", uid))
    };

    Ok(base.join(DIR_NAME).join(SOCK_NAME))
}

pub fn request(sock: impl AsRef<Path>, req: &Request) -> frame::Result<Response> {
    let mut stream = UnixStream::connect(sock)?;
    frame::send(&mut stream, req)?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    frame::recv(&mut stream)
}
