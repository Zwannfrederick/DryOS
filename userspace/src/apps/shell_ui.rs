// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Shared DRY Glass 2.0 UI view for `dry-shell` (`DRY Terminal`).

use crate::apps::PixelAppWindow;
use crate::gfx::Rect;
use crate::shell::{LineBuilder, ShellEngine, PROMPT_STR};
use crate::surface::{
    SharedWindowSurface, SurfaceCell, STYLE_ACCENT, STYLE_HEADER, STYLE_MUTED, STYLE_OK,
    STYLE_PROMPT, STYLE_WARN, TERM_COLS,
};
use crate::ttf::{font_metrics, measure_text, x_at_byte_index, FontId};
use crate::ui::containers::status_badge;
use crate::ui::icons::{draw_vector_icon, IconKind};
use crate::ui::layout::{inset_rect, split_top};
use crate::ui::theme::{StatusKind, UiTheme};
use crate::ui::widgets::scrollbar_v;
use crate::UserSysInfo;

pub const SHELL_PIXEL_VADDR: u64 = 0x1c00_0000;
pub const SHELL_WIN_W: u16 = 560;
pub const SHELL_WIN_H: u16 = 352;

pub enum ShellBackend {
    Legacy4K {
        vaddr: u64,
        cap: u64,
        surf: &'static SharedWindowSurface,
        cols: usize,
        rows: usize,
    },
    Pixel2M {
        win: PixelAppWindow,
    },
}

impl ShellBackend {
    #[inline]
    pub fn current_seq(&self) -> u32 {
        match self {
            Self::Legacy4K { surf, .. } => surf.current_seq(),
            Self::Pixel2M { win } => win.current_seq(),
        }
    }
}

fn style_color(t: UiTheme, style: u8) -> (FontId, u32) {
    match style {
        STYLE_PROMPT | STYLE_ACCENT => (FontId::MonoSemiBold, t.accent_fg),
        STYLE_MUTED => (FontId::MonoRegular, t.fg_muted),
        STYLE_OK => (FontId::MonoRegular, t.ok_fg),
        STYLE_WARN => (FontId::MonoSemiBold, t.warn_fg),
        STYLE_HEADER => (FontId::MonoSemiBold, t.fg_primary),
        _ => (FontId::MonoRegular, t.fg_primary),
    }
}

pub fn viewport_rows(t: UiTheme, h: usize) -> usize {
    let head_h = (t.ctrl_h).clamp(28, 36);
    let vp_h = h.saturating_sub(head_h + t.sp_sm * 2 + 16);
    let m = font_metrics(FontId::MonoRegular, t.mono_px);
    let line_h = (m.line_height + 2).max(15) as usize;
    (vp_h / line_h).max(2)
}

