// Author: Dustin Pilgrim
// License: MIT

use thiserror::Error;

pub type WlResult<T> = Result<T, WlError>;

#[derive(Debug, Error)]
pub enum WlError {
    #[error("wayland connect failed: {0}")]
    Connect(String),

    #[error("missing required global: {0}")]
    MissingGlobal(&'static str),

    #[error("unknown output: {0}")]
    UnknownOutput(String),

    #[error("buffer size mismatch (expected {expected}, got {got})")]
    BufferSizeMismatch { expected: usize, got: usize },

    #[error("shm error: {0}")]
    Shm(String),

    #[error("protocol error: {0}")]
    Protocol(String),
}
