/// Memory pressure helpers.
///
/// These are all best-effort — the kernel is free to ignore any of these hints.
/// They never corrupt data; they only affect RSS and swap behaviour.
///
/// Uses `rustix` for all syscall wrappers instead of `libc`:
///   - no unsafe at call sites
///   - no glibc PLT indirection
///   - keeps `libc` out of the core crate's dependency tree entirely
///
/// # Usage pattern
///
/// Call `pixels_cold` immediately after a frame is committed to the Wayland
/// compositor. The compositor now owns the pixels in SHM; our copy of the
/// target pixels is only needed if a *new* transition fires (to serve as the
/// "old" snapshot). Marking them cold lets the kernel page them out under
/// memory pressure while keeping the virtual address range valid.
///
/// Call `pixels_free` on the OLD snapshot Vec<u8> as soon as a transition
/// finishes (inside RenderEngine when it drops the old frame). This signals
/// that the pages are immediately reclaimable without a swap write.
///
/// `heap_trim` has been removed. It only existed to compensate for glibc's
/// arena-hoarding behaviour. Use `mimalloc` as the global allocator instead —
/// it returns freed pages to the OS automatically, making an explicit trim
/// call unnecessary. Add to the daemon's main.rs:
///
/// ```rust
/// #[global_allocator]
/// static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
/// ```
///
/// and to Cargo.toml:
///
/// ```toml
/// mimalloc = { version = "0.1", default-features = false }
/// ```

#[cfg(target_os = "linux")]
mod inner {
    use rustix::mm::{Advice, madvise};
    use rustix::param::page_size;

    /// Page-align `data` and call madvise with the given advice.
    /// Silently does nothing if the region is smaller than one page.
    #[inline]
    fn madvise_range(data: &[u8], advice: Advice) {
        if data.is_empty() {
            return;
        }

        let page  = page_size();
        let addr  = data.as_ptr() as usize;

        // Round start UP to the next page boundary.
        let start = (addr + page - 1) & !(page - 1);
        // Round end DOWN to a page boundary.
        let end   = (addr + data.len()) & !(page - 1);

        if end <= start {
            return; // region spans less than one full page
        }

        // SAFETY: pointer is page-aligned and within our allocation.
        // madvise is advisory only — it cannot corrupt data.
        let _ = unsafe {
            madvise(
                start as *mut std::ffi::c_void,
                end - start,
                advice,
            )
        };
    }

    /// Mark pixel data as cold after it has been committed to the compositor.
    ///
    /// MADV_COLD (Linux 5.4+): pages stay mapped but are moved to the tail of
    /// the LRU and evicted first under memory pressure. Content remains valid —
    /// a page fault brings them back silently.
    #[inline]
    pub fn pixels_cold(pixels: &[u8]) {
        madvise_range(pixels, Advice::LinuxCold);
    }

    /// Mark a buffer as immediately reclaimable before dropping it.
    ///
    /// MADV_FREE (Linux 4.5+): pages may be reclaimed without a swap write;
    /// content is undefined after this call — do not read afterwards. If the
    /// kernel hasn't reclaimed them by the time we write new data, they are
    /// reused at zero cost.
    #[inline]
    pub fn pixels_free(pixels: &[u8]) {
        madvise_range(pixels, Advice::LinuxFree);
    }

    /// Hint sequential access pattern before a decode/scale pass.
    ///
    /// MADV_SEQUENTIAL: kernel increases read-ahead on the range.
    #[inline]
    pub fn pixels_sequential(pixels: &[u8]) {
        madvise_range(pixels, Advice::Sequential);
    }
}

// ── public API ───────────────────────────────────────────────────────────────

/// Mark pixel buffer as cold after committing to the compositor.
/// No-op on non-Linux targets.
#[inline]
pub fn pixels_cold(pixels: &[u8]) {
    #[cfg(target_os = "linux")]
    inner::pixels_cold(pixels);
    #[cfg(not(target_os = "linux"))]
    let _ = pixels;
}

/// Mark a buffer as immediately reclaimable before dropping it.
/// No-op on non-Linux targets.
#[inline]
pub fn pixels_free(pixels: &[u8]) {
    #[cfg(target_os = "linux")]
    inner::pixels_free(pixels);
    #[cfg(not(target_os = "linux"))]
    let _ = pixels;
}

/// Hint sequential access before a decode/scale pass.
/// No-op on non-Linux targets.
#[inline]
pub fn pixels_sequential(pixels: &[u8]) {
    #[cfg(target_os = "linux")]
    inner::pixels_sequential(pixels);
    #[cfg(not(target_os = "linux"))]
    let _ = pixels;
}
