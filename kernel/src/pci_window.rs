// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-gated 64-bit PCI BAR discovery (`QUERY_BARS`) and bounded MMIO window mapping
//! (`MAP_WINDOW`, `UC` or prefetchable `WC`) split from `pci.rs` for clean size/cohesion bounds.

use core::mem::size_of;

use crate::cspace::cap::{CSpaceId, CapError, CapObjectKind, CapPtr};
use crate::cspace::revoke::attach_cap_mapping_range_record;
use crate::cspace::with_pinned_cap;
use crate::ipc::map_cap_err_to_sys;
use crate::pci_bar::{
    build_user_pci_info, UserPciDeviceInfo, MAX_WINDOW_PAGES, PCI_DEV_FLAG_IOMMU_ACTIVE,
    PCI_DEV_FLAG_MSI, PCI_DEV_FLAG_MSIX, PCI_DEV_FLAG_VIRTIO,
};
use crate::pmm::PAGE_SIZE;
use crate::security::{current_lease_epoch, CapRights, SecLabel};
use crate::uaccess::{copy_to_user, SysError};
use crate::vspace::pte::{VmProt, USER_VADDR_LIMIT};
use crate::vspace::{map_mmio_4k, unmap_4k, VSpaceHandle};

use super::{
    claim_pci_slot, enable_pci_bme, is_pci_cap_live, record_pci_denied_first_or,
    release_claimed_pci_slot, validated_pci_slot,
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
