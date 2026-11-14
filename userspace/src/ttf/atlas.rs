// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Bounded static `.bss` TrueType glyph atlas (`96 KiB`, zero heap) for single-threaded Ring 3 apps.

use core::cell::UnsafeCell;

use super::raster::{rasterize_glyph, RasterScratch};
use super::sfnt::FontFace;

static FONT_SANS_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/DrySans-Regular.ttf");
static FONT_SANS_SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/DrySans-SemiBold.ttf");
static FONT_MONO_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/DryMono-Regular.ttf");
static FONT_MONO_SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/DryMono-SemiBold.ttf");

const ATLAS_POOL_BYTES: usize = 96 * 1024;
const ATLAS_MAX_SLOTS: usize = 512;
const ATLAS_BUCKETS: usize = 1024;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontId {
    SansRegular = 0,
    SansSemiBold = 1,
    MonoRegular = 2,
    MonoSemiBold = 3,
}

impl FontId {
    #[inline]
    pub const fn as_idx(self) -> usize {
        self as usize
    }

    #[inline]
    pub const fn raw_bytes(self) -> &'static [u8] {
        match self {
            Self::SansRegular => FONT_SANS_REGULAR,
            Self::SansSemiBold => FONT_SANS_SEMIBOLD,
            Self::MonoRegular => FONT_MONO_REGULAR,
            Self::MonoSemiBold => FONT_MONO_SEMIBOLD,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct FontMetrics {
    pub size_px: u8,
    pub ascent: i32,
    pub descent: i32,
    pub line_height: i32,
}

#[derive(Clone, Copy, Default)]
pub struct GlyphEntry {
    pub key: u32,
    pub pool_off: u32,
    pub adv_24_8: i32,
    pub w: u8,
    pub h: u8,
    pub bear_x: i8,
    pub bear_y: i8,
}

impl GlyphEntry {
    const EMPTY: Self = Self {
        key: u32::MAX,
        pool_off: 0,
        adv_24_8: 0,
        w: 0,
        h: 0,
        bear_x: 0,
        bear_y: 0,
    };
}

struct AtlasState {
    faces: [Option<FontFace>; 4],
    scratch: RasterScratch,
    pool: [u8; ATLAS_POOL_BYTES],
    pool_used: usize,
    slots: [GlyphEntry; ATLAS_MAX_SLOTS],
    n_slots: usize,
    buckets: [u16; ATLAS_BUCKETS],
    hits: u32,
    misses: u32,
    resets: u32,
}

impl AtlasState {
    const fn new() -> Self {
        Self {
            faces: [None, None, None, None],
            scratch: RasterScratch::new(),
            pool: [0; ATLAS_POOL_BYTES],
            pool_used: 0,
            slots: [GlyphEntry::EMPTY; ATLAS_MAX_SLOTS],
            n_slots: 0,
            buckets: [0; ATLAS_BUCKETS],
            hits: 0,
            misses: 0,
            resets: 0,
        }
    }

    fn face(&mut self, font: FontId) -> Option<FontFace> {
        let idx = font.as_idx();
        if self.faces[idx].is_none() {
            self.faces[idx] = FontFace::parse(font.raw_bytes());
        }
        self.faces[idx]
    }

    #[inline]
    fn pack_key(font: FontId, size_px: u8, gid: u16) -> u32 {
        ((font as u32) << 24) | ((size_px as u32) << 16) | (gid as u32)
    }

    #[inline]
    fn hash_key(k: u32) -> usize {
        let mut x = k.wrapping_mul(0x9E37_79B9);
        x ^= x >> 16;
        (x as usize) & (ATLAS_BUCKETS - 1)
    }

    fn lookup_slot(&self, key: u32) -> Option<usize> {
        let mut h = Self::hash_key(key);
        for _ in 0..ATLAS_BUCKETS {
            let b = self.buckets[h];
            if b == 0 {
                return None;
            }
            let s = (b - 1) as usize;
            if self.slots[s].key == key {
                return Some(s);
            }
            h = (h + 1) & (ATLAS_BUCKETS - 1);
        }
        None
    }

    fn insert_slot(&mut self, entry: GlyphEntry) {
        let mut h = Self::hash_key(entry.key);
        for _ in 0..ATLAS_BUCKETS {
            if self.buckets[h] == 0 {
                let idx = self.n_slots;
                self.slots[idx] = entry;
                self.buckets[h] = (idx + 1) as u16;
                self.n_slots += 1;
                return;
            }
            h = (h + 1) & (ATLAS_BUCKETS - 1);
        }
    }

    fn reset_cache(&mut self) {
        self.pool_used = 0;
        self.n_slots = 0;
        self.buckets.fill(0);
        self.resets = self.resets.saturating_add(1);
    }
}

struct SyncAtlasCell(UnsafeCell<AtlasState>);
// SAFETY: All DRY OS userspace services run in single-threaded Ring-3 address spaces.
unsafe impl Sync for SyncAtlasCell {}

static ATLAS: SyncAtlasCell = SyncAtlasCell(UnsafeCell::new(AtlasState::new()));

pub fn font_metrics(font: FontId, size_px: u8) -> FontMetrics {
    let sz = size_px.clamp(8, 64);
    // SAFETY: Single-threaded process access; mutable borrow never escapes this call.
    let st = unsafe { &mut *ATLAS.0.get() };
    if let Some(face) = st.face(font) {
        let upm = face.units_per_em.max(1) as i32;
        let asc = ((face.ascent as i32 * sz as i32) + upm - 1) / upm;
        let desc = ((-face.descent as i32 * sz as i32) + upm - 1) / upm;
        let gap = (face.line_gap as i32 * sz as i32) / upm;
        let lh = (asc + desc + gap).max(sz as i32 + 3);
        FontMetrics {
            size_px: sz,
            ascent: asc.max(sz as i32 * 4 / 5),
            descent: desc.max(2),
            line_height: lh,
        }
    } else {
        FontMetrics {
            size_px: sz,
            ascent: sz as i32,
            descent: 3,
            line_height: sz as i32 + 4,
        }
    }
}

pub fn glyph_advance_24_8(font: FontId, size_px: u8, ch: char) -> i32 {
    let sz = size_px.clamp(8, 64);
    // SAFETY: Single-threaded process access; mutable borrow never escapes this call.
    let st = unsafe { &mut *ATLAS.0.get() };
    let Some(face) = st.face(font) else {
        return (sz as i32) << 7;
    };
    let gid = face.glyph_id(ch);
    let upm = face.units_per_em.max(1) as i64;
    let (adv_fu, _) = face.hmetric(gid);
    ((adv_fu as i64 * (sz as i64 * 256) + (upm >> 1)) / upm) as i32
}

pub fn with_glyph<R>(
    font: FontId,
    size_px: u8,
    ch: char,
    f: impl FnOnce(GlyphEntry, &[u8]) -> R,
) -> R {
    let sz = size_px.clamp(8, 64);
    // SAFETY: Single-threaded process access; mutable borrow is confined to this scope.
    let st = unsafe { &mut *ATLAS.0.get() };
    let Some(face) = st.face(font) else {
        return f(GlyphEntry::EMPTY, &[]);
    };
    let gid = face.glyph_id(ch);
    let key = AtlasState::pack_key(font, sz, gid);

    if let Some(slot_idx) = st.lookup_slot(key) {
        st.hits = st.hits.saturating_add(1);
        let e = st.slots[slot_idx];
        let len = (e.w as usize) * (e.h as usize);
        let off = e.pool_off as usize;
        let slice = st.pool.get(off..off + len).unwrap_or(&[]);
        return f(e, slice);
    }

    st.misses = st.misses.saturating_add(1);
    let m = rasterize_glyph(&face, gid, sz, &mut st.scratch);
    let need = (m.w as usize) * (m.h as usize);
    if need > ATLAS_POOL_BYTES {
        return f(GlyphEntry::EMPTY, &[]);
    }
    if st.n_slots >= ATLAS_MAX_SLOTS || st.pool_used + need > ATLAS_POOL_BYTES {
        st.reset_cache();
    }

    let off = st.pool_used;
    if need > 0 {
        st.pool[off..off + need].copy_from_slice(&st.scratch.cov[..need]);
        st.pool_used += need;
    }
    let entry = GlyphEntry {
        key,
        pool_off: off as u32,
        adv_24_8: m.adv_24_8,
        w: m.w,
        h: m.h,
        bear_x: m.bear_x,
        bear_y: m.bear_y,
    };
    st.insert_slot(entry);
    let slice = st.pool.get(off..off + need).unwrap_or(&[]);
    f(entry, slice)
}

#[inline]
pub fn atlas_stats() -> (u32, u32, u32, usize) {
    // SAFETY: Single-threaded process access.
    let st = unsafe { &*ATLAS.0.get() };
    (st.hits, st.misses, st.resets, st.pool_used)
}
