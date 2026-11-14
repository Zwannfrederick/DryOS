// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Shared 2D UI Toolkit (`PixelCanvas` & `AppPalette`) for Native `SharedPixelSurface` Apps.

use crate::gfx::Rect;
use crate::glass::blur::in_rounded_rect;
use crate::glass::typography::{draw_str_2x_raw, draw_str_raw};

#[derive(Clone, Copy)]
pub struct AppPalette {
    pub bg: u32,
    pub surface: u32,
    pub surface_alt: u32,
    pub btn_bg: u32,
    pub btn_accent: u32,
    pub border: u32,
    pub fg: u32,
    pub fg_muted: u32,
    pub fg_on_accent: u32,
    pub accent: u32,
    pub ok: u32,
    pub warn: u32,
}

impl AppPalette {
    pub const fn for_mode(light_mode: bool) -> Self {
        if light_mode {
            Self {
                bg: 0x00F4_F7FB,
                surface: 0x00FF_FFFF,
                surface_alt: 0x00E6_EEF8,
                btn_bg: 0x00E2_EAF4,
                btn_accent: 0x0002_84C7,
                border: 0x00B6_C6DA,
                fg: 0x000F_172A,
                fg_muted: 0x0047_5569,
                fg_on_accent: 0x00FF_FFFF,
                accent: 0x0003_69A1,
                ok: 0x0004_7857,
                warn: 0x00B4_5309,
            }
        } else {
            Self {
                bg: 0x000E_1524,
                surface: 0x0013_1D31,
                surface_alt: 0x0018_253E,
                btn_bg: 0x001E_2D4A,
                btn_accent: 0x000E_7490,
                border: 0x0033_4666,
                fg: 0x00F8_FAFC,
                fg_muted: 0x0094_A3B8,
                fg_on_accent: 0x00F8_FAFC,
                accent: 0x0022_D3EE,
                ok: 0x0034_D399,
                warn: 0x00FB_BF24,
            }
        }
    }
}

pub struct PixelCanvas<'a> {
    pub buf: &'a mut [u32],
    pub w: usize,
    pub h: usize,
}

impl<'a> PixelCanvas<'a> {
    pub fn new(buf: &'a mut [u32], w: usize, h: usize) -> Self {
        Self { buf, w, h }
    }

    #[inline]
    pub fn full_rect(&self) -> Rect {
        Rect::new(0, 0, self.w, self.h)
    }

    pub fn clear(&mut self, color: u32) {
        let len = (self.w * self.h).min(self.buf.len());
        self.buf[..len].fill(color);
    }

    pub fn fill_rect(&mut self, r: Rect, color: u32) {
        let c = r.clamp_to(self.w, self.h);
        if c.is_empty() {
            return;
        }
        for py in c.y..(c.y + c.h) {
            let off = py * self.w + c.x;
            self.buf[off..(off + c.w)].fill(color);
        }
    }

    pub fn fill_rounded_card(&mut self, r: Rect, corner_r: usize, fill: u32, border: u32) {
        let c = r.clamp_to(self.w, self.h);
        if c.is_empty() {
            return;
        }
        for py in c.y..(c.y + c.h) {
            let row_off = py * self.w;
            for px in c.x..(c.x + c.w) {
                if !in_rounded_rect(r, corner_r, px, py) {
                    continue;
                }
                let on_edge = py == r.y
                    || py + 1 == r.y + r.h
                    || px == r.x
                    || px + 1 == r.x + r.w
                    || !in_rounded_rect(r, corner_r, px.saturating_sub(1), py)
                    || !in_rounded_rect(r, corner_r, px + 1, py)
                    || !in_rounded_rect(r, corner_r, px, py.saturating_sub(1))
                    || !in_rounded_rect(r, corner_r, px, py + 1);
                self.buf[row_off + px] = if on_edge { border } else { fill };
            }
        }
    }

    pub fn text(&mut self, x: usize, y: usize, s: &[u8], fg: u32, bg: Option<u32>) {
        let clip = self.full_rect();
        // SAFETY: `self.buf` covers `self.w * self.h` pixels.
        unsafe { draw_str_raw(self.buf.as_mut_ptr(), self.w, self.h, x, y, s, fg, bg, clip) };
    }

    pub fn text_2x(&mut self, x: usize, y: usize, s: &[u8], fg: u32) {
        let clip = self.full_rect();
        // SAFETY: `self.buf` covers `self.w * self.h` pixels.
        unsafe { draw_str_2x_raw(self.buf.as_mut_ptr(), self.w, self.h, x, y, s, fg, clip) };
    }

    pub fn button(&mut self, r: Rect, label: &[u8], fill: u32, fg: u32, border: u32) {
        self.fill_rounded_card(r, 5, fill, border);
        let tx = r.x + r.w.saturating_sub(label.len() * 8) / 2;
        let ty = r.y + r.h.saturating_sub(16) / 2;
        self.text(tx, ty, label, fg, None);
    }
}
