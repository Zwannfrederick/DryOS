// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Adaptive Topbar Window Strip (`Ascension 4.0 — Track A`).
//!
//! Replaces the crowded per-app tab strip with a compact, strictly width-bounded strip
//! (`<= 35%` of `screen_w`, single `TOPBAR_H = 28` row, zero wrapping or horizontal scrolling):
//! - Focused application's vector icon, readable name, and multi-window group badge
//! - Limited MRU recent-app icons dynamically selected for available width
//! - Compact `+N` overflow badge opening the searchable Window Overview

use crate::action::{AppId, APP_COUNT};
use crate::compositor::{write_u32_ascii, DesktopCompositor};
use crate::gfx::Rect;
use crate::glass::overlays::topbar_brand_pill_rect;
use crate::glass::theme::GlassTokens;
use crate::ttf::{measure_text, FontId, PixelFormat};
use crate::ui::canvas::Canvas;
use crate::ui::icons::{draw_vector_icon, IconKind};
use crate::ui::theme::UiTheme;
use crate::wm::{WindowManager, MAX_WINDOWS, TOPBAR_H};

pub const MAX_STRIP_SLOTS: usize = 8;
pub const MAX_RECENT_ICONS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StripWinSummary {
    pub win_id: u32,
    pub app: AppId,
    pub visible: bool,
    pub minimized: bool,
    pub focused: bool,
}

