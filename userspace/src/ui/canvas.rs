// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Pure `#![no_std]`, integer-arithmetic anti-aliased 2D `Canvas` (`RGB888` & GOP `PixelFormat`).

use crate::gfx::Rect;
use crate::glass::theme::ThemeMode;
use crate::ttf::{
    div255, draw_text_baseline_raw, draw_text_ellipsis_raw, draw_text_selected_raw,
    draw_text_top_raw, font_metrics, measure_text, FontId, PixelFormat,
};

#[inline]
pub fn isqrt64(n: u64) -> u64 {
    if n < 2 {
        return n;
    }
    let mut x = 1u64 << ((64 - n.leading_zeros()).div_ceil(2));
    loop {
        let nx = (x + n / x) >> 1;
        if nx >= x {
            return x;
        }
        x = nx;
    }
}

#[inline]
pub fn lerp_rgb24(a: u32, b: u32, t_num: u32, t_den: u32) -> u32 {
    if t_den == 0 {
        return a;
    }
    let t = t_num.min(t_den);
    let inv = t_den - t;
    let r = (((a >> 16) & 0xFF) * inv + ((b >> 16) & 0xFF) * t + (t_den >> 1)) / t_den;
    let g = (((a >> 8) & 0xFF) * inv + ((b >> 8) & 0xFF) * t + (t_den >> 1)) / t_den;
    let bl = ((a & 0xFF) * inv + (b & 0xFF) * t + (t_den >> 1)) / t_den;
    (r << 16) | (g << 8) | bl
}

/// 256-level AA coverage `[0..=255]` inside a rounded rectangle (`sd <= 0` in `Q4` space).
#[inline]
pub fn rrect_cov(r: Rect, corner_r: usize, px: usize, py: usize) -> u8 {
    let rad = corner_r.min(r.w / 2).min(r.h / 2);
    if rad == 0 {
        return 255;
    }
    let rx = px - r.x;
    let ry = py - r.y;
    let in_left = rx < rad;
    let in_right = rx >= r.w - rad;
    let in_top = ry < rad;
    let in_bot = ry >= r.h - rad;
    if !(in_left || in_right) || !(in_top || in_bot) {
        return 255;
    }
    let r_q4 = (rad as i64) * 16;
    let cx_q4 = if in_left { r_q4 } else { (r.w - rad) as i64 * 16 };
    let cy_q4 = if in_top { r_q4 } else { (r.h - rad) as i64 * 16 };
    let px_q4 = rx as i64 * 16 + 8;
    let py_q4 = ry as i64 * 16 + 8;
    let dx = px_q4 - cx_q4;
    let dy = py_q4 - cy_q4;
    let dist_q4 = isqrt64((dx * dx + dy * dy) as u64) as i64;
    let sd_q4 = dist_q4 - r_q4;
    (((10 - sd_q4).clamp(0, 20) * 255 + 10) / 20) as u8
}

/// Safe clipped 2D drawing target backed by raw framebuffer or `SharedPixelSurface` pixels.
#[derive(Clone, Copy)]
pub struct Canvas {
    pub ptr: *mut u32,
    pub w: usize,
    pub h: usize,
    pub stride: usize,
    pub fmt: PixelFormat,
    pub clip: Rect,
}

impl Canvas {
    #[inline]
    pub fn from_slice(buf: &mut [u32], w: usize, h: usize) -> Self {
        let len = buf.len();
        let safe_h = len.checked_div(w).map_or(0, |rows| h.min(rows));
        Self {
            ptr: buf.as_mut_ptr(),
            w,
            h: safe_h,
            stride: w,
            fmt: PixelFormat::RGB888,
            clip: Rect::new(0, 0, w, safe_h),
        }
    }

    /// Create a canvas over an arbitrary raw pixel pointer (`back_ptr` or FB).
    /// # Safety
    /// `ptr` must remain valid for writes up to `h * stride` `u32` elements.
    #[inline]
    pub unsafe fn from_raw(
        ptr: *mut u32,
        w: usize,
        h: usize,
        stride: usize,
        fmt: PixelFormat,
        clip: Rect,
    ) -> Self {
        Self {
            ptr,
            w,
            h,
            stride,
            fmt,
            clip: clip.clamp_to(w, h),
        }
    }

    #[inline]
    pub fn sub_clip(&self, r: Rect) -> Self {
        let mut copy = *self;
        copy.clip = self.clip.intersect(r).clamp_to(self.w, self.h);
        copy
    }

