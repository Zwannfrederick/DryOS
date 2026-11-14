// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! UTF-8 text shaping, measuring, hit-testing, ellipsis truncation, selection, and AA blending.

use crate::gfx::Rect;
use crate::UserFbInfo;

use super::atlas::{font_metrics, glyph_advance_24_8, with_glyph, FontId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelFormat {
    pub r_shift: u8,
    pub g_shift: u8,
    pub b_shift: u8,
}

impl PixelFormat {
    pub const RGB888: Self = Self {
        r_shift: 16,
        g_shift: 8,
        b_shift: 0,
    };

    #[inline]
    pub const fn from_fb(info: &UserFbInfo) -> Self {
        Self {
            r_shift: info.red_shift,
            g_shift: info.green_shift,
            b_shift: info.blue_shift,
        }
    }

    #[inline(always)]
    pub const fn pack(self, r: u8, g: u8, b: u8) -> u32 {
        ((r as u32) << self.r_shift)
            | ((g as u32) << self.g_shift)
            | ((b as u32) << self.b_shift)
    }

    #[inline(always)]
    pub const fn pack_rgb24(self, rgb: u32) -> u32 {
        if self.r_shift == 16 && self.g_shift == 8 && self.b_shift == 0 {
            rgb & 0x00FF_FFFF
        } else {
            self.pack(
                ((rgb >> 16) & 0xFF) as u8,
                ((rgb >> 8) & 0xFF) as u8,
                (rgb & 0xFF) as u8,
            )
        }
    }

    #[inline(always)]
    pub const fn unpack(self, px: u32) -> (u8, u8, u8) {
        if self.r_shift == 16 && self.g_shift == 8 && self.b_shift == 0 {
            (
                ((px >> 16) & 0xFF) as u8,
                ((px >> 8) & 0xFF) as u8,
                (px & 0xFF) as u8,
            )
        } else {
            (
                ((px >> self.r_shift) & 0xFF) as u8,
                ((px >> self.g_shift) & 0xFF) as u8,
                ((px >> self.b_shift) & 0xFF) as u8,
            )
        }
    }

    #[inline(always)]
    pub fn blend_rgb24(self, dst_px: u32, fg_rgb: u32, alpha: u8) -> u32 {
        if alpha == 0 {
            return dst_px;
        }
        if alpha == 255 {
            return self.pack_rgb24(fg_rgb);
        }
        let a = alpha as u32;
        let inv = 255 - a;
        let fr = (fg_rgb >> 16) & 0xFF;
        let fg = (fg_rgb >> 8) & 0xFF;
        let fb = fg_rgb & 0xFF;
        let (br, bg, bb) = self.unpack(dst_px);
        let r = div255(fr * a + br as u32 * inv);
        let g = div255(fg * a + bg as u32 * inv);
        let b = div255(fb * a + bb as u32 * inv);
        self.pack(r, g, b)
    }
}

#[inline(always)]
pub const fn div255(x: u32) -> u8 {
    let t = x + 128;
    ((t + (t >> 8)) >> 8) as u8
}

/// Iterator decoding UTF-8 byte slices into `(start_byte, end_byte, char)` without allocation.
pub struct Utf8BytesIter<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Utf8BytesIter<'a> {
    #[inline]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
}

impl<'a> Iterator for Utf8BytesIter<'a> {
    type Item = (usize, usize, char);

