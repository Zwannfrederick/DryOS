// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Shared DRY Glass 2.0 UI view & Help dialog for `dry-settings` (`Settings`).

use crate::apps::{PixelAppWindow, PointerStep};
use crate::gfx::Rect;
use crate::sup_services::SupervisorPrefs;
use crate::ttf::{font_metrics, measure_text, FontId};
use crate::ui::app_icons::{draw_app_icon, AppIconKind};
use crate::ui::brand::{draw_brand_symbol, draw_brand_wordmark};
use crate::ui::containers::{
    card, list_row, sidebar_nav, sidebar_preferred_w, sidebar_tooltip_overlay, status_badge,
};
use crate::ui::icons::{draw_sys_icon_id, draw_vector_icon, IconKind};
use crate::ui::canvas::VectorPalette;
use crate::ui::layout::{inset_rect, split_bot, split_left, split_top, UiCtx};
use crate::ui::theme::{StatusKind, UiTheme};
use crate::ui::widgets::{
    button, scrollbar_v, segmented_control, toggle_switch, ButtonOpts, ButtonVariant,
};

pub const SET_PIXEL_VADDR: u64 = 0x1c00_0000;
pub const DLG_PIXEL_VADDR: u64 = 0x1c20_0000;
pub const SET_WIN_W: u16 = 620;
pub const SET_WIN_H: u16 = 410;
pub const DLG_WIN_W: u16 = 380;
pub const DLG_WIN_H: u16 = 210;
pub const ROW_COUNT: usize = 10;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsAction {
    None,
    SelectCategory(usize),
    SelectRow(usize),
    AdjustRow(usize, i32),
    SetKeymap(u8),
    SetPointerSpeed(u8),
    SetScaleBits(u8),
    ToggleHelp,
    TriggerGpuTest,
    ToggleShowcase,
}

pub struct SettingsView<'a> {
    pub prefs: SupervisorPrefs,
    pub sel_row: usize,
    pub list_scroll: usize,
    pub dlg_open: bool,
    pub dpi_200: bool,
    pub showcase_open: bool,
    pub showcase_scroll: usize,
    pub status_msg: &'a [u8],
}

fn row_spec(idx: usize, light_mode: bool) -> (IconKind, &'static [u8], &'static [u8]) {
    match idx {
        0 => (IconKind::WorkspaceGrid, b"Window Layout", b"[1]"),
        1 => (IconKind::Keyboard, b"Keyboard", b"[2]"),
        2 => (IconKind::Mouse, b"Pointer Speed", b"[3]"),
        3 => (IconKind::Accessibility, b"High Contrast", b"[4]"),
        4 => (IconKind::Mouse, b"Large Cursor", b"[5]"),
        5 => (IconKind::Power, b"Idle Blank", b"[6]"),
        6 => (IconKind::Lock, b"Session Lock", b"[7/L]"),
        7 => (
            if light_mode { IconKind::Sun } else { IconKind::Moon },
            b"Daylight Theme",
            b"[8/T]",
        ),
        8 => (IconKind::QuickSettings, b"Reduced Blur", b"[B]"),
        _ => (IconKind::Display, b"Display Scale", b"[0/9]"),
    }
}

const SETTINGS_CATS: [(&[u8], IconKind); 5] = [
    (b"Workspace", IconKind::WorkspaceGrid),
    (b"Input", IconKind::Keyboard),
    (b"Appearance", IconKind::Sun),
    (b"Security", IconKind::Shield),
    (b"Identity", IconKind::WorkspaceGrid),
];

pub fn settings_vis_rows(t: UiTheme, w: usize, h: usize) -> usize {
    let top_h = (t.ctrl_h + 12).clamp(40, 56);
    let foot_h = 30usize;
    let mid_h = h.saturating_sub(top_h + foot_h + t.sp_sm * 2);
    let sb_w = sidebar_preferred_w(t, w, &SETTINGS_CATS);
    let hdr_h = if sb_w < 104 { 28usize } else { 0usize };
    let m_body = font_metrics(FontId::SansRegular, t.body_px);
    let row_h = (m_body.line_height as usize + 18).clamp(36, 56);
    (mid_h.saturating_sub(14 + hdr_h) / row_h).max(1)
}

