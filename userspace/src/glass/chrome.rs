// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! DRY Glass Compositor Chrome — Wallpaper, Frosted Topbar, Tabs & AA Windows (`DRY Glass 2.0`).

use crate::compositor::{pack_rgb, write_u32_ascii, DesktopCompositor};
use crate::gfx::{Rect, BOX_HORIZ};
use crate::glass::blur::{backdrop_frosted_rect, blend_soft_rrect_shadow, fill_rounded_raw, in_rounded_rect};
pub use crate::glass::dock_bar::{expand_blur_damage, paint_dock_and_snap};
use crate::glass::overlays::{
    app_from_title_kind, topbar_brand_pill_rect, topbar_notif_pill_rect, topbar_qs_pill_rect,
};
use crate::glass::theme::{GlassTokens, ThemeMode};
use crate::glass::typography::draw_str_raw;
use crate::glass::GLASS_CORNER_R;
use crate::shell::LineBuilder;
use crate::surface::PIXEL_SURF_HEADER_BYTES;
use crate::ttf::{measure_text, FontId, PixelFormat};
use crate::ui::canvas::{rrect_cov, Canvas};
use crate::ui::icons::{draw_vector_icon, IconKind};
use crate::ui::theme::UiTheme;
use crate::wm::{
    LayoutMode, WindowManager, WindowSlot, MAX_WINDOWS, SHADOW_OFF, TITLE_KIND_CALC,
    TITLE_KIND_DIALOG, TITLE_KIND_EDITOR, TITLE_KIND_FILES, TITLE_KIND_SETTINGS, TITLE_KIND_SYSMON,
    TITLE_KIND_TERMINAL, TOPBAR_H, WIN_BORDER, WIN_TITLE_H,
};
use crate::wm_layout::{TABSTRIP_H, TABSTRIP_Y};

pub fn paint_wallpaper(
    comp: &mut DesktopCompositor,
    p3_mode: bool,
    _tokens: &GlassTokens,
    clip: Rect,
) {
    comp.paint_wallpaper_clipped(p3_mode, clip);
}

pub fn paint_topbar_and_tabs(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    tokens: &GlassTokens,
    clip: Rect,
) {
    let tb_clip = clip.intersect(Rect::new(0, 0, comp.w, TOPBAR_H)).clamp_to(comp.w, comp.h);
    if !tb_clip.is_empty() {
        let blur = wm.p3_mode && !wm.low_overhead_glass && tokens.mode != ThemeMode::HighContrast;
        unsafe {
            backdrop_frosted_rect(
                comp.back_ptr, comp.w, comp.h, &comp.fb_info,
                Rect::new(0, 0, comp.w, TOPBAR_H), tb_clip,
                tokens.panel_tint_rgb, tokens.panel_alpha, blur, 0,
                tokens.glass_rim, tokens.glass_rim_soft,
            );
        }
        if wm.p3_mode {
            paint_p3_topbar_items(comp, wm, tokens, tb_clip);
        } else {
            let brand: &[u8] = if wm.desktop_mode {
                b"DRY OS // DESKTOP PREVIEW 2A-2"
            } else {
                b"DRY OS // P1 INTERACTIVE DESKTOP"
            };
            unsafe {
                draw_str_raw(comp.back_ptr, comp.w, comp.h, 14, 6, brand, comp.pal.fg_primary, None, tb_clip);
            }
            if wm.desktop_mode && comp.w >= 960 {
                paint_legacy_mode_pill(comp, wm, tokens, tb_clip);
            }
            if comp.w >= 960 {
                paint_legacy_right_status(comp, wm, tb_clip);
            }
        }
    }
    if wm.layout == LayoutMode::Tabbed {
        paint_tabstrip(comp, wm, tokens, clip);
    }
}

