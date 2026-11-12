// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Ring 3 Multi-Window Scene Rasterizer, Clipped Z-Order Compositor & Software Cursor Blitter
//! (`Milestone 2A-2`).
//!
//! Design Invariants:
//! 1. **Cursor-Free Scene Backbuffer (`back_ptr`):** `back_ptr` (`WB` RAM) holds only the desktop,
//!    top bar, and Z-ordered windows. The 12x16 software pointer is overlaid exclusively while
//!    flushing damaged scanlines into `fb_ptr` (`WC` GOP MMIO), making cursor trails impossible
//!    and avoiding any glyph re-rasterization on cursor motion (`<= 384` px touched per move!).
//! 2. **Clipped Bottom-to-Top Z-Order Painting:** `repaint_scene_rect` clips every background fill,
//!    window shadow, border, titlebar, and cell grid strictly to `clip`, then paints `wm.z_stack`
//!    from bottom to top so background window updates never bleed through overlapping foreground windows.

use core::arch::asm;
use core::ptr::write_volatile;

use crate::gfx::{
    glyph_8x16, DirtyRegion, Rect, BOX_HORIZ, CURSOR_H, CURSOR_SPRITE_12X16, CURSOR_W,
    MAX_DIRTY_RECTS,
};
use crate::surface::{
    SurfaceCell, STYLE_ACCENT, STYLE_HEADER, STYLE_MUTED, STYLE_OK, STYLE_PROMPT, STYLE_WARN,
    TERM_CELLS,
};
use crate::wm::{
    LayoutMode, WindowManager, WindowSlot, SHADOW_OFF, TITLE_KIND_SYSMON, TITLE_KIND_TERMINAL,
    TOPBAR_H, WIN_BORDER, WIN_TITLE_H,
};
use crate::{rdtsc_user, UserFbInfo};

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg_canvas: u32,
    pub bg_grid: u32,
    pub bg_topbar: u32,
    pub bg_pill: u32,
    pub bg_shadow: u32,
    pub bg_win: u32,
    pub bg_title_focus: u32,
    pub bg_title_idle: u32,
    pub bg_close: u32,
    pub border_focus: u32,
    pub border_idle: u32,
    pub fg_primary: u32,
    pub fg_muted: u32,
    pub fg_accent: u32,
    pub fg_ok: u32,
    pub fg_warn: u32,
    pub fg_prompt: u32,
    pub cur_outline: u32,
    pub cur_fill: u32,
}

impl Palette {
    pub fn from_fb(info: &UserFbInfo) -> Self {
        Self {
            bg_canvas: pack_rgb(info, 0x0B, 0x10, 0x1B),
            bg_grid: pack_rgb(info, 0x18, 0x24, 0x38),
            bg_topbar: pack_rgb(info, 0x0E, 0x16, 0x28),
            bg_pill: pack_rgb(info, 0x1B, 0x2A, 0x44),
            bg_shadow: pack_rgb(info, 0x05, 0x08, 0x0E),
            bg_win: pack_rgb(info, 0x0F, 0x16, 0x24),
            bg_title_focus: pack_rgb(info, 0x17, 0x25, 0x3D),
            bg_title_idle: pack_rgb(info, 0x12, 0x1B, 0x2C),
            bg_close: pack_rgb(info, 0x3B, 0x1D, 0x2A),
            border_focus: pack_rgb(info, 0x22, 0xD3, 0xEE),
            border_idle: pack_rgb(info, 0x33, 0x41, 0x55),
            fg_primary: pack_rgb(info, 0xF8, 0xFA, 0xFC),
            fg_muted: pack_rgb(info, 0x94, 0xA3, 0xB8),
            fg_accent: pack_rgb(info, 0x22, 0xD3, 0xEE),
            fg_ok: pack_rgb(info, 0x34, 0xD3, 0x99),
            fg_warn: pack_rgb(info, 0xFB, 0xBF, 0x24),
            fg_prompt: pack_rgb(info, 0x38, 0xBD, 0xF8),
            cur_outline: pack_rgb(info, 0x06, 0x09, 0x0F),
            cur_fill: pack_rgb(info, 0xF8, 0xFA, 0xFC),
        }
    }