    #[inline]
    pub fn clear(&mut self, rgb: u32) {
        self.fill_rect(Rect::new(0, 0, self.w, self.h), rgb);
    }

    #[inline(always)]
    fn blend_px(&mut self, x: usize, y: usize, rgb: u32, alpha: u8) {
        if alpha == 0 {
            return;
        }
        // SAFETY: callers always clamp `(x, y)` to `self.clip <= (self.w, self.h)`.
        let dst = unsafe { self.ptr.add(y * self.stride + x) };
        if alpha == 255 {
            unsafe { *dst = self.fmt.pack_rgb24(rgb) };
        } else {
            let d = unsafe { *dst };
            unsafe { *dst = self.fmt.blend_rgb24(d, rgb, alpha) };
        }
    }

    pub fn fill_rect(&mut self, r: Rect, rgb: u32) {
        let c = r.intersect(self.clip).clamp_to(self.w, self.h);
        if c.is_empty() {
            return;
        }
        let packed = self.fmt.pack_rgb24(rgb);
        for py in c.y..(c.y + c.h) {
            // SAFETY: clamped within `self.w, self.h`.
            let row = unsafe { core::slice::from_raw_parts_mut(self.ptr.add(py * self.stride + c.x), c.w) };
            row.fill(packed);
        }
    }

    pub fn fill_rect_alpha(&mut self, r: Rect, rgb: u32, alpha: u8) {
        if alpha == 0 {
            return;
        }
        if alpha == 255 {
            self.fill_rect(r, rgb);
            return;
        }
        let c = r.intersect(self.clip).clamp_to(self.w, self.h);
        for py in c.y..(c.y + c.h) {
            for px in c.x..(c.x + c.w) {
                self.blend_px(px, py, rgb, alpha);
            }
        }
    }

    pub fn fill_vgrad_rect(&mut self, r: Rect, top_rgb: u32, bot_rgb: u32) {
        let c = r.intersect(self.clip).clamp_to(self.w, self.h);
        if c.is_empty() {
            return;
        }
        let den = r.h.saturating_sub(1).max(1) as u32;
        for py in c.y..(c.y + c.h) {
            let col = lerp_rgb24(top_rgb, bot_rgb, (py - r.y) as u32, den);
            let packed = self.fmt.pack_rgb24(col);
            // SAFETY: clamped within `self.w, self.h`.
            let row = unsafe { core::slice::from_raw_parts_mut(self.ptr.add(py * self.stride + c.x), c.w) };
            row.fill(packed);
        }
    }

    pub fn hline(&mut self, x: usize, y: usize, w: usize, rgb: u32, alpha: u8) {
        self.fill_rect_alpha(Rect::new(x, y, w, 1), rgb, alpha);
    }

    pub fn vline(&mut self, x: usize, y: usize, h: usize, rgb: u32, alpha: u8) {
        self.fill_rect_alpha(Rect::new(x, y, 1, h), rgb, alpha);
    }

    pub fn fill_rrect_aa(&mut self, r: Rect, corner_r: usize, rgb: u32, alpha: u8) {
        if alpha == 0 || r.is_empty() {
            return;
        }
        let rad = corner_r.min(r.w / 2).min(r.h / 2);
        if rad == 0 {
            self.fill_rect_alpha(r, rgb, alpha);
            return;
        }
        let c = r.intersect(self.clip).clamp_to(self.w, self.h);
        if c.is_empty() {
            return;
        }
        let packed = self.fmt.pack_rgb24(rgb);
        let mid_y0 = r.y + rad;
        let mid_y1 = r.y + r.h - rad;
        let mid_x0 = r.x + rad;
        let mid_x1 = r.x + r.w - rad;

        for py in c.y..(c.y + c.h) {
            if py >= mid_y0 && py < mid_y1 {
                if alpha == 255 {
                    // SAFETY: `c` is clamped to `self.w, self.h`.
                    let row = unsafe { core::slice::from_raw_parts_mut(self.ptr.add(py * self.stride + c.x), c.w) };
                    row.fill(packed);
                } else {
                    for px in c.x..(c.x + c.w) {
                        self.blend_px(px, py, rgb, alpha);
                    }
                }
                continue;
            }
            let flat_s = c.x.max(mid_x0);
            let flat_e = (c.x + c.w).min(mid_x1);
            for px in c.x..flat_s.min(c.x + c.w) {
                let cov = rrect_cov(r, rad, px, py);
                let a = div255(cov as u32 * alpha as u32);
                self.blend_px(px, py, rgb, a);
            }
            if flat_e > flat_s {
                if alpha == 255 {
                    // SAFETY: `flat_s..flat_e` within `c`.
                    let row = unsafe {
                        core::slice::from_raw_parts_mut(
                            self.ptr.add(py * self.stride + flat_s),
                            flat_e - flat_s,
                        )
                    };
                    row.fill(packed);
                } else {
                    for px in flat_s..flat_e {
                        self.blend_px(px, py, rgb, alpha);
                    }
                }
            }
            for px in flat_e.max(c.x)..(c.x + c.w) {
                let cov = rrect_cov(r, rad, px, py);
                let a = div255(cov as u32 * alpha as u32);
                self.blend_px(px, py, rgb, a);
            }
        }
    }

