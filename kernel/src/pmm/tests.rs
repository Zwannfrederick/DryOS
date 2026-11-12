// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Milestone `1B-A` Verification Suite: Physical Memory Inventory, Cross-Domain Zeroing, OOM
//! Conservation, `Untyped` `split/retype/revoke`, `1/2/4/20`-vCPU `rdtsc` Contention Benchmarks,
//! Unpinned Worker-Spawner Liveness Regression, and 5 Deterministic Negative Fault Tests.

use core::cell::UnsafeCell;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::arch::{read_cr3, rdtsc};
use crate::pmm::untyped::{
    carve_untyped, release_untyped_to_pmm, untyped_retype, untyped_retype_checked, untyped_revoke,
    untyped_split, RetypeObjKind,
};
use crate::pmm::{
    adjust_frame_pins_and_maps, alloc_pages, bootstrap_alloc_pages, bootstrap_state,
    carve_contiguous_for_untyped, flush_all_magazines_quiescent, free_pages,
    free_pages_global_direct, inject_double_alloc_corrupt_slot_for_test, inventory,
    measure_fragmentation, perf_counters, query_frame_state, release_contiguous_from_untyped,
    reset_perf_counters, resolve_frame_ptr, scrub_and_free_frame, AllocFlags, BootstrapState,
    FrameHandle, FrameState, PageOrder, PmmError, HUGE_2M_FRAMES, LAPIC_MMIO_PHYS_BASE,
    LOW_MEM_GUARD_END_PFN, PAGE_SHIFT, PAGE_SIZE,
};
use crate::sched::preempt_tests::{begin_preemptive_epoch, drain_preemptive_epoch_and_audit};
use crate::sched::{
    current_cpu_idx, exit_current_thread, init_scheduler, note_current_thread_progress,
    set_preemptive_mode_active, tcb, try_spawn_kernel_thread_ex,
};
use crate::security::{SecDomain, SecError, SecFlags, SecLabel};
use crate::serial_println;

const LAT_SAMPLES: usize = 2_048;
const BENCH_ROUNDS_PER_CPU: usize = 512;
const BENCH_BATCH: usize = 16;

const LIVENESS_WAVES: usize = 4;
const LIVENESS_WAVE_WORKERS: usize = 12;
const LIVENESS_TOTAL_WORKERS: usize = LIVENESS_WAVES * LIVENESS_WAVE_WORKERS;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PmmFaultTest {
    None,
    DoubleFree,
    UseAfterFree,
    DoubleAlloc,
    OwnershipMismatch,
    UntypedRevoke,
}

impl PmmFaultTest {
    pub fn from_cmdline(cmdline: &str) -> Self {
        if cmdline.contains("pmm_fault=double_free") {
            Self::DoubleFree
        } else if cmdline.contains("pmm_fault=uaf") {
            Self::UseAfterFree
        } else if cmdline.contains("pmm_fault=double_alloc") {
            Self::DoubleAlloc
        } else if cmdline.contains("pmm_fault=owner_mismatch") {
            Self::OwnershipMismatch
        } else if cmdline.contains("pmm_fault=untyped_revoke") {
            Self::UntypedRevoke
        } else {
            Self::None
        }
    }
}

struct StaticScratch<T>(UnsafeCell<T>);
// SAFETY: Used strictly inside single-threaded BSP checks before SMP PMM epochs.
unsafe impl<T> Sync for StaticScratch<T> {}

static SAMPLE_BUF: StaticScratch<[u64; LAT_SAMPLES]> =
    StaticScratch(UnsafeCell::new([0u64; LAT_SAMPLES]));

static BENCH_OPS_COMPLETED: AtomicU64 = AtomicU64::new(0);
static LIVENESS_WAVE_DONE: AtomicUsize = AtomicUsize::new(0);
static LIVENESS_SPAWNED_OK: AtomicUsize = AtomicUsize::new(0);
static LIVENESS_ALL_WAVES_DONE: AtomicBool = AtomicBool::new(false);
static LIVENESS_MAX_WAVE_TSC: AtomicU64 = AtomicU64::new(0);

#[inline]
fn assert_page_all_zero(ptr: *const u8) {
    let qwords = ptr as *const u64;
    for i in 0..(PAGE_SIZE / 8) {
        // SAFETY: `ptr` points to a valid 4 KiB-aligned owned HHDM page.
        let v = unsafe { read_volatile(qwords.add(i)) };
        assert_eq!(v, 0, "Stale non-zero qword {:#x} at offset {}", v, i * 8);
    }
}

