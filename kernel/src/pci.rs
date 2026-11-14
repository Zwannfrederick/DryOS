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
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};

use crate::arch::{irq_restore, irq_save_cli};
use crate::cspace::cap::{CSpaceId, CapError, CapObjectKind, CapPtr};
use crate::cspace::objects::{
    cspace_alloc_frame, cspace_vspace_map_2m, cspace_vspace_map_4k, cspace_vspace_unmap_2m,
    cspace_vspace_unmap_4k,
};
use crate::cspace::revoke::{attach_cap_mapping_range_record, cspace_destroy_object};
use crate::cspace::{insert_root_cap, split_global_slot, with_pinned_cap};
use crate::iommu::{attach_device_domain, detach_and_scrub_domain, map_dma_range, unmap_dma_range, VtdStatus};
use crate::ipc::map_cap_err_to_sys;
use crate::pci_bar::{
    DEV_MAP_OP_LEGACY_VIRTIO, DEV_MAP_OP_MAP_WINDOW, DEV_MAP_OP_MASK, DEV_MAP_OP_QUERY_BARS,
};
use crate::pmm::{
    adjust_frame_iommu_pin, FrameHandle, PageOrder, HUGE_2M_FRAMES, PAGE_SIZE,
};
use crate::proc::process_vspace_cap;
use crate::security::{current_lease_epoch, CapRights, SecDomain, SecFlags, SecLabel};
use crate::serial_println;
use crate::uaccess::{copy_to_user, SysError};
use crate::vspace::pte::{
    pci_msix_table_vaddr, pci_virtio_common_vaddr, VmProt, KERNEL_MSIX_MAX_PAGES, USER_VADDR_LIMIT,
};
use crate::vspace::{inspect_vspace_live, map_mmio_4k, unmap_4k, VSpaceHandle, VSpaceId};

#[path = "pci_cfg.rs"]
mod pci_cfg;
#[path = "pci_window.rs"]
mod pci_window;

pub use pci_cfg::{
    UserVirtioPciInfo, PCI_OBJ_IOMMU_TEST, PCI_OBJ_IVSHMEM, PCI_OBJ_QEMU_EDU,
    PCI_OBJ_VIRTIO_BLK, PCI_OBJ_VIRTIO_GPU, PCI_OBJ_VIRTIO_RNG,
};
pub(crate) use pci_cfg::{
    pci_read_u16, pci_read_u32, pci_read_u8, pci_write_u16, pci_write_u32, probe_all_bars_raw,
    probe_pci_function, ValidatedVirtioPci, PCI_DEV_COUNT,
};

#[inline]
pub fn pci_cfg_probe_all_bars_raw(bus: u8, dev: u8, func: u8) -> ([u32; 6], [u32; 6]) {
    probe_all_bars_raw(bus, dev, func)
}

const MAX_DMA_PINS_PER_DEV: usize = 16;
const DMA_ALLOC_FLAG_2M: u64 = 1;

struct PciVirtioSlot {
    present: AtomicBool,
    generation: AtomicU32,
    dma_lock: AtomicBool,
    owner_cspace: AtomicU16,
    dma_denied_ops: AtomicU64,
    quiesce_count: AtomicU64,
    dev_inner: UnsafeCell<ValidatedVirtioPci>,
    pinned_frames: UnsafeCell<[Option<FrameHandle>; MAX_DMA_PINS_PER_DEV]>,
}

impl PciVirtioSlot {
    const fn new() -> Self {
        Self {
            present: AtomicBool::new(false),
            generation: AtomicU32::new(1),
            dma_lock: AtomicBool::new(false),
            owner_cspace: AtomicU16::new(0),
            dma_denied_ops: AtomicU64::new(0),
            quiesce_count: AtomicU64::new(0),
            dev_inner: UnsafeCell::new(ValidatedVirtioPci::EMPTY),
            pinned_frames: UnsafeCell::new([None; MAX_DMA_PINS_PER_DEV]),
        }
    }
}

