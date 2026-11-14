// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! x86-64 4-Level Virtual Address Space (`VSpace`) Core, `CR3` Context Switching, and 4 KiB
//! `map_4k / unmap_4k / protect_4k` Page-Table Operations (`Milestone 1B-B`).

pub mod huge;
pub mod kernel_handover;
pub mod pte;
pub mod tests;
pub mod tlb;
pub mod tlb_debt;

pub use kernel_handover::{
    activate_kernel_vspace_on_ap, init_and_activate_kernel_vspace, KernelVSpaceSummary,
};

use core::cell::UnsafeCell;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{
    fence, AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};

use crate::arch::{irq_restore, irq_save_cli, write_cr3};
use crate::gdt_tss::MAX_CPUS;
use crate::percpu::read_gs_u64_at_8;
use crate::pmm::{
    adjust_frame_pins_and_maps, alloc_pages, free_pages, map_ref_dec_data_pfn_after_shootdown,
    map_ref_inc_data_frame, phys_to_hhdm, pt_frame_adjust_entries, pt_frame_unpin_for_reclaim,
    AllocFlags, FrameHandle, PageOrder, PAGE_SHIFT,
};
use crate::security::{SecDomain, SecFlags, SecLabel};
use crate::vspace::pte::{
    is_canonical_48, prot_to_pte_flags, pt_indices, validate_user_range, VSpaceError, VmProt,
    PAGE_SIZE_4K, PTE_HUGE_PAGE, PTE_NX, PTE_PHYS_MASK_2M, PTE_PHYS_MASK_4K, PTE_PRESENT,
    PTE_SW_MMIO, PTE_USER, PTE_USER_TABLE_INTERMEDIATE, PTE_WRITABLE,
};

