// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-power` — Isolated Ring 3 (`CPL=3`, `SecDomain::RootSystem`, `0x0005`) Power Lifecycle
//! Service (`Milestone 2A-3` `P3-C`).
//!
//! Architectural & Security Properties:
//! - Sole userspace service delegated `CapObjectKind::PowerCtrl` (`CapRights::MANAGE`) to invoke
//!   privileged kernel transitions (`POWER_OP_SHUTDOWN_S5`, `POWER_OP_REBOOT`, `POWER_OP_SUSPEND_S3`).
//! - Blocks infinitely (`timeout = 0`) on `pwr_ep_cap` when idle (`0` busy-polling or timer wakeups).
//! - Enforces unforgeable capability badge `BADGE_SUPERVISOR` on all mutating power transitions,
//!   rejecting forged/unprivileged callers with `IPC_OP_ERR_PERM`.

#![no_std]
#![no_main]

use dry_userspace::shell::LineBuilder;
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_revoke, sys_exit, sys_ipc_recv_ext,
    sys_ipc_reply_inline, sys_power_ctl, sys_power_query, sys_write_debug, UserIpcRecvExt,
    UserPowerInfo, BADGE_SUPERVISOR, ENOSYS, IPC_OP_ERR_INVAL, IPC_OP_ERR_NOSYS, IPC_OP_ERR_PERM,
    IPC_OP_EXIT_CLEAN, IPC_OP_PONG, IPC_OP_PWR_QUERY, IPC_OP_PWR_REBOOT, IPC_OP_PWR_SHUTDOWN_S5,
    IPC_OP_PWR_SUSPEND_S3, POWER_OP_REBOOT, POWER_OP_SHUTDOWN_S5, POWER_OP_SUSPEND_S3,
};

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 160];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

#[inline]
fn reply_err(ep: u64, tok: u64, op: u32, count: u64) {
    if tok != 0 {
        let _ = sys_ipc_reply_inline(ep, tok, pack_msg_tag(op, 0, 1, 0), [count, 0, 0, 0]);
    }
}

#[no_mangle]
pub extern "C" fn _start(pwr_ep_cap: u64, power_ctrl_cap: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || pwr_ep_cap == 0 || power_ctrl_cap == 0 {
        sys_exit(190);
    }

    let mut init_info = UserPowerInfo::default();
    let _ = sys_power_query(&mut init_info);
    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-POWER] Ready in CPL3 RootSystem [0x0005] fadt=");
    lb.push_u64(init_info.fadt_present as u64);
    lb.push_bytes(b" s5=");
    lb.push_u64(init_info.s5_supported as u64);
    lb.push_bytes(b" reset=");
    lb.push_u64(init_info.reset_supported as u64);
    lb.push_bytes(b" s3_aml=");
    lb.push_u64(init_info.s3_aml_present as u64);
    lb.push_bytes(b" s3_sup=");
    lb.push_u64(init_info.s3_supported as u64);
    log_line(&lb);

    let mut unauth_rejects = 0u64;
    let mut s3_rejects = 0u64;

    loop {
        let mut ext = UserIpcRecvExt::default();
        let (tag, reply_tok, _) = match sys_ipc_recv_ext(pwr_ep_cap, 0, 0, &mut ext) {
            Ok(v) => v,
            Err(_) => break,
        };
        if ext.transferred_cap != 0 {
            let _ = sys_cap_revoke(ext.transferred_cap, 1);
        }

        let op = msg_tag_label(tag);
        match op {
            IPC_OP_PWR_QUERY => {
                let mut p = UserPowerInfo::default();
                let _ = sys_power_query(&mut p);
                let flags = (p.fadt_present as u64)
                    | ((p.s5_supported as u64) << 1)
                    | ((p.reset_supported as u64) << 2)
                    | ((p.s3_aml_present as u64) << 3)
                    | ((p.s3_supported as u64) << 4);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        pwr_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 4, 0),
                        [p.idle_hlt_calls, p.idle_hlt_tsc, p.elapsed_tsc, flags],
                    );
                }
            }
            IPC_OP_PWR_SUSPEND_S3 => {
                if ext.sender_badge != BADGE_SUPERVISOR {
                    unauth_rejects = unauth_rejects.wrapping_add(1);
                    reply_err(pwr_ep_cap, reply_tok, IPC_OP_ERR_PERM, unauth_rejects);
                    continue;
                }
                s3_rejects = s3_rejects.wrapping_add(1);
                let err_op = match sys_power_ctl(POWER_OP_SUSPEND_S3, power_ctrl_cap) {
                    Err(ENOSYS) => IPC_OP_ERR_NOSYS,
                    Err(_) => IPC_OP_ERR_PERM,
                    Ok(_) => IPC_OP_PONG,
                };
                let mut slb = LineBuilder::new();
                slb.push_bytes(b"[DRY-POWER] S3 request audited -> -ENOSYS count=");
                slb.push_u64(s3_rejects);
                log_line(&slb);
                reply_err(pwr_ep_cap, reply_tok, err_op, s3_rejects);
            }
            IPC_OP_PWR_SHUTDOWN_S5 => {
                if ext.sender_badge != BADGE_SUPERVISOR {
                    unauth_rejects = unauth_rejects.wrapping_add(1);
                    reply_err(pwr_ep_cap, reply_tok, IPC_OP_ERR_PERM, unauth_rejects);
                    continue;
                }
                let _ = sys_write_debug(b"[DRY-POWER] Executing ACPI S5 hardware poweroff\n");
                let _ = sys_power_ctl(POWER_OP_SHUTDOWN_S5, power_ctrl_cap);
                reply_err(pwr_ep_cap, reply_tok, IPC_OP_ERR_NOSYS, 0);
            }
            IPC_OP_PWR_REBOOT => {
                if ext.sender_badge != BADGE_SUPERVISOR {
                    unauth_rejects = unauth_rejects.wrapping_add(1);
                    reply_err(pwr_ep_cap, reply_tok, IPC_OP_ERR_PERM, unauth_rejects);
                    continue;
                }
                let _ = sys_write_debug(b"[DRY-POWER] Executing hardware system reset\n");
                let _ = sys_power_ctl(POWER_OP_REBOOT, power_ctrl_cap);
                reply_err(pwr_ep_cap, reply_tok, IPC_OP_ERR_NOSYS, 0);
            }
            IPC_OP_EXIT_CLEAN => {
                if ext.sender_badge != BADGE_SUPERVISOR {
                    unauth_rejects = unauth_rejects.wrapping_add(1);
                    reply_err(pwr_ep_cap, reply_tok, IPC_OP_ERR_PERM, unauth_rejects);
                    continue;
                }
                let mut xlb = LineBuilder::new();
                xlb.push_bytes(b"[DRY-POWER-STATS] unauth_rej=");
                xlb.push_u64(unauth_rejects);
                xlb.push_bytes(b" s3_rej=");
                xlb.push_u64(s3_rejects);
                log_line(&xlb);
                if reply_tok != 0 {
                    let _ = sys_ipc_reply_inline(
                        pwr_ep_cap,
                        reply_tok,
                        pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
                        [unauth_rejects, s3_rejects, 0, 0],
                    );
                }
                let _ = sys_cap_revoke(power_ctrl_cap, 1);
                sys_exit(0);
            }
            _ => {
                reply_err(pwr_ep_cap, reply_tok, IPC_OP_ERR_INVAL, 0);
            }
        }
    }

    let _ = sys_cap_revoke(power_ctrl_cap, 1);
    sys_exit(0);
}
