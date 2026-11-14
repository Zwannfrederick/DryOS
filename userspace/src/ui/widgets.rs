// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Shared DRY Glass 2.0 native UI widgets: buttons, toggles, checkboxes, sliders, segments,
//! and scrollbars.

use crate::gfx::Rect;
use crate::input::InputEventKind;
use crate::keymap::{KEY_LEFT, KEY_RIGHT};
use crate::ttf::{font_metrics, measure_text, FontId};

use super::canvas::Canvas;
use super::icons::{draw_vector_icon, IconKind};
use super::layout::UiCtx;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonVariant {
    Primary,
    Secondary,
    Subtle,
    Danger,
}

#[derive(Clone, Copy)]
pub struct ButtonOpts<'a> {
    pub label: &'a [u8],
    pub icon: Option<IconKind>,
    pub variant: ButtonVariant,
    pub disabled: bool,
    pub selected: bool,
}

impl<'a> ButtonOpts<'a> {
    pub const fn secondary(label: &'a [u8]) -> Self {
        Self {
            label,
            icon: None,
            variant: ButtonVariant::Secondary,
            disabled: false,
            selected: false,
        }
    }

    pub const fn primary(label: &'a [u8]) -> Self {
        Self {
            label,
            icon: None,
            variant: ButtonVariant::Primary,
            disabled: false,
            selected: false,
        }
    }
}

pub fn button(c: &mut Canvas, ui: &mut UiCtx, id: u16, r: Rect, opts: ButtonOpts<'_>) -> bool {
    let resp = ui.interact(id, r, !opts.disabled, opts.disabled);
    let t = ui.theme;
    let rad = t.r_sm;

    let (fill, border, fg) = match opts.variant {
        ButtonVariant::Primary => {
            if opts.disabled {
                (t.bg_card_alt, t.border_subtle, t.fg_muted)
            } else if resp.pressed {
                (t.accent_hover, t.accent, t.fg_on_accent)
            } else if resp.hovered {
                (t.accent_hover, t.focus_ring, t.fg_on_accent)
            } else {
                (t.accent, t.accent, t.fg_on_accent)
            }
        }
        ButtonVariant::Danger => {
            if opts.disabled {
                (t.bg_card_alt, t.border_subtle, t.fg_muted)
            } else if resp.pressed || resp.hovered {
                (t.danger, t.danger, t.fg_on_accent)
            } else {
                (t.danger_bg, t.danger, t.danger_fg)
            }
        }
        ButtonVariant::Subtle => {
            if opts.selected {
                (t.bg_selected, t.accent_fg, t.fg_primary)
            } else if resp.pressed {
                (t.bg_pressed, t.border_strong, t.fg_primary)
            } else if resp.hovered {
                (t.bg_hover, t.border_subtle, t.fg_primary)
            } else {
                (t.bg_card_alt, t.border_subtle, t.fg_secondary)
            }
        }
        ButtonVariant::Secondary => {
            if opts.selected {
                (t.bg_selected, t.focus_ring, t.fg_primary)
            } else if opts.disabled {
                (t.bg_card, t.border_subtle, t.fg_muted)
            } else if resp.pressed {
                (t.bg_pressed, t.border_strong, t.fg_primary)
            } else if resp.hovered {
                (t.bg_hover, t.border_strong, t.fg_primary)
            } else {
                (t.bg_card_alt, t.border_subtle, t.fg_primary)
            }
        }
    };

    c.fill_bordered_rrect_aa(r, rad, fill, border);
    if resp.focused {
        let ring = Rect::new(r.x + 1, r.y + 1, r.w.saturating_sub(2), r.h.saturating_sub(2));
        c.stroke_rrect_aa(ring, rad.saturating_sub(1), t.focus_ring, 220);
    }

    let font = if opts.variant == ButtonVariant::Primary || opts.selected {
        FontId::SansSemiBold
    } else {
        FontId::SansRegular
    };
    let sz = t.body_px;
    let m = font_metrics(font, sz);
    let tw = measure_text(font, sz, opts.label);
    let icon_sz = (sz as usize + 2).min(r.h.saturating_sub(8));
    let icon_gap = if opts.icon.is_some() && !opts.label.is_empty() {
        6usize
    } else {
        0usize
    };
    let total_w = (if opts.icon.is_some() { icon_sz + icon_gap } else { 0 }) as i32 + tw;
    let sx = r.x as i32 + ((r.w as i32 - total_w) / 2).max(6);
    let ty = r.y as i32 + ((r.h as i32 - (m.ascent + m.descent)) / 2);

    let mut cur_x = sx;
    if let Some(ic) = opts.icon {
        let ir = Rect::new(
            cur_x.max(0) as usize,
            r.y + (r.h.saturating_sub(icon_sz)) / 2,
            icon_sz,
            icon_sz,
        );
        let accent_ic = if opts.variant == ButtonVariant::Primary {
            t.fg_on_accent
        } else {
            t.accent_fg
        };
        draw_vector_icon(c, ir, ic, fg, accent_ic);
        cur_x += (icon_sz + icon_gap) as i32;
    }
    if !opts.label.is_empty() {
        let mut sc = c.sub_clip(Rect::new(r.x + 4, r.y + 2, r.w.saturating_sub(8), r.h.saturating_sub(4)));
        let _ = sc.draw_text(cur_x, ty, font, sz, opts.label, fg);
    }
    resp.clicked
}

