// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Clipped 1x (`8x16`) & Edge-Smoothed 2x (`16x32`) Display Typography Renderers.

use crate::gfx::{glyph_8x16, Rect};

#[inline(always)]
fn bit_at(g: &[u8; 16], cx: usize, cy: usize) -> bool {
    (g[cy] & (0x80 >> cx)) != 0
}

/// Render an `8x16` glyph onto a `u32` buffer clipped to `clip`.
/// # Safety
/// `buf` must cover `buf_w * buf_h` `u32` pixels.
#[allow(clippy::too_many_arguments)]
pub unsafe fn draw_glyph_raw(
    buf: *mut u32,
    buf_w: usize,
    buf_h: usize,
    gx: usize,
    gy: usize,
    ch: u8,
    fg: u32,
    bg: Option<u32>,
    clip: Rect,
) {
    let inter = Rect::new(gx, gy, 8, 16).intersect(clip).clamp_to(buf_w, buf_h);
    if inter.is_empty() {
        return;
    }
    let g = glyph_8x16(ch);
    for py in inter.y..(inter.y + inter.h) {
        let bits = g[py - gy];
        let row_off = py * buf_w;
        for px in inter.x..(inter.x + inter.w) {
            let cx = px - gx;
            if (bits & (0x80 >> cx)) != 0 {
                unsafe { *buf.add(row_off + px) = fg };
            } else if let Some(bcol) = bg {
                unsafe { *buf.add(row_off + px) = bcol };
            }
        }
    }
}

/// Render an ASCII string at 1x (`8x16`) onto a `u32` buffer clipped to `clip`.
/// # Safety
/// `buf` must cover `buf_w * buf_h` `u32` pixels.
#[allow(clippy::too_many_arguments)]
pub unsafe fn draw_str_raw(
    buf: *mut u32,
    buf_w: usize,
    buf_h: usize,
    x: usize,
    y: usize,
    text: &[u8],
    fg: u32,
    bg: Option<u32>,
    clip: Rect,
) {
    for (i, &ch) in text.iter().enumerate() {
        unsafe { draw_glyph_raw(buf, buf_w, buf_h, x + i * 8, y, ch, fg, bg, clip) };
    }
}

/// Render an ASCII heading at `2x` (`16x32`) with diagonal Scale2x-style corner smoothing so
/// display headers & calculator digits avoid blocky stair-stepping.
/// # Safety
/// `buf` must cover `buf_w * buf_h` `u32` pixels.
#[allow(clippy::too_many_arguments)]
pub unsafe fn draw_str_2x_raw(
    buf: *mut u32,
    buf_w: usize,
    buf_h: usize,
    x: usize,
    y: usize,
    text: &[u8],
    fg: u32,
    clip: Rect,
) {
    for (i, &ch) in text.iter().enumerate() {
        let gx = x + i * 16;
        let inter = Rect::new(gx, y, 16, 32).intersect(clip).clamp_to(buf_w, buf_h);
        if inter.is_empty() {
            continue;
        }
        let g = glyph_8x16(ch);
        for sy in 0..16usize {
            for sx in 0..8usize {
                let c = bit_at(&g, sx, sy);
                let u = sy > 0 && bit_at(&g, sx, sy - 1);
                let d = sy < 15 && bit_at(&g, sx, sy + 1);
                let l = sx > 0 && bit_at(&g, sx - 1, sy);
                let r = sx < 7 && bit_at(&g, sx + 1, sy);
                // Diagonal bridge check for cleaner oblique strokes (`/`, `\`, curves).
                let tl = c || (u && l && !(u && r));
                let tr = c || (u && r && !(u && l));
                let bl = c || (d && l && !(d && r));
                let br = c || (d && r && !(d && l));
                let px_base = gx + sx * 2;
                let py_base = y + sy * 2;
                let pts = [(0usize, 0usize, tl), (1, 0, tr), (0, 1, bl), (1, 1, br)];
                for &(ox, oy, on) in &pts {
                    if on && inter.contains(px_base + ox, py_base + oy) {
                        unsafe { *buf.add((py_base + oy) * buf_w + px_base + ox) = fg };
                    }
                }
            }
        }
    }
}
