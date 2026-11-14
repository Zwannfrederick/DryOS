// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Window Manager Input Routing & Event Dispatch (`Milestone 2A-2`–`DRY Glass 2.0`).

use crate::action::{AppId, DesktopAction};
use crate::gfx::{DirtyRegion, Rect};
use crate::glass::DOCK_CLEARANCE;
use crate::input::{InputEvent, InputEventKind, MOD_ALT, MOUSE_BTN_LEFT};
use crate::keymap::{KEY_DOWN, KEY_LEFT, KEY_RIGHT, KEY_TAB, KEY_UP};
use crate::wm::WindowManager;
use crate::wm_types::{
    HitPart, LayoutMode, SessionVisualState, WindowRole, WmInputAction, MAX_WINDOWS, TOPBAR_H,
};

impl WindowManager {
    fn push_client_pointer_event(&mut self, slot_idx: usize, mut ev: InputEvent, clamp: bool) {
        if slot_idx >= MAX_WINDOWS || !self.windows[slot_idx].allocated || !self.windows[slot_idx].is_pixel {
            return;
        }
        let (gx, gy) = self.windows[slot_idx].grid_origin();
        let pw = (self.windows[slot_idx].pixel_w as isize).max(1);
        let ph = (self.windows[slot_idx].pixel_h as isize).max(1);
        let rx = (self.cursor_x as isize) - (gx as isize);
        let ry = (self.cursor_y as isize) - (gy as isize);
        if clamp {
            ev.dx = rx.clamp(0, pw - 1) as i16;
            ev.dy = ry.clamp(0, ph - 1) as i16;
        } else {
            ev.dx = rx as i16;
            ev.dy = ry as i16;
        }
        ev.buttons = self.buttons_down;
        let _ = self.windows[slot_idx].ev_queue.push_replacing_abs(ev);
    }

    fn route_pointer_move_to_pixel(&mut self, ev: InputEvent) {
        if let Some(cap_slot) = self.pointer_capture_slot {
            if cap_slot < MAX_WINDOWS && self.windows[cap_slot].allocated && self.windows[cap_slot].visible {
                self.push_client_pointer_event(cap_slot, ev, true);
                return;
            }
            self.pointer_capture_slot = None;
        }
        if !self.p3_mode || self.glass.any_overlay_open() {
            if let Some(old) = self.hover_slot.take() {
                self.send_hover_leave(old, ev);
            }
            return;
        }
        let hit = self.hit_test_topmost(self.cursor_x, self.cursor_y);
        if let Some((s, HitPart::ClientBody)) = hit {
            if self.windows[s].is_pixel {
                if let Some(old) = self.hover_slot {
                    if old != s {
                        self.send_hover_leave(old, ev);
                    }
                }
                self.hover_slot = Some(s);
                self.push_client_pointer_event(s, ev, true);
                return;
            }
        }
        if let Some(old) = self.hover_slot.take() {
            self.send_hover_leave(old, ev);
        }
    }

    fn send_hover_leave(&mut self, slot_idx: usize, mut ev: InputEvent) {
        if slot_idx < MAX_WINDOWS
            && self.windows[slot_idx].allocated
            && self.windows[slot_idx].visible
            && self.windows[slot_idx].is_pixel
        {
            ev.dx = -1;
            ev.dy = -1;
            ev.buttons = self.buttons_down;
            let _ = self.windows[slot_idx].ev_queue.push_replacing_abs(ev);
        }
    }

    fn handle_mouse_move(
        &mut self,
        ev: InputEvent,
        scene_dirty: &mut DirtyRegion,
        cursor_dirty: &mut DirtyRegion,
    ) -> WmInputAction {
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
                if self.p3_mode && self.layout == LayoutMode::Floating {
                    let new_sp = if self.cursor_x <= 12 {
                        Some(Rect::new(
                            10,
                            TOPBAR_H + 10,
                            (self.screen_w / 2).saturating_sub(16),
                            self.screen_h.saturating_sub(TOPBAR_H + DOCK_CLEARANCE + 18),
                        ))
                    } else if self.cursor_x + 12 >= self.screen_w {
                        Some(Rect::new(
                            (self.screen_w / 2) + 6,
                            TOPBAR_H + 10,
                            (self.screen_w / 2).saturating_sub(16),
                            self.screen_h.saturating_sub(TOPBAR_H + DOCK_CLEARANCE + 18),
                        ))
                    } else {
                        None
                    };
                    if new_sp != self.glass.snap_preview {
                        if let Some(old_r) = self.glass.snap_preview {
                            scene_dirty.mark(old_r);
                        }
                        if let Some(nr) = new_sp {
                            scene_dirty.mark(nr);
                        }
                        self.glass.snap_preview = new_sp;
                    }
                }
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
            return WmInputAction::CursorMoved;
        }

