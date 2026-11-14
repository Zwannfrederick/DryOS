// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `DRY Glass` Overlays, Hit-Testing, Searchable Launcher, Genuine Quick Settings,
//! Notification Center & Frosted Session Lock Shield (`Milestone Ascension 3.0`).

use crate::action::{AppId, DesktopAction, APP_COUNT};
use crate::compositor::DesktopCompositor;
use crate::gfx::{DirtyRegion, Rect};
use crate::glass::blur::{backdrop_frosted_rect, fill_rounded_raw};
use crate::glass::icons::{draw_icon16, icon_for_app, ICON_BELL, ICON_SEARCH};
use crate::glass::theme::{GlassTokens, ThemeMode};
use crate::glass::typography::draw_str_raw;
use crate::glass::{DOCK_H, DOCK_MARGIN_B};
use crate::input::{InputEvent, InputEventKind, MOUSE_BTN_LEFT};
use crate::keymap::{KEY_BACKSPACE, KEY_DOWN, KEY_ENTER, KEY_ESC, KEY_UP};
use crate::shell::LineBuilder;
use crate::wm::{
    LayoutMode, SessionVisualState, WindowManager, WmInputAction, TITLE_KIND_CALC, TITLE_KIND_EDITOR,
    TITLE_KIND_FILES, TITLE_KIND_SETTINGS, TITLE_KIND_SYSMON, TITLE_KIND_TERMINAL, TOPBAR_H,
};
use crate::wm_ipc::wm_log_line;

const DOCK_ITEM_W: usize = 36;
const DOCK_ITEM_GAP: usize = 10;

#[inline]
pub const fn app_from_title_kind(tk: u8) -> Option<AppId> {
    match tk {
        TITLE_KIND_TERMINAL => Some(AppId::Terminal),
        TITLE_KIND_SYSMON => Some(AppId::LifeStation),
        TITLE_KIND_FILES => Some(AppId::Files),
        TITLE_KIND_SETTINGS => Some(AppId::Settings),
        TITLE_KIND_EDITOR => Some(AppId::Editor),
        TITLE_KIND_CALC => Some(AppId::Calculator),
        _ => None,
    }
}

#[inline]
pub const fn topbar_brand_pill_rect(wm: &WindowManager) -> Rect {
    if wm.screen_w < 1200 {
        Rect::new(4, 4, 80, 20)
    } else {
        Rect::new(6, 4, 102, 20)
    }
}

#[inline]
pub fn topbar_notif_pill_rect(wm: &WindowManager) -> Rect {
    Rect::new(wm.screen_w.saturating_sub(178), 4, 66, 20)
}

#[inline]
pub fn topbar_qs_pill_rect(wm: &WindowManager) -> Rect {
    Rect::new(wm.screen_w.saturating_sub(106), 4, 100, 20)
}

#[inline]
pub fn dock_rect(wm: &WindowManager) -> Rect {
    let w = APP_COUNT * DOCK_ITEM_W + (APP_COUNT.saturating_sub(1)) * DOCK_ITEM_GAP + 24;
    let x = wm.screen_w.saturating_sub(w) / 2;
    let y = wm.screen_h.saturating_sub(DOCK_H + DOCK_MARGIN_B);
    Rect::new(x, y, w, DOCK_H)
}

#[inline]
pub fn dock_item_rect(wm: &WindowManager, idx: usize) -> Rect {
    let dr = dock_rect(wm);
    Rect::new(
        dr.x + 12 + idx * (DOCK_ITEM_W + DOCK_ITEM_GAP),
        dr.y + 4,
        DOCK_ITEM_W,
        DOCK_ITEM_W,
    )
}

#[inline]
pub fn launcher_modal_rect(wm: &WindowManager) -> Rect {
    let w = 460usize.min(wm.screen_w.saturating_sub(32));
    let x = wm.screen_w.saturating_sub(w) / 2;
    Rect::new(x, TOPBAR_H + 34, w, 330)
}

#[inline]
pub fn launcher_app_rect(wm: &WindowManager, row: usize) -> Rect {
    let m = launcher_modal_rect(wm);
    Rect::new(m.x + 14, m.y + 60 + row * 42, m.w - 28, 36)
}

