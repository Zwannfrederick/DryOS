// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Multi-CPU Cooperative `H1` Scheduler Stress Suite & Negative `H1` Bug-Class Fault Tests.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::arch::{rdtsc, read_rsp};
use crate::gdt_tss::MAX_CPUS;
use crate::percpu::get_percpu_by_idx;
use crate::sched::runqueue::cpu_rq;
use crate::sched::state::{
    sched_invariant_panic, ExecState, ParkToken, ThreadId, WakeOutcome, NO_CPU,
};
use crate::sched::{
    cancel_park_current, commit_park_current, current_cpu_idx, current_slot_on_cpu,
    current_thread_id, idle_dispatch_once, init_scheduler, notify_thread, prepare_park_current,
    tcb, total_ctx_switches, try_spawn_kernel_thread, verify_slot_canary, wake_thread, yield_now,
    AP_PHASE_DONE_CPUS, AP_PHASE_EPOCH, AP_SCHED_RELEASE, GLOBAL_EXITED_THREADS, GLOBAL_MIGRATIONS,
    GLOBAL_RECYCLED_SLOTS, GLOBAL_SPAWNED_THREADS, PHASE_PENDING_THREADS, WORKER_POOL_CAP,
};

const YIELD_ITERS: u64 = 32;
const PARK_ROUNDS: u64 = 32;
const RECYCLE_THREADS: usize = 48;
const RECYCLE_ITERS: u64 = 8;
const MAX_PAIRS: usize = 16;
const PHASE_TIMEOUT_TSC: u64 = 9_000_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SchedFaultTest {
    None,
    StackOwner,
    OrphanReady,
    LostWakeup,
    ExitResurrect,
}

impl SchedFaultTest {
    pub fn from_cmdline(cmdline: &str) -> Self {
        if cmdline.contains("sched_fault=stack_owner") {
            Self::StackOwner
        } else if cmdline.contains("sched_fault=orphan_ready") {
            Self::OrphanReady
        } else if cmdline.contains("sched_fault=lost_wakeup") {
            Self::LostWakeup
        } else if cmdline.contains("sched_fault=exit_resurrect") {
            Self::ExitResurrect
        } else {
            Self::None
        }
    }
}

pub struct SchedVerificationSummary {
    pub cpus: usize,
    pub spawned: u64,
    pub exited: u64,
    pub recycled_slots: u64,
    pub ctx_switches: u64,
    pub migrations: u64,
    pub park_rounds: u64,
    pub work_units: u64,
    pub stale_wakes_rejected: u64,
}

static WORK_UNITS_DONE: AtomicU64 = AtomicU64::new(0);
static PARK_ROUNDS_DONE: AtomicU64 = AtomicU64::new(0);
static CONSUMER_TIDS: [AtomicU32; MAX_PAIRS] = [const { AtomicU32::new(0) }; MAX_PAIRS];
static CONSUMER_ACK_ROUND: [AtomicU64; MAX_PAIRS] = [const { AtomicU64::new(0) }; MAX_PAIRS];
static RECYCLE_TIDS: [AtomicU32; RECYCLE_THREADS] = [const { AtomicU32::new(0) }; RECYCLE_THREADS];

fn spawn_with_idle_assist(
    prio: usize,
    target_cpu: usize,
    entry: fn(u64),
    arg: u64,
) -> ThreadId {
    let start = rdtsc();
    loop {
        if let Some(tid) = try_spawn_kernel_thread(prio, target_cpu, entry, arg) {
            return tid;
        }
        let _ = idle_dispatch_once(0);
        if rdtsc().wrapping_sub(start) >= PHASE_TIMEOUT_TSC {
            sched_invariant_panic(format_args!(
                "Timed out waiting for Dead worker slot reclamation during spawn"
            ));
        }
        core::hint::spin_loop();
    }
}

fn begin_phase_on_all_cpus(expected_threads: usize) {
    AP_PHASE_DONE_CPUS.store(0, Ordering::Release);
    PHASE_PENDING_THREADS.store(expected_threads, Ordering::Release);
    AP_PHASE_EPOCH.fetch_add(1, Ordering::AcqRel);
}

fn drain_phase_and_audit(online_cpus: usize) {
    let start = rdtsc();
    while PHASE_PENDING_THREADS.load(Ordering::Acquire) > 0 {
        if !idle_dispatch_once(0) {
            core::hint::spin_loop();
        }
        if rdtsc().wrapping_sub(start) >= PHASE_TIMEOUT_TSC {
            audit_quiescent_h1_invariants(online_cpus);
            sched_invariant_panic(format_args!(
                "Phase timeout: pending_threads={} (possible H1 Bug3 lost wakeup or deadlock)",
                PHASE_PENDING_THREADS.load(Ordering::Acquire)
            ));
        }
    }

    let ap_target = online_cpus - 1;
    while AP_PHASE_DONE_CPUS.load(Ordering::Acquire) < ap_target {
        assert!(
            rdtsc().wrapping_sub(start) < PHASE_TIMEOUT_TSC,
            "Timed out waiting for AP idle convergence"
        );
        core::hint::spin_loop();
    }

    audit_quiescent_h1_invariants(online_cpus);
}

