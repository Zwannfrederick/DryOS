// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Deadlock-Free SMP TLB Invalidation, Monotonic Seqlock Request Protocol & Cycle Breakdown
//! (`Milestone 1B-B` & `Milestone 1B-C` Stage F TLB Debt Fix).
//!
//! # Deadlock Elimination, Seqlock Torn-Read Immunity & Reclamation Protocol
//! 1. **IRQ-Context Ban:** `shootdown_tlb` MUST NEVER be called inside an interrupt handler
//!    (`irq_depth > 0`); violated calls panic immediately (`[DRY-TLB-FAIL]`).
//! 2. **Strict Lock DAG (`VSpace::lock` [at most 1] -> `TLB_REQ_LOCK` [leaf]) & Linearizability:**
//!    `unmap_*` and `protect_*` complete `shootdown_tlb` *before* releasing `VSpace::lock` so a
//!    concurrent `map_*` on another CPU can never fault against an un-flushed Intel SDM §4.10.2.3
//!    `PDE/PDPTE/PML4E` paging-structure cache. Unmapped data frames (`map_count >= 1`) and
//!    unlinked PT frames (`pin_count == 1`) stay pinned until *after* shootdown ACKs complete.
//! 3. **Monotonic `REQ_GEN` Seqlock & Generation-Exact `CPU_ACKED_GEN` (`Milestone 1B-C` Fix):**
//!    Eliminates the `1B-B` torn-read race where a vCPU preempted by KVM between reading `REQ_GEN`
//!    (`G`) and `REQ_TARGET_MASK` (`G + 1`) could match `G + 1`'s target mask against stale `G`
//!    (causing the `155 + 1301 = 1456` vs `1454` `+2` telemetry discrepancy). Readers re-verify
//!    `REQ_GEN == gen` before acting, publish `CPU_ACKED_GEN[cpu] = gen` strictly *after* flushing,
//!    and the initiator tallies each target bit once (`ipis_handled + coop_acks == ipis_sent`).

use core::sync::atomic::{fence, AtomicBool, AtomicU16, AtomicU64, AtomicU8, AtomicUsize, Ordering};

use crate::apic::{lapic_eoi, send_fixed_ipi, VEC_TLB_SHOOTDOWN};
use crate::arch::{invlpg, irq_restore, irq_save_cli, rdtsc, read_cr3, read_rflags, write_cr3};
use crate::gdt_tss::MAX_CPUS;
use crate::percpu::{current_percpu, get_percpu_by_idx, read_gs_u64_at_8};
use crate::sched::preempt::{preempt_disable, preempt_enable};
use crate::vspace::pte::{PAGE_SIZE_4K, PTE_PHYS_MASK_4K};
use crate::vspace::VSpaceId;

pub const FULL_FLUSH_SENTINEL: u64 = u64::MAX;
const MAX_SINGLE_INVLPG_PAGES: u64 = 16;

const ACK_NONE: u8 = 0;
const ACK_VIA_IPI: u8 = 1;
const ACK_VIA_SPIN_POLL: u8 = 2;
const ACK_VIA_CR3_LEAVE: u8 = 3;

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
static REQ_START_TSC: AtomicU64 = AtomicU64::new(0);

#[repr(C, align(64))]
struct CpuTlbSlot {
    claimed_gen: AtomicU64,
    acked_gen: AtomicU64,
    remote_sched_cyc: AtomicU64,
    remote_flush_cyc: AtomicU64,
    raw_ipi_entries: AtomicU64,
    ack_kind: AtomicU8,
    _pad: [u8; 23],
}

impl CpuTlbSlot {
    const fn new() -> Self {
        Self {
            claimed_gen: AtomicU64::new(0),
            acked_gen: AtomicU64::new(0),
            remote_sched_cyc: AtomicU64::new(0),
            remote_flush_cyc: AtomicU64::new(0),
            raw_ipi_entries: AtomicU64::new(0),
            ack_kind: AtomicU8::new(ACK_NONE),
            _pad: [0; 23],
        }
    }
}

static CPU_TLB: [CpuTlbSlot; MAX_CPUS] = [const { CpuTlbSlot::new() }; MAX_CPUS];

static TOTAL_TLB_SHOOTDOWNS: AtomicU64 = AtomicU64::new(0);
static TOTAL_TLB_IPI_SENT: AtomicU64 = AtomicU64::new(0);
static TOTAL_TLB_IPI_HANDLED: AtomicU64 = AtomicU64::new(0);
static TOTAL_TLB_COOP_ACKS: AtomicU64 = AtomicU64::new(0);
static TOTAL_TLB_POLL_ACKS: AtomicU64 = AtomicU64::new(0);
static TOTAL_TLB_LEAVE_ACKS: AtomicU64 = AtomicU64::new(0);