#[inline]
pub fn qs_panel_rect(wm: &WindowManager) -> Rect {
    let w = 324usize.min(wm.screen_w.saturating_sub(24));
    Rect::new(wm.screen_w.saturating_sub(w + 10), TOPBAR_H + 8, w, 308)
}

#[inline]
pub fn qs_row_rect(wm: &WindowManager, row: usize) -> Rect {
    let p = qs_panel_rect(wm);
    Rect::new(p.x + 14, p.y + 42 + row * 38, p.w - 28, 32)
}

#[inline]
pub fn notif_panel_rect(wm: &WindowManager) -> Rect {
    let w = 330usize.min(wm.screen_w.saturating_sub(24));
    Rect::new(wm.screen_w.saturating_sub(w + 10), TOPBAR_H + 8, w, 258)
}

#[inline]
pub fn notif_clear_rect(wm: &WindowManager) -> Rect {
    let p = notif_panel_rect(wm);
    Rect::new(p.x + p.w - 72, p.y + 10, 58, 22)
}

fn contains_ascii_icase(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    let to_low = |b: u8| if b.is_ascii_uppercase() { b + 32 } else { b };
    for start in 0..=(haystack.len() - needle.len()) {
        let mut ok = true;
        for j in 0..needle.len() {
            if to_low(haystack[start + j]) != to_low(needle[j]) {
                ok = false;
                break;
            }
        }
        if ok {
            return true;
        }
    }
    false
}

pub fn filter_launcher_apps(wm: &WindowManager, out: &mut [AppId; APP_COUNT]) -> usize {
    let q = &wm.glass.launcher_query[..wm.glass.launcher_len as usize];
    let mut n = 0usize;
    for app in AppId::ALL {
        if contains_ascii_icase(app.display_title(), q) || contains_ascii_icase(app.slug(), q) {
            out[n] = app;
            n += 1;
        }
    }
    n
}

fn log_glass_toggle(kind: &[u8], open: bool) {
    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-GLASS] ");
    lb.push_bytes(kind);
    lb.push_bytes(b" open=");
    lb.push_u64(u64::from(open));
    wm_log_line(&lb);
}

pub fn toggle_launcher(wm: &mut WindowManager, dirty: &mut DirtyRegion) {
    let next = !wm.glass.launcher_open;
    if wm.glass.qs_open {
        dirty.mark(qs_panel_rect(wm));
    }
    if wm.glass.notif_open {
        dirty.mark(notif_panel_rect(wm));
    }
    wm.glass.close_overlays();
    wm.glass.launcher_open = next;
    dirty.mark(Rect::new(0, 0, wm.screen_w, TOPBAR_H));
    dirty.mark(launcher_modal_rect(wm));
    log_glass_toggle(b"LAUNCHER", next);
}

pub fn close_all_overlays(wm: &mut WindowManager, dirty: &mut DirtyRegion) {
    if wm.glass.launcher_open {
        dirty.mark(launcher_modal_rect(wm));
        log_glass_toggle(b"LAUNCHER", false);
    }
    if wm.glass.qs_open {
        dirty.mark(qs_panel_rect(wm));
        log_glass_toggle(b"QS", false);
    }
    if wm.glass.notif_open {
        dirty.mark(notif_panel_rect(wm));
        log_glass_toggle(b"NOTIF", false);
    }
    wm.glass.close_overlays();
    dirty.mark(Rect::new(0, 0, wm.screen_w, TOPBAR_H));
}

