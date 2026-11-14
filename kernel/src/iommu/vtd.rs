// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Intel `VT-d` DMA Remapping engine (`Milestone 2C-2`).
//!
//! Enforces strict per-PCI-device Second-Level Page Tables (`SLPT`) with synchronous Context/IOTLB
//! invalidations, fault recording inspection (`FRCD`), and full PMM frame reclamation on teardown.

use core::cell::UnsafeCell;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};

use crate::arch::{irq_restore, irq_save_cli};
use crate::iommu::dmar::{dmar_snapshot, is_dmar_present_or_malformed, MAX_DRHD_UNITS};
use crate::iommu::slpt::{
    clear_slpt_4k, flush_descriptor_cacheline, flush_entire_page, free_all_domain_frames,
    get_or_alloc_bus_ctx, map_slpt_4k, release_bus_ctx_if_idle, DomainState, VtdUnit,
    VTD_KERNEL_LABEL,
};
use crate::pci::PCI_DEV_COUNT;
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
const CCMD_CIRG_DEVICE: u64 = 3 << 61;

const IOTLB_IVT: u64 = 1 << 63;
const IOTLB_IIRG_GLOBAL: u64 = 1 << 60;
const IOTLB_IIRG_DOMAIN: u64 = 2 << 60;
const IOTLB_DR: u64 = 1 << 49;
const IOTLB_DW: u64 = 1 << 48;

const VTD_POLL_SPIN_LIMIT: usize = 1_000_000;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VtdStatus {
    Uninitialized = 0,
    NotPresent = 1,
    Enforcing = 2,
    FailedClosed = 3,
}

struct VtdEngineState {
    units: [VtdUnit; MAX_DRHD_UNITS],
    domains: [DomainState; PCI_DEV_COUNT],
}

impl VtdEngineState {
    const fn new() -> Self {
        Self {
            units: [VtdUnit::empty(); MAX_DRHD_UNITS],
            domains: [
                DomainState::empty(1),
                DomainState::empty(2),
                DomainState::empty(3),
                DomainState::empty(4),
                DomainState::empty(5),
                DomainState::empty(6),
            ],
        }
    }
}

struct VtdLockBox {
    locked: AtomicBool,
    inner: UnsafeCell<VtdEngineState>,
}
unsafe impl Sync for VtdLockBox {}

static VTD_STATUS: AtomicU8 = AtomicU8::new(VtdStatus::Uninitialized as u8);
static VTD_FAULT_COUNT: AtomicU64 = AtomicU64::new(0);
static VTD_ENGINE: VtdLockBox = VtdLockBox {
    locked: AtomicBool::new(false),
    inner: UnsafeCell::new(VtdEngineState::new()),
};

