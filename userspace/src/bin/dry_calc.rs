// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-calc` (`Calculator`) — Isolated Ring 3 (`CPL=3`, `SecDomain::Userspace`, `0x0107`)
//! Native Pixel-Surface Desktop & Programmer Calculator (`SharedPixelSurface` + `PixelCanvas`).

#![no_std]
#![no_main]

use core::slice::from_raw_parts_mut;

use dry_userspace::gfx::Rect;
use dry_userspace::glass::app_kit::{AppPalette, PixelCanvas};
use dry_userspace::input::{InputEvent, InputEventKind, MOUSE_BTN_LEFT};
use dry_userspace::keymap::{KEY_BACKSPACE, KEY_ENTER, KEY_ESC};
use dry_userspace::shell::LineBuilder;
use dry_userspace::sup_services::SupervisorPrefs;
use dry_userspace::surface::{
    init_pixel_surface_rw, publish_pixel_rect_rw, PIXEL_SURF_HEADER_BYTES, SURF_ATTACH_FLAG_PIXEL,
};
use dry_userspace::wm::{WindowRole, TITLE_KIND_CALC};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_frame_alloc,
    sys_ipc_call_inline, sys_ipc_call_with_grant, sys_notify_create, sys_notify_wait,
    sys_sysinfo, sys_vspace_map, sys_write_debug, UserSysInfo, IPC_OP_INPUT_EVENT, IPC_OP_PONG,
    IPC_OP_SUP_APP_EXITED, IPC_OP_SUP_HELLO, IPC_OP_SUP_QUERY_PREFS, IPC_OP_SURFACE_ATTACH,
    IPC_OP_SURFACE_PRESENT, IPC_OP_WIN_BIND_NOTIF, IPC_OP_WIN_CLOSE, IPC_OP_WIN_CLOSED,
    IPC_OP_WIN_POLL_EVENT, MSG_FLAG_NON_BLOCKING, RIGHT_MAP, RIGHT_READ, RIGHT_SIGNAL,
    VSPACE_MAP_OP_2M_RW, VSPACE_MAP_OP_UNMAP_2M,
};

const CALC_SURF_VADDR: u64 = 0x1c00_0000;
const WIN_W: u16 = 360;
const WIN_H: u16 = 416;
const INIT_X: u64 = 460;
const INIT_Y: u64 = 92;
const NOTIF_SHUTDOWN_BIT: u64 = 1 << 1;

const DISP_CARD: Rect = Rect::new(14, 12, 332, 108);
const BTN_LABELS: [&[u8]; 20] = [
    b"C", b"+/-", b"%", b"/",
    b"7", b"8", b"9", b"*",
    b"4", b"5", b"6", b"-",
    b"1", b"2", b"3", b"+",
    b"0", b"<<", b">>", b"=",
];

#[inline]
fn btn_rect(idx: usize) -> Rect {
    let col = idx % 4;
    let row = idx / 4;
    Rect::new(14 + col * 85, 132 + row * 56, 77, 48)
}

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 160];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

fn push_i64(lb: &mut LineBuilder, v: i64) {
    if v < 0 {
        lb.push_byte(b'-');
        lb.push_u64(v.unsigned_abs());
    } else {
        lb.push_u64(v as u64);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
}

impl Op {
    const fn label(self) -> &'static [u8] {
        match self {
            Self::Add => b"+",
            Self::Sub => b"-",
            Self::Mul => b"*",
            Self::Div => b"/",
            Self::Rem => b"%",
            Self::Shl => b"<<",
            Self::Shr => b">>",
        }
    }

    const fn ch(self) -> u8 {
        match self {
            Self::Add => b'+',
            Self::Sub => b'-',
            Self::Mul => b'*',
            Self::Div => b'/',
            Self::Rem => b'%',
            Self::Shl => b'<',
            Self::Shr => b'>',
        }
    }
}

struct CalcState {
    acc: i64,
    entry: i64,
    pending_op: Option<Op>,
    entering: bool,
    err_msg: Option<&'static [u8]>,
    expr_buf: [u8; 36],
    expr_len: usize,
    light_mode: bool,
}

