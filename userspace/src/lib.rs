// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Minimal `no_std` DRY Native Syscall ABI (`v0.2`) bindings for Ring 3 userspace binaries.

#![no_std]

use core::arch::asm;
use core::panic::PanicInfo;

pub const DRY_SYSCALL_ABI_VERSION: u64 = 0x0001_0000;

pub const SYS_ABI_PING: u64 = 0;
pub const SYS_WRITE_DEBUG: u64 = 1;
pub const SYS_YIELD: u64 = 2;
pub const SYS_EXIT: u64 = 3;
pub const SYS_GET_TID: u64 = 4;
pub const SYS_CAP_QUERY: u64 = 5;
pub const SYS_EP_CREATE: u64 = 6;
pub const SYS_IPC_SEND: u64 = 7;
pub const SYS_IPC_RECV: u64 = 8;
pub const SYS_IPC_CALL: u64 = 9;
pub const SYS_IPC_REPLY: u64 = 10;
pub const SYS_IPC_REPLY_RECV: u64 = 11;
pub const SYS_NOTIFY_CREATE: u64 = 12;
pub const SYS_NOTIFY_SIGNAL: u64 = 13;
pub const SYS_NOTIFY_WAIT: u64 = 14;
pub const SYS_CAP_DERIVE: u64 = 15;
pub const SYS_CAP_REVOKE: u64 = 16;
pub const SYS_PROC_SPAWN: u64 = 17;
pub const SYS_PROC_WAIT: u64 = 18;
pub const SYS_FB_MAP: u64 = 19;

pub const EPERM: isize = -1;
pub const EBADCAP: isize = -9;
pub const ENOCHILD: isize = -10;
pub const EAGAIN: isize = -11;
pub const ENOMEM: isize = -12;
pub const EFAULT: isize = -14;
pub const EBUSY: isize = -16;
pub const EINVAL: isize = -22;
pub const EPIPE: isize = -32;
pub const EDEADLK: isize = -35;
pub const ENOSYS: isize = -38;
pub const EMSGSIZE: isize = -90;
pub const EREVOKED: isize = -110;
pub const ETIMEDOUT: isize = -116;
pub const ECANCELED: isize = -125;

// Capability right bits (`kernel/src/security.rs`)
pub const RIGHT_READ: u16 = 1 << 0;
pub const RIGHT_WRITE: u16 = 1 << 1;
pub const RIGHT_EXEC: u16 = 1 << 2;
pub const RIGHT_GRANT: u16 = 1 << 3;
pub const RIGHT_SIGNAL: u16 = 1 << 4;
pub const RIGHT_RETYPE: u16 = 1 << 5;
pub const RIGHT_MAP: u16 = 1 << 6;
pub const RIGHT_MANAGE: u16 = 1 << 7;
pub const RIGHT_REVOKE: u16 = 1 << 8;
pub const RIGHT_SEND: u16 = 1 << 9;
pub const RIGHT_RECV: u16 = 1 << 10;
pub const RIGHT_CALL: u16 = 1 << 11;
pub const RIGHT_REPLY: u16 = 1 << 12;
pub const RIGHT_WAIT: u16 = 1 << 13;
pub const RIGHT_SPAWN: u16 = 1 << 14;

// IPC opcodes shared across `dry-init`, `dry-echo`, and `dry-display`
pub const IPC_OP_PING_TRACE: u32 = 1;
pub const IPC_OP_PING_SILENT: u32 = 2;
pub const IPC_OP_PONG: u32 = 3;
pub const IPC_OP_CRASH_UD2: u32 = 4;
pub const IPC_OP_EXIT_CLEAN: u32 = 5;
pub const IPC_OP_DISPLAY_DONE: u32 = 6;

/// Boot capability table mapped read-only into `dry-init` at `arg0`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RootInitBootInfo {
    pub magic: u64,
    pub echo_mod_cap: u64,
    pub display_mod_cap: u64,
    pub fb_cap: u64,
}

