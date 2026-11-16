// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Minimal `no_std` DRY Native Syscall ABI (`v0.2`) bindings & Ring 3 service support modules.

#![no_std]

pub mod abi_ext;
pub mod broker_lifecycle;
pub mod action;
pub mod apps;
pub mod compositor;
pub mod display_banner;
pub mod gfx;
pub mod glass;
pub mod gpu_audit;
pub mod gpu_proto;
pub mod init_legacy;
pub mod input;
pub mod ioring;
pub mod island_audit;
pub mod island_proto;
pub mod keymap;
pub mod lx_compat_meta;
pub mod lx_drm_audit;
pub mod lx_drv_proto;
pub mod lx_island_audit;
pub mod persist_fs;
pub mod virtio_gpu;
pub mod ps2;
pub mod shell;
pub mod shell_ext;
pub mod sup_audit;
pub mod sup_ctx;
pub mod sup_services;
pub mod supervisor;
pub mod surface;
pub mod ttf;
pub mod ui;
pub mod vault_crypto;
pub mod vault_proto;
pub mod vfs_mem;
pub mod vfs_proto;
pub mod wm;
pub mod wm_events;
pub mod wm_ipc;
pub mod wm_layout;
pub mod wm_types;

pub use abi_ext::*;

use core::arch::asm;
use core::mem::size_of;
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
pub const SYS_IRQ_BIND: u64 = 20;
pub const SYS_IRQ_ACK: u64 = 21;
pub const SYS_IOPORT_RW: u64 = 22;
pub const SYS_SYSINFO: u64 = 23;
pub const SYS_FRAME_ALLOC: u64 = 24;
pub const SYS_VSPACE_MAP: u64 = 25;
pub const SYS_PROC_LIST: u64 = 26;
pub const SYS_PROC_KILL: u64 = 27;
pub const SYS_POWER_CTL: u64 = 28;

pub const EPERM: isize = -1;
pub const ENOENT: isize = -2;
pub const EBADCAP: isize = -9;
pub const ENOCHILD: isize = -10;
pub const EAGAIN: isize = -11;
pub const ENOMEM: isize = -12;
pub const EFAULT: isize = -14;
pub const EBUSY: isize = -16;
pub const EEXIST: isize = -17;
pub const EINVAL: isize = -22;
pub const ENOSPC: isize = -28;
pub const EROFS: isize = -30;
pub const EPIPE: isize = -32;
pub const EDEADLK: isize = -35;
pub const ENOSYS: isize = -38;
pub const EMSGSIZE: isize = -90;
pub const EREVOKED: isize = -110;
pub const EALREADY: isize = -114;
pub const ETIMEDOUT: isize = -116;
pub const ECANCELED: isize = -125;

pub const MAX_IPC_EXTRA_BYTES: usize = 256;

// IPC MsgTag / syscall flag bits (`kernel/src/ipc/mod.rs`)
pub const MSG_FLAG_EXTRA_BUF: u8 = 1 << 0;
pub const MSG_FLAG_GRANT_CAP: u8 = 1 << 1;
pub const MSG_FLAG_NON_BLOCKING: u8 = 1 << 2;
pub const MSG_FLAG_SHARED_DESC: u8 = 1 << 3;
pub const NOTIFY_FLAG_BIND_TCB: u64 = 1 << 4;
pub const NOTIFY_FLAG_UNBIND_TCB: u64 = 1 << 5;
pub const IPC_OP_BOUND_NOTIF: u32 = 0x7fff_0001;

// IRQ / IoPort / VSpace map operation constants (`kernel/src/syscall/cold.rs`)
pub const IRQF_ONESHOT: u64 = 1;
pub const IRQ_ACK_UNMASK: u64 = 0;
pub const IRQ_ACK_MASK: u64 = 1;
pub const IRQ_ACK_UNBIND: u64 = 2;

pub const IOPORT_OP_INB: u64 = 0;
pub const IOPORT_OP_OUTB: u64 = 1;

pub const VSPACE_MAP_OP_4K_RO: u64 = 0;
pub const VSPACE_MAP_OP_4K_RW: u64 = 1;
pub const VSPACE_MAP_OP_2M_RO: u64 = 2;
pub const VSPACE_MAP_OP_2M_RW: u64 = 3;
pub const VSPACE_MAP_OP_UNMAP_4K: u64 = 4;
pub const VSPACE_MAP_OP_UNMAP_2M: u64 = 5;

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

