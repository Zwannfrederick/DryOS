// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-Gated PCI VirtIO Discovery, MMIO BAR Mapping & Pinned `DEVICE_DMA` Allocation
//! (`Milestone 2B` & `Milestone 2C-1` — `ADR-004`, `ADR-007` Rule 10, `INV-CAP-08`, `R-03`).
//!
//! Keeps Ring 0 completely free of block/filesystem/RNG/GPU/Linux drivers:
//! 1. Scans PCI Configuration Space (`0xCF8`/`0xCFC`) on the BSP for VirtIO 1.0 Modern Block
//!    (`1AF4:1042`/`1001`), Entropy (`1AF4:1044`/`1005`), and GPU (`1AF4:1050`) controllers and
//!    validates MMIO structures (`COMMON`, `NOTIFY`, `ISR`, optional `DEVICE`) against
//!    `FrameState::ReservedOrMmio`.
//! 2. Exposes `SYS_DEV_MAP` (`CapObjectKind::PciDev`, `VmProt::USER_MMIO_UC`) and `SYS_DMA_ALLOC`
//!    (`SecDomain::RootSystem` non-`DEV_WORKSPACE` `DEVICE_DMA | NO_DELEGATE` pinned `4K`/`2M` frames).
//! 3. Enforces `R-03` & `INV-CAP-08`: active DMA frames carry `iommu_pin_count == 1` (rejecting
//!    premature `cspace_destroy_object` with `-EBUSY`), and driver crash/exit/revoke clears PCI
//!    `Command.BME = 0` in hardware *before* unpinning DMA frames and reclaiming memory.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};

use crate::cspace::cap::{CSpaceId, CapError, CapObjectKind, CapPtr};
use crate::cspace::revoke::attach_cap_mapping_range_record;
use crate::cspace::{insert_root_cap, split_global_slot, with_pinned_cap};
use crate::iommu::{attach_device_domain, detach_and_scrub_domain, is_dma_permitted, is_hw_enforcing};
use crate::ipc::map_cap_err_to_sys;
use crate::pci_bar::{
    DEV_MAP_OP_CFG_RW, DEV_MAP_OP_ENUM_MINT, DEV_MAP_OP_LEGACY_VIRTIO, DEV_MAP_OP_MAP_WINDOW,
    DEV_MAP_OP_MAP_WINDOW_2M, DEV_MAP_OP_MASK, DEV_MAP_OP_QUERY_BARS, PCI_BAR_FLAG_64BIT,
    PCI_BAR_FLAG_IO, PCI_BAR_FLAG_MMIO, PCI_BAR_FLAG_PREFETCHABLE, PCI_DEV_FLAG_ECAM,
    PCI_DEV_FLAG_FLR, PCI_DEV_FLAG_MSI, PCI_DEV_FLAG_MSIX, PCI_DEV_FLAG_PCIE, PCI_DEV_FLAG_REBAR,
    PCI_DEV_FLAG_VIRTIO,
};
use crate::pmm::PAGE_SIZE;
use crate::security::{current_lease_epoch, CapRights, SecDomain, SecFlags, SecLabel};
use crate::serial_println;
use crate::uaccess::{copy_to_user, SysError};
use crate::vspace::pte::{
    pci_ecam_vaddr, pci_msix_table_vaddr, pci_virtio_common_vaddr, VmProt, KERNEL_MSIX_MAX_PAGES,
    USER_VADDR_LIMIT,
};
use crate::vspace::{inspect_vspace_live, map_mmio_4k, unmap_4k, VSpaceHandle, VSpaceId};

#[path = "pci_cfg.rs"]
mod pci_cfg;
#[path = "pci_window.rs"]
mod pci_window;

