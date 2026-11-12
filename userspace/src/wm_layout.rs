// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Window Manager Layout Engine: `Floating`, `SideBySideTiled`, `Stacked`, `Tabbed` & Auto-Floating
//! `DialogTransient` Geometry Policy (`Milestone 2A-3`).

use crate::gfx::{DirtyRegion, Rect};
use crate::wm::{
    LayoutMode, WindowManager, WindowRole, MAX_WINDOWS, SHADOW_OFF, TOPBAR_H,
};

pub const TABSTRIP_Y: usize = TOPBAR_H + 8;
pub const TABSTRIP_H: usize = 24;
pub const TAB_PILL_W: usize = 148;
pub const TAB_PILL_GAP: usize = 6;

impl WindowManager {
    /// Collect allocated, visible `WindowRole::Normal` slot indices in deterministic slot/id order.
    pub fn collect_normal_slots(&self, out: &mut [usize; MAX_WINDOWS]) -> usize {
        let mut n = 0usize;
        for slot_i in 0..MAX_WINDOWS {
            let w = &self.windows[slot_i];
            if w.allocated && w.visible && w.role == WindowRole::Normal {
                out[n] = slot_i;
                n += 1;
            }
        }
        // Sort by `win.id` so tab/tile order never jumps around when focus changes.
        for i in 1..n {
            let key = out[i];
            let key_id = self.windows[key].id;
            let mut j = i;
            while j > 0 && self.windows[out[j - 1]].id > key_id {
                out[j] = out[j - 1];
                j -= 1;
            }
            out[j] = key;
        }
        n
    }

    #[inline]
    pub fn tabstrip_rect(&self) -> Rect {
        Rect::new(24, TABSTRIP_Y, self.screen_w.saturating_sub(48), TABSTRIP_H)
    }

    #[inline]
    pub const fn tab_pill_rect(&self, tab_idx: usize) -> Rect {
        Rect::new(
            24 + tab_idx * (TAB_PILL_W + TAB_PILL_GAP),
            TABSTRIP_Y,
            TAB_PILL_W,
            TABSTRIP_H,
        )
    }

    /// Hit-test the clickable tab bar when `self.layout == LayoutMode::Tabbed`.
    pub fn hit_test_tabstrip(&self, px: usize, py: usize) -> Option<usize> {
        if self.layout != LayoutMode::Tabbed {
            return None;
        }
        let mut normal = [0usize; MAX_WINDOWS];
        let n = self.collect_normal_slots(&mut normal);
        for (tab_i, &slot_idx) in normal.iter().take(n).enumerate() {
            if self.tab_pill_rect(tab_i).contains(px, py) {
                return Some(slot_idx);
            }
        }
        None
    }

    /// Keep `WindowRole::DialogTransient` windows above `WindowRole::Normal` windows in `z_stack`
    /// so modal/helper dialogs never disappear behind tiled or tabbed workspaces.
    pub fn ensure_dialogs_above_normal(&mut self, dirty: &mut DirtyRegion) {
        let mut reordered = [0usize; MAX_WINDOWS];
        let mut p = 0usize;
        for z in 0..self.z_len {
            let idx = self.z_stack[z];
            if self.windows[idx].role == WindowRole::Normal {
                reordered[p] = idx;
                p += 1;
            }
        }
        for z in 0..self.z_len {
            let idx = self.z_stack[z];
            if self.windows[idx].role == WindowRole::DialogTransient {
                reordered[p] = idx;
                p += 1;
            }
        }
        if reordered[..self.z_len] != self.z_stack[..self.z_len] {
            self.z_stack[..self.z_len].copy_from_slice(&reordered[..self.z_len]);
            for z in 0..self.z_len {
                let idx = self.z_stack[z];
                if self.windows[idx].role == WindowRole::DialogTransient {
                    dirty.mark(self.windows[idx].outer_rect());
                }
            }
        }
    }

