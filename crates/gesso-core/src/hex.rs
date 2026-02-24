// Author: Dustin Pilgrim
// License: MIT

pub fn nybble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub fn byte(hi: u8, lo: u8) -> Option<u8> {
    Some((nybble(hi)? << 4) | nybble(lo)?)
}
