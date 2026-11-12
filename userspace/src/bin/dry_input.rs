// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-input` — Isolated Ring 3 (`CPL=3`) Unified PS/2 Keyboard (`IRQ1`) & Mouse (`IRQ12`) Driver.
//!
//! Enforces **Single i8042 Controller Ownership** in `SecDomain::RootSystem` (`compartment_id = 0x0003`,
//! `RFLAGS.IOPL == 0`): exclusively owns `IoPortCap([0x0060, 0x0064])`, `irq1_cap` (`1 << 1`), and
//! optional `irq12_cap` (`1 << 12`) bound to one `Notification` (`notif_cap`).
//!
//! Operates in two sink modes:
//! - Direct consumer pull (`IPC_OP_INPUT_WAIT`, used in Milestone 2A-1 P1 & `-vga none` headless).
//! - Window Manager push (`IPC_OP_WM_INPUT_EVENT` over `BADGE_INPUT_SVC` `disp_ep`, Milestone 2A-2).

#![no_std]
#![no_main]

use core::arch::asm;
use dry_userspace::input::{
    InputEvent, InputEventKind, InputRingQueue, DEV_ID_SYNTH_TEST,
};
use dry_userspace::keymap::KeymapLayout;
use dry_userspace::ps2::{init_i8042_keyboard, init_i8042_mouse, Ps2Decoder};
use dry_userspace::shell::LineBuilder;
use dry_userspace::{
    msg_tag_label, pack_msg_tag, rdtsc_user, read_cs_ss, sys_cap_revoke, sys_exit,
    sys_ipc_call_inline, sys_ipc_recv_ext, sys_ipc_reply_inline, sys_irq_ack, sys_irq_bind,
    sys_notify_create, sys_notify_wait, sys_write_debug, UserIpcRecvExt, BADGE_SUPERVISOR, EAGAIN,
    EBADCAP, EPIPE, EREVOKED, IPC_OP_CRASH_UD2, IPC_OP_ERR_INVAL, IPC_OP_ERR_PERM,
    IPC_OP_EXIT_CLEAN, IPC_OP_INPUT_EVENT, IPC_OP_INPUT_INIT_MOUSE, IPC_OP_INPUT_INIT_PORT,
    IPC_OP_INPUT_OVERFLOW_TEST, IPC_OP_INPUT_QUERY_STATS, IPC_OP_INPUT_SET_PREFS,
    IPC_OP_INPUT_SET_WM_SINK, IPC_OP_INPUT_WAIT, IPC_OP_PONG, IPC_OP_WM_INPUT_EVENT, IRQF_ONESHOT,
    IRQ_ACK_MASK, IRQ_ACK_UNBIND, IRQ_ACK_UNMASK, MSG_FLAG_NON_BLOCKING,
};

const QUEUE_CAPACITY: usize = 64;
const IDLE_PARK_TIMEOUT_CYC: u64 = 3_500_000;
const IRQ1_BIT: u64 = 1 << 1;
const IRQ12_BIT: u64 = 1 << 12;

struct LatencyRing {
    samples: [u32; 64],
    count: usize,
}

impl LatencyRing {
    const fn new() -> Self {
        Self {
            samples: [0; 64],
            count: 0,
        }
    }

    #[inline]
    fn record(&mut self, cyc: u64) {
        let idx = self.count & 63;
        self.samples[idx] = cyc.min(u32::MAX as u64) as u32;
        self.count = self.count.wrapping_add(1);
    }

    fn percentiles(&self) -> (u64, u64, u64) {
        let n = self.count.min(64);
        if n == 0 {
            return (0, 0, 0);
        }
        let mut tmp = [0u32; 64];
        tmp[..n].copy_from_slice(&self.samples[..n]);
        for i in 1..n {
            let key = tmp[i];
            let mut j = i;
            while j > 0 && tmp[j - 1] > key {
                tmp[j] = tmp[j - 1];
                j -= 1;
            }
            tmp[j] = key;
        }
        let p50 = tmp[(n - 1) * 50 / 100] as u64;
        let p95 = tmp[(n - 1) * 95 / 100] as u64;
        let p99 = tmp[(n - 1) * 99 / 100] as u64;
        (p50, p95, p99)
    }
}