    /// Switch directly to `target` (`Floating`, `SideBySideTiled`, `Stacked`, `Tabbed`), saving
    /// floating geometries when leaving `Floating` and restoring them when returning to `Floating`.
    pub fn set_layout_mode(&mut self, target: LayoutMode, dirty: &mut DirtyRegion) -> LayoutMode {
        self.dragging_slot = None;
        let prev = self.layout;
        if prev == target {
            self.apply_layout_geometries(dirty);
            return self.layout;
        }
        if prev == LayoutMode::Floating {
            for w in &mut self.windows {
                if w.allocated && w.role == WindowRole::Normal {
                    w.saved_floating_rect = w.rect;
                }
            }
        }
        if prev == LayoutMode::Tabbed || target == LayoutMode::Tabbed {
            dirty.mark(self.tabstrip_rect());
        }
        self.layout = target;
        if target == LayoutMode::Floating {
            for w in &mut self.windows {
                if w.allocated && w.role == WindowRole::Normal && !w.saved_floating_rect.is_empty() {
                    let old_o = w.outer_rect();
                    w.rect = w.saved_floating_rect;
                    dirty.mark(old_o);
                    dirty.mark(w.outer_rect());
                }
            }
        } else {
            self.apply_layout_geometries(dirty);
        }
        self.ensure_dialogs_above_normal(dirty);
        dirty.mark(Rect::new(0, 0, self.screen_w, TOPBAR_H));
        self.layout
    }

    /// 1-command toggle between `Floating` and `SideBySideTiled` (`Alt+T`, Topbar pill, `tile`).
    pub fn toggle_tiling(&mut self, dirty: &mut DirtyRegion) -> LayoutMode {
        let next = if self.layout == LayoutMode::Floating {
            LayoutMode::SideBySideTiled
        } else {
            LayoutMode::Floating
        };
        self.set_layout_mode(next, dirty)
    }

