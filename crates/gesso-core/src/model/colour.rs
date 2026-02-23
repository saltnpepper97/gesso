use serde::{Deserialize, Serialize};
use crate::hex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Colour {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Debug, thiserror::Error)]
pub enum ColourParseError {
    #[error("colour must start with '#'")]
    MissingHash,
    #[error("invalid hex")]
    InvalidHex,
    #[error("invalid length (use #RGB or #RRGGBB)")]
    InvalidLength,
}

impl Colour {
    pub const BLACK: Colour = Colour { r: 0, g: 0, b: 0 };

    pub fn parse(input: &str) -> Result<Self, ColourParseError> {
        let s = input.trim();
        let Some(hexstr) = s.strip_prefix('#') else {
            return Err(ColourParseError::MissingHash);
        };

        match hexstr.len() {
            3 => {
                let r = hex::nybble(hexstr.as_bytes()[0]).ok_or(ColourParseError::InvalidHex)?;
                let g = hex::nybble(hexstr.as_bytes()[1]).ok_or(ColourParseError::InvalidHex)?;
                let b = hex::nybble(hexstr.as_bytes()[2]).ok_or(ColourParseError::InvalidHex)?;
                Ok(Colour {
                    r: (r << 4) | r,
                    g: (g << 4) | g,
                    b: (b << 4) | b,
                })
            }
            6 => {
                let bytes = hexstr.as_bytes();
                Ok(Colour {
                    r: hex::byte(bytes[0], bytes[1]).ok_or(ColourParseError::InvalidHex)?,
                    g: hex::byte(bytes[2], bytes[3]).ok_or(ColourParseError::InvalidHex)?,
                    b: hex::byte(bytes[4], bytes[5]).ok_or(ColourParseError::InvalidHex)?,
                })
            }
            _ => Err(ColourParseError::InvalidLength),
        }
    }
}
