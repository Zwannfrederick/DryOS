// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Shared DRY Glass 2.0 UI view & Help dialog for `dry-settings` (`Settings`).

use crate::apps::{PixelAppWindow, PointerStep};
use crate::gfx::Rect;
use crate::sup_services::SupervisorPrefs;
use crate::ttf::{font_metrics, measure_text, FontId};
use crate::ui::containers::{
    card, list_row, sidebar_nav, sidebar_preferred_w, sidebar_tooltip_overlay, status_badge,
};
use crate::ui::icons::{draw_vector_icon, IconKind};
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
}

pub struct SettingsView<'a> {
    pub prefs: SupervisorPrefs,
    pub sel_row: usize,
    pub list_scroll: usize,
    pub dlg_open: bool,
    pub status_msg: &'a [u8],
}

fn row_spec(idx: usize, light_mode: bool) -> (IconKind, &'static [u8], &'static [u8]) {
    match idx {
        0 => (IconKind::LayoutTiled, b"Window Layout", b"[1]"),
        1 => (IconKind::Sliders, b"Keyboard", b"[2]"),
        2 => (IconKind::Sliders, b"Pointer Speed", b"[3]"),
        3 => (IconKind::Shield, b"High Contrast", b"[4]"),
        4 => (IconKind::Search, b"Large Cursor", b"[5]"),
        5 => (IconKind::Power, b"Idle Blank", b"[6]"),
        6 => (IconKind::Lock, b"Session Lock", b"[7/L]"),
        7 => (
            if light_mode { IconKind::Sun } else { IconKind::Moon },
            b"Daylight Theme",
            b"[8/T]",
        ),
        8 => (IconKind::Sliders, b"Reduced Blur", b"[9/B]"),
        _ => (IconKind::LayoutFloat, b"Display Scale", b"[0/S]"),
    }
}

const SETTINGS_CATS: [(&[u8], IconKind); 4] = [
    (b"Workspace", IconKind::LayoutTiled),
    (b"Input", IconKind::Sliders),
    (b"Appearance", IconKind::Sun),
    (b"Security", IconKind::Shield),
];

pub fn settings_vis_rows(t: UiTheme, w: usize, h: usize) -> usize {
    let top_h = (t.ctrl_h + 12).clamp(40, 48);
    let foot_h = 30usize;
    let mid_h = h.saturating_sub(top_h + foot_h + t.sp_sm * 2);
    let sb_w = sidebar_preferred_w(t, w, &SETTINGS_CATS);
    let hdr_h = if sb_w < 104 { 28usize } else { 0usize };
    let m_body = font_metrics(FontId::SansRegular, t.body_px);
    let row_h = (m_body.line_height as usize + 18).clamp(36, 48);
    (mid_h.saturating_sub(14 + hdr_h) / row_h).max(1)
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
    let t = win.theme();
    let w = win.w as usize;
    let h = win.h as usize;

    let top_h = (t.ctrl_h + 12).clamp(40, 48);
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
    let help_w = (measure_text(FontId::SansSemiBold, t.caption_px, help_lbl).max(48) as usize) + 46;
    let gpu_w = (measure_text(FontId::SansSemiBold, t.caption_px, gpu_lbl).max(52) as usize) + 46;
    let help_x = w.saturating_sub(t.sp_md + help_w);
    let gpu_x = help_x.saturating_sub(8 + gpu_w);

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
    let st_w = measure_text(FontId::SansSemiBold, t.caption_px, st_lbl).max(0) as usize + 22;
    let badge_y = top_r.y + (top_h.saturating_sub(20)) / 2;
    if t.sp_md + st_w + 8 <= gpu_x {
        status_badge(&mut c, t, t.sp_md, badge_y, st_lbl, st_kind);
    }
    let dpi_lbl: &[u8] = match (view.prefs.theme_flags >> 4) & 3 {
        1 => b"125% DPI",
        2 => b"150% DPI",
        _ => b"100% DPI",
    };
    let dpi_w = measure_text(FontId::SansSemiBold, t.caption_px, dpi_lbl).max(0) as usize + 22;
    let dpi_x = t.sp_md + st_w + 8;
    if dpi_x + dpi_w + 8 <= gpu_x {
        status_badge(&mut c, t, dpi_x, badge_y, dpi_lbl, StatusKind::Accent);
    }

    // Left category sidebar
    let mut cat = category_for_row(view.sel_row).min(SETTINGS_CATS.len() - 1);
    if sidebar_nav(&mut c, &mut ui, 30, sb_r, &SETTINGS_CATS, &mut cat) {
        act = SettingsAction::SelectCategory(cat);
    }

    // Right settings list card
    c.fill_bordered_rrect_aa(list_r, t.r_md, t.bg_card, t.border_subtle);
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
    let row_h = (m_body.line_height as usize + 18).clamp(36, 48);
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

        let ctrl_h = rr.h.saturating_sub(8).clamp(24, 30);
        let ctrl_y = rr.y + (rr.h.saturating_sub(ctrl_h)) / 2;
        let dpi_s = t.dpi_pct as usize;
        let mut ctrl_acted = false;

        let ctrl_w: usize = match idx {
            1 | 2 => (124 * dpi_s / 100).min(rr.w * 42 / 100),
            3 | 4 | 7 | 8 => 46,
            9 => (174 * dpi_s / 100).min(rr.w * 52 / 100),
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
                let mut dpi_idx = ((view.prefs.theme_flags >> 4) & 3).min(2) as usize;
                if segmented_control(
                    &mut c,
                    &mut ui,
                    220,
                    ctrl_r,
                    &[b"100%", b"125%", b"150%"],
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
    let hint = b"[Up/Down] Select  [1-9,0] Edit  [H] Help  [Q] Close";
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
        (b"1-9, 0/S", b"Adjust row / Cycle Display Scale"),
        (b"T  /  B", b"Daylight Theme / Low-Overhead Blur"),
        (b"C  /  L", b"Large Cursor / Lock DRY Vault"),
        (b"H  /  G  /  Q", b"Help Dialog / GPU Fault / Quit"),
    ];
    let step_h = (body_r.h / rows.len()).max(20);
    for (i, (k, desc)) in rows.iter().enumerate() {
        let ry = body_r.y + i * step_h;
        c.draw_text(
            body_r.x as i32,
            ry as i32,
            FontId::MonoRegular,
            t.caption_px,
            k,
            t.accent_fg,
        );
        let dx = body_r.x + 88;
        c.draw_text_ellipsis(
            dx as i32,
            ry as i32,
            (body_r.x + body_r.w).saturating_sub(dx) as i32,
            FontId::SansRegular,
            t.caption_px,
            desc,
            t.fg_secondary,
        );
    }

    let mut ui = UiCtx::begin(
        &mut win.persist,
        t,
        step.clicked,
        step.released,
        step.scroll_dy,
        None,
    );
    let close_w = 112usize.min(btn_area.w);
    let close_r = Rect::new(
        btn_area.x + (btn_area.w.saturating_sub(close_w)) / 2,
        btn_area.y + 2,
        close_w,
        btn_area.h.saturating_sub(2),
    );
    let close_clicked = button(
        &mut c,
        &mut ui,
        300,
        close_r,
        ButtonOpts {
            label: b"Close (Esc)",
            icon: Some(IconKind::Close),
            variant: ButtonVariant::Primary,
            disabled: false,
            selected: false,
        },
    );
    ui.finish();
    close_clicked
}
