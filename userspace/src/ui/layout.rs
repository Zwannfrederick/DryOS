// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Immediate-mode widget interaction context, Tab focus traversal, and responsive rect splitters.

use crate::gfx::Rect;
use crate::input::{InputEvent, InputEventKind, MOD_SHIFT, MOUSE_BTN_LEFT};
use crate::keymap::{KEY_ENTER, KEY_SPACE, KEY_TAB};

use super::theme::UiTheme;

const MAX_FOCUS_ITEMS: usize = 64;

#[derive(Clone, Copy, Default)]
pub struct WidgetResp {
    pub hovered: bool,
    pub pressed: bool,
    pub clicked: bool,
    pub focused: bool,
}

pub struct UiPersist {
    pub focused: u16,
    pub hovered: u16,
    pub active: u16,
    pub mx: i32,
    pub my: i32,
    pub mouse_down: bool,
    pub focus_order: [u16; MAX_FOCUS_ITEMS],
    pub focus_count: usize,
}

impl UiPersist {
    pub const fn new() -> Self {
        Self {
            focused: 0,
            hovered: 0,
            active: 0,
            mx: -1,
            my: -1,
            mouse_down: false,
            focus_order: [0; MAX_FOCUS_ITEMS],
            focus_count: 0,
        }
    }

    pub fn update_from_event(&mut self, ev: InputEvent, clicked: &mut bool, released: &mut bool) {
        match ev.kind {
            InputEventKind::MouseMove => {
                self.mx = ev.dx as i32;
                self.my = ev.dy as i32;
            }
            InputEventKind::MouseButtonDown if (ev.scancode as u8) == MOUSE_BTN_LEFT => {
                self.mx = ev.dx as i32;
                self.my = ev.dy as i32;
                self.mouse_down = true;
                *clicked = true;
            }
            InputEventKind::MouseButtonUp if (ev.scancode as u8) == MOUSE_BTN_LEFT => {
                self.mx = ev.dx as i32;
                self.my = ev.dy as i32;
                self.mouse_down = false;
                *released = true;
            }
            _ => {}
        }
    }
}

impl Default for UiPersist {
    fn default() -> Self {
        Self::new()
    }
}

pub struct UiCtx<'a> {
    pub persist: &'a mut UiPersist,
    pub theme: UiTheme,
    pub clicked: bool,
    pub released: bool,
    pub scroll_dy: i16,
    pub key: Option<InputEvent>,
    next_hovered: u16,
}

impl<'a> UiCtx<'a> {
    pub fn begin(
        persist: &'a mut UiPersist,
        theme: UiTheme,
        clicked: bool,
        released: bool,
        scroll_dy: i16,
        key: Option<InputEvent>,
    ) -> Self {
        persist.focus_count = 0;
        Self {
            persist,
            theme,
            clicked,
            released,
            scroll_dy,
            key,
            next_hovered: 0,
        }
    }

    #[inline]
    pub fn hit(&self, r: Rect) -> bool {
        self.persist.mx >= 0
            && self.persist.my >= 0
            && r.contains(self.persist.mx as usize, self.persist.my as usize)
    }

    pub fn register_focusable(&mut self, id: u16) {
        if id != 0 && self.persist.focus_count < MAX_FOCUS_ITEMS {
            self.persist.focus_order[self.persist.focus_count] = id;
            self.persist.focus_count += 1;
        }
    }

    pub fn interact(&mut self, id: u16, r: Rect, focusable: bool, disabled: bool) -> WidgetResp {
        if disabled {
            return WidgetResp::default();
        }
        if focusable {
            self.register_focusable(id);
        }
        let hovered = self.hit(r);
        if hovered {
            self.next_hovered = id;
        }
        let mut clicked = self.clicked && hovered;
        if self.clicked && hovered {
            self.persist.active = id;
            if focusable {
                self.persist.focused = id;
            }
        }
        let focused = focusable && self.persist.focused == id;
        if focused {
            if let Some(kev) = self.key {
                if kev.kind == InputEventKind::KeyPress
                    && (kev.keycode == KEY_ENTER || kev.keycode == KEY_SPACE)
                {
                    clicked = true;
                    self.key = None;
                }
            }
        }
        WidgetResp {
            hovered,
            pressed: self.persist.active == id && self.persist.mouse_down,
            clicked,
            focused,
        }
    }

