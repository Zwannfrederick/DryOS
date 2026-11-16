// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Floating Frosted Dock, Snap Preview & Blur Damage Expansion (`DRY Glass 2.0`).

use crate::action::AppId;
use crate::compositor::DesktopCompositor;
use crate::gfx::{DirtyRegion, Rect};
use crate::glass::blur::{backdrop_frosted_rect, blend_soft_rrect_shadow};
use crate::glass::overlays::{dock_item_rect, dock_rect};
use crate::glass::theme::{GlassTokens, ThemeMode};
use crate::ttf::PixelFormat;
use crate::ui::app_icons::draw_app_icon_for_app;
use crate::ui::canvas::Canvas;
use crate::ui::theme::UiTheme;
use crate::wm::{WindowManager, TOPBAR_H};

#[inline]
fn pad_rect(r: Rect, pad: usize) -> Rect {
    Rect::new(
        r.x.saturating_sub(pad),
        r.y.saturating_sub(pad),
        r.w + pad * 2,
        r.h + pad * 2,
    )
}

pub fn paint_dock_and_snap(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    tokens: &GlassTokens,
    clip: Rect,
) {
    let blur = wm.p3_mode && !wm.low_overhead_glass && tokens.mode != ThemeMode::HighContrast;
    let ui = UiTheme::new(tokens.mode, wm.dpi_pct());
    let fmt = PixelFormat::from_fb(&comp.fb_info);

    if wm.p3_mode {
        if let Some(sr) = wm.glass.snap_preview {
            let sc = sr.intersect(clip).clamp_to(comp.w, comp.h);
            if !sc.is_empty() {
                unsafe {
                    backdrop_frosted_rect(
                        comp.back_ptr,
                        comp.w,
                        comp.h,
                        &comp.fb_info,
                        sr,
                        sc,
                        (
                            ((ui.accent >> 16) & 0xff) as u8,
                            ((ui.accent >> 8) & 0xff) as u8,
                            (ui.accent & 0xff) as u8,
                        ),
                        56,
                        false,
                        10,
                        tokens.glass_rim,
                        tokens.glass_rim_soft,
                    );
                }
                let mut c = unsafe {
                    Canvas::from_raw(comp.back_ptr, comp.w, comp.h, comp.fb_stride, fmt, sc)
                };
                c.stroke_rrect_aa(sr, 10, ui.accent, 220);
            }
        }
    }

    if !wm.p3_mode {
        return;
    }

    let dr = dock_rect(wm);
    let dclip = pad_rect(dr, 6).intersect(clip).clamp_to(comp.w, comp.h);
    if dclip.is_empty() {
        return;
    }

    let dr_inner = dr.intersect(dclip);
    if tokens.mode != ThemeMode::HighContrast {
        unsafe {
            blend_soft_rrect_shadow(
                comp.back_ptr,
                comp.w,
                comp.h,
                &comp.fb_info,
                dr,
                dclip,
                12,
                tokens.shadow_rgb,
                84,
                5,
            );
        }
    }

    if !dr_inner.is_empty() {
        unsafe {
            backdrop_frosted_rect(
                comp.back_ptr,
                comp.w,
                comp.h,
                &comp.fb_info,
                dr,
                dr_inner,
                tokens.dock_tint_rgb,
                tokens.dock_alpha,
                blur,
                12,
                tokens.glass_rim,
                tokens.glass_rim_soft,
            );
        }
    }

    let mut c =
        unsafe { Canvas::from_raw(comp.back_ptr, comp.w, comp.h, comp.fb_stride, fmt, dclip) };
    let rim_col = if tokens.mode == ThemeMode::HighContrast {
        ui.accent
    } else {
        ui.border_strong
    };
    c.stroke_rrect_aa(dr, 12, rim_col, 190);
    if tokens.mode != ThemeMode::HighContrast {
        c.hline(dr.x + 12, dr.y + 1, dr.w.saturating_sub(24), ui.border_strong, 115);
    }

    let focused_badge = wm.focused_slot().map_or(0, |s| wm.windows[s].owner_badge);
    for (idx, app) in AppId::ALL.iter().copied().enumerate() {
        let ir = dock_item_rect(wm, idx);
        let (win_total, win_min) = wm.count_windows_for_app(app);
        let is_focused = focused_badge == app.win_badge();
        let all_min = win_total > 0 && win_min >= win_total;
        let is_running = (wm.running_apps_mask & (1u8 << idx)) != 0 || win_total > 0;

        if is_focused {
            c.fill_rrect_aa(ir, 9, ui.accent_subtle, 245);
            c.stroke_rrect_aa(ir, 9, ui.accent, 235);
        } else if is_running {
            c.fill_rrect_aa(ir, 9, ui.bg_card_alt, 235);
            c.stroke_rrect_aa(ir, 9, ui.border_strong, 185);
        } else {
            c.fill_rrect_aa(ir, 9, ui.bg_card, 200);
            c.stroke_rrect_aa(ir, 9, ui.border_subtle, 145);
        }

        let icon_r = Rect::new(ir.x + 4, ir.y + 4, ir.w.saturating_sub(8), ir.h.saturating_sub(8));
        draw_app_icon_for_app(&mut c, icon_r, app, tokens.mode);

        if is_focused {
            let dot = Rect::new(ir.x + 10, ir.y + ir.h + 1, ir.w.saturating_sub(20), 3);
            c.fill_rrect_aa(dot, 1, ui.accent, 255);
        } else if win_total > 1 {
            let dcol = if all_min { ui.warn } else { ui.fg_primary };
            c.fill_rrect_aa(Rect::new(ir.x + 11, ir.y + ir.h + 1, 5, 2), 1, dcol, 230);
            c.fill_rrect_aa(Rect::new(ir.x + 20, ir.y + ir.h + 1, 5, 2), 1, dcol, 230);
        } else if is_running {
            let dot = Rect::new(ir.x + 14, ir.y + ir.h + 1, ir.w.saturating_sub(28), 2);
            c.fill_rrect_aa(dot, 1, if all_min { ui.warn } else { ui.fg_muted }, 220);
        }
    }
}

pub fn expand_blur_damage(wm: &WindowManager, dirty: &mut DirtyRegion) {
    if !wm.p3_mode {
        return;
    }
    let tb = Rect::new(0, 0, wm.screen_w, TOPBAR_H);
    if dirty.intersect_any(tb) {
        dirty.mark(tb);
    }
    let dr = pad_rect(dock_rect(wm), 6);
    if dirty.intersect_any(dr) {
        dirty.mark(dr);
    }
    if wm.glass.overview_open {
        let ov_r = pad_rect(crate::glass::overview::overview_modal_rect(wm), 6);
        if dirty.intersect_any(ov_r) {
            dirty.mark(ov_r);
        }
    }
    if wm.glass.launcher_open {
        let full_lr = pad_rect(crate::glass::overlays::launcher_modal_rect(wm), 6);
        if dirty.intersect_any(full_lr) {
            dirty.mark(full_lr);
        }
    }
    if wm.glass.qs_open {
        let qr = pad_rect(crate::glass::overlays::qs_panel_rect(wm), 6);
        if dirty.intersect_any(qr) {
            dirty.mark(qr);
        }
    }
    if wm.glass.notif_open {
        let nr = pad_rect(crate::glass::overlays::notif_panel_rect(wm), 6);
        if dirty.intersect_any(nr) {
            dirty.mark(nr);
        }
    }
}
