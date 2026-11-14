// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Shared DRY Glass 2.0 UI view for `dry-files` (`Files`).

use crate::apps::{PixelAppWindow, PointerStep};
use crate::gfx::Rect;
use crate::shell::LineBuilder;
use crate::ttf::{font_metrics, measure_text, FontId};
use crate::ui::containers::{
    breadcrumb_bar, card, list_row, sidebar_nav, sidebar_preferred_w, sidebar_tooltip_overlay,
    status_badge, table_header,
};
use crate::ui::icons::{draw_vector_icon, IconKind};
use crate::ui::layout::{inset_rect, split_bot, split_left, split_top, UiCtx};
use crate::ui::theme::{StatusKind, UiTheme};
use crate::ui::widgets::{button, scrollbar_v, ButtonOpts, ButtonVariant};
use crate::vfs_proto::{VfsDirEntry, VFS_FLAG_PRIVATE};

pub const FILES_PIXEL_VADDR: u64 = 0x1c00_0000;
pub const FILES_WIN_W: u16 = 620;
pub const FILES_WIN_H: u16 = 384;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FilesPromptMode {
    Browse,
    Create,
    Write,
    Rename,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FilesAction {
    None,
    SelectMount(usize),
    SelectEntry(usize),
    OpenEntry(usize),
    StartCreate,
    StartWrite,
    StartRename,
    DeleteSelected,
}

pub struct FilesView<'a> {
    pub mount_idx: usize,
    pub persist_available: bool,
    pub readonly: bool,
    pub volatile: bool,
    pub entries: &'a [VfsDirEntry],
    pub sel_idx: usize,
    pub list_scroll: usize,
    pub preview_bytes: &'a [u8],
    pub preview_total: usize,
    pub prompt_mode: FilesPromptMode,
    pub prompt_input: &'a [u8],
    pub status_msg: &'a [u8],
    pub status_warn: bool,
}

