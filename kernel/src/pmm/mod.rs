// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Physical Memory Manager (`PMM`) — Runtime Frame Allocator, Per-CPU Magazines & Fragmentation.
//!
//! Pairs with `pmm::meta` (`FrameMeta` + `BootstrapAllocator`) and `pmm::untyped` (`ADR-003`
//! `Untyped` split/retype/revoke capability foundation).

pub mod meta;
pub mod tests;
pub mod untyped;

pub use meta::{
    bootstrap_alloc_pages, bootstrap_state, inventory, phys_to_hhdm, pmm_invariant_panic,
    query_frame_state, zero_frame_4k, AllocFlags, BootstrapState, FrameHandle, FrameState,
    PageOrder, PmmError, PmmInventory, ZeroState, HUGE_2M_FRAMES, LAPIC_MMIO_PHYS_BASE,
    LOW_MEM_GUARD_END_PFN, PAGE_SHIFT, PAGE_SIZE,
};

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::arch::{irq_restore, irq_save_cli};
use crate::gdt_tss::MAX_CPUS;
use crate::mem_verify::MemorySummary;
use crate::percpu::read_gs_u64_at_8;
use crate::pmm::meta::{
    build_inventory_and_meta, frame_meta, next_generation, pack_free_word1, pack_seclabel,
    pack_word0, unpack_free_word1, unpack_seclabel, unpack_word0, NO_CPU_OWNER, PFN_NONE,
    PMM_META,
};
use crate::security::SecLabel;

pub const MAGAZINE_CAP: usize = 64;
pub const MAGAZINE_BATCH: usize = 32;

#[derive(Clone, Copy, Debug)]
pub struct FragReport {
    pub free_global_frames: u64,
    pub free_cached_frames: u64,
    pub contig_2m_blocks: u64,
    pub external_frag_bps: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct PmmPerfCounters {
    pub mag_hits: u64,
    pub mag_misses: u64,
    pub global_lock_spins: u64,
    pub zero_runs: u64,
    pub zero_elided: u64,
}

struct GlobalPoolInner {
    head_pfn: u32,
    free_count: u64,
}

struct GlobalFreePool {
    locked: AtomicBool,
    inner: UnsafeCell<GlobalPoolInner>,
}

// SAFETY: `inner` is accessed exclusively while holding `locked` with local IRQs masked (`CLI`).
unsafe impl Sync for GlobalFreePool {}

static GLOBAL_POOL: GlobalFreePool = GlobalFreePool {
    locked: AtomicBool::new(false),
    inner: UnsafeCell::new(GlobalPoolInner {
        head_pfn: PFN_NONE,
        free_count: 0,
    }),
};

#[repr(C, align(64))]
struct PerCpuFrameCache {
    len: usize,
    pfns: [u32; MAGAZINE_CAP],
}

impl PerCpuFrameCache {
    const fn new() -> Self {
        Self {
            len: 0,
            pfns: [PFN_NONE; MAGAZINE_CAP],
        }
    }
}

struct PerCpuMagazines(UnsafeCell<[PerCpuFrameCache; MAX_CPUS]>);
// SAFETY: Slot `cpu_idx` is accessed exclusively by `cpu_idx` under `irq_save_cli()` (or by BSP
// during quiescent single-CPU barrier audits when all APs are idle).
unsafe impl Sync for PerCpuMagazines {}

static PER_CPU_MAGAZINES: PerCpuMagazines =
    PerCpuMagazines(UnsafeCell::new([const { PerCpuFrameCache::new() }; MAX_CPUS]));

static PERF_MAG_HITS: AtomicU64 = AtomicU64::new(0);
static PERF_MAG_MISSES: AtomicU64 = AtomicU64::new(0);
static PERF_GLOBAL_SPINS: AtomicU64 = AtomicU64::new(0);
static PERF_ZERO_RUNS: AtomicU64 = AtomicU64::new(0);
static PERF_ZERO_ELIDED: AtomicU64 = AtomicU64::new(0);

#[inline]
fn lock_global_pool_with_cli<R>(f: impl FnOnce(&mut GlobalPoolInner) -> R) -> R {
    let saved_if = irq_save_cli();
    let mut spins = 0u64;
    while GLOBAL_POOL
        .locked
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        spins += 1;
        core::hint::spin_loop();
    }
    if spins > 0 {
        PERF_GLOBAL_SPINS.fetch_add(spins, Ordering::Relaxed);
    }
    // SAFETY: Exclusive lock + `CLI` held on current CPU.
    let out = f(unsafe { &mut *GLOBAL_POOL.inner.get() });
    GLOBAL_POOL.locked.store(false, Ordering::Release);
    irq_restore(saved_if);
    out
}

