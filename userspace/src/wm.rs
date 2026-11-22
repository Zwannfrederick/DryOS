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
use crate::glass::GlassState;
use crate::gpu_svc::{DisplayHandoverMachine, FenceTimeline, VblankPacer};
use crate::input::InputEvent;
use crate::surface::{
    PIXEL_SURF_MAX_H, PIXEL_SURF_MAX_PIXELS, PIXEL_SURF_MAX_W, TERM_COLS, TERM_ROWS,
};
use crate::vault_crypto::scrub_bytes;

pub use crate::wm_types::*;

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
    pub light_mode: bool,
    pub low_overhead_glass: bool,
    pub dpi_scale: u8,
    pub cursor_x: usize,
    pub cursor_y: usize,
    pub cursor_visible: bool,
    pub buttons_down: u8,
    pub next_win_id: u32,
    pub windows: [WindowSlot; MAX_WINDOWS],
    /// Bottom-to-top Z-order indices into `windows` (`z_stack[z_len - 1]` is topmost).
    pub z_stack: [usize; MAX_WINDOWS],
    pub z_len: usize,
    /// Most-recently-used apps (index 0 = most recent active/focused; closed recent apps retained at tail).
    pub mru_apps: [AppId; APP_COUNT],
    pub mru_len: u8,
    pub dragging_slot: Option<usize>,
    pub drag_off_x: usize,
    pub drag_off_y: usize,
    pub pointer_capture_slot: Option<usize>,
    pub hover_slot: Option<usize>,
    pub unauthorized_rejects: u64,
    pub invalid_req_rejects: u64,
    pub cursor_clamp_hits: u64,
    pub focus_switches: u64,
    pub z_raises: u64,
    pub drag_moves: u64,
    pub dpms_blank_count: u64,
    pub dpms_wake_count: u64,
    pub locked_blocked_events: u64,
    pub vault_auth_required: bool,
    pub lock_pin_buf: [u8; 32],
    pub lock_pin_len: u8,
    pub vault_auth_failures: u32,
    pub gpu_ep_cap: u64,
    pub gpu_presents: u64,
    pub gpu_fallbacks: u64,
    pub handover: DisplayHandoverMachine,
    pub gpu_fences: FenceTimeline,
    pub vblank: VblankPacer,
    pub broker_in_data_path: bool,
    pub back_caps: [u64; 8],
    pub glass: GlassState,
    pub clip_buf: [u8; 512],
    pub clip_len: u16,
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
            light_mode: false,
            low_overhead_glass: false,
            dpi_scale: 0,
            cursor_x: 640,
            cursor_y: 400,
            cursor_visible: false,
            buttons_down: 0,
            next_win_id: 1,
            windows: [const { WindowSlot::empty() }; MAX_WINDOWS],
            z_stack: [0; MAX_WINDOWS],
            z_len: 0,
            mru_apps: [AppId::Terminal; APP_COUNT],
            mru_len: 0,
            dragging_slot: None,
            drag_off_x: 0,
            drag_off_y: 0,
            pointer_capture_slot: None,
            hover_slot: None,
            unauthorized_rejects: 0,
            invalid_req_rejects: 0,
            cursor_clamp_hits: 0,
            focus_switches: 0,
            z_raises: 0,
            drag_moves: 0,
            dpms_blank_count: 0,
            dpms_wake_count: 0,
            locked_blocked_events: 0,
            vault_auth_required: false,
            lock_pin_buf: [0; 32],
            lock_pin_len: 0,
            vault_auth_failures: 0,
            gpu_ep_cap: 0,
            gpu_presents: 0,
            gpu_fallbacks: 0,
            handover: DisplayHandoverMachine::new(),
            gpu_fences: FenceTimeline::new(),
            vblank: VblankPacer::new(3000, 60),
            broker_in_data_path: false,
            back_caps: [0; 8],
            glass: GlassState::new(),
            clip_buf: [0; 512],
            clip_len: 0,
        }
    }

    pub fn clear_lock_pin(&mut self) {
        scrub_bytes(&mut self.lock_pin_buf);
        self.lock_pin_len = 0;
    }

    pub fn take_lock_pin(&mut self) -> ([u8; 32], usize) {
        let len = (self.lock_pin_len as usize).min(32);
        let mut out = [0u8; 32];
        out[..len].copy_from_slice(&self.lock_pin_buf[..len]);
        self.clear_lock_pin();
        (out, len)
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
        if self.screen_w < 1200 {
            Rect::new(90 + idx * 60, 4, 56, 20)
        } else {
            Rect::new(116 + idx * 78, 4, 72, 20)
        }
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
        if next == SessionVisualState::SessionLocked || self.session_state == SessionVisualState::SessionLocked {
            self.clear_lock_pin();
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

    #[inline]
    pub fn slot_app_id(slot: &WindowSlot) -> Option<AppId> {
        AppId::from_win_badge(slot.owner_badge)
            .or_else(|| AppId::from_title_kind(slot.title_kind))
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
    fn insert_slot(
        &mut self,
        owner_badge: u64,
        win_w: usize,
        win_h: usize,
        cols: u16,
        rows: u16,
        is_pixel: bool,
        pixel_w: u16,
        pixel_h: u16,
        req_x: usize,
        req_y: usize,
        role: WindowRole,
        title_kind: u8,
        surf_cap: u64,
        surf_vaddr: u64,
        snap_cap: u64,
        snap_vaddr: u64,
        focus_on_open: bool,
        dirty: &mut DirtyRegion,
    ) -> Result<usize, WmError> {
        let Some(slot_idx) = (0..MAX_WINDOWS).find(|&i| !self.windows[i].allocated) else {
            self.invalid_req_rejects = self.invalid_req_rejects.wrapping_add(1);
            return Err(WmError::NoFreeSlots);
        };
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
        let cfg_flags = self.theme_flags_byte();
        let owner_app = AppId::from_win_badge(owner_badge).or_else(|| AppId::from_title_kind(title_kind));
        let same_app_count = if let Some(app) = owner_app {
            self.windows
                .iter()
                .filter(|w| w.allocated && Self::slot_app_id(w) == Some(app))
                .count()
        } else {
            0
        };
        let ordinal = (same_app_count + 1).min(255) as u8;

        let slot = &mut self.windows[slot_idx];
        slot.reset_in_place();
        slot.allocated = true;
        slot.id = win_id;
        slot.owner_badge = owner_badge;
        slot.role = role;
        slot.title_kind = title_kind;
        slot.visible = true;
        slot.minimized = false;
        slot.instance_ordinal = ordinal;
        if ordinal > 1 && title_kind != TITLE_KIND_DIALOG {
            let base: &[u8] = match title_kind {
                TITLE_KIND_SYSMON => b"Life Station",
                TITLE_KIND_TERMINAL => b"DRY Terminal",
                TITLE_KIND_FILES => b"DRY Files",
                TITLE_KIND_SETTINGS => b"Settings",
                TITLE_KIND_EDITOR => b"DRY Notes",
                TITLE_KIND_CALC => b"Calculator",
                _ => b"Window",
            };
            let mut buf = [0u8; 32];
            let n = base.len().min(26);
            buf[..n].copy_from_slice(&base[..n]);
            let mut p = n;
            buf[p] = b' ';
            buf[p + 1] = b'#';
            p += 2;
            if ordinal >= 10 {
                buf[p] = b'0' + (ordinal / 10);
                p += 1;
            }
            buf[p] = b'0' + (ordinal % 10);
            p += 1;
            slot.set_custom_title(&buf[..p]);
        }
        slot.cols = cols;
        slot.rows = rows;
        slot.rect = rect;
        slot.saved_floating_rect = rect;
        slot.surf_cap = surf_cap;
        slot.surf_vaddr = surf_vaddr;
        slot.is_pixel = is_pixel;
        slot.pixel_w = pixel_w;
        slot.pixel_h = pixel_h;
        slot.snap_cap = snap_cap;
        slot.snap_vaddr = snap_vaddr;
        slot.last_cfg_w = pixel_w;
        slot.last_cfg_h = pixel_h;
        slot.last_cfg_flags = cfg_flags;

        if focus_on_open || self.z_len == 0 {
            self.z_stack[self.z_len] = slot_idx;
            self.z_len += 1;
            self.focus_and_raise(slot_idx, dirty);
        } else {
            if let Some(app) = owner_app {
                self.note_app_mru_background(app);
            }
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
        let (win_w, win_h) = compute_window_size(cols, rows);
        self.insert_slot(
            owner_badge,
            win_w,
            win_h,
            cols,
            rows,
            false,
            0,
            0,
            req_x,
            req_y,
            role,
            title_kind,
            surf_cap,
            surf_vaddr,
            0,
            0,
            focus_on_open,
            dirty,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn register_pixel_window(
        &mut self,
        owner_badge: u64,
        pixel_w: u16,
        pixel_h: u16,
        req_x: usize,
        req_y: usize,
        role: WindowRole,
        title_kind: u8,
        surf_cap: u64,
        surf_vaddr: u64,
        snap_cap: u64,
        snap_vaddr: u64,
        focus_on_open: bool,
        dirty: &mut DirtyRegion,
    ) -> Result<usize, WmError> {
        let pw = pixel_w as usize;
        let ph = pixel_h as usize;
        if pw < 160
            || ph < 120
            || pw > PIXEL_SURF_MAX_W
            || ph > PIXEL_SURF_MAX_H
            || pw * ph > PIXEL_SURF_MAX_PIXELS
        {
            self.invalid_req_rejects = self.invalid_req_rejects.wrapping_add(1);
            return Err(WmError::InvalidGeometry);
        }
        let (win_w, win_h) = compute_pixel_window_size(pixel_w, pixel_h);
        self.insert_slot(
            owner_badge,
            win_w,
            win_h,
            (pw / 8).clamp(1, TERM_COLS) as u16,
            (ph / 16).clamp(1, TERM_ROWS) as u16,
            true,
            pixel_w,
            pixel_h,
            req_x,
            req_y,
            role,
            title_kind,
            surf_cap,
            surf_vaddr,
            snap_cap,
            snap_vaddr,
            focus_on_open,
            dirty,
        )
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
            if let Some(app) = Self::slot_app_id(&self.windows[slot_idx]) {
                self.note_app_mru(app);
            }
            return true;
        }
        if let Some(app) = Self::slot_app_id(&self.windows[slot_idx]) {
            self.note_app_mru(app);
        }
        !was_top
    }

    /// Remove `slot_idx` from Z-order, return its mapped caps/addresses for immediate kernel
    /// unmap/revoke, and focus the new topmost window if any remain.
    pub fn remove_window_slot(
        &mut self,
        slot_idx: usize,
        dirty: &mut DirtyRegion,
    ) -> Option<RemovedWindowSlot> {
        if slot_idx >= MAX_WINDOWS || !self.windows[slot_idx].allocated {
            return None;
        }
        if self.dragging_slot == Some(slot_idx) {
            self.dragging_slot = None;
        }
        if self.pointer_capture_slot == Some(slot_idx) {
            self.pointer_capture_slot = None;
        }
        if self.hover_slot == Some(slot_idx) {
            self.hover_slot = None;
        }
        let old_outer = self.windows[slot_idx].outer_rect();
        let res = RemovedWindowSlot {
            surf_cap: self.windows[slot_idx].surf_cap,
            surf_vaddr: self.windows[slot_idx].surf_vaddr,
            is_pixel: self.windows[slot_idx].is_pixel,
            snap_cap: self.windows[slot_idx].snap_cap,
            snap_vaddr: self.windows[slot_idx].snap_vaddr,
            notif_cap: self.windows[slot_idx].notif_cap,
            waiter_tok: self.windows[slot_idx].waiter_reply_tok,
        };
        self.windows[slot_idx].reset_in_place();

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
            if self.windows[new_top].allocated && self.windows[new_top].visible {
                let _ = self.focus_and_raise(new_top, dirty);
            }
        }
        if self.layout != LayoutMode::Floating {
            self.apply_layout_geometries(dirty);
        }
        Some(res)
    }

    #[inline]
    pub fn enqueue_desktop_action(&mut self, act: DesktopAction) -> WmInputAction {
        if self.action_queue.push(act) {
            WmInputAction::DesktopActionQueued(act)
        } else {
            WmInputAction::None
        }
    }

    #[inline]
    pub const fn dpi_pct(&self) -> u16 {
        match self.dpi_scale {
            1 => 125,
            2 => 150,
            _ => 100,
        }
    }

    #[inline]
    pub const fn theme_flags_byte(&self) -> u8 {
        (self.high_contrast as u8)
            | ((self.cursor_large as u8) << 1)
            | ((self.light_mode as u8) << 2)
            | ((self.low_overhead_glass as u8) << 3)
            | ((self.dpi_scale & 3) << 4)
    }

    /// Push coalesced `WindowConfig` notifications to any `is_pixel` window whose target client
    /// size or theme/DPI flags changed since its last config delivery.
    pub fn sync_pixel_window_configs(&mut self) -> bool {
        let flags = self.theme_flags_byte();
        let mut pushed = false;
        for w in &mut self.windows {
            if !w.allocated || !w.is_pixel {
                continue;
            }
            let (cw, ch) = client_dims_for_window_rect(w.rect.w, w.rect.h);
            if cw != w.last_cfg_w || ch != w.last_cfg_h || flags != w.last_cfg_flags {
                w.last_cfg_w = cw;
                w.last_cfg_h = ch;
                w.last_cfg_flags = flags;
                if w
                    .ev_queue
                    .push_replacing_abs(InputEvent::window_config(cw, ch, flags))
                {
                    pushed = true;
                }
            }
        }
        pushed
    }
}

impl Default for WindowManager {
    fn default() -> Self {
        Self::new()
    }
}
