// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! DRY Glass Overlays — Spotlight Launcher, Quick Settings, Notifications & Lock Shield (`DRY Glass 2.0`).

use crate::action::{AppId, DesktopAction, APP_COUNT};
use crate::compositor::DesktopCompositor;
use crate::gfx::{DirtyRegion, Rect};
use crate::glass::blur::{backdrop_frosted_rect, blend_soft_rrect_shadow};
use crate::glass::theme::{GlassTokens, ThemeMode};
use crate::glass::{DOCK_H, DOCK_MARGIN_B};
use crate::input::{InputEvent, InputEventKind, MOUSE_BTN_LEFT};
use crate::keymap::{KEY_BACKSPACE, KEY_DOWN, KEY_ENTER, KEY_ESC, KEY_UP};
use crate::shell::LineBuilder;
use crate::ttf::{FontId, PixelFormat};
use crate::ui::canvas::Canvas;
use crate::ui::icons::{draw_vector_icon, IconKind};
use crate::ui::theme::UiTheme;
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
    Rect::new(dr.x + 12 + idx * (DOCK_ITEM_W + DOCK_ITEM_GAP), dr.y + 4, DOCK_ITEM_W, DOCK_ITEM_W)
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
    Rect::new(wm.screen_w.saturating_sub(w + 10), TOPBAR_H + 8, w, 348)
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

#[inline]
pub(crate) fn contains_ascii_icase(haystack: &[u8], needle: &[u8]) -> bool {
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

pub(crate) fn log_glass_toggle(kind: &[u8], open: bool) {
    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-GLASS] ");
    lb.push_bytes(kind);
    lb.push_bytes(b" open=");
    lb.push_u64(u64::from(open));
    wm_log_line(&lb);
}

#[inline]
pub(crate) fn pad_overlay(r: Rect) -> Rect {
    Rect::new(r.x.saturating_sub(8), r.y.saturating_sub(6), r.w + 16, r.h + 16)
}

pub fn toggle_launcher(wm: &mut WindowManager, dirty: &mut DirtyRegion) {
    let next = !wm.glass.launcher_open;
    if wm.glass.overview_open {
        dirty.mark(pad_overlay(crate::glass::overview::overview_modal_rect(wm)));
        log_glass_toggle(b"OVERVIEW", false);
    }
    if wm.glass.qs_open {
        dirty.mark(pad_overlay(qs_panel_rect(wm)));
    }
    if wm.glass.notif_open {
        dirty.mark(pad_overlay(notif_panel_rect(wm)));
    }
    wm.glass.close_overlays();
    wm.glass.launcher_open = next;
    dirty.mark(Rect::new(0, 0, wm.screen_w, TOPBAR_H));
    dirty.mark(pad_overlay(launcher_modal_rect(wm)));
    log_glass_toggle(b"LAUNCHER", next);
}

