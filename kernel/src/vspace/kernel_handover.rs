// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `KERNEL_VSPACE` (`id = 0`) Construction, All-CPU `CR3` Handoff & Safe `BootloaderReclaimable`
//! Reclamation (`Milestone 1B-B` Reqs #3, #6, #10).

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::Ordering;

use crate::apic::apic_phys_base;
use crate::arch::{read_cr3, write_cr3};
use crate::gdt_tss::verify_cpu_tss_and_canaries;
use crate::idt::verify_breakpoint_trap_recovery;
use crate::mem_verify::MemorySummary;
use crate::percpu::get_percpu_by_idx;
use crate::pmm::{
    inventory, pt_frame_adjust_entries, query_frame_state, reclaim_bootloader_frames_raw,
    FrameState, PAGE_SHIFT,
};
use crate::sched::{set_ap_epoch_kind, ApEpochKind, AP_PHASE_DONE_CPUS, AP_PHASE_EPOCH};
use crate::vspace::pte::{
    detect_and_enable_cpu_paging_caps, prot_to_pte_flags, pt_indices, vtd_unit_vaddr,
    PagingHwCaps, VSpaceError, VmProt, KERNEL_VTD_MAX_PAGES, PAGE_SIZE_4K, PTE_HUGE_PAGE,
    PTE_KERNEL_TABLE_INTERMEDIATE, PTE_NX, PTE_PHYS_MASK_4K, PTE_PRESENT, PTE_USER, PTE_WRITABLE,
};
use crate::vspace::{
    alloc_pinned_pt_frame, pt_virt_ptr, tlb, BOOT_CR3_VSPACE_SENTINEL, BOOT_FRAMES_RECLAIMED,
    CPU_ACTIVE_CR3, CPU_ACTIVE_VSPACE, KERNEL_VSPACE_ID, VSPACE_TABLE,
};

macro_rules! linker_sym_addr {
    ($sym:ident) => {{
        let addr: u64;
        // SAFETY: Pure RIP-relative address calculation (`lea`) of a linker-script boundary symbol;
        // avoids LLVM's `extern static u8` non-aliasing constant-folding on co-located section markers.
        unsafe {
            core::arch::asm!(
                concat!("lea {out}, [rip + ", stringify!($sym), "]"),
                out = out(reg) addr,
                options(nomem, nostack, preserves_flags),
            );
        }
        addr
    }};
}

#[derive(Clone, Copy, Debug)]
pub struct KernelVSpaceSummary {
    pub boot_cr3_phys: u64,
    pub kernel_cr3_phys: u64,
    pub kernel_pt_frames: u64,
    pub boot_reclaim_blocked_while_active: bool,
    pub reclaimed_boot_frames: u64,
    pub kernel_wx_enforced: bool,
    pub hw_caps: PagingHwCaps,
}