// Unforgeable capability badges minted by `dry-init` onto `disp_ep` & `sup_ep` (`Milestone 2A-2 / 2A-3`)
pub const BADGE_SUPERVISOR: u64 = 0x0001_ffff;
pub const BADGE_INPUT_SVC: u64 = 0x0003_ffff;
pub const BADGE_POWER_SVC: u64 = 0x0005_ffff;
pub const BADGE_WIN_LEGACY_P1: u64 = 0x54e2;
pub const BADGE_WIN_SHELL: u64 = 0x0102_0001;
pub const BADGE_WIN_SYSMON: u64 = 0x0103_0002;
pub const BADGE_WIN_FILES: u64 = 0x0104_0003;
pub const BADGE_WIN_SETTINGS: u64 = 0x0105_0004;
pub const BADGE_WIN_EDITOR: u64 = 0x0106_0005;
pub const BADGE_WIN_CALC: u64 = 0x0107_0006;
pub const BADGE_WIN_ROGUE_PROBE: u64 = 0x0109_0099;

pub const BADGE_SUP_SHELL: u64 = 0x5501;
pub const BADGE_SUP_SYSMON: u64 = 0x5502;
pub const BADGE_SUP_FILES: u64 = 0x5503;
pub const BADGE_SUP_SETTINGS: u64 = 0x5504;
pub const BADGE_SUP_DISPLAY: u64 = 0x5505;
pub const BADGE_SUP_EDITOR: u64 = 0x5506;
pub const BADGE_SUP_CALC: u64 = 0x5507;
pub const BADGE_SUP_ROGUE: u64 = 0x5599;

// IPC opcodes shared across `dry-init`, `dry-echo`, `dry-display`, `dry-input`, `dry-shell`, and `dry-sysmon`
pub const IPC_OP_PING_TRACE: u32 = 1;
pub const IPC_OP_PING_SILENT: u32 = 2;
pub const IPC_OP_PONG: u32 = 3;
pub const IPC_OP_CRASH_UD2: u32 = 4;
pub const IPC_OP_EXIT_CLEAN: u32 = 5;
pub const IPC_OP_DISPLAY_DONE: u32 = 6;

pub const IPC_OP_INPUT_INIT_PORT: u32 = 10;
pub const IPC_OP_INPUT_WAIT: u32 = 11;
pub const IPC_OP_INPUT_EVENT: u32 = 12;
pub const IPC_OP_INPUT_OVERFLOW_TEST: u32 = 13;
pub const IPC_OP_INPUT_INIT_MOUSE: u32 = 14;
pub const IPC_OP_INPUT_SET_WM_SINK: u32 = 15;
pub const IPC_OP_INPUT_QUERY_STATS: u32 = 16;

pub const IPC_OP_SURFACE_ATTACH: u32 = 20;
pub const IPC_OP_SURFACE_PRESENT: u32 = 21;
pub const IPC_OP_SURFACE_MOVE: u32 = 22;
pub const IPC_OP_SURFACE_FOCUS: u32 = 23;
pub const IPC_OP_WIN_BIND_NOTIF: u32 = 24;
pub const IPC_OP_WIN_WAIT_EVENT: u32 = 25;
pub const IPC_OP_WIN_POLL_EVENT: u32 = 26;
pub const IPC_OP_WIN_CLOSE: u32 = 27;
pub const IPC_OP_WM_INPUT_EVENT: u32 = 28;
pub const IPC_OP_WM_TOGGLE_TILING: u32 = 29;

pub const IPC_OP_SUP_HELLO: u32 = 30;
pub const IPC_OP_SUP_RECONNECT_INPUT: u32 = 31;
pub const IPC_OP_SUP_SHUTDOWN: u32 = 32;
pub const IPC_OP_SUP_APP_EXITED: u32 = 33;
pub const IPC_OP_WM_OWNER_GONE: u32 = 34;
pub const IPC_OP_WIN_EMPTY: u32 = 35;
pub const IPC_OP_WIN_CLOSED: u32 = 36;
pub const IPC_OP_ERR_PERM: u32 = 37;
pub const IPC_OP_ERR_INVAL: u32 = 38;
pub const IPC_OP_WM_QUERY_STATE: u32 = 39;