    fn next(&mut self) -> Option<Self::Item> {
        let start = self.pos;
        let b0 = *self.bytes.get(start)?;
        if b0 < 0x80 {
            self.pos += 1;
            return Some((start, self.pos, b0 as char));
        }
        if (b0 & 0xE0) == 0xC0 {
            if let Some(&b1) = self.bytes.get(start + 1) {
                if (b1 & 0xC0) == 0x80 {
                    let cp = (((b0 & 0x1F) as u32) << 6) | ((b1 & 0x3F) as u32);
                    if let Some(ch) = char::from_u32(cp) {
                        self.pos += 2;
                        return Some((start, self.pos, ch));
                    }
                }
            }
        } else if (b0 & 0xF0) == 0xE0 {
            if let (Some(&b1), Some(&b2)) = (self.bytes.get(start + 1), self.bytes.get(start + 2)) {
                if (b1 & 0xC0) == 0x80 && (b2 & 0xC0) == 0x80 {
                    let cp = (((b0 & 0x0F) as u32) << 12)
                        | (((b1 & 0x3F) as u32) << 6)
                        | ((b2 & 0x3F) as u32);
                    if let Some(ch) = char::from_u32(cp) {
                        self.pos += 3;
                        return Some((start, self.pos, ch));
                    }
                }
            }
        } else if (b0 & 0xF8) == 0xF0 {
            if let (Some(&b1), Some(&b2), Some(&b3)) = (
                self.bytes.get(start + 1),
                self.bytes.get(start + 2),
                self.bytes.get(start + 3),
            ) {
                if (b1 & 0xC0) == 0x80 && (b2 & 0xC0) == 0x80 && (b3 & 0xC0) == 0x80 {
                    let cp = (((b0 & 0x07) as u32) << 18)
                        | (((b1 & 0x3F) as u32) << 12)
                        | (((b2 & 0x3F) as u32) << 6)
                        | ((b3 & 0x3F) as u32);
                    if let Some(ch) = char::from_u32(cp) {
                        self.pos += 4;
                        return Some((start, self.pos, ch));
                    }
                }
            }
        }
        self.pos += 1;
        Some((start, self.pos, '?'))
    }
}

pub fn measure_text(font: FontId, size_px: u8, text: &[u8]) -> i32 {
    let mut pen_fp = 0i32;
    for (_, _, ch) in Utf8BytesIter::new(text) {
        pen_fp += glyph_advance_24_8(font, size_px, ch);
    }
    (pen_fp + 128) >> 8
}

pub fn x_at_byte_index(font: FontId, size_px: u8, text: &[u8], byte_idx: usize) -> i32 {
    let target = byte_idx.min(text.len());
    let mut pen_fp = 0i32;
    for (s, _, ch) in Utf8BytesIter::new(text) {
        if s >= target {
            break;
        }
        pen_fp += glyph_advance_24_8(font, size_px, ch);
    }
    (pen_fp + 128) >> 8
}

pub fn byte_index_at_x(font: FontId, size_px: u8, text: &[u8], rel_x: i32) -> usize {
    if rel_x <= 0 {
        return 0;
    }
    let mut pen_fp = 0i32;
    let target_fp = rel_x << 8;
    for (s, e, ch) in Utf8BytesIter::new(text) {
        let adv = glyph_advance_24_8(font, size_px, ch);
        if target_fp < pen_fp + (adv >> 1) {
            return s;
        }
        pen_fp += adv;
        if target_fp < pen_fp {
            return e;
        }
    }
    text.len()
}

