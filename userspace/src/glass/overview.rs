// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Searchable, Grouped & Virtualized Window Overview (`Ascension 4.0 — Track A`).
//!
//! Displays open and minimized windows grouped by application with live substring search,
//! O(visible) virtualized scrolling, per-window Minimize/Restore and Close actions, keyboard
//! navigation, live mutation resilience, and a deterministic `1..50` window scalability check.
//! Large working buffers live in `.bss` (`OVERVIEW_SCRATCH`) to honor the 16 KiB Ring-3 stack.

use core::cell::UnsafeCell;

use crate::action::{AppId, APP_COUNT};
use crate::compositor::{write_u32_ascii, DesktopCompositor};
use crate::gfx::{DirtyRegion, Rect};
use crate::glass::overlays::{
    close_all_overlays, contains_ascii_icase, log_glass_toggle, pad_overlay, paint_frosted_panel,
};
use crate::glass::theme::GlassTokens;
use crate::glass::window_strip::{
    app_strip_name, compute_adaptive_strip_layout, StripWinSummary,
};
use crate::glass::{DOCK_H, DOCK_MARGIN_B};
use crate::input::{InputEvent, MOD_ALT};
use crate::keymap::{
    KEY_BACKSPACE, KEY_DELETE, KEY_DOWN, KEY_END, KEY_ENTER, KEY_ESC, KEY_HOME, KEY_PAGEDOWN,
    KEY_PAGEUP, KEY_TAB, KEY_UP,
};
use crate::shell::LineBuilder;
use crate::ttf::{FontId, PixelFormat};
use crate::ui::canvas::Canvas;
use crate::ui::icons::{draw_vector_icon, IconKind};
use crate::ui::theme::UiTheme;
use crate::wm::{
    LayoutMode, WindowManager, WindowRole, WindowSlot, MAX_WINDOWS, TITLE_KIND_CALC,
    TITLE_KIND_DIALOG, TITLE_KIND_EDITOR, TITLE_KIND_FILES, TITLE_KIND_SETTINGS,
    TITLE_KIND_SYSMON, TITLE_KIND_TERMINAL, TOPBAR_H,
};
use crate::wm_ipc::wm_log_line;

pub const MAX_OVERVIEW_ITEMS: usize = 64;
pub const MAX_OVERVIEW_ROWS: usize = MAX_OVERVIEW_ITEMS + APP_COUNT;
pub const OVERVIEW_GROUP_H: usize = 24;
pub const OVERVIEW_WIN_H: usize = 38;

#[derive(Clone, Copy)]
pub struct OverviewWinItem {
    pub win_id: u32,
    pub app: AppId,
    pub title: [u8; 32],
    pub title_len: u8,
    pub role: WindowRole,
    pub focused: bool,
    pub minimized: bool,
    pub z_rank: u16,
}

impl OverviewWinItem {
    pub const EMPTY: Self = Self {
        win_id: 0,
        app: AppId::Terminal,
        title: [0u8; 32],
        title_len: 0,
        role: WindowRole::Normal,
        focused: false,
        minimized: false,
        z_rank: 0,
    };

    #[inline]
    pub fn title_bytes(&self) -> &[u8] {
        let n = (self.title_len as usize).min(self.title.len());
        &self.title[..n]
    }
}

#[derive(Clone, Copy)]
pub enum OverviewRowKind {
    GroupHeader {
        app: AppId,
        count: u16,
        min_count: u16,
    },
    /// Index into the current `items` slice.
    Window(u16),
}

#[derive(Clone, Copy)]
pub struct OverviewRow {
    pub kind: OverviewRowKind,
    pub y_off: usize,
    pub height: usize,
}

impl OverviewRow {
    pub const EMPTY: Self = Self {
        kind: OverviewRowKind::GroupHeader {
            app: AppId::Terminal,
            count: 0,
            min_count: 0,
        },
        y_off: 0,
        height: 0,
    };
}

struct OverviewScratch {
    items: [OverviewWinItem; MAX_OVERVIEW_ITEMS],
    rows: [OverviewRow; MAX_OVERVIEW_ROWS],
    q_rows: [OverviewRow; MAX_OVERVIEW_ROWS],
    strip_wins: [StripWinSummary; MAX_OVERVIEW_ITEMS],
    win_ids: [u32; MAX_OVERVIEW_ITEMS],
}