pub fn render_shell_pixel(
    win: &mut PixelAppWindow,
    engine: &ShellEngine,
    sys: Option<&UserSysInfo>,
) {
    let t = win.theme();
    let w = win.w as usize;
    let h = win.h as usize;

    let head_h = (t.ctrl_h).clamp(28, 36);
    let (head_r, body_outer) = split_top(Rect::new(0, 0, w, h), head_h, 0);
    let vp_r = inset_rect(body_outer, t.sp_sm + 2, t.sp_sm);

    let mut c = win.canvas();
    c.clear(t.bg_app);

    // Top context strip
    c.fill_rect(head_r, t.bg_sidebar);
    c.hline(head_r.x, head_r.y + head_r.h.saturating_sub(1), head_r.w, t.border_subtle, 220);

    let cap_m = font_metrics(FontId::SansSemiBold, t.caption_px);
    let h_ty = head_r.y as i32 + ((head_r.h as i32 - (cap_m.ascent + cap_m.descent)) / 2);
    draw_vector_icon(
        &mut c,
        Rect::new(
            head_r.x + 12,
            head_r.y + (head_r.h.saturating_sub(14)) / 2,
            14,
            14,
        ),
        IconKind::Terminal,
        t.fg_primary,
        t.accent_fg,
    );
    let title_adv = c.draw_text(
        (head_r.x + 32) as i32,
        h_ty,
        FontId::SansSemiBold,
        t.caption_px,
        b"DRY Terminal",
        t.fg_primary,
    );

    if let Some(s) = sys {
        let mut info_lb = LineBuilder::new();
        info_lb.push_bytes(b"cpu:");
        info_lb.push_u64(s.online_cpus as u64);
        info_lb.push_bytes(b"  sys:");
        info_lb.push_u64(s.total_syscalls);
        let ix = head_r.x + 40 + title_adv.max(0) as usize;
        let rem = (w / 2).saturating_sub(ix);
        if rem > 40 {
            c.draw_text_ellipsis(
                ix as i32,
                h_ty,
                rem as i32,
                FontId::MonoRegular,
                t.caption_px,
                info_lb.as_bytes(),
                t.fg_muted,
            );
        }
    }

    let mut cmd_lb = LineBuilder::new();
    cmd_lb.push_bytes(b"CMD #");
    cmd_lb.push_u64(engine.cmd_count as u64);
    let cmd_w = measure_text(FontId::SansSemiBold, t.caption_px, cmd_lb.as_bytes()).max(0) as usize + 22;

    let mut scr_lb = LineBuilder::new();
    let (scr_text, scr_kind) = if engine.scroll_up() > 0 {
        scr_lb.push_bytes(b"SCROLL +");
        scr_lb.push_u64(engine.scroll_up() as u64);
        (scr_lb.as_bytes(), StatusKind::Warn)
    } else {
        (b"LIVE" as &[u8], StatusKind::Ok)
    };
    let scr_w = measure_text(FontId::SansSemiBold, t.caption_px, scr_text).max(0) as usize + 22;

    let badge_y = head_r.y + (head_r.h.saturating_sub(20)) / 2;
    let cmd_x = w.saturating_sub(12 + cmd_w);
    status_badge(&mut c, t, cmd_x, badge_y, cmd_lb.as_bytes(), StatusKind::Neutral);
    if cmd_x > scr_w + 180 {
        status_badge(&mut c, t, cmd_x - scr_w - 8, badge_y, scr_text, scr_kind);
    }

    // Recessed terminal card
    c.fill_bordered_rrect_aa(vp_r, t.r_md, t.bg_input, t.border_subtle);
    let inner = Rect::new(
        vp_r.x + 10,
        vp_r.y + 8,
        vp_r.w.saturating_sub(24),
        vp_r.h.saturating_sub(16),
    );

    let mono_sz = t.mono_px;
    let m = font_metrics(FontId::MonoRegular, mono_sz);
    let line_h = (m.line_height + 2).max(15) as usize;
    let rows = (inner.h / line_h).max(2);
    let hist_viewport = rows - 1;

    let total_count = engine.count();
    let end_idx = total_count.saturating_sub(engine.scroll_up());
    let visible_hist = end_idx.min(hist_viewport);
    let start_idx = end_idx - visible_hist;
    let prompt_row = visible_hist;

    {
        let mut sc = c.sub_clip(inner);
        for r in 0..visible_hist {
            let row_cells = engine.history_row(start_idx + r);
            let ry = (inner.y + r * line_h + 1) as i32;
            render_cell_row(&mut sc, t, inner.x as i32, ry, inner.w, mono_sz, row_cells);
        }

        // Active prompt + cursor row
        let py = inner.y + prompt_row * line_h;
        sc.fill_rect_alpha(
            Rect::new(inner.x.saturating_sub(4), py, inner.w + 8, line_h),
            t.accent_subtle,
            95,
        );
        let p_adv = sc.draw_text(
            inner.x as i32,
            (py + 1) as i32,
            FontId::MonoSemiBold,
            mono_sz,
            PROMPT_STR,
            t.accent_fg,
        );
        let inp_x = inner.x as i32 + p_adv.max(0);
        let inp_bytes = engine.editor.as_bytes();
        let inp_max = (inner.x + inner.w).saturating_sub(inp_x.max(0) as usize) as i32;
        sc.draw_text_ellipsis(
            inp_x,
            (py + 1) as i32,
            inp_max,
            FontId::MonoRegular,
            mono_sz,
            inp_bytes,
            t.fg_primary,
        );
        let cur_dx = x_at_byte_index(
            FontId::MonoRegular,
            mono_sz,
            inp_bytes,
            engine.editor.cursor(),
        );
        let cx = (inp_x + cur_dx).clamp(inner.x as i32, (inner.x + inner.w - 3) as i32) as usize;
        sc.fill_rect(Rect::new(cx, py + 1, 2, line_h.saturating_sub(2)), t.accent_fg);
    }

    let sb_r = Rect::new(
        vp_r.x + vp_r.w.saturating_sub(10),
        vp_r.y + 6,
        6,
        vp_r.h.saturating_sub(12),
    );
    scrollbar_v(
        &mut c,
        sb_r,
        total_count,
        hist_viewport,
        start_idx,
        t.bg_card_alt,
        t.border_strong,
    );
}

fn render_cell_row(
    sc: &mut crate::ui::canvas::Canvas,
    t: UiTheme,
    x0: i32,
    y: i32,
    max_w: usize,
    sz: u8,
    cells: &[SurfaceCell; TERM_COLS],
) {
    let mut trimmed = TERM_COLS;
    while trimmed > 0 && (cells[trimmed - 1].ch == b' ' || cells[trimmed - 1].ch == 0) {
        trimmed -= 1;
    }
    if trimmed == 0 {
        return;
    }

    let mut x = x0;
    let right_limit = x0 + max_w as i32;
    let mut i = 0usize;
    let mut run_buf = [0u8; TERM_COLS];

    while i < trimmed && x < right_limit {
        let st = cells[i].style;
        let mut j = i;
        while j < trimmed && cells[j].style == st {
            let ch = cells[j].ch;
            run_buf[j - i] = if (0x20..=0x7e).contains(&ch) { ch } else { b' ' };
            j += 1;
        }
        let (font, rgb) = style_color(t, st);
        let adv = sc.draw_text_ellipsis(
            x,
            y,
            right_limit - x,
            font,
            sz,
            &run_buf[..(j - i)],
            rgb,
        );
        x += adv.max(0);
        i = j;
    }
}