impl CalcState {
    const fn new(light_mode: bool) -> Self {
        Self {
            acc: 0,
            entry: 0,
            pending_op: None,
            entering: false,
            err_msg: None,
            expr_buf: [0; 36],
            expr_len: 0,
            light_mode,
        }
    }

    fn set_expr(&mut self, lhs: i64, op: Op, rhs: Option<i64>) {
        let mut lb = LineBuilder::new();
        push_i64(&mut lb, lhs);
        lb.push_byte(b' ');
        lb.push_bytes(op.label());
        if let Some(r) = rhs {
            lb.push_byte(b' ');
            push_i64(&mut lb, r);
            lb.push_bytes(b" =");
        }
        let s = lb.as_bytes();
        let n = s.len().min(self.expr_buf.len());
        self.expr_buf[..n].copy_from_slice(&s[..n]);
        self.expr_len = n;
    }

    fn clear(&mut self) {
        self.acc = 0;
        self.entry = 0;
        self.pending_op = None;
        self.entering = false;
        self.err_msg = None;
        self.expr_len = 0;
    }

    fn push_digit(&mut self, d: i64) {
        self.err_msg = None;
        let cur = if self.entering { self.entry } else { 0 };
        let neg = cur < 0;
        let next = cur
            .checked_mul(10)
            .and_then(|v| if neg { v.checked_sub(d) } else { v.checked_add(d) });
        if let Some(v) = next {
            self.entry = v;
            self.entering = true;
        }
    }

    fn backspace(&mut self) {
        if self.entering {
            self.entry /= 10;
        }
    }

    fn toggle_sign(&mut self) {
        self.err_msg = None;
        if self.entering {
            if let Some(v) = self.entry.checked_neg() {
                self.entry = v;
            }
        } else if let Some(v) = self.acc.checked_neg() {
            self.acc = v;
            self.entry = v;
        }
    }

