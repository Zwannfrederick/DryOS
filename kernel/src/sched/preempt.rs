// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Preemptive `H1` Scheduler Control (`preempt_count`, `need_resched`, & Local APIC Timer ISR).
//!
//! Enforces the Milestone 1A-G interrupt-context vs thread-context stack ownership and non-blocking
//! contracts:
//! - `IST = 0`: `TrapFrame` + `SwitchFrame` are pushed onto the interrupted thread's private stack.
//! - Local APIC `EOI` is issued strictly *before* `switch_to_slot` so `ISR[0xEF]` never masks
//!   subsequent ticks on the CPU after switching away.
//! - `0` spinlocks, `0` heap allocations, and `0` sleeping operations inside `on_lapic_timer_irq`.
//! - Live zero-progress watchdog (`IRQ_STORM_MAX_STREAK`) catches hardware/software timer storms.

use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};

use crate::apic::QUANTUM_TICKS;
use crate::arch::{cli, rdtsc, read_rflags, read_rsp};
use crate::gdt_tss::MAX_CPUS;
use crate::idt::TrapFrame;
use crate::percpu::{current_percpu, PerCpuData};
use crate::sched::context::PostSwitchKind;
use crate::sched::runqueue::pop_local_or_steal;
use crate::sched::state::{preempt_invariant_panic, ExecState, ThreadId};
use crate::sched::{
    current_cpu_idx, current_slot_on_cpu, record_worker_dispatch, switch_to_slot, tcb,
    verify_slot_canary, wake_thread, yield_now, GLOBAL_DEFERRED_RESCHEDS, GLOBAL_PREEMPTIONS,
    GLOBAL_TIMER_IRQS, ONLINE_SCHED_CPUS, PHASE_PENDING_THREADS, TOTAL_TCB_SLOTS, WORKER_POOL_CAP,
};

const IRQ_STORM_MAX_STREAK: u32 = 256;
const EPOCH_WATCHDOG_TSC: u64 = 15_000_000_000;

static PREEMPT_MODE_ACTIVE: AtomicBool = AtomicBool::new(false);
static NESTED_IRQ_FAULT_INJECT: AtomicBool = AtomicBool::new(false);
static EPOCH_START_TSC: AtomicU64 = AtomicU64::new(0);
static ACTIVE_SUBSTAGE: AtomicU8 = AtomicU8::new(0);

#[inline]
pub fn arm_epoch_watchdog(substage: u8) {
    ACTIVE_SUBSTAGE.store(substage, Ordering::Release);
    EPOCH_START_TSC.store(rdtsc(), Ordering::Release);
}

#[inline]
pub fn disarm_epoch_watchdog() {
    EPOCH_START_TSC.store(0, Ordering::Release);
}

#[inline]
pub fn is_preemptive_mode_active() -> bool {
    PREEMPT_MODE_ACTIVE.load(Ordering::Acquire)
}

#[inline]
pub fn set_preemptive_mode_active(active: bool) {
    PREEMPT_MODE_ACTIVE.store(active, Ordering::Release);
}

#[inline]
pub fn set_nested_irq_fault_inject(enable: bool) {
    NESTED_IRQ_FAULT_INJECT.store(enable, Ordering::Release);
}