pub fn close_all_overlays(wm: &mut WindowManager, dirty: &mut DirtyRegion) {
    if wm.glass.overview_open {
        dirty.mark(pad_overlay(crate::glass::overview::overview_modal_rect(wm)));
        log_glass_toggle(b"OVERVIEW", false);
    }

    if wm.glass.launcher_open {
        dirty.mark(pad_overlay(launcher_modal_rect(wm)));
        log_glass_toggle(b"LAUNCHER", false);
    }
    if wm.glass.qs_open {
        dirty.mark(pad_overlay(qs_panel_rect(wm)));
        log_glass_toggle(b"QS", false);
    }
    if wm.glass.notif_open {
        dirty.mark(pad_overlay(notif_panel_rect(wm)));
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
        if let Some(hit) = crate::glass::window_strip::hit_test_adaptive_strip(wm, wm.cursor_x, wm.cursor_y) {
            return Some(match hit {
                crate::glass::window_strip::StripHit::ToggleOverview => {
                    crate::glass::overview::toggle_overview(wm, dirty);
                    WmInputAction::None
                }
                crate::glass::window_strip::StripHit::App(app) => {
                    close_all_overlays(wm, dirty);
                    wm.enqueue_desktop_action(DesktopAction::LaunchOrFocusApp(app))
                }
            });
        }
        if topbar_qs_pill_rect(wm).contains(wm.cursor_x, wm.cursor_y) {
            let next = !wm.glass.qs_open;
            close_all_overlays(wm, dirty);
            wm.glass.qs_open = next;
            dirty.mark(pad_overlay(qs_panel_rect(wm)));
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
            dirty.mark(pad_overlay(notif_panel_rect(wm)));
            dirty.mark(Rect::new(0, 0, wm.screen_w, TOPBAR_H));
            log_glass_toggle(b"NOTIF", next);
            return Some(WmInputAction::None);
        }
    }

    if wm.glass.overview_open {
        if let Some(act) = crate::glass::overview::handle_overview_click(wm, dirty) {
            return Some(act);
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
            let cur_flags = wm.theme_flags_byte();
            for row in 0..7usize {
                if qs_row_rect(wm, row).contains(wm.cursor_x, wm.cursor_y) {
                    dirty.mark(pad_overlay(qr));
                    let act = match row {
                        0 => DesktopAction::SetThemeContrast(cur_flags ^ 4),
                        1 => DesktopAction::SetThemeContrast(cur_flags ^ 8),
                        2 => DesktopAction::SetThemeContrast(cur_flags ^ 1),
                        3 => DesktopAction::SetThemeContrast(cur_flags ^ 2),
                        4 => DesktopAction::SetLayoutMode(wm.layout.next_mode() as u8),
                        5 => {
                            let next_dpi = (wm.dpi_scale + 1) % 3;
                            DesktopAction::SetThemeContrast((cur_flags & 0x0f) | (next_dpi << 4))
                        }
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
                dirty.mark(pad_overlay(nr));
                dirty.mark(Rect::new(0, 0, wm.screen_w, TOPBAR_H));
            }
            return Some(WmInputAction::None);
        }
        close_all_overlays(wm, dirty);
    }

    if dock_rect(wm).contains(wm.cursor_x, wm.cursor_y) {
        for idx in 0..APP_COUNT {
            if dock_item_rect(wm, idx).contains(wm.cursor_x, wm.cursor_y) {
                return Some(wm.enqueue_desktop_action(DesktopAction::LaunchOrFocusApp(AppId::ALL[idx])));
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
    if wm.glass.overview_open {
        return crate::glass::overview::handle_overview_key(wm, ev, dirty);
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
    let ui = UiTheme::new(tokens.mode, wm.dpi_pct());
    let fmt = PixelFormat::from_fb(&comp.fb_info);

    if wm.glass.overview_open {
        crate::glass::overview::paint_overview(comp, wm, tokens, clip, blur);
    }
    if wm.glass.launcher_open {
        paint_launcher(comp, wm, tokens, &ui, fmt, clip, blur);
    }
    if wm.glass.qs_open {
        paint_qs(comp, wm, tokens, &ui, fmt, clip, blur);
    }
    if wm.glass.notif_open {
        paint_notifs(comp, wm, tokens, &ui, fmt, clip, blur);
    }
}

pub(crate) fn paint_frosted_panel(
    comp: &mut DesktopCompositor,
    tokens: &GlassTokens,
    ui: &UiTheme,
    fmt: PixelFormat,
    rect: Rect,
    clip: Rect,
    blur: bool,
) -> Option<Canvas> {
    let sc = Rect::new(rect.x.saturating_sub(6), rect.y.saturating_sub(4), rect.w + 12, rect.h + 12)
        .intersect(clip)
        .clamp_to(comp.w, comp.h);
    if sc.is_empty() {
        return None;
    }
    if tokens.mode != ThemeMode::HighContrast {
        unsafe {
            blend_soft_rrect_shadow(
                comp.back_ptr, comp.w, comp.h, &comp.fb_info, rect, sc,
                12, tokens.shadow_rgb, 96, 6,
            );
        }
    }
    let ic = rect.intersect(sc);
    if !ic.is_empty() {
        unsafe {
            backdrop_frosted_rect(
                comp.back_ptr, comp.w, comp.h, &comp.fb_info, rect, ic,
                tokens.overlay_tint_rgb, tokens.overlay_alpha, blur, 12,
                tokens.glass_rim, tokens.glass_rim_soft,
            );
        }
    }
    let mut c = unsafe { Canvas::from_raw(comp.back_ptr, comp.w, comp.h, comp.fb_stride, fmt, sc) };
    let bd = if tokens.mode == ThemeMode::HighContrast { ui.accent } else { ui.border_strong };
    c.stroke_rrect_aa(rect, 12, bd, 235);
    Some(c)
}

fn paint_launcher(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    tokens: &GlassTokens,
    ui: &UiTheme,
    fmt: PixelFormat,
    clip: Rect,
    blur: bool,
) {
    let r = launcher_modal_rect(wm);
    let Some(mut c) = paint_frosted_panel(comp, tokens, ui, fmt, r, clip, blur) else { return };
    let sb = Rect::new(r.x + 14, r.y + 14, r.w - 28, 36);
    c.fill_bordered_rrect_aa(sb, 8, ui.bg_input, ui.accent);
    draw_vector_icon(&mut c, Rect::new(sb.x + 10, sb.y + 10, 16, 16), IconKind::Search, ui.accent, ui.accent);

    if wm.glass.launcher_len == 0 {
        c.draw_text((sb.x + 34) as i32, (sb.y + 9) as i32, FontId::SansRegular, 13, b"Search DRY OS applications...", ui.fg_muted);
    } else {
        c.draw_text((sb.x + 34) as i32, (sb.y + 9) as i32, FontId::SansSemiBold, 13, &wm.glass.launcher_query[..wm.glass.launcher_len as usize], ui.fg_primary);
    }

    let mut apps = [AppId::Terminal; APP_COUNT];
    let n = filter_launcher_apps(wm, &mut apps);
    if n == 0 {
        c.draw_text((r.x + 22) as i32, (r.y + 74) as i32, FontId::SansRegular, 13, b"No matching applications found.", ui.fg_muted);
        return;
    }

    for (row, &app) in apps.iter().take(n).enumerate() {
        let rr = launcher_app_rect(wm, row);
        let sel = row == wm.glass.launcher_sel.min(n - 1);
        c.fill_bordered_rrect_aa(rr, 8, if sel { ui.accent_subtle } else { ui.bg_card_alt }, if sel { ui.accent } else { ui.border_subtle });
        draw_vector_icon(
            &mut c, Rect::new(rr.x + 10, rr.y + 8, 20, 20), IconKind::from_app(app),
            if sel { ui.accent_fg } else { ui.fg_primary }, ui.accent,
        );
        c.draw_text(
            (rr.x + 38) as i32, (rr.y + 9) as i32,
            if sel { FontId::SansSemiBold } else { FontId::SansRegular },
            13, app.display_title(), ui.fg_primary,
        );
        let running = (wm.running_apps_mask & (1u8 << (app as usize))) != 0;
        let tag: &[u8] = if running { b"Running" } else { b"Open" };
        let tw = if running { 60 } else { 44 };
        let tr = Rect::new(rr.x + rr.w.saturating_sub(tw + 10), rr.y + 7, tw, 22);
        c.fill_rrect_aa(tr, 6, if running { ui.ok_bg } else { ui.bg_card }, 255);
        c.draw_text_centered(tr, FontId::SansSemiBold, 11, tag, if running { ui.ok_fg } else { ui.fg_muted });
    }
}

fn paint_qs(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    tokens: &GlassTokens,
    ui: &UiTheme,
    fmt: PixelFormat,
    clip: Rect,
    blur: bool,
) {
    let r = qs_panel_rect(wm);
    let Some(mut c) = paint_frosted_panel(comp, tokens, ui, fmt, r, clip, blur) else { return };
    draw_vector_icon(&mut c, Rect::new(r.x + 14, r.y + 12, 16, 16), IconKind::Sliders, ui.accent, ui.accent);
    c.draw_text((r.x + 36) as i32, (r.y + 11) as i32, FontId::SansSemiBold, 14, b"Quick Settings", ui.fg_primary);

    let mode_val: &[u8] = match tokens.mode {
        ThemeMode::FrostedDaylight => b"Daylight",
        ThemeMode::HighContrast => b"Contrast",
        ThemeMode::ObsidianDark => b"Obsidian",
    };
    let blur_val: &[u8] = if wm.low_overhead_glass { b"Off" } else { b"Frosted" };
    let hc_val: &[u8] = if wm.high_contrast { b"On" } else { b"Off" };
    let cur_val: &[u8] = if wm.cursor_large { b"Large" } else { b"Standard" };
    let lay_val: &[u8] = match wm.layout {
        LayoutMode::Floating => b"Floating",
        LayoutMode::SideBySideTiled => b"Tiled",
        LayoutMode::Stacked => b"Stacked",
        LayoutMode::Tabbed => b"Tabbed",
    };
    let dpi_val: &[u8] = match wm.dpi_scale {
        1 => b"125%",
        2 => b"150%",
        _ => b"100%",
    };
    let rows: [(IconKind, &[u8], &[u8], bool); 7] = [
        (IconKind::Sun, b"Appearance", mode_val, wm.light_mode),
        (IconKind::Sliders, b"Glass Blur", blur_val, !wm.low_overhead_glass),
        (IconKind::Shield, b"High Contrast", hc_val, wm.high_contrast),
        (IconKind::Info, b"Pointer Size", cur_val, wm.cursor_large),
        (IconKind::LayoutTiled, b"Window Layout", lay_val, true),
        (IconKind::Refresh, b"Display Scale", dpi_val, wm.dpi_scale != 0),
        (IconKind::Lock, b"Lock Session", b"Shield", false),
    ];

    for (row, &(icon, lbl, val, active)) in rows.iter().enumerate() {
        let rr = qs_row_rect(wm, row);
        c.fill_bordered_rrect_aa(rr, 8, ui.bg_card_alt, ui.border_subtle);
        draw_vector_icon(&mut c, Rect::new(rr.x + 10, rr.y + 8, 16, 16), icon, if active { ui.accent } else { ui.fg_secondary }, ui.accent);
        c.draw_text((rr.x + 32) as i32, (rr.y + 7) as i32, FontId::SansRegular, 12, lbl, ui.fg_primary);
        let pill = Rect::new(rr.x + rr.w.saturating_sub(92), rr.y + 5, 84, 22);
        c.fill_rrect_aa(pill, 6, if active { ui.accent_subtle } else { ui.bg_card }, 255);
        c.draw_text_centered(pill, FontId::SansSemiBold, 11, val, if active { ui.accent_fg } else { ui.fg_muted });
    }

    let foot: &[u8] = if wm.gpu_ep_cap != 0 { b"Backend: VirtIO-GPU 2D Active" } else { b"Backend: UEFI GOP Fallback" };
    let foot_col = if wm.gpu_ep_cap != 0 { ui.ok } else { ui.warn };
    c.draw_text((r.x + 16) as i32, r.y.saturating_add(r.h.saturating_sub(24)) as i32, FontId::SansSemiBold, 11, foot, foot_col);
}

fn paint_notifs(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    tokens: &GlassTokens,
    ui: &UiTheme,
    fmt: PixelFormat,
    clip: Rect,
    blur: bool,
) {
    let r = notif_panel_rect(wm);
    let Some(mut c) = paint_frosted_panel(comp, tokens, ui, fmt, r, clip, blur) else { return };
    draw_vector_icon(&mut c, Rect::new(r.x + 14, r.y + 12, 16, 16), IconKind::Bell, ui.accent, ui.accent);
    c.draw_text((r.x + 36) as i32, (r.y + 11) as i32, FontId::SansSemiBold, 14, b"Notifications", ui.fg_primary);
    let cr = notif_clear_rect(wm);
    c.fill_bordered_rrect_aa(cr, 6, ui.bg_card_alt, ui.border_subtle);
    c.draw_text_centered(cr, FontId::SansSemiBold, 11, b"Clear", ui.accent_fg);

    if wm.glass.notif_len == 0 {
        let emp = Rect::new(r.x + 14, r.y + 56, r.w - 28, 74);
        c.fill_bordered_rrect_aa(emp, 8, ui.bg_card_alt, ui.border_subtle);
        c.draw_text_centered(emp, FontId::SansRegular, 12, b"All caught up - no notifications", ui.fg_muted);
        return;
    }
    let show = wm.glass.notif_len.min(4);
    for idx in 0..show {
        let n = &wm.glass.notifs[wm.glass.notif_len - 1 - idx];
        let nr = Rect::new(r.x + 14, r.y + 48 + idx * 48, r.w - 28, 42);
        c.fill_bordered_rrect_aa(nr, 8, ui.bg_card_alt, ui.border_subtle);
        let dot = match n.kind { 1 => ui.ok, 2 => ui.warn, _ => ui.accent };
        c.fill_circle_aa((nr.x + 12) as i32, (nr.y + 21) as i32, 4, dot, 255);
        c.draw_text_ellipsis(
            (nr.x + 24) as i32, (nr.y + 12) as i32, (nr.w.saturating_sub(34)) as i32,
            FontId::SansRegular, 12, &n.msg[..n.len as usize], ui.fg_primary,
        );
    }
}

pub fn paint_lock_shield_glass(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    tokens: &GlassTokens,
    clip: Rect,
) {
    let ui = UiTheme::new(tokens.mode, wm.dpi_pct());
    let fmt = PixelFormat::from_fb(&comp.fb_info);
    let full = clip.clamp_to(comp.w, comp.h);
    if full.is_empty() {
        return;
    }
    comp.paint_wallpaper_clipped(wm.p3_mode, full);
    let mut bgc = unsafe { Canvas::from_raw(comp.back_ptr, comp.w, comp.h, comp.fb_stride, fmt, full) };
    bgc.fill_rect_alpha(full, 0x050811, 145);

    let (cw, ch) = (560usize.min(comp.w.saturating_sub(24)), 236usize);
    let cx = comp.w.saturating_sub(cw) / 2;
    let cy = comp.h.saturating_sub(ch) / 2;
    let card = Rect::new(cx, cy, cw, ch);
    let cclip = Rect::new(card.x.saturating_sub(6), card.y.saturating_sub(4), card.w + 12, card.h + 12)
        .intersect(clip)
        .clamp_to(comp.w, comp.h);
    if cclip.is_empty() {
        return;
    }
    unsafe {
        blend_soft_rrect_shadow(comp.back_ptr, comp.w, comp.h, &comp.fb_info, card, cclip, 12, tokens.shadow_rgb, 110, 6);
    }
    let mut c = unsafe { Canvas::from_raw(comp.back_ptr, comp.w, comp.h, comp.fb_stride, fmt, cclip) };
    c.fill_bordered_rrect_aa(card, 12, ui.bg_card, ui.border_strong);

    let hdr = Rect::new(cx + 16, cy + 16, cw - 32, 32);
    c.fill_rrect_aa(hdr, 8, ui.bg_card_alt, 255);
    draw_vector_icon(&mut c, Rect::new(hdr.x + 10, hdr.y + 8, 16, 16), IconKind::Lock, ui.accent, ui.accent);
    c.draw_text((hdr.x + 34) as i32, (hdr.y + 7) as i32, FontId::SansSemiBold, 14, b"DRY OS Session Lock Shield", ui.fg_primary);
    c.draw_text((cx + 20) as i32, (cy + 60) as i32, FontId::SansRegular, 12, b"Input routing isolated to CPL=3 compositor gate.", ui.ok);

    if wm.vault_auth_required {
        c.draw_text((cx + 20) as i32, (cy + 82) as i32, FontId::SansSemiBold, 12, b"DRY Vault Passphrase (PBKDF2-SHA256):", ui.fg_secondary);
        let box_r = Rect::new(cx + 20, cy + 104, cw - 40, 32);
        c.fill_bordered_rrect_aa(box_r, 8, ui.bg_input, ui.accent);
        let mut plb = LineBuilder::new();
        for _ in 0..(wm.lock_pin_len as usize).min(24) {
            plb.push_byte(b'*');
        }
        plb.push_byte(b'_');
        c.draw_text((box_r.x + 12) as i32, (box_r.y + 7) as i32, FontId::MonoSemiBold, 13, plb.as_bytes(), ui.fg_primary);

        let mut flb = LineBuilder::new();
        flb.push_bytes(b"Auth failures: ");
        flb.push_u64(wm.vault_auth_failures as u64);
        flb.push_bytes(b"   Blocked inputs: ");
        flb.push_u64(wm.locked_blocked_events);
        c.draw_text(
            (cx + 20) as i32, (cy + 144) as i32, FontId::SansRegular, 11, flb.as_bytes(),
            if wm.vault_auth_failures > 0 { ui.warn } else { ui.fg_muted },
        );
    } else {
        c.draw_text((cx + 20) as i32, (cy + 86) as i32, FontId::SansRegular, 12, b"Ring 3 applications stay running safely underneath.", ui.fg_muted);
        let mut blb = LineBuilder::new();
        blb.push_bytes(b"Blocked inputs while locked: ");
        blb.push_u64(wm.locked_blocked_events);
        c.draw_text((cx + 20) as i32, (cy + 114) as i32, FontId::SansSemiBold, 12, blb.as_bytes(), ui.fg_primary);
    }

    let btn = wm.unlock_button_rect();
    c.fill_bordered_rrect_aa(btn, 8, ui.accent, ui.accent_hover);
    let lbl: &[u8] = if wm.vault_auth_required {
        b"Authenticate & Unlock (Enter)"
    } else {
        b"Unlock Session (Enter / Click)"
    };
    c.draw_text_centered(btn, FontId::SansSemiBold, 12, lbl, ui.fg_on_accent);
}