#[inline]
fn fill_page_pattern(ptr: *mut u8, pattern: u64) {
    let qwords = ptr as *mut u64;
    for i in 0..(PAGE_SIZE / 8) {
        // SAFETY: `ptr` points to a valid 4 KiB-aligned owned HHDM page.
        unsafe { write_volatile(qwords.add(i), pattern ^ (i as u64)) };
    }
}

fn sort_samples(buf: &mut [u64; LAT_SAMPLES]) {
    let mut gap = LAT_SAMPLES / 2;
    while gap > 0 {
        for i in gap..LAT_SAMPLES {
            let key = buf[i];
            let mut j = i;
            while j >= gap && buf[j - gap] > key {
                buf[j] = buf[j - gap];
                j -= gap;
            }
            buf[j] = key;
        }
        gap /= 2;
    }
}

fn trigger_negative_fault(fault: PmmFaultTest, online_cpus: usize) {
    let root_lbl = SecLabel::new(SecDomain::RootSystem, SecFlags::NONE, 1, 10);
    let user_lbl = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 200, 20);

    match fault {
        PmmFaultTest::None => {}
        PmmFaultTest::DoubleFree => {
            let h = alloc_pages(PageOrder::Order4KiB, user_lbl, AllocFlags::ZERO).unwrap();
            free_pages(h, PageOrder::Order4KiB, user_lbl);
            free_pages(h, PageOrder::Order4KiB, user_lbl);
        }
        PmmFaultTest::UseAfterFree => {
            let h1 = alloc_pages(PageOrder::Order4KiB, user_lbl, AllocFlags::ZERO).unwrap();
            free_pages(h1, PageOrder::Order4KiB, user_lbl);
            // Re-allocate the exact LIFO magazine slot to a new owner (`generation + 1`).
            let h2 = alloc_pages(PageOrder::Order4KiB, root_lbl, AllocFlags::ZERO).unwrap();
            assert_eq!(h1.pfn, h2.pfn);
            let _ = resolve_frame_ptr(h1, user_lbl);
        }
        PmmFaultTest::DoubleAlloc => {
            let h = alloc_pages(PageOrder::Order4KiB, user_lbl, AllocFlags::ZERO).unwrap();
            free_pages(h, PageOrder::Order4KiB, user_lbl);
            inject_double_alloc_corrupt_slot_for_test(user_lbl);
            let _ = alloc_pages(PageOrder::Order4KiB, user_lbl, AllocFlags::ZERO);
        }
        PmmFaultTest::OwnershipMismatch => {
            let h = alloc_pages(PageOrder::Order4KiB, root_lbl, AllocFlags::ZERO).unwrap();
            // Wrong security domain (`Userspace` vs `RootSystem`) attempting to free `h`.
            free_pages(h, PageOrder::Order4KiB, user_lbl);
        }
        PmmFaultTest::UntypedRevoke => {
            flush_all_magazines_quiescent(online_cpus);
            let root_cap = carve_untyped(4, root_lbl, 0).unwrap();
            let (child0, _child1) = untyped_split(root_lbl, root_cap, 0).unwrap();
            let _ = untyped_retype(root_lbl, child0, RetypeObjKind::Frame4KiB, 1, 0).unwrap();
            let _ = untyped_revoke(root_lbl, root_cap, 0).unwrap();
            // Stale `child0` capability token must be deterministically rejected after `revoke`.
            let _ = untyped_retype_checked(root_lbl, child0, RetypeObjKind::Frame4KiB, 1, 0);
        }
    }
}