static LAST_IPI_SEND_CYC: AtomicU64 = AtomicU64::new(0);
static LAST_REMOTE_SCHED_CYC: AtomicU64 = AtomicU64::new(0);
static LAST_FLUSH_EXEC_CYC: AtomicU64 = AtomicU64::new(0);
static LAST_ACK_WAIT_CYC: AtomicU64 = AtomicU64::new(0);

/// Fault-injection hook used strictly by negative test `vspace_stale_tlb`.
static SKIP_IPI_FOR_STALE_TLB_TEST: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug)]
pub struct TlbTelemetry {
    pub shootdowns: u64,
    pub ipis_sent: u64,
    pub ipis_handled: u64,
    pub coop_acks: u64,
    pub poll_acks: u64,
    pub leave_acks: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct ShootdownBreakdown {
    pub ipi_send_cyc: u64,
    pub remote_sched_cyc: u64,
    pub flush_exec_cyc: u64,
    pub ack_wait_cyc: u64,
}

pub fn tlb_invariant_panic(args: core::fmt::Arguments<'_>) -> ! {
    crate::enter_fatal_or_halt();
    crate::serial::emergency_print(format_args!("\r\n[DRY-TLB-FAIL] {}\r\n", args));
    panic!("TLB coherence invariant violation");
}

pub fn init_tlb_shootdown_timeout(tsc_mhz: u64) {
    let cycles = (tsc_mhz.max(500)) * 1_500_000;
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
    TOTAL_TLB_POLL_ACKS.store(0, Ordering::Release);
    TOTAL_TLB_LEAVE_ACKS.store(0, Ordering::Release);
}

pub fn tlb_telemetry() -> TlbTelemetry {
    TlbTelemetry {
        shootdowns: TOTAL_TLB_SHOOTDOWNS.load(Ordering::Acquire),
        ipis_sent: TOTAL_TLB_IPI_SENT.load(Ordering::Acquire),
        ipis_handled: TOTAL_TLB_IPI_HANDLED.load(Ordering::Acquire),
        coop_acks: TOTAL_TLB_COOP_ACKS.load(Ordering::Acquire),
        poll_acks: TOTAL_TLB_POLL_ACKS.load(Ordering::Acquire),
        leave_acks: TOTAL_TLB_LEAVE_ACKS.load(Ordering::Acquire),
    }
}

pub fn last_shootdown_breakdown() -> ShootdownBreakdown {
    ShootdownBreakdown {
        ipi_send_cyc: LAST_IPI_SEND_CYC.load(Ordering::Acquire),
        remote_sched_cyc: LAST_REMOTE_SCHED_CYC.load(Ordering::Acquire),
        flush_exec_cyc: LAST_FLUSH_EXEC_CYC.load(Ordering::Acquire),
        ack_wait_cyc: LAST_ACK_WAIT_CYC.load(Ordering::Acquire),
    }
}

pub fn cpu_raw_tlb_ipi_entries(cpu_idx: usize) -> u64 {
    CPU_TLB[cpu_idx].raw_ipi_entries.load(Ordering::Acquire)
}

#[inline]
pub(crate) fn flush_range_local(base_vaddr: u64, page_count: u64) -> u64 {
    let t0 = rdtsc();
    if page_count == FULL_FLUSH_SENTINEL || page_count > MAX_SINGLE_INVLPG_PAGES {
        let cur = read_cr3() & PTE_PHYS_MASK_4K;
        // SAFETY: Reloading the currently active valid PML4 physical address flushes non-global TLBs.
        unsafe { write_cr3(cur) };
    } else {
        for i in 0..page_count {
            invlpg(base_vaddr + i * PAGE_SIZE_4K);
        }
    }
    rdtsc().wrapping_sub(t0).max(1)
}

fn service_pending_shootdown_on_cpu(cpu_idx: usize, from_ipi: bool) {
    loop {
        let gen = REQ_GEN.load(Ordering::Acquire);
        if gen == 0 {
            return;
        }
        let targets = REQ_TARGET_MASK.load(Ordering::Acquire);
        let req_vs = REQ_VSPACE_ID.load(Ordering::Acquire);
        let base_vaddr = REQ_BASE_VADDR.load(Ordering::Acquire);
        let count = REQ_PAGE_COUNT.load(Ordering::Acquire);
        let start_tsc = REQ_START_TSC.load(Ordering::Acquire);
        if REQ_GEN.load(Ordering::Acquire) != gen {
            core::hint::spin_loop();
            continue;
        }

        if (targets & (1u64 << cpu_idx)) == 0 {
            return;
        }
        let slot = &CPU_TLB[cpu_idx];
        let prev = slot.claimed_gen.load(Ordering::Acquire);
        if prev >= gen
            || slot
                .claimed_gen
                .compare_exchange(prev, gen, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }

        let t_entry = rdtsc();
        let flush_cyc = if crate::vspace::cpu_active_vspace(cpu_idx) == req_vs
            || req_vs == crate::vspace::KERNEL_VSPACE_ID
        {
            flush_range_local(base_vaddr, count)
        } else {
            1
        };

        slot.remote_sched_cyc
            .store(t_entry.wrapping_sub(start_tsc).max(1), Ordering::Relaxed);
        slot.remote_flush_cyc.store(flush_cyc, Ordering::Relaxed);
        slot.ack_kind.store(
            if from_ipi { ACK_VIA_IPI } else { ACK_VIA_SPIN_POLL },
            Ordering::Relaxed,
        );
        // Publish `gen` strictly after `invlpg` / `mov cr3` completes on `cpu_idx`.
        slot.acked_gen.store(gen, Ordering::SeqCst);
        return;
    }
}

/// Hardware IDT vector `0xEE` (`VEC_TLB_SHOOTDOWN`) handler (`Interrupt Gate`, `RFLAGS.IF == 0`).
pub fn on_tlb_shootdown_ipi() {
    let cpu_idx = read_gs_u64_at_8() as usize;
    CPU_TLB[cpu_idx]
        .raw_ipi_entries
        .fetch_add(1, Ordering::Relaxed);
    service_pending_shootdown_on_cpu(cpu_idx, true);
    lapic_eoi();
}

/// Cooperative poll invoked inside `CLI` spinloops so `IF == 0` lock waiters never block shootdown ACKs.
#[inline]
pub fn poll_and_ack_pending_shootdown(cpu_idx: usize) {
    service_pending_shootdown_on_cpu(cpu_idx, false);
}

/// Called inside `switch_cpu_vspace_if_needed` immediately AFTER `write_cr3(new_cr3)` flushed
/// `old_vspace`'s non-global TLB entries on `cpu_idx`.
#[inline]
pub(crate) fn ack_if_leaving_vspace(cpu_idx: usize, old_vspace: VSpaceId) {
    let gen = REQ_GEN.load(Ordering::Acquire);
    if gen == 0 {
        return;
    }
    let targets = REQ_TARGET_MASK.load(Ordering::Acquire);
    let req_vs = REQ_VSPACE_ID.load(Ordering::Acquire);
    let start_tsc = REQ_START_TSC.load(Ordering::Acquire);
    if REQ_GEN.load(Ordering::Acquire) != gen || req_vs != old_vspace {
        return;
    }
    if (targets & (1u64 << cpu_idx)) != 0 {
        let slot = &CPU_TLB[cpu_idx];
        let prev = slot.claimed_gen.load(Ordering::Acquire);
        if prev < gen
            && slot
                .claimed_gen
                .compare_exchange(prev, gen, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            slot.remote_sched_cyc
                .store(rdtsc().wrapping_sub(start_tsc).max(1), Ordering::Relaxed);
            slot.remote_flush_cyc.store(1, Ordering::Relaxed);
            slot.ack_kind.store(ACK_VIA_CR3_LEAVE, Ordering::Relaxed);
            slot.acked_gen.store(gen, Ordering::SeqCst);
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

    let local_flush_cyc = if crate::vspace::cpu_active_vspace(caller_cpu) == vspace_id
        || vspace_id == crate::vspace::KERNEL_VSPACE_ID
    {
        flush_range_local(base_vaddr, page_count)
    } else {
        1
    };

    fence(Ordering::SeqCst);
    let initial_targets = active_cpus_ref.load(Ordering::SeqCst) & !(1u64 << caller_cpu);
    if initial_targets == 0 || SKIP_IPI_FOR_STALE_TLB_TEST.load(Ordering::Acquire) {
        LAST_FLUSH_EXEC_CYC.store(local_flush_cyc, Ordering::Release);
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

    let t_ipi_start = rdtsc();
    REQ_VSPACE_ID.store(vspace_id, Ordering::Relaxed);
    REQ_BASE_VADDR.store(base_vaddr, Ordering::Relaxed);
    REQ_PAGE_COUNT.store(page_count, Ordering::Relaxed);
    REQ_TARGET_MASK.store(targets, Ordering::Relaxed);
    REQ_START_TSC.store(t_ipi_start, Ordering::Relaxed);
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
    let t_ipi_done = rdtsc();

    let mut pending = targets;
    loop {
        pending &= active_cpus_ref.load(Ordering::SeqCst);
        let mut chk = pending;
        while chk != 0 {
            let dst = chk.trailing_zeros() as usize;
            chk &= chk - 1;
            if CPU_TLB[dst].acked_gen.load(Ordering::Acquire) >= gen {
                pending &= !(1u64 << dst);
            }
        }
        if pending == 0 {
            break;
        }
        if rdtsc().wrapping_sub(t_ipi_done) > timeout_tsc {
            REQ_GEN.store(0, Ordering::SeqCst);
            TLB_LOCK_OWNER_CPU.store(NO_TLB_LOCK_OWNER, Ordering::Relaxed);
            TLB_REQ_LOCK.store(false, Ordering::Release);
            irq_restore(saved_if);
            tlb_invariant_panic(format_args!(
                "Shootdown timeout: vspace={} vaddr={:#x} targets={:#x} acked={:#x}",
                vspace_id,
                base_vaddr,
                targets,
                targets & !pending
            ));
        }
        core::hint::spin_loop();
    }
    let t_wait_done = rdtsc();

    // Seal epoch `gen` *before* tallying per-target disjoint acknowledgment classes.
    REQ_GEN.store(0, Ordering::SeqCst);

    let mut ipi_acks = 0u64;
    let mut poll_acks = 0u64;
    let mut leave_acks = 0u64;
    let mut max_remote_sched = 1u64;
    let mut max_flush_exec = local_flush_cyc;
    let mut tally = targets;
    while tally != 0 {
        let dst = tally.trailing_zeros() as usize;
        tally &= tally - 1;
        let slot = &CPU_TLB[dst];
        if slot.acked_gen.load(Ordering::Acquire) == gen {
            let r_sched = slot.remote_sched_cyc.load(Ordering::Relaxed);
            let r_flush = slot.remote_flush_cyc.load(Ordering::Relaxed);
            if r_sched > max_remote_sched {
                max_remote_sched = r_sched;
            }
            if r_flush > max_flush_exec {
                max_flush_exec = r_flush;
            }
            match slot.ack_kind.load(Ordering::Acquire) {
                ACK_VIA_IPI => ipi_acks += 1,
                ACK_VIA_SPIN_POLL => poll_acks += 1,
                _ => leave_acks += 1,
            }
        } else {
            // `dst` cleared `active_cpus` via `write_cr3` prior to observing `gen` (`CR3` flushed TLB).
            leave_acks += 1;
        }
    }
    debug_assert_eq!(ipi_acks + poll_acks + leave_acks, ipis_sent);

    TOTAL_TLB_SHOOTDOWNS.fetch_add(1, Ordering::Relaxed);
    TOTAL_TLB_IPI_SENT.fetch_add(ipis_sent, Ordering::Relaxed);
    TOTAL_TLB_IPI_HANDLED.fetch_add(ipi_acks, Ordering::Relaxed);
    TOTAL_TLB_COOP_ACKS.fetch_add(poll_acks + leave_acks, Ordering::Relaxed);
    TOTAL_TLB_POLL_ACKS.fetch_add(poll_acks, Ordering::Relaxed);
    TOTAL_TLB_LEAVE_ACKS.fetch_add(leave_acks, Ordering::Relaxed);

    LAST_IPI_SEND_CYC.store(t_ipi_done.wrapping_sub(t_ipi_start).max(1), Ordering::Release);
    LAST_REMOTE_SCHED_CYC.store(max_remote_sched, Ordering::Release);
    LAST_FLUSH_EXEC_CYC.store(max_flush_exec, Ordering::Release);
    LAST_ACK_WAIT_CYC.store(t_wait_done.wrapping_sub(t_ipi_done).max(1), Ordering::Release);

    TLB_LOCK_OWNER_CPU.store(NO_TLB_LOCK_OWNER, Ordering::Relaxed);
    TLB_REQ_LOCK.store(false, Ordering::Release);
    irq_restore(saved_if);
    if if_was_enabled {
        preempt_enable();
    }
    ipis_sent
}