/// Blit one cached AA glyph bitmap into destination `u32` buffer clipped to `clip`.
/// # Safety
/// `buf` must be writable for `buf_w * buf_h` `u32` pixels.
#[allow(clippy::too_many_arguments)]
unsafe fn draw_char_baseline_raw(
    buf: *mut u32,
    buf_w: usize,
    buf_h: usize,
    fmt: PixelFormat,
    font: FontId,
    size_px: u8,
    pen_x_fp: i32,
    baseline_y: i32,
    ch: char,
    fg_rgb: u32,
    clip: Rect,
) -> i32 {
    with_glyph(font, size_px, ch, |g, cov| {
        if g.w > 0 && g.h > 0 && !clip.is_empty() {
            let pen_px = (pen_x_fp + 128) >> 8;
            let gx = pen_px + g.bear_x as i32;
            let gy = baseline_y - g.bear_y as i32;
            let gw = g.w as i32;
            let gh = g.h as i32;

            let c_x0 = (clip.x as i32).max(0);
            let c_y0 = (clip.y as i32).max(0);
            let c_x1 = ((clip.x + clip.w) as i32).min(buf_w as i32);
            let c_y1 = ((clip.y + clip.h) as i32).min(buf_h as i32);

            let ix0 = gx.max(c_x0);
            let iy0 = gy.max(c_y0);
            let ix1 = (gx + gw).min(c_x1);
            let iy1 = (gy + gh).min(c_y1);

            if ix0 < ix1 && iy0 < iy1 {
                let packed_fg = fmt.pack_rgb24(fg_rgb);
                for py in iy0..iy1 {
                    let sy = (py - gy) as usize;
                    let src_row = &cov[sy * g.w as usize..];
                    let dst_row = unsafe { buf.add(py as usize * buf_w) };
                    for px in ix0..ix1 {
                        let sx = (px - gx) as usize;
                        let a = src_row[sx];
                        if a == 255 {
                            unsafe { *dst_row.add(px as usize) = packed_fg };
                        } else if a > 0 {
                            let d = unsafe { *dst_row.add(px as usize) };
                            unsafe {
                                *dst_row.add(px as usize) = fmt.blend_rgb24(d, fg_rgb, a);
                            }
                        }
                    }
                }
            }
        }
        g.adv_24_8
    })
}

/// Draw UTF-8 byte string anchored by baseline Y. Returns total horizontal pixel advance.
/// # Safety
/// `buf` must cover `buf_w * buf_h` `u32` pixels.
#[allow(clippy::too_many_arguments)]
pub unsafe fn draw_text_baseline_raw(
    buf: *mut u32,
    buf_w: usize,
    buf_h: usize,
    fmt: PixelFormat,
    font: FontId,
    size_px: u8,
    x: i32,
    baseline_y: i32,
    text: &[u8],
    fg_rgb: u32,
    clip: Rect,
) -> i32 {
    if clip.is_empty() || text.is_empty() {
        return 0;
    }
    let mut pen_fp = x << 8;
    let clip_right_fp = ((clip.x + clip.w) as i32 + 4) << 8;
    for (_, _, ch) in Utf8BytesIter::new(text) {
        if pen_fp > clip_right_fp {
            break;
        }
        pen_fp += unsafe {
            draw_char_baseline_raw(
                buf, buf_w, buf_h, fmt, font, size_px, pen_fp, baseline_y, ch, fg_rgb, clip,
            )
        };
    }
    ((pen_fp - (x << 8)) + 128) >> 8
}

/// Draw UTF-8 byte string anchored by top-left Y (`baseline = y + metrics.ascent`).
/// # Safety
/// `buf` must cover `buf_w * buf_h` `u32` pixels.
#[allow(clippy::too_many_arguments)]
pub unsafe fn draw_text_top_raw(
    buf: *mut u32,
    buf_w: usize,
    buf_h: usize,
    fmt: PixelFormat,
    font: FontId,
    size_px: u8,
    x: i32,
    top_y: i32,
    text: &[u8],
    fg_rgb: u32,
    clip: Rect,
) -> i32 {
    let m = font_metrics(font, size_px);
    unsafe {
        draw_text_baseline_raw(
            buf,
            buf_w,
            buf_h,
            fmt,
            font,
            size_px,
            x,
            top_y + m.ascent,
            text,
            fg_rgb,
            clip,
        )
    }
}