pub type VSpaceId = u16;
pub const KERNEL_VSPACE_ID: VSpaceId = 0;
pub const MAX_VSPACES: usize = 32;
const NO_CPU_LOCK: usize = usize::MAX;
const KERNEL_PT_LABEL: SecLabel = SecLabel::new(SecDomain::Kernel, SecFlags::NONE, 0, 0);
pub(crate) const BOOT_CR3_VSPACE_SENTINEL: VSpaceId = u16::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VSpaceHandle {
    pub id: VSpaceId,
    pub generation: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedLeafPte {
    pub phys_base: u64,
    pub raw_leaf: u64,
    pub effective_user: bool,
    pub effective_writable: bool,
    pub effective_exec: bool,
    pub is_huge_2m: bool,
}

pub(crate) struct VSpaceSlot {
    pub allocated: AtomicBool,
    pub locked: AtomicBool,
    pub lock_owner_cpu: AtomicUsize,
    pub generation: AtomicU32,
    pub pml4_pfn: AtomicU32,
    pub pml4_gen: AtomicU32,
    pub cr3_phys: AtomicU64,
    pub active_cpus: AtomicU64,
    pub mapped_4k_equiv_pages: AtomicU64,
    pub pt_frames_owned: AtomicU64,
    pub owner_label: UnsafeCell<SecLabel>,
}

// SAFETY: Mutable page-table state and `owner_label` are mutated solely under `locked` (`CLI` held).
unsafe impl Sync for VSpaceSlot {}

impl VSpaceSlot {
    const fn new() -> Self {
        Self {
            allocated: AtomicBool::new(false),
            locked: AtomicBool::new(false),
            lock_owner_cpu: AtomicUsize::new(NO_CPU_LOCK),
            generation: AtomicU32::new(1),
            pml4_pfn: AtomicU32::new(0),
            pml4_gen: AtomicU32::new(0),
            cr3_phys: AtomicU64::new(0),
            active_cpus: AtomicU64::new(0),
            mapped_4k_equiv_pages: AtomicU64::new(0),
            pt_frames_owned: AtomicU64::new(0),
            owner_label: UnsafeCell::new(KERNEL_PT_LABEL),
        }
    }
}

#[allow(clippy::declare_interior_mutable_const)]
const INIT_VSPACE_SLOT: VSpaceSlot = VSpaceSlot::new();
pub(crate) static VSPACE_TABLE: [VSpaceSlot; MAX_VSPACES] = [INIT_VSPACE_SLOT; MAX_VSPACES];

#[allow(clippy::declare_interior_mutable_const)]
const INIT_CPU_VS: AtomicU16 = AtomicU16::new(BOOT_CR3_VSPACE_SENTINEL);
pub(crate) static CPU_ACTIVE_VSPACE: [AtomicU16; MAX_CPUS] = [INIT_CPU_VS; MAX_CPUS];

#[allow(clippy::declare_interior_mutable_const)]
const ZERO_CR3: AtomicU64 = AtomicU64::new(0);
pub(crate) static CPU_ACTIVE_CR3: [AtomicU64; MAX_CPUS] = [ZERO_CR3; MAX_CPUS];

pub(crate) static BOOT_FRAMES_RECLAIMED: AtomicU64 = AtomicU64::new(0);

pub fn vspace_invariant_panic(args: core::fmt::Arguments<'_>) -> ! {
    crate::enter_fatal_or_halt();
    crate::serial::emergency_print(format_args!("\r\n[DRY-VSPACE-FAIL] {}\r\n", args));
    panic!("VSpace invariant violation");
}

#[inline]
pub fn cpu_active_vspace(cpu_idx: usize) -> VSpaceId {
    CPU_ACTIVE_VSPACE[cpu_idx].load(Ordering::Acquire)
}

#[inline]
pub fn cr3_perf_counters() -> (u64, u64) {
    let mut switches = 0u64;
    let mut noops = 0u64;
    for cpu in 0..MAX_CPUS {
        let pcpu = crate::percpu::get_percpu_by_idx(cpu);
        switches = switches.wrapping_add(pcpu.cr3_switches_local.load(Ordering::Acquire));
        noops = noops.wrapping_add(pcpu.cr3_noops_local.load(Ordering::Acquire));
    }
    (switches, noops)
}

#[inline]
pub fn reset_cr3_perf_counters() {
    for cpu in 0..MAX_CPUS {
        let pcpu = crate::percpu::get_percpu_by_idx(cpu);
        pcpu.cr3_switches_local.store(0, Ordering::Release);
        pcpu.cr3_noops_local.store(0, Ordering::Release);
    }
}

#[inline]
fn assert_no_vspace_lock_held_by_cpu(cpu_idx: usize) {
    for (idx, slot) in VSPACE_TABLE.iter().enumerate() {
        if slot.lock_owner_cpu.load(Ordering::Relaxed) == cpu_idx {
            vspace_invariant_panic(format_args!(
                "Lock-order violation: nested VSpace lock ({}) requested on cpu={}",
                idx, cpu_idx
            ));
        }
    }
}

pub(crate) fn with_vspace_locked<R>(
    handle: VSpaceHandle,
    f: impl FnOnce(&VSpaceSlot, SecLabel) -> Result<R, VSpaceError>,
) -> Result<R, VSpaceError> {
    let idx = handle.id as usize;
    if idx == 0 || idx >= MAX_VSPACES {
        return Err(VSpaceError::InvalidVSpace);
    }
    let slot = &VSPACE_TABLE[idx];
    let saved_if = irq_save_cli();
    let cpu_idx = read_gs_u64_at_8() as usize;
    assert_no_vspace_lock_held_by_cpu(cpu_idx);
    tlb::assert_tlb_req_lock_not_held_by_cpu(cpu_idx);

    while slot
        .locked
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        if crate::is_panicking() {
            crate::arch::halt_forever();
        }
        tlb::poll_and_ack_pending_shootdown(cpu_idx);
        core::hint::spin_loop();
    }
    slot.lock_owner_cpu.store(cpu_idx, Ordering::Relaxed);

    let res = if !slot.allocated.load(Ordering::Acquire)
        || slot.generation.load(Ordering::Acquire) != handle.generation
    {
        Err(VSpaceError::InvalidVSpace)
    } else {
        // SAFETY: Protected by `slot.locked`.
        let owner = unsafe { *slot.owner_label.get() };
        f(slot, owner)
    };

    slot.lock_owner_cpu.store(NO_CPU_LOCK, Ordering::Relaxed);
    slot.locked.store(false, Ordering::Release);
    irq_restore(saved_if);
    res
}

pub(crate) fn alloc_pinned_pt_frame() -> Result<FrameHandle, VSpaceError> {
    let h = alloc_pages(PageOrder::Order4KiB, KERNEL_PT_LABEL, AllocFlags::ZERO)
        .map_err(|_| VSpaceError::OutOfMemory)?;
    adjust_frame_pins_and_maps(h.pfn, h.generation, 1, 0);
    Ok(h)
}

#[inline]
pub(crate) fn pt_virt_ptr(pfn: u32) -> *mut u64 {
    phys_to_hhdm((pfn as u64) << PAGE_SHIFT) as *mut u64
}

