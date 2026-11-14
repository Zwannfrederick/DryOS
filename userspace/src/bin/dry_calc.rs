// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-calc` (`Calculator`) — Isolated Ring 3 (`CPL=3`, `SecDomain::Userspace`, `0x0107`)
//! Native DRY Glass 2.0 Desktop & Programmer Calculator (`SharedPixelSurface` + `ui/` toolkit).

#![no_std]
#![no_main]

use dry_userspace::apps::calc_ui::{render_calc, CalcView};
use dry_userspace::apps::{app_log_line, app_poll_tsc, PixelAppWindow, PointerStep};
use dry_userspace::input::{InputEvent, InputEventKind};
use dry_userspace::keymap::{KEY_BACKSPACE, KEY_ENTER, KEY_ESC};
use dry_userspace::shell::LineBuilder;
use dry_userspace::wm_types::{WindowRole, TITLE_KIND_CALC};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_ipc_call_inline,
    sys_ipc_call_with_grant, sys_notify_create, sys_notify_wait, IPC_OP_INPUT_EVENT, IPC_OP_PONG,
    IPC_OP_SUP_APP_EXITED, IPC_OP_SUP_HELLO, IPC_OP_WIN_CLOSED, IPC_OP_WIN_POLL_EVENT,
    MSG_FLAG_NON_BLOCKING, RIGHT_READ, RIGHT_SIGNAL,
};

const CALC_SURF_VADDR: u64 = 0x1c00_0000;
const WIN_W: u16 = 360;
const WIN_H: u16 = 416;
const INIT_X: u64 = 460;
const INIT_Y: u64 = 92;
const NOTIF_SHUTDOWN_BIT: u64 = 1 << 1;

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
}

impl CalcState {
    const fn new() -> Self {
        Self {
            acc: 0,
            entry: 0,
            pending_op: None,
            entering: false,
            err_msg: None,
            expr_buf: [0; 36],
            expr_len: 0,
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
        app_log_line(&lb);
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

    fn handle_key(&mut self, ev: InputEvent) -> (bool, bool) {
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

    fn view(&self) -> CalcView<'_> {
        CalcView {
            expr: &self.expr_buf[..self.expr_len],
            pending_op_lbl: self.pending_op.map(Op::label),
            err_msg: self.err_msg,
            value: self.shown_value(),
        }
    }
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

    let init_flags = PixelAppWindow::query_theme_flags(sup_ep_cap);
    let mut win = PixelAppWindow::alloc_and_map(CALC_SURF_VADDR, WIN_W, WIN_H, init_flags)
        .unwrap_or_else(|_| sys_exit(248));
    let mut calc = CalcState::new();
    let _ = render_calc(&mut win, &calc.view(), PointerStep::default());
    win.publish_initial();
    let win_id = win.attach(
        disp_ep_cap,
        notif_cap,
        INIT_X,
        INIT_Y,
        WindowRole::Normal,
        TITLE_KIND_CALC,
        0,
    );

    let mut rlb = LineBuilder::new();
    rlb.push_bytes(b"[DRY-CALC] Ready in CPL3 Userspace win_id=");
    rlb.push_u64(win_id as u64);
    app_log_line(&rlb);

    let poll_tsc = app_poll_tsc(200_000);

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
                    if win.handle_window_config(ev) {
                        dirty = true;
                    } else if ev.kind == InputEventKind::KeyPress {
                        let (d, q) = calc.handle_key(ev);
                        dirty |= d;
                        if q {
                            exit_now = true;
                            break;
                        }
                    } else {
                        let step = win.ingest_pointer(ev);
                        if step.clicked || step.scroll_dy != 0 {
                            if let Some(btn) = render_calc(&mut win, &calc.view(), step) {
                                calc.press_btn(btn);
                            }
                            dirty = true;
                        } else if step.redraw {
                            dirty = true;
                        }
                    }
                } else {
                    break;
                }
            }
        }
        if win.sync_prefs(sup_ep_cap) {
            dirty = true;
        }
        if exit_now {
            break;
        }
        if dirty && win_id != 0 {
            let _ = render_calc(&mut win, &calc.view(), PointerStep::default());
            let _ = win.present_full(disp_ep_cap);
        }
        if let Ok(bits) = sys_notify_wait(notif_cap, 0, poll_tsc) {
            if (bits & NOTIF_SHUTDOWN_BIT) != 0 {
                break;
            }
        }
    }

    win.teardown(disp_ep_cap);
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