fn files_nav_items(persist_available: bool) -> [(&'static [u8], IconKind); 3] {
    [
        (b"/boot", IconKind::Shield),
        (b"/ram", IconKind::Memory),
        (
            b"/persist",
            if persist_available { IconKind::Folder } else { IconKind::Warning },
        ),
    ]
}

pub fn files_vis_rows(t: UiTheme, w: usize, h: usize) -> usize {
    let top_h = (t.ctrl_h + 12).clamp(40, 48);
    let foot_h = 32usize;
    let main_h = h.saturating_sub(top_h + foot_h + t.sp_sm * 2);
    let nav_items = files_nav_items(true);
    let sb_w = sidebar_preferred_w(t, w, &nav_items);
    let right_w = w.saturating_sub(t.sp_md * 2 + sb_w + t.sp_sm);
    let list_h = if right_w >= 400 { main_h } else { main_h * 56 / 100 };
    let row_h = (font_metrics(FontId::SansRegular, t.body_px).line_height as usize + 8).max(24);
    (list_h.saturating_sub(34) / row_h).max(1)
}

pub fn render_files(
    win: &mut PixelAppWindow,
    view: &FilesView<'_>,
    step: PointerStep,
) -> FilesAction {
    let t = win.theme();
    let w = win.w as usize;
    let h = win.h as usize;

    let top_h = (t.ctrl_h + 12).clamp(40, 48);
    let foot_h = 32usize;
    let root = Rect::new(0, 0, w, h);
    let (top_r, rest1) = split_top(root, top_h, 0);
    let (mid_outer, foot_r) = split_bot(rest1, foot_h, 0);
    let mid_r = inset_rect(mid_outer, t.sp_md, t.sp_sm);

    let nav_items = files_nav_items(view.persist_available);
    let sb_w = sidebar_preferred_w(t, w, &nav_items);
    let (sb_r, content_r) = split_left(mid_r, sb_w, t.sp_sm);
    let (list_r, preview_r) = if content_r.w >= 400 {
        split_left(content_r, content_r.w * 53 / 100, t.sp_sm)
    } else {
        split_top(content_r, content_r.h * 56 / 100, t.sp_sm)
    };

    let mut c = win.canvas();
    c.clear(t.bg_app);

    // Top toolbar
    c.fill_rect(top_r, t.bg_sidebar);
    c.hline(top_r.x, top_r.y + top_r.h.saturating_sub(1), top_r.w, t.border_subtle, 220);

    let btn_h = top_h.saturating_sub(14).max(26);
    let btn_y = top_r.y + (top_h.saturating_sub(btn_h)) / 2;
    let mut act = FilesAction::None;

    let mut ui = UiCtx::begin(
        &mut win.persist,
        t,
        step.clicked,
        step.released,
        step.scroll_dy,
        None,
    );

    let act_btns: [(&[u8], ButtonVariant, FilesAction); 4] = [
        (b"New", ButtonVariant::Primary, FilesAction::StartCreate),
        (b"Write", ButtonVariant::Secondary, FilesAction::StartWrite),
        (b"Rename", ButtonVariant::Secondary, FilesAction::StartRename),
        (b"Del", ButtonVariant::Danger, FilesAction::DeleteSelected),
    ];
    let mut rx = w.saturating_sub(t.sp_md);
    let mut btn_rects = [Rect::new(0, 0, 0, 0); 4];
    for i in (0..4).rev() {
        let bw = (measure_text(FontId::SansSemiBold, t.caption_px, act_btns[i].0).max(32) as usize) + 18;
        rx = rx.saturating_sub(bw);
        btn_rects[i] = Rect::new(rx, btn_y, bw, btn_h);
        rx = rx.saturating_sub(6);
    }

    let can_edit_item = !view.readonly && !view.entries.is_empty();
    for i in 0..4 {
        let disabled = if i == 0 { view.readonly } else { !can_edit_item };
        let opts = ButtonOpts {
            label: act_btns[i].0,
            icon: None,
            variant: act_btns[i].1,
            disabled,
            selected: false,
        };
        if button(&mut c, &mut ui, 10 + i as u16, btn_rects[i], opts) {
            act = act_btns[i].2;
        }
    }

    let mount_lbl: &[u8] = match view.mount_idx {
        1 => b"ram",
        2 => b"persist",
        _ => b"boot",
    };
    let (badge_lbl, badge_kind): (&[u8], StatusKind) = if view.readonly {
        (b"RO-BOOT", StatusKind::Neutral)
    } else if view.volatile {
        (b"VOL-RAM", StatusKind::Warn)
    } else {
        (b"DRYFS CoW", StatusKind::Ok)
    };
    let badge_w = measure_text(FontId::SansSemiBold, t.caption_px, badge_lbl).max(0) as usize + 22;
    let bc_max_w = rx.saturating_sub(t.sp_md + badge_w + 10);
    let bc_need = (measure_text(FontId::SansSemiBold, t.body_px, b"VFS > persist").max(80) as usize) + 52;
    if bc_max_w >= bc_need {
        let bc_r = Rect::new(t.sp_md, btn_y, bc_max_w.min(220), btn_h);
        let _ = breadcrumb_bar(&mut c, &mut ui, 20, bc_r, &[b"VFS", mount_lbl]);
        let bx = bc_r.x + bc_r.w + 8;
        if bx + badge_w <= rx {
            status_badge(&mut c, t, bx, top_r.y + (top_h.saturating_sub(20)) / 2, badge_lbl, badge_kind);
        }
    } else if rx > t.sp_md + badge_w + 4 {
        let mut bx = t.sp_md;
        if sb_r.w < 104 {
            let act_mount = nav_items[view.mount_idx.min(nav_items.len() - 1)].0;
            let mw = measure_text(FontId::SansSemiBold, t.caption_px, act_mount).max(0) as usize + 22;
            if bx + mw + 6 + badge_w <= rx {
                status_badge(
                    &mut c,
                    t,
                    bx,
                    top_r.y + (top_h.saturating_sub(20)) / 2,
                    act_mount,
                    StatusKind::Accent,
                );
                bx += mw + 6;
            }
        }
        status_badge(
            &mut c,
            t,
            bx,
            top_r.y + (top_h.saturating_sub(20)) / 2,
            badge_lbl,
            badge_kind,
        );
    }

    // Left sidebar
    let mut sel_mount = view.mount_idx;
    if sidebar_nav(&mut c, &mut ui, 30, sb_r, &nav_items, &mut sel_mount) {
        act = FilesAction::SelectMount(sel_mount);
    }

    // Directory list pane
    c.fill_bordered_rrect_aa(list_r, t.r_md, t.bg_card, t.border_subtle);
    let th_h = 22usize;
    let th_r = Rect::new(list_r.x + 2, list_r.y + 2, list_r.w.saturating_sub(4), th_h);
    let size_col_w = 64usize;
    let name_col_w = th_r.w.saturating_sub(size_col_w + 14);
    table_header(&mut c, t, th_r, &[(b"NAME", name_col_w), (b"SIZE", size_col_w)]);

    let rows_area = Rect::new(
        list_r.x + 6,
        list_r.y + th_h + 6,
        list_r.w.saturating_sub(18),
        list_r.h.saturating_sub(th_h + 12),
    );
    let row_h = (font_metrics(FontId::SansRegular, t.body_px).line_height as usize + 8).max(24);
    let vis_rows = (rows_area.h / row_h).max(1);
    let m_body = font_metrics(FontId::SansRegular, t.body_px);
    let mut rc = c.sub_clip(rows_area);

    if rows_area.h >= 12 {
        for r in 0..vis_rows {
            let idx = view.list_scroll + r;
            if idx >= view.entries.len() {
                break;
            }
            let rr = Rect::new(rows_area.x, rows_area.y + r * row_h, rows_area.w, row_h.saturating_sub(2));
            let selected = idx == view.sel_idx;
            if list_row(&mut rc, &mut ui, 50 + r as u16, rr, selected, (idx & 1) == 1) {
                act = if selected {
                    FilesAction::OpenEntry(idx)
                } else {
                    FilesAction::SelectEntry(idx)
                };
            }

            let e = &view.entries[idx];
            let is_priv = (e.flags & VFS_FLAG_PRIVATE) != 0;
            let ic = if is_priv { IconKind::Lock } else { IconKind::File };
            let ic_rgb = if is_priv { t.warn_fg } else if selected { t.accent_fg } else { t.fg_secondary };
            draw_vector_icon(
                &mut rc,
                Rect::new(rr.x + 6, rr.y + (rr.h.saturating_sub(14)) / 2, 14, 14),
                ic,
                ic_rgb,
                t.accent_fg,
            );

            let mut sz_lb = LineBuilder::new();
            if is_priv {
                sz_lb.push_bytes(b"PRIV");
            } else {
                sz_lb.push_u64(e.size_bytes as u64);
                sz_lb.push_byte(b'B');
            }
            let sz_w = measure_text(FontId::MonoRegular, t.caption_px, sz_lb.as_bytes()).max(0) as usize;
            let ty = rr.y as i32 + ((rr.h as i32 - (m_body.ascent + m_body.descent)) / 2);
            let sz_x = (rr.x + rr.w).saturating_sub(sz_w + 6);
            rc.draw_text(
                sz_x as i32,
                ty + 1,
                FontId::MonoRegular,
                t.caption_px,
                sz_lb.as_bytes(),
                if is_priv { t.warn_fg } else { t.fg_muted },
            );

            let name_max = sz_x.saturating_sub(rr.x + 28);
            rc.draw_text_ellipsis(
                (rr.x + 24) as i32,
                ty,
                name_max as i32,
                if selected { FontId::SansSemiBold } else { FontId::SansRegular },
                t.body_px,
                e.name_bytes(),
                if is_priv { t.warn_fg } else { t.fg_primary },
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
        view.entries.len(),
        vis_rows,
        view.list_scroll,
        t.bg_card_alt,
        t.border_strong,
    );

    let hov_id = ui.hovered_id();
    ui.finish();

    // File Preview Card
    let mut sub_lb = LineBuilder::new();
    let title_slice = if view.sel_idx < view.entries.len() {
        sub_lb.push_u64(view.preview_total as u64);
        sub_lb.push_bytes(b" B");
        view.entries[view.sel_idx].name_bytes()
    } else {
        b"Empty Directory" as &[u8]
    };
    let pv_in = card(&mut c, t, preview_r, Some(title_slice), Some(sub_lb.as_bytes()));
    let mut psc = c.sub_clip(pv_in);
    if view.preview_bytes.is_empty() {
        psc.draw_text(
            pv_in.x as i32,
            pv_in.y as i32,
            FontId::SansRegular,
            t.caption_px,
            b"Select a file or press [Enter/V] to view.",
            t.fg_muted,
        );
    } else {
        let pm = font_metrics(FontId::MonoRegular, t.caption_px);
        let p_lh = (pm.line_height + 2).max(14) as usize;
        let max_lines = (pv_in.h / p_lh).max(1);
        render_preview_lines(&mut psc, pv_in, p_lh, max_lines, t.caption_px, view.preview_bytes, t.fg_primary);
    }

    // Bottom status / prompt bar
    c.fill_rect(foot_r, t.bg_sidebar);
    c.hline(foot_r.x, foot_r.y, foot_r.w, t.border_subtle, 220);
    let cm = font_metrics(FontId::SansRegular, t.caption_px);
    let fy = foot_r.y as i32 + ((foot_r.h as i32 - (cm.ascent + cm.descent)) / 2);

    if view.prompt_mode != FilesPromptMode::Browse {
        let p_rect = inset_rect(foot_r, 8, 4);
        c.fill_bordered_rrect_aa(p_rect, t.r_sm, t.bg_input, t.focus_ring);
        let prefix: &[u8] = match (view.prompt_mode, view.mount_idx) {
            (FilesPromptMode::Create, 2) => b"CREATE /persist/> ",
            (FilesPromptMode::Create, _) => b"CREATE /ram/> ",
            (FilesPromptMode::Write, _) => b"WRITE TEXT> ",
            (FilesPromptMode::Rename, 2) => b"RENAME TO /persist/> ",
            _ => b"RENAME TO /ram/> ",
        };
        let mut plb = LineBuilder::new();
        plb.push_bytes(prefix);
        plb.push_bytes(view.prompt_input);
        plb.push_byte(b'_');
        c.draw_text_ellipsis(
            (p_rect.x + 8) as i32,
            fy,
            p_rect.w.saturating_sub(150) as i32,
            FontId::MonoRegular,
            t.caption_px,
            plb.as_bytes(),
            t.accent_fg,
        );
        let hint = b"[Enter] Confirm  [Esc] Cancel";
        let hw = measure_text(FontId::SansRegular, t.caption_px, hint).max(0) as usize;
        c.draw_text(
            (p_rect.x + p_rect.w).saturating_sub(hw + 8) as i32,
            fy,
            FontId::SansRegular,
            t.caption_px,
            hint,
            t.fg_muted,
        );
    } else {
        let hint: &[u8] = if view.readonly {
            b"[1-3/Tab] Mount  [Enter/V] View  [Q] Close"
        } else {
            b"[N] New  [E] Write  [R] Ren  [D] Del  [Q]"
        };
        let hw = measure_text(FontId::SansRegular, t.caption_px, hint).max(0) as usize;
        let hx = w.saturating_sub(t.sp_md + hw);
        let smax = hx.saturating_sub(t.sp_md + 10);
        c.draw_text_ellipsis(
            t.sp_md as i32,
            fy,
            smax as i32,
            FontId::SansRegular,
            t.caption_px,
            view.status_msg,
            if view.status_warn { t.warn_fg } else { t.ok_fg },
        );
        c.draw_text(hx as i32, fy, FontId::SansRegular, t.caption_px, hint, t.fg_muted);
    }

    sidebar_tooltip_overlay(&mut c, t, sb_r, &nav_items, 30, hov_id);
    act
}

fn render_preview_lines(
    c: &mut crate::ui::canvas::Canvas,
    r: Rect,
    line_h: usize,
    max_lines: usize,
    sz: u8,
    bytes: &[u8],
    rgb: u32,
) {
    let mut start = 0usize;
    let mut row = 0usize;
    let mut clean = [0u8; 72];
    while start < bytes.len() && row < max_lines {
        let rem = &bytes[start..];
        let nl = rem.iter().position(|&b| b == b'\n').unwrap_or(rem.len());
        let line = &rem[..nl];
        let mut off = 0usize;
        if line.is_empty() {
            row += 1;
        }
        while off < line.len() && row < max_lines {
            let mut take = (line.len() - off).min(clean.len());
            for (k, &ch) in line[off..(off + take)].iter().enumerate() {
                clean[k] = if (0x20..=0x7e).contains(&ch) { ch } else { b'.' };
            }
            while take > 1 && measure_text(FontId::MonoRegular, sz, &clean[..take]) > r.w as i32 {
                take -= 1;
            }
            c.draw_text(
                r.x as i32,
                (r.y + row * line_h) as i32,
                FontId::MonoRegular,
                sz,
                &clean[..take],
                rgb,
            );
            off += take;
            row += 1;
        }
        start += (nl + 1).min(rem.len());
    }
}