pub const ROOT_INIT_BOOT_MAGIC: u64 = 0x4452_595f_494e_4954; // "DRY_INIT"

/// Sanitized capability metadata returned by `SYS_CAP_QUERY` (zero kernel pointers or PFNs).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UserCapInfo {
    pub obj_kind: u8,
    pub domain: u8,
    pub flags: u16,
    pub rights_bits: u32,
    pub compartment_id: u16,
    pub _pad: u16,
    pub lease_epoch: u32,
    pub badge: u64,
    pub abi_reserved: u64,
}

/// Sanitized linear framebuffer metadata returned by `SYS_FB_MAP`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UserFbInfo {
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bpp: u16,
    pub memory_model: u8,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
    pub red_size: u8,
    pub green_size: u8,
    pub blue_size: u8,
    pub _pad: [u8; 3],
    pub mapped_bytes: u64,
}

#[inline(always)]
pub const fn pack_msg_tag(label: u32, flags: u8, words: u8, extra_len: u16) -> u64 {
    (((label & 0x7fff_ffff) as u64) << 32)
        | ((extra_len as u64) << 16)
        | ((flags as u64) << 8)
        | (words as u64)
}

#[inline(always)]
pub const fn msg_tag_label(tag: u64) -> u32 {
    ((tag >> 32) & 0x7fff_ffff) as u32
}

#[inline(always)]
pub fn raw_syscall6(
    nr: u64,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
) -> (isize, u64) {
    let ret0: isize;
    let ret1: u64;
    // SAFETY: Executes Ring 3 `syscall` according to DRY Native Syscall ABI v0.2 (`ADR-002`).
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr as isize => ret0,
            inlateout("rdi") a0 => _,
            inlateout("rsi") a1 => _,
            inlateout("rdx") a2 => ret1,
            inlateout("r10") a3 => _,
            inlateout("r8") a4 => _,
            inlateout("r9") a5 => _,
            out("rcx") _,
            out("r11") _,
            options(nostack),
        );
    }
    (ret0, ret1)
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub fn raw_ipc_syscall(
    nr: u64,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
    mrs_in: [u64; 4],
) -> (isize, u64, [u64; 4]) {
    let ret0: isize;
    let ret1: u64;
    let mr0: u64;
    let mr1: u64;
    let mr2: u64;
    let mr3: u64;
    // SAFETY: Binds `r12..r15` directly as zero-copy inline IPC message registers (`ADR-002`).
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr as isize => ret0,
            inlateout("rdi") a0 => _,
            inlateout("rsi") a1 => _,
            inlateout("rdx") a2 => ret1,
            inlateout("r10") a3 => _,
            inlateout("r8") a4 => _,
            inlateout("r9") a5 => _,
            inlateout("r12") mrs_in[0] => mr0,
            inlateout("r13") mrs_in[1] => mr1,
            inlateout("r14") mrs_in[2] => mr2,
            inlateout("r15") mrs_in[3] => mr3,
            out("rcx") _,
            out("r11") _,
            options(nostack),
        );
    }
    (ret0, ret1, [mr0, mr1, mr2, mr3])
}

#[inline]
pub fn sys_abi_ping() -> (isize, u64) {
    raw_syscall6(SYS_ABI_PING, 0, 0, 0, 0, 0, 0)
}

#[inline]
pub fn sys_write_debug(bytes: &[u8]) -> isize {
    raw_syscall6(SYS_WRITE_DEBUG, bytes.as_ptr() as u64, bytes.len() as u64, 0, 0, 0, 0).0
}

#[inline]
pub fn sys_write_debug_raw(ptr: u64, len: u64) -> isize {
    raw_syscall6(SYS_WRITE_DEBUG, ptr, len, 0, 0, 0, 0).0
}

#[inline]
pub fn sys_yield() -> isize {
    raw_syscall6(SYS_YIELD, 0, 0, 0, 0, 0, 0).0
}