        self.route_pointer_move_to_pixel(ev);
        WmInputAction::CursorMoved
    }

    fn handle_mouse_button_down(
        &mut self,
        ev: InputEvent,
        scene_dirty: &mut DirtyRegion,
        cursor_dirty: &mut DirtyRegion,
    ) -> WmInputAction {
        if !self.cursor_visible {
            self.cursor_visible = true;
            cursor_dirty.mark(self.cursor_rect());
        }
        self.buttons_down = ev.buttons;
        if (ev.scancode as u8) != MOUSE_BTN_LEFT {
            return WmInputAction::None;
        }
        if let Some(act) = crate::glass::overlays::handle_overlay_click(self, scene_dirty) {
            return act;
        }
        if self.cursor_y < TOPBAR_H
            && self.topbar_mode_pill_rect().contains(self.cursor_x, self.cursor_y)
        {
            let m = if self.p3_mode {
                self.set_layout_mode(self.layout.next_mode(), scene_dirty)
            } else {
                self.toggle_tiling(scene_dirty)
            };
            return WmInputAction::LayoutToggled(m);
        }
        if let Some(tab_slot) = self.hit_test_tabstrip(self.cursor_x, self.cursor_y) {
            let _ = self.focus_and_raise(tab_slot, scene_dirty);
            return WmInputAction::FocusChanged(self.windows[tab_slot].id);
        }
        if let Some((slot_idx, part)) = self.hit_test_topmost(self.cursor_x, self.cursor_y) {
            if part == HitPart::MinimizeButton {
                let _ = self.minimize_slot(slot_idx, scene_dirty);
                if let Some(new_f) = self.focused_slot() {
                    return WmInputAction::FocusChanged(self.windows[new_f].id);
                }
                return WmInputAction::None;
            }
            let focus_changed = self.focus_and_raise(slot_idx, scene_dirty);
            let win_id = self.windows[slot_idx].id;
            match part {
                HitPart::CloseButton => {
                    self.windows[slot_idx].close_requested = true;
                    return WmInputAction::CloseRequested(win_id);
                }
                HitPart::MinimizeButton => return WmInputAction::None,
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
                    if self.windows[slot_idx].is_pixel {
                        self.pointer_capture_slot = Some(slot_idx);
                        self.push_client_pointer_event(slot_idx, ev, true);
                        return WmInputAction::PointerRouted(win_id);
                    }
                    if focus_changed {
                        return WmInputAction::FocusChanged(win_id);
                    }
                }
            }
        }
        WmInputAction::None
    }

    fn handle_mouse_button_up(
        &mut self,
        ev: InputEvent,
        scene_dirty: &mut DirtyRegion,
    ) -> WmInputAction {
        self.buttons_down = ev.buttons;
        if (ev.scancode as u8) != MOUSE_BTN_LEFT {
            return WmInputAction::None;
        }
        if let Some(d_slot) = self.dragging_slot.take() {
            if let Some(sr) = self.glass.snap_preview.take() {
                scene_dirty.mark(sr);
                if self.windows[d_slot].allocated {
                    let old_o = self.windows[d_slot].outer_rect();
                    let rw = self.windows[d_slot].rect.w;
                    let rh = self.windows[d_slot].rect.h;
                    let tx = if sr.x > self.screen_w / 3 {
                        (sr.x + sr.w).saturating_sub(rw) as isize
                    } else {
                        sr.x as isize
                    };
                    let (nx, ny) = self.clamp_window_pos(tx, sr.y as isize, rw, rh);
                    self.windows[d_slot].rect.x = nx;
                    self.windows[d_slot].rect.y = ny;
                    self.windows[d_slot].saved_floating_rect = self.windows[d_slot].rect;
                    scene_dirty.mark(old_o);
                    scene_dirty.mark(self.windows[d_slot].outer_rect());
                }
            }
            if self.windows[d_slot].allocated {
                return WmInputAction::DragEnded(
                    self.windows[d_slot].id,
                    self.windows[d_slot].rect.x,
                    self.windows[d_slot].rect.y,
                );
            }
            return WmInputAction::None;
        }

        if let Some(cap_slot) = self.pointer_capture_slot.take() {
            if cap_slot < MAX_WINDOWS
                && self.windows[cap_slot].allocated
                && self.windows[cap_slot].visible
                && self.windows[cap_slot].is_pixel
            {
                let wid = self.windows[cap_slot].id;
                self.push_client_pointer_event(cap_slot, ev, true);
                return WmInputAction::PointerRouted(wid);
            }
        }
        if let Some((slot_idx, HitPart::ClientBody)) =
            self.hit_test_topmost(self.cursor_x, self.cursor_y)
        {
            if self.windows[slot_idx].is_pixel {
                let wid = self.windows[slot_idx].id;
                self.push_client_pointer_event(slot_idx, ev, true);
                return WmInputAction::PointerRouted(wid);
            }
        }
        WmInputAction::None
    }

    fn handle_key_press(&mut self, ev: InputEvent, scene_dirty: &mut DirtyRegion) -> WmInputAction {
        if (ev.modifiers & MOD_ALT) != 0 {
            if self.p3_mode {
                let scan7 = (ev.scancode & 0x7f) as u8;
                if ev.ascii == b'w' || ev.ascii == b'W' || scan7 == 0x11 {
                    crate::glass::overview::toggle_overview(self, scene_dirty);
                    return WmInputAction::None;
                }
                if self.glass.overview_open
                    && (ev.ascii == b'm' || ev.ascii == b'M' || scan7 == 0x32)
                {
                    if let Some(act) = crate::glass::overlays::handle_overlay_key(self, ev, scene_dirty) {
                        return act;
                    }
                }
                if ev.ascii == b'm' || ev.ascii == b'M' || scan7 == 0x32 {
                    if let Some(f_slot) = self.focused_slot() {
                        let _ = self.minimize_slot(f_slot, scene_dirty);
                        if let Some(new_f) = self.focused_slot() {
                            return WmInputAction::FocusChanged(self.windows[new_f].id);
                        }
                    }
                    return WmInputAction::None;
                }
                if ev.ascii == b' ' || scan7 == 0x39 {
                    crate::glass::overlays::toggle_launcher(self, scene_dirty);
                    return WmInputAction::None;
                }
                let maybe_app = match (ev.ascii, scan7) {
                    (b'1', _) | (_, 0x02) => Some(AppId::Terminal),
                    (b'2', _) | (_, 0x03) => Some(AppId::LifeStation),
                    (b'3', _) | (_, 0x04) => Some(AppId::Files),
                    (b'4', _) | (_, 0x05) => Some(AppId::Settings),
                    (b'5', _) | (_, 0x06) => Some(AppId::Editor),
                    (b'6', _) | (_, 0x07) => Some(AppId::Calculator),
                    _ => None,
                };
                if let Some(app) = maybe_app {
                    crate::glass::overlays::close_all_overlays(self, scene_dirty);
                    return self.enqueue_desktop_action(DesktopAction::LaunchOrFocusApp(app));
                }
                if ev.ascii == b'l' || ev.ascii == b'L' || scan7 == 0x26 {
                    crate::glass::overlays::close_all_overlays(self, scene_dirty);
                    return self.enqueue_desktop_action(DesktopAction::LockSession);
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
            if self.p3_mode && (ev.ascii == b'g' || ev.ascii == b'G' || (ev.scancode & 0x7f) == 0x22)
            {
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

        if let Some(act) = crate::glass::overlays::handle_overlay_key(self, ev, scene_dirty) {
            return act;
        }

        if let Some(f_slot) = self.focused_slot() {
            let _ = self.windows[f_slot].ev_queue.push(ev);
            return WmInputAction::KeyRouted(self.windows[f_slot].id);
        }
        WmInputAction::None
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
            return crate::glass::overlays::handle_locked_input(self, ev, scene_dirty, cursor_dirty);
        }

        match ev.kind {
            InputEventKind::MouseMove => self.handle_mouse_move(ev, scene_dirty, cursor_dirty),
            InputEventKind::MouseButtonDown => {
                self.handle_mouse_button_down(ev, scene_dirty, cursor_dirty)
            }
            InputEventKind::MouseButtonUp => self.handle_mouse_button_up(ev, scene_dirty),
            InputEventKind::MouseScroll => {
                if crate::glass::overview::handle_overview_scroll(self, ev.dy, scene_dirty) {
                    return WmInputAction::None;
                }
                let target_slot = self
                    .hit_test_topmost(self.cursor_x, self.cursor_y)
                    .map(|(s, _)| s)
                    .or_else(|| self.focused_slot());
                if let Some(s) = target_slot {
                    if self.windows[s].is_pixel {
                        let (gx, gy) = self.windows[s].grid_origin();
                        let mut cev = ev;
                        cev.dx = self.cursor_x.saturating_sub(gx) as i16;
                        cev.dy = self.cursor_y.saturating_sub(gy) as i16;
                        let _ = self.windows[s].ev_queue.push(cev);
                    } else {
                        let _ = self.windows[s].ev_queue.push(ev);
                    }
                    return WmInputAction::WheelRouted(self.windows[s].id);
                }
                WmInputAction::None
            }
            InputEventKind::KeyPress => self.handle_key_press(ev, scene_dirty),
            _ => WmInputAction::None,
        }
    }
}