/// Initialize `BootstrapAllocator`, `FrameMeta`, and persistent `PMM` on the BSP.
pub fn init_pmm(mem: &MemorySummary) -> PmmInventory {
    let (inv, head_pfn) = build_inventory_and_meta(mem);
    // SAFETY: Single-threaded BSP prior to publishing `PMM_META.initialized`.
    unsafe {
        let pool = &mut *GLOBAL_POOL.inner.get();
        pool.head_pfn = head_pfn;
        pool.free_count = inv.free_init_frames;
    }
    PMM_META.initialized.store(true, Ordering::Release);
    inv
}

#[inline]
fn claim_free_pfn_into_allocated(
    pfn: u32,
    expected_state: FrameState,
    expected_cpu: u8,
    label: SecLabel,
) -> (FrameHandle, ZeroState, u8, u16) {
    let meta = frame_meta(pfn).unwrap_or_else(|| {
        pmm_invariant_panic(format_args!(
            "Double allocation / corrupt free-state detected: out-of-range pfn={:#x}",
            pfn
        ))
    });
    let old_w0 = meta.word0.load(Ordering::Acquire);
    let (state, zero, _, last_dom, pins, maps, cur_gen) = unpack_word0(old_w0);
    let (_, last_comp, cache_cpu) = unpack_free_word1(meta.word1.load(Ordering::Acquire));

    if state != expected_state || cache_cpu != expected_cpu || pins != 0 || maps != 0 {
        pmm_invariant_panic(format_args!(
            "Double allocation / corrupt free-state detected: pfn={:#x} state={:?} expected={:?} cpu_tag={:#x} pins={} maps={}",
            pfn, state, expected_state, cache_cpu, pins, maps
        ));
    }

    let new_w0 = pack_word0(
        FrameState::AllocatedFrame,
        ZeroState::Dirty,
        PageOrder::Order4KiB as u8,
        label.domain as u8,
        0,
        0,
        cur_gen,
    );
    meta.word1.store(pack_seclabel(label), Ordering::Relaxed);
    if meta
        .word0
        .compare_exchange(old_w0, new_w0, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        pmm_invariant_panic(format_args!(
            "Double allocation / corrupt free-state detected: concurrent CAS race on pfn={:#x}",
            pfn
        ));
    }
    (FrameHandle { pfn, generation: cur_gen }, zero, last_dom, last_comp)
}

