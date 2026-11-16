// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Bare-Metal UEFI GOP Fatal Diagnostic Console.
//!
//! Renders fatal panics, unrecoverable CPU exceptions, and hardware init failures directly onto
//! the validated UEFI GOP framebuffer (`hhdm_virt` early, `KERNEL_GOP_DIAG_BASE` after `CR3` handover)
//! so laptops without a physical RS-232 COM port display actionable diagnostics instead of silent hangs.

use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::ptr::write_volatile;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::display::framebuffer_summary;
use crate::limine::get_latched_framebuffer;
use crate::serial::copy_diag_ring_tail_panic;
use crate::vspace::pte::KERNEL_GOP_DIAG_BASE;

static KERNEL_GOP_DIAG_MAPPED: AtomicBool = AtomicBool::new(false);

struct PanicScratch {
    hdr: [u8; 256],
    hdr_len: usize,
    log: [u8; 3072],
}

struct SyncScratch(UnsafeCell<PanicScratch>);
// SAFETY: Accessed strictly after `enter_fatal_or_halt()` acquires exclusive panic ownership.
unsafe impl Sync for SyncScratch {}

static PANIC_SCRATCH: SyncScratch = SyncScratch(UnsafeCell::new(PanicScratch {
    hdr: [0; 256],
    hdr_len: 0,
    log: [0; 3072],
}));

#[inline]
pub fn set_kernel_gop_diag_mapped(mapped: bool) {
    KERNEL_GOP_DIAG_MAPPED.store(mapped, Ordering::Release);
}

struct BufWriter<'a> {
    buf: &'a mut [u8],
    len: &'a mut usize,
}

impl Write for BufWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if *self.len >= self.buf.len() {
                break;
            }
            self.buf[*self.len] = b;
            *self.len += 1;
        }
        Ok(())
    }
}