    fn apply_op(&mut self, op: Op, rhs: i64) -> Result<i64, &'static [u8]> {
        match op {
            Op::Add => self.acc.checked_add(rhs).ok_or(b"ERR: OVFL" as &[u8]),
            Op::Sub => self.acc.checked_sub(rhs).ok_or(b"ERR: OVFL" as &[u8]),
            Op::Mul => self.acc.checked_mul(rhs).ok_or(b"ERR: OVFL" as &[u8]),
            Op::Div => {
                if rhs == 0 {
                    Err(b"ERR: DIV0" as &[u8])
                } else {
                    self.acc.checked_div(rhs).ok_or(b"ERR: OVFL" as &[u8])
                }
            }
            Op::Rem => {
                if rhs == 0 {
                    Err(b"ERR: DIV0" as &[u8])
                } else {
                    self.acc.checked_rem(rhs).ok_or(b"ERR: OVFL" as &[u8])
                }
            }
            Op::Shl => self.acc.checked_shl((rhs.clamp(0, 63)) as u32).ok_or(b"ERR: OVFL" as &[u8]),
            Op::Shr => self.acc.checked_shr((rhs.clamp(0, 63)) as u32).ok_or(b"ERR: OVFL" as &[u8]),
        }
    }

    fn press_op(&mut self, op: Op) {
        self.err_msg = None;
        if let Some(prev) = self.pending_op {
            if self.entering {
                match self.apply_op(prev, self.entry) {
                    Ok(v) => self.acc = v,
                    Err(e) => {
                        self.err_msg = Some(e);
                        self.pending_op = None;
                        self.entering = false;
                        return;
                    }
                }
            }
        } else if self.entering {
            self.acc = self.entry;
        }
        self.pending_op = Some(op);
        self.entering = false;
        self.set_expr(self.acc, op, None);
    }

    fn eval(&mut self) {
        self.err_msg = None;
        let op_ch = if let Some(op) = self.pending_op {
            let rhs = if self.entering { self.entry } else { self.acc };
            let lhs = self.acc;
            match self.apply_op(op, rhs) {
                Ok(v) => {
                    self.set_expr(lhs, op, Some(rhs));
                    self.acc = v;
                    self.entry = v;
                    op.ch()
                }
                Err(e) => {
                    self.err_msg = Some(e);
                    self.pending_op = None;
                    self.entering = false;
                    return;
                }
            }
        } else {
            if self.entering {
                self.acc = self.entry;
            }
            b'='
        };
        self.pending_op = None;
        self.entering = false;

        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-CALC] EVAL op=");
        lb.push_byte(op_ch);
        lb.push_bytes(b" res=");
        push_i64(&mut lb, self.acc);
        log_line(&lb);
    }

    fn shown_value(&self) -> i64 {
        if self.entering { self.entry } else { self.acc }
    }

    fn press_btn(&mut self, idx: usize) {
        match idx {
            0 => self.clear(),
            1 => self.toggle_sign(),
            2 => self.press_op(Op::Rem),
            3 => self.press_op(Op::Div),
            4 => self.push_digit(7),
            5 => self.push_digit(8),
            6 => self.push_digit(9),
            7 => self.press_op(Op::Mul),
            8 => self.push_digit(4),
            9 => self.push_digit(5),
            10 => self.push_digit(6),
            11 => self.press_op(Op::Sub),
            12 => self.push_digit(1),
            13 => self.push_digit(2),
            14 => self.push_digit(3),
            15 => self.press_op(Op::Add),
            16 => self.push_digit(0),
            17 => self.press_op(Op::Shl),
            18 => self.press_op(Op::Shr),
            19 => self.eval(),
            _ => {}
        }
    }

    fn handle_event(&mut self, ev: InputEvent) -> (bool, bool) {
        if ev.kind == InputEventKind::MouseButtonDown && (ev.scancode as u8) == MOUSE_BTN_LEFT {
            let mx = ev.dx.max(0) as usize;
            let my = ev.dy.max(0) as usize;
            for i in 0..20 {
                if btn_rect(i).contains(mx, my) {
                    self.press_btn(i);
                    return (true, false);
                }
            }
            return (false, false);
        }
        if ev.kind != InputEventKind::KeyPress {
            return (false, false);
        }
        if ev.keycode == KEY_ESC || ev.ascii == b'q' || ev.ascii == b'Q' {
            return (false, true);
        }
        if ev.keycode == KEY_ENTER || ev.ascii == b'=' {
            self.eval();
            return (true, false);
        }
        if ev.keycode == KEY_BACKSPACE {
            self.backspace();
            return (true, false);
        }
        match ev.ascii {
            b'0'..=b'9' => self.push_digit((ev.ascii - b'0') as i64),
            b'+' => self.press_op(Op::Add),
            b'-' => self.press_op(Op::Sub),
            b'*' => self.press_op(Op::Mul),
            b'/' => self.press_op(Op::Div),
            b'%' => self.press_op(Op::Rem),
            b'<' => self.press_op(Op::Shl),
            b'>' => self.press_op(Op::Shr),
            b'c' | b'C' => self.clear(),
            _ => return (false, false),
        }
        (true, false)
    }

    fn render(&self, pix: &mut [u32]) {
        let pal = AppPalette::for_mode(self.light_mode);
        let mut c = PixelCanvas::new(pix, WIN_W as usize, WIN_H as usize);
        c.clear(pal.bg);
        c.fill_rounded_card(DISP_CARD, 8, pal.surface, pal.border);

        c.text(26, 20, &self.expr_buf[..self.expr_len], pal.fg_muted, None);
        if let Some(op) = self.pending_op {
            c.text(320, 20, op.label(), pal.accent, None);
        }

        let mut val_lb = LineBuilder::new();
        if let Some(err) = self.err_msg {
            val_lb.push_bytes(err);
        } else {
            push_i64(&mut val_lb, self.shown_value());
        }
        let vbytes = val_lb.as_bytes();
        let show = if vbytes.len() > 18 { &vbytes[(vbytes.len() - 18)..] } else { vbytes };
        let vx = 334usize.saturating_sub(show.len() * 16).max(24);
        c.text_2x(
            vx,
            40,
            show,
            if self.err_msg.is_some() { pal.warn } else { pal.fg },
        );

        let raw_u = self.shown_value() as u64;
        let mut hb = LineBuilder::new();
        hb.push_bytes(b"HEX 0x");
        hb.push_hex_u64(raw_u);
        c.text(26, 80, hb.as_bytes(), pal.accent, None);

        let mut bb = LineBuilder::new();
        bb.push_bytes(b"BIN ");
        for bit in (0..16).rev() {
            bb.push_byte(if ((raw_u >> bit) & 1) != 0 { b'1' } else { b'0' });
            if bit > 0 && (bit & 3) == 0 {
                bb.push_byte(b' ');
            }
        }
        c.text(26, 98, bb.as_bytes(), pal.fg_muted, None);

        for (i, &lbl) in BTN_LABELS.iter().enumerate() {
            let r = btn_rect(i);
            let (fill, fg, bdr) = match i {
                19 => (pal.btn_accent, pal.fg_on_accent, pal.accent),
                0 => (pal.surface_alt, pal.warn, pal.border),
                2 | 3 | 7 | 11 | 15 | 17 | 18 => (pal.surface_alt, pal.accent, pal.border),
                _ => (pal.btn_bg, pal.fg, pal.border),
            };
            c.button(r, lbl, fill, fg, bdr);
        }
    }
}

