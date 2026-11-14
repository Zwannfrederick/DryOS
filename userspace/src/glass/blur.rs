// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Anti-Aliased Glass Backdrop Blur, Rounded AA Fill & Feathered Elevation Shadows.

use crate::abi_ext::UserFbInfo;
use crate::compositor::pack_rgb;
use crate::gfx::Rect;
use crate::glass::theme::{blend_packed, blend_rgb, unpack_rgb};
use crate::ui::canvas::rrect_cov;

const BLUR_R: usize = 4;
const TILE_W: usize = 64;
const TILE_H: usize = 32;
const PAD_W: usize = TILE_W + BLUR_R * 2;
const PAD_H: usize = TILE_H + BLUR_R * 2;

#[inline]
pub fn in_rounded_rect(r: Rect, corner_r: usize, px: usize, py: usize) -> bool {
    rrect_cov(r, corner_r, px, py) >= 128
}

#[inline]
fn blend_raw_32(dst: u32, src: u32, cov: u8) -> u32 {
    if cov == 0 {
        return dst;
    }
    if cov == 255 {
        return src;
    }
    let a = cov as u32;
    let inv = 255 - a;
    let b0 = (((dst & 0xff) * inv + (src & 0xff) * a) / 255) & 0xff;
    let b1 = ((((dst >> 8) & 0xff) * inv + ((src >> 8) & 0xff) * a) / 255) & 0xff;
    let b2 = ((((dst >> 16) & 0xff) * inv + ((src >> 16) & 0xff) * a) / 255) & 0xff;
    (b2 << 16) | (b1 << 8) | b0
}

#[inline]
fn apply_glass_rim(
    info: &UserFbInfo,
    bg_rgb: (u8, u8, u8),
    rect: Rect,
    px: usize,
    py: usize,
    rim_top: u32,
    rim_outer: u32,
) -> (u8, u8, u8) {
    let on_top = py == rect.y;
    let on_bot = py + 1 == rect.y + rect.h;
    let on_side = px == rect.x || px + 1 == rect.x + rect.w;
    if on_top {
        blend_rgb(bg_rgb, unpack_rgb(info, rim_top), 148)
    } else if on_bot || on_side {
        blend_rgb(bg_rgb, unpack_rgb(info, rim_outer), 110)
    } else {
        bg_rgb
    }
}

