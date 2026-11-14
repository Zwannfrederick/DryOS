// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-Gated PCI VirtIO Discovery, MMIO BAR Mapping & Pinned `DEVICE_DMA` Allocation
//! (`Milestone 2B` — `ADR-004`, `ADR-007` Rule 10, `INV-CAP-08`, `R-03`).
//!
//! Keeps Ring 0 completely free of block/filesystem drivers:
//! 1. Scans PCI Configuration Space (`0xCF8`/`0xCFC`) on the BSP for VirtIO 1.0 Modern Block
//!    controllers (`1AF4:1042` or transitional `1AF4:1001`) and validates all four modern MMIO
//!    structures (`COMMON=1`, `NOTIFY=2`, `ISR=3`, `DEVICE=4`) against `FrameState::ReservedOrMmio`.
//! 2. Exposes `SYS_DEV_MAP` (`CapObjectKind::PciDev`, `VmProt::USER_MMIO_UC`) and `SYS_DMA_ALLOC`
//!    (`SecDomain::RootSystem` non-`DEV_WORKSPACE` `DEVICE_DMA | NO_DELEGATE` pinned 4K frames).
//! 3. Enforces `R-03` & `INV-CAP-08`: active DMA frames carry `iommu_pin_count == 1` (rejecting
//!    premature `cspace_destroy_object` with `-EBUSY`), and driver crash/exit/revoke clears PCI
//!    `Command.BME = 0` in hardware *before* unpinning DMA frames and reclaiming memory.

use core::cell::UnsafeCell;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};

use crate::arch::{inl, irq_restore, irq_save_cli, outl, outw};
use crate::cspace::cap::{CSpaceId, CapError, CapObjectKind, CapPtr};
use crate::cspace::objects::{cspace_alloc_frame, cspace_vspace_map_4k};
use crate::cspace::revoke::attach_cap_mapping_range_record;
use crate::cspace::{insert_root_cap, split_global_slot, with_pinned_cap};
use crate::ipc::map_cap_err_to_sys;
use crate::pmm::{
    adjust_frame_iommu_pin, query_frame_state, FrameHandle, FrameState, PageOrder, PAGE_SIZE,
};
use crate::proc::process_vspace_cap;
use crate::security::{current_lease_epoch, CapRights, SecDomain, SecFlags, SecLabel};
use crate::serial_println;
use crate::uaccess::{copy_to_user, SysError};
use crate::vspace::pte::{VmProt, USER_VADDR_LIMIT};
use crate::vspace::{inspect_vspace_live, map_mmio_4k, unmap_4k, VSpaceHandle, VSpaceId};

const PCI_CONFIG_ADDR: u16 = 0x0cf8;
const PCI_CONFIG_DATA: u16 = 0x0cfc;

const VIRTIO_VENDOR_ID: u16 = 0x1af4;
const VIRTIO_DEV_BLK_LEGACY_OR_TRANS: u16 = 0x1001;
const VIRTIO_DEV_BLK_MODERN: u16 = 0x1042;

const PCI_CAP_ID_VNDR: u8 = 0x09;
const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
const VIRTIO_PCI_CAP_ISR_CFG: u8 = 3;
const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;

const MAX_MMIO_BAR_PAGES: usize = 16;
const MAX_DMA_PINS_PER_DEV: usize = 16;
const PCI_BLK_OBJ_ID: u32 = 0;

/// Sanitized 32-byte VirtIO 1.0 Modern PCI structure offsets copied to CPL=3 by `SYS_DEV_MAP` (`0` phys addrs).
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, Default)]
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

#[derive(Clone, Copy)]
struct ValidatedVirtioPci {
    bus: u8,
    dev: u8,
    func: u8,
    bar_idx: u8,
    phys_base: u64,
    num_pages: u16,
    info: UserVirtioPciInfo,
}