pub fn handle_locked_input(
    wm: &mut WindowManager,
    ev: InputEvent,
    scene_dirty: &mut DirtyRegion,
    cursor_dirty: &mut DirtyRegion,
) -> WmInputAction {
    match ev.kind {
        InputEventKind::MouseMove => {
            let old_c = wm.cursor_rect();
            wm.cursor_visible = true;
            wm.desktop_mode = true;
            let max_x = wm.screen_w.saturating_sub(1) as isize;
            let max_y = wm.screen_h.saturating_sub(1) as isize;
            wm.cursor_x = ((wm.cursor_x as isize) + (ev.dx as isize)).clamp(0, max_x) as usize;
            wm.cursor_y = ((wm.cursor_y as isize) + (ev.dy as isize)).clamp(0, max_y) as usize;
            cursor_dirty.mark(old_c);
            cursor_dirty.mark(wm.cursor_rect());
            WmInputAction::CursorMoved
        }
        InputEventKind::MouseButtonDown => {
            if !wm.cursor_visible {
                wm.cursor_visible = true;
                cursor_dirty.mark(wm.cursor_rect());
            }
            wm.buttons_down = ev.buttons;
            if (ev.scancode as u8) == MOUSE_BTN_LEFT
                && wm.unlock_button_rect().contains(wm.cursor_x, wm.cursor_y)
            {
                if wm.vault_auth_required {
                    if wm.lock_pin_len > 0 {
                        scene_dirty.mark(Rect::new(0, 0, wm.screen_w, wm.screen_h));
                        return wm.enqueue_desktop_action(DesktopAction::SubmitVaultUnlock(wm.lock_pin_len));
                    }
                    wm.locked_blocked_events = wm.locked_blocked_events.wrapping_add(1);
                    scene_dirty.mark(Rect::new(0, 0, wm.screen_w, wm.screen_h));
                    return WmInputAction::LockBlocked;
                }
                wm.session_state = SessionVisualState::Active;
                scene_dirty.mark(Rect::new(0, 0, wm.screen_w, wm.screen_h));
                return wm.enqueue_desktop_action(DesktopAction::UnlockSession);
            }
            wm.locked_blocked_events = wm.locked_blocked_events.wrapping_add(1);
            scene_dirty.mark(Rect::new(0, 0, wm.screen_w, wm.screen_h));
            WmInputAction::LockBlocked
        }
        InputEventKind::KeyPress => {
            if wm.vault_auth_required {
                if ev.keycode == KEY_ENTER {
                    if wm.lock_pin_len > 0 {
                        scene_dirty.mark(Rect::new(0, 0, wm.screen_w, wm.screen_h));
                        return wm.enqueue_desktop_action(DesktopAction::SubmitVaultUnlock(wm.lock_pin_len));
                    }
                    wm.locked_blocked_events = wm.locked_blocked_events.wrapping_add(1);
                    scene_dirty.mark(Rect::new(0, 0, wm.screen_w, wm.screen_h));
                    return WmInputAction::LockBlocked;
                }
                if ev.keycode == KEY_BACKSPACE {
                    if wm.lock_pin_len > 0 {
                        wm.lock_pin_len -= 1;
                        wm.lock_pin_buf[wm.lock_pin_len as usize] = 0;
                    }
                    scene_dirty.mark(Rect::new(0, 0, wm.screen_w, wm.screen_h));
                    return WmInputAction::LockPinEdited;
                }
                if ev.keycode == KEY_ESC {
                    wm.clear_lock_pin();
                    scene_dirty.mark(Rect::new(0, 0, wm.screen_w, wm.screen_h));
                    return WmInputAction::LockPinEdited;
                }
                if (0x21..=0x7e).contains(&ev.ascii) && (ev.modifiers & crate::input::MOD_ALT) == 0 {
                    if (wm.lock_pin_len as usize) < wm.lock_pin_buf.len() {
                        wm.lock_pin_buf[wm.lock_pin_len as usize] = ev.ascii;
                        wm.lock_pin_len += 1;
                    }
                    scene_dirty.mark(Rect::new(0, 0, wm.screen_w, wm.screen_h));
                    return WmInputAction::LockPinEdited;
                }
                wm.locked_blocked_events = wm.locked_blocked_events.wrapping_add(1);
                scene_dirty.mark(Rect::new(0, 0, wm.screen_w, wm.screen_h));
                return WmInputAction::LockBlocked;
            }
            if ev.keycode == KEY_ENTER {
                wm.session_state = SessionVisualState::Active;
                scene_dirty.mark(Rect::new(0, 0, wm.screen_w, wm.screen_h));
                return wm.enqueue_desktop_action(DesktopAction::UnlockSession);
            }
            wm.locked_blocked_events = wm.locked_blocked_events.wrapping_add(1);
            scene_dirty.mark(Rect::new(0, wm.screen_h / 2, wm.screen_w, 96));
            WmInputAction::LockBlocked
        }
        InputEventKind::MouseScroll => {
            wm.locked_blocked_events = wm.locked_blocked_events.wrapping_add(1);
            scene_dirty.mark(Rect::new(0, wm.screen_h / 2, wm.screen_w, 96));
            WmInputAction::LockBlocked
        }
        _ => WmInputAction::None,
    }
}