fn verify_inventory_zeroing_and_oom(online_cpus: usize) {
    let inv = inventory();
    assert!(inv.hhdm_offset >= 0xffff_8000_0000_0000);
    assert_eq!(PageOrder::Order4KiB.frame_count(), 1);
    assert_eq!(PageOrder::Order2MiB.frame_count(), HUGE_2M_FRAMES);
    assert_eq!(bootstrap_state(), BootstrapState::Sealed);
    assert_eq!(bootstrap_alloc_pages(1, 1), Err(PmmError::BootstrapSealed));
    let k2m_lbl = SecLabel::new(SecDomain::Kernel, SecFlags::NONE, 0, 0);
    let h2m = alloc_pages(PageOrder::Order2MiB, k2m_lbl, AllocFlags::ZERO)
        .expect("Order2MiB allocation must succeed");
    assert_eq!((h2m.pfn as usize) & (HUGE_2M_FRAMES - 1), 0);
    free_pages(h2m, PageOrder::Order2MiB, k2m_lbl);

    // Verify `< 64 KiB` low guard, Local APIC MMIO `0xFEE0_0000`, active Limine `CR3`, and kernel
    // image physical base are strictly protected from general allocation.
    for low_pfn in 0..LOW_MEM_GUARD_END_PFN {
        assert_eq!(
            query_frame_state((low_pfn as u64) << PAGE_SHIFT),
            FrameState::ReservedOrMmio
        );
    }
    assert_eq!(query_frame_state(LAPIC_MMIO_PHYS_BASE), FrameState::ReservedOrMmio);
    let cr3_phys = read_cr3() & 0x000f_ffff_ffff_f000;
    assert_eq!(
        query_frame_state(cr3_phys),
        FrameState::BootloaderReclaimable,
        "Active Limine CR3 page table {:#x} must stay BootloaderReclaimable",
        cr3_phys
    );

    // ADR-007 Cross-Domain & Cross-Compartment Mandatory Zeroing Proof (`Kernel -> Root -> User10 -> User20`).
    let kern_lbl = SecLabel::new(SecDomain::Kernel, SecFlags::NONE, 0, 0);
    let root_lbl = SecLabel::new(SecDomain::RootSystem, SecFlags::NONE, 0, 1);
    let user10 = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 10, 2);
    let user20 = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 20, 3);
    let user30 = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 30, 4);

    let hk = alloc_pages(PageOrder::Order4KiB, kern_lbl, AllocFlags::ZERO).unwrap();
    let pk = resolve_frame_ptr(hk, kern_lbl);
    assert_page_all_zero(pk);
    fill_page_pattern(pk, 0xdead_beef_cafe_f00d);
    free_pages(hk, PageOrder::Order4KiB, kern_lbl);

    // Even with `UNINIT_SAME_DOMAIN_OK`, `Kernel -> RootSystem` MUST force `rep stosq` zeroing.
    let hr = alloc_pages(
        PageOrder::Order4KiB,
        root_lbl,
        AllocFlags::UNINIT_SAME_DOMAIN_OK,
    )
    .unwrap();
    assert_eq!(hr.pfn, hk.pfn);
    assert_eq!(hr.generation, hk.generation + 1);
    let pr = resolve_frame_ptr(hr, root_lbl);
    assert_page_all_zero(pr);
    fill_page_pattern(pr, 0xbadc_0ffe_e0dd_f00d);
    free_pages(hr, PageOrder::Order4KiB, root_lbl);

    // `RootSystem -> Userspace(10)` MUST force zeroing.
    let hu10 = alloc_pages(
        PageOrder::Order4KiB,
        user10,
        AllocFlags::UNINIT_SAME_DOMAIN_OK,
    )
    .unwrap();
    assert_eq!(hu10.pfn, hk.pfn);
    let pu10 = resolve_frame_ptr(hu10, user10);
    assert_page_all_zero(pu10);
    fill_page_pattern(pu10, 0x1111_2222_3333_4444);
    free_pages(hu10, PageOrder::Order4KiB, user10);

    // `Userspace(10) -> Userspace(20)` (different compartment!) MUST also force zeroing.
    let hu20 = alloc_pages(
        PageOrder::Order4KiB,
        user20,
        AllocFlags::UNINIT_SAME_DOMAIN_OK,
    )
    .unwrap();
    assert_eq!(hu20.pfn, hk.pfn);
    let pu20 = resolve_frame_ptr(hu20, user20);
    assert_page_all_zero(pu20);
    fill_page_pattern(pu20, 0x5555_6666_7777_8888);
    free_pages(hu20, PageOrder::Order4KiB, user20);

    // Same `(Userspace, compartment=20)` with `UNINIT_SAME_DOMAIN_OK` elides redundant zeroing;
    // `scrub_and_free_frame` marks `VerifiedZero` so `Userspace(30)` `AllocFlags::ZERO` also elides!
    assert!(perf_counters().zero_runs >= 4);
    reset_perf_counters();
    let hu20_reuse = alloc_pages(
        PageOrder::Order4KiB,
        user20,
        AllocFlags::UNINIT_SAME_DOMAIN_OK,
    )
    .unwrap();
    assert_eq!(perf_counters().zero_elided, 1);
    scrub_and_free_frame(hu20_reuse, user20);

    let hu30 = alloc_pages(PageOrder::Order4KiB, user30, AllocFlags::ZERO).unwrap();
    assert_eq!(hu30.pfn, hk.pfn);
    assert_eq!(perf_counters().zero_elided, 2);
    assert_page_all_zero(resolve_frame_ptr(hu30, user30));
    free_pages(hu30, PageOrder::Order4KiB, user30);
    flush_all_magazines_quiescent(online_cpus);

    // Full-RAM OOM Exhaustion & 100% Reclaim Conservation Test.
    // Link allocated `(pfn, generation)` handles intrusively inside the first 8 bytes of each owned
    // 4 KiB HHDM page so OOM verification scales to arbitrary RAM (`512M..64G+`) with `0 B` `.bss`.
    const NONE_PACKED: u64 = u64::MAX;
    let mut head_packed = NONE_PACKED;
    let mut count = 0usize;
    while let Ok(h) = alloc_pages(
        PageOrder::Order4KiB,
        kern_lbl,
        AllocFlags::UNINIT_SAME_DOMAIN_OK,
    ) {
        assert!(h.pfn >= LOW_MEM_GUARD_END_PFN && h.pfn < inv.max_tracked_pfn);
        let slot = resolve_frame_ptr(h, kern_lbl) as *mut u64;
        // SAFETY: `h` is exclusively owned by `kern_lbl` on single-threaded BSP.
        unsafe { write_volatile(slot, head_packed) };
        head_packed = (h.pfn as u64) | ((h.generation as u64) << 32);
        count += 1;
    }
    assert_eq!(
        count as u64, inv.free_init_frames,
        "OOM allocated {} frames != expected free_init {}",
        count, inv.free_init_frames
    );
    assert_eq!(
        alloc_pages(PageOrder::Order4KiB, kern_lbl, AllocFlags::ZERO),
        Err(PmmError::OutOfMemory)
    );
    while head_packed != NONE_PACKED {
        let h = FrameHandle {
            pfn: head_packed as u32,
            generation: (head_packed >> 32) as u32,
        };
        let slot = resolve_frame_ptr(h, kern_lbl) as *const u64;
        // SAFETY: Read intrusive predecessor link before returning `h` to `PMM`.
        let next_packed = unsafe { read_volatile(slot) };
        free_pages(h, PageOrder::Order4KiB, kern_lbl);
        head_packed = next_packed;
    }
    flush_all_magazines_quiescent(online_cpus);

    let post_oom = measure_fragmentation(online_cpus);
    assert_eq!(post_oom.free_cached_frames, 0);
    let leaked = inv.free_init_frames.saturating_sub(post_oom.free_global_frames);
    assert_eq!(leaked, 0, "Frame leak after OOM reclaim: {}", leaked);

    serial_println!(
        "[DRY-PMM] PASS: max_pfn={} usable_frames={} free_init={} reclaimable_frames={} kernel_meta_frames={} reserved_frames={} meta_bytes={} meta_overhead_bps={} bootstrap_sealed=true cross_domain_zero_ok=true oom_reclaim_conserved=true leaked_frames={}",
        inv.max_tracked_pfn,
        inv.usable_frames,
        inv.free_init_frames,
        inv.reclaimable_frames,
        inv.kernel_meta_frames,
        inv.reserved_mmio_frames,
        inv.meta_bytes,
        inv.meta_overhead_bps,
        leaked
    );
}

