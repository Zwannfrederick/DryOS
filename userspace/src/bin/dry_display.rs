// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry_display` — Isolated Ring 3 (`CPL=3`) Linear Framebuffer Visual Proof (`Milestone 1C` Stage E).
//!
//! Maps the UEFI GOP framebuffer exclusively via `SYS_FB_MAP` (`CapObjectKind::Framebuffer`),
//! renders a crisp geometric dark-slate diagnostic card + 8x16 pixel-accurate bitmap typography
//! with zero floating-point/SIMD instructions, flushes WC/UC stores (`sfence`), and reports
//! completion to `dry_init` over Capability IPC.

#![no_std]
#![no_main]

use core::arch::asm;
use core::ptr::write_volatile;
use dry_userspace::{
    pack_msg_tag, read_cs_ss, sys_exit, sys_fb_map, sys_ipc_call_inline, sys_write_debug,
    UserFbInfo, IPC_OP_DISPLAY_DONE,
};

const FB_MAP_VADDR: u64 = 0x1000_0000;
const BOX_HORIZ: u8 = 0x80; // Internal byte token for `─` (`U+2500`)

#[inline]
fn pack_rgb(info: &UserFbInfo, r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << (info.red_shift & 31))
        | ((g as u32) << (info.green_shift & 31))
        | ((b as u32) << (info.blue_shift & 31))
}