fn log_line(lb: &LineBuilder) {
    let mut buf = [0u8; 196];
    let s = lb.as_bytes();
    let n = s.len().min(buf.len() - 1);
    buf[..n].copy_from_slice(&s[..n]);
    buf[n] = b'\n';
    let _ = sys_write_debug(&buf[..=n]);
}

fn handle_irq_wakeup(
    bits: u64,
    decoder: &mut Ps2Decoder,
    ioport_cap: u64,
    irq1_cap: u64,
    irq12_cap: u64,
    queue: &mut InputRingQueue<QUEUE_CAPACITY>,
    irq12_lat: &mut LatencyRing,
) {
    let t0 = rdtsc_user();
    let _ = decoder.drain_hw_fifo(ioport_cap, queue);
    if (bits & IRQ1_BIT) != 0 {
        let _ = sys_irq_ack(irq1_cap, IRQ_ACK_UNMASK);
    }
    if irq12_cap != 0 && (bits & IRQ12_BIT) != 0 {
        let _ = sys_irq_ack(irq12_cap, IRQ_ACK_UNMASK);
        irq12_lat.record(rdtsc_user().wrapping_sub(t0));
    }
    // Second drain pass after unmasking IOAPIC pins prevents OBF edge latch stalls under IRQ1/IRQ12 races.
    let _ = decoder.drain_hw_fifo(ioport_cap, queue);
}

fn run_overflow_self_test(queue: &mut InputRingQueue<QUEUE_CAPACITY>) -> (u64, u64) {
    let mut preserved = [InputEvent::EMPTY; QUEUE_CAPACITY];
    let mut preserved_n = 0usize;
    while let Some(ev) = queue.pop() {
        if preserved_n < QUEUE_CAPACITY {
            preserved[preserved_n] = ev;
            preserved_n += 1;
        }
    }

    let before_ovf = queue.overflow_count();
    let burst_total = (QUEUE_CAPACITY + 16) as u32;
    for seq in 0..burst_total {
        let mut synth = InputEvent::EMPTY;
        synth.seq = seq;
        synth.device_id = DEV_ID_SYNTH_TEST;
        synth.kind = InputEventKind::KeyRelease;
        let _ = queue.push(synth);
    }
    let retained = queue.len() as u64;
    let delta_ovf = queue.overflow_count().saturating_sub(before_ovf);
    while queue.pop().is_some() {}

    for item in preserved.iter().take(preserved_n) {
        let _ = queue.push(*item);
    }
    (retained, delta_ovf)
}

