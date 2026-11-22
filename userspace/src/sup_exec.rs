// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Dynamic ELF spawning from VFS (`/bin/...`, `/persist/...`, `/ram/...`) via `Frame2M`
//! (`IPC_OP_SUP_EXEC_VFS`, zero kernel VFS bloat, zero frame leaks).

use crate::abi_ext::sys_proc_spawn_with_len;
use crate::init_legacy::log_line;
use crate::shell::LineBuilder;
use crate::vfs_proto::{vfs_read_file, vfs_stat, VFS_FLAG_DIR};
use crate::{
    msg_tag_label, pack_msg_tag, sys_cap_revoke, sys_frame_alloc, sys_ipc_reply_inline,
    sys_proc_kill, sys_proc_wait, sys_vspace_map, BADGE_SUP_SHELL, EINVAL, EIO, EPERM,
    IPC_OP_ERR_INVAL, IPC_OP_ERR_PERM, IPC_OP_PONG, MAX_IPC_EXTRA_BYTES, VSPACE_MAP_OP_2M_RW,
    VSPACE_MAP_OP_UNMAP_2M,
};

const SUP_EXEC_VADDR: u64 = 0x2c00_0000;
const EXEC_COMP_ID: u16 = 0x0190;
const TWO_MIB: usize = 2 * 1024 * 1024;

#[inline]
fn reply_exec_err(sup_ep: u64, reply_tok: u64, opcode: u32, err_code: isize) {
    if reply_tok != 0 {
        let _ = sys_ipc_reply_inline(
            sup_ep,
            reply_tok,
            pack_msg_tag(opcode, 0, 1, 0),
            [err_code as u64, 0, 0, 0],
        );
    }
}

pub fn spawn_vfs_elf_checked(
    vfs_sup_cap: u64,
    path: &[u8],
    domain_code: u8,
    comp_id: u16,
    grant0: (u64, u16, u64),
    grant1: (u64, u16),
) -> Result<(u64, u64), isize> {
    if vfs_sup_cap == 0 || path.is_empty() || path.len() > 96 {
        return Err(EINVAL);
    }
    let (_ino, size, flags) = vfs_stat(vfs_sup_cap, path)?;
    if (flags & VFS_FLAG_DIR) != 0 || !(64..=TWO_MIB).contains(&size) {
        return Err(EINVAL);
    }
    let frame_cap = sys_frame_alloc(1)?;
    if let Err(e) = sys_vspace_map(frame_cap, SUP_EXEC_VADDR, VSPACE_MAP_OP_2M_RW) {
        let _ = sys_cap_revoke(frame_cap, 2);
        return Err(e);
    }

    // SAFETY: `frame_cap` (`2 MiB`) is mapped RW exclusively by `dry-init` at `SUP_EXEC_VADDR`
    // and unmapped + revoked before returning below.
    let buf = unsafe { core::slice::from_raw_parts_mut(SUP_EXEC_VADDR as *mut u8, size) };
    let res = match vfs_read_file(vfs_sup_cap, path, 0, buf) {
        Ok((got, _)) if got == size => {
            if &buf[0..4] != b"\x7fELF" {
                Err(EINVAL)
            } else {
                sys_proc_spawn_with_len(frame_cap, domain_code, comp_id, grant0, grant1, size)
            }
        }
        Ok(_) => Err(EIO),
        Err(e) => Err(e),
    };

    let _ = sys_vspace_map(frame_cap, SUP_EXEC_VADDR, VSPACE_MAP_OP_UNMAP_2M);
    let _ = sys_cap_revoke(frame_cap, 2);
    res
}