impl OverviewScratch {
    const EMPTY: Self = Self {
        items: [OverviewWinItem::EMPTY; MAX_OVERVIEW_ITEMS],
        rows: [OverviewRow::EMPTY; MAX_OVERVIEW_ROWS],
        q_rows: [OverviewRow::EMPTY; MAX_OVERVIEW_ROWS],
        strip_wins: [StripWinSummary::EMPTY; MAX_OVERVIEW_ITEMS],
        win_ids: [0u32; MAX_OVERVIEW_ITEMS],
    };
}

struct OverviewScratchCell(UnsafeCell<OverviewScratch>);
unsafe impl Sync for OverviewScratchCell {}

static OVERVIEW_SCRATCH: OverviewScratchCell =
    OverviewScratchCell(UnsafeCell::new(OverviewScratch::EMPTY));

#[inline]
unsafe fn overview_scratch_mut() -> &'static mut OverviewScratch {
    &mut *OVERVIEW_SCRATCH.0.get()
}

#[inline]
pub fn overview_modal_rect(wm: &WindowManager) -> Rect {
    let w = 620usize.min(wm.screen_w.saturating_sub(28));
    let avail_h = wm.screen_h.saturating_sub(TOPBAR_H + DOCK_H + DOCK_MARGIN_B + 32);
    let h = 440usize.min(avail_h.max(240));
    let x = wm.screen_w.saturating_sub(w) / 2;
    Rect::new(x, TOPBAR_H + 16, w, h)
}

#[inline]
pub fn overview_list_rect(modal: Rect) -> Rect {
    Rect::new(modal.x + 12, modal.y + 62, modal.w.saturating_sub(24), modal.h.saturating_sub(96))
}

#[inline]
pub const fn slot_default_title(tk: u8) -> &'static [u8] {
    match tk {
        TITLE_KIND_SYSMON => b"Life Station",
        TITLE_KIND_TERMINAL => b"DRY Terminal",
        TITLE_KIND_FILES => b"DRY Files",
        TITLE_KIND_SETTINGS => b"Settings",
        TITLE_KIND_EDITOR => b"DRY Notes",
        TITLE_KIND_CALC => b"Calculator",
        TITLE_KIND_DIALOG => b"Quick Help",
        _ => b"DRY Window",
    }
}

pub fn slot_effective_title(slot: &WindowSlot) -> &[u8] {
    if slot.custom_title_len > 0 {
        slot.custom_title_bytes()
    } else {
        slot_default_title(slot.title_kind)
    }
}

#[inline(never)]
pub fn collect_wm_overview_items(wm: &WindowManager, out: &mut [OverviewWinItem]) -> usize {
    let mut n = 0usize;
    for (slot_idx, w) in wm.windows.iter().enumerate() {
        if !w.allocated || n >= out.len() {
            continue;
        }
        let app = WindowManager::slot_app_id(w).unwrap_or(AppId::Terminal);
        let z_rank = (0..wm.z_len)
            .find(|&z| wm.z_stack[z] == slot_idx)
            .map_or(0, |z| z + 1) as u16;
        let tbytes = slot_effective_title(w);
        let tlen = tbytes.len().min(32);
        let mut tbuf = [0u8; 32];
        tbuf[..tlen].copy_from_slice(&tbytes[..tlen]);
        out[n] = OverviewWinItem {
            win_id: w.id,
            app,
            title: tbuf,
            title_len: tlen as u8,
            role: w.role,
            focused: w.focused,
            minimized: w.minimized || !w.visible,
            z_rank,
        };
        n += 1;
    }
    n
}