/// Quiescent `H1` invariant auditor run after every phase (and on timeout).
pub fn audit_quiescent_h1_invariants(online_cpus: usize) {
    let mut all_rqs_empty = true;
    for cpu in 0..online_cpus {
        if !cpu_rq(cpu).is_all_empty() {
            all_rqs_empty = false;
        }
        verify_slot_canary(cpu);
        assert_eq!(
            tcb(cpu).sched.stack_owner(),
            cpu,
            "Idle TCB {} not owned by CPU {} at quiescent barrier",
            cpu,
            cpu
        );
    }

    for idx in 0..WORKER_POOL_CAP {
        let slot = MAX_CPUS + idx;
        let w = tcb(slot);
        let (state, _tok, gen26) = w.sched.snapshot();
        if state == ExecState::Ready && all_rqs_empty {
            sched_invariant_panic(format_args!(
                "H1 Bug2 orphaned Ready thread detected: slot={} gen={} missing from all PerCpuRunQueues",
                slot, gen26
            ));
        }
        assert_eq!(
            state,
            ExecState::Dead,
            "Quiescent worker slot {} in unexpected state {:?}",
            slot,
            state
        );
        assert_eq!(w.sched.stack_owner(), NO_CPU, "Dead slot {} still has stack_owner", slot);
        assert!(
            w.slot_free.load(Ordering::Acquire)
                || w.cspace_id.load(Ordering::Acquire) != super::KERNEL_NO_CSPACE,
            "Dead slot {} not marked slot_free",
            slot
        );
        verify_slot_canary(slot);
    }
}

// =============================================================================
// Phase P1: Cooperative `yield_now`, Callee-Saved Frame & Cross-CPU Migration
// =============================================================================

fn yield_worker(seed: u64) {
    let cpu0 = current_cpu_idx();
    let slot = current_slot_on_cpu(cpu0);
    let my_tid = current_thread_id();
    assert_eq!(my_tid.slot(), slot);
    // Callee-saved local state verified across 32 hardware `dry_switch_to` calls + CPU migrations.
    let mut rolling_guard: u64 = 0x9e37_79b9_7f4a_7c15u64 ^ (seed.wrapping_mul(0x517c_c1b7_2722_0a95));

    for step in 0..YIELD_ITERS {
        let expected_next = rolling_guard.rotate_left(7) ^ step ^ (slot as u64);
        yield_now();
        if step == 0 {
            // Keep `prio = 0` across the first `yield_now()` hop (`cpu0 -> (cpu0 + 1) % online`) so
            // peer vCPUs cannot lose all cross-CPU migrations to work-stealing under KVM host preemption,
            // then switch to migratable `prio 1..3` for the remaining 31 steps.
            tcb(slot)
                .prio
                .store(1 + ((seed as usize) % 3), Ordering::Release);
        }

        let cpu_now = current_cpu_idx();
        assert_eq!(current_thread_id(), my_tid, "ThreadId mutated across yield/migration");
        let rsp = read_rsp();
        let base = tcb(slot).stack_base.load(Ordering::Relaxed);
        let top = tcb(slot).stack_top.load(Ordering::Relaxed);
        assert!(rsp > base && rsp <= top, "RSP escaped worker stack after yield");
        assert_eq!(tcb(slot).sched.stack_owner(), cpu_now, "Stack owner mismatch after yield");

        rolling_guard = rolling_guard.rotate_left(7) ^ step ^ (slot as u64);
        assert_eq!(rolling_guard, expected_next, "Callee-saved register/stack corruption");
        WORK_UNITS_DONE.fetch_add(1, Ordering::AcqRel);
    }
}

// =============================================================================
// Phase P2: `park` / `wake` (`Notification` Doorbell) Contention
// =============================================================================

fn park_consumer_worker(pair_idx_u64: u64) {
    let pair = pair_idx_u64 as usize;
    let slot = current_slot_on_cpu(current_cpu_idx());
    let my_tcb = tcb(slot);

    for round in 0..PARK_ROUNDS {
        let bit = 1u64 << round;
        loop {
            prepare_park_current();
            let fired = my_tcb.notif.consume();
            if (fired & bit) != 0 {
                cancel_park_current();
                break;
            }
            let _ = commit_park_current();
        }
        PARK_ROUNDS_DONE.fetch_add(1, Ordering::AcqRel);
        WORK_UNITS_DONE.fetch_add(1, Ordering::AcqRel);
        CONSUMER_ACK_ROUND[pair].store(round + 1, Ordering::Release);
    }
}

