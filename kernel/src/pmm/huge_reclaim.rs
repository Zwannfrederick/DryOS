// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Milestone `1B-B` PMM Extensions: `Order2MiB` (`512`-Frame) Huge-Page Allocation, Post-`CR3`
//! `BootloaderReclaimable` Reclamation, and Atomic Page-Table / Data-Frame Reference Accounting.

use core::sync::atomic::Ordering;

use crate::pmm::meta::{
    frame_meta, next_generation, pack_free_word1, pack_seclabel, pack_word0, phys_to_hhdm,
    pmm_invariant_panic, unpack_free_word1, unpack_seclabel, unpack_word0, zero_frame_4k,
    AllocFlags, FrameHandle, FrameState, PageOrder, PmmError, ZeroState, HUGE_2M_FRAMES,
    LOW_MEM_GUARD_END_PFN, NO_CPU_OWNER, PAGE_SHIFT, PFN_NONE, PMM_META,
};
use crate::pmm::{lock_global_pool_with_cli, record_zero_accounting};
use crate::security::{SecDomain, SecFlags, SecLabel};

/// Allocate one 2 MiB-aligned contiguous block (`512` x 4 KiB frames) bound to `label`.
pub(crate) fn alloc_order_2m(label: SecLabel, flags: AllocFlags) -> Result<FrameHandle, PmmError> {
    let max_pfn = PMM_META.max_tracked_pfn.load(Ordering::Acquire) as usize;

    let (base_pfn, block_gen, need_zero_mask_any) = lock_global_pool_with_cli(|pool| {
        if (pool.free_count as usize) < HUGE_2M_FRAMES {
            return Err(PmmError::OutOfMemory);
        }

        let mut found_base: Option<u32> = None;
        let mut max_gen = 1u32;
        let mut base = HUGE_2M_FRAMES;
        while base + HUGE_2M_FRAMES <= max_pfn {
            let mut run_ok = true;
            let mut cand_max_gen = 1u32;
            for off in 0..HUGE_2M_FRAMES {
                let Some(m) = frame_meta((base + off) as u32) else {
                    run_ok = false;
                    break;
                };
                let (st, _, _, _, pins, maps, g) = unpack_word0(m.word0.load(Ordering::Relaxed));
                if st != FrameState::FreeGlobal || pins != 0 || maps != 0 {
                    run_ok = false;
                    break;
                }
                if g > cand_max_gen {
                    cand_max_gen = g;
                }
            }
            if run_ok {
                found_base = Some(base as u32);
                max_gen = cand_max_gen;
                break;
            }
            base += HUGE_2M_FRAMES;
        }

        let base_u32 = found_base.ok_or(PmmError::OutOfMemory)?;
        let end_u32 = base_u32 + (HUGE_2M_FRAMES as u32);

        let mut new_head = PFN_NONE;
        let mut prev_kept = PFN_NONE;
        let mut cur = pool.head_pfn;
        let mut unlinked = 0usize;
        let mut any_dirty_or_cross = false;

        while cur != PFN_NONE {
            let m = frame_meta(cur).unwrap();
            let (next_pfn, last_comp, _) = unpack_free_word1(m.word1.load(Ordering::Relaxed));
            if cur >= base_u32 && cur < end_u32 {
                let (_, prev_zero, _, last_dom, _, _, _) =
                    unpack_word0(m.word0.load(Ordering::Relaxed));
                let same_comp =
                    last_dom == (label.domain as u8) && last_comp == label.compartment_id;
                if prev_zero != ZeroState::VerifiedZero
                    && (!same_comp
                        || !flags.contains(AllocFlags::UNINIT_SAME_DOMAIN_OK)
                        || flags.contains(AllocFlags::ZERO))
                {
                    any_dirty_or_cross = true;
                }
                m.word1.store(pack_seclabel(label), Ordering::Relaxed);
                m.word0.store(
                    pack_word0(
                        FrameState::AllocatedFrame,
                        ZeroState::Dirty,
                        PageOrder::Order2MiB as u8,
                        label.domain as u8,
                        0,
                        0,
                        max_gen,
                    ),
                    Ordering::Release,
                );
                unlinked += 1;
            } else {
                if prev_kept == PFN_NONE {
                    new_head = cur;
                } else {
                    let pm = frame_meta(prev_kept).unwrap();
                    let (_, prev_lc, prev_ct) = unpack_free_word1(pm.word1.load(Ordering::Relaxed));
                    pm.word1
                        .store(pack_free_word1(cur, prev_lc, prev_ct), Ordering::Relaxed);
                }
                prev_kept = cur;
            }
            cur = next_pfn;
        }
        if prev_kept != PFN_NONE {
            let pm = frame_meta(prev_kept).unwrap();
            let (_, lc, ct) = unpack_free_word1(pm.word1.load(Ordering::Relaxed));
            pm.word1
                .store(pack_free_word1(PFN_NONE, lc, ct), Ordering::Relaxed);
        }
        assert_eq!(unlinked, HUGE_2M_FRAMES, "Order2MiB unlink count mismatch");
        pool.head_pfn = new_head;
        pool.free_count -= HUGE_2M_FRAMES as u64;
        Ok((base_u32, max_gen, any_dirty_or_cross))
    })?;

    if need_zero_mask_any {
        for off in 0..HUGE_2M_FRAMES {
            let pfn = base_pfn + (off as u32);
            zero_frame_4k(phys_to_hhdm((pfn as u64) << PAGE_SHIFT));
        }
        record_zero_accounting(HUGE_2M_FRAMES as u64, 0);
    } else {
        record_zero_accounting(0, HUGE_2M_FRAMES as u64);
    }

    Ok(FrameHandle {
        pfn: base_pfn,
        generation: block_gen,
    })
}

