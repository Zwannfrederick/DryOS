// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Pure Integer Separable Backdrop Blur, Rounded Corners & Specular Glass Effects.
//!
//! Uses a `[1, 4, 6, 4, 1] / 256` 2D Binomial/Gaussian-approximation separable filter via a
//! 5-row stack ring buffer (`< 2.5 KiB` stack, zero heap, zero SSE/floating point) plus an
//! optional low-overhead single-pass translucent mode.

use crate::compositor::pack_rgb;
use crate::gfx::Rect;
use crate::glass::theme::{blend_packed, blend_rgb, unpack_rgb};
use crate::UserFbInfo;

const STRIP_W: usize = 128;

#[inline(always)]
pub fn in_rounded_rect(r: Rect, corner_r: usize, px: usize, py: usize) -> bool {
    if corner_r == 0 || r.w < corner_r * 2 || r.h < corner_r * 2 {
        return true;
    }
    let rx = px - r.x;
    let ry = py - r.y;
    let dx = if rx < corner_r {
        corner_r - rx
    } else if rx >= r.w - corner_r {
        rx - (r.w - 1 - corner_r)
    } else {
        return true;
    };
    let dy = if ry < corner_r {
        corner_r - ry
    } else if ry >= r.h - corner_r {
        ry - (r.h - 1 - corner_r)
    } else {
        return true;
    };
    dx * dx + dy * dy <= corner_r * corner_r + 1
}

/// Draw a solid rounded rectangle clipped to `clip`.
/// # Safety
/// `back_ptr` must cover `fb_w * fb_h` `u32` pixels in mapped WB RAM.
pub unsafe fn fill_rounded_raw(
    back_ptr: *mut u32,
    fb_w: usize,
    fb_h: usize,
    r: Rect,
    clip: Rect,
    corner_r: usize,
    color: u32,
) {
    let inter = r.intersect(clip).clamp_to(fb_w, fb_h);
    if inter.is_empty() {
        return;
    }
    for py in inter.y..(inter.y + inter.h) {
        let row_off = py * fb_w;
        for px in inter.x..(inter.x + inter.w) {
            if in_rounded_rect(r, corner_r, px, py) {
                unsafe { *back_ptr.add(row_off + px) = color };
            }
        }
    }
}