fn query_light_mode(sup_ep: u64) -> bool {
    if let Ok((rtag, _, rmrs)) = sys_ipc_call_inline(
        sup_ep,
        pack_msg_tag(IPC_OP_SUP_QUERY_PREFS, 0, 1, 0),
        [0; 4],
        0,
    ) {
        if msg_tag_label(rtag) == IPC_OP_PONG {
            let p = SupervisorPrefs::unpack_mr0(rmrs[0]);
            return (p.theme_flags & 4) != 0 && (p.theme_flags & 1) == 0;
        }
    }
    false
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(sup_ep_cap: u64, _arg1: u64) -> ! {
    let (cs, _) = read_cs_ss();
    if (cs & 3) != 3 || sup_ep_cap == 0 {
        sys_exit(245);
    }
    let notif_cap = sys_notify_create(0).unwrap_or_else(|_| sys_exit(246));
    let disp_ep_cap = match sys_ipc_call_with_grant(
        sup_ep_cap,
        IPC_OP_SUP_HELLO,
        [0x0107, 0, 0, 0],
        notif_cap,
        RIGHT_READ | RIGHT_SIGNAL,
        0,
        0,
    ) {
        Ok((t, cap, _)) if msg_tag_label(t) == IPC_OP_PONG && cap != 0 => cap,
        _ => sys_exit(247),
    };

    let surf_cap = sys_frame_alloc(1).unwrap_or_else(|_| sys_exit(248));
    if sys_vspace_map(surf_cap, CALC_SURF_VADDR, VSPACE_MAP_OP_2M_RW).is_err() {
        sys_exit(249);
    }
    // SAFETY: `CALC_SURF_VADDR` is a freshly mapped `2 MiB` `USER_RW` frame.
    unsafe { init_pixel_surface_rw(CALC_SURF_VADDR, WIN_W, WIN_H) };
    // SAFETY: Pixel plane starts after the 64-byte header and covers `WIN_W * WIN_H` `u32`s.
    let pix = unsafe {
        from_raw_parts_mut(
            (CALC_SURF_VADDR + PIXEL_SURF_HEADER_BYTES as u64) as *mut u32,
            (WIN_W as usize) * (WIN_H as usize),
        )
    };

    let mut calc = CalcState::new(query_light_mode(sup_ep_cap));
    calc.render(pix);
    // SAFETY: `CALC_SURF_VADDR` initialized above.
    unsafe { publish_pixel_rect_rw(CALC_SURF_VADDR, 0, 0, WIN_W, WIN_H) };

    let attach_mrs = [
        (WIN_W as u64) | ((WIN_H as u64) << 16),
        INIT_X | (INIT_Y << 32),
        (WindowRole::Normal as u64) | ((TITLE_KIND_CALC as u64) << 8) | SURF_ATTACH_FLAG_PIXEL,
        0,
    ];
    let mut win_id = 0u32;
    if let Ok((rtag, _, rmrs)) = sys_ipc_call_with_grant(
        disp_ep_cap,
        IPC_OP_SURFACE_ATTACH,
        attach_mrs,
        surf_cap,
        RIGHT_READ | RIGHT_MAP,
        0,
        0,
    ) {
        if msg_tag_label(rtag) == IPC_OP_PONG {
            win_id = rmrs[0] as u32;
        }
    }
    if win_id != 0 {
        let _ = sys_ipc_call_with_grant(
            disp_ep_cap,
            IPC_OP_WIN_BIND_NOTIF,
            [win_id as u64, 0, 0, 0],
            notif_cap,
            RIGHT_READ | RIGHT_SIGNAL,
            0,
            0,
        );
    }

    let mut rlb = LineBuilder::new();
    rlb.push_bytes(b"[DRY-CALC] Ready in CPL3 Userspace win_id=");
    rlb.push_u64(win_id as u64);
    log_line(&rlb);

    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    let poll_tsc = 200_000u64.saturating_mul(sys.tsc_mhz.max(1000));

    loop {
        let mut dirty = false;
        let mut exit_now = false;
        if win_id != 0 {
            while let Ok((ptag, _, pmrs)) = sys_ipc_call_inline(
                disp_ep_cap,
                pack_msg_tag(IPC_OP_WIN_POLL_EVENT, 0, 1, 0),
                [win_id as u64, 0, 0, 0],
                0,
            ) {
                let plbl = msg_tag_label(ptag);
                if plbl == IPC_OP_WIN_CLOSED {
                    exit_now = true;
                    break;
                }
                if plbl == IPC_OP_INPUT_EVENT {
                    let (ev, _) = InputEvent::from_ipc_mrs(pmrs);
                    let (d, q) = calc.handle_event(ev);
                    dirty |= d;
                    if q {
                        exit_now = true;
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        let next_light = query_light_mode(sup_ep_cap);
        if next_light != calc.light_mode {
            calc.light_mode = next_light;
            dirty = true;
        }
        if exit_now {
            break;
        }
        if dirty && win_id != 0 {
            calc.render(pix);
            // SAFETY: `CALC_SURF_VADDR` remains mapped RW until teardown.
            unsafe { publish_pixel_rect_rw(CALC_SURF_VADDR, 0, 0, WIN_W, WIN_H) };
            let _ = sys_ipc_call_inline(
                disp_ep_cap,
                pack_msg_tag(IPC_OP_SURFACE_PRESENT, 0, 1, 0),
                [win_id as u64, 0, 0, 0],
                0,
            );
        }
        if let Ok(bits) = sys_notify_wait(notif_cap, 0, poll_tsc) {
            if (bits & NOTIF_SHUTDOWN_BIT) != 0 {
                break;
            }
        }
    }

    if win_id != 0 {
        let _ = sys_ipc_call_inline(
            disp_ep_cap,
            pack_msg_tag(IPC_OP_WIN_CLOSE, 0, 1, 0),
            [win_id as u64, 0, 0, 0],
            0,
        );
    }
    let _ = sys_vspace_map(surf_cap, CALC_SURF_VADDR, VSPACE_MAP_OP_UNMAP_2M);
    let _ = sys_cap_revoke(surf_cap, 2);
    let _ = sys_cap_revoke(disp_ep_cap, 1);
    let _ = sys_cap_revoke(notif_cap, 2);
    let _ = sys_ipc_call_inline(
        sup_ep_cap,
        pack_msg_tag(IPC_OP_SUP_APP_EXITED, MSG_FLAG_NON_BLOCKING, 1, 0),
        [0x0107, 0, 0, 0],
        0,
    );
    sys_exit(0);
}
