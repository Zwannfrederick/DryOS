// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Intel `VT-d` Second-Level Page Tables (`SLPT`) and per-bus Context Table management (`Milestone 2C-2`).

use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};

use crate::pmm::{
    alloc_pages, free_pages, phys_to_hhdm, AllocFlags, FrameHandle, PageOrder, PAGE_SIZE,
};
use crate::security::{SecDomain, SecFlags, SecLabel};

pub(super) const VTD_KERNEL_LABEL: SecLabel =
    SecLabel::new(SecDomain::Kernel, SecFlags::NONE, 0, 0);

pub(super) const SLPT_R: u64 = 1 << 0;
pub(super) const SLPT_W: u64 = 1 << 1;
pub(super) const SLPT_PHYS_MASK: u64 = 0x000f_ffff_ffff_f000;
pub(super) const MAX_BUSES_PER_UNIT: usize = 8;
pub(super) const MAX_PT_FRAMES_PER_DOMAIN: usize = 96;

#[derive(Clone, Copy)]
pub(super) struct BusContextSlot {
    pub(super) active: bool,
    pub(super) bus: u8,
    pub(super) attach_count: u8,
    pub(super) frame: Option<FrameHandle>,
}

impl BusContextSlot {
    pub(super) const fn empty() -> Self {
        Self {
            active: false,
            bus: 0,
            attach_count: 0,
            frame: None,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct VtdUnit {
    pub(super) active: bool,
    pub(super) mmio_virt: u64,
    pub(super) mmio_bytes: usize,
    pub(super) segment: u16,
    pub(super) include_all: bool,
    pub(super) nfr: usize,
    pub(super) fro_off: usize,
    pub(super) iotlb_off: usize,
    pub(super) levels_4: bool,
    pub(super) coherent: bool,
    pub(super) caching_mode: bool,
    pub(super) need_rwbf: bool,
    pub(super) root_frame: Option<FrameHandle>,
    pub(super) ctx_buses: [BusContextSlot; MAX_BUSES_PER_UNIT],
}

impl VtdUnit {
    pub(super) const fn empty() -> Self {
        Self {
            active: false,
            mmio_virt: 0,
            mmio_bytes: 0,
            segment: 0,
            include_all: false,
            nfr: 0,
            fro_off: 0,
            iotlb_off: 0,
            levels_4: false,
            coherent: true,
            caching_mode: false,
            need_rwbf: false,
            root_frame: None,
            ctx_buses: [BusContextSlot::empty(); MAX_BUSES_PER_UNIT],
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct DomainState {
    pub(super) attached: bool,
    pub(super) unit_idx: u8,
    pub(super) segment: u16,
    pub(super) bus: u8,
    pub(super) dev: u8,
    pub(super) func: u8,
    pub(super) domain_id: u16,
    pub(super) sl_root: Option<FrameHandle>,
    pub(super) pt_count: usize,
    pub(super) pt_frames: [Option<FrameHandle>; MAX_PT_FRAMES_PER_DOMAIN],
}

impl DomainState {
    pub(super) const fn empty(did: u16) -> Self {
        Self {
            attached: false,
            unit_idx: 0,
            segment: 0,
            bus: 0,
            dev: 0,
            func: 0,
            domain_id: did,
            sl_root: None,
            pt_count: 0,
            pt_frames: [None; MAX_PT_FRAMES_PER_DOMAIN],
        }
    }
}

#[inline]
pub(super) fn flush_descriptor_cacheline(coherent: bool, ptr: *const u8) {
    if !coherent {
        // SAFETY: `ptr` points inside a mapped kernel HHDM 4 KiB table frame.
        unsafe {
            asm!(
                "clflush [{p}]",
                "mfence",
                p = in(reg) ptr,
                options(nostack, preserves_flags)
            );
        }
    }
}

pub(super) fn flush_entire_page(coherent: bool, ptr: *const u8) {
    if !coherent {
        for off in (0..PAGE_SIZE).step_by(64) {
            // SAFETY: Covers all 64-byte lines in a single mapped 4 KiB frame.
            unsafe {
                asm!("clflush [{p}]", p = in(reg) ptr.add(off), options(nostack, preserves_flags));
            }
        }
        // SAFETY: Orders cacheline flushes before VT-d invalidation MMIO writes.
        unsafe {
            asm!("mfence", options(nostack, preserves_flags));
        }
    }
}

fn alloc_domain_pt_frame(dom: &mut DomainState, coherent: bool) -> Result<FrameHandle, ()> {
    if dom.pt_count >= MAX_PT_FRAMES_PER_DOMAIN {
        return Err(());
    }
    let fh = alloc_pages(PageOrder::Order4KiB, VTD_KERNEL_LABEL, AllocFlags::ZERO).map_err(|_| ())?;
    flush_entire_page(coherent, fh.hhdm_ptr());
    dom.pt_frames[dom.pt_count] = Some(fh);
    dom.pt_count += 1;
    Ok(fh)
}

pub(super) fn map_slpt_4k(
    dom: &mut DomainState,
    coherent: bool,
    dma_phys: u64,
    levels_4: bool,
) -> Result<(), ()> {
    let root_h = dom.sl_root.ok_or(())?;
    let mut table_phys = root_h.phys_addr();
    let max_addr = if levels_4 { 1u64 << 48 } else { 1u64 << 39 };
    if dma_phys >= max_addr || (dma_phys & ((PAGE_SIZE as u64) - 1)) != 0 {
        return Err(());
    }
    let start_level = if levels_4 { 3 } else { 2 };
    for lvl in (1..=start_level).rev() {
        let idx = ((dma_phys >> (12 + lvl * 9)) & 0x1ff) as usize;
        // SAFETY: `table_phys` belongs to an allocated `4 KiB` HHDM SLPT node in `dom`.
        let entry_ptr = unsafe { (phys_to_hhdm(table_phys) as *mut u64).add(idx) };
        let cur = unsafe { read_volatile(entry_ptr) };
        table_phys = if (cur & (SLPT_R | SLPT_W)) != 0 {
            cur & SLPT_PHYS_MASK
        } else {
            let next_h = alloc_domain_pt_frame(dom, coherent)?;
            let next_pa = next_h.phys_addr();
            unsafe { write_volatile(entry_ptr, next_pa | SLPT_R | SLPT_W) };
            flush_descriptor_cacheline(coherent, entry_ptr as *const u8);
            next_pa
        };
    }
    let leaf_idx = ((dma_phys >> 12) & 0x1ff) as usize;
    // SAFETY: Leaf index inside valid allocated PT page.
    let leaf_ptr = unsafe { (phys_to_hhdm(table_phys) as *mut u64).add(leaf_idx) };
    unsafe { write_volatile(leaf_ptr, (dma_phys & SLPT_PHYS_MASK) | SLPT_R | SLPT_W) };
    flush_descriptor_cacheline(coherent, leaf_ptr as *const u8);
    Ok(())
}

pub(super) fn clear_slpt_4k(coherent: bool, root_phys: u64, dma_phys: u64, levels_4: bool) {
    let mut table_phys = root_phys;
    let start_level = if levels_4 { 3 } else { 2 };
    for lvl in (1..=start_level).rev() {
        let idx = ((dma_phys >> (12 + lvl * 9)) & 0x1ff) as usize;
        // SAFETY: `table_phys` is a valid mapped SLPT frame.
        let cur = unsafe { read_volatile((phys_to_hhdm(table_phys) as *const u64).add(idx)) };
        if (cur & (SLPT_R | SLPT_W)) == 0 {
            return;
        }
        table_phys = cur & SLPT_PHYS_MASK;
    }
    let leaf_idx = ((dma_phys >> 12) & 0x1ff) as usize;
    // SAFETY: Clearing leaf translation entry on mapped PT page.
    let leaf_ptr = unsafe { (phys_to_hhdm(table_phys) as *mut u64).add(leaf_idx) };
    unsafe { write_volatile(leaf_ptr, 0) };
    flush_descriptor_cacheline(coherent, leaf_ptr as *const u8);
}

pub(super) fn free_all_domain_frames(dom: &mut DomainState) {
    for slot in dom.pt_frames[..dom.pt_count].iter_mut() {
        if let Some(fh) = slot.take() {
            free_pages(fh, PageOrder::Order4KiB, VTD_KERNEL_LABEL);
        }
    }
    dom.pt_count = 0;
    if let Some(root_h) = dom.sl_root.take() {
        free_pages(root_h, PageOrder::Order4KiB, VTD_KERNEL_LABEL);
    }
}

pub(super) fn get_or_alloc_bus_ctx(unit: &mut VtdUnit, bus: u8) -> Result<usize, ()> {
    if let Some(idx) = unit.ctx_buses.iter().position(|c| c.active && c.bus == bus) {
        return Ok(idx);
    }
    let idx = unit.ctx_buses.iter().position(|c| !c.active).ok_or(())?;
    let fh = alloc_pages(PageOrder::Order4KiB, VTD_KERNEL_LABEL, AllocFlags::ZERO).map_err(|_| ())?;
    flush_entire_page(unit.coherent, fh.hhdm_ptr());
    let root_h = unit.root_frame.ok_or(())?;
    // SAFETY: Writing 128-bit Root Entry (`bus` in `0..256`) inside mapped Root Table.
    unsafe {
        let root_ptr = (root_h.hhdm_ptr() as *mut u64).add((bus as usize) * 2);
        write_volatile(root_ptr.add(1), 0);
        write_volatile(root_ptr, fh.phys_addr() | 0x1);
        flush_descriptor_cacheline(unit.coherent, root_ptr as *const u8);
    }
    unit.ctx_buses[idx] = BusContextSlot {
        active: true,
        bus,
        attach_count: 0,
        frame: Some(fh),
    };
    Ok(idx)
}

pub(super) fn release_bus_ctx_if_idle(unit: &mut VtdUnit, bus: u8) -> Option<FrameHandle> {
    let idx = unit.ctx_buses.iter().position(|c| c.active && c.bus == bus)?;
    if unit.ctx_buses[idx].attach_count > 0 {
        return None;
    }
    if let Some(root_h) = unit.root_frame {
        // SAFETY: Clearing Root Entry for idle bus before invalidating caches and freeing frame.
        unsafe {
            let root_ptr = (root_h.hhdm_ptr() as *mut u64).add((bus as usize) * 2);
            write_volatile(root_ptr, 0);
            write_volatile(root_ptr.add(1), 0);
            flush_descriptor_cacheline(unit.coherent, root_ptr as *const u8);
        }
    }
    let fh = unit.ctx_buses[idx].frame.take();
    unit.ctx_buses[idx] = BusContextSlot::empty();
    fh
}