pub fn handle_overlay_click(wm: &mut WindowManager, dirty: &mut DirtyRegion) -> Option<WmInputAction> {
    if !wm.p3_mode {
        return None;
    }
    if wm.cursor_y < TOPBAR_H {
        if topbar_brand_pill_rect(wm).contains(wm.cursor_x, wm.cursor_y) {
            toggle_launcher(wm, dirty);
            return Some(WmInputAction::None);
        }
        if topbar_qs_pill_rect(wm).contains(wm.cursor_x, wm.cursor_y) {
            let next = !wm.glass.qs_open;
            close_all_overlays(wm, dirty);
            wm.glass.qs_open = next;
            dirty.mark(qs_panel_rect(wm));
            dirty.mark(Rect::new(0, 0, wm.screen_w, TOPBAR_H));
            log_glass_toggle(b"QS", next);
            return Some(WmInputAction::None);
        }
        if topbar_notif_pill_rect(wm).contains(wm.cursor_x, wm.cursor_y) {
            let next = !wm.glass.notif_open;
            close_all_overlays(wm, dirty);
            wm.glass.notif_open = next;
            if next {
                wm.glass.notif_unread = 0;
            }
            dirty.mark(notif_panel_rect(wm));
            dirty.mark(Rect::new(0, 0, wm.screen_w, TOPBAR_H));
            log_glass_toggle(b"NOTIF", next);
            return Some(WmInputAction::None);
        }
    }

    if wm.glass.launcher_open {
        let lr = launcher_modal_rect(wm);
        if lr.contains(wm.cursor_x, wm.cursor_y) {
            let mut apps = [AppId::Terminal; APP_COUNT];
            let n = filter_launcher_apps(wm, &mut apps);
            for (row, &app) in apps.iter().take(n).enumerate() {
                if launcher_app_rect(wm, row).contains(wm.cursor_x, wm.cursor_y) {
                    close_all_overlays(wm, dirty);
                    return Some(wm.enqueue_desktop_action(DesktopAction::LaunchOrFocusApp(app)));
                }
            }
            return Some(WmInputAction::None);
        }
        close_all_overlays(wm, dirty);
    }

    if wm.glass.qs_open {
        let qr = qs_panel_rect(wm);
        if qr.contains(wm.cursor_x, wm.cursor_y) {
            let cur_flags = u8::from(wm.high_contrast)
                | (u8::from(wm.cursor_large) << 1)
                | (u8::from(wm.light_mode) << 2)
                | (u8::from(wm.low_overhead_glass) << 3);
            for row in 0..6usize {
                if qs_row_rect(wm, row).contains(wm.cursor_x, wm.cursor_y) {
                    dirty.mark(qr);
                    let act = match row {
                        0 => DesktopAction::SetThemeContrast(cur_flags ^ 4),
                        1 => DesktopAction::SetThemeContrast(cur_flags ^ 8),
                        2 => DesktopAction::SetThemeContrast(cur_flags ^ 1),
                        3 => DesktopAction::SetThemeContrast(cur_flags ^ 2),
                        4 => DesktopAction::SetLayoutMode(wm.layout.next_mode() as u8),
                        _ => {
                            close_all_overlays(wm, dirty);
                            DesktopAction::LockSession
                        }
                    };
                    return Some(wm.enqueue_desktop_action(act));
                }
            }
            return Some(WmInputAction::None);
        }
        close_all_overlays(wm, dirty);
    }

    if wm.glass.notif_open {
        let nr = notif_panel_rect(wm);
        if nr.contains(wm.cursor_x, wm.cursor_y) {
            if notif_clear_rect(wm).contains(wm.cursor_x, wm.cursor_y) {
                wm.glass.clear_notifs();
                dirty.mark(nr);
                dirty.mark(Rect::new(0, 0, wm.screen_w, TOPBAR_H));
            }
            return Some(WmInputAction::None);
        }
        close_all_overlays(wm, dirty);
    }

    if dock_rect(wm).contains(wm.cursor_x, wm.cursor_y) {
        for idx in 0..APP_COUNT {
            if dock_item_rect(wm, idx).contains(wm.cursor_x, wm.cursor_y) {
                return Some(
                    wm.enqueue_desktop_action(DesktopAction::LaunchOrFocusApp(AppId::ALL[idx])),
                );
            }
        }
        return Some(WmInputAction::None);
    }

    None
}

