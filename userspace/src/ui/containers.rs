// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Structured layout containers: sidebars, toolbars, breadcrumbs, tabs, cards, badges, tables,
//! popovers, and modal dialogs.

use crate::gfx::Rect;
use crate::ttf::{font_metrics, measure_text, FontId};

use super::canvas::Canvas;
use super::icons::{draw_vector_icon, IconKind};
use super::layout::{inset_rect, UiCtx};
use super::theme::{StatusKind, UiTheme};

pub const SIDEBAR_COMPACT_W: usize = 52;

/// Compute adaptive sidebar width: uses readable full width at normal geometry,
/// or switches to `SIDEBAR_COMPACT_W` icon-rail (`< 104`) on narrow/HiDPI tiling.
pub fn sidebar_preferred_w(
    t: UiTheme,
    win_w: usize,
    items: &[(&[u8], IconKind)],
) -> usize {
    let mut max_lw = 0usize;
    for &(label, _) in items {
        let lw = measure_text(FontId::SansSemiBold, t.body_px, label).max(0) as usize;
        if lw > max_lw {
            max_lw = lw;
        }
    }
    let full_w = max_lw + 58;
    let min_content = (390 * t.dpi_pct as usize) / 100;
    if win_w >= 540 && full_w + t.sp_md * 2 + t.sp_sm + min_content <= win_w {
        full_w.clamp(124, 176)
    } else {
        SIDEBAR_COMPACT_W
    }
}

pub fn sidebar_nav(
    c: &mut Canvas,
    ui: &mut UiCtx,
    base_id: u16,
    r: Rect,
    items: &[(&[u8], IconKind)],
    selected: &mut usize,
) -> bool {
    let t = ui.theme;
    c.fill_bordered_rrect_aa(r, t.r_md, t.bg_sidebar, t.border_subtle);
    let compact = r.w < 104;
    let inner = inset_rect(r, if compact { 6 } else { 8 }, 10);
    let row_h = 32usize;
    let gap = 4usize;
    let mut changed = false;

    for (i, &(label, icon)) in items.iter().enumerate() {
        let ry = inner.y + i * (row_h + gap);
        if ry + row_h > inner.y + inner.h {
            break;
        }
        let rr = Rect::new(inner.x, ry, inner.w, row_h);
        let resp = ui.interact(base_id.wrapping_add(i as u16), rr, true, false);
        if resp.clicked && *selected != i {
            *selected = i;
            changed = true;
        }
        let is_sel = *selected == i;
        if is_sel {
            c.fill_bordered_rrect_aa(rr, t.r_sm, t.bg_selected, t.border_strong);
            c.fill_rrect_aa(Rect::new(rr.x + 2, rr.y + 6, 3, rr.h.saturating_sub(12)), 2, t.accent_fg, 255);
        } else if resp.hovered {
            c.fill_rrect_aa(rr, t.r_sm, t.bg_hover, 210);
        }
        if resp.focused {
            c.stroke_rrect_aa(rr, t.r_sm, t.focus_ring, 210);
        }

        let ic_sz = 16usize;
        let ix = if compact {
            rr.x + (rr.w.saturating_sub(ic_sz)) / 2
        } else {
            rr.x + 10
        };
        let ir = Rect::new(ix, rr.y + (rr.h - ic_sz) / 2, ic_sz, ic_sz);
        let fg = if is_sel { t.fg_primary } else { t.fg_secondary };
        let acc = if is_sel { t.accent_fg } else { t.fg_muted };
        draw_vector_icon(c, ir, icon, fg, acc);

        if !compact {
            let font = if is_sel { FontId::SansSemiBold } else { FontId::SansRegular };
            let m = font_metrics(font, t.body_px);
            let ty = rr.y as i32 + ((rr.h as i32 - (m.ascent + m.descent)) / 2);
            let tx = (rr.x + 32) as i32;
            c.draw_text_ellipsis(
                tx,
                ty,
                (rr.x + rr.w).saturating_sub(tx as usize + 6) as i32,
                font,
                t.body_px,
                label,
                fg,
            );
        }
    }
    changed
}

/// Floating hover tooltip for compact icon-rail sidebars, painted after right pane cards.
pub fn sidebar_tooltip_overlay(
    c: &mut Canvas,
    t: UiTheme,
    sb_r: Rect,
    items: &[(&[u8], IconKind)],
    base_id: u16,
    hovered_id: u16,
) {
    if sb_r.w >= 104 || hovered_id < base_id {
        return;
    }
    let idx = (hovered_id - base_id) as usize;
    if idx >= items.len() {
        return;
    }
    let label = items[idx].0;
    let ry = sb_r.y + 10 + idx * 36;
    let tw = measure_text(FontId::SansSemiBold, t.caption_px, label).max(20) as usize;
    let pw = tw + 18;
    let ph = 26usize;
    let px = (sb_r.x + sb_r.w + 6).min(c.w.saturating_sub(pw + 6));
    let tip_r = Rect::new(px, ry + 3, pw, ph);
    c.fill_bordered_rrect_aa(tip_r, t.r_sm, t.bg_card_alt, t.accent);
    c.draw_text_centered(tip_r, FontId::SansSemiBold, t.caption_px, label, t.fg_primary);
}