fn paint_p3_topbar_items(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    tokens: &GlassTokens,
    tb_clip: Rect,
) {
    let fmt = PixelFormat::from_fb(&comp.fb_info);
    let ui = UiTheme::new(tokens.mode, wm.dpi_pct());
    let mut c = unsafe { Canvas::from_raw(comp.back_ptr, comp.w, comp.h, comp.fb_stride, fmt, tb_clip) };

    c.fill_rect(Rect::new(0, TOPBAR_H - 1, comp.w, 1), ui.border_subtle);

    let br = topbar_brand_pill_rect(wm);
    let br_active = wm.glass.launcher_open;
    let br_bg = if br_active { ui.accent } else { ui.bg_card_alt };
    let br_fg = if br_active { ui.fg_on_accent } else { ui.fg_primary };
    c.fill_bordered_rrect_aa(br, 6, br_bg, if br_active { ui.accent } else { ui.border_subtle });
    draw_vector_icon(&mut c, Rect::new(br.x + 6, br.y + 4, 12, 12), IconKind::Search, br_fg, ui.accent);
    let blbl: &[u8] = if wm.screen_w < 1200 { b"Glass" } else { b"DRY Glass" };
    c.draw_text((br.x + 22) as i32, (br.y + 3) as i32, FontId::SansSemiBold, 11, blbl, br_fg);

    crate::glass::window_strip::paint_adaptive_window_strip(comp, wm, tokens, tb_clip);

    let mode_r = wm.topbar_mode_pill_rect();
    c.fill_bordered_rrect_aa(mode_r, 6, ui.bg_card_alt, ui.border_subtle);
    let (m_icon, m_lbl): (IconKind, &[u8]) = match wm.layout {
        LayoutMode::Floating => (IconKind::LayoutFloat, b"FLOAT"),
        LayoutMode::SideBySideTiled => (IconKind::LayoutTiled, b"TILED"),
        LayoutMode::Stacked => (IconKind::LayoutTabbed, b"STACK"),
        LayoutMode::Tabbed => (IconKind::LayoutTabbed, b"TABBED"),
    };
    draw_vector_icon(&mut c, Rect::new(mode_r.x + 8, mode_r.y + 4, 12, 12), m_icon, ui.accent, ui.accent);
    c.draw_text((mode_r.x + 25) as i32, (mode_r.y + 3) as i32, FontId::SansSemiBold, 11, m_lbl, ui.fg_primary);

    let notif_r = topbar_notif_pill_rect(wm);
    let qs_r = topbar_qs_pill_rect(wm);
    if comp.w >= 1140 {
        let mut st = LineBuilder::new();
        st.push_bytes(if wm.gpu_ep_cap != 0 { b"GPU W:" } else { b"GOP W:" });
        st.push_u64(wm.active_window_count() as u64);
        let sb = st.as_bytes();
        let tw = measure_text(FontId::MonoSemiBold, 11, sb).max(0) as usize;
        let sx = notif_r.x.saturating_sub(tw + 12);
        if sx > mode_r.x + mode_r.w + 8 {
            let col = if wm.gpu_ep_cap != 0 { ui.ok } else { ui.warn };
            c.fill_circle_aa((sx - 6) as i32, 14, 3, col, 255);
            c.draw_text(sx as i32, 7, FontId::MonoSemiBold, 11, sb, col);
        }
    }

    let n_open = wm.glass.notif_open;
    let nbg = if n_open { ui.accent } else { ui.bg_card_alt };
    let nfg = if n_open { ui.fg_on_accent } else { ui.fg_primary };
    c.fill_bordered_rrect_aa(notif_r, 6, nbg, if n_open { ui.accent } else { ui.border_subtle });
    draw_vector_icon(
        &mut c, Rect::new(notif_r.x + 7, notif_r.y + 4, 12, 12), IconKind::Bell, nfg,
        if n_open { ui.fg_on_accent } else if wm.glass.notif_unread > 0 { ui.accent } else { nfg },
    );
    let mut nlb = LineBuilder::new();
    nlb.push_u64(wm.glass.notif_unread as u64);
    c.draw_text((notif_r.x + 25) as i32, (notif_r.y + 3) as i32, FontId::SansSemiBold, 11, nlb.as_bytes(), nfg);

    let q_open = wm.glass.qs_open;
    let qbg = if q_open { ui.accent } else { ui.bg_card_alt };
    let qfg = if q_open { ui.fg_on_accent } else { ui.fg_primary };
    let (q_icon, qlbl): (IconKind, &[u8]) = match tokens.mode {
        ThemeMode::FrostedDaylight => (IconKind::Sun, b"Light"),
        ThemeMode::HighContrast => (IconKind::Shield, b"HC"),
        ThemeMode::ObsidianDark => (IconKind::Moon, b"Dark"),
    };
    c.fill_bordered_rrect_aa(qs_r, 6, qbg, if q_open { ui.accent } else { ui.border_subtle });
    draw_vector_icon(
        &mut c, Rect::new(qs_r.x + 8, qs_r.y + 4, 12, 12), q_icon,
        if q_open { ui.fg_on_accent } else { ui.accent }, ui.accent,
    );
    c.draw_text((qs_r.x + 24) as i32, (qs_r.y + 3) as i32, FontId::SansSemiBold, 11, qlbl, qfg);
    c.draw_text(qs_r.x.saturating_add(qs_r.w.saturating_sub(24)) as i32, (qs_r.y + 3) as i32, FontId::SansRegular, 10, b"QS", qfg);
}