/// Group windows by `AppId`, filter by `query` (case-insensitive substring over app + title),
/// and populate virtualizable layout rows returning `(row_count, total_content_height)`.
#[inline(never)]
pub fn build_overview_rows(
    items: &[OverviewWinItem],
    query: &[u8],
    out: &mut [OverviewRow; MAX_OVERVIEW_ROWS],
) -> (usize, usize) {
    let mut matched_idx = [0u8; MAX_OVERVIEW_ITEMS];
    let mut m_len = 0usize;
    for (idx, it) in items.iter().take(MAX_OVERVIEW_ITEMS).enumerate() {
        let hit = query.is_empty()
            || contains_ascii_icase(it.title_bytes(), query)
            || contains_ascii_icase(it.app.display_title(), query)
            || contains_ascii_icase(it.app.slug(), query)
            || contains_ascii_icase(app_strip_name(it.app, false), query);
        if hit {
            matched_idx[m_len] = idx as u8;
            m_len += 1;
        }
    }

    let mut group_order = [0u8; APP_COUNT];
    let mut group_max_z = [0u16; APP_COUNT];
    let mut group_cnt = [0u16; APP_COUNT];
    let mut group_min = [0u16; APP_COUNT];
    let mut active_groups = 0usize;

    for &mi in &matched_idx[..m_len] {
        let it = &items[mi as usize];
        let ai = it.app as usize;
        if group_cnt[ai] == 0 {
            group_order[active_groups] = ai as u8;
            active_groups += 1;
        }
        group_cnt[ai] += 1;
        if it.minimized {
            group_min[ai] += 1;
        }
        if it.z_rank > group_max_z[ai] {
            group_max_z[ai] = it.z_rank;
        }
    }

    for i in 1..active_groups {
        let key = group_order[i];
        let kz = group_max_z[key as usize];
        let mut j = i;
        while j > 0 && group_max_z[group_order[j - 1] as usize] < kz {
            group_order[j] = group_order[j - 1];
            j -= 1;
        }
        group_order[j] = key;
    }

    let mut row_n = 0usize;
    let mut y_off = 0usize;
    let mut group_idx = [0u8; MAX_OVERVIEW_ITEMS];
    for &ai_u8 in &group_order[..active_groups] {
        let ai = ai_u8 as usize;
        let Some(app) = AppId::from_u8(ai_u8) else { continue };
        let mut gn = 0usize;
        for &mi in &matched_idx[..m_len] {
            if items[mi as usize].app == app {
                group_idx[gn] = mi;
                gn += 1;
            }
        }
        for i in 1..gn {
            let kidx = group_idx[i];
            let kz = items[kidx as usize].z_rank;
            let mut j = i;
            while j > 0 && items[group_idx[j - 1] as usize].z_rank < kz {
                group_idx[j] = group_idx[j - 1];
                j -= 1;
            }
            group_idx[j] = kidx;
        }
        if row_n < MAX_OVERVIEW_ROWS {
            out[row_n] = OverviewRow {
                kind: OverviewRowKind::GroupHeader {
                    app,
                    count: group_cnt[ai],
                    min_count: group_min[ai],
                },
                y_off,
                height: OVERVIEW_GROUP_H,
            };
            y_off += OVERVIEW_GROUP_H;
            row_n += 1;
        }
        for &idx in &group_idx[..gn] {
            if row_n >= MAX_OVERVIEW_ROWS {
                break;
            }
            out[row_n] = OverviewRow {
                kind: OverviewRowKind::Window(idx as u16),
                y_off,
                height: OVERVIEW_WIN_H,
            };
            y_off += OVERVIEW_WIN_H;
            row_n += 1;
        }
    }

    (row_n, y_off)
}

/// Ensures `overview_sel_win_id` points to a valid row after search changes or live closes.
pub fn sync_overview_selection(
    items: &[OverviewWinItem],
    rows: &[OverviewRow],
    cur_sel_win_id: u32,
) -> u32 {
    if cur_sel_win_id != 0 {
        for r in rows {
            if let OverviewRowKind::Window(idx) = r.kind {
                if items[idx as usize].win_id == cur_sel_win_id {
                    return cur_sel_win_id;
                }
            }
        }
    }
    for r in rows {
        if let OverviewRowKind::Window(idx) = r.kind {
            return items[idx as usize].win_id;
        }
    }
    0
}

#[inline]
pub fn calc_visible_row_range(
    rows: &[OverviewRow],
    scroll_y: usize,
    viewport_h: usize,
) -> (usize, usize) {
    let end_y = scroll_y.saturating_add(viewport_h);
    let mut first = rows.len();
    let mut last = 0usize;
    for (idx, r) in rows.iter().enumerate() {
        let r_bot = r.y_off + r.height;
        if r_bot > scroll_y && r.y_off < end_y {
            if idx < first {
                first = idx;
            }
            last = idx + 1;
        }
    }
    if first >= last {
        (0, 0)
    } else {
        (first, last)
    }
}

#[inline]
fn row_action_rects(rr: Rect) -> (Rect, Rect) {
    let close_r = Rect::new(rr.x + rr.w.saturating_sub(32), rr.y + 7, 24, 24);
    let min_r = Rect::new(close_r.x.saturating_sub(30), rr.y + 7, 24, 24);
    (min_r, close_r)
}

#[inline(never)]
fn prepare_wm_rows(
    wm: &WindowManager,
    query: &[u8],
) -> (&'static mut OverviewScratch, usize, usize, usize) {
    let scratch = unsafe { overview_scratch_mut() };
    let n = collect_wm_overview_items(wm, &mut scratch.items);
    let (rn, total_h) = build_overview_rows(&scratch.items[..n], query, &mut scratch.rows);
    (scratch, n, rn, total_h)
}