/// Draw UTF-8 string truncated cleanly with `…` if wider than `max_w`.
/// # Safety
/// `buf` must cover `buf_w * buf_h` `u32` pixels.
#[allow(clippy::too_many_arguments)]
pub unsafe fn draw_text_ellipsis_raw(
    buf: *mut u32,
    buf_w: usize,
    buf_h: usize,
    fmt: PixelFormat,
    font: FontId,
    size_px: u8,
    x: i32,
    top_y: i32,
    max_w: i32,
    text: &[u8],
    fg_rgb: u32,
    clip: Rect,
) -> i32 {
    if max_w <= 0 || clip.is_empty() || text.is_empty() {
        return 0;
    }
    let full_w = measure_text(font, size_px, text);
    if full_w <= max_w {
        return unsafe {
            draw_text_top_raw(buf, buf_w, buf_h, fmt, font, size_px, x, top_y, text, fg_rgb, clip)
        };
    }

    let m = font_metrics(font, size_px);
    let baseline_y = top_y + m.ascent;
    let ell_adv = glyph_advance_24_8(font, size_px, '\u{2026}');
    let budget_fp = ((max_w << 8) - ell_adv).max(0);

    let mut pen_fp = 0i32;
    for (_, _, ch) in Utf8BytesIter::new(text) {
        let adv = glyph_advance_24_8(font, size_px, ch);
        if pen_fp + adv > budget_fp {
            break;
        }
        let drawn_adv = unsafe {
            draw_char_baseline_raw(
                buf,
                buf_w,
                buf_h,
                fmt,
                font,
                size_px,
                (x << 8) + pen_fp,
                baseline_y,
                ch,
                fg_rgb,
                clip,
            )
        };
        pen_fp += drawn_adv;
    }
    pen_fp += unsafe {
        draw_char_baseline_raw(
            buf,
            buf_w,
            buf_h,
            fmt,
            font,
            size_px,
            (x << 8) + pen_fp,
            baseline_y,
            '\u{2026}',
            fg_rgb,
            clip,
        )
    };
    (pen_fp + 128) >> 8
}

/// Draw UTF-8 string highlighting selected byte range `[sel_start..sel_end]` with `sel_bg_rgb`.
/// # Safety
/// `buf` must cover `buf_w * buf_h` `u32` pixels.
#[allow(clippy::too_many_arguments)]
pub unsafe fn draw_text_selected_raw(
    buf: *mut u32,
    buf_w: usize,
    buf_h: usize,
    fmt: PixelFormat,
    font: FontId,
    size_px: u8,
    x: i32,
    top_y: i32,
    text: &[u8],
    fg_rgb: u32,
    sel_start: usize,
    sel_end: usize,
    sel_bg_rgb: u32,
    sel_fg_rgb: u32,
    clip: Rect,
) {
    if clip.is_empty() {
        return;
    }
    let m = font_metrics(font, size_px);
    let baseline_y = top_y + m.ascent;
    let s0 = sel_start.min(sel_end).min(text.len());
    let s1 = sel_start.max(sel_end).min(text.len());

    if s0 < s1 {
        let sx0 = x + x_at_byte_index(font, size_px, text, s0);
        let sx1 = x + x_at_byte_index(font, size_px, text, s1);
        if sx1 > sx0 && top_y >= 0 {
            let sr = Rect::new(
                sx0.max(0) as usize,
                top_y as usize,
                (sx1 - sx0.max(0)).max(0) as usize,
                m.line_height.max(1) as usize,
            )
            .intersect(clip)
            .clamp_to(buf_w, buf_h);
            if !sr.is_empty() {
                let packed = fmt.pack_rgb24(sel_bg_rgb);
                for py in sr.y..(sr.y + sr.h) {
                    let row = py * buf_w;
                    for px in sr.x..(sr.x + sr.w) {
                        unsafe { *buf.add(row + px) = packed };
                    }
                }
            }
        }
    }

    let mut pen_fp = x << 8;
    for (bs, be, ch) in Utf8BytesIter::new(text) {
        let in_sel = bs >= s0 && be <= s1;
        let col = if in_sel { sel_fg_rgb } else { fg_rgb };
        pen_fp += unsafe {
            draw_char_baseline_raw(
                buf, buf_w, buf_h, fmt, font, size_px, pen_fp, baseline_y, ch, col, clip,
            )
        };
    }
}