#[inline]
fn put_px(base: *mut u32, stride_px: usize, w: usize, h: usize, x: usize, y: usize, color: u32) {
    if x < w && y < h {
        // SAFETY: `x < w` and `y < h` lie strictly within `info.pitch * info.height` mapped via `SYS_FB_MAP`.
        unsafe {
            write_volatile(base.add(y * stride_px + x), color);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_rect(
    base: *mut u32,
    stride_px: usize,
    fb_w: usize,
    fb_h: usize,
    rx: usize,
    ry: usize,
    rw: usize,
    rh: usize,
    color: u32,
) {
    let y_end = (ry + rh).min(fb_h);
    let x_end = (rx + rw).min(fb_w);
    for y in ry..y_end {
        let row_off = y * stride_px;
        for x in rx..x_end {
            // SAFETY: Bounds clamped to `(fb_w, fb_h)` inside mapped GOP framebuffer.
            unsafe {
                write_volatile(base.add(row_off + x), color);
            }
        }
    }
}

/// Crisp 8x16 monospace bitmap glyph generator covering every glyph in the Stage E banner.
const fn glyph_8x16(ch: u8) -> [u8; 16] {
    match ch {
        BOX_HORIZ | b'-' => [0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0],
        b':' => [0, 0, 0, 0, 0x18, 0x18, 0, 0, 0, 0x18, 0x18, 0, 0, 0, 0, 0],
        b'!' => [0, 0, 0x18, 0x3C, 0x3C, 0x3C, 0x18, 0x18, 0x18, 0, 0x18, 0x18, 0, 0, 0, 0],
        b'3' => [0, 0, 0x3C, 0x66, 0x06, 0x06, 0x1C, 0x06, 0x06, 0x06, 0x66, 0x3C, 0, 0, 0, 0],
        b'A' => [0, 0, 0x18, 0x3C, 0x66, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x66, 0x66, 0, 0, 0, 0],
        b'C' => [0, 0, 0x3C, 0x66, 0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x66, 0x3C, 0, 0, 0, 0],
        b'D' => [0, 0, 0x78, 0x6C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x6C, 0x78, 0, 0, 0, 0],
        b'E' => [0, 0, 0x7E, 0x60, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x60, 0x60, 0x7E, 0, 0, 0, 0],
        b'H' => [0, 0, 0x66, 0x66, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x66, 0x66, 0x66, 0, 0, 0, 0],
        b'I' => [0, 0, 0x3C, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0, 0, 0, 0],
        b'K' => [0, 0, 0x66, 0x6C, 0x78, 0x70, 0x70, 0x78, 0x6C, 0x66, 0x66, 0x66, 0, 0, 0, 0],
        b'L' => [0, 0, 0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x7E, 0, 0, 0, 0],
        b'M' => [0, 0, 0x63, 0x77, 0x7F, 0x6B, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0, 0, 0, 0],
        b'N' => [0, 0, 0x66, 0x76, 0x7E, 0x7E, 0x6E, 0x66, 0x66, 0x66, 0x66, 0x66, 0, 0, 0, 0],
        b'O' => [0, 0, 0x3C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0, 0, 0, 0],
        b'P' => [0, 0, 0x7C, 0x66, 0x66, 0x66, 0x7C, 0x60, 0x60, 0x60, 0x60, 0x60, 0, 0, 0, 0],
        b'R' => [0, 0, 0x7C, 0x66, 0x66, 0x66, 0x7C, 0x78, 0x6C, 0x66, 0x66, 0x66, 0, 0, 0, 0],
        b'S' => [0, 0, 0x3C, 0x66, 0x60, 0x30, 0x1C, 0x06, 0x06, 0x66, 0x66, 0x3C, 0, 0, 0, 0],
        b'T' => [0, 0, 0x7E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0, 0, 0, 0],
        b'U' => [0, 0, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0, 0, 0, 0],
        b'V' => [0, 0, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x18, 0x18, 0, 0, 0, 0],
        b'Y' => [0, 0, 0x66, 0x66, 0x66, 0x3C, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0, 0, 0, 0],
        b'a' => [0, 0, 0, 0, 0, 0x3C, 0x06, 0x3E, 0x66, 0x66, 0x6E, 0x3B, 0, 0, 0, 0],
        b'c' => [0, 0, 0, 0, 0, 0x3C, 0x66, 0x60, 0x60, 0x60, 0x66, 0x3C, 0, 0, 0, 0],
        b'e' => [0, 0, 0, 0, 0, 0x3C, 0x66, 0x7E, 0x60, 0x60, 0x66, 0x3C, 0, 0, 0, 0],
        b'f' => [0, 0, 0x1C, 0x30, 0x30, 0x7C, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0, 0, 0, 0],
        b'g' => [0, 0, 0, 0, 0, 0x3E, 0x66, 0x66, 0x66, 0x66, 0x3E, 0x06, 0x66, 0x3C, 0, 0],
        b'i' => [0, 0, 0x18, 0x18, 0, 0x38, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0, 0, 0, 0],
        b'k' => [0, 0, 0x60, 0x60, 0x60, 0x66, 0x6C, 0x78, 0x78, 0x6C, 0x66, 0x66, 0, 0, 0, 0],
        b'l' => [0, 0, 0x38, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0, 0, 0, 0],
        b'm' => [0, 0, 0, 0, 0, 0x6E, 0x7F, 0x6B, 0x6B, 0x6B, 0x63, 0x63, 0, 0, 0, 0],
        b'n' => [0, 0, 0, 0, 0, 0x7C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0, 0, 0, 0],
        b'o' => [0, 0, 0, 0, 0, 0x3C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0, 0, 0, 0],
        b'p' => [0, 0, 0, 0, 0, 0x7C, 0x66, 0x66, 0x66, 0x7C, 0x60, 0x60, 0x60, 0x60, 0, 0],
        b'r' => [0, 0, 0, 0, 0, 0x6E, 0x76, 0x60, 0x60, 0x60, 0x60, 0x60, 0, 0, 0, 0],
        b's' => [0, 0, 0, 0, 0, 0x3E, 0x60, 0x3C, 0x06, 0x06, 0x66, 0x3C, 0, 0, 0, 0],
        b't' => [0, 0, 0x10, 0x30, 0x30, 0x7C, 0x30, 0x30, 0x30, 0x30, 0x36, 0x1C, 0, 0, 0, 0],
        b'y' => [0, 0, 0, 0, 0, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3E, 0x06, 0x66, 0x3C, 0, 0],
        _ => [0; 16],
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_bytes(
    base: *mut u32,
    stride_px: usize,
    fb_w: usize,
    fb_h: usize,
    start_x: usize,
    start_y: usize,
    scale: usize,
    text: &[u8],
    color: u32,
) -> u64 {
    let mut fg_count = 0u64;
    let adv_x = 9 * scale;
    for (idx, &ch) in text.iter().enumerate() {
        let g = glyph_8x16(ch);
        let gx = start_x + idx * adv_x;
        for (row, &bits) in g.iter().enumerate() {
            if bits == 0 {
                continue;
            }
            for col in 0..8usize {
                if (bits & (0x80 >> col)) != 0 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            put_px(
                                base,
                                stride_px,
                                fb_w,
                                fb_h,
                                gx + col * scale + sx,
                                start_y + row * scale + sy,
                                color,
                            );
                            fg_count += 1;
                        }
                    }
                }
            }
        }
    }
    fg_count
}

#[no_mangle]
pub extern "C" fn _start(disp_ep_cap: u64, fb_cap: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || disp_ep_cap == 0 || fb_cap == 0 {
        sys_exit(110);
    }

    let mut info = UserFbInfo::default();
    if sys_fb_map(fb_cap, FB_MAP_VADDR, &mut info).is_err() {
        sys_exit(111);
    }

    let w = info.width as usize;
    let h = info.height as usize;
    let stride_px = (info.pitch as usize) / 4;
    if w < 320 || h < 240 || stride_px < w {
        sys_exit(112);
    }
    let fb_ptr = FB_MAP_VADDR as *mut u32;

    // Studio color tokens (WCAG AAA high-contrast on deep obsidian/slate, zero gradients).
    let bg_canvas = pack_rgb(&info, 0x0B, 0x0F, 0x17);
    let bg_card = pack_rgb(&info, 0x11, 0x18, 0x27);
    let border_col = pack_rgb(&info, 0x28, 0x37, 0x4D);
    let accent_cyan = pack_rgb(&info, 0x22, 0xD3, 0xEE);
    let accent_ok = pack_rgb(&info, 0x34, 0xD3, 0x99);
    let fg_primary = pack_rgb(&info, 0xF8, 0xFA, 0xFC);
    let fg_label = pack_rgb(&info, 0x94, 0xA3, 0xB8);

    // 1. Clear full screen to deep obsidian `#0B0F17`.
    fill_rect(fb_ptr, stride_px, w, h, 0, 0, w, h, bg_canvas);

    let scale = if w >= 800 && h >= 520 { 2usize } else { 1usize };
    let card_w = (320 * scale).min(w.saturating_sub(32));
    let card_h = (220 * scale).min(h.saturating_sub(32));
    let card_x = (w - card_w) / 2;
    let card_y = (h - card_h) / 2;

    // 2. Geometric studio diagnostic frame: 2px crisp border + solid top status bar.
    fill_rect(fb_ptr, stride_px, w, h, card_x, card_y, card_w, card_h, border_col);
    fill_rect(
        fb_ptr,
        stride_px,
        w,
        h,
        card_x + 2,
        card_y + 2,
        card_w - 4,
        card_h - 4,
        bg_card,
    );
    fill_rect(
        fb_ptr,
        stride_px,
        w,
        h,
        card_x + 2,
        card_y + 2,
        card_w - 4,
        4 * scale,
        accent_cyan,
    );

    let tx = card_x + 24 * scale;
    let mut ty = card_y + 22 * scale;
    let line_step = 22 * scale;
    let mut fg_pixels = 0u64;

    // 3. Render exact Stage E.2 banner lines.
    fg_pixels += draw_bytes(fb_ptr, stride_px, w, h, tx, ty, scale, b"DRY OS", fg_primary);
    ty += line_step;

    let divider = [BOX_HORIZ; 26];
    fg_pixels += draw_bytes(fb_ptr, stride_px, w, h, tx, ty, scale, &divider, accent_cyan);
    ty += line_step;

    fg_pixels += draw_bytes(fb_ptr, stride_px, w, h, tx, ty, scale, b"System: ", fg_label);
    fg_pixels += draw_bytes(
        fb_ptr,
        stride_px,
        w,
        h,
        tx + 8 * 9 * scale,
        ty,
        scale,
        b"ONLINE",
        accent_ok,
    );
    ty += line_step;

    fg_pixels += draw_bytes(fb_ptr, stride_px, w, h, tx, ty, scale, b"Kernel: ", fg_label);
    fg_pixels += draw_bytes(
        fb_ptr,
        stride_px,
        w,
        h,
        tx + 8 * 9 * scale,
        ty,
        scale,
        b"Microkernel",
        fg_primary,
    );
    ty += line_step;

    fg_pixels += draw_bytes(fb_ptr, stride_px, w, h, tx, ty, scale, b"Userspace: ", fg_label);
    fg_pixels += draw_bytes(
        fb_ptr,
        stride_px,
        w,
        h,
        tx + 11 * 9 * scale,
        ty,
        scale,
        b"Ring 3",
        fg_primary,
    );
    ty += line_step;

    fg_pixels += draw_bytes(fb_ptr, stride_px, w, h, tx, ty, scale, b"IPC: ", fg_label);
    fg_pixels += draw_bytes(
        fb_ptr,
        stride_px,
        w,
        h,
        tx + 5 * 9 * scale,
        ty,
        scale,
        b"ACTIVE",
        accent_ok,
    );
    ty += line_step + 10 * scale;

    fg_pixels += draw_bytes(
        fb_ptr,
        stride_px,
        w,
        h,
        tx,
        ty,
        scale,
        b"Hello from DRY OS!",
        accent_cyan,
    );

    // Flush store buffer to uncached/write-combining GOP video memory before notifying supervisor.
    // SAFETY: `sfence` is unprivileged on x86-64 (`-sse` independent architectural store fence:
    // note `lock add dword ptr [rsp - 8], 0` works universally even with `-C target-feature=-sse`!).
    unsafe {
        asm!("lock add dword ptr [rsp - 8], 0", options(nostack));
    }

    let _ = sys_write_debug(b"[DRY-DISPLAY] Rendered Ring 3 framebuffer banner\n");
    let _ = sys_ipc_call_inline(
        disp_ep_cap,
        pack_msg_tag(IPC_OP_DISPLAY_DONE, 0, 3, 0),
        [w as u64, h as u64, fg_pixels, 0],
        0,
    );
    sys_exit(0);
}