    pub fn fill_vgrad_rrect_aa(
        &mut self,
        r: Rect,
        corner_r: usize,
        top_rgb: u32,
        bot_rgb: u32,
        alpha: u8,
    ) {
        if alpha == 0 || r.is_empty() {
            return;
        }
        let rad = corner_r.min(r.w / 2).min(r.h / 2);
        let c = r.intersect(self.clip).clamp_to(self.w, self.h);
        let den = r.h.saturating_sub(1).max(1) as u32;
        for py in c.y..(c.y + c.h) {
            let rgb = lerp_rgb24(top_rgb, bot_rgb, (py - r.y) as u32, den);
            for px in c.x..(c.x + c.w) {
                let cov = if rad == 0 { 255 } else { rrect_cov(r, rad, px, py) };
                let a = div255(cov as u32 * alpha as u32);
                self.blend_px(px, py, rgb, a);
            }
        }
    }

    pub fn stroke_rrect_aa(&mut self, r: Rect, corner_r: usize, rgb: u32, alpha: u8) {
        if alpha == 0 || r.w < 2 || r.h < 2 {
            return;
        }
        let c = r.intersect(self.clip).clamp_to(self.w, self.h);
        if c.is_empty() {
            return;
        }
        let rad = corner_r.min(r.w / 2).min(r.h / 2);
        let inner = Rect::new(r.x + 1, r.y + 1, r.w - 2, r.h - 2);
        let irad = rad.saturating_sub(1);
        for py in c.y..(c.y + c.h) {
            let in_top_band = py < r.y + rad + 1;
            let in_bot_band = py + rad + 1 >= r.y + r.h;
            if !in_top_band && !in_bot_band {
                if r.x >= c.x && r.x < c.x + c.w {
                    self.blend_px(r.x, py, rgb, alpha);
                }
                let rx = r.x + r.w - 1;
                if rx >= c.x && rx < c.x + c.w {
                    self.blend_px(rx, py, rgb, alpha);
                }
                continue;
            }
            for px in c.x..(c.x + c.w) {
                let outer_cov = rrect_cov(r, rad, px, py);
                if outer_cov == 0 {
                    continue;
                }
                let inner_cov = if inner.contains(px, py) {
                    rrect_cov(inner, irad, px, py)
                } else {
                    0
                };
                let edge_cov = outer_cov.saturating_sub(inner_cov);
                if edge_cov > 0 {
                    self.blend_px(px, py, rgb, div255(edge_cov as u32 * alpha as u32));
                }
            }
        }
    }

    pub fn fill_bordered_rrect_aa(
        &mut self,
        r: Rect,
        corner_r: usize,
        fill_rgb: u32,
        border_rgb: u32,
    ) {
        self.fill_rrect_aa(r, corner_r, fill_rgb, 255);
        self.stroke_rrect_aa(r, corner_r, border_rgb, 255);
    }