fn verify_untyped_lifecycle(online_cpus: usize) {
    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);

    let sys_lbl = SecLabel::new(SecDomain::RootSystem, SecFlags::DEVICE_DMA, 7, 100);
    let root_cap = carve_untyped(16, sys_lbl, 10).expect("carve_untyped(16) failed");
    let (child0, child1) = untyped_split(sys_lbl, root_cap, 5).expect("untyped_split failed");
    // Parent with active children cannot be directly retyped (`watermark` overlap prevention).
    assert_eq!(
        untyped_retype(sys_lbl, root_cap, RetypeObjKind::Frame4KiB, 1, 5),
        Err(SecError::Inval)
    );

    let f_slice = untyped_retype(sys_lbl, child0, RetypeObjKind::Frame4KiB, 2, 5).unwrap();
    assert_page_all_zero(f_slice.hhdm_ptr);
    fill_page_pattern(f_slice.hhdm_ptr, 0xcafe_babe_1234_5678);

    let tcb_slice = untyped_retype(sys_lbl, child0, RetypeObjKind::TcbObj, 2, 5).unwrap();
    assert_eq!(tcb_slice.byte_len, 2048);
    let ep_slice = untyped_retype(sys_lbl, child0, RetypeObjKind::EndpointObj, 4, 5).unwrap();
    assert_eq!(ep_slice.byte_len, 256);
    let ntfn_slice = untyped_retype(sys_lbl, child0, RetypeObjKind::NotificationObj, 4, 5).unwrap();
    assert_eq!(ntfn_slice.byte_len, 256);

    let pt_slice = untyped_retype(sys_lbl, child1, RetypeObjKind::PageTable4KiB, 1, 5).unwrap();
    assert_page_all_zero(pt_slice.hhdm_ptr);
    fill_page_pattern(pt_slice.hhdm_ptr, 0xfeed_face_dead_beef);
    let _cnode_slice = untyped_retype(sys_lbl, child1, RetypeObjKind::CNodeSlotBlock, 1, 5).unwrap();

    // Active IOMMU DMA pin on `f_slice` MUST block `untyped_revoke` with `Err(SecError::BusyPinned)`
    // without partially revoking `child1`.
    adjust_frame_pins_and_maps(f_slice.backing_pfn, f_slice.backing_pfn_gen, 1, 0);
    assert_eq!(untyped_revoke(sys_lbl, root_cap, 5), Err(SecError::BusyPinned));
    assert!(untyped_retype(sys_lbl, child1, RetypeObjKind::Frame4KiB, 1, 5).is_ok());

    // Unpin DMA -> `untyped_revoke` succeeds, bumps generations, scrubs memory, resets watermarks.
    adjust_frame_pins_and_maps(f_slice.backing_pfn, f_slice.backing_pfn_gen, -1, 0);
    let reclaimed = untyped_revoke(sys_lbl, root_cap, 5).expect("untyped_revoke failed");
    assert!(reclaimed >= 4);

    assert_eq!(
        untyped_retype(sys_lbl, child0, RetypeObjKind::Frame4KiB, 1, 5),
        Err(SecError::Revoked)
    );
    assert_eq!(
        untyped_retype(sys_lbl, child1, RetypeObjKind::Frame4KiB, 1, 5),
        Err(SecError::Revoked)
    );

    // Re-retyping from freshly revoked `root_cap` yields clean zeroed memory at offset 0.
    let reused = untyped_retype(sys_lbl, root_cap, RetypeObjKind::Frame4KiB, 1, 5).unwrap();
    assert_eq!(reused.backing_pfn, f_slice.backing_pfn);
    assert_eq!(reused.backing_pfn_gen, f_slice.backing_pfn_gen + 1);
    assert_page_all_zero(reused.hhdm_ptr);

    let _ = untyped_revoke(sys_lbl, root_cap, 5).unwrap();
    release_untyped_to_pmm(sys_lbl, root_cap, 5).expect("release_untyped_to_pmm failed");

    let after = measure_fragmentation(online_cpus);
    assert_eq!(after.free_global_frames, before.free_global_frames);
    assert_eq!(after.contig_2m_blocks, before.contig_2m_blocks);

    serial_println!(
        "[DRY-UNTYPED] PASS: region_frames=16 split_ok=true retype_ok=true pin_guard_ok=true revoke_scrub_ok=true conserved=true"
    );
}

