// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-linux-abi` — Per-Execution Ring-3 Linux Personality Monitor (`0x0191`).

#![no_std]
#![no_main]

use dry_userspace::abi_ext::{
    sys_proc_spawn_with_redirect, IPC_OP_SUP_LINUX_RUN, SYSCALL_REDIRECT_LABEL,
};
use dry_userspace::init_legacy::domain_spec;
use dry_userspace::linux_abi::syscalls::GuestCtx;
use dry_userspace::linux_abi::{COMP_ID_LINUX_ABI, COMP_ID_LINUX_GUEST};
use dry_userspace::shell::LineBuilder;
use dry_userspace::vfs_proto::{sys_ipc_recv_extra, sys_ipc_reply_extra, vfs_read_file, vfs_stat, VFS_FLAG_DIR};
use dry_userspace::{
    msg_tag_label, pack_msg_tag, read_cs_ss, sys_cap_derive, sys_cap_revoke, sys_ep_create,
    sys_exit, sys_frame_alloc, sys_ipc_reply_inline, sys_proc_kill, sys_proc_wait, sys_sysinfo,
    sys_vspace_map, sys_write_debug, UserIpcRecvExt, UserSysInfo, EINVAL, EIO, ENOMEM,
    IPC_OP_ERR_INVAL, IPC_OP_PONG, MAX_IPC_EXTRA_BYTES, RIGHT_CALL, RIGHT_READ, RIGHT_SEND,
    VSPACE_MAP_OP_2M_RW, VSPACE_MAP_OP_UNMAP_2M,
};

const ELF_STAGE_VADDR: u64 = 0x2800_0000;
const TWO_MIB: usize = 2 * 1024 * 1024;

fn log_line(lb: &LineBuilder) {
    let mut out = [0u8; 192];
    let s = lb.as_bytes();
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s[..n]);
    out[n] = b'\n';
    let _ = sys_write_debug(&out[..=n]);
}