// SAFETY: `dev_inner` is written once on BSP before `present = true`; `pinned_frames` is mutated under `dma_lock` + `CLI`.
unsafe impl Sync for PciVirtioSlot {}

static PCI_DEVS: [PciVirtioSlot; PCI_DEV_COUNT] = [const { PciVirtioSlot::new() }; PCI_DEV_COUNT];

#[inline]
fn with_dma_lock<R>(
    dev_idx: usize,
    f: impl FnOnce(&PciVirtioSlot, &mut [Option<FrameHandle>; MAX_DMA_PINS_PER_DEV]) -> R,
) -> R {
    let slot = &PCI_DEVS[dev_idx];
    let saved_if = irq_save_cli();
    while slot
        .dma_lock
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    // SAFETY: Exclusive `dma_lock` + `CLI` held.
    let out = f(slot, unsafe { &mut *slot.pinned_frames.get() });
    slot.dma_lock.store(false, Ordering::Release);
    irq_restore(saved_if);
    out
}

#[inline]
pub fn is_pci_dev_present(dev_idx: usize) -> bool {
    dev_idx < PCI_DEV_COUNT && PCI_DEVS[dev_idx].present.load(Ordering::Acquire)
}

#[inline]
pub fn is_pci_blk_present() -> bool {
    is_pci_dev_present(PCI_OBJ_VIRTIO_BLK)
}

