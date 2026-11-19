// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Intel `VT-d` per-device DMA domain lifecycle, dynamic DID allocation, and `SLPT` map/unmap.

use core::ptr::{read_volatile, write_volatile};

use crate::iommu::dmar::dmar_snapshot;
use crate::iommu::ops::{IommuPerms, IommuStatus};
use crate::iommu::slpt::{
    bdf_has_active_rmrr, clear_slpt_4k, flush_descriptor_cacheline, flush_entire_page,
    free_all_domain_frames, get_or_alloc_bus_ctx, map_slpt_4k, map_slpt_4k_iova, write_ctx_entry,
    BOOT_RMRR_DID, VTD_KERNEL_LABEL,
};
use crate::iommu::vtd::{
    invalidate_context_cache, invalidate_iotlb_cache, maybe_release_bus_ctx,
    poll_and_clear_unit_faults, status, with_vtd_lock, VtdEngineState, CCMD_CIRG_DEVICE,
    IOTLB_IIRG_DOMAIN,
};
use crate::pci::MAX_MANAGED_PCI_DEVS;
use crate::pmm::{alloc_pages, AllocFlags, PageOrder, PAGE_SIZE};

/// Pick the first free Domain ID in `2..max_domains` (reserving `0` and `BOOT_RMRR_DID = 1`).
pub(crate) fn pick_did(max_domains: usize, in_use: &[u16]) -> Option<u16> {
    let limit = max_domains.min(u16::MAX as usize);
    for cand in 2..limit {
        let did = cand as u16;
        if did != BOOT_RMRR_DID && !in_use.contains(&did) {
            return Some(did);
        }
    }
    None
}

fn alloc_did_for_unit(eng: &VtdEngineState, unit_idx: usize) -> Option<u16> {
    let mut in_use = [0u16; MAX_MANAGED_PCI_DEVS];
    let mut n = 0usize;
    for dom in eng.domains.iter() {
        if dom.attached && dom.unit_idx as usize == unit_idx {
            in_use[n] = dom.domain_id;
            n += 1;
        }
    }
    pick_did(eng.units[unit_idx].max_domains, &in_use[..n])
}

fn find_unit_for_bdf(eng: &VtdEngineState, bus: u8, dev: u8, func: u8) -> Option<usize> {
    let dmar = dmar_snapshot()?;
    for u in 0..dmar.drhd_count {
        if !eng.units[u].active {
            continue;
        }
        let src = &dmar.drhds[u];
        if src.segment != 0 {
            continue;
        }
        for &sc in src.scopes.iter().take(src.scope_count) {
            if sc.matches_pci_bdf(bus, dev, func) {
                return if eng.units[u].translation_enabled {
                    Some(u)
                } else {
                    None
                };
            }
        }
    }
    (0..dmar.drhd_count).find(|&u| {
        eng.units[u].active
            && eng.units[u].translation_enabled
            && eng.units[u].include_all
            && eng.units[u].segment == 0
    })
}

