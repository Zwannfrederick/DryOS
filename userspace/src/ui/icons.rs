// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Resolution-independent anti-aliased vector icons rendered via `Canvas` SDF geometry.

use crate::action::AppId;
use crate::gfx::Rect;

use super::canvas::Canvas;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconKind {
    Terminal,
    LifeStation,
    Files,
    Settings,
    Editor,
    Calculator,
    Search,
    Bell,
    Power,
    Lock,
    Close,
    Check,
    ChevronLeft,
    ChevronRight,
    ChevronDown,
    Sun,
    Moon,
    Sliders,
    Folder,
    File,
    Save,
    Plus,
    Trash,
    Shield,
    Cpu,
    Memory,
    Refresh,
    LayoutTiled,
    LayoutFloat,
    LayoutTabbed,
    Warning,
    Info,
}

impl IconKind {
    #[inline]
    pub const fn from_app(app: AppId) -> Self {
        match app {
            AppId::Terminal => Self::Terminal,
            AppId::LifeStation => Self::LifeStation,
            AppId::Files => Self::Files,
            AppId::Settings => Self::Settings,
            AppId::Editor => Self::Editor,
            AppId::Calculator => Self::Calculator,
        }
    }
}

/// Map normalized `[0..=160]` (`x * 10` on 16x16 grid) coordinate into `Q4` pixels inside `r`.
#[inline(always)]
fn gx(r: Rect, norm_x10: i32) -> i32 {
    (r.x as i32) * 16 + (norm_x10 * r.w as i32) / 10
}

#[inline(always)]
fn gy(r: Rect, norm_x10: i32) -> i32 {
    (r.y as i32) * 16 + (norm_x10 * r.h as i32) / 10
}

#[inline(always)]
fn stroke_w(r: Rect, weight_q4: i32) -> i32 {
    ((weight_q4 * r.w.min(r.h) as i32) / 16).max(16)
}

