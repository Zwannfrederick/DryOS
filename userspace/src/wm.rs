// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Ring 3 Multi-Window Manager State, Policy, Z-Order, Hit-Testing, Focus Routing & Optional
//! Side-by-Side Tiling (`ADR-013`, `Milestone 2A-2`).
//!
//! Architectural Boundary:
//! - Lives 100% in `no_std` userspace (`0` UI/window lines in `dry-kernel`).
//! - Contains zero raw framebuffer/MMIO pointers; emits explicit damage `Rect`s for `compositor.rs`.
//! - Enforces unforgeable kernel capability badge ownership (`WindowSlot::owner_badge == sender_badge`)
//!   on every surface/event operation and restricts `IPC_OP_WM_INPUT_EVENT` strictly to `BADGE_INPUT_SVC`.

use crate::action::{ActionQueue, AppId, DesktopAction, APP_COUNT};
use crate::gfx::{DirtyRegion, Rect, CURSOR_H, CURSOR_W};
use crate::input::{InputEvent, InputEventKind, InputRingQueue, MOD_ALT, MOUSE_BTN_LEFT};
use crate::keymap::{KEY_DOWN, KEY_ENTER, KEY_LEFT, KEY_RIGHT, KEY_TAB, KEY_UP};
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

pub struct WindowManager {
    pub screen_w: usize,
    pub screen_h: usize,
    pub desktop_mode: bool,
    pub p3_mode: bool,
    pub running_apps_mask: u8,
    pub sup_notif_cap: u64,
    pub sup_wake_cap: u64,
    pub action_queue: ActionQueue<16>,
    pub layout: LayoutMode,
    pub session_state: SessionVisualState,
    pub blank_timeout_tsc: u64,
    pub last_input_tsc: u64,
    pub high_contrast: bool,
    pub cursor_large: bool,
    pub cursor_x: usize,
    pub cursor_y: usize,
    pub cursor_visible: bool,
    pub buttons_down: u8,
    pub next_win_id: u32,
    pub windows: [WindowSlot; MAX_WINDOWS],
    /// Bottom-to-top Z-order indices into `windows` (`z_stack[z_len - 1]` is topmost).
    pub z_stack: [usize; MAX_WINDOWS],
    pub z_len: usize,
    pub dragging_slot: Option<usize>,
    pub drag_off_x: usize,
    pub drag_off_y: usize,
    pub unauthorized_rejects: u64,
    pub invalid_req_rejects: u64,
    pub cursor_clamp_hits: u64,
    pub focus_switches: u64,
    pub z_raises: u64,
    pub drag_moves: u64,
    pub dpms_blank_count: u64,
    pub dpms_wake_count: u64,
    pub locked_blocked_events: u64,
}

impl WindowManager {
    pub const fn new() -> Self {
        Self {
            screen_w: 1280,
            screen_h: 800,
            desktop_mode: false,
            p3_mode: false,
            running_apps_mask: 0,
            sup_notif_cap: 0,
            sup_wake_cap: 0,
            action_queue: ActionQueue::new(),
            layout: LayoutMode::Floating,
            session_state: SessionVisualState::Active,
            blank_timeout_tsc: 0,
            last_input_tsc: 0,
            high_contrast: false,
            cursor_large: false,
            cursor_x: 640,
            cursor_y: 400,
            cursor_visible: false,
            buttons_down: 0,
            next_win_id: 1,
            windows: [const { WindowSlot::empty() }; MAX_WINDOWS],
            z_stack: [0; MAX_WINDOWS],
            z_len: 0,
            dragging_slot: None,
            drag_off_x: 0,
            drag_off_y: 0,
            unauthorized_rejects: 0,
            invalid_req_rejects: 0,
            cursor_clamp_hits: 0,
            focus_switches: 0,
            z_raises: 0,
            drag_moves: 0,
            dpms_blank_count: 0,
            dpms_wake_count: 0,
            locked_blocked_events: 0,
        }
    }