/// Validate and release an `Order2MiB` (`512`-frame) block back to `GlobalFreePool`.
pub(crate) fn free_order_2m(handle: FrameHandle, caller_label: SecLabel) {
    if (handle.pfn as usize) & (HUGE_2M_FRAMES - 1) != 0 {
        pmm_invariant_panic(format_args!(
            "Frame ownership / domain / pin violation: unaligned Order2MiB pfn={:#x}",
            handle.pfn
        ));
    }

    lock_global_pool_with_cli(|pool| {
        for off in 0..HUGE_2M_FRAMES {
            let pfn = handle.pfn + (off as u32);
            let Some(m) = frame_meta(pfn) else {
                pmm_invariant_panic(format_args!(
                    "Frame ownership / domain / pin violation: non-RAM Order2MiB pfn={:#x}",
                    pfn
                ));
            };
            let (st, _, ord, _, pins, maps, cur_gen) = unpack_word0(m.word0.load(Ordering::Acquire));
            if st == FrameState::FreeGlobal || st == FrameState::FreePerCpu {
                pmm_invariant_panic(format_args!(
                    "Double free detected: Order2MiB pfn={:#x} state={:?}",
                    pfn, st
                ));
            }
            if st != FrameState::AllocatedFrame || cur_gen != handle.generation {
                pmm_invariant_panic(format_args!(
                    "Use-after-free / stale generation detected: Order2MiB pfn={:#x} gen={} cur={}",
                    pfn, handle.generation, cur_gen
                ));
            }
            let owner = unpack_seclabel(m.word1.load(Ordering::Acquire));
            if owner.domain != caller_label.domain
                || owner.compartment_id != caller_label.compartment_id
                || ord != (PageOrder::Order2MiB as u8)
                || pins != 0
                || maps != 0
            {
                pmm_invariant_panic(format_args!(
                    "Frame ownership / domain / pin violation: Order2MiB pfn={:#x} pins={} maps={} ord={}",
                    pfn, pins, maps, ord
                ));
            }
        }

        let next_gen = next_generation(handle.generation);
        for off in (0..HUGE_2M_FRAMES).rev() {
            let pfn = handle.pfn + (off as u32);
            let m = frame_meta(pfn).unwrap();
            m.word1.store(
                pack_free_word1(pool.head_pfn, caller_label.compartment_id, NO_CPU_OWNER),
                Ordering::Relaxed,
            );
            m.word0.store(
                pack_word0(
                    FrameState::FreeGlobal,
                    ZeroState::Dirty,
                    0,
                    caller_label.domain as u8,
                    0,
                    0,
                    next_gen,
                ),
                Ordering::Release,
            );
            pool.head_pfn = pfn;
            pool.free_count += 1;
        }
    });
}