fn paint_legacy_mode_pill(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    tokens: &GlassTokens,
    tb_clip: Rect,
) {
    let pill = wm.topbar_mode_pill_rect();
    let mode_lbl: &[u8] = match wm.layout {
        LayoutMode::Floating => b"MODE: FLOAT",
        LayoutMode::SideBySideTiled => b"MODE: TILED",
        LayoutMode::Stacked => b"MODE: STACK",
        LayoutMode::Tabbed => b"MODE: TABBED",
    };
    unsafe {
        fill_rounded_raw(comp.back_ptr, comp.w, comp.h, pill, tb_clip, 5, tokens.pill_bg);
        draw_str_raw(comp.back_ptr, comp.w, comp.h, pill.x + 8, 6, mode_lbl, tokens.accent, None, tb_clip);
    }
}

fn paint_legacy_right_status(comp: &mut DesktopCompositor, wm: &WindowManager, tb_clip: Rect) {
    let mut status = [b' '; 44];
    let mut sp = 0usize;
    let active_lbl: &[u8] = match wm.focused_slot().map(|s| wm.windows[s].title_kind) {
        Some(TITLE_KIND_SYSMON) => b"FOCUS:SYSMON  WINS:",
        Some(TITLE_KIND_TERMINAL) => b"FOCUS:TERMINAL WINS:",
        Some(TITLE_KIND_FILES) => b"FOCUS:FILES   WINS:",
        Some(TITLE_KIND_SETTINGS) => b"FOCUS:SETTINGS WINS:",
        Some(TITLE_KIND_EDITOR) => b"FOCUS:EDITOR  WINS:",
        Some(TITLE_KIND_CALC) => b"FOCUS:CALC    WINS:",
        Some(_) => b"FOCUS:APP     WINS:",
        None => b"FOCUS:NONE    WINS:",
    };
    for &b in active_lbl {
        status[sp] = b;
        sp += 1;
    }
    sp += write_u32_ascii(&mut status[sp..], wm.active_window_count() as u32);
    let suffix: &[u8] = if wm.desktop_mode { b" | ALT+TAB / ALT+T" } else { b" | ALT+ARROWS: MOVE" };
    for &b in suffix {
        if sp < status.len() {
            status[sp] = b;
            sp += 1;
        }
    }
    let rx = comp.w.saturating_sub(sp * 8 + 14);
    unsafe {
        draw_str_raw(comp.back_ptr, comp.w, comp.h, rx, 6, &status[..sp], comp.pal.fg_ok, None, tb_clip);
    }
}