    pub fn init_screen(&mut self, w: usize, h: usize) {
        self.screen_w = w;
        self.screen_h = h;
        self.cursor_x = w / 2;
        self.cursor_y = h / 2;
    }

    #[inline]
    pub const fn cursor_rect(&self) -> Rect {
        if self.cursor_visible {
            let scale = if self.cursor_large { 2 } else { 1 };
            Rect::new(self.cursor_x, self.cursor_y, CURSOR_W * scale, CURSOR_H * scale)
        } else {
            Rect::EMPTY
        }
    }

    #[inline]
    pub const fn topbar_launcher_pill_rect(&self, idx: usize) -> Rect {
        Rect::new(116 + idx * 78, 4, 72, 20)
    }

    #[inline]
    pub fn topbar_mode_pill_rect(&self) -> Rect {
        let pill_w = 112usize;
        let rx = (self.screen_w / 2).saturating_sub(pill_w / 2);
        Rect::new(rx, 4, pill_w, 20)
    }

    #[inline]
    pub fn unlock_button_rect(&self) -> Rect {
        let bw = 296usize;
        let bh = 28usize;
        Rect::new(
            self.screen_w.saturating_sub(bw) / 2,
            (self.screen_h / 2) + 46,
            bw,
            bh,
        )
    }

    pub fn set_session_state(
        &mut self,
        next: SessionVisualState,
        now_tsc: u64,
        scene_dirty: &mut DirtyRegion,
    ) {
        if next != SessionVisualState::Active {
            self.dragging_slot = None;
        }
        if next == SessionVisualState::DisplayBlanked
            && self.session_state != SessionVisualState::DisplayBlanked
        {
            self.dpms_blank_count = self.dpms_blank_count.wrapping_add(1);
        }
        self.last_input_tsc = now_tsc;
        self.session_state = next;
        scene_dirty.mark(Rect::new(0, 0, self.screen_w, self.screen_h));
    }

    #[inline]
    pub fn next_dpms_recv_timeout_tsc(&self, now_tsc: u64) -> u64 {
        if self.session_state != SessionVisualState::Active || self.blank_timeout_tsc == 0 {
            return 0;
        }
        let base = if self.last_input_tsc == 0 {
            now_tsc
        } else {
            self.last_input_tsc
        };
        let deadline = base.saturating_add(self.blank_timeout_tsc);
        deadline.saturating_sub(now_tsc).max(1)
    }

    pub fn set_running_apps_mask(&mut self, mask: u8, dirty: &mut DirtyRegion) {
        if self.running_apps_mask != mask {
            self.running_apps_mask = mask;
            dirty.mark(Rect::new(0, 0, self.screen_w, TOPBAR_H));
        }
    }

    pub fn focus_owner_badge(&mut self, owner_badge: u64, dirty: &mut DirtyRegion) -> Option<u32> {
        let mut z = self.z_len;
        while z > 0 {
            z -= 1;
            let idx = self.z_stack[z];
            let w = &self.windows[idx];
            if w.allocated && w.visible && w.owner_badge == owner_badge {
                let _ = self.focus_and_raise(idx, dirty);
                return Some(self.windows[idx].id);
            }
        }
        None
    }

    #[inline]
    pub fn active_window_count(&self) -> usize {
        self.z_len
    }

    #[inline]
    pub fn focused_slot(&self) -> Option<usize> {
        (0..MAX_WINDOWS).find(|&i| self.windows[i].allocated && self.windows[i].focused)
    }

    #[inline]
    pub fn clamp_window_pos(&self, x: isize, y: isize, win_w: usize, win_h: usize) -> (usize, usize) {
        let max_x = self
            .screen_w
            .saturating_sub(win_w + SHADOW_OFF + 4)
            .max(4) as isize;
        let min_y = (TOPBAR_H + 4) as isize;
        let max_y = self
            .screen_h
            .saturating_sub(win_h + SHADOW_OFF + 4)
            .max(TOPBAR_H + 4) as isize;
        let cx = x.clamp(4, max_x) as usize;
        let cy = y.clamp(min_y, max_y) as usize;
        (cx, cy)
    }

