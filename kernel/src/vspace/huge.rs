// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Cleanly Separated `2 MiB` Huge-Page (`PDE.PS`) Sub-Stage & Independent `rdtsc` Comparison
//! (`Milestone 1B-B` Req #11).

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};

use crate::arch::rdtsc;
use crate::pmm::{
    alloc_pages, flush_all_magazines_quiescent, free_pages, map_ref_inc_data_frame,
    measure_fragmentation, pt_frame_adjust_entries, AllocFlags, FrameHandle, PageOrder,
    HUGE_2M_FRAMES, PAGE_SHIFT,
};
use crate::security::{SecDomain, SecFlags, SecLabel};
use crate::serial_println;
use crate::vspace::pte::{
    prot_to_pte_flags, pt_indices, validate_user_range, VSpaceError, VmProt, PAGE_SIZE_2M,
    PAGE_SIZE_4K, PTE_HUGE_PAGE, PTE_PHYS_MASK_2M, PTE_PHYS_MASK_4K, PTE_PRESENT,
    PTE_USER_TABLE_INTERMEDIATE,
};
use crate::vspace::{
    alloc_pinned_pt_frame, create_vspace, destroy_vspace, map_4k, pt_virt_ptr,
    switch_cpu_vspace_if_needed, tlb, unmap_4k, vspace_owned_pt_frames, walk_vspace_address,
    with_vspace_locked, PendingReclaim, VSpaceHandle, KERNEL_VSPACE_ID,
};

/// Map an `Order2MiB` (`512`-frame) huge page at 2 MiB-aligned `vaddr` via `PDE.PS`.
pub fn map_2m(
    handle: VSpaceHandle,
    vaddr: u64,
    frame: FrameHandle,
    prot: VmProt,
) -> Result<(), VSpaceError> {
    validate_user_range(vaddr, PAGE_SIZE_2M, PAGE_SIZE_2M)?;
    if (frame.pfn as usize) & (HUGE_2M_FRAMES - 1) != 0 {
        return Err(VSpaceError::UnalignedAddress);
    }
    let leaf_flags = prot_to_pte_flags(prot, true)?;
    let (i4, i3, i2, _) = pt_indices(vaddr);

    with_vspace_locked(handle, |slot, owner_label| {
        let pml4_pfn = slot.pml4_pfn.load(Ordering::Acquire);
        // SAFETY: `slot.locked` held.
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
            if (read_volatile(pd.add(i2)) & PTE_PRESENT) != 0 {
                return Err(VSpaceError::AlreadyMapped);
            }

            map_ref_inc_data_frame(
                frame,
                PageOrder::Order2MiB,
                owner_label,
                prot.contains(VmProt::WRITE),
            );
            let _ = pt_frame_adjust_entries(pd_pfn, 1);
            write_volatile(pd.add(i2), (frame.phys_addr() & PTE_PHYS_MASK_2M) | leaf_flags);
            slot.mapped_4k_equiv_pages.fetch_add(512, Ordering::Relaxed);
            Ok(())
        }
    })
}

/// Unmap a `2 MiB` huge page (`PDE.PS`), reclaim emptied `PD/PDPT` tables, and shoot down TLBs.
pub fn unmap_2m(handle: VSpaceHandle, vaddr: u64) -> Result<FrameHandle, VSpaceError> {
    validate_user_range(vaddr, PAGE_SIZE_2M, PAGE_SIZE_2M)?;
    let (i4, i3, i2, _) = pt_indices(vaddr);

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
            let old_pde = read_volatile(pd.add(i2));
            if (old_pde & PTE_PRESENT) == 0 {
                return Err(VSpaceError::NotMapped);
            }
            if (old_pde & PTE_HUGE_PAGE) == 0 {
                return Err(VSpaceError::PageSizeMismatch);
            }

            write_volatile(pd.add(i2), 0);
            let mut pending = PendingReclaim {
                data_pfn: ((old_pde & PTE_PHYS_MASK_2M) >> PAGE_SHIFT) as u32,
                order: PageOrder::Order2MiB,
                pt_pfns: [0; 3],
                pt_len: 0,
            };

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
            fence(Ordering::SeqCst);
            slot.mapped_4k_equiv_pages.fetch_sub(512, Ordering::Relaxed);
            // Intel SDM Vol. 3A 4.10.4.1: `invlpg [vaddr]` invalidates a 2 MiB PDE.PS entry; if
            // empty parent `PD`/`PDPT` directories were also unlinked, flush full paging caches.
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

/// Update permissions on an active `2 MiB` huge page (`W^X` enforced) and synchronize TLBs.
pub fn protect_2m(handle: VSpaceHandle, vaddr: u64, new_prot: VmProt) -> Result<(), VSpaceError> {
    validate_user_range(vaddr, PAGE_SIZE_2M, PAGE_SIZE_2M)?;
    let new_flags = prot_to_pte_flags(new_prot, true)?;
    let (i4, i3, i2, _) = pt_indices(vaddr);

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
            let old_pde = read_volatile(pd.add(i2));
            if (old_pde & PTE_PRESENT) == 0 {
                return Err(VSpaceError::NotMapped);
            }
            if (old_pde & PTE_HUGE_PAGE) == 0 {
                return Err(VSpaceError::PageSizeMismatch);
            }
            write_volatile(pd.add(i2), (old_pde & PTE_PHYS_MASK_2M) | new_flags);
            fence(Ordering::SeqCst);
            let _ = tlb::shootdown_tlb(handle.id, &slot.active_cpus, vaddr, 1);
            Ok(())
        }
    })
}

