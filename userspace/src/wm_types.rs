// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Shared Window Manager Data Types & Geometry Constants (`ADR-013`, `Milestone 2A-2`–`2B`).

use crate::action::DesktopAction;
use crate::gfx::Rect;
use crate::input::InputRingQueue;
use crate::surface::{SurfaceSnapshot, TERM_COLS, TERM_ROWS};

/// Maximum concurrent composited windows.
/// Bounded by kernel `MAX_CSPACES = 32` and ~4 MiB surface+snapshot memory per pixel window.
pub const MAX_WINDOWS: usize = 16;
pub const WIN_EVENT_QUEUE_CAP: usize = 32;
pub const SUP_BIT_WM_ACTION: u64 = 1 << 0;

pub const TOPBAR_H: usize = 28;
pub const WIN_PAD_X: usize = 12;
pub const WIN_TITLE_H: usize = 28;
pub const WIN_PAD_BOTTOM: usize = 10;
pub const WIN_BORDER: usize = 2;
pub const SHADOW_OFF: usize = 6;

pub const TITLE_KIND_TERMINAL: u8 = 0;
pub const TITLE_KIND_SYSMON: u8 = 1;
pub const TITLE_KIND_GENERIC: u8 = 2;
pub const TITLE_KIND_FILES: u8 = 3;
pub const TITLE_KIND_SETTINGS: u8 = 4;
pub const TITLE_KIND_DIALOG: u8 = 5;
pub const TITLE_KIND_EDITOR: u8 = 6;
pub const TITLE_KIND_CALC: u8 = 7;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowRole {
    Normal = 0,
    /// Modal/helper dialog role: stays auto-floating above tiled/stacked/tabbed workspaces.
    DialogTransient = 1,
}