pub fn tab_bar(
    c: &mut Canvas,
    ui: &mut UiCtx,
    base_id: u16,
    r: Rect,
    tabs: &[&[u8]],
    active: &mut usize,
) -> bool {
    let t = ui.theme;
    c.hline(r.x, r.y + r.h.saturating_sub(1), r.w, t.border_subtle, 255);
    let mut cx = r.x;
    let mut changed = false;

    for (i, &label) in tabs.iter().enumerate() {
        let tw = (measure_text(FontId::SansSemiBold, t.body_px, label) as usize + 22).max(64);
        if cx + tw > r.x + r.w {
            break;
        }
        let tr = Rect::new(cx, r.y, tw, r.h.saturating_sub(1));
        let resp = ui.interact(base_id.wrapping_add(i as u16), tr, true, false);
        if resp.clicked && *active != i {
            *active = i;
            changed = true;
        }
        let is_act = *active == i;
        if is_act {
            c.fill_rrect_aa(
                Rect::new(tr.x, tr.y + 1, tr.w, tr.h.saturating_sub(2)),
                t.r_sm,
                t.bg_card_alt,
                255,
            );
            c.fill_rect(Rect::new(tr.x + 8, r.y + r.h.saturating_sub(3), tr.w.saturating_sub(16), 2), t.accent_fg);
        } else if resp.hovered {
            c.fill_rrect_aa(tr, t.r_sm, t.bg_hover, 180);
        }
        if resp.focused {
            c.stroke_rrect_aa(tr, t.r_sm, t.focus_ring, 200);
        }
        let font = if is_act { FontId::SansSemiBold } else { FontId::SansRegular };
        let fg = if is_act { t.fg_primary } else { t.fg_muted };
        c.draw_text_centered(tr, font, t.body_px, label, fg);
        cx += tw + 4;
    }
    changed
}

pub fn breadcrumb_bar(
    c: &mut Canvas,
    ui: &mut UiCtx,
    base_id: u16,
    r: Rect,
    segments: &[&[u8]],
) -> Option<usize> {
    let t = ui.theme;
    c.fill_bordered_rrect_aa(r, t.r_sm, t.bg_card_alt, t.border_subtle);
    let mut cx = r.x + 8;
    let mut clicked_idx = None;
    let m = font_metrics(FontId::SansSemiBold, t.body_px);
    let cy = r.y + (r.h.saturating_sub(14)) / 2;

    for (i, &seg) in segments.iter().enumerate() {
        let is_last = i + 1 == segments.len();
        let font = if is_last { FontId::SansSemiBold } else { FontId::SansRegular };
        let sw = (measure_text(font, t.body_px, seg) as usize + 14).max(28);
        if cx + sw > r.x + r.w.saturating_sub(8) {
            break;
        }
        let sr = Rect::new(cx, r.y + 3, sw, r.h.saturating_sub(6));
        let resp = ui.interact(base_id.wrapping_add(i as u16), sr, !is_last, false);
        if resp.clicked {
            clicked_idx = Some(i);
        }
        if resp.hovered && !is_last {
            c.fill_rrect_aa(sr, t.r_sm, t.bg_hover, 200);
        }
        let fg = if is_last { t.fg_primary } else { t.fg_muted };
        let ty = sr.y as i32 + ((sr.h as i32 - (m.ascent + m.descent)) / 2);
        c.draw_text(sr.x as i32 + 6, ty, font, t.body_px, seg, fg);
        cx += sw;

        if !is_last && cx + 16 < r.x + r.w {
            draw_vector_icon(c, Rect::new(cx, cy, 14, 14), IconKind::ChevronRight, t.fg_muted, t.fg_muted);
            cx += 16;
        }
    }
    clicked_idx
}

