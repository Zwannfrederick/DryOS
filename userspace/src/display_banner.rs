// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Legacy Milestone 1C One-Shot Framebuffer Diagnostic Banner (`Stage E.2`).
//! Extracted from `dry_display.rs` to keep binary modules clean and well below `< 800` lines.

use core::ptr::write_volatile;

use crate::compositor::{flush_video_stores, pack_rgb, Palette};
use crate::gfx::{glyph_8x16, BOX_HORIZ};
use crate::{
    pack_msg_tag, sys_exit, sys_ipc_call_inline, sys_write_debug, UserFbInfo, IPC_OP_DISPLAY_DONE,
};

#[inline]
fn put_px_mmio(base: *mut u32, stride_px: usize, w: usize, h: usize, x: usize, y: usize, color: u32) {
    if x < w && y < h {
        // SAFETY: `(x, y)` lies strictly inside mapped GOP framebuffer.
        unsafe {
            write_volatile(base.add(y * stride_px + x), color);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_rect_mmio(
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
            // SAFETY: Bounds clamped to `(fb_w, fb_h)`.
            unsafe {
                write_volatile(base.add(row_off + x), color);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_bytes_mmio(
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
                            put_px_mmio(
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

/// Legacy Milestone 1C one-shot visual proof (`Stage E.2`). Kept 100% intact for regression parity.
pub fn run_legacy_banner_mode(fb_vaddr: u64, disp_ep_cap: u64, info: &UserFbInfo) -> ! {
    let w = info.width as usize;
    let h = info.height as usize;
    let stride_px = (info.pitch as usize) / 4;
    let fb_ptr = fb_vaddr as *mut u32;
    let pal = Palette::from_fb(info);
    let bg_card = pack_rgb(info, 0x11, 0x18, 0x27);
    let border_col = pack_rgb(info, 0x28, 0x37, 0x4D);

    fill_rect_mmio(fb_ptr, stride_px, w, h, 0, 0, w, h, pal.bg_canvas);

    let scale = if w >= 800 && h >= 520 { 2usize } else { 1usize };
    let card_w = (320 * scale).min(w.saturating_sub(32));
    let card_h = (220 * scale).min(h.saturating_sub(32));
    let card_x = (w - card_w) / 2;
    let card_y = (h - card_h) / 2;

    fill_rect_mmio(fb_ptr, stride_px, w, h, card_x, card_y, card_w, card_h, border_col);
    fill_rect_mmio(fb_ptr, stride_px, w, h, card_x + 2, card_y + 2, card_w - 4, card_h - 4, bg_card);
    fill_rect_mmio(fb_ptr, stride_px, w, h, card_x + 2, card_y + 2, card_w - 4, 4 * scale, pal.fg_accent);

    let tx = card_x + 24 * scale;
    let mut ty = card_y + 22 * scale;
    let line_step = 22 * scale;
    let mut fg_pixels = 0u64;

    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx, ty, scale, b"DRY OS", pal.fg_primary);
    ty += line_step;

    let divider = [BOX_HORIZ; 26];
    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx, ty, scale, &divider, pal.fg_accent);
    ty += line_step;

    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx, ty, scale, b"System: ", pal.fg_muted);
    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx + 8 * 9 * scale, ty, scale, b"ONLINE", pal.fg_ok);
    ty += line_step;

    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx, ty, scale, b"Kernel: ", pal.fg_muted);
    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx + 8 * 9 * scale, ty, scale, b"Microkernel", pal.fg_primary);
    ty += line_step;

    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx, ty, scale, b"Userspace: ", pal.fg_muted);
    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx + 11 * 9 * scale, ty, scale, b"Ring 3", pal.fg_primary);
    ty += line_step;

    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx, ty, scale, b"IPC: ", pal.fg_muted);
    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx + 5 * 9 * scale, ty, scale, b"ACTIVE", pal.fg_ok);
    ty += line_step + 10 * scale;

    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx, ty, scale, b"Hello from DRY OS!", pal.fg_accent);

    flush_video_stores();
    let _ = sys_write_debug(b"[DRY-DISPLAY] Rendered Ring 3 framebuffer banner\n");
    let _ = sys_ipc_call_inline(
        disp_ep_cap,
        pack_msg_tag(IPC_OP_DISPLAY_DONE, 0, 3, 0),
        [w as u64, h as u64, fg_pixels, 0],
        0,
    );
    sys_exit(0);
}