fn paint_tabstrip(comp: &mut DesktopCompositor, wm: &WindowManager, tokens: &GlassTokens, clip: Rect) {
    let strip_clip = clip.intersect(Rect::new(0, TABSTRIP_Y, comp.w, TABSTRIP_H)).clamp_to(comp.w, comp.h);
    if strip_clip.is_empty() {
        return;
    }
    let mut normal_slots = [0usize; MAX_WINDOWS];
    let n = wm.collect_normal_slots(&mut normal_slots);

    if wm.p3_mode {
        let fmt = PixelFormat::from_fb(&comp.fb_info);
        let ui = UiTheme::new(tokens.mode, wm.dpi_pct());
        let mut c = unsafe { Canvas::from_raw(comp.back_ptr, comp.w, comp.h, comp.fb_stride, fmt, strip_clip) };
        for (tab_i, &slot_idx) in normal_slots.iter().take(n).enumerate() {
            let tr = wm.tab_pill_rect(tab_i);
            let w = &wm.windows[slot_idx];
            let (bg, bd, fg) = if w.focused {
                (ui.bg_card_alt, ui.accent, ui.fg_primary)
            } else {
                (ui.bg_card, ui.border_subtle, ui.fg_muted)
            };
            c.fill_bordered_rrect_aa(tr, 6, bg, bd);
            if w.focused {
                c.fill_rrect_aa(Rect::new(tr.x + 8, tr.y + tr.h - 2, tr.w.saturating_sub(16), 2), 1, ui.accent, 255);
            }
            if let Some(app) = app_from_title_kind(w.title_kind) {
                draw_vector_icon(&mut c, Rect::new(tr.x + 8, tr.y + 4, 14, 14), IconKind::from_app(app), fg, ui.accent);
            }
            let lbl: &[u8] = match w.title_kind {
                TITLE_KIND_TERMINAL => b"Terminal",
                TITLE_KIND_SYSMON => b"Life Station",
                TITLE_KIND_FILES => b"Files",
                TITLE_KIND_SETTINGS => b"Settings",
                TITLE_KIND_EDITOR => b"Notes",
                TITLE_KIND_CALC => b"Calculator",
                _ => b"Window",
            };
            c.draw_text((tr.x + 26) as i32, (tr.y + 4) as i32, FontId::SansSemiBold, 11, lbl, fg);
        }
        return;
    }

    for (tab_i, &slot_idx) in normal_slots.iter().take(n).enumerate() {
        let tr = wm.tab_pill_rect(tab_i);
        let w = &wm.windows[slot_idx];
        let (tbg, tfg, bcol) = if w.focused {
            (tokens.pill_active, tokens.fg_primary, tokens.border_focus)
        } else {
            (tokens.pill_bg, tokens.fg_muted, tokens.border_idle)
        };
        unsafe { fill_rounded_raw(comp.back_ptr, comp.w, comp.h, tr, strip_clip, 5, tbg); }
        comp.fill_back_clipped(Rect::new(tr.x + 4, tr.y + tr.h - 2, tr.w.saturating_sub(8), 2), strip_clip, bcol);
        let lbl: &[u8] = match w.title_kind {
            TITLE_KIND_TERMINAL => b"TAB: Terminal",
            TITLE_KIND_SYSMON => b"TAB: LifeStn",
            TITLE_KIND_FILES => b"TAB: Files",
            TITLE_KIND_SETTINGS => b"TAB: Settings",
            TITLE_KIND_EDITOR => b"TAB: Notes",
            TITLE_KIND_CALC => b"TAB: Calc",
            _ => b"TAB: Window",
        };
        unsafe { draw_str_raw(comp.back_ptr, comp.w, comp.h, tr.x + 10, tr.y + 4, lbl, tfg, None, strip_clip); }
    }
}

pub fn paint_window_glass(
    comp: &mut DesktopCompositor,
    win: &WindowSlot,
    p3_mode: bool,
    dpi_pct: u16,
    tokens: &GlassTokens,
    clip: Rect,
) {
    if !win.allocated || !win.visible {
        return;
    }
    let wclip = win.outer_rect().intersect(clip).clamp_to(comp.w, comp.h);
    if wclip.is_empty() {
        return;
    }
    if p3_mode {
        paint_p3_window(comp, win, dpi_pct, tokens, wclip);
    } else {
        paint_legacy_window(comp, win, tokens, wclip);
    }
}

