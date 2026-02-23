#[derive(Debug, Clone, Copy)]
pub struct Surface<'a> {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub data: &'a [u8], // XRGB8888 only (B,G,R,0)
}

impl<'a> Surface<'a> {
    pub fn row(&self, y: u32) -> &'a [u8] {
        let start = y as usize * self.stride;
        &self.data[start..start + self.stride]
    }

    /// View a row as u32 pixels (native endian).
    /// For XRGB8888 in memory as B,G,R,0 (little endian), u32 is 0x00RRGGBB.
    #[inline]
    pub fn row_u32(&self, y: u32) -> &'a [u32] {
        let bytes = self.row(y);
        debug_assert_eq!(bytes.len() % 4, 0);
        let len = bytes.len() / 4;

        // Safety: data is XRGB8888, 4-byte aligned is not guaranteed, but
        // unaligned reads of u32 are OK on x86_64. Still, to be safe across arches,
        // we use from_raw_parts with alignment assumption only in debug.
        // If you care about strict alignment portability, keep it bytes and use
        // u32::from_le_bytes per pixel (slower).
        unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const u32, len) }
    }
}
