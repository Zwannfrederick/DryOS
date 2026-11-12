// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-Delegated Atomic Window Surface (`Frame4K`, `USER_RW` in owning app, `USER_RO` in
//! `dry-display` compositor) & Private Compositor Snapshot (`Milestone 2A-2` Section 7 Audit).
//!
//! Memory-Model & Security Guarantees:
//! 1. **Zero `&mut` Aliasing / Data-Race UB:** Every byte of `SharedWindowSurface` is backed by
//!    `core::sync::atomic` types (`AtomicU64`/`AtomicU32`/`AtomicU16`); neither client nor
//!    compositor ever materializes `&mut SharedWindowSurface` over shared pages.
//! 2. **Torn-Frame Elimination:** Writers bracket updates with an odd/even `seq` generation counter
//!    (`Release`/`Acquire`), while `dry-display` copies modified rows into a private scratch buffer
//!    and commits into `SurfaceSnapshot` only when `seq` is even and unchanged across the copy.
//! 3. **Untrusted Geometry Clamping (Anti-TOCTOU):** Header dimensions and dirty row bounds are
//!    loaded once into local registers and validated against the window's registered `(cols, rows)`
//!    before indexing any array.

use core::mem::size_of;
use core::sync::atomic::{fence, AtomicU16, AtomicU32, AtomicU64, Ordering};

pub const SURFACE_MAGIC: u64 = 0x4452_595f_5355_5246; // "DRY_SURF"
pub const TERM_COLS: usize = 80;
pub const TERM_ROWS: usize = 24;
pub const TERM_CELLS: usize = TERM_COLS * TERM_ROWS; // 1,920 cells

pub const STYLE_DEFAULT: u8 = 0;
pub const STYLE_PROMPT: u8 = 1;
pub const STYLE_ACCENT: u8 = 2;
pub const STYLE_MUTED: u8 = 3;
pub const STYLE_OK: u8 = 4;
pub const STYLE_WARN: u8 = 5;
pub const STYLE_HEADER: u8 = 6;

pub const SURF_FLAG_CURSOR_VISIBLE: u32 = 1 << 0;

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct SurfaceCell {
    pub ch: u8,
    pub style: u8,
}

impl SurfaceCell {
    pub const BLANK: Self = Self {
        ch: b' ',
        style: STYLE_DEFAULT,
    };

    #[inline]
    pub const fn pack_u16(self) -> u16 {
        (self.ch as u16) | ((self.style as u16) << 8)
    }

    #[inline]
    pub const fn unpack_u16(raw: u16) -> Self {
        Self {
            ch: (raw & 0xff) as u8,
            style: ((raw >> 8) & 0xff) as u8,
        }
    }
}

/// 64-byte atomic header + 3,840-byte `[AtomicU16; 1920]` cell grid = `3,904` bytes (`<= 4,096` B).
#[repr(C)]
pub struct SharedWindowSurface {
    pub magic: AtomicU64,
    pub seq: AtomicU32,
    pub cols: AtomicU16,
    pub rows: AtomicU16,
    pub cursor_col: AtomicU16,
    pub cursor_row: AtomicU16,
    pub dirty_row_min: AtomicU16,
    pub dirty_row_max: AtomicU16,
    pub flags: AtomicU32,
    pub cmd_count: AtomicU32,
    pub ext_irqs_snap: AtomicU32,
    pub uptime_sec_snap: AtomicU32,
    pub reserved: [AtomicU32; 6],
    pub cells: [AtomicU16; TERM_CELLS],
}

pub type SharedTerminalSurface = SharedWindowSurface;

const _: () = assert!(size_of::<SharedWindowSurface>() == 3904);
const _: () = assert!(size_of::<SharedWindowSurface>() <= 4096);

