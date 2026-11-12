// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Shared Window Manager Data Types & Geometry Constants (`ADR-013`, `Milestone 2A-2`–`2B`).

use crate::action::DesktopAction;
use crate::gfx::Rect;
use crate::input::InputRingQueue;
use crate::surface::{SurfaceSnapshot, TERM_COLS, TERM_ROWS};

pub const MAX_WINDOWS: usize = 8;
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
    CloseButton,
    ClientBody,
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
    pub focused: bool,
    pub close_requested: bool,
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
            focused: false,
            close_requested: false,
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
        }
    }

    #[inline]
    pub const fn outer_rect(&self) -> Rect {
        if !self.allocated || !self.visible {
            Rect::EMPTY
        } else {
            Rect::new(
                self.rect.x,
                self.rect.y,
                self.rect.w + SHADOW_OFF,
                self.rect.h + SHADOW_OFF,
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
