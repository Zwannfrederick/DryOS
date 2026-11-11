// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Deadlock-Free SMP TLB Invalidation & Shootdown Engine (`Milestone 1B-B`).
//!
//! # Deadlock Elimination, Paging-Cache Linearizability & Reclamation Protocol
//! 1. **IRQ-Context Ban:** `shootdown_tlb` MUST NEVER be called inside an interrupt handler
//!    (`irq_depth > 0`); violated calls panic immediately (`[DRY-TLB-FAIL]`).
//! 2. **Strict Lock DAG (`VSpace::lock` [at most 1] -> `TLB_REQ_LOCK` [leaf]) & Linearizability:**
//!    `unmap_*` and `protect_*` complete `shootdown_tlb` *before* releasing `VSpace::lock` so a
//!    concurrent `map_*` on another CPU can never allocate a new `PT/PD/PDPT` and fault against an
//!    un-flushed Intel SDM §4.10.2.3 `PDE/PDPTE/PML4E` paging-structure cache. When any intermediate
//!    directory is unlinked (`pt_len > 0`), `FULL_FLUSH_SENTINEL` reloads `CR3` on `active_cpus`.
//!    Unmapped data frames (`map_count >= 1`) and unlinked PT frames (`pin_count == 1`) remain pinned
//!    in `PendingReclaim` until *after* both `shootdown_tlb` and `VSpace::lock` complete.
//! 3. **Mutual `RFLAGS.IF == 0` Spin-Wait Immunity:**
//!    Any CPU spinning on `VSpace::lock` or `TLB_REQ_LOCK` with local interrupts masked (`CLI`)
//!    actively executes `poll_and_ack_pending_shootdown(caller_cpu)`, and `on_tlb_shootdown_ipi` is
//!    100% lock-free, eliminating all cross-CPU `CLI` wait cycles.
//! 4. **Bounded Hardware `TSC` Timeout:**
//!    If a remote target CPU in `active_cpus` stays wedged with `IF == 0` past `SHOOTDOWN_TIMEOUT_TSC`
//!    (`~350 ms` default), `shootdown_tlb` aborts deterministically with `[DRY-TLB-FAIL] Shootdown timeout`
//!    (`exit=35`) *before* `PendingReclaim` can return any frame to `PMM`.

use core::sync::atomic::{fence, AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering};

use crate::apic::{lapic_eoi, send_fixed_ipi, VEC_TLB_SHOOTDOWN};
use crate::arch::{invlpg, irq_restore, irq_save_cli, rdtsc, read_cr3, read_rflags, write_cr3};
use crate::gdt_tss::MAX_CPUS;
use crate::percpu::{current_percpu, get_percpu_by_idx, read_gs_u64_at_8};
use crate::sched::preempt::{preempt_disable, preempt_enable};
use crate::vspace::pte::{PTE_PHYS_MASK_4K, PAGE_SIZE_4K};
use crate::vspace::VSpaceId;

pub const FULL_FLUSH_SENTINEL: u64 = u64::MAX;
const MAX_SINGLE_INVLPG_PAGES: u64 = 16;

static SHOOTDOWN_TIMEOUT_TSC: AtomicU64 = AtomicU64::new(900_000_000);

const NO_TLB_LOCK_OWNER: usize = usize::MAX;
static TLB_REQ_LOCK: AtomicBool = AtomicBool::new(false);
static TLB_LOCK_OWNER_CPU: AtomicUsize = AtomicUsize::new(NO_TLB_LOCK_OWNER);
static NEXT_REQ_GEN: AtomicU64 = AtomicU64::new(0);
static REQ_GEN: AtomicU64 = AtomicU64::new(0);
static REQ_VSPACE_ID: AtomicU16 = AtomicU16::new(0);
static REQ_BASE_VADDR: AtomicU64 = AtomicU64::new(0);
static REQ_PAGE_COUNT: AtomicU64 = AtomicU64::new(0);
static REQ_TARGET_MASK: AtomicU64 = AtomicU64::new(0);
static REQ_ACK_MASK: AtomicU64 = AtomicU64::new(0);