    /// Resolve a `window_id` (`0` = caller's primary owned window) with mandatory badge check.
    pub fn resolve_owned_slot(&mut self, sender_badge: u64, req_win_id: u32) -> Result<usize, WmError> {
        if sender_badge == 0 {
            self.unauthorized_rejects = self.unauthorized_rejects.wrapping_add(1);
            return Err(WmError::PermDenied);
        }
        if req_win_id == 0 {
            for (idx, w) in self.windows.iter().enumerate() {
                if w.allocated && w.owner_badge == sender_badge {
                    return Ok(idx);
                }
            }
            self.unauthorized_rejects = self.unauthorized_rejects.wrapping_add(1);
            return Err(WmError::PermDenied);
        }
        for (idx, w) in self.windows.iter().enumerate() {
            if w.allocated && w.id == req_win_id {
                if w.owner_badge == sender_badge {
                    return Ok(idx);
                }
                self.unauthorized_rejects = self.unauthorized_rejects.wrapping_add(1);
                return Err(WmError::PermDenied);
            }
        }
        self.unauthorized_rejects = self.unauthorized_rejects.wrapping_add(1);
        Err(WmError::PermDenied)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn register_window(
        &mut self,
        owner_badge: u64,
        cols: u16,
        rows: u16,
        req_x: usize,
        req_y: usize,
        role: WindowRole,
        title_kind: u8,
        surf_cap: u64,
        surf_vaddr: u64,
        focus_on_open: bool,
        dirty: &mut DirtyRegion,
    ) -> Result<usize, WmError> {
        if cols == 0
            || rows == 0
            || (cols as usize) > TERM_COLS
            || (rows as usize) > TERM_ROWS
        {
            self.invalid_req_rejects = self.invalid_req_rejects.wrapping_add(1);
            return Err(WmError::InvalidGeometry);
        }

        // Replace existing slot if same owner re-attaches without explicit `window_id` (P1 compat),
        // else claim first free slot.
        let slot_idx = if let Some(free) = (0..MAX_WINDOWS).find(|&i| !self.windows[i].allocated) {
            free
        } else {
            self.invalid_req_rejects = self.invalid_req_rejects.wrapping_add(1);
            return Err(WmError::NoFreeSlots);
        };

        let (win_w, win_h) = compute_window_size(cols, rows);
        let (default_x, default_y) = if req_x == 0 && req_y == 0 {
            let cx = self.screen_w.saturating_sub(win_w + SHADOW_OFF) / 2;
            let cy = TOPBAR_H + self.screen_h.saturating_sub(TOPBAR_H + win_h + SHADOW_OFF) / 2;
            (cx.max(8) as isize, cy.max(TOPBAR_H + 8) as isize)
        } else {
            (req_x as isize, req_y as isize)
        };
        let (wx, wy) = self.clamp_window_pos(default_x, default_y, win_w, win_h);
        let rect = Rect::new(wx, wy, win_w, win_h);

        let win_id = self.next_win_id;
        self.next_win_id = self.next_win_id.wrapping_add(1).max(1);

        let slot = &mut self.windows[slot_idx];
        *slot = WindowSlot::empty();
        slot.allocated = true;
        slot.id = win_id;
        slot.owner_badge = owner_badge;
        slot.role = role;
        slot.title_kind = title_kind;
        slot.visible = true;
        slot.cols = cols;
        slot.rows = rows;
        slot.rect = rect;
        slot.saved_floating_rect = rect;
        slot.surf_cap = surf_cap;
        slot.surf_vaddr = surf_vaddr;

        if focus_on_open || self.z_len == 0 {
            self.z_stack[self.z_len] = slot_idx;
            self.z_len += 1;
            self.focus_and_raise(slot_idx, dirty);
        } else {
            // Insert just below current focused top window so Window A keeps initial keyboard focus
            // when Window B (`dry-sysmon`) attaches!
            let top = self.z_stack[self.z_len - 1];
            self.z_stack[self.z_len - 1] = slot_idx;
            self.z_stack[self.z_len] = top;
            self.z_len += 1;
            dirty.mark(self.windows[slot_idx].outer_rect());
            dirty.mark(Rect::new(0, 0, self.screen_w, TOPBAR_H));
        }

        if self.z_len >= 2 {
            self.desktop_mode = true;
        }
        if self.layout != LayoutMode::Floating {
            self.apply_layout_geometries(dirty);
        } else {
            self.ensure_dialogs_above_normal(dirty);
        }
        Ok(slot_idx)
    }

    /// Focus `slot_idx` and raise it to the top of its role band (`Normal` below `DialogTransient`),
    /// marking changed window frames + topbar (and tab strip in `Tabbed` mode).
    pub fn focus_and_raise(&mut self, slot_idx: usize, dirty: &mut DirtyRegion) -> bool {
        if slot_idx >= MAX_WINDOWS || !self.windows[slot_idx].allocated || !self.windows[slot_idx].visible {
            return false;
        }
        let prev_focus = self.focused_slot();
        let was_top = self.z_len > 0 && self.z_stack[self.z_len - 1] == slot_idx;

        if let Some(pos) = (0..self.z_len).find(|&p| self.z_stack[p] == slot_idx) {
            for i in pos..(self.z_len - 1) {
                self.z_stack[i] = self.z_stack[i + 1];
            }
            self.z_stack[self.z_len - 1] = slot_idx;
        }
        if self.layout != LayoutMode::Floating {
            self.ensure_dialogs_above_normal(dirty);
        }
        if !was_top && self.z_len > 1 {
            self.z_raises = self.z_raises.wrapping_add(1);
        }

        for (i, w) in self.windows.iter_mut().enumerate() {
            let new_f = i == slot_idx;
            if w.allocated && w.focused != new_f {
                w.focused = new_f;
                dirty.mark(w.outer_rect());
            }
        }
        if !was_top {
            dirty.mark(self.windows[slot_idx].outer_rect());
        }
        if self.layout == LayoutMode::Tabbed {
            dirty.mark(self.tabstrip_rect());
        } else if self.layout == LayoutMode::Stacked {
            self.apply_layout_geometries(dirty);
        }
        if prev_focus != Some(slot_idx) {
            self.focus_switches = self.focus_switches.wrapping_add(1);
            dirty.mark(Rect::new(0, 0, self.screen_w, TOPBAR_H));
            return true;
        }
        !was_top
    }

    /// Remove `slot_idx` from Z-order, return its `(surf_cap, surf_vaddr, notif_cap, waiter_tok)`
    /// for immediate kernel unmap/revoke, and focus the new topmost window if any remain.
    pub fn remove_window_slot(
        &mut self,
        slot_idx: usize,
        dirty: &mut DirtyRegion,
    ) -> Option<(u64, u64, u64, u64)> {
        if slot_idx >= MAX_WINDOWS || !self.windows[slot_idx].allocated {
            return None;
        }
        if self.dragging_slot == Some(slot_idx) {
            self.dragging_slot = None;
        }
        let old_outer = self.windows[slot_idx].outer_rect();
        let res = (
            self.windows[slot_idx].surf_cap,
            self.windows[slot_idx].surf_vaddr,
            self.windows[slot_idx].notif_cap,
            self.windows[slot_idx].waiter_reply_tok,
        );
        self.windows[slot_idx] = WindowSlot::empty();

        if let Some(pos) = (0..self.z_len).find(|&p| self.z_stack[p] == slot_idx) {
            for i in pos..(self.z_len - 1) {
                self.z_stack[i] = self.z_stack[i + 1];
            }
            self.z_len -= 1;
        }
        dirty.mark(old_outer);
        dirty.mark(Rect::new(0, 0, self.screen_w, TOPBAR_H));

        if self.z_len > 0 {
            let new_top = self.z_stack[self.z_len - 1];
            let _ = self.focus_and_raise(new_top, dirty);
        }
        if self.layout != LayoutMode::Floating {
            self.apply_layout_geometries(dirty);
        }
        Some(res)
    }

    pub fn hit_test_topmost(&self, px: usize, py: usize) -> Option<(usize, HitPart)> {
        let mut z = self.z_len;
        while z > 0 {
            z -= 1;
            let idx = self.z_stack[z];
            let w = &self.windows[idx];
            if !w.allocated || !w.visible {
                continue;
            }
            if w.rect.contains(px, py) {
                if w.close_btn_rect().contains(px, py) {
                    return Some((idx, HitPart::CloseButton));
                }
                if w.titlebar_rect().contains(px, py) || py < w.rect.y + WIN_BORDER + WIN_TITLE_H {
                    return Some((idx, HitPart::TitleBar));
                }
                return Some((idx, HitPart::ClientBody));
            }
        }
        None
    }

    pub fn move_window_by(
        &mut self,
        slot_idx: usize,
        dx: isize,
        dy: isize,
        dirty: &mut DirtyRegion,
    ) -> (usize, usize) {
        if slot_idx >= MAX_WINDOWS || !self.windows[slot_idx].allocated {
            return (0, 0);
        }
        if self.layout != LayoutMode::Floating
            && self.windows[slot_idx].role == WindowRole::Normal
        {
            self.set_layout_mode(LayoutMode::Floating, dirty);
        }
        let old_outer = self.windows[slot_idx].outer_rect();
        let cur = self.windows[slot_idx].rect;
        let (nx, ny) = self.clamp_window_pos(
            (cur.x as isize).saturating_add(dx),
            (cur.y as isize).saturating_add(dy),
            cur.w,
            cur.h,
        );
        self.windows[slot_idx].rect.x = nx;
        self.windows[slot_idx].rect.y = ny;
        self.windows[slot_idx].saved_floating_rect = self.windows[slot_idx].rect;
        let new_outer = self.windows[slot_idx].outer_rect();
        dirty.mark(old_outer);
        dirty.mark(new_outer);
        (nx, ny)
    }

    /// Cycle focus to the next visible window (`Alt + Tab`).
    pub fn cycle_focus_next(&mut self, dirty: &mut DirtyRegion) -> Option<u32> {
        if self.z_len < 2 {
            return self.focused_slot().map(|s| self.windows[s].id);
        }
        // Bottom-most window in `z_stack[0]` rotates to top focused position so all windows cycle fairly!
        let next_slot = self.z_stack[0];
        let _ = self.focus_and_raise(next_slot, dirty);
        Some(self.windows[next_slot].id)
    }

    #[inline]
    fn queue_desktop_action(&mut self, act: DesktopAction) -> WmInputAction {
        if self.action_queue.push(act) {
            WmInputAction::DesktopActionQueued(act)
        } else {
            WmInputAction::None
        }
    }

    /// Process a normalized `InputEvent` from `dry-input` (`BADGE_INPUT_SVC`).
    /// `scene_dirty` receives rects requiring `back_buf` re-rasterization; `cursor_dirty` receives
    /// cursor-only MMIO blit rects (`old_cursor_rect`, `new_cursor_rect`).
    pub fn handle_input_event(
        &mut self,
        ev: InputEvent,
        scene_dirty: &mut DirtyRegion,
        cursor_dirty: &mut DirtyRegion,
    ) -> WmInputAction {
        let now_tsc = crate::rdtsc_user();
        self.last_input_tsc = now_tsc;

        if self.session_state == SessionVisualState::DisplayBlanked {
            match ev.kind {
                InputEventKind::KeyPress
                | InputEventKind::MouseMove
                | InputEventKind::MouseButtonDown
                | InputEventKind::MouseScroll => {
                    self.session_state = SessionVisualState::Active;
                    self.dpms_wake_count = self.dpms_wake_count.wrapping_add(1);
                    scene_dirty.mark(Rect::new(0, 0, self.screen_w, self.screen_h));
                    return WmInputAction::DpmsWoke;
                }
                _ => return WmInputAction::None,
            }
        }

        if self.session_state == SessionVisualState::SessionLocked {
            match ev.kind {
                InputEventKind::MouseMove => {
                    let old_c = self.cursor_rect();
                    self.cursor_visible = true;
                    self.desktop_mode = true;
                    let max_x = (self.screen_w.saturating_sub(1)) as isize;
                    let max_y = (self.screen_h.saturating_sub(1)) as isize;
                    let raw_x = (self.cursor_x as isize) + (ev.dx as isize);
                    let raw_y = (self.cursor_y as isize) + (ev.dy as isize);
                    self.cursor_x = raw_x.clamp(0, max_x) as usize;
                    self.cursor_y = raw_y.clamp(0, max_y) as usize;
                    cursor_dirty.mark(old_c);
                    cursor_dirty.mark(self.cursor_rect());
                    return WmInputAction::CursorMoved;
                }
                InputEventKind::MouseButtonDown => {
                    if !self.cursor_visible {
                        self.cursor_visible = true;
                        cursor_dirty.mark(self.cursor_rect());
                    }
                    self.buttons_down = ev.buttons;
                    if (ev.scancode as u8) == MOUSE_BTN_LEFT
                        && self.unlock_button_rect().contains(self.cursor_x, self.cursor_y)
                    {
                        self.session_state = SessionVisualState::Active;
                        scene_dirty.mark(Rect::new(0, 0, self.screen_w, self.screen_h));
                        return self.queue_desktop_action(DesktopAction::UnlockSession);
                    }
                    self.locked_blocked_events = self.locked_blocked_events.wrapping_add(1);
                    scene_dirty.mark(Rect::new(0, self.screen_h / 2, self.screen_w, 96));
                    return WmInputAction::LockBlocked;
                }
                InputEventKind::KeyPress => {
                    if ev.keycode == KEY_ENTER {
                        self.session_state = SessionVisualState::Active;
                        scene_dirty.mark(Rect::new(0, 0, self.screen_w, self.screen_h));
                        return self.queue_desktop_action(DesktopAction::UnlockSession);
                    }
                    self.locked_blocked_events = self.locked_blocked_events.wrapping_add(1);
                    scene_dirty.mark(Rect::new(0, self.screen_h / 2, self.screen_w, 96));
                    return WmInputAction::LockBlocked;
                }
                InputEventKind::MouseScroll => {
                    self.locked_blocked_events = self.locked_blocked_events.wrapping_add(1);
                    scene_dirty.mark(Rect::new(0, self.screen_h / 2, self.screen_w, 96));
                    return WmInputAction::LockBlocked;
                }
                _ => return WmInputAction::None,
            }
        }

        match ev.kind {
            InputEventKind::MouseMove => {
                let old_c = self.cursor_rect();
                self.cursor_visible = true;
                self.desktop_mode = true;

                let raw_x = (self.cursor_x as isize) + (ev.dx as isize);
                let raw_y = (self.cursor_y as isize) + (ev.dy as isize);
                let max_x = (self.screen_w.saturating_sub(1)) as isize;
                let max_y = (self.screen_h.saturating_sub(1)) as isize;
                if raw_x < 0 || raw_x > max_x || raw_y < 0 || raw_y > max_y {
                    self.cursor_clamp_hits = self.cursor_clamp_hits.wrapping_add(1);
                }
                self.cursor_x = raw_x.clamp(0, max_x) as usize;
                self.cursor_y = raw_y.clamp(0, max_y) as usize;
                let new_c = self.cursor_rect();
                cursor_dirty.mark(old_c);
                cursor_dirty.mark(new_c);

                if let Some(d_slot) = self.dragging_slot {
                    if self.windows[d_slot].allocated {
                        let old_outer = self.windows[d_slot].outer_rect();
                        let rw = self.windows[d_slot].rect.w;
                        let rh = self.windows[d_slot].rect.h;
                        let target_x = (self.cursor_x as isize) - (self.drag_off_x as isize);
                        let target_y = (self.cursor_y as isize) - (self.drag_off_y as isize);
                        let (nx, ny) = self.clamp_window_pos(target_x, target_y, rw, rh);
                        if nx != self.windows[d_slot].rect.x || ny != self.windows[d_slot].rect.y {
                            self.windows[d_slot].rect.x = nx;
                            self.windows[d_slot].rect.y = ny;
                            self.windows[d_slot].saved_floating_rect = self.windows[d_slot].rect;
                            self.drag_moves = self.drag_moves.wrapping_add(1);
                            scene_dirty.mark(old_outer);
                            scene_dirty.mark(self.windows[d_slot].outer_rect());
                            return WmInputAction::WindowDragged(self.windows[d_slot].id);
                        }
                    }
                }
                WmInputAction::CursorMoved
            }
            InputEventKind::MouseButtonDown => {
                if !self.cursor_visible {
                    self.cursor_visible = true;
                    cursor_dirty.mark(self.cursor_rect());
                }
                self.buttons_down = ev.buttons;
                if (ev.scancode as u8) != MOUSE_BTN_LEFT {
                    return WmInputAction::None;
                }
                if self.cursor_y < TOPBAR_H {
                    if self.p3_mode {
                        for idx in 0..APP_COUNT {
                            if self
                                .topbar_launcher_pill_rect(idx)
                                .contains(self.cursor_x, self.cursor_y)
                            {
                                return self
                                    .queue_desktop_action(DesktopAction::LaunchOrFocusApp(AppId::ALL[idx]));
                            }
                        }
                    }
                    if self.topbar_mode_pill_rect().contains(self.cursor_x, self.cursor_y) {
                        let m = if self.p3_mode {
                            self.set_layout_mode(self.layout.next_mode(), scene_dirty)
                        } else {
                            self.toggle_tiling(scene_dirty)
                        };
                        return WmInputAction::LayoutToggled(m);
                    }
                }
                if let Some(tab_slot) = self.hit_test_tabstrip(self.cursor_x, self.cursor_y) {
                    let _ = self.focus_and_raise(tab_slot, scene_dirty);
                    return WmInputAction::FocusChanged(self.windows[tab_slot].id);
                }
                if let Some((slot_idx, part)) = self.hit_test_topmost(self.cursor_x, self.cursor_y) {
                    let focus_changed = self.focus_and_raise(slot_idx, scene_dirty);
                    let win_id = self.windows[slot_idx].id;
                    match part {
                        HitPart::CloseButton => {
                            self.windows[slot_idx].close_requested = true;
                            return WmInputAction::CloseRequested(win_id);
                        }
                        HitPart::TitleBar => {
                            if self.layout != LayoutMode::Floating
                                && self.windows[slot_idx].role == WindowRole::Normal
                            {
                                self.set_layout_mode(LayoutMode::Floating, scene_dirty);
                            }
                            let wx = self.windows[slot_idx].rect.x;
                            let wy = self.windows[slot_idx].rect.y;
                            self.dragging_slot = Some(slot_idx);
                            self.drag_off_x = self.cursor_x.saturating_sub(wx);
                            self.drag_off_y = self.cursor_y.saturating_sub(wy);
                            return WmInputAction::FocusChanged(win_id);
                        }
                        HitPart::ClientBody => {
                            if focus_changed {
                                return WmInputAction::FocusChanged(win_id);
                            }
                        }
                    }
                }
                WmInputAction::None
            }
            InputEventKind::MouseButtonUp => {
                self.buttons_down = ev.buttons;
                if (ev.scancode as u8) == MOUSE_BTN_LEFT {
                    if let Some(d_slot) = self.dragging_slot.take() {
                        if self.windows[d_slot].allocated {
                            return WmInputAction::DragEnded(
                                self.windows[d_slot].id,
                                self.windows[d_slot].rect.x,
                                self.windows[d_slot].rect.y,
                            );
                        }
                    }
                }
                WmInputAction::None
            }
            InputEventKind::MouseScroll => {
                let target_slot = self
                    .hit_test_topmost(self.cursor_x, self.cursor_y)
                    .map(|(s, _)| s)
                    .or_else(|| self.focused_slot());
                if let Some(s) = target_slot {
                    let _ = self.windows[s].ev_queue.push(ev);
                    return WmInputAction::WheelRouted(self.windows[s].id);
                }
                WmInputAction::None
            }
            InputEventKind::KeyPress => {
                if (ev.modifiers & MOD_ALT) != 0 {
                    if self.p3_mode {
                        let scan7 = (ev.scancode & 0x7f) as u8;
                        let maybe_app = match (ev.ascii, scan7) {
                            (b'1', _) | (_, 0x02) => Some(AppId::Terminal),
                            (b'2', _) | (_, 0x03) => Some(AppId::LifeStation),
                            (b'3', _) | (_, 0x04) => Some(AppId::Files),
                            (b'4', _) | (_, 0x05) => Some(AppId::Settings),
                            _ => None,
                        };
                        if let Some(app) = maybe_app {
                            return self.queue_desktop_action(DesktopAction::LaunchOrFocusApp(app));
                        }
                        if ev.ascii == b'l' || ev.ascii == b'L' || scan7 == 0x26 {
                            return self.queue_desktop_action(DesktopAction::LockSession);
                        }
                    }
                    if ev.keycode == KEY_TAB {
                        if let Some(wid) = self.cycle_focus_next(scene_dirty) {
                            return WmInputAction::FocusChanged(wid);
                        }
                        return WmInputAction::None;
                    }
                    if ev.ascii == b't' || ev.ascii == b'T' || (ev.scancode & 0x7f) == 0x14 {
                        let m = self.toggle_tiling(scene_dirty);
                        return WmInputAction::LayoutToggled(m);
                    }
                    if self.p3_mode && (ev.ascii == b'g' || ev.ascii == b'G' || (ev.scancode & 0x7f) == 0x22) {
                        let m = self.set_layout_mode(self.layout.next_mode(), scene_dirty);
                        return WmInputAction::LayoutToggled(m);
                    }
                    let delta = match ev.keycode {
                        KEY_LEFT => Some((-32isize, 0isize)),
                        KEY_RIGHT => Some((32, 0)),
                        KEY_UP => Some((0, -24)),
                        KEY_DOWN => Some((0, 24)),
                        _ => None,
                    };
                    if let Some((dx, dy)) = delta {
                        if let Some(f_slot) = self.focused_slot() {
                            let wid = self.windows[f_slot].id;
                            let (nx, ny) = self.move_window_by(f_slot, dx, dy, scene_dirty);
                            return WmInputAction::KeyMoved(wid, nx, ny);
                        }
                    }
                    return WmInputAction::None;
                }

                if let Some(f_slot) = self.focused_slot() {
                    let _ = self.windows[f_slot].ev_queue.push(ev);
                    return WmInputAction::KeyRouted(self.windows[f_slot].id);
                }
                WmInputAction::None
            }
            _ => WmInputAction::None,
        }
    }
}

impl Default for WindowManager {
    fn default() -> Self {
        Self::new()
    }
}