struct PciBlkGlobal {
    present: AtomicBool,
    generation: AtomicU32,
    cfg_lock: AtomicBool,
    dma_lock: AtomicBool,
    owner_cspace: AtomicU16,
    dma_denied_ops: AtomicU64,
    quiesce_count: AtomicU64,
    dev_inner: UnsafeCell<ValidatedVirtioPci>,
    pinned_frames: UnsafeCell<[Option<FrameHandle>; MAX_DMA_PINS_PER_DEV]>,
}

// SAFETY: `dev_inner` is written once on BSP before `present = true`; `pinned_frames` is mutated under `dma_lock` + `CLI`.
unsafe impl Sync for PciBlkGlobal {}

static PCI_BLK: PciBlkGlobal = PciBlkGlobal {
    present: AtomicBool::new(false),
    generation: AtomicU32::new(1),
    cfg_lock: AtomicBool::new(false),
    dma_lock: AtomicBool::new(false),
    owner_cspace: AtomicU16::new(0),
    dma_denied_ops: AtomicU64::new(0),
    quiesce_count: AtomicU64::new(0),
    dev_inner: UnsafeCell::new(ValidatedVirtioPci {
        bus: 0,
        dev: 0,
        func: 0,
        bar_idx: 0,
        phys_base: 0,
        num_pages: 0,
        info: UserVirtioPciInfo {
            common_off: 0,
            notify_off: 0,
            notify_mult: 0,
            isr_off: 0,
            device_off: 0,
            mapped_pages: 0,
            bus_dev_fn: 0,
            vendor_id: 0,
            device_id: 0,
            _pad: 0,
        },
    }),
    pinned_frames: UnsafeCell::new([None; MAX_DMA_PINS_PER_DEV]),
};

#[inline]
fn with_pci_cfg_lock<R>(f: impl FnOnce() -> R) -> R {
    let saved_if = irq_save_cli();
    while PCI_BLK
        .cfg_lock
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    let out = f();
    PCI_BLK.cfg_lock.store(false, Ordering::Release);
    irq_restore(saved_if);
    out
}

#[inline]
fn with_dma_lock<R>(
    f: impl FnOnce(&mut [Option<FrameHandle>; MAX_DMA_PINS_PER_DEV]) -> R,
) -> R {
    let saved_if = irq_save_cli();
    while PCI_BLK
        .dma_lock
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    // SAFETY: Exclusive `dma_lock` + `CLI` held.
    let out = f(unsafe { &mut *PCI_BLK.pinned_frames.get() });
    PCI_BLK.dma_lock.store(false, Ordering::Release);
    irq_restore(saved_if);
    out
}

#[inline]
fn pci_cfg_addr(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    0x8000_0000u32
        | ((bus as u32) << 16)
        | (((dev & 0x1f) as u32) << 11)
        | (((func & 0x07) as u32) << 8)
        | ((offset & 0xfc) as u32)
}

fn pci_read_u32(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    let addr = pci_cfg_addr(bus, dev, func, offset);
    with_pci_cfg_lock(|| {
        // SAFETY: Standard PCI Mechanism #1 Configuration Space ports `0xCF8`/`0xCFC`.
        unsafe {
            outl(PCI_CONFIG_ADDR, addr);
            inl(PCI_CONFIG_DATA)
        }
    })
}

#[inline]
fn pci_read_u16(bus: u8, dev: u8, func: u8, offset: u8) -> u16 {
    let shift = ((offset & 2) * 8) as u32;
    ((pci_read_u32(bus, dev, func, offset) >> shift) & 0xffff) as u16
}

#[inline]
fn pci_read_u8(bus: u8, dev: u8, func: u8, offset: u8) -> u8 {
    let shift = ((offset & 3) * 8) as u32;
    ((pci_read_u32(bus, dev, func, offset) >> shift) & 0xff) as u8
}