fn bench_global_direct_worker(arg: u64) {
    let lbl = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 100 + (arg as u16), 1);
    let mut batch = [FrameHandle { pfn: 0, generation: 0 }; BENCH_BATCH];
    for r in 0..BENCH_ROUNDS_PER_CPU {
        for slot in &mut batch {
            *slot = alloc_pages(
                PageOrder::Order4KiB,
                lbl,
                AllocFlags(AllocFlags::BYPASS_PERCPU_CACHE.0 | AllocFlags::UNINIT_SAME_DOMAIN_OK.0),
            )
            .unwrap();
        }
        for slot in &batch {
            free_pages_global_direct(*slot, lbl);
        }
        if (r & 15) == 0 {
            note_current_thread_progress();
        }
    }
    BENCH_OPS_COMPLETED.fetch_add((BENCH_ROUNDS_PER_CPU * BENCH_BATCH * 2) as u64, Ordering::AcqRel);
    exit_current_thread()
}

fn bench_magazine_worker(arg: u64) {
    let lbl = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 300 + (arg as u16), 1);
    let mut batch = [FrameHandle { pfn: 0, generation: 0 }; 48];
    for r in 0..BENCH_ROUNDS_PER_CPU {
        // Every 8th round bursts 48 frames (> MAGAZINE_BATCH=32) to exercise multi-CPU refill/drain;
        // remaining rounds stay in CPU-local magazine (`>= 95%` cache hit rate).
        let n = if (r & 7) == 7 { 48 } else { BENCH_BATCH };
        for slot in batch.iter_mut().take(n) {
            *slot = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::UNINIT_SAME_DOMAIN_OK).unwrap();
        }
        for slot in batch.iter().take(n) {
            free_pages(*slot, PageOrder::Order4KiB, lbl);
        }
        BENCH_OPS_COMPLETED.fetch_add((n * 2) as u64, Ordering::Relaxed);
        if (r & 15) == 0 {
            note_current_thread_progress();
        }
    }
    exit_current_thread()
}