pub use crate::pci_slots::{
    active_gop_bdf, latched_boot_pci_funcs, pci_topology_summary, BootPciFuncRecord,
    BOOT_PCI_ENUM_OFFSET, MAX_BOOT_PCI_FUNCS, PCI_OBJ_DISPLAY_AUX, PCI_OBJ_IGPU, PCI_OBJ_IVSHMEM,
    PCI_OBJ_NET, PCI_OBJ_NVME, PCI_OBJ_QEMU_EDU, PCI_OBJ_VIRTIO_BLK, PCI_OBJ_VIRTIO_GPU,
    PCI_OBJ_VIRTIO_RNG, PCI_OBJ_XHCI,
};
pub(crate) use crate::pci_slots::{
    encode_bar_desc_flags, publish_boot_pci_count, push_boot_pci_record, record_topology_hints,
    slot_label, MAX_MANAGED_PCI_DEVS, PCI_LEGACY_FIXED_SLOTS,
};
pub use pci_cfg::UserVirtioPciInfo;
pub(crate) use pci_cfg::{
    pci_ecam_read_u32, pci_raw_cfg_rw, pci_read_u16, pci_read_u32, pci_read_u8, pci_write_u16,
    pci_write_u32, probe_all_bars_raw, probe_pci_function, ValidatedVirtioPci,
};

#[inline]
pub fn pci_cfg_probe_all_bars_raw(bus: u8, dev: u8, func: u8) -> ([u32; 6], [u32; 6]) {
    probe_all_bars_raw(bus, dev, func)
}

struct PciVirtioSlot {
    present: AtomicBool,
    generation: AtomicU32,
    owner_cspace: AtomicU16,
    dma_denied_ops: AtomicU64,
    quiesce_count: AtomicU64,
    dev_inner: UnsafeCell<ValidatedVirtioPci>,
}

impl PciVirtioSlot {
    const fn new() -> Self {
        Self {
            present: AtomicBool::new(false),
            generation: AtomicU32::new(1),
            owner_cspace: AtomicU16::new(0),
            dma_denied_ops: AtomicU64::new(0),
            quiesce_count: AtomicU64::new(0),
            dev_inner: UnsafeCell::new(ValidatedVirtioPci::EMPTY),
        }
    }
}

// SAFETY: `dev_inner` is written once on BSP before `present = true`; atomics synchronize state.
unsafe impl Sync for PciVirtioSlot {}

static PCI_DEVS: [PciVirtioSlot; MAX_MANAGED_PCI_DEVS] =
    [const { PciVirtioSlot::new() }; MAX_MANAGED_PCI_DEVS];