fn pci_write_u16(bus: u8, dev: u8, func: u8, offset: u8, val: u16) {
    let addr = pci_cfg_addr(bus, dev, func, offset);
    let data_port = PCI_CONFIG_DATA + ((offset & 2) as u16);
    with_pci_cfg_lock(|| {
        // SAFETY: Aligned 16-bit write within `0xCFC..=0xCFE` under `PCI_CFG_LOCK`.
        unsafe {
            outl(PCI_CONFIG_ADDR, addr);
            outw(data_port, val);
        }
    });
}

#[inline]
pub fn is_pci_blk_present() -> bool {
    PCI_BLK.present.load(Ordering::Acquire)
}

#[inline]
pub fn is_pci_cap_live(obj_id: u32, expected_gen: u32) -> bool {
    obj_id == PCI_BLK_OBJ_ID
        && PCI_BLK.present.load(Ordering::Acquire)
        && PCI_BLK.generation.load(Ordering::Acquire) == expected_gen
}

#[inline]
pub fn pci_dma_telemetry() -> (u64, u64) {
    (
        PCI_BLK.dma_denied_ops.load(Ordering::Acquire),
        PCI_BLK.quiesce_count.load(Ordering::Acquire),
    )
}

fn read_pci_mmio_bar_base(bus: u8, dev: u8, func: u8, bar_idx: u8) -> Option<u64> {
    if bar_idx >= 6 {
        return None;
    }
    let reg = 0x10u8 + bar_idx * 4;
    let lo = pci_read_u32(bus, dev, func, reg);
    if (lo & 1) != 0 {
        return None; // Reject I/O port BARs; require Memory BAR (`bit 0 == 0`).
    }
    let mem_type = (lo >> 1) & 0x3;
    let phys = if mem_type == 2 {
        if bar_idx >= 5 {
            return None;
        }
        let hi = pci_read_u32(bus, dev, func, reg + 4);
        ((hi as u64) << 32) | ((lo & !0xfu32) as u64)
    } else {
        (lo & !0xfu32) as u64
    };
    (phys != 0).then_some(phys)
}