fn measure_pmm_performance_and_contention(online_cpus: usize, tsc_mhz: u64) {
    flush_all_magazines_quiescent(online_cpus);
    let frag_before = measure_fragmentation(online_cpus);

    // Fragmentation probe: pin 1 single 4 KiB frame at the start of 16 distinct 2 MiB-aligned
    // blocks (`contig_2m_mid == contig_2m_before - 16`), then release and verify 100% recovery.
    let frag_lbl = SecLabel::new(SecDomain::RootSystem, SecFlags::NONE, 0, 0);
    let mut pinned_pfns = [0u32; 16];
    for slot in &mut pinned_pfns {
        *slot = carve_contiguous_for_untyped(1, HUGE_2M_FRAMES, frag_lbl).unwrap();
    }
    let frag_mid = measure_fragmentation(online_cpus);
    assert_eq!(frag_mid.contig_2m_blocks + 16, frag_before.contig_2m_blocks);
    for pfn in pinned_pfns {
        release_contiguous_from_untyped(pfn, 1, frag_lbl);
    }

    // 1. Single-CPU `rdtsc` cycle percentiles (`p50 / p95 / p99`) for default zeroed `alloc_pages`,
    //    `free_pages`, `rep stosq` zeroing, and zero-elided (`VerifiedZero`/same-domain) fast path.
    let bench_lbl = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 50, 1);
    let mut warm = [FrameHandle { pfn: 0, generation: 0 }; 32];
    for w in &mut warm {
        *w = alloc_pages(PageOrder::Order4KiB, bench_lbl, AllocFlags::ZERO).unwrap();
    }
    for w in &warm {
        free_pages(*w, PageOrder::Order4KiB, bench_lbl);
    }

    // SAFETY: Single-threaded BSP quiescent benchmark prior to SMP epoch.
    let samples = unsafe { &mut *SAMPLE_BUF.0.get() };
    let mut tmp_handles = [FrameHandle { pfn: 0, generation: 0 }; 32];

    for chunk in 0..(LAT_SAMPLES / 32) {
        for i in 0..32 {
            let t0 = rdtsc();
            let h = alloc_pages(PageOrder::Order4KiB, bench_lbl, AllocFlags::ZERO).unwrap();
            let dt = rdtsc().wrapping_sub(t0);
            tmp_handles[i] = h;
            samples[chunk * 32 + i] = dt;
        }
        for h in &tmp_handles {
            free_pages(*h, PageOrder::Order4KiB, bench_lbl);
        }
    }
    sort_samples(samples);
    let (alloc_p50, alloc_p95, alloc_p99) = (samples[1_024], samples[1_945], samples[2_027]);

    for chunk in 0..(LAT_SAMPLES / 32) {
        for i in 0..32 {
            let t0 = rdtsc();
            let h = alloc_pages(PageOrder::Order4KiB, bench_lbl, AllocFlags::UNINIT_SAME_DOMAIN_OK).unwrap();
            let dt = rdtsc().wrapping_sub(t0);
            tmp_handles[i] = h;
            samples[chunk * 32 + i] = dt;
        }
        for h in &tmp_handles {
            free_pages(*h, PageOrder::Order4KiB, bench_lbl);
        }
    }
    sort_samples(samples);
    let zero_elided_p50 = samples[1_024];

    for chunk in 0..(LAT_SAMPLES / 32) {
        for h in &mut tmp_handles {
            *h = alloc_pages(PageOrder::Order4KiB, bench_lbl, AllocFlags::UNINIT_SAME_DOMAIN_OK).unwrap();
        }
        for (i, h) in tmp_handles.iter().enumerate() {
            let t0 = rdtsc();
            free_pages(*h, PageOrder::Order4KiB, bench_lbl);
            samples[chunk * 32 + i] = rdtsc().wrapping_sub(t0);
        }
    }
    sort_samples(samples);
    let (free_p50, free_p95, free_p99) = (samples[1_024], samples[1_945], samples[2_027]);

    let zh = alloc_pages(PageOrder::Order4KiB, bench_lbl, AllocFlags::ZERO).unwrap();
    let zptr = resolve_frame_ptr(zh, bench_lbl);
    for s in samples.iter_mut() {
        let t0 = rdtsc();
        crate::pmm::zero_frame_4k(zptr);
        *s = rdtsc().wrapping_sub(t0);
    }
    free_pages(zh, PageOrder::Order4KiB, bench_lbl);
    sort_samples(samples);
    let (zero_p50, zero_p95, zero_p99) = (samples[1_024], samples[1_945], samples[2_027]);
    flush_all_magazines_quiescent(online_cpus);

    // 2. Multi-CPU Contention Run A: Global-Direct (`BYPASS_PERCPU_CACHE`) across `online_cpus`.
    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    reset_perf_counters();
    BENCH_OPS_COMPLETED.store(0, Ordering::Release);
    begin_preemptive_epoch(0xb0, online_cpus);
    let t_glob0 = rdtsc();
    for cpu in 0..online_cpus {
        assert!(
            try_spawn_kernel_thread_ex(1, cpu, bench_global_direct_worker, cpu as u64, true).is_some()
        );
    }
    drain_preemptive_epoch_and_audit(online_cpus);
    let glob_cycles = rdtsc().wrapping_sub(t_glob0).max(1);
    let global_spins = perf_counters().global_lock_spins;
    let glob_ops = BENCH_OPS_COMPLETED.load(Ordering::Acquire);
    let global_mops_x100 = (glob_ops * tsc_mhz * 100) / glob_cycles;

    // 3. Multi-CPU Contention Run B: Per-CPU `CLI` Magazine across `online_cpus`.
    init_scheduler(online_cpus);
    reset_perf_counters();
    BENCH_OPS_COMPLETED.store(0, Ordering::Release);
    begin_preemptive_epoch(0xb1, online_cpus);
    let t_mag0 = rdtsc();
    for cpu in 0..online_cpus {
        assert!(try_spawn_kernel_thread_ex(1, cpu, bench_magazine_worker, cpu as u64, true).is_some());
    }
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);
    let mag_cycles = rdtsc().wrapping_sub(t_mag0).max(1);
    let stats_mag = perf_counters();
    let mag_ops = BENCH_OPS_COMPLETED.load(Ordering::Acquire);
    let throughput_mops_x100 = (mag_ops * tsc_mhz * 100) / mag_cycles;
    let total_alloc_reqs = (stats_mag.mag_hits + stats_mag.mag_misses).max(1);
    let mag_hit_pct = (stats_mag.mag_hits * 100) / total_alloc_reqs;

    flush_all_magazines_quiescent(online_cpus);
    let frag_after = measure_fragmentation(online_cpus);
    assert_eq!(frag_after.free_global_frames, frag_before.free_global_frames);
    assert_eq!(frag_after.contig_2m_blocks, frag_before.contig_2m_blocks);

    serial_println!(
        "[DRY-PMM-BENCH] PASS: cpus={} samples={} rounds_per_cpu={} alloc_p50_cyc={} alloc_p95_cyc={} alloc_p99_cyc={} free_p50_cyc={} free_p95_cyc={} free_p99_cyc={} zero_4k_p50_cyc={} zero_4k_p95_cyc={} zero_4k_p99_cyc={} zero_elided_p50_cyc={} throughput_mops_x100={} global_mops_x100={} mag_hit_pct={} global_spins={} mag_spins={} contig_2m_before={} contig_2m_mid={} contig_2m_after={} ext_frag_bps_after={}",
        online_cpus,
        LAT_SAMPLES,
        BENCH_ROUNDS_PER_CPU,
        alloc_p50,
        alloc_p95,
        alloc_p99,
        free_p50,
        free_p95,
        free_p99,
        zero_p50,
        zero_p95,
        zero_p99,
        zero_elided_p50,
        throughput_mops_x100,
        global_mops_x100,
        mag_hit_pct,
        global_spins,
        stats_mag.global_lock_spins,
        frag_before.contig_2m_blocks,
        frag_mid.contig_2m_blocks,
        frag_after.contig_2m_blocks,
        frag_after.external_frag_bps,
    );
}

