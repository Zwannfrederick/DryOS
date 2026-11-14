// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Ring 3 Multi-Window Scene Rasterizer, Clipped Z-Order Compositor & Software Cursor Blitter
//! (`Milestone 2A-2` & `DRY Glass` `Ascension 3.0`).
//!
//! Design Invariants:
//! 1. **Cursor-Free Scene Backbuffer (`back_ptr`):** `back_ptr` (`WB` RAM) holds only the desktop,
//!    top bar, dock, windows, and overlays. The 12x16 software pointer is overlaid exclusively
//!    while flushing damaged scanlines into `fb_ptr` (`WC` GOP MMIO), making cursor trails impossible
//!    and avoiding any glyph re-rasterization on cursor motion (`<= 384` px touched per move!).
//! 2. **Clipped Bottom-to-Top Z-Order Painting:** `repaint_scene_rect` delegates layering to
//!    `glass::chrome` and `glass::overlays`, strictly clipped to `clip`, painting `wm.z_stack`
//!    from bottom to top so background updates never bleed through foreground windows.

use core::arch::asm;
use core::ptr::write_volatile;

use crate::gfx::{
    glyph_8x16, DirtyRegion, Rect, CURSOR_H, CURSOR_SPRITE_12X16, CURSOR_W, MAX_DIRTY_RECTS,
};
use crate::glass::theme::{GlassTokens, ThemeMode};
use crate::surface::{
    SurfaceCell, STYLE_ACCENT, STYLE_HEADER, STYLE_MUTED, STYLE_OK, STYLE_PROMPT, STYLE_WARN,
    TERM_CELLS,
};
use crate::wm::{SessionVisualState, WindowManager};
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
        Self::from_fb_prefs(info, false, false)
    }

    pub fn from_fb_prefs(info: &UserFbInfo, high_contrast: bool, light_mode: bool) -> Self {
        if high_contrast {
            return Self {
                bg_canvas: pack_rgb(info, 0x00, 0x00, 0x00),
                bg_grid: pack_rgb(info, 0x24, 0x24, 0x24),
                bg_topbar: pack_rgb(info, 0x00, 0x00, 0x00),
                bg_pill: pack_rgb(info, 0x00, 0x2B, 0x36),
                bg_shadow: pack_rgb(info, 0x00, 0x00, 0x00),
                bg_win: pack_rgb(info, 0x04, 0x06, 0x0A),
                bg_title_focus: pack_rgb(info, 0x00, 0x3B, 0x4D),
                bg_title_idle: pack_rgb(info, 0x14, 0x14, 0x14),
                bg_close: pack_rgb(info, 0x6E, 0x00, 0x18),
                border_focus: pack_rgb(info, 0x00, 0xFF, 0xEE),
                border_idle: pack_rgb(info, 0x94, 0xA3, 0xB8),
                fg_primary: pack_rgb(info, 0xFF, 0xFF, 0xFF),
                fg_muted: pack_rgb(info, 0xD8, 0xE2, 0xEC),
                fg_accent: pack_rgb(info, 0x00, 0xFF, 0xEE),
                fg_ok: pack_rgb(info, 0x39, 0xFF, 0x88),
                fg_warn: pack_rgb(info, 0xFF, 0xEA, 0x00),
                fg_prompt: pack_rgb(info, 0x5C, 0xE1, 0xFF),
                cur_outline: pack_rgb(info, 0x00, 0x00, 0x00),
                cur_fill: pack_rgb(info, 0xFF, 0xEA, 0x00),
            };
        }
        if light_mode {
            return Self {
                bg_canvas: pack_rgb(info, 0xDC, 0xE9, 0xF7),
                bg_grid: pack_rgb(info, 0xA4, 0xBC, 0xD6),
                bg_topbar: pack_rgb(info, 0xF6, 0xFA, 0xFE),
                bg_pill: pack_rgb(info, 0xE2, 0xEC, 0xF7),
                bg_shadow: pack_rgb(info, 0x1E, 0x29, 0x3B),
                bg_win: pack_rgb(info, 0xF4, 0xF8, 0xFD),
                bg_title_focus: pack_rgb(info, 0xE0, 0xEF, 0xFD),
                bg_title_idle: pack_rgb(info, 0xEC, 0xF1, 0xF7),
                bg_close: pack_rgb(info, 0xFE, 0xE2, 0xE2),
                border_focus: pack_rgb(info, 0x02, 0x84, 0xC7),
                border_idle: pack_rgb(info, 0x94, 0xA8, 0xC0),
                fg_primary: pack_rgb(info, 0x0F, 0x17, 0x2A),
                fg_muted: pack_rgb(info, 0x47, 0x55, 0x69),
                fg_accent: pack_rgb(info, 0x03, 0x69, 0xA1),
                fg_ok: pack_rgb(info, 0x04, 0x78, 0x57),
                fg_warn: pack_rgb(info, 0xB4, 0x53, 0x09),
                fg_prompt: pack_rgb(info, 0x03, 0x69, 0xA1),
                cur_outline: pack_rgb(info, 0x0F, 0x17, 0x2A),
                cur_fill: pack_rgb(info, 0xFF, 0xFF, 0xFF),
            };
        }
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
    pub wall_ptr: *mut u32,
    pub wall_key: u32,
    pub w: usize,
    pub h: usize,
    pub fb_stride: usize,
    pub fb_info: UserFbInfo,
    pub pal: Palette,
    pub glass: GlassTokens,
    pub scene_dirty: DirtyRegion,
    pub mmio_dirty: DirtyRegion,
    pub scratch_cells: [SurfaceCell; TERM_CELLS],
}