fn probe_virtio_blk_function(bus: u8, dev: u8, func: u8) -> Option<ValidatedVirtioPci> {
    let id = pci_read_u32(bus, dev, func, 0x00);
    let vendor_id = (id & 0xffff) as u16;
    let device_id = (id >> 16) as u16;
    if vendor_id != VIRTIO_VENDOR_ID
        || (device_id != VIRTIO_DEV_BLK_MODERN && device_id != VIRTIO_DEV_BLK_LEGACY_OR_TRANS)
    {
        return None;
    }
    let status = pci_read_u16(bus, dev, func, 0x06);
    if (status & 0x0010) == 0 {
        return None;
    }

    let mut common: Option<(u8, u32, u32)> = None;
    let mut notify: Option<(u8, u32, u32, u32)> = None;
    let mut isr: Option<(u8, u32, u32)> = None;
    let mut device: Option<(u8, u32, u32)> = None;

    let mut cap_ptr = pci_read_u8(bus, dev, func, 0x34) & 0xfc;
    let mut hops = 0usize;
    while cap_ptr >= 0x40 && hops < 48 {
        hops += 1;
        let cap_id = pci_read_u8(bus, dev, func, cap_ptr);
        let next_ptr = pci_read_u8(bus, dev, func, cap_ptr.wrapping_add(1)) & 0xfc;
        if cap_id == PCI_CAP_ID_VNDR {
            let cap_len = pci_read_u8(bus, dev, func, cap_ptr.wrapping_add(2));
            let cfg_type = pci_read_u8(bus, dev, func, cap_ptr.wrapping_add(3));
            let bar_idx = pci_read_u8(bus, dev, func, cap_ptr.wrapping_add(4));
            if cap_len >= 16 && read_pci_mmio_bar_base(bus, dev, func, bar_idx).is_some() {
                let off = pci_read_u32(bus, dev, func, cap_ptr.wrapping_add(8));
                let len = pci_read_u32(bus, dev, func, cap_ptr.wrapping_add(12));
                match cfg_type {
                    VIRTIO_PCI_CAP_COMMON_CFG if common.is_none() => {
                        common = Some((bar_idx, off, len));
                    }
                    VIRTIO_PCI_CAP_NOTIFY_CFG if notify.is_none() && cap_len >= 20 => {
                        let mult = pci_read_u32(bus, dev, func, cap_ptr.wrapping_add(16));
                        notify = Some((bar_idx, off, len, mult));
                    }
                    VIRTIO_PCI_CAP_ISR_CFG if isr.is_none() => {
                        isr = Some((bar_idx, off, len));
                    }
                    VIRTIO_PCI_CAP_DEVICE_CFG if device.is_none() => {
                        device = Some((bar_idx, off, len));
                    }
                    _ => {}
                }
            }
        }
        cap_ptr = next_ptr;
    }

    let (c_bar, c_off, c_len) = common?;
    let (n_bar, n_off, n_len, n_mult) = notify?;
    let (i_bar, i_off, i_len) = isr?;
    let (d_bar, d_off, d_len) = device?;
    if c_bar != n_bar || c_bar != i_bar || c_bar != d_bar {
        return None;
    }
    let phys_base = read_pci_mmio_bar_base(bus, dev, func, c_bar)?;
    if (phys_base & ((PAGE_SIZE as u64) - 1)) != 0 {
        return None;
    }

    let max_end = (c_off as u64 + c_len as u64)
        .max(n_off as u64 + n_len as u64)
        .max(i_off as u64 + i_len as u64)
        .max(d_off as u64 + d_len as u64);
    let pages = max_end.div_ceil(PAGE_SIZE as u64) as usize;
    if pages == 0 || pages > MAX_MMIO_BAR_PAGES {
        return None;
    }
    for p in 0..pages {
        let pa = phys_base + (p as u64) * (PAGE_SIZE as u64);
        if query_frame_state(pa) != FrameState::ReservedOrMmio {
            return None;
        }
    }

    // Keep Bus Master disabled until an authorized Ring 3 driver calls `SYS_DEV_MAP`.
    let cmd = pci_read_u16(bus, dev, func, 0x04);
    pci_write_u16(bus, dev, func, 0x04, (cmd & !0x0004) | 0x0402);

    let bdf = ((bus as u16) << 8) | (((dev & 0x1f) as u16) << 3) | ((func & 0x07) as u16);
    Some(ValidatedVirtioPci {
        bus,
        dev,
        func,
        bar_idx: c_bar,
        phys_base,
        num_pages: pages as u16,
        info: UserVirtioPciInfo {
            common_off: c_off,
            notify_off: n_off,
            notify_mult: n_mult,
            isr_off: i_off,
            device_off: d_off,
            mapped_pages: pages as u16,
            bus_dev_fn: bdf,
            vendor_id,
            device_id,
            _pad: 0,
        },
    })
}

/// Enumerate PCI buses `0..=4` on the BSP after `init_pmm` to discover any attached VirtIO-Blk device.
pub fn init_pci_devices() -> bool {
    for bus in 0..=4u8 {
        for dev in 0..32u8 {
            let id0 = pci_read_u32(bus, dev, 0, 0x00);
            if (id0 & 0xffff) == 0xffff {
                continue;
            }
            let hdr = pci_read_u8(bus, dev, 0, 0x0e);
            let max_fn = if (hdr & 0x80) != 0 { 8u8 } else { 1u8 };
            for func in 0..max_fn {
                if let Some(vdev) = probe_virtio_blk_function(bus, dev, func) {
                    // SAFETY: Single-threaded BSP init before `present = true`.
                    unsafe {
                        *PCI_BLK.dev_inner.get() = vdev;
                    }
                    PCI_BLK.present.store(true, Ordering::Release);
                    serial_println!(
                        "[DRY-PCI] PASS: virtio-blk {:02x}:{:02x}.{} id={:#06x}:{:#06x} bar={} phys={:#x} pages={} common={:#x} notify={:#x}(x{}) dev={:#x}",
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
                    );
                    return true;
                }
            }
        }
    }
    serial_println!("[DRY-PCI] No VirtIO-Blk PCI controller attached; persistent /persist disabled");
    false
}