pub fn handle_overlay_key(wm: &mut WindowManager, ev: InputEvent, dirty: &mut DirtyRegion) -> Option<WmInputAction> {
    if !wm.p3_mode || !wm.glass.any_overlay_open() {
        return None;
    }
    if ev.keycode == KEY_ESC {
        close_all_overlays(wm, dirty);
        return Some(WmInputAction::None);
    }
    if wm.glass.launcher_open {
        let mut apps = [AppId::Terminal; APP_COUNT];
        let n = filter_launcher_apps(wm, &mut apps);
        if ev.keycode == KEY_ENTER && n > 0 {
            let sel = wm.glass.launcher_sel.min(n - 1);
            let app = apps[sel];
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"[DRY-GLASS] LAUNCHER_EXEC app=");
            lb.push_bytes(app.slug());
            wm_log_line(&lb);
            close_all_overlays(wm, dirty);
            return Some(wm.enqueue_desktop_action(DesktopAction::LaunchOrFocusApp(app)));
        }
        if ev.keycode == KEY_UP {
            wm.glass.launcher_sel = wm.glass.launcher_sel.saturating_sub(1);
            dirty.mark(launcher_modal_rect(wm));
            return Some(WmInputAction::None);
        }
        if ev.keycode == KEY_DOWN && wm.glass.launcher_sel + 1 < n {
            wm.glass.launcher_sel += 1;
            dirty.mark(launcher_modal_rect(wm));
            return Some(WmInputAction::None);
        }
        if ev.keycode == KEY_BACKSPACE {
            if wm.glass.launcher_len > 0 {
                wm.glass.launcher_len -= 1;
                let n2 = filter_launcher_apps(wm, &mut apps);
                wm.glass.clamp_launcher_sel(n2);
                dirty.mark(launcher_modal_rect(wm));
            }
            return Some(WmInputAction::None);
        }
        if (0x20..=0x7e).contains(&ev.ascii) {
            let len = wm.glass.launcher_len as usize;
            if len < wm.glass.launcher_query.len() {
                wm.glass.launcher_query[len] = ev.ascii;
                wm.glass.launcher_len += 1;
                let n2 = filter_launcher_apps(wm, &mut apps);
                wm.glass.clamp_launcher_sel(n2);
                dirty.mark(launcher_modal_rect(wm));
            }
            return Some(WmInputAction::None);
        }
        return Some(WmInputAction::None);
    }
    None
}

pub fn paint_overlays(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    tokens: &GlassTokens,
    clip: Rect,
) {
    if !wm.p3_mode {
        return;
    }
    let blur = !wm.low_overhead_glass && tokens.mode != ThemeMode::HighContrast;
    if wm.glass.launcher_open {
        paint_launcher(comp, wm, tokens, blur, clip);
    } else if wm.glass.qs_open {
        paint_quick_settings(comp, wm, tokens, blur, clip);
    } else if wm.glass.notif_open {
        paint_notif_center(comp, wm, tokens, blur, clip);
    }
}