#[inline]
pub fn sys_exit(code: i32) -> ! {
    let _ = raw_syscall6(SYS_EXIT, code as u64, 0, 0, 0, 0, 0);
    loop {
        // SAFETY: Unreachable fallback; `ud2` traps into Ring 0 fault reaper.
        unsafe {
            asm!("ud2", options(nomem, nostack));
        }
    }
}

#[inline]
pub fn sys_get_tid() -> (u64, u64) {
    let (r0, r1) = raw_syscall6(SYS_GET_TID, 0, 0, 0, 0, 0, 0);
    (r0 as u64, r1)
}

#[inline]
pub fn sys_cap_query(cap_raw: u64, out: &mut UserCapInfo) -> isize {
    raw_syscall6(
        SYS_CAP_QUERY,
        cap_raw,
        (out as *mut UserCapInfo) as u64,
        0,
        0,
        0,
        0,
    )
    .0
}

#[inline]
pub fn sys_cap_query_raw(cap_raw: u64, out_ptr: u64) -> isize {
    raw_syscall6(SYS_CAP_QUERY, cap_raw, out_ptr, 0, 0, 0, 0).0
}

#[inline]
pub fn sys_ep_create(domain_spec: u64) -> Result<u64, isize> {
    let (r0, cap) = raw_syscall6(SYS_EP_CREATE, domain_spec, 0, 0, 0, 0, 0);
    if r0 < 0 { Err(r0) } else { Ok(cap) }
}

#[inline]
pub fn sys_notify_create(domain_spec: u64) -> Result<u64, isize> {
    let (r0, cap) = raw_syscall6(SYS_NOTIFY_CREATE, domain_spec, 0, 0, 0, 0, 0);
    if r0 < 0 { Err(r0) } else { Ok(cap) }
}

#[inline]
pub fn sys_notify_signal(notif_cap: u64, bits: u64) -> Result<u64, isize> {
    let (r0, out_bits) = raw_syscall6(SYS_NOTIFY_SIGNAL, notif_cap, bits, 0, 0, 0, 0);
    if r0 < 0 { Err(r0) } else { Ok(out_bits) }
}

#[inline]
pub fn sys_notify_wait(notif_cap: u64, flags: u64, timeout_ticks: u64) -> Result<u64, isize> {
    let (r0, bits) = raw_syscall6(SYS_NOTIFY_WAIT, notif_cap, flags, timeout_ticks, 0, 0, 0);
    if r0 < 0 { Err(r0) } else { Ok(bits) }
}

#[inline]
pub fn sys_cap_derive(
    src_cap: u64,
    new_rights: u16,
    lease_epoch: u32,
    badge: u64,
) -> Result<u64, isize> {
    let (r0, cap) = raw_syscall6(
        SYS_CAP_DERIVE,
        src_cap,
        new_rights as u64,
        lease_epoch as u64,
        badge,
        0,
        0,
    );
    if r0 < 0 { Err(r0) } else { Ok(cap) }
}

#[inline]
pub fn sys_cap_revoke(cap_ptr: u64, mode: u64) -> Result<usize, isize> {
    let (r0, _) = raw_syscall6(SYS_CAP_REVOKE, cap_ptr, mode, 0, 0, 0, 0);
    if r0 < 0 { Err(r0) } else { Ok(r0 as usize) }
}

#[inline]
pub fn sys_ipc_send_inline(ep_cap: u64, tag: u64, mrs: [u64; 4], timeout: u64) -> Result<(), isize> {
    let (r0, _, _) = raw_ipc_syscall(SYS_IPC_SEND, ep_cap, tag, timeout, 0, 0, 0, mrs);
    if r0 < 0 { Err(r0) } else { Ok(()) }
}

#[inline]
pub fn sys_ipc_recv_inline(
    ep_cap: u64,
    flags: u64,
    timeout: u64,
) -> Result<(u64, u64, [u64; 4]), isize> {
    let (r0, aux, mrs) = raw_ipc_syscall(SYS_IPC_RECV, ep_cap, flags, timeout, 0, 0, 0, [0; 4]);
    if r0 < 0 {
        Err(r0)
    } else {
        Ok((r0 as u64, aux, mrs))
    }
}