/// 5x7 column bitmap (`bit0`=top..`bit6`=bottom) for readable 2x-scaled fatal console output.
fn glyph_5x7(ch: u8) -> [u8; 5] {
    let c = if ch.is_ascii_lowercase() { ch - 32 } else { ch };
    match c {
        b'A' => [0x7e, 0x09, 0x09, 0x09, 0x7e],
        b'B' => [0x7f, 0x49, 0x49, 0x49, 0x36],
        b'C' => [0x3e, 0x41, 0x41, 0x41, 0x22],
        b'D' => [0x7f, 0x41, 0x41, 0x41, 0x3e],
        b'E' => [0x7f, 0x49, 0x49, 0x49, 0x41],
        b'F' => [0x7f, 0x09, 0x09, 0x09, 0x01],
        b'G' => [0x3e, 0x41, 0x49, 0x49, 0x7a],
        b'H' => [0x7f, 0x08, 0x08, 0x08, 0x7f],
        b'I' => [0x00, 0x41, 0x7f, 0x41, 0x00],
        b'J' => [0x20, 0x40, 0x41, 0x3f, 0x01],
        b'K' => [0x7f, 0x08, 0x14, 0x22, 0x41],
        b'L' => [0x7f, 0x40, 0x40, 0x40, 0x40],
        b'M' => [0x7f, 0x02, 0x0c, 0x02, 0x7f],
        b'N' => [0x7f, 0x04, 0x08, 0x10, 0x7f],
        b'O' => [0x3e, 0x41, 0x41, 0x41, 0x3e],
        b'P' => [0x7f, 0x09, 0x09, 0x09, 0x06],
        b'Q' => [0x3e, 0x41, 0x51, 0x21, 0x5e],
        b'R' => [0x7f, 0x09, 0x19, 0x29, 0x46],
        b'S' => [0x46, 0x49, 0x49, 0x49, 0x31],
        b'T' => [0x01, 0x01, 0x7f, 0x01, 0x01],
        b'U' => [0x3f, 0x40, 0x40, 0x40, 0x3f],
        b'V' => [0x1f, 0x20, 0x40, 0x20, 0x1f],
        b'W' => [0x3f, 0x40, 0x38, 0x40, 0x3f],
        b'X' => [0x63, 0x14, 0x08, 0x14, 0x63],
        b'Y' => [0x07, 0x08, 0x70, 0x08, 0x07],
        b'Z' => [0x61, 0x51, 0x49, 0x45, 0x43],
        b'0' => [0x3e, 0x51, 0x49, 0x45, 0x3e],
        b'1' => [0x00, 0x42, 0x7f, 0x40, 0x00],
        b'2' => [0x42, 0x61, 0x51, 0x49, 0x46],
        b'3' => [0x21, 0x41, 0x45, 0x4b, 0x31],
        b'4' => [0x18, 0x14, 0x12, 0x7f, 0x10],
        b'5' => [0x27, 0x45, 0x45, 0x45, 0x39],
        b'6' => [0x3c, 0x4a, 0x49, 0x49, 0x30],
        b'7' => [0x01, 0x71, 0x09, 0x05, 0x03],
        b'8' => [0x36, 0x49, 0x49, 0x49, 0x36],
        b'9' => [0x06, 0x49, 0x49, 0x29, 0x1e],
        b'-' => [0x08, 0x08, 0x08, 0x08, 0x08],
        b'_' => [0x40, 0x40, 0x40, 0x40, 0x40],
        b':' => [0x00, 0x36, 0x36, 0x00, 0x00],
        b'.' => [0x00, 0x60, 0x60, 0x00, 0x00],
        b',' => [0x00, 0x50, 0x30, 0x00, 0x00],
        b'/' => [0x20, 0x10, 0x08, 0x04, 0x02],
        b'=' => [0x14, 0x14, 0x14, 0x14, 0x14],
        b'+' => [0x08, 0x08, 0x3e, 0x08, 0x08],
        b'[' => [0x00, 0x7f, 0x41, 0x41, 0x00],
        b']' => [0x00, 0x41, 0x41, 0x7f, 0x00],
        b'(' => [0x00, 0x1c, 0x22, 0x41, 0x00],
        b')' => [0x00, 0x41, 0x22, 0x1c, 0x00],
        b'#' => [0x14, 0x7f, 0x14, 0x7f, 0x14],
        b'!' => [0x00, 0x00, 0x5f, 0x00, 0x00],
        b'|' => [0x00, 0x00, 0x7f, 0x00, 0x00],
        b'>' => [0x41, 0x22, 0x14, 0x08, 0x00],
        b'<' => [0x08, 0x14, 0x22, 0x41, 0x00],
        _ => [0; 5],
    }
}

#[inline]
fn pack_rgb(r: u8, g: u8, b: u8, rs: u8, gs: u8, bs: u8) -> u32 {
    ((r as u32) << (rs & 31)) | ((g as u32) << (gs & 31)) | ((b as u32) << (bs & 31))
}

#[derive(Clone, Copy)]
struct FbView {
    base: *mut u8,
    pitch: usize,
    w: usize,
    h: usize,
}

unsafe fn fill_rect(fb: FbView, x0: usize, y0: usize, rw: usize, rh: usize, px: u32) {
    let y_end = (y0 + rh).min(fb.h);
    let x_end = (x0 + rw).min(fb.w);
    for y in y0..y_end {
        let row = unsafe { fb.base.add(y * fb.pitch) as *mut u32 };
        for x in x0..x_end {
            unsafe { write_volatile(row.add(x), px) };
        }
    }
}

unsafe fn draw_char_2x(fb: FbView, x0: usize, y0: usize, ch: u8, fg: u32) {
    let cols = glyph_5x7(ch);
    for (cx, &bits) in cols.iter().enumerate() {
        for cy in 0..7usize {
            if ((bits >> cy) & 1) == 0 {
                continue;
            }
            let px_x = x0 + cx * 2;
            let px_y = y0 + cy * 2;
            if px_x + 1 < fb.w && px_y + 1 < fb.h {
                unsafe { fill_rect(fb, px_x, px_y, 2, 2, fg) };
            }
        }
    }
}