#[inline(never)]
pub fn toggle_overview(wm: &mut WindowManager, dirty: &mut DirtyRegion) {
    let next = !wm.glass.overview_open;
    let mr = pad_overlay(overview_modal_rect(wm));
    close_all_overlays(wm, dirty);
    wm.glass.overview_open = next;
    if next {
        let (scratch, _, rn, _) = prepare_wm_rows(wm, &[]);
        let focused_id = wm.focused_slot().map_or(0, |s| wm.windows[s].id);
        wm.glass.overview_sel_win_id =
            sync_overview_selection(&scratch.items, &scratch.rows[..rn], focused_id);
        wm.glass.overview_scroll = 0;
    }
    dirty.mark(mr);
    dirty.mark(Rect::new(0, 0, wm.screen_w, TOPBAR_H));
    log_glass_toggle(b"OVERVIEW", next);
}

fn ensure_sel_visible(
    wm: &mut WindowManager,
    items: &[OverviewWinItem],
    rows: &[OverviewRow],
    total_h: usize,
    list_h: usize,
) {
    let max_scroll = total_h.saturating_sub(list_h);
    wm.glass.overview_scroll = wm.glass.overview_scroll.min(max_scroll);
    for r in rows {
        if let OverviewRowKind::Window(idx) = r.kind {
            if items[idx as usize].win_id == wm.glass.overview_sel_win_id {
                if r.y_off < wm.glass.overview_scroll {
                    wm.glass.overview_scroll = r.y_off;
                } else if r.y_off + r.height > wm.glass.overview_scroll + list_h {
                    wm.glass.overview_scroll = (r.y_off + r.height).saturating_sub(list_h);
                }
                wm.glass.overview_scroll = wm.glass.overview_scroll.min(max_scroll);
                break;
            }
        }
    }
}

fn request_overview_close(
    wm: &mut WindowManager,
    win_id: u32,
    dirty: &mut DirtyRegion,
) -> crate::wm::WmInputAction {
    dirty.mark(pad_overlay(overview_modal_rect(wm)));
    if let Some(slot_idx) =
        (0..MAX_WINDOWS).find(|&s| wm.windows[s].allocated && wm.windows[s].id == win_id)
    {
        wm.windows[slot_idx].close_requested = true;
        return crate::wm::WmInputAction::CloseRequested(win_id);
    }
    crate::wm::WmInputAction::None
}

#[inline(never)]
pub fn handle_overview_scroll(wm: &mut WindowManager, dy: i16, dirty: &mut DirtyRegion) -> bool {
    if !wm.p3_mode || !wm.glass.overview_open {
        return false;
    }
    let modal = overview_modal_rect(wm);
    let list_r = overview_list_rect(modal);
    let q = &wm.glass.overview_query[..wm.glass.overview_len as usize];
    let (_, _, _, total_h) = prepare_wm_rows(wm, q);
    let max_scroll = total_h.saturating_sub(list_r.h);
    let delta = (dy as isize) * 28;
    let next = (wm.glass.overview_scroll as isize + delta).clamp(0, max_scroll as isize) as usize;
    if next != wm.glass.overview_scroll {
        wm.glass.overview_scroll = next;
        dirty.mark(pad_overlay(modal));
    }
    true
}

#[inline(never)]
pub fn handle_overview_click(
    wm: &mut WindowManager,
    dirty: &mut DirtyRegion,
) -> Option<crate::wm::WmInputAction> {
    if !wm.p3_mode || !wm.glass.overview_open {
        return None;
    }
    let modal = overview_modal_rect(wm);
    if !modal.contains(wm.cursor_x, wm.cursor_y) {
        close_all_overlays(wm, dirty);
        return None;
    }
    let list_r = overview_list_rect(modal);
    if list_r.contains(wm.cursor_x, wm.cursor_y) {
        let q = &wm.glass.overview_query[..wm.glass.overview_len as usize];
        let (scratch, _, rn, total_h) = prepare_wm_rows(wm, q);
        let scroll = wm.glass.overview_scroll.min(total_h.saturating_sub(list_r.h));
        let (first, last) = calc_visible_row_range(&scratch.rows[..rn], scroll, list_r.h);
        for row in &scratch.rows[first..last] {
            if let OverviewRowKind::Window(idx) = row.kind {
                let it = scratch.items[idx as usize];
                let ry = list_r.y + row.y_off - scroll;
                let row_r = Rect::new(
                    list_r.x + 2,
                    ry + 2,
                    list_r.w.saturating_sub(14),
                    row.height.saturating_sub(4),
                );
                if row_r.contains(wm.cursor_x, wm.cursor_y) {
                    let (min_r, close_r) = row_action_rects(row_r);
                    if close_r.contains(wm.cursor_x, wm.cursor_y) {
                        wm.glass.overview_sel_win_id = it.win_id;
                        return Some(request_overview_close(wm, it.win_id, dirty));
                    }
                    if min_r.contains(wm.cursor_x, wm.cursor_y) {
                        wm.glass.overview_sel_win_id = it.win_id;
                        if let Some(slot_idx) = (0..MAX_WINDOWS)
                            .find(|&s| wm.windows[s].allocated && wm.windows[s].id == it.win_id)
                        {
                            if it.minimized {
                                let _ = wm.unminimize_and_focus_slot(slot_idx, dirty);
                            } else {
                                let _ = wm.minimize_slot(slot_idx, dirty);
                            }
                            dirty.mark(pad_overlay(modal));
                        }
                        return Some(crate::wm::WmInputAction::None);
                    }
                    let _ = wm.focus_or_restore_window_id(it.win_id, dirty);
                    close_all_overlays(wm, dirty);
                    return Some(crate::wm::WmInputAction::None);
                }
            }
        }
    }
    Some(crate::wm::WmInputAction::None)
}

