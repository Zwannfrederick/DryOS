// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Pure-integer (`24.8` fixed-point) 256-level analytical grayscale TrueType rasterizer.
//!
//! All scratch state lives in caller-owned `.bss` (`RasterScratch`), keeping Ring-3 stack frames
//! well under 1 KiB.

use super::sfnt::{FontFace, OutlineBuf, OutlinePt};

pub const MAX_RASTER_W: usize = 96;
pub const MAX_RASTER_H: usize = 96;
pub const MAX_RASTER_PIXELS: usize = MAX_RASTER_W * MAX_RASTER_H;
pub const MAX_EDGES: usize = 2048;

#[derive(Clone, Copy, Default)]
pub struct Edge {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub dir: i8,
}

#[derive(Clone, Copy, Default)]
pub struct RasterMetrics {
    pub w: u8,
    pub h: u8,
    pub bear_x: i8,
    pub bear_y: i8,
    pub adv_24_8: i32,
}

pub struct RasterScratch {
    pub outline: OutlineBuf,
    pub edges: [Edge; MAX_EDGES],
    pub n_edges: usize,
    pub row_delta: [i32; MAX_RASTER_W + 2],
    pub cov: [u8; MAX_RASTER_PIXELS],
}

impl RasterScratch {
    pub const fn new() -> Self {
        Self {
            outline: OutlineBuf::new(),
            edges: [Edge {
                x0: 0,
                y0: 0,
                x1: 0,
                y1: 0,
                dir: 0,
            }; MAX_EDGES],
            n_edges: 0,
            row_delta: [0; MAX_RASTER_W + 2],
            cov: [0; MAX_RASTER_PIXELS],
        }
    }

    #[inline]
    fn push_edge(&mut self, p0: (i32, i32), p1: (i32, i32)) {
        if p0.1 == p1.1 || self.n_edges >= MAX_EDGES {
            return;
        }
        let (x0, y0, x1, y1, dir) = if p0.1 < p1.1 {
            (p0.0, p0.1, p1.0, p1.1, 1i8)
        } else {
            (p1.0, p1.1, p0.0, p0.1, -1i8)
        };
        self.edges[self.n_edges] = Edge { x0, y0, x1, y1, dir };
        self.n_edges += 1;
    }

    fn flatten_quad(&mut self, p0: (i32, i32), c: (i32, i32), p2: (i32, i32)) {
        let err = (p0.0 + p2.0 - 2 * c.0).abs() + (p0.1 + p2.1 - 2 * c.1).abs();
        if err <= 36 {
            self.push_edge(p0, p2);
            return;
        }
        let steps: i32 = if err <= 144 {
            2
        } else if err <= 576 {
            4
        } else if err <= 2304 {
            8
        } else {
            14
        };
        let den = (steps * steps) as i64;
        let half = den >> 1;
        let mut prev = p0;
        for k in 1..steps {
            let t1 = steps - k;
            let w0 = (t1 * t1) as i64;
            let w1 = (2 * k * t1) as i64;
            let w2 = (k * k) as i64;
            let nx = ((w0 * p0.0 as i64 + w1 * c.0 as i64 + w2 * p2.0 as i64 + half) / den) as i32;
            let ny = ((w0 * p0.1 as i64 + w1 * c.1 as i64 + w2 * p2.1 as i64 + half) / den) as i32;
            self.push_edge(prev, (nx, ny));
            prev = (nx, ny);
        }
        self.push_edge(prev, p2);
    }
}