/// Mint the root `CapObjectKind::PciDev` capability exclusively into `dry-init`'s `RootSystem` `CSpace`.
pub fn mint_root_pci_blk_cap(
    cspace_id: CSpaceId,
    root_subj: SecLabel,
) -> Result<CapPtr, CapError> {
    if root_subj.domain != SecDomain::RootSystem
        || root_subj.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        return Err(CapError::DomainViolation);
    }
    if !is_pci_blk_present() {
        return Err(CapError::InvalidHandle);
    }
    let gen = PCI_BLK.generation.load(Ordering::Acquire);
    insert_root_cap(
        cspace_id,
        CapObjectKind::PciDev,
        PCI_BLK_OBJ_ID,
        gen,
        CapRights::PCI_ROOT,
        root_subj,
        0,
        0,
    )
}

fn validated_pci_blk() -> Option<ValidatedVirtioPci> {
    if !is_pci_blk_present() {
        return None;
    }
    // SAFETY: Immutable after `PCI_BLK.present` is `true`.
    Some(unsafe { *PCI_BLK.dev_inner.get() })
}

/// Hardware-safe PCI DMA teardown (`R-03` & `INV-CAP-08`): clears `PCI Command.BME = 0` + masks legacy
/// INTx *before* releasing `iommu_pin_count` on every `DEVICE_DMA` frame owned by `cspace_id`.
pub fn quiesce_pci_for_cspace(cspace_id: CSpaceId) {
    if cspace_id == 0 || PCI_BLK.owner_cspace.load(Ordering::Acquire) != cspace_id {
        return;
    }
    if let Some(vdev) = validated_pci_blk() {
        let cmd = pci_read_u16(vdev.bus, vdev.dev, vdev.func, 0x04);
        // Clear Bus Master Enable (`bit 2 = 0`) and keep legacy INTx masked (`bit 10 = 1`).
        pci_write_u16(vdev.bus, vdev.dev, vdev.func, 0x04, (cmd & !0x0004) | 0x0402);
    }
    with_dma_lock(|pins| {
        if PCI_BLK.owner_cspace.load(Ordering::Acquire) == cspace_id {
            for slot in pins.iter_mut() {
                if let Some(fh) = slot.take() {
                    let _ = adjust_frame_iommu_pin(fh, -1);
                }
            }
            PCI_BLK.owner_cspace.store(0, Ordering::Release);
            PCI_BLK.quiesce_count.fetch_add(1, Ordering::Relaxed);
        }
    });
}

/// Invoked from `ipc::on_cap_slot_revoked` when a `CapObjectKind::PciDev` slot is revoked.
pub fn on_pci_cap_revoked(obj_id: u32, expected_gen: u32, global_slot: u32) {
    if is_pci_cap_live(obj_id, expected_gen) {
        let (cs_id, _) = split_global_slot(global_slot);
        quiesce_pci_for_cspace(cs_id);
    }
}

