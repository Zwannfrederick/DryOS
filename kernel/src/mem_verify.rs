// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Early Boot Limine Memory Map & Higher-Half Direct Map (HHDM) Hardware Verification.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU32, Ordering};

use crate::limine::{
    EXEC_ADDR_REQ, HHDM_REQ, LIMINE_MEMMAP_BOOTLOADER_RECLAIMABLE,
    LIMINE_MEMMAP_EXECUTABLE_AND_MODULES, LIMINE_MEMMAP_USABLE, MEMMAP_REQ,
};

/// Immutable canary placed in `.rodata` to cross-verify higher-half virtual vs HHDM alias.
#[used]
static RODATA_HHDM_CANARY: u64 = 0x4452_594f_535f_4848; // "DRYOS_HH"

static MEM_TOTAL_ENTRIES: AtomicU32 = AtomicU32::new(0);
static MEM_USABLE_REGIONS: AtomicU32 = AtomicU32::new(0);
static MEM_USABLE_MIB: AtomicU32 = AtomicU32::new(0);

#[inline]
pub fn latched_mem_summary() -> (u32, u32, u32) {
    (
        MEM_TOTAL_ENTRIES.load(Ordering::Acquire),
        MEM_USABLE_REGIONS.load(Ordering::Acquire),
        MEM_USABLE_MIB.load(Ordering::Acquire),
    )
}

pub struct MemorySummary {
    pub hhdm_offset: u64,
    pub total_entries: usize,
    pub usable_regions: usize,
    pub usable_bytes: u64,
    pub reclaimable_bytes: u64,
    pub kernel_phys_base: u64,
    pub kernel_virt_base: u64,
}

pub fn verify_memory_and_hhdm() -> MemorySummary {
    let hhdm_resp = HHDM_REQ.read().response;
    assert!(!hhdm_resp.is_null(), "Limine HHDM response is null");
    // SAFETY: Non-null response pointer populated by Limine before `_start`.
    let hhdm_offset = unsafe { (*hhdm_resp).offset };
    assert!(
        hhdm_offset >= 0xffff_8000_0000_0000 && (hhdm_offset & 0xfff) == 0,
        "Invalid or unaligned HHDM offset: {:#x}",
        hhdm_offset
    );

    let exec_resp = EXEC_ADDR_REQ.read().response;
    assert!(!exec_resp.is_null(), "Limine Executable Address response is null");
    // SAFETY: Non-null response pointer populated by Limine before `_start`.
    let (kernel_phys_base, kernel_virt_base) =
        unsafe { ((*exec_resp).physical_base, (*exec_resp).virtual_base) };
    assert_eq!(
        kernel_virt_base, 0xffff_ffff_8000_0000,
        "Unexpected kernel virtual base: {:#x}",
        kernel_virt_base
    );
    assert_eq!(kernel_phys_base & 0xfff, 0, "Unaligned kernel physical base");

    // 1. Cross-check `.rodata` canary via Higher-Half Virtual Address vs HHDM Physical Alias.
    let canary_virt = (&raw const RODATA_HHDM_CANARY) as u64;
    assert!(canary_virt >= kernel_virt_base, "Canary below kernel_virt_base");
    let canary_phys = kernel_phys_base + (canary_virt - kernel_virt_base);
    let canary_hhdm_ptr = (hhdm_offset + canary_phys) as *const u64;
    // SAFETY: `canary_phys` is our own loaded `.rodata` frame, mapped linearly in HHDM by Limine.
    let via_hhdm = unsafe { read_volatile(canary_hhdm_ptr) };
    assert_eq!(
        via_hhdm, RODATA_HHDM_CANARY,
        "HHDM alias mismatch: expected {:#x}, got {:#x}",
        RODATA_HHDM_CANARY, via_hhdm
    );

    // 2. Walk and validate Limine Memory Map monotonicity, kernel containment, and usable RAM.
    let mm_resp = MEMMAP_REQ.read().response;
    assert!(!mm_resp.is_null(), "Limine Memory Map response is null");
    // SAFETY: Non-null response pointer and entry array populated by Limine before `_start`.
    let (entry_count, entries_ptr) = unsafe { ((*mm_resp).entry_count as usize, (*mm_resp).entries) };
    assert!(entry_count > 0 && !entries_ptr.is_null(), "Empty Limine memory map");

    let mut usable_regions = 0usize;
    let mut usable_bytes = 0u64;
    let mut reclaimable_bytes = 0u64;
    let mut found_kernel_region = false;
    let mut probe_phys_page: Option<u64> = None;
    let mut prev_end = 0u64;

    for idx in 0..entry_count {
        // SAFETY: `idx < entry_count` within bootloader-provided pointer table.
        let entry = unsafe { &**entries_ptr.add(idx) };
        assert!(entry.length > 0, "Zero-length memmap entry at index {}", idx);
        let end = entry.base.checked_add(entry.length).expect("Memmap entry address overflow");
        if idx > 0 {
            assert!(
                entry.base >= prev_end,
                "Non-monotonic / overlapping memmap entries at {}: {:#x} < {:#x}",
                idx,
                entry.base,
                prev_end
            );
        }
        prev_end = end;

        match entry.kind {
            LIMINE_MEMMAP_USABLE => {
                usable_regions += 1;
                usable_bytes += entry.length;
                // Pick a page-aligned usable frame above 1 MiB for non-destructive RW HHDM probe.
                if probe_phys_page.is_none() && entry.base >= 0x10_0000 && entry.length >= 0x1000 {
                    probe_phys_page = Some((entry.base + 0xfff) & !0xfff);
                }
            }
            LIMINE_MEMMAP_BOOTLOADER_RECLAIMABLE => {
                reclaimable_bytes += entry.length;
            }
            LIMINE_MEMMAP_EXECUTABLE_AND_MODULES
                if kernel_phys_base >= entry.base && kernel_phys_base < end =>
            {
                found_kernel_region = true;
            }
            _ => {}
        }
    }

    assert!(found_kernel_region, "Kernel physical base {:#x} missing from EXECUTABLE_AND_MODULES", kernel_phys_base);
    assert!(usable_bytes >= 16 * 1024 * 1024, "Insufficient usable RAM: {} bytes", usable_bytes);

    // 3. Active RW canary probe on a free `LIMINE_MEMMAP_USABLE` frame through HHDM.
    let probe_phys = probe_phys_page.expect("No >=1MiB USABLE page found for HHDM RW probe");
    let probe_ptr = (hhdm_offset + probe_phys) as *mut u64;
    let pattern: u64 = 0xa55a_f00d_cafe_2026 ^ probe_phys;
    // SAFETY: Limine guarantees `LIMINE_MEMMAP_USABLE` frames are free RAM not used by stack,
    // page tables, or bootloader structures, and mapped read-write in HHDM.
    unsafe {
        let orig = read_volatile(probe_ptr);
        write_volatile(probe_ptr, pattern);
        let readback = read_volatile(probe_ptr);
        write_volatile(probe_ptr, orig);
        assert_eq!(readback, pattern, "HHDM RW probe failed at phys {:#x}", probe_phys);
    }

    MEM_TOTAL_ENTRIES.store(entry_count as u32, Ordering::Release);
    MEM_USABLE_REGIONS.store(usable_regions as u32, Ordering::Release);
    MEM_USABLE_MIB.store((usable_bytes / (1024 * 1024)) as u32, Ordering::Release);

    MemorySummary {
        hhdm_offset,
        total_entries: entry_count,
        usable_regions,
        usable_bytes,
        reclaimable_bytes,
        kernel_phys_base,
        kernel_virt_base,
    }
}
