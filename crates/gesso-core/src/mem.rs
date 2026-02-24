// Author: Dustin Pilgrim
// License: MIT

#[inline]
pub fn pages_dontneed(buf: &[u8]) {
    #[cfg(target_os = "linux")]
    unsafe {
        const MADV_DONTNEED: i32 = 9;

        #[cfg(target_arch = "x86_64")]
        const SYS_MADVISE: usize = 28;
        #[cfg(target_arch = "aarch64")]
        const SYS_MADVISE: usize = 233;

        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        {
            // Round inward to page boundaries so we never advise a partial page.
            let page = page_size();
            let addr  = buf.as_ptr() as usize;
            let start = (addr + page - 1) & !(page - 1);
            let end   = (addr + buf.len()) & !(page - 1);

            if end <= start { return; }

            let len = end - start;

            // Raw syscall — avoids a libc/rustix dependency in gesso-core.
            #[cfg(target_arch = "x86_64")]
            {
                let mut ret: isize;
                core::arch::asm!(
                    "syscall",
                    inlateout("rax") SYS_MADVISE as isize => ret,
                    in("rdi") start,
                    in("rsi") len,
                    in("rdx") MADV_DONTNEED as isize,
                    lateout("rcx") _,
                    lateout("r11") _,
                    options(nostack, preserves_flags),
                );
                let _ = ret;
            }
            #[cfg(target_arch = "aarch64")]
            {
                let mut ret: isize;
                core::arch::asm!(
                    "svc #0",
                    inlateout("x8") SYS_MADVISE as isize => _,
                    inlateout("x0") start           => ret,
                    in("x1") len,
                    in("x2") MADV_DONTNEED as isize,
                    options(nostack, preserves_flags),
                );
                let _ = ret;
            }
        }

        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        let _ = buf;
    }

    #[cfg(not(target_os = "linux"))]
    let _ = buf;
}

// ── Existing sequential / cold / free hints ──────────────────────────────────

/// Hint that we are about to do a full sequential scan of this pixel buffer.
#[inline]
pub fn pixels_sequential(buf: &[u8]) {
    #[cfg(target_os = "linux")]
    madvise_linux(buf, 2 /* MADV_SEQUENTIAL */);
    #[cfg(not(target_os = "linux"))]
    let _ = buf;
}

/// Hint that we will not access this pixel buffer again soon.
#[inline]
pub fn pixels_cold(buf: &[u8]) {
    #[cfg(target_os = "linux")]
    madvise_linux(buf, 4 /* MADV_DONTNEED — reclaim pages */);
    #[cfg(not(target_os = "linux"))]
    let _ = buf;
}

/// Same as `pixels_cold`; name kept for call-site readability.
#[inline]
pub fn pixels_free(buf: &[u8]) {
    pixels_cold(buf);
}

// ── Internal helpers ─────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[inline(always)]
fn page_size() -> usize {
    // getpagesize / sysconf(_SC_PAGESIZE) — cache result at program start if needed,
    // but for a wallpaper daemon one syscall on teardown is negligible.
    #[cfg(target_arch = "x86_64")]
    const SYS_GETPAGESIZE: usize = 12;
    #[cfg(target_arch = "aarch64")]
    const SYS_GETPAGESIZE: usize = 29;

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    unsafe {
        let mut ret: usize;
        #[cfg(target_arch = "x86_64")]
        core::arch::asm!(
            "syscall",
            inlateout("rax") SYS_GETPAGESIZE => ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack, preserves_flags),
        );
        #[cfg(target_arch = "aarch64")]
        core::arch::asm!(
            "svc #0",
            inlateout("x8") SYS_GETPAGESIZE => _,
            inlateout("x0") 0usize => ret,
            options(nostack, preserves_flags),
        );
        if ret == 0 { 4096 } else { ret }
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    4096
}

#[cfg(target_os = "linux")]
#[inline]
fn madvise_linux(buf: &[u8], advice: i32) {
    #[cfg(target_arch = "x86_64")]
    const SYS_MADVISE: usize = 28;
    #[cfg(target_arch = "aarch64")]
    const SYS_MADVISE: usize = 233;

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    unsafe {
        let page  = page_size();
        let addr  = buf.as_ptr() as usize;
        let start = (addr + page - 1) & !(page - 1);
        let end   = (addr + buf.len()) & !(page - 1);
        if end <= start { return; }
        let len = end - start;

        #[cfg(target_arch = "x86_64")]
        {
            let mut ret: isize;
            core::arch::asm!(
                "syscall",
                inlateout("rax") SYS_MADVISE as isize => ret,
                in("rdi") start,
                in("rsi") len,
                in("rdx") advice as isize,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack, preserves_flags),
            );
            let _ = ret;
        }
        #[cfg(target_arch = "aarch64")]
        {
            let mut ret: isize;
            core::arch::asm!(
                "svc #0",
                inlateout("x8") SYS_MADVISE as isize => _,
                inlateout("x0") start => ret,
                in("x1") len,
                in("x2") advice as isize,
                options(nostack, preserves_flags),
            );
            let _ = ret;
        }
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    let _ = (buf, advice);
}
