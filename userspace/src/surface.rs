// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-Delegated Shared Terminal Surface (`Frame4K`, `USER_RW` in `dry-shell`, `USER_RO`
//! in `dry-display` compositor).
//!
//! Enforces strict display isolation: `dry-shell` (`SecDomain::Userspace`) never holds or maps
//! `CapObjectKind::Framebuffer`; instead it writes character/style cells into its own 4 KiB frame
//! and grants a narrowed `READ | MAP` capability to `dry-display` (`SecDomain::RootSystem`).

use core::mem::size_of;

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
}

/// 64-byte header + 3,840-byte `80x24` cell grid = `3,904` bytes (`<= 4,096` single 4 KiB page).
#[repr(C)]
pub struct SharedTerminalSurface {
    pub magic: u64,
    pub seq: u64,
    pub cols: u16,
    pub rows: u16,
    pub cursor_col: u16,
    pub cursor_row: u16,
    pub dirty_row_min: u16,
    pub dirty_row_max: u16,
    pub focused: u8,
    pub cursor_visible: u8,
    pub _pad0: u16,
    pub cmd_count: u32,
    pub ext_irqs_snap: u32,
    pub uptime_sec_snap: u32,
    pub _reserved: [u8; 20],
    pub cells: [SurfaceCell; TERM_CELLS],
}

const _: () = assert!(size_of::<SharedTerminalSurface>() == 3904);
const _: () = assert!(size_of::<SharedTerminalSurface>() <= 4096);

impl SharedTerminalSurface {
    pub fn init(&mut self) {
        self.magic = SURFACE_MAGIC;
        self.seq = 1;
        self.cols = TERM_COLS as u16;
        self.rows = TERM_ROWS as u16;
        self.cursor_col = 0;
        self.cursor_row = 0;
        self.dirty_row_min = 0;
        self.dirty_row_max = (TERM_ROWS - 1) as u16;
        self.focused = 1;
        self.cursor_visible = 1;
        self._pad0 = 0;
        self.cmd_count = 0;
        self.ext_irqs_snap = 0;
        self.uptime_sec_snap = 0;
        self._reserved = [0; 20];
        self.cells = [SurfaceCell::BLANK; TERM_CELLS];
    }
}