impl DesktopCompositor {
    pub const fn uninit() -> Self {
        Self {
            fb_ptr: core::ptr::null_mut(),
            back_ptr: core::ptr::null_mut(),
            wall_ptr: core::ptr::null_mut(),
            wall_key: u32::MAX,
            w: 0,
            h: 0,
            fb_stride: 0,
            fb_info: UserFbInfo {
                width: 0,
                height: 0,
                pitch: 0,
                bpp: 0,
                memory_model: 0,
                red_shift: 0,
                green_shift: 0,
                blue_shift: 0,
                red_size: 0,
                green_size: 0,
                blue_size: 0,
                _pad: [0; 3],
                mapped_bytes: 0,
            },
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
            glass: GlassTokens::zeroed(),
            scene_dirty: DirtyRegion::new(),
            mmio_dirty: DirtyRegion::new(),
            scratch_cells: [SurfaceCell::BLANK; TERM_CELLS],
        }
    }

    pub fn init(
        &mut self,
        fb_ptr: *mut u32,
        back_ptr: *mut u32,
        wall_ptr: *mut u32,
        info: &UserFbInfo,
    ) {
        self.fb_ptr = fb_ptr;
        self.back_ptr = back_ptr;
        self.wall_ptr = wall_ptr;
        self.wall_key = u32::MAX;
        self.w = info.width as usize;
        self.h = info.height as usize;
        self.fb_stride = (info.pitch as usize) / 4;
        self.fb_info = *info;
        self.pal = Palette::from_fb_prefs(info, false, false);
        self.glass = GlassTokens::from_fb(info, ThemeMode::ObsidianDark);
        self.scene_dirty = DirtyRegion::new();
        self.mmio_dirty = DirtyRegion::new();
    }

    pub fn apply_display_prefs(&mut self, wm: &WindowManager) {
        self.pal = Palette::from_fb_prefs(&self.fb_info, wm.high_contrast, wm.light_mode);
        self.glass = GlassTokens::from_fb(
            &self.fb_info,
            ThemeMode::from_wm(wm.high_contrast, wm.light_mode),
        );
        self.wall_key = u32::MAX;
        self.scene_dirty.mark(Rect::new(0, 0, self.w, self.h));
    }