pub const IPC_OP_SUP_ACTION: u32 = 40;
pub const IPC_OP_SUP_QUERY_APPS: u32 = 41;
pub const IPC_OP_WM_BIND_SUP_NOTIF: u32 = 42;
pub const IPC_OP_WM_POLL_ACTION: u32 = 43;
pub const IPC_OP_WM_FOCUS_OWNER: u32 = 44;
pub const IPC_OP_WM_SET_LAYOUT: u32 = 45;
pub const IPC_OP_WM_SET_PREFS: u32 = 46;
pub const IPC_OP_WM_SET_SESSION_STATE: u32 = 47;
pub const IPC_OP_INPUT_SET_PREFS: u32 = 48;
pub const IPC_OP_ERR_NOSYS: u32 = 49;
pub const IPC_OP_SUP_MINT_ROGUE_TEST: u32 = 50;
pub const IPC_OP_SUP_QUERY_PREFS: u32 = 51;
pub const IPC_OP_SUP_GET_VFS_CAP: u32 = 52;
pub const IPC_OP_PWR_QUERY: u32 = 53;
pub const IPC_OP_PWR_SHUTDOWN_S5: u32 = 54;
pub const IPC_OP_PWR_REBOOT: u32 = 55;
pub const IPC_OP_PWR_SUSPEND_S3: u32 = 56;

pub const POWER_OP_QUERY: u64 = 0;
pub const POWER_OP_SHUTDOWN_S5: u64 = 1;
pub const POWER_OP_REBOOT: u64 = 2;
pub const POWER_OP_SUSPEND_S3: u64 = 3;

pub const BOOT_MANIFEST_OFFSET: u64 = 0x0100;

/// Boot capability table mapped read-only into `dry-init` at `arg0` (`0x0070_0000`).
/// Legacy Milestone 1C zeroes trailing fields (`session_mode == 0`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RootInitBootInfo {
    pub magic: u64,
    pub echo_mod_cap: u64,
    pub display_mod_cap: u64,
    pub fb_cap: u64,
    pub input_mod_cap: u64,
    pub shell_mod_cap: u64,
    pub irq1_cap: u64,
    pub i8042_port_cap: u64,
    pub session_mode: u64,
    pub irq12_cap: u64,
    pub sysmon_mod_cap: u64,
    pub vfs_mod_cap: u64,
    pub files_mod_cap: u64,
    pub settings_mod_cap: u64,
    pub power_mod_cap: u64,
    pub power_ctrl_cap: u64,
    pub boot_manifest_count: u64,
    pub blk_mod_cap: u64,
    pub vault_mod_cap: u64,
    pub pci_blk_cap: u64,
    pub vault_ctrl_cap: u64,
    pub broker_mod_cap: u64,
    pub mock_drv_mod_cap: u64,
    pub lx_rng_mod_cap: u64,
    pub pci_rng_cap: u64,
    pub gpu_mod_cap: u64,
    pub pci_gpu_cap: u64,
    pub editor_mod_cap: u64,
    pub calc_mod_cap: u64,
    pub lx_drm_mod_cap: u64,
    pub pci_disp_aux_cap: u64,
    pub boot_pci_func_count: u64,
}

const _: () = assert!(size_of::<RootInitBootInfo>() == BOOT_MANIFEST_OFFSET as usize);

pub const BOOT_PCI_ENUM_OFFSET: u64 = 0x0800;
pub const MAX_BOOT_PCI_FUNCS: usize = 64;

/// Sanitized 32-byte PCI function descriptor published read-only at `boot_info_addr + 0x0800`.
#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub struct BootPciFuncRecord {
    pub bus_dev_fn: u16,
    pub vendor_id: u16,
    pub device_id: u16,
    pub subsys_vendor_id: u16,
    pub subsys_id: u16,
    pub revision_id: u8,
    pub prog_if: u8,
    pub subclass: u8,
    pub class_code: u8,
    pub managed_slot: u8,
    pub dev_flags: u8,
    pub bar_flags: [u8; 6],
    pub irq_pin: u8,
    pub _pad: u8,
    pub bar0_size_bytes: u64,
}

const _: () = assert!(size_of::<BootPciFuncRecord>() == 32);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BootModManifestEntry {
    pub name: [u8; 40],
    pub size_bytes: u64,
}

