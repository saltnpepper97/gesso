use std::fs;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

use crate::frame;
use crate::protocol::{Request, Response};

pub fn bind(sock: impl AsRef<Path>) -> std::io::Result<UnixListener> {
    let sock = sock.as_ref();

    // Ensure parent directory exists
    if let Some(dir) = sock.parent() {
        create_runtime_dir(dir)?;
    }

    // Remove stale socket
    if sock.exists() {
        let _ = fs::remove_file(sock);
    }

    let listener = UnixListener::bind(sock)?;

    // Restrict socket permissions to 0600
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(sock, fs::Permissions::from_mode(0o600));
    }

    Ok(listener)
}

fn create_runtime_dir(dir: &Path) -> std::io::Result<()> {
    if !dir.exists() {
        fs::create_dir_all(dir)?;
    }

    // Restrict dir permissions to 0700
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }

    Ok(())
}

/// Blocking loop — run in dedicated thread.
pub fn run_server(
    listener: UnixListener,
    handler: impl Fn(Request) -> Response,
) -> frame::Result<()> {
    loop {
        let (mut stream, _) = listener.accept()?;
        handle_one(&mut stream, &handler)?;
    }
}

fn handle_one(
    stream: &mut UnixStream,
    handler: &impl Fn(Request) -> Response,
) -> frame::Result<()> {
    let req: Request = frame::recv(stream)?;
    let resp = handler(req);
    frame::send(stream, &resp)?;
    let _ = stream.shutdown(std::net::Shutdown::Both);
    Ok(())
}