#[inline]
fn apply_mandatory_zero_policy(
    handle: FrameHandle,
    prev_zero: ZeroState,
    last_domain: u8,
    last_comp: u16,
    req_label: SecLabel,
    flags: AllocFlags,
) {
    let same_compartment =
        last_domain == (req_label.domain as u8) && last_comp == req_label.compartment_id;
    let must_zero = if prev_zero == ZeroState::VerifiedZero {
        false
    } else if !same_compartment {
        // ADR-007 Mandatory Isolation Gate: cross-domain or cross-compartment transitions ALWAYS zero.
        true
    } else {
        !flags.contains(AllocFlags::UNINIT_SAME_DOMAIN_OK) || flags.contains(AllocFlags::ZERO)
    };

    if must_zero {
        zero_frame_4k(handle.hhdm_ptr());
        PERF_ZERO_RUNS.fetch_add(1, Ordering::Relaxed);
    } else {
        PERF_ZERO_ELIDED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Allocate `2^order` physical frames bound to `label` under `ADR-007` isolation rules.
pub fn alloc_pages(
    order: PageOrder,
    label: SecLabel,
    flags: AllocFlags,
) -> Result<FrameHandle, PmmError> {
    if order != PageOrder::Order4KiB {
        return Err(PmmError::OrderUnsupportedYet);
    }

    if flags.contains(AllocFlags::BYPASS_PERCPU_CACHE) {
        let popped = lock_global_pool_with_cli(|pool| {
            let pfn = pool.head_pfn;
            if pfn == PFN_NONE {
                return None;
            }
            let meta = frame_meta(pfn).unwrap_or_else(|| {
                pmm_invariant_panic(format_args!(
                    "Double allocation / corrupt free-state detected: invalid head_pfn={:#x}",
                    pfn
                ))
            });
            let (next_pfn, _, _) = unpack_free_word1(meta.word1.load(Ordering::Acquire));
            pool.head_pfn = next_pfn;
            pool.free_count -= 1;
            Some(claim_free_pfn_into_allocated(
                pfn,
                FrameState::FreeGlobal,
                NO_CPU_OWNER,
                label,
            ))
        });
        let (handle, prev_zero, last_dom, last_comp) = popped.ok_or(PmmError::OutOfMemory)?;
        apply_mandatory_zero_policy(handle, prev_zero, last_dom, last_comp, label, flags);
        return Ok(handle);
    }

    let saved_if = irq_save_cli();
    let cpu_idx = read_gs_u64_at_8() as usize;
    // SAFETY: `CLI` prevents preemption or migration on `cpu_idx`.
    let mag = unsafe { &mut (*PER_CPU_MAGAZINES.0.get())[cpu_idx] };

    if mag.len == 0 {
        PERF_MAG_MISSES.fetch_add(1, Ordering::Relaxed);
        let refilled = lock_global_pool_with_cli(|pool| {
            let mut count = 0usize;
            while count < MAGAZINE_BATCH && pool.head_pfn != PFN_NONE {
                let pfn = pool.head_pfn;
                let meta = frame_meta(pfn).unwrap();
                let old_w0 = meta.word0.load(Ordering::Acquire);
                let (st, zero, _, last_dom, pins, maps, gen32) = unpack_word0(old_w0);
                let (next_pfn, last_comp, cpu_tag) =
                    unpack_free_word1(meta.word1.load(Ordering::Acquire));
                if st != FrameState::FreeGlobal || cpu_tag != NO_CPU_OWNER || pins != 0 || maps != 0 {
                    pmm_invariant_panic(format_args!(
                        "Double allocation / corrupt free-state detected: refill pfn={:#x} state={:?}",
                        pfn, st
                    ));
                }
                pool.head_pfn = next_pfn;
                pool.free_count -= 1;
                meta.word1.store(
                    pack_free_word1(PFN_NONE, last_comp, cpu_idx as u8),
                    Ordering::Relaxed,
                );
                meta.word0.store(
                    pack_word0(FrameState::FreePerCpu, zero, 0, last_dom, 0, 0, gen32),
                    Ordering::Release,
                );
                mag.pfns[count] = pfn;
                count += 1;
            }
            count
        });
        mag.len = refilled;
        if mag.len == 0 {
            irq_restore(saved_if);
            return Err(PmmError::OutOfMemory);
        }
    } else {
        PERF_MAG_HITS.fetch_add(1, Ordering::Relaxed);
    }

    mag.len -= 1;
    let pfn = mag.pfns[mag.len];
    mag.pfns[mag.len] = PFN_NONE;
    let (handle, prev_zero, last_dom, last_comp) =
        claim_free_pfn_into_allocated(pfn, FrameState::FreePerCpu, cpu_idx as u8, label);
    irq_restore(saved_if);

    // Zeroing runs outside `CLI` & outside `GLOBAL_POOL` lock for bounded IRQ latency and parallel scaling.
    apply_mandatory_zero_policy(handle, prev_zero, last_dom, last_comp, label, flags);
    Ok(handle)
}

fn release_validated_frame(
    handle: FrameHandle,
    caller_label: SecLabel,
    zero_after_free: ZeroState,
    bypass_cache: bool,
) {
    let Some(meta) = frame_meta(handle.pfn) else {
        pmm_invariant_panic(format_args!(
            "Frame ownership / domain / pin violation: non-RAM pfn={:#x}",
            handle.pfn
        ));
    };

    let saved_if = irq_save_cli();
    let cpu_idx = read_gs_u64_at_8() as usize;

    let old_w0 = meta.word0.load(Ordering::Acquire);
    let (state, _, order_code, _, pins, maps, cur_gen) = unpack_word0(old_w0);
    if state == FrameState::FreePerCpu || state == FrameState::FreeGlobal {
        pmm_invariant_panic(format_args!(
            "Double free detected: pfn={:#x} handle_gen={} cur_gen={} state={:?}",
            handle.pfn, handle.generation, cur_gen, state
        ));
    }
    if state == FrameState::ReservedOrMmio
        || state == FrameState::KernelImageOrBootMeta
        || state == FrameState::BootloaderReclaimable
    {
        pmm_invariant_panic(format_args!(
            "Frame ownership / domain / pin violation: immutable boot/MMIO pfn={:#x} state={:?}",
            handle.pfn, state
        ));
    }
    if state != FrameState::AllocatedFrame || cur_gen != handle.generation {
        pmm_invariant_panic(format_args!(
            "Use-after-free / stale generation detected: pfn={:#x} handle_gen={} cur_gen={} state={:?}",
            handle.pfn, handle.generation, cur_gen, state
        ));
    }
    let owner = unpack_seclabel(meta.word1.load(Ordering::Acquire));
    if owner.domain != caller_label.domain
        || owner.compartment_id != caller_label.compartment_id
        || pins != 0
        || maps != 0
        || order_code != (PageOrder::Order4KiB as u8)
    {
        pmm_invariant_panic(format_args!(
            "Frame ownership / domain / pin violation: pfn={:#x} owner=({:?},{}) caller=({:?},{}) pins={} maps={}",
            handle.pfn, owner.domain, owner.compartment_id, caller_label.domain, caller_label.compartment_id, pins, maps
        ));
    }

    let next_gen = next_generation(cur_gen);

    if bypass_cache {
        lock_global_pool_with_cli(|pool| {
            let new_w0 = pack_word0(
                FrameState::FreeGlobal,
                zero_after_free,
                0,
                owner.domain as u8,
                0,
                0,
                next_gen,
            );
            if meta
                .word0
                .compare_exchange(old_w0, new_w0, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                pmm_invariant_panic(format_args!(
                    "Double free detected: concurrent CAS race on pfn={:#x}",
                    handle.pfn
                ));
            }
            meta.word1.store(
                pack_free_word1(pool.head_pfn, owner.compartment_id, NO_CPU_OWNER),
                Ordering::Release,
            );
            pool.head_pfn = handle.pfn;
            pool.free_count += 1;
        });
        irq_restore(saved_if);
        return;
    }

    let new_w0 = pack_word0(
        FrameState::FreePerCpu,
        zero_after_free,
        0,
        owner.domain as u8,
        0,
        0,
        next_gen,
    );
    if meta
        .word0
        .compare_exchange(old_w0, new_w0, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        pmm_invariant_panic(format_args!(
            "Double free detected: concurrent CAS race on pfn={:#x}",
            handle.pfn
        ));
    }
    meta.word1.store(
        pack_free_word1(PFN_NONE, owner.compartment_id, cpu_idx as u8),
        Ordering::Release,
    );

    // SAFETY: `CLI` held on `cpu_idx`.
    let mag = unsafe { &mut (*PER_CPU_MAGAZINES.0.get())[cpu_idx] };
    if mag.len == MAGAZINE_CAP {
        drain_magazine_batch_locked(cpu_idx, mag, MAGAZINE_BATCH);
    }
    mag.pfns[mag.len] = handle.pfn;
    mag.len += 1;
    irq_restore(saved_if);
}

/// Free `2^order` physical frames previously allocated to `caller_label`.
#[inline]
pub fn free_pages(handle: FrameHandle, order: PageOrder, caller_label: SecLabel) {
    assert_eq!(order, PageOrder::Order4KiB, "Only Order4KiB supported in 1B-A");
    release_validated_frame(handle, caller_label, ZeroState::Dirty, false);
}

#[inline]
pub fn free_pages_global_direct(handle: FrameHandle, caller_label: SecLabel) {
    release_validated_frame(handle, caller_label, ZeroState::Dirty, true);
}

/// Zero `handle` right before releasing so the next `AllocFlags::ZERO` caller elides `rep stosq`.
pub fn scrub_and_free_frame(handle: FrameHandle, caller_label: SecLabel) {
    let ptr = resolve_frame_ptr(handle, caller_label);
    zero_frame_4k(ptr);
    release_validated_frame(handle, caller_label, ZeroState::VerifiedZero, false);
}

fn drain_magazine_batch_locked(cpu_idx: usize, mag: &mut PerCpuFrameCache, count: usize) {
    let to_drain = count.min(mag.len);
    if to_drain == 0 {
        return;
    }
    lock_global_pool_with_cli(|pool| {
        for _ in 0..to_drain {
            mag.len -= 1;
            let pfn = mag.pfns[mag.len];
            mag.pfns[mag.len] = PFN_NONE;
            let meta = frame_meta(pfn).unwrap();
            let (st, zero, _, last_dom, pins, maps, gen32) =
                unpack_word0(meta.word0.load(Ordering::Acquire));
            let (_, last_comp, cpu_tag) = unpack_free_word1(meta.word1.load(Ordering::Acquire));
            assert!(
                st == FrameState::FreePerCpu && cpu_tag == (cpu_idx as u8) && pins == 0 && maps == 0
            );
            meta.word1.store(
                pack_free_word1(pool.head_pfn, last_comp, NO_CPU_OWNER),
                Ordering::Relaxed,
            );
            meta.word0.store(
                pack_word0(FrameState::FreeGlobal, zero, 0, last_dom, 0, 0, gen32),
                Ordering::Release,
            );
            pool.head_pfn = pfn;
            pool.free_count += 1;
        }
    });
}

/// Flush all cached `FreePerCpu` frames across `0..online_cpus` back into `GlobalFreePool`.
pub fn flush_all_magazines_quiescent(online_cpus: usize) {
    let saved_if = irq_save_cli();
    for cpu_idx in 0..online_cpus {
        // SAFETY: Quiescent single-owner phase boundary (`PHASE_PENDING_THREADS == 0`).
        let mag = unsafe { &mut (*PER_CPU_MAGAZINES.0.get())[cpu_idx] };
        let all = mag.len;
        drain_magazine_batch_locked(cpu_idx, mag, all);
    }
    irq_restore(saved_if);
}

/// Validate `handle` (`AllocatedFrame`, live `generation`, matching `(domain, compartment_id)`)
/// and return its HHDM virtual pointer. Deterministically traps UAF and ownership bugs.
pub fn resolve_frame_ptr(handle: FrameHandle, caller_label: SecLabel) -> *mut u8 {
    let Some(meta) = frame_meta(handle.pfn) else {
        pmm_invariant_panic(format_args!(
            "Frame ownership / domain / pin violation: non-RAM pfn={:#x}",
            handle.pfn
        ));
    };
    let (state, _, _, _, _, _, cur_gen) = unpack_word0(meta.word0.load(Ordering::Acquire));
    if state != FrameState::AllocatedFrame || cur_gen != handle.generation {
        pmm_invariant_panic(format_args!(
            "Use-after-free / stale generation detected: pfn={:#x} handle_gen={} cur_gen={} state={:?}",
            handle.pfn, handle.generation, cur_gen, state
        ));
    }
    let owner = unpack_seclabel(meta.word1.load(Ordering::Acquire));
    if owner.domain != caller_label.domain || owner.compartment_id != caller_label.compartment_id {
        pmm_invariant_panic(format_args!(
            "Frame ownership / domain / pin violation: access pfn={:#x} owner=({:?},{}) caller=({:?},{})",
            handle.pfn, owner.domain, owner.compartment_id, caller_label.domain, caller_label.compartment_id
        ));
    }
    handle.hhdm_ptr()
}

pub fn adjust_frame_pins_and_maps(pfn: u32, expected_gen: u32, pin_delta: i32, map_delta: i32) {
    let meta = frame_meta(pfn).expect("Invalid PFN in adjust_frame_pins_and_maps");
    loop {
        let old_w0 = meta.word0.load(Ordering::Acquire);
        let (st, zero, ord, last_dom, pins, maps, cur_gen) = unpack_word0(old_w0);
        if (st != FrameState::AllocatedFrame && st != FrameState::UntypedRetypedObj)
            || cur_gen != expected_gen
        {
            pmm_invariant_panic(format_args!(
                "Use-after-free / stale generation detected: pin/map pfn={:#x} expected_gen={} cur_gen={} state={:?}",
                pfn, expected_gen, cur_gen, st
            ));
        }
        let new_pins = (pins as i32 + pin_delta) as u16;
        let new_maps = (maps as i32 + map_delta) as u16;
        let new_w0 = pack_word0(st, zero, ord, last_dom, new_pins, new_maps, cur_gen);
        if meta
            .word0
            .compare_exchange_weak(old_w0, new_w0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return;
        }
    }
}

pub(crate) fn carve_contiguous_for_untyped(
    frame_count: usize,
    align_frames: usize,
    label: SecLabel,
) -> Result<u32, PmmError> {
    assert!(frame_count > 0 && align_frames.is_power_of_two());
    let max_pfn = PMM_META.max_tracked_pfn.load(Ordering::Acquire) as usize;
    let align_mask = align_frames - 1;

    lock_global_pool_with_cli(|pool| {
        if (pool.free_count as usize) < frame_count {
            return Err(PmmError::OutOfMemory);
        }
        let mut found_base: Option<u32> = None;
        let mut pfn = (LOW_MEM_GUARD_END_PFN as usize + align_mask) & !align_mask;
        while pfn + frame_count <= max_pfn {
            let mut run_ok = true;
            for off in 0..frame_count {
                let (st, _, _, _, pins, maps, _) =
                    unpack_word0(frame_meta((pfn + off) as u32).unwrap().word0.load(Ordering::Relaxed));
                if st != FrameState::FreeGlobal || pins != 0 || maps != 0 {
                    run_ok = false;
                    break;
                }
            }
            if run_ok {
                found_base = Some(pfn as u32);
                break;
            }
            pfn += align_frames;
        }

        let base = found_base.ok_or(PmmError::OutOfMemory)?;
        let end = base + (frame_count as u32);

        let mut new_head = PFN_NONE;
        let mut prev_kept = PFN_NONE;
        let mut cur = pool.head_pfn;
        let mut unlinked = 0usize;
        while cur != PFN_NONE {
            let m = frame_meta(cur).unwrap();
            let (next_pfn, _, _) = unpack_free_word1(m.word1.load(Ordering::Relaxed));
            if cur >= base && cur < end {
                let (_, _, _, _, _, _, gen32) = unpack_word0(m.word0.load(Ordering::Relaxed));
                m.word1.store(pack_seclabel(label), Ordering::Relaxed);
                m.word0.store(
                    pack_word0(FrameState::UntypedFree, ZeroState::Dirty, 0, label.domain as u8, 0, 0, gen32),
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
        assert_eq!(unlinked, frame_count, "Untyped carve unlink count mismatch");
        pool.head_pfn = new_head;
        pool.free_count -= frame_count as u64;
        Ok(base)
    })
}

pub(crate) fn release_contiguous_from_untyped(base_pfn: u32, frame_count: usize, label: SecLabel) {
    lock_global_pool_with_cli(|pool| {
        for off in (0..frame_count).rev() {
            let pfn = base_pfn + (off as u32);
            let m = frame_meta(pfn).unwrap();
            let (st, _, _, _, pins, maps, cur_gen) = unpack_word0(m.word0.load(Ordering::Acquire));
            assert!(st == FrameState::UntypedFree && pins == 0 && maps == 0);
            zero_frame_4k(phys_to_hhdm((pfn as u64) << PAGE_SHIFT));
            let next_gen = next_generation(cur_gen);
            m.word1.store(
                pack_free_word1(pool.head_pfn, label.compartment_id, NO_CPU_OWNER),
                Ordering::Relaxed,
            );
            m.word0.store(
                pack_word0(
                    FrameState::FreeGlobal,
                    ZeroState::VerifiedZero,
                    0,
                    label.domain as u8,
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

pub fn measure_fragmentation(online_cpus: usize) -> FragReport {
    let max_pfn = PMM_META.max_tracked_pfn.load(Ordering::Acquire) as usize;
    let free_global = lock_global_pool_with_cli(|pool| pool.free_count);

    let mut free_cached = 0u64;
    for cpu in 0..online_cpus {
        // SAFETY: Diagnostic read of per-CPU cache occupancy.
        free_cached += unsafe { (*PER_CPU_MAGAZINES.0.get())[cpu].len as u64 };
    }

    let mut contig_2m_blocks = 0u64;
    let mut base = HUGE_2M_FRAMES;
    while base + HUGE_2M_FRAMES <= max_pfn {
        let mut all_free = true;
        for off in 0..HUGE_2M_FRAMES {
            let (st, _, _, _, _, _, _) =
                unpack_word0(frame_meta((base + off) as u32).unwrap().word0.load(Ordering::Relaxed));
            if st != FrameState::FreeGlobal {
                all_free = false;
                break;
            }
        }
        if all_free {
            contig_2m_blocks += 1;
        }
        base += HUGE_2M_FRAMES;
    }

    let free_in_2m = contig_2m_blocks * (HUGE_2M_FRAMES as u64);
    let external_frag_bps = if free_global == 0 || free_in_2m >= free_global {
        0
    } else {
        ((free_global - free_in_2m) * 10_000) / free_global
    };

    FragReport {
        free_global_frames: free_global,
        free_cached_frames: free_cached,
        contig_2m_blocks,
        external_frag_bps,
    }
}

pub fn reset_perf_counters() {
    PERF_MAG_HITS.store(0, Ordering::Release);
    PERF_MAG_MISSES.store(0, Ordering::Release);
    PERF_GLOBAL_SPINS.store(0, Ordering::Release);
    PERF_ZERO_RUNS.store(0, Ordering::Release);
    PERF_ZERO_ELIDED.store(0, Ordering::Release);
}

pub fn perf_counters() -> PmmPerfCounters {
    PmmPerfCounters {
        mag_hits: PERF_MAG_HITS.load(Ordering::Acquire),
        mag_misses: PERF_MAG_MISSES.load(Ordering::Acquire),
        global_lock_spins: PERF_GLOBAL_SPINS.load(Ordering::Acquire),
        zero_runs: PERF_ZERO_RUNS.load(Ordering::Acquire),
        zero_elided: PERF_ZERO_ELIDED.load(Ordering::Acquire),
    }
}

pub(crate) fn inject_double_alloc_corrupt_slot_for_test(label: SecLabel) {
    let saved_if = irq_save_cli();
    let cpu_idx = read_gs_u64_at_8() as usize;
    // SAFETY: `CLI` held on `cpu_idx`.
    let mag = unsafe { &mut (*PER_CPU_MAGAZINES.0.get())[cpu_idx] };
    assert!(mag.len > 0, "Expected non-empty local magazine for double_alloc fault test");
    let target_pfn = mag.pfns[mag.len - 1];
    let m = frame_meta(target_pfn).unwrap();
    let (_, _, _, _, _, _, cur_gen) = unpack_word0(m.word0.load(Ordering::Acquire));
    m.word1.store(pack_seclabel(label), Ordering::Relaxed);
    m.word0.store(
        pack_word0(
            FrameState::AllocatedFrame,
            ZeroState::Dirty,
            0,
            label.domain as u8,
            0,
            0,
            cur_gen,
        ),
        Ordering::Release,
    );
    irq_restore(saved_if);
}
