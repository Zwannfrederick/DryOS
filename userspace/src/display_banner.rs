// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Legacy Milestone 1C One-Shot Framebuffer Banner (`Stage E.2`).
//! Showcases the original DRY OS Iso-Kernel Prism + wordmark lockup while preserving `leaked_frames == 0`.

use core::ptr::write_volatile;
use core::slice::from_raw_parts_mut;

use crate::compositor::{flush_video_stores, pack_rgb, Palette};
use crate::gfx::{glyph_8x16, Rect, BOX_HORIZ};
use crate::glass::theme::ThemeMode;
use crate::ttf::{FontId, PixelFormat};
use crate::ui::brand::draw_boot_lockup;
use crate::ui::canvas::Canvas;
use crate::ui::icons::{draw_vector_icon, IconKind};
use crate::ui::theme::UiTheme;
use crate::{
    pack_msg_tag, sys_cap_revoke, sys_exit, sys_frame_alloc, sys_ipc_call_inline, sys_vspace_map,
    sys_write_debug, UserFbInfo, IPC_OP_DISPLAY_DONE, VSPACE_MAP_OP_2M_RW, VSPACE_MAP_OP_UNMAP_2M,
};

const BANNER_SCRATCH_VA: u64 = 0x2400_0000;

#[inline]
fn put_px_mmio(base: *mut u32, stride_px: usize, w: usize, h: usize, x: usize, y: usize, color: u32) {
    if x < w && y < h {
        // SAFETY: `(x, y)` lies strictly inside mapped GOP framebuffer.
        unsafe { write_volatile(base.add(y * stride_px + x), color) };
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
            unsafe { write_volatile(base.add(row_off + x), color) };
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
                            put_px_mmio(base, stride_px, fb_w, fb_h, gx + col * scale + sx, start_y + row * scale + sy, color);
                            fg_count += 1;
                        }
                    }
                }
            }
        }
    }
    fg_count
}