fn park_producer_worker(pair_idx_u64: u64) {
    let pair = pair_idx_u64 as usize;
    let target = ThreadId::from_raw(CONSUMER_TIDS[pair].load(Ordering::Acquire));

    for round in 0..PARK_ROUNDS {
        while CONSUMER_ACK_ROUND[pair].load(Ordering::Acquire) < round {
            yield_now();
        }
        // Vary timing (0, 1, or 2 yields) to hit pre-arm, armed-in-flight, and blocked windows.
        for _ in 0..(round % 3) {
            yield_now();
        }
        let bit = 1u64 << round;
        let preferred_cpu = current_cpu_idx().wrapping_add(pair + 1);
        let outcome = notify_thread(target, bit, preferred_cpu);
        assert_ne!(outcome, WakeOutcome::Terminated, "Live consumer unexpectedly Terminated");
        WORK_UNITS_DONE.fetch_add(1, Ordering::AcqRel);
    }

    while CONSUMER_ACK_ROUND[pair].load(Ordering::Acquire) < PARK_ROUNDS {
        yield_now();
    }
}

// =============================================================================
// Phase P3: `exit` + Live Stack-Slot Recycling + Stale-Handle Wakeup Races
// =============================================================================

fn recycle_exit_worker(idx_u64: u64) {
    let idx = idx_u64 as usize;
    for step in 0..RECYCLE_ITERS {
        if idx > 0 && step == 2 {
            let prev_raw = RECYCLE_TIDS[idx - 1].load(Ordering::Acquire);
            if prev_raw != 0 {
                // Concurrent wakeup race against a peer that may be Running, Exiting, Dead, or Recycled!
                let _ = wake_thread(ThreadId::from_raw(prev_raw), current_cpu_idx() + 1);
            }
        }
        yield_now();
        WORK_UNITS_DONE.fetch_add(1, Ordering::AcqRel);
    }
}

// =============================================================================
// Negative `H1` Bug-Class Fault Scenarios (`sched_fault=*`)
// =============================================================================

fn unmarked_park_fault_worker(_arg: u64) {
    // Intentionally violate H1 Step 1 (`prepare_park_current` omitted) -> triggers Bug3 guard!
    let _ = commit_park_current();
}

fn noop_short_worker(_arg: u64) {
    yield_now();
}

fn run_negative_sched_fault(online_cpus: usize, fault: SchedFaultTest) -> ! {
    AP_SCHED_RELEASE.store(true, Ordering::Release);

    match fault {
        SchedFaultTest::StackOwner => {
            // H1 Bug1: Slot marked `Ready` and pushed to CPU 1 while `stack_owner` is still held by CPU 0.
            let slot = MAX_CPUS;
            let w = tcb(slot);
            w.slot_free.store(false, Ordering::Release);
            let tid = w.sched.init_spawned_ready(slot, 2);
            w.sched
                .force_raw_for_fault_test(ExecState::Ready, ParkToken::None, 2, 0);
            begin_phase_on_all_cpus(1);
            assert!(cpu_rq(1 % online_cpus).push(0, tid.raw()));
            drain_phase_and_audit(online_cpus);
        }
        SchedFaultTest::OrphanReady => {
            // H1 Bug2: Task transitioned to `ExecState::Ready` without publishing to any `PerCpuRunQueue`.
            let slot = MAX_CPUS;
            let w = tcb(slot);
            w.slot_free.store(false, Ordering::Release);
            let _ = w.sched.init_spawned_ready(slot, 2);
            audit_quiescent_h1_invariants(online_cpus);
        }
        SchedFaultTest::LostWakeup => {
            // H1 Bug3: Thread enters `commit_park` without `ParkToken::Armed`.
            begin_phase_on_all_cpus(1);
            let _ = spawn_with_idle_assist(0, 1 % online_cpus, unmarked_park_fault_worker, 0);
            drain_phase_and_audit(online_cpus);
        }
        SchedFaultTest::ExitResurrect => {
            // H1 Bug4: Cleanly run & exit a thread to `Dead`, then inject its retired `ThreadId` into RQ.
            begin_phase_on_all_cpus(1);
            let retired_tid = spawn_with_idle_assist(0, 0, noop_short_worker, 0);
            drain_phase_and_audit(online_cpus);

            begin_phase_on_all_cpus(1);
            assert!(cpu_rq(1 % online_cpus).push(0, retired_tid.raw()));
            drain_phase_and_audit(online_cpus);
        }
        SchedFaultTest::None => unreachable!(),
    }
    panic!("Negative scheduler fault test failed to trap as expected");
}

