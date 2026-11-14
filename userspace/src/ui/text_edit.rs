// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! UTF-8 safe single-line input field and multi-line code/text editor viewport widget.

use crate::gfx::Rect;
use crate::input::{InputEventKind, MOD_CTRL, MOD_SHIFT};
use crate::keymap::{
    KEY_BACKSPACE, KEY_DELETE, KEY_DOWN, KEY_END, KEY_HOME, KEY_LEFT, KEY_RIGHT, KEY_SPACE, KEY_UP,
};
use crate::ttf::{byte_index_at_x, font_metrics, x_at_byte_index, FontId};

use super::canvas::Canvas;
use super::layout::UiCtx;
use super::widgets::scrollbar_v;

pub const FIELD_CAP: usize = 128;

#[inline]
pub fn utf8_prev(buf: &[u8], pos: usize) -> usize {
    let mut i = pos.min(buf.len());
    if i == 0 {
        return 0;
    }
    i -= 1;
    while i > 0 && (buf[i] & 0xC0) == 0x80 {
        i -= 1;
    }
    i
}

#[inline]
pub fn utf8_next(buf: &[u8], pos: usize) -> usize {
    let mut i = pos.min(buf.len());
    if i >= buf.len() {
        return buf.len();
    }
    i += 1;
    while i < buf.len() && (buf[i] & 0xC0) == 0x80 {
        i += 1;
    }
    i
}

pub struct TextFieldState {
    pub buf: [u8; FIELD_CAP],
    pub len: usize,
    pub cursor: usize,
    pub sel_anchor: Option<usize>,
    pub scroll_x: i32,
}

impl TextFieldState {
    pub const fn new() -> Self {
        Self {
            buf: [0; FIELD_CAP],
            len: 0,
            cursor: 0,
            sel_anchor: None,
            scroll_x: 0,
        }
    }

    pub fn set_bytes(&mut self, src: &[u8]) {
        let n = src.len().min(FIELD_CAP);
        self.buf[..n].copy_from_slice(&src[..n]);
        self.len = n;
        self.cursor = n;
        self.sel_anchor = None;
        self.scroll_x = 0;
    }

    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    fn delete_selection(&mut self) -> bool {
        let Some(anc) = self.sel_anchor.take() else {
            return false;
        };
        let a = anc.min(self.cursor).min(self.len);
        let b = anc.max(self.cursor).min(self.len);
        if a == b {
            return false;
        }
        let tail = self.len - b;
        self.buf.copy_within(b..self.len, a);
        self.len -= b - a;
        self.cursor = a;
        let _ = tail;
        true
    }

    pub fn insert_byte(&mut self, ch: u8) -> bool {
        let _ = self.delete_selection();
        if self.len >= FIELD_CAP {
            return false;
        }
        let c = self.cursor.min(self.len);
        self.buf.copy_within(c..self.len, c + 1);
        self.buf[c] = ch;
        self.len += 1;
        self.cursor = c + 1;
        true
    }

    pub fn backspace(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        if self.cursor == 0 || self.len == 0 {
            return false;
        }
        let prev = utf8_prev(&self.buf[..self.len], self.cursor);
        self.buf.copy_within(self.cursor..self.len, prev);
        self.len -= self.cursor - prev;
        self.cursor = prev;
        true
    }

    pub fn delete_fwd(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        if self.cursor >= self.len {
            return false;
        }
        let next = utf8_next(&self.buf[..self.len], self.cursor);
        self.buf.copy_within(next..self.len, self.cursor);
        self.len -= next - self.cursor;
        true
    }
}

