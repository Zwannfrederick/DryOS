// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Shared DRY Glass 2.0 UI view for `dry-calc` (`Calculator`).

use crate::apps::{PixelAppWindow, PointerStep};
use crate::gfx::Rect;
use crate::shell::LineBuilder;
use crate::ttf::{font_metrics, measure_text, scale_px, FontId};
use crate::ui::containers::{card, status_badge};
use crate::ui::layout::{inset_rect, split_top, UiCtx};
use crate::ui::theme::StatusKind;
use crate::ui::widgets::{button, ButtonOpts, ButtonVariant};

const BTN_LABELS: [&[u8]; 20] = [
    b"C", b"+/-", b"%", b"/",
    b"7", b"8", b"9", b"*",
    b"4", b"5", b"6", b"-",
    b"1", b"2", b"3", b"+",
    b"0", b"<<", b">>", b"=",
];

pub struct CalcView<'a> {
    pub expr: &'a [u8],
    pub pending_op_lbl: Option<&'a [u8]>,
    pub err_msg: Option<&'a [u8]>,
    pub value: i64,
}

fn push_i64_val(lb: &mut LineBuilder, v: i64) {
    if v < 0 {
        lb.push_byte(b'-');
        lb.push_u64(v.unsigned_abs());
    } else {
        lb.push_u64(v as u64);
    }
}

pub fn render_calc(
    win: &mut PixelAppWindow,
    view: &CalcView<'_>,
    step: PointerStep,
) -> Option<usize> {
    let t = win.theme();
    let w = win.w as usize;
    let h = win.h as usize;

    let outer = inset_rect(Rect::new(0, 0, w, h), t.sp_md, t.sp_sm);
    let disp_h = (h * 30 / 100).clamp(112, 168);
    let (disp_r, keypad_r) = split_top(outer, disp_h, t.sp_sm);

    let mut c = win.canvas();
    c.clear(t.bg_app);

    let inner = card(&mut c, t, disp_r, None, None);
    let mut sc = c.sub_clip(inner);

    let cap_m = font_metrics(FontId::SansRegular, t.caption_px);
    let row1_h = (cap_m.line_height as usize).max(16);
    if !view.expr.is_empty() {
        sc.draw_text_ellipsis(
            inner.x as i32,
            inner.y as i32,
            inner.w.saturating_sub(48) as i32,
            FontId::SansRegular,
            t.caption_px,
            view.expr,
            t.fg_muted,
        );
    }
    if let Some(op_lbl) = view.pending_op_lbl {
        let bw = measure_text(FontId::SansSemiBold, t.caption_px, op_lbl).max(0) as usize + 22;
        status_badge(
            &mut sc,
            t,
            (inner.x + inner.w).saturating_sub(bw),
            inner.y.saturating_sub(2),
            op_lbl,
            StatusKind::Accent,
        );
    }

    let mut val_lb = LineBuilder::new();
    if let Some(err) = view.err_msg {
        val_lb.push_bytes(err);
    } else {
        push_i64_val(&mut val_lb, view.value);
    }
    let vbytes = val_lb.as_bytes();
    let show = if vbytes.len() > 20 { &vbytes[(vbytes.len() - 20)..] } else { vbytes };

    let mut val_px = scale_px(26, t.dpi_pct).min(34);
    let mut vw = measure_text(FontId::SansSemiBold, val_px, show).max(0) as usize;
    if vw > inner.w && val_px > 18 {
        val_px = 18;
        vw = measure_text(FontId::SansSemiBold, val_px, show).max(0) as usize;
    }
    let val_m = font_metrics(FontId::SansSemiBold, val_px);
    let val_y = inner.y + row1_h + 2;
    let vx = (inner.x + inner.w).saturating_sub(vw).max(inner.x) as i32;
    sc.draw_text_ellipsis(
        vx,
        val_y as i32,
        inner.w as i32,
        FontId::SansSemiBold,
        val_px,
        show,
        if view.err_msg.is_some() { t.danger_fg } else { t.fg_primary },
    );

    let div_y = val_y + (val_m.line_height as usize).max(24) + 4;
    if div_y + 18 <= inner.y + inner.h {
        sc.hline(inner.x, div_y, inner.w, t.border_subtle, 170);
        let mono_sz = t.caption_px.max(10);
        let mono_m = font_metrics(FontId::MonoRegular, mono_sz);
        let hex_y = div_y + 5;

        let raw_u = view.value as u64;
        let mut hb = LineBuilder::new();
        hb.push_bytes(b"HEX 0x");
        hb.push_hex_u64(raw_u);
        sc.draw_text_ellipsis(
            inner.x as i32,
            hex_y as i32,
            inner.w as i32,
            FontId::MonoRegular,
            mono_sz,
            hb.as_bytes(),
            t.accent_fg,
        );

        let bin_y = hex_y + (mono_m.line_height as usize).max(13) + 2;
        if bin_y + (mono_m.line_height as usize) <= inner.y + inner.h + 2 {
            let mut bb = LineBuilder::new();
            bb.push_bytes(b"BIN ");
            for bit in (0..16).rev() {
                bb.push_byte(if ((raw_u >> bit) & 1) != 0 { b'1' } else { b'0' });
                if bit > 0 && (bit & 3) == 0 {
                    bb.push_byte(b' ');
                }
            }
            sc.draw_text_ellipsis(
                inner.x as i32,
                bin_y as i32,
                inner.w as i32,
                FontId::MonoRegular,
                mono_sz,
                bb.as_bytes(),
                t.fg_secondary,
            );
        }
    }

    let gap = t.sp_sm;
    let col_w = keypad_r.w.saturating_sub(3 * gap) / 4;
    let row_h = keypad_r.h.saturating_sub(4 * gap) / 5;
    let mut ui = UiCtx::begin(
        &mut win.persist,
        t,
        step.clicked,
        step.released,
        step.scroll_dy,
        None,
    );
    let mut pressed_btn = None;

    for (i, &lbl) in BTN_LABELS.iter().enumerate() {
        let col = i % 4;
        let row = i / 4;
        let bw = if col == 3 {
            keypad_r.x + keypad_r.w - (keypad_r.x + col * (col_w + gap))
        } else {
            col_w
        };
        let bh = if row == 4 {
            keypad_r.y + keypad_r.h - (keypad_r.y + row * (row_h + gap))
        } else {
            row_h
        };
        let br = Rect::new(
            keypad_r.x + col * (col_w + gap),
            keypad_r.y + row * (row_h + gap),
            bw.max(20),
            bh.max(20),
        );
        let variant = match i {
            19 => ButtonVariant::Primary,
            0 => ButtonVariant::Danger,
            1 | 2 | 3 | 7 | 11 | 15 | 17 | 18 => ButtonVariant::Subtle,
            _ => ButtonVariant::Secondary,
        };
        let opts = ButtonOpts {
            label: lbl,
            icon: None,
            variant,
            disabled: false,
            selected: false,
        };
        if button(&mut c, &mut ui, 10 + i as u16, br, opts) {
            pressed_btn = Some(i);
        }
    }

    ui.finish();
    pressed_btn
}