impl SharedWindowSurface {
    /// Initialize an exclusively-owned `USER_RW` surface mapping at `vaddr` using atomic stores only.
    ///
    /// # Safety
    /// `vaddr` must point to a mapped, 8-byte-aligned 4 KiB writable frame in the caller's `VSpace`.
    pub unsafe fn init_at(vaddr: u64, cols: u16, rows: u16, cursor_visible: bool) -> &'static Self {
        let s = unsafe { &*(vaddr as *const Self) };
        let c = cols.clamp(1, TERM_COLS as u16);
        let r = rows.clamp(1, TERM_ROWS as u16);
        let blank = SurfaceCell::BLANK.pack_u16();
        s.seq.store(1, Ordering::Relaxed);
        for cell in &s.cells {
            cell.store(blank, Ordering::Relaxed);
        }
        s.cols.store(c, Ordering::Relaxed);
        s.rows.store(r, Ordering::Relaxed);
        s.cursor_col.store(0, Ordering::Relaxed);
        s.cursor_row.store(0, Ordering::Relaxed);
        s.dirty_row_min.store(0, Ordering::Relaxed);
        s.dirty_row_max.store(r - 1, Ordering::Relaxed);
        s.flags.store(
            if cursor_visible {
                SURF_FLAG_CURSOR_VISIBLE
            } else {
                0
            },
            Ordering::Relaxed,
        );
        s.cmd_count.store(0, Ordering::Relaxed);
        s.ext_irqs_snap.store(0, Ordering::Relaxed);
        s.uptime_sec_snap.store(0, Ordering::Relaxed);
        for word in &s.reserved {
            word.store(0, Ordering::Relaxed);
        }
        s.magic.store(SURFACE_MAGIC, Ordering::Release);
        s.seq.store(2, Ordering::Release);
        s
    }

    #[inline]
    pub fn begin_write(&self) -> u32 {
        let cur = self.seq.load(Ordering::Relaxed);
        let odd = cur | 1;
        self.seq.store(odd, Ordering::Release);
        fence(Ordering::AcqRel);
        odd
    }

    #[inline]
    pub fn end_write(&self, odd_token: u32) -> u32 {
        let even = odd_token.wrapping_add(1) & !1;
        let next = if even == 0 { 2 } else { even };
        self.seq.store(next, Ordering::Release);
        next
    }

    #[inline]
    pub fn store_row_slice(&self, cols: usize, row: usize, src: &[SurfaceCell]) {
        let base = row.saturating_mul(cols);
        let limit = src.len().min(cols);
        for (i, cell) in src.iter().take(limit).enumerate() {
            if base + i < TERM_CELLS {
                self.cells[base + i].store(cell.pack_u16(), Ordering::Relaxed);
            }
        }
    }

    #[inline]
    pub fn current_seq(&self) -> u32 {
        self.seq.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceError {
    BadMagic,
    BadGeometry,
}

/// Validated compositor-private snapshot of a client's `SharedWindowSurface`.
/// Stored inside `dry-display`'s private RAM so cursor moves, drags, and Z-order redraws never
/// touch untrusted client pages asynchronously.
pub struct SurfaceSnapshot {
    pub valid: bool,
    pub seq: u32,
    pub cols: u16,
    pub rows: u16,
    pub cursor_col: u16,
    pub cursor_row: u16,
    pub cursor_visible: bool,
    pub cmd_count: u32,
    pub ext_irqs_snap: u32,
    pub uptime_sec_snap: u32,
    pub cells: [SurfaceCell; TERM_CELLS],
}

impl SurfaceSnapshot {
    pub const fn new() -> Self {
        Self {
            valid: false,
            seq: 0,
            cols: TERM_COLS as u16,
            rows: TERM_ROWS as u16,
            cursor_col: 0,
            cursor_row: 0,
            cursor_visible: false,
            cmd_count: 0,
            ext_irqs_snap: 0,
            uptime_sec_snap: 0,
            cells: [SurfaceCell::BLANK; TERM_CELLS],
        }
    }

    /// Atomically snapshot `[rmin..=rmax]` from a `USER_RO` shared surface at `ro_vaddr` into `self`
    /// using `scratch` to prevent torn updates if a client mutates shared memory concurrently.
    ///
    /// Returns `Ok((rmin, rmax, changed))` or `Err(())` if magic/geometry validation fails.
    ///
    /// # Safety
    /// `ro_vaddr` must be a valid `USER_RO` 4 KiB mapping in `dry-display`'s `VSpace`.
    pub unsafe fn capture_from_ro(
        &mut self,
        ro_vaddr: u64,
        expected_cols: u16,
        expected_rows: u16,
        scratch: &mut [SurfaceCell; TERM_CELLS],
    ) -> Result<(usize, usize, bool), SurfaceError> {
        let shared = unsafe { &*(ro_vaddr as *const SharedWindowSurface) };
        if shared.magic.load(Ordering::Acquire) != SURFACE_MAGIC {
            return Err(SurfaceError::BadMagic);
        }

        let cols = shared.cols.load(Ordering::Relaxed);
        let rows = shared.rows.load(Ordering::Relaxed);
        if cols == 0
            || rows == 0
            || cols != expected_cols
            || rows != expected_rows
            || (cols as usize) > TERM_COLS
            || (rows as usize) > TERM_ROWS
        {
            return Err(SurfaceError::BadGeometry);
        }
        let c_usize = cols as usize;
        let r_usize = rows as usize;
        let total = c_usize * r_usize;

        for _attempt in 0..3 {
            let s1 = shared.seq.load(Ordering::Acquire);
            if (s1 & 1) != 0 {
                core::hint::spin_loop();
                continue;
            }

            let raw_rmin = shared.dirty_row_min.load(Ordering::Relaxed) as usize;
            let raw_rmax = shared.dirty_row_max.load(Ordering::Relaxed) as usize;
            let rmin = if !self.valid {
                0
            } else {
                raw_rmin.min(r_usize - 1)
            };
            let rmax = if !self.valid {
                r_usize - 1
            } else {
                raw_rmax.max(rmin).min(r_usize - 1)
            };

            let start_cell = rmin * c_usize;
            let end_cell = ((rmax + 1) * c_usize).min(total);
            for (dst, src) in scratch[start_cell..end_cell]
                .iter_mut()
                .zip(&shared.cells[start_cell..end_cell])
            {
                *dst = SurfaceCell::unpack_u16(src.load(Ordering::Relaxed));
            }
            let ccol = shared.cursor_col.load(Ordering::Relaxed).min(cols - 1);
            let crow = shared.cursor_row.load(Ordering::Relaxed).min(rows - 1);
            let flags = shared.flags.load(Ordering::Relaxed);
            let cmds = shared.cmd_count.load(Ordering::Relaxed);
            let eirqs = shared.ext_irqs_snap.load(Ordering::Relaxed);
            let up_s = shared.uptime_sec_snap.load(Ordering::Relaxed);

            fence(Ordering::Acquire);
            let s2 = shared.seq.load(Ordering::Relaxed);
            if s1 == s2 {
                let prev_crow = self.cursor_row as usize;
                self.cells[start_cell..end_cell].copy_from_slice(&scratch[start_cell..end_cell]);
                self.valid = true;
                self.seq = s1;
                self.cols = cols;
                self.rows = rows;
                self.cursor_col = ccol;
                self.cursor_row = crow;
                self.cursor_visible = (flags & SURF_FLAG_CURSOR_VISIBLE) != 0;
                self.cmd_count = cmds;
                self.ext_irqs_snap = eirqs;
                self.uptime_sec_snap = up_s;
                let eff_min = rmin.min(prev_crow.min(r_usize - 1)).min(crow as usize);
                let eff_max = rmax.max(prev_crow.min(r_usize - 1)).max(crow as usize);
                return Ok((eff_min, eff_max, true));
            }
        }

        // Writer remained mid-update across all 3 bounded probes: preserve previous valid snapshot!
        Ok((0, 0, false))
    }
}

impl Default for SurfaceSnapshot {
    fn default() -> Self {
        Self::new()
    }
}