#[inline]
pub(crate) fn reset_pci_slot_owner(dev_idx: usize, expected_cspace: CSpaceId) {
    let slot = &PCI_DEVS[dev_idx];
    if slot.owner_cspace.load(Ordering::Acquire) == expected_cspace {
        slot.owner_cspace.store(0, Ordering::Release);
        slot.quiesce_count.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub fn is_pci_dev_present(dev_idx: usize) -> bool {
    dev_idx < MAX_MANAGED_PCI_DEVS && PCI_DEVS[dev_idx].present.load(Ordering::Acquire)
}

#[inline]
pub fn is_pci_blk_present() -> bool {
    is_pci_dev_present(PCI_OBJ_VIRTIO_BLK)
}

#[inline]
pub fn is_pci_cap_live(obj_id: u32, expected_gen: u32) -> bool {
    let idx = obj_id as usize;
    idx < MAX_MANAGED_PCI_DEVS
        && PCI_DEVS[idx].present.load(Ordering::Acquire)
        && PCI_DEVS[idx].generation.load(Ordering::Acquire) == expected_gen
}

#[inline]
pub fn pci_dma_telemetry() -> (u64, u64) {
    let mut denied = 0u64;
    let mut quiesces = 0u64;
    for slot in PCI_DEVS.iter() {
        denied = denied.saturating_add(slot.dma_denied_ops.load(Ordering::Acquire));
        quiesces = quiesces.saturating_add(slot.quiesce_count.load(Ordering::Acquire));
    }
    (denied, quiesces)
}

/// Enumerate PCI/PCIe buses (`0..=max_bus` via bridge-aware discovery + `MCFG`) on the BSP
/// after `init_pmm` into the dynamic `64`-slot registry without hardcoding vendor drivers.
#[inline(never)]
pub fn init_pci_devices() -> bool {
    crate::pci_bar::run_bar64_unit_tests();
    crate::pci_ecam::run_mcfg_unit_tests();
    let max_bus = crate::pci_ecam::mcfg_max_bus();
    let mut bus_mask = [0u64; 4];
    for b in 0..=15u8 {
        bus_mask[(b as usize) >> 6] |= 1u64 << (b & 63);
    }
    let mut scanned_mask = [0u64; 4];
    let mut found = 0usize;
    let mut rec_cnt = 0usize;

    for _pass in 0..8 {
        let mut any_new = false;
        for bus_u in 0..=(max_bus as usize) {
            let w = bus_u >> 6;
            let bit = 1u64 << (bus_u & 63);
            if (bus_mask[w] & bit) == 0 || (scanned_mask[w] & bit) != 0 {
                continue;
            }
            scanned_mask[w] |= bit;
            any_new = true;
            let bus = bus_u as u8;
            for dev in 0..32u8 {
                let id0 = pci_read_u32(bus, dev, 0, 0x00);
                if (id0 & 0xffff) == 0xffff {
                    continue;
                }
                let hdr = pci_read_u8(bus, dev, 0, 0x0e);
                let max_fn = if (hdr & 0x80) != 0 { 8u8 } else { 1u8 };
                for func in 0..max_fn {
                    let id = pci_read_u32(bus, dev, func, 0x00);
                    let vid = (id & 0xffff) as u16;
                    if vid == 0xffff {
                        continue;
                    }
                    let did = (id >> 16) as u16;
                    let class_rev = pci_read_u32(bus, dev, func, 0x08);
                    let fn_hdr = pci_read_u8(bus, dev, func, 0x0e) & 0x7f;
                    let class_code = (class_rev >> 24) as u8;
                    let subclass = ((class_rev >> 16) & 0xff) as u8;
                    if fn_hdr == 1 && class_code == 0x06 && subclass == 0x04 {
                        let bus_info = pci_read_u32(bus, dev, func, 0x18);
                        let sec = ((bus_info >> 8) & 0xff) as u8;
                        let sub = ((bus_info >> 16) & 0xff) as u8;
                        if sec > bus && sec <= max_bus {
                            for b2 in sec..=sub.min(max_bus) {
                                bus_mask[(b2 as usize) >> 6] |= 1u64 << (b2 & 63);
                            }
                        }
                    }
                    let subsys = if fn_hdr == 0 {
                        pci_read_u32(bus, dev, func, 0x2c)
                    } else {
                        0
                    };
                    let irq_pin = pci_read_u8(bus, dev, func, 0x3d);
                    let bdf =
                        ((bus as u16) << 8) | (((dev & 0x1f) as u16) << 3) | ((func & 0x07) as u16);

                    let mut rec = BootPciFuncRecord {
                        bus_dev_fn: bdf,
                        vendor_id: vid,
                        device_id: did,
                        subsys_vendor_id: (subsys & 0xffff) as u16,
                        subsys_id: (subsys >> 16) as u16,
                        revision_id: (class_rev & 0xff) as u8,
                        prog_if: ((class_rev >> 8) & 0xff) as u8,
                        subclass,
                        class_code,
                        managed_slot: 0xff,
                        dev_flags: 0,
                        bar_flags: [0; 6],
                        irq_pin,
                        _pad: 0,
                        bar0_size_bytes: 0,
                    };
                    if fn_hdr == 0 {
                        for (bi, bflag) in rec.bar_flags.iter_mut().enumerate() {
                            let raw = pci_read_u32(bus, dev, func, (0x10 + bi * 4) as u8);
                            if raw != 0 && (raw & 1) != 0 {
                                *bflag = PCI_BAR_FLAG_IO as u8;
                            } else if (raw & !0xf) != 0 {
                                let mut f = PCI_BAR_FLAG_MMIO as u8;
                                if (raw & 0x6) == 0x4 {
                                    f |= PCI_BAR_FLAG_64BIT as u8;
                                }
                                if (raw & 0x8) != 0 {
                                    f |= PCI_BAR_FLAG_PREFETCHABLE as u8;
                                }
                                *bflag = f;
                            }
                        }
                    }
                    record_topology_hints(&rec);

                    if let Some((pref_slot, vdev)) = probe_pci_function(bus, dev, func) {
                        let chosen = match pref_slot {
                            Some(s)
                                if s < MAX_MANAGED_PCI_DEVS
                                    && !PCI_DEVS[s].present.load(Ordering::Relaxed) =>
                            {
                                Some(s)
                            }
                            _ => (PCI_LEGACY_FIXED_SLOTS..MAX_MANAGED_PCI_DEVS)
                                .find(|&s| !PCI_DEVS[s].present.load(Ordering::Relaxed)),
                        };
                        if let Some(slot_idx) = chosen {
                            rec.managed_slot = slot_idx as u8;
                            rec.bar0_size_bytes = vdev.bars[0].size_bytes;
                            for (bi, b) in vdev.bars.iter().enumerate() {
                                rec.bar_flags[bi] = encode_bar_desc_flags(*b);
                            }
                            if vdev.is_virtio {
                                rec.dev_flags |= PCI_DEV_FLAG_VIRTIO as u8;
                            }
                            if vdev.msi_cap_off != 0 {
                                rec.dev_flags |= PCI_DEV_FLAG_MSI as u8;
                            }
                            if vdev.msix_cap_off != 0 {
                                rec.dev_flags |= PCI_DEV_FLAG_MSIX as u8;
                            }
                            if vdev.ext_caps.pcie_cap_off != 0 {
                                rec.dev_flags |= PCI_DEV_FLAG_PCIE as u8;
                            }
                            if vdev.ext_caps.ecam_phys != 0 {
                                rec.dev_flags |= PCI_DEV_FLAG_ECAM as u8;
                            }
                            if vdev.ext_caps.has_flr {
                                rec.dev_flags |= PCI_DEV_FLAG_FLR as u8;
                            }
                            if vdev.ext_caps.rebar_cap_off != 0 {
                                rec.dev_flags |= PCI_DEV_FLAG_REBAR as u8;
                            }
                            // SAFETY: Single-threaded BSP init before `present = true`.
                            unsafe {
                                *PCI_DEVS[slot_idx].dev_inner.get() = vdev;
                            }
                            PCI_DEVS[slot_idx].present.store(true, Ordering::Release);
                            found += 1;
                            let label = slot_label(slot_idx);
                            let bar64_cnt = vdev
                                .bars
                                .iter()
                                .filter(|b| b.kind == crate::pci_bar::PciBarKind::Mem64)
                                .count();
                            let above_4g = vdev.bars.iter().any(|b| b.is_mmio() && b.above_4g);
                            serial_println!(
                                "[DRY-PCI] PASS: {} {:02x}:{:02x}.{} id={:#06x}:{:#06x} bar={} phys={:#x} pages={} common={:#x} notify={:#x}(x{}) dev={:#x} bar64={} above4g={} msi={:#x} msix={:#x}({}) slot={} ecam={} pcie={:#x} flr={} rebar={:#x}",
                                label,
                                vdev.bus,
                                vdev.dev,
                                vdev.func,
                                vdev.info.vendor_id,
                                vdev.info.device_id,
                                vdev.bar_idx,
                                vdev.phys_base,
                                vdev.num_pages,
                                vdev.info.common_off,
                                vdev.info.notify_off,
                                vdev.info.notify_mult,
                                vdev.info.device_off,
                                bar64_cnt,
                                above_4g as u8,
                                vdev.msi_cap_off,
                                vdev.msix_cap_off,
                                vdev.msix_table_len,
                                slot_idx,
                                (vdev.ext_caps.ecam_phys != 0) as u8,
                                vdev.ext_caps.pcie_cap_off,
                                vdev.ext_caps.has_flr as u8,
                                vdev.ext_caps.rebar_cap_off,
                            );
                        }
                    }
                    push_boot_pci_record(rec_cnt, rec);
                    rec_cnt += 1;
                }
            }
        }
        if !any_new {
            break;
        }
    }
    publish_boot_pci_count(rec_cnt);
    let (dgpu_pk, vmd_pk, lpss_cnt) = pci_topology_summary();
    serial_println!(
        "[DRY-PCI-TOPO] funcs={} managed={} igpu_gop={} dgpu_3d_passive={} vmd_passive={} lpss_i2c={} nvme={} xhci={}",
        rec_cnt,
        found,
        active_gop_bdf().is_some() as u8,
        (dgpu_pk != 0) as u8,
        (vmd_pk != 0) as u8,
        lpss_cnt,
        PCI_DEVS[PCI_OBJ_NVME].present.load(Ordering::Acquire) as u8,
        PCI_DEVS[PCI_OBJ_XHCI].present.load(Ordering::Acquire) as u8,
    );
    if found == 0 {
        serial_println!("[DRY-PCI] No managed PCI controllers attached");
    }
    found > 0
}

/// Mint a root `CapObjectKind::PciDev` capability (`dev_idx`: Blk/RNG/GPU) exclusively into `RootSystem`.
pub fn mint_root_pci_dev_cap(
    cspace_id: CSpaceId,
    root_subj: SecLabel,
    dev_idx: usize,
) -> Result<CapPtr, CapError> {
    if root_subj.domain != SecDomain::RootSystem
        || root_subj.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        return Err(CapError::DomainViolation);
    }
    if !is_pci_dev_present(dev_idx) {
        return Err(CapError::InvalidHandle);
    }
    let gen = PCI_DEVS[dev_idx].generation.load(Ordering::Acquire);
    insert_root_cap(
        cspace_id,
        CapObjectKind::PciDev,
        dev_idx as u32,
        gen,
        CapRights::PCI_ROOT,
        root_subj,
        0,
        0,
    )
}

/// Mint the root VirtIO-Blk `CapObjectKind::PciDev` capability into `dry-init`'s `RootSystem` `CSpace`.
pub fn mint_root_pci_blk_cap(
    cspace_id: CSpaceId,
    root_subj: SecLabel,
) -> Result<CapPtr, CapError> {
    mint_root_pci_dev_cap(cspace_id, root_subj, PCI_OBJ_VIRTIO_BLK)
}

pub(crate) fn validated_pci_slot(dev_idx: usize) -> Option<ValidatedVirtioPci> {
    if dev_idx >= MAX_MANAGED_PCI_DEVS || !PCI_DEVS[dev_idx].present.load(Ordering::Acquire) {
        return None;
    }
    // SAFETY: Immutable after `PCI_DEVS[dev_idx].present` is published with `Release`.
    Some(unsafe { *PCI_DEVS[dev_idx].dev_inner.get() })
}

#[inline]
pub(super) fn is_iommu_enforcing_for_pci() -> bool {
    is_hw_enforcing()
}

/// Enumerate supervisor-owned MMIO shadow pages (`ECAM`, MSI-X tables, and VirtIO `common_cfg`)
/// to map in `KERNEL_VSPACE` (`PML4[385]`) before CR3 handover.
pub(crate) fn for_each_kernel_shadow_mmio_page(mut f: impl FnMut(u64, u64)) {
    for slot_idx in 0..MAX_MANAGED_PCI_DEVS {
        let Some(vdev) = validated_pci_slot(slot_idx) else {
            continue;
        };
        if vdev.ext_caps.ecam_phys != 0 {
            f(pci_ecam_vaddr(slot_idx), vdev.ext_caps.ecam_phys);
        }
        if vdev.preserve_active_gop {
            continue;
        }
        if vdev.msix_cap_off != 0 {
            let bar = vdev.bars[vdev.msix_table_bir as usize];
            if bar.is_mmio() {
                let start_pa = (bar.base_phys + vdev.msix_table_off as u64) & !((PAGE_SIZE as u64) - 1);
                let span = ((vdev.msix_table_off as usize) & (PAGE_SIZE - 1))
                    + (vdev.msix_table_len as usize) * 16;
                let pages = span.div_ceil(PAGE_SIZE).clamp(1, KERNEL_MSIX_MAX_PAGES);
                let va_base = pci_msix_table_vaddr(slot_idx);
                for p in 0..pages {
                    f(
                        va_base + (p as u64) * (PAGE_SIZE as u64),
                        start_pa + (p as u64) * (PAGE_SIZE as u64),
                    );
                }
            }
        }
        if vdev.is_virtio {
            let c_pa = (vdev.phys_base + vdev.info.common_off as u64) & !((PAGE_SIZE as u64) - 1);
            f(pci_virtio_common_vaddr(slot_idx), c_pa);
        }
    }
}

pub(crate) fn claim_pci_slot(
    dev_idx: usize,
    cspace_id: CSpaceId,
    vdev: &ValidatedVirtioPci,
) -> Result<bool, CapError> {
    if !is_dma_permitted() {
        return Err(CapError::InsufficientRights);
    }
    let slot = &PCI_DEVS[dev_idx];
    let claimed_new = match slot.owner_cspace.compare_exchange(
        0,
        cspace_id,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => true,
        Err(cur) if cur == cspace_id => false,
        Err(_) => return Err(CapError::BusyActiveObject),
    };
    if claimed_new && attach_device_domain(dev_idx, vdev.bus, vdev.dev, vdev.func).is_err() {
        slot.owner_cspace.store(0, Ordering::Release);
        return Err(CapError::InsufficientRights);
    }
    Ok(claimed_new)
}

pub(crate) fn claim_pci_slot_for_irq(
    dev_idx: usize,
    cspace_id: CSpaceId,
    vdev: &ValidatedVirtioPci,
) -> Result<bool, CapError> {
    claim_pci_slot(dev_idx, cspace_id, vdev)
}

pub(crate) fn release_claimed_pci_slot(dev_idx: usize, claimed_new: bool) {
    if claimed_new {
        detach_and_scrub_domain(dev_idx);
        PCI_DEVS[dev_idx].owner_cspace.store(0, Ordering::Release);
    }
}

pub(super) fn enable_pci_bme(vdev: &ValidatedVirtioPci) {
    if vdev.preserve_active_gop || !is_dma_permitted() {
        return;
    }
    let cmd = pci_read_u16(vdev.bus, vdev.dev, vdev.func, 0x04);
    pci_write_u16(vdev.bus, vdev.dev, vdev.func, 0x04, cmd | 0x0406);
}

fn quiesce_slot_for_cspace(dev_idx: usize, cspace_id: CSpaceId) {
    let slot = &PCI_DEVS[dev_idx];
    if cspace_id == 0 || slot.owner_cspace.load(Ordering::Acquire) != cspace_id {
        return;
    }
    let is_active_gop = validated_pci_slot(dev_idx).is_some_and(|v| v.preserve_active_gop);
    if !is_active_gop {
        crate::irq::msi::quiesce_pci_msi_for_dev(dev_idx);
        if let Some(vdev) = validated_pci_slot(dev_idx) {
            if vdev.is_virtio && ((vdev.info.common_off as usize) & 0xfff) <= 0xfec {
                let common_va =
                    pci_virtio_common_vaddr(dev_idx) + ((vdev.info.common_off as u64) & 0xfff);
                // SAFETY: `common_va + 0x14` is `common_cfg.device_status` on the pre-mapped `UC` shadow page (`PML4[385]`).
                unsafe {
                    core::ptr::write_volatile((common_va + 0x14) as *mut u8, 0);
                    for _ in 0..1024 {
                        if core::ptr::read_volatile((common_va + 0x14) as *const u8) == 0 {
                            break;
                        }
                        core::hint::spin_loop();
                    }
                }
            }
            let cmd = pci_read_u16(vdev.bus, vdev.dev, vdev.func, 0x04);
            // Clear Bus Master Enable (`bit 2 = 0`) and keep legacy INTx masked (`bit 10 = 1`).
            pci_write_u16(vdev.bus, vdev.dev, vdev.func, 0x04, (cmd & !0x0004) | 0x0402);
            let _ = pci_read_u16(vdev.bus, vdev.dev, vdev.func, 0x04);
        }
    }
    crate::dma::quiesce_dma_for_slot(dev_idx, cspace_id);
}

/// Hardware-safe PCI DMA teardown (`R-03` & `INV-CAP-08`): clears `PCI Command.BME = 0` + masks legacy
/// INTx *before* releasing `iommu_pin_count` on every `DEVICE_DMA` frame owned by `cspace_id`.
pub fn quiesce_pci_for_cspace(cspace_id: CSpaceId) {
    if cspace_id == 0 {
        return;
    }
    for dev_idx in 0..MAX_MANAGED_PCI_DEVS {
        quiesce_slot_for_cspace(dev_idx, cspace_id);
    }
}

/// Invoked from `ipc::on_cap_slot_revoked` when a `CapObjectKind::PciDev` slot is revoked.
pub fn on_pci_cap_revoked(obj_id: u32, expected_gen: u32, global_slot: u32) {
    if is_pci_cap_live(obj_id, expected_gen) {
        let (cs_id, _) = split_global_slot(global_slot);
        quiesce_slot_for_cspace(obj_id as usize, cs_id);
    }
}

pub(crate) fn record_pci_denied_first_or(dev_idx: Option<usize>) {
    let idx = dev_idx.filter(|&i| i < MAX_MANAGED_PCI_DEVS).unwrap_or(0);
    PCI_DEVS[idx].dma_denied_ops.fetch_add(1, Ordering::Relaxed);
}

/// `SYS_DEV_MAP` (`29`): Map the authorized VirtIO MMIO BAR (`op=0`), query sanitized BAR metadata
/// (`op=1`), map a bounded MMIO window (`op=2`), perform capability-gated PCI/ECAM config R/W
/// (`op=3`), or mint a dynamically enumerated `PciDev` slot capability (`op=5`).
#[inline(never)]
pub fn sys_dev_map(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    vspace_id: VSpaceId,
    pci_cap_raw: u64,
    user_vaddr_or_op: u64,
    arg2: u64,
) -> Result<(isize, u64), SysError> {
    if subj_label.domain != SecDomain::RootSystem
        || subj_label.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        record_pci_denied_first_or(None);
        return Err(SysError::Perm);
    }
    let op = user_vaddr_or_op & DEV_MAP_OP_MASK;
    if user_vaddr_or_op == DEV_MAP_OP_QUERY_BARS {
        return Ok((
            pci_window::sys_dev_query_bars(cspace_id, subj_label, pci_cap_raw, arg2)?,
            0,
        ));
    }
    if op == DEV_MAP_OP_CFG_RW {
        return pci_window::sys_dev_cfg_rw(
            cspace_id,
            subj_label,
            pci_cap_raw,
            user_vaddr_or_op,
            arg2,
        );
    }
    if op == DEV_MAP_OP_ENUM_MINT {
        return pci_window::sys_dev_enum_mint(cspace_id, subj_label, pci_cap_raw, user_vaddr_or_op);
    }
    let Some((vs_gen, _, _)) = inspect_vspace_live(vspace_id) else {
        return Err(SysError::Inval);
    };
    let vs_h = VSpaceHandle {
        id: vspace_id,
        generation: vs_gen,
    };
    if op == DEV_MAP_OP_MAP_WINDOW_2M {
        return Ok((
            pci_window::sys_dev_map_window_2m(
                cspace_id,
                subj_label,
                vs_h,
                pci_cap_raw,
                user_vaddr_or_op & !DEV_MAP_OP_MASK,
                arg2,
            )?,
            0,
        ));
    }
    if op == DEV_MAP_OP_MAP_WINDOW {
        return Ok((
            pci_window::sys_dev_map_window(
                cspace_id,
                subj_label,
                vs_h,
                pci_cap_raw,
                user_vaddr_or_op & !DEV_MAP_OP_MASK,
                arg2,
            )?,
            0,
        ));
    }
    if op != DEV_MAP_OP_LEGACY_VIRTIO {
        return Err(SysError::Inval);
    }

    let user_vaddr = user_vaddr_or_op;
    let user_info_out_ptr = arg2;
    if user_vaddr < (PAGE_SIZE as u64)
        || user_info_out_ptr == 0
        || (user_info_out_ptr & 3) != 0
    {
        return Err(SysError::Inval);
    }

    let req = CapRights(CapRights::READ.0 | CapRights::WRITE.0 | CapRights::MAP.0);
    let now_epoch = current_lease_epoch();
    let ptr = CapPtr(pci_cap_raw);
    let mut mapped_info: Option<(usize, u32, ValidatedVirtioPci)> = None;
    let mut inval_range = false;

    let map_res = with_pinned_cap(cspace_id, subj_label, ptr, req, now_epoch, |res| {
        if res.obj_kind != CapObjectKind::PciDev || !is_pci_cap_live(res.obj_id, res.expected_obj_gen) {
            return Err(CapError::WrongObjectKind);
        }
        let dev_idx = res.obj_id as usize;
        let Some(vdev) = validated_pci_slot(dev_idx) else {
            return Err(CapError::InvalidHandle);
        };
        let mapped_bytes = (vdev.num_pages as u64) * (PAGE_SIZE as u64);
        let Some(end_vaddr) = user_vaddr.checked_add(mapped_bytes) else {
            inval_range = true;
            return Err(CapError::PagingError);
        };
        if end_vaddr > USER_VADDR_LIMIT {
            inval_range = true;
            return Err(CapError::PagingError);
        }

        if vdev.msix_cap_off != 0 && vdev.msix_table_bir == vdev.bar_idx {
            let tbl_bar = vdev.bars[vdev.msix_table_bir as usize];
            if tbl_bar.is_mmio() {
                let tbl_start_pa =
                    (tbl_bar.base_phys + vdev.msix_table_off as u64) & !((PAGE_SIZE as u64) - 1);
                let tbl_end_pa = tbl_bar.base_phys
                    + (vdev.msix_table_off as u64)
                    + (vdev.msix_table_len as u64) * 16;
                let dev_end_pa = vdev.phys_base + (vdev.num_pages as u64) * (PAGE_SIZE as u64);
                if vdev.phys_base < tbl_end_pa && dev_end_pa > tbl_start_pa {
                    return Err(CapError::InsufficientRights);
                }
            }
        }
        let claimed_new = claim_pci_slot(dev_idx, cspace_id, &vdev)?;
        let pages = vdev.num_pages as usize;
        for p in 0..pages {
            let va = user_vaddr + (p as u64) * (PAGE_SIZE as u64);
            let pa = vdev.phys_base + (p as u64) * (PAGE_SIZE as u64);
            if map_mmio_4k(vs_h, va, pa, VmProt::USER_MMIO_UC).is_err() {
                for rb in 0..p {
                    let _ = unmap_4k(vs_h, user_vaddr + (rb as u64) * (PAGE_SIZE as u64));
                }
                release_claimed_pci_slot(dev_idx, claimed_new);
                return Err(CapError::PagingError);
            }
        }
        if let Err(e) = attach_cap_mapping_range_record(
            res.global_slot,
            ptr.slot_gen(),
            vs_h.id,
            vs_h.generation,
            user_vaddr,
            false,
            vdev.num_pages,
        ) {
            for rb in 0..pages {
                let _ = unmap_4k(vs_h, user_vaddr + (rb as u64) * (PAGE_SIZE as u64));
            }
            release_claimed_pci_slot(dev_idx, claimed_new);
            return Err(e);
        }

        enable_pci_bme(&vdev);
        mapped_info = Some((dev_idx, res.global_slot, vdev));
        Ok(())
    });
    if let Err(e) = map_res {
        record_pci_denied_first_or(None);
        return Err(if inval_range {
            SysError::Inval
        } else {
            map_cap_err_to_sys(e)
        });
    }

    let (dev_idx, global_slot, vdev) = mapped_info.ok_or(SysError::NoSys)?;
    // SAFETY: `UserVirtioPciInfo` is `#[repr(C)]` POD (`32` bytes) with `_pad = 0`.
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (&raw const vdev.info).cast::<u8>(),
            size_of::<UserVirtioPciInfo>(),
        )
    };
    if let Err(fault) = copy_to_user(user_info_out_ptr, bytes) {
        let _ = crate::cspace::revoke::detach_matching_cap_mapping_record(
            global_slot,
            vs_h.id,
            user_vaddr,
        );
        quiesce_slot_for_cspace(dev_idx, cspace_id);
        for rb in 0..(vdev.num_pages as usize) {
            let _ = unmap_4k(vs_h, user_vaddr + (rb as u64) * (PAGE_SIZE as u64));
        }
        return Err(fault);
    }
    Ok((vdev.num_pages as isize, 0))
}