/// Fail-closed gate checked before claiming a PCI device or enabling `Command.BME = 1`.
pub fn attach_device_domain(dev_idx: usize, bus: u8, dev: u8, func: u8) -> Result<(), ()> {
    if dev_idx >= MAX_MANAGED_PCI_DEVS {
        return Err(());
    }
    match status() {
        IommuStatus::Enforcing => {}
        IommuStatus::TestUnsafeNoIommu => return Ok(()),
        IommuStatus::Uninitialized
        | IommuStatus::UnsupportedFailClosed
        | IommuStatus::FailedClosed => return Err(()),
    }
    let rmrr_snap = dmar_snapshot();

    with_vtd_lock(|eng| {
        if eng.domains[dev_idx].attached {
            return Ok(());
        }
        let unit_idx = find_unit_for_bdf(eng, bus, dev, func).ok_or(())?;
        let did_u16 = alloc_did_for_unit(eng, unit_idx).ok_or(())?;
        let (coherent, levels_4, seg) = (
            eng.units[unit_idx].coherent,
            eng.units[unit_idx].levels_4,
            eng.units[unit_idx].segment,
        );
        let root_h =
            alloc_pages(PageOrder::Order4KiB, VTD_KERNEL_LABEL, AllocFlags::ZERO).map_err(|_| ())?;
        flush_entire_page(coherent, root_h.hhdm_ptr());
        eng.domains[dev_idx].domain_id = did_u16;
        eng.domains[dev_idx].sl_root = Some(root_h);
        eng.domains[dev_idx].pt_count = 0;

        if let Some(dmar) = rmrr_snap {
            for rmrr in dmar.rmrrs.iter().take(dmar.rmrr_count) {
                if !rmrr.valid || rmrr.segment != seg {
                    continue;
                }
                let matched = (0..rmrr.scope_count)
                    .any(|s| rmrr.scopes[s].matches_pci_bdf(bus, dev, func));
                if !matched {
                    continue;
                }
                let mut pa = rmrr.base_addr & !((PAGE_SIZE as u64) - 1);
                let end_pa = rmrr.limit_addr & !((PAGE_SIZE as u64) - 1);
                while pa <= end_pa {
                    if map_slpt_4k(&mut eng.domains[dev_idx], coherent, pa, levels_4).is_err() {
                        free_all_domain_frames(&mut eng.domains[dev_idx]);
                        return Err(());
                    }
                    pa = match pa.checked_add(PAGE_SIZE as u64) {
                        Some(n) => n,
                        None => break,
                    };
                }
            }
        }

        let bus_slot = match get_or_alloc_bus_ctx(&mut eng.units[unit_idx], bus) {
            Ok(i) => i,
            Err(()) => {
                free_all_domain_frames(&mut eng.domains[dev_idx]);
                return Err(());
            }
        };
        let ctx_fh = eng.units[unit_idx].ctx_buses[bus_slot].frame.unwrap();
        let devfn = (((dev & 0x1f) as usize) << 3) | ((func & 0x07) as usize);
        let sid = ((bus as u64) << 8) | (devfn as u64);
        let did = did_u16 as u64;
        let has_boot_rmrr = bdf_has_active_rmrr(rmrr_snap, seg, bus, dev, func)
            && eng.rmrr_domains[unit_idx].sl_root.is_some();

        // VT-d §6.2.2: Clear a pre-mapped Present RMRR Context Entry & invalidate before reprogramming.
        // SAFETY: Context Entry sits inside allocated per-bus Context Table page `ctx_fh`.
        let was_present = unsafe {
            let ctx_ptr = (ctx_fh.hhdm_ptr() as *const u64).add(devfn * 2);
            (read_volatile(ctx_ptr) & 0x1) != 0
        };
        if was_present {
            // SAFETY: Clearing old Present bit before invalidating cached RMRR translation.
            unsafe {
                let ctx_ptr = (ctx_fh.hhdm_ptr() as *mut u64).add(devfn * 2);
                write_volatile(ctx_ptr, 0);
                write_volatile(ctx_ptr.add(1), 0);
                flush_descriptor_cacheline(coherent, ctx_ptr as *const u8);
            }
            let rdid = BOOT_RMRR_DID as u64;
            let _ = invalidate_context_cache(
                &eng.units[unit_idx],
                CCMD_CIRG_DEVICE | rdid | (sid << 16),
            );
            let _ = invalidate_iotlb_cache(&eng.units[unit_idx], IOTLB_IIRG_DOMAIN | (rdid << 32));
        }

        write_ctx_entry(
            ctx_fh.hhdm_ptr(),
            coherent,
            dev,
            func,
            levels_4,
            did,
            root_h.phys_addr(),
        );

        let inv_ok = invalidate_context_cache(
            &eng.units[unit_idx],
            CCMD_CIRG_DEVICE | did | (sid << 16),
        ) && invalidate_iotlb_cache(&eng.units[unit_idx], IOTLB_IIRG_DOMAIN | (did << 32));
        if !inv_ok {
            if has_boot_rmrr {
                if let Some(rmrr_root) = eng.rmrr_domains[unit_idx].sl_root {
                    write_ctx_entry(
                        ctx_fh.hhdm_ptr(),
                        coherent,
                        dev,
                        func,
                        levels_4,
                        BOOT_RMRR_DID as u64,
                        rmrr_root.phys_addr(),
                    );
                }
            } else {
                // SAFETY: Clearing Context Entry on invalidation failure rollback.
                unsafe {
                    let ctx_ptr = (ctx_fh.hhdm_ptr() as *mut u64).add(devfn * 2);
                    write_volatile(ctx_ptr, 0);
                    write_volatile(ctx_ptr.add(1), 0);
                    flush_descriptor_cacheline(coherent, ctx_ptr as *const u8);
                }
                maybe_release_bus_ctx(&mut eng.units[unit_idx], bus);
            }
            free_all_domain_frames(&mut eng.domains[dev_idx]);
            return Err(());
        }

        eng.units[unit_idx].ctx_buses[bus_slot].attach_count += 1;
        eng.domains[dev_idx].attached = true;
        eng.domains[dev_idx].unit_idx = unit_idx as u8;
        eng.domains[dev_idx].segment = seg;
        eng.domains[dev_idx].bus = bus;
        eng.domains[dev_idx].dev = dev;
        eng.domains[dev_idx].func = func;
        Ok(())
    })
}

