// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Milestone 2B (`DRY Native Syscall ABI v0.2`) Extensions: Capability-Gated PCI/DMA Mapping,
//! `DRY Vault` Lease Epoch & `READ_ONLY_EXPORT` Sealing, and Storage/Vault IPC Opcodes.

use core::mem::size_of;

use crate::{
    pack_msg_tag, raw_ipc_syscall, raw_syscall6, MSG_FLAG_GRANT_CAP, SYS_IPC_REPLY, SYS_PROC_SPAWN,
};

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UserPowerInfo {
    pub idle_hlt_calls: u64,
    pub idle_hlt_tsc: u64,
    pub elapsed_tsc: u64,
    pub fadt_present: u8,
    pub s5_supported: u8,
    pub reset_supported: u8,
    pub s3_aml_present: u8,
    pub s3_supported: u8,
    pub _pad: [u8; 3],
    pub denied_ops: u64,
}

const _: () = assert!(size_of::<UserPowerInfo>() == 40);

/// Sanitized 24-byte per-process snapshot POD returned by `SYS_PROC_LIST` (`0` kernel pointers/PFNs).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UserProcInfo {
    pub pid: u32,
    pub state: u8,
    pub domain: u8,
    pub compartment_id: u16,
    pub mapped_pages_4k: u32,
    pub pt_frames_4k: u32,
    pub syscall_entries: u64,
}

const _: () = assert!(size_of::<UserProcInfo>() == 24);

/// Sanitized capability metadata returned by `SYS_CAP_QUERY` (`32` bytes).
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

const _: () = assert!(size_of::<UserCapInfo>() == 32);

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

/// Sanitized 104-byte system telemetry POD returned by `SYS_SYSINFO` (zero kernel pointers/PFNs).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UserSysInfo {
    pub abi_version: u64,
    pub online_cpus: u32,
    pub apic_mode: u8,
    pub ioapic_present: u8,
    pub fb_present: u8,
    pub _pad0: u8,
    pub tsc_mhz: u64,
    pub uptime_ms: u64,
    pub elapsed_tsc: u64,
    pub timer_irqs: u64,
    pub total_ext_irqs: u64,
    pub total_syscalls: u64,
    pub total_ctx_switches: u64,
    pub usable_frames_4k: u32,
    pub free_frames_4k: u32,
    pub irq1_delivered: u64,
    pub irq12_delivered: u64,
    pub total_ipc_msgs: u64,
}

const _: () = assert!(size_of::<UserSysInfo>() == 104);

/// Extended IPC send/grant descriptor (`48` bytes POD, matches `kernel::ipc::UserIpcSendExt`).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UserIpcSendExt {
    pub extra_buf_ptr: u64,
    pub grant_cap: u64,
    pub grant_rights: u16,
    pub _pad: u16,
    pub grant_lease: u32,
    pub grant_badge: u64,
    pub shared_region_cap: u64,
    pub shared_offset_and_len: u64,
}

const _: () = assert!(size_of::<UserIpcSendExt>() == 48);

/// Extended IPC receive descriptor (`40` bytes POD, matches `kernel::ipc::UserIpcRecvExt`).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UserIpcRecvExt {
    pub extra_buf_ptr: u64,
    pub extra_buf_cap: u64,
    pub sender_badge: u64,
    pub reply_token: u64,
    pub transferred_cap: u64,
}

const _: () = assert!(size_of::<UserIpcRecvExt>() == 40);

pub const SYS_DEV_MAP: u64 = 29;
pub const SYS_DMA_ALLOC: u64 = 30;
pub const SYS_VAULT_CTL: u64 = 31;

pub const EXDEV: isize = -18;

// `SecFlags` bitmask constants (`kernel/src/security.rs`)
pub const SEC_FLAG_READ_ONLY_EXPORT: u8 = 1 << 0;
pub const SEC_FLAG_DEV_WORKSPACE: u8 = 1 << 1;
pub const SEC_FLAG_DEVICE_DMA: u8 = 1 << 2;
pub const SEC_FLAG_NO_DELEGATE: u8 = 1 << 3;

