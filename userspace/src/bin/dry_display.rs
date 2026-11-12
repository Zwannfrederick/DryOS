// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry_display` — Isolated Ring 3 (`CPL=3`) Framebuffer Banner & P1 Desktop Compositor.
//!
//! Supports two modes selected by initial endpoint capability rights:
//! 1. **Milestone 1C Banner Mode (`SEND | CALL`):** Renders the Stage E diagnostic card once and
//!    reports `IPC_OP_DISPLAY_DONE` to `dry_init`.
//! 2. **P1 Interactive Compositor Mode (`RECV | REPLY`):** Allocates a WB RAM backbuffer (`Frame2M`),
//!    attaches `dry-shell`'s `SharedTerminalSurface` (`USER_RO` `Frame4K` via `MSG_FLAG_GRANT_CAP`),
//!    renders a movable floating terminal window, tracks `DirtyRegion`, and blits only damaged
//!    scanlines to the UEFI GOP framebuffer.

#![no_std]
#![no_main]

use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};
use dry_userspace::gfx::{glyph_8x16, DirtyRegion, Rect, BOX_HORIZ};
use dry_userspace::surface::{
    SharedTerminalSurface, SurfaceCell, STYLE_ACCENT, STYLE_HEADER, STYLE_MUTED, STYLE_OK,
    STYLE_PROMPT, STYLE_WARN, SURFACE_MAGIC, TERM_COLS, TERM_ROWS,
};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, rdtsc_user, read_cs_ss, sys_cap_query, sys_cap_revoke, sys_exit,
    sys_fb_map, sys_frame_alloc, sys_ipc_call_inline, sys_ipc_recv_ext, sys_ipc_reply_inline,
    sys_vspace_map, sys_write_debug, UserCapInfo, UserFbInfo, UserIpcRecvExt, IPC_OP_DISPLAY_DONE,
    IPC_OP_EXIT_CLEAN, IPC_OP_PONG, IPC_OP_SURFACE_ATTACH, IPC_OP_SURFACE_FOCUS,
    IPC_OP_SURFACE_MOVE, IPC_OP_SURFACE_PRESENT, RIGHT_RECV, VSPACE_MAP_OP_2M_RW,
    VSPACE_MAP_OP_4K_RO, VSPACE_MAP_OP_UNMAP_4K,
};

const FB_MAP_VADDR: u64 = 0x1000_0000;
const SURFACE_MAP_VADDR: u64 = 0x1800_0000;
const BACKBUF_VADDR: u64 = 0x2000_0000;
const TWO_MIB: usize = 2 * 1024 * 1024;

const TOPBAR_H: usize = 28;
const WIN_PAD_X: usize = 12;
const WIN_TITLE_H: usize = 28;
const WIN_PAD_BOTTOM: usize = 12;
const WIN_BORDER: usize = 2;
const SHADOW_OFF: usize = 6;
const WIN_W: usize = TERM_COLS * 8 + WIN_PAD_X * 2 + WIN_BORDER * 2; // 668 px
const WIN_H: usize = WIN_TITLE_H + TERM_ROWS * 16 + WIN_PAD_BOTTOM + WIN_BORDER * 2; // 428 px

#[derive(Clone, Copy)]
struct Palette {
    bg_canvas: u32,
    bg_grid: u32,
    bg_topbar: u32,
    bg_shadow: u32,
    bg_win: u32,
    bg_title: u32,
    border_focus: u32,
    border_idle: u32,
    fg_primary: u32,
    fg_muted: u32,
    fg_accent: u32,
    fg_ok: u32,
    fg_warn: u32,
    fg_prompt: u32,
}