fn paint_p3_window(
    comp: &mut DesktopCompositor,
    win: &WindowSlot,
    dpi_pct: u16,
    tokens: &GlassTokens,
    wclip: Rect,
) {
    let corner_r = if tokens.mode == ThemeMode::HighContrast { 0 } else { GLASS_CORNER_R };
    if tokens.mode != ThemeMode::HighContrast {
        unsafe {
            blend_soft_rrect_shadow(
                comp.back_ptr, comp.w, comp.h, &comp.fb_info, win.rect, wclip,
                corner_r, tokens.shadow_rgb, if win.focused { 84 } else { 44 }, SHADOW_OFF,
            );
        }
    }

    let fmt = PixelFormat::from_fb(&comp.fb_info);
    let ui = UiTheme::new(tokens.mode, dpi_pct);
    let (wx, wy, ww, wh) = (win.rect.x, win.rect.y, win.rect.w, win.rect.h);
    let mut c = unsafe { Canvas::from_raw(comp.back_ptr, comp.w, comp.h, comp.fb_stride, fmt, wclip) };

    c.fill_rrect_aa(win.rect, corner_r, ui.bg_app, 255);
    let tb_rgb = if win.focused { ui.bg_sidebar } else { ui.bg_card };
    let tb_clip_y_end = (wy + WIN_BORDER + WIN_TITLE_H).min(wy + wh);
    let tb_band = Rect::new(wx, wy, ww, tb_clip_y_end - wy).intersect(wclip);
    for py in tb_band.y..(tb_band.y + tb_band.h) {
        let row_off = py * comp.fb_stride;
        for px in tb_band.x..(tb_band.x + tb_band.w) {
            let cov = rrect_cov(win.rect, corner_r, px, py);
            if cov != 0 {
                unsafe {
                    let p = comp.back_ptr.add(row_off + px);
                    *p = fmt.blend_rgb24(*p, tb_rgb, cov);
                }
            }
        }
    }
    c.fill_rect(Rect::new(wx + 1, wy + WIN_BORDER + WIN_TITLE_H - 1, ww.saturating_sub(2), 1), ui.border_subtle);

    let border_col = if win.focused {
        if tokens.mode == ThemeMode::HighContrast { ui.accent } else { ui.border_strong }
    } else {
        ui.border_subtle
    };
    c.stroke_rrect_aa(win.rect, corner_r, border_col, 240);
    if win.focused && tokens.mode != ThemeMode::HighContrast {
        c.hline(wx + corner_r, wy + 1, ww.saturating_sub(corner_r * 2), ui.border_strong, 140);
    }

    let title_fg = if win.focused { ui.fg_primary } else { ui.fg_muted };
    let mut title_x = wx + WIN_BORDER + 10;
    if let Some(app) = WindowManager::slot_app_id(win) {
        draw_vector_icon(&mut c, Rect::new(title_x, wy + WIN_BORDER + 6, 16, 16), IconKind::from_app(app), title_fg, ui.accent);
        title_x += 22;
    }
    let min_r = win.minimize_btn_rect();
    let title_str = crate::glass::overview::slot_effective_title(win);
    let title_max = min_r.x.saturating_sub(title_x + 10);
    c.draw_text_ellipsis(
        title_x as i32,
        (wy + WIN_BORDER + 6) as i32,
        title_max as i32,
        FontId::SansSemiBold,
        13,
        title_str,
        title_fg,
    );

    c.fill_bordered_rrect_aa(min_r, 5, ui.bg_card_alt, ui.border_subtle);
    c.fill_rect(Rect::new(min_r.x + 6, min_r.y + min_r.h / 2, min_r.w.saturating_sub(12), 2), title_fg);

    let close_r = win.close_btn_rect();
    let close_bg = if win.focused { 0xC83E44 } else { ui.bg_card_alt };
    let close_fg = if win.focused { 0xFFFFFF } else { ui.fg_muted };
    c.fill_rrect_aa(close_r, 5, close_bg, 255);
    let ci = Rect::new(close_r.x + (close_r.w.saturating_sub(10)) / 2, close_r.y + (close_r.h.saturating_sub(10)) / 2, 10, 10);
    draw_vector_icon(&mut c, ci, IconKind::Close, close_fg, ui.accent);

    if !win.is_pixel {
        let mut badge = [b' '; 20];
        let mut p = 0usize;
        let prefix: &[u8] = if win.title_kind == TITLE_KIND_TERMINAL { b"CMD:" } else { b"UP:" };
        for &b in prefix {
            badge[p] = b;
            p += 1;
        }
        let val = if win.title_kind == TITLE_KIND_TERMINAL { win.snapshot.cmd_count } else { win.snapshot.uptime_sec_snap };
        p += write_u32_ascii(&mut badge[p..], val);
        if win.title_kind != TITLE_KIND_TERMINAL && p < badge.len() {
            badge[p] = b's';
            p += 1;
        }
        let bs = &badge[..p];
        let bw = (measure_text(FontId::MonoRegular, 10, bs).max(0) as usize) + 12;
        let bx = min_r.x.saturating_sub(bw + 8);
        if bx > title_x + 120 {
            let br = Rect::new(bx, wy + WIN_BORDER + 5, bw, 18);
            c.fill_rrect_aa(br, 5, ui.bg_card_alt, 220);
            c.draw_text_centered(br, FontId::MonoRegular, 10, bs, ui.ok);
        }
    }

    paint_window_client_body(comp, win, wclip, fmt.pack_rgb24(ui.bg_app));
}