    /// Compute and apply non-floating geometries for `WindowRole::Normal` windows while keeping all
    /// `WindowRole::DialogTransient` windows auto-floating above the workspace.
    pub fn apply_layout_geometries(&mut self, dirty: &mut DirtyRegion) {
        if self.layout == LayoutMode::Floating {
            self.ensure_dialogs_above_normal(dirty);
            return;
        }

        let mut normal_slots = [0usize; MAX_WINDOWS];
        let n = self.collect_normal_slots(&mut normal_slots);
        if n == 0 {
            if self.layout == LayoutMode::Tabbed {
                dirty.mark(self.tabstrip_rect());
            }
            return;
        }

        match self.layout {
            LayoutMode::Floating => {}
            LayoutMode::SideBySideTiled => {
                let gap = 14usize;
                let ty = TOPBAR_H + 14;
                if n == 1 {
                    let idx = normal_slots[0];
                    let old_o = self.windows[idx].outer_rect();
                    let nat = self.windows[idx].saved_floating_rect;
                    let (rw, rh) = if nat.is_empty() {
                        (self.windows[idx].rect.w, self.windows[idx].rect.h)
                    } else {
                        (nat.w, nat.h)
                    };
                    let (nx, ny) = self.clamp_window_pos(
                        ((self.screen_w.saturating_sub(rw)) / 2) as isize,
                        ty as isize,
                        rw,
                        rh,
                    );
                    self.windows[idx].rect = Rect::new(nx, ny, rw, rh);
                    dirty.mark(old_o);
                    dirty.mark(self.windows[idx].outer_rect());
                } else if n == 2 {
                    let (l_idx, r_idx) = (normal_slots[0], normal_slots[1]);
                    let lnat = self.windows[l_idx].saved_floating_rect;
                    let rnat = self.windows[r_idx].saved_floating_rect;
                    let (lw, lh) = if lnat.is_empty() {
                        (self.windows[l_idx].rect.w, self.windows[l_idx].rect.h)
                    } else {
                        (lnat.w, lnat.h)
                    };
                    let (rw, rh) = if rnat.is_empty() {
                        (self.windows[r_idx].rect.w, self.windows[r_idx].rect.h)
                    } else {
                        (rnat.w, rnat.h)
                    };
                    let total_w = lw + gap + rw;
                    let start_x = self.screen_w.saturating_sub(total_w) / 2;

                    let old_l = self.windows[l_idx].outer_rect();
                    let (lx, ly) = self.clamp_window_pos(start_x.max(8) as isize, ty as isize, lw, lh);
                    self.windows[l_idx].rect = Rect::new(lx, ly, lw, lh);
                    dirty.mark(old_l);
                    dirty.mark(self.windows[l_idx].outer_rect());

                    let old_r = self.windows[r_idx].outer_rect();
                    let (rx, ry) = self.clamp_window_pos((lx + lw + gap) as isize, ty as isize, rw, rh);
                    self.windows[r_idx].rect = Rect::new(rx, ry, rw, rh);
                    dirty.mark(old_r);
                    dirty.mark(self.windows[r_idx].outer_rect());
                } else {
                    let pad_x = 12usize;
                    let gap_x = 12usize;
                    let pad_y = 6usize;
                    let gap_y = 10usize;
                    let col_w = self.screen_w.saturating_sub(pad_x * 2 + gap_x + SHADOW_OFF) / 2;
                    let rows_grid = n.div_ceil(2).max(1);
                    let avail_h = self
                        .screen_h
                        .saturating_sub(TOPBAR_H + pad_y * 2 + SHADOW_OFF);
                    let cell_h = avail_h.saturating_sub(gap_y * (rows_grid - 1)) / rows_grid;
                    for (k, &idx) in normal_slots.iter().take(n).enumerate() {
                        let col_k = k % 2;
                        let row_k = k / 2;
                        let old_o = self.windows[idx].outer_rect();
                        let nat = self.windows[idx].saved_floating_rect;
                        let nat_h = if nat.is_empty() { self.windows[idx].rect.h } else { nat.h };
                        let ww = col_w.max(240);
                        let wh = cell_h.min(nat_h).max(140);
                        let nx = pad_x + col_k * (col_w + gap_x);
                        let ny = TOPBAR_H + pad_y + row_k * (cell_h + gap_y);
                        self.windows[idx].rect = Rect::new(nx, ny, ww, wh);
                        dirty.mark(old_o);
                        dirty.mark(self.windows[idx].outer_rect());
                    }
                }
            }
            LayoutMode::Stacked => {
                // Cascade normal windows from top-left `(48, TOPBAR_H + 20)` in Z-order so every
                // background window's titlebar strip remains visible and directly clickable.
                let mut stack_k = 0usize;
                for z in 0..self.z_len {
                    let idx = self.z_stack[z];
                    if self.windows[idx].role != WindowRole::Normal {
                        continue;
                    }
                    let old_o = self.windows[idx].outer_rect();
                    let nat = self.windows[idx].saved_floating_rect;
                    let (ww, wh) = if nat.is_empty() {
                        (self.windows[idx].rect.w, self.windows[idx].rect.h)
                    } else {
                        (nat.w, nat.h)
                    };
                    let tx = (48 + stack_k * 42) as isize;
                    let ty = (TOPBAR_H + 20 + stack_k * 36) as isize;
                    let (nx, ny) = self.clamp_window_pos(tx, ty, ww, wh);
                    self.windows[idx].rect = Rect::new(nx, ny, ww, wh);
                    dirty.mark(old_o);
                    dirty.mark(self.windows[idx].outer_rect());
                    stack_k += 1;
                }
            }
            LayoutMode::Tabbed => {
                // Shared centered deck below `TABSTRIP_Y + TABSTRIP_H + 8`; topmost normal window
                // owns the deck foreground while the tab strip exposes all normal windows.
                dirty.mark(self.tabstrip_rect());
                let deck_y = (TABSTRIP_Y + TABSTRIP_H + 8) as isize;
                for &idx in normal_slots.iter().take(n) {
                    let old_o = self.windows[idx].outer_rect();
                    let nat = self.windows[idx].saved_floating_rect;
                    let (ww, wh) = if nat.is_empty() {
                        (self.windows[idx].rect.w, self.windows[idx].rect.h)
                    } else {
                        (nat.w, nat.h)
                    };
                    let deck_x = (self.screen_w.saturating_sub(ww + SHADOW_OFF) / 2).max(24) as isize;
                    let (nx, ny) = self.clamp_window_pos(deck_x, deck_y, ww, wh);
                    self.windows[idx].rect = Rect::new(nx, ny, ww, wh);
                    dirty.mark(old_o);
                    dirty.mark(self.windows[idx].outer_rect());
                }
            }
        }
        self.ensure_dialogs_above_normal(dirty);
    }
}
