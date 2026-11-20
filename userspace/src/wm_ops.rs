// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Extracted MRU, focus cycling, minimize/restore, hit-testing & floating move operations
//! for [`WindowManager`] (keeps `wm.rs` focused on core window lifecycle and IPC handling).

use crate::action::{AppId, APP_COUNT};
use crate::gfx::{DirtyRegion, Rect};
use crate::wm::WindowManager;
use crate::wm_types::{HitPart, LayoutMode, WindowRole, MAX_WINDOWS, TOPBAR_H, WIN_BORDER, WIN_TITLE_H};

impl WindowManager {
    /// Record `app` as most recently used at the front of `mru_apps` (for the Adaptive Window Strip).
    pub fn note_app_mru(&mut self, app: AppId) {
        let len = (self.mru_len as usize).min(APP_COUNT);
        if let Some(pos) = (0..len).find(|&i| self.mru_apps[i] == app) {
            for j in (1..=pos).rev() {
                self.mru_apps[j] = self.mru_apps[j - 1];
            }
            self.mru_apps[0] = app;
            return;
        }
        let new_len = len.min(APP_COUNT - 1);
        for j in (1..=new_len).rev() {
            self.mru_apps[j] = self.mru_apps[j - 1];
        }
        self.mru_apps[0] = app;
        if len < APP_COUNT {
            self.mru_len = (len + 1) as u8;
        }
    }

    /// Append `app` into `mru_apps` behind the current head if not already present (used when
    /// a background window opens without stealing focus).
    pub fn note_app_mru_background(&mut self, app: AppId) {
        let len = (self.mru_len as usize).min(APP_COUNT);
        if (0..len).any(|i| self.mru_apps[i] == app) {
            return;
        }
        if len < APP_COUNT {
            self.mru_apps[len] = app;
            self.mru_len = (len + 1) as u8;
        }
    }

    /// Returns `(total_allocated, minimized_count)` for `app`.
    pub fn count_windows_for_app(&self, app: AppId) -> (usize, usize) {
        let mut total = 0usize;
        let mut minimized = 0usize;
        for w in &self.windows {
            if w.allocated && Self::slot_app_id(w) == Some(app) {
                total += 1;
                if w.minimized || !w.visible {
                    minimized += 1;
                }
            }
        }
        (total, minimized)
    }

    /// Cycle through windows belonging to `app` (or restore minimized window of `app`).
    pub fn focus_or_cycle_app(&mut self, app: AppId, dirty: &mut DirtyRegion) -> Option<u32> {
        let cur_focus = self.focused_slot();
        if let Some(f_idx) = cur_focus {
            if Self::slot_app_id(&self.windows[f_idx]) == Some(app) {
                // Find the lowest visible window of `app` in z_stack to cycle within the app group.
                for z in 0..self.z_len {
                    let idx = self.z_stack[z];
                    let w = &self.windows[idx];
                    if w.allocated && w.visible && idx != f_idx && Self::slot_app_id(w) == Some(app) {
                        let _ = self.focus_and_raise(idx, dirty);
                        return Some(self.windows[idx].id);
                    }
                }
            }
        }
        let mut z = self.z_len;
        while z > 0 {
            z -= 1;
            let idx = self.z_stack[z];
            let w = &self.windows[idx];
            if w.allocated && w.visible && Self::slot_app_id(w) == Some(app) {
                let _ = self.focus_and_raise(idx, dirty);
                return Some(self.windows[idx].id);
            }
        }
        let mut z2 = self.z_len;
        while z2 > 0 {
            z2 -= 1;
            let idx = self.z_stack[z2];
            let w = &self.windows[idx];
            if w.allocated && Self::slot_app_id(w) == Some(app) {
                let _ = self.unminimize_and_focus_slot(idx, dirty);
                return Some(self.windows[idx].id);
            }
        }
        None
    }