/// Reclaim all `FrameState::BootloaderReclaimable` frames into `GlobalFreePool` after scrubbing.
///
/// # Safety
/// Caller (`crate::vspace`) MUST have verified that ALL online CPUs switched `CR3` off Limine's
/// `boot_cr3`, are running on permanent kernel stacks, and copied `cmdline` into `.bss`.
pub(crate) unsafe fn reclaim_bootloader_frames_raw() -> u64 {
    let max_pfn = PMM_META.max_tracked_pfn.load(Ordering::Acquire);
    let mut reclaimed = 0u64;

    for pfn in (LOW_MEM_GUARD_END_PFN..max_pfn).rev() {
        let Some(m) = frame_meta(pfn) else { continue };
        let old_w0 = m.word0.load(Ordering::Acquire);
        let (st, _, _, _, pins, maps, cur_gen) = unpack_word0(old_w0);
        if st != FrameState::BootloaderReclaimable {
            continue;
        }
        assert!(pins == 0 && maps == 0, "Pinned BootloaderReclaimable frame {:#x}", pfn);
        zero_frame_4k(phys_to_hhdm((pfn as u64) << PAGE_SHIFT));

        let next_gen = next_generation(cur_gen);
        lock_global_pool_with_cli(|pool| {
            m.word1.store(
                pack_free_word1(pool.head_pfn, 0, NO_CPU_OWNER),
                Ordering::Relaxed,
            );
            m.word0.store(
                pack_word0(
                    FrameState::FreeGlobal,
                    ZeroState::VerifiedZero,
                    0,
                    SecDomain::Kernel as u8,
                    0,
                    0,
                    next_gen,
                ),
                Ordering::Release,
            );
            pool.head_pfn = pfn;
            pool.free_count += 1;
        });
        reclaimed += 1;
    }
    reclaimed
}

/// Atomically adjust active child-entry count (`maps` in `0..=512`) on a pinned Page-Table frame.
pub(crate) fn pt_frame_adjust_entries(pt_pfn: u32, delta: i32) -> u16 {
    let meta = frame_meta(pt_pfn).expect("Invalid PT PFN");
    loop {
        let old_w0 = meta.word0.load(Ordering::Acquire);
        let (st, zero, ord, last_dom, pins, maps, cur_gen) = unpack_word0(old_w0);
        if st != FrameState::AllocatedFrame || pins != 1 {
            pmm_invariant_panic(format_args!(
                "Corrupt page-table FrameMeta pfn={:#x} state={:?} pins={} maps={}",
                pt_pfn, st, pins, maps
            ));
        }
        let next_maps_i32 = (maps as i32) + delta;
        assert!(
            (0..=512).contains(&next_maps_i32),
            "PT entry count out of [0,512] on pfn={:#x}: {}",
            pt_pfn,
            next_maps_i32
        );
        let new_maps = next_maps_i32 as u16;
        let new_w0 = pack_word0(st, zero, ord, last_dom, pins, new_maps, cur_gen);
        if meta
            .word0
            .compare_exchange_weak(old_w0, new_w0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return new_maps;
        }
    }
}

/// Unpin an empty (`maps == 0`, `pins == 1`) Page-Table frame after TLB shootdown so it can be freed.
pub(crate) fn pt_frame_unpin_for_reclaim(pt_pfn: u32) -> FrameHandle {
    let meta = frame_meta(pt_pfn).expect("Invalid PT PFN in unpin");
    loop {
        let old_w0 = meta.word0.load(Ordering::Acquire);
        let (st, zero, ord, last_dom, pins, maps, cur_gen) = unpack_word0(old_w0);
        if st != FrameState::AllocatedFrame || pins != 1 || maps != 0 {
            pmm_invariant_panic(format_args!(
                "Cannot unpin non-empty/unpinned PT frame pfn={:#x} state={:?} pins={} maps={}",
                pt_pfn, st, pins, maps
            ));
        }
        let new_w0 = pack_word0(st, zero, ord, last_dom, 0, 0, cur_gen);
        if meta
            .word0
            .compare_exchange_weak(old_w0, new_w0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return FrameHandle {
                pfn: pt_pfn,
                generation: cur_gen,
            };
        }
    }
}