struct ShowcaseLayout {
    brand_r: Rect,
    grid_r: Rect,
    sys_r: Rect,
    cols: usize,
    cell_w: usize,
    cell_h: usize,
    vis_rows: usize,
    max_scroll: usize,
}

fn compute_showcase_layout(t: UiTheme, list_r: Rect) -> ShowcaseLayout {
    let dpi_s = t.dpi_pct as usize;
    let pad = (10 * dpi_s / 100).clamp(8, 16);
    let brand_h = (52 * dpi_s / 100).clamp(46, 72);
    let sys_h = (32 * dpi_s / 100).clamp(28, 44);
    let inner = inset_rect(list_r, pad, (pad / 2).max(4));
    let brand_r = Rect::new(inner.x, inner.y, inner.w, brand_h);
    let grid_y = brand_r.y + brand_r.h + 6;
    let sys_y = (list_r.y + list_r.h).saturating_sub(sys_h + (pad / 2).max(4));
    let grid_h = sys_y.saturating_sub(grid_y + 6);
    let grid_r = Rect::new(inner.x, grid_y, inner.w, grid_h);
    let sys_r = Rect::new(inner.x, sys_y, inner.w, sys_h);

    let cell_w_target = (60 * dpi_s / 100).clamp(58, 108);
    let cols = (grid_r.w.saturating_sub(8) / cell_w_target).clamp(3, 7);
    let cell_w = (grid_r.w.saturating_sub(8)) / cols;
    let cell_h = (60 * dpi_s / 100).clamp(56, 98);
    let vis_rows = (grid_r.h / cell_h).max(1);
    let total_rows = AppIconKind::ALL.len().div_ceil(cols);
    ShowcaseLayout {
        brand_r,
        grid_r,
        sys_r,
        cols,
        cell_w,
        cell_h,
        vis_rows,
        max_scroll: total_rows.saturating_sub(vis_rows),
    }
}

pub fn showcase_grid_metrics(t: UiTheme, w: usize, h: usize) -> (usize, usize) {
    let top_h = (t.ctrl_h + 12).clamp(40, 56);
    let mid_outer = Rect::new(0, top_h, w, h.saturating_sub(top_h + 30));
    let mid_r = inset_rect(mid_outer, t.sp_md, t.sp_sm);
    let sb_w = sidebar_preferred_w(t, w, &SETTINGS_CATS);
    let (_, list_r) = split_left(mid_r, sb_w, t.sp_sm);
    let lay = compute_showcase_layout(t, list_r);
    (lay.cols, lay.max_scroll)
}

const SHOWCASE_SYS_SAMPLE: &[(&[u8], &[u8])] = &[
    (b"cpu", b"CPU"),
    (b"ram", b"RAM"),
    (b"drive-nvme", b"NVMe"),
    (b"gpu", b"GPU"),
    (b"display", b"Disp"),
    (b"network-eth", b"Net"),
    (b"usb-port", b"USB"),
    (b"battery", b"Pwr"),
    (b"permission-cap", b"Cap"),
    (b"driver-island", b"Drv"),
    (b"status-success", b"OK"),
    (b"progress-ring", b"Prog"),
];