fn with_vtd_lock<R>(f: impl FnOnce(&mut VtdEngineState) -> R) -> R {
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
pub fn status() -> VtdStatus {
    match VTD_STATUS.load(Ordering::Acquire) {
        1 => VtdStatus::NotPresent,
        2 => VtdStatus::Enforcing,
        3 => VtdStatus::FailedClosed,
        _ => VtdStatus::Uninitialized,
    }
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

fn invalidate_context_cache(unit: &VtdUnit, cmd_bits: u64) -> bool {
    if !flush_write_buffers(unit) {
        return false;
    }
    // SAFETY: `unit.mmio_virt` is mapped Supervisor MMIO UC.
    unsafe {
        write64(unit.mmio_virt, REG_CCMD, CCMD_ICC | cmd_bits);
        wait_until(|| (read64(unit.mmio_virt, REG_CCMD) & CCMD_ICC) == 0)
    }
}

fn invalidate_iotlb_cache(unit: &VtdUnit, cmd_bits: u64) -> bool {
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

fn maybe_release_bus_ctx(unit: &mut VtdUnit, bus: u8) {
    if let Some(fh) = release_bus_ctx_if_idle(unit, bus) {
        let _ = invalidate_context_cache(unit, CCMD_CIRG_GLOBAL);
        let _ = invalidate_iotlb_cache(unit, IOTLB_IIRG_GLOBAL);
        free_pages(fh, PageOrder::Order4KiB, VTD_KERNEL_LABEL);
    }
}

fn init_single_unit(
    unit: &mut VtdUnit,
    mmio_virt: u64,
    pages: u8,
    segment: u16,
    include_all: bool,
) -> bool {
    let mmio_bytes = (pages as usize) * PAGE_SIZE;
    if mmio_bytes < PAGE_SIZE || pages as usize > KERNEL_VTD_MAX_PAGES {
        return false;
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
    if nd <= PCI_DEV_COUNT {
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
    unit.mmio_virt = mmio_virt;
    unit.mmio_bytes = mmio_bytes;
    unit.segment = segment;
    unit.include_all = include_all;
    unit.nfr = nfr;
    unit.fro_off = fro_off;
    unit.iotlb_off = iotlb_off;
    unit.levels_4 = levels_4;
    unit.coherent = coherent;
    unit.caching_mode = ((cap >> 7) & 1) != 0;
    unit.need_rwbf = ((cap >> 4) & 1) != 0;
    unit.root_frame = Some(root_h);

    // SAFETY: Disable translation cleanly if firmware left it on, mask fault IRQ, set RTADDR.
    unsafe {
        let gsts = read32(mmio_virt, REG_GSTS);
        if (gsts & GSTS_TES) != 0 && !send_gcmd(unit, GCMD_TE, false) {
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

    poll_and_clear_unit_faults(unit);
    true
}

/// Initialize Intel VT-d units from ACPI `DMAR`. Must run once on BSP right after `init_and_activate_kernel_vspace`.
#[inline(never)]
pub fn init_vtd_units() {
    let (present, malformed) = is_dmar_present_or_malformed();
    if malformed {
        VTD_STATUS.store(VtdStatus::FailedClosed as u8, Ordering::Release);
        serial_println!("[DRY-IOMMU] VTD_FAIL_CLOSED: malformed DMAR");
        return;
    }
    if !present {
        VTD_STATUS.store(VtdStatus::NotPresent as u8, Ordering::Release);
        serial_println!("[DRY-IOMMU] VTD_NOT_PRESENT (fail-open baseline disabled)");
        return;
    }
    let Some(dmar) = dmar_snapshot() else {
        VTD_STATUS.store(VtdStatus::FailedClosed as u8, Ordering::Release);
        return;
    };

    let (ok, units_up, bits, cm, coh) = with_vtd_lock(|eng| {
        for u in 0..dmar.drhd_count {
            let src = &dmar.drhds[u];
            if !init_single_unit(
                &mut eng.units[u],
                vtd_unit_vaddr(u),
                src.mapped_pages,
                src.segment,
                src.include_all,
            ) {
                return (false, 0usize, 0u8, false, true);
            }
        }
        let u0 = &eng.units[0];
        let bits = if u0.levels_4 { 48 } else { 39 };
        (true, dmar.drhd_count, bits, u0.caching_mode, u0.coherent)
    });

    if ok {
        VTD_STATUS.store(VtdStatus::Enforcing as u8, Ordering::Release);
        serial_println!(
            "[DRY-IOMMU] VTD_ACTIVE units={} sagaw={}-bit caching_mode={} coherent={}",
            units_up,
            bits,
            cm,
            coh
        );
    } else {
        VTD_STATUS.store(VtdStatus::FailedClosed as u8, Ordering::Release);
        serial_println!("[DRY-IOMMU] VTD_FAIL_CLOSED: DRHD initialization error");
    }
}

fn find_unit_for_bdf(eng: &VtdEngineState, bus: u8, dev: u8, func: u8) -> Option<usize> {
    let dmar = dmar_snapshot()?;
    for u in 0..dmar.drhd_count {
        if !eng.units[u].active {
            continue;
        }
        let src = &dmar.drhds[u];
        for s in 0..src.scope_count {
            let sc = src.scopes[s];
            if sc.valid && sc.bus == bus && sc.dev == dev && sc.func == func {
                return Some(u);
            }
        }
    }
    (0..dmar.drhd_count).find(|&u| eng.units[u].active && eng.units[u].include_all)
}

/// Fail-closed gate checked before claiming a PCI device or enabling `Command.BME = 1`.
pub fn attach_device_domain(dev_idx: usize, bus: u8, dev: u8, func: u8) -> Result<(), ()> {
    match status() {
        VtdStatus::NotPresent => return Ok(()),
        VtdStatus::Uninitialized | VtdStatus::FailedClosed => return Err(()),
        VtdStatus::Enforcing => {}
    }
    if dev_idx >= PCI_DEV_COUNT {
        return Err(());
    }
    let rmrr_snap = dmar_snapshot();

    with_vtd_lock(|eng| {
        if eng.domains[dev_idx].attached {
            return Ok(());
        }
        let unit_idx = find_unit_for_bdf(eng, bus, dev, func).ok_or(())?;
        let (coherent, levels_4, seg) = (
            eng.units[unit_idx].coherent,
            eng.units[unit_idx].levels_4,
            eng.units[unit_idx].segment,
        );
        let root_h =
            alloc_pages(PageOrder::Order4KiB, VTD_KERNEL_LABEL, AllocFlags::ZERO).map_err(|_| ())?;
        flush_entire_page(coherent, root_h.hhdm_ptr());
        eng.domains[dev_idx].sl_root = Some(root_h);
        eng.domains[dev_idx].pt_count = 0;

        if let Some(dmar) = rmrr_snap {
            for r in 0..dmar.rmrr_count {
                let rmrr = &dmar.rmrrs[r];
                if !rmrr.valid || rmrr.segment != seg {
                    continue;
                }
                let matched = (0..rmrr.scope_count).any(|s| {
                    let sc = rmrr.scopes[s];
                    sc.valid && sc.bus == bus && sc.dev == dev && sc.func == func
                });
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
        let aw = if levels_4 { 2u64 } else { 1u64 };
        let did = eng.domains[dev_idx].domain_id as u64;
        // SAFETY: Writing 128-bit Context Entry inside allocated per-bus Context Table.
        unsafe {
            let ctx_ptr = (ctx_fh.hhdm_ptr() as *mut u64).add(devfn * 2);
            write_volatile(ctx_ptr.add(1), aw | (did << 8));
            write_volatile(ctx_ptr, root_h.phys_addr() | 0x1);
            flush_descriptor_cacheline(coherent, ctx_ptr as *const u8);
        }

        let sid = ((bus as u64) << 8) | (devfn as u64);
        let inv_ok = invalidate_context_cache(
            &eng.units[unit_idx],
            CCMD_CIRG_DEVICE | did | (sid << 16),
        ) && invalidate_iotlb_cache(&eng.units[unit_idx], IOTLB_IIRG_DOMAIN | (did << 32));
        if !inv_ok {
            // SAFETY: Clearing Context Entry on invalidation failure rollback.
            unsafe {
                let ctx_ptr = (ctx_fh.hhdm_ptr() as *mut u64).add(devfn * 2);
                write_volatile(ctx_ptr, 0);
                write_volatile(ctx_ptr.add(1), 0);
                flush_descriptor_cacheline(coherent, ctx_ptr as *const u8);
            }
            maybe_release_bus_ctx(&mut eng.units[unit_idx], bus);
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

/// Map `len_bytes` (`4 KiB` or `2 MiB` pinned `DEVICE_DMA`) into `dev_idx`'s isolated SLPT.
pub fn map_dma_range(dev_idx: usize, phys_base: u64, len_bytes: u64) -> Result<(), ()> {
    match status() {
        VtdStatus::NotPresent => return Ok(()),
        VtdStatus::Uninitialized | VtdStatus::FailedClosed => return Err(()),
        VtdStatus::Enforcing => {}
    }
    if dev_idx >= PCI_DEV_COUNT || (len_bytes & ((PAGE_SIZE as u64) - 1)) != 0 || len_bytes == 0 {
        return Err(());
    }
    let pages = (len_bytes / (PAGE_SIZE as u64)) as usize;
    with_vtd_lock(|eng| {
        let dom = &mut eng.domains[dev_idx];
        if !dom.attached {
            return Err(());
        }
        let u = &eng.units[dom.unit_idx as usize];
        let (coherent, levels_4, did) = (u.coherent, u.levels_4, dom.domain_id as u64);
        for p in 0..pages {
            let pa = phys_base + (p as u64) * (PAGE_SIZE as u64);
            if map_slpt_4k(dom, coherent, pa, levels_4).is_err() {
                if let Some(root_h) = dom.sl_root {
                    let rpa = root_h.phys_addr();
                    for rb in 0..p {
                        clear_slpt_4k(
                            coherent,
                            rpa,
                            phys_base + (rb as u64) * (PAGE_SIZE as u64),
                            levels_4,
                        );
                    }
                }
                return Err(());
            }
        }
        if !invalidate_iotlb_cache(u, IOTLB_IIRG_DOMAIN | (did << 32)) {
            return Err(());
        }
        Ok(())
    })
}

/// Unmap `len_bytes` from `dev_idx`'s SLPT and synchronize IOTLB.
pub fn unmap_dma_range(dev_idx: usize, phys_base: u64, len_bytes: u64) {
    if status() != VtdStatus::Enforcing || dev_idx >= PCI_DEV_COUNT {
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
            let rpa = root_h.phys_addr();
            for p in 0..pages {
                clear_slpt_4k(
                    u.coherent,
                    rpa,
                    phys_base + (p as u64) * (PAGE_SIZE as u64),
                    u.levels_4,
                );
            }
        }
        let _ = invalidate_iotlb_cache(u, IOTLB_IIRG_DOMAIN | ((dom.domain_id as u64) << 32));
    });
}

/// Clear Context Entry, invalidate Context + Domain IOTLB, and free all SLPT frames (`leaked_frames == 0`).
pub fn detach_and_scrub_domain(dev_idx: usize) {
    if status() != VtdStatus::Enforcing || dev_idx >= PCI_DEV_COUNT {
        return;
    }
    with_vtd_lock(|eng| {
        if !eng.domains[dev_idx].attached {
            return;
        }
        let unit_idx = eng.domains[dev_idx].unit_idx as usize;
        let bus = eng.domains[dev_idx].bus;
        let dev = eng.domains[dev_idx].dev;
        let func = eng.domains[dev_idx].func;
        let did = eng.domains[dev_idx].domain_id as u64;
        let devfn = (((dev & 0x1f) as usize) << 3) | ((func & 0x07) as usize);

        if let Some(bs) = eng.units[unit_idx]
            .ctx_buses
            .iter()
            .position(|c| c.active && c.bus == bus)
        {
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
        maybe_release_bus_ctx(&mut eng.units[unit_idx], bus);

        free_all_domain_frames(&mut eng.domains[dev_idx]);
        eng.domains[dev_idx].attached = false;
    });
}

fn poll_and_clear_unit_faults(unit: &mut VtdUnit) -> u64 {
    if !unit.active {
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
    if status() == VtdStatus::Enforcing {
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