/// Execute the isolated `2 MiB` huge-page verification & cost-comparison sub-stage AFTER 4 KiB
/// verification has already succeeded (`[DRY-HUGEPAGE] PASS:`).
pub fn verify_hugepage_2m_substage(online_cpus: usize) {
    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);

    let lbl = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 777, 1);
    let vs = create_vspace(lbl).expect("create_vspace for 2MiB sub-stage failed");
    let huge_vaddr: u64 = 0x0000_0000_4000_0000; // 1 GiB (2 MiB-aligned)

    // Unaligned `vaddr` and `W^X` violation must fail closed.
    let block = alloc_pages(PageOrder::Order2MiB, lbl, AllocFlags::ZERO).expect("alloc Order2MiB");
    assert_eq!(
        map_2m(vs, huge_vaddr + 0x1000, block, VmProt::USER_RW),
        Err(VSpaceError::UnalignedAddress)
    );
    assert_eq!(
        map_2m(vs, huge_vaddr, block, VmProt::USER_WX_FORBIDDEN),
        Err(VSpaceError::WxViolation)
    );

    let mut map_2m_cyc = u64::MAX;
    for trial in 0..3 {
        let t_map0 = rdtsc();
        map_2m(vs, huge_vaddr, block, VmProt::USER_RW).expect("map_2m failed");
        map_2m_cyc = map_2m_cyc.min(rdtsc().wrapping_sub(t_map0).max(1));
        if trial < 2 {
            let _ = unmap_2m(vs, huge_vaddr).expect("warmup unmap_2m failed");
        }
    }

    let pt_frames_2m = vspace_owned_pt_frames(vs).unwrap();
    assert_eq!(pt_frames_2m, 2, "Expected exactly PDPT + PD (2 frames) for 2MiB leaf");
    let resolved = walk_vspace_address(vs.id, huge_vaddr + 0x1f_f000).unwrap();
    assert!(resolved.is_huge_2m && resolved.effective_user && resolved.effective_writable && !resolved.effective_exec);

    // Switch into `vs`, write + read across the first and 512th 4 KiB sub-pages, protect RO, switch back.
    switch_cpu_vspace_if_needed(0, vs.id);
    // SAFETY: `[huge_vaddr .. huge_vaddr + 2MiB)` is mapped RW in active `vs`.
    unsafe {
        write_volatile(huge_vaddr as *mut u64, 0x2000_0000_cafe_0001);
        write_volatile((huge_vaddr + PAGE_SIZE_2M - 8) as *mut u64, 0x2000_0000_cafe_0512);
        assert_eq!(read_volatile(huge_vaddr as *const u64), 0x2000_0000_cafe_0001);
        assert_eq!(
            read_volatile((huge_vaddr + PAGE_SIZE_2M - 8) as *const u64),
            0x2000_0000_cafe_0512
        );
    }
    protect_2m(vs, huge_vaddr, VmProt::USER_RO).expect("protect_2m RO failed");
    let ro_walk = walk_vspace_address(vs.id, huge_vaddr).unwrap();
    assert!(!ro_walk.effective_writable && !ro_walk.effective_exec);
    // Size-mismatch guard: `unmap_4k` on a 2 MiB page must fail closed.
    assert_eq!(unmap_4k(vs, huge_vaddr), Err(VSpaceError::PageSizeMismatch));
    switch_cpu_vspace_if_needed(0, KERNEL_VSPACE_ID);

    let t_unmap0 = rdtsc();
    let returned = unmap_2m(vs, huge_vaddr).expect("unmap_2m failed");
    let unmap_2m_cyc = rdtsc().wrapping_sub(t_unmap0).max(1);
    assert_eq!(returned, block);
    assert_eq!(vspace_owned_pt_frames(vs).unwrap(), 0);
    free_pages(returned, PageOrder::Order2MiB, lbl);

    // Measure equivalent 2 MiB region mapped via `512 x map_4k` for honest side-by-side comparison.
    let mut frames_4k = [FrameHandle { pfn: 0, generation: 0 }; HUGE_2M_FRAMES];
    for slot in &mut frames_4k {
        *slot = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::UNINIT_SAME_DOMAIN_OK).unwrap();
    }
    let t_4k0 = rdtsc();
    for (i, fh) in frames_4k.iter().enumerate() {
        map_4k(vs, huge_vaddr + (i as u64) * PAGE_SIZE_4K, *fh, VmProt::USER_RW).unwrap();
    }
    let map_512x4k_cyc = rdtsc().wrapping_sub(t_4k0).max(1);
    let pt_frames_4k = vspace_owned_pt_frames(vs).unwrap();
    assert_eq!(pt_frames_4k, 3, "Expected PDPT + PD + PT (3 frames) for 512x4K");
    let pt_frames_saved = pt_frames_4k - pt_frames_2m;

    for (i, _) in frames_4k.iter().enumerate() {
        let fh = unmap_4k(vs, huge_vaddr + (i as u64) * PAGE_SIZE_4K).unwrap();
        free_pages(fh, PageOrder::Order4KiB, lbl);
    }
    destroy_vspace(vs).expect("destroy_vspace after 2MiB sub-stage failed");
    flush_all_magazines_quiescent(online_cpus);

    let after = measure_fragmentation(online_cpus);
    assert_eq!(after.free_global_frames, before.free_global_frames);
    assert_eq!(after.contig_2m_blocks, before.contig_2m_blocks);

    serial_println!(
        "[DRY-HUGEPAGE] PASS: order2m_ok=true wx_ok=true pt_frames_2m={} pt_frames_512x4k={} pt_frames_saved={} map_2m_cyc={} map_512x4k_cyc={} unmap_2m_cyc={} conserved=true",
        pt_frames_2m,
        pt_frames_4k,
        pt_frames_saved,
        map_2m_cyc,
        map_512x4k_cyc,
        unmap_2m_cyc
    );
}
