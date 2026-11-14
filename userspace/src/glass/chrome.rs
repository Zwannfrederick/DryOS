// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `DRY Glass` Desktop Chrome: Layered Wallpaper, Frosted Top Panel, Soft Shadows,
//! Rounded Glass Windows, Unified Pixel/Cell Client Blitter & Floating Dock.

use crate::action::{AppId, APP_COUNT};
use crate::compositor::{pack_rgb, write_u32_ascii, DesktopCompositor};
use crate::gfx::{DirtyRegion, Rect, BOX_HORIZ};
use crate::glass::blur::{backdrop_frosted_rect, blend_shadow_rect, fill_rounded_raw, in_rounded_rect};
use crate::glass::icons::{draw_icon16, icon_for_app, ICON_SEARCH};
use crate::glass::overlays::{
    app_from_title_kind, dock_item_rect, dock_rect, topbar_brand_pill_rect,
    topbar_notif_pill_rect, topbar_qs_pill_rect,
};
use crate::glass::theme::{blend_rgb, GlassTokens, ThemeMode};
use crate::glass::typography::draw_str_raw;
use crate::glass::GLASS_CORNER_R;
use crate::shell::LineBuilder;
use crate::surface::PIXEL_SURF_HEADER_BYTES;
use crate::wm::{
    LayoutMode, WindowManager, WindowSlot, MAX_WINDOWS, SHADOW_OFF, TITLE_KIND_CALC,
    TITLE_KIND_DIALOG, TITLE_KIND_EDITOR, TITLE_KIND_FILES, TITLE_KIND_SETTINGS, TITLE_KIND_SYSMON,
    TITLE_KIND_TERMINAL, TOPBAR_H, WIN_BORDER, WIN_TITLE_H,
};
use crate::wm_layout::{TABSTRIP_H, TABSTRIP_Y};

#[inline(always)]
fn lerp_u8(a: u8, b: u8, num: usize, den: usize) -> u8 {
    let va = a as usize;
    let vb = b as usize;
    if vb >= va {
        (va + ((vb - va) * num) / den.max(1)) as u8
    } else {
        (va - ((va - vb) * num) / den.max(1)) as u8
    }
}