fn liveness_cpu_spinner_worker(_arg: u64) {
    // Non-yielding CPU-bound competitor (`0` `yield_now()` calls!): relies exclusively on hardware
    // Local APIC timer preemption until the unpinned `T_spawner` completes all 4 waves.
    while !LIVENESS_ALL_WAVES_DONE.load(Ordering::Acquire) {
        note_current_thread_progress();
        core::hint::spin_loop();
    }
    exit_current_thread()
}

fn liveness_pmm_wave_worker(arg: u64) {
    let comp = 1000 + (arg as u16);
    let lbl = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, comp, 77);
    let h0 = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::ZERO).unwrap();
    let h1 = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::ZERO).unwrap();
    let p0 = resolve_frame_ptr(h0, lbl);
    assert_page_all_zero(p0);
    // SAFETY: Exclusively owned 4 KiB HHDM page.
    unsafe { write_volatile(p0 as *mut u64, 0x7777_0000_0000_0000 ^ arg) };
    free_pages(h1, PageOrder::Order4KiB, lbl);
    free_pages(h0, PageOrder::Order4KiB, lbl);
    note_current_thread_progress();
    LIVENESS_WAVE_DONE.fetch_add(1, Ordering::AcqRel);
    exit_current_thread()
}

fn unpinned_spawner_worker(online_cpus: u64) {
    let online = online_cpus as usize;
    let mut max_wave_tsc = 0u64;
    for wave in 0..LIVENESS_WAVES {
        let t0 = rdtsc();
        for i in 0..LIVENESS_WAVE_WORKERS {
            let id = (wave * LIVENESS_WAVE_WORKERS + i) as u64;
            let dst_cpu = (current_cpu_idx() + i + 1) % online;
            // Pure preemptible wait (`0` `yield_now()` calls, `0` BSP pinning!).
            while try_spawn_kernel_thread_ex(1, dst_cpu, liveness_pmm_wave_worker, id, true).is_none() {
                note_current_thread_progress();
                core::hint::spin_loop();
            }
            LIVENESS_SPAWNED_OK.fetch_add(1, Ordering::AcqRel);
        }
        let target_done = (wave + 1) * LIVENESS_WAVE_WORKERS;
        while LIVENESS_WAVE_DONE.load(Ordering::Acquire) < target_done {
            note_current_thread_progress();
            core::hint::spin_loop();
        }
        let dt = rdtsc().wrapping_sub(t0);
        if dt > max_wave_tsc {
            max_wave_tsc = dt;
        }
    }
    LIVENESS_MAX_WAVE_TSC.store(max_wave_tsc, Ordering::Release);
    LIVENESS_ALL_WAVES_DONE.store(true, Ordering::Release);
    exit_current_thread()
}

