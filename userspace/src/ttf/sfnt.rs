// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Minimal `#![no_std]`, zero-heap SFNT/TrueType parser (`head`, `hhea`, `maxp`, `hmtx`, `cmap`,
//! `loca`, `glyf`) with simple + compound glyph outline extraction.

pub const MAX_OUTLINE_POINTS: usize = 512;
pub const MAX_OUTLINE_CONTOURS: usize = 96;

#[derive(Clone, Copy, Default)]
pub struct OutlinePt {
    pub x: i32,
    pub y: i32,
    pub on_curve: bool,
}

pub struct OutlineBuf {
    pub pts: [OutlinePt; MAX_OUTLINE_POINTS],
    pub cont_ends: [u16; MAX_OUTLINE_CONTOURS],
    pub n_pts: usize,
    pub n_conts: usize,
    pub xmin: i32,
    pub ymin: i32,
    pub xmax: i32,
    pub ymax: i32,
}

impl OutlineBuf {
    pub const fn new() -> Self {
        Self {
            pts: [OutlinePt { x: 0, y: 0, on_curve: false }; MAX_OUTLINE_POINTS],
            cont_ends: [0; MAX_OUTLINE_CONTOURS],
            n_pts: 0,
            n_conts: 0,
            xmin: 0,
            ymin: 0,
            xmax: 0,
            ymax: 0,
        }
    }

    pub fn clear(&mut self) {
        self.n_pts = 0;
        self.n_conts = 0;
        self.xmin = i32::MAX;
        self.ymin = i32::MAX;
        self.xmax = i32::MIN;
        self.ymax = i32::MIN;
    }
}