/// Scheduler `CR3` context-switching hook invoked inside `complete_post_switch_from_asm` (`CLI` held).
///
/// - **Kernel-to-Kernel / Same-`VSpace` Fast Path:** Zero `mov cr3`, zero cross-CPU atomics.
/// - **Cross-`VSpace` Switch (`old_vs -> next_vs`):**
///   1. Sets `cpu_idx` bit in `next_vs.active_cpus` (`SeqCst`) *before* `write_cr3`.
///   2. Executes `write_cr3(next_cr3)`.
///   3. Clears `cpu_idx` bit in `old_vs.active_cpus` (`SeqCst`) *after* `write_cr3` and satisfies
///      any in-flight shootdown targeting `old_vs` on `cpu_idx`.
#[inline]
pub fn switch_cpu_vspace_if_needed(cpu_idx: usize, next_vs: VSpaceId) {
    let cur_vs = CPU_ACTIVE_VSPACE[cpu_idx].load(Ordering::Relaxed);
    let pcpu = crate::percpu::get_percpu_by_idx(cpu_idx);
    if cur_vs == BOOT_CR3_VSPACE_SENTINEL || cur_vs == next_vs {
        // CPU-local non-LOCKed increment (`CLI` held on `cpu_idx`): zero global atomic contention!
        let prev = pcpu.cr3_noops_local.load(Ordering::Relaxed);
        pcpu.cr3_noops_local.store(prev + 1, Ordering::Relaxed);
        return;
    }

    let next_idx = next_vs as usize;
    assert!(next_idx < MAX_VSPACES, "Invalid target VSpaceId {}", next_vs);
    let next_slot = &VSPACE_TABLE[next_idx];
    assert!(
        next_slot.allocated.load(Ordering::Acquire),
        "Attempted CR3 switch to unallocated VSpace {}",
        next_vs
    );

    let bit = 1u64 << cpu_idx;
    next_slot.active_cpus.fetch_or(bit, Ordering::SeqCst);
    let next_cr3 = next_slot.cr3_phys.load(Ordering::Acquire);

    // SAFETY: `next_cr3` shares identical supervisor-only `PML4[256..512]` mappings (`GDT/TSS/IST1`,
    // per-CPU stacks, `GS_BASE`, `IDT`, `.text/.rodata/.data/.bss`, HHDM, and Local APIC).
    unsafe { write_cr3(next_cr3) };
    CPU_ACTIVE_CR3[cpu_idx].store(next_cr3, Ordering::Release);
    CPU_ACTIVE_VSPACE[cpu_idx].store(next_vs, Ordering::Release);
    let prev_sw = pcpu.cr3_switches_local.load(Ordering::Relaxed);
    pcpu.cr3_switches_local.store(prev_sw + 1, Ordering::Relaxed);

    if (cur_vs as usize) < MAX_VSPACES {
        VSPACE_TABLE[cur_vs as usize]
            .active_cpus
            .fetch_and(!bit, Ordering::SeqCst);
        if cur_vs != KERNEL_VSPACE_ID {
            tlb::ack_if_leaving_vspace(cpu_idx, cur_vs);
        }
    }
}

/// Allocate an isolated `VSpace` (`PML4[0..256]` zeroed, `PML4[256..512]` cloned from `KERNEL_VSPACE`).
pub fn create_vspace(owner_label: SecLabel) -> Result<VSpaceHandle, VSpaceError> {
    let pml4_h = alloc_pinned_pt_frame()?;
    let kpml4_pfn = VSPACE_TABLE[0].pml4_pfn.load(Ordering::Acquire);

    // SAFETY: `pml4_h` is freshly zeroed and exclusively owned; `kpml4_pfn` upper half is immutable.
    unsafe {
        let dst = pt_virt_ptr(pml4_h.pfn);
        let src = pt_virt_ptr(kpml4_pfn);
        for i in 256..512 {
            let entry = read_volatile(src.add(i));
            debug_assert_eq!(entry & PTE_USER, 0);
            write_volatile(dst.add(i), entry);
        }
    }

    for (idx, slot) in VSPACE_TABLE.iter().enumerate().skip(1) {
        if slot
            .allocated
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let gen = slot.generation.load(Ordering::Acquire);
            slot.pml4_pfn.store(pml4_h.pfn, Ordering::Relaxed);
            slot.pml4_gen.store(pml4_h.generation, Ordering::Relaxed);
            slot.cr3_phys.store(pml4_h.phys_addr(), Ordering::Release);
            slot.active_cpus.store(0, Ordering::Release);
            slot.mapped_4k_equiv_pages.store(0, Ordering::Release);
            slot.pt_frames_owned.store(0, Ordering::Release);
            // SAFETY: Slot just transitioned `false -> true`; no concurrent reader holds valid generation yet.
            unsafe { *slot.owner_label.get() = owner_label };
            return Ok(VSpaceHandle {
                id: idx as VSpaceId,
                generation: gen,
            });
        }
    }

    let unpinned = pt_frame_unpin_for_reclaim(pml4_h.pfn);
    free_pages(unpinned, PageOrder::Order4KiB, KERNEL_PT_LABEL);
    Err(VSpaceError::OutOfMemory)
}