pub fn icon_button(
    c: &mut Canvas,
    ui: &mut UiCtx,
    id: u16,
    r: Rect,
    icon: IconKind,
    variant: ButtonVariant,
    disabled: bool,
) -> bool {
    button(
        c,
        ui,
        id,
        r,
        ButtonOpts {
            label: b"",
            icon: Some(icon),
            variant,
            disabled,
            selected: false,
        },
    )
}

pub fn toggle_switch(
    c: &mut Canvas,
    ui: &mut UiCtx,
    id: u16,
    r: Rect,
    label: &[u8],
    value: &mut bool,
    disabled: bool,
) -> bool {
    let resp = ui.interact(id, r, !disabled, disabled);
    let mut changed = false;
    if resp.clicked {
        *value = !*value;
        changed = true;
    }
    let t = ui.theme;
    let sw_w = 38usize.min(r.w);
    let sw_h = 20usize.min(r.h);
    let sw_r = Rect::new(
        r.x + r.w.saturating_sub(sw_w),
        r.y + (r.h.saturating_sub(sw_h)) / 2,
        sw_w,
        sw_h,
    );

    if resp.hovered && !disabled {
        c.fill_rrect_aa(r, t.r_sm, t.bg_hover, 140);
    }
    if resp.focused {
        c.stroke_rrect_aa(r, t.r_sm, t.focus_ring, 210);
    }

    if !label.is_empty() {
        let m = font_metrics(FontId::SansRegular, t.body_px);
        let ty = r.y as i32 + ((r.h as i32 - (m.ascent + m.descent)) / 2);
        let max_w = sw_r.x.saturating_sub(r.x + 12) as i32;
        c.draw_text_ellipsis(
            (r.x + 4) as i32,
            ty,
            max_w,
            FontId::SansRegular,
            t.body_px,
            label,
            if disabled { t.fg_muted } else { t.fg_primary },
        );
    }

    let (track_bg, track_border) = if *value {
        (t.accent, t.accent)
    } else {
        (t.bg_card_alt, t.border_strong)
    };
    c.fill_bordered_rrect_aa(sw_r, sw_h / 2, track_bg, track_border);

    let rad_px = (sw_h / 2).saturating_sub(3).max(4);
    let cx = if *value {
        (sw_r.x + sw_r.w - sw_h / 2) as i32
    } else {
        (sw_r.x + sw_h / 2) as i32
    };
    let cy = (sw_r.y + sw_h / 2) as i32;
    c.fill_circle_aa(cx * 16, cy * 16, (rad_px as i32) * 16, t.fg_on_accent, 255);
    changed
}

pub fn checkbox(
    c: &mut Canvas,
    ui: &mut UiCtx,
    id: u16,
    r: Rect,
    label: &[u8],
    checked: &mut bool,
    disabled: bool,
) -> bool {
    let resp = ui.interact(id, r, !disabled, disabled);
    let mut changed = false;
    if resp.clicked {
        *checked = !*checked;
        changed = true;
    }
    let t = ui.theme;
    let box_sz = 18usize.min(r.h);
    let box_r = Rect::new(r.x + 2, r.y + (r.h.saturating_sub(box_sz)) / 2, box_sz, box_sz);
    let (bg, border) = if *checked {
        (t.accent, t.accent)
    } else if resp.hovered {
        (t.bg_hover, t.border_strong)
    } else {
        (t.bg_input, t.border_strong)
    };
    c.fill_bordered_rrect_aa(box_r, 4, bg, border);
    if resp.focused {
        c.stroke_rrect_aa(r, t.r_sm, t.focus_ring, 200);
    }
    if *checked {
        draw_vector_icon(c, box_r, IconKind::Check, t.fg_on_accent, t.fg_on_accent);
    }
    if !label.is_empty() {
        let m = font_metrics(FontId::SansRegular, t.body_px);
        let ty = r.y as i32 + ((r.h as i32 - (m.ascent + m.descent)) / 2);
        let tx = (box_r.x + box_r.w + 8) as i32;
        c.draw_text_ellipsis(
            tx,
            ty,
            (r.x + r.w).saturating_sub(tx.max(0) as usize) as i32,
            FontId::SansRegular,
            t.body_px,
            label,
            if disabled { t.fg_muted } else { t.fg_primary },
        );
    }
    changed
}