    pub fn fill_circle_aa(&mut self, cx_q4: i32, cy_q4: i32, rad_q4: i32, rgb: u32, alpha: u8) {
        if rad_q4 <= 0 || alpha == 0 {
            return;
        }
        let x0 = ((cx_q4 - rad_q4 - 16) >> 4).max(self.clip.x as i32);
        let y0 = ((cy_q4 - rad_q4 - 16) >> 4).max(self.clip.y as i32);
        let x1 = ((cx_q4 + rad_q4 + 32) >> 4).min((self.clip.x + self.clip.w) as i32);
        let y1 = ((cy_q4 + rad_q4 + 32) >> 4).min((self.clip.y + self.clip.h) as i32);
        for py in y0..y1 {
            let py_q4 = py * 16 + 8 - cy_q4;
            for px in x0..x1 {
                let px_q4 = px * 16 + 8 - cx_q4;
                let d_q4 = isqrt64((px_q4 as i64 * px_q4 as i64 + py_q4 as i64 * py_q4 as i64) as u64) as i64;
                let sd = d_q4 - rad_q4 as i64;
                let cov = (((10 - sd).clamp(0, 20) * 255 + 10) / 20) as u8;
                if cov > 0 {
                    self.blend_px(px as usize, py as usize, rgb, div255(cov as u32 * alpha as u32));
                }
            }
        }
    }