    #[inline]
    pub fn style_fg(&self, style: u8) -> u32 {
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
pub fn pack_rgb(info: &UserFbInfo, r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << (info.red_shift & 31))
        | ((g as u32) << (info.green_shift & 31))
        | ((b as u32) << (info.blue_shift & 31))
}

#[inline]
pub fn flush_video_stores() {
    // SAFETY: Architectural store fence compatible with `-C target-feature=-sse`.
    unsafe {
        asm!("lock add dword ptr [rsp - 8], 0", options(nostack));
    }
}

pub fn write_u32_ascii(out: &mut [u8], mut v: u32) -> usize {
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

pub struct DesktopCompositor {
    pub fb_ptr: *mut u32,
    pub back_ptr: *mut u32,
    pub w: usize,
    pub h: usize,
    pub fb_stride: usize,
    pub pal: Palette,
    pub scene_dirty: DirtyRegion,
    pub mmio_dirty: DirtyRegion,
    pub scratch_cells: [SurfaceCell; TERM_CELLS],
}

impl DesktopCompositor {
    pub const fn uninit() -> Self {
        Self {
            fb_ptr: core::ptr::null_mut(),
            back_ptr: core::ptr::null_mut(),
            w: 0,
            h: 0,
            fb_stride: 0,
            pal: Palette {
                bg_canvas: 0,
                bg_grid: 0,
                bg_topbar: 0,
                bg_pill: 0,
                bg_shadow: 0,
                bg_win: 0,
                bg_title_focus: 0,
                bg_title_idle: 0,
                bg_close: 0,
                border_focus: 0,
                border_idle: 0,
                fg_primary: 0,
                fg_muted: 0,
                fg_accent: 0,
                fg_ok: 0,
                fg_warn: 0,
                fg_prompt: 0,
                cur_outline: 0,
                cur_fill: 0,
            },
            scene_dirty: DirtyRegion::new(),
            mmio_dirty: DirtyRegion::new(),
            scratch_cells: [SurfaceCell::BLANK; TERM_CELLS],
        }
    }

    pub fn init(&mut self, fb_ptr: *mut u32, back_ptr: *mut u32, info: &UserFbInfo) {
        self.fb_ptr = fb_ptr;
        self.back_ptr = back_ptr;
        self.w = info.width as usize;
        self.h = info.height as usize;
        self.fb_stride = (info.pitch as usize) / 4;
        self.pal = Palette::from_fb(info);
        self.scene_dirty = DirtyRegion::new();
        self.mmio_dirty = DirtyRegion::new();
    }

    #[inline]
    fn fill_back_clipped(&mut self, r: Rect, clip: Rect, color: u32) {
        let c = r.intersect(clip).clamp_to(self.w, self.h);
        if c.is_empty() {
            return;
        }
        for y in c.y..(c.y + c.h) {
            let row_base = y * self.w + c.x;
            // SAFETY: `back_ptr` covers `w * h` `u32` pixels in mapped WB RAM.
            let slice = unsafe { core::slice::from_raw_parts_mut(self.back_ptr.add(row_base), c.w) };
            slice.fill(color);
        }
    }

    #[inline]
    fn draw_glyph_clipped(&mut self, gx: usize, gy: usize, ch: u8, fg: u32, bg: u32, clip: Rect) {
        let cell_r = Rect::new(gx, gy, 8, 16);
        let inter = cell_r.intersect(clip).clamp_to(self.w, self.h);
        if inter.is_empty() {
            return;
        }
        let g = glyph_8x16(ch);
        // Fast path when entire 8x16 cell is inside `clip`.
        if inter.w == 8 && inter.h == 16 {
            for (ry, &bits) in g.iter().enumerate() {
                let off = (gy + ry) * self.w + gx;
                for cx in 0..8usize {
                    let col = if (bits & (0x80 >> cx)) != 0 { fg } else { bg };
                    // SAFETY: `inter` clamped to `(self.w, self.h)`.
                    unsafe {
                        *self.back_ptr.add(off + cx) = col;
                    }
                }
            }
            return;
        }
        for py in inter.y..(inter.y + inter.h) {
            let bits = g[py - gy];
            let off = py * self.w;
            for px in inter.x..(inter.x + inter.w) {
                let cx = px - gx;
                let col = if (bits & (0x80 >> cx)) != 0 { fg } else { bg };
                // SAFETY: `(px, py)` clamped to `(self.w, self.h)`.
                unsafe {
                    *self.back_ptr.add(off + px) = col;
                }
            }
        }
    }

    fn draw_str_clipped(&mut self, x: usize, y: usize, text: &[u8], fg: u32, bg: u32, clip: Rect) {
        for (i, &ch) in text.iter().enumerate() {
            self.draw_glyph_clipped(x + i * 8, y, ch, fg, bg, clip);
        }
    }

    fn paint_background_and_topbar(&mut self, wm: &WindowManager, clip: Rect) {
        let c = clip.clamp_to(self.w, self.h);
        if c.is_empty() {
            return;
        }
        let y_start = c.y.max(TOPBAR_H);
        let y_end = c.y + c.h;
        if y_start < y_end {
            let bg_clip = Rect::new(c.x, y_start, c.w, y_end - y_start);
            self.fill_back_clipped(bg_clip, bg_clip, self.pal.bg_canvas);
            let first_gy = y_start.div_ceil(64) * 64;
            let mut gy = first_gy;
            while gy < y_end {
                let first_gx = c.x.div_ceil(64) * 64;
                let mut gx = first_gx;
                while gx < c.x + c.w {
                    for &(dx, dy) in &[(0usize, 0usize), (1, 0), (0, 1)] {
                        let px = gx + dx;
                        let py = gy + dy;
                        if bg_clip.contains(px, py) && px < self.w && py < self.h {
                            // SAFETY: `(px, py)` verified `< (self.w, self.h)`.
                            unsafe {
                                *self.back_ptr.add(py * self.w + px) = self.pal.bg_grid;
                            }
                        }
                    }
                    gx += 64;
                }
                gy += 64;
            }
        }

        if c.y < TOPBAR_H {
            let tb_clip = c.intersect(Rect::new(0, 0, self.w, TOPBAR_H));
            self.fill_back_clipped(Rect::new(0, 0, self.w, TOPBAR_H - 1), tb_clip, self.pal.bg_topbar);
            self.fill_back_clipped(Rect::new(0, TOPBAR_H - 1, self.w, 1), tb_clip, self.pal.border_idle);

            let brand: &[u8] = if wm.desktop_mode {
                b"DRY OS // DESKTOP PREVIEW 2A-2"
            } else {
                b"DRY OS // P1 INTERACTIVE DESKTOP"
            };
            self.draw_str_clipped(14, 6, brand, self.pal.fg_primary, self.pal.bg_topbar, tb_clip);

            // Center layout pill (`[MODE: FLOAT]` / `[MODE: TILED]`) + active window indicator.
            let pill = wm.topbar_mode_pill_rect();
            if wm.desktop_mode && self.w >= 960 {
                self.fill_back_clipped(pill, tb_clip, self.pal.bg_pill);
                let mode_lbl: &[u8] = match wm.layout {
                    LayoutMode::Floating => b"MODE: FLOAT",
                    LayoutMode::SideBySideTiled => b"MODE: TILED",
                };
                self.draw_str_clipped(
                    pill.x + 12,
                    6,
                    mode_lbl,
                    self.pal.fg_accent,
                    self.pal.bg_pill,
                    tb_clip,
                );
            }

            if self.w >= 960 {
                let mut status = [b' '; 44];
                let mut sp = 0usize;
                let active_lbl: &[u8] = match wm.focused_slot().map(|s| wm.windows[s].title_kind) {
                    Some(TITLE_KIND_SYSMON) => b"FOCUS:SYSMON  WINS:",
                    Some(TITLE_KIND_TERMINAL) => b"FOCUS:TERMINAL WINS:",
                    Some(_) => b"FOCUS:APP     WINS:",
                    None => b"FOCUS:NONE    WINS:",
                };
                for &b in active_lbl {
                    status[sp] = b;
                    sp += 1;
                }
                sp += write_u32_ascii(&mut status[sp..], wm.active_window_count() as u32);
                let suffix: &[u8] = if wm.desktop_mode {
                    b" | ALT+TAB / ALT+T"
                } else {
                    b" | ALT+ARROWS: MOVE"
                };
                for &b in suffix {
                    if sp < status.len() {
                        status[sp] = b;
                        sp += 1;
                    }
                }
                let rx = self.w.saturating_sub(sp * 8 + 14);
                self.draw_str_clipped(rx, 6, &status[..sp], self.pal.fg_ok, self.pal.bg_topbar, tb_clip);
            }
        }
    }

    fn paint_window_clipped(&mut self, win: &WindowSlot, clip: Rect) {
        if !win.allocated || !win.visible {
            return;
        }
        let outer = win.outer_rect();
        let wclip = outer.intersect(clip).clamp_to(self.w, self.h);
        if wclip.is_empty() {
            return;
        }

        let wx = win.rect.x;
        let wy = win.rect.y;
        let ww = win.rect.w;
        let wh = win.rect.h;

        // 1. Drop shadow (only outside the window body itself so we don't double-fill body pixels).
        self.fill_back_clipped(
            Rect::new(wx + ww, wy + SHADOW_OFF, SHADOW_OFF, wh),
            wclip,
            self.pal.bg_shadow,
        );
        self.fill_back_clipped(
            Rect::new(wx + SHADOW_OFF, wy + wh, ww.saturating_sub(SHADOW_OFF), SHADOW_OFF),
            wclip,
            self.pal.bg_shadow,
        );

        // 2. Crisp 2px focus/unfocused border.
        let bcol = if win.focused {
            self.pal.border_focus
        } else {
            self.pal.border_idle
        };
        self.fill_back_clipped(Rect::new(wx, wy, ww, WIN_BORDER), wclip, bcol);
        self.fill_back_clipped(Rect::new(wx, wy + wh - WIN_BORDER, ww, WIN_BORDER), wclip, bcol);
        self.fill_back_clipped(Rect::new(wx, wy, WIN_BORDER, wh), wclip, bcol);
        self.fill_back_clipped(Rect::new(wx + ww - WIN_BORDER, wy, WIN_BORDER, wh), wclip, bcol);

        // 3. Titlebar chrome + close `[x]` button.
        let tb_bg = if win.focused {
            self.pal.bg_title_focus
        } else {
            self.pal.bg_title_idle
        };
        let tb_rect = win.titlebar_rect();
        if !tb_rect.intersect(wclip).is_empty() {
            self.fill_back_clipped(tb_rect, wclip, tb_bg);
            let title_fg = if win.focused {
                self.pal.fg_primary
            } else {
                self.pal.fg_muted
            };
            let title_str: &[u8] = match win.title_kind {
                TITLE_KIND_SYSMON => b"SYSTEM MONITOR // dry-sysmon [0x0103]",
                TITLE_KIND_TERMINAL => b"DRY TERMINAL // dry-shell [0x0102]",
                _ => b"DRY APP // [CPL=3]",
            };
            self.draw_str_clipped(
                wx + WIN_BORDER + 10,
                wy + WIN_BORDER + 6,
                title_str,
                title_fg,
                tb_bg,
                wclip,
            );

            let close_r = win.close_btn_rect();
            self.fill_back_clipped(close_r, wclip, self.pal.bg_close);
            self.draw_str_clipped(
                close_r.x + 7,
                close_r.y + 2,
                b"x",
                self.pal.fg_warn,
                self.pal.bg_close,
                wclip,
            );

            // Live compact telemetry badge to the left of `[x]`.
            let mut badge = [b' '; 20];
            let mut p = 0usize;
            if win.title_kind == TITLE_KIND_TERMINAL {
                for &b in b"CMD:" {
                    badge[p] = b;
                    p += 1;
                }
                p += write_u32_ascii(&mut badge[p..], win.snapshot.cmd_count);
            } else {
                for &b in b"UP:" {
                    badge[p] = b;
                    p += 1;
                }
                p += write_u32_ascii(&mut badge[p..], win.snapshot.uptime_sec_snap);
                if p < badge.len() {
                    badge[p] = b's';
                    p += 1;
                }
            }
            let bx = close_r.x.saturating_sub(p * 8 + 10);
            if bx > wx + WIN_BORDER + title_str.len() * 8 + 16 {
                self.draw_str_clipped(bx, wy + WIN_BORDER + 6, &badge[..p], self.pal.fg_ok, tb_bg, wclip);
            }
        }

        // 4. Client surface body & cached character grid.
        let body_rect = Rect::new(
            wx + WIN_BORDER,
            wy + WIN_BORDER + WIN_TITLE_H,
            ww - WIN_BORDER * 2,
            wh - WIN_BORDER * 2 - WIN_TITLE_H,
        );
        let body_clip = body_rect.intersect(wclip);
        if body_clip.is_empty() {
            return;
        }

        let (grid_x, grid_y) = win.grid_origin();
        let cols = win.cols as usize;
        let rows = win.rows as usize;
        let grid_rect = Rect::new(grid_x, grid_y, cols * 8, rows * 16);

        // Fill padding strips around `grid_rect` (or entire body if snapshot not yet populated).
        if !win.snapshot.valid {
            self.fill_back_clipped(body_rect, body_clip, self.pal.bg_win);
            return;
        }
        self.fill_back_clipped(
            Rect::new(body_rect.x, body_rect.y, body_rect.w, grid_y - body_rect.y),
            body_clip,
            self.pal.bg_win,
        );
        let grid_bottom = grid_y + rows * 16;
        let body_bottom = body_rect.y + body_rect.h;
        if body_bottom > grid_bottom {
            self.fill_back_clipped(
                Rect::new(body_rect.x, grid_bottom, body_rect.w, body_bottom - grid_bottom),
                body_clip,
                self.pal.bg_win,
            );
        }
        self.fill_back_clipped(
            Rect::new(body_rect.x, grid_y, grid_x - body_rect.x, rows * 16),
            body_clip,
            self.pal.bg_win,
        );
        let grid_right = grid_x + cols * 8;
        let body_right = body_rect.x + body_rect.w;
        if body_right > grid_right {
            self.fill_back_clipped(
                Rect::new(grid_right, grid_y, body_right - grid_right, rows * 16),
                body_clip,
                self.pal.bg_win,
            );
        }

        let gclip = grid_rect.intersect(body_clip);
        if gclip.is_empty() {
            return;
        }
        let r_start = (gclip.y - grid_y) / 16;
        let r_end = ((gclip.y + gclip.h - 1 - grid_y) / 16).min(rows - 1);
        let c_start = (gclip.x - grid_x) / 8;
        let c_end = ((gclip.x + gclip.w - 1 - grid_x) / 8).min(cols - 1);

        let cur_vis = win.snapshot.cursor_visible && win.focused;
        let cur_r = win.snapshot.cursor_row as usize;
        let cur_c = win.snapshot.cursor_col as usize;

        for r in r_start..=r_end {
            let py = grid_y + r * 16;
            let row_base = r * cols;
            for c in c_start..=c_end {
                let cell = win.snapshot.cells[row_base + c];
                let ch = if (0x20..=0x7E).contains(&cell.ch) || cell.ch == BOX_HORIZ {
                    cell.ch
                } else {
                    b' '
                };
                let is_cur = cur_vis && r == cur_r && c == cur_c;
                let (fg, bg) = if is_cur {
                    (self.pal.bg_win, self.pal.fg_accent)
                } else {
                    (self.pal.style_fg(cell.style), self.pal.bg_win)
                };
                self.draw_glyph_clipped(grid_x + c * 8, py, ch, fg, bg, gclip);
            }
        }
    }

    /// Repaint `clip` in `back_ptr` from bottom-to-top Z-order and queue `clip` for MMIO flush.
    pub fn repaint_scene_rect(&mut self, wm: &WindowManager, clip: Rect) {
        let c = clip.clamp_to(self.w, self.h);
        if c.is_empty() {
            return;
        }
        self.paint_background_and_topbar(wm, c);
        for z in 0..wm.z_len {
            let slot_idx = wm.z_stack[z];
            self.paint_window_clipped(&wm.windows[slot_idx], c);
        }
        self.mmio_dirty.mark(c);
    }

    /// Drain all pending `scene_dirty` + `mmio_dirty` rectangles and stream modified scanlines
    /// (compositing the 12x16 software cursor on-the-fly) into the GOP framebuffer.
    pub fn flush_all(&mut self, wm: &WindowManager) -> (u64, u64) {
        let mut s_rects = [Rect::EMPTY; MAX_DIRTY_RECTS];
        let sn = self.scene_dirty.drain_into(self.w, self.h, &mut s_rects);
        for r in s_rects.iter().take(sn) {
            self.repaint_scene_rect(wm, *r);
        }

        let mut m_rects = [Rect::EMPTY; MAX_DIRTY_RECTS];
        let mn = self.mmio_dirty.drain_into(self.w, self.h, &mut m_rects);
        if mn == 0 {
            return (0, 0);
        }

        let cur_r = wm.cursor_rect();
        let t0 = rdtsc_user();
        let mut total_px = 0u64;

        for r in m_rects.iter().take(mn) {
            total_px += (r.w * r.h) as u64;
            let has_cur = !cur_r.intersect(*r).is_empty();
            for y in r.y..(r.y + r.h) {
                let row_has_cur = has_cur && y >= cur_r.y && y < cur_r.y + CURSOR_H;
                if !row_has_cur {
                    let mut x = r.x;
                    let x_end = r.x + r.w;
                    // SAFETY: `r` is clamped to `(self.w, self.h)`.
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
                } else {
                    let sprite_row = &CURSOR_SPRITE_12X16[y - cur_r.y];
                    for x in r.x..(r.x + r.w) {
                        // SAFETY: `(x, y)` clamped to `(self.w, self.h)`.
                        let mut px = unsafe { *self.back_ptr.add(y * self.w + x) };
                        if x >= cur_r.x && x < cur_r.x + CURSOR_W {
                            match sprite_row[x - cur_r.x] {
                                1 => px = self.pal.cur_outline,
                                2 => px = self.pal.cur_fill,
                                3 => px = self.pal.fg_accent,
                                _ => {}
                            }
                        }
                        unsafe {
                            write_volatile(self.fb_ptr.add(y * self.fb_stride + x), px);
                        }
                    }
                }
            }
        }

        flush_video_stores();
        let dt = rdtsc_user().saturating_sub(t0);
        (total_px, dt)
    }
}