pub fn handle_exec_vfs_ipc(
    sup_ep: u64,
    reply_tok: u64,
    sender_badge: u64,
    vfs_sup_cap: u64,
    path: &[u8],
    tsc_mhz: u64,
) {
    if sender_badge != BADGE_SUP_SHELL {
        reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_PERM, EPERM);
        return;
    }

    let mut spawn_err: Option<isize> = None;
    let mut child_rc: i32 = 0;
    let mut child_pid: u64 = 0;

    match spawn_vfs_elf_checked(vfs_sup_cap, path, 0, EXEC_COMP_ID, (0, 0, 0), (0, 0)) {
        Ok((handle, pid)) => {
            child_pid = pid;
            let timeout = tsc_mhz.max(1000).saturating_mul(5_000_000);
            match sys_proc_wait(handle, 0, timeout) {
                Ok((0, code)) => child_rc = (code as u32) as i32,
                Ok((kind, detail)) => {
                    child_rc = -(((kind as i32) << 8) | (((detail & 0xff) as i32) + 1));
                }
                Err(_) => {
                    let _ = sys_proc_kill(handle);
                    let _ = sys_proc_wait(handle, 0, 0);
                    spawn_err = Some(crate::ETIMEDOUT);
                }
            }
        }
        Err(e) => spawn_err = Some(e),
    }

    if let Some(err) = spawn_err {
        let mut elb = LineBuilder::new();
        elb.push_bytes(b"[DRY-SUP] EXEC_VFS_FAIL path=");
        elb.push_bytes(path);
        elb.push_bytes(b" err=");
        elb.push_i64(err as i64);
        log_line(&elb);
        reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, err);
        return;
    }

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-SUP] EXEC_VFS path=");
    lb.push_bytes(path);
    lb.push_bytes(b" pid=");
    lb.push_u64(child_pid);
    lb.push_bytes(b" rc=");
    lb.push_i64(child_rc as i64);
    log_line(&lb);

    if reply_tok != 0 {
        let _ = sys_ipc_reply_inline(
            sup_ep,
            reply_tok,
            pack_msg_tag(IPC_OP_PONG, 0, 2, 0),
            [(child_rc as i64) as u64, child_pid, 0, 0],
        );
    }
}

pub fn handle_linux_run_ipc(
    sup_ep: u64,
    reply_tok: u64,
    sender_badge: u64,
    vfs_ep: u64,
    vfs_sup_cap: u64,
    path: &[u8],
    tsc_mhz: u64,
) {
    if sender_badge != BADGE_SUP_SHELL {
        reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_PERM, EPERM);
        return;
    }
    if vfs_ep == 0 || vfs_sup_cap == 0 || path.is_empty() || path.len() > 96 {
        reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, EINVAL);
        return;
    }
    let ipc_spec = crate::init_legacy::domain_spec(0, 0x0100);
    let cli_rights = crate::RIGHT_READ | crate::RIGHT_SEND | crate::RIGHT_CALL;
    let srv_rights = crate::RIGHT_READ | crate::RIGHT_RECV | crate::RIGHT_REPLY;
    let vfs_rights = cli_rights | crate::RIGHT_GRANT;
    let Ok(ctl_ep) = crate::sys_ep_create(ipc_spec) else {
        reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, crate::ENOMEM);
        return;
    };
    let Ok(ctl_cli) = crate::sys_cap_derive(ctl_ep, cli_rights, 0, crate::BADGE_SUPERVISOR) else {
        let _ = sys_cap_revoke(ctl_ep, 2);
        reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, EIO);
        return;
    };
    let abi_comp = crate::linux_abi::COMP_ID_LINUX_ABI;
    let abi_vfs_badge = (2u64 << 24) | (abi_comp as u64);
    let Ok(vfs_abi) = crate::sys_cap_derive(vfs_ep, vfs_rights, 0, abi_vfs_badge) else {
        let _ = sys_cap_revoke(ctl_cli, 1);
        let _ = sys_cap_revoke(ctl_ep, 2);
        reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, EIO);
        return;
    };

    let runner = match spawn_vfs_elf_checked(
        vfs_sup_cap,
        b"/bin/dry-linux-abi.elf",
        1,
        abi_comp,
        (ctl_ep, srv_rights, 0),
        (vfs_abi, vfs_rights),
    ) {
        Ok(v) => v,
        Err(e) => {
            let _ = sys_cap_revoke(vfs_abi, 1);
            let _ = sys_cap_revoke(ctl_cli, 1);
            let _ = sys_cap_revoke(ctl_ep, 2);
            reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, e);
            return;
        }
    };

    let timeout = tsc_mhz.max(1000).saturating_mul(6_000_000);
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let call_res = crate::vfs_proto::sys_ipc_call_extra(
        ctl_cli,
        crate::abi_ext::IPC_OP_SUP_LINUX_RUN,
        [0; 4],
        path,
        &mut rx,
        timeout,
    );
    if call_res.is_err() {
        let _ = sys_proc_kill(runner.0);
    }
    let _ = sys_proc_wait(runner.0, 0, timeout);
    let _ = sys_cap_revoke(vfs_abi, 1);
    let _ = sys_cap_revoke(ctl_cli, 1);
    let _ = sys_cap_revoke(ctl_ep, 2);

    match call_res {
        Ok((tag, rx_len, rmrs)) if msg_tag_label(tag) == IPC_OP_PONG => {
            let rc = (rmrs[0] as i64) as i32;
            let pid = rmrs[1];
            let sc = rmrs[2];
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"[DRY-SUP] LINUX_RUN path=");
            lb.push_bytes(path);
            lb.push_bytes(b" pid=");
            lb.push_u64(pid);
            lb.push_bytes(b" rc=");
            lb.push_i64(rc as i64);
            lb.push_bytes(b" syscalls=");
            lb.push_u64(sc);
            log_line(&lb);
            if reply_tok != 0 {
                let _ = crate::vfs_proto::sys_ipc_reply_extra(
                    sup_ep,
                    reply_tok,
                    IPC_OP_PONG,
                    [rmrs[0], pid, sc, rmrs[3]],
                    &rx[..rx_len.min(MAX_IPC_EXTRA_BYTES)],
                );
            }
        }
        Ok((_, _, rmrs)) => {
            reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, rmrs[0] as isize);
        }
        Err(e) => {
            reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, e);
        }
    }
}

