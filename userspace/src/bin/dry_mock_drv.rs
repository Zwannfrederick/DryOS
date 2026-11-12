// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-mock-drv` — Isolated Ring 3 (`CPL=3`, `SecDomain::Userspace`, `0x0209`) Adaptive Driver
//! Island Prototype (`Milestone 2C-0` — `ADR-016`).
//!
//! Capabilities & Lifecycle Properties:
//! - Runs in an unprivileged `SecDomain::Userspace` compartment (`0x0209`) with its own hardware
//!   `VSpace` and `CSpace`; self-verifies `SYS_PROC_KILL(1) == Err(EPERM)` at startup.
//! - Owns a single 4 KiB `IoRingSharedPage` (`ISLAND_RING_VADDR = 0x2400_0000`) delegated only via
//!   `Device Broker` (`dry-broker.elf`); when `dry-mock-drv` crashes (`#UD`) or exits, `reclaim_process`
//!   revokes and unmaps its `IoRing` CDT subtree across all client `VSpace`s (`0` leaked frames).
//! - Parks with `timeout_tsc = 0` (`0 Hz` timer polling when idle) on either `sys_ipc_recv_ext`
//!   (`park_mode == 0`) or `sys_notify_wait(sq_notif)` (`park_mode == 1`).

#![no_std]
#![no_main]

use core::arch::asm;