#[allow(clippy::declare_interior_mutable_const)]
const ZERO_U64: AtomicU64 = AtomicU64::new(0);
static CPU_ACKED_GEN: [AtomicU64; MAX_CPUS] = [ZERO_U64; MAX_CPUS];

static TOTAL_TLB_SHOOTDOWNS: AtomicU64 = AtomicU64::new(0);
static TOTAL_TLB_IPI_SENT: AtomicU64 = AtomicU64::new(0);
static TOTAL_TLB_IPI_HANDLED: AtomicU64 = AtomicU64::new(0);
static TOTAL_TLB_COOP_ACKS: AtomicU64 = AtomicU64::new(0);

/// Fault-injection hook used strictly by negative test `vspace_stale_tlb`.
static SKIP_IPI_FOR_STALE_TLB_TEST: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug)]
pub struct TlbTelemetry {
    pub shootdowns: u64,
    pub ipis_sent: u64,
    pub ipis_handled: u64,
    pub coop_acks: u64,
}

pub fn tlb_invariant_panic(args: core::fmt::Arguments<'_>) -> ! {
    crate::enter_fatal_or_halt();
    crate::serial::emergency_print(format_args!("\r\n[DRY-TLB-FAIL] {}\r\n", args));
    panic!("TLB coherence invariant violation");
}

pub fn init_tlb_shootdown_timeout(tsc_mhz: u64) {
    // 350 ms hardware TSC deadline (min 200M cycles so synthetic low-MHz timers never false-trip).
    let cycles = (tsc_mhz.max(500)) * 350_000;
    SHOOTDOWN_TIMEOUT_TSC.store(cycles, Ordering::Release);
}

pub(crate) fn set_shootdown_timeout_cycles_for_test(cycles: u64) {
    SHOOTDOWN_TIMEOUT_TSC.store(cycles.max(1_000_000), Ordering::Release);
}

pub(crate) fn set_skip_ipi_for_stale_tlb_test(enable: bool) {
    SKIP_IPI_FOR_STALE_TLB_TEST.store(enable, Ordering::SeqCst);
}

pub fn reset_tlb_telemetry() {
    TOTAL_TLB_SHOOTDOWNS.store(0, Ordering::Release);
    TOTAL_TLB_IPI_SENT.store(0, Ordering::Release);
    TOTAL_TLB_IPI_HANDLED.store(0, Ordering::Release);
    TOTAL_TLB_COOP_ACKS.store(0, Ordering::Release);
}

pub fn tlb_telemetry() -> TlbTelemetry {
    TlbTelemetry {
        shootdowns: TOTAL_TLB_SHOOTDOWNS.load(Ordering::Acquire),
        ipis_sent: TOTAL_TLB_IPI_SENT.load(Ordering::Acquire),
        ipis_handled: TOTAL_TLB_IPI_HANDLED.load(Ordering::Acquire),
        coop_acks: TOTAL_TLB_COOP_ACKS.load(Ordering::Acquire),
    }
}

#[inline]
fn flush_range_local(base_vaddr: u64, page_count: u64) {
    if page_count == FULL_FLUSH_SENTINEL || page_count > MAX_SINGLE_INVLPG_PAGES {
        // Reloading `CR3` (with `CR4.PCIDE == 0`) flushes all non-global user TLB + paging-cache entries.
        let cur = read_cr3() & PTE_PHYS_MASK_4K;
        // SAFETY: Reloading the currently active valid PML4 physical address is side-effect-free except TLB flush.
        unsafe { write_cr3(cur) };
    } else {
        for i in 0..page_count {
            invlpg(base_vaddr + i * PAGE_SIZE_4K);
        }
    }
}