fn rollback_mapped_slpt_pages(
    coherent: bool,
    root_phys: u64,
    iova_base: u64,
    pages: usize,
    levels_4: bool,
) {
    for rb in 0..pages {
        clear_slpt_4k(
            coherent,
            root_phys,
            iova_base + (rb as u64) * (PAGE_SIZE as u64),
            levels_4,
        );
    }
}

/// Map `[iova_base .. iova_base + len_bytes)` -> `[phys_base .. phys_base + len_bytes)`
/// into `dev_idx`'s isolated SLPT with explicit DMA direction permissions (`R`/`W`).
pub fn map_dma_range_iova(
    dev_idx: usize,
    iova_base: u64,
    phys_base: u64,
    len_bytes: u64,
    perms: IommuPerms,
) -> Result<(), ()> {
    let page_mask = (PAGE_SIZE as u64) - 1;
    if dev_idx >= MAX_MANAGED_PCI_DEVS
        || (iova_base & page_mask) != 0
        || (phys_base & page_mask) != 0
        || (len_bytes & page_mask) != 0
        || len_bytes == 0
    {
        return Err(());
    }
    let last_off = len_bytes - (PAGE_SIZE as u64);
    let last_iova = iova_base.checked_add(last_off).ok_or(())?;
    let last_pa = phys_base.checked_add(last_off).ok_or(())?;
    match status() {
        IommuStatus::Enforcing => {}
        IommuStatus::TestUnsafeNoIommu => return Ok(()),
        IommuStatus::Uninitialized
        | IommuStatus::UnsupportedFailClosed
        | IommuStatus::FailedClosed => return Err(()),
    }
    let pages = (len_bytes / (PAGE_SIZE as u64)) as usize;
    with_vtd_lock(|eng| {
        let dom = &mut eng.domains[dev_idx];
        if !dom.attached {
            return Err(());
        }
        let u = &eng.units[dom.unit_idx as usize];
        let max_addr = if u.levels_4 { 1u64 << 48 } else { 1u64 << 39 };
        if last_iova >= max_addr || last_pa >= max_addr {
            return Err(());
        }
        let (coherent, levels_4, did) = (u.coherent, u.levels_4, dom.domain_id as u64);
        for p in 0..pages {
            let off = (p as u64) * (PAGE_SIZE as u64);
            let iova = iova_base + off;
            let pa = phys_base + off;
            if map_slpt_4k_iova(dom, coherent, iova, pa, perms, levels_4).is_err() {
                if let Some(root_h) = dom.sl_root {
                    rollback_mapped_slpt_pages(coherent, root_h.phys_addr(), iova_base, p, levels_4);
                }
                return Err(());
            }
        }
        if !invalidate_iotlb_cache(u, IOTLB_IIRG_DOMAIN | (did << 32)) {
            if let Some(root_h) = dom.sl_root {
                rollback_mapped_slpt_pages(coherent, root_h.phys_addr(), iova_base, pages, levels_4);
            }
            return Err(());
        }
        Ok(())
    })
}

/// Map `len_bytes` (`4 KiB` or `2 MiB` pinned `DEVICE_DMA`) identity (`IOVA = phys_base`).
pub fn map_dma_range(dev_idx: usize, phys_base: u64, len_bytes: u64) -> Result<(), ()> {
    map_dma_range_iova(dev_idx, phys_base, phys_base, len_bytes, IommuPerms::RW)
}

/// Unmap `len_bytes` from `dev_idx`'s SLPT (`iova_base`) and synchronize IOTLB.
pub fn unmap_dma_range(dev_idx: usize, iova_base: u64, len_bytes: u64) {
    let page_mask = (PAGE_SIZE as u64) - 1;
    if status() != IommuStatus::Enforcing
        || dev_idx >= MAX_MANAGED_PCI_DEVS
        || (iova_base & page_mask) != 0
        || (len_bytes & page_mask) != 0
        || len_bytes == 0
        || iova_base.checked_add(len_bytes - (PAGE_SIZE as u64)).is_none()
    {
        return;
    }
    let pages = (len_bytes / (PAGE_SIZE as u64)) as usize;
    with_vtd_lock(|eng| {
        let dom = &mut eng.domains[dev_idx];
        if !dom.attached {
            return;
        }
        let u = &eng.units[dom.unit_idx as usize];
        if let Some(root_h) = dom.sl_root {
            rollback_mapped_slpt_pages(u.coherent, root_h.phys_addr(), iova_base, pages, u.levels_4);
        }
        let _ = invalidate_iotlb_cache(u, IOTLB_IIRG_DOMAIN | ((dom.domain_id as u64) << 32));
    });
}