/// Apply frosted backdrop blur + translucent tint + top specular glass rim over `rect`.
/// # Safety
/// `back_ptr` must cover `fb_w * fb_h` `u32` pixels in mapped WB RAM.
#[allow(clippy::too_many_arguments)]
pub unsafe fn backdrop_frosted_rect(
    back_ptr: *mut u32,
    fb_w: usize,
    fb_h: usize,
    info: &UserFbInfo,
    rect: Rect,
    clip: Rect,
    tint_rgb: (u8, u8, u8),
    alpha: u8,
    blur_enabled: bool,
    corner_r: usize,
    rim_top: u32,
    rim_outer: u32,
) {
    let inter = rect.intersect(clip).clamp_to(fb_w, fb_h);
    if inter.is_empty() {
        return;
    }

    if !blur_enabled {
        for py in inter.y..(inter.y + inter.h) {
            let row_off = py * fb_w;
            for px in inter.x..(inter.x + inter.w) {
                if !in_rounded_rect(rect, corner_r, px, py) {
                    continue;
                }
                let p = unsafe { back_ptr.add(row_off + px) };
                let blended = blend_packed(info, unsafe { *p }, tint_rgb, alpha);
                unsafe { *p = blended };
            }
        }
    } else {
        // Horizontal `[1, 4, 6, 4, 1]` sum (`0..=4080` fits in `u16`), sliding 5-row vertical ring.
        let mut x0 = inter.x;
        while x0 < inter.x + inter.w {
            let chunk_w = (inter.x + inter.w - x0).min(STRIP_W);
            let mut ring = [[(0u16, 0u16, 0u16); STRIP_W]; 5];

            let fill_hrow = |src_y: usize, slot: &mut [(u16, u16, u16); STRIP_W]| {
                let sy = src_y.min(fb_h.saturating_sub(1));
                let row_ptr = unsafe { back_ptr.add(sy * fb_w) };
                for (cx, cell) in slot.iter_mut().enumerate().take(chunk_w) {
                    let gx = x0 + cx;
                    let p0 = unpack_rgb(info, unsafe { *row_ptr.add(gx.saturating_sub(2)) });
                    let p1 = unpack_rgb(info, unsafe { *row_ptr.add(gx.saturating_sub(1)) });
                    let p2 = unpack_rgb(info, unsafe { *row_ptr.add(gx) });
                    let p3 = unpack_rgb(info, unsafe { *row_ptr.add((gx + 1).min(fb_w - 1)) });
                    let p4 = unpack_rgb(info, unsafe { *row_ptr.add((gx + 2).min(fb_w - 1)) });
                    let sr = p0.0 as u16 + p1.0 as u16 * 4 + p2.0 as u16 * 6 + p3.0 as u16 * 4 + p4.0 as u16;
                    let sg = p0.1 as u16 + p1.1 as u16 * 4 + p2.1 as u16 * 6 + p3.1 as u16 * 4 + p4.1 as u16;
                    let sb = p0.2 as u16 + p1.2 as u16 * 4 + p2.2 as u16 * 6 + p3.2 as u16 * 4 + p4.2 as u16;
                    *cell = (sr, sg, sb);
                }
            };

            let y_start = inter.y;
            fill_hrow(y_start.saturating_sub(2), &mut ring[0]);
            fill_hrow(y_start.saturating_sub(1), &mut ring[1]);
            fill_hrow(y_start, &mut ring[2]);
            fill_hrow((y_start + 1).min(fb_h - 1), &mut ring[3]);

            for (idx, py) in (inter.y..(inter.y + inter.h)).enumerate() {
                let ahead_y = (py + 2).min(fb_h - 1);
                fill_hrow(ahead_y, &mut ring[(idx + 4) % 5]);

                let r0 = &ring[idx % 5];
                let r1 = &ring[(idx + 1) % 5];
                let r2 = &ring[(idx + 2) % 5];
                let r3 = &ring[(idx + 3) % 5];
                let r4 = &ring[(idx + 4) % 5];
                let row_off = py * fb_w;

                for cx in 0..chunk_w {
                    let gx = x0 + cx;
                    if !in_rounded_rect(rect, corner_r, gx, py) {
                        continue;
                    }
                    let vr = ((r0[cx].0 as u32)
                        + (r1[cx].0 as u32) * 4
                        + (r2[cx].0 as u32) * 6
                        + (r3[cx].0 as u32) * 4
                        + (r4[cx].0 as u32)
                        + 128)
                        >> 8;
                    let vg = ((r0[cx].1 as u32)
                        + (r1[cx].1 as u32) * 4
                        + (r2[cx].1 as u32) * 6
                        + (r3[cx].1 as u32) * 4
                        + (r4[cx].1 as u32)
                        + 128)
                        >> 8;
                    let vb = ((r0[cx].2 as u32)
                        + (r1[cx].2 as u32) * 4
                        + (r2[cx].2 as u32) * 6
                        + (r3[cx].2 as u32) * 4
                        + (r4[cx].2 as u32)
                        + 128)
                        >> 8;
                    let blurred = (vr.min(255) as u8, vg.min(255) as u8, vb.min(255) as u8);
                    let tinted = blend_rgb(blurred, tint_rgb, alpha);
                    unsafe {
                        *back_ptr.add(row_off + gx) = pack_rgb(info, tinted.0, tinted.1, tinted.2);
                    }
                }
            }
            x0 += chunk_w;
        }
    }

    // Draw crisp 1px outer glass rim and brighter top specular sheen.
    for py in inter.y..(inter.y + inter.h) {
        let on_top = py == rect.y;
        let on_bot = py + 1 == rect.y + rect.h;
        let row_off = py * fb_w;
        for px in inter.x..(inter.x + inter.w) {
            if !in_rounded_rect(rect, corner_r, px, py) {
                continue;
            }
            let on_edge = on_top || on_bot || px == rect.x || px + 1 == rect.x + rect.w;
            let corner_adj = corner_r > 0
                && (!in_rounded_rect(rect, corner_r, px.saturating_sub(1), py)
                    || !in_rounded_rect(rect, corner_r, px + 1, py)
                    || !in_rounded_rect(rect, corner_r, px, py.saturating_sub(1))
                    || !in_rounded_rect(rect, corner_r, px, py + 1));
            if on_edge || corner_adj {
                let col = if on_top { rim_top } else { rim_outer };
                unsafe { *back_ptr.add(row_off + px) = col };
            }
        }
    }
}

/// Alpha-blend a rectangular shadow band onto `back_ptr`.
/// # Safety
/// `back_ptr` must cover `fb_w * fb_h` pixels.
#[allow(clippy::too_many_arguments)]
pub unsafe fn blend_shadow_rect(
    back_ptr: *mut u32,
    fb_w: usize,
    fb_h: usize,
    info: &UserFbInfo,
    r: Rect,
    clip: Rect,
    shadow_rgb: (u8, u8, u8),
    alpha: u8,
) {
    let inter = r.intersect(clip).clamp_to(fb_w, fb_h);
    if inter.is_empty() {
        return;
    }
    for py in inter.y..(inter.y + inter.h) {
        let row_off = py * fb_w;
        for px in inter.x..(inter.x + inter.w) {
            let p = unsafe { back_ptr.add(row_off + px) };
            let out = blend_packed(info, unsafe { *p }, shadow_rgb, alpha);
            unsafe { *p = out };
        }
    }
}