fn service_pending_shootdown_on_cpu(cpu_idx: usize, from_ipi: bool) {
    let gen = REQ_GEN.load(Ordering::Acquire);
    if gen == 0 {
        return;
    }
    let bit = 1u64 << cpu_idx;
    if (REQ_TARGET_MASK.load(Ordering::Acquire) & bit) == 0 {
        return;
    }
    let prev = CPU_ACKED_GEN[cpu_idx].load(Ordering::Acquire);
    if prev >= gen {
        return;
    }
    if CPU_ACKED_GEN[cpu_idx]
        .compare_exchange(prev, gen, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    let req_vs = REQ_VSPACE_ID.load(Ordering::Relaxed);
    let base_vaddr = REQ_BASE_VADDR.load(Ordering::Relaxed);
    let count = REQ_PAGE_COUNT.load(Ordering::Relaxed);

    if crate::vspace::cpu_active_vspace(cpu_idx) == req_vs || req_vs == crate::vspace::KERNEL_VSPACE_ID {
        flush_range_local(base_vaddr, count);
    }

    // `SeqCst` orders all prior `invlpg`/`mov cr3` instructions before the initiator sees `REQ_ACK_MASK`
    // and returns unmapped data/page-table frames to `PMM`.
    REQ_ACK_MASK.fetch_or(bit, Ordering::SeqCst);
    if from_ipi {
        TOTAL_TLB_IPI_HANDLED.fetch_add(1, Ordering::Relaxed);
    } else {
        TOTAL_TLB_COOP_ACKS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Hardware IDT vector `0xEE` (`VEC_TLB_SHOOTDOWN`) handler (`Interrupt Gate`, `RFLAGS.IF == 0`).
/// Lock-free, allocation-free, and safe across any interrupted Ring 0 instruction boundary.
pub fn on_tlb_shootdown_ipi() {
    let cpu_idx = read_gs_u64_at_8() as usize;
    service_pending_shootdown_on_cpu(cpu_idx, true);
    lapic_eoi();
}

/// Cooperative poll invoked inside `CLI` spinloops so `IF == 0` lock waiters never block shootdown ACKs.
#[inline]
pub fn poll_and_ack_pending_shootdown(cpu_idx: usize) {
    service_pending_shootdown_on_cpu(cpu_idx, false);
}

/// Called inside `switch_cpu_vspace_if_needed` immediately AFTER `write_cr3(new_cr3)` flushed
/// `old_vspace`'s non-global TLB entries on `cpu_idx`, satisfying any in-flight shootdown for `old_vspace`.
#[inline]
pub(crate) fn ack_if_leaving_vspace(cpu_idx: usize, old_vspace: VSpaceId) {
    let gen = REQ_GEN.load(Ordering::Acquire);
    if gen == 0 || REQ_VSPACE_ID.load(Ordering::Relaxed) != old_vspace {
        return;
    }
    let bit = 1u64 << cpu_idx;
    if (REQ_TARGET_MASK.load(Ordering::Acquire) & bit) != 0 {
        let prev = CPU_ACKED_GEN[cpu_idx].load(Ordering::Relaxed);
        if prev < gen
            && CPU_ACKED_GEN[cpu_idx]
                .compare_exchange(prev, gen, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
        {
            REQ_ACK_MASK.fetch_or(bit, Ordering::SeqCst);
            TOTAL_TLB_COOP_ACKS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[inline]
pub(crate) fn assert_tlb_req_lock_not_held_by_cpu(cpu_idx: usize) {
    if TLB_LOCK_OWNER_CPU.load(Ordering::Relaxed) == cpu_idx {
        tlb_invariant_panic(format_args!(
            "Lock-order violation: VSpace::lock requested while holding leaf TLB_REQ_LOCK on cpu={}",
            cpu_idx
        ));
    }
}

/// Invalidate `[base_vaddr .. base_vaddr + page_count * 4096)` locally and across all remote CPUs
/// currently active in `vspace_id`. Returns the number of remote IPIs dispatched.
pub fn shootdown_tlb(
    vspace_id: VSpaceId,
    active_cpus_ref: &AtomicU64,
    base_vaddr: u64,
    page_count: u64,
) -> u64 {
    let pcpu = current_percpu();
    if pcpu.irq_depth.load(Ordering::Relaxed) != 0 {
        tlb_invariant_panic(format_args!(
            "Illegal shootdown_tlb inside IRQ context on cpu={} vspace={}",
            pcpu.cpu_idx, vspace_id
        ));
    }

    let if_was_enabled = (read_rflags() & (1 << 9)) != 0;
    if if_was_enabled {
        preempt_disable();
    }
    let caller_cpu = read_gs_u64_at_8() as usize;

    if crate::vspace::cpu_active_vspace(caller_cpu) == vspace_id
        || vspace_id == crate::vspace::KERNEL_VSPACE_ID
    {
        flush_range_local(base_vaddr, page_count);
    }

    // Order prior PTE modification (`SeqCst`) before reading `active_cpus_ref` (`SeqCst`).
    fence(Ordering::SeqCst);
    let initial_targets = active_cpus_ref.load(Ordering::SeqCst) & !(1u64 << caller_cpu);
    if initial_targets == 0 || SKIP_IPI_FOR_STALE_TLB_TEST.load(Ordering::Acquire) {
        if if_was_enabled {
            preempt_enable();
        }
        return 0;
    }

    let timeout_tsc = SHOOTDOWN_TIMEOUT_TSC.load(Ordering::Relaxed);
    let t_lock0 = rdtsc();
    let saved_if = irq_save_cli();

    while TLB_REQ_LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        poll_and_ack_pending_shootdown(caller_cpu);
        if rdtsc().wrapping_sub(t_lock0) > timeout_tsc {
            irq_restore(saved_if);
            tlb_invariant_panic(format_args!(
                "Shootdown timeout acquiring TLB_REQ_LOCK on cpu={} vspace={}",
                caller_cpu, vspace_id
            ));
        }
        core::hint::spin_loop();
    }
    TLB_LOCK_OWNER_CPU.store(caller_cpu, Ordering::Relaxed);

    let targets = active_cpus_ref.load(Ordering::SeqCst) & !(1u64 << caller_cpu);
    if targets == 0 {
        TLB_LOCK_OWNER_CPU.store(NO_TLB_LOCK_OWNER, Ordering::Relaxed);
        TLB_REQ_LOCK.store(false, Ordering::Release);
        irq_restore(saved_if);
        if if_was_enabled {
            preempt_enable();
        }
        return 0;
    }

    REQ_ACK_MASK.store(0, Ordering::Release);
    REQ_VSPACE_ID.store(vspace_id, Ordering::Relaxed);
    REQ_BASE_VADDR.store(base_vaddr, Ordering::Relaxed);
    REQ_PAGE_COUNT.store(page_count, Ordering::Relaxed);
    REQ_TARGET_MASK.store(targets, Ordering::Release);
    let gen = NEXT_REQ_GEN.fetch_add(1, Ordering::AcqRel) + 1;
    REQ_GEN.store(gen, Ordering::SeqCst);

    let mut ipis_sent = 0u64;
    let mut rem = targets;
    while rem != 0 {
        let dst_cpu = rem.trailing_zeros() as usize;
        rem &= rem - 1;
        send_fixed_ipi(get_percpu_by_idx(dst_cpu).lapic_id, VEC_TLB_SHOOTDOWN);
        ipis_sent += 1;
    }
    TOTAL_TLB_SHOOTDOWNS.fetch_add(1, Ordering::Relaxed);
    TOTAL_TLB_IPI_SENT.fetch_add(ipis_sent, Ordering::Relaxed);

    let t_wait0 = rdtsc();
    loop {
        let acked = REQ_ACK_MASK.load(Ordering::SeqCst);
        let switched_away = !active_cpus_ref.load(Ordering::SeqCst);
        if ((acked | switched_away) & targets) == targets {
            break;
        }
        if rdtsc().wrapping_sub(t_wait0) > timeout_tsc {
            REQ_GEN.store(0, Ordering::Release);
            TLB_LOCK_OWNER_CPU.store(NO_TLB_LOCK_OWNER, Ordering::Relaxed);
            TLB_REQ_LOCK.store(false, Ordering::Release);
            irq_restore(saved_if);
            tlb_invariant_panic(format_args!(
                "Shootdown timeout: vspace={} vaddr={:#x} targets={:#x} acked={:#x}",
                vspace_id, base_vaddr, targets, acked
            ));
        }
        core::hint::spin_loop();
    }

    REQ_GEN.store(0, Ordering::Release);
    TLB_LOCK_OWNER_CPU.store(NO_TLB_LOCK_OWNER, Ordering::Relaxed);
    TLB_REQ_LOCK.store(false, Ordering::Release);
    irq_restore(saved_if);
    if if_was_enabled {
        preempt_enable();
    }
    ipis_sent
}
