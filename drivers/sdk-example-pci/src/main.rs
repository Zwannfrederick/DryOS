// SPDX-License-Identifier: MIT OR Apache-2.0
//! `dry-sdk-demo` — Out-of-tree Reference Driver Island built strictly on `dry-driver-sdk`.
//!
//! Demonstrates versioned ABI v1 negotiation (`ISLAND_OP_HELLO`), bounded MMIO/DMA contract
//! verification, `IoRingSharedPage` completion, crash/restart generation bumps, and clean
//! `ISLAND_OP_QUIESCE_UNLOAD` teardown in `SecDomain::Userspace`.

#![no_std]
#![no_main]

use core::arch::asm;

use dry_driver_sdk::{
    check_ring_header, validate_abi_hello, DmaRegion, IslandLifecycle, MmioWindow,
    ISLAND_FEAT_IORING, ISLAND_FEAT_MMIO_BOUNDS, ISLAND_FEAT_QUIESCE, SDK_IORING_MAGIC,
};
use dry_userspace::abi_ext::revoke_unused_cap;
use dry_userspace::init_legacy::log_line;
use dry_userspace::ioring::IoRingSharedPage;
use dry_userspace::island_proto::{
    execute_mock_sqe, island_session_badge, mock_island_transform, BADGE_ISLAND_BROKER,
    IPC_OP_ERR_STALE_GEN, ISLAND_OP_HELLO, ISLAND_OP_QUIESCE_UNLOAD, MOCK_OP_BIND_NOTIF,
    MOCK_OP_INIT_ISLAND, MOCK_OP_SET_PARK_MODE, MOCK_OP_TRANSFORM, MOCK_OP_WAKE_PROBE,
};
use dry_userspace::shell::LineBuilder;
use dry_userspace::{
    msg_tag_label, pack_msg_tag, rdtsc_user, read_cs_ss, sys_cap_revoke, sys_exit,
    sys_frame_alloc, sys_get_tid, sys_ipc_recv_ext, sys_ipc_reply_inline,
    sys_ipc_reply_with_grant, sys_notify_bind_tcb, sys_notify_signal, sys_notify_unbind_tcb,
    sys_proc_kill, sys_vspace_map, UserIpcRecvExt, BADGE_SUPERVISOR, EINVAL, EPERM, EREVOKED,
    IPC_OP_BOUND_NOTIF, IPC_OP_CRASH_UD2, IPC_OP_ERR_INVAL, IPC_OP_ERR_PERM, IPC_OP_EXIT_CLEAN,
    IPC_OP_PONG, RIGHT_GRANT, RIGHT_MAP, RIGHT_READ, RIGHT_REVOKE, RIGHT_WRITE,
    VSPACE_MAP_OP_4K_RW, VSPACE_MAP_OP_UNMAP_4K,
};

const ISLAND_RING_VADDR: u64 = 0x2400_0000;

#[inline]
fn reply_err(ep: u64, tok: u64, op: u32, err_code: isize) {
    if tok != 0 {
        let _ = sys_ipc_reply_inline(
            ep,
            tok,
            pack_msg_tag(op, 0, 1, 0),
            [(-err_code) as u64, 0, 0, 0],
        );
    }
}

fn cleanup_and_exit(sq_notif: u64, cq_notif: u64, ring_cap: u64) -> ! {
    if sq_notif != 0 {
        let _ = sys_notify_unbind_tcb(sq_notif);
        let _ = sys_cap_revoke(sq_notif, 1);
    }
    if cq_notif != 0 {
        let _ = sys_cap_revoke(cq_notif, 1);
    }
    let _ = sys_vspace_map(ring_cap, ISLAND_RING_VADDR, VSPACE_MAP_OP_UNMAP_4K);
    let _ = sys_cap_revoke(ring_cap, 1);
    sys_exit(0);
}