    #[inline]
    pub fn take_key(&mut self) -> Option<InputEvent> {
        self.key.take()
    }

    pub fn finish(mut self) {
        self.persist.hovered = self.next_hovered;
        if let Some(kev) = self.key {
            if kev.kind == InputEventKind::KeyPress
                && kev.keycode == KEY_TAB
                && self.persist.focus_count > 0
            {
                let count = self.persist.focus_count;
                let back = (kev.modifiers & MOD_SHIFT) != 0;
                let cur_pos = self.persist.focus_order[..count]
                    .iter()
                    .position(|&x| x == self.persist.focused);
                let next_idx = match cur_pos {
                    None => 0,
                    Some(i) if back => (i + count - 1) % count,
                    Some(i) => (i + 1) % count,
                };
                self.persist.focused = self.persist.focus_order[next_idx];
                self.key = None;
            }
        }
        if self.released || !self.persist.mouse_down {
            self.persist.active = 0;
        }
    }
}

#[inline]
pub const fn inset_rect(r: Rect, px: usize, py: usize) -> Rect {
    Rect::new(
        r.x + px,
        r.y + py,
        r.w.saturating_sub(px * 2),
        r.h.saturating_sub(py * 2),
    )
}

#[inline]
pub const fn split_top(r: Rect, top_h: usize, gap: usize) -> (Rect, Rect) {
    let th = if top_h > r.h { r.h } else { top_h };
    let rest_y = r.y + th + gap;
    let rest_h = r.h.saturating_sub(th + gap);
    (Rect::new(r.x, r.y, r.w, th), Rect::new(r.x, rest_y, r.w, rest_h))
}

#[inline]
pub const fn split_bot(r: Rect, bot_h: usize, gap: usize) -> (Rect, Rect) {
    let bh = if bot_h > r.h { r.h } else { bot_h };
    let top_h = r.h.saturating_sub(bh + gap);
    let bot_y = r.y + r.h.saturating_sub(bh);
    (Rect::new(r.x, r.y, r.w, top_h), Rect::new(r.x, bot_y, r.w, bh))
}

#[inline]
pub const fn split_left(r: Rect, left_w: usize, gap: usize) -> (Rect, Rect) {
    let lw = if left_w > r.w { r.w } else { left_w };
    let rest_x = r.x + lw + gap;
    let rest_w = r.w.saturating_sub(lw + gap);
    (Rect::new(r.x, r.y, lw, r.h), Rect::new(rest_x, r.y, rest_w, r.h))
}

#[inline]
pub const fn split_right(r: Rect, right_w: usize, gap: usize) -> (Rect, Rect) {
    let rw = if right_w > r.w { r.w } else { right_w };
    let left_w = r.w.saturating_sub(rw + gap);
    let rx = r.x + r.w.saturating_sub(rw);
    (Rect::new(r.x, r.y, left_w, r.h), Rect::new(rx, r.y, rw, r.h))
}

#[inline]
pub fn h_cell(r: Rect, cols: usize, idx: usize, gap: usize) -> Rect {
    if cols == 0 || idx >= cols {
        return Rect::EMPTY;
    }
    let total_gap = gap * cols.saturating_sub(1);
    let cell_w = r.w.saturating_sub(total_gap) / cols;
    let x = r.x + idx * (cell_w + gap);
    let w = if idx + 1 == cols { r.x + r.w - x } else { cell_w };
    Rect::new(x, r.y, w, r.h)
}
