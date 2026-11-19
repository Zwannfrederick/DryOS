// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-gated 64-bit PCI BAR discovery (`QUERY_BARS`) and bounded MMIO window mapping
//! (`MAP_WINDOW`, `UC` or prefetchable `WC`) split from `pci.rs` for clean size/cohesion bounds.

use core::mem::size_of;

use crate::cspace::cap::{CSpaceId, CapError, CapObjectKind, CapPtr};
use crate::cspace::revoke::attach_cap_mapping_range_record;
use crate::cspace::with_pinned_cap;
use crate::ipc::map_cap_err_to_sys;
use crate::pci_bar::{
    build_user_pci_info, UserPciDeviceInfo, MAX_WINDOW_PAGES, PCI_DEV_FLAG_ACS,
    PCI_DEV_FLAG_AER, PCI_DEV_FLAG_ECAM, PCI_DEV_FLAG_FLR, PCI_DEV_FLAG_IOMMU_ACTIVE,
    PCI_DEV_FLAG_MSI, PCI_DEV_FLAG_MSIX, PCI_DEV_FLAG_PCIE, PCI_DEV_FLAG_REBAR,
    PCI_DEV_FLAG_SRIOV, PCI_DEV_FLAG_VIRTIO,
};
use crate::pmm::PAGE_SIZE;
use crate::security::{current_lease_epoch, CapRights, SecLabel};
use crate::uaccess::{copy_to_user, SysError};
use crate::vspace::huge::{map_mmio_2m, unmap_2m};
use crate::vspace::pte::{VmProt, PAGE_SIZE_2M, USER_VADDR_LIMIT};
use crate::vspace::{map_mmio_4k, unmap_4k, VSpaceHandle};