fn paint_launcher(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    tokens: &GlassTokens,
    blur: bool,
    clip: Rect,
) {
    let r = launcher_modal_rect(wm);
    let ic = r.intersect(clip).clamp_to(comp.w, comp.h);
    if ic.is_empty() {
        return;
    }
    unsafe {
        backdrop_frosted_rect(
            comp.back_ptr, comp.w, comp.h, &comp.fb_info, r, ic,
            tokens.overlay_tint_rgb, tokens.overlay_alpha, blur, 10,
            tokens.border_focus, tokens.glass_rim_soft,
        );
    }
    let sb = Rect::new(r.x + 14, r.y + 14, r.w - 28, 34);
    unsafe {
        fill_rounded_raw(comp.back_ptr, comp.w, comp.h, sb, ic, 6, tokens.card_bg);
        draw_icon16(comp.back_ptr, comp.w, comp.h, sb.x + 10, sb.y + 9, &ICON_SEARCH, tokens.fg_primary, tokens.accent, ic);
    }
    let q = &wm.glass.launcher_query[..wm.glass.launcher_len as usize];
    let mut qlb = LineBuilder::new();
    if q.is_empty() {
        qlb.push_bytes(b"Type app name... (Esc close)");
    } else {
        qlb.push_bytes(q);
        qlb.push_byte(b'_');
    }
    unsafe {
        draw_str_raw(
            comp.back_ptr, comp.w, comp.h, sb.x + 34, sb.y + 9,
            qlb.as_bytes(), if q.is_empty() { tokens.fg_muted } else { tokens.fg_primary }, None, ic,
        );
    }
    let mut apps = [AppId::Terminal; APP_COUNT];
    let n = filter_launcher_apps(wm, &mut apps);
    for (row, &app) in apps.iter().take(n).enumerate() {
        let rr = launcher_app_rect(wm, row);
        let sel = row == wm.glass.launcher_sel.min(n - 1);
        let bg = if sel { tokens.pill_active } else { tokens.card_alt };
        unsafe {
            fill_rounded_raw(comp.back_ptr, comp.w, comp.h, rr, ic, 6, bg);
            draw_icon16(comp.back_ptr, comp.w, comp.h, rr.x + 10, rr.y + 10, icon_for_app(app), tokens.fg_primary, tokens.accent, ic);
            draw_str_raw(comp.back_ptr, comp.w, comp.h, rr.x + 34, rr.y + 10, app.display_title(), tokens.fg_primary, None, ic);
        }
        let running = (wm.running_apps_mask & (1u8 << (app as usize))) != 0;
        let tag: &[u8] = if running { b"RUNNING" } else { b"LAUNCH" };
        unsafe {
            draw_str_raw(
                comp.back_ptr, comp.w, comp.h, rr.x + rr.w - tag.len() * 8 - 12, rr.y + 10,
                tag, if running { tokens.ok } else { tokens.accent }, None, ic,
            );
        }
    }
}

