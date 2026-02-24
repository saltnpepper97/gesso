// Author: Dustin Pilgrim
// License: MIT

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use thiserror::Error;

pub const MAX_FRAME_LEN: usize = 8 * 1024 * 1024; // 8 MiB

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("encode: {0}")]
    Encode(String),

    #[error("decode: {0}")]
    Decode(String),

    #[error("frame too large: {0}")]
    FrameTooLarge(usize),
}

pub type Result<T> = std::result::Result<T, FrameError>;

pub fn send<T: serde::Serialize>(w: &mut UnixStream, msg: &T) -> Result<()> {
    let bytes =
        postcard::to_stdvec(msg).map_err(|e| FrameError::Encode(e.to_string()))?;

    if bytes.len() > MAX_FRAME_LEN {
        return Err(FrameError::FrameTooLarge(bytes.len()));
    }

    let len = bytes.len() as u32;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&bytes)?;
    w.flush()?;
    Ok(())
}

pub fn recv<T: serde::de::DeserializeOwned>(r: &mut UnixStream) -> Result<T> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;

    if len > MAX_FRAME_LEN {
        return Err(FrameError::FrameTooLarge(len));
    }

    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;

    postcard::from_bytes(&buf)
        .map_err(|e| FrameError::Decode(e.to_string()))
}