pub fn card(c: &mut Canvas, t: UiTheme, r: Rect, title: Option<&[u8]>, subtitle: Option<&[u8]>) -> Rect {
    c.fill_bordered_rrect_aa(r, t.r_md, t.bg_card, t.border_subtle);
    let mut inner = inset_rect(r, t.sp_md, t.sp_md);
    if let Some(ttl) = title {
        let m = font_metrics(FontId::SansSemiBold, t.heading_px);
        let adv = c.draw_text_ellipsis(
            inner.x as i32,
            inner.y as i32,
            inner.w as i32,
            FontId::SansSemiBold,
            t.heading_px,
            ttl,
            t.fg_primary,
        );
        if let Some(sub) = subtitle {
            let sx = inner.x as i32 + adv + 10;
            let rem = (inner.x + inner.w).saturating_sub(sx.max(0) as usize) as i32;
            if rem > 24 {
                c.draw_text_ellipsis(
                    sx,
                    inner.y as i32 + 2,
                    rem,
                    FontId::SansRegular,
                    t.caption_px,
                    sub,
                    t.fg_muted,
                );
            }
        }
        let head_h = (m.line_height as usize).max(16) + 8;
        c.hline(inner.x, inner.y + head_h - 3, inner.w, t.border_subtle, 180);
        inner = Rect::new(
            inner.x,
            inner.y + head_h,
            inner.w,
            inner.h.saturating_sub(head_h),
        );
    }
    inner
}

pub fn status_badge(c: &mut Canvas, t: UiTheme, x: usize, y: usize, text: &[u8], kind: StatusKind) -> usize {
    let (bg, fg) = t.status_colors(kind);
    let sz = t.caption_px;
    let m = font_metrics(FontId::SansSemiBold, sz);
    let tw = measure_text(FontId::SansSemiBold, sz, text).max(0) as usize;
    let bw = tw + 22;
    let bh = (m.line_height as usize + 4).max(20);
    let br = Rect::new(x, y, bw, bh);
    c.fill_rrect_aa(br, bh / 2, bg, 255);
    c.fill_circle_aa((x as i32 + 8) * 16, (y as i32 + bh as i32 / 2) * 16, 44, fg, 255);
    let ty = y as i32 + ((bh as i32 - (m.ascent + m.descent)) / 2);
    c.draw_text((x + 15) as i32, ty, FontId::SansSemiBold, sz, text, fg);
    bw
}