fn paint_legacy_window(
    comp: &mut DesktopCompositor,
    win: &WindowSlot,
    tokens: &GlassTokens,
    wclip: Rect,
) {
    let (wx, wy, ww, wh) = (win.rect.x, win.rect.y, win.rect.w, win.rect.h);
    comp.fill_back_clipped(Rect::new(wx + ww, wy + SHADOW_OFF, SHADOW_OFF, wh), wclip, comp.pal.bg_shadow);
    comp.fill_back_clipped(Rect::new(wx + SHADOW_OFF, wy + wh, ww.saturating_sub(SHADOW_OFF), SHADOW_OFF), wclip, comp.pal.bg_shadow);

    let bcol = if win.focused { tokens.border_focus } else { tokens.border_idle };
    let win_inter = win.rect.intersect(wclip);
    for py in win_inter.y..(win_inter.y + win_inter.h) {
        let on_top = py < wy + WIN_BORDER;
        let on_bot = py >= wy + wh - WIN_BORDER;
        let row_off = py * comp.w;
        for px in win_inter.x..(win_inter.x + win_inter.w) {
            if in_rounded_rect(win.rect, 0, px, py) && (on_top || on_bot || px < wx + WIN_BORDER || px >= wx + ww - WIN_BORDER) {
                unsafe { *comp.back_ptr.add(row_off + px) = bcol };
            }
        }
    }

    let tb_rect = win.titlebar_rect();
    if !tb_rect.intersect(wclip).is_empty() {
        let tb_rgb = if win.focused { tokens.title_focus_rgb } else { tokens.title_idle_rgb };
        comp.fill_back_clipped(tb_rect, wclip, pack_rgb(&comp.fb_info, tb_rgb.0, tb_rgb.1, tb_rgb.2));
        let title_fg = if win.focused { tokens.fg_primary } else { tokens.fg_muted };
        let title_str: &[u8] = match win.title_kind {
            TITLE_KIND_SYSMON => b"SYSTEM MONITOR // dry-sysmon [0x0103]",
            TITLE_KIND_TERMINAL => b"DRY TERMINAL // dry-shell [0x0102]",
            TITLE_KIND_FILES => b"FILES // dry-files [0x0104]",
            TITLE_KIND_SETTINGS => b"SETTINGS // dry-settings [0x0105]",
            TITLE_KIND_EDITOR => b"DRY NOTES // dry-editor [0x0106]",
            TITLE_KIND_CALC => b"CALCULATOR // dry-calc [0x0107]",
            TITLE_KIND_DIALOG => b"QUICK HELP // Dialog [AUTO-FLOAT]",
            _ => b"DRY APP // [CPL=3]",
        };
        unsafe { draw_str_raw(comp.back_ptr, comp.w, comp.h, wx + WIN_BORDER + 10, wy + WIN_BORDER + 6, title_str, title_fg, None, wclip); }
        let close_r = win.close_btn_rect();
        unsafe {
            fill_rounded_raw(comp.back_ptr, comp.w, comp.h, close_r, wclip, 0, tokens.close_bg);
            draw_str_raw(comp.back_ptr, comp.w, comp.h, close_r.x + 7, close_r.y + 2, b"x", tokens.close_fg, None, wclip);
        }
    }
    paint_window_client_body(comp, win, wclip, comp.pal.bg_win);
}