fn paint_quick_settings(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    tokens: &GlassTokens,
    blur: bool,
    clip: Rect,
) {
    let r = qs_panel_rect(wm);
    let ic = r.intersect(clip).clamp_to(comp.w, comp.h);
    if ic.is_empty() {
        return;
    }
    unsafe {
        backdrop_frosted_rect(
            comp.back_ptr, comp.w, comp.h, &comp.fb_info, r, ic,
            tokens.overlay_tint_rgb, tokens.overlay_alpha, blur, 10,
            tokens.glass_rim, tokens.glass_rim_soft,
        );
        draw_str_raw(comp.back_ptr, comp.w, comp.h, r.x + 16, r.y + 12, b"QUICK SETTINGS", tokens.accent, None, ic);
    }
    let rows: [&[u8]; 6] = [
        if wm.light_mode { b"[1] Theme   : Frosted Daylight" } else { b"[1] Theme   : Obsidian Dark" },
        if wm.low_overhead_glass { b"[2] Material: Low-Overhead" } else { b"[2] Material: Frosted Blur" },
        if wm.high_contrast { b"[3] Contrast: High-Contrast" } else { b"[3] Contrast: Standard AA" },
        if wm.cursor_large { b"[4] Pointer : 2x Large" } else { b"[4] Pointer : 1x Standard" },
        match wm.layout {
            LayoutMode::Floating => b"[5] Layout  : Floating",
            LayoutMode::SideBySideTiled => b"[5] Layout  : Side-by-Side",
            LayoutMode::Stacked => b"[5] Layout  : Stacked",
            LayoutMode::Tabbed => b"[5] Layout  : Tabbed",
        },
        b"[6] Action  : Lock Session Shield",
    ];
    for (i, lbl) in rows.iter().enumerate() {
        let rr = qs_row_rect(wm, i);
        unsafe {
            fill_rounded_raw(comp.back_ptr, comp.w, comp.h, rr, ic, 6, tokens.card_alt);
            draw_str_raw(comp.back_ptr, comp.w, comp.h, rr.x + 10, rr.y + 8, lbl, tokens.fg_primary, None, ic);
        }
    }
    let mut flb = LineBuilder::new();
    flb.push_bytes(if wm.gpu_ep_cap != 0 { b"Scanout: VirtIO-GPU P=" } else { b"Scanout: UEFI GOP P=" });
    flb.push_u64(wm.gpu_presents);
    flb.push_bytes(b" F=");
    flb.push_u64(wm.gpu_fallbacks);
    unsafe {
        draw_str_raw(
            comp.back_ptr, comp.w, comp.h, r.x + 16, r.y + r.h - 26,
            flb.as_bytes(), if wm.gpu_ep_cap != 0 { tokens.ok } else { tokens.warn }, None, ic,
        );
    }
}

fn paint_notif_center(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    tokens: &GlassTokens,
    blur: bool,
    clip: Rect,
) {
    let r = notif_panel_rect(wm);
    let ic = r.intersect(clip).clamp_to(comp.w, comp.h);
    if ic.is_empty() {
        return;
    }
    unsafe {
        backdrop_frosted_rect(
            comp.back_ptr, comp.w, comp.h, &comp.fb_info, r, ic,
            tokens.overlay_tint_rgb, tokens.overlay_alpha, blur, 10,
            tokens.glass_rim, tokens.glass_rim_soft,
        );
        draw_icon16(comp.back_ptr, comp.w, comp.h, r.x + 14, r.y + 12, &ICON_BELL, tokens.fg_primary, tokens.accent, ic);
        draw_str_raw(comp.back_ptr, comp.w, comp.h, r.x + 34, r.y + 12, b"NOTIFICATIONS", tokens.fg_primary, None, ic);
        let cr = notif_clear_rect(wm);
        fill_rounded_raw(comp.back_ptr, comp.w, comp.h, cr, ic, 5, tokens.pill_bg);
        draw_str_raw(comp.back_ptr, comp.w, comp.h, cr.x + 9, cr.y + 3, b"Clear", tokens.accent, None, ic);
    }
    if wm.glass.notif_len == 0 {
        unsafe {
            draw_str_raw(comp.back_ptr, comp.w, comp.h, r.x + 16, r.y + 52, b"No live notifications recorded.", tokens.fg_muted, None, ic);
        }
        return;
    }
    for i in 0..wm.glass.notif_len {
        let idx = wm.glass.notif_len - 1 - i;
        let n = &wm.glass.notifs[idx];
        let nr = Rect::new(r.x + 12, r.y + 42 + i * 33, r.w - 24, 27);
        let fg = match n.kind {
            1 => tokens.ok,
            2 => tokens.warn,
            _ => tokens.fg_primary,
        };
        unsafe {
            fill_rounded_raw(comp.back_ptr, comp.w, comp.h, nr, ic, 6, tokens.card_alt);
            draw_str_raw(comp.back_ptr, comp.w, comp.h, nr.x + 8, nr.y + 5, &n.msg[..n.len as usize], fg, None, ic);
        }
    }
}