fn render_showcase_panel(
    c: &mut crate::ui::canvas::Canvas,
    t: UiTheme,
    list_r: Rect,
    scroll: usize,
) {
    let dpi_s = t.dpi_pct as usize;
    let lay = compute_showcase_layout(t, list_r);
    let brand_r = lay.brand_r;
    c.fill_bordered_rrect_aa(brand_r, t.r_sm, t.bg_card_alt, t.border_subtle);

    let sym_sz = (brand_r.h.saturating_sub(14)).clamp(24, 48);
    let sym_x = brand_r.x + 10;
    let sym_y = brand_r.y + (brand_r.h.saturating_sub(sym_sz)) / 2;
    draw_brand_symbol(c, Rect::new(sym_x, sym_y, sym_sz, sym_sz), t.mode, false);

    let wm_x = sym_x + sym_sz + 10;
    let wm_w = (128 * dpi_s / 100).min(brand_r.w.saturating_sub(sym_sz + 24));
    let wm_h = (wm_w / 5).max(20).min(brand_r.h.saturating_sub(10));
    draw_brand_wordmark(
        c,
        Rect::new(wm_x, brand_r.y + (brand_r.h.saturating_sub(wm_h)) / 2, wm_w, wm_h),
        t.mode,
    );

    let lbl_x = wm_x + wm_w + 10;
    let lbl_px = (10 * dpi_s / 100).clamp(10, 16) as u8;
    if lbl_x + 88 <= brand_r.x + brand_r.w {
        let m = font_metrics(FontId::SansSemiBold, lbl_px);
        let ly = brand_r.y as i32 + ((brand_r.h as i32 - (m.ascent + m.descent)) / 2);
        c.draw_text_ellipsis(
            lbl_x as i32,
            ly,
            (brand_r.x + brand_r.w).saturating_sub(lbl_x + 8) as i32,
            FontId::SansSemiBold,
            lbl_px,
            b"21 Apps + 91 Sys",
            t.fg_secondary,
        );
    }

    let grid_r = lay.grid_r;
    let ic_sz = (30 * dpi_s / 100).clamp(26, 48).min(lay.cell_w.saturating_sub(12));
    let cap_px = (9 * dpi_s / 100).clamp(9, 15) as u8;
    let total_rows = AppIconKind::ALL.len().div_ceil(lay.cols);
    let start_row = scroll.min(lay.max_scroll);

    let mut gc = c.sub_clip(grid_r);
    for (idx, &kind) in AppIconKind::ALL.iter().enumerate() {
        let row = idx / lay.cols;
        if row < start_row || row >= start_row + lay.vis_rows {
            continue;
        }
        let col = idx % lay.cols;
        let vr = row - start_row;
        let cx = grid_r.x + col * lay.cell_w;
        let cy = grid_r.y + vr * lay.cell_h;
        let card_r = Rect::new(cx + 2, cy + 2, lay.cell_w.saturating_sub(4), lay.cell_h.saturating_sub(4));
        gc.fill_bordered_rrect_aa(card_r, t.r_sm, t.bg_card_alt, t.border_subtle);
        let ir = Rect::new(card_r.x + (card_r.w.saturating_sub(ic_sz)) / 2, card_r.y + 5, ic_sz, ic_sz);
        draw_app_icon(&mut gc, ir, kind, t.mode);
        let tw = measure_text(FontId::SansRegular, cap_px, kind.label()).max(0) as usize;
        let lx = card_r.x + (card_r.w.saturating_sub(tw.min(card_r.w.saturating_sub(4)))) / 2;
        let ly = (ir.y + ir.h + 3) as i32;
        gc.draw_text_ellipsis(
            lx as i32,
            ly,
            card_r.w.saturating_sub(4) as i32,
            FontId::SansRegular,
            cap_px,
            kind.label(),
            t.fg_primary,
        );
    }

    if total_rows > lay.vis_rows {
        let sb = Rect::new(grid_r.x + grid_r.w.saturating_sub(6), grid_r.y + 2, 5, grid_r.h.saturating_sub(4));
        scrollbar_v(c, sb, total_rows, lay.vis_rows, start_row, t.bg_card_alt, t.border_strong);
    }

    let sys_r = lay.sys_r;
    c.fill_bordered_rrect_aa(sys_r, t.r_sm, t.bg_card_alt, t.border_subtle);
    let pal = VectorPalette::from_mode(t.mode);
    let s_ic = (16 * dpi_s / 100).clamp(14, 24).min(sys_r.h.saturating_sub(8));
    let mut sx = sys_r.x + 8;
    let sy = sys_r.y + (sys_r.h.saturating_sub(s_ic)) / 2;
    for &(id, _) in SHOWCASE_SYS_SAMPLE {
        if sx + s_ic + 6 > sys_r.x + sys_r.w {
            break;
        }
        draw_sys_icon_id(c, Rect::new(sx, sy, s_ic, s_ic), id, pal);
        sx += s_ic + (sys_r.w.saturating_sub(16) / SHOWCASE_SYS_SAMPLE.len()).saturating_sub(s_ic).max(4);
    }
}