/// Validate and increment `map_count` (`+1`) on `1` (`Order4KiB`) or `512` (`Order2MiB`) data frames.
pub(crate) fn map_ref_inc_data_frame(
    handle: FrameHandle,
    order: PageOrder,
    expected_owner: SecLabel,
    writable: bool,
) {
    let count = order.frame_count();
    for off in 0..count {
        let pfn = handle.pfn + (off as u32);
        let Some(meta) = frame_meta(pfn) else {
            pmm_invariant_panic(format_args!(
                "Frame ownership / domain / pin violation: map non-RAM pfn={:#x}",
                pfn
            ));
        };
        let owner = unpack_seclabel(meta.word1.load(Ordering::Acquire));
        let same_owner = owner.domain == expected_owner.domain
            && owner.compartment_id == expected_owner.compartment_id;
        let ro_system_export = !writable
            && owner.domain == SecDomain::RootSystem
            && owner.flags.contains(SecFlags::READ_ONLY_EXPORT)
            && expected_owner.domain != SecDomain::Kernel;
        if !same_owner && !ro_system_export {
            pmm_invariant_panic(format_args!(
                "Frame ownership / domain / pin violation: map pfn={:#x} owner=({:?},{}) expected=({:?},{})",
                pfn, owner.domain, owner.compartment_id, expected_owner.domain, expected_owner.compartment_id
            ));
        }
        loop {
            let old_w0 = meta.word0.load(Ordering::Acquire);
            let (st, zero, ord, last_dom, pins, maps, cur_gen) = unpack_word0(old_w0);
            if (st != FrameState::AllocatedFrame && st != FrameState::UntypedRetypedObj)
                || cur_gen != handle.generation
                || (st == FrameState::AllocatedFrame && ord != (order as u8))
            {
                pmm_invariant_panic(format_args!(
                    "Use-after-free / stale generation detected: map pfn={:#x} gen={} cur={} ord={}",
                    pfn, handle.generation, cur_gen, ord
                ));
            }
            assert!(maps < 4095, "map_ref_count overflow on pfn={:#x}", pfn);
            let new_w0 = pack_word0(st, zero, ord, last_dom, pins, maps + 1, cur_gen);
            if meta
                .word0
                .compare_exchange_weak(old_w0, new_w0, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
    }
}

/// Decrement `map_count` (`-1`) on `1` (`Order4KiB`) or `512` (`Order2MiB`) unmapped data frames
/// strictly AFTER SMP TLB shootdown completion, returning the validated `FrameHandle`.
pub(crate) fn map_ref_dec_data_pfn_after_shootdown(base_pfn: u32, order: PageOrder) -> FrameHandle {
    let count = order.frame_count();
    let mut head_gen = 0u32;
    for off in 0..count {
        let pfn = base_pfn + (off as u32);
        let meta = frame_meta(pfn).expect("Invalid PFN in map_ref_dec_data_pfn_after_shootdown");
        loop {
            let old_w0 = meta.word0.load(Ordering::Acquire);
            let (st, zero, ord, last_dom, pins, maps, cur_gen) = unpack_word0(old_w0);
            if (st != FrameState::AllocatedFrame && st != FrameState::UntypedRetypedObj) || maps == 0
            {
                pmm_invariant_panic(format_args!(
                    "Corrupt mapped data frame on post-shootdown unmap: pfn={:#x} state={:?} maps={}",
                    pfn, st, maps
                ));
            }
            let new_w0 = pack_word0(st, zero, ord, last_dom, pins, maps - 1, cur_gen);
            if meta
                .word0
                .compare_exchange_weak(old_w0, new_w0, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                if off == 0 {
                    head_gen = cur_gen;
                }
                break;
            }
        }
    }
    FrameHandle {
        pfn: base_pfn,
        generation: head_gen,
    }
}