pub const ROOT_INIT_BOOT_MAGIC: u64 = 0x4452_595f_494e_4954; // "DRY_INIT"

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

/// Bind `notif_cap` (`CapRights::WAIT`) to the calling thread's `TCB` so synchronous `Endpoint`
/// waits (`sys_ipc_recv*` / `sys_ipc_reply_recv*`) also deliver `IPC_OP_BOUND_NOTIF` (`0 Hz`).
#[inline]
pub fn sys_notify_bind_tcb(notif_cap: u64) -> Result<(), isize> {
    let (r0, _) = raw_syscall6(SYS_NOTIFY_WAIT, notif_cap, NOTIFY_FLAG_BIND_TCB, 0, 0, 0, 0);
    if r0 < 0 { Err(r0) } else { Ok(()) }
}

/// Unbind any `Notification` (or specifically `notif_cap` when non-zero) from the calling `TCB`.
#[inline]
pub fn sys_notify_unbind_tcb(notif_cap: u64) -> Result<(), isize> {
    let (r0, _) = raw_syscall6(SYS_NOTIFY_WAIT, notif_cap, NOTIFY_FLAG_UNBIND_TCB, 0, 0, 0, 0);
    if r0 < 0 { Err(r0) } else { Ok(()) }
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
pub fn sys_ipc_recv_ext(
    ep_cap: u64,
    flags: u64,
    timeout: u64,
    ext: &mut UserIpcRecvExt,
) -> Result<(u64, u64, [u64; 4]), isize> {
    let (r0, aux, mrs) = raw_ipc_syscall(
        SYS_IPC_RECV,
        ep_cap,
        flags,
        timeout,
        (ext as *mut UserIpcRecvExt) as u64,
        0,
        0,
        [0; 4],
    );
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
pub fn sys_ipc_call_with_grant(
    ep_cap: u64,
    label: u32,
    mrs: [u64; 4],
    grant_cap: u64,
    grant_rights: u16,
    grant_badge: u64,
    timeout: u64,
) -> Result<(u64, u64, [u64; 4]), isize> {
    let flags = if grant_cap != 0 { MSG_FLAG_GRANT_CAP } else { 0 };
    let tag = pack_msg_tag(label, flags, 4, 0);
    let send_ext = UserIpcSendExt {
        extra_buf_ptr: 0,
        grant_cap,
        grant_rights,
        _pad: 0,
        grant_lease: 0,
        grant_badge,
        shared_region_cap: 0,
        shared_offset_and_len: 0,
    };
    let mut recv_ext = UserIpcRecvExt::default();
    let (r0, aux, out_mrs) = raw_ipc_syscall(
        SYS_IPC_CALL,
        ep_cap,
        tag,
        timeout,
        if grant_cap != 0 {
            (&raw const send_ext) as u64
        } else {
            0
        },
        (&raw mut recv_ext) as u64,
        0,
        mrs,
    );
    if r0 < 0 {
        Err(r0)
    } else {
        let cap_or_badge = if recv_ext.transferred_cap != 0 {
            recv_ext.transferred_cap
        } else {
            aux
        };
        Ok((r0 as u64, cap_or_badge, out_mrs))
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
pub fn sys_ipc_reply_with_grant(
    ep_cap: u64,
    reply_token: u64,
    label: u32,
    mrs: [u64; 4],
    grant_cap: u64,
    grant_rights: u16,
    grant_badge: u64,
) -> Result<(), isize> {
    let flags = if grant_cap != 0 { MSG_FLAG_GRANT_CAP } else { 0 };
    let tag = pack_msg_tag(label, flags, 4, 0);
    let send_ext = UserIpcSendExt {
        extra_buf_ptr: 0,
        grant_cap,
        grant_rights,
        _pad: 0,
        grant_lease: 0,
        grant_badge,
        shared_region_cap: 0,
        shared_offset_and_len: 0,
    };
    let (r0, _, _) = raw_ipc_syscall(
        SYS_IPC_REPLY,
        ep_cap,
        reply_token,
        tag,
        if grant_cap != 0 {
            (&raw const send_ext) as u64
        } else {
            0
        },
        0,
        0,
        mrs,
    );
    if r0 < 0 { Err(r0) } else { Ok(()) }
}

#[inline]
pub const fn msg_tag_extra_len(tag: u64) -> usize {
    ((tag >> 16) & 0xffff) as usize
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

#[inline]
pub fn sys_irq_bind(irq_cap: u64, notif_cap: u64, flags: u64) -> Result<u8, isize> {
    let (r0, _) = raw_syscall6(SYS_IRQ_BIND, irq_cap, notif_cap, flags, 0, 0, 0);
    if r0 < 0 { Err(r0) } else { Ok(r0 as u8) }
}

#[inline]
pub fn sys_irq_ack(irq_cap: u64, action: u64) -> Result<u64, isize> {
    let (r0, count) = raw_syscall6(SYS_IRQ_ACK, irq_cap, action, 0, 0, 0, 0);
    if r0 < 0 { Err(r0) } else { Ok(count) }
}

#[inline]
pub fn sys_ioport_inb(ioport_cap: u64, port: u16) -> Result<u8, isize> {
    let (r0, _) = raw_syscall6(
        SYS_IOPORT_RW,
        ioport_cap,
        IOPORT_OP_INB,
        port as u64,
        0,
        0,
        0,
    );
    if r0 < 0 { Err(r0) } else { Ok(r0 as u8) }
}

#[inline]
pub fn sys_ioport_outb(ioport_cap: u64, port: u16, val: u8) -> Result<(), isize> {
    let (r0, _) = raw_syscall6(
        SYS_IOPORT_RW,
        ioport_cap,
        IOPORT_OP_OUTB,
        port as u64,
        val as u64,
        0,
        0,
    );
    if r0 < 0 { Err(r0) } else { Ok(()) }
}

#[inline]
pub fn sys_sysinfo(out: &mut UserSysInfo) -> Result<(), isize> {
    let (r0, _) = raw_syscall6(SYS_SYSINFO, (out as *mut UserSysInfo) as u64, 0, 0, 0, 0, 0);
    if r0 < 0 { Err(r0) } else { Ok(()) }
}

#[inline]
pub fn sys_frame_alloc(order: u64) -> Result<u64, isize> {
    let (r0, cap) = raw_syscall6(SYS_FRAME_ALLOC, order, 0, 0, 0, 0, 0);
    if r0 < 0 { Err(r0) } else { Ok(cap) }
}

#[inline]
pub fn sys_vspace_map(frame_cap: u64, vaddr: u64, op: u64) -> Result<(), isize> {
    let (r0, _) = raw_syscall6(SYS_VSPACE_MAP, frame_cap, vaddr, op, 0, 0, 0);
    if r0 < 0 { Err(r0) } else { Ok(()) }
}

#[inline]
pub fn sys_proc_list(out: &mut [UserProcInfo]) -> Result<(usize, usize), isize> {
    let ptr = if out.is_empty() {
        0
    } else {
        out.as_mut_ptr() as u64
    };
    let (r0, total) = raw_syscall6(SYS_PROC_LIST, ptr, out.len() as u64, 0, 0, 0, 0);
    if r0 < 0 {
        Err(r0)
    } else {
        Ok((r0 as usize, total as usize))
    }
}

#[inline]
pub fn sys_proc_kill(child_handle: u64) -> Result<(), isize> {
    let (r0, _) = raw_syscall6(SYS_PROC_KILL, child_handle, 0, 0, 0, 0, 0);
    if r0 < 0 { Err(r0) } else { Ok(()) }
}

#[inline]
pub fn sys_power_query(out: &mut UserPowerInfo) -> Result<(), isize> {
    let (r0, _) = raw_syscall6(
        SYS_POWER_CTL,
        POWER_OP_QUERY,
        (out as *mut UserPowerInfo) as u64,
        0,
        0,
        0,
        0,
    );
    if r0 < 0 { Err(r0) } else { Ok(()) }
}

#[inline]
pub fn sys_power_ctl(op: u64, power_cap: u64) -> Result<isize, isize> {
    let (r0, _) = raw_syscall6(SYS_POWER_CTL, op, power_cap, 0, 0, 0, 0);
    if r0 < 0 { Err(r0) } else { Ok(r0) }
}

/// Read unprivileged architectural `TSC` counter at `CPL=3` (zero SSE/SIMD instructions).
#[inline]
pub fn rdtsc_user() -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: `rdtsc` is unprivileged when `CR4.TSD == 0`.
    unsafe {
        asm!(
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
    ((high as u64) << 32) | (low as u64)
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