#[inline]
fn reply_ctl_err(ep: u64, tok: u64, err: isize) {
    if tok != 0 {
        let _ = sys_ipc_reply_inline(
            ep,
            tok,
            pack_msg_tag(IPC_OP_ERR_INVAL, 0, 1, 0),
            [err as u64, 0, 0, 0],
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(ctl_ep: u64, vfs_cap: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 || ctl_ep == 0 || vfs_cap == 0 {
        sys_exit(194);
    }

    let mut sys = UserSysInfo::default();
    let _ = sys_sysinfo(&mut sys);

    let mut ctl_rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let mut ctl_ext = UserIpcRecvExt::default();
    let Ok((tag, plen, _)) = sys_ipc_recv_extra(ctl_ep, 0, 0, &mut ctl_rx, &mut ctl_ext) else {
        sys_exit(195);
    };
    if ctl_ext.transferred_cap != 0 {
        let _ = sys_cap_revoke(ctl_ext.transferred_cap, 1);
    }
    if msg_tag_label(tag) != IPC_OP_SUP_LINUX_RUN || plen == 0 || plen > 96 {
        reply_ctl_err(ctl_ep, ctl_ext.reply_token, EINVAL);
        sys_exit(196);
    }
    let path = &ctl_rx[..plen];

    let (_, elf_size, vflags) = match vfs_stat(vfs_cap, path) {
        Ok(v) => v,
        Err(e) => {
            reply_ctl_err(ctl_ep, ctl_ext.reply_token, e);
            sys_exit(197);
        }
    };
    if (vflags & VFS_FLAG_DIR) != 0 || !(64..=TWO_MIB).contains(&elf_size) {
        reply_ctl_err(ctl_ep, ctl_ext.reply_token, EINVAL);
        sys_exit(197);
    }

    let Ok(frame_cap) = sys_frame_alloc(1) else {
        reply_ctl_err(ctl_ep, ctl_ext.reply_token, ENOMEM);
        sys_exit(198);
    };
    if let Err(e) = sys_vspace_map(frame_cap, ELF_STAGE_VADDR, VSPACE_MAP_OP_2M_RW) {
        let _ = sys_cap_revoke(frame_cap, 2);
        reply_ctl_err(ctl_ep, ctl_ext.reply_token, e);
        sys_exit(198);
    }

    let Ok(redir_ep) = sys_ep_create(domain_spec(1, COMP_ID_LINUX_ABI)) else {
        let _ = sys_vspace_map(frame_cap, ELF_STAGE_VADDR, VSPACE_MAP_OP_UNMAP_2M);
        let _ = sys_cap_revoke(frame_cap, 2);
        reply_ctl_err(ctl_ep, ctl_ext.reply_token, EIO);
        sys_exit(199);
    };
    let Ok(redir_send) = sys_cap_derive(redir_ep, RIGHT_READ | RIGHT_SEND | RIGHT_CALL, 0, 0) else {
        let _ = sys_cap_revoke(redir_ep, 2);
        let _ = sys_vspace_map(frame_cap, ELF_STAGE_VADDR, VSPACE_MAP_OP_UNMAP_2M);
        let _ = sys_cap_revoke(frame_cap, 2);
        reply_ctl_err(ctl_ep, ctl_ext.reply_token, EIO);
        sys_exit(199);
    };

    // SAFETY: `frame_cap` (`2 MiB`) is exclusively mapped `USER_RW` at `ELF_STAGE_VADDR`.
    let elf_buf = unsafe { core::slice::from_raw_parts_mut(ELF_STAGE_VADDR as *mut u8, elf_size) };
    let spawn_res = match vfs_read_file(vfs_cap, path, 0, elf_buf) {
        Ok((got, _)) if got == elf_size && &elf_buf[0..4] == b"\x7fELF" => {
            sys_proc_spawn_with_redirect(frame_cap, COMP_ID_LINUX_GUEST, redir_send, elf_size)
        }
        Ok(_) => Err(EINVAL),
        Err(e) => Err(e),
    };

    let _ = sys_vspace_map(frame_cap, ELF_STAGE_VADDR, VSPACE_MAP_OP_UNMAP_2M);
    let _ = sys_cap_revoke(frame_cap, 2);

    let Ok((guest_handle, guest_pid)) = spawn_res else {
        let _ = sys_cap_revoke(redir_send, 1);
        let _ = sys_cap_revoke(redir_ep, 2);
        reply_ctl_err(ctl_ep, ctl_ext.reply_token, spawn_res.unwrap_err());
        sys_exit(200);
    };

    let mut lb = LineBuilder::new();
    lb.push_bytes(b"[DRY-LXABI] Running Linux ELF ");
    lb.push_bytes(path);
    lb.push_bytes(b" pid=");
    lb.push_u64(guest_pid);
    log_line(&lb);

    let mut ctx = GuestCtx::new(redir_ep, guest_pid, vfs_cap, sys.tsc_mhz);
    let mut rx = [0u8; MAX_IPC_EXTRA_BYTES];
    let recv_timeout = sys.tsc_mhz.max(1000).saturating_mul(2_000_000);

    while ctx.exited.is_none() {
        let mut ext = UserIpcRecvExt::default();
        let Ok((rtag, rx_len, rmrs)) = sys_ipc_recv_extra(redir_ep, 0, recv_timeout, &mut rx, &mut ext) else {
            break;
        };
        if ext.transferred_cap != 0 {
            let _ = sys_cap_revoke(ext.transferred_cap, 1);
        }
        if msg_tag_label(rtag) != SYSCALL_REDIRECT_LABEL || rx_len < 24 {
            break;
        }
        let r10 = u64::from_le_bytes(rx[0..8].try_into().unwrap());
        let r8 = u64::from_le_bytes(rx[8..16].try_into().unwrap());
        let r9 = u64::from_le_bytes(rx[16..24].try_into().unwrap());
        if ctx.handle_syscall(rmrs[0], [rmrs[1], rmrs[2], rmrs[3], r10, r8, r9], ext.reply_token).is_err() {
            break;
        }
    }

    if ctx.exited.is_none() {
        let _ = sys_proc_kill(guest_handle);
    }
    let exit_rc = match sys_proc_wait(guest_handle, 0, sys.tsc_mhz.max(1000).saturating_mul(2_000_000)) {
        Ok((0, code)) => (code as u32) as i32,
        Ok((kind, detail)) => -(((kind as i32) << 8) | (((detail & 0xff) as i32) + 1)),
        Err(_) => -1,
    };

    let _ = sys_cap_revoke(redir_send, 1);
    let _ = sys_cap_revoke(redir_ep, 2);

    let slen = ctx.fds.stdout_len.min(MAX_IPC_EXTRA_BYTES);
    let _ = sys_ipc_reply_extra(
        ctl_ep,
        ctl_ext.reply_token,
        IPC_OP_PONG,
        [(exit_rc as i64) as u64, guest_pid, ctx.syscall_count, slen as u64],
        &ctx.fds.stdout_buf[..slen],
    );

    let mut dl = LineBuilder::new();
    dl.push_bytes(b"[DRY-LXABI] Done pid=");
    dl.push_u64(guest_pid);
    dl.push_bytes(b" rc=");
    dl.push_i64(exit_rc as i64);
    dl.push_bytes(b" syscalls=");
    dl.push_u64(ctx.syscall_count);
    log_line(&dl);
    sys_exit(0);
}