impl Palette {
    fn from_fb(info: &UserFbInfo) -> Self {
        Self {
            bg_canvas: pack_rgb(info, 0x0B, 0x0F, 0x17),
            bg_grid: pack_rgb(info, 0x16, 0x20, 0x32),
            bg_topbar: pack_rgb(info, 0x0F, 0x17, 0x2A),
            bg_shadow: pack_rgb(info, 0x05, 0x07, 0x0C),
            bg_win: pack_rgb(info, 0x0F, 0x15, 0x23),
            bg_title: pack_rgb(info, 0x16, 0x20, 0x33),
            border_focus: pack_rgb(info, 0x22, 0xD3, 0xEE),
            border_idle: pack_rgb(info, 0x33, 0x41, 0x55),
            fg_primary: pack_rgb(info, 0xF8, 0xFA, 0xFC),
            fg_muted: pack_rgb(info, 0x94, 0xA3, 0xB8),
            fg_accent: pack_rgb(info, 0x22, 0xD3, 0xEE),
            fg_ok: pack_rgb(info, 0x34, 0xD3, 0x99),
            fg_warn: pack_rgb(info, 0xFB, 0xBF, 0x24),
            fg_prompt: pack_rgb(info, 0x38, 0xBD, 0xF8),
        }
    }

    #[inline]
    fn style_fg(&self, style: u8) -> u32 {
        match style {
            STYLE_PROMPT => self.fg_prompt,
            STYLE_ACCENT => self.fg_accent,
            STYLE_MUTED => self.fg_muted,
            STYLE_OK => self.fg_ok,
            STYLE_WARN => self.fg_warn,
            STYLE_HEADER => self.fg_accent,
            _ => self.fg_primary,
        }
    }
}

#[inline]
fn pack_rgb(info: &UserFbInfo, r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << (info.red_shift & 31))
        | ((g as u32) << (info.green_shift & 31))
        | ((b as u32) << (info.blue_shift & 31))
}

