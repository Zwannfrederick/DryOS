// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Shared DRY Glass 2.0 UI view for `dry-editor` (`DRY Notes`).

use crate::apps::{PixelAppWindow, PointerStep};
use crate::gfx::Rect;
use crate::shell::LineBuilder;
use crate::ttf::{font_metrics, measure_text, FontId};
use crate::ui::containers::status_badge;
use crate::ui::icons::{draw_vector_icon, IconKind};
use crate::ui::layout::{inset_rect, split_bot, split_top, UiCtx};
use crate::ui::text_edit::{multiline_viewport, MultiLineParams};
use crate::ui::theme::StatusKind;
use crate::ui::widgets::{button, ButtonOpts, ButtonVariant};

pub enum EditorAction {
    None,
    Save,
    Clear,
    MoveCursor(usize, usize),
}

pub struct EditorView<'a> {
    pub path: &'a [u8],
    pub persist_ok: bool,
    pub line_slices: &'a [&'a [u8]],
    pub cur_row: usize,
    pub cur_col: usize,
    pub scroll_line: &'a mut usize,
    pub dirty_doc: bool,
    pub total_bytes: usize,
    pub status: &'a [u8],
}

pub fn render_editor(
    win: &mut PixelAppWindow,
    view: &mut EditorView<'_>,
    step: PointerStep,
) -> EditorAction {
    let t = win.theme();
    let w = win.w as usize;
    let h = win.h as usize;

    let top_h = (t.ctrl_h + 14).clamp(42, 54);
    let foot_h = 30usize;
    let root = Rect::new(0, 0, w, h);
    let (top_r, rest_r) = split_top(root, top_h, 0);
    let (body_outer, foot_r) = split_bot(rest_r, foot_h, 0);
    let body_r = inset_rect(body_outer, t.sp_md, t.sp_sm);

    let mut c = win.canvas();
    c.clear(t.bg_app);

    c.fill_rect(top_r, t.bg_sidebar);
    c.hline(top_r.x, top_r.y + top_r.h.saturating_sub(1), top_r.w, t.border_subtle, 220);

    let btn_h = (top_h.saturating_sub(14)).max(26);
    let btn_y = top_r.y + (top_h.saturating_sub(btn_h)) / 2;
    let save_w = ((measure_text(FontId::SansSemiBold, t.body_px, b"Save (Ctrl+S)").max(90) as usize)
        + 42)
        .min(w / 3);
    let clear_w = ((measure_text(FontId::SansSemiBold, t.body_px, b"Clear").max(52) as usize) + 26)
        .min(w / 5);

    let save_x = w.saturating_sub(t.sp_md + save_w);
    let clear_x = save_x.saturating_sub(t.sp_sm + clear_w);
    let save_r = Rect::new(save_x, btn_y, save_w, btn_h);
    let clear_r = Rect::new(clear_x, btn_y, clear_w, btn_h);

    let path_x = t.sp_md;
    let path_max = clear_x.saturating_sub(path_x + t.sp_sm);
    if path_max >= 96 {
        let pill_r = Rect::new(path_x, btn_y, path_max.min(276), btn_h);
        c.fill_bordered_rrect_aa(pill_r, t.r_sm, t.bg_card, t.border_subtle);
        draw_vector_icon(
            &mut c,
            Rect::new(pill_r.x + 8, pill_r.y + (btn_h.saturating_sub(14)) / 2, 14, 14),
            IconKind::Editor,
            t.fg_primary,
            t.accent_fg,
        );
        let cap_m = font_metrics(FontId::MonoRegular, t.caption_px);
        let p_ty = pill_r.y as i32 + ((pill_r.h as i32 - (cap_m.ascent + cap_m.descent)) / 2);
        let mut text_rem = pill_r.w.saturating_sub(32);
        if pill_r.w >= 232 {
            let tag = if view.persist_ok { b"PERSIST" as &[u8] } else { b"RAM" as &[u8] };
            let tag_w = measure_text(FontId::SansSemiBold, t.caption_px, tag).max(0) as usize + 22;
            if text_rem > tag_w + 64 {
                status_badge(
                    &mut c,
                    t,
                    pill_r.x + pill_r.w - tag_w - 4,
                    pill_r.y + (btn_h.saturating_sub(20)) / 2,
                    tag,
                    if view.persist_ok { StatusKind::Ok } else { StatusKind::Warn },
                );
                text_rem = text_rem.saturating_sub(tag_w + 8);
            }
        }
        c.draw_text_ellipsis(
            (pill_r.x + 26) as i32,
            p_ty,
            text_rem as i32,
            FontId::MonoRegular,
            t.caption_px,
            view.path,
            t.fg_primary,
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

    let save_opts = ButtonOpts {
        label: b"Save (Ctrl+S)",
        icon: Some(IconKind::Save),
        variant: ButtonVariant::Primary,
        disabled: false,
        selected: false,
    };
    let clear_opts = ButtonOpts::secondary(b"Clear");
    let clicked_save = button(&mut c, &mut ui, 1, save_r, save_opts);
    let clicked_clear = button(&mut c, &mut ui, 2, clear_r, clear_opts);

    let hit_pos = multiline_viewport(
        &mut c,
        &mut ui,
        3,
        body_r,
        MultiLineParams {
            lines: view.line_slices,
            cursor_line: view.cur_row,
            cursor_col: view.cur_col,
            scroll_line: view.scroll_line,
            font: FontId::MonoRegular,
            show_gutter: true,
            show_cursor: true,
        },
    );

    ui.finish();

    c.fill_rect(foot_r, t.bg_sidebar);
    c.hline(foot_r.x, foot_r.y, foot_r.w, t.border_subtle, 220);

    let cap_m = font_metrics(FontId::SansRegular, t.caption_px);
    let fy = foot_r.y as i32 + ((foot_r.h as i32 - (cap_m.ascent + cap_m.descent)) / 2);

    let mut pos_lb = LineBuilder::new();
    pos_lb.push_bytes(b"Ln ");
    pos_lb.push_u64((view.cur_row + 1) as u64);
    pos_lb.push_bytes(b", Col ");
    pos_lb.push_u64((view.cur_col + 1) as u64);
    let pos_adv = c.draw_text(
        t.sp_md as i32,
        fy,
        FontId::SansSemiBold,
        t.caption_px,
        pos_lb.as_bytes(),
        t.fg_secondary,
    );

    let badge_x = t.sp_md + pos_adv.max(0) as usize + 10;
    let badge_w = status_badge(
        &mut c,
        t,
        badge_x,
        foot_r.y + (foot_r.h.saturating_sub(20)) / 2,
        if view.dirty_doc { b"MODIFIED" } else { b"SAVED" },
        if view.dirty_doc { StatusKind::Warn } else { StatusKind::Ok },
    );

    let mut byte_lb = LineBuilder::new();
    byte_lb.push_u64(view.total_bytes as u64);
    byte_lb.push_bytes(b" B");
    let bw = measure_text(FontId::MonoRegular, t.caption_px, byte_lb.as_bytes()).max(0) as usize;
    let bx = w.saturating_sub(t.sp_md + bw);
    c.draw_text(
        bx as i32,
        fy,
        FontId::MonoRegular,
        t.caption_px,
        byte_lb.as_bytes(),
        t.fg_secondary,
    );

    let st_x = badge_x + badge_w + 12;
    let st_max = bx.saturating_sub(st_x + 10);
    if st_max > 32 && !view.status.is_empty() {
        c.draw_text_ellipsis(
            st_x as i32,
            fy,
            st_max as i32,
            FontId::SansRegular,
            t.caption_px,
            view.status,
            t.fg_muted,
        );
    }

    if clicked_save {
        EditorAction::Save
    } else if clicked_clear {
        EditorAction::Clear
    } else if let Some((row, col)) = hit_pos {
        EditorAction::MoveCursor(row, col)
    } else {
        EditorAction::None
    }
}
