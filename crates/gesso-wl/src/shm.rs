// Author: Dustin Pilgrim
// License: MIT

use crate::{WlError, WlResult};
use memmap2::MmapMut;
use std::fs::File;
use std::os::fd::AsFd;
use wayland_client::QueueHandle;
use wayland_client::protocol::{wl_buffer, wl_shm, wl_shm::Format};

pub struct ShmBuffer {
    pub width: u32,
    pub height: u32,
    #[allow(dead_code)]
    pub stride: usize,

    // Total byte size of the buffer (stride * height).
    pub len: usize,

    // IMPORTANT:
    // Keep the backing file so we can re-map later after unmapping at idle.
    // The compositor has its own fd copy from create_pool(), so we *could*
    // close ours after creation, but we keep it to support re-mapping.
    file: File,

    // The client-side mapping is OPTIONAL so we can unmap at idle while keeping
    // wl_buffer alive (compositor can still read from its own mapping).
    pub mmap: Option<MmapMut>,

    pub wl_buffer: wl_buffer::WlBuffer,
    pub busy: bool,
}

impl ShmBuffer {
    /// Ensure the buffer is mapped in this process and return a mutable slice.
    /// This is called right before rendering into the buffer.
    #[inline]
    pub fn map_slice_mut(&mut self, expected: usize) -> WlResult<&mut [u8]> {
        if self.len < expected {
            return Err(WlError::Shm(format!(
                "buffer too small (have {}, need {})",
                self.len, expected
            )));
        }

        if self.mmap.is_none() {
            let mmap = unsafe {
                MmapMut::map_mut(&self.file).map_err(|e| WlError::Shm(e.to_string()))?
            };
            self.mmap = Some(mmap);
        }

        Ok(&mut self.mmap.as_mut().unwrap()[..expected])
    }

    /// Drop the client-side mapping to reclaim RSS while leaving wl_buffer alive.
    ///
    /// Safe even if the compositor is still using the buffer: it has its own fd/mapping.
    #[inline]
    pub fn unmap(&mut self) {
        self.mmap = None;
    }
}

pub fn create_xrgb8888(
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<crate::state::WlState>,
    width: u32,
    height: u32,
    out_global: u32,
    which: usize, // 0 or 1
) -> WlResult<ShmBuffer> {
    let stride = (width as usize)
        .checked_mul(4)
        .ok_or_else(|| WlError::Shm("overflow".into()))?;
    let len = stride
        .checked_mul(height as usize)
        .ok_or_else(|| WlError::Shm("overflow".into()))?;

    let f = tempfile::tempfile().map_err(|e| WlError::Shm(e.to_string()))?;
    f.set_len(len as u64)
        .map_err(|e| WlError::Shm(e.to_string()))?;

    // Map once up-front so first render has a mapping ready.
    let mmap = unsafe { MmapMut::map_mut(&f).map_err(|e| WlError::Shm(e.to_string()))? };

    // Send fd to compositor; compositor receives its own copy.
    let pool = shm.create_pool(f.as_fd(), len as i32, qh, ());
    let wl_buffer = pool.create_buffer(
        0,
        width as i32,
        height as i32,
        stride as i32,
        Format::Xrgb8888,
        qh,
        (out_global, which),
    );
    pool.destroy();

    Ok(ShmBuffer {
        width,
        height,
        stride,
        len,
        file: f,
        mmap: Some(mmap),
        wl_buffer,
        busy: false,
    })
}
