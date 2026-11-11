// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `Milestone 1B-C` Stage F: Dedicated SMP TLB Performance Debt Analysis, Subset Target Isolation,
//! `1/2/4/8/20`-vCPU KVM Oversubscription Breakdown, and Safe Batched Shootdown (`unmap_4k_batch`).

use core::cell::UnsafeCell;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::arch::rdtsc;
use crate::pmm::{
    alloc_pages, flush_all_magazines_quiescent, free_pages, map_ref_dec_data_pfn_after_shootdown,
    measure_fragmentation, pt_frame_adjust_entries, pt_frame_unpin_for_reclaim, AllocFlags,
    FrameHandle, PageOrder, PAGE_SHIFT,
};
use crate::sched::preempt_tests::{begin_preemptive_epoch, drain_preemptive_epoch_and_audit};
use crate::sched::{
    current_cpu_idx, exit_current_thread, init_scheduler, note_current_thread_progress,
    set_preemptive_mode_active, try_spawn_thread_in_vspace,
};
use crate::security::{SecDomain, SecFlags, SecLabel};
use crate::serial_println;
use crate::vspace::pte::{
    pt_indices, validate_user_range, VSpaceError, VmProt, PAGE_SIZE_4K, PTE_HUGE_PAGE,
    PTE_PHYS_MASK_4K, PTE_PRESENT,
};
use crate::vspace::tlb::{
    cpu_raw_tlb_ipi_entries, flush_range_local, last_shootdown_breakdown, reset_tlb_telemetry,
    shootdown_tlb, tlb_telemetry, FULL_FLUSH_SENTINEL,
};
use crate::vspace::{
    create_vspace, destroy_vspace, map_4k, protect_4k, pt_virt_ptr, unmap_4k, with_vspace_locked,
    VSpaceHandle, KERNEL_PT_LABEL, KERNEL_VSPACE_ID,
};

pub const MAX_BATCH_UNMAP: usize = 16;
const DEBT_SAMPLES: usize = 128;

struct DebtSamples {
    ipi_send: [u64; DEBT_SAMPLES],
    remote_sched: [u64; DEBT_SAMPLES],
    flush_exec: [u64; DEBT_SAMPLES],
    ack_wait: [u64; DEBT_SAMPLES],
}

struct SyncDebtSamples(UnsafeCell<DebtSamples>);
// SAFETY: Written exclusively by the single initiator worker on CPU 0; read quiescently post-drain.
unsafe impl Sync for SyncDebtSamples {}

static DEBT_BUF: SyncDebtSamples = SyncDebtSamples(UnsafeCell::new(DebtSamples {
    ipi_send: [0; DEBT_SAMPLES],
    remote_sched: [0; DEBT_SAMPLES],
    flush_exec: [0; DEBT_SAMPLES],
    ack_wait: [0; DEBT_SAMPLES],
}));

static DEBT_VS_TARGET: AtomicU64 = AtomicU64::new(0);
static DEBT_EXPECTED_READY: AtomicUsize = AtomicUsize::new(0);
static DEBT_READY_COUNT: AtomicUsize = AtomicUsize::new(0);
static DEBT_STOP_WORKERS: AtomicBool = AtomicBool::new(false);
static DEBT_NON_MEMBER_IPIS: AtomicU64 = AtomicU64::new(0);
static DEBT_UNBATCHED_CYC: AtomicU64 = AtomicU64::new(0);
static DEBT_BATCHED_CYC: AtomicU64 = AtomicU64::new(0);

#[inline]
const fn pack_vs(h: VSpaceHandle) -> u64 {
    (h.id as u64) | ((h.generation as u64) << 16)
}

#[inline]
const fn unpack_vs(raw: u64) -> VSpaceHandle {
    VSpaceHandle {
        id: (raw & 0xffff) as u16,
        generation: (raw >> 16) as u32,
    }
}

fn sort_u64(arr: &mut [u64]) {
    let mut gap = arr.len() / 2;
    while gap > 0 {
        for i in gap..arr.len() {
            let tmp = arr[i];
            let mut j = i;
            while j >= gap && arr[j - gap] > tmp {
                arr[j] = arr[j - gap];
                j -= gap;
            }
            arr[j] = tmp;
        }
        gap /= 2;
    }
}