    #[inline]
    pub fn fill_back_clipped(&mut self, r: Rect, clip: Rect, color: u32) {
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
    pub fn draw_glyph_clipped(
        &mut self,
        gx: usize,
        gy: usize,
        ch: u8,
        fg: u32,
        bg: u32,
        clip: Rect,
    ) {
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

    pub fn draw_str_clipped(
        &mut self,
        x: usize,
        y: usize,
        text: &[u8],
        fg: u32,
        bg: u32,
        clip: Rect,
    ) {
        for (i, &ch) in text.iter().enumerate() {
            self.draw_glyph_clipped(x + i * 8, y, ch, fg, bg, clip);
        }
    }

    /// Build or reuse the cached full-screen wallpaper in `wall_ptr`.
    pub fn ensure_wallpaper_cache(&mut self, p3_mode: bool) {
        if self.wall_ptr.is_null() || self.w == 0 || self.h == 0 {
            return;
        }
        let want_key = (u32::from(p3_mode) << 8) | (self.glass.mode as u32);
        if self.wall_key == want_key {
            return;
        }
        const BAYER_4X4: [i32; 16] = [-8, 0, -6, 2, 4, -4, 6, -2, -5, 3, -7, 1, 7, -1, 5, -3];
        let t = self.glass;
        let top = t.wall_top_rgb;
        let bot = t.wall_bot_rgb;
        let glow = t.wall_glow_rgb;
        let hm1 = self.h.saturating_sub(1).max(1) as i32;
        let cx1 = (self.w * 28 / 100) as i64;
        let cy1 = (self.h * 24 / 100) as i64;
        let rx1 = (self.w * 56 / 100).max(1) as i64;
        let ry1 = (self.h * 58 / 100).max(1) as i64;
        let cx2 = (self.w * 80 / 100) as i64;
        let cy2 = (self.h * 78 / 100) as i64;
        let rx2 = (self.w * 52 / 100).max(1) as i64;
        let ry2 = (self.h * 54 / 100).max(1) as i64;

        for py in 0..self.h {
            let y16 = ((py as i32) << 12) / hm1; // 0..4096
            let br16 = ((top.0 as i32) << 4) + (((bot.0 as i32 - top.0 as i32) * y16) >> 8);
            let bg16 = ((top.1 as i32) << 4) + (((bot.1 as i32 - top.1 as i32) * y16) >> 8);
            let bb16 = ((top.2 as i32) << 4) + (((bot.2 as i32 - top.2 as i32) * y16) >> 8);
            let dy1 = ((py as i64 - cy1) * 256) / ry1;
            let dy2 = ((py as i64 - cy2) * 256) / ry2;
            let dy1_sq = dy1 * dy1;
            let dy2_sq = dy2 * dy2;
            let row_ptr = unsafe { self.wall_ptr.add(py * self.w) };
            let grid_row = !p3_mode && (py & 15) == 0;
            let bayer_row = (py & 3) << 2;

            for px in 0..self.w {
                if grid_row && (px & 15) == 0 {
                    unsafe { *row_ptr.add(px) = t.dot_grid };
                    continue;
                }
                let mut r16 = br16;
                let mut g16 = bg16;
                let mut b16 = bb16;
                if p3_mode {
                    let dx1 = ((px as i64 - cx1) * 256) / rx1;
                    let d1 = dx1 * dx1 + dy1_sq;
                    let mut w_glow = 0i32;
                    if d1 < 65_536 {
                        let s = 256 - ((d1 >> 8) as i32);
                        w_glow += (s * s * 96) >> 16; // up to ~37% ambient bloom
                    }
                    let dx2 = ((px as i64 - cx2) * 256) / rx2;
                    let d2 = dx2 * dx2 + dy2_sq;
                    if d2 < 65_536 {
                        let s = 256 - ((d2 >> 8) as i32);
                        w_glow += (s * s * 64) >> 16; // soft secondary corner sheen
                    }
                    if w_glow > 0 {
                        let w = w_glow.min(180);
                        r16 += ((((glow.0 as i32) << 4) - r16) * w) >> 8;
                        g16 += ((((glow.1 as i32) << 4) - g16) * w) >> 8;
                        b16 += ((((glow.2 as i32) << 4) - b16) * w) >> 8;
                    }
                    let dither = BAYER_4X4[bayer_row | (px & 3)];
                    r16 += dither;
                    g16 += dither;
                    b16 += dither;
                }
                let r = (r16 >> 4).clamp(0, 255) as u8;
                let g = (g16 >> 4).clamp(0, 255) as u8;
                let b = (b16 >> 4).clamp(0, 255) as u8;
                unsafe { *row_ptr.add(px) = pack_rgb(&self.fb_info, r, g, b) };
            }
        }
        self.wall_key = want_key;
    }

    /// Blit cached wallpaper pixels clipped to `clip` (falls back to flat gradient if uncached).
    pub fn paint_wallpaper_clipped(&mut self, p3_mode: bool, clip: Rect) {
        let c = clip.clamp_to(self.w, self.h);
        if c.is_empty() {
            return;
        }
        self.ensure_wallpaper_cache(p3_mode);
        if !self.wall_ptr.is_null() {
            for py in c.y..(c.y + c.h) {
                unsafe {
                    let src = self.wall_ptr.add(py * self.w + c.x);
                    let dst = self.back_ptr.add(py * self.fb_stride + c.x);
                    core::ptr::copy_nonoverlapping(src, dst, c.w);
                }
            }
            return;
        }
        let col = pack_rgb(
            &self.fb_info,
            self.glass.wall_top_rgb.0,
            self.glass.wall_top_rgb.1,
            self.glass.wall_top_rgb.2,
        );
        self.fill_back_clipped(c, c, col);
    }

    /// Blit an `0x00RRGGBB` `SharedPixelSurface` into `back_ptr` clipped to `cclip`.
    ///
    /// # Safety
    /// `src_base` must point to at least `src_w * (gy + cclip.h)` readable pixels.
    pub unsafe fn blit_pixel_surface_clipped(
        &mut self,
        src_base: *const u32,
        src_w: usize,
        gx: usize,
        gy: usize,
        cclip: Rect,
    ) {
        let ic = cclip.clamp_to(self.w, self.h);
        if ic.is_empty() || src_base.is_null() {
            return;
        }
        let native_bgrx = self.fb_info.red_shift == 16
            && self.fb_info.green_shift == 8
            && self.fb_info.blue_shift == 0;
        for py in ic.y..(ic.y + ic.h) {
            let sy = py - gy;
            let sx0 = ic.x - gx;
            unsafe {
                let src_row = src_base.add(sy * src_w + sx0);
                let dst_row = self.back_ptr.add(py * self.fb_stride + ic.x);
                if native_bgrx {
                    core::ptr::copy_nonoverlapping(src_row, dst_row, ic.w);
                } else {
                    for i in 0..ic.w {
                        let argb = *src_row.add(i);
                        *dst_row.add(i) = pack_rgb(
                            &self.fb_info,
                            ((argb >> 16) & 0xff) as u8,
                            ((argb >> 8) & 0xff) as u8,
                            (argb & 0xff) as u8,
                        );
                    }
                }
            }
        }
    }

    /// Repaint `clip` in `back_ptr` from bottom-to-top Z-order and queue `clip` for MMIO flush.
    pub fn repaint_scene_rect(&mut self, wm: &WindowManager, clip: Rect) {
        let c = clip.clamp_to(self.w, self.h);
        if c.is_empty() {
            return;
        }
        if wm.session_state == SessionVisualState::DisplayBlanked {
            self.fill_back_clipped(c, c, 0);
            self.mmio_dirty.mark(c);
            return;
        }
        let tokens = self.glass;
        let dpi_pct = wm.dpi_pct();
        if wm.session_state == SessionVisualState::SessionLocked {
            crate::glass::overlays::paint_lock_shield_glass(self, wm, &tokens, c);
            self.mmio_dirty.mark(c);
            return;
        }
        crate::glass::chrome::paint_wallpaper(self, wm.p3_mode, &tokens, c);
        crate::glass::chrome::paint_topbar_and_tabs(self, wm, &tokens, c);
        for z in 0..wm.z_len {
            let slot_idx = wm.z_stack[z];
            crate::glass::chrome::paint_window_glass(
                self,
                &wm.windows[slot_idx],
                wm.p3_mode,
                dpi_pct,
                &tokens,
                c,
            );
        }
        crate::glass::chrome::paint_dock_and_snap(self, wm, &tokens, c);
        crate::glass::overlays::paint_overlays(self, wm, &tokens, c);
        self.mmio_dirty.mark(c);
    }

    /// Drain all pending `scene_dirty` + `mmio_dirty` rectangles and stream modified scanlines
    /// (compositing the 12x16 software cursor on-the-fly) into the GOP framebuffer or bound `dry-gpu`.
    pub fn flush_all(&mut self, wm: &mut WindowManager) -> (u64, u64) {
        crate::glass::chrome::expand_blur_damage(wm, &mut self.scene_dirty);
        let mut s_rects = [Rect::EMPTY; MAX_DIRTY_RECTS];
        let sn = self.scene_dirty.drain_into(self.w, self.h, &mut s_rects);
        for r in s_rects.iter().take(sn) {
            self.repaint_scene_rect(wm, *r);
        }

        let mut m_rects = [Rect::EMPTY; MAX_DIRTY_RECTS];
        let mut mn = self.mmio_dirty.drain_into(self.w, self.h, &mut m_rects);
        if mn == 0 {
            return (0, 0);
        }
        if wm.gpu_ep_cap != 0 {
            let mut bbox = m_rects[0];
            for r in m_rects.iter().take(mn).skip(1) {
                bbox = bbox.union(*r);
            }
            if let Some(res) = crate::wm_ipc::try_gpu_present_or_fallback(wm, bbox) {
                return res;
            }
            m_rects[0] = Rect::new(0, 0, self.w, self.h);
            mn = 1;
        }

        let cur_r = if wm.session_state == SessionVisualState::DisplayBlanked {
            Rect::EMPTY
        } else {
            wm.cursor_rect()
        };
        let cur_scale = if wm.cursor_large { 2usize } else { 1usize };
        let t0 = rdtsc_user();
        let mut total_px = 0u64;

        for r in m_rects.iter().take(mn) {
            total_px += (r.w * r.h) as u64;
            let has_cur = !cur_r.intersect(*r).is_empty();
            for y in r.y..(r.y + r.h) {
                let row_has_cur = has_cur && y >= cur_r.y && y < cur_r.y + cur_r.h;
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
                    let sy = ((y - cur_r.y) / cur_scale).min(CURSOR_H - 1);
                    let sprite_row = &CURSOR_SPRITE_12X16[sy];
                    for x in r.x..(r.x + r.w) {
                        // SAFETY: `(x, y)` clamped to `(self.w, self.h)`.
                        let mut px = unsafe { *self.back_ptr.add(y * self.w + x) };
                        if x >= cur_r.x && x < cur_r.x + cur_r.w {
                            let sx = ((x - cur_r.x) / cur_scale).min(CURSOR_W - 1);
                            match sprite_row[sx] {
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