fn map_kernel_4k_init(pml4_pfn: u32, vaddr: u64, phys: u64, leaf_flags: u64, pt_counter: &mut u64) {
    let (i4, i3, i2, i1) = pt_indices(vaddr);
    assert!(i4 >= 256, "Kernel init mapping below PML4[256]: {:#x}", vaddr);

    // SAFETY: Single-threaded BSP building `KERNEL_VSPACE` before publishing its `CR3`.
    unsafe {
        let pml4 = pt_virt_ptr(pml4_pfn);
        let mut e4 = read_volatile(pml4.add(i4));
        if (e4 & PTE_PRESENT) == 0 {
            let pdpt = alloc_pinned_pt_frame().expect("OOM allocating kernel PDPT");
            *pt_counter += 1;
            let _ = pt_frame_adjust_entries(pml4_pfn, 1);
            e4 = pdpt.phys_addr() | PTE_KERNEL_TABLE_INTERMEDIATE;
            write_volatile(pml4.add(i4), e4);
        }
        let pdpt_pfn = ((e4 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;
        let pdpt = pt_virt_ptr(pdpt_pfn);
        let mut e3 = read_volatile(pdpt.add(i3));
        if (e3 & PTE_PRESENT) == 0 {
            let pd = alloc_pinned_pt_frame().expect("OOM allocating kernel PD");
            *pt_counter += 1;
            let _ = pt_frame_adjust_entries(pdpt_pfn, 1);
            e3 = pd.phys_addr() | PTE_KERNEL_TABLE_INTERMEDIATE;
            write_volatile(pdpt.add(i3), e3);
        }
        let pd_pfn = ((e3 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;
        let pd = pt_virt_ptr(pd_pfn);
        let mut e2 = read_volatile(pd.add(i2));
        if (e2 & PTE_PRESENT) == 0 {
            let pt = alloc_pinned_pt_frame().expect("OOM allocating kernel PT");
            *pt_counter += 1;
            let _ = pt_frame_adjust_entries(pd_pfn, 1);
            e2 = pt.phys_addr() | PTE_KERNEL_TABLE_INTERMEDIATE;
            write_volatile(pd.add(i2), e2);
        }
        let pt_pfn = ((e2 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;
        let pt = pt_virt_ptr(pt_pfn);
        assert_eq!(
            read_volatile(pt.add(i1)) & PTE_PRESENT,
            0,
            "Duplicate kernel 4K mapping at {:#x}",
            vaddr
        );
        let _ = pt_frame_adjust_entries(pt_pfn, 1);
        write_volatile(pt.add(i1), (phys & PTE_PHYS_MASK_4K) | leaf_flags);
    }
}

fn audit_kernel_upper_half_wx_and_us(pml4_pfn: u32) -> bool {
    // SAFETY: Read-only walk of immutable `KERNEL_VSPACE` upper half (`PML4[256..512]`).
    unsafe {
        let pml4 = pt_virt_ptr(pml4_pfn);
        for i4 in 0..256 {
            assert_eq!(read_volatile(pml4.add(i4)) & PTE_PRESENT, 0);
        }
        for i4 in 256..512 {
            let e4 = read_volatile(pml4.add(i4));
            if (e4 & PTE_PRESENT) == 0 {
                continue;
            }
            if (e4 & PTE_USER) != 0 {
                return false;
            }
            let pdpt = pt_virt_ptr(((e4 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32);
            for i3 in 0..512 {
                let e3 = read_volatile(pdpt.add(i3));
                if (e3 & PTE_PRESENT) == 0 {
                    continue;
                }
                if (e3 & PTE_USER) != 0 || (e3 & PTE_HUGE_PAGE) != 0 {
                    return false;
                }
                let pd = pt_virt_ptr(((e3 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32);
                for i2 in 0..512 {
                    let e2 = read_volatile(pd.add(i2));
                    if (e2 & PTE_PRESENT) == 0 {
                        continue;
                    }
                    if (e2 & PTE_USER) != 0 || (e2 & PTE_HUGE_PAGE) != 0 {
                        return false;
                    }
                    let pt = pt_virt_ptr(((e2 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32);
                    for i1 in 0..512 {
                        let leaf = read_volatile(pt.add(i1));
                        if (leaf & PTE_PRESENT) == 0 {
                            continue;
                        }
                        if (leaf & PTE_USER) != 0 {
                            return false;
                        }
                        if ((leaf & PTE_WRITABLE) != 0) && ((leaf & PTE_NX) == 0) {
                            return false;
                        }
                    }
                }
            }
        }
    }
    true
}

/// Attempt to reclaim Limine `BootloaderReclaimable` frames into `PMM`.
///
/// Fails closed with `Err(VSpaceError::BootTableStillActive)` if ANY online CPU's active `CR3`
/// still resides inside a `BootloaderReclaimable` physical frame (`Milestone 1B-B` Req #10).
pub fn reclaim_bootloader_reclaimable_frames(online_cpus: usize) -> Result<u64, VSpaceError> {
    for cpu in 0..online_cpus {
        let cr3_phys = CPU_ACTIVE_CR3[cpu].load(Ordering::Acquire);
        if query_frame_state(cr3_phys) == FrameState::BootloaderReclaimable
            || CPU_ACTIVE_VSPACE[cpu].load(Ordering::Acquire) == BOOT_CR3_VSPACE_SENTINEL
        {
            return Err(VSpaceError::BootTableStillActive);
        }
    }
    // SAFETY: Verified above that `0..online_cpus` switched `CR3` onto permanent `KERNEL_VSPACE`,
    // all CPUs run on permanent `'static` kernel stacks, and `cmdline` was latched into `.bss`.
    let count = unsafe { reclaim_bootloader_frames_raw() };
    BOOT_FRAMES_RECLAIMED.fetch_add(count, Ordering::AcqRel);
    Ok(count)
}

/// Invoked on each AP (`1..online_cpus`) during `ApEpochKind::KernelVSpaceSync` to enable
/// `WP + NXE (+ SMEP)`, switch `CR3` from Limine's `boot_cr3` to `KERNEL_VSPACE`, and self-test
/// per-CPU `GDT/TSS/IST1/GS_BASE/IDT` continuity.
pub fn activate_kernel_vspace_on_ap(cpu_idx: usize) {
    let _ = detect_and_enable_cpu_paging_caps(cpu_idx);
    let kcr3 = VSPACE_TABLE[0].cr3_phys.load(Ordering::Acquire);
    assert_ne!(kcr3, 0, "KERNEL_VSPACE CR3 not ready on AP {}", cpu_idx);

    // SAFETY: `kcr3` maps identical kernel `.text/.rodata/.data/.bss`, HHDM, and APIC MMIO.
    unsafe { write_cr3(kcr3) };
    assert_eq!(read_cr3() & PTE_PHYS_MASK_4K, kcr3);

    CPU_ACTIVE_CR3[cpu_idx].store(kcr3, Ordering::Release);
    CPU_ACTIVE_VSPACE[cpu_idx].store(KERNEL_VSPACE_ID, Ordering::Release);
    VSPACE_TABLE[0]
        .active_cpus
        .fetch_or(1u64 << cpu_idx, Ordering::SeqCst);

    verify_cpu_tss_and_canaries(cpu_idx);
    let _ = verify_breakpoint_trap_recovery(get_percpu_by_idx(cpu_idx));
}

/// Construct `KERNEL_VSPACE` (`id = 0`) from PMM frames, prove `BootloaderReclaimable` cannot be
/// reclaimed while `boot_cr3` is active, transition BSP + all APs onto `KERNEL_VSPACE`, and safely
/// reclaim Limine's `BootloaderReclaimable` frames into `PMM`.
pub fn init_and_activate_kernel_vspace(
    mem: &MemorySummary,
    online_cpus: usize,
    tsc_mhz: u64,
) -> KernelVSpaceSummary {
    tlb::init_tlb_shootdown_timeout(tsc_mhz);
    let boot_cr3 = read_cr3() & PTE_PHYS_MASK_4K;
    for cpu in 0..online_cpus {
        CPU_ACTIVE_CR3[cpu].store(boot_cr3, Ordering::Release);
        CPU_ACTIVE_VSPACE[cpu].store(BOOT_CR3_VSPACE_SENTINEL, Ordering::Release);
    }

    let blocked_ok =
        reclaim_bootloader_reclaimable_frames(online_cpus) == Err(VSpaceError::BootTableStillActive);
    assert!(blocked_ok, "Active boot_cr3 was not protected against premature reclaim");

    let pml4_h = alloc_pinned_pt_frame().expect("OOM allocating KERNEL_VSPACE PML4");
    let mut pt_frames = 1u64;

    let kvirt_start = linker_sym_addr!(__kernel_virt_start);
    let text_start = linker_sym_addr!(__kernel_text_start);
    let text_end = linker_sym_addr!(__kernel_text_end);
    let rodata_start = linker_sym_addr!(__kernel_rodata_start);
    let rodata_end = linker_sym_addr!(__kernel_rodata_end);
    let data_start = linker_sym_addr!(__kernel_data_start);
    let kvirt_end = linker_sym_addr!(__kernel_virt_end);
    assert_eq!(kvirt_start, text_start);

    let rx_flags = prot_to_pte_flags(VmProt::KERNEL_RX, false).unwrap();
    let ro_flags = prot_to_pte_flags(VmProt::KERNEL_RO, false).unwrap();
    let rw_flags = prot_to_pte_flags(VmProt::KERNEL_RW, false).unwrap();
    let uc_flags = prot_to_pte_flags(VmProt::KERNEL_MMIO_UC, false).unwrap();

    for (seg_start, seg_end, flags) in [
        (text_start, text_end, rx_flags),
        (rodata_start, rodata_end, ro_flags),
        (data_start, kvirt_end, rw_flags),
    ] {
        let mut v = seg_start;
        while v < seg_end {
            let phys = mem.kernel_phys_base + (v - mem.kernel_virt_base);
            map_kernel_4k_init(pml4_h.pfn, v, phys, flags, &mut pt_frames);
            v += PAGE_SIZE_4K;
        }
    }

    let inv = inventory();
    let ktext_phys_start = mem.kernel_phys_base + (text_start - mem.kernel_virt_base);
    let kro_phys_end = mem.kernel_phys_base + (rodata_end - mem.kernel_virt_base);
    let apic_page_phys = apic_phys_base() & PTE_PHYS_MASK_4K;
    let ioapic_page_phys = crate::acpi::ioapic_phys_base().map(|p| p & PTE_PHYS_MASK_4K);

    for pfn in 0..inv.max_tracked_pfn {
        let phys = (pfn as u64) << PAGE_SHIFT;
        if phys == apic_page_phys || ioapic_page_phys == Some(phys) {
            continue;
        }
        if query_frame_state(phys) == FrameState::ReservedOrMmio {
            continue;
        }
        let flags = if phys >= ktext_phys_start && phys < kro_phys_end {
            ro_flags
        } else {
            rw_flags
        };
        map_kernel_4k_init(pml4_h.pfn, mem.hhdm_offset + phys, phys, flags, &mut pt_frames);
    }

    map_kernel_4k_init(
        pml4_h.pfn,
        mem.hhdm_offset + apic_page_phys,
        apic_page_phys,
        uc_flags,
        &mut pt_frames,
    );
    if let Some(io_phys) = ioapic_page_phys {
        if io_phys != apic_page_phys {
            assert_eq!(
                query_frame_state(io_phys),
                FrameState::ReservedOrMmio,
                "IOAPIC base {:#x} must reside in ReservedOrMmio space",
                io_phys
            );
            map_kernel_4k_init(
                pml4_h.pfn,
                mem.hhdm_offset + io_phys,
                io_phys,
                uc_flags,
                &mut pt_frames,
            );
        }
    }

    let drhd_count = crate::iommu::dmar_snapshot().map_or(0, |d| d.drhd_count);
    for u in 0..drhd_count {
        let base = crate::iommu::dmar_snapshot().unwrap().drhds[u].register_base_phys;
        let mut mapped_pages = 0u8;
        for p in 0..KERNEL_VTD_MAX_PAGES {
            let pa = base.saturating_add((p as u64) * PAGE_SIZE_4K);
            if query_frame_state(pa) != FrameState::ReservedOrMmio {
                break;
            }
            map_kernel_4k_init(
                pml4_h.pfn,
                vtd_unit_vaddr(u) + (p as u64) * PAGE_SIZE_4K,
                pa,
                uc_flags,
                &mut pt_frames,
            );
            mapped_pages += 1;
        }
        crate::iommu::dmar::set_drhd_mapped_pages(u, mapped_pages);
    }

    crate::pci::for_each_kernel_shadow_mmio_page(|va, pa| {
        assert_eq!(
            query_frame_state(pa),
            FrameState::ReservedOrMmio,
            "PCI supervisor MMIO page {:#x} must reside in ReservedOrMmio space",
            pa
        );
        map_kernel_4k_init(pml4_h.pfn, va, pa, uc_flags, &mut pt_frames);
    });

    if let Some(vfb) = crate::display::framebuffer_summary() {
        for p in 0..(vfb.num_pages as usize) {
            let va = crate::vspace::pte::KERNEL_GOP_DIAG_BASE + (p as u64) * PAGE_SIZE_4K;
            let pa = vfb.phys_base + (p as u64) * PAGE_SIZE_4K;
            map_kernel_4k_init(pml4_h.pfn, va, pa, uc_flags, &mut pt_frames);
        }
    }

    let wx_ok = audit_kernel_upper_half_wx_and_us(pml4_h.pfn);
    assert!(wx_ok, "KERNEL_VSPACE upper-half W^X or U/S=0 audit failed");

    let kernel_cr3 = pml4_h.phys_addr();
    let kslot = &VSPACE_TABLE[0];
    kslot.pml4_pfn.store(pml4_h.pfn, Ordering::Relaxed);
    kslot.pml4_gen.store(pml4_h.generation, Ordering::Relaxed);
    kslot.cr3_phys.store(kernel_cr3, Ordering::Release);
    kslot.pt_frames_owned.store(pt_frames, Ordering::Release);
    kslot.allocated.store(true, Ordering::Release);

    let bsp_caps = detect_and_enable_cpu_paging_caps(0);
    // SAFETY: `kernel_cr3` maps active BSP code, stack, GDT/TSS/IST1, GS_BASE, IDT, HHDM, and APIC.
    unsafe { write_cr3(kernel_cr3) };
    crate::pci_ecam::mark_kernel_ecam_ready();
    if crate::display::framebuffer_summary().is_some() {
        crate::panic_fb::set_kernel_gop_diag_mapped(true);
    }
    assert_eq!(read_cr3() & PTE_PHYS_MASK_4K, kernel_cr3);
    CPU_ACTIVE_CR3[0].store(kernel_cr3, Ordering::Release);
    CPU_ACTIVE_VSPACE[0].store(KERNEL_VSPACE_ID, Ordering::Release);
    kslot.active_cpus.fetch_or(1, Ordering::SeqCst);

    verify_cpu_tss_and_canaries(0);
    let _ = verify_breakpoint_trap_recovery(get_percpu_by_idx(0));

    if online_cpus > 1 {
        set_ap_epoch_kind(ApEpochKind::KernelVSpaceSync);
        AP_PHASE_DONE_CPUS.store(0, Ordering::Release);
        AP_PHASE_EPOCH.fetch_add(1, Ordering::AcqRel);
        while AP_PHASE_DONE_CPUS.load(Ordering::Acquire) < (online_cpus - 1) {
            if crate::is_panicking() {
                crate::arch::halt_forever();
            }
            core::hint::spin_loop();
        }
    }

    let reclaimed =
        reclaim_bootloader_reclaimable_frames(online_cpus).expect("Post-switch boot reclaim failed");
    assert_eq!(reclaimed, inv.reclaimable_frames);

    KernelVSpaceSummary {
        boot_cr3_phys: boot_cr3,
        kernel_cr3_phys: kernel_cr3,
        kernel_pt_frames: pt_frames,
        boot_reclaim_blocked_while_active: blocked_ok,
        reclaimed_boot_frames: reclaimed,
        kernel_wx_enforced: wx_ok,
        hw_caps: bsp_caps,
    }
}
