// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Intel `VT-d` DMA Remapping engine (`Milestone 2C-2`).
//!
//! Enforces strict per-PCI-device Second-Level Page Tables (`SLPT`) with synchronous Context/IOTLB
//! invalidations, fault recording inspection (`FRCD`), and full PMM frame reclamation on teardown.

use core::cell::UnsafeCell;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};

use crate::arch::{irq_restore, irq_save_cli};
use crate::iommu::dmar::{
    dmar_snapshot, is_dmar_present_or_malformed, DrhdUnit, ParsedDmar, MAX_DRHD_UNITS,
};
use crate::iommu::ivrs::{is_ivrs_present_or_malformed, ivrs_snapshot};
use crate::iommu::ops::{
    evaluate_iommu_policy, is_dma_allowed_status, is_hw_enforcing_status, IommuBackendKind,
    IommuCaps, IommuStatus,
};
use crate::iommu::slpt::{
    flush_entire_page, free_all_domain_frames, is_dedicated_gop_drhd, premap_unit_rmrrs,
    release_bus_ctx_if_idle, DomainState, VtdUnit, BOOT_RMRR_DID, VTD_KERNEL_LABEL,
};
use crate::pci::{active_gop_bdf, MAX_MANAGED_PCI_DEVS};
use crate::pmm::{alloc_pages, free_pages, AllocFlags, PageOrder, PAGE_SIZE};
use crate::serial_println;
use crate::vspace::pte::{vtd_unit_vaddr, KERNEL_VTD_MAX_PAGES};

const REG_CAP: usize = 0x08;
const REG_ECAP: usize = 0x10;
const REG_GCMD: usize = 0x18;
const REG_GSTS: usize = 0x1c;
const REG_RTADDR: usize = 0x20;
const REG_CCMD: usize = 0x28;
const REG_FSTS: usize = 0x34;
const REG_FECTL: usize = 0x38;

const GCMD_TE: u32 = 1 << 31;
const GCMD_SRTP: u32 = 1 << 30;
const GCMD_WBF: u32 = 1 << 27;
const GSTS_TES: u32 = 1 << 31;
const GSTS_WBFS: u32 = 1 << 27;
const GSTS_PERSISTENT_MASK: u32 = (1 << 31) | (1 << 28) | (1 << 26) | (1 << 25) | (1 << 23);

const CCMD_ICC: u64 = 1 << 63;
const CCMD_CIRG_GLOBAL: u64 = 1 << 61;
pub(super) const CCMD_CIRG_DEVICE: u64 = 3 << 61;

const IOTLB_IVT: u64 = 1 << 63;
const IOTLB_IIRG_GLOBAL: u64 = 1 << 60;
pub(super) const IOTLB_IIRG_DOMAIN: u64 = 2 << 60;
const IOTLB_DR: u64 = 1 << 49;
const IOTLB_DW: u64 = 1 << 48;

const VTD_POLL_SPIN_LIMIT: usize = 1_000_000;

const fn init_device_domains() -> [DomainState; MAX_MANAGED_PCI_DEVS] {
    [const { DomainState::empty(0) }; MAX_MANAGED_PCI_DEVS]
}

pub(super) struct VtdEngineState {
    pub(super) units: [VtdUnit; MAX_DRHD_UNITS],
    pub(super) domains: [DomainState; MAX_MANAGED_PCI_DEVS],
    pub(super) rmrr_domains: [DomainState; MAX_DRHD_UNITS],
}

impl VtdEngineState {
    const fn new() -> Self {
        Self {
            units: [VtdUnit::empty(); MAX_DRHD_UNITS],
            domains: init_device_domains(),
            rmrr_domains: [const { DomainState::empty(BOOT_RMRR_DID) }; MAX_DRHD_UNITS],
        }
    }
}

struct VtdLockBox {
    locked: AtomicBool,
    inner: UnsafeCell<VtdEngineState>,
}
unsafe impl Sync for VtdLockBox {}