pub fn handle_i2c_hid_test_ipc(
    sup_ep: u64,
    reply_tok: u64,
    sender_badge: u64,
    vfs_sup_cap: u64,
    broker_mod_cap: u64,
    tsc_mhz: u64,
) {
    if sender_badge != BADGE_SUP_SHELL {
        reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_PERM, EPERM);
        return;
    }
    if vfs_sup_cap == 0 || broker_mod_cap == 0 {
        reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, EINVAL);
        return;
    }
    let path = b"/bin/dry-i2c-hid.elf";
    let Ok((_ino, size, flags)) = vfs_stat(vfs_sup_cap, path) else {
        reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, EIO);
        return;
    };
    if (flags & VFS_FLAG_DIR) != 0 || !(64..=TWO_MIB).contains(&size) {
        reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, EINVAL);
        return;
    }
    let Ok(frame_cap) = sys_frame_alloc(1) else {
        reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, EIO);
        return;
    };
    if let Err(e) = sys_vspace_map(frame_cap, SUP_EXEC_VADDR, VSPACE_MAP_OP_2M_RW) {
        let _ = sys_cap_revoke(frame_cap, 2);
        reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, e);
        return;
    }
    // SAFETY: `frame_cap` (`2 MiB`) is mapped exclusively RW in `dry-init` at `SUP_EXEC_VADDR`.
    let elf_ok = {
        let buf = unsafe { core::slice::from_raw_parts_mut(SUP_EXEC_VADDR as *mut u8, size) };
        matches!(vfs_read_file(vfs_sup_cap, path, 0, buf), Ok((got, _)) if got == size && &buf[0..4] == b"\x7fELF")
    };
    let _ = sys_vspace_map(frame_cap, SUP_EXEC_VADDR, VSPACE_MAP_OP_UNMAP_2M);
    if !elf_ok {
        let _ = sys_cap_revoke(frame_cap, 2);
        reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, EINVAL);
        return;
    }

    let ipc_timeout = 2_000_000u64.saturating_mul(tsc_mhz.max(1000));
    let spec = crate::init_legacy::domain_spec(0, crate::island_proto::COMP_ID_DEV_BROKER);
    let cli_rights = crate::RIGHT_READ | crate::RIGHT_SEND | crate::RIGHT_CALL;
    let srv_rights = crate::RIGHT_READ | crate::RIGHT_RECV | crate::RIGHT_REPLY;
    let Ok(broker_ep) = crate::sys_ep_create(spec) else {
        let _ = sys_cap_revoke(frame_cap, 2);
        reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, EIO);
        return;
    };
    let Ok(admin_cap) = crate::sys_cap_derive(
        broker_ep,
        cli_rights,
        0,
        crate::island_proto::BADGE_BROKER_ADMIN,
    ) else {
        let _ = sys_cap_revoke(broker_ep, 2);
        let _ = sys_cap_revoke(frame_cap, 2);
        reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, EIO);
        return;
    };
    let Ok((proc_handle, _)) = crate::sys_proc_spawn(
        broker_mod_cap,
        1,
        crate::island_proto::COMP_ID_DEV_BROKER,
        (broker_ep, srv_rights, 0x0808),
        (frame_cap, crate::RIGHT_READ),
    ) else {
        let _ = sys_cap_revoke(admin_cap, 1);
        let _ = sys_cap_revoke(broker_ep, 2);
        let _ = sys_cap_revoke(frame_cap, 2);
        reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, EIO);
        return;
    };

    let dy_le = (-7i16).to_le_bytes();
    let pkt: u64 = u64::from_le_bytes([8, 0, 1, 1, 12, 0, dy_le[0], dy_le[1]]);
    let test_res = (|| -> Result<(i16, i16, u64), isize> {
        let (htag, _, hmrs) = crate::sys_ipc_call_inline(
            admin_cap,
            pack_msg_tag(crate::island_proto::BROKER_OP_ISLAND_HELLO, 0, 1, 0),
            [crate::island_proto::ISLAND_ABI_VERSION_V1, 0, 0, 0],
            ipc_timeout,
        )?;
        if msg_tag_label(htag) != IPC_OP_PONG
            || hmrs[0] != crate::island_proto::ISLAND_ABI_VERSION_V1
            || hmrs[1] != 1
        {
            return Err(EINVAL);
        }
        let (f1_tag, _, f1_mrs) = crate::sys_ipc_call_inline(
            admin_cap,
            pack_msg_tag(crate::island_proto::BROKER_OP_FORWARD_TRANSFORM, 0, 4, 0),
            [pkt, 0, 0, 1],
            ipc_timeout,
        )?;
        if msg_tag_label(f1_tag) != IPC_OP_PONG {
            return Err(EIO);
        }
        let dx1 = (f1_mrs[0] & 0xffff) as u16 as i16;
        let dy1 = ((f1_mrs[0] >> 16) & 0xffff) as u16 as i16;
        if dx1 != 12 || dy1 != -7 {
            return Err(EINVAL);
        }
        let (ctag, _, cmrs) = crate::sys_ipc_call_inline(
            admin_cap,
            pack_msg_tag(crate::island_proto::BROKER_OP_CRASH_AND_RECOVER, 0, 1, 0),
            [0; 4],
            ipc_timeout,
        )?;
        if msg_tag_label(ctag) != IPC_OP_PONG || cmrs[0] != 2 {
            return Err(EIO);
        }
        let (f2_tag, _, f2_mrs) = crate::sys_ipc_call_inline(
            admin_cap,
            pack_msg_tag(crate::island_proto::BROKER_OP_FORWARD_TRANSFORM, 0, 4, 0),
            [pkt, 0, 0, 2],
            ipc_timeout,
        )?;
        if msg_tag_label(f2_tag) != IPC_OP_PONG {
            return Err(EIO);
        }
        let dx2 = (f2_mrs[0] & 0xffff) as u16 as i16;
        let dy2 = ((f2_mrs[0] >> 16) & 0xffff) as u16 as i16;
        if dx2 != 12 || dy2 != -7 {
            return Err(EINVAL);
        }
        let _ = crate::sys_ipc_call_inline(
            admin_cap,
            pack_msg_tag(crate::island_proto::BROKER_OP_SHUTDOWN, 0, 1, 0),
            [0; 4],
            ipc_timeout,
        );
        Ok((dx2, dy2, cmrs[0]))
    })();

    if test_res.is_err() {
        let _ = sys_proc_kill(proc_handle);
    }
    if sys_proc_wait(proc_handle, 0, ipc_timeout).is_err() {
        let _ = sys_proc_kill(proc_handle);
        let _ = sys_proc_wait(proc_handle, 0, ipc_timeout);
    }
    let _ = sys_cap_revoke(admin_cap, 1);
    let _ = sys_cap_revoke(broker_ep, 2);
    let _ = sys_cap_revoke(frame_cap, 2);

    match test_res {
        Ok((dx, dy, gen)) => {
            let mut lb = LineBuilder::new();
            lb.push_bytes(b"[DRY-I2C-HID-TEST] PASS gen=");
            lb.push_u64(gen);
            lb.push_bytes(b" dx=");
            lb.push_i64(i64::from(dx));
            lb.push_bytes(b" dy=");
            lb.push_i64(i64::from(dy));
            log_line(&lb);
            if reply_tok != 0 {
                let _ = sys_ipc_reply_inline(
                    sup_ep,
                    reply_tok,
                    pack_msg_tag(IPC_OP_PONG, 0, 3, 0),
                    [gen, i64::from(dx) as u64, i64::from(dy) as u64, 0],
                );
            }
        }
        Err(e) => reply_exec_err(sup_ep, reply_tok, IPC_OP_ERR_INVAL, e),
    }
}