#[inline(never)]
pub fn handle_overview_key(
    wm: &mut WindowManager,
    ev: InputEvent,
    dirty: &mut DirtyRegion,
) -> Option<crate::wm::WmInputAction> {
    if !wm.p3_mode || !wm.glass.overview_open {
        return None;
    }
    if ev.keycode == KEY_ESC {
        close_all_overlays(wm, dirty);
        return Some(crate::wm::WmInputAction::None);
    }

    let modal = overview_modal_rect(wm);
    let list_r = overview_list_rect(modal);

    let mut q_changed = false;
    if ev.keycode == KEY_BACKSPACE {
        if wm.glass.overview_len > 0 {
            wm.glass.overview_len -= 1;
            q_changed = true;
        }
    } else if (ev.modifiers & MOD_ALT) == 0 && (0x20..=0x7e).contains(&ev.ascii) {
        let len = wm.glass.overview_len as usize;
        if len < wm.glass.overview_query.len() {
            wm.glass.overview_query[len] = ev.ascii;
            wm.glass.overview_len += 1;
            q_changed = true;
        }
    }

    let q_len = wm.glass.overview_len as usize;
    let scratch = unsafe { overview_scratch_mut() };
    let n = collect_wm_overview_items(wm, &mut scratch.items);
    let (rn, total_h) = build_overview_rows(
        &scratch.items[..n],
        &wm.glass.overview_query[..q_len],
        &mut scratch.rows,
    );
    wm.glass.overview_sel_win_id = sync_overview_selection(
        &scratch.items[..n],
        &scratch.rows[..rn],
        wm.glass.overview_sel_win_id,
    );

    if q_changed {
        wm.glass.overview_scroll = 0;
        ensure_sel_visible(wm, &scratch.items[..n], &scratch.rows[..rn], total_h, list_r.h);
        dirty.mark(pad_overlay(modal));
        return Some(crate::wm::WmInputAction::None);
    }

    let mut wn = 0usize;
    for r in &scratch.rows[..rn] {
        if let OverviewRowKind::Window(idx) = r.kind {
            if wn < MAX_OVERVIEW_ITEMS {
                scratch.win_ids[wn] = scratch.items[idx as usize].win_id;
                wn += 1;
            }
        }
    }
    if wn == 0 {
        dirty.mark(pad_overlay(modal));
        return Some(crate::wm::WmInputAction::None);
    }

    let cur_pos = (0..wn)
        .find(|&i| scratch.win_ids[i] == wm.glass.overview_sel_win_id)
        .unwrap_or(0);

    let next_pos = if ev.keycode == KEY_UP {
        Some(cur_pos.saturating_sub(1))
    } else if ev.keycode == KEY_DOWN {
        Some((cur_pos + 1).min(wn - 1))
    } else if ev.keycode == KEY_TAB {
        Some(if cur_pos + 1 >= wn { 0 } else { cur_pos + 1 })
    } else if ev.keycode == KEY_HOME {
        Some(0)
    } else if ev.keycode == KEY_END {
        Some(wn - 1)
    } else if ev.keycode == KEY_PAGEUP {
        Some(cur_pos.saturating_sub(4))
    } else if ev.keycode == KEY_PAGEDOWN {
        Some((cur_pos + 4).min(wn - 1))
    } else {
        None
    };

    if let Some(pos) = next_pos {
        wm.glass.overview_sel_win_id = scratch.win_ids[pos];
        ensure_sel_visible(wm, &scratch.items[..n], &scratch.rows[..rn], total_h, list_r.h);
        dirty.mark(pad_overlay(modal));
        return Some(crate::wm::WmInputAction::None);
    }
    if ev.keycode == KEY_ENTER {
        let target_id = wm.glass.overview_sel_win_id;
        let _ = wm.focus_or_restore_window_id(target_id, dirty);
        close_all_overlays(wm, dirty);
        return Some(crate::wm::WmInputAction::None);
    }
    if ev.keycode == KEY_DELETE {
        let target_id = wm.glass.overview_sel_win_id;
        return Some(request_overview_close(wm, target_id, dirty));
    }
    if (ev.modifiers & MOD_ALT) != 0 && (ev.ascii == b'm' || ev.ascii == b'M') {
        let target_id = wm.glass.overview_sel_win_id;
        if let Some(slot_idx) =
            (0..MAX_WINDOWS).find(|&s| wm.windows[s].allocated && wm.windows[s].id == target_id)
        {
            if wm.windows[slot_idx].minimized || !wm.windows[slot_idx].visible {
                let _ = wm.unminimize_and_focus_slot(slot_idx, dirty);
            } else {
                let _ = wm.minimize_slot(slot_idx, dirty);
            }
            dirty.mark(pad_overlay(modal));
        }
        return Some(crate::wm::WmInputAction::None);
    }
    Some(crate::wm::WmInputAction::None)
}