static VTD_STATUS: AtomicU8 = AtomicU8::new(IommuStatus::Uninitialized as u8);
static IOMMU_BACKEND: AtomicU8 = AtomicU8::new(IommuBackendKind::None as u8);
static IOMMU_CAPS_BITS: AtomicU8 = AtomicU8::new(0);
static IOMMU_CAPS_FLAGS: AtomicU8 = AtomicU8::new(0);
static VTD_GFX_SCANOUT_DEFERRED: AtomicBool = AtomicBool::new(false);
static VTD_FAULT_COUNT: AtomicU64 = AtomicU64::new(0);
static VTD_ENGINE: VtdLockBox = VtdLockBox {
    locked: AtomicBool::new(false),
    inner: UnsafeCell::new(VtdEngineState::new()),
};

#[inline]
pub fn is_gfx_scanout_deferred() -> bool {
    VTD_GFX_SCANOUT_DEFERRED.load(Ordering::Acquire)
}

pub(super) fn with_vtd_lock<R>(f: impl FnOnce(&mut VtdEngineState) -> R) -> R {
    let saved_if = irq_save_cli();
    while VTD_ENGINE
        .locked
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    // SAFETY: Serialized by `VTD_ENGINE.locked` with `CLI`.
    let res = f(unsafe { &mut *VTD_ENGINE.inner.get() });
    VTD_ENGINE.locked.store(false, Ordering::Release);
    irq_restore(saved_if);
    res
}

#[inline]
pub fn status() -> IommuStatus {
    IommuStatus::from_raw(VTD_STATUS.load(Ordering::Acquire))
}

#[inline]
pub fn is_dma_permitted() -> bool {
    is_dma_allowed_status(status())
}

#[inline]
pub fn is_hw_enforcing() -> bool {
    is_hw_enforcing_status(status())
}

pub fn backend_kind() -> IommuBackendKind {
    match IOMMU_BACKEND.load(Ordering::Acquire) {
        1 => IommuBackendKind::IntelVtd,
        2 => IommuBackendKind::AmdVi,
        3 => IommuBackendKind::QemuTestBypass,
        _ => IommuBackendKind::None,
    }
}

pub fn caps() -> IommuCaps {
    let flags = IOMMU_CAPS_FLAGS.load(Ordering::Acquire);
    IommuCaps {
        backend: backend_kind(),
        max_aw_bits: IOMMU_CAPS_BITS.load(Ordering::Acquire),
        page_4k: (flags & (1 << 0)) != 0,
        page_2m: (flags & (1 << 1)) != 0,
        coherent: (flags & (1 << 2)) != 0,
        int_remap: (flags & (1 << 3)) != 0,
        fault_reporting: (flags & (1 << 4)) != 0,
        reserved_regions: (flags & (1 << 5)) != 0,
    }
}

#[inline]
pub(super) fn latched_fault_count() -> u64 {
    VTD_FAULT_COUNT.load(Ordering::Acquire)
}

#[inline]
unsafe fn read32(base: u64, off: usize) -> u32 {
    unsafe { read_volatile((base as usize + off) as *const u32) }
}

#[inline]
unsafe fn write32(base: u64, off: usize, val: u32) {
    unsafe { write_volatile((base as usize + off) as *mut u32, val) }
}

#[inline]
unsafe fn read64(base: u64, off: usize) -> u64 {
    unsafe { read_volatile((base as usize + off) as *const u64) }
}

#[inline]
unsafe fn write64(base: u64, off: usize, val: u64) {
    unsafe { write_volatile((base as usize + off) as *mut u64, val) }
}

fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    for _ in 0..VTD_POLL_SPIN_LIMIT {
        if cond() {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

fn send_gcmd(unit: &VtdUnit, set_bit: u32, expect_set: bool) -> bool {
    // SAFETY: `unit.mmio_virt` is mapped Supervisor MMIO UC (`0..unit.mmio_bytes`).
    unsafe {
        let gsts = read32(unit.mmio_virt, REG_GSTS);
        let base_cmd = gsts & GSTS_PERSISTENT_MASK;
        let cmd = if expect_set {
            base_cmd | set_bit
        } else {
            base_cmd & !set_bit
        };
        write32(unit.mmio_virt, REG_GCMD, cmd);
        wait_until(|| {
            let st = read32(unit.mmio_virt, REG_GSTS);
            if expect_set {
                (st & set_bit) != 0
            } else {
                (st & set_bit) == 0
            }
        })
    }
}

fn flush_write_buffers(unit: &VtdUnit) -> bool {
    if !unit.need_rwbf {
        return true;
    }
    // SAFETY: `unit.mmio_virt` is valid Supervisor MMIO UC.
    unsafe {
        let gsts = read32(unit.mmio_virt, REG_GSTS);
        write32(unit.mmio_virt, REG_GCMD, (gsts & GSTS_PERSISTENT_MASK) | GCMD_WBF);
        wait_until(|| (read32(unit.mmio_virt, REG_GSTS) & GSTS_WBFS) == 0)
    }
}

pub(super) fn invalidate_context_cache(unit: &VtdUnit, cmd_bits: u64) -> bool {
    if !flush_write_buffers(unit) {
        return false;
    }
    // SAFETY: `unit.mmio_virt` is mapped Supervisor MMIO UC.
    unsafe {
        write64(unit.mmio_virt, REG_CCMD, CCMD_ICC | cmd_bits);
        wait_until(|| (read64(unit.mmio_virt, REG_CCMD) & CCMD_ICC) == 0)
    }
}

pub(super) fn invalidate_iotlb_cache(unit: &VtdUnit, cmd_bits: u64) -> bool {
    // SAFETY: `unit.iotlb_off + 8` verified inside `unit.mmio_bytes` during init.
    unsafe {
        write64(
            unit.mmio_virt,
            unit.iotlb_off,
            IOTLB_IVT | IOTLB_DR | IOTLB_DW | cmd_bits,
        );
        wait_until(|| (read64(unit.mmio_virt, unit.iotlb_off) & IOTLB_IVT) == 0)
    }
}

pub(super) fn maybe_release_bus_ctx(unit: &mut VtdUnit, bus: u8) {
    if let Some(fh) = release_bus_ctx_if_idle(unit, bus) {
        let _ = invalidate_context_cache(unit, CCMD_CIRG_GLOBAL);
        let _ = invalidate_iotlb_cache(unit, IOTLB_IIRG_GLOBAL);
        free_pages(fh, PageOrder::Order4KiB, VTD_KERNEL_LABEL);
    }
}

fn init_single_unit(
    dmar: &ParsedDmar,
    unit_idx: usize,
    unit: &mut VtdUnit,
    rmrr_dom: &mut DomainState,
    mmio_virt: u64,
    src: &DrhdUnit,
) -> bool {
    let pages = src.mapped_pages;
    let mmio_bytes = (pages as usize) * PAGE_SIZE;
    if mmio_bytes < PAGE_SIZE || pages as usize > KERNEL_VTD_MAX_PAGES {
        return false;
    }

    let gop_bdf = active_gop_bdf();
    if dmar.drhd_count > 1 && is_dedicated_gop_drhd(src, gop_bdf) {
        let (gb, gd, gf) = gop_bdf.unwrap_or((0, 0, 0));
        unit.active = true;
        unit.translation_enabled = false;
        unit.gop_scanout_deferred = true;
        unit.mmio_virt = mmio_virt;
        unit.mmio_bytes = mmio_bytes;
        unit.segment = src.segment;
        unit.include_all = src.include_all;
        VTD_GFX_SCANOUT_DEFERRED.store(true, Ordering::Release);
        serial_println!(
            "[DRY-IOMMU] GFX_SCANOUT_DEFERRED unit={} bdf={:02x}:{:02x}.{} (active GOP protected)",
            unit_idx,
            gb,
            gd,
            gf
        );
        return true;
    }

    // SAFETY: `mmio_virt..mmio_virt + mmio_bytes` mapped in `init_and_activate_kernel_vspace`.
    let (cap, ecap) = unsafe { (read64(mmio_virt, REG_CAP), read64(mmio_virt, REG_ECAP)) };

    let sagaw = ((cap >> 8) & 0x1f) as u8;
    let levels_4 = if (sagaw & 0x04) != 0 {
        true
    } else if (sagaw & 0x02) != 0 {
        false
    } else {
        serial_println!("[DRY-IOMMU] INIT_FAIL: unsupported SAGAW={:#x}", sagaw);
        return false;
    };

    let nd = 1usize << (4 + (((cap & 0x7) as usize) * 2).min(12));
    if nd <= (BOOT_RMRR_DID as usize) + 1 {
        serial_println!("[DRY-IOMMU] INIT_FAIL: domain count too small nd={}", nd);
        return false;
    }

    let nfr = (((cap >> 40) & 0xff) as usize) + 1;
    let fro_off = (((cap >> 24) & 0x3ff) as usize) * 16;
    let iro_off = (((ecap >> 8) & 0x3ff) as usize) * 16;
    let iotlb_off = iro_off + 8;
    if fro_off + nfr * 16 > mmio_bytes || iotlb_off + 8 > mmio_bytes {
        serial_println!(
            "[DRY-IOMMU] INIT_FAIL: FRO/IRO outside mapped MMIO fro={:#x} iro={:#x} bytes={:#x}",
            fro_off,
            iro_off,
            mmio_bytes
        );
        return false;
    }

    let Ok(root_h) = alloc_pages(PageOrder::Order4KiB, VTD_KERNEL_LABEL, AllocFlags::ZERO) else {
        return false;
    };
    let coherent = (ecap & 1) != 0;
    flush_entire_page(coherent, root_h.hhdm_ptr());

    unit.active = true;
    unit.translation_enabled = false;
    unit.gop_scanout_deferred = false;
    unit.mmio_virt = mmio_virt;
    unit.mmio_bytes = mmio_bytes;
    unit.segment = src.segment;
    unit.include_all = src.include_all;
    unit.nfr = nfr;
    unit.fro_off = fro_off;
    unit.iotlb_off = iotlb_off;
    unit.levels_4 = levels_4;
    unit.coherent = coherent;
    unit.caching_mode = ((cap >> 7) & 1) != 0;
    unit.need_rwbf = ((cap >> 4) & 1) != 0;
    unit.max_domains = nd;
    unit.root_frame = Some(root_h);

    // SAFETY: Disable translation cleanly if firmware left it on, mask fault IRQ, pre-map RMRRs, set RTADDR.
    unsafe {
        let gsts = read32(mmio_virt, REG_GSTS);
        if (gsts & GSTS_TES) != 0 && !send_gcmd(unit, GCMD_TE, false) {
            return false;
        }
        if !premap_unit_rmrrs(dmar, unit_idx, unit, rmrr_dom) {
            return false;
        }
        write32(mmio_virt, REG_FECTL, 1 << 31);
        write64(mmio_virt, REG_RTADDR, root_h.phys_addr());
        let _ = read64(mmio_virt, REG_RTADDR);
    }

    if !send_gcmd(unit, GCMD_SRTP, true)
        || !invalidate_context_cache(unit, CCMD_CIRG_GLOBAL)
        || !invalidate_iotlb_cache(unit, IOTLB_IIRG_GLOBAL)
        || !send_gcmd(unit, GCMD_TE, true)
    {
        return false;
    }

    unit.translation_enabled = true;
    poll_and_clear_unit_faults(unit);
    true
}

fn rollback_all_units(eng: &mut VtdEngineState, count: usize) {
    for idx in 0..count.min(MAX_DRHD_UNITS) {
        if eng.units[idx].active && !eng.units[idx].gop_scanout_deferred {
            // SAFETY: Disable translation if active before tearing down root/context frames.
            let te_on = unsafe { (read32(eng.units[idx].mmio_virt, REG_GSTS) & GSTS_TES) != 0 };
            if te_on {
                let _ = send_gcmd(&eng.units[idx], GCMD_TE, false);
            }
            for b in eng.units[idx].ctx_buses.iter_mut() {
                if let Some(fh) = b.frame.take() {
                    free_pages(fh, PageOrder::Order4KiB, VTD_KERNEL_LABEL);
                }
                *b = crate::iommu::slpt::BusContextSlot::empty();
            }
            free_all_domain_frames(&mut eng.rmrr_domains[idx]);
            if let Some(rf) = eng.units[idx].root_frame.take() {
                free_pages(rf, PageOrder::Order4KiB, VTD_KERNEL_LABEL);
            }
        }
        eng.units[idx] = VtdUnit::empty();
    }
    VTD_GFX_SCANOUT_DEFERRED.store(false, Ordering::Release);
}

/// Initialize hardware IOMMU (Intel VT-d or AMD-Vi IVRS discovery) and apply fail-closed DMA policy.
#[inline(never)]
pub fn init_vtd_units(cmdline: &str) {
    VTD_GFX_SCANOUT_DEFERRED.store(false, Ordering::Release);
    IOMMU_BACKEND.store(IommuBackendKind::None as u8, Ordering::Release);
    IOMMU_CAPS_BITS.store(0, Ordering::Release);
    IOMMU_CAPS_FLAGS.store(0, Ordering::Release);

    let policy = evaluate_iommu_policy(cmdline);
    let (present, malformed) = is_dmar_present_or_malformed();
    if malformed {
        VTD_STATUS.store(IommuStatus::FailedClosed as u8, Ordering::Release);
        serial_println!("[DRY-IOMMU] FAIL_CLOSED: malformed DMAR");
        return;
    }

    if present {
        let Some(dmar) = dmar_snapshot() else {
            VTD_STATUS.store(IommuStatus::FailedClosed as u8, Ordering::Release);
            return;
        };

        let (ok, units_up, bits, cm, coh) = with_vtd_lock(|eng| {
            let mut bits = 48u8;
            let mut cm = false;
            let mut coh = true;
            let mut have_enforced = false;
            for u in 0..dmar.drhd_count {
                let src = &dmar.drhds[u];
                if !init_single_unit(
                    dmar,
                    u,
                    &mut eng.units[u],
                    &mut eng.rmrr_domains[u],
                    vtd_unit_vaddr(u),
                    src,
                ) {
                    rollback_all_units(eng, u + 1);
                    return (false, 0usize, 0u8, false, true);
                }
                if eng.units[u].translation_enabled {
                    let u_bits = if eng.units[u].levels_4 { 48 } else { 39 };
                    if !have_enforced {
                        bits = u_bits;
                        cm = eng.units[u].caching_mode;
                        coh = eng.units[u].coherent;
                        have_enforced = true;
                    } else {
                        bits = bits.min(u_bits);
                        cm |= eng.units[u].caching_mode;
                        coh &= eng.units[u].coherent;
                    }
                }
            }
            if !have_enforced {
                rollback_all_units(eng, dmar.drhd_count);
            }
            (have_enforced, dmar.drhd_count, bits, cm, coh)
        });

        if ok {
            let mut cflags = (1u8 << 0) | (1u8 << 4); // 4K + fault_reporting
            if coh {
                cflags |= 1 << 2;
            }
            if dmar.rmrr_count > 0 {
                cflags |= 1 << 5;
            }
            IOMMU_BACKEND.store(IommuBackendKind::IntelVtd as u8, Ordering::Release);
            IOMMU_CAPS_BITS.store(bits, Ordering::Release);
            IOMMU_CAPS_FLAGS.store(cflags, Ordering::Release);
            VTD_STATUS.store(IommuStatus::Enforcing as u8, Ordering::Release);
            serial_println!(
                "[DRY-IOMMU] VTD_ACTIVE units={} sagaw={}-bit caching_mode={} coherent={}",
                units_up,
                bits,
                cm,
                coh
            );
        } else {
            VTD_STATUS.store(IommuStatus::FailedClosed as u8, Ordering::Release);
            serial_println!("[DRY-IOMMU] FAIL_CLOSED: DRHD initialization error");
        }
        return;
    }

    let (ivrs_present, ivrs_malformed) = is_ivrs_present_or_malformed();
    if ivrs_malformed {
        IOMMU_BACKEND.store(IommuBackendKind::AmdVi as u8, Ordering::Release);
        VTD_STATUS.store(IommuStatus::FailedClosed as u8, Ordering::Release);
        serial_println!("[DRY-IOMMU] FAIL_CLOSED: malformed IVRS");
        return;
    }
    if ivrs_present {
        if let Some(ivrs) = ivrs_snapshot() {
            IOMMU_BACKEND.store(IommuBackendKind::AmdVi as u8, Ordering::Release);
            IOMMU_CAPS_BITS.store(ivrs.pa_bits, Ordering::Release);
            serial_println!(
                "[DRY-IOMMU] IVRS detected ivhd={} ivmd={} pa={}b va={}b amd_svm={}; AMD-Vi backend unverified",
                ivrs.ivhd_count,
                ivrs.ivmd_count,
                ivrs.pa_bits,
                ivrs.va_bits,
                ivrs.amd_svm_cpu as u8
            );
        }
    }

    if policy.allow_qemu_test_bypass {
        IOMMU_BACKEND.store(IommuBackendKind::QemuTestBypass as u8, Ordering::Release);
        IOMMU_CAPS_BITS.store(48, Ordering::Release);
        VTD_STATUS.store(IommuStatus::TestUnsafeNoIommu as u8, Ordering::Release);
        serial_println!(
            "[DRY-IOMMU] QEMU_TEST_NO_IOMMU active (hv={} opt_in={} live_usb={}): test bypass enabled",
            policy.hypervisor_cpuid as u8,
            policy.qemu_test_opt_in as u8,
            policy.live_usb as u8
        );
    } else {
        VTD_STATUS.store(IommuStatus::UnsupportedFailClosed as u8, Ordering::Release);
        serial_println!(
            "[DRY-IOMMU] FAIL_CLOSED: no supported hardware IOMMU (hv={} opt_in={} live_usb={}); DMA claims & BME refused",
            policy.hypervisor_cpuid as u8,
            policy.qemu_test_opt_in as u8,
            policy.live_usb as u8
        );
    }
}

pub(super) fn poll_and_clear_unit_faults(unit: &mut VtdUnit) -> u64 {
    if !unit.active || !unit.translation_enabled {
        return 0;
    }
    let mut new_faults = 0u64;
    // SAFETY: `unit.fro_off + nfr * 16` verified within `unit.mmio_bytes` during init.
    unsafe {
        for idx in 0..unit.nfr {
            let off = unit.fro_off + idx * 16;
            let hi = read64(unit.mmio_virt, off + 8);
            if (hi & (1u64 << 63)) != 0 {
                let lo = read64(unit.mmio_virt, off);
                let sid = (hi & 0xffff) as u16;
                let reason = ((hi >> 32) & 0xff) as u8;
                let is_write = ((hi >> 62) & 1) == 0;
                let fault_addr = lo & !0xfffu64;

                VTD_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
                new_faults += 1;
                serial_println!(
                    "[DRY-IOMMU] FAULT sid={:#06x} reason={:#x} rw={} addr={:#x}",
                    sid,
                    reason,
                    if is_write { "W" } else { "R" },
                    fault_addr
                );
                write64(unit.mmio_virt, off + 8, 1u64 << 63);
            }
        }
        let fsts = read32(unit.mmio_virt, REG_FSTS);
        if (fsts & 0x7d) != 0 {
            write32(unit.mmio_virt, REG_FSTS, fsts & 0x7d);
        }
    }
    new_faults
}

/// Poll and W1C-clear all VT-d Fault Recording Registers (`FRCD`) across active DRHD units.
pub fn poll_and_clear_faults() -> u64 {
    if status() == IommuStatus::Enforcing {
        with_vtd_lock(|eng| {
            for u in eng.units.iter_mut() {
                if u.active {
                    let _ = poll_and_clear_unit_faults(u);
                }
            }
        });
    }
    VTD_FAULT_COUNT.load(Ordering::Acquire)
}