impl WindowRole {
    #[inline]
    pub const fn from_u8(v: u8) -> Self {
        if v == 1 {
            Self::DialogTransient
        } else {
            Self::Normal
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutMode {
    Floating = 0,
    SideBySideTiled = 1,
    Stacked = 2,
    Tabbed = 3,
}

impl LayoutMode {
    #[inline]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Floating),
            1 => Some(Self::SideBySideTiled),
            2 => Some(Self::Stacked),
            3 => Some(Self::Tabbed),
            _ => None,
        }
    }

    #[inline]
    pub const fn next_mode(self) -> Self {
        match self {
            Self::Floating => Self::SideBySideTiled,
            Self::SideBySideTiled => Self::Stacked,
            Self::Stacked => Self::Tabbed,
            Self::Tabbed => Self::Floating,
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionVisualState {
    Active = 0,
    DisplayBlanked = 1,
    SessionLocked = 2,
}

impl SessionVisualState {
    #[inline]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Active),
            1 => Some(Self::DisplayBlanked),
            2 => Some(Self::SessionLocked),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitPart {
    TitleBar,
    MinimizeButton,
    CloseButton,
    ClientBody,
}

#[derive(Clone, Copy)]
pub struct RemovedWindowSlot {
    pub surf_cap: u64,
    pub surf_vaddr: u64,
    pub is_pixel: bool,
    pub snap_cap: u64,
    pub snap_vaddr: u64,
    pub notif_cap: u64,
    pub waiter_tok: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WmError {
    PermDenied,
    InvalidGeometry,
    NoFreeSlots,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WmInputAction {
    None,
    CursorMoved,
    FocusChanged(u32),
    WindowDragged(u32),
    DragEnded(u32, usize, usize),
    CloseRequested(u32),
    LayoutToggled(LayoutMode),
    KeyRouted(u32),
    PointerRouted(u32),
    WheelRouted(u32),
    KeyMoved(u32, usize, usize),
    DesktopActionQueued(DesktopAction),
    DpmsWoke,
    LockBlocked,
    LockPinEdited,
}

pub struct WindowSlot {
    pub allocated: bool,
    pub id: u32,
    pub owner_badge: u64,
    pub role: WindowRole,
    pub title_kind: u8,
    pub visible: bool,
    pub minimized: bool,
    pub focused: bool,
    pub close_requested: bool,
    pub instance_ordinal: u8,
    pub custom_title: [u8; 32],
    pub custom_title_len: u8,
    pub cols: u16,
    pub rows: u16,
    pub rect: Rect,
    pub saved_floating_rect: Rect,
    pub surf_cap: u64,
    pub surf_vaddr: u64,
    pub notif_cap: u64,
    pub waiter_reply_tok: u64,
    pub ev_queue: InputRingQueue<WIN_EVENT_QUEUE_CAP>,
    pub snapshot: SurfaceSnapshot,
    pub is_pixel: bool,
    pub pixel_w: u16,
    pub pixel_h: u16,
    pub snap_cap: u64,
    pub snap_vaddr: u64,
    pub last_cfg_w: u16,
    pub last_cfg_h: u16,
    pub last_cfg_flags: u8,
}

impl WindowSlot {
    pub const fn empty() -> Self {
        Self {
            allocated: false,
            id: 0,
            owner_badge: 0,
            role: WindowRole::Normal,
            title_kind: TITLE_KIND_TERMINAL,
            visible: false,
            minimized: false,
            focused: false,
            close_requested: false,
            instance_ordinal: 0,
            custom_title: [0u8; 32],
            custom_title_len: 0,
            cols: TERM_COLS as u16,
            rows: TERM_ROWS as u16,
            rect: Rect::EMPTY,
            saved_floating_rect: Rect::EMPTY,
            surf_cap: 0,
            surf_vaddr: 0,
            notif_cap: 0,
            waiter_reply_tok: 0,
            ev_queue: InputRingQueue::new(),
            snapshot: SurfaceSnapshot::new(),
            is_pixel: false,
            pixel_w: 0,
            pixel_h: 0,
            snap_cap: 0,
            snap_vaddr: 0,
            last_cfg_w: 0,
            last_cfg_h: 0,
            last_cfg_flags: 0,
        }
    }

    /// Resets slot fields in place without materializing a ~14 KiB `WindowSlot` on the 16 KiB Ring-3 stack.
    pub fn reset_in_place(&mut self) {
        self.allocated = false;
        self.id = 0;
        self.owner_badge = 0;
        self.role = WindowRole::Normal;
        self.title_kind = TITLE_KIND_TERMINAL;
        self.visible = false;
        self.minimized = false;
        self.focused = false;
        self.close_requested = false;
        self.instance_ordinal = 0;
        self.custom_title = [0; 32];
        self.custom_title_len = 0;
        self.cols = TERM_COLS as u16;
        self.rows = TERM_ROWS as u16;
        self.rect = Rect::EMPTY;
        self.saved_floating_rect = Rect::EMPTY;
        self.surf_cap = 0;
        self.surf_vaddr = 0;
        self.notif_cap = 0;
        self.waiter_reply_tok = 0;
        self.ev_queue.reset();
        self.snapshot.reset_in_place();
        self.is_pixel = false;
        self.pixel_w = 0;
        self.pixel_h = 0;
        self.snap_cap = 0;
        self.snap_vaddr = 0;
        self.last_cfg_w = 0;
        self.last_cfg_h = 0;
        self.last_cfg_flags = 0;
    }

    #[inline]
    pub const fn outer_rect(&self) -> Rect {
        if !self.allocated || !self.visible {
            Rect::EMPTY
        } else {
            let p = SHADOW_OFF + 2;
            Rect::new(
                self.rect.x.saturating_sub(p),
                self.rect.y.saturating_sub(p),
                self.rect.w + p * 2,
                self.rect.h + p * 2,
            )
        }
    }

    #[inline]
    pub const fn titlebar_rect(&self) -> Rect {
        Rect::new(
            self.rect.x + WIN_BORDER,
            self.rect.y + WIN_BORDER,
            self.rect.w.saturating_sub(WIN_BORDER * 2),
            WIN_TITLE_H,
        )
    }

    #[inline]
    pub const fn close_btn_rect(&self) -> Rect {
        let bx = (self.rect.x + self.rect.w).saturating_sub(WIN_BORDER + 28);
        Rect::new(bx, self.rect.y + WIN_BORDER + 4, 22, 20)
    }

    #[inline]
    pub const fn minimize_btn_rect(&self) -> Rect {
        let bx = self.close_btn_rect().x.saturating_sub(26);
        Rect::new(bx, self.rect.y + WIN_BORDER + 4, 22, 20)
    }

    /// Stores a custom UTF-8/ASCII window title suffix/override capped to 32 bytes.
    pub fn set_custom_title(&mut self, title: &[u8]) {
        let n = title.len().min(self.custom_title.len());
        self.custom_title = [0u8; 32];
        self.custom_title[..n].copy_from_slice(&title[..n]);
        self.custom_title_len = n as u8;
    }

    #[inline]
    pub fn custom_title_bytes(&self) -> &[u8] {
        let n = (self.custom_title_len as usize).min(self.custom_title.len());
        &self.custom_title[..n]
    }

    #[inline]
    pub const fn grid_origin(&self) -> (usize, usize) {
        (
            self.rect.x + WIN_BORDER + WIN_PAD_X,
            self.rect.y + WIN_BORDER + WIN_TITLE_H + 4,
        )
    }
}

#[inline]
pub const fn compute_window_size(cols: u16, rows: u16) -> (usize, usize) {
    let w = (cols as usize) * 8 + WIN_PAD_X * 2 + WIN_BORDER * 2;
    let h = WIN_TITLE_H + (rows as usize) * 16 + WIN_PAD_BOTTOM + WIN_BORDER * 2;
    (w, h)
}

pub const PIXEL_WIN_CHROME_W: usize = WIN_PAD_X * 2 + WIN_BORDER * 2;
pub const PIXEL_WIN_CHROME_H: usize = WIN_TITLE_H + WIN_PAD_BOTTOM + WIN_BORDER * 2;

#[inline]
pub const fn compute_pixel_window_size(pw: u16, ph: u16) -> (usize, usize) {
    let w = (pw as usize) + PIXEL_WIN_CHROME_W;
    let h = (ph as usize) + PIXEL_WIN_CHROME_H;
    (w, h)
}

#[inline]
pub const fn client_dims_for_window_rect(win_w: usize, win_h: usize) -> (u16, u16) {
    crate::surface::clamp_pixel_dims(
        win_w.saturating_sub(PIXEL_WIN_CHROME_W),
        win_h.saturating_sub(PIXEL_WIN_CHROME_H),
    )
}