pub fn progress_bar(c: &mut Canvas, t: UiTheme, r: Rect, pct: u8, kind: StatusKind) {
    let (_, fill) = t.status_colors(kind);
    let fill_col = if kind == StatusKind::Neutral { t.accent_fg } else { fill };
    c.fill_bordered_rrect_aa(r, r.h / 2, t.bg_input, t.border_subtle);
    let p = pct.min(100) as usize;
    if p > 0 && r.w > 4 {
        let fw = ((r.w - 2) * p / 100).max(r.h.min(r.w));
        c.fill_rrect_aa(Rect::new(r.x + 1, r.y + 1, fw.min(r.w - 2), r.h.saturating_sub(2)), r.h / 2, fill_col, 255);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn metric_card(
    c: &mut Canvas,
    t: UiTheme,
    r: Rect,
    label: &[u8],
    value: &[u8],
    sub: &[u8],
    kind: StatusKind,
    pct: Option<u8>,
) {
    c.fill_bordered_rrect_aa(r, t.r_md, t.bg_card, t.border_subtle);
    let (_, fg_accent) = t.status_colors(kind);
    let accent = if kind == StatusKind::Neutral { t.accent_fg } else { fg_accent };
    let inner = inset_rect(r, 12, 8);
    if inner.w < 16 || inner.h < 12 {
        return;
    }
    let mut mc = c.sub_clip(inner);
    let m_cap = font_metrics(FontId::SansSemiBold, t.caption_px);
    let m_val = font_metrics(FontId::SansSemiBold, t.title_px);
    let cap_h = m_cap.line_height.max(12) as usize;
    let val_h = m_val.line_height.max(16) as usize;

    mc.draw_text_ellipsis(
        inner.x as i32,
        inner.y as i32,
        inner.w as i32 - 12,
        FontId::SansSemiBold,
        t.caption_px,
        label,
        t.fg_muted,
    );
    mc.fill_circle_aa((inner.x + inner.w - 4) as i32 * 16, (inner.y + 6) as i32 * 16, 48, accent, 255);

    if inner.h >= cap_h + val_h + 2 {
        mc.draw_text_ellipsis(
            inner.x as i32,
            (inner.y + cap_h + 2) as i32,
            inner.w as i32,
            FontId::SansSemiBold,
            t.title_px,
            value,
            t.fg_primary,
        );
    }

    if let Some(p) = pct {
        if inner.h >= cap_h + val_h + 26 {
            progress_bar(&mut mc, t, Rect::new(inner.x, inner.y + cap_h + val_h + 6, inner.w, 7), p, kind);
        }
    }
    if !sub.is_empty() && inner.h >= cap_h + val_h + cap_h + 6 {
        mc.draw_text_ellipsis(
            inner.x as i32,
            (inner.y + inner.h.saturating_sub(cap_h)) as i32,
            inner.w as i32,
            FontId::SansRegular,
            t.caption_px,
            sub,
            t.fg_secondary,
        );
    }
}

pub fn table_header(c: &mut Canvas, t: UiTheme, r: Rect, cols: &[(&[u8], usize)]) {
    c.fill_rrect_aa(r, t.r_sm, t.bg_card_alt, 255);
    c.hline(r.x + 2, r.y + r.h.saturating_sub(1), r.w.saturating_sub(4), t.border_subtle, 255);
    let m = font_metrics(FontId::SansSemiBold, t.caption_px);
    let ty = r.y as i32 + ((r.h as i32 - (m.ascent + m.descent)) / 2);
    let mut cx = r.x + 10;
    for &(title, col_w) in cols {
        if cx >= r.x + r.w {
            break;
        }
        let max_w = (r.x + r.w - cx).min(col_w).saturating_sub(6) as i32;
        c.draw_text_ellipsis(cx as i32, ty, max_w, FontId::SansSemiBold, t.caption_px, title, t.fg_muted);
        cx += col_w;
    }
}

pub fn list_row(
    c: &mut Canvas,
    ui: &mut UiCtx,
    id: u16,
    r: Rect,
    selected: bool,
    alt: bool,
) -> bool {
    let resp = ui.interact(id, r, true, false);
    let t = ui.theme;
    if selected {
        c.fill_bordered_rrect_aa(r, t.r_sm, t.bg_selected, t.focus_ring);
    } else if resp.hovered {
        c.fill_rrect_aa(r, t.r_sm, t.bg_hover, 230);
    } else if alt {
        c.fill_rrect_aa(r, t.r_sm, t.bg_card_alt, 120);
    }
    if resp.focused {
        c.stroke_rrect_aa(r, t.r_sm, t.focus_ring, 210);
    }
    resp.clicked
}

pub fn modal_dialog_frame(
    c: &mut Canvas,
    t: UiTheme,
    viewport: Rect,
    modal_r: Rect,
    title: &[u8],
) -> Rect {
    c.fill_rect_alpha(viewport, 0x0000_0000, 150);
    let shadow = Rect::new(modal_r.x + 4, modal_r.y + 6, modal_r.w, modal_r.h);
    c.fill_rrect_aa(shadow, t.r_lg, 0x0000_0000, 96);
    c.fill_bordered_rrect_aa(modal_r, t.r_lg, t.bg_card, t.border_strong);
    let m = font_metrics(FontId::SansSemiBold, t.heading_px);
    c.draw_text_ellipsis(
        (modal_r.x + 16) as i32,
        (modal_r.y + 12) as i32,
        modal_r.w.saturating_sub(32) as i32,
        FontId::SansSemiBold,
        t.heading_px,
        title,
        t.fg_primary,
    );
    let head_h = (m.line_height as usize + 18).max(36);
    c.hline(modal_r.x + 12, modal_r.y + head_h - 1, modal_r.w.saturating_sub(24), t.border_subtle, 255);
    Rect::new(
        modal_r.x + 14,
        modal_r.y + head_h + 8,
        modal_r.w.saturating_sub(28),
        modal_r.h.saturating_sub(head_h + 20),
    )
}

pub fn popover_menu(
    c: &mut Canvas,
    ui: &mut UiCtx,
    base_id: u16,
    menu_r: Rect,
    items: &[(&[u8], Option<IconKind>)],
) -> Option<usize> {
    let t = ui.theme;
    let shadow = Rect::new(menu_r.x + 3, menu_r.y + 4, menu_r.w, menu_r.h);
    c.fill_rrect_aa(shadow, t.r_md, 0x0000_0000, 90);
    c.fill_bordered_rrect_aa(menu_r, t.r_md, t.bg_card, t.border_strong);
    let inner = inset_rect(menu_r, 6, 6);
    let row_h = 28usize;
    let m = font_metrics(FontId::SansRegular, t.body_px);
    let mut picked = None;

    for (i, &(label, ic)) in items.iter().enumerate() {
        let rr = Rect::new(inner.x, inner.y + i * row_h, inner.w, row_h.saturating_sub(2));
        if rr.y + rr.h > inner.y + inner.h {
            break;
        }
        let resp = ui.interact(base_id.wrapping_add(i as u16), rr, true, false);
        if resp.clicked {
            picked = Some(i);
        }
        if resp.hovered {
            c.fill_rrect_aa(rr, t.r_sm, t.bg_hover, 255);
        }
        let mut tx = rr.x + 8;
        if let Some(kind) = ic {
            draw_vector_icon(c, Rect::new(tx, rr.y + 5, 14, 14), kind, t.fg_secondary, t.accent_fg);
            tx += 22;
        }
        let ty = rr.y as i32 + ((rr.h as i32 - (m.ascent + m.descent)) / 2);
        c.draw_text_ellipsis(
            tx as i32,
            ty,
            (rr.x + rr.w).saturating_sub(tx + 6) as i32,
            FontId::SansRegular,
            t.body_px,
            label,
            t.fg_primary,
        );
    }
    picked
}