/// Requirement 11: Unpinned worker-thread spawner (`T_spawner`, `slot >= MAX_CPUS`) competing at
/// equal priority (`prio = 1`) against oversubscribed non-yielding CPU-bound spinners (`0` `yield_now()`,
/// `0` BSP pinning) across `1, 2, 4, 20` vCPUs.
fn verify_spawner_liveness_regression(online_cpus: usize, tsc_mhz: u64) {
    let n_spinners = (online_cpus + 2).min(22);
    let total_epoch_threads = 1 + n_spinners + LIVENESS_TOTAL_WORKERS;

    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    LIVENESS_WAVE_DONE.store(0, Ordering::Release);
    LIVENESS_SPAWNED_OK.store(0, Ordering::Release);
    LIVENESS_ALL_WAVES_DONE.store(false, Ordering::Release);
    LIVENESS_MAX_WAVE_TSC.store(0, Ordering::Release);

    begin_preemptive_epoch(0xb2, total_epoch_threads);
    for s in 0..n_spinners {
        assert!(
            try_spawn_kernel_thread_ex(1, s % online_cpus, liveness_cpu_spinner_worker, s as u64, true)
                .is_some()
        );
    }
    let spawner_tid = try_spawn_kernel_thread_ex(
        1,
        online_cpus - 1,
        unpinned_spawner_worker,
        online_cpus as u64,
        true,
    )
    .expect("Failed to spawn unpinned T_spawner");
    let spawner_slot = spawner_tid.slot();

    // BSP immediately enters general worker dispatch (`slot 0` is NOT pinned as spawner!).
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);
    flush_all_magazines_quiescent(online_cpus);

    let spawned = LIVENESS_SPAWNED_OK.load(Ordering::Acquire);
    let exited = LIVENESS_WAVE_DONE.load(Ordering::Acquire);
    let spawner_preempts = tcb(spawner_slot).preempted_count.load(Ordering::Acquire);
    let max_wave_wait_us = LIVENESS_MAX_WAVE_TSC.load(Ordering::Acquire) / tsc_mhz.max(1);
    assert_eq!(spawned, LIVENESS_TOTAL_WORKERS);
    assert_eq!(exited, LIVENESS_TOTAL_WORKERS);

    let post = measure_fragmentation(online_cpus);
    assert_eq!(post.free_global_frames, inventory().free_init_frames);

    serial_println!(
        "[DRY-LIVENESS-REG] PASS: cpus={} spinners={} unpinned_spawner_waves={} spawned={} exited={} spawner_preempts={} max_wave_wait_us={} starved=0",
        online_cpus,
        n_spinners,
        LIVENESS_WAVES,
        spawned,
        exited,
        spawner_preempts,
        max_wave_wait_us,
    );
}

pub fn verify_pmm_and_untyped(online_cpus: usize, tsc_mhz: u64, fault: PmmFaultTest) {
    trigger_negative_fault(fault, online_cpus);
    verify_inventory_zeroing_and_oom(online_cpus);
    verify_untyped_lifecycle(online_cpus);
    measure_pmm_performance_and_contention(online_cpus, tsc_mhz);
    verify_spawner_liveness_regression(online_cpus, tsc_mhz);
}