/// `SYS_DEV_MAP` (`29`): Map the VirtIO-Blk MMIO BAR (`VmProt::USER_MMIO_UC`), enable PCI Bus Master,
/// and copy sanitized `UserVirtioPciInfo` offsets to `user_info_out_ptr`.
pub fn sys_dev_map(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    vspace_id: VSpaceId,
    pci_cap_raw: u64,
    user_vaddr: u64,
    user_info_out_ptr: u64,
) -> Result<isize, SysError> {
    if subj_label.domain != SecDomain::RootSystem
        || subj_label.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        PCI_BLK.dma_denied_ops.fetch_add(1, Ordering::Relaxed);
        return Err(SysError::Perm);
    }
    if (user_vaddr & ((PAGE_SIZE as u64) - 1)) != 0
        || user_vaddr < (PAGE_SIZE as u64)
        || user_info_out_ptr == 0
        || (user_info_out_ptr & 3) != 0
    {
        return Err(SysError::Inval);
    }
    let Some(vdev) = validated_pci_blk() else {
        return Err(SysError::NoSys);
    };
    let mapped_bytes = (vdev.num_pages as u64) * (PAGE_SIZE as u64);
    let Some(end_vaddr) = user_vaddr.checked_add(mapped_bytes) else {
        return Err(SysError::Inval);
    };
    if end_vaddr > USER_VADDR_LIMIT {
        return Err(SysError::Inval);
    }
    let Some((vs_gen, _, _)) = inspect_vspace_live(vspace_id) else {
        return Err(SysError::Inval);
    };
    let vs_h = VSpaceHandle {
        id: vspace_id,
        generation: vs_gen,
    };

    let req = CapRights(CapRights::READ.0 | CapRights::WRITE.0 | CapRights::MAP.0);
    let now_epoch = current_lease_epoch();
    let ptr = CapPtr(pci_cap_raw);

    with_pinned_cap(cspace_id, subj_label, ptr, req, now_epoch, |res| {
        if res.obj_kind != CapObjectKind::PciDev || !is_pci_cap_live(res.obj_id, res.expected_obj_gen) {
            return Err(CapError::WrongObjectKind);
        }
        let claimed_new = match PCI_BLK.owner_cspace.compare_exchange(
            0,
            cspace_id,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(cur) if cur == cspace_id => false,
            Err(_) => return Err(CapError::BusyActiveObject),
        };

        let pages = vdev.num_pages as usize;
        for p in 0..pages {
            let va = user_vaddr + (p as u64) * (PAGE_SIZE as u64);
            let pa = vdev.phys_base + (p as u64) * (PAGE_SIZE as u64);
            if map_mmio_4k(vs_h, va, pa, VmProt::USER_MMIO_UC).is_err() {
                for rb in 0..p {
                    let _ = unmap_4k(vs_h, user_vaddr + (rb as u64) * (PAGE_SIZE as u64));
                }
                if claimed_new {
                    PCI_BLK.owner_cspace.store(0, Ordering::Release);
                }
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
            if claimed_new {
                PCI_BLK.owner_cspace.store(0, Ordering::Release);
            }
            return Err(e);
        }

        let cmd = pci_read_u16(vdev.bus, vdev.dev, vdev.func, 0x04);
        pci_write_u16(vdev.bus, vdev.dev, vdev.func, 0x04, cmd | 0x0406);
        Ok(())
    })
    .map_err(|e| {
        PCI_BLK.dma_denied_ops.fetch_add(1, Ordering::Relaxed);
        map_cap_err_to_sys(e)
    })?;

    // SAFETY: `UserVirtioPciInfo` is `#[repr(C)]` POD (`32` bytes) with `_pad = 0`.
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (&raw const vdev.info).cast::<u8>(),
            size_of::<UserVirtioPciInfo>(),
        )
    };
    if let Err(fault) = copy_to_user(user_info_out_ptr, bytes) {
        quiesce_pci_for_cspace(cspace_id);
        for rb in 0..(vdev.num_pages as usize) {
            let _ = unmap_4k(vs_h, user_vaddr + (rb as u64) * (PAGE_SIZE as u64));
        }
        return Err(fault);
    }
    Ok(vdev.num_pages as isize)
}