fn render_aa_banner_card(
    fb_ptr: *mut u32,
    stride_px: usize,
    w: usize,
    h: usize,
    info: &UserFbInfo,
) -> Option<u64> {
    let cap = sys_frame_alloc(1).ok()?;
    if sys_vspace_map(cap, BANNER_SCRATCH_VA, VSPACE_MAP_OP_2M_RW).is_err() {
        let _ = sys_cap_revoke(cap, 2);
        return None;
    }

    let cw = 640usize.min(w.saturating_sub(32));
    let ch = 400usize.min(h.saturating_sub(32));
    if cw < 320 || ch < 220 || cw * ch > 524_288 {
        let _ = sys_vspace_map(cap, BANNER_SCRATCH_VA, VSPACE_MAP_OP_UNMAP_2M);
        let _ = sys_cap_revoke(cap, 2);
        return None;
    }

    let fmt = PixelFormat::from_fb(info);
    let ui = UiTheme::new(ThemeMode::ObsidianDark, 100);
    // SAFETY: `BANNER_SCRATCH_VA` maps a 2 MiB RW frame and `cw * ch <= 524_288`.
    let buf = unsafe { from_raw_parts_mut(BANNER_SCRATCH_VA as *mut u32, cw * ch) };
    let mut c = Canvas::from_slice(buf, cw, ch);
    c.fmt = fmt;
    c.clear(0x000B_0F18);

    c.fill_bordered_rrect_aa(Rect::new(0, 0, cw, ch), 14, ui.bg_card, ui.border_strong);
    c.fill_rect(Rect::new(14, 1, cw.saturating_sub(28), 3), ui.accent);

    let lock_w = 320usize.min(cw.saturating_sub(48));
    let lock_h = (lock_w / 4).max(48);
    draw_boot_lockup(
        &mut c,
        Rect::new((cw - lock_w) / 2, 22, lock_w, lock_h),
        ThemeMode::ObsidianDark,
    );

    let div_y = 22 + lock_h + 16;
    c.hline(28, div_y, cw.saturating_sub(56), ui.border_subtle, 255);

    let rows: [(IconKind, &[u8], &[u8], u32); 4] = [
        (IconKind::StatusSuccess, b"System State", b"ONLINE (UEFI GOP + VirtIO Ready)", ui.ok_fg),
        (IconKind::Cpu, b"Architecture", b"Capability Microkernel (Zero Ambient)", ui.fg_primary),
        (IconKind::Shield, b"Execution Ring", b"CPL=3 Isolated Userspace Compartments", ui.fg_primary),
        (IconKind::DriverIsland, b"IPC & Display", b"Synchronous Message Passing Active", ui.ok_fg),
    ];
    let mut ry = div_y + 18;
    for &(ic, key, val, col) in &rows {
        let card_r = Rect::new(28, ry, cw.saturating_sub(56), 44);
        c.fill_bordered_rrect_aa(card_r, 8, ui.bg_card_alt, ui.border_subtle);
        draw_vector_icon(&mut c, Rect::new(card_r.x + 12, card_r.y + 13, 18, 18), ic, col, ui.accent_fg);
        c.draw_text((card_r.x + 40) as i32, (card_r.y + 13) as i32, FontId::SansSemiBold, 13, key, ui.fg_muted);
        c.draw_text((card_r.x + 168) as i32, (card_r.y + 13) as i32, FontId::SansRegular, 13, val, col);
        ry += 52;
    }

    c.draw_text_centered(
        Rect::new(28, ch.saturating_sub(42), cw.saturating_sub(56), 24),
        FontId::SansSemiBold,
        14,
        b"Hello from DRY OS!",
        ui.accent_fg,
    );

    let bg_packed = fmt.pack_rgb24(0x000B_0F18);
    let mut fg_pixels = 0u64;
    for &px in buf.iter() {
        if px != bg_packed {
            fg_pixels += 1;
        }
    }

    let ox = (w - cw) / 2;
    let oy = (h - ch) / 2;
    for row in 0..ch {
        let src = &buf[row * cw..(row + 1) * cw];
        let dst_off = (oy + row) * stride_px + ox;
        for (col, &px) in src.iter().enumerate() {
            // SAFETY: `(ox + col, oy + row)` lies within `(w, h)`.
            unsafe { write_volatile(fb_ptr.add(dst_off + col), px) };
        }
    }

    let _ = sys_vspace_map(cap, BANNER_SCRATCH_VA, VSPACE_MAP_OP_UNMAP_2M);
    let _ = sys_cap_revoke(cap, 2);
    Some(fg_pixels.max(1))
}

fn render_fallback_mmio_card(
    fb_ptr: *mut u32,
    stride_px: usize,
    w: usize,
    h: usize,
    info: &UserFbInfo,
    pal: &Palette,
) -> u64 {
    let bg_card = pack_rgb(info, 0x11, 0x18, 0x27);
    let border_col = pack_rgb(info, 0x28, 0x37, 0x4D);
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
    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx, ty, scale, b"System: ONLINE", pal.fg_ok);
    ty += line_step;
    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx, ty, scale, b"Userspace: Ring 3", pal.fg_primary);
    ty += line_step + 10 * scale;
    fg_pixels += draw_bytes_mmio(fb_ptr, stride_px, w, h, tx, ty, scale, b"Hello from DRY OS!", pal.fg_accent);
    fg_pixels
}

/// Legacy Milestone 1C one-shot visual proof (`Stage E.2`) with DRY OS Prism lockup.
pub fn run_legacy_banner_mode(fb_vaddr: u64, disp_ep_cap: u64, info: &UserFbInfo) -> ! {
    let w = info.width as usize;
    let h = info.height as usize;
    let stride_px = (info.pitch as usize) / 4;
    let fb_ptr = fb_vaddr as *mut u32;
    let pal = Palette::from_fb(info);
    let bg_rgb = pack_rgb(info, 0x0B, 0x0F, 0x18);

    fill_rect_mmio(fb_ptr, stride_px, w, h, 0, 0, w, h, bg_rgb);
    let fg_pixels = render_aa_banner_card(fb_ptr, stride_px, w, h, info)
        .unwrap_or_else(|| render_fallback_mmio_card(fb_ptr, stride_px, w, h, info, &pal));

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