#[no_mangle]
pub extern "C" fn _start(drv_ep_cap: u64, _arg1: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || drv_ep_cap == 0 {
        sys_exit(180);
    }

    let mac_isolated = sys_proc_kill(1) == Err(EPERM);
    let Ok(mmio_win) = MmioWindow::new(0x2000_0000, 4096) else {
        sys_exit(181);
    };
    if mmio_win.check_access(0, 4).is_err() || mmio_win.check_access(4096, 4).is_ok() {
        sys_exit(181);
    }
    let Ok(dma_win) = DmaRegion::new(ISLAND_RING_VADDR as usize, 0x1000_0000, 4096) else {
        sys_exit(181);
    };
    if dma_win.check_slice(64, 512, 64).is_err() {
        sys_exit(181);
    }

    let ring_cap = match sys_frame_alloc(0) {
        Ok(c) => c,
        Err(_) => sys_exit(181),
    };
    if sys_vspace_map(ring_cap, ISLAND_RING_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
        let _ = sys_cap_revoke(ring_cap, 1);
        sys_exit(182);
    }
    // SAFETY: `ISLAND_RING_VADDR` is a freshly allocated, `USER_RW`-mapped 4 KiB frame.
    let ring = unsafe { IoRingSharedPage::init_at(ISLAND_RING_VADDR) };
    if check_ring_header(SDK_IORING_MAGIC, 0, 0, 32).is_err() {
        cleanup_and_exit(0, 0, ring_cap);
    }

    let mut lc = IslandLifecycle::new();
    let mut sq_notif = 0u64;
    let mut cq_notif = 0u64;
    let mut processed_ops = 0u64;
    let mut ring_ops = 0u64;
    let mut wake_events = 0u64;

    loop {
        let mut ext = UserIpcRecvExt::default();
        let (tag, tok, mrs, t_awake) = match sys_ipc_recv_ext(drv_ep_cap, 0, 0, &mut ext) {
            Ok((tg, tk, mr)) => {
                wake_events = wake_events.wrapping_add(1);
                (tg, tk, mr, rdtsc_user())
            }
            Err(_) => break,
        };

        let op = msg_tag_label(tag);
        let gen = lc.generation();
        if op == IPC_OP_BOUND_NOTIF {
            let mut drained = false;
            while let Some(sqe) = ring.pop_sqe() {
                let cqe = execute_mock_sqe(sqe, gen);
                let _ = ring.push_cqe(cqe);
                processed_ops = processed_ops.wrapping_add(1);
                ring_ops = ring_ops.wrapping_add(1);
                drained = true;
            }
            if drained && cq_notif != 0 {
                let _ = sys_notify_signal(cq_notif, 1);
            }
            continue;
        }

        if op != MOCK_OP_BIND_NOTIF {
            revoke_unused_cap(ext.transferred_cap);
        }

        let is_sup = ext.sender_badge == BADGE_SUPERVISOR;
        let is_broker = ext.sender_badge == BADGE_ISLAND_BROKER || is_sup;
        let is_session = gen != 0 && ext.sender_badge == island_session_badge(gen);

        match op {
            MOCK_OP_BIND_NOTIF => {
                if !is_broker || ext.transferred_cap == 0 {
                    revoke_unused_cap(ext.transferred_cap);
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                match mrs[0] {
                    0 => {
                        if sq_notif != 0 {
                            let _ = sys_notify_unbind_tcb(sq_notif);
                            let _ = sys_cap_revoke(sq_notif, 1);
                        }
                        sq_notif = ext.transferred_cap;
                        let _ = sys_notify_bind_tcb(sq_notif);
                    }
                    1 => {
                        if cq_notif != 0 {
                            let _ = sys_cap_revoke(cq_notif, 1);
                        }
                        cq_notif = ext.transferred_cap;
                    }
                    _ => {
                        revoke_unused_cap(ext.transferred_cap);
                        reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                        continue;
                    }
                }
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                        [1, 0, 0, 0],
                    );
                }
            }
            MOCK_OP_INIT_ISLAND => {
                if !is_broker || mrs[0] == 0 {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if lc.on_start(mrs[0]).is_err() {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }
                let (tid, _) = sys_get_tid();
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-ISLAND-SDK-DEMO] Ready in CPL3 Userspace gen=");
                lb.push_u64(lc.generation());
                lb.push_bytes(b" mac_isolated=");
                lb.push_u64(u64::from(mac_isolated));
                lb.push_bytes(b" sdk_v1=1");
                log_line(&lb);

                if tok != 0 {
                    let ring_rights =
                        RIGHT_READ | RIGHT_WRITE | RIGHT_MAP | RIGHT_GRANT | RIGHT_REVOKE;
                    if sys_ipc_reply_with_grant(
                        drv_ep_cap,
                        tok,
                        IPC_OP_PONG,
                        [lc.generation(), u64::from(mac_isolated), tid, 1],
                        ring_cap,
                        ring_rights,
                        0,
                    )
                    .is_err()
                    {
                        cleanup_and_exit(sq_notif, cq_notif, ring_cap);
                    }
                }
            }
            ISLAND_OP_HELLO => {
                if !is_broker {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                let feats = ISLAND_FEAT_IORING | ISLAND_FEAT_QUIESCE | ISLAND_FEAT_MMIO_BOUNDS;
                match validate_abi_hello(mrs[0], lc.generation(), feats) {
                    Ok(reply_words) => {
                        if tok != 0 {
                            let _ = sys_ipc_reply_inline(
                                drv_ep_cap,
                                tok,
                                pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                                reply_words,
                            );
                        }
                    }
                    Err(e) => reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, e.errno()),
                }
            }
            MOCK_OP_TRANSFORM => {
                if !is_broker && !is_session {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if mrs[3] != 0 && mrs[3] != gen {
                    if tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            drv_ep_cap,
                            tok,
                            pack_msg_tag(IPC_OP_ERR_STALE_GEN, 0, 2, 0),
                            [(-EREVOKED) as u64, gen, 0, 0],
                        );
                    }
                    continue;
                }
                let digest = mock_island_transform(mrs[0], mrs[1], mrs[2], gen);
                processed_ops = processed_ops.wrapping_add(1);
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [digest, processed_ops, gen, wake_events],
                    );
                }
            }
            MOCK_OP_WAKE_PROBE => {
                if !is_broker && !is_session {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [t_awake, gen, wake_events, processed_ops],
                    );
                }
            }
            MOCK_OP_SET_PARK_MODE => {
                if !is_broker {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                let park_mode = if mrs[0] == 1 && sq_notif != 0 { 1 } else { 0 };
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [park_mode, gen, 0, 0],
                    );
                }
            }
            ISLAND_OP_QUIESCE_UNLOAD => {
                if !is_broker {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                let _ = lc.on_quiesce();
                let _ = lc.on_unload();
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [processed_ops, ring_ops, wake_events, gen],
                    );
                }
                cleanup_and_exit(sq_notif, cq_notif, ring_cap);
            }
            IPC_OP_CRASH_UD2 => {
                if !is_sup {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                lc.on_fault();
                let mut clb = LineBuilder::new();
                clb.push_bytes(b"[DRY-ISLAND-SDK-DEMO] Injecting controlled #UD fault gen=");
                clb.push_u64(gen);
                log_line(&clb);
                // SAFETY: Controlled crash test authorized by supervisor/broker.
                unsafe {
                    asm!("ud2", options(nomem, nostack));
                }
            }
            IPC_OP_EXIT_CLEAN => {
                if !is_sup {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [processed_ops, ring_ops, wake_events, gen],
                    );
                }
                cleanup_and_exit(sq_notif, cq_notif, ring_cap);
            }
            _ => reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL),
        }
    }
    cleanup_and_exit(sq_notif, cq_notif, ring_cap);
}