pub fn paint_lock_shield_glass(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    tokens: &GlassTokens,
    clip: Rect,
) {
    comp.fill_back_clipped(Rect::new(0, 0, comp.w, comp.h), clip, comp.pal.bg_shadow);
    let (cw, ch) = (640usize, 224usize);
    let cx = comp.w.saturating_sub(cw) / 2;
    let cy = comp.h.saturating_sub(ch) / 2;
    let card = Rect::new(cx, cy, cw, ch);
    let cclip = card.intersect(clip).clamp_to(comp.w, comp.h);
    if cclip.is_empty() {
        return;
    }
    unsafe {
        fill_rounded_raw(comp.back_ptr, comp.w, comp.h, card, cclip, 8, tokens.border_focus);
        fill_rounded_raw(comp.back_ptr, comp.w, comp.h, Rect::new(cx + 2, cy + 2, cw - 4, ch - 4), cclip, 6, comp.pal.bg_win);
        fill_rounded_raw(comp.back_ptr, comp.w, comp.h, Rect::new(cx + 2, cy + 2, cw - 4, 32), cclip, 6, tokens.pill_active);
        draw_str_raw(comp.back_ptr, comp.w, comp.h, cx + 18, cy + 10, b"DRY OS // SESSION LOCK SHIELD [CPL=3 COMPOSITOR GATE]", tokens.accent, None, cclip);
        draw_str_raw(comp.back_ptr, comp.w, comp.h, cx + 18, cy + 48, b"STATUS : INPUT ROUTING BLOCKED TO ALL USER COMPARTMENTS", tokens.ok, None, cclip);
    }
    if wm.vault_auth_required {
        let mut plb = LineBuilder::new();
        plb.push_bytes(b"Passphrase [ ");
        for _ in 0..(wm.lock_pin_len as usize).min(24) {
            plb.push_byte(b'*');
        }
        plb.push_bytes(b"_ ] (Type + Enter)");
        let mut flb = LineBuilder::new();
        flb.push_bytes(b"Auth failures: ");
        flb.push_u64(wm.vault_auth_failures as u64);
        flb.push_bytes(b" | Blocked inputs: ");
        flb.push_u64(wm.locked_blocked_events);
        unsafe {
            draw_str_raw(comp.back_ptr, comp.w, comp.h, cx + 18, cy + 70, b"AUTH   : DRY VAULT PASSPHRASE REQUIRED (PBKDF2-SHA256)", tokens.accent, None, cclip);
            draw_str_raw(comp.back_ptr, comp.w, comp.h, cx + 18, cy + 96, plb.as_bytes(), tokens.fg_primary, None, cclip);
            draw_str_raw(comp.back_ptr, comp.w, comp.h, cx + 18, cy + 124, flb.as_bytes(), if wm.vault_auth_failures > 0 { tokens.warn } else { tokens.fg_muted }, None, cclip);
        }
    } else {
        let mut blb = LineBuilder::new();
        blb.push_bytes(b"Blocked key/mouse inputs while locked: ");
        blb.push_u64(wm.locked_blocked_events);
        unsafe {
            draw_str_raw(comp.back_ptr, comp.w, comp.h, cx + 18, cy + 72, b"AUTH   : UNAUTHENTICATED LOCK SHIELD (DRY Vault Auth PLANNED)", tokens.warn, None, cclip);
            draw_str_raw(comp.back_ptr, comp.w, comp.h, cx + 18, cy + 98, b"Active Ring 3 apps continue executing isolated underneath.", tokens.fg_muted, None, cclip);
            draw_str_raw(comp.back_ptr, comp.w, comp.h, cx + 18, cy + 124, blb.as_bytes(), tokens.fg_primary, None, cclip);
        }
    }
    let btn = wm.unlock_button_rect();
    let lbl: &[u8] = if wm.vault_auth_required {
        b"[ AUTHENTICATE & UNLOCK (Enter) ]"
    } else {
        b"[ UNLOCK SESSION (Enter/Click) ]"
    };
    unsafe {
        fill_rounded_raw(comp.back_ptr, comp.w, comp.h, btn, cclip, 6, tokens.pill_active);
        draw_str_raw(comp.back_ptr, comp.w, comp.h, btn.x + 16, btn.y + 6, lbl, tokens.accent, None, cclip);
    }
}