/// Anti-aliased packed rounded rectangle fill using `rrect_cov`.
///
/// # Safety
/// `back_ptr` must cover `fb_w * fb_h` 32-bit pixels.
pub unsafe fn fill_rounded_raw(
    back_ptr: *mut u32,
    fb_w: usize,
    fb_h: usize,
    r: Rect,
    clip: Rect,
    corner_r: usize,
    color: u32,
) {
    let ic = r.intersect(clip).clamp_to(fb_w, fb_h);
    if ic.is_empty() {
        return;
    }
    for py in ic.y..(ic.y + ic.h) {
        let row = py * fb_w;
        for px in ic.x..(ic.x + ic.w) {
            let cov = rrect_cov(r, corner_r, px, py);
            if cov != 0 {
                let p = back_ptr.add(row + px);
                *p = blend_raw_32(*p, color, cov);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn frosted_tile(
    back_ptr: *mut u32,
    fb_w: usize,
    fb_h: usize,
    info: &UserFbInfo,
    rect: Rect,
    tile: Rect,
    tint_rgb: (u8, u8, u8),
    alpha: u8,
    corner_r: usize,
    rim_top: u32,
    rim_outer: u32,
) {
    let min_x = tile.x.saturating_sub(BLUR_R);
    let max_x = (tile.x + tile.w + BLUR_R).min(fb_w);
    let min_y = tile.y.saturating_sub(BLUR_R);
    let max_y = (tile.y + tile.h + BLUR_R).min(fb_h);
    let pw = max_x - min_x;
    let ph = max_y - min_y;
    if pw == 0 || ph == 0 || pw > PAD_W || ph > PAD_H {
        return;
    }

    let mut horiz = [(0u8, 0u8, 0u8); PAD_W * PAD_H];
    let kx = BLUR_R as isize;
    for ly in 0..ph {
        let py = min_y + ly;
        let row_ptr = back_ptr.add(py * fb_w);
        for lx in 0..pw {
            let px = min_x + lx;
            let (mut sr, mut sg, mut sb, mut cnt) = (0u32, 0u32, 0u32, 0u32);
            let mut dx = -kx;
            while dx <= kx {
                let sx = (px as isize + dx).clamp(0, fb_w as isize - 1) as usize;
                let (r, g, b) = unpack_rgb(info, *row_ptr.add(sx));
                sr += r as u32;
                sg += g as u32;
                sb += b as u32;
                cnt += 1;
                dx += 1;
            }
            horiz[ly * pw + lx] = ((sr / cnt) as u8, (sg / cnt) as u8, (sb / cnt) as u8);
        }
    }

    let ox = tile.x - min_x;
    let oy = tile.y - min_y;
    for ty in 0..tile.h {
        let py = tile.y + ty;
        let ly = oy + ty;
        let out_row = back_ptr.add(py * fb_w);
        for tx in 0..tile.w {
            let px = tile.x + tx;
            let cov = rrect_cov(rect, corner_r, px, py);
            if cov == 0 {
                continue;
            }
            let lx = ox + tx;
            let (mut sr, mut sg, mut sb, mut cnt) = (0u32, 0u32, 0u32, 0u32);
            let mut dy = -kx;
            while dy <= kx {
                let sy = (ly as isize + dy).clamp(0, ph as isize - 1) as usize;
                let c = horiz[sy * pw + lx];
                sr += c.0 as u32;
                sg += c.1 as u32;
                sb += c.2 as u32;
                cnt += 1;
                dy += 1;
            }
            let blurred = ((sr / cnt) as u8, (sg / cnt) as u8, (sb / cnt) as u8);
            let tinted = blend_rgb(blurred, tint_rgb, alpha);
            let rimmed = apply_glass_rim(info, tinted, rect, px, py, rim_top, rim_outer);
            let dst = out_row.add(px);
            if cov == 255 {
                *dst = pack_rgb(info, rimmed.0, rimmed.1, rimmed.2);
            } else {
                *dst = blend_packed(info, *dst, rimmed, cov);
            }
        }
    }
}

/// 2-pass separable frosted blur with anti-aliased rounded corners & subtle inner highlight rim.
///
/// # Safety
/// `back_ptr` must cover `fb_w * fb_h` 32-bit pixels.
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
    let ic = rect.intersect(clip).clamp_to(fb_w, fb_h);
    if ic.is_empty() {
        return;
    }
    if !blur_enabled {
        for py in ic.y..(ic.y + ic.h) {
            let row = py * fb_w;
            for px in ic.x..(ic.x + ic.w) {
                let cov = rrect_cov(rect, corner_r, px, py);
                if cov == 0 {
                    continue;
                }
                let p = back_ptr.add(row + px);
                let orig = unpack_rgb(info, *p);
                let base = blend_rgb(orig, tint_rgb, alpha);
                let rimmed = apply_glass_rim(info, base, rect, px, py, rim_top, rim_outer);
                *p = if cov == 255 {
                    pack_rgb(info, rimmed.0, rimmed.1, rimmed.2)
                } else {
                    blend_packed(info, *p, rimmed, cov)
                };
            }
        }
        return;
    }

    let mut y = ic.y;
    let y_end = ic.y + ic.h;
    let x_end = ic.x + ic.w;
    while y < y_end {
        let th = (y_end - y).min(TILE_H);
        let mut x = ic.x;
        while x < x_end {
            let tw = (x_end - x).min(TILE_W);
            frosted_tile(
                back_ptr,
                fb_w,
                fb_h,
                info,
                rect,
                Rect::new(x, y, tw, th),
                tint_rgb,
                alpha,
                corner_r,
                rim_top,
                rim_outer,
            );
            x += tw;
        }
        y += th;
    }
}

/// Soft alpha-blended shadow rectangle.
///
/// # Safety
/// `back_ptr` must cover `fb_w * fb_h` 32-bit pixels.
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
    let ic = r.intersect(clip).clamp_to(fb_w, fb_h);
    if ic.is_empty() || alpha == 0 {
        return;
    }
    for py in ic.y..(ic.y + ic.h) {
        let row = py * fb_w;
        for px in ic.x..(ic.x + ic.w) {
            let p = back_ptr.add(row + px);
            *p = blend_packed(info, *p, shadow_rgb, alpha);
        }
    }
}

/// Feathered rounded-rectangle drop shadow (skips interior pixels of `r`).
///
/// # Safety
/// `back_ptr` must cover `fb_w * fb_h` 32-bit pixels.
#[allow(clippy::too_many_arguments)]
pub unsafe fn blend_soft_rrect_shadow(
    back_ptr: *mut u32,
    fb_w: usize,
    fb_h: usize,
    info: &UserFbInfo,
    r: Rect,
    clip: Rect,
    corner_r: usize,
    shadow_rgb: (u8, u8, u8),
    max_alpha: u8,
    spread: usize,
) {
    if spread == 0 || max_alpha == 0 {
        return;
    }
    let y_off = (spread / 3).max(1);
    let halo = Rect::new(
        r.x.saturating_sub(spread),
        r.y.saturating_sub(spread.saturating_sub(y_off)),
        r.w + spread * 2,
        r.h + spread * 2 + y_off,
    );
    let ic = halo.intersect(clip).clamp_to(fb_w, fb_h);
    if ic.is_empty() {
        return;
    }
    let layers = spread.clamp(2, 6);
    for layer in 0..layers {
        let pad = ((layers - layer) * spread) / layers;
        let lr = Rect::new(
            r.x.saturating_sub(pad),
            r.y.saturating_sub(pad) + y_off,
            r.w + pad * 2,
            r.h + pad * 2,
        );
        let lcov_r = corner_r + pad;
        let alpha = ((max_alpha as usize * (layer + 1)) / (layers * layers)).max(2) as u8;
        let lic = lr.intersect(ic);
        for py in lic.y..(lic.y + lic.h) {
            let row = py * fb_w;
            for px in lic.x..(lic.x + lic.w) {
                if rrect_cov(r, corner_r, px, py) >= 240 {
                    continue;
                }
                let cov = rrect_cov(lr, lcov_r, px, py);
                if cov != 0 {
                    let a = ((alpha as u16 * cov as u16) / 255) as u8;
                    if a != 0 {
                        let p = back_ptr.add(row + px);
                        *p = blend_packed(info, *p, shadow_rgb, a);
                    }
                }
            }
        }
    }
}