#[inline]
pub fn is_pci_cap_live(obj_id: u32, expected_gen: u32) -> bool {
    let idx = obj_id as usize;
    idx < PCI_DEV_COUNT
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

/// Enumerate PCI buses `0..=4` on the BSP after `init_pmm` to discover VirtIO and test controllers.
#[inline(never)]
pub fn init_pci_devices() -> bool {
    crate::pci_bar::run_bar64_unit_tests();
    let mut found = 0usize;
    for bus in 0..=4u8 {
        for dev in 0..32u8 {
            let id0 = pci_read_u32(bus, dev, 0, 0x00);
            if (id0 & 0xffff) == 0xffff {
                continue;
            }
            let hdr = pci_read_u8(bus, dev, 0, 0x0e);
            let max_fn = if (hdr & 0x80) != 0 { 8u8 } else { 1u8 };
            for func in 0..max_fn {
                let Some((slot_idx, vdev)) = probe_pci_function(bus, dev, func) else {
                    continue;
                };
                if PCI_DEVS[slot_idx].present.load(Ordering::Relaxed) {
                    continue;
                }
                // SAFETY: Single-threaded BSP init before `present = true`.
                unsafe {
                    *PCI_DEVS[slot_idx].dev_inner.get() = vdev;
                }
                PCI_DEVS[slot_idx].present.store(true, Ordering::Release);
                found += 1;
                let label = match slot_idx {
                    PCI_OBJ_VIRTIO_BLK => "virtio-blk",
                    PCI_OBJ_VIRTIO_RNG => "virtio-rng",
                    PCI_OBJ_VIRTIO_GPU => "virtio-gpu",
                    PCI_OBJ_QEMU_EDU => "qemu-edu",
                    PCI_OBJ_IVSHMEM => "ivshmem",
                    PCI_OBJ_IOMMU_TEST => "iommu-testdev",
                    _ => "pci",
                };
                let bar64_cnt = vdev
                    .bars
                    .iter()
                    .filter(|b| b.kind == crate::pci_bar::PciBarKind::Mem64)
                    .count();
                let above_4g = vdev.bars.iter().any(|b| b.is_mmio() && b.above_4g);
                serial_println!(
                    "[DRY-PCI] PASS: {} {:02x}:{:02x}.{} id={:#06x}:{:#06x} bar={} phys={:#x} pages={} common={:#x} notify={:#x}(x{}) dev={:#x} bar64={} above4g={} msi={:#x} msix={:#x}({})",
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
                );
            }
        }
    }
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
    if dev_idx >= PCI_DEV_COUNT || !PCI_DEVS[dev_idx].present.load(Ordering::Acquire) {
        return None;
    }
    // SAFETY: Immutable after `PCI_DEVS[dev_idx].present` is published with `Release`.
    Some(unsafe { *PCI_DEVS[dev_idx].dev_inner.get() })
}

#[inline]
pub(super) fn is_iommu_enforcing_for_pci() -> bool {
    crate::iommu::status() == VtdStatus::Enforcing
}

/// Enumerate supervisor-owned MMIO shadow pages (MSI-X tables and VirtIO `common_cfg`) to map in
/// `KERNEL_VSPACE` (`PML4[385]`) before CR3 handover.
pub(crate) fn for_each_kernel_shadow_mmio_page(mut f: impl FnMut(u64, u64)) {
    for slot_idx in 0..PCI_DEV_COUNT {
        let Some(vdev) = validated_pci_slot(slot_idx) else {
            continue;
        };
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

pub(super) fn claim_pci_slot(
    dev_idx: usize,
    cspace_id: CSpaceId,
    vdev: &ValidatedVirtioPci,
) -> Result<bool, CapError> {
    if matches!(
        crate::iommu::status(),
        VtdStatus::Uninitialized | VtdStatus::FailedClosed
    ) {
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
) -> Result<(), CapError> {
    claim_pci_slot(dev_idx, cspace_id, vdev).map(|_| ())
}

pub(super) fn release_claimed_pci_slot(dev_idx: usize, claimed_new: bool) {
    if claimed_new {
        detach_and_scrub_domain(dev_idx);
        PCI_DEVS[dev_idx].owner_cspace.store(0, Ordering::Release);
    }
}

pub(super) fn enable_pci_bme(vdev: &ValidatedVirtioPci) {
    let cmd = pci_read_u16(vdev.bus, vdev.dev, vdev.func, 0x04);
    pci_write_u16(vdev.bus, vdev.dev, vdev.func, 0x04, cmd | 0x0406);
}

fn quiesce_slot_for_cspace(dev_idx: usize, cspace_id: CSpaceId) {
    let slot = &PCI_DEVS[dev_idx];
    if cspace_id == 0 || slot.owner_cspace.load(Ordering::Acquire) != cspace_id {
        return;
    }
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
    with_dma_lock(dev_idx, |slot, pins| {
        if slot.owner_cspace.load(Ordering::Acquire) == cspace_id {
            detach_and_scrub_domain(dev_idx);
            for pin in pins.iter_mut() {
                if let Some(fh) = pin.take() {
                    let _ = adjust_frame_iommu_pin(fh, -1);
                }
            }
            slot.owner_cspace.store(0, Ordering::Release);
            slot.quiesce_count.fetch_add(1, Ordering::Relaxed);
        }
    });
}

/// Hardware-safe PCI DMA teardown (`R-03` & `INV-CAP-08`): clears `PCI Command.BME = 0` + masks legacy
/// INTx *before* releasing `iommu_pin_count` on every `DEVICE_DMA` frame owned by `cspace_id`.
pub fn quiesce_pci_for_cspace(cspace_id: CSpaceId) {
    if cspace_id == 0 {
        return;
    }
    for dev_idx in 0..PCI_DEV_COUNT {
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

fn record_pci_denied_first_or(dev_idx: Option<usize>) {
    let idx = dev_idx.filter(|&i| i < PCI_DEV_COUNT).unwrap_or(0);
    PCI_DEVS[idx].dma_denied_ops.fetch_add(1, Ordering::Relaxed);
}

fn unmap_and_destroy_dma_frame(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    vs_cap: CapPtr,
    frame_cap: CapPtr,
    user_vaddr: u64,
    is_2m: bool,
) {
    if is_2m {
        let _ = cspace_vspace_unmap_2m(cspace_id, subj_label, vs_cap, frame_cap, user_vaddr, 0);
    } else {
        let _ = cspace_vspace_unmap_4k(cspace_id, subj_label, vs_cap, frame_cap, user_vaddr, 0);
    }
    let _ = cspace_destroy_object(cspace_id, subj_label, frame_cap, 0);
}

/// `SYS_DEV_MAP` (`29`): Map the authorized VirtIO MMIO BAR (`op=0`), query sanitized BAR metadata
/// (`op=1`), or map a bounded MMIO window at `page_offset` (`op=2`, `UC` or prefetchable `WC`).
#[inline(never)]
pub fn sys_dev_map(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    vspace_id: VSpaceId,
    pci_cap_raw: u64,
    user_vaddr_or_op: u64,
    arg2: u64,
) -> Result<isize, SysError> {
    if subj_label.domain != SecDomain::RootSystem
        || subj_label.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        record_pci_denied_first_or(None);
        return Err(SysError::Perm);
    }
    let op = user_vaddr_or_op & DEV_MAP_OP_MASK;
    if user_vaddr_or_op == DEV_MAP_OP_QUERY_BARS {
        return pci_window::sys_dev_query_bars(cspace_id, subj_label, pci_cap_raw, arg2);
    }
    let Some((vs_gen, _, _)) = inspect_vspace_live(vspace_id) else {
        return Err(SysError::Inval);
    };
    let vs_h = VSpaceHandle {
        id: vspace_id,
        generation: vs_gen,
    };
    if op == DEV_MAP_OP_MAP_WINDOW {
        return pci_window::sys_dev_map_window(
            cspace_id,
            subj_label,
            vs_h,
            pci_cap_raw,
            user_vaddr_or_op & !DEV_MAP_OP_MASK,
            arg2,
        );
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
    let mut mapped_info: Option<(usize, ValidatedVirtioPci)> = None;
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
        mapped_info = Some((dev_idx, vdev));
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

    let (dev_idx, vdev) = mapped_info.ok_or(SysError::NoSys)?;
    // SAFETY: `UserVirtioPciInfo` is `#[repr(C)]` POD (`32` bytes) with `_pad = 0`.
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (&raw const vdev.info).cast::<u8>(),
            size_of::<UserVirtioPciInfo>(),
        )
    };
    if let Err(fault) = copy_to_user(user_info_out_ptr, bytes) {
        quiesce_slot_for_cspace(dev_idx, cspace_id);
        for rb in 0..(vdev.num_pages as usize) {
            let _ = unmap_4k(vs_h, user_vaddr + (rb as u64) * (PAGE_SIZE as u64));
        }
        return Err(fault);
    }
    Ok(vdev.num_pages as isize)
}

/// `SYS_DMA_ALLOC` (`30`): Allocate a pinned `4 KiB` or `2 MiB` (`user_vaddr & DMA_ALLOC_FLAG_2M`)
/// `DEVICE_DMA | NO_DELEGATE` frame for an authorized `RootSystem` driver holding `pci_cap`,
/// map it (`USER_RW`), write physical DMA address to `paddr_out_ptr`, and return `(0, frame_cap.0)`.
pub fn sys_dma_alloc(
    pid: u32,
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    pci_cap_raw: u64,
    user_vaddr: u64,
    paddr_out_ptr: u64,
) -> Result<(isize, u64), SysError> {
    if subj_label.domain != SecDomain::RootSystem
        || subj_label.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        record_pci_denied_first_or(None);
        return Err(SysError::Perm);
    }
    let is_2m = (user_vaddr & DMA_ALLOC_FLAG_2M) != 0;
    let map_vaddr = user_vaddr & !DMA_ALLOC_FLAG_2M;
    let align_mask = if is_2m {
        (HUGE_2M_FRAMES as u64 * PAGE_SIZE as u64) - 1
    } else {
        (PAGE_SIZE as u64) - 1
    };
    if (map_vaddr & align_mask) != 0
        || map_vaddr < (PAGE_SIZE as u64)
        || paddr_out_ptr == 0
        || (paddr_out_ptr & 7) != 0
    {
        return Err(SysError::Inval);
    }
    let vs_cap = process_vspace_cap(pid).ok_or(SysError::Perm)?;
    let req = CapRights(CapRights::READ.0 | CapRights::WRITE.0 | CapRights::MAP.0);
    let now_epoch = current_lease_epoch();

    let dev_idx = with_pinned_cap(cspace_id, subj_label, CapPtr(pci_cap_raw), req, now_epoch, |res| {
        if res.obj_kind != CapObjectKind::PciDev || !is_pci_cap_live(res.obj_id, res.expected_obj_gen) {
            return Err(CapError::WrongObjectKind);
        }
        Ok(res.obj_id as usize)
    })
    .map_err(|e| {
        record_pci_denied_first_or(None);
        map_cap_err_to_sys(e)
    })?;

    let dma_label = SecLabel::new(
        SecDomain::RootSystem,
        SecFlags(SecFlags::DEVICE_DMA.0 | SecFlags::NO_DELEGATE.0),
        subj_label.compartment_id,
        subj_label.policy_tag,
    );
    let order = if is_2m {
        PageOrder::Order2MiB
    } else {
        PageOrder::Order4KiB
    };
    let (frame_cap, fh) =
        cspace_alloc_frame(cspace_id, subj_label, dma_label, order, false, 0)
            .map_err(map_cap_err_to_sys)?;

    let map_res = if is_2m {
        cspace_vspace_map_2m(cspace_id, subj_label, vs_cap, frame_cap, map_vaddr, VmProt::USER_RW, 0)
    } else {
        cspace_vspace_map_4k(cspace_id, subj_label, vs_cap, frame_cap, map_vaddr, VmProt::USER_RW, 0)
    };
    if let Err(e) = map_res {
        let _ = cspace_destroy_object(cspace_id, subj_label, frame_cap, 0);
        return Err(map_cap_err_to_sys(e));
    }

    let len_bytes = if is_2m {
        (HUGE_2M_FRAMES as u64) * (PAGE_SIZE as u64)
    } else {
        PAGE_SIZE as u64
    };
    let dma_paddr: u64 = fh.phys_addr();
    let Some(vdev) = validated_pci_slot(dev_idx) else {
        unmap_and_destroy_dma_frame(cspace_id, subj_label, vs_cap, frame_cap, map_vaddr, is_2m);
        return Err(SysError::Inval);
    };

    let mut rollback_claimed = false;
    let pinned_ok = with_dma_lock(dev_idx, |_slot, pins| {
        let claimed_new = claim_pci_slot(dev_idx, cspace_id, &vdev).map_err(map_cap_err_to_sys)?;
        let Some(free_slot) = pins.iter_mut().find(|s| s.is_none()) else {
            release_claimed_pci_slot(dev_idx, claimed_new);
            return Err(SysError::NoMem);
        };
        if adjust_frame_iommu_pin(fh, 1).is_err() {
            release_claimed_pci_slot(dev_idx, claimed_new);
            return Err(SysError::NoMem);
        }
        if map_dma_range(dev_idx, dma_paddr, len_bytes).is_err() {
            let _ = adjust_frame_iommu_pin(fh, -1);
            release_claimed_pci_slot(dev_idx, claimed_new);
            return Err(SysError::NoMem);
        }
        *free_slot = Some(fh);
        rollback_claimed = claimed_new;
        Ok(())
    });
    if let Err(e) = pinned_ok {
        unmap_and_destroy_dma_frame(cspace_id, subj_label, vs_cap, frame_cap, map_vaddr, is_2m);
        return Err(e);
    }

    let pa_bytes = dma_paddr.to_le_bytes();
    if let Err(fault) = copy_to_user(paddr_out_ptr, &pa_bytes) {
        with_dma_lock(dev_idx, |_slot, pins| {
            unmap_dma_range(dev_idx, dma_paddr, len_bytes);
            for pin in pins.iter_mut() {
                if *pin == Some(fh) {
                    *pin = None;
                    let _ = adjust_frame_iommu_pin(fh, -1);
                    break;
                }
            }
            if rollback_claimed && pins.iter().all(|p| p.is_none()) {
                release_claimed_pci_slot(dev_idx, true);
            }
        });
        unmap_and_destroy_dma_frame(cspace_id, subj_label, vs_cap, frame_cap, map_vaddr, is_2m);
        return Err(fault);
    }

    Ok((0, frame_cap.0))
}