pub fn category_for_row(row: usize) -> usize {
    match row {
        0 => 0,
        1 | 2 => 1,
        3 | 4 | 7 | 8 | 9 => 2,
        _ => 3,
    }
}

pub fn first_row_for_category(cat: usize) -> usize {
    match cat {
        0 => 0,
        1 => 1,
        2 => 7,
        _ => 5,
    }
}

pub fn render_settings(
    win: &mut PixelAppWindow,
    view: &SettingsView<'_>,
    step: PointerStep,
) -> SettingsAction {
    let base_t = win.theme();
    let t = UiTheme::new(
        base_t.mode,
        if view.dpi_200 { 200 } else { base_t.dpi_pct },
    );
    let w = win.w as usize;
    let h = win.h as usize;

    let top_h = (t.ctrl_h + 12).clamp(40, 56);
    let foot_h = 30usize;
    let root = Rect::new(0, 0, w, h);
    let (top_r, rest1) = split_top(root, top_h, 0);
    let (mid_outer, foot_r) = split_bot(rest1, foot_h, 0);
    let mid_r = inset_rect(mid_outer, t.sp_md, t.sp_sm);

    let sb_w = sidebar_preferred_w(t, w, &SETTINGS_CATS);
    let (sb_r, list_r) = split_left(mid_r, sb_w, t.sp_sm);

    let mut c = win.canvas();
    c.clear(t.bg_app);

    // Top bar
    c.fill_rect(top_r, t.bg_sidebar);
    c.hline(top_r.x, top_r.y + top_r.h.saturating_sub(1), top_r.w, t.border_subtle, 220);

    let mut ui = UiCtx::begin(
        &mut win.persist,
        t,
        step.clicked,
        step.released,
        step.scroll_dy,
        None,
    );
    let mut act = SettingsAction::None;

    let btn_h = top_h.saturating_sub(14).max(26);
    let btn_y = top_r.y + (top_h.saturating_sub(btn_h)) / 2;
    let help_lbl = b"Help (H)";
    let gpu_lbl = b"GPU Test";
    let icons_lbl = b"Icons (V)";
    let help_w = (measure_text(FontId::SansSemiBold, t.caption_px, help_lbl).max(48) as usize) + 38;
    let gpu_w = (measure_text(FontId::SansSemiBold, t.caption_px, gpu_lbl).max(52) as usize) + 38;
    let icons_w = (measure_text(FontId::SansSemiBold, t.caption_px, icons_lbl).max(50) as usize) + 38;
    let help_x = w.saturating_sub(t.sp_md + help_w);
    let gpu_x = help_x.saturating_sub(6 + gpu_w);
    let icons_x = gpu_x.saturating_sub(6 + icons_w);

    let show_icons_btn = icons_x >= t.sp_md + 145;
    if show_icons_btn
        && button(
            &mut c,
            &mut ui,
            19,
            Rect::new(icons_x, btn_y, icons_w, btn_h),
            ButtonOpts {
                label: icons_lbl,
                icon: Some(IconKind::WorkspaceGrid),
                variant: ButtonVariant::Secondary,
                disabled: false,
                selected: view.showcase_open,
            },
        )
    {
        act = SettingsAction::ToggleShowcase;
    }

    if button(
        &mut c,
        &mut ui,
        20,
        Rect::new(gpu_x, btn_y, gpu_w, btn_h),
        ButtonOpts {
            label: gpu_lbl,
            icon: Some(IconKind::Shield),
            variant: ButtonVariant::Secondary,
            disabled: false,
            selected: false,
        },
    ) {
        act = SettingsAction::TriggerGpuTest;
    }
    if button(
        &mut c,
        &mut ui,
        21,
        Rect::new(help_x, btn_y, help_w, btn_h),
        ButtonOpts {
            label: help_lbl,
            icon: Some(IconKind::Info),
            variant: ButtonVariant::Secondary,
            disabled: false,
            selected: view.dlg_open,
        },
    ) {
        act = SettingsAction::ToggleHelp;
    }

    // Status badges on left of top bar
    let (st_lbl, st_kind): (&[u8], StatusKind) = if view.prefs.persist_backed {
        (b"DRYFS Persist", StatusKind::Ok)
    } else {
        (b"Volatile RAM", StatusKind::Warn)
    };
    let badge_limit = if show_icons_btn { icons_x } else { gpu_x };
    let sb_h = (font_metrics(FontId::SansSemiBold, t.caption_px).line_height as usize + 6).max(20);
    let badge_y = top_r.y + (top_h.saturating_sub(sb_h)) / 2;
    let st_w = measure_text(FontId::SansSemiBold, t.caption_px, st_lbl).max(0) as usize + 22;
    let mut dpi_x = t.sp_md;
    if t.sp_md + st_w + 8 <= badge_limit {
        status_badge(&mut c, t, t.sp_md, badge_y, st_lbl, st_kind);
        dpi_x = t.sp_md + st_w + 8;
    }
    let dpi_lbl: &[u8] = if view.dpi_200 {
        b"200% DPI"
    } else {
        match (view.prefs.theme_flags >> 4) & 3 {
            1 => b"125% DPI",
            2 => b"150% DPI",
            _ => b"100% DPI",
        }
    };
    let dpi_w = measure_text(FontId::SansSemiBold, t.caption_px, dpi_lbl).max(0) as usize + 22;
    if dpi_x + dpi_w + 8 <= badge_limit {
        status_badge(&mut c, t, dpi_x, badge_y, dpi_lbl, StatusKind::Accent);
    }

    // Left category sidebar
    let mut cat = if view.showcase_open {
        4
    } else {
        category_for_row(view.sel_row).min(SETTINGS_CATS.len() - 2)
    };
    if sidebar_nav(&mut c, &mut ui, 30, sb_r, &SETTINGS_CATS, &mut cat) {
        act = SettingsAction::SelectCategory(cat);
    }

    // Right settings list card or Visual Identity Showcase
    c.fill_bordered_rrect_aa(list_r, t.r_md, t.bg_card, t.border_subtle);
    if view.showcase_open {
        render_showcase_panel(&mut c, t, list_r, view.showcase_scroll);
        let hov_id = ui.hovered_id();
        ui.finish();
        sidebar_tooltip_overlay(&mut c, t, sb_r, &SETTINGS_CATS, 30, hov_id);

        c.fill_rect(foot_r, t.bg_sidebar);
        c.hline(foot_r.x, foot_r.y, foot_r.w, t.border_subtle, 220);
        let cm = font_metrics(FontId::SansRegular, t.caption_px);
        let fy = foot_r.y as i32 + ((foot_r.h as i32 - (cm.ascent + cm.descent)) / 2);
        c.draw_text_ellipsis(
            t.sp_md as i32,
            fy,
            w.saturating_sub(t.sp_md * 2) as i32,
            FontId::SansRegular,
            t.caption_px,
            b"[V] Back to Settings  [T/4] Theme  [0/9] Scale 100%-200%  [Up/Down] Scroll",
            t.fg_secondary,
        );
        return act;
    }
    let hdr_h = if sb_w < 104 && list_r.h >= 26 {
        let bh = 24usize.min(list_r.h.saturating_sub(8));
        let hdr_r = Rect::new(list_r.x + 8, list_r.y + 4, list_r.w.saturating_sub(16), bh);
        c.fill_rrect_aa(hdr_r, t.r_sm, t.bg_card_alt, 235);
        let active_cat = SETTINGS_CATS[cat];
        if bh >= 16 {
            draw_vector_icon(
                &mut c,
                Rect::new(hdr_r.x + 8, hdr_r.y + (bh.saturating_sub(14)) / 2, 14, 14),
                active_cat.1,
                t.fg_primary,
                t.accent_fg,
            );
        }
        let hm = font_metrics(FontId::SansSemiBold, t.caption_px);
        let hy = hdr_r.y as i32 + ((hdr_r.h as i32 - (hm.ascent + hm.descent)) / 2);
        let mut hc = c.sub_clip(hdr_r);
        hc.draw_text(
            (hdr_r.x + 28) as i32,
            hy,
            FontId::SansSemiBold,
            t.caption_px,
            active_cat.0,
            t.fg_primary,
        );
        bh + 4
    } else {
        0usize
    };
    let m_body = font_metrics(FontId::SansRegular, t.body_px);
    let row_h = (m_body.line_height as usize + 18).clamp(36, 56);
    let rows_area = Rect::new(
        list_r.x + 8,
        list_r.y + 7 + hdr_h,
        list_r.w.saturating_sub(22),
        list_r.h.saturating_sub(14 + hdr_h),
    );
    let vis_rows = (rows_area.h / row_h).max(1);
    let light_mode = (view.prefs.theme_flags & 4) != 0;

    for r in 0..vis_rows {
        let idx = view.list_scroll + r;
        if idx >= ROW_COUNT {
            break;
        }
        let rr = Rect::new(
            rows_area.x,
            rows_area.y + r * row_h,
            rows_area.w,
            row_h.saturating_sub(4),
        );
        let selected = idx == view.sel_row;
        let row_clicked = list_row(&mut c, &mut ui, 60 + r as u16, rr, selected, (idx & 1) == 1);

        let (ic, title, key_hint) = row_spec(idx, light_mode);
        draw_vector_icon(
            &mut c,
            Rect::new(rr.x + 8, rr.y + (rr.h.saturating_sub(16)) / 2, 16, 16),
            ic,
            if selected { t.accent_fg } else { t.fg_secondary },
            t.accent_fg,
        );

        let ctrl_h = rr.h.saturating_sub(8).clamp(24, 34);
        let ctrl_y = rr.y + (rr.h.saturating_sub(ctrl_h)) / 2;
        let dpi_s = t.dpi_pct as usize;
        let mut ctrl_acted = false;

        let ctrl_w: usize = match idx {
            1 | 2 => (124 * dpi_s / 100).min(rr.w * 42 / 100),
            3 | 4 | 7 | 8 => 46,
            9 => (210 * dpi_s / 100).min(rr.w * 58 / 100),
            _ => (112 * dpi_s / 100).min(rr.w * 38 / 100),
        };
        let ctrl_r = Rect::new(
            (rr.x + rr.w).saturating_sub(ctrl_w + 6),
            ctrl_y,
            ctrl_w,
            ctrl_h,
        );

        match idx {
            0 => {
                let lbl: &[u8] = match view.prefs.layout_mode {
                    1 => b"Tiled",
                    2 => b"Stacked",
                    3 => b"Tabbed",
                    _ => b"Floating",
                };
                if button(
                    &mut c,
                    &mut ui,
                    120 + idx as u16,
                    ctrl_r,
                    ButtonOpts {
                        label: lbl,
                        icon: Some(IconKind::ChevronRight),
                        variant: ButtonVariant::Secondary,
                        disabled: false,
                        selected: false,
                    },
                ) {
                    act = SettingsAction::AdjustRow(0, 1);
                    ctrl_acted = true;
                }
            }
            1 => {
                let mut km = if view.prefs.keymap == 1 { 1 } else { 0 };
                if segmented_control(&mut c, &mut ui, 200, ctrl_r, &[b"US", b"TR-Q"], &mut km) {
                    act = SettingsAction::SetKeymap(km as u8);
                    ctrl_acted = true;
                }
            }
            2 => {
                let mut spd = if view.prefs.pointer_speed >= 2 { 1 } else { 0 };
                if segmented_control(&mut c, &mut ui, 210, ctrl_r, &[b"1x", b"2x"], &mut spd) {
                    act = SettingsAction::SetPointerSpeed((spd + 1) as u8);
                    ctrl_acted = true;
                }
            }
            3 | 4 | 7 | 8 => {
                let bit = match idx {
                    3 => 1u8,
                    4 => 2u8,
                    7 => 4u8,
                    _ => 8u8,
                };
                let mut on = (view.prefs.theme_flags & bit) != 0;
                if toggle_switch(&mut c, &mut ui, 120 + idx as u16, ctrl_r, b"", &mut on, false) {
                    act = SettingsAction::AdjustRow(idx, 1);
                    ctrl_acted = true;
                }
            }
            5 => {
                let lbl: &[u8] = match view.prefs.blank_timeout_sec {
                    0 => b"Off",
                    5 => b"5 sec",
                    30 => b"30 sec",
                    60 => b"60 sec",
                    _ => b"Custom",
                };
                if button(
                    &mut c,
                    &mut ui,
                    120 + idx as u16,
                    ctrl_r,
                    ButtonOpts {
                        label: lbl,
                        icon: Some(IconKind::ChevronRight),
                        variant: ButtonVariant::Secondary,
                        disabled: false,
                        selected: false,
                    },
                ) {
                    act = SettingsAction::AdjustRow(5, 1);
                    ctrl_acted = true;
                }
            }
            6 => {
                if button(
                    &mut c,
                    &mut ui,
                    126,
                    ctrl_r,
                    ButtonOpts {
                        label: b"Lock Now",
                        icon: Some(IconKind::Lock),
                        variant: ButtonVariant::Danger,
                        disabled: false,
                        selected: false,
                    },
                ) {
                    act = SettingsAction::AdjustRow(6, 1);
                    ctrl_acted = true;
                }
            }
            _ => {
                let mut dpi_idx = if view.dpi_200 {
                    3
                } else {
                    ((view.prefs.theme_flags >> 4) & 3).min(2) as usize
                };
                if segmented_control(
                    &mut c,
                    &mut ui,
                    220,
                    ctrl_r,
                    &[b"100%", b"125%", b"150%", b"200%"],
                    &mut dpi_idx,
                ) {
                    act = SettingsAction::SetScaleBits(dpi_idx as u8);
                    ctrl_acted = true;
                }
            }
        }

        if row_clicked && !ctrl_acted {
            act = if selected {
                SettingsAction::AdjustRow(idx, 1)
            } else {
                SettingsAction::SelectRow(idx)
            };
        }

        let tx = rr.x + 30;
        let ty = rr.y as i32 + ((rr.h as i32 - (m_body.ascent + m_body.descent)) / 2);
        let hint_w = measure_text(FontId::MonoRegular, t.caption_px, key_hint).max(0) as usize + 12;
        let avail = ctrl_r.x.saturating_sub(tx + 8);
        let title_w = measure_text(FontId::SansSemiBold, t.body_px, title).max(0) as usize;
        let show_hint = avail >= title_w + hint_w + 8;
        let title_max = if show_hint { avail.saturating_sub(hint_w) } else { avail };
        c.draw_text_ellipsis(
            tx as i32,
            ty,
            title_max as i32,
            if selected { FontId::SansSemiBold } else { FontId::SansRegular },
            t.body_px,
            title,
            t.fg_primary,
        );
        if show_hint {
            let hx = ctrl_r.x.saturating_sub(hint_w);
            c.draw_text(
                hx as i32,
                ty + 1,
                FontId::MonoRegular,
                t.caption_px,
                key_hint,
                t.fg_muted,
            );
        }
    }

    let sb_track = Rect::new(
        list_r.x + list_r.w.saturating_sub(10),
        rows_area.y,
        6,
        rows_area.h,
    );
    scrollbar_v(
        &mut c,
        sb_track,
        ROW_COUNT,
        vis_rows,
        view.list_scroll,
        t.bg_card_alt,
        t.border_strong,
    );

    let hov_id = ui.hovered_id();
    ui.finish();
    sidebar_tooltip_overlay(&mut c, t, sb_r, &SETTINGS_CATS, 30, hov_id);

    // Footer bar
    c.fill_rect(foot_r, t.bg_sidebar);
    c.hline(foot_r.x, foot_r.y, foot_r.w, t.border_subtle, 220);
    let cm = font_metrics(FontId::SansRegular, t.caption_px);
    let fy = foot_r.y as i32 + ((foot_r.h as i32 - (cm.ascent + cm.descent)) / 2);
    let hint = b"[Up/Down] Sel  [1-8,0/9] Edit  [V] Icons  [H] Help  [Q] Close";
    let hw = measure_text(FontId::SansRegular, t.caption_px, hint).max(0) as usize;
    let show_msg = if view.status_msg.is_empty() {
        b"Live DRY Glass 2.1 Settings" as &[u8]
    } else {
        view.status_msg
    };
    let st_max = w.saturating_sub(t.sp_md * 2 + hw + 12);
    if st_max >= 80 {
        c.draw_text_ellipsis(
            t.sp_md as i32,
            fy,
            st_max as i32,
            FontId::SansRegular,
            t.caption_px,
            show_msg,
            t.fg_secondary,
        );
        c.draw_text(
            (w.saturating_sub(t.sp_md + hw)) as i32,
            fy,
            FontId::SansRegular,
            t.caption_px,
            hint,
            t.fg_muted,
        );
    } else {
        c.draw_text_ellipsis(
            t.sp_md as i32,
            fy,
            w.saturating_sub(t.sp_md * 2) as i32,
            FontId::SansRegular,
            t.caption_px,
            show_msg,
            t.fg_secondary,
        );
    }

    act
}