fn paint_window_client_body(comp: &mut DesktopCompositor, win: &WindowSlot, wclip: Rect, body_bg: u32) {
    let (wx, wy, ww, wh) = (win.rect.x, win.rect.y, win.rect.w, win.rect.h);
    let body_rect = Rect::new(
        wx + WIN_BORDER, wy + WIN_BORDER + WIN_TITLE_H,
        ww.saturating_sub(WIN_BORDER * 2), wh.saturating_sub(WIN_BORDER * 2 + WIN_TITLE_H),
    );
    let body_clip = body_rect.intersect(wclip);
    if body_clip.is_empty() {
        return;
    }
    let (gx, gy) = win.grid_origin();
    let (content_w, content_h) = if win.is_pixel {
        (win.pixel_w as usize, win.pixel_h as usize)
    } else {
        ((win.cols as usize) * 8, (win.rows as usize) * 16)
    };
    let valid = if win.is_pixel { win.snap_vaddr != 0 } else { win.snapshot.valid };
    if !valid {
        comp.fill_back_clipped(body_rect, body_clip, body_bg);
        return;
    }

    comp.fill_back_clipped(Rect::new(body_rect.x, body_rect.y, body_rect.w, gy.saturating_sub(body_rect.y)), body_clip, body_bg);
    let c_bot = gy + content_h;
    if body_rect.y + body_rect.h > c_bot {
        comp.fill_back_clipped(Rect::new(body_rect.x, c_bot, body_rect.w, body_rect.y + body_rect.h - c_bot), body_clip, body_bg);
    }
    comp.fill_back_clipped(Rect::new(body_rect.x, gy, gx.saturating_sub(body_rect.x), content_h), body_clip, body_bg);
    let c_right = gx + content_w;
    if body_rect.x + body_rect.w > c_right {
        comp.fill_back_clipped(Rect::new(c_right, gy, body_rect.x + body_rect.w - c_right, content_h), body_clip, body_bg);
    }

    let cclip = Rect::new(gx, gy, content_w, content_h).intersect(body_clip);
    if cclip.is_empty() {
        return;
    }
    if win.is_pixel {
        let src_base = (win.snap_vaddr + PIXEL_SURF_HEADER_BYTES as u64) as *const u32;
        unsafe { comp.blit_pixel_surface_clipped(src_base, content_w, gx, gy, cclip) };
        return;
    }

    let (cols, rows) = (win.cols as usize, win.rows as usize);
    let r_start = (cclip.y - gy) / 16;
    let r_end = ((cclip.y + cclip.h - 1 - gy) / 16).min(rows - 1);
    let c_start = (cclip.x - gx) / 8;
    let c_end = ((cclip.x + cclip.w - 1 - gx) / 8).min(cols - 1);
    let cur_vis = win.snapshot.cursor_visible && win.focused;
    let cur_r = win.snapshot.cursor_row as usize;
    let cur_c = win.snapshot.cursor_col as usize;

    for r in r_start..=r_end {
        let py = gy + r * 16;
        let row_base = r * cols;
        for c in c_start..=c_end {
            let cell = win.snapshot.cells[row_base + c];
            let ch = if (0x20..=0x7E).contains(&cell.ch) || cell.ch == BOX_HORIZ { cell.ch } else { b' ' };
            let (fg, bg) = if cur_vis && r == cur_r && c == cur_c {
                (comp.pal.bg_win, comp.pal.fg_accent)
            } else {
                (comp.pal.style_fg(cell.style), comp.pal.bg_win)
            };
            comp.draw_glyph_clipped(gx + c * 8, py, ch, fg, bg, cclip);
        }
    }
}