#[inline]
fn flush_video_stores() {
    // SAFETY: Architectural store fence compatible with `-C target-feature=-sse`.
    unsafe {
        asm!("lock add dword ptr [rsp - 8], 0", options(nostack));
    }
}

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
fn run_legacy_banner_mode(disp_ep_cap: u64, info: &UserFbInfo) -> ! {
    let w = info.width as usize;
    let h = info.height as usize;
    let stride_px = (info.pitch as usize) / 4;
    let fb_ptr = FB_MAP_VADDR as *mut u32;
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

struct Compositor {
    fb_ptr: *mut u32,
    back_ptr: *mut u32,
    w: usize,
    h: usize,
    fb_stride: usize,
    pal: Palette,
    win_x: usize,
    win_y: usize,
    focused: bool,
    last_cursor_row: usize,
    last_cmd_count: u32,
    last_ext_irqs: u32,
    surface_attached: bool,
    surface_cap: u64,
    dirty: DirtyRegion,
}

impl Compositor {
    #[inline]
    fn fill_back(&mut self, r: Rect, color: u32) {
        let c = r.clamp_to(self.w, self.h);
        if c.is_empty() {
            return;
        }
        for y in c.y..(c.y + c.h) {
            let row_base = y * self.w + c.x;
            // SAFETY: `back_ptr` spans `w * h` `u32` pixels in mapped WB RAM.
            let slice = unsafe { core::slice::from_raw_parts_mut(self.back_ptr.add(row_base), c.w) };
            slice.fill(color);
        }
    }

    fn draw_str_back(&mut self, x: usize, y: usize, text: &[u8], fg: u32, bg: u32) {
        for (i, &ch) in text.iter().enumerate() {
            self.draw_glyph_cell_back(x + i * 8, y, ch, fg, bg);
        }
    }

    #[inline]
    fn draw_glyph_cell_back(&mut self, gx: usize, gy: usize, ch: u8, fg: u32, bg: u32) {
        if gx + 8 > self.w || gy + 16 > self.h {
            return;
        }
        let g = glyph_8x16(ch);
        for (ry, &bits) in g.iter().enumerate() {
            let off = (gy + ry) * self.w + gx;
            for cx in 0..8usize {
                let col = if (bits & (0x80 >> cx)) != 0 { fg } else { bg };
                // SAFETY: `gx + 8 <= w` and `gy + 16 <= h` checked above.
                unsafe {
                    *self.back_ptr.add(off + cx) = col;
                }
            }
        }
    }

    fn paint_desktop_region(&mut self, region: Rect) {
        let c = region.clamp_to(self.w, self.h);
        if c.is_empty() {
            return;
        }
        let y_start = c.y.max(TOPBAR_H);
        let y_end = c.y + c.h;
        if y_start < y_end {
            self.fill_back(Rect::new(c.x, y_start, c.w, y_end - y_start), self.pal.bg_canvas);
            // Architectural 64px studio grid ticks.
            let first_gy = y_start.div_ceil(64) * 64;
            let mut gy = first_gy;
            while gy < y_end {
                let first_gx = c.x.div_ceil(64) * 64;
                let mut gx = first_gx;
                while gx < c.x + c.w {
                    if gx + 3 <= self.w && gy + 3 <= self.h {
                        // SAFETY: Bounds checked against `(self.w, self.h)`.
                        unsafe {
                            *self.back_ptr.add(gy * self.w + gx) = self.pal.bg_grid;
                            *self.back_ptr.add(gy * self.w + gx + 1) = self.pal.bg_grid;
                            *self.back_ptr.add((gy + 1) * self.w + gx) = self.pal.bg_grid;
                        }
                    }
                    gx += 64;
                }
                gy += 64;
            }
        }

        if c.y < TOPBAR_H {
            self.fill_back(Rect::new(0, 0, self.w, TOPBAR_H - 1), self.pal.bg_topbar);
            self.fill_back(Rect::new(0, TOPBAR_H - 1, self.w, 1), self.pal.border_idle);
            self.draw_str_back(
                16,
                6,
                b"DRY OS // P1 INTERACTIVE DESKTOP",
                self.pal.fg_primary,
                self.pal.bg_topbar,
            );
            if self.w >= 900 {
                let right = b"CPL=3 COMPOSITOR | IRQ1+IPC | ALT+ARROWS: MOVE";
                let rx = self.w.saturating_sub(right.len() * 8 + 16);
                self.draw_str_back(rx, 6, right, self.pal.fg_accent, self.pal.bg_topbar);
            }
        }
    }

    #[inline]
    fn window_outer_rect(&self) -> Rect {
        Rect::new(self.win_x, self.win_y, WIN_W + SHADOW_OFF, WIN_H + SHADOW_OFF)
    }

    fn render_window_chrome(&mut self, cmd_count: u32, ext_irqs: u32) {
        let wx = self.win_x;
        let wy = self.win_y;
        self.fill_back(Rect::new(wx + SHADOW_OFF, wy + SHADOW_OFF, WIN_W, WIN_H), self.pal.bg_shadow);
        let bcol = if self.focused {
            self.pal.border_focus
        } else {
            self.pal.border_idle
        };
        self.fill_back(Rect::new(wx, wy, WIN_W, WIN_H), bcol);
        self.fill_back(
            Rect::new(wx + WIN_BORDER, wy + WIN_BORDER, WIN_W - WIN_BORDER * 2, WIN_TITLE_H),
            self.pal.bg_title,
        );
        self.fill_back(
            Rect::new(
                wx + WIN_BORDER,
                wy + WIN_BORDER + WIN_TITLE_H,
                WIN_W - WIN_BORDER * 2,
                WIN_H - WIN_BORDER * 2 - WIN_TITLE_H,
            ),
            self.pal.bg_win,
        );
        self.render_window_title_text(cmd_count, ext_irqs);
    }

    fn render_window_title_text(&mut self, cmd_count: u32, ext_irqs: u32) {
        let tx = self.win_x + WIN_BORDER + 10;
        let ty = self.win_y + WIN_BORDER + 6;
        self.fill_back(
            Rect::new(
                self.win_x + WIN_BORDER,
                self.win_y + WIN_BORDER,
                WIN_W - WIN_BORDER * 2,
                WIN_TITLE_H,
            ),
            self.pal.bg_title,
        );
        self.draw_str_back(
            tx,
            ty,
            b"TERMINAL // dry-shell [CPL=3 0x0102]",
            self.pal.fg_primary,
            self.pal.bg_title,
        );

        let mut badge = [b' '; 24];
        let mut p = 0usize;
        for &b in b"CMD:" {
            badge[p] = b;
            p += 1;
        }
        p += write_u32_ascii(&mut badge[p..], cmd_count);
        for &b in b" IRQ:" {
            badge[p] = b;
            p += 1;
        }
        p += write_u32_ascii(&mut badge[p..], ext_irqs);
        let bx = (self.win_x + WIN_W).saturating_sub(WIN_BORDER + p * 8 + 10);
        self.draw_str_back(bx, ty, &badge[..p], self.pal.fg_ok, self.pal.bg_title);
    }

    fn render_surface_rows(&mut self, row_min: usize, row_max: usize) {
        if !self.surface_attached {
            return;
        }
        let surf_ptr = SURFACE_MAP_VADDR as *const SharedTerminalSurface;
        // SAFETY: `SURFACE_MAP_VADDR` is mapped `USER_RO` from `dry-shell`'s `Frame4K`.
        let magic = unsafe { read_volatile(&raw const (*surf_ptr).magic) };
        if magic != SURFACE_MAGIC {
            return;
        }
        // SAFETY: Reading POD header fields from `USER_RO` shared surface.
        let (cur_col, cur_row, cur_vis, cmds, irqs) = unsafe {
            (
                read_volatile(&raw const (*surf_ptr).cursor_col) as usize,
                read_volatile(&raw const (*surf_ptr).cursor_row) as usize,
                read_volatile(&raw const (*surf_ptr).cursor_visible) != 0,
                read_volatile(&raw const (*surf_ptr).cmd_count),
                read_volatile(&raw const (*surf_ptr).ext_irqs_snap),
            )
        };

        if cmds != self.last_cmd_count || irqs != self.last_ext_irqs {
            self.last_cmd_count = cmds;
            self.last_ext_irqs = irqs;
            self.render_window_title_text(cmds, irqs);
            self.dirty.mark(Rect::new(
                self.win_x + WIN_BORDER,
                self.win_y + WIN_BORDER,
                WIN_W - WIN_BORDER * 2,
                WIN_TITLE_H,
            ));
        }

        let r_lo = row_min.min(self.last_cursor_row).min(TERM_ROWS - 1);
        let r_hi = row_max.max(cur_row).max(self.last_cursor_row).min(TERM_ROWS - 1);
        self.last_cursor_row = cur_row.min(TERM_ROWS - 1);

        let grid_x = self.win_x + WIN_BORDER + WIN_PAD_X;
        let grid_y = self.win_y + WIN_BORDER + WIN_TITLE_H + 4;

        for r in r_lo..=r_hi {
            let py = grid_y + r * 16;
            for c in 0..TERM_COLS {
                // SAFETY: `r * TERM_COLS + c < 1920` inside `SharedTerminalSurface.cells`.
                let cell: SurfaceCell =
                    unsafe { read_volatile(&raw const (*surf_ptr).cells[r * TERM_COLS + c]) };
                let ch = if (0x20..=0x7E).contains(&cell.ch) {
                    cell.ch
                } else {
                    b' '
                };
                let is_cur = cur_vis && self.focused && r == cur_row && c == cur_col;
                let (fg, bg) = if is_cur {
                    (self.pal.bg_win, self.pal.fg_accent)
                } else {
                    (self.pal.style_fg(cell.style), self.pal.bg_win)
                };
                self.draw_glyph_cell_back(grid_x + c * 8, py, ch, fg, bg);
            }
        }

        self.dirty.mark(Rect::new(
            grid_x,
            grid_y + r_lo * 16,
            TERM_COLS * 8,
            (r_hi - r_lo + 1) * 16,
        ));
    }

    /// Blit only the accumulated `DirtyRegion` from cached WB RAM to GOP MMIO (`u64` pixel pairs).
    fn blit_dirty_to_gop(&mut self) -> (u64, u64) {
        let Some(r) = self.dirty.take(self.w, self.h) else {
            return (0, 0);
        };
        let t0 = rdtsc_user();
        for y in r.y..(r.y + r.h) {
            let mut x = r.x;
            let x_end = r.x + r.w;
            // SAFETY: Both `back_ptr` and `fb_ptr` cover `(0..w, 0..h)` with valid alignment.
            unsafe {
                let mut src = self.back_ptr.add(y * self.w + x);
                let mut dst = self.fb_ptr.add(y * self.fb_stride + x);
                if (x & 1) != 0 && x < x_end {
                    write_volatile(dst, *src);
                    src = src.add(1);
                    dst = dst.add(1);
                    x += 1;
                }
                while x + 2 <= x_end {
                    write_volatile(dst.cast::<u64>(), *(src.cast::<u64>()));
                    src = src.add(2);
                    dst = dst.add(2);
                    x += 2;
                }
                if x < x_end {
                    write_volatile(dst, *src);
                }
            }
        }
        flush_video_stores();
        let dt = rdtsc_user().saturating_sub(t0);
        ((r.w * r.h) as u64, dt)
    }
}

fn write_u32_ascii(out: &mut [u8], mut v: u32) -> usize {
    if v == 0 {
        if !out.is_empty() {
            out[0] = b'0';
            return 1;
        }
        return 0;
    }
    let mut tmp = [0u8; 10];
    let mut n = 0usize;
    while v > 0 {
        tmp[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
    }
    let take = n.min(out.len());
    for i in 0..take {
        out[i] = tmp[n - 1 - i];
    }
    take
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

    let mut ep_info = UserCapInfo::default();
    if sys_cap_query(disp_ep_cap, &mut ep_info) < 0 {
        sys_exit(113);
    }
    if (ep_info.rights_bits & (RIGHT_RECV as u32)) == 0 {
        run_legacy_banner_mode(disp_ep_cap, &info);
    }

    // P1 Interactive Compositor Mode: allocate 2 MiB WB RAM backbuffer frames.
    let needed_bytes = w.saturating_mul(h).saturating_mul(4);
    let num_2m = needed_bytes.div_ceil(TWO_MIB).min(4);
    let mut back_caps = [0u64; 4];
    for (i, slot) in back_caps.iter_mut().take(num_2m).enumerate() {
        let cap = match sys_frame_alloc(1) {
            Ok(c) => c,
            Err(_) => sys_exit(114),
        };
        let va = BACKBUF_VADDR + (i * TWO_MIB) as u64;
        if sys_vspace_map(cap, va, VSPACE_MAP_OP_2M_RW).is_err() {
            sys_exit(115);
        }
        *slot = cap;
    }

    let init_wx = w.saturating_sub(WIN_W + SHADOW_OFF) / 2;
    let init_wy = TOPBAR_H + (h.saturating_sub(TOPBAR_H + WIN_H + SHADOW_OFF)) / 2;

    let mut comp = Compositor {
        fb_ptr: FB_MAP_VADDR as *mut u32,
        back_ptr: BACKBUF_VADDR as *mut u32,
        w,
        h,
        fb_stride: stride_px,
        pal: Palette::from_fb(&info),
        win_x: init_wx.max(8),
        win_y: init_wy.max(TOPBAR_H + 8),
        focused: true,
        last_cursor_row: 0,
        last_cmd_count: 0,
        last_ext_irqs: 0,
        surface_attached: false,
        surface_cap: 0,
        dirty: DirtyRegion::new(),
    };

    comp.paint_desktop_region(Rect::new(0, 0, w, h));
    comp.render_window_chrome(0, 0);
    comp.dirty.mark(Rect::new(0, 0, w, h));
    let _ = comp.blit_dirty_to_gop();
    let _ = sys_write_debug(b"[DRY-DISPLAY] P1 Ring 3 desktop compositor active\n");

    loop {
        let mut ext = UserIpcRecvExt::default();
        let (tag, reply_tok, mrs) = match sys_ipc_recv_ext(disp_ep_cap, 0, 0, &mut ext) {
            Ok(v) => v,
            Err(_) => break,
        };
        let op = msg_tag_label(tag);
        match op {
            IPC_OP_SURFACE_ATTACH => {
                if ext.transferred_cap != 0 {
                    if comp.surface_attached {
                        let _ = sys_vspace_map(comp.surface_cap, SURFACE_MAP_VADDR, VSPACE_MAP_OP_UNMAP_4K);
                        let _ = sys_cap_revoke(comp.surface_cap, 1);
                    }
                    if sys_vspace_map(ext.transferred_cap, SURFACE_MAP_VADDR, VSPACE_MAP_OP_4K_RO).is_ok() {
                        comp.surface_cap = ext.transferred_cap;
                        comp.surface_attached = true;
                        comp.render_window_chrome(0, 0);
                        comp.render_surface_rows(0, TERM_ROWS - 1);
                        comp.dirty.mark(comp.window_outer_rect());
                    }
                }
                let (_, cyc) = comp.blit_dirty_to_gop();
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [w as u64, h as u64, comp.win_x as u64, cyc],
                    );
                }
            }
            IPC_OP_SURFACE_PRESENT => {
                if comp.surface_attached {
                    let surf_ptr = SURFACE_MAP_VADDR as *const SharedTerminalSurface;
                    // SAFETY: `SURFACE_MAP_VADDR` is mapped `USER_RO`.
                    let (r_min, r_max) = unsafe {
                        (
                            read_volatile(&raw const (*surf_ptr).dirty_row_min) as usize,
                            read_volatile(&raw const (*surf_ptr).dirty_row_max) as usize,
                        )
                    };
                    comp.render_surface_rows(r_min, r_max);
                }
                let (px, cyc) = comp.blit_dirty_to_gop();
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [px, cyc, 0, 0],
                    );
                }
            }
            IPC_OP_SURFACE_MOVE => {
                let dx = mrs[0] as i64;
                let dy = mrs[1] as i64;
                let max_x = w.saturating_sub(WIN_W + SHADOW_OFF + 8).max(8) as i64;
                let max_y = h.saturating_sub(WIN_H + SHADOW_OFF + 8).max(TOPBAR_H + 8) as i64;
                let old_rect = comp.window_outer_rect();
                comp.win_x = ((comp.win_x as i64) + dx).clamp(8, max_x) as usize;
                comp.win_y = ((comp.win_y as i64) + dy).clamp((TOPBAR_H + 8) as i64, max_y) as usize;
                let new_rect = comp.window_outer_rect();

                comp.paint_desktop_region(old_rect);
                comp.render_window_chrome(comp.last_cmd_count, comp.last_ext_irqs);
                comp.render_surface_rows(0, TERM_ROWS - 1);
                comp.dirty.mark(old_rect.union(new_rect));
                let (_, cyc) = comp.blit_dirty_to_gop();
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                        [comp.win_x as u64, comp.win_y as u64, cyc, 0],
                    );
                }
            }
            IPC_OP_SURFACE_FOCUS => {
                comp.focused = mrs[0] != 0;
                comp.render_window_chrome(comp.last_cmd_count, comp.last_ext_irqs);
                comp.render_surface_rows(comp.last_cursor_row, comp.last_cursor_row);
                comp.dirty.mark(comp.window_outer_rect());
                let (px, cyc) = comp.blit_dirty_to_gop();
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [px, cyc, 0, 0],
                    );
                }
            }
            IPC_OP_EXIT_CLEAN => {
                if comp.surface_attached {
                    let _ = sys_vspace_map(comp.surface_cap, SURFACE_MAP_VADDR, VSPACE_MAP_OP_UNMAP_4K);
                    let _ = sys_cap_revoke(comp.surface_cap, 1);
                }
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
                }
                sys_exit(0);
            }
            _ => {
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        disp_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [0; 4],
                    );
                }
            }
        }
    }
    sys_exit(0);
}