#[inline]
pub fn note_current_thread_progress() {
    let slot = current_slot_on_cpu(current_cpu_idx());
    tcb(slot).progress_seq.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn preempt_count() -> u32 {
    current_percpu().preempt_count.load(Ordering::Acquire)
}

/// Increment per-CPU preemption disable nesting counter (`Acquire` barrier against reordering).
#[inline]
pub fn preempt_disable() {
    let pcpu = current_percpu();
    let prev = pcpu.preempt_count.fetch_add(1, Ordering::Acquire);
    assert!(prev < u32::MAX - 1, "preempt_count overflow");
}

/// Decrement per-CPU preemption disable nesting counter; if dropping `1 -> 0` outside IRQ context
/// with `IF == 1` and `need_resched == true`, immediately deliver the deferred preemption.
#[inline]
pub fn preempt_enable() {
    let pcpu = current_percpu();
    let prev = pcpu.preempt_count.fetch_sub(1, Ordering::AcqRel);
    if prev == 0 {
        preempt_invariant_panic(format_args!(
            "preempt_count underflow on cpu={}",
            pcpu.cpu_idx
        ));
    }
    if prev == 1
        && pcpu.irq_depth.load(Ordering::Relaxed) == 0
        && (read_rflags() & (1 << 9)) != 0
    {
        let cpu_idx = pcpu.cpu_idx as usize;
        if current_slot_on_cpu(cpu_idx) >= MAX_CPUS
            && pcpu.need_resched.swap(false, Ordering::AcqRel)
        {
            pcpu.preemptions.fetch_add(1, Ordering::Relaxed);
            pcpu.deferred_rescheds.fetch_add(1, Ordering::Relaxed);
            GLOBAL_PREEMPTIONS.fetch_add(1, Ordering::Relaxed);
            GLOBAL_DEFERRED_RESCHEDS.fetch_add(1, Ordering::Relaxed);
            yield_now();
        }
    }
}

/// Lock-free ISR/idle scan waking worker threads whose bounded IPC TSC deadline has expired.
#[inline]
pub fn poll_expired_ipc_deadlines(now_tsc: u64, offset: usize, stride: usize) {
    let step = stride.max(1);
    let mut idx = offset;
    while idx < WORKER_POOL_CAP {
        let slot = MAX_CPUS + idx;
        let w = tcb(slot);
        let dl = w.ipc_deadline_tsc.load(Ordering::Relaxed);
        if dl != 0
            && now_tsc >= dl
            && w.ipc_deadline_tsc
                .compare_exchange(dl, 0, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
        {
            let (_, _, gen26) = w.sched.snapshot();
            let last = w.last_cpu.load(Ordering::Relaxed);
            let pref = if last < MAX_CPUS { last } else { 0 };
            let _ = wake_thread(ThreadId::new(slot, gen26), pref);
        }
        idx += step;
    }
}

pub fn start_cpu_timer_for_phase(pcpu: &'static PerCpuData) {
    pcpu.need_resched.store(false, Ordering::Release);
    pcpu.quantum_rem.store(QUANTUM_TICKS, Ordering::Release);
    pcpu.storm_streak.store(0, Ordering::Release);
    pcpu.storm_slot.store(u64::MAX, Ordering::Release);
    pcpu.timer_armed.store(true, Ordering::Release);
    crate::apic::arm_default_oneshot_tick();
}

pub fn stop_cpu_timer_for_phase(pcpu: &'static PerCpuData) {
    cli();
    pcpu.timer_armed.store(false, Ordering::Release);
    crate::apic::stop_cpu_lapic_timer();
    // Drain at most one already-latched IRR edge while `timer_armed == false` so no stale IRQ
    // survives into a subsequent phase; leaves `RFLAGS.IF == 0` on exit.
    // SAFETY: IDT + Local APIC are valid and `timer_armed == false` prevents re-arming.
    unsafe {
        asm!("sti", "nop", "cli", options(nomem, nostack, preserves_flags));
    }
    assert_eq!(
        pcpu.irq_depth.load(Ordering::Acquire),
        0,
        "Non-zero irq_depth after timer stop"
    );
    assert_eq!(
        pcpu.preempt_count.load(Ordering::Acquire),
        0,
        "Leaked preempt_count after phase"
    );
    assert!(
        !crate::apic::is_timer_isr_bit_set(),
        "Local APIC ISR[0xEF] still set after timer stop (missing EOI)"
    );
}

/// Local APIC Timer Interrupt Handler (`VEC_LAPIC_TIMER = 0xEF`, `IST = 0`, `Interrupt Gate IF=0`).
///
/// Strict non-blocking contract: `0` spinlocks, `0` heap allocations, `0` sleeping operations.
pub fn on_lapic_timer_irq(frame: &mut TrapFrame) {
    let pcpu = current_percpu();
    let cpu_idx = pcpu.cpu_idx as usize;

    let depth = pcpu.irq_depth.fetch_add(1, Ordering::AcqRel) + 1;
    if depth > 1 {
        preempt_invariant_panic(format_args!(
            "Nested timer interrupt violation (irq_depth={}) on cpu={}",
            depth, cpu_idx
        ));
    }
    if (read_rflags() & (1 << 9)) != 0 {
        preempt_invariant_panic(format_args!(
            "Timer ISR entered with RFLAGS.IF=1 on cpu={}",
            cpu_idx
        ));
    }

    let cur_slot = pcpu.current_slot.load(Ordering::Relaxed) as usize;
    let isr_rsp = read_rsp();
    let stk_base = tcb(cur_slot).stack_base.load(Ordering::Relaxed);
    let stk_top = tcb(cur_slot).stack_top.load(Ordering::Relaxed);
    if isr_rsp <= stk_base || isr_rsp > stk_top {
        preempt_invariant_panic(format_args!(
            "IRQ stack ownership violation: rsp={:#x} outside slot={} stack [{:#x},{:#x}] on cpu={}",
            isr_rsp, cur_slot, stk_base, stk_top, cpu_idx
        ));
    }
    if isr_rsp >= pcpu.df_ist1_base && isr_rsp < pcpu.df_ist1_top {
        preempt_invariant_panic(format_args!(
            "Timer IRQ illegally landed on #DF IST1 stack on cpu={}",
            cpu_idx
        ));
    }
    verify_slot_canary(cur_slot);
    assert_ne!(
        frame.rflags & (1 << 9),
        0,
        "Timer IRQ delivered while interrupted frame had IF=0"
    );

    pcpu.timer_irqs.fetch_add(1, Ordering::AcqRel);
    GLOBAL_TIMER_IRQS.fetch_add(1, Ordering::Relaxed);

    // Issue Local APIC EOI strictly BEFORE any context switch so `ISR[0xEF]` never blocks future ticks.
    crate::apic::lapic_eoi();

    if NESTED_IRQ_FAULT_INJECT.load(Ordering::Relaxed) {
        // SAFETY: Intentional negative fault test verifying `irq_depth > 1` detection.
        unsafe {
            asm!("sti", "int 0xef", options(nostack));
        }
    }

    if !pcpu.timer_armed.load(Ordering::Acquire) {
        pcpu.irq_depth.fetch_sub(1, Ordering::Release);
        return;
    }

    if !PREEMPT_MODE_ACTIVE.load(Ordering::Acquire) {
        crate::apic::arm_default_oneshot_tick();
        pcpu.irq_depth.fetch_sub(1, Ordering::Release);
        return;
    }

    let online_for_dl = ONLINE_SCHED_CPUS.load(Ordering::Relaxed).max(1);
    poll_expired_ipc_deadlines(rdtsc(), cpu_idx % online_for_dl, online_for_dl);

    if (frame.cs & 3) == 3 && cur_slot >= MAX_CPUS {
        let pid = tcb(cur_slot).process_id.load(Ordering::Acquire);
        if pid != 0 && crate::proc::is_process_kill_requested(pid) {
            pcpu.irq_depth.store(0, Ordering::Release);
            crate::proc::terminate_killed_from_cpl3_irq();
        }
        tcb(cur_slot).progress_seq.fetch_add(1, Ordering::Relaxed);
    }

    if cpu_idx == 0 {
        let ep_start = EPOCH_START_TSC.load(Ordering::Relaxed);
        let wd_limit = if ONLINE_SCHED_CPUS.load(Ordering::Relaxed) > 8 {
            EPOCH_WATCHDOG_TSC * 3
        } else {
            EPOCH_WATCHDOG_TSC
        };
        if ep_start != 0 && rdtsc().wrapping_sub(ep_start) >= wd_limit {
            crate::enter_fatal_or_halt();
            crate::serial::emergency_print(format_args!(
                "\r\n[DRY-PREEMPT-WATCHDOG] substage=G{} pending={} preempts={} irqs={} cpu0_slot={}\r\n",
                ACTIVE_SUBSTAGE.load(Ordering::Relaxed),
                PHASE_PENDING_THREADS.load(Ordering::Relaxed),
                GLOBAL_PREEMPTIONS.load(Ordering::Relaxed),
                GLOBAL_TIMER_IRQS.load(Ordering::Relaxed),
                cur_slot
            ));
            for s in MAX_CPUS..TOTAL_TCB_SLOTS {
                let (st, tk, gn) = tcb(s).sched.snapshot();
                if st != ExecState::Dead {
                    crate::serial::emergency_print(format_args!(
                        "  [TCB slot={} gen={} state={:?} tok={:?} owner={} inflight={} preempts={}]\r\n",
                        s,
                        gn,
                        st,
                        tk,
                        tcb(s).sched.stack_owner(),
                        tcb(s).sched.in_flight_refs(),
                        tcb(s).preempted_count.load(Ordering::Relaxed),
                    ));
                }
            }
            panic!("Preemptive epoch watchdog fired");
        }
    }

    // Live zero-progress / interrupt-storm watchdog on both active worker threads and idle dispatchers.
    let storm_monitored =
        cur_slot >= MAX_CPUS || PHASE_PENDING_THREADS.load(Ordering::Relaxed) > 0;
    if storm_monitored && pcpu.preempt_count.load(Ordering::Relaxed) == 0 {
        let prog = tcb(cur_slot).progress_seq.load(Ordering::Relaxed);
        let prev_slot = pcpu.storm_slot.load(Ordering::Relaxed);
        let prev_prog = pcpu.last_progress_seq.load(Ordering::Relaxed);
        if prev_slot == (cur_slot as u64) && prog == prev_prog {
            let streak = pcpu.storm_streak.fetch_add(1, Ordering::Relaxed) + 1;
            if streak >= IRQ_STORM_MAX_STREAK {
                preempt_invariant_panic(format_args!(
                    "Timer interrupt storm / zero-progress livelock detected on cpu={} slot={} streak={}",
                    cpu_idx, cur_slot, streak
                ));
            }
        } else {
            pcpu.storm_slot.store(cur_slot as u64, Ordering::Relaxed);
            pcpu.last_progress_seq.store(prog, Ordering::Relaxed);
            pcpu.storm_streak.store(0, Ordering::Relaxed);
        }
    }

    let rem = pcpu.quantum_rem.load(Ordering::Relaxed);
    if rem > 1 {
        pcpu.quantum_rem.store(rem - 1, Ordering::Relaxed);
        crate::apic::arm_default_oneshot_tick();
        pcpu.irq_depth.fetch_sub(1, Ordering::Release);
        return;
    }
    pcpu.quantum_rem.store(QUANTUM_TICKS, Ordering::Relaxed);

    if cur_slot < MAX_CPUS || pcpu.preempt_count.load(Ordering::Acquire) > 0 {
        pcpu.need_resched.store(true, Ordering::Release);
        crate::apic::arm_default_oneshot_tick();
        pcpu.irq_depth.fetch_sub(1, Ordering::Release);
        return;
    }

    let online = ONLINE_SCHED_CPUS.load(Ordering::Relaxed);
    let Some(raw_ticket) = pop_local_or_steal(cpu_idx, online) else {
        crate::apic::arm_default_oneshot_tick();
        pcpu.irq_depth.fetch_sub(1, Ordering::Release);
        return;
    };

    let ticket = ThreadId::from_raw(raw_ticket);
    let next_slot = ticket.slot();
    let target_cpu = (cpu_idx + 1) % online;

    pcpu.need_resched.store(false, Ordering::Release);
    pcpu.preemptions.fetch_add(1, Ordering::Relaxed);
    GLOBAL_PREEMPTIONS.fetch_add(1, Ordering::Relaxed);
    tcb(cur_slot).preempted_count.fetch_add(1, Ordering::Relaxed);

    tcb(cur_slot).sched.begin_yield(cur_slot, cpu_idx);
    tcb(next_slot).sched.dispatch_popped_ticket(ticket, cpu_idx);
    record_worker_dispatch(next_slot, cpu_idx);

    pcpu.irq_depth.store(0, Ordering::Release);
    switch_to_slot(cpu_idx, cur_slot, next_slot, PostSwitchKind::Yield { target_cpu });

    let resumed_cpu = current_cpu_idx();
    assert_eq!(current_slot_on_cpu(resumed_cpu), cur_slot);
    assert_eq!(tcb(cur_slot).sched.stack_owner(), resumed_cpu);
}