pub fn slider(
    c: &mut Canvas,
    ui: &mut UiCtx,
    id: u16,
    r: Rect,
    min_v: i32,
    max_v: i32,
    val: &mut i32,
) -> bool {
    let resp = ui.interact(id, r, true, false);
    let span = (max_v - min_v).max(1);
    let mut changed = false;

    let pad_x = 10usize;
    let track_x0 = r.x + pad_x;
    let track_w = r.w.saturating_sub(pad_x * 2).max(16);

    if (resp.clicked || (resp.pressed && ui.persist.mouse_down)) && ui.persist.mx >= 0 {
        let rel = (ui.persist.mx - track_x0 as i32).clamp(0, track_w as i32);
        let nv = min_v + ((rel * span + (track_w as i32 / 2)) / track_w as i32);
        if nv != *val {
            *val = nv;
            changed = true;
        }
    }
    if resp.focused {
        if let Some(kev) = ui.key {
            if kev.kind == InputEventKind::KeyPress {
                if kev.keycode == KEY_LEFT && *val > min_v {
                    *val -= 1;
                    changed = true;
                    let _ = ui.take_key();
                } else if kev.keycode == KEY_RIGHT && *val < max_v {
                    *val += 1;
                    changed = true;
                    let _ = ui.take_key();
                }
            }
        }
    }

    let t = ui.theme;
    let clamped = (*val).clamp(min_v, max_v);
    let fill_w = (((clamped - min_v) as usize) * track_w) / span as usize;
    let cy = r.y + r.h / 2;

    if resp.focused {
        c.stroke_rrect_aa(r, t.r_sm, t.focus_ring, 180);
    }
    let track_r = Rect::new(track_x0, cy.saturating_sub(3), track_w, 6);
    c.fill_bordered_rrect_aa(track_r, 3, t.bg_card_alt, t.border_subtle);
    if fill_w > 0 {
        let fill_r = Rect::new(track_x0, cy.saturating_sub(3), fill_w.max(6).min(track_w), 6);
        c.fill_rrect_aa(fill_r, 3, t.accent, 255);
    }
    let thumb_x = (track_x0 + fill_w) as i32;
    let thumb_r_q4 = if resp.hovered || resp.pressed { 128 } else { 112 };
    c.fill_circle_aa(thumb_x * 16, (cy as i32) * 16, thumb_r_q4, t.fg_primary, 255);
    c.stroke_circle_aa(thumb_x * 16, (cy as i32) * 16, thumb_r_q4, 24, t.accent, 255);
    changed
}

pub fn segmented_control(
    c: &mut Canvas,
    ui: &mut UiCtx,
    base_id: u16,
    r: Rect,
    options: &[&[u8]],
    selected: &mut usize,
) -> bool {
    if options.is_empty() || r.w < 16 {
        return false;
    }
    let t = ui.theme;
    c.fill_bordered_rrect_aa(r, t.r_md, t.bg_input, t.border_subtle);

    let n = options.len();
    let inner = Rect::new(r.x + 3, r.y + 3, r.w.saturating_sub(6), r.h.saturating_sub(6));
    let seg_w = inner.w / n;
    let mut changed = false;

    for (i, &label) in options.iter().enumerate() {
        let sx = inner.x + i * seg_w;
        let sw = if i + 1 == n { inner.x + inner.w - sx } else { seg_w };
        let sr = Rect::new(sx, inner.y, sw, inner.h);
        let resp = ui.interact(base_id.wrapping_add(i as u16), sr, true, false);
        if resp.clicked && *selected != i {
            *selected = i;
            changed = true;
        }
        let is_sel = *selected == i;
        if is_sel {
            c.fill_bordered_rrect_aa(sr, t.r_sm, t.bg_card_alt, t.border_strong);
        } else if resp.hovered {
            c.fill_rrect_aa(sr, t.r_sm, t.bg_hover, 180);
        }
        if resp.focused {
            c.stroke_rrect_aa(sr, t.r_sm, t.focus_ring, 210);
        }
        let font = if is_sel { FontId::SansSemiBold } else { FontId::SansRegular };
        let fg = if is_sel { t.fg_primary } else { t.fg_muted };
        c.draw_text_centered(sr, font, t.caption_px.max(12), label, fg);
    }
    changed
}

pub fn scrollbar_v(
    c: &mut Canvas,
    track: Rect,
    total_items: usize,
    visible_items: usize,
    scroll_off: usize,
    track_rgb: u32,
    thumb_rgb: u32,
) {
    if track.w < 4 || track.h < 16 || total_items <= visible_items || total_items == 0 {
        return;
    }
    c.fill_rrect_aa(track, track.w / 2, track_rgb, 150);
    let thumb_h = ((visible_items * track.h) / total_items).max(18).min(track.h);
    let max_off = total_items - visible_items;
    let clamped_off = scroll_off.min(max_off);
    let travel = track.h - thumb_h;
    let thumb_y = track.y + (clamped_off * travel).checked_div(max_off).unwrap_or(0);
    let thumb_r = Rect::new(track.x + 1, thumb_y, track.w.saturating_sub(2).max(2), thumb_h);
    c.fill_rrect_aa(thumb_r, thumb_r.w / 2, thumb_rgb, 220);
}