/// `SYS_DMA_ALLOC` (`30`): Allocate a pinned 4 KiB `DEVICE_DMA | NO_DELEGATE` frame for an authorized
/// `RootSystem` driver holding `pci_cap`, map it at `user_vaddr` (`USER_RW`), write physical DMA
/// address to `paddr_out_ptr`, and return `(0, frame_cap.0)`.
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
        PCI_BLK.dma_denied_ops.fetch_add(1, Ordering::Relaxed);
        return Err(SysError::Perm);
    }
    if (user_vaddr & ((PAGE_SIZE as u64) - 1)) != 0
        || user_vaddr < (PAGE_SIZE as u64)
        || paddr_out_ptr == 0
        || (paddr_out_ptr & 7) != 0
    {
        return Err(SysError::Inval);
    }
    let vs_cap = process_vspace_cap(pid).ok_or(SysError::Perm)?;
    let req = CapRights(CapRights::READ.0 | CapRights::WRITE.0 | CapRights::MAP.0);
    let now_epoch = current_lease_epoch();

    with_pinned_cap(cspace_id, subj_label, CapPtr(pci_cap_raw), req, now_epoch, |res| {
        if res.obj_kind != CapObjectKind::PciDev || !is_pci_cap_live(res.obj_id, res.expected_obj_gen) {
            return Err(CapError::WrongObjectKind);
        }
        Ok(())
    })
    .map_err(|e| {
        PCI_BLK.dma_denied_ops.fetch_add(1, Ordering::Relaxed);
        map_cap_err_to_sys(e)
    })?;

    let dma_label = SecLabel::new(
        SecDomain::RootSystem,
        SecFlags(SecFlags::DEVICE_DMA.0 | SecFlags::NO_DELEGATE.0),
        subj_label.compartment_id,
        subj_label.policy_tag,
    );
    let (frame_cap, fh) = cspace_alloc_frame(
        cspace_id,
        subj_label,
        dma_label,
        PageOrder::Order4KiB,
        false,
        0,
    )
    .map_err(map_cap_err_to_sys)?;

    if let Err(e) = cspace_vspace_map_4k(
        cspace_id,
        subj_label,
        vs_cap,
        frame_cap,
        user_vaddr,
        VmProt::USER_RW,
        0,
    ) {
        let _ = crate::cspace::revoke::cspace_destroy_object(cspace_id, subj_label, frame_cap, 0);
        return Err(map_cap_err_to_sys(e));
    }

    let pinned_ok = with_dma_lock(|pins| {
        let claimed_new = match PCI_BLK.owner_cspace.compare_exchange(
            0,
            cspace_id,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(cur) if cur == cspace_id => false,
            Err(_) => return Err(SysError::Busy),
        };
        let Some(free_slot) = pins.iter_mut().find(|s| s.is_none()) else {
            if claimed_new {
                PCI_BLK.owner_cspace.store(0, Ordering::Release);
            }
            return Err(SysError::NoMem);
        };
        if adjust_frame_iommu_pin(fh, 1).is_err() {
            if claimed_new {
                PCI_BLK.owner_cspace.store(0, Ordering::Release);
            }
            return Err(SysError::NoMem);
        }
        *free_slot = Some(fh);
        Ok(())
    });
    if let Err(e) = pinned_ok {
        let _ = crate::cspace::objects::cspace_vspace_unmap_4k(
            cspace_id, subj_label, vs_cap, frame_cap, user_vaddr, 0,
        );
        let _ = crate::cspace::revoke::cspace_destroy_object(cspace_id, subj_label, frame_cap, 0);
        return Err(e);
    }

    let dma_paddr: u64 = fh.phys_addr();
    let pa_bytes = dma_paddr.to_le_bytes();
    if let Err(fault) = copy_to_user(paddr_out_ptr, &pa_bytes) {
        with_dma_lock(|pins| {
            for slot in pins.iter_mut() {
                if *slot == Some(fh) {
                    *slot = None;
                    let _ = adjust_frame_iommu_pin(fh, -1);
                    break;
                }
            }
        });
        let _ = crate::cspace::objects::cspace_vspace_unmap_4k(
            cspace_id, subj_label, vs_cap, frame_cap, user_vaddr, 0,
        );
        let _ = crate::cspace::revoke::cspace_destroy_object(cspace_id, subj_label, frame_cap, 0);
        return Err(fault);
    }

    Ok((0, frame_cap.0))
}