/// True when `dev_idx`'s active VT-d unit has hardware Snoop/Coherency (`ECAP.C = 1`) or no IOMMU requires CLFLUSH.
pub fn is_device_coherent(dev_idx: usize) -> bool {
    if status() != IommuStatus::Enforcing || dev_idx >= MAX_MANAGED_PCI_DEVS {
        return true;
    }
    with_vtd_lock(|eng| {
        let dom = &eng.domains[dev_idx];
        if !dom.attached {
            return false;
        }
        eng.units[dom.unit_idx as usize].coherent
    })
}

/// Clear Context Entry, invalidate Context + Domain IOTLB, and free all SLPT frames (`leaked_frames == 0`).
pub fn detach_and_scrub_domain(dev_idx: usize) {
    if status() != IommuStatus::Enforcing || dev_idx >= MAX_MANAGED_PCI_DEVS {
        return;
    }
    let rmrr_snap = dmar_snapshot();
    with_vtd_lock(|eng| {
        if !eng.domains[dev_idx].attached {
            return;
        }
        let unit_idx = eng.domains[dev_idx].unit_idx as usize;
        let seg = eng.domains[dev_idx].segment;
        let bus = eng.domains[dev_idx].bus;
        let dev = eng.domains[dev_idx].dev;
        let func = eng.domains[dev_idx].func;
        let did = eng.domains[dev_idx].domain_id as u64;
        let devfn = (((dev & 0x1f) as usize) << 3) | ((func & 0x07) as usize);

        let bs_opt = eng.units[unit_idx]
            .ctx_buses
            .iter()
            .position(|c| c.active && c.bus == bus);
        if let Some(bs) = bs_opt {
            if let Some(ctx_fh) = eng.units[unit_idx].ctx_buses[bs].frame {
                // SAFETY: Clear present bit on Context Entry before invalidating caches.
                unsafe {
                    let ctx_ptr = (ctx_fh.hhdm_ptr() as *mut u64).add(devfn * 2);
                    write_volatile(ctx_ptr, 0);
                    write_volatile(ctx_ptr.add(1), 0);
                    flush_descriptor_cacheline(eng.units[unit_idx].coherent, ctx_ptr as *const u8);
                }
            }
            if eng.units[unit_idx].ctx_buses[bs].attach_count > 0 {
                eng.units[unit_idx].ctx_buses[bs].attach_count -= 1;
            }
        }

        let sid = ((bus as u64) << 8) | (devfn as u64);
        let _ = invalidate_context_cache(
            &eng.units[unit_idx],
            CCMD_CIRG_DEVICE | did | (sid << 16),
        );
        let _ = invalidate_iotlb_cache(&eng.units[unit_idx], IOTLB_IIRG_DOMAIN | (did << 32));
        poll_and_clear_unit_faults(&mut eng.units[unit_idx]);

        if bdf_has_active_rmrr(rmrr_snap, seg, bus, dev, func) {
            if let (Some(bs), Some(rmrr_root)) = (bs_opt, eng.rmrr_domains[unit_idx].sl_root) {
                if let Some(ctx_fh) = eng.units[unit_idx].ctx_buses[bs].frame {
                    let (coherent, levels_4) =
                        (eng.units[unit_idx].coherent, eng.units[unit_idx].levels_4);
                    write_ctx_entry(
                        ctx_fh.hhdm_ptr(),
                        coherent,
                        dev,
                        func,
                        levels_4,
                        BOOT_RMRR_DID as u64,
                        rmrr_root.phys_addr(),
                    );
                    let rdid = BOOT_RMRR_DID as u64;
                    let _ = invalidate_context_cache(
                        &eng.units[unit_idx],
                        CCMD_CIRG_DEVICE | rdid | (sid << 16),
                    );
                    let _ = invalidate_iotlb_cache(
                        &eng.units[unit_idx],
                        IOTLB_IIRG_DOMAIN | (rdid << 32),
                    );
                }
            }
        } else {
            maybe_release_bus_ctx(&mut eng.units[unit_idx], bus);
        }

        free_all_domain_frames(&mut eng.domains[dev_idx]);
        eng.domains[dev_idx].domain_id = 0;
        eng.domains[dev_idx].attached = false;
    });
}
