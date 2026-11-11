// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Physical Memory Inventory, One-Way `BootstrapAllocator`, and 16-Byte `FrameMeta` State Model.

use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};

use crate::limine::{
    LIMINE_MEMMAP_ACPI_NVS, LIMINE_MEMMAP_ACPI_RECLAIMABLE, LIMINE_MEMMAP_BAD_MEMORY,
    LIMINE_MEMMAP_BOOTLOADER_RECLAIMABLE, LIMINE_MEMMAP_EXECUTABLE_AND_MODULES,
    LIMINE_MEMMAP_FRAMEBUFFER, LIMINE_MEMMAP_RESERVED, LIMINE_MEMMAP_USABLE, MEMMAP_REQ,
};
use crate::mem_verify::MemorySummary;
use crate::security::{SecDomain, SecFlags, SecLabel};

pub const PAGE_SHIFT: u32 = 12;
pub const PAGE_SIZE: usize = 4096;
pub const HUGE_2M_FRAMES: usize = 512;
/// Low `< 64 KiB` (`PFN 0..16`) BIOS/real-mode/AP guard: never handed out as general RAM.
pub const LOW_MEM_GUARD_END_PFN: u32 = 16;
/// Standard x86 Local APIC MMIO base (`0xFEE0_0000`) — must never classify as allocatable RAM.
pub const LAPIC_MMIO_PHYS_BASE: u64 = 0xfee0_0000;
pub(crate) const PFN_NONE: u32 = u32::MAX;
pub(crate) const NEVER_OWNED_DOMAIN: u8 = 3;
pub(crate) const NO_CPU_OWNER: u8 = 0xff;

/// Extensible allocation order (`Order4KiB` implemented in `1B-A`; `Order2MiB` ready for `1B-B`).
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageOrder {
    Order4KiB = 0,
    Order2MiB = 9,
}

impl PageOrder {
    #[inline]
    pub const fn frame_count(self) -> usize {
        1usize << (self as u8)
    }
}

/// Allocation policy flags (`ADR-007` cross-domain zeroing is ALWAYS enforced regardless of flags).
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllocFlags(pub u8);

impl AllocFlags {
    /// Guarantee returned page is all-zero (`rep stosq` skipped only if `ZeroState == VerifiedZero`).
    pub const ZERO: Self = Self(1 << 0);
    /// Allow skipping `rep stosq` ONLY if the frame's last owner had identical `(SecDomain, compartment_id)`.
    pub const UNINIT_SAME_DOMAIN_OK: Self = Self(1 << 1);
    /// Bypass per-CPU magazine and hit `GlobalFreePool` directly (contention benchmarking / diagnostics).
    pub const BYPASS_PERCPU_CACHE: Self = Self(1 << 2);