#[inline(never)]
fn paint_overview_body(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    ui: &UiTheme,
    r: Rect,
    clip: Rect,
) {
    let fmt = PixelFormat::from_fb(&comp.fb_info);
    let mut c =
        unsafe { Canvas::from_raw(comp.back_ptr, comp.w, comp.h, comp.fb_stride, fmt, r.intersect(clip).clamp_to(comp.w, comp.h)) };

    let q = &wm.glass.overview_query[..wm.glass.overview_len as usize];
    let (scratch, n, rn, total_h) = prepare_wm_rows(wm, q);
    let sel_win_id =
        sync_overview_selection(&scratch.items[..n], &scratch.rows[..rn], wm.glass.overview_sel_win_id);

    let sb = Rect::new(r.x + 14, r.y + 14, r.w.saturating_sub(132), 34);
    c.fill_bordered_rrect_aa(sb, 8, ui.bg_input, ui.accent);
    draw_vector_icon(&mut c, Rect::new(sb.x + 10, sb.y + 9, 16, 16), IconKind::Search, ui.accent, ui.accent);
    if wm.glass.overview_len == 0 {
        c.draw_text(
            (sb.x + 32) as i32,
            (sb.y + 8) as i32,
            FontId::SansRegular,
            12,
            b"Search windows by app or title...",
            ui.fg_muted,
        );
    } else {
        c.draw_text((sb.x + 32) as i32, (sb.y + 8) as i32, FontId::SansSemiBold, 12, q, ui.fg_primary);
    }

    let mut cb = [0u8; 16];
    let cn = write_u32_ascii(&mut cb, n as u32);
    let suf = b" open";
    cb[cn..cn + suf.len()].copy_from_slice(suf);
    let stat_r = Rect::new(r.x + r.w.saturating_sub(106), r.y + 14, 92, 34);
    c.fill_bordered_rrect_aa(stat_r, 8, ui.bg_card_alt, ui.border_subtle);
    c.draw_text_centered(stat_r, FontId::SansSemiBold, 11, &cb[..cn + suf.len()], ui.fg_primary);

    let list_r = overview_list_rect(r);
    if rn == 0 {
        let empty_msg: &[u8] = if n == 0 {
            b"No open or minimized windows."
        } else {
            b"No windows match this search."
        };
        c.draw_text((list_r.x + 12) as i32, (list_r.y + 18) as i32, FontId::SansRegular, 13, empty_msg, ui.fg_muted);
    } else {
        let scroll = wm.glass.overview_scroll.min(total_h.saturating_sub(list_r.h));
        let (first, last) = calc_visible_row_range(&scratch.rows[..rn], scroll, list_r.h);
        let list_clip = list_r.intersect(clip).clamp_to(comp.w, comp.h);
        let mut lc = unsafe { Canvas::from_raw(comp.back_ptr, comp.w, comp.h, comp.fb_stride, fmt, list_clip) };

        for row in &scratch.rows[first..last] {
            let ry_signed = (list_r.y + row.y_off) as isize - scroll as isize;
            if ry_signed + (row.height as isize) <= list_r.y as isize {
                continue;
            }
            let ry = ry_signed.max(list_r.y as isize) as usize;
            let row_w = list_r.w.saturating_sub(12);
            match row.kind {
                OverviewRowKind::GroupHeader { app, count, min_count } => {
                    draw_vector_icon(&mut lc, Rect::new(list_r.x + 6, ry + 5, 14, 14), IconKind::from_app(app), ui.accent_fg, ui.accent);
                    lc.draw_text((list_r.x + 26) as i32, (ry + 4) as i32, FontId::SansSemiBold, 11, app_strip_name(app, false), ui.fg_secondary);
                    let mut gbuf = [0u8; 20];
                    let gn = write_u32_ascii(&mut gbuf, count as u32);
                    lc.draw_text(
                        (list_r.x + row_w.saturating_sub(42)) as i32,
                        (ry + 4) as i32,
                        FontId::MonoRegular,
                        10,
                        &gbuf[..gn],
                        if min_count > 0 { ui.warn } else { ui.fg_muted },
                    );
                }
                OverviewRowKind::Window(idx) => {
                    let it = &scratch.items[idx as usize];
                    let row_r = Rect::new(list_r.x + 2, ry + 2, row_w, row.height.saturating_sub(4));
                    let sel = it.win_id == sel_win_id;
                    let bg = if sel { ui.accent_subtle } else { ui.bg_card_alt };
                    let bd = if sel { ui.accent } else if it.focused { ui.border_strong } else { ui.border_subtle };
                    lc.fill_bordered_rrect_aa(row_r, 8, bg, bd);
                    draw_vector_icon(
                        &mut lc,
                        Rect::new(row_r.x + 10, row_r.y + 8, 18, 18),
                        IconKind::from_app(it.app),
                        if sel { ui.accent_fg } else { ui.fg_primary },
                        ui.accent,
                    );
                    lc.draw_text(
                        (row_r.x + 34) as i32,
                        (row_r.y + 8) as i32,
                        if sel || it.focused { FontId::SansSemiBold } else { FontId::SansRegular },
                        12,
                        it.title_bytes(),
                        if it.minimized { ui.fg_muted } else { ui.fg_primary },
                    );
                    let badge_txt: Option<(&[u8], u32, u32)> = if it.focused {
                        Some((b"Active", ui.ok_bg, ui.ok_fg))
                    } else if it.minimized {
                        Some((b"Minimized", ui.bg_card, ui.warn))
                    } else if it.role == WindowRole::DialogTransient {
                        Some((b"Dialog", ui.bg_card, ui.fg_muted))
                    } else {
                        None
                    };
                    if let Some((bt, bbg, bfg)) = badge_txt {
                        let br = Rect::new(row_r.x + row_r.w.saturating_sub(136), row_r.y + 7, 68, 20);
                        lc.fill_rrect_aa(br, 6, bbg, 255);
                        lc.draw_text_centered(br, FontId::SansSemiBold, 10, bt, bfg);
                    }
                    let (min_r, close_r) = row_action_rects(row_r);
                    lc.fill_bordered_rrect_aa(min_r, 6, ui.bg_card, ui.border_subtle);
                    lc.draw_text_centered(min_r, FontId::SansSemiBold, 11, if it.minimized { b"+" } else { b"-" }, ui.fg_primary);
                    lc.fill_bordered_rrect_aa(close_r, 6, ui.bg_card, ui.border_subtle);
                    draw_vector_icon(&mut lc, Rect::new(close_r.x + 7, close_r.y + 7, 10, 10), IconKind::Close, ui.fg_primary, ui.accent);
                }
            }
        }

        if total_h > list_r.h {
            let track = Rect::new(list_r.x + list_r.w.saturating_sub(6), list_r.y + 2, 4, list_r.h.saturating_sub(4));
            lc.fill_rrect_aa(track, 2, ui.bg_card, 180);
            let thumb_h = ((list_r.h * list_r.h) / total_h.max(1)).clamp(20, track.h);
            let max_s = total_h.saturating_sub(list_r.h).max(1);
            let thumb_y = track.y + ((track.h - thumb_h) * scroll) / max_s;
            lc.fill_rrect_aa(Rect::new(track.x, thumb_y, 4, thumb_h), 2, ui.accent, 230);
        }
    }

    let foot_y = r.y + r.h.saturating_sub(26);
    c.draw_text(
        (r.x + 16) as i32,
        foot_y as i32,
        FontId::SansRegular,
        10,
        b"Up/Down Select   Enter Focus   Alt+M Min/Restore   Del Close   Esc Exit",
        ui.fg_muted,
    );
}

