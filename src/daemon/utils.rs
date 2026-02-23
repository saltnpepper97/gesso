// Author: Dustin Pilgrim
// License: MIT

use anyhow::Result;
use std::io::Write;
use std::os::unix::net::UnixStream;

use crate::protocol::Response;

pub fn is_broken_pipe(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(|ioe| ioe.kind() == std::io::ErrorKind::BrokenPipe)
    })
}

pub fn is_client_disconnect(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        if let Some(ioe) = c.downcast_ref::<std::io::Error>() {
            use std::io::ErrorKind::*;
            return matches!(ioe.kind(), BrokenPipe | ConnectionReset | UnexpectedEof);
        }
        false
    })
}

pub fn root_io_msg(e: &anyhow::Error) -> String {
    for c in e.chain() {
        if let Some(ioe) = c.downcast_ref::<std::io::Error>() {
            return format!("{}", ioe);
        }
    }
    e.to_string()
}

pub fn write_resp(stream: &mut UnixStream, resp: Response) -> Result<()> {
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
