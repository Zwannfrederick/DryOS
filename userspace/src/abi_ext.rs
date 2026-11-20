// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Milestone 2B (`DRY Native Syscall ABI v0.2`) Extensions: Capability-Gated PCI/DMA Mapping,
//! `DRY Vault` Lease Epoch & `READ_ONLY_EXPORT` Sealing, and Storage/Vault IPC Opcodes.

use core::mem::size_of;

use crate::{
    pack_msg_tag, raw_ipc_syscall, raw_syscall6, MSG_FLAG_GRANT_CAP, SYS_IPC_REPLY, SYS_PROC_SPAWN,
    SYS_SYSINFO,
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

pub const DEV_MAP_OP_QUERY_BARS: u64 = 1;
pub const DEV_MAP_OP_MAP_WINDOW: u64 = 2;
pub const DEV_MAP_OP_CFG_RW: u64 = 3;
pub const DEV_MAP_OP_MAP_WINDOW_2M: u64 = 4;
pub const DEV_MAP_OP_ENUM_MINT: u64 = 5;

pub const PCI_BAR_FLAG_MMIO: u32 = 1 << 0;
pub const PCI_BAR_FLAG_64BIT: u32 = 1 << 1;
pub const PCI_BAR_FLAG_PREFETCHABLE: u32 = 1 << 2;
pub const PCI_BAR_FLAG_ABOVE_4G: u32 = 1 << 3;
pub const PCI_BAR_FLAG_IO: u32 = 1 << 4;

pub const PCI_DEV_FLAG_IOMMU_ACTIVE: u16 = 1 << 0;
pub const PCI_DEV_FLAG_MSI: u16 = 1 << 1;
pub const PCI_DEV_FLAG_MSIX: u16 = 1 << 2;
pub const PCI_DEV_FLAG_VIRTIO: u16 = 1 << 3;
pub const PCI_DEV_FLAG_PCIE: u16 = 1 << 4;
pub const PCI_DEV_FLAG_ECAM: u16 = 1 << 5;
pub const PCI_DEV_FLAG_FLR: u16 = 1 << 6;
pub const PCI_DEV_FLAG_REBAR: u16 = 1 << 7;
pub const PCI_DEV_FLAG_ACS: u16 = 1 << 8;
pub const PCI_DEV_FLAG_AER: u16 = 1 << 9;
pub const PCI_DEV_FLAG_SRIOV: u16 = 1 << 10;

#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub struct UserPciBarInfo {
    pub size_bytes: u64,
    pub flags: u32,
    pub page_suboff: u16,
    pub _pad: u16,
}

#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub struct UserPciDeviceInfo {
    pub vendor_id: u16,
    pub device_id: u16,
    pub bus_dev_fn: u16,
    pub dev_flags: u16,
    pub msix_table_size: u16,
    pub msi_cap_present: u8,
    pub iommu_protected: u8,
    pub iommu_faults: u32,
    pub bars: [UserPciBarInfo; 6],
}

const _: () = assert!(size_of::<UserPciBarInfo>() == 16);
const _: () = assert!(size_of::<UserPciDeviceInfo>() == 112);

#[inline]
pub fn sys_dev_query_bars(pci_cap: u64, out: &mut UserPciDeviceInfo) -> Result<(), isize> {
    let (r0, _) = raw_syscall6(
        SYS_DEV_MAP,
        pci_cap,
        DEV_MAP_OP_QUERY_BARS,
        (out as *mut UserPciDeviceInfo) as u64,
        0,
        0,
        0,
    );
    if r0 < 0 {
        Err(r0)
    } else {
        Ok(())
    }
}

