// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `Milestone 1B-C` Stage E Synthetic In-Kernel `rdtsc` Microbenchmarks:
//! Fast-Path Lookup, `ADR-007` Security Check, `derive / mint / revoke` Latency, Child-Count
//! Scaling (`k = 1, 8, 32, 64`), and Multi-CPU `CNode` Contention (`1..20` vCPUs).

use core::cell::UnsafeCell;
use core::hint::black_box;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::arch::rdtsc;
use crate::cspace::objects::cspace_alloc_frame;
use crate::cspace::revoke::{cspace_delete_cap, cspace_destroy_object, cspace_revoke_children};
use crate::cspace::{
    create_cspace, cspace_copy, cspace_derive, cspace_lookup_fast, cspace_metadata_stats,
    destroy_cspace, CSpaceId, CapPtr,
};
use crate::pmm::PageOrder;
use crate::sched::preempt_tests::{begin_preemptive_epoch, drain_preemptive_epoch_and_audit};
use crate::sched::{
    exit_current_thread, init_scheduler, note_current_thread_progress, set_preemptive_mode_active,
    try_spawn_thread_in_vspace,
};
use crate::security::{
    authorize_cspace_fast, encode_compartment, CapRights, SecDomain, SecFlags, SecLabel,
};
use crate::serial_println;
use crate::vspace::KERNEL_VSPACE_ID;

const BENCH_SAMPLES: usize = 512;

struct BenchBuf(UnsafeCell<[u64; BENCH_SAMPLES]>);
// SAFETY: Used exclusively during single-threaded quiescent benchmark collection on BSP.
unsafe impl Sync for BenchBuf {}

static BENCH_ARR_A: BenchBuf = BenchBuf(UnsafeCell::new([0; BENCH_SAMPLES]));
static BENCH_ARR_B: BenchBuf = BenchBuf(UnsafeCell::new([0; BENCH_SAMPLES]));

static CONTEND_CSPACES: [AtomicU64; 32] = [const { AtomicU64::new(0) }; 32];
static CONTEND_CAPS: [AtomicU64; 32] = [const { AtomicU64::new(0) }; 32];
static CONTEND_EXPECTED: AtomicUsize = AtomicUsize::new(0);
static CONTEND_READY: AtomicUsize = AtomicUsize::new(0);
static CONTEND_P50_SUM: AtomicU64 = AtomicU64::new(0);
static CONTEND_P99_MAX: AtomicU64 = AtomicU64::new(0);
static CONTEND_MAX_ELAPSED: AtomicU64 = AtomicU64::new(1);

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

fn contention_bench_worker(cpu_arg: u64) {
    let idx = cpu_arg as usize;
    let cs = CONTEND_CSPACES[idx].load(Ordering::Acquire) as CSpaceId;
    let root = CapPtr(CONTEND_CAPS[idx].load(Ordering::Acquire));
    let lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(3, (idx as u8) + 1),
        1,
    );
    let expected = CONTEND_EXPECTED.load(Ordering::Acquire);

    CONTEND_READY.fetch_add(1, Ordering::AcqRel);
    while CONTEND_READY.load(Ordering::Acquire) < expected {
        note_current_thread_progress();
        core::hint::spin_loop();
    }

    let mut local_lat = [0u64; 128];
    let t_start = rdtsc();
    for slot in &mut local_lat {
        let t0 = rdtsc();
        let r = black_box(cspace_lookup_fast(cs, lbl, black_box(root), CapRights::READ, 1));
        assert!(r.is_ok());
        *slot = rdtsc().wrapping_sub(t0).max(1);
    }
    for _ in 0..384 {
        let r = black_box(cspace_lookup_fast(cs, lbl, black_box(root), CapRights::READ, 1));
        assert!(r.is_ok());
    }
    for _ in 0..64 {
        let ch = cspace_derive(cs, lbl, root, CapRights::READ, 0, 0, 1).unwrap();
        cspace_delete_cap(cs, lbl, ch, 1).unwrap();
        note_current_thread_progress();
    }
    let elapsed = rdtsc().wrapping_sub(t_start).max(1);

    sort_u64(&mut local_lat);
    CONTEND_P50_SUM.fetch_add(local_lat[64], Ordering::Relaxed);
    let p99 = local_lat[126];
    let mut cur_max = CONTEND_P99_MAX.load(Ordering::Relaxed);
    while p99 > cur_max {
        match CONTEND_P99_MAX.compare_exchange_weak(
            cur_max,
            p99,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(v) => cur_max = v,
        }
    }
    let mut cur_el = CONTEND_MAX_ELAPSED.load(Ordering::Relaxed);
    while elapsed > cur_el {
        match CONTEND_MAX_ELAPSED.compare_exchange_weak(
            cur_el,
            elapsed,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(v) => cur_el = v,
        }
    }
    exit_current_thread()
}