/// Run the full Milestone 1A-F cooperative `H1` scheduler verification across `online_cpus`.
pub fn verify_cooperative_scheduler(
    online_cpus: usize,
    fault: SchedFaultTest,
) -> SchedVerificationSummary {
    init_scheduler(online_cpus);
    WORK_UNITS_DONE.store(0, Ordering::Release);
    PARK_ROUNDS_DONE.store(0, Ordering::Release);

    if fault != SchedFaultTest::None {
        run_negative_sched_fault(online_cpus, fault);
    }

    AP_SCHED_RELEASE.store(true, Ordering::Release);

    // Phase P1: Cooperative `yield_now` + cross-CPU migration (`prio 0` first hop -> `prio 1..3`).
    let n_yield = (online_cpus * 2).clamp(8, 24);
    begin_phase_on_all_cpus(n_yield);
    for i in 0..n_yield {
        let init_prio = if i < online_cpus { 0 } else { 1 + (i % 3) };
        let _ = spawn_with_idle_assist(init_prio, i % online_cpus, yield_worker, i as u64);
    }
    drain_phase_and_audit(online_cpus);

    // Phase P2: Contended `Notification` park/wake pairs across CPUs.
    let n_pairs = online_cpus.clamp(4, 12);
    for p in 0..n_pairs {
        CONSUMER_ACK_ROUND[p].store(0, Ordering::Release);
        CONSUMER_TIDS[p].store(0, Ordering::Release);
    }
    begin_phase_on_all_cpus(n_pairs * 2);
    for (p, consumer_slot) in CONSUMER_TIDS.iter().enumerate().take(n_pairs) {
        let cid = spawn_with_idle_assist(1, p % online_cpus, park_consumer_worker, p as u64);
        consumer_slot.store(cid.raw(), Ordering::Release);
        let _ = spawn_with_idle_assist(1, (p + 1) % online_cpus, park_producer_worker, p as u64);
    }
    drain_phase_and_audit(online_cpus);

    // Phase P3: 48 exiting threads across 36 physical stack slots -> forces live `Dead` slot
    // reclamation & stack reuse under SMP load, plus stale `ThreadId` wakeup rejection.
    for slot in &RECYCLE_TIDS {
        slot.store(0, Ordering::Release);
    }
    begin_phase_on_all_cpus(RECYCLE_THREADS);
    for (i, recycle_slot) in RECYCLE_TIDS.iter().enumerate() {
        let tid = spawn_with_idle_assist(1 + (i % 3), i % online_cpus, recycle_exit_worker, i as u64);
        recycle_slot.store(tid.raw(), Ordering::Release);
    }
    drain_phase_and_audit(online_cpus);

    let mut stale_wakes_rejected = 0u64;
    for slot in &RECYCLE_TIDS {
        let old_tid = ThreadId::from_raw(slot.load(Ordering::Acquire));
        assert_eq!(
            wake_thread(old_tid, 0),
            WakeOutcome::Terminated,
            "Retired/recycled ThreadId unexpectedly accepted wakeup!"
        );
        stale_wakes_rejected += 1;
    }

    let spawned = GLOBAL_SPAWNED_THREADS.load(Ordering::Acquire);
    let exited = GLOBAL_EXITED_THREADS.load(Ordering::Acquire);
    let recycled_slots = GLOBAL_RECYCLED_SLOTS.load(Ordering::Acquire);
    let ctx_switches = total_ctx_switches();
    let migrations = GLOBAL_MIGRATIONS.load(Ordering::Acquire);
    let park_rounds = PARK_ROUNDS_DONE.load(Ordering::Acquire);
    let work_units = WORK_UNITS_DONE.load(Ordering::Acquire);

    assert_eq!(spawned, exited, "Spawned vs exited thread count mismatch");
    assert!(recycled_slots >= 28, "Expected >= 28 recycled worker stack allocations");

    let mut percpu_sw_sum = 0u64;
    for cpu in 0..online_cpus {
        let sw = get_percpu_by_idx(cpu).ctx_switches.load(Ordering::Acquire);
        assert!(sw > 0, "CPU {} performed 0 context switches during scheduler suite", cpu);
        percpu_sw_sum += sw;
    }
    assert_eq!(percpu_sw_sum, ctx_switches, "Per-CPU ctx_switches sum != total_ctx_switches()");
    if online_cpus >= 2 {
        assert!(migrations > 0, "Expected cross-CPU thread migrations on SMP");
    }

    SchedVerificationSummary {
        cpus: online_cpus,
        spawned,
        exited,
        recycled_slots,
        ctx_switches,
        migrations,
        park_rounds,
        work_units,
        stale_wakes_rejected,
    }
}