impl Default for TextFieldState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn text_field(
    c: &mut Canvas,
    ui: &mut UiCtx,
    id: u16,
    r: Rect,
    st: &mut TextFieldState,
    placeholder: &[u8],
    mono: bool,
) -> bool {
    let resp = ui.interact(id, r, true, false);
    let t = ui.theme;
    let font = if mono { FontId::MonoRegular } else { FontId::SansRegular };
    let sz = if mono { t.mono_px } else { t.body_px };
    let pad_x = 10usize;
    let text_x = (r.x + pad_x) as i32 - st.scroll_x;
    let mut changed = false;

    if resp.clicked && ui.persist.mx >= 0 {
        let rel = ui.persist.mx - text_x;
        let idx = byte_index_at_x(font, sz, st.as_bytes(), rel);
        st.cursor = idx;
        st.sel_anchor = None;
    } else if resp.pressed && ui.persist.mouse_down && ui.persist.mx >= 0 {
        let rel = ui.persist.mx - text_x;
        let idx = byte_index_at_x(font, sz, st.as_bytes(), rel);
        if st.sel_anchor.is_none() {
            st.sel_anchor = Some(st.cursor);
        }
        st.cursor = idx;
    }

    if resp.focused {
        if let Some(kev) = ui.key {
            if kev.kind == InputEventKind::KeyPress {
                let shift = (kev.modifiers & MOD_SHIFT) != 0;
                let ctrl = (kev.modifiers & MOD_CTRL) != 0;
                if ctrl && (kev.ascii == b'a' || kev.ascii == b'A') {
                    st.sel_anchor = Some(0);
                    st.cursor = st.len;
                    let _ = ui.take_key();
                } else if kev.keycode == KEY_LEFT {
                    if shift && st.sel_anchor.is_none() {
                        st.sel_anchor = Some(st.cursor);
                    } else if !shift {
                        st.sel_anchor = None;
                    }
                    st.cursor = utf8_prev(st.as_bytes(), st.cursor);
                    let _ = ui.take_key();
                } else if kev.keycode == KEY_RIGHT {
                    if shift && st.sel_anchor.is_none() {
                        st.sel_anchor = Some(st.cursor);
                    } else if !shift {
                        st.sel_anchor = None;
                    }
                    st.cursor = utf8_next(st.as_bytes(), st.cursor);
                    let _ = ui.take_key();
                } else if kev.keycode == KEY_HOME {
                    if shift && st.sel_anchor.is_none() {
                        st.sel_anchor = Some(st.cursor);
                    } else if !shift {
                        st.sel_anchor = None;
                    }
                    st.cursor = 0;
                    let _ = ui.take_key();
                } else if kev.keycode == KEY_END {
                    if shift && st.sel_anchor.is_none() {
                        st.sel_anchor = Some(st.cursor);
                    } else if !shift {
                        st.sel_anchor = None;
                    }
                    st.cursor = st.len;
                    let _ = ui.take_key();
                } else if kev.keycode == KEY_BACKSPACE {
                    changed |= st.backspace();
                    let _ = ui.take_key();
                } else if kev.keycode == KEY_DELETE {
                    changed |= st.delete_fwd();
                    let _ = ui.take_key();
                } else if kev.keycode == KEY_SPACE || (0x20..=0x7E).contains(&kev.ascii) && !ctrl {
                    let b = if kev.keycode == KEY_SPACE { b' ' } else { kev.ascii };
                    changed |= st.insert_byte(b);
                    let _ = ui.take_key();
                }
            }
        }
    }

    let inner_w = r.w.saturating_sub(pad_x * 2) as i32;
    let cur_px = x_at_byte_index(font, sz, st.as_bytes(), st.cursor);
    if cur_px - st.scroll_x > inner_w - 4 {
        st.scroll_x = (cur_px - inner_w + 4).max(0);
    } else if cur_px - st.scroll_x < 0 {
        st.scroll_x = cur_px.max(0);
    }

    let border = if resp.focused {
        t.focus_ring
    } else if resp.hovered {
        t.border_strong
    } else {
        t.border_subtle
    };
    c.fill_bordered_rrect_aa(r, t.r_sm, t.bg_input, border);

    let m = font_metrics(font, sz);
    let ty = r.y as i32 + ((r.h as i32 - (m.ascent + m.descent)) / 2);
    let clip_r = Rect::new(r.x + 6, r.y + 2, r.w.saturating_sub(12), r.h.saturating_sub(4));
    let mut sc = c.sub_clip(clip_r);
    let draw_x = (r.x + pad_x) as i32 - st.scroll_x;

    if st.len == 0 && !placeholder.is_empty() {
        let _ = sc.draw_text(draw_x, ty, font, sz, placeholder, t.fg_muted);
    } else if let Some(anc) = st.sel_anchor {
        sc.draw_text_selected(
            draw_x,
            ty,
            font,
            sz,
            st.as_bytes(),
            t.fg_primary,
            anc,
            st.cursor,
            t.accent,
            t.fg_on_accent,
        );
    } else {
        let _ = sc.draw_text(draw_x, ty, font, sz, st.as_bytes(), t.fg_primary);
    }

    if resp.focused {
        let cx = (draw_x + cur_px).max((r.x + 4) as i32) as usize;
        let cy = r.y + 5;
        let ch = r.h.saturating_sub(10).max(10);
        sc.fill_rect(Rect::new(cx, cy, 2, ch), t.accent_fg);
    }
    changed
}

/// Draw a slice of logical lines inside a scrollable editor/viewer area with optional gutter & selection.
pub struct MultiLineParams<'a> {
    pub lines: &'a [&'a [u8]],
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub scroll_line: &'a mut usize,
    pub font: FontId,
    pub show_gutter: bool,
    pub show_cursor: bool,
}

