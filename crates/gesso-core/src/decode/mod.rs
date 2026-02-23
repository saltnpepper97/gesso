use std::path::Path;

mod png;
mod jpeg;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("unsupported format (only png and jpeg supported)")]
    Unsupported,

    #[error("png decode failed: {0}")]
    Png(String),

    #[error("jpeg decode failed: {0}")]
    Jpeg(String),
}

pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub stride: usize,   // width * 4
    pub pixels: Vec<u8>, // XRGB8888: B,G,R,0
}

fn is_png(buf: &[u8]) -> bool {
    buf.len() >= 8 && buf[..8] == [137, 80, 78, 71, 13, 10, 26, 10]
}

fn is_jpeg(buf: &[u8]) -> bool {
    buf.len() >= 2 && buf[0] == 0xFF && buf[1] == 0xD8
}

pub fn decode_image(path: &Path) -> Result<DecodedImage, DecodeError> {
    let data = std::fs::read(path)?;

    if is_png(&data) {
        return png::decode_png(&data).map_err(DecodeError::Png);
    }
    if is_jpeg(&data) {
        return jpeg::decode_jpeg(&data).map_err(DecodeError::Jpeg);
    }

    Err(DecodeError::Unsupported)
}