/// Safe batched 4 KiB unmap (`Stage F.4 & F.5`): clears `count` (`1..=16`) contiguous leaf PTEs
/// under a single `VSpace::lock`, executes **1** synchronous SMP `shootdown_tlb`, and releases
/// `map_ref_count` + emptied PT frames strictly *after* all target CPUs ACK.
pub fn unmap_4k_batch(
    handle: VSpaceHandle,
    base_vaddr: u64,
    count: usize,
    out_frames: &mut [FrameHandle],
) -> Result<usize, VSpaceError> {
    if count == 0 || count > MAX_BATCH_UNMAP || out_frames.len() < count {
        return Err(VSpaceError::AddressOverflow);
    }
    let total_bytes = (count as u64) * PAGE_SIZE_4K;
    validate_user_range(base_vaddr, total_bytes, PAGE_SIZE_4K)?;

    let (data_pfns, pt_pfns, pt_len) = with_vspace_locked(handle, |slot, _| {
        let pml4_pfn = slot.pml4_pfn.load(Ordering::Acquire);
        let mut data_pfns = [0u32; MAX_BATCH_UNMAP];
        let mut pt_pfns = [0u32; MAX_BATCH_UNMAP * 3];
        let mut pt_len = 0usize;

        // SAFETY: `slot.locked` held exclusively.
        unsafe {
            let pml4 = pt_virt_ptr(pml4_pfn);
            for (idx, dst_pfn) in data_pfns.iter_mut().enumerate().take(count) {
                let vaddr = base_vaddr + (idx as u64) * PAGE_SIZE_4K;
                let (i4, i3, i2, i1) = pt_indices(vaddr);
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
                *dst_pfn = ((old_leaf & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32;

                if pt_frame_adjust_entries(pt_pfn, -1) == 0 {
                    write_volatile(pd.add(i2), 0);
                    pt_pfns[pt_len] = pt_pfn;
                    pt_len += 1;
                    slot.pt_frames_owned.fetch_sub(1, Ordering::Relaxed);
                    if pt_frame_adjust_entries(pd_pfn, -1) == 0 {
                        write_volatile(pdpt.add(i3), 0);
                        pt_pfns[pt_len] = pd_pfn;
                        pt_len += 1;
                        slot.pt_frames_owned.fetch_sub(1, Ordering::Relaxed);
                        if pt_frame_adjust_entries(pdpt_pfn, -1) == 0 {
                            write_volatile(pml4.add(i4), 0);
                            pt_pfns[pt_len] = pdpt_pfn;
                            pt_len += 1;
                            slot.pt_frames_owned.fetch_sub(1, Ordering::Relaxed);
                            let _ = pt_frame_adjust_entries(pml4_pfn, -1);
                        }
                    }
                }
            }

            fence(Ordering::SeqCst);
            slot.mapped_4k_equiv_pages
                .fetch_sub(count as u64, Ordering::Relaxed);
            let flush_pages = if pt_len > 0 {
                FULL_FLUSH_SENTINEL
            } else {
                count as u64
            };
            let _ = shootdown_tlb(handle.id, &slot.active_cpus, base_vaddr, flush_pages);
            Ok((data_pfns, pt_pfns, pt_len))
        }
    })?;

    for (dst, &pfn) in out_frames.iter_mut().zip(data_pfns.iter()).take(count) {
        *dst = map_ref_dec_data_pfn_after_shootdown(pfn, PageOrder::Order4KiB);
    }
    for &pt_pfn in &pt_pfns[..pt_len] {
        let pt_h = pt_frame_unpin_for_reclaim(pt_pfn);
        free_pages(pt_h, PageOrder::Order4KiB, KERNEL_PT_LABEL);
    }
    Ok(count)
}

fn subset_member_peer_worker(_arg: u64) {
    crate::sched::preempt_disable();
    DEBT_READY_COUNT.fetch_add(1, Ordering::AcqRel);
    while !DEBT_STOP_WORKERS.load(Ordering::Acquire) {
        note_current_thread_progress();
        core::hint::spin_loop();
    }
    crate::sched::preempt_enable();
    exit_current_thread()
}

fn subset_non_member_worker(_arg: u64) {
    crate::sched::preempt_disable();
    let my_cpu = current_cpu_idx();
    let before = cpu_raw_tlb_ipi_entries(my_cpu);
    DEBT_READY_COUNT.fetch_add(1, Ordering::AcqRel);
    while !DEBT_STOP_WORKERS.load(Ordering::Acquire) {
        note_current_thread_progress();
        core::hint::spin_loop();
    }
    assert_eq!(current_cpu_idx(), my_cpu);
    let delta = cpu_raw_tlb_ipi_entries(my_cpu).saturating_sub(before);
    DEBT_NON_MEMBER_IPIS.fetch_add(delta, Ordering::AcqRel);
    crate::sched::preempt_enable();
    exit_current_thread()
}

fn subset_initiator_worker(_arg: u64) {
    let expected = DEBT_EXPECTED_READY.load(Ordering::Acquire);
    let vs = unpack_vs(DEBT_VS_TARGET.load(Ordering::Acquire));
    while DEBT_READY_COUNT.load(Ordering::Acquire) < expected {
        note_current_thread_progress();
        core::hint::spin_loop();
    }
    for i in 0..64usize {
        let prot = if (i & 1) == 0 { VmProt::USER_RO } else { VmProt::USER_RW };
        protect_4k(vs, 0x0090_0000, prot).unwrap();
        if (i & 15) == 0 {
            note_current_thread_progress();
        }
    }
    DEBT_STOP_WORKERS.store(true, Ordering::Release);
    exit_current_thread()
}

fn breakdown_and_batch_initiator_worker(_arg: u64) {
    let expected = DEBT_EXPECTED_READY.load(Ordering::Acquire);
    let vs = unpack_vs(DEBT_VS_TARGET.load(Ordering::Acquire));
    while DEBT_READY_COUNT.load(Ordering::Acquire) < expected {
        note_current_thread_progress();
        core::hint::spin_loop();
    }

    // SAFETY: Single initiator writes `DEBT_BUF`.
    let s = unsafe { &mut *DEBT_BUF.0.get() };
    for i in 0..DEBT_SAMPLES {
        let prot = if (i & 1) == 0 { VmProt::USER_RO } else { VmProt::USER_RW };
        protect_4k(vs, 0x0090_0000, prot).unwrap();
        let b = last_shootdown_breakdown();
        s.ipi_send[i] = b.ipi_send_cyc;
        s.remote_sched[i] = b.remote_sched_cyc;
        s.flush_exec[i] = b.flush_exec_cyc;
        s.ack_wait[i] = b.ack_wait_cyc;
        if (i & 15) == 0 {
            note_current_thread_progress();
        }
    }

    // Compare `16 x unmap_4k` (16 sequential shootdowns) vs `unmap_4k_batch` (1 shootdown)
    // while `0x0090_0000` anchors the PT so both measure steady-state leaf invalidation + IPIs.
    let lbl = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 610, 1);
    let mut frames = [FrameHandle { pfn: 0, generation: 0 }; MAX_BATCH_UNMAP];
    for slot in &mut frames {
        *slot = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::ZERO).unwrap();
    }
    let batch_base = 0x0090_1000u64;

    let mut unbatched_best = u64::MAX;
    let mut batched_best = u64::MAX;
    for _trial in 0..4 {
        for (idx, fh) in frames.iter().enumerate() {
            map_4k(vs, batch_base + (idx as u64) * PAGE_SIZE_4K, *fh, VmProt::USER_RW).unwrap();
        }
        let t_u0 = rdtsc();
        for (idx, slot) in frames.iter_mut().enumerate() {
            *slot = unmap_4k(vs, batch_base + (idx as u64) * PAGE_SIZE_4K).unwrap();
        }
        let u_cyc = rdtsc().wrapping_sub(t_u0).max(1);
        if u_cyc < unbatched_best {
            unbatched_best = u_cyc;
        }

        for (idx, fh) in frames.iter().enumerate() {
            map_4k(vs, batch_base + (idx as u64) * PAGE_SIZE_4K, *fh, VmProt::USER_RW).unwrap();
        }
        let mut out_reclaimed = [FrameHandle { pfn: 0, generation: 0 }; MAX_BATCH_UNMAP];
        let t_b0 = rdtsc();
        unmap_4k_batch(vs, batch_base, MAX_BATCH_UNMAP, &mut out_reclaimed).unwrap();
        let b_cyc = rdtsc().wrapping_sub(t_b0).max(1);
        if b_cyc < batched_best {
            batched_best = b_cyc;
        }
        frames = out_reclaimed;
        note_current_thread_progress();
    }

    for fh in frames {
        free_pages(fh, PageOrder::Order4KiB, lbl);
    }
    DEBT_UNBATCHED_CYC.store(unbatched_best, Ordering::Release);
    DEBT_BATCHED_CYC.store(batched_best, Ordering::Release);
    DEBT_STOP_WORKERS.store(true, Ordering::Release);
    exit_current_thread()
}