use super::{
    claim_pci_slot, enable_pci_bme, is_pci_cap_live, mint_root_pci_dev_cap, pci_raw_cfg_rw,
    record_pci_denied_first_or, release_claimed_pci_slot, validated_pci_slot, MAX_MANAGED_PCI_DEVS,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum DevMapSubErr {
    None,
    Inval,
    Perm,
}

#[inline(never)]
pub(super) fn sys_dev_query_bars(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    pci_cap_raw: u64,
    user_out_ptr: u64,
) -> Result<isize, SysError> {
    if user_out_ptr == 0 || (user_out_ptr & 7) != 0 {
        return Err(SysError::Inval);
    }
    let now_epoch = current_lease_epoch();
    let ptr = CapPtr(pci_cap_raw);
    let mut out_info: Option<UserPciDeviceInfo> = None;
    let res = with_pinned_cap(cspace_id, subj_label, ptr, CapRights::READ, now_epoch, |cap| {
        if cap.obj_kind != CapObjectKind::PciDev || !is_pci_cap_live(cap.obj_id, cap.expected_obj_gen) {
            return Err(CapError::WrongObjectKind);
        }
        let Some(vdev) = validated_pci_slot(cap.obj_id as usize) else {
            return Err(CapError::InvalidHandle);
        };
        let mut dev_flags = 0u16;
        if vdev.msi_cap_off != 0 {
            dev_flags |= PCI_DEV_FLAG_MSI;
        }
        if vdev.msix_cap_off != 0 {
            dev_flags |= PCI_DEV_FLAG_MSIX;
        }
        if vdev.is_virtio {
            dev_flags |= PCI_DEV_FLAG_VIRTIO;
        }
        if vdev.ext_caps.pcie_cap_off != 0 {
            dev_flags |= PCI_DEV_FLAG_PCIE;
        }
        if vdev.ext_caps.ecam_phys != 0 {
            dev_flags |= PCI_DEV_FLAG_ECAM;
        }
        if vdev.ext_caps.has_flr {
            dev_flags |= PCI_DEV_FLAG_FLR;
        }
        if vdev.ext_caps.rebar_cap_off != 0 {
            dev_flags |= PCI_DEV_FLAG_REBAR;
        }
        if vdev.ext_caps.acs_cap_off != 0 {
            dev_flags |= PCI_DEV_FLAG_ACS;
        }
        if vdev.ext_caps.aer_cap_off != 0 {
            dev_flags |= PCI_DEV_FLAG_AER;
        }
        if vdev.ext_caps.sriov_cap_off != 0 {
            dev_flags |= PCI_DEV_FLAG_SRIOV;
        }
        if super::is_iommu_enforcing_for_pci() {
            dev_flags |= PCI_DEV_FLAG_IOMMU_ACTIVE;
        }
        let mut info = build_user_pci_info(
            vdev.info.vendor_id,
            vdev.info.device_id,
            vdev.info.bus_dev_fn,
            dev_flags,
            vdev.msix_table_len,
            &vdev.bars,
        );
        info.iommu_faults = crate::iommu::poll_and_clear_faults().min(u32::MAX as u64) as u32;
        out_info = Some(info);
        Ok(())
    });
    if let Err(e) = res {
        record_pci_denied_first_or(None);
        return Err(map_cap_err_to_sys(e));
    }
    let info = out_info.ok_or(SysError::NoSys)?;
    // SAFETY: `UserPciDeviceInfo` is `#[repr(C, align(8))]` POD with zero physical addresses.
    let bytes = unsafe {
        core::slice::from_raw_parts((&raw const info).cast::<u8>(), size_of::<UserPciDeviceInfo>())
    };
    copy_to_user(user_out_ptr, bytes)?;
    Ok(0)
}

#[inline(never)]
pub(super) fn sys_dev_map_window(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    vs_h: VSpaceHandle,
    pci_cap_raw: u64,
    map_vaddr: u64,
    packed_arg: u64,
) -> Result<isize, SysError> {
    let bar_idx = (packed_arg & 0xff) as usize;
    let cache_mode = ((packed_arg >> 8) & 0xff) as u8;
    let page_count = ((packed_arg >> 16) & 0xffff) as usize;
    let page_offset = packed_arg >> 32;

    if map_vaddr < (PAGE_SIZE as u64)
        || (map_vaddr & ((PAGE_SIZE as u64) - 1)) != 0
        || bar_idx >= 6
        || cache_mode > 1
        || page_count == 0
        || page_count > MAX_WINDOW_PAGES
    {
        return Err(SysError::Inval);
    }
    let window_bytes = (page_count as u64) * (PAGE_SIZE as u64);
    let Some(end_vaddr) = map_vaddr.checked_add(window_bytes) else {
        return Err(SysError::Inval);
    };
    if end_vaddr > USER_VADDR_LIMIT {
        return Err(SysError::Inval);
    }
    let Some(end_bar_off) = page_offset
        .checked_add(page_count as u64)
        .and_then(|p| p.checked_mul(PAGE_SIZE as u64))
    else {
        return Err(SysError::Inval);
    };

    let req = CapRights(CapRights::READ.0 | CapRights::WRITE.0 | CapRights::MAP.0);
    let now_epoch = current_lease_epoch();
    let ptr = CapPtr(pci_cap_raw);
    let mut sub_err = DevMapSubErr::None;

    let res = with_pinned_cap(cspace_id, subj_label, ptr, req, now_epoch, |cap| {
        if cap.obj_kind != CapObjectKind::PciDev || !is_pci_cap_live(cap.obj_id, cap.expected_obj_gen) {
            return Err(CapError::WrongObjectKind);
        }
        let dev_idx = cap.obj_id as usize;
        let Some(vdev) = validated_pci_slot(dev_idx) else {
            return Err(CapError::InvalidHandle);
        };
        if vdev.preserve_active_gop {
            sub_err = DevMapSubErr::Perm;
            return Err(CapError::InsufficientRights);
        }
        let bar = vdev.bars[bar_idx];
        let eff_bar_limit = bar.size_bytes.max(PAGE_SIZE as u64);
        if !bar.is_mmio() || end_bar_off > eff_bar_limit {
            sub_err = DevMapSubErr::Inval;
            return Err(CapError::PagingError);
        }
        if cache_mode == 1 && !bar.prefetchable {
            sub_err = DevMapSubErr::Inval;
            return Err(CapError::PagingError);
        }
        // Prevent untrusted Ring-3 drivers from overwriting Ring-0-owned MSI-X vectors directly.
        if vdev.msix_cap_off != 0 && bar_idx == vdev.msix_table_bir as usize {
            let tbl_start_p = (vdev.msix_table_off as u64) / (PAGE_SIZE as u64);
            let tbl_end_p = ((vdev.msix_table_off as u64)
                .saturating_add((vdev.msix_table_len as u64) * 16))
            .div_ceil(PAGE_SIZE as u64);
            let win_end_p = page_offset + page_count as u64;
            if page_offset < tbl_end_p && win_end_p > tbl_start_p {
                sub_err = DevMapSubErr::Perm;
                return Err(CapError::InsufficientRights);
            }
        }

        let claimed_new = claim_pci_slot(dev_idx, cspace_id, &vdev)?;
        let prot = if cache_mode == 1 {
            VmProt::USER_MMIO_WC
        } else {
            VmProt::USER_MMIO_UC
        };
        let bar_page_base = bar.base_phys & !((PAGE_SIZE as u64) - 1);
        let bar_page_off_bytes = page_offset * (PAGE_SIZE as u64);
        for p in 0..page_count {
            let va = map_vaddr + (p as u64) * (PAGE_SIZE as u64);
            let pa = bar_page_base + bar_page_off_bytes + (p as u64) * (PAGE_SIZE as u64);
            if map_mmio_4k(vs_h, va, pa, prot).is_err() {
                for rb in 0..p {
                    let _ = unmap_4k(vs_h, map_vaddr + (rb as u64) * (PAGE_SIZE as u64));
                }
                release_claimed_pci_slot(dev_idx, claimed_new);
                return Err(CapError::PagingError);
            }
        }
        if let Err(e) = attach_cap_mapping_range_record(
            cap.global_slot,
            ptr.slot_gen(),
            vs_h.id,
            vs_h.generation,
            map_vaddr,
            false,
            page_count as u16,
        ) {
            for rb in 0..page_count {
                let _ = unmap_4k(vs_h, map_vaddr + (rb as u64) * (PAGE_SIZE as u64));
            }
            release_claimed_pci_slot(dev_idx, claimed_new);
            return Err(e);
        }

        enable_pci_bme(&vdev);
        Ok(())
    });

    if let Err(e) = res {
        record_pci_denied_first_or(None);
        return Err(match sub_err {
            DevMapSubErr::Inval => SysError::Inval,
            DevMapSubErr::Perm => SysError::Perm,
            DevMapSubErr::None => map_cap_err_to_sys(e),
        });
    }
    Ok(page_count as isize)
}

const MAX_WINDOW_2M_PAGES: usize = 16; // Up to 32 MiB per 2 MiB MMIO mapping call.

/// `DEV_MAP_OP_MAP_WINDOW_2M` (`op = 4`): Map `1..=16` `2 MiB` huge-page (`PDE.PS`) MMIO pages
/// (`UC` or prefetchable `WC` via `PTE_PAT_2M`) into CPL=3 with MSI-X table overlap checks.
#[inline(never)]
pub(super) fn sys_dev_map_window_2m(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    vs_h: VSpaceHandle,
    pci_cap_raw: u64,
    map_vaddr: u64,
    packed_arg: u64,
) -> Result<isize, SysError> {
    let bar_idx = (packed_arg & 0xff) as usize;
    let cache_mode = ((packed_arg >> 8) & 0xff) as u8;
    let huge_count = ((packed_arg >> 16) & 0xffff) as usize;
    let huge_offset = packed_arg >> 32;

    if map_vaddr < PAGE_SIZE_2M
        || (map_vaddr & (PAGE_SIZE_2M - 1)) != 0
        || bar_idx >= 6
        || cache_mode > 1
        || huge_count == 0
        || huge_count > MAX_WINDOW_2M_PAGES
    {
        return Err(SysError::Inval);
    }
    let Some(window_bytes) = (huge_count as u64).checked_mul(PAGE_SIZE_2M) else {
        return Err(SysError::Inval);
    };
    let Some(end_vaddr) = map_vaddr.checked_add(window_bytes) else {
        return Err(SysError::Inval);
    };
    if end_vaddr > USER_VADDR_LIMIT {
        return Err(SysError::Inval);
    }
    let Some(bar_off_bytes) = huge_offset.checked_mul(PAGE_SIZE_2M) else {
        return Err(SysError::Inval);
    };
    let Some(end_bar_off) = bar_off_bytes.checked_add(window_bytes) else {
        return Err(SysError::Inval);
    };

    let req = CapRights(CapRights::READ.0 | CapRights::WRITE.0 | CapRights::MAP.0);
    let now_epoch = current_lease_epoch();
    let ptr = CapPtr(pci_cap_raw);
    let mut sub_err = DevMapSubErr::None;

    let res = with_pinned_cap(cspace_id, subj_label, ptr, req, now_epoch, |cap| {
        if cap.obj_kind != CapObjectKind::PciDev || !is_pci_cap_live(cap.obj_id, cap.expected_obj_gen) {
            return Err(CapError::WrongObjectKind);
        }
        let dev_idx = cap.obj_id as usize;
        let Some(vdev) = validated_pci_slot(dev_idx) else {
            return Err(CapError::InvalidHandle);
        };
        if vdev.preserve_active_gop {
            sub_err = DevMapSubErr::Perm;
            return Err(CapError::InsufficientRights);
        }
        let bar = vdev.bars[bar_idx];
        if !bar.is_mmio()
            || (bar.base_phys & (PAGE_SIZE_2M - 1)) != 0
            || end_bar_off > bar.size_bytes
        {
            sub_err = DevMapSubErr::Inval;
            return Err(CapError::PagingError);
        }
        if cache_mode == 1 && !bar.prefetchable {
            sub_err = DevMapSubErr::Inval;
            return Err(CapError::PagingError);
        }
        if vdev.msix_cap_off != 0 && bar_idx == vdev.msix_table_bir as usize {
            let tbl_start = (vdev.msix_table_off as u64) & !((PAGE_SIZE as u64) - 1);
            let tbl_end = (vdev.msix_table_off as u64)
                .saturating_add((vdev.msix_table_len as u64) * 16)
                .next_multiple_of(PAGE_SIZE as u64);
            if bar_off_bytes < tbl_end && end_bar_off > tbl_start {
                sub_err = DevMapSubErr::Perm;
                return Err(CapError::InsufficientRights);
            }
        }

        let claimed_new = claim_pci_slot(dev_idx, cspace_id, &vdev)?;
        let prot = if cache_mode == 1 {
            VmProt::USER_MMIO_WC
        } else {
            VmProt::USER_MMIO_UC
        };
        let mut cur_pa = bar.base_phys + bar_off_bytes;
        for p in 0..huge_count {
            let va = map_vaddr + (p as u64) * PAGE_SIZE_2M;
            if map_mmio_2m(vs_h, va, cur_pa, prot).is_err() {
                for rb in 0..p {
                    let _ = unmap_2m(vs_h, map_vaddr + (rb as u64) * PAGE_SIZE_2M);
                }
                release_claimed_pci_slot(dev_idx, claimed_new);
                return Err(CapError::PagingError);
            }
            cur_pa += PAGE_SIZE_2M;
        }
        if let Err(e) = attach_cap_mapping_range_record(
            cap.global_slot,
            ptr.slot_gen(),
            vs_h.id,
            vs_h.generation,
            map_vaddr,
            true,
            huge_count as u16,
        ) {
            for rb in 0..huge_count {
                let _ = unmap_2m(vs_h, map_vaddr + (rb as u64) * PAGE_SIZE_2M);
            }
            release_claimed_pci_slot(dev_idx, claimed_new);
            return Err(e);
        }

        enable_pci_bme(&vdev);
        Ok(())
    });

    if let Err(e) = res {
        record_pci_denied_first_or(None);
        return Err(match sub_err {
            DevMapSubErr::Inval => SysError::Inval,
            DevMapSubErr::Perm => SysError::Perm,
            DevMapSubErr::None => map_cap_err_to_sys(e),
        });
    }
    Ok(huge_count as isize)
}

#[inline]
fn cfg_ranges_overlap(off: u16, width: u8, start: u16, end: u16) -> bool {
    let w_end = off.saturating_add(width as u16);
    off < end && w_end > start
}

fn is_protected_cfg_write_offset(
    vdev: &crate::pci::ValidatedVirtioPci,
    off: u16,
    width: u8,
) -> bool {
    if cfg_ranges_overlap(off, width, 0x00, 0x08)
        || cfg_ranges_overlap(off, width, 0x10, 0x28)
        || cfg_ranges_overlap(off, width, 0x30, 0x34)
        || cfg_ranges_overlap(off, width, 0x3c, 0x40)
    {
        return true;
    }
    if vdev.msi_cap_off != 0 {
        let s = vdev.msi_cap_off as u16;
        if cfg_ranges_overlap(off, width, s, s.saturating_add(24)) {
            return true;
        }
    }
    if vdev.msix_cap_off != 0 {
        let s = vdev.msix_cap_off as u16;
        if cfg_ranges_overlap(off, width, s, s.saturating_add(12)) {
            return true;
        }
    }
    if vdev.ext_caps.pcie_cap_off != 0 {
        let s = vdev.ext_caps.pcie_cap_off as u16;
        if cfg_ranges_overlap(off, width, s, s.saturating_add(10)) {
            return true;
        }
    }
    if vdev.ext_caps.rebar_cap_off != 0 {
        let s = vdev.ext_caps.rebar_cap_off;
        if cfg_ranges_overlap(off, width, s, s.saturating_add(0x30)) {
            return true;
        }
    }
    if vdev.ext_caps.sriov_cap_off != 0 {
        let s = vdev.ext_caps.sriov_cap_off;
        if cfg_ranges_overlap(off, width, s, s.saturating_add(0x40)) {
            return true;
        }
    }
    if vdev.ext_caps.acs_cap_off != 0 {
        let s = vdev.ext_caps.acs_cap_off;
        if cfg_ranges_overlap(off, width, s, s.saturating_add(0x10)) {
            return true;
        }
    }
    false
}

/// `DEV_MAP_OP_CFG_RW` (`op = 3`): Capability-gated PCI/PCIe config-space access (`0..4096` via
/// ECAM when mapped, else `0..256` via `0xCF8`/`0xCFC`). Reads require `READ` only; writes
/// require `READ | WRITE`, IOMMU slot ownership, and non-protected register offsets.
#[inline(never)]
pub(super) fn sys_dev_cfg_rw(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    pci_cap_raw: u64,
    op_arg: u64,
    write_arg: u64,
) -> Result<(isize, u64), SysError> {
    let offset = ((op_arg >> 12) & 0xfff) as u16;
    let width = ((op_arg >> 24) & 0xf) as u8;
    let is_write = ((op_arg >> 28) & 1) != 0;
    if !matches!(width, 1 | 2 | 4)
        || (offset & ((width as u16) - 1)) != 0
        || (offset as usize) + (width as usize) > 4096
    {
        return Err(SysError::Inval);
    }

    let req = if is_write {
        CapRights(CapRights::READ.0 | CapRights::WRITE.0)
    } else {
        CapRights::READ
    };
    let now_epoch = current_lease_epoch();
    let ptr = CapPtr(pci_cap_raw);
    let mut sub_err = DevMapSubErr::None;
    let mut no_ecam = false;
    let mut out_val = 0u32;

    let res = with_pinned_cap(cspace_id, subj_label, ptr, req, now_epoch, |cap| {
        if cap.obj_kind != CapObjectKind::PciDev || !is_pci_cap_live(cap.obj_id, cap.expected_obj_gen) {
            return Err(CapError::WrongObjectKind);
        }
        let dev_idx = cap.obj_id as usize;
        let Some(vdev) = validated_pci_slot(dev_idx) else {
            return Err(CapError::InvalidHandle);
        };
        if (offset as usize) + (width as usize) > 256
            && (vdev.ext_caps.ecam_phys == 0 || !crate::pci_ecam::is_kernel_ecam_ready())
        {
            no_ecam = true;
            return Err(CapError::InvalidHandle);
        }
        if is_write {
            if vdev.preserve_active_gop || is_protected_cfg_write_offset(&vdev, offset, width) {
                sub_err = DevMapSubErr::Perm;
                return Err(CapError::InsufficientRights);
            }
            let claimed_new = claim_pci_slot(dev_idx, cspace_id, &vdev)?;
            let Some(val) = pci_raw_cfg_rw(dev_idx, &vdev, offset, width, true, write_arg as u32) else {
                release_claimed_pci_slot(dev_idx, claimed_new);
                no_ecam = true;
                return Err(CapError::InvalidHandle);
            };
            out_val = val;
        } else {
            let Some(val) = pci_raw_cfg_rw(dev_idx, &vdev, offset, width, false, 0) else {
                no_ecam = true;
                return Err(CapError::InvalidHandle);
            };
            out_val = val;
        }
        Ok(())
    });

    if let Err(e) = res {
        record_pci_denied_first_or(None);
        return Err(if no_ecam {
            SysError::NoSys
        } else {
            match sub_err {
                DevMapSubErr::Inval => SysError::Inval,
                DevMapSubErr::Perm => SysError::Perm,
                DevMapSubErr::None => map_cap_err_to_sys(e),
            }
        });
    }
    Ok((0, out_val as u64))
}

/// `DEV_MAP_OP_ENUM_MINT` (`op = 5`): Mint a `CapObjectKind::PciDev` capability for any live
/// dynamically enumerated slot (`0..64`) for a `RootSystem` caller holding `PCI_ROOT` (`MANAGE|GRANT`).
#[inline(never)]
pub(super) fn sys_dev_enum_mint(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    pci_cap_raw: u64,
    op_arg: u64,
) -> Result<(isize, u64), SysError> {
    let slot_idx = ((op_arg >> 12) & 0x3ff) as usize;
    if slot_idx >= MAX_MANAGED_PCI_DEVS {
        return Err(SysError::Inval);
    }
    let req = CapRights(CapRights::MANAGE.0 | CapRights::GRANT.0);
    let now_epoch = current_lease_epoch();
    let check_res = with_pinned_cap(cspace_id, subj_label, CapPtr(pci_cap_raw), req, now_epoch, |cap| {
        if cap.obj_kind != CapObjectKind::PciDev || !is_pci_cap_live(cap.obj_id, cap.expected_obj_gen) {
            return Err(CapError::WrongObjectKind);
        }
        Ok(())
    });
    if let Err(e) = check_res {
        record_pci_denied_first_or(None);
        return Err(map_cap_err_to_sys(e));
    }
    let new_cap = mint_root_pci_dev_cap(cspace_id, subj_label, slot_idx).map_err(|e| {
        record_pci_denied_first_or(None);
        map_cap_err_to_sys(e)
    })?;
    Ok((0, new_cap.0))
}