#[inline]
pub fn sys_ipc_call_inline(
    ep_cap: u64,
    tag: u64,
    mrs: [u64; 4],
    timeout: u64,
) -> Result<(u64, u64, [u64; 4]), isize> {
    let (r0, aux, out_mrs) = raw_ipc_syscall(SYS_IPC_CALL, ep_cap, tag, timeout, 0, 0, 0, mrs);
    if r0 < 0 {
        Err(r0)
    } else {
        Ok((r0 as u64, aux, out_mrs))
    }
}

#[inline]
pub fn sys_ipc_reply_inline(
    ep_cap: u64,
    reply_token: u64,
    tag: u64,
    mrs: [u64; 4],
) -> Result<(), isize> {
    let (r0, _, _) = raw_ipc_syscall(SYS_IPC_REPLY, ep_cap, reply_token, tag, 0, 0, 0, mrs);
    if r0 < 0 { Err(r0) } else { Ok(()) }
}

#[inline]
pub fn sys_ipc_reply_recv_inline(
    ep_cap: u64,
    reply_token: u64,
    reply_tag: u64,
    reply_mrs: [u64; 4],
    recv_flags: u64,
    recv_timeout: u64,
) -> Result<(u64, u64, [u64; 4]), isize> {
    let (r0, aux, out_mrs) = raw_ipc_syscall(
        SYS_IPC_REPLY_RECV,
        ep_cap,
        reply_token,
        reply_tag,
        recv_flags,
        recv_timeout,
        0,
        reply_mrs,
    );
    if r0 < 0 {
        Err(r0)
    } else {
        Ok((r0 as u64, aux, out_mrs))
    }
}

#[inline]
pub fn sys_proc_spawn(
    boot_mod_cap: u64,
    domain_code: u8,
    compartment_id: u16,
    grant0: (u64, u16, u64),
    grant1: (u64, u16),
) -> Result<(u64, u64), isize> {
    let dom_comp = (domain_code as u64) | ((compartment_id as u64) << 16) | (0xffffu64 << 32);
    let g0_rb = (grant0.1 as u64) | (grant0.2 << 16);
    let (r0, pid) = raw_syscall6(
        SYS_PROC_SPAWN,
        boot_mod_cap,
        dom_comp,
        grant0.0,
        g0_rb,
        grant1.0,
        grant1.1 as u64,
    );
    if r0 < 0 {
        Err(r0)
    } else {
        Ok((r0 as u64, pid))
    }
}

#[inline]
pub fn sys_proc_wait(child_handle: u64, flags: u64, timeout: u64) -> Result<(isize, u64), isize> {
    let (r0, detail) = raw_syscall6(SYS_PROC_WAIT, child_handle, flags, timeout, 0, 0, 0);
    if r0 < 0 {
        Err(r0)
    } else {
        Ok((r0, detail))
    }
}

#[inline]
pub fn sys_fb_map(fb_cap: u64, user_vaddr: u64, out: &mut UserFbInfo) -> Result<usize, isize> {
    let (r0, _) = raw_syscall6(
        SYS_FB_MAP,
        fb_cap,
        user_vaddr,
        (out as *mut UserFbInfo) as u64,
        0,
        0,
        0,
    );
    if r0 < 0 {
        Err(r0)
    } else {
        Ok(r0 as usize)
    }
}

/// Read live architectural `CS` and `SS` segment selectors at CPL=3 to prove hardware Ring 3.
#[inline]
pub fn read_cs_ss() -> (u16, u16) {
    let cs: u16;
    let ss: u16;
    // SAFETY: Reading segment selectors into GPRs is unprivileged in 64-bit mode.
    unsafe {
        asm!(
            "mov {cs:x}, cs",
            "mov {ss:x}, ss",
            cs = out(reg) cs,
            ss = out(reg) ss,
            options(nomem, nostack, preserves_flags),
        );
    }
    (cs, ss)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys_exit(101)
}