pub fn verify_tlb_debt_and_oversubscription(online_cpus: usize) {
    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);

    if online_cpus == 1 {
        let mut local_samples = [0u64; DEBT_SAMPLES];
        for slot in &mut local_samples {
            *slot = flush_range_local(0x0090_0000, 1);
        }
        sort_u64(&mut local_samples);
        serial_println!(
            "[DRY-TLB-DEBT] PASS: cpus=1 subset_target_only_ok=true non_member_ipis=0 counter_partition_exact=true ipi_send_p50_cyc=0 ipi_send_p99_cyc=0 remote_sched_p50_cyc=0 remote_sched_p99_cyc=0 flush_exec_p50_cyc={} flush_exec_p99_cyc={} ack_wait_p50_cyc=0 ack_wait_p99_cyc=0 unmap16_unbatched_cyc=0 unmap16_batched_cyc=0",
            local_samples[DEBT_SAMPLES / 2],
            local_samples[(DEBT_SAMPLES * 99) / 100]
        );
        return;
    }

    let lbl_t = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 610, 1);
    let lbl_o = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 611, 1);
    let vs_target = create_vspace(lbl_t).unwrap();
    let vs_other = create_vspace(lbl_o).unwrap();
    let anchor_t = alloc_pages(PageOrder::Order4KiB, lbl_t, AllocFlags::ZERO).unwrap();
    let anchor_o = alloc_pages(PageOrder::Order4KiB, lbl_o, AllocFlags::ZERO).unwrap();
    map_4k(vs_target, 0x0090_0000, anchor_t, VmProt::USER_RW).unwrap();
    map_4k(vs_other, 0x0090_0000, anchor_o, VmProt::USER_RW).unwrap();

    // 1. Subset target isolation proof (`Stage F.2`):
    //    - On 2 vCPUs: CPU 1 holds `vs_other` (0 members in `vs_target`).
    //    - On >= 3 vCPUs (`4, 8, 20`): half of remote CPUs hold `vs_target` while the other half hold `vs_other`.
    let remote_cpus = online_cpus - 1;
    let member_peers = if remote_cpus == 1 { 0 } else { remote_cpus / 2 };
    let non_member_peers = remote_cpus - member_peers;

    DEBT_VS_TARGET.store(pack_vs(vs_target), Ordering::Release);
    DEBT_EXPECTED_READY.store(remote_cpus, Ordering::Release);
    DEBT_READY_COUNT.store(0, Ordering::Release);
    DEBT_STOP_WORKERS.store(false, Ordering::Release);
    DEBT_NON_MEMBER_IPIS.store(0, Ordering::Release);
    reset_tlb_telemetry();

    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xe8, 1 + remote_cpus);
    assert!(try_spawn_thread_in_vspace(1, 0, KERNEL_VSPACE_ID, subset_initiator_worker, 0, true).is_some());
    for m in 0..member_peers {
        assert!(
            try_spawn_thread_in_vspace(1, 1 + m, vs_target.id, subset_member_peer_worker, m as u64, true)
                .is_some()
        );
    }
    for nm in 0..non_member_peers {
        let cpu = 1 + member_peers + nm;
        assert!(
            try_spawn_thread_in_vspace(1, cpu, vs_other.id, subset_non_member_worker, nm as u64, true)
                .is_some()
        );
    }
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);

    let non_member_ipis = DEBT_NON_MEMBER_IPIS.load(Ordering::Acquire);
    assert_eq!(non_member_ipis, 0, "Non-member CPUs received shootdown IPIs: {}", non_member_ipis);
    let sub_tel = tlb_telemetry();
    assert_eq!(sub_tel.ipis_sent, (member_peers as u64) * 64);
    assert_eq!(sub_tel.ipis_handled + sub_tel.coop_acks, sub_tel.ipis_sent);

    // 2. All-peers 4-component breakdown & batched vs unbatched shootdown (`Stage F.1, F.3, F.4`).
    DEBT_EXPECTED_READY.store(remote_cpus, Ordering::Release);
    DEBT_READY_COUNT.store(0, Ordering::Release);
    DEBT_STOP_WORKERS.store(false, Ordering::Release);
    reset_tlb_telemetry();

    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xe9, 1 + remote_cpus);
    assert!(
        try_spawn_thread_in_vspace(1, 0, KERNEL_VSPACE_ID, breakdown_and_batch_initiator_worker, 0, true)
            .is_some()
    );
    for p in 0..remote_cpus {
        assert!(
            try_spawn_thread_in_vspace(1, 1 + p, vs_target.id, subset_member_peer_worker, p as u64, true)
                .is_some()
        );
    }
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);

    let tel = tlb_telemetry();
    let partition_exact = tel.ipis_handled + tel.coop_acks == tel.ipis_sent;
    assert!(partition_exact, "TLB counter partition mismatch: {:?}", tel);

    let r_t = unmap_4k(vs_target, 0x0090_0000).unwrap();
    let r_o = unmap_4k(vs_other, 0x0090_0000).unwrap();
    free_pages(r_t, PageOrder::Order4KiB, lbl_t);
    free_pages(r_o, PageOrder::Order4KiB, lbl_o);
    destroy_vspace(vs_target).unwrap();
    destroy_vspace(vs_other).unwrap();
    flush_all_magazines_quiescent(online_cpus);

    let after = measure_fragmentation(online_cpus);
    assert_eq!(after.free_global_frames, before.free_global_frames);

    // SAFETY: Quiescent after epoch drain.
    let s = unsafe { &mut *DEBT_BUF.0.get() };
    sort_u64(&mut s.ipi_send);
    sort_u64(&mut s.remote_sched);
    sort_u64(&mut s.flush_exec);
    sort_u64(&mut s.ack_wait);
    let p50 = DEBT_SAMPLES / 2;
    let p99 = (DEBT_SAMPLES * 99) / 100;

    serial_println!(
        "[DRY-TLB-DEBT] PASS: cpus={} subset_target_only_ok=true non_member_ipis={} counter_partition_exact={} poll_acks={} leave_acks={} ipi_send_p50_cyc={} ipi_send_p99_cyc={} remote_sched_p50_cyc={} remote_sched_p99_cyc={} flush_exec_p50_cyc={} flush_exec_p99_cyc={} ack_wait_p50_cyc={} ack_wait_p99_cyc={} unmap16_unbatched_cyc={} unmap16_batched_cyc={}",
        online_cpus,
        non_member_ipis,
        partition_exact,
        tel.poll_acks,
        tel.leave_acks,
        s.ipi_send[p50],
        s.ipi_send[p99],
        s.remote_sched[p50],
        s.remote_sched[p99],
        s.flush_exec[p50],
        s.flush_exec[p99],
        s.ack_wait[p50],
        s.ack_wait[p99],
        DEBT_UNBATCHED_CYC.load(Ordering::Acquire),
        DEBT_BATCHED_CYC.load(Ordering::Acquire),
    );
}