pub fn draw_vector_icon(c: &mut Canvas, r: Rect, kind: IconKind, fg: u32, accent: u32) {
    if r.w < 8 || r.h < 8 {
        return;
    }
    let sw = stroke_w(r, 15); // ~1.0px at 16px, ~1.7px at 28px
    let thin = stroke_w(r, 12);

    match kind {
        IconKind::Terminal => {
            let box_r = Rect::new(r.x + 1, r.y + 2, r.w.saturating_sub(2), r.h.saturating_sub(4));
            c.stroke_rrect_aa(box_r, (r.w / 5).max(3), fg, 220);
            c.stroke_line_aa(gx(r, 40), gy(r, 56), gx(r, 72), gy(r, 80), sw, accent, 255);
            c.stroke_line_aa(gx(r, 72), gy(r, 80), gx(r, 40), gy(r, 104), sw, accent, 255);
            c.stroke_line_aa(gx(r, 84), gy(r, 104), gx(r, 120), gy(r, 104), sw, fg, 240);
        }
        IconKind::LifeStation => {
            let box_r = Rect::new(r.x + 1, r.y + 2, r.w.saturating_sub(2), r.h.saturating_sub(4));
            c.stroke_rrect_aa(box_r, (r.w / 5).max(3), fg, 210);
            let pts = [
                (28, 86),
                (52, 86),
                (68, 44),
                (88, 118),
                (106, 70),
                (120, 86),
                (132, 86),
            ];
            for w in pts.windows(2) {
                c.stroke_line_aa(
                    gx(r, w[0].0),
                    gy(r, w[0].1),
                    gx(r, w[1].0),
                    gy(r, w[1].1),
                    sw,
                    accent,
                    255,
                );
            }
        }
        IconKind::Files | IconKind::Folder => {
            let tab = Rect::new(
                r.x + r.w / 8,
                r.y + r.h * 3 / 16,
                r.w * 5 / 16,
                r.h / 4,
            );
            c.fill_rrect_aa(tab, (r.w / 8).max(2), accent, 235);
            let body = Rect::new(
                r.x + r.w / 8,
                r.y + r.h * 5 / 16,
                r.w * 3 / 4,
                r.h * 8 / 16,
            );
            c.fill_rrect_aa(body, (r.w / 6).max(3), fg, if kind == IconKind::Files { 46 } else { 32 });
            c.stroke_rrect_aa(body, (r.w / 6).max(3), fg, 235);
            c.stroke_line_aa(gx(r, 24), gy(r, 70), gx(r, 136), gy(r, 70), thin, accent, 210);
        }
        IconKind::Settings => {
            let cx = gx(r, 80);
            let cy = gy(r, 80);
            let outer = (54 * r.w as i32) / 16;
            let inner = (24 * r.w as i32) / 16;
            c.stroke_circle_aa(cx, cy, outer, sw, fg, 245);
            c.stroke_circle_aa(cx, cy, inner, thin, accent, 255);
            let teeth = [
                (80, 12, 80, 26),
                (80, 134, 80, 148),
                (12, 80, 26, 80),
                (134, 80, 148, 80),
                (30, 30, 42, 42),
                (118, 118, 130, 130),
                (130, 30, 118, 42),
                (42, 118, 30, 130),
            ];
            for (x0, y0, x1, y1) in teeth {
                c.stroke_line_aa(gx(r, x0), gy(r, y0), gx(r, x1), gy(r, y1), sw, fg, 240);
            }
        }
        IconKind::Editor | IconKind::File => {
            let page = Rect::new(
                r.x + r.w * 3 / 16,
                r.y + r.h / 8,
                r.w * 10 / 16,
                r.h * 3 / 4,
            );
            c.stroke_rrect_aa(page, (r.w / 6).max(2), fg, 235);
            c.stroke_line_aa(gx(r, 48), gy(r, 54), gx(r, 108), gy(r, 54), thin, fg, 190);
            c.stroke_line_aa(gx(r, 48), gy(r, 80), gx(r, 94), gy(r, 80), thin, fg, 190);
            if kind == IconKind::Editor {
                c.stroke_line_aa(gx(r, 76), gy(r, 118), gx(r, 134), gy(r, 60), sw, accent, 255);
            } else {
                c.stroke_line_aa(gx(r, 48), gy(r, 106), gx(r, 108), gy(r, 106), thin, accent, 210);
            }
        }
        IconKind::Calculator => {
            let frame = Rect::new(
                r.x + r.w * 3 / 16,
                r.y + r.h / 10,
                r.w * 10 / 16,
                r.h * 4 / 5,
            );
            c.stroke_rrect_aa(frame, (r.w / 5).max(3), fg, 235);
            let disp = Rect::new(
                r.x + r.w * 5 / 16,
                r.y + r.h * 4 / 16,
                r.w * 6 / 16,
                (r.h / 7).max(2),
            );
            c.fill_rrect_aa(disp, 2, accent, 230);
            let dot_r = (14 * r.w as i32) / 16;
            c.fill_circle_aa(gx(r, 62), gy(r, 88), dot_r, fg, 230);
            c.fill_circle_aa(gx(r, 98), gy(r, 88), dot_r, fg, 230);
            c.fill_circle_aa(gx(r, 62), gy(r, 116), dot_r, fg, 230);
            c.fill_circle_aa(gx(r, 98), gy(r, 116), dot_r, accent, 255);
        }
        IconKind::Search => {
            c.stroke_circle_aa(gx(r, 70), gy(r, 70), (54 * r.w as i32) / 16, sw, fg, 250);
            c.stroke_line_aa(gx(r, 96), gy(r, 96), gx(r, 134), gy(r, 134), sw, accent, 255);
        }
        IconKind::Bell => {
            c.stroke_line_aa(gx(r, 80), gy(r, 24), gx(r, 44), gy(r, 96), sw, fg, 245);
            c.stroke_line_aa(gx(r, 80), gy(r, 24), gx(r, 116), gy(r, 96), sw, fg, 245);
            c.stroke_line_aa(gx(r, 32), gy(r, 102), gx(r, 128), gy(r, 102), sw, fg, 245);
            c.fill_circle_aa(gx(r, 80), gy(r, 126), (18 * r.w as i32) / 16, accent, 255);
        }
        IconKind::Power => {
            c.stroke_circle_aa(gx(r, 80), gy(r, 86), (44 * r.w as i32) / 16, sw, fg, 235);
            c.stroke_line_aa(gx(r, 80), gy(r, 24), gx(r, 80), gy(r, 76), sw, accent, 255);
        }
        IconKind::Lock => {
            c.stroke_circle_aa(gx(r, 80), gy(r, 56), (26 * r.w as i32) / 16, thin, accent, 245);
            let body = Rect::new(
                r.x + r.w / 4,
                r.y + r.h * 7 / 16,
                r.w / 2,
                r.h * 7 / 16,
            );
            c.fill_rrect_aa(body, (r.w / 6).max(2), fg, 235);
        }
        IconKind::Close => {
            c.stroke_line_aa(gx(r, 42), gy(r, 42), gx(r, 118), gy(r, 118), sw, fg, 250);
            c.stroke_line_aa(gx(r, 118), gy(r, 42), gx(r, 42), gy(r, 118), sw, fg, 250);
        }
        IconKind::Check => {
            c.stroke_line_aa(gx(r, 34), gy(r, 86), gx(r, 66), gy(r, 118), sw, fg, 255);
            c.stroke_line_aa(gx(r, 66), gy(r, 118), gx(r, 128), gy(r, 48), sw, fg, 255);
        }
        IconKind::ChevronLeft => {
            c.stroke_line_aa(gx(r, 102), gy(r, 38), gx(r, 58), gy(r, 80), sw, fg, 245);
            c.stroke_line_aa(gx(r, 58), gy(r, 80), gx(r, 102), gy(r, 122), sw, fg, 245);
        }
        IconKind::ChevronRight => {
            c.stroke_line_aa(gx(r, 58), gy(r, 38), gx(r, 102), gy(r, 80), sw, fg, 245);
            c.stroke_line_aa(gx(r, 102), gy(r, 80), gx(r, 58), gy(r, 122), sw, fg, 245);
        }
        IconKind::ChevronDown => {
            c.stroke_line_aa(gx(r, 40), gy(r, 58), gx(r, 80), gy(r, 102), sw, fg, 245);
            c.stroke_line_aa(gx(r, 80), gy(r, 102), gx(r, 120), gy(r, 58), sw, fg, 245);
        }
        IconKind::Sun => {
            c.fill_circle_aa(gx(r, 80), gy(r, 80), (28 * r.w as i32) / 16, accent, 255);
            for (x0, y0, x1, y1) in [
                (80, 18, 80, 34),
                (80, 126, 80, 142),
                (18, 80, 34, 80),
                (126, 80, 142, 80),
            ] {
                c.stroke_line_aa(gx(r, x0), gy(r, y0), gx(r, x1), gy(r, y1), thin, fg, 235);
            }
        }
        IconKind::Moon => {
            c.fill_circle_aa(gx(r, 76), gy(r, 82), (46 * r.w as i32) / 16, accent, 245);
            c.stroke_circle_aa(gx(r, 98), gy(r, 64), (36 * r.w as i32) / 16, sw, fg, 190);
        }
        IconKind::Sliders => {
            for (yy, kx) in [(48, 56), (80, 104), (112, 68)] {
                c.stroke_line_aa(gx(r, 24), gy(r, yy), gx(r, 136), gy(r, yy), thin, fg, 200);
                c.fill_circle_aa(gx(r, kx), gy(r, yy), (16 * r.w as i32) / 16, accent, 255);
            }
        }
        IconKind::Save => {
            let b = Rect::new(r.x + 2, r.y + 2, r.w.saturating_sub(4), r.h.saturating_sub(4));
            c.stroke_rrect_aa(b, 3, fg, 235);
            let top = Rect::new(r.x + r.w / 4, r.y + 2, r.w / 2, r.h / 4);
            c.fill_rect_alpha(top, accent, 235);
        }
        IconKind::Plus => {
            c.stroke_line_aa(gx(r, 80), gy(r, 34), gx(r, 80), gy(r, 126), sw, fg, 250);
            c.stroke_line_aa(gx(r, 34), gy(r, 80), gx(r, 126), gy(r, 80), sw, fg, 250);
        }
        IconKind::Trash => {
            c.stroke_line_aa(gx(r, 34), gy(r, 44), gx(r, 126), gy(r, 44), sw, fg, 235);
            let can = Rect::new(
                r.x + r.w * 4 / 16,
                r.y + r.h * 5 / 16,
                r.w * 8 / 16,
                r.h * 9 / 16,
            );
            c.stroke_rrect_aa(can, 2, fg, 235);
            c.stroke_line_aa(gx(r, 64), gy(r, 32), gx(r, 96), gy(r, 32), thin, accent, 240);
        }
        IconKind::Shield => {
            c.stroke_line_aa(gx(r, 80), gy(r, 22), gx(r, 130), gy(r, 44), sw, fg, 235);
            c.stroke_line_aa(gx(r, 130), gy(r, 44), gx(r, 118), gy(r, 110), sw, fg, 235);
            c.stroke_line_aa(gx(r, 118), gy(r, 110), gx(r, 80), gy(r, 140), sw, accent, 255);
            c.stroke_line_aa(gx(r, 80), gy(r, 140), gx(r, 42), gy(r, 110), sw, accent, 255);
            c.stroke_line_aa(gx(r, 42), gy(r, 110), gx(r, 30), gy(r, 44), sw, fg, 235);
            c.stroke_line_aa(gx(r, 30), gy(r, 44), gx(r, 80), gy(r, 22), sw, fg, 235);
        }
        IconKind::Cpu | IconKind::Memory => {
            let core = Rect::new(
                r.x + r.w * 4 / 16,
                r.y + r.h * 4 / 16,
                r.w * 8 / 16,
                r.h * 8 / 16,
            );
            c.stroke_rrect_aa(core, 2, fg, 235);
            c.fill_rrect_aa(
                Rect::new(r.x + r.w * 6 / 16, r.y + r.h * 6 / 16, r.w / 4, r.h / 4),
                1,
                accent,
                235,
            );
        }
        IconKind::Refresh => {
            c.stroke_circle_aa(gx(r, 80), gy(r, 80), (46 * r.w as i32) / 16, sw, fg, 235);
            c.stroke_line_aa(gx(r, 108), gy(r, 34), gx(r, 130), gy(r, 56), sw, accent, 255);
        }
        IconKind::LayoutTiled => {
            let l = Rect::new(r.x + 2, r.y + 2, r.w / 2 - 3, r.h - 4);
            let rt = Rect::new(r.x + r.w / 2 + 1, r.y + 2, r.w / 2 - 3, r.h / 2 - 3);
            let rb = Rect::new(r.x + r.w / 2 + 1, r.y + r.h / 2 + 1, r.w / 2 - 3, r.h / 2 - 3);
            c.stroke_rrect_aa(l, 2, accent, 255);
            c.stroke_rrect_aa(rt, 2, fg, 220);
            c.stroke_rrect_aa(rb, 2, fg, 220);
        }
        IconKind::LayoutFloat => {
            let a = Rect::new(r.x + 2, r.y + 4, r.w * 5 / 8, r.h * 5 / 8);
            let b = Rect::new(r.x + r.w * 3 / 8 - 1, r.y + 2, r.w * 5 / 8, r.h * 5 / 8);
            c.stroke_rrect_aa(a, 2, fg, 190);
            c.fill_bordered_rrect_aa(b, 2, 0x0016_1B24, accent);
        }
        IconKind::LayoutTabbed => {
            let t0 = Rect::new(r.x + 2, r.y + 2, r.w / 2 - 3, 4);
            c.fill_rrect_aa(t0, 1, accent, 255);
            let b = Rect::new(r.x + 2, r.y + 5, r.w - 4, r.h - 7);
            c.stroke_rrect_aa(b, 2, fg, 235);
        }
        IconKind::Warning => {
            c.stroke_line_aa(gx(r, 80), gy(r, 24), gx(r, 136), gy(r, 126), sw, accent, 255);
            c.stroke_line_aa(gx(r, 136), gy(r, 126), gx(r, 24), gy(r, 126), sw, accent, 255);
            c.stroke_line_aa(gx(r, 24), gy(r, 126), gx(r, 80), gy(r, 24), sw, accent, 255);
            c.stroke_line_aa(gx(r, 80), gy(r, 60), gx(r, 80), gy(r, 94), sw, fg, 255);
            c.fill_circle_aa(gx(r, 80), gy(r, 112), (9 * r.w as i32) / 16, fg, 255);
        }
        IconKind::Info => {
            c.stroke_circle_aa(gx(r, 80), gy(r, 80), (50 * r.w as i32) / 16, thin, accent, 240);
            c.fill_circle_aa(gx(r, 80), gy(r, 48), (10 * r.w as i32) / 16, accent, 255);
            c.stroke_line_aa(gx(r, 80), gy(r, 70), gx(r, 80), gy(r, 114), sw, fg, 255);
        }
    }
}