#[allow(clippy::needless_range_loop)]
pub fn multiline_viewport(
    c: &mut Canvas,
    ui: &mut UiCtx,
    id: u16,
    r: Rect,
    params: MultiLineParams<'_>,
) -> Option<(usize, usize)> {
    let resp = ui.interact(id, r, true, false);
    let t = ui.theme;
    let sz = if params.font == FontId::MonoRegular || params.font == FontId::MonoSemiBold {
        t.mono_px
    } else {
        t.body_px
    };
    let m = font_metrics(params.font, sz);
    let line_h = (m.line_height + 2).max(16) as usize;
    let vis_rows = (r.h.saturating_sub(8) / line_h).max(1);
    let total = params.lines.len();

    if ui.hit(r) && ui.scroll_dy != 0 {
        if ui.scroll_dy > 0 {
            *params.scroll_line = params.scroll_line.saturating_sub(2);
        } else {
            let max_s = total.saturating_sub(vis_rows);
            *params.scroll_line = (*params.scroll_line + 2).min(max_s);
        }
    }

    if resp.focused {
        if let Some(kev) = ui.key {
            if kev.kind == InputEventKind::KeyPress {
                if kev.keycode == KEY_UP && *params.scroll_line > 0 {
                    *params.scroll_line -= 1;
                    let _ = ui.take_key();
                } else if kev.keycode == KEY_DOWN {
                    let max_s = total.saturating_sub(vis_rows);
                    *params.scroll_line = (*params.scroll_line + 1).min(max_s);
                    let _ = ui.take_key();
                }
            }
        }
    }

    c.fill_bordered_rrect_aa(
        r,
        t.r_md,
        t.bg_input,
        if resp.focused { t.focus_ring } else { t.border_subtle },
    );

    let gutter_w = if params.show_gutter { 42usize } else { 10usize };
    let text_x0 = r.x + gutter_w + 8;
    if params.show_gutter {
        c.fill_rect(
            Rect::new(r.x + 1, r.y + 1, gutter_w, r.h.saturating_sub(2)),
            t.bg_sidebar,
        );
        c.vline(r.x + gutter_w + 1, r.y + 2, r.h.saturating_sub(4), t.border_subtle, 220);
    }

    let mut hit_pos = None;
    if resp.clicked && ui.persist.my >= (r.y + 4) as i32 {
        let rel_row = ((ui.persist.my - (r.y + 4) as i32) as usize) / line_h;
        let line_idx = (*params.scroll_line + rel_row).min(total.saturating_sub(1));
        if let Some(line_bytes) = params.lines.get(line_idx) {
            let rel_x = ui.persist.mx - text_x0 as i32;
            let col_b = byte_index_at_x(params.font, sz, line_bytes, rel_x);
            hit_pos = Some((line_idx, col_b));
        }
    }

    let body_clip = Rect::new(
        r.x + 2,
        r.y + 4,
        r.w.saturating_sub(12),
        r.h.saturating_sub(8),
    );
    let mut sc = c.sub_clip(body_clip);

    for row in 0..vis_rows {
        let li = *params.scroll_line + row;
        if li >= total {
            break;
        }
        let ry = r.y + 4 + row * line_h;
        if params.show_cursor && li == params.cursor_line {
            sc.fill_rect_alpha(
                Rect::new(text_x0 - 4, ry, r.w.saturating_sub(gutter_w + 14), line_h),
                t.accent_subtle,
                110,
            );
        }
        if params.show_gutter {
            let mut nb = [0u8; 8];
            let lno = format_u32(li as u32 + 1, &mut nb);
            let _ = sc.draw_text(
                (r.x + 8) as i32,
                ry as i32 + 1,
                FontId::MonoRegular,
                t.caption_px,
                lno,
                if li == params.cursor_line { t.accent_fg } else { t.fg_muted },
            );
        }
        let line_bytes = params.lines[li];
        let _ = sc.draw_text(
            text_x0 as i32,
            ry as i32 + 1,
            params.font,
            sz,
            line_bytes,
            t.fg_primary,
        );
        if params.show_cursor && li == params.cursor_line {
            let cx = text_x0 as i32 + x_at_byte_index(params.font, sz, line_bytes, params.cursor_col);
            sc.fill_rect(
                Rect::new(cx.max(text_x0 as i32) as usize, ry + 1, 2, line_h.saturating_sub(2)),
                t.accent_fg,
            );
        }
    }

    let sb_r = Rect::new(r.x + r.w.saturating_sub(9), r.y + 4, 6, r.h.saturating_sub(8));
    scrollbar_v(c, sb_r, total, vis_rows, *params.scroll_line, t.bg_card_alt, t.border_strong);
    hit_pos
}

fn format_u32(mut v: u32, out: &mut [u8; 8]) -> &[u8] {
    if v == 0 {
        out[7] = b'0';
        return &out[7..];
    }
    let mut i = 8usize;
    while v > 0 && i > 0 {
        i -= 1;
        out[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    &out[i..]
}