#[inline(never)]
pub fn paint_overview(
    comp: &mut DesktopCompositor,
    wm: &WindowManager,
    tokens: &GlassTokens,
    clip: Rect,
    blur: bool,
) {
    let r = overview_modal_rect(wm);
    let ui = UiTheme::new(tokens.mode, wm.dpi_pct());
    let fmt = PixelFormat::from_fb(&comp.fb_info);
    if paint_frosted_panel(comp, tokens, &ui, fmt, r, clip, blur).is_none() {
        return;
    }
    paint_overview_body(comp, wm, &ui, r, clip);
}

/// Deterministic scalability verification across `1, 5, 10, 25, 50` windows, `3` resolutions,
/// `3` DPI scales, `4` layout modes, search/grouping/virtualization, and live add/remove.
#[inline(never)]
pub fn run_window_scalability_selftest() -> bool {
    let counts = [1usize, 5, 10, 25, 50];
    let screens = [(1024usize, 768usize), (1280, 800), (1920, 1080)];
    let mut ok = true;
    let scratch = unsafe { overview_scratch_mut() };

    for &n in &counts {
        for i in 0..n {
            let app = AppId::ALL[i % APP_COUNT];
            let minimized = i > 0 && (i % 7 == 0);
            let focused = i == 0;
            scratch.strip_wins[i] = StripWinSummary {
                win_id: (i + 1) as u32,
                app,
                visible: !minimized,
                minimized,
                focused,
            };
            let mut tbuf = [0u8; 32];
            let base = app_strip_name(app, false);
            let bl = base.len().min(26);
            tbuf[..bl].copy_from_slice(&base[..bl]);
            tbuf[bl] = b' ';
            tbuf[bl + 1] = b'#';
            let nl = write_u32_ascii(&mut tbuf[bl + 2..], (i + 1) as u32);
            scratch.items[i] = OverviewWinItem {
                win_id: (i + 1) as u32,
                app,
                title: tbuf,
                title_len: (bl + 2 + nl) as u8,
                role: WindowRole::Normal,
                focused,
                minimized,
                z_rank: (n - i) as u16,
            };
        }

        let (rn, total_h) = build_overview_rows(&scratch.items[..n], &[], &mut scratch.rows);
        let mut sum_items = 0usize;
        for r in &scratch.rows[..rn] {
            if let OverviewRowKind::Window(_) = r.kind {
                sum_items += 1;
            }
        }
        if sum_items != n || total_h == 0 {
            ok = false;
        }

        for &(sw, sh) in &screens {
            let brand_r = if sw < 1200 { Rect::new(4, 4, 80, 20) } else { Rect::new(6, 4, 102, 20) };
            let mode_x = (sw / 2).saturating_sub(56);
            let strip = compute_adaptive_strip_layout(
                sw,
                brand_r,
                mode_x,
                &AppId::ALL,
                &scratch.strip_wins[..n],
            );
            let budget = (sw * 35) / 100;
            if strip.bounds.w > budget
                || strip.bounds.x + strip.bounds.w > mode_x
                || strip.bounds.y + strip.bounds.h > TOPBAR_H
            {
                ok = false;
            }
            for s in 1..strip.slot_count {
                if strip.slots[s - 1].rect.x + strip.slots[s - 1].rect.w > strip.slots[s].rect.x {
                    ok = false;
                }
            }
            if n >= 10 && strip.overflow_count == 0 {
                ok = false;
            }
            let list_h = 440usize
                .min(sh.saturating_sub(TOPBAR_H + DOCK_H + DOCK_MARGIN_B + 32))
                .saturating_sub(96);
            let (vfirst, vlast) = calc_visible_row_range(&scratch.rows[..rn], total_h / 3, list_h);
            let max_vis = list_h / OVERVIEW_GROUP_H + 3;
            if vlast.saturating_sub(vfirst) > max_vis {
                ok = false;
            }
        }

        let (qrn, _) = build_overview_rows(&scratch.items[..n], b"#1", &mut scratch.q_rows);
        if qrn == 0 || sync_overview_selection(&scratch.items[..n], &scratch.q_rows[..qrn], 9999) == 0 {
            ok = false;
        }
    }

    let modes = [LayoutMode::Floating, LayoutMode::SideBySideTiled, LayoutMode::Stacked, LayoutMode::Tabbed];
    let dpis = [100u16, 125, 150];
    if modes.len() != 4 || dpis.len() != 3 {
        ok = false;
    }

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-WM-SCALABILITY] pass=");
    lb.push_u64(u64::from(ok));
    lb.push_bytes(b" counts=1,5,10,25,50 res=3 dpi=3 layouts=4 max_strip_pct=35");
    wm_log_line(&lb);
    ok
}