use dry_userspace::init_legacy::log_line;
use dry_userspace::ioring::IoRingSharedPage;
use dry_userspace::island_proto::{
    execute_mock_sqe, island_session_badge, mock_island_transform, BADGE_ISLAND_BROKER,
    IPC_OP_ERR_STALE_GEN, ISLAND_DOORBELL_CTL, MOCK_OP_BIND_NOTIF, MOCK_OP_INIT_ISLAND,
    MOCK_OP_SET_PARK_MODE, MOCK_OP_TRANSFORM, MOCK_OP_WAKE_PROBE,
};
use dry_userspace::shell::LineBuilder;
use dry_userspace::{
    msg_tag_label, pack_msg_tag, rdtsc_user, read_cs_ss, sys_cap_revoke, sys_exit, sys_frame_alloc,
    sys_get_tid, sys_ipc_recv_ext, sys_ipc_reply_inline, sys_ipc_reply_with_grant,
    sys_notify_signal, sys_notify_wait, sys_proc_kill, sys_sysinfo, sys_vspace_map,
    UserIpcRecvExt, UserSysInfo, BADGE_SUPERVISOR, EAGAIN, EINVAL, EPERM,
    EREVOKED, ETIMEDOUT, IPC_OP_CRASH_UD2, IPC_OP_ERR_INVAL, IPC_OP_ERR_PERM, IPC_OP_EXIT_CLEAN,
    IPC_OP_PONG, RIGHT_GRANT, RIGHT_MAP, RIGHT_READ, RIGHT_REVOKE, RIGHT_WRITE,
    VSPACE_MAP_OP_4K_RW,
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

#[no_mangle]
pub extern "C" fn _start(drv_ep_cap: u64, _arg1: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || drv_ep_cap == 0 {
        sys_exit(180);
    }

    // Verify `ADR-007` MAC confinement: `SecDomain::Userspace` cannot invoke supervisor kill (`-EPERM`).
    let mac_isolated = sys_proc_kill(1) == Err(EPERM);

    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);
    let ctl_wait_tsc = 50_000u64.saturating_mul(sys.tsc_mhz.max(1000));

    let ring_cap = match sys_frame_alloc(0) {
        Ok(c) => c,
        Err(_) => sys_exit(181),
    };
    if sys_vspace_map(ring_cap, ISLAND_RING_VADDR, VSPACE_MAP_OP_4K_RW).is_err() {
        sys_exit(182);
    }
    // SAFETY: `ISLAND_RING_VADDR` is a freshly allocated, `USER_RW`-mapped 4 KiB frame in our private `VSpace`.
    let ring = unsafe { IoRingSharedPage::init_at(ISLAND_RING_VADDR) };

    let mut generation = 0u64;
    let mut sq_notif = 0u64;
    let mut cq_notif = 0u64;
    let mut park_mode = 0u64;
    let mut processed_ops = 0u64;
    let mut ring_ops = 0u64;
    let mut wake_events = 0u64;

    loop {
        let mut ext = UserIpcRecvExt::default();
        let (tag, tok, mrs, t_awake) = if park_mode == 1 && sq_notif != 0 {
            let mut drained = false;
            while let Some(sqe) = ring.pop_sqe() {
                let cqe = execute_mock_sqe(sqe, generation);
                let _ = ring.push_cqe(cqe);
                processed_ops = processed_ops.wrapping_add(1);
                ring_ops = ring_ops.wrapping_add(1);
                drained = true;
            }
            if drained && cq_notif != 0 {
                let _ = sys_notify_signal(cq_notif, 1);
            }

            let bits = match sys_notify_wait(sq_notif, 0, 0) {
                Ok(b) => b,
                Err(_) => break,
            };
            let awake_tsc = rdtsc_user();
            wake_events = wake_events.wrapping_add(1);
            if (bits & ISLAND_DOORBELL_CTL) == 0 {
                continue;
            }
            match sys_ipc_recv_ext(drv_ep_cap, 0, ctl_wait_tsc, &mut ext) {
                Ok((tg, tk, mr)) => (tg, tk, mr, awake_tsc),
                Err(ETIMEDOUT | EAGAIN) => continue,
                Err(_) => break,
            }
        } else {
            match sys_ipc_recv_ext(drv_ep_cap, 0, 0, &mut ext) {
                Ok((tg, tk, mr)) => {
                    let awake_tsc = rdtsc_user();
                    wake_events = wake_events.wrapping_add(1);
                    (tg, tk, mr, awake_tsc)
                }
                Err(_) => break,
            }
        };

        let op = msg_tag_label(tag);
        let is_sup = ext.sender_badge == BADGE_SUPERVISOR;
        let is_broker = ext.sender_badge == BADGE_ISLAND_BROKER || is_sup;
        let is_session = generation != 0 && ext.sender_badge == island_session_badge(generation);

        match op {
            MOCK_OP_BIND_NOTIF => {
                if !is_broker || ext.transferred_cap == 0 {
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                match mrs[0] {
                    0 => {
                        if sq_notif != 0 {
                            let _ = sys_cap_revoke(sq_notif, 1);
                        }
                        sq_notif = ext.transferred_cap;
                    }
                    1 => {
                        if cq_notif != 0 {
                            let _ = sys_cap_revoke(cq_notif, 1);
                        }
                        cq_notif = ext.transferred_cap;
                    }
                    _ => {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
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
                    if ext.transferred_cap != 0 {
                        let _ = sys_cap_revoke(ext.transferred_cap, 1);
                    }
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                generation = mrs[0];
                let (tid, _) = sys_get_tid();
                let mut lb = LineBuilder::new();
                lb.push_bytes(b"[DRY-ISLAND-MOCK] Ready in CPL3 Userspace [0x0209] gen=");
                lb.push_u64(generation);
                lb.push_bytes(b" mac_isolated=");
                lb.push_u64(u64::from(mac_isolated));
                lb.push_bytes(b" tid=");
                lb.push_u64(tid);
                log_line(&lb);

                if tok != 0 {
                    // Grant `RIGHT_REVOKE` to `Device Broker` so Broker can deterministically
                    // revoke + unmap all transitive client `IoRing` leases on generation rotation
                    // even before process exit (`ADR-016` control-plane ownership).
                    let ring_rights =
                        RIGHT_READ | RIGHT_WRITE | RIGHT_MAP | RIGHT_GRANT | RIGHT_REVOKE;
                    if sys_ipc_reply_with_grant(
                        drv_ep_cap,
                        tok,
                        IPC_OP_PONG,
                        [generation, u64::from(mac_isolated), tid, 1],
                        ring_cap,
                        ring_rights,
                        0,
                    )
                    .is_err()
                    {
                        sys_exit(183);
                    }
                }
            }
            MOCK_OP_TRANSFORM => {
                if !is_broker && !is_session {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if mrs[3] != 0 && mrs[3] != generation {
                    if tok != 0 {
                        let _ = sys_ipc_reply_inline(
                            drv_ep_cap,
                            tok,
                            pack_msg_tag(IPC_OP_ERR_STALE_GEN, 0, 2, 0),
                            [(-EREVOKED) as u64, generation, 0, 0],
                        );
                    }
                    continue;
                }
                let digest = mock_island_transform(mrs[0], mrs[1], mrs[2], generation);
                processed_ops = processed_ops.wrapping_add(1);
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [digest, processed_ops, generation, wake_events],
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
                        [t_awake, generation, wake_events, processed_ops],
                    );
                }
            }
            MOCK_OP_SET_PARK_MODE => {
                if !is_broker {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                park_mode = if mrs[0] == 1 && sq_notif != 0 { 1 } else { 0 };
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [park_mode, generation, 0, 0],
                    );
                }
            }
            IPC_OP_CRASH_UD2 => {
                if !is_sup {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                let mut clb = LineBuilder::new();
                clb.push_bytes(b"[DRY-ISLAND-MOCK] Injecting controlled CPL3 #UD fault gen=");
                clb.push_u64(generation);
                log_line(&clb);
                // SAFETY: Deliberate supervisor-authorized `#UD` fault to test Driver Island crash recovery.
                unsafe {
                    asm!("ud2", options(nomem, nostack));
                }
            }
            IPC_OP_EXIT_CLEAN => {
                if !is_sup {
                    reply_err(drv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if sq_notif != 0 {
                    let _ = sys_cap_revoke(sq_notif, 1);
                }
                if cq_notif != 0 {
                    let _ = sys_cap_revoke(cq_notif, 1);
                }
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        drv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [processed_ops, ring_ops, wake_events, generation],
                    );
                }
                sys_exit(0);
            }
            _ => {
                if ext.transferred_cap != 0 {
                    let _ = sys_cap_revoke(ext.transferred_cap, 1);
                }
                reply_err(drv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
            }
        }
    }
    sys_exit(0);
}