    pub fn stroke_circle_aa(
        &mut self,
        cx_q4: i32,
        cy_q4: i32,
        rad_q4: i32,
        thick_q4: i32,
        rgb: u32,
        alpha: u8,
    ) {
        if rad_q4 <= 0 || thick_q4 <= 0 || alpha == 0 {
            return;
        }
        let half_t = (thick_q4 / 2).max(4);
        let pad = rad_q4 + half_t + 20;
        let x0 = ((cx_q4 - pad) >> 4).max(self.clip.x as i32);
        let y0 = ((cy_q4 - pad) >> 4).max(self.clip.y as i32);
        let x1 = ((cx_q4 + pad + 16) >> 4).min((self.clip.x + self.clip.w) as i32);
        let y1 = ((cy_q4 + pad + 16) >> 4).min((self.clip.y + self.clip.h) as i32);
        for py in y0..y1 {
            let py_q4 = py * 16 + 8 - cy_q4;
            for px in x0..x1 {
                let px_q4 = px * 16 + 8 - cx_q4;
                let d_q4 = isqrt64((px_q4 as i64 * px_q4 as i64 + py_q4 as i64 * py_q4 as i64) as u64) as i64;
                let ring_d = (d_q4 - rad_q4 as i64).abs() - half_t as i64;
                let cov = (((9 - ring_d).clamp(0, 18) * 255 + 9) / 18) as u8;
                if cov > 0 {
                    self.blend_px(px as usize, py as usize, rgb, div255(cov as u32 * alpha as u32));
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn stroke_line_aa(
        &mut self,
        x0_q4: i32,
        y0_q4: i32,
        x1_q4: i32,
        y1_q4: i32,
        thick_q4: i32,
        rgb: u32,
        alpha: u8,
    ) {
        if alpha == 0 || thick_q4 <= 0 {
            return;
        }
        let half_t = (thick_q4 / 2).max(4) as i64;
        let pad = half_t as i32 + 20;
        let bx0 = (x0_q4.min(x1_q4) - pad) >> 4;
        let by0 = (y0_q4.min(y1_q4) - pad) >> 4;
        let bx1 = (x0_q4.max(x1_q4) + pad + 16) >> 4;
        let by1 = (y0_q4.max(y1_q4) + pad + 16) >> 4;

        let cx0 = bx0.max(self.clip.x as i32);
        let cy0 = by0.max(self.clip.y as i32);
        let cx1 = bx1.min((self.clip.x + self.clip.w) as i32);
        let cy1 = by1.min((self.clip.y + self.clip.h) as i32);

        let vx = (x1_q4 - x0_q4) as i64;
        let vy = (y1_q4 - y0_q4) as i64;
        let len_sq = vx * vx + vy * vy;

        for py in cy0..cy1 {
            let py_q4 = py as i64 * 16 + 8;
            for px in cx0..cx1 {
                let px_q4 = px as i64 * 16 + 8;
                let wx = px_q4 - x0_q4 as i64;
                let wy = py_q4 - y0_q4 as i64;
                let dist_q4 = if len_sq == 0 {
                    isqrt64((wx * wx + wy * wy) as u64) as i64
                } else {
                    let dot = (wx * vx + wy * vy).clamp(0, len_sq);
                    let qx = wx - (dot * vx) / len_sq;
                    let qy = wy - (dot * vy) / len_sq;
                    isqrt64((qx * qx + qy * qy) as u64) as i64
                };
                let sd = dist_q4 - half_t;
                let cov = (((9 - sd).clamp(0, 18) * 255 + 9) / 18) as u8;
                if cov > 0 {
                    self.blend_px(px as usize, py as usize, rgb, div255(cov as u32 * alpha as u32));
                }
            }
        }
    }

    pub fn draw_sparkline_aa(
        &mut self,
        r: Rect,
        samples: &[u16],
        max_val: u16,
        stroke_rgb: u32,
        fill_rgb: u32,
        fill_alpha: u8,
    ) {
        if samples.len() < 2 || r.w < 4 || r.h < 4 || max_val == 0 {
            return;
        }
        let n = samples.len();
        let den = n - 1;
        let mut prev_x = 0i32;
        let mut prev_y = 0i32;
        let bot_y = r.y + r.h;

        let sub = self.sub_clip(r);
        let mut c = sub;
        if fill_alpha > 0 {
            for px in r.x..(r.x + r.w) {
                let rel = px - r.x;
                let idx_num = rel * den;
                let i0 = (idx_num / r.w).min(den - 1);
                let frac = idx_num % r.w;
                let v0 = samples[i0] as usize;
                let v1 = samples[i0 + 1] as usize;
                let v = (v0 * (r.w - frac) + v1 * frac) / r.w;
                let h = (v.min(max_val as usize) * r.h) / max_val as usize;
                let top = bot_y.saturating_sub(h);
                for py in top..bot_y {
                    if c.clip.contains(px, py) {
                        c.blend_px(px, py, fill_rgb, fill_alpha);
                    }
                }
            }
        }

        for (i, &val) in samples.iter().enumerate() {
            let x_q4 = ((r.x as i64 + ((i as i64 * (r.w as i64 - 1)) / den as i64)) * 16) as i32;
            let h_px = ((val.min(max_val) as i64 * (r.h as i64 - 4)) / max_val as i64) as i32;
            let y_q4 = (bot_y as i32 - 2 - h_px) * 16;
            if i > 0 {
                c.stroke_line_aa(prev_x, prev_y, x_q4, y_q4, 24, stroke_rgb, 255);
            }
            prev_x = x_q4;
            prev_y = y_q4;
        }
    }

    pub fn draw_text(&mut self, x: i32, top_y: i32, font: FontId, size_px: u8, text: &[u8], rgb: u32) -> i32 {
        // SAFETY: `self.ptr` and `self.clip` respect `self.w, self.h`.
        unsafe {
            draw_text_top_raw(
                self.ptr,
                self.stride,
                self.h,
                self.fmt,
                font,
                size_px,
                x,
                top_y,
                text,
                rgb,
                self.clip,
            )
        }
    }

    pub fn draw_text_baseline(
        &mut self,
        x: i32,
        baseline_y: i32,
        font: FontId,
        size_px: u8,
        text: &[u8],
        rgb: u32,
    ) -> i32 {
        // SAFETY: `self.ptr` covers `self.stride * self.h`.
        unsafe {
            draw_text_baseline_raw(
                self.ptr,
                self.stride,
                self.h,
                self.fmt,
                font,
                size_px,
                x,
                baseline_y,
                text,
                rgb,
                self.clip,
            )
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_ellipsis(
        &mut self,
        x: i32,
        top_y: i32,
        max_w: i32,
        font: FontId,
        size_px: u8,
        text: &[u8],
        rgb: u32,
    ) -> i32 {
        // SAFETY: `self.ptr` covers `self.stride * self.h`.
        unsafe {
            draw_text_ellipsis_raw(
                self.ptr,
                self.stride,
                self.h,
                self.fmt,
                font,
                size_px,
                x,
                top_y,
                max_w,
                text,
                rgb,
                self.clip,
            )
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_selected(
        &mut self,
        x: i32,
        top_y: i32,
        font: FontId,
        size_px: u8,
        text: &[u8],
        fg_rgb: u32,
        sel_start: usize,
        sel_end: usize,
        sel_bg_rgb: u32,
        sel_fg_rgb: u32,
    ) {
        // SAFETY: `self.ptr` covers `self.stride * self.h`.
        unsafe {
            draw_text_selected_raw(
                self.ptr,
                self.stride,
                self.h,
                self.fmt,
                font,
                size_px,
                x,
                top_y,
                text,
                fg_rgb,
                sel_start,
                sel_end,
                sel_bg_rgb,
                sel_fg_rgb,
                self.clip,
            );
        }
    }

    pub fn draw_text_centered(&mut self, r: Rect, font: FontId, size_px: u8, text: &[u8], rgb: u32) {
        let m = font_metrics(font, size_px);
        let tw = measure_text(font, size_px, text);
        let x = r.x as i32 + ((r.w as i32 - tw) / 2);
        let y = r.y as i32 + ((r.h as i32 - (m.ascent + m.descent)) / 2);
        let mut clipped = self.sub_clip(r);
        let _ = clipped.draw_text(x, y, font, size_px, text, rgb);
    }

    /// Render a single Q12 `[0..=4096]` normalized SDF primitive into `r`.
    pub fn draw_q12_shape(&mut self, r: Rect, s: Q12Shape, pal: VectorPalette, small: bool) {
        if r.w < 2 || r.h < 2 || s.alpha == 0 {
            return;
        }
        match s.optical {
            VecOptical::SmallOnly if !small => return,
            VecOptical::LargeOnly if small => return,
            _ => {}
        }
        let rgb0 = pal.resolve(s.color, s.rgb_top);
        let dw = r.w as i32;
        let dh = r.h as i32;
        match s.kind {
            VecShapeKind::RRect | VecShapeKind::VGradRRect => {
                let sx = r.x as i32 + (s.x as i32 * dw + 2048).div_euclid(4096);
                let sy = r.y as i32 + (s.y as i32 * dh + 2048).div_euclid(4096);
                let rw = ((s.w as i32 * dw + 2048).div_euclid(4096)).max(1);
                let rh = ((s.h as i32 * dh + 2048).div_euclid(4096)).max(1);
                let rx = sx.max(0) as usize;
                let ry = sy.max(0) as usize;
                let trim_x = rx as i32 - sx;
                let trim_y = ry as i32 - sy;
                if rw <= trim_x || rh <= trim_y {
                    return;
                }
                let shape_r = Rect::new(rx, ry, (rw - trim_x) as usize, (rh - trim_y) as usize);
                let rad = ((s.radius as i32 * dh + 2048).div_euclid(4096)).max(0) as usize;
                if s.kind == VecShapeKind::VGradRRect {
                    self.fill_vgrad_rrect_aa(shape_r, rad, rgb0, s.rgb_bot, s.alpha);
                } else if s.thick == 0 {
                    self.fill_rrect_aa(shape_r, rad, rgb0, s.alpha);
                } else {
                    self.stroke_rrect_aa(shape_r, rad, rgb0, s.alpha);
                }
            }
            VecShapeKind::Circle => {
                let cx = (r.x as i32) * 16 + (s.x as i32 * dw + 128).div_euclid(256);
                let cy = (r.y as i32) * 16 + (s.y as i32 * dh + 128).div_euclid(256);
                let rad = ((s.w as i32 * dh + 128).div_euclid(256)).max(12);
                self.fill_circle_aa(cx, cy, rad, rgb0, s.alpha);
            }
            VecShapeKind::Ring => {
                let cx = (r.x as i32) * 16 + (s.x as i32 * dw + 128).div_euclid(256);
                let cy = (r.y as i32) * 16 + (s.y as i32 * dh + 128).div_euclid(256);
                let rad = ((s.w as i32 * dh + 128).div_euclid(256)).max(12);
                let th = ((s.thick as i32 * dh + 128).div_euclid(256)).max(12);
                self.stroke_circle_aa(cx, cy, rad, th, rgb0, s.alpha);
            }
            VecShapeKind::Line => {
                let x0 = (r.x as i32) * 16 + (s.x as i32 * dw + 128).div_euclid(256);
                let y0 = (r.y as i32) * 16 + (s.y as i32 * dh + 128).div_euclid(256);
                let x1 = (r.x as i32) * 16 + (s.w as i32 * dw + 128).div_euclid(256);
                let y1 = (r.y as i32) * 16 + (s.h as i32 * dh + 128).div_euclid(256);
                let th = ((s.thick as i32 * dh + 128).div_euclid(256)).max(12);
                self.stroke_line_aa(x0, y0, x1, y1, th, rgb0, s.alpha);
            }
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VecShapeKind { RRect = 0, VGradRRect = 1, Circle = 2, Ring = 3, Line = 4 }
#[repr(u8)] #[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VecOptical { Always = 0, SmallOnly = 1, LargeOnly = 2 }
#[repr(u8)] #[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VecColorSlot { Custom = 0, FgPrimary = 1, FgMuted = 2, Accent = 3, Ok = 4, Warn = 5, Danger = 6, BgSurface = 7, RimHighlight = 8 }

#[derive(Clone, Copy, Debug)]
pub struct Q12Shape {
    pub kind: VecShapeKind, pub optical: VecOptical, pub color: VecColorSlot, pub alpha: u8,
    pub x: i16, pub y: i16, pub w: i16, pub h: i16, pub radius: i16, pub thick: i16,
    pub rgb_top: u32, pub rgb_bot: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct VectorPalette {
    pub fg_primary: u32, pub fg_muted: u32, pub accent: u32, pub ok: u32,
    pub warn: u32, pub danger: u32, pub bg_surface: u32, pub rim_highlight: u32,
}

impl VectorPalette {
    pub const fn from_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::ObsidianDark => Self {
                fg_primary: 0x00F1_F5F9, fg_muted: 0x008E_9CB0, accent: 0x0038_BDF8,
                ok: 0x0034_D399, warn: 0x00FB_BF24, danger: 0x00F8_7171,
                bg_surface: 0x0017_1E2E, rim_highlight: 0x00FF_FFFF,
            },
            ThemeMode::FrostedDaylight => Self {
                fg_primary: 0x000F_172A, fg_muted: 0x0052_6075, accent: 0x001D_4ED8,
                ok: 0x0004_7857, warn: 0x00B4_5309, danger: 0x00B9_1C1C,
                bg_surface: 0x00FF_FFFF, rim_highlight: 0x00FF_FFFF,
            },
            ThemeMode::HighContrast => Self {
                fg_primary: 0x00FF_FFFF, fg_muted: 0x00CB_D5E1, accent: 0x00FF_E600,
                ok: 0x004A_DE80, warn: 0x00FD_E047, danger: 0x00FC_A5A5,
                bg_surface: 0x000C_1018, rim_highlight: 0x00FF_E600,
            },
        }
    }

    pub fn from_fg_accent(fg: u32, accent: u32) -> Self {
        let luma = ((fg >> 16) & 0xFF) * 299 + ((fg >> 8) & 0xFF) * 587 + (fg & 0xFF) * 114;
        if luma >= 128_000 {
            Self {
                fg_primary: fg, fg_muted: lerp_rgb24(fg, 0x0047_5569, 45, 100), accent,
                ok: 0x0034_D399, warn: 0x00FB_BF24, danger: 0x00F8_7171,
                bg_surface: 0x0017_1E2E, rim_highlight: 0x00FF_FFFF,
            }
        } else {
            Self {
                fg_primary: fg, fg_muted: lerp_rgb24(fg, 0x0064_748B, 35, 100), accent,
                ok: 0x0004_7857, warn: 0x00B4_5309, danger: 0x00B9_1C1C,
                bg_surface: 0x00FF_FFFF, rim_highlight: 0x00FF_FFFF,
            }
        }
    }

    #[inline]
    pub const fn resolve(&self, slot: VecColorSlot, custom_rgb: u32) -> u32 {
        match slot {
            VecColorSlot::Custom => custom_rgb,
            VecColorSlot::FgPrimary => self.fg_primary,
            VecColorSlot::FgMuted => self.fg_muted,
            VecColorSlot::Accent => self.accent,
            VecColorSlot::Ok => self.ok,
            VecColorSlot::Warn => self.warn,
            VecColorSlot::Danger => self.danger,
            VecColorSlot::BgSurface => self.bg_surface,
            VecColorSlot::RimHighlight => self.rim_highlight,
        }
    }
}

/// Center-fit a `Rect` to preserve exact `vb_w : vb_h` aspect ratio without clipping.
#[inline]
pub fn fit_aspect_rect(r: Rect, vb_w: u16, vb_h: u16) -> Rect {
    if r.is_empty() || vb_w == 0 || vb_h == 0 {
        return r;
    }
    let sw = vb_w as usize;
    let sh = vb_h as usize;
    if r.w * sh > r.h * sw {
        let fit_w = ((r.h * sw) / sh).max(1);
        Rect::new(r.x + (r.w - fit_w) / 2, r.y, fit_w, r.h)
    } else {
        let fit_h = ((r.w * sh) / sw).max(1);
        Rect::new(r.x, r.y + (r.h - fit_h) / 2, r.w, fit_h)
    }
}