impl Default for RasterScratch {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(clippy::too_many_arguments)]
fn accumulate_trapezoid(
    row_delta: &mut [i32; MAX_RASTER_W + 2],
    gw: usize,
    xa: i32,
    ya: i32,
    xb: i32,
    yb: i32,
    dir: i32,
) {
    let dy_total = yb - ya;
    if dy_total <= 0 {
        return;
    }
    let dx_total = xb - xa;
    if dx_total == 0 {
        if xa <= 0 {
            row_delta[0] += dir * dy_total * 512;
        } else {
            let col = (xa >> 8) as usize;
            if col < gw {
                let xf = xa & 255;
                row_delta[col] += dir * dy_total * (512 - 2 * xf);
                row_delta[col + 1] += dir * dy_total * (2 * xf);
            }
        }
        return;
    }

    let mut x_cur = xa;
    let mut y_cur = ya;
    let max_steps = gw + 8;
    for _ in 0..max_steps {
        if y_cur >= yb {
            break;
        }
        let col: i32;
        let x_next: i32;
        let y_next: i32;
        if dx_total > 0 {
            if x_cur < 0 {
                col = -1;
                if xb <= 0 {
                    x_next = xb;
                    y_next = yb;
                } else {
                    x_next = 0;
                    y_next = ya + (((-xa) as i64 * dy_total as i64) / dx_total as i64) as i32;
                }
            } else {
                col = x_cur >> 8;
                let xb_edge = (col + 1) << 8;
                if xb > xb_edge && (col as usize) < gw {
                    x_next = xb_edge;
                    y_next = ya + (((xb_edge - xa) as i64 * dy_total as i64) / dx_total as i64) as i32;
                } else {
                    x_next = xb;
                    y_next = yb;
                }
            }
        } else if x_cur <= 0 {
            col = -1;
            x_next = xb;
            y_next = yb;
        } else {
            col = (x_cur - 1) >> 8;
            let xb_edge = col << 8;
            if xb < xb_edge {
                x_next = xb_edge;
                y_next = ya + (((xb_edge - xa) as i64 * dy_total as i64) / dx_total as i64) as i32;
            } else {
                x_next = xb;
                y_next = yb;
            }
        }

        let yn = y_next.clamp(y_cur, yb);
        let seg_dy = yn - y_cur;
        if seg_dy > 0 {
            if col < 0 {
                row_delta[0] += dir * seg_dy * 512;
            } else if (col as usize) < gw {
                let c = col as usize;
                let base = col << 8;
                let x0f = x_cur - base;
                let x1f = x_next - base;
                let sum = x0f + x1f;
                row_delta[c] += dir * seg_dy * (512 - sum);
                row_delta[c + 1] += dir * seg_dy * sum;
            }
        }
        x_cur = x_next;
        y_cur = yn;
    }
}

#[inline(always)]
fn pt_to_local(p: OutlinePt, scale_fp: i64, upm: i64, ox_fp: i64, oy_fp: i64) -> (i32, i32, bool) {
    let lx = ((p.x as i64 * scale_fp) / upm - ox_fp) as i32;
    let ly = ((-p.y as i64 * scale_fp) / upm - oy_fp) as i32;
    (lx, ly, p.on_curve)
}