pub fn render_help_dialog(win: &mut PixelAppWindow, step: PointerStep) -> bool {
    let t = win.theme();
    let w = win.w as usize;
    let h = win.h as usize;

    let mut c = win.canvas();
    c.clear(t.bg_app);

    let outer = inset_rect(Rect::new(0, 0, w, h), t.sp_sm, t.sp_sm);
    let inner = card(&mut c, t, outer, Some(b"Settings Shortcuts"), Some(b"DRY Glass 2.0"));
    let (body_r, btn_area) = split_bot(inner, (t.ctrl_h + 4).clamp(28, 34), 6);

    let rows: [(&[u8], &[u8]); 4] = [
        (b"1-8, 0/9", b"Adjust row / Cycle 100-150% or Toggle 200% DPI"),
        (b"T  /  B", b"Daylight Theme / Low-Overhead Blur"),
        (b"C / L / V", b"Large Cursor / Lock Vault / Visual Icons Gallery"),
        (b"H  /  G  /  Q", b"Help Dialog / GPU Fault / Quit"),
    ];
    let step_h = (body_r.h / rows.len()).max(20);
    let dx = body_r.x + 88;
    for (i, (k, desc)) in rows.iter().enumerate() {
        let ry = body_r.y + i * step_h;
        c.draw_text(body_r.x as i32, ry as i32, FontId::MonoRegular, t.caption_px, k, t.accent_fg);
        let max_w = (body_r.x + body_r.w).saturating_sub(dx) as i32;
        c.draw_text_ellipsis(dx as i32, ry as i32, max_w, FontId::SansRegular, t.caption_px, desc, t.fg_secondary);
    }

    let mut ui = UiCtx::begin(&mut win.persist, t, step.clicked, step.released, step.scroll_dy, None);
    let close_w = 112usize.min(btn_area.w);
    let close_r = Rect::new(
        btn_area.x + (btn_area.w.saturating_sub(close_w)) / 2,
        btn_area.y + 2,
        close_w,
        btn_area.h.saturating_sub(2),
    );
    let opts = ButtonOpts {
        label: b"Close (Esc)",
        icon: Some(IconKind::Close),
        variant: ButtonVariant::Primary,
        disabled: false,
        selected: false,
    };
    let close_clicked = button(&mut c, &mut ui, 300, close_r, opts);
    ui.finish();
    close_clicked
}
