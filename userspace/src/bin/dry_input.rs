// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-input` — Isolated Ring 3 (`CPL=3`) PS/2 i8042 Input Driver Service.
//!
//! Owns narrow `IrqCap` (`IRQ1`) and `IoPortCap` (`[0x0060, 0x0064]`) in `SecDomain::RootSystem`
//! (`compartment_id = 0x0003`) with `RFLAGS.IOPL == 0`. Decodes Set-1 + `0xE0` extended scancodes
//! into device-agnostic `InputEvent`s inside a bounded 64-slot ring buffer and serves unprivileged
//! consumers (`dry-shell`) over Capability IPC.

#![no_std]
#![no_main]

use core::arch::asm;
use dry_userspace::input::{
    InputEvent, InputEventKind, InputRingQueue, DEV_ID_SYNTH_TEST,
};
use dry_userspace::keymap::KeymapLayout;
use dry_userspace::ps2::{init_i8042_keyboard, Ps2Decoder};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_ipc_recv_ext,
    sys_ipc_recv_inline, sys_ipc_reply_inline, sys_irq_ack, sys_irq_bind, sys_notify_create,
    sys_notify_wait, sys_write_debug, UserIpcRecvExt, EAGAIN, IPC_OP_CRASH_UD2, IPC_OP_EXIT_CLEAN,
    IPC_OP_INPUT_EVENT, IPC_OP_INPUT_INIT_PORT, IPC_OP_INPUT_OVERFLOW_TEST, IPC_OP_INPUT_WAIT,
    IPC_OP_PONG, IRQF_ONESHOT, IRQ_ACK_UNBIND, IRQ_ACK_UNMASK, MSG_FLAG_NON_BLOCKING,
};

const QUEUE_CAPACITY: usize = 64;
const IDLE_PARK_TIMEOUT_CYC: u64 = 3_500_000; // ~1.1 ms H1 park; wakes immediately on hardware IRQ1

fn handle_irq1_wakeup(
    decoder: &mut Ps2Decoder,
    ioport_cap: u64,
    irq1_cap: u64,
    queue: &mut InputRingQueue<QUEUE_CAPACITY>,
) {
    let _ = decoder.drain_hw_fifo(ioport_cap, queue);
    let _ = sys_irq_ack(irq1_cap, IRQ_ACK_UNMASK);
    // Drain once more after unmasking IOAPIC pin 1 so an edge arriving mid-drain cannot stall OBF.
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

    // 2. Create private `Notification`, initialize PS/2 i8042 controller, and bind `IRQ1` one-shot.
    let notif_cap = match sys_notify_create(0) {
        Ok(c) => c,
        Err(_) => sys_exit(123),
    };
    if init_i8042_keyboard(ioport_cap).is_err() {
        sys_exit(124);
    }
    let vec = match sys_irq_bind(irq1_cap, notif_cap, IRQF_ONESHOT) {
        Ok(v) => v,
        Err(_) => sys_exit(125),
    };

    let _ = sys_write_debug(b"[DRY-INPUT] PS/2 i8042 IRQ1 service ready in CPL3\n");
    if init_reply_tok != 0 {
        let _ = sys_ipc_reply_inline(
            srv_ep_cap,
            init_reply_tok,
            pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
            [vec as u64, 0, 0, 0],
        );
    }

    let mut decoder = Ps2Decoder::new(KeymapLayout::UsQwerty);
    let mut queue: InputRingQueue<QUEUE_CAPACITY> = InputRingQueue::new();
    let mut pending_waiter: Option<u64> = None;

    loop {
        if sys_notify_wait(notif_cap, MSG_FLAG_NON_BLOCKING as u64, 0).is_ok() {
            handle_irq1_wakeup(&mut decoder, ioport_cap, irq1_cap, &mut queue);
        }

        if let Some(tok) = pending_waiter {
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

        match sys_ipc_recv_inline(srv_ep_cap, MSG_FLAG_NON_BLOCKING as u64, 0) {
            Ok((tag, reply_tok, _mrs)) => {
                match msg_tag_label(tag) {
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
                        let _ = sys_irq_ack(irq1_cap, IRQ_ACK_UNBIND);
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

        // Park cleanly in H1 scheduler until hardware `IRQ1` signals `notif_cap` (or short admin timeout).
        if sys_notify_wait(notif_cap, 0, IDLE_PARK_TIMEOUT_CYC).is_ok() {
            handle_irq1_wakeup(&mut decoder, ioport_cap, irq1_cap, &mut queue);
        }
    }

    let _ = sys_irq_ack(irq1_cap, IRQ_ACK_UNBIND);
    let _ = sys_cap_revoke(notif_cap, 2);
    sys_exit(0);
}