unsafe fn draw_line(fb: FbView, x: usize, y: usize, s: &[u8], fg: u32) {
    let max_cols = fb.w.saturating_sub(x) / 12;
    for (i, &b) in s.iter().take(max_cols).enumerate() {
        unsafe { draw_char_2x(fb, x + i * 12, y, b, fg) };
    }
}

/// Render a high-contrast bare-metal diagnostic screen to UEFI GOP when a kernel panic or fatal error hits.
#[inline(never)]
pub fn render_fatal_screen(title: &str, detail: fmt::Arguments<'_>) {
    let (base_va, w, h, pitch, rs, gs, bs) = if let Some(vfb) = framebuffer_summary() {
        let va = if KERNEL_GOP_DIAG_MAPPED.load(Ordering::Acquire) {
            KERNEL_GOP_DIAG_BASE
        } else if let Some(lfb) = get_latched_framebuffer() {
            lfb.hhdm_virt
        } else {
            return;
        };
        (
            va,
            vfb.info.width as usize,
            vfb.info.height as usize,
            vfb.info.pitch as usize,
            vfb.info.red_shift,
            vfb.info.green_shift,
            vfb.info.blue_shift,
        )
    } else if let Some(lfb) = get_latched_framebuffer() {
        if KERNEL_GOP_DIAG_MAPPED.load(Ordering::Acquire) || lfb.bpp != 32 {
            return;
        }
        (
            lfb.hhdm_virt,
            lfb.width as usize,
            lfb.height as usize,
            lfb.pitch as usize,
            lfb.red_mask_shift,
            lfb.green_mask_shift,
            lfb.blue_mask_shift,
        )
    } else {
        return;
    };

    if w < 320 || h < 200 || pitch < w * 4 || base_va == 0 {
        return;
    }

    // SAFETY: Protected by `enter_fatal_or_halt()`; `base_va` maps `pitch * h` UC GOP aperture.
    unsafe {
        let scratch = &mut *PANIC_SCRATCH.0.get();
        scratch.hdr_len = 0;
        let _ = BufWriter {
            buf: &mut scratch.hdr,
            len: &mut scratch.hdr_len,
        }
        .write_fmt(detail);
        let log_n = copy_diag_ring_tail_panic(&mut scratch.log);

        let fb = FbView {
            base: base_va as *mut u8,
            pitch,
            w,
            h,
        };
        let bg = pack_rgb(15, 23, 42, rs, gs, bs);
        let hdr_bg = pack_rgb(153, 27, 27, rs, gs, bs);
        let fg_white = pack_rgb(248, 250, 252, rs, gs, bs);
        let fg_muted = pack_rgb(203, 213, 225, rs, gs, bs);
        let fg_amber = pack_rgb(252, 211, 77, rs, gs, bs);

        fill_rect(fb, 0, 0, w, h, bg);
        fill_rect(fb, 0, 0, w, 44, hdr_bg);
        draw_line(fb, 16, 14, title.as_bytes(), fg_white);
        draw_line(fb, 16, 56, &scratch.hdr[..scratch.hdr_len], fg_amber);

        let max_lines = h.saturating_sub(100) / 18;
        let tail = &scratch.log[..log_n];
        let mut line_starts = [0usize; 64];
        let mut line_cnt = 0usize;
        let mut cur_start = 0usize;
        for (idx, &b) in tail.iter().enumerate() {
            if b == b'\n' {
                if idx > cur_start {
                    line_starts[line_cnt & 63] = cur_start;
                    line_cnt += 1;
                }
                cur_start = idx + 1;
            }
        }
        if cur_start < tail.len() {
            line_starts[line_cnt & 63] = cur_start;
            line_cnt += 1;
        }
        let show = line_cnt.min(max_lines).min(64);
        let first_idx = line_cnt.saturating_sub(show);
        for row in 0..show {
            let st = line_starts[(first_idx + row) & 63];
            let rem = &tail[st..];
            let end = rem.iter().position(|&b| b == b'\n' || b == b'\r').unwrap_or(rem.len());
            draw_line(fb, 16, 84 + row * 18, &rem[..end], fg_muted);
        }
    }
}
