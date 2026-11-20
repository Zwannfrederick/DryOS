// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-nv-island` — Capability-Isolated Ring-3 NVIDIA Level-1 Driver Island (`0x0218`).
//!
//! Strictly confined to `SecDomain::Userspace`. Performs ZERO MMIO writes, ZERO PCI config
//! writes, ZERO DMA allocations, and ZERO GPU firmware uploads. Reports honest Level-1
//! readiness (`BLOCKED` pending VT-x + VT-d IR + VFIO + OpenRM + GSP-RM) and rejects
//! unauthorized callers (`EPERM`) and corrupt IPC payloads (`EINVAL`).

#![no_std]
#![no_main]

use core::arch::asm;

use dry_userspace::abi_ext::revoke_unused_cap;
use dry_userspace::init_legacy::log_line;
use dry_userspace::nvidia_island::protocol::{
    BADGE_NV_CLIENT, NV_OP_AUDIT_CORRUPT, NV_OP_PING_HEALTH, NV_OP_QUERY_STATUS,
};
use dry_userspace::shell::LineBuilder;
use dry_userspace::vfs_proto::sys_ipc_recv_extra;
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_exit, sys_get_tid, sys_ipc_reply_inline,
    sys_proc_kill, UserIpcRecvExt, BADGE_SUPERVISOR, EINVAL, EPERM, IPC_OP_CRASH_UD2,
    IPC_OP_ERR_INVAL, IPC_OP_ERR_NOSYS, IPC_OP_ERR_PERM, IPC_OP_EXIT_CLEAN, IPC_OP_PONG,
    MAX_IPC_EXTRA_BYTES,
};

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
pub extern "C" fn _start(nv_ep_cap: u64, _arg1: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || nv_ep_cap == 0 {
        sys_exit(190);
    }

    let mac_isolated = sys_proc_kill(1) == Err(EPERM);
    let (pid, _) = sys_get_tid();
    {
        let mut lb = LineBuilder::new();
        lb.push_bytes(b"[DRY-NV-ISLAND] Ready in CPL3 Userspace [0x0218] pid=");
        lb.push_u64(pid);
        lb.push_bytes(b" mac_isolated=");
        lb.push_u64(u64::from(mac_isolated));
        lb.push_bytes(b" level=1 status=BLOCKED");
        log_line(&lb);
    }

    let mut ops_count = 0u64;
    loop {
        let mut ext = UserIpcRecvExt::default();
        let mut extra_rx = [0u8; MAX_IPC_EXTRA_BYTES];
        let (tag, extra_len, _mrs) =
            match sys_ipc_recv_extra(nv_ep_cap, 0, 0, &mut extra_rx, &mut ext) {
                Ok(v) => v,
                Err(_) => break,
            };
        revoke_unused_cap(ext.transferred_cap);

        let tok = ext.reply_token;
        let op = msg_tag_label(tag);
        let is_sup = ext.sender_badge == BADGE_SUPERVISOR;
        let is_auth = is_sup || ext.sender_badge == BADGE_NV_CLIENT;

        if !is_auth {
            reply_err(nv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
            continue;
        }

        match op {
            NV_OP_PING_HEALTH | NV_OP_QUERY_STATUS => {
                ops_count = ops_count.wrapping_add(1);
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        nv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [1, u64::from(mac_isolated), pid, ops_count],
                    );
                }
            }
            NV_OP_AUDIT_CORRUPT => {
                let valid = extra_len >= 12
                    && extra_rx[..8] == *b"NVCTRL01"
                    && extra_rx[8] == (extra_rx[9] ^ 0x5A);
                if !valid {
                    reply_err(nv_ep_cap, tok, IPC_OP_ERR_INVAL, EINVAL);
                    continue;
                }
                ops_count = ops_count.wrapping_add(1);
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        nv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [1, ops_count, 0, 0],
                    );
                }
            }
            IPC_OP_CRASH_UD2 => {
                if !is_sup {
                    reply_err(nv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                let _ = sys_ipc_reply_inline(
                    nv_ep_cap,
                    tok,
                    pack_msg_tag(IPC_OP_PONG, 0, 1, 0),
                    [1, 0, 0, 0],
                );
                let _ = dry_userspace::sys_write_debug(
                    b"[DRY-NV-ISLAND] Controlled #UD fault injected by supervisor\n",
                );
                // SAFETY: Supervisor-authorized fault-isolation self-test.
                unsafe {
                    asm!("ud2", options(nomem, nostack));
                }
            }
            IPC_OP_EXIT_CLEAN => {
                if !is_sup {
                    reply_err(nv_ep_cap, tok, IPC_OP_ERR_PERM, EPERM);
                    continue;
                }
                if tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        nv_ep_cap,
                        tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [ops_count, 0, 0, 0],
                    );
                }
                sys_exit(0);
            }
            _ => reply_err(nv_ep_cap, tok, IPC_OP_ERR_NOSYS, EINVAL),
        }
    }
    sys_exit(0);
}