#[inline]
pub fn sys_dev_map_window(
    pci_cap: u64,
    user_vaddr: u64,
    bar_idx: u8,
    write_combining: bool,
    page_count: u16,
    page_offset: u32,
) -> Result<usize, isize> {
    if (user_vaddr & 0xfff) != 0 {
        return Err(crate::EINVAL);
    }
    let packed = (bar_idx as u64)
        | ((write_combining as u64) << 8)
        | ((page_count as u64) << 16)
        | ((page_offset as u64) << 32);
    let (r0, _) = raw_syscall6(
        SYS_DEV_MAP,
        pci_cap,
        user_vaddr | DEV_MAP_OP_MAP_WINDOW,
        packed,
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
pub fn sys_dev_cfg_read(pci_cap: u64, offset: u16, width: u8) -> Result<u32, isize> {
    if !matches!(width, 1 | 2 | 4) || (offset as usize) + (width as usize) > 4096 {
        return Err(crate::EINVAL);
    }
    let op = DEV_MAP_OP_CFG_RW
        | (((offset as u64) & 0xfff) << 12)
        | (((width as u64) & 0xf) << 24);
    let (r0, r1) = raw_syscall6(SYS_DEV_MAP, pci_cap, op, 0, 0, 0, 0);
    if r0 < 0 {
        Err(r0)
    } else {
        Ok(r1 as u32)
    }
}

#[inline]
pub fn sys_dev_cfg_write(pci_cap: u64, offset: u16, width: u8, val: u32) -> Result<(), isize> {
    if !matches!(width, 1 | 2 | 4) || (offset as usize) + (width as usize) > 4096 {
        return Err(crate::EINVAL);
    }
    let op = DEV_MAP_OP_CFG_RW
        | (((offset as u64) & 0xfff) << 12)
        | (((width as u64) & 0xf) << 24)
        | (1u64 << 28);
    let (r0, _) = raw_syscall6(SYS_DEV_MAP, pci_cap, op, val as u64, 0, 0, 0);
    if r0 < 0 {
        Err(r0)
    } else {
        Ok(())
    }
}

#[inline]
pub fn sys_dev_mint_slot(pci_root_cap: u64, slot_idx: u16) -> Result<u64, isize> {
    let op = DEV_MAP_OP_ENUM_MINT | (((slot_idx as u64) & 0x3ff) << 12);
    let (r0, cap) = raw_syscall6(SYS_DEV_MAP, pci_root_cap, op, 0, 0, 0, 0);
    if r0 < 0 {
        Err(r0)
    } else {
        Ok(cap)
    }
}

pub fn sys_dev_map_window_2m(
    pci_cap: u64,
    user_vaddr: u64,
    bar_idx: u8,
    write_combining: bool,
    huge_count: u16,
    huge_offset: u32,
) -> Result<usize, isize> {
    if (user_vaddr & 0x1f_ffff) != 0 {
        return Err(crate::EINVAL);
    }
    let packed = (bar_idx as u64)
        | ((write_combining as u64) << 8)
        | ((huge_count as u64) << 16)
        | ((huge_offset as u64) << 32);
    let (r0, _) = raw_syscall6(
        SYS_DEV_MAP,
        pci_cap,
        user_vaddr | DEV_MAP_OP_MAP_WINDOW_2M,
        packed,
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

pub const DMA_OP_ALLOC_SG: u64 = 1;
pub const DMA_OP_FREE: u64 = 2;
pub const DMA_OP_SYNC: u64 = 3;
pub const DMA_OP_QUERY_STATS: u64 = 4;

pub const DMA_DIR_BIDIR: u8 = 0;
pub const DMA_DIR_TO_DEV: u8 = 1;
pub const DMA_DIR_FROM_DEV: u8 = 2;

pub const DMA_MAX_SG_SEGS: usize = 64;
pub const DMA_SG_FLAG_IOVA_DISTINCT: u8 = 1 << 0;
pub const DMA_SG_FLAG_HW_IOMMU: u8 = 1 << 1;

#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub struct UserDmaSgSeg {
    pub iova: u64,
    pub len_bytes: u32,
    pub flags: u32,
}

#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub struct UserDmaSgDesc {
    pub user_vaddr: u64,
    pub buf_id: u64,
    pub iova_base: u64,
    pub total_bytes: u64,
    pub page_count: u16,
    pub direction: u8,
    pub out_flags: u8,
    pub _pad: u32,
}

#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub struct UserDmaStats {
    pub active_sg_bufs: u32,
    pub active_single_pins: u32,
    pub pinned_pages: u32,
    pub hw_enforcing: u8,
    pub _pad: [u8; 3],
    pub mapped_iova_bytes: u64,
    pub iova_cursor: u64,
    pub iommu_faults: u64,
}

const _: () = assert!(size_of::<UserDmaSgSeg>() == 16);
const _: () = assert!(size_of::<UserDmaSgDesc>() == 40);
const _: () = assert!(size_of::<UserDmaStats>() == 40);

#[inline]
pub fn sys_dma_alloc_sg(
    pci_cap: u64,
    user_vaddr: u64,
    page_count: u16,
    direction: u8,
    segs_out: Option<&mut [UserDmaSgSeg]>,
) -> Result<UserDmaSgDesc, isize> {
    let mut desc = UserDmaSgDesc {
        user_vaddr,
        page_count,
        direction,
        ..UserDmaSgDesc::default()
    };
    let segs_ptr = match segs_out {
        Some(s) => {
            if s.len() < page_count as usize {
                return Err(crate::EINVAL);
            }
            s.as_mut_ptr() as u64
        }
        None => 0,
    };
    let (r0, _) = raw_syscall6(
        crate::SYS_DMA_CTL,
        pci_cap,
        DMA_OP_ALLOC_SG,
        (&raw mut desc) as u64,
        segs_ptr,
        0,
        0,
    );
    if r0 < 0 {
        Err(r0)
    } else {
        Ok(desc)
    }
}

#[inline]
pub fn sys_dma_free(pci_cap: u64, buf_id: u64) -> Result<usize, isize> {
    let (r0, _) = raw_syscall6(crate::SYS_DMA_CTL, pci_cap, DMA_OP_FREE, buf_id, 0, 0, 0);
    if r0 < 0 {
        Err(r0)
    } else {
        Ok(r0 as usize)
    }
}

#[inline]
pub fn sys_dma_sync(pci_cap: u64, buf_id: u64, direction: u8) -> Result<(), isize> {
    let (r0, _) = raw_syscall6(
        crate::SYS_DMA_CTL,
        pci_cap,
        DMA_OP_SYNC,
        buf_id,
        direction as u64,
        0,
        0,
    );
    if r0 < 0 {
        Err(r0)
    } else {
        Ok(())
    }
}

#[inline]
pub fn sys_dma_query_stats(pci_cap: u64, out: &mut UserDmaStats) -> Result<(), isize> {
    let (r0, _) = raw_syscall6(
        crate::SYS_DMA_CTL,
        pci_cap,
        DMA_OP_QUERY_STATS,
        (out as *mut UserDmaStats) as u64,
        0,
        0,
        0,
    );
    if r0 < 0 {
        Err(r0)
    } else {
        Ok(())
    }
}

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

pub const BOOT_EXT_INFO_OFFSET: u64 = 0x0700;
pub const ROOT_BOOT_EXT_MAGIC: u64 = 0x4452_595f_4245_5854; // "DRY_BEXT"

pub const BOOT_FLAG_LIVE_USB: u64 = 1 << 0;
pub const BOOT_FLAG_STORAGE_RO: u64 = 1 << 1;
pub const BOOT_FLAG_RECOVERY: u64 = 1 << 2;
pub const BOOT_FLAG_UART_PRESENT: u64 = 1 << 3;
pub const BOOT_FLAG_VTD_ENFORCING: u64 = 1 << 4;
pub const BOOT_FLAG_VTD_GFX_DEFERRED: u64 = 1 << 5;
pub const BOOT_FLAG_SAFE_GOP: u64 = 1 << 6;
pub const BOOT_FLAG_USB_PERSIST_PREFER: u64 = 1 << 7;
pub const BOOT_FLAG_DIAG_SELFTEST: u64 = 1 << 8;

pub const HWCHECK_MODE_OFF: u32 = 0;
pub const HWCHECK_MODE_SAFE: u32 = 1;
pub const HWCHECK_MODE_DESKTOP_SAFE: u32 = 2;
pub const HWCHECK_MODE_EXTENDED: u32 = 3;
pub const HWCHECK_MODE_RECOVERY: u32 = 4;
pub const HWCHECK_MODE_NVIDIA_EXP: u32 = 5;

pub const HWVAL_FLAG_AUTO: u32 = 1 << 0;
pub const HWVAL_FLAG_STORAGE_RO: u32 = 1 << 1;
pub const HWVAL_FLAG_PCI_PASSIVE: u32 = 1 << 2;
pub const HWVAL_FLAG_NVIDIA_EXP: u32 = 1 << 3;
pub const HWVAL_FLAG_TEST_USB_WRITE_FAIL: u32 = 1 << 4;
pub const HWVAL_FLAG_TEST_USB_DISCONNECT: u32 = 1 << 5;
pub const HWVAL_FLAG_TEST_MALFORMED_ACPI: u32 = 1 << 6;
pub const HWVAL_FLAG_TEST_BAD_PCI_BAR: u32 = 1 << 7;
pub const HWVAL_FLAG_TEST_NO_IOMMU_MODE: u32 = 1 << 8;
pub const HWVAL_FLAG_TEST_NV_NO_GPU: u32 = 1 << 9;
pub const HWVAL_FLAG_TEST_NV_BAD_BAR: u32 = 1 << 10;
pub const HWVAL_FLAG_TEST_NV_VM_CRASH: u32 = 1 << 11;
pub const HWVAL_FLAG_TEST_NV_TIMEOUT: u32 = 1 << 12;
pub const HWVAL_FLAG_TEST_NV_CORRUPT_IPC: u32 = 1 << 13;
pub const HWVAL_FLAG_TEST_NV_UNAUTH_CAP: u32 = 1 << 14;
pub const HWVAL_FLAG_TEST_SEC_PROBE: u32 = 1 << 15;

pub const ACPI_AUDIT_RSDP_OK: u32 = 1 << 0;
pub const ACPI_AUDIT_XSDT_OK: u32 = 1 << 1;
pub const ACPI_AUDIT_RSDT_FALLBACK: u32 = 1 << 2;
pub const ACPI_AUDIT_MADT_OK: u32 = 1 << 3;
pub const ACPI_AUDIT_MCFG_OK: u32 = 1 << 4;
pub const ACPI_AUDIT_DMAR_OK: u32 = 1 << 5;
pub const ACPI_AUDIT_IVRS_FOUND: u32 = 1 << 6;
pub const ACPI_AUDIT_FACP_OK: u32 = 1 << 7;
pub const ACPI_AUDIT_CHECKSUM_FAIL: u32 = 1 << 8;
pub const ACPI_AUDIT_MALFORMED_TABLE: u32 = 1 << 9;

pub const IOMMU_AUDIT_DMAR_PRESENT: u32 = 1 << 0;
pub const IOMMU_AUDIT_HW_ENFORCING: u32 = 1 << 1;
pub const IOMMU_AUDIT_INT_REMAP: u32 = 1 << 2;
pub const IOMMU_AUDIT_IVRS_PRESENT: u32 = 1 << 3;
pub const IOMMU_AUDIT_FAIL_CLOSED: u32 = 1 << 4;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RootInitBootExtInfo {
    pub magic: u64,
    pub usb_mod_cap: u64,
    pub pci_xhci_cap: u64,
    pub nvme_mod_cap: u64,
    pub pci_nvme_cap: u64,
    pub lx_net_mod_cap: u64,
    pub pci_net_cap: u64,
    pub pci_igpu_cap: u64,
    pub boot_flags: u64,
    pub dgpu_packed: u64,
    pub vmd_packed: u64,
    pub lpss_i2c_count: u64,
    pub hwcheck_mode: u32,
    pub hwval_flags: u32,
    pub acpi_audit_flags: u32,
    pub iommu_audit_flags: u32,
    pub mem_usable_mib: u32,
    pub mem_regions: u32,
    pub fb_pitch: u32,
    pub fb_width: u16,
    pub fb_height: u16,
    pub fb_phys_base: u64,
    pub cpuid_features: u64,
    pub boot_disk_guid: [u8; 16],
    pub tsc_khz: u32,
    pub secure_boot_state: u32,
    pub acpi_oem_id: [u8; 6],
    pub pci_passive_mode: u8,
    pub mem_total_entries: u8,
    pub sdk_demo_mod_cap: u64,
    pub nv_island_mod_cap: u64,
}

const _: () = assert!(size_of::<RootInitBootExtInfo>() == 192);
const _: () = assert!(size_of::<RootInitBootExtInfo>() <= 256);

impl RootInitBootExtInfo {
    pub const fn empty() -> Self {
        Self {
            magic: 0, usb_mod_cap: 0, pci_xhci_cap: 0, nvme_mod_cap: 0,
            pci_nvme_cap: 0, lx_net_mod_cap: 0, pci_net_cap: 0, pci_igpu_cap: 0,
            boot_flags: 0, dgpu_packed: 0, vmd_packed: 0, lpss_i2c_count: 0,
            hwcheck_mode: 0, hwval_flags: 0, acpi_audit_flags: 0, iommu_audit_flags: 0,
            mem_usable_mib: 0, mem_regions: 0, fb_pitch: 0, fb_width: 0, fb_height: 0,
            fb_phys_base: 0, cpuid_features: 0, boot_disk_guid: [0; 16],
            tsc_khz: 0, secure_boot_state: 0, acpi_oem_id: [0; 6],
            pci_passive_mode: 0, mem_total_entries: 0, sdk_demo_mod_cap: 0,
            nv_island_mod_cap: 0,
        }
    }
}

/// Read kernel `UserSysInfo` and recent kernel `dmesg` ring bytes (`DIAG_RING`, up to 16 KiB).
#[inline]
pub fn sys_dmesg_read(info: &mut UserSysInfo, out: &mut [u8]) -> Result<usize, isize> {
    let ptr = if out.is_empty() { 0 } else { out.as_mut_ptr() as u64 };
    let (r0, _) = raw_syscall6(
        SYS_SYSINFO,
        (info as *mut UserSysInfo) as u64,
        ptr,
        out.len() as u64,
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

/// Revoke a transferred capability slot received over IPC when the opcode does not consume it.
#[inline]
pub fn revoke_unused_cap(cap: u64) {
    if cap != 0 {
        let _ = crate::sys_cap_revoke(cap, 1);
    }
}