pub fn rasterize_glyph(
    face: &FontFace,
    gid: u16,
    size_px: u8,
    sc: &mut RasterScratch,
) -> RasterMetrics {
    let upm = face.units_per_em.max(1) as i64;
    let (adv_fu, _) = face.hmetric(gid);
    let adv_24_8 = ((adv_fu as i64 * (size_px as i64 * 256) + (upm >> 1)) / upm) as i32;

    if !face.load_glyph(gid, &mut sc.outline) || sc.outline.n_pts == 0 {
        return RasterMetrics {
            w: 0,
            h: 0,
            bear_x: 0,
            bear_y: 0,
            adv_24_8,
        };
    }

    let scale_fp = size_px as i64 * 256;
    let xmin_fp = (sc.outline.xmin as i64 * scale_fp) / upm;
    let xmax_fp = (sc.outline.xmax as i64 * scale_fp) / upm;
    let ytop_fp = (-sc.outline.ymax as i64 * scale_fp) / upm;
    let ybot_fp = (-sc.outline.ymin as i64 * scale_fp) / upm;

    let px_left = ((xmin_fp >> 8) as i32) - 1;
    let px_right = (((xmax_fp + 255) >> 8) as i32) + 1;
    let py_top = ((ytop_fp >> 8) as i32) - 1;
    let py_bot = (((ybot_fp + 255) >> 8) as i32) + 1;

    let gw = (px_right - px_left).clamp(1, MAX_RASTER_W as i32) as usize;
    let gh = (py_bot - py_top).clamp(1, MAX_RASTER_H as i32) as usize;
    let bear_x = px_left.clamp(-127, 127) as i8;
    let bear_y = (-py_top).clamp(-127, 127) as i8;

    let ox_fp = px_left as i64 * 256;
    let oy_fp = py_top as i64 * 256;

    sc.n_edges = 0;
    let mut c_start = 0usize;
    for ci in 0..sc.outline.n_conts {
        let c_end = sc.outline.cont_ends[ci] as usize;
        if c_end >= sc.outline.n_pts || c_end < c_start {
            break;
        }
        let c_len = c_end - c_start + 1;
        if c_len >= 2 {
            let first = pt_to_local(sc.outline.pts[c_start], scale_fp, upm, ox_fp, oy_fp);
            let last = pt_to_local(sc.outline.pts[c_end], scale_fp, upm, ox_fp, oy_fp);
            let (start_pos, walk_from, close_to_start): ((i32, i32), usize, bool) = if first.2 {
                ((first.0, first.1), 1, true)
            } else if last.2 {
                ((last.0, last.1), 0, false)
            } else {
                (((last.0 + first.0) >> 1, (last.1 + first.1) >> 1), 0, true)
            };

            let mut cur = start_pos;
            let mut pending: Option<(i32, i32)> = None;
            for k in walk_from..c_len {
                let p = pt_to_local(sc.outline.pts[c_start + k], scale_fp, upm, ox_fp, oy_fp);
                if p.2 {
                    if let Some(ctrl) = pending.take() {
                        sc.flatten_quad(cur, ctrl, (p.0, p.1));
                    } else {
                        sc.push_edge(cur, (p.0, p.1));
                    }
                    cur = (p.0, p.1);
                } else {
                    if let Some(ctrl) = pending.take() {
                        let mid = ((ctrl.0 + p.0) >> 1, (ctrl.1 + p.1) >> 1);
                        sc.flatten_quad(cur, ctrl, mid);
                        cur = mid;
                    }
                    pending = Some((p.0, p.1));
                }
            }
            if close_to_start {
                if let Some(ctrl) = pending.take() {
                    sc.flatten_quad(cur, ctrl, start_pos);
                } else {
                    sc.push_edge(cur, start_pos);
                }
            }
        }
        c_start = c_end + 1;
    }

    for sy in 0..gh {
        sc.row_delta[..=gw].fill(0);
        let y_top = (sy as i32) << 8;
        let y_bot = ((sy + 1) as i32) << 8;

        for ei in 0..sc.n_edges {
            let e = sc.edges[ei];
            if e.y1 <= y_top || e.y0 >= y_bot {
                continue;
            }
            let ya = e.y0.max(y_top);
            let yb = e.y1.min(y_bot);
            if ya >= yb {
                continue;
            }
            let edy = (e.y1 - e.y0) as i64;
            let edx = (e.x1 - e.x0) as i64;
            let xa = e.x0 + ((edx * (ya - e.y0) as i64) / edy) as i32;
            let xb = e.x0 + ((edx * (yb - e.y0) as i64) / edy) as i32;
            accumulate_trapezoid(&mut sc.row_delta, gw, xa, ya, xb, yb, e.dir as i32);
        }

        let mut acc = 0i32;
        let row_off = sy * gw;
        for px in 0..gw {
            acc += sc.row_delta[px];
            let area = acc.unsigned_abs().min(131_072);
            let cov = ((area * 255 + 65_536) >> 17) as u8;
            sc.cov[row_off + px] = cov;
        }
    }

    RasterMetrics {
        w: gw as u8,
        h: gh as u8,
        bear_x,
        bear_y,
        adv_24_8,
    }
}