// `SYS_VAULT_CTL` operations (`kernel/src/vault_ctrl.rs`)
pub const VAULT_CTL_QUERY_EPOCH: u64 = 0;
pub const VAULT_CTL_ADVANCE_EPOCH: u64 = 1;
pub const VAULT_CTL_SEAL_RO_EXPORT: u64 = 2;

// Unforgeable badges for `dry-vfs`, `dry-blk`, and `dry-vault` (`Milestone 2B`)
pub const BADGE_VFS_VAULT_ROOT: u64 = 0x0007_ffff;
pub const BADGE_VFS_DEV_WS: u64 = 0x002a_ffff;
pub const BADGE_BLK_VFS: u64 = 0x0004_6001;
pub const BADGE_VAULT_SUP: u64 = 0x0001_7001;
pub const BADGE_VAULT_SHELL: u64 = 0x0102_7002;
pub const BADGE_VAULT_SYSMON: u64 = 0x0103_7003;
pub const BADGE_VAULT_DEV_WS: u64 = 0x002a_7004;

// Milestone 2B WM / Supervisor / VFS / Block / Vault IPC opcodes
pub const IPC_OP_WM_SET_AUTH_LOCK: u32 = 57;
pub const IPC_OP_WM_TAKE_LOCK_PIN: u32 = 58;
pub const IPC_OP_SUP_GET_VAULT_CAP: u32 = 59;
pub const IPC_OP_SUP_CORRUPT_SB0_TEST: u32 = 139;

pub const VFS_OP_MOUNT_BLK: u32 = 109;
pub const VFS_OP_SYNC: u32 = 110;
pub const VFS_OP_LINK: u32 = 111;
pub const VFS_OP_CORRUPT_SB0_TEST: u32 = 112;

pub const BLK_OP_SETUP_RING: u32 = 120;
pub const BLK_OP_ATTACH_DATA: u32 = 121;
pub const BLK_OP_QUERY_INFO: u32 = 122;
pub const BLK_OP_DETACH_RING: u32 = 123;

pub const VAULT_OP_STATUS: u32 = 130;
pub const VAULT_OP_UNLOCK: u32 = 131;
pub const VAULT_OP_LOCK_REVOKE: u32 = 132;
pub const VAULT_OP_PASSWD: u32 = 133;
pub const VAULT_OP_MINT_LEASE: u32 = 134;
pub const VAULT_OP_SEAL_RO_STATUS: u32 = 135;

/// Sanitized 32-byte VirtIO 1.0 Modern PCI structure offsets copied to CPL=3 by `SYS_DEV_MAP`.
#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub struct UserVirtioPciInfo {
    pub common_off: u32,
    pub notify_off: u32,
    pub notify_mult: u32,
    pub isr_off: u32,
    pub device_off: u32,
    pub mapped_pages: u16,
    pub bus_dev_fn: u16,
    pub vendor_id: u16,
    pub device_id: u16,
    pub _pad: u32,
}

const _: () = assert!(size_of::<UserVirtioPciInfo>() == 32);

pub const DMA_ALLOC_FLAG_2M: u64 = 1;