/// Deterministic coordinate-pure backdrop wallpaper (`y_start..y_end`) so partial dirty clips
/// seamlessly match full-screen repaints without state.
pub fn paint_wallpaper(comp: &mut DesktopCompositor, tokens: &GlassTokens, clip: Rect) {
    let c = clip.clamp_to(comp.w, comp.h);
    if c.is_empty() {
        return;
    }
    let fb_w = comp.w;
    let fb_h = comp.h;
    let info = comp.fb_info;
    let glow_cx = (fb_w * 2 / 3) as isize;
    let glow_cy = (fb_h * 2 / 5) as isize;
    let glow_r2 = ((fb_w / 2) * (fb_w / 2)).max(1);
    let blur_wall = tokens.mode != ThemeMode::HighContrast;

    for py in c.y..(c.y + c.h) {
        let base_r = lerp_u8(tokens.wall_top_rgb.0, tokens.wall_bot_rgb.0, py, fb_h);
        let base_g = lerp_u8(tokens.wall_top_rgb.1, tokens.wall_bot_rgb.1, py, fb_h);
        let base_b = lerp_u8(tokens.wall_top_rgb.2, tokens.wall_bot_rgb.2, py, fb_h);
        let dy = (py as isize - glow_cy).unsigned_abs();
        let dy2 = dy * dy * 2;
        let row_off = py * fb_w;
        let grid_y = (py & 63) == 0;
        let grid_y1 = (py & 63) == 1;

        for px in c.x..(c.x + c.w) {
            let col = if blur_wall {
                let dx = (px as isize - glow_cx).unsigned_abs();
                let d2 = dx * dx + dy2;
                let rgb = if d2 < glow_r2 {
                    let strength = ((glow_r2 - d2) * 110) / glow_r2;
                    blend_rgb((base_r, base_g, base_b), tokens.wall_glow_rgb, strength as u8)
                } else {
                    (base_r, base_g, base_b)
                };
                if ((grid_y && (px & 63) <= 1) || (grid_y1 && (px & 63) == 0)) && py >= TOPBAR_H {
                    tokens.dot_grid
                } else {
                    pack_rgb(&info, rgb.0, rgb.1, rgb.2)
                }
            } else if ((grid_y && (px & 63) <= 1) || (grid_y1 && (px & 63) == 0)) && py >= TOPBAR_H {
                tokens.dot_grid
            } else {
                comp.pal.bg_canvas
            };
            // SAFETY: `(px, py)` clamped to `(comp.w, comp.h)`.
            unsafe { *comp.back_ptr.add(row_off + px) = col };
        }
    }
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
        // SAFETY: `back_ptr` covers `(comp.w, comp.h)`.
        unsafe {
            backdrop_frosted_rect(
                comp.back_ptr,
                comp.w,
                comp.h,
                &comp.fb_info,
                Rect::new(0, 0, comp.w, TOPBAR_H),
                tb_clip,
                tokens.panel_tint_rgb,
                tokens.panel_alpha,
                blur,
                0,
                tokens.glass_rim,
                tokens.glass_rim_soft,
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
                draw_str_raw(
                    comp.back_ptr,
                    comp.w,
                    comp.h,
                    14,
                    6,
                    brand,
                    comp.pal.fg_primary,
                    None,
                    tb_clip,
                );
            }
            if (wm.desktop_mode || wm.p3_mode) && comp.w >= 960 {
                paint_mode_pill(comp, wm, tokens, tb_clip);
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
    let br = topbar_brand_pill_rect(wm);
    let br_bg = if wm.glass.launcher_open {
        tokens.pill_active
    } else {
        tokens.pill_bg
    };
    unsafe {
        fill_rounded_raw(comp.back_ptr, comp.w, comp.h, br, tb_clip, 5, br_bg);
        draw_icon16(
            comp.back_ptr,
            comp.w,
            comp.h,
            br.x + 5,
            br.y + 2,
            &ICON_SEARCH,
            tokens.fg_primary,
            tokens.accent,
            tb_clip,
        );
        let blbl: &[u8] = if wm.screen_w < 1200 { b"Glass" } else { b"DRY Glass" };
        draw_str_raw(
            comp.back_ptr,
            comp.w,
            comp.h,
            br.x + 23,
            br.y + 2,
            blbl,
            tokens.fg_primary,
            None,
            tb_clip,
        );
    }

    let focused_badge = wm.focused_slot().map_or(0, |s| wm.windows[s].owner_badge);
    for idx in 0..APP_COUNT {
        let app = AppId::ALL[idx];
        let pr = wm.topbar_launcher_pill_rect(idx);
        let is_focused = focused_badge == app.win_badge();
        let is_running = (wm.running_apps_mask & (1u8 << idx)) != 0
            || wm
                .windows
                .iter()
                .any(|w| w.allocated && w.owner_badge == app.win_badge());
        let (pbg, pfg) = if is_focused {
            (tokens.pill_active, tokens.fg_primary)
        } else if is_running {
            (tokens.pill_bg, tokens.ok)
        } else {
            (tokens.card_bg, tokens.fg_muted)
        };
        unsafe {
            fill_rounded_raw(comp.back_ptr, comp.w, comp.h, pr, tb_clip, 4, pbg);
        }
        if is_focused {
            comp.fill_back_clipped(
                Rect::new(pr.x + 4, pr.y + pr.h - 2, pr.w.saturating_sub(8), 2),
                tb_clip,
                tokens.border_focus,
            );
        }
        let lbl = if wm.screen_w < 1200 {
            app.compact_pill_label()
        } else {
            app.pill_label()
        };
        let tx = pr.x + pr.w.saturating_sub(lbl.len() * 8) / 2;
        unsafe {
            draw_str_raw(comp.back_ptr, comp.w, comp.h, tx, pr.y + 2, lbl, pfg, None, tb_clip);
        }
    }

    paint_mode_pill(comp, wm, tokens, tb_clip);

    let notif_r = topbar_notif_pill_rect(wm);
    let qs_r = topbar_qs_pill_rect(wm);
    if comp.w >= 1140 {
        let mut st = LineBuilder::new();
        st.push_bytes(if wm.gpu_ep_cap != 0 { b"GPU W:" } else { b"GOP W:" });
        st.push_u64(wm.active_window_count() as u64);
        let sl = st.as_bytes();
        let sx = notif_r.x.saturating_sub(sl.len() * 8 + 10);
        let mode_r = wm.topbar_mode_pill_rect();
        if sx > mode_r.x + mode_r.w + 8 {
            let col = if wm.gpu_ep_cap != 0 { tokens.ok } else { tokens.warn };
            unsafe {
                draw_str_raw(comp.back_ptr, comp.w, comp.h, sx, 6, sl, col, None, tb_clip);
            }
        }
    }

    let nbg = if wm.glass.notif_open {
        tokens.pill_active
    } else {
        tokens.pill_bg
    };
    let mut nlb = LineBuilder::new();
    nlb.push_bytes(b"N:");
    nlb.push_u64(wm.glass.notif_unread as u64);
    unsafe {
        fill_rounded_raw(comp.back_ptr, comp.w, comp.h, notif_r, tb_clip, 5, nbg);
        draw_icon16(
            comp.back_ptr,
            comp.w,
            comp.h,
            notif_r.x + 6,
            notif_r.y + 2,
            &crate::glass::icons::ICON_BELL,
            tokens.fg_primary,
            if wm.glass.notif_unread > 0 { tokens.accent } else { tokens.fg_muted },
            tb_clip,
        );
        draw_str_raw(
            comp.back_ptr,
            comp.w,
            comp.h,
            notif_r.x + 26,
            notif_r.y + 2,
            nlb.as_bytes(),
            tokens.fg_primary,
            None,
            tb_clip,
        );
    }

    let qbg = if wm.glass.qs_open {
        tokens.pill_active
    } else {
        tokens.pill_bg
    };
    let qlbl: &[u8] = match tokens.mode {
        ThemeMode::FrostedDaylight => b"Light | QS",
        ThemeMode::HighContrast => b"HC    | QS",
        ThemeMode::ObsidianDark => b"Dark  | QS",
    };
    unsafe {
        fill_rounded_raw(comp.back_ptr, comp.w, comp.h, qs_r, tb_clip, 5, qbg);
        draw_str_raw(
            comp.back_ptr,
            comp.w,
            comp.h,
            qs_r.x + qs_r.w.saturating_sub(qlbl.len() * 8) / 2,
            qs_r.y + 2,
            qlbl,
            tokens.accent,
            None,
            tb_clip,
        );
    }
}

fn paint_mode_pill(
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
        draw_str_raw(
            comp.back_ptr,
            comp.w,
            comp.h,
            pill.x + 8,
            6,
            mode_lbl,
            tokens.accent,
            None,
            tb_clip,
        );
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
    let suffix: &[u8] = if wm.desktop_mode {
        b" | ALT+TAB / ALT+T"
    } else {
        b" | ALT+ARROWS: MOVE"
    };
    for &b in suffix {
        if sp < status.len() {
            status[sp] = b;
            sp += 1;
        }
    }
    let rx = comp.w.saturating_sub(sp * 8 + 14);
    unsafe {
        draw_str_raw(
            comp.back_ptr,
            comp.w,
            comp.h,
            rx,
            6,
            &status[..sp],
            comp.pal.fg_ok,
            None,
            tb_clip,
        );
    }
}

fn paint_tabstrip(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    tokens: &GlassTokens,
    clip: Rect,
) {
    let strip_clip = clip
        .intersect(Rect::new(0, TABSTRIP_Y, comp.w, TABSTRIP_H))
        .clamp_to(comp.w, comp.h);
    if strip_clip.is_empty() {
        return;
    }
    let mut normal_slots = [0usize; MAX_WINDOWS];
    let n = wm.collect_normal_slots(&mut normal_slots);
    for (tab_i, &slot_idx) in normal_slots.iter().take(n).enumerate() {
        let tr = wm.tab_pill_rect(tab_i);
        let w = &wm.windows[slot_idx];
        let (tbg, tfg, bcol) = if w.focused {
            (tokens.pill_active, tokens.fg_primary, tokens.border_focus)
        } else {
            (tokens.pill_bg, tokens.fg_muted, tokens.border_idle)
        };
        unsafe {
            fill_rounded_raw(comp.back_ptr, comp.w, comp.h, tr, strip_clip, 5, tbg);
        }
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
        unsafe {
            draw_str_raw(comp.back_ptr, comp.w, comp.h, tr.x + 10, tr.y + 4, lbl, tfg, None, strip_clip);
        }
    }
}

pub fn paint_window_glass(
    comp: &mut DesktopCompositor,
    win: &WindowSlot,
    p3_mode: bool,
    tokens: &GlassTokens,
    clip: Rect,
) {
    if !win.allocated || !win.visible {
        return;
    }
    let outer = win.outer_rect();
    let wclip = outer.intersect(clip).clamp_to(comp.w, comp.h);
    if wclip.is_empty() {
        return;
    }
    let (wx, wy, ww, wh) = (win.rect.x, win.rect.y, win.rect.w, win.rect.h);

    // 1. Soft multi-band alpha-blended shadow.
    if tokens.mode == ThemeMode::HighContrast {
        comp.fill_back_clipped(Rect::new(wx + ww, wy + SHADOW_OFF, SHADOW_OFF, wh), wclip, comp.pal.bg_shadow);
        comp.fill_back_clipped(Rect::new(wx + SHADOW_OFF, wy + wh, ww.saturating_sub(SHADOW_OFF), SHADOW_OFF), wclip, comp.pal.bg_shadow);
    } else {
        for layer in 0..3usize {
            let off = (layer + 1) * 2;
            let a = (72 - layer * 22) as u8;
            unsafe {
                blend_shadow_rect(
                    comp.back_ptr, comp.w, comp.h, &comp.fb_info,
                    Rect::new(wx + ww + layer * 2, wy + off, 2, wh.saturating_sub(layer)),
                    wclip, tokens.shadow_rgb, a,
                );
                blend_shadow_rect(
                    comp.back_ptr, comp.w, comp.h, &comp.fb_info,
                    Rect::new(wx + off, wy + wh + layer * 2, ww.saturating_sub(off - 2), 2),
                    wclip, tokens.shadow_rgb, a,
                );
            }
        }
    }

    // 2. Rounded outer frame border.
    let corner_r = if tokens.mode == ThemeMode::HighContrast { 0 } else { GLASS_CORNER_R };
    let bcol = if win.focused { tokens.border_focus } else { tokens.border_idle };
    let win_inter = win.rect.intersect(wclip);
    for py in win_inter.y..(win_inter.y + win_inter.h) {
        let on_top = py < wy + WIN_BORDER;
        let on_bot = py >= wy + wh - WIN_BORDER;
        let row_off = py * comp.w;
        for px in win_inter.x..(win_inter.x + win_inter.w) {
            if !in_rounded_rect(win.rect, corner_r, px, py) {
                continue;
            }
            let on_side = px < wx + WIN_BORDER || px >= wx + ww - WIN_BORDER;
            let on_round_edge = corner_r > 0
                && (!in_rounded_rect(win.rect, corner_r, px.saturating_sub(1), py)
                    || !in_rounded_rect(win.rect, corner_r, px + 1, py)
                    || !in_rounded_rect(win.rect, corner_r, px, py.saturating_sub(1))
                    || !in_rounded_rect(win.rect, corner_r, px, py + 1));
            if on_top || on_bot || on_side || on_round_edge {
                unsafe { *comp.back_ptr.add(row_off + px) = bcol };
            }
        }
    }

    // 3. Titlebar chrome + app icon + `[×]` close button.
    let tb_rect = win.titlebar_rect();
    if !tb_rect.intersect(wclip).is_empty() {
        let tb_rgb = if win.focused { tokens.title_focus_rgb } else { tokens.title_idle_rgb };
        let tb_col = pack_rgb(&comp.fb_info, tb_rgb.0, tb_rgb.1, tb_rgb.2);
        let tb_inter = tb_rect.intersect(wclip);
        for py in tb_inter.y..(tb_inter.y + tb_inter.h) {
            let row_off = py * comp.w;
            for px in tb_inter.x..(tb_inter.x + tb_inter.w) {
                if in_rounded_rect(win.rect, corner_r, px, py) {
                    unsafe { *comp.back_ptr.add(row_off + px) = tb_col };
                }
            }
        }
        let title_fg = if win.focused { tokens.fg_primary } else { tokens.fg_muted };
        let mut title_x = wx + WIN_BORDER + 10;
        if let Some(app) = app_from_title_kind(win.title_kind) {
            unsafe {
                draw_icon16(
                    comp.back_ptr, comp.w, comp.h,
                    title_x, wy + WIN_BORDER + 6,
                    icon_for_app(app), title_fg, tokens.accent, wclip,
                );
            }
            title_x += 22;
        }
        let title_str: &[u8] = match win.title_kind {
            TITLE_KIND_SYSMON if p3_mode => b"LIFE STATION // dry-sysmon [0x0103]",
            TITLE_KIND_SYSMON => b"SYSTEM MONITOR // dry-sysmon [0x0103]",
            TITLE_KIND_TERMINAL => b"DRY TERMINAL // dry-shell [0x0102]",
            TITLE_KIND_FILES => b"FILES // dry-files [0x0104]",
            TITLE_KIND_SETTINGS => b"SETTINGS // dry-settings [0x0105]",
            TITLE_KIND_EDITOR => b"DRY NOTES // dry-editor [0x0106]",
            TITLE_KIND_CALC => b"CALCULATOR // dry-calc [0x0107]",
            TITLE_KIND_DIALOG => b"QUICK HELP // Dialog [AUTO-FLOAT]",
            _ => b"DRY APP // [CPL=3]",
        };
        unsafe {
            draw_str_raw(comp.back_ptr, comp.w, comp.h, title_x, wy + WIN_BORDER + 6, title_str, title_fg, None, wclip);
        }

        let close_r = win.close_btn_rect();
        unsafe {
            fill_rounded_raw(comp.back_ptr, comp.w, comp.h, close_r, wclip, 4, tokens.close_bg);
            draw_str_raw(comp.back_ptr, comp.w, comp.h, close_r.x + 7, close_r.y + 2, b"x", tokens.close_fg, None, wclip);
        }

        let mut badge = [b' '; 20];
        let mut p = 0usize;
        if win.is_pixel {
            for &b in b"PIX" {
                badge[p] = b;
                p += 1;
            }
        } else if win.title_kind == TITLE_KIND_TERMINAL {
            for &b in b"CMD:" {
                badge[p] = b;
                p += 1;
            }
            p += write_u32_ascii(&mut badge[p..], win.snapshot.cmd_count);
        } else {
            for &b in b"UP:" {
                badge[p] = b;
                p += 1;
            }
            p += write_u32_ascii(&mut badge[p..], win.snapshot.uptime_sec_snap);
            if p < badge.len() {
                badge[p] = b's';
                p += 1;
            }
        }
        let bx = close_r.x.saturating_sub(p * 8 + 10);
        if bx > title_x + title_str.len() * 8 + 12 {
            unsafe {
                draw_str_raw(comp.back_ptr, comp.w, comp.h, bx, wy + WIN_BORDER + 6, &badge[..p], tokens.ok, None, wclip);
            }
        }
    }

    // 4. Client body (`SharedPixelSurface` RGB blit OR `SharedWindowSurface` character grid).
    paint_window_client_body(comp, win, wclip);
}

fn paint_window_client_body(comp: &mut DesktopCompositor, win: &WindowSlot, wclip: Rect) {
    let (wx, wy, ww, wh) = (win.rect.x, win.rect.y, win.rect.w, win.rect.h);
    let body_rect = Rect::new(
        wx + WIN_BORDER,
        wy + WIN_BORDER + WIN_TITLE_H,
        ww.saturating_sub(WIN_BORDER * 2),
        wh.saturating_sub(WIN_BORDER * 2 + WIN_TITLE_H),
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
        comp.fill_back_clipped(body_rect, body_clip, comp.pal.bg_win);
        return;
    }

    // Fill outer padding strips cleanly around `content_rect`.
    comp.fill_back_clipped(Rect::new(body_rect.x, body_rect.y, body_rect.w, gy - body_rect.y), body_clip, comp.pal.bg_win);
    let c_bot = gy + content_h;
    if body_rect.y + body_rect.h > c_bot {
        comp.fill_back_clipped(Rect::new(body_rect.x, c_bot, body_rect.w, body_rect.y + body_rect.h - c_bot), body_clip, comp.pal.bg_win);
    }
    comp.fill_back_clipped(Rect::new(body_rect.x, gy, gx - body_rect.x, content_h), body_clip, comp.pal.bg_win);
    let c_right = gx + content_w;
    if body_rect.x + body_rect.w > c_right {
        comp.fill_back_clipped(Rect::new(c_right, gy, body_rect.x + body_rect.w - c_right, content_h), body_clip, comp.pal.bg_win);
    }

    let cclip = Rect::new(gx, gy, content_w, content_h).intersect(body_clip);
    if cclip.is_empty() {
        return;
    }
    if win.is_pixel {
        let src_base = (win.snap_vaddr + PIXEL_SURF_HEADER_BYTES as u64) as *const u32;
        let native_bgrx = comp.fb_info.red_shift == 16 && comp.fb_info.green_shift == 8 && comp.fb_info.blue_shift == 0;
        for py in cclip.y..(cclip.y + cclip.h) {
            let sy = py - gy;
            let sx0 = cclip.x - gx;
            let src_row = unsafe { src_base.add(sy * content_w + sx0) };
            let dst_row = unsafe { comp.back_ptr.add(py * comp.w + cclip.x) };
            if native_bgrx {
                unsafe { core::ptr::copy_nonoverlapping(src_row, dst_row, cclip.w) };
            } else {
                for i in 0..cclip.w {
                    let argb = unsafe { *src_row.add(i) };
                    let p = pack_rgb(&comp.fb_info, ((argb >> 16) & 0xff) as u8, ((argb >> 8) & 0xff) as u8, (argb & 0xff) as u8);
                    unsafe { *dst_row.add(i) = p };
                }
            }
        }
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

pub fn paint_dock_and_snap(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    tokens: &GlassTokens,
    clip: Rect,
) {
    if !wm.p3_mode {
        return;
    }
    if let Some(sr) = wm.glass.snap_preview {
        let sc = sr.intersect(clip).clamp_to(comp.w, comp.h);
        if !sc.is_empty() {
            let tint = match tokens.mode {
                ThemeMode::FrostedDaylight => (0x02, 0x84, 0xC7),
                _ => (0x22, 0xD3, 0xEE),
            };
            unsafe {
                backdrop_frosted_rect(
                    comp.back_ptr, comp.w, comp.h, &comp.fb_info, sr, sc, tint, 64, false, 8,
                    tokens.border_focus, tokens.border_focus,
                );
            }
        }
    }

    let dr = dock_rect(wm);
    let dclip = dr.intersect(clip).clamp_to(comp.w, comp.h);
    if dclip.is_empty() {
        return;
    }
    let blur = !wm.low_overhead_glass && tokens.mode != ThemeMode::HighContrast;
    unsafe {
        backdrop_frosted_rect(
            comp.back_ptr, comp.w, comp.h, &comp.fb_info, dr, dclip,
            tokens.dock_tint_rgb, tokens.dock_alpha, blur, 10,
            tokens.glass_rim, tokens.glass_rim_soft,
        );
    }

    let focused_badge = wm.focused_slot().map_or(0, |s| wm.windows[s].owner_badge);
    for idx in 0..APP_COUNT {
        let app = AppId::ALL[idx];
        let ir = dock_item_rect(wm, idx);
        let is_focused = focused_badge == app.win_badge();
        let is_running = (wm.running_apps_mask & (1u8 << idx)) != 0
            || wm.windows.iter().any(|w| w.allocated && w.owner_badge == app.win_badge());
        let tile_bg = if is_focused { tokens.pill_active } else { tokens.pill_bg };
        unsafe {
            fill_rounded_raw(comp.back_ptr, comp.w, comp.h, ir, dclip, 7, tile_bg);
            draw_icon16(
                comp.back_ptr, comp.w, comp.h,
                ir.x + (ir.w - 16) / 2, ir.y + (ir.h - 16) / 2,
                icon_for_app(app), tokens.fg_primary,
                if is_running { tokens.accent } else { tokens.fg_muted },
                dclip,
            );
        }
        if is_focused || is_running {
            let dot_col = if is_focused { tokens.border_focus } else { tokens.ok };
            let dw = if is_focused { 16 } else { 6 };
            comp.fill_back_clipped(Rect::new(ir.x + (ir.w - dw) / 2, ir.y + ir.h + 1, dw, 2), dclip, dot_col);
        }
    }
}

pub fn expand_blur_damage(wm: &WindowManager, dirty: &mut DirtyRegion) {
    if !wm.p3_mode {
        return;
    }
    let dr = dock_rect(wm);
    if !dirty.intersect_any(dr) {
        // Leave clean dock untouched for fast incremental redraws.
    } else {
        dirty.mark(dr);
    }
    if wm.glass.launcher_open {
        let full_lr = crate::glass::overlays::launcher_modal_rect(wm);
        if dirty.intersect_any(full_lr) {
            dirty.mark(full_lr);
        }
    }
    if wm.glass.qs_open {
        let qr = crate::glass::overlays::qs_panel_rect(wm);
        if dirty.intersect_any(qr) {
            dirty.mark(qr);
        }
    }
    if wm.glass.notif_open {
        let nr = crate::glass::overlays::notif_panel_rect(wm);
        if dirty.intersect_any(nr) {
            dirty.mark(nr);
        }
    }
}