    #[inline]
    pub const fn contains(self, rhs: Self) -> bool {
        (self.0 & rhs.0) == rhs.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHandle {
    pub pfn: u32,
    pub generation: u32,
}

impl FrameHandle {
    #[inline]
    pub const fn phys_addr(self) -> u64 {
        (self.pfn as u64) << PAGE_SHIFT
    }

    #[inline]
    pub fn hhdm_ptr(self) -> *mut u8 {
        phys_to_hhdm(self.phys_addr())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PmmError {
    BootstrapSealed,
    BootstrapExhausted,
    OutOfMemory,
    OrderUnsupportedYet,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootstrapState {
    Uninit = 0,
    Active = 1,
    Sealed = 2,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameState {
    ReservedOrMmio = 0,
    KernelImageOrBootMeta = 1,
    BootloaderReclaimable = 2,
    FreeGlobal = 3,
    FreePerCpu = 4,
    AllocatedFrame = 5,
    UntypedFree = 6,
    UntypedRetypedObj = 7,
}

impl FrameState {
    #[inline]
    pub(crate) const fn from_u8(v: u8) -> Self {
        match v & 0x7 {
            1 => Self::KernelImageOrBootMeta,
            2 => Self::BootloaderReclaimable,
            3 => Self::FreeGlobal,
            4 => Self::FreePerCpu,
            5 => Self::AllocatedFrame,
            6 => Self::UntypedFree,
            7 => Self::UntypedRetypedObj,
            _ => Self::ReservedOrMmio,
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZeroState {
    Dirty = 0,
    VerifiedZero = 1,
}

/// 16-byte per-PFN metadata entry (`0.390625%` of physical RAM).
#[repr(C, align(16))]
pub struct FrameMeta {
    pub(crate) word0: AtomicU64,
    pub(crate) word1: AtomicU64,
}

const _: () = assert!(size_of::<FrameMeta>() == 16);

#[inline]
pub(crate) const fn pack_word0(
    state: FrameState,
    zero: ZeroState,
    order_code: u8,
    last_domain: u8,
    pin_count: u16,
    map_count: u16,
    generation: u32,
) -> u64 {
    (state as u64)
        | ((zero as u64) << 3)
        | (((order_code & 0xf) as u64) << 4)
        | (((last_domain & 0x3) as u64) << 8)
        | (((pin_count & 0x3ff) as u64) << 10)
        | (((map_count & 0xfff) as u64) << 20)
        | ((generation as u64) << 32)
}

#[inline]
pub(crate) const fn unpack_word0(w: u64) -> (FrameState, ZeroState, u8, u8, u16, u16, u32) {
    let state = FrameState::from_u8((w & 0x7) as u8);
    let zero = if ((w >> 3) & 1) != 0 {
        ZeroState::VerifiedZero
    } else {
        ZeroState::Dirty
    };
    (
        state,
        zero,
        ((w >> 4) & 0xf) as u8,
        ((w >> 8) & 0x3) as u8,
        ((w >> 10) & 0x3ff) as u16,
        ((w >> 20) & 0xfff) as u16,
        (w >> 32) as u32,
    )
}

#[inline]
pub(crate) const fn pack_seclabel(label: SecLabel) -> u64 {
    (label.domain as u64)
        | ((label.flags.0 as u64) << 8)
        | ((label.compartment_id as u64) << 16)
        | ((label.policy_tag as u64) << 32)
}

#[inline]
pub(crate) const fn unpack_seclabel(w: u64) -> SecLabel {
    let domain = match (w & 0xff) as u8 {
        0 => SecDomain::Kernel,
        1 => SecDomain::RootSystem,
        _ => SecDomain::Userspace,
    };
    SecLabel::new(
        domain,
        SecFlags(((w >> 8) & 0xff) as u8),
        ((w >> 16) & 0xffff) as u16,
        (w >> 32) as u32,
    )
}

#[inline]
pub(crate) const fn pack_free_word1(next_pfn: u32, last_compartment: u16, cache_cpu: u8) -> u64 {
    (next_pfn as u64) | ((last_compartment as u64) << 32) | ((cache_cpu as u64) << 48)
}

#[inline]
pub(crate) const fn unpack_free_word1(w: u64) -> (u32, u16, u8) {
    (w as u32, ((w >> 32) & 0xffff) as u16, ((w >> 48) & 0xff) as u8)
}

#[inline]
pub(crate) const fn next_generation(cur: u32) -> u32 {
    if cur == u32::MAX { 1 } else { cur + 1 }
}

pub fn pmm_invariant_panic(args: core::fmt::Arguments<'_>) -> ! {
    crate::enter_fatal_or_halt();
    crate::serial::emergency_print(format_args!("\r\n[DRY-PMM-FAIL] {}\r\n", args));
    panic!("PMM invariant violation");
}

/// Hardware-accelerated 4 KiB zeroing (`512 x 8B` via `cld; rep stosq`).
#[inline]
pub fn zero_frame_4k(hhdm_ptr: *mut u8) {
    debug_assert_eq!((hhdm_ptr as usize) & (PAGE_SIZE - 1), 0);
    // SAFETY: Caller owns the 4 KiB physical page mapped read-write at `hhdm_ptr`.
    unsafe {
        asm!(
            "cld",
            "rep stosq",
            inout("rdi") hhdm_ptr => _,
            inout("rcx") (PAGE_SIZE / 8) => _,
            in("rax") 0u64,
            options(nostack),
        );
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PmmInventory {
    pub hhdm_offset: u64,
    pub max_tracked_pfn: u32,
    pub usable_frames: u64,
    pub free_init_frames: u64,
    pub reclaimable_frames: u64,
    pub kernel_meta_frames: u64,
    pub reserved_mmio_frames: u64,
    pub meta_bytes: u64,
    pub meta_overhead_bps: u64,
}

struct BootstrapStateInner {
    cursor_phys: u64,
    limit_phys: u64,
    carved_start_pfn: u32,
    carved_end_pfn: u32,
}

struct BootstrapAllocGlobal {
    state: AtomicU8,
    inner: UnsafeCell<BootstrapStateInner>,
}

// SAFETY: Mutated strictly on the single-threaded BSP prior to `seal()` and SMP bring-up.
unsafe impl Sync for BootstrapAllocGlobal {}

static BOOTSTRAP_ALLOC: BootstrapAllocGlobal = BootstrapAllocGlobal {
    state: AtomicU8::new(BootstrapState::Uninit as u8),
    inner: UnsafeCell::new(BootstrapStateInner {
        cursor_phys: 0,
        limit_phys: 0,
        carved_start_pfn: 0,
        carved_end_pfn: 0,
    }),
};

pub(crate) struct PmmGlobalMeta {
    pub initialized: AtomicBool,
    pub hhdm_offset: AtomicU64,
    pub meta_base_ptr: AtomicU64,
    pub max_tracked_pfn: AtomicU32,
    pub inventory: UnsafeCell<PmmInventory>,
}

// SAFETY: `inventory` is written once on BSP before `initialized = true` (`Release`).
unsafe impl Sync for PmmGlobalMeta {}

pub(crate) static PMM_META: PmmGlobalMeta = PmmGlobalMeta {
    initialized: AtomicBool::new(false),
    hhdm_offset: AtomicU64::new(0),
    meta_base_ptr: AtomicU64::new(0),
    max_tracked_pfn: AtomicU32::new(0),
    inventory: UnsafeCell::new(PmmInventory {
        hhdm_offset: 0,
        max_tracked_pfn: 0,
        usable_frames: 0,
        free_init_frames: 0,
        reclaimable_frames: 0,
        kernel_meta_frames: 0,
        reserved_mmio_frames: 0,
        meta_bytes: 0,
        meta_overhead_bps: 0,
    }),
};

#[inline]
pub fn phys_to_hhdm(phys: u64) -> *mut u8 {
    (PMM_META.hhdm_offset.load(Ordering::Relaxed) + phys) as *mut u8
}

#[inline]
pub fn bootstrap_state() -> BootstrapState {
    match BOOTSTRAP_ALLOC.state.load(Ordering::Acquire) {
        1 => BootstrapState::Active,
        2 => BootstrapState::Sealed,
        _ => BootstrapState::Uninit,
    }
}

/// Attempt an early-boot page-aligned physical allocation from `BootstrapAllocator`.
/// Once `init_pmm` seals the bootstrap phase, every call deterministically returns `Err(BootstrapSealed)`.
pub fn bootstrap_alloc_pages(page_count: usize, align_pages: usize) -> Result<u64, PmmError> {
    if bootstrap_state() != BootstrapState::Active {
        return Err(PmmError::BootstrapSealed);
    }
    let align_bytes = (align_pages.max(1) * PAGE_SIZE) as u64;
    let size_bytes = (page_count * PAGE_SIZE) as u64;
    // SAFETY: `BootstrapState::Active` only exists during single-threaded BSP `init_pmm`.
    let phys = unsafe {
        let inner = &mut *BOOTSTRAP_ALLOC.inner.get();
        let aligned = (inner.cursor_phys + align_bytes - 1) & !(align_bytes - 1);
        let next = aligned
            .checked_add(size_bytes)
            .ok_or(PmmError::BootstrapExhausted)?;
        if next > inner.limit_phys {
            return Err(PmmError::BootstrapExhausted);
        }
        if inner.carved_start_pfn == inner.carved_end_pfn {
            inner.carved_start_pfn = (aligned >> PAGE_SHIFT) as u32;
        }
        inner.cursor_phys = next;
        inner.carved_end_pfn = (next >> PAGE_SHIFT) as u32;
        aligned
    };
    let hhdm_ptr = phys_to_hhdm(phys);
    for i in 0..page_count {
        // SAFETY: `[phys .. phys + size_bytes)` lies inside a verified `LIMINE_MEMMAP_USABLE` region.
        unsafe { zero_frame_4k(hhdm_ptr.add(i * PAGE_SIZE)) };
    }
    Ok(phys)
}

#[inline]
pub(crate) fn frame_meta(pfn: u32) -> Option<&'static FrameMeta> {
    let max_pfn = PMM_META.max_tracked_pfn.load(Ordering::Relaxed);
    if pfn >= max_pfn {
        return None;
    }
    let base = PMM_META.meta_base_ptr.load(Ordering::Relaxed) as *const FrameMeta;
    // SAFETY: `FrameMeta[0..max_pfn]` was carved and initialized permanently during `init_pmm`.
    Some(unsafe { &*base.add(pfn as usize) })
}

#[inline]
pub fn query_frame_state(phys_addr: u64) -> FrameState {
    let pfn_u64 = phys_addr >> PAGE_SHIFT;
    if pfn_u64 >= (PMM_META.max_tracked_pfn.load(Ordering::Acquire) as u64) {
        return FrameState::ReservedOrMmio;
    }
    let (state, _, _, _, _, _, _) = unpack_word0(frame_meta(pfn_u64 as u32).unwrap().word0.load(Ordering::Acquire));
    state
}

#[inline]
pub fn inventory() -> PmmInventory {
    assert!(PMM_META.initialized.load(Ordering::Acquire), "PMM not initialized");
    // SAFETY: Immutable after `PMM_META.initialized` is published.
    unsafe { *PMM_META.inventory.get() }
}

/// Build the Limine memory inventory, carve & seal `BootstrapAllocator` `FrameMeta`, and return `(inv, head_pfn)`.
pub(crate) fn build_inventory_and_meta(mem: &MemorySummary) -> (PmmInventory, u32) {
    assert!(!PMM_META.initialized.load(Ordering::Acquire), "init_pmm called twice");
    PMM_META.hhdm_offset.store(mem.hhdm_offset, Ordering::Release);

    let mm_resp = MEMMAP_REQ.read().response;
    assert!(!mm_resp.is_null(), "Limine memmap null");
    // SAFETY: Validated previously in `verify_memory_and_hhdm`.
    let (entry_count, entries_ptr) = unsafe { ((*mm_resp).entry_count as usize, (*mm_resp).entries) };

    let mut max_ram_end: u64 = 0;
    let mut best_boot_base: u64 = 0;
    let mut best_boot_end: u64 = 0;
    let mut reserved_high_frames: u64 = 0;

    for idx in 0..entry_count {
        // SAFETY: `idx < entry_count` in Limine memmap response.
        let e = unsafe { &**entries_ptr.add(idx) };
        let end = e.base + e.length;
        if matches!(
            e.kind,
            LIMINE_MEMMAP_USABLE
                | LIMINE_MEMMAP_BOOTLOADER_RECLAIMABLE
                | LIMINE_MEMMAP_EXECUTABLE_AND_MODULES
        ) {
            if end > max_ram_end {
                max_ram_end = end;
            }
            if e.kind == LIMINE_MEMMAP_USABLE {
                let cand_start = ((e.base.max(0x10_0000)) + 0xfff) & !0xfff;
                let cand_end = end & !0xfff;
                if cand_end > cand_start && (cand_end - cand_start) > (best_boot_end - best_boot_base) {
                    best_boot_base = cand_start;
                    best_boot_end = cand_end;
                }
            }
        }
    }

    let max_tracked_pfn_u64 = (max_ram_end + (PAGE_SIZE as u64 - 1)) >> PAGE_SHIFT;
    assert!(max_tracked_pfn_u64 > 0 && max_tracked_pfn_u64 < (PFN_NONE as u64));
    let max_tracked_pfn = max_tracked_pfn_u64 as u32;
    assert!((LAPIC_MMIO_PHYS_BASE >> PAGE_SHIFT) >= max_tracked_pfn_u64);

    let meta_bytes = (max_tracked_pfn as usize) * size_of::<FrameMeta>();
    let meta_pages = meta_bytes.div_ceil(PAGE_SIZE);
    // SAFETY: Single-threaded BSP initialization before SMP bring-up.
    unsafe {
        let inner = &mut *BOOTSTRAP_ALLOC.inner.get();
        inner.cursor_phys = best_boot_base;
        inner.limit_phys = best_boot_end;
        inner.carved_start_pfn = 0;
        inner.carved_end_pfn = 0;
    }
    BOOTSTRAP_ALLOC.state.store(BootstrapState::Active as u8, Ordering::Release);
    let meta_phys = bootstrap_alloc_pages(meta_pages, 1).expect("Bootstrap FrameMeta alloc failed");
    BOOTSTRAP_ALLOC.state.store(BootstrapState::Sealed as u8, Ordering::Release);
    assert_eq!(bootstrap_alloc_pages(1, 1), Err(PmmError::BootstrapSealed));

    let (carved_start, carved_end) = unsafe {
        let inner = &*BOOTSTRAP_ALLOC.inner.get();
        (inner.carved_start_pfn, inner.carved_end_pfn)
    };

    PMM_META.meta_base_ptr.store(phys_to_hhdm(meta_phys) as u64, Ordering::Release);
    PMM_META.max_tracked_pfn.store(max_tracked_pfn, Ordering::Release);

    let default_reserved_w0 = pack_word0(
        FrameState::ReservedOrMmio,
        ZeroState::Dirty,
        0,
        NEVER_OWNED_DOMAIN,
        0,
        0,
        1,
    );
    for pfn in 0..max_tracked_pfn {
        let m = frame_meta(pfn).unwrap();
        m.word0.store(default_reserved_w0, Ordering::Relaxed);
        m.word1.store(0, Ordering::Relaxed);
    }

    let kern_label_packed = pack_seclabel(SecLabel::new(SecDomain::Kernel, SecFlags::NONE, 0, 0));
    let mut usable_frames = 0u64;
    let mut free_init_frames = 0u64;
    let mut reclaimable_frames = 0u64;
    let mut kernel_meta_frames = 0u64;
    let mut head_pfn = PFN_NONE;

    for idx in (0..entry_count).rev() {
        // SAFETY: `idx < entry_count` in Limine memmap response.
        let e = unsafe { &**entries_ptr.add(idx) };
        let end = e.base + e.length;
        if e.base >= max_ram_end {
            reserved_high_frames += e.length.div_ceil(PAGE_SIZE as u64);
            continue;
        }
        let start_pfn = ((e.base + 0xfff) >> PAGE_SHIFT) as u32;
        let end_pfn = ((end >> PAGE_SHIFT) as u32).min(max_tracked_pfn);
        if end_pfn <= start_pfn {
            continue;
        }
        match e.kind {
            LIMINE_MEMMAP_USABLE => {
                for pfn in (start_pfn..end_pfn).rev() {
                    if pfn < LOW_MEM_GUARD_END_PFN
                        || ((pfn as u64) << PAGE_SHIFT) == LAPIC_MMIO_PHYS_BASE
                    {
                        continue;
                    }
                    usable_frames += 1;
                    let m = frame_meta(pfn).unwrap();
                    if pfn >= carved_start && pfn < carved_end {
                        m.word0.store(
                            pack_word0(
                                FrameState::KernelImageOrBootMeta,
                                ZeroState::VerifiedZero,
                                0,
                                SecDomain::Kernel as u8,
                                0,
                                0,
                                1,
                            ),
                            Ordering::Relaxed,
                        );
                        m.word1.store(kern_label_packed, Ordering::Relaxed);
                        kernel_meta_frames += 1;
                    } else {
                        m.word0.store(
                            pack_word0(
                                FrameState::FreeGlobal,
                                ZeroState::Dirty,
                                0,
                                NEVER_OWNED_DOMAIN,
                                0,
                                0,
                                1,
                            ),
                            Ordering::Relaxed,
                        );
                        m.word1.store(pack_free_word1(head_pfn, 0xffff, NO_CPU_OWNER), Ordering::Relaxed);
                        head_pfn = pfn;
                        free_init_frames += 1;
                    }
                }
            }
            LIMINE_MEMMAP_BOOTLOADER_RECLAIMABLE => {
                for pfn in start_pfn..end_pfn {
                    if pfn < LOW_MEM_GUARD_END_PFN {
                        continue;
                    }
                    let m = frame_meta(pfn).unwrap();
                    m.word0.store(
                        pack_word0(FrameState::BootloaderReclaimable, ZeroState::Dirty, 0, SecDomain::Kernel as u8, 0, 0, 1),
                        Ordering::Relaxed,
                    );
                    m.word1.store(kern_label_packed, Ordering::Relaxed);
                    reclaimable_frames += 1;
                }
            }
            LIMINE_MEMMAP_EXECUTABLE_AND_MODULES => {
                for pfn in start_pfn..end_pfn {
                    let m = frame_meta(pfn).unwrap();
                    m.word0.store(
                        pack_word0(FrameState::KernelImageOrBootMeta, ZeroState::Dirty, 0, SecDomain::Kernel as u8, 0, 0, 1),
                        Ordering::Relaxed,
                    );
                    m.word1.store(kern_label_packed, Ordering::Relaxed);
                    kernel_meta_frames += 1;
                }
            }
            LIMINE_MEMMAP_RESERVED
            | LIMINE_MEMMAP_ACPI_RECLAIMABLE
            | LIMINE_MEMMAP_ACPI_NVS
            | LIMINE_MEMMAP_BAD_MEMORY
            | LIMINE_MEMMAP_FRAMEBUFFER => {}
            _ => {}
        }
    }

    let mut reserved_low_or_holes = 0u64;
    for pfn in 0..max_tracked_pfn {
        let (st, _, _, _, _, _, _) = unpack_word0(frame_meta(pfn).unwrap().word0.load(Ordering::Relaxed));
        if st == FrameState::ReservedOrMmio {
            reserved_low_or_holes += 1;
        }
    }
    assert_eq!(
        free_init_frames + reclaimable_frames + kernel_meta_frames + reserved_low_or_holes,
        max_tracked_pfn as u64,
        "Tracked PFN inventory conservation mismatch"
    );

    let total_tracked_bytes = (max_tracked_pfn as u64) * (PAGE_SIZE as u64);
    let meta_bytes_aligned = (meta_pages * PAGE_SIZE) as u64;
    let inv = PmmInventory {
        hhdm_offset: mem.hhdm_offset,
        max_tracked_pfn,
        usable_frames,
        free_init_frames,
        reclaimable_frames,
        kernel_meta_frames,
        reserved_mmio_frames: reserved_low_or_holes + reserved_high_frames,
        meta_bytes: meta_bytes_aligned,
        meta_overhead_bps: (meta_bytes_aligned * 10_000) / total_tracked_bytes,
    };
    // SAFETY: Single-threaded BSP initialization.
    unsafe {
        *PMM_META.inventory.get() = inv;
    }
    (inv, head_pfn)
}