fn measure_revoke_k_children_median(cs: CSpaceId, lbl: SecLabel, root: CapPtr, k: usize) -> u64 {
    let mut runs = [0u64; 9];
    for slot in &mut runs {
        for _ in 0..k {
            let _ = cspace_derive(cs, lbl, root, CapRights::READ, 0, 0, 1).unwrap();
        }
        let t0 = rdtsc();
        let cnt = cspace_revoke_children(cs, lbl, root, 1).unwrap();
        *slot = rdtsc().wrapping_sub(t0).max(1);
        assert_eq!(cnt, k);
    }
    sort_u64(&mut runs);
    runs[4]
}

pub fn measure_cspace_benchmarks(online_cpus: usize, tsc_mhz: u64) {
    let lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(4, 1),
        1,
    );
    let (cs, _) = create_cspace(lbl, 0x70).unwrap();
    let (root_fr, _) = cspace_alloc_frame(cs, lbl, lbl, PageOrder::Order4KiB, false, 0).unwrap();

    // SAFETY: Quiescent single-threaded BSP benchmark prior to contention workers.
    let buf_a = unsafe { &mut *BENCH_ARR_A.0.get() };
    let buf_b = unsafe { &mut *BENCH_ARR_B.0.get() };

    for slot in buf_a.iter_mut() {
        let t0 = rdtsc();
        let res = black_box(cspace_lookup_fast(
            black_box(cs),
            black_box(lbl),
            black_box(root_fr),
            CapRights::READ,
            1,
        ));
        assert!(res.is_ok());
        *slot = rdtsc().wrapping_sub(t0).max(1);
    }
    sort_u64(buf_a);
    let (lk_p50, lk_p95, lk_p99) = (buf_a[256], buf_a[486], buf_a[506]);

    for slot in buf_a.iter_mut() {
        let t0 = rdtsc();
        let ok = black_box(authorize_cspace_fast(
            black_box(lbl),
            black_box(lbl),
            CapRights::FRAME_DATA_ROOT,
            CapRights::READ,
            10,
            1,
        ));
        assert!(ok.is_ok());
        *slot = rdtsc().wrapping_sub(t0).max(1);
    }
    sort_u64(buf_a);
    let (sec_p50, sec_p95, sec_p99) = (buf_a[256], buf_a[486], buf_a[506]);

    for i in 0..BENCH_SAMPLES {
        let t_d = rdtsc();
        let ch = cspace_copy(cs, lbl, root_fr, 1).unwrap();
        buf_a[i] = rdtsc().wrapping_sub(t_d).max(1);

        let t_r = rdtsc();
        let cnt = cspace_revoke_children(cs, lbl, root_fr, 1).unwrap();
        buf_b[i] = rdtsc().wrapping_sub(t_r).max(1);
        assert_eq!(cnt, 1);
        let _ = ch;
    }
    sort_u64(buf_a);
    sort_u64(buf_b);
    let (der_p50, der_p95, der_p99) = (buf_a[256], buf_a[486], buf_a[506]);
    let (rev1_p50, rev1_p95, rev1_p99) = (buf_b[256], buf_b[486], buf_b[506]);

    for slot in buf_a.iter_mut() {
        let t_m = rdtsc();
        let m = cspace_derive(cs, lbl, root_fr, CapRights::READ, 8, 0xbeef, 1).unwrap();
        *slot = rdtsc().wrapping_sub(t_m).max(1);
        cspace_delete_cap(cs, lbl, m, 1).unwrap();
    }
    sort_u64(buf_a);
    let (mint_p50, mint_p95, mint_p99) = (buf_a[256], buf_a[486], buf_a[506]);

    let rev_1 = measure_revoke_k_children_median(cs, lbl, root_fr, 1);
    let rev_8 = measure_revoke_k_children_median(cs, lbl, root_fr, 8);
    let rev_32 = measure_revoke_k_children_median(cs, lbl, root_fr, 32);
    let rev_64 = measure_revoke_k_children_median(cs, lbl, root_fr, 64);

    cspace_destroy_object(cs, lbl, root_fr, 1).unwrap();
    destroy_cspace(cs).unwrap();

    // Per-CPU contention benchmark across `min(online_cpus, MAX_CSPACES)` CPUs:
    let bench_workers = online_cpus.min(crate::cspace::MAX_CSPACES);
    let mut cspaces = [0u16; 16];
    let mut frames = [CapPtr::NULL; 16];
    for w in 0..bench_workers {
        let w_lbl = SecLabel::new(
            SecDomain::Userspace,
            SecFlags::NONE,
            encode_compartment(3, (w as u8) + 1),
            1,
        );
        let (wcs, _) = create_cspace(w_lbl, 0x80 + (w as u8)).unwrap();
        let (wfr, _) = cspace_alloc_frame(wcs, w_lbl, w_lbl, PageOrder::Order4KiB, false, 0).unwrap();
        cspaces[w] = wcs;
        frames[w] = wfr;
        CONTEND_CSPACES[w].store(wcs as u64, Ordering::Release);
        CONTEND_CAPS[w].store(wfr.0, Ordering::Release);
    }
    // When `online_cpus > MAX_CSPACES` (`-smp 20` sharing 16 `CSpace`s), workers `16..20` wrap onto `CSpace 0..4`.
    for w in bench_workers..online_cpus {
        let base = w % bench_workers;
        CONTEND_CSPACES[w].store(cspaces[base] as u64, Ordering::Release);
        CONTEND_CAPS[w].store(frames[base].0, Ordering::Release);
    }

    CONTEND_EXPECTED.store(online_cpus, Ordering::Release);
    CONTEND_READY.store(0, Ordering::Release);
    CONTEND_P50_SUM.store(0, Ordering::Release);
    CONTEND_P99_MAX.store(0, Ordering::Release);
    CONTEND_MAX_ELAPSED.store(1, Ordering::Release);

    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xf5, online_cpus);
    for cpu in 0..online_cpus {
        let slot_idx = cpu % bench_workers;
        assert!(
            try_spawn_thread_in_vspace(
                1,
                cpu,
                KERNEL_VSPACE_ID,
                contention_bench_worker,
                slot_idx as u64,
                true
            )
            .is_some()
        );
    }
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);

    for w in 0..bench_workers {
        let w_lbl = SecLabel::new(
            SecDomain::Userspace,
            SecFlags::NONE,
            encode_compartment(3, (w as u8) + 1),
            1,
        );
        cspace_destroy_object(cspaces[w], w_lbl, frames[w], 1).unwrap();
        destroy_cspace(cspaces[w]).unwrap();
    }

    let contend_p50 = (CONTEND_P50_SUM.load(Ordering::Acquire) / (online_cpus as u64)).max(1);
    let contend_p99 = CONTEND_P99_MAX.load(Ordering::Acquire).max(contend_p50);
    let total_ops = (online_cpus as u64) * (512 + 128);
    let max_el = CONTEND_MAX_ELAPSED.load(Ordering::Acquire).max(1);
    let contend_mops_x100 = ((total_ops * tsc_mhz * 100) / max_el).max(1);
    let meta = cspace_metadata_stats();

    serial_println!(
        "[DRY-CSPACE-BENCH] PASS: cpus={} samples={} bytes_per_slot={} cnode_slots={} pool_kib={} lookup_p50_cyc={} lookup_p95_cyc={} lookup_p99_cyc={} sec_p50_cyc={} sec_p95_cyc={} sec_p99_cyc={} derive_p50_cyc={} derive_p95_cyc={} derive_p99_cyc={} mint_p50_cyc={} mint_p95_cyc={} mint_p99_cyc={} revoke1_p50_cyc={} revoke1_p95_cyc={} revoke1_p99_cyc={} revoke_1_cyc={} revoke_8_cyc={} revoke_32_cyc={} revoke_64_cyc={} contend_lookup_p50_cyc={} contend_lookup_p99_cyc={} contend_mops_x100={}",
        online_cpus,
        BENCH_SAMPLES,
        meta.bytes_per_slot,
        meta.slots_per_cnode,
        meta.total_pool_bytes / 1024,
        lk_p50,
        lk_p95,
        lk_p99,
        sec_p50,
        sec_p95,
        sec_p99,
        der_p50,
        der_p95,
        der_p99,
        mint_p50,
        mint_p95,
        mint_p99,
        rev1_p50,
        rev1_p95,
        rev1_p99,
        rev_1,
        rev_8,
        rev_32,
        rev_64,
        contend_p50,
        contend_p99,
        contend_mops_x100,
    );
}