#[no_mangle]
pub extern "C" fn _start(srv_ep_cap: u64, irq1_cap: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || srv_ep_cap == 0 || irq1_cap == 0 {
        sys_exit(120);
    }

    // 1. Receive narrowed `i8042` `IoPortCap` (`[0x0060, 0x0064]`) from `dry-init`.
    let mut init_ext = UserIpcRecvExt::default();
    let (init_tag, init_reply_tok, _) = match sys_ipc_recv_ext(srv_ep_cap, 0, 0, &mut init_ext) {
        Ok(v) => v,
        Err(_) => sys_exit(121),
    };
    let ioport_cap = init_ext.transferred_cap;
    if msg_tag_label(init_tag) != IPC_OP_INPUT_INIT_PORT || ioport_cap == 0 {
        sys_exit(122);
    }

    // 2. Create private `Notification`, initialize PS/2 i8042 keyboard, and bind `IRQ1` one-shot.
    let notif_cap = match sys_notify_create(0) {
        Ok(c) => c,
        Err(_) => sys_exit(123),
    };
    if init_i8042_keyboard(ioport_cap).is_err() {
        sys_exit(124);
    }
    let vec1 = match sys_irq_bind(irq1_cap, notif_cap, IRQF_ONESHOT) {
        Ok(v) => v,
        Err(_) => sys_exit(125),
    };

    let _ = sys_write_debug(b"[DRY-INPUT] PS/2 i8042 IRQ1 service ready in CPL3\n");
    if init_reply_tok != 0 {
        let _ = sys_ipc_reply_inline(
            srv_ep_cap,
            init_reply_tok,
            pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
            [vec1 as u64, 0, 0, 0],
        );
    }

    let mut decoder = Ps2Decoder::new(KeymapLayout::UsQwerty);
    let mut queue: InputRingQueue<QUEUE_CAPACITY> = InputRingQueue::new();
    let mut pending_waiter: Option<u64> = None;
    let mut irq12_cap: u64 = 0;
    let mut mouse_irq_bound = false;
    let mut wm_ep_cap: u64 = 0;
    let mut irq12_lat = LatencyRing::new();
    let mut logged_first_mouse = false;

    loop {
        if let Ok(bits) = sys_notify_wait(notif_cap, MSG_FLAG_NON_BLOCKING as u64, 0) {
            handle_irq_wakeup(
                bits,
                &mut decoder,
                ioport_cap,
                irq1_cap,
                irq12_cap,
                &mut queue,
                &mut irq12_lat,
            );
        }

        // In desktop WM mode (`wm_ep_cap != 0`), forward normalized keyboard & mouse events immediately.
        if wm_ep_cap != 0 {
            while let Some(ev) = queue.pop() {
                if !logged_first_mouse
                    && (ev.kind == InputEventKind::MouseMove
                        || ev.kind == InputEventKind::MouseButtonDown)
                {
                    logged_first_mouse = true;
                    let mut lb = LineBuilder::new();
                    lb.push_bytes(b"[DRY-INPUT] MOUSE_FIRST seq=");
                    lb.push_u64(ev.seq as u64);
                    lb.push_bytes(b" dx=");
                    if ev.dx < 0 {
                        lb.push_byte(b'-');
                        lb.push_u64((-ev.dx) as u64);
                    } else {
                        lb.push_u64(ev.dx as u64);
                    }
                    lb.push_bytes(b" dy=");
                    if ev.dy < 0 {
                        lb.push_byte(b'-');
                        lb.push_u64((-ev.dy) as u64);
                    } else {
                        lb.push_u64(ev.dy as u64);
                    }
                    lb.push_bytes(b" btn=");
                    lb.push_u64(ev.buttons as u64);
                    log_line(&lb);
                }
                match sys_ipc_call_inline(
                    wm_ep_cap,
                    pack_msg_tag(IPC_OP_WM_INPUT_EVENT, 0, 4, 0),
                    ev.to_ipc_mrs(queue.overflow_count()),
                    0,
                ) {
                    Ok(_) => {}
                    Err(EPIPE | EREVOKED | EBADCAP) => {
                        wm_ep_cap = 0;
                        break;
                    }
                    Err(_) => break,
                }
            }
        } else if let Some(tok) = pending_waiter {
            if let Some(ev) = queue.pop() {
                let _ = sys_ipc_reply_inline(
                    srv_ep_cap,
                    tok,
                    pack_msg_tag(IPC_OP_INPUT_EVENT, 0, 4, 0),
                    ev.to_ipc_mrs(queue.overflow_count()),
                );
                pending_waiter = None;
            }
        }

        let mut recv_ext = UserIpcRecvExt::default();
        match sys_ipc_recv_ext(srv_ep_cap, MSG_FLAG_NON_BLOCKING as u64, 0, &mut recv_ext) {
            Ok((tag, reply_tok, mrs)) => {
                match msg_tag_label(tag) {
                    IPC_OP_INPUT_INIT_MOUSE => {
                        let cap12 = recv_ext.transferred_cap;
                        let mut vec12 = 0u64;
                        let mut ok_flag = 0u64;
                        let mut wheel_flag = 0u64;
                        if cap12 != 0 {
                            let _ = sys_irq_ack(irq1_cap, IRQ_ACK_MASK);
                            if let Ok((true, has_wheel)) = init_i8042_mouse(ioport_cap) {
                                if let Ok(v) = sys_irq_bind(cap12, notif_cap, IRQF_ONESHOT) {
                                    irq12_cap = cap12;
                                    mouse_irq_bound = true;
                                    decoder.mouse_enabled = true;
                                    decoder.mouse.has_wheel = has_wheel;
                                    vec12 = v as u64;
                                    ok_flag = 1;
                                    wheel_flag = u64::from(has_wheel);
                                }
                            }
                            let _ = sys_irq_bind(irq1_cap, notif_cap, IRQF_ONESHOT);
                            let mut lb = LineBuilder::new();
                            lb.push_bytes(b"[DRY-INPUT] PS/2 AUX mouse init ok=");
                            lb.push_u64(ok_flag);
                            lb.push_bytes(b" wheel=");
                            lb.push_u64(wheel_flag);
                            lb.push_bytes(b" vec=0x");
                            lb.push_hex_u64(vec12);
                            log_line(&lb);
                        }
                        if reply_tok != 0 {
                            let _ = sys_ipc_reply_inline(
                                srv_ep_cap,
                                reply_tok,
                                pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                                [ok_flag, vec12, wheel_flag, 0],
                            );
                        }
                    }
                    IPC_OP_INPUT_SET_PREFS => {
                        if recv_ext.sender_badge != BADGE_SUPERVISOR {
                            if reply_tok != 0 {
                                let _ = sys_ipc_reply_inline(
                                    srv_ep_cap,
                                    reply_tok,
                                    pack_msg_tag(IPC_OP_ERR_PERM, 0, 1, 0),
                                    [0; 4],
                                );
                            }
                        } else if let Some(layout) = KeymapLayout::from_u8((mrs[0] & 0xff) as u8) {
                            let speed = (mrs[1] as i16).clamp(1, 2);
                            decoder.set_layout(layout);
                            decoder.mouse.speed_mult = speed;
                            let mut lb = LineBuilder::new();
                            lb.push_bytes(b"[DRY-INPUT] PREFS keymap=");
                            lb.push_u64(layout as u64);
                            lb.push_bytes(b" speed=");
                            lb.push_u64(speed as u64);
                            log_line(&lb);
                            if reply_tok != 0 {
                                let _ = sys_ipc_reply_inline(
                                    srv_ep_cap,
                                    reply_tok,
                                    pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                                    [layout as u64, speed as u64, 0, 0],
                                );
                            }
                        } else if reply_tok != 0 {
                            let _ = sys_ipc_reply_inline(
                                srv_ep_cap,
                                reply_tok,
                                pack_msg_tag(IPC_OP_ERR_INVAL, 0, 1, 0),
                                [0; 4],
                            );
                        }
                    }
                    IPC_OP_INPUT_SET_WM_SINK => {
                        if recv_ext.transferred_cap != 0 {
                            wm_ep_cap = recv_ext.transferred_cap;
                        }
                        if reply_tok != 0 {
                            let _ = sys_ipc_reply_inline(
                                srv_ep_cap,
                                reply_tok,
                                pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                                [u64::from(wm_ep_cap != 0), 0, 0, 0],
                            );
                        }
                    }
                    IPC_OP_INPUT_QUERY_STATS => {
                        let (p50, p95, p99) = irq12_lat.percentiles();
                        if reply_tok != 0 {
                            let packed_lat = (p50 & 0xfffff)
                                | ((p95 & 0xfffff) << 20)
                                | ((p99 & 0xfffff) << 40);
                            let _ = sys_ipc_reply_inline(
                                srv_ep_cap,
                                reply_tok,
                                pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                                [
                                    decoder.kbd_events,
                                    decoder.mouse.packets_ok,
                                    decoder.mouse.desync_drops | (queue.coalesced_moves() << 32),
                                    packed_lat,
                                ],
                            );
                        }
                    }
                    IPC_OP_INPUT_WAIT => {
                        if let Some(ev) = queue.pop() {
                            let _ = sys_ipc_reply_inline(
                                srv_ep_cap,
                                reply_tok,
                                pack_msg_tag(IPC_OP_INPUT_EVENT, 0, 4, 0),
                                ev.to_ipc_mrs(queue.overflow_count()),
                            );
                        } else if reply_tok != 0 {
                            pending_waiter = Some(reply_tok);
                        }
                    }
                    IPC_OP_INPUT_OVERFLOW_TEST => {
                        let (retained, delta_ovf) = run_overflow_self_test(&mut queue);
                        if reply_tok != 0 {
                            let _ = sys_ipc_reply_inline(
                                srv_ep_cap,
                                reply_tok,
                                pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                                [
                                    retained,
                                    delta_ovf,
                                    queue.overflow_count(),
                                    decoder.invalid_bytes(),
                                ],
                            );
                        }
                    }
                    IPC_OP_CRASH_UD2 => {
                        let _ = sys_write_debug(b"[DRY-INPUT] Injecting CPL3 #UD crash for recovery test\n");
                        // SAFETY: Intentional CPL=3 `#UD` fault to prove supervisor crash containment & restart.
                        unsafe {
                            asm!("ud2", options(nomem, nostack));
                        }
                    }
                    IPC_OP_EXIT_CLEAN => {
                        let (p50, p95, p99) = irq12_lat.percentiles();
                        if mouse_irq_bound {
                            let mut lb = LineBuilder::new();
                            lb.push_bytes(b"[DRY-INPUT-STATS] kbd_ev=");
                            lb.push_u64(decoder.kbd_events);
                            lb.push_bytes(b" mouse_pkts=");
                            lb.push_u64(decoder.mouse.packets_ok);
                            lb.push_bytes(b" mouse_moves=");
                            lb.push_u64(decoder.mouse.move_events);
                            lb.push_bytes(b" mouse_btns=");
                            lb.push_u64(decoder.mouse.button_events);
                            lb.push_bytes(b" desync=");
                            lb.push_u64(decoder.mouse.desync_drops);
                            lb.push_bytes(b" coal=");
                            lb.push_u64(queue.coalesced_moves());
                            lb.push_bytes(b" irq12_drain_p50=");
                            lb.push_u64(p50);
                            lb.push_bytes(b" p95=");
                            lb.push_u64(p95);
                            lb.push_bytes(b" p99=");
                            lb.push_u64(p99);
                            log_line(&lb);
                        }
                        let _ = sys_irq_ack(irq1_cap, IRQ_ACK_UNBIND);
                        if mouse_irq_bound && irq12_cap != 0 {
                            let _ = sys_irq_ack(irq12_cap, IRQ_ACK_UNBIND);
                        }
                        if wm_ep_cap != 0 {
                            let _ = sys_cap_revoke(wm_ep_cap, 1);
                        }
                        let _ = sys_cap_revoke(notif_cap, 2);
                        if reply_tok != 0 {
                            let _ = sys_ipc_reply_inline(
                                srv_ep_cap,
                                reply_tok,
                                pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                                [queue.overflow_count(), 0, 0, 0],
                            );
                        }
                        sys_exit(0);
                    }
                    _ => {
                        if reply_tok != 0 {
                            let _ = sys_ipc_reply_inline(
                                srv_ep_cap,
                                reply_tok,
                                pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                                [0; 4],
                            );
                        }
                    }
                }
                continue;
            }
            Err(EAGAIN) => {}
            Err(_) => break,
        }

        if let Ok(bits) = sys_notify_wait(notif_cap, 0, IDLE_PARK_TIMEOUT_CYC) {
            handle_irq_wakeup(
                bits,
                &mut decoder,
                ioport_cap,
                irq1_cap,
                irq12_cap,
                &mut queue,
                &mut irq12_lat,
            );
        }
    }

    let _ = sys_irq_ack(irq1_cap, IRQ_ACK_UNBIND);
    if mouse_irq_bound && irq12_cap != 0 {
        let _ = sys_irq_ack(irq12_cap, IRQ_ACK_UNBIND);
    }
    let _ = sys_cap_revoke(notif_cap, 2);
    sys_exit(0);
}