impl Default for OutlineBuf {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
pub struct FontFace {
    data: &'static [u8],
    pub units_per_em: u16,
    pub ascent: i16,
    pub descent: i16,
    pub line_gap: i16,
    index_to_loc_format: i16,
    pub num_glyphs: u16,
    num_hmetrics: u16,
    cmap_sub_off: usize,
    cmap_format: u16,
    loca_off: usize,
    loca_len: usize,
    glyf_off: usize,
    glyf_len: usize,
    hmtx_off: usize,
    hmtx_len: usize,
}

#[inline]
fn u16_be(b: &[u8], off: usize) -> Option<u16> {
    let s = b.get(off..off.checked_add(2)?)?;
    Some(u16::from_be_bytes([s[0], s[1]]))
}

#[inline]
fn i16_be(b: &[u8], off: usize) -> Option<i16> {
    Some(u16_be(b, off)? as i16)
}

#[inline]
fn u32_be(b: &[u8], off: usize) -> Option<u32> {
    let s = b.get(off..off.checked_add(4)?)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn find_table(data: &[u8], tag: &[u8; 4]) -> Option<(usize, usize)> {
    let num_tables = u16_be(data, 4)? as usize;
    for i in 0..num_tables.min(64) {
        let rec = 12 + i * 16;
        let t = data.get(rec..rec + 4)?;
        if t == tag {
            let off = u32_be(data, rec + 8)? as usize;
            let len = u32_be(data, rec + 12)? as usize;
            if off.checked_add(len)? <= data.len() {
                return Some((off, len));
            }
        }
    }
    None
}

fn pick_cmap(data: &[u8], cmap_off: usize, cmap_len: usize) -> Option<(usize, u16)> {
    let sub = data.get(cmap_off..cmap_off + cmap_len)?;
    let n = u16_be(sub, 2)? as usize;
    let mut best_f4: Option<(usize, u16)> = None;
    for i in 0..n.min(32) {
        let rec = 4 + i * 8;
        let plat = u16_be(sub, rec)?;
        let enc = u16_be(sub, rec + 2)?;
        let rel = u32_be(sub, rec + 4)? as usize;
        let abs = cmap_off.checked_add(rel)?;
        let fmt = u16_be(data, abs)?;
        if (plat == 3 && enc == 10 && fmt == 12) || (plat == 0 && enc == 4 && fmt == 12) {
            return Some((abs, 12));
        }
        if fmt == 4 && ((plat == 3 && enc == 1) || (plat == 0 && enc == 3)) && best_f4.is_none() {
            best_f4 = Some((abs, 4));
        }
    }
    best_f4
}

impl FontFace {
    pub fn parse(data: &'static [u8]) -> Option<Self> {
        let sfnt = u32_be(data, 0)?;
        if sfnt != 0x0001_0000 && sfnt != u32::from_be_bytes(*b"true") {
            return None;
        }
        let (head_o, head_l) = find_table(data, b"head")?;
        let (hhea_o, hhea_l) = find_table(data, b"hhea")?;
        let (maxp_o, _) = find_table(data, b"maxp")?;
        let (hmtx_off, hmtx_len) = find_table(data, b"hmtx")?;
        let (cmap_o, cmap_l) = find_table(data, b"cmap")?;
        let (loca_off, loca_len) = find_table(data, b"loca")?;
        let (glyf_off, glyf_len) = find_table(data, b"glyf")?;
        if head_l < 54 || hhea_l < 36 {
            return None;
        }

        let units_per_em = u16_be(data, head_o + 18)?;
        let index_to_loc_format = i16_be(data, head_o + 50)?;
        let ascent = i16_be(data, hhea_o + 4)?;
        let descent = i16_be(data, hhea_o + 6)?;
        let line_gap = i16_be(data, hhea_o + 8)?;
        let num_hmetrics = u16_be(data, hhea_o + 34)?;
        let num_glyphs = u16_be(data, maxp_o + 4)?;
        let (cmap_sub_off, cmap_format) = pick_cmap(data, cmap_o, cmap_l)?;
        if units_per_em == 0 || num_glyphs == 0 || num_hmetrics == 0 {
            return None;
        }

        Some(Self {
            data,
            units_per_em,
            ascent,
            descent,
            line_gap,
            index_to_loc_format,
            num_glyphs,
            num_hmetrics,
            cmap_sub_off,
            cmap_format,
            loca_off,
            loca_len,
            glyf_off,
            glyf_len,
            hmtx_off,
            hmtx_len,
        })
    }

    pub fn glyph_id(&self, ch: char) -> u16 {
        let cp = ch as u32;
        if self.cmap_format == 12 {
            let n = u32_be(self.data, self.cmap_sub_off + 12).unwrap_or(0) as usize;
            let mut lo = 0usize;
            let mut hi = n;
            while lo < hi {
                let mid = (lo + hi) / 2;
                let rec = self.cmap_sub_off + 16 + mid * 12;
                let start = u32_be(self.data, rec).unwrap_or(u32::MAX);
                let end = u32_be(self.data, rec + 4).unwrap_or(0);
                if cp < start {
                    hi = mid;
                } else if cp > end {
                    lo = mid + 1;
                } else {
                    let g0 = u32_be(self.data, rec + 8).unwrap_or(0);
                    let gid = g0.saturating_add(cp - start);
                    return if gid < self.num_glyphs as u32 { gid as u16 } else { 0 };
                }
            }
            return 0;
        }

        if cp > 0xFFFF {
            return 0;
        }
        let c16 = cp as u16;
        let seg_count = (u16_be(self.data, self.cmap_sub_off + 6).unwrap_or(0) / 2) as usize;
        let end_base = self.cmap_sub_off + 14;
        let start_base = end_base + seg_count * 2 + 2;
        let delta_base = start_base + seg_count * 2;
        let range_base = delta_base + seg_count * 2;

        let mut lo = 0usize;
        let mut hi = seg_count;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let end_c = u16_be(self.data, end_base + mid * 2).unwrap_or(0xFFFF);
            if c16 > end_c {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo >= seg_count {
            return 0;
        }
        let start_c = u16_be(self.data, start_base + lo * 2).unwrap_or(0xFFFF);
        if c16 < start_c {
            return 0;
        }
        let delta = i16_be(self.data, delta_base + lo * 2).unwrap_or(0);
        let range_off = u16_be(self.data, range_base + lo * 2).unwrap_or(0);
        let gid = if range_off == 0 {
            (c16 as i32 + delta as i32) as u16
        } else {
            let slot = range_base + lo * 2 + range_off as usize + (c16 - start_c) as usize * 2;
            let raw = u16_be(self.data, slot).unwrap_or(0);
            if raw == 0 { 0 } else { (raw as i32 + delta as i32) as u16 }
        };
        if gid < self.num_glyphs { gid } else { 0 }
    }

    pub fn hmetric(&self, gid: u16) -> (u16, i16) {
        let g = gid.min(self.num_glyphs.saturating_sub(1)) as usize;
        let nhm = self.num_hmetrics as usize;
        if g < nhm {
            let off = self.hmtx_off + g * 4;
            if off + 4 <= self.hmtx_off + self.hmtx_len {
                let adv = u16_be(self.data, off).unwrap_or(0);
                let lsb = i16_be(self.data, off + 2).unwrap_or(0);
                return (adv, lsb);
            }
        }
        let last_off = self.hmtx_off + nhm.saturating_sub(1) * 4;
        let adv = u16_be(self.data, last_off).unwrap_or(0);
        let lsb_off = self.hmtx_off + nhm * 4 + (g - nhm) * 2;
        let lsb = if lsb_off + 2 <= self.hmtx_off + self.hmtx_len {
            i16_be(self.data, lsb_off).unwrap_or(0)
        } else {
            0
        };
        (adv, lsb)
    }

    fn glyph_slice(&self, gid: u16) -> Option<&'static [u8]> {
        if gid >= self.num_glyphs {
            return None;
        }
        let idx = gid as usize;
        let (s0, s1) = if self.index_to_loc_format == 0 {
            if (idx + 2) * 2 > self.loca_len {
                return None;
            }
            let a = u16_be(self.data, self.loca_off + idx * 2)? as usize * 2;
            let b = u16_be(self.data, self.loca_off + idx * 2 + 2)? as usize * 2;
            (a, b)
        } else {
            if (idx + 2) * 4 > self.loca_len {
                return None;
            }
            let a = u32_be(self.data, self.loca_off + idx * 4)? as usize;
            let b = u32_be(self.data, self.loca_off + idx * 4 + 4)? as usize;
            (a, b)
        };
        if s1 <= s0 || s1 > self.glyf_len {
            return Some(&[]);
        }
        self.data.get(self.glyf_off + s0..self.glyf_off + s1)
    }

    pub fn load_glyph(&self, gid: u16, out: &mut OutlineBuf) -> bool {
        out.clear();
        let ok = self.append_glyph(gid, [16384, 0, 0, 16384], 0, 0, 0, out);
        if ok && out.n_pts == 0 {
            out.xmin = 0;
            out.ymin = 0;
            out.xmax = 0;
            out.ymax = 0;
        }
        ok
    }

    fn append_glyph(
        &self,
        gid: u16,
        mat: [i32; 4],
        tx: i32,
        ty: i32,
        depth: u8,
        out: &mut OutlineBuf,
    ) -> bool {
        if depth > 4 {
            return false;
        }
        let Some(g) = self.glyph_slice(gid) else {
            return false;
        };
        if g.is_empty() {
            return true;
        }
        if g.len() < 10 {
            return false;
        }
        let n_cont = i16_be(g, 0).unwrap_or(0);
        if n_cont >= 0 {
            self.load_simple(g, n_cont as usize, mat, tx, ty, out)
        } else {
            self.load_compound(g, mat, tx, ty, depth, out)
        }
    }

    fn load_simple(
        &self,
        g: &[u8],
        n_cont: usize,
        mat: [i32; 4],
        tx: i32,
        ty: i32,
        out: &mut OutlineBuf,
    ) -> bool {
        if n_cont == 0 {
            return true;
        }
        let end_off = 10 + n_cont * 2;
        if g.len() < end_off + 2 || out.n_conts + n_cont > MAX_OUTLINE_CONTOURS {
            return false;
        }
        let pt_base = out.n_pts;
        let mut last_end = 0usize;
        for i in 0..n_cont {
            let e = u16_be(g, 10 + i * 2).unwrap_or(0) as usize;
            last_end = last_end.max(e);
            out.cont_ends[out.n_conts] = (pt_base + e).min(MAX_OUTLINE_POINTS - 1) as u16;
            out.n_conts += 1;
        }
        let n_pts = last_end + 1;
        if pt_base + n_pts > MAX_OUTLINE_POINTS {
            return false;
        }
        let instr_len = u16_be(g, end_off).unwrap_or(0) as usize;
        let mut pos = end_off + 2 + instr_len;
        if pos > g.len() {
            return false;
        }

        // First pass: decode flags into `out.pts[pt_base..pt_base+n_pts].x` temporarily (`u8` bits).
        let mut i = 0usize;
        while i < n_pts {
            let Some(&fl) = g.get(pos) else {
                return false;
            };
            pos += 1;
            let rep = if (fl & 0x08) != 0 {
                let r = *g.get(pos).unwrap_or(&0) as usize;
                pos += 1;
                r + 1
            } else {
                1
            };
            for _ in 0..rep {
                if i < n_pts {
                    out.pts[pt_base + i].x = fl as i32;
                    out.pts[pt_base + i].on_curve = (fl & 0x01) != 0;
                    i += 1;
                }
            }
        }

        // Second pass: decode X deltas into temporary raw FUnits stored in `pts[k].x`.
        let mut cur_x = 0i32;
        for k in 0..n_pts {
            let fl = out.pts[pt_base + k].x as u8;
            if (fl & 0x02) != 0 {
                let dx = *g.get(pos).unwrap_or(&0) as i32;
                pos += 1;
                cur_x += if (fl & 0x10) != 0 { dx } else { -dx };
            } else if (fl & 0x10) == 0 {
                cur_x += i16_be(g, pos).unwrap_or(0) as i32;
                pos += 2;
            }
            out.pts[pt_base + k].x = cur_x;
            out.pts[pt_base + k].y = fl as i32;
        }

        // Third pass: decode Y deltas and apply affine matrix + translation immediately.
        let mut cur_y = 0i32;
        for k in 0..n_pts {
            let fl = out.pts[pt_base + k].y as u8;
            if (fl & 0x04) != 0 {
                let dy = *g.get(pos).unwrap_or(&0) as i32;
                pos += 1;
                cur_y += if (fl & 0x20) != 0 { dy } else { -dy };
            } else if (fl & 0x20) == 0 {
                cur_y += i16_be(g, pos).unwrap_or(0) as i32;
                pos += 2;
            }
            let rx = out.pts[pt_base + k].x;
            let ry = cur_y;
            let on = out.pts[pt_base + k].on_curve;
            let fx = ((mat[0] * rx + mat[2] * ry + 8192) >> 14) + tx;
            let fy = ((mat[1] * rx + mat[3] * ry + 8192) >> 14) + ty;
            // Store transformed point and adjust `out.n_pts` directly.
            out.pts[pt_base + k] = OutlinePt { x: fx, y: fy, on_curve: on };
            out.xmin = out.xmin.min(fx);
            out.ymin = out.ymin.min(fy);
            out.xmax = out.xmax.max(fx);
            out.ymax = out.ymax.max(fy);
        }
        out.n_pts = pt_base + n_pts;
        true
    }

    fn load_compound(
        &self,
        g: &[u8],
        mat: [i32; 4],
        tx: i32,
        ty: i32,
        depth: u8,
        out: &mut OutlineBuf,
    ) -> bool {
        let mut pos = 10usize;
        for _ in 0..24 {
            let Some(flags) = u16_be(g, pos) else {
                break;
            };
            let Some(sub_gid) = u16_be(g, pos + 2) else {
                break;
            };
            pos += 4;
            let (arg1, arg2) = if (flags & 0x0001) != 0 {
                let a = i16_be(g, pos).unwrap_or(0) as i32;
                let b = i16_be(g, pos + 2).unwrap_or(0) as i32;
                pos += 4;
                (a, b)
            } else {
                let a = g.get(pos).copied().unwrap_or(0) as i8 as i32;
                let b = g.get(pos + 1).copied().unwrap_or(0) as i8 as i32;
                pos += 2;
                (a, b)
            };

            let mut cm = [16384i32, 0, 0, 16384];
            if (flags & 0x0008) != 0 {
                let s = i16_be(g, pos).unwrap_or(16384) as i32;
                pos += 2;
                cm = [s, 0, 0, s];
            } else if (flags & 0x0040) != 0 {
                let sx = i16_be(g, pos).unwrap_or(16384) as i32;
                let sy = i16_be(g, pos + 2).unwrap_or(16384) as i32;
                pos += 4;
                cm = [sx, 0, 0, sy];
            } else if (flags & 0x0080) != 0 {
                cm[0] = i16_be(g, pos).unwrap_or(16384) as i32;
                cm[1] = i16_be(g, pos + 2).unwrap_or(0) as i32;
                cm[2] = i16_be(g, pos + 4).unwrap_or(0) as i32;
                cm[3] = i16_be(g, pos + 6).unwrap_or(16384) as i32;
                pos += 8;
            }

            let (dx, dy) = if (flags & 0x0002) != 0 {
                if (flags & 0x0800) != 0 {
                    (
                        (cm[0] * arg1 + cm[2] * arg2 + 8192) >> 14,
                        (cm[1] * arg1 + cm[3] * arg2 + 8192) >> 14,
                    )
                } else {
                    (arg1, arg2)
                }
            } else {
                (0, 0)
            };

            let combined = [
                (mat[0] * cm[0] + mat[2] * cm[1] + 8192) >> 14,
                (mat[1] * cm[0] + mat[3] * cm[1] + 8192) >> 14,
                (mat[0] * cm[2] + mat[2] * cm[3] + 8192) >> 14,
                (mat[1] * cm[2] + mat[3] * cm[3] + 8192) >> 14,
            ];
            let sub_tx = ((mat[0] * dx + mat[2] * dy + 8192) >> 14) + tx;
            let sub_ty = ((mat[1] * dx + mat[3] * dy + 8192) >> 14) + ty;

            let _ = self.append_glyph(sub_gid, combined, sub_tx, sub_ty, depth + 1, out);

            if (flags & 0x0020) == 0 {
                break;
            }
        }
        true
    }
}