    pub fn focus_owner_badge(&mut self, owner_badge: u64, dirty: &mut DirtyRegion) -> Option<u32> {
        if let Some(f_idx) = self.focused_slot() {
            if self.windows[f_idx].owner_badge == owner_badge {
                for z in 0..self.z_len {
                    let idx = self.z_stack[z];
                    let w = &self.windows[idx];
                    if w.allocated && w.visible && idx != f_idx && w.owner_badge == owner_badge {
                        let _ = self.focus_and_raise(idx, dirty);
                        return Some(self.windows[idx].id);
                    }
                }
            }
        }
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
        let mut z2 = self.z_len;
        while z2 > 0 {
            z2 -= 1;
            let idx = self.z_stack[z2];
            let w = &self.windows[idx];
            if w.allocated && w.owner_badge == owner_badge {
                let _ = self.unminimize_and_focus_slot(idx, dirty);
                return Some(self.windows[idx].id);
            }
        }
        None
    }

    /// Minimize `slot_idx` without terminating its backing process; move it to bottom of `z_stack`.
    pub fn minimize_slot(&mut self, slot_idx: usize, dirty: &mut DirtyRegion) -> bool {
        if slot_idx >= MAX_WINDOWS
            || !self.windows[slot_idx].allocated
            || !self.windows[slot_idx].visible
        {
            return false;
        }
        let old_outer = self.windows[slot_idx].outer_rect();
        if self.dragging_slot == Some(slot_idx) {
            self.dragging_slot = None;
        }
        if self.pointer_capture_slot == Some(slot_idx) {
            self.pointer_capture_slot = None;
        }
        if self.hover_slot == Some(slot_idx) {
            self.hover_slot = None;
        }
        self.windows[slot_idx].visible = false;
        self.windows[slot_idx].minimized = true;
        self.windows[slot_idx].focused = false;

        if let Some(pos) = (0..self.z_len).find(|&p| self.z_stack[p] == slot_idx) {
            for i in (1..=pos).rev() {
                self.z_stack[i] = self.z_stack[i - 1];
            }
            self.z_stack[0] = slot_idx;
        }
        dirty.mark(old_outer);
        dirty.mark(Rect::new(0, 0, self.screen_w, TOPBAR_H));

        if self.z_len > 0 {
            let top = self.z_stack[self.z_len - 1];
            if self.windows[top].allocated && self.windows[top].visible {
                let _ = self.focus_and_raise(top, dirty);
            }
        }
        if self.layout != LayoutMode::Floating {
            self.apply_layout_geometries(dirty);
        }
        true
    }

    /// Restore a minimized window slot and focus/raise it to the top.
    pub fn unminimize_and_focus_slot(&mut self, slot_idx: usize, dirty: &mut DirtyRegion) -> bool {
        if slot_idx >= MAX_WINDOWS || !self.windows[slot_idx].allocated {
            return false;
        }
        let was_min = !self.windows[slot_idx].visible || self.windows[slot_idx].minimized;
        self.windows[slot_idx].visible = true;
        self.windows[slot_idx].minimized = false;
        let changed = self.focus_and_raise(slot_idx, dirty);
        if was_min {
            if self.layout != LayoutMode::Floating {
                self.apply_layout_geometries(dirty);
            }
            dirty.mark(self.windows[slot_idx].outer_rect());
            dirty.mark(Rect::new(0, 0, self.screen_w, TOPBAR_H));
        }
        changed || was_min
    }

    /// Focus or restore a window by its stable `WindowSlot::id`.
    pub fn focus_or_restore_window_id(&mut self, win_id: u32, dirty: &mut DirtyRegion) -> bool {
        if let Some(slot_idx) = (0..MAX_WINDOWS).find(|&i| self.windows[i].allocated && self.windows[i].id == win_id) {
            return self.unminimize_and_focus_slot(slot_idx, dirty);
        }
        false
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
                if w.minimize_btn_rect().contains(px, py) {
                    return Some((idx, HitPart::MinimizeButton));
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
        let mut visible_count = 0usize;
        let mut lowest_vis: Option<usize> = None;
        for z in 0..self.z_len {
            let idx = self.z_stack[z];
            if self.windows[idx].allocated && self.windows[idx].visible {
                if lowest_vis.is_none() {
                    lowest_vis = Some(idx);
                }
                visible_count += 1;
            }
        }
        if visible_count < 2 {
            return self.focused_slot().map(|s| self.windows[s].id);
        }
        if let Some(next_slot) = lowest_vis {
            let _ = self.focus_and_raise(next_slot, dirty);
            return Some(self.windows[next_slot].id);
        }
        None
    }
}