impl StripWinSummary {
    pub const EMPTY: Self = Self {
        win_id: 0,
        app: AppId::Terminal,
        visible: false,
        minimized: false,
        focused: false,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripHit {
    ToggleOverview,
    App(AppId),
}

#[derive(Clone, Copy, Debug)]
pub struct StripSlotLayout {
    pub rect: Rect,
    pub hit: StripHit,
    pub app: Option<AppId>,
    pub win_count: u16,
    pub min_count: u16,
    pub is_focused: bool,
    pub is_running: bool,
    pub is_overflow: bool,
    pub overflow_count: u16,
}

impl StripSlotLayout {
    pub const EMPTY: Self = Self {
        rect: Rect::EMPTY,
        hit: StripHit::ToggleOverview,
        app: None,
        win_count: 0,
        min_count: 0,
        is_focused: false,
        is_running: false,
        is_overflow: false,
        overflow_count: 0,
    };
}

pub struct AdaptiveStripLayout {
    pub bounds: Rect,
    pub slots: [StripSlotLayout; MAX_STRIP_SLOTS],
    pub slot_count: usize,
    pub focused_app: Option<AppId>,
    pub total_windows: usize,
    pub overflow_count: usize,
}

#[inline]
pub const fn app_strip_name(app: AppId, compact: bool) -> &'static [u8] {
    if compact {
        match app {
            AppId::Terminal => b"Term",
            AppId::LifeStation => b"Life",
            AppId::Files => b"Files",
            AppId::Settings => b"Set",
            AppId::Editor => b"Notes",
            AppId::Calculator => b"Calc",
        }
    } else {
        match app {
            AppId::Terminal => b"Terminal",
            AppId::LifeStation => b"Life Station",
            AppId::Files => b"Files",
            AppId::Settings => b"Settings",
            AppId::Editor => b"Notes",
            AppId::Calculator => b"Calculator",
        }
    }
}

/// Snapshot current live windows from `WindowManager` for unified strip computation.
pub fn snapshot_wm_strip_wins(wm: &WindowManager, out: &mut [StripWinSummary; MAX_WINDOWS]) -> usize {
    let mut n = 0usize;
    for w in &wm.windows {
        if w.allocated {
            if let Some(app) = WindowManager::slot_app_id(w) {
                out[n] = StripWinSummary {
                    win_id: w.id,
                    app,
                    visible: w.visible,
                    minimized: w.minimized || !w.visible,
                    focused: w.focused,
                };
                n += 1;
            }
        }
    }
    n
}

/// Pure, allocation-free Adaptive Window Strip layout calculator shared by production
/// rendering/hit-testing and `1..50` window scalability self-tests.
pub fn compute_adaptive_strip_layout(
    screen_w: usize,
    brand_r: Rect,
    mode_pill_x: usize,
    mru_apps: &[AppId],
    wins: &[StripWinSummary],
) -> AdaptiveStripLayout {
    let strip_x = brand_r.x + brand_r.w + 6;
    let budget_35 = (screen_w * 35) / 100;
    let max_before_mode = mode_pill_x.saturating_sub(strip_x + 8);
    let max_w = budget_35.min(max_before_mode);

    let mut counts = [0u16; APP_COUNT];
    let mut min_counts = [0u16; APP_COUNT];
    let mut focused_app: Option<AppId> = None;

    for w in wins {
        let idx = w.app as usize;
        if idx < APP_COUNT {
            counts[idx] = counts[idx].saturating_add(1);
            if w.minimized || !w.visible {
                min_counts[idx] = min_counts[idx].saturating_add(1);
            }
            if w.focused && w.visible {
                focused_app = Some(w.app);
            }
        }
    }

    let total_windows = wins.len();
    let mut slots = [StripSlotLayout::EMPTY; MAX_STRIP_SLOTS];
    let mut slot_count = 0usize;

    let focus_w = if screen_w < 1200 { 94usize } else { 102usize }.min(max_w);
    let mut cx = strip_x;
    let mut shown_running_wins = 0usize;

    if focus_w >= 48 {
        let f_cnt = focused_app.map_or(0, |a| counts[a as usize]);
        let f_min = focused_app.map_or(0, |a| min_counts[a as usize]);
        if f_cnt > 0 {
            shown_running_wins += 1;
        }
        slots[0] = StripSlotLayout {
            rect: Rect::new(cx, 4, focus_w, 20),
            hit: StripHit::ToggleOverview,
            app: focused_app,
            win_count: f_cnt,
            min_count: f_min,
            is_focused: true,
            is_running: f_cnt > 0,
            is_overflow: false,
            overflow_count: 0,
        };
        slot_count = 1;
        cx += focus_w + 4;
    }

    // Order recent-app candidates: open non-focused apps in MRU order first,
    // then remaining open apps, then recently closed MRU apps for 1-click relaunch.
    let mut candidates = [AppId::Terminal; APP_COUNT];
    let mut cand_running = [false; APP_COUNT];
    let mut cand_n = 0usize;
    let mut used = [false; APP_COUNT];
    if let Some(fa) = focused_app {
        used[fa as usize] = true;
    }

    for &a in mru_apps {
        let ai = a as usize;
        if ai < APP_COUNT && !used[ai] && counts[ai] > 0 {
            used[ai] = true;
            candidates[cand_n] = a;
            cand_running[cand_n] = true;
            cand_n += 1;
        }
    }
    for a in AppId::ALL {
        let ai = a as usize;
        if !used[ai] && counts[ai] > 0 {
            used[ai] = true;
            candidates[cand_n] = a;
            cand_running[cand_n] = true;
            cand_n += 1;
        }
    }
    for &a in mru_apps {
        let ai = a as usize;
        if ai < APP_COUNT && !used[ai] {
            used[ai] = true;
            candidates[cand_n] = a;
            cand_running[cand_n] = false;
            cand_n += 1;
        }
    }

    let overflow_reserve = if total_windows > 2 { 44usize } else { 0usize };
    let max_end_x = strip_x + max_w;

    for (placed_recent, i) in (0..cand_n).enumerate() {
        if placed_recent >= MAX_RECENT_ICONS || slot_count + 1 >= MAX_STRIP_SLOTS {
            break;
        }
        let app = candidates[i];
        let ai = app as usize;
        let cnt = counts[ai];
        let is_run = cand_running[i];
        let pill_w = if cnt > 1 { 34usize } else { 26usize };
        let reserve = if total_windows > shown_running_wins + usize::from(is_run) {
            overflow_reserve
        } else {
            0
        };
        if cx + pill_w + reserve > max_end_x {
            break;
        }
        slots[slot_count] = StripSlotLayout {
            rect: Rect::new(cx, 4, pill_w, 20),
            hit: StripHit::App(app),
            app: Some(app),
            win_count: cnt,
            min_count: min_counts[ai],
            is_focused: false,
            is_running: is_run,
            is_overflow: false,
            overflow_count: 0,
        };
        slot_count += 1;
        cx += pill_w + 4;
        if is_run {
            shown_running_wins += 1;
        }
    }

    let overflow_count = total_windows.saturating_sub(shown_running_wins);
    if overflow_count > 0 && slot_count < MAX_STRIP_SLOTS {
        let ov_w = if overflow_count >= 10 { 40usize } else { 34usize };
        if cx + ov_w <= max_end_x {
            slots[slot_count] = StripSlotLayout {
                rect: Rect::new(cx, 4, ov_w, 20),
                hit: StripHit::ToggleOverview,
                app: None,
                win_count: 0,
                min_count: 0,
                is_focused: false,
                is_running: false,
                is_overflow: true,
                overflow_count: overflow_count.min(u16::MAX as usize) as u16,
            };
            slot_count += 1;
            cx += ov_w;
        }
    }

    let used_w = cx.saturating_sub(strip_x).min(max_w);
    AdaptiveStripLayout {
        bounds: Rect::new(strip_x, 4, used_w, 20),
        slots,
        slot_count,
        focused_app,
        total_windows,
        overflow_count,
    }
}

#[inline(never)]
pub fn wm_strip_layout(wm: &WindowManager) -> AdaptiveStripLayout {
    let mut wins = [StripWinSummary::EMPTY; MAX_WINDOWS];
    let n = snapshot_wm_strip_wins(wm, &mut wins);
    let mru_len = (wm.mru_len as usize).min(APP_COUNT);
    compute_adaptive_strip_layout(
        wm.screen_w,
        topbar_brand_pill_rect(wm),
        wm.topbar_mode_pill_rect().x,
        &wm.mru_apps[..mru_len],
        &wins[..n],
    )
}

#[inline(never)]
pub fn hit_test_adaptive_strip(wm: &WindowManager, px: usize, py: usize) -> Option<StripHit> {
    if !wm.p3_mode || py >= TOPBAR_H {
        return None;
    }
    let layout = wm_strip_layout(wm);
    for slot in &layout.slots[..layout.slot_count] {
        if slot.rect.contains(px, py) {
            return Some(slot.hit);
        }
    }
    None
}

#[inline(never)]
pub fn paint_adaptive_window_strip(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    tokens: &GlassTokens,
    tb_clip: Rect,
) {
    let layout = wm_strip_layout(wm);
    let fmt = PixelFormat::from_fb(&comp.fb_info);
    let ui = UiTheme::new(tokens.mode, wm.dpi_pct());
    let mut c = unsafe { Canvas::from_raw(comp.back_ptr, comp.w, comp.h, comp.fb_stride, fmt, tb_clip) };

    for slot in &layout.slots[..layout.slot_count] {
        let r = slot.rect;
        if slot.is_focused {
            let active_ov = wm.glass.overview_open;
            let (bg, bd, fg) = if active_ov {
                (ui.accent, ui.accent, ui.fg_on_accent)
            } else if slot.is_running {
                (ui.accent_subtle, ui.accent, ui.fg_primary)
            } else {
                (ui.bg_card_alt, ui.border_subtle, ui.fg_muted)
            };
            c.fill_bordered_rrect_aa(r, 6, bg, bd);
            if let Some(app) = slot.app {
                draw_vector_icon(
                    &mut c,
                    Rect::new(r.x + 6, r.y + 4, 12, 12),
                    IconKind::from_app(app),
                    fg,
                    ui.accent,
                );
                let mut badge_buf = [0u8; 4];
                let badge_len = if slot.win_count > 1 {
                    write_u32_ascii(&mut badge_buf, slot.win_count as u32)
                } else {
                    0
                };
                let badge_w = if badge_len > 0 { 16usize } else { 0usize };
                let avail_lbl = r.w.saturating_sub(24 + badge_w);
                let full_lbl = app_strip_name(app, false);
                let lbl = if (measure_text(FontId::SansSemiBold, 11, full_lbl).max(0) as usize) <= avail_lbl {
                    full_lbl
                } else {
                    app_strip_name(app, true)
                };
                c.draw_text((r.x + 22) as i32, (r.y + 3) as i32, FontId::SansSemiBold, 11, lbl, fg);
                if badge_len > 0 {
                    let br = Rect::new(r.x + r.w.saturating_sub(18), r.y + 3, 14, 14);
                    c.fill_rrect_aa(br, 4, if active_ov { ui.bg_card } else { ui.accent }, 235);
                    c.draw_text_centered(
                        br,
                        FontId::SansSemiBold,
                        10,
                        &badge_buf[..badge_len],
                        if active_ov { ui.fg_primary } else { ui.fg_on_accent },
                    );
                }
            } else {
                draw_vector_icon(
                    &mut c,
                    Rect::new(r.x + 6, r.y + 4, 12, 12),
                    IconKind::LayoutTiled,
                    fg,
                    ui.accent,
                );
                c.draw_text((r.x + 22) as i32, (r.y + 3) as i32, FontId::SansSemiBold, 11, b"Overview", fg);
            }
            if !active_ov && slot.is_running {
                c.fill_rrect_aa(Rect::new(r.x + 8, r.y + r.h - 2, r.w.saturating_sub(16), 2), 1, ui.accent, 255);
            }
        } else if slot.is_overflow {
            let active_ov = wm.glass.overview_open;
            let (bg, bd, fg) = if active_ov {
                (ui.accent, ui.accent, ui.fg_on_accent)
            } else {
                (ui.bg_card_alt, ui.border_strong, ui.fg_primary)
            };
            c.fill_bordered_rrect_aa(r, 6, bg, bd);
            let mut obuf = [0u8; 6];
            obuf[0] = b'+';
            let n = write_u32_ascii(&mut obuf[1..], slot.overflow_count as u32);
            c.draw_text_centered(r, FontId::SansSemiBold, 10, &obuf[..1 + n], fg);
        } else if let Some(app) = slot.app {
            let (bg, bd, fg) = if slot.is_running {
                (ui.bg_card_alt, ui.border_strong, ui.fg_primary)
            } else {
                (ui.bg_card, ui.border_subtle, ui.fg_muted)
            };
            c.fill_bordered_rrect_aa(r, 6, bg, bd);
            let ix = if slot.win_count > 1 { r.x + 4 } else { r.x + (r.w.saturating_sub(12)) / 2 };
            draw_vector_icon(
                &mut c,
                Rect::new(ix, r.y + 4, 12, 12),
                IconKind::from_app(app),
                fg,
                ui.accent,
            );
            if slot.win_count > 1 {
                let mut cb = [0u8; 4];
                let cn = write_u32_ascii(&mut cb, slot.win_count as u32);
                c.draw_text(
                    (r.x + 19) as i32,
                    (r.y + 4) as i32,
                    FontId::SansSemiBold,
                    10,
                    &cb[..cn],
                    ui.accent_fg,
                );
            }
            if slot.is_running {
                let dot_col = if slot.min_count >= slot.win_count { ui.warn } else { ui.ok };
                c.fill_circle_aa((r.x + r.w / 2) as i32, (r.y + r.h - 2) as i32, 1, dot_col, 255);
            }
        }
    }
}