#[inline]
pub fn sys_dev_map(
    pci_cap: u64,
    user_vaddr: u64,
    out: &mut UserVirtioPciInfo,
) -> Result<usize, isize> {
    let (r0, _) = raw_syscall6(
        SYS_DEV_MAP,
        pci_cap,
        user_vaddr,
        (out as *mut UserVirtioPciInfo) as u64,
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

/// Allocate a pinned 4 KiB `DEVICE_DMA | NO_DELEGATE` frame mapped `USER_RW` at `user_vaddr`.
/// Returns `(frame_cap, dma_phys_addr)`.
#[inline]
pub fn sys_dma_alloc(pci_cap: u64, user_vaddr: u64) -> Result<(u64, u64), isize> {
    let mut dma_paddr: u64 = 0;
    let (r0, frame_cap) = raw_syscall6(
        SYS_DMA_ALLOC,
        pci_cap,
        user_vaddr,
        (&raw mut dma_paddr) as u64,
        0,
        0,
        0,
    );
    if r0 < 0 {
        Err(r0)
    } else {
        Ok((frame_cap, dma_paddr))
    }
}

/// Allocate a pinned 2 MiB `DEVICE_DMA | NO_DELEGATE` huge frame mapped `USER_RW` at `user_vaddr`.
/// Returns `(frame_cap, dma_phys_addr)`.
#[inline]
pub fn sys_dma_alloc_2m(pci_cap: u64, user_vaddr: u64) -> Result<(u64, u64), isize> {
    if (user_vaddr & 0x1f_ffff) != 0 {
        return Err(crate::EINVAL);
    }
    sys_dma_alloc(pci_cap, user_vaddr | DMA_ALLOC_FLAG_2M)
}

#[inline]
pub fn sys_vault_ctl(
    vault_cap: u64,
    op: u64,
    arg0: u64,
    arg1: u64,
) -> Result<(isize, u64), isize> {
    let (r0, r1) = raw_syscall6(SYS_VAULT_CTL, vault_cap, op, arg0, arg1, 0, 0);
    if r0 < 0 {
        Err(r0)
    } else {
        Ok((r0, r1))
    }
}

#[inline]
pub fn sys_vault_query_epoch() -> Result<u32, isize> {
    let (_, ep) = sys_vault_ctl(0, VAULT_CTL_QUERY_EPOCH, 0, 0)?;
    Ok(ep as u32)
}

#[inline]
pub fn sys_proc_spawn_ex(
    boot_mod_cap: u64,
    domain_code: u8,
    sec_flags: u8,
    compartment_id: u16,
    grant0: (u64, u16, u64),
    grant1: (u64, u16),
) -> Result<(u64, u64), isize> {
    let dom_comp = (domain_code as u64)
        | ((sec_flags as u64) << 8)
        | ((compartment_id as u64) << 16)
        | (0xffffu64 << 32);
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
#[allow(clippy::too_many_arguments)]
pub fn sys_ipc_reply_with_grant_lease(
    ep_cap: u64,
    reply_token: u64,
    label: u32,
    mrs: [u64; 4],
    grant_cap: u64,
    grant_rights: u16,
    grant_badge: u64,
    grant_lease: u32,
) -> Result<(), isize> {
    let flags = if grant_cap != 0 { MSG_FLAG_GRANT_CAP } else { 0 };
    let tag = pack_msg_tag(label, flags, 4, 0);
    let send_ext = UserIpcSendExt {
        extra_buf_ptr: 0,
        grant_cap,
        grant_rights,
        _pad: 0,
        grant_lease,
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
    if r0 < 0 {
        Err(r0)
    } else {
        Ok(())
    }
}

/// `SYS_PROC_SPAWN` flag (`domain_and_comp` bit 48): `grant1` is bound as the child's
/// `FaultEndpoint` (kept in the parent's `CSpace`, not transferred).
pub const SPAWN_FLAG_FAULT_EP: u64 = 1 << 48;
/// Label of a kernel-originated fault message: `mr0 = vector | error_code << 16`, `mr1 = rip`,
/// `mr2 = cr2`, `mr3 = rsp`. Authenticate it by the badge of the bound capability, not the label.
pub const FAULT_MSG_LABEL: u32 = 0x7fa1;
/// Reply label resuming the faulting thread (`mr0 != 0` = new `RIP`); any other label terminates.
pub const FAULT_REPLY_RESUME: u32 = 0x7fa2;
pub const FAULT_REPLY_KILL: u32 = 0x7fa3;

/// Spawn a child whose Ring 3 CPU exceptions are delivered to `fault_ep_cap` (`SEND | CALL`).
#[inline]
pub fn sys_proc_spawn_with_fault_ep(
    boot_mod_cap: u64,
    domain_code: u8,
    compartment_id: u16,
    grant0: (u64, u16, u64),
    fault_ep_cap: u64,
) -> Result<(u64, u64), isize> {
    let dom_comp = (domain_code as u64)
        | ((compartment_id as u64) << 16)
        | (0xffffu64 << 32)
        | SPAWN_FLAG_FAULT_EP;
    let g0_rb = (grant0.1 as u64) | (grant0.2 << 16);
    let (r0, pid) =
        raw_syscall6(SYS_PROC_SPAWN, boot_mod_cap, dom_comp, grant0.0, g0_rb, fault_ep_cap, 0);
    if r0 < 0 {
        Err(r0)
    } else {
        Ok((r0 as u64, pid))
    }
}