/// Destroy an inactive user `VSpace`, unmapping any residual user mappings and reclaiming all
/// owned intermediate `PT/PD/PDPT` frames plus its root `PML4` frame.
pub fn destroy_vspace(handle: VSpaceHandle) -> Result<(), VSpaceError> {
    let pml4_pfn = with_vspace_locked(handle, |slot, _| {
        if slot.active_cpus.load(Ordering::SeqCst) != 0 {
            return Err(VSpaceError::VSpaceActiveBusy);
        }
        let pml4 = slot.pml4_pfn.load(Ordering::Acquire);
        // SAFETY: `active_cpus == 0` and `slot.locked` held -> zero CPUs have this `VSpace`'s
        // non-global TLB entries cached (`CR4.PCIDE == 0`).
        unsafe {
            let pml4_ptr = pt_virt_ptr(pml4);
            for i4 in 0..256 {
                let e4 = read_volatile(pml4_ptr.add(i4));
                if (e4 & PTE_PRESENT) == 0 {
                    continue;
                }
                write_volatile(pml4_ptr.add(i4), 0);
                let pdpt_pfn = ((e4 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;
                let pdpt_ptr = pt_virt_ptr(pdpt_pfn);
                for i3 in 0..512 {
                    let e3 = read_volatile(pdpt_ptr.add(i3));
                    if (e3 & PTE_PRESENT) == 0 {
                        continue;
                    }
                    write_volatile(pdpt_ptr.add(i3), 0);
                    let pd_pfn = ((e3 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;
                    let pd_ptr = pt_virt_ptr(pd_pfn);
                    for i2 in 0..512 {
                        let e2 = read_volatile(pd_ptr.add(i2));
                        if (e2 & PTE_PRESENT) == 0 {
                            continue;
                        }
                        write_volatile(pd_ptr.add(i2), 0);
                        if (e2 & PTE_HUGE_PAGE) != 0 {
                            let base_pfn = ((e2 & PTE_PHYS_MASK_2M) >> PAGE_SHIFT) as u32;
                            let _ = map_ref_dec_data_pfn_after_shootdown(base_pfn, PageOrder::Order2MiB);
                            let _ = pt_frame_adjust_entries(pd_pfn, -1);
                        } else {
                            let pt_pfn = ((e2 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;
                            let pt_ptr = pt_virt_ptr(pt_pfn);
                            for i1 in 0..512 {
                                let e1 = read_volatile(pt_ptr.add(i1));
                                if (e1 & PTE_PRESENT) != 0 {
                                    write_volatile(pt_ptr.add(i1), 0);
                                    if (e1 & PTE_SW_MMIO) == 0 {
                                        let dpfn = ((e1 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;
                                        let _ = map_ref_dec_data_pfn_after_shootdown(dpfn, PageOrder::Order4KiB);
                                    }
                                    let _ = pt_frame_adjust_entries(pt_pfn, -1);
                                }
                            }
                            let pt_h = pt_frame_unpin_for_reclaim(pt_pfn);
                            free_pages(pt_h, PageOrder::Order4KiB, KERNEL_PT_LABEL);
                            slot.pt_frames_owned.fetch_sub(1, Ordering::Relaxed);
                            let _ = pt_frame_adjust_entries(pd_pfn, -1);
                        }
                    }
                    let pd_h = pt_frame_unpin_for_reclaim(pd_pfn);
                    free_pages(pd_h, PageOrder::Order4KiB, KERNEL_PT_LABEL);
                    slot.pt_frames_owned.fetch_sub(1, Ordering::Relaxed);
                    let _ = pt_frame_adjust_entries(pdpt_pfn, -1);
                }
                let pdpt_h = pt_frame_unpin_for_reclaim(pdpt_pfn);
                free_pages(pdpt_h, PageOrder::Order4KiB, KERNEL_PT_LABEL);
                slot.pt_frames_owned.fetch_sub(1, Ordering::Relaxed);
                let _ = pt_frame_adjust_entries(pml4, -1);
            }
        }
        assert_eq!(slot.pt_frames_owned.load(Ordering::Relaxed), 0);
        slot.mapped_4k_equiv_pages.store(0, Ordering::Release);
        slot.cr3_phys.store(0, Ordering::Release);
        let next_gen = slot.generation.load(Ordering::Relaxed).wrapping_add(1).max(1);
        slot.generation.store(next_gen, Ordering::Release);
        slot.allocated.store(false, Ordering::Release);
        Ok(pml4)
    })?;

    let pml4_h = pt_frame_unpin_for_reclaim(pml4_pfn);
    free_pages(pml4_h, PageOrder::Order4KiB, KERNEL_PT_LABEL);
    Ok(())
}

/// Map a 4 KiB physical `frame` at `vaddr` in `handle` with `prot` (`W^X` enforced by default).
pub fn map_4k(
    handle: VSpaceHandle,
    vaddr: u64,
    frame: FrameHandle,
    prot: VmProt,
) -> Result<(), VSpaceError> {
    map_4k_internal(handle, vaddr, frame, prot, false)
}

pub(crate) fn map_4k_cap_authorized(
    handle: VSpaceHandle,
    vaddr: u64,
    frame: FrameHandle,
    prot: VmProt,
) -> Result<(), VSpaceError> {
    map_4k_internal(handle, vaddr, frame, prot, true)
}

fn map_4k_internal(
    handle: VSpaceHandle,
    vaddr: u64,
    frame: FrameHandle,
    prot: VmProt,
    cap_authorized: bool,
) -> Result<(), VSpaceError> {
    validate_user_range(vaddr, PAGE_SIZE_4K, PAGE_SIZE_4K)?;
    let leaf_flags = prot_to_pte_flags(prot, false)?;
    let (i4, i3, i2, i1) = pt_indices(vaddr);

    with_vspace_locked(handle, |slot, owner_label| {
        let pml4_pfn = slot.pml4_pfn.load(Ordering::Acquire);
        // SAFETY: `slot.locked` grants exclusive mutation of `PML4[0..256]`.
        unsafe {
            let pml4 = pt_virt_ptr(pml4_pfn);
            let mut e4 = read_volatile(pml4.add(i4));
            if (e4 & PTE_PRESENT) == 0 {
                let pdpt = alloc_pinned_pt_frame()?;
                slot.pt_frames_owned.fetch_add(1, Ordering::Relaxed);
                let _ = pt_frame_adjust_entries(pml4_pfn, 1);
                e4 = pdpt.phys_addr() | PTE_USER_TABLE_INTERMEDIATE;
                write_volatile(pml4.add(i4), e4);
            }
            let pdpt_pfn = ((e4 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;
            let pdpt = pt_virt_ptr(pdpt_pfn);
            let mut e3 = read_volatile(pdpt.add(i3));
            if (e3 & PTE_PRESENT) == 0 {
                let pd = alloc_pinned_pt_frame()?;
                slot.pt_frames_owned.fetch_add(1, Ordering::Relaxed);
                let _ = pt_frame_adjust_entries(pdpt_pfn, 1);
                e3 = pd.phys_addr() | PTE_USER_TABLE_INTERMEDIATE;
                write_volatile(pdpt.add(i3), e3);
            }
            let pd_pfn = ((e3 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;
            let pd = pt_virt_ptr(pd_pfn);
            let mut e2 = read_volatile(pd.add(i2));
            if (e2 & (PTE_PRESENT | PTE_HUGE_PAGE)) == (PTE_PRESENT | PTE_HUGE_PAGE) {
                return Err(VSpaceError::AlreadyMapped);
            }
            if (e2 & PTE_PRESENT) == 0 {
                let pt = alloc_pinned_pt_frame()?;
                slot.pt_frames_owned.fetch_add(1, Ordering::Relaxed);
                let _ = pt_frame_adjust_entries(pd_pfn, 1);
                e2 = pt.phys_addr() | PTE_USER_TABLE_INTERMEDIATE;
                write_volatile(pd.add(i2), e2);
            }
            let pt_pfn = ((e2 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;
            let pt = pt_virt_ptr(pt_pfn);
            if (read_volatile(pt.add(i1)) & PTE_PRESENT) != 0 {
                return Err(VSpaceError::AlreadyMapped);
            }

            map_ref_inc_data_frame(
                frame,
                PageOrder::Order4KiB,
                owner_label,
                prot.contains(VmProt::WRITE),
                cap_authorized,
            );
            let _ = pt_frame_adjust_entries(pt_pfn, 1);
            write_volatile(pt.add(i1), frame.phys_addr() | leaf_flags);
            slot.mapped_4k_equiv_pages.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    })
}

/// Map a validated device MMIO 4 KiB physical page (`FrameState::ReservedOrMmio`) at `vaddr`.
/// Strictly rejects RAM/Kernel/PMM frames (`INV-SEC-01`) and `EXEC` (`W^X`).
pub fn map_mmio_4k(
    handle: VSpaceHandle,
    vaddr: u64,
    phys_addr: u64,
    prot: VmProt,
) -> Result<(), VSpaceError> {
    validate_user_range(vaddr, PAGE_SIZE_4K, PAGE_SIZE_4K)?;
    if (phys_addr & (PAGE_SIZE_4K - 1)) != 0 {
        return Err(VSpaceError::UnalignedAddress);
    }
    let max_pa = pte::cpu_max_phys_addr_limit();
    if phys_addr
        .checked_add(PAGE_SIZE_4K - 1)
        .is_none_or(|end_pa| end_pa > max_pa)
    {
        return Err(VSpaceError::AddressOverflow);
    }
    if prot.contains(VmProt::EXEC) {
        return Err(VSpaceError::WxViolation);
    }
    if crate::pmm::query_frame_state(phys_addr) != crate::pmm::FrameState::ReservedOrMmio {
        return Err(VSpaceError::PermissionViolation);
    }
    let leaf_flags = prot_to_pte_flags(prot, false)? | PTE_SW_MMIO;
    let (i4, i3, i2, i1) = pt_indices(vaddr);

    with_vspace_locked(handle, |slot, _| {
        let pml4_pfn = slot.pml4_pfn.load(Ordering::Acquire);
        // SAFETY: `slot.locked` grants exclusive mutation of `PML4[0..256]`.
        unsafe {
            let pml4 = pt_virt_ptr(pml4_pfn);
            let mut e4 = read_volatile(pml4.add(i4));
            if (e4 & PTE_PRESENT) == 0 {
                let pdpt = alloc_pinned_pt_frame()?;
                slot.pt_frames_owned.fetch_add(1, Ordering::Relaxed);
                let _ = pt_frame_adjust_entries(pml4_pfn, 1);
                e4 = pdpt.phys_addr() | PTE_USER_TABLE_INTERMEDIATE;
                write_volatile(pml4.add(i4), e4);
            }
            let pdpt_pfn = ((e4 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;
            let pdpt = pt_virt_ptr(pdpt_pfn);
            let mut e3 = read_volatile(pdpt.add(i3));
            if (e3 & PTE_PRESENT) == 0 {
                let pd = alloc_pinned_pt_frame()?;
                slot.pt_frames_owned.fetch_add(1, Ordering::Relaxed);
                let _ = pt_frame_adjust_entries(pdpt_pfn, 1);
                e3 = pd.phys_addr() | PTE_USER_TABLE_INTERMEDIATE;
                write_volatile(pdpt.add(i3), e3);
            }
            let pd_pfn = ((e3 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;
            let pd = pt_virt_ptr(pd_pfn);
            let mut e2 = read_volatile(pd.add(i2));
            if (e2 & (PTE_PRESENT | PTE_HUGE_PAGE)) == (PTE_PRESENT | PTE_HUGE_PAGE) {
                return Err(VSpaceError::AlreadyMapped);
            }
            if (e2 & PTE_PRESENT) == 0 {
                let pt = alloc_pinned_pt_frame()?;
                slot.pt_frames_owned.fetch_add(1, Ordering::Relaxed);
                let _ = pt_frame_adjust_entries(pd_pfn, 1);
                e2 = pt.phys_addr() | PTE_USER_TABLE_INTERMEDIATE;
                write_volatile(pd.add(i2), e2);
            }
            let pt_pfn = ((e2 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;
            let pt = pt_virt_ptr(pt_pfn);
            if (read_volatile(pt.add(i1)) & PTE_PRESENT) != 0 {
                return Err(VSpaceError::AlreadyMapped);
            }
            let _ = pt_frame_adjust_entries(pt_pfn, 1);
            write_volatile(pt.add(i1), (phys_addr & PTE_PHYS_MASK_4K) | leaf_flags);
            slot.mapped_4k_equiv_pages.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    })
}

pub(crate) struct PendingReclaim {
    pub data_pfn: u32,
    pub order: PageOrder,
    pub is_mmio: bool,
    pub pt_pfns: [u32; 3],
    pub pt_len: usize,
}

impl PendingReclaim {
    pub fn push_pt(&mut self, pfn: u32) {
        self.pt_pfns[self.pt_len] = pfn;
        self.pt_len += 1;
    }

    pub fn finalize_after_shootdown(self) -> FrameHandle {
        let data_h = if self.is_mmio {
            FrameHandle {
                pfn: self.data_pfn,
                generation: 0,
            }
        } else {
            map_ref_dec_data_pfn_after_shootdown(self.data_pfn, self.order)
        };
        for i in 0..self.pt_len {
            let pt_h = pt_frame_unpin_for_reclaim(self.pt_pfns[i]);
            free_pages(pt_h, PageOrder::Order4KiB, KERNEL_PT_LABEL);
        }
        data_h
    }
}

/// Unmap a 4 KiB page at `vaddr`, reclaim any emptied `PT/PD/PDPT` levels, execute SMP TLB
/// shootdown *outside* `VSpace::lock`, and release frame pins/maps strictly *after* shootdown ACKs.
pub fn unmap_4k(handle: VSpaceHandle, vaddr: u64) -> Result<FrameHandle, VSpaceError> {
    validate_user_range(vaddr, PAGE_SIZE_4K, PAGE_SIZE_4K)?;
    let (i4, i3, i2, i1) = pt_indices(vaddr);

    let reclaim = with_vspace_locked(handle, |slot, _| {
        let pml4_pfn = slot.pml4_pfn.load(Ordering::Acquire);
        // SAFETY: `slot.locked` held.
        unsafe {
            let pml4 = pt_virt_ptr(pml4_pfn);
            let e4 = read_volatile(pml4.add(i4));
            if (e4 & PTE_PRESENT) == 0 {
                return Err(VSpaceError::NotMapped);
            }
            let pdpt_pfn = ((e4 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;
            let pdpt = pt_virt_ptr(pdpt_pfn);
            let e3 = read_volatile(pdpt.add(i3));
            if (e3 & PTE_PRESENT) == 0 {
                return Err(VSpaceError::NotMapped);
            }
            let pd_pfn = ((e3 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;
            let pd = pt_virt_ptr(pd_pfn);
            let e2 = read_volatile(pd.add(i2));
            if (e2 & PTE_PRESENT) == 0 {
                return Err(VSpaceError::NotMapped);
            }
            if (e2 & PTE_HUGE_PAGE) != 0 {
                return Err(VSpaceError::PageSizeMismatch);
            }
            let pt_pfn = ((e2 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;
            let pt = pt_virt_ptr(pt_pfn);
            let old_leaf = read_volatile(pt.add(i1));
            if (old_leaf & PTE_PRESENT) == 0 {
                return Err(VSpaceError::NotMapped);
            }

            write_volatile(pt.add(i1), 0);
            let mut pending = PendingReclaim {
                data_pfn: ((old_leaf & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32,
                order: PageOrder::Order4KiB,
                is_mmio: (old_leaf & PTE_SW_MMIO) != 0,
                pt_pfns: [0; 3],
                pt_len: 0,
            };

            if pt_frame_adjust_entries(pt_pfn, -1) == 0 {
                write_volatile(pd.add(i2), 0);
                pending.push_pt(pt_pfn);
                slot.pt_frames_owned.fetch_sub(1, Ordering::Relaxed);
                if pt_frame_adjust_entries(pd_pfn, -1) == 0 {
                    write_volatile(pdpt.add(i3), 0);
                    pending.push_pt(pd_pfn);
                    slot.pt_frames_owned.fetch_sub(1, Ordering::Relaxed);
                    if pt_frame_adjust_entries(pdpt_pfn, -1) == 0 {
                        write_volatile(pml4.add(i4), 0);
                        pending.push_pt(pdpt_pfn);
                        slot.pt_frames_owned.fetch_sub(1, Ordering::Relaxed);
                        let _ = pt_frame_adjust_entries(pml4_pfn, -1);
                    }
                }
            }
            fence(Ordering::SeqCst);
            slot.mapped_4k_equiv_pages.fetch_sub(1, Ordering::Relaxed);
            let flush_pages = if pending.pt_len > 0 {
                tlb::FULL_FLUSH_SENTINEL
            } else {
                1
            };
            let _ = tlb::shootdown_tlb(handle.id, &slot.active_cpus, vaddr, flush_pages);
            Ok(pending)
        }
    })?;

    Ok(reclaim.finalize_after_shootdown())
}

/// Update permissions on an existing 4 KiB mapping (`W^X` enforced) and synchronize TLBs.
pub fn protect_4k(handle: VSpaceHandle, vaddr: u64, new_prot: VmProt) -> Result<(), VSpaceError> {
    validate_user_range(vaddr, PAGE_SIZE_4K, PAGE_SIZE_4K)?;
    let new_flags = prot_to_pte_flags(new_prot, false)?;
    let (i4, i3, i2, i1) = pt_indices(vaddr);

    with_vspace_locked(handle, |slot, _| {
        let pml4_pfn = slot.pml4_pfn.load(Ordering::Acquire);
        // SAFETY: `slot.locked` held.
        unsafe {
            let e4 = read_volatile(pt_virt_ptr(pml4_pfn).add(i4));
            if (e4 & PTE_PRESENT) == 0 {
                return Err(VSpaceError::NotMapped);
            }
            let e3 = read_volatile(
                pt_virt_ptr(((e4 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32).add(i3),
            );
            if (e3 & PTE_PRESENT) == 0 {
                return Err(VSpaceError::NotMapped);
            }
            let pd = pt_virt_ptr(((e3 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32);
            let e2 = read_volatile(pd.add(i2));
            if (e2 & PTE_PRESENT) == 0 {
                return Err(VSpaceError::NotMapped);
            }
            if (e2 & PTE_HUGE_PAGE) != 0 {
                return Err(VSpaceError::PageSizeMismatch);
            }
            let pt = pt_virt_ptr(((e2 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32);
            let old = read_volatile(pt.add(i1));
            if (old & PTE_PRESENT) == 0 {
                return Err(VSpaceError::NotMapped);
            }
            if (old & PTE_SW_MMIO) != 0 && new_prot.contains(VmProt::EXEC) {
                return Err(VSpaceError::WxViolation);
            }
            if new_prot.contains(VmProt::WRITE) && (old & PTE_SW_MMIO) == 0 {
                let data_pfn = ((old & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;
                if matches!(
                    crate::pmm::inspect_frame_live(data_pfn),
                    Some((_, _, _, _, owner)) if owner.flags.contains(SecFlags::READ_ONLY_EXPORT)
                ) {
                    return Err(VSpaceError::PermissionViolation);
                }
            }
            let sw_mmio = old & PTE_SW_MMIO;
            write_volatile(pt.add(i1), (old & PTE_PHYS_MASK_4K) | new_flags | sw_mmio);
            fence(Ordering::SeqCst);
            let _ = tlb::shootdown_tlb(handle.id, &slot.active_cpus, vaddr, 1);
            Ok(())
        }
    })
}

/// Verify that `vaddr` in `vspace_id` is mapped with `U/S == 1` (`PTE_USER`) across all levels
/// before allowing any `Userspace`-attributed access (`ADR-007` supervisor-boundary gate).
pub fn enforce_user_access_or_panic(vspace_id: VSpaceId, vaddr: u64, write: bool, exec: bool) {
    let Some(resolved) = walk_vspace_address(vspace_id, vaddr) else {
        vspace_invariant_panic(format_args!(
            "Unmapped user access rejected: vspace={} vaddr={:#x}",
            vspace_id, vaddr
        ));
    };
    if !resolved.effective_user {
        vspace_invariant_panic(format_args!(
            "U/S permission bypass rejected: supervisor-only page at vaddr={:#x} in vspace={}",
            vaddr, vspace_id
        ));
    }
    if write && !resolved.effective_writable {
        vspace_invariant_panic(format_args!(
            "Write permission violation at vaddr={:#x} in vspace={}",
            vaddr, vspace_id
        ));
    }
    if exec && !resolved.effective_exec {
        vspace_invariant_panic(format_args!(
            "NX execution violation at vaddr={:#x} in vspace={}",
            vaddr, vspace_id
        ));
    }
}

/// Query owned intermediate page-table frame count (`PDPT + PD + PT`) for `handle`.
pub fn vspace_owned_pt_frames(handle: VSpaceHandle) -> Result<u64, VSpaceError> {
    with_vspace_locked(handle, |slot, _| Ok(slot.pt_frames_owned.load(Ordering::Relaxed)))
}

/// Lock-free inspection of `(generation, active_cpus_mask, owner_label)` for `CSpace` `VSpace` gates.
#[inline(always)]
pub fn inspect_vspace_live(vspace_id: VSpaceId) -> Option<(u32, u64, SecLabel)> {
    if vspace_id == KERNEL_VSPACE_ID || (vspace_id as usize) >= MAX_VSPACES {
        return None;
    }
    let slot = &VSPACE_TABLE[vspace_id as usize];
    if !slot.allocated.load(Ordering::Acquire) {
        return None;
    }
    let gen = slot.generation.load(Ordering::Acquire);
    let active = slot.active_cpus.load(Ordering::Acquire);
    // SAFETY: `owner_label` is written prior to setting `allocated = true` and immutable while allocated.
    let lbl = unsafe { *slot.owner_label.get() };
    Some((gen, active, lbl))
}

#[inline]
pub fn inspect_vspace_page_counts(vspace_id: VSpaceId) -> (u32, u32) {
    if vspace_id == KERNEL_VSPACE_ID || (vspace_id as usize) >= MAX_VSPACES {
        return (0, 0);
    }
    let slot = &VSPACE_TABLE[vspace_id as usize];
    if !slot.allocated.load(Ordering::Acquire) {
        return (0, 0);
    }
    let mapped = slot.mapped_4k_equiv_pages.load(Ordering::Relaxed).min(u32::MAX as u64) as u32;
    let pts = slot.pt_frames_owned.load(Ordering::Relaxed).min(u32::MAX as u64) as u32;
    (mapped, pts)
}

/// Walk `vspace_id` (`0` = `KERNEL_VSPACE`) for any canonical `vaddr` and return effective permissions.
pub fn walk_vspace_address(vspace_id: VSpaceId, vaddr: u64) -> Option<ResolvedLeafPte> {
    if !is_canonical_48(vaddr) || (vspace_id as usize) >= MAX_VSPACES {
        return None;
    }
    let slot = &VSPACE_TABLE[vspace_id as usize];
    if !slot.allocated.load(Ordering::Acquire) {
        return None;
    }
    let pml4_pfn = slot.pml4_pfn.load(Ordering::Acquire);
    let (i4, i3, i2, i1) = pt_indices(vaddr);

    // SAFETY: Read-only diagnostic page walk via HHDM.
    unsafe {
        let e4 = read_volatile(pt_virt_ptr(pml4_pfn).add(i4));
        if (e4 & PTE_PRESENT) == 0 {
            return None;
        }
        let mut eff_user = (e4 & PTE_USER) != 0;
        let mut eff_rw = (e4 & PTE_WRITABLE) != 0;
        let mut eff_exec = (e4 & PTE_NX) == 0;

        let e3 = read_volatile(pt_virt_ptr(((e4 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32).add(i3));
        if (e3 & PTE_PRESENT) == 0 {
            return None;
        }
        eff_user &= (e3 & PTE_USER) != 0;
        eff_rw &= (e3 & PTE_WRITABLE) != 0;
        eff_exec &= (e3 & PTE_NX) == 0;

        let e2 = read_volatile(pt_virt_ptr(((e3 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32).add(i2));
        if (e2 & PTE_PRESENT) == 0 {
            return None;
        }
        eff_user &= (e2 & PTE_USER) != 0;
        eff_rw &= (e2 & PTE_WRITABLE) != 0;
        eff_exec &= (e2 & PTE_NX) == 0;
        if (e2 & PTE_HUGE_PAGE) != 0 {
            return Some(ResolvedLeafPte {
                phys_base: e2 & PTE_PHYS_MASK_2M,
                raw_leaf: e2,
                effective_user: eff_user,
                effective_writable: eff_rw,
                effective_exec: eff_exec,
                is_huge_2m: true,
            });
        }

        let e1 = read_volatile(pt_virt_ptr(((e2 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32).add(i1));
        if (e1 & PTE_PRESENT) == 0 {
            return None;
        }
        eff_user &= (e1 & PTE_USER) != 0;
        eff_rw &= (e1 & PTE_WRITABLE) != 0;
        eff_exec &= (e1 & PTE_NX) == 0;
        Some(ResolvedLeafPte {
            phys_base: e1 & PTE_PHYS_MASK_4K,
            raw_leaf: e1,
            effective_user: eff_user,
            effective_writable: eff_rw,
            effective_exec: eff_exec,
            is_huge_2m: false,
        })
    }
}
