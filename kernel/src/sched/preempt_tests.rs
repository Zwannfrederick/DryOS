// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Milestone 1A-G Preemptive `H1` Scheduler Verification Suite & Negative Fault Tests.
//!
//! Executes in strict incremental sub-stages:
//! - **Sub-Stage G1 (`IRQ-Only`):** Per-CPU Local APIC timer delivery, `EOI` (`ISR[0xEF] == 0`),
//!   and `TrapFrame` 15-GPR + `RFLAGS` (`IF/CF/ZF`) + 16-byte stack alignment integrity.
//! - **Sub-Stage G2 (`preempt_count` Nesting):** `0 -> 1 -> 2 -> 1 -> 0` preemption suppression
//!   and immediate `need_resched` delivery on outermost `preempt_enable()`.
//! - **Sub-Stage G3 (`Non-Yielding Spinner Preemption`):** Pure CPU-bound kernel threads (`0`
//!   `yield_now()` calls) involuntarily preempted across `1, 2, 4, 20` vCPUs with full register
//!   and arithmetic flag verification.
//! - **Sub-Stage G4 (`Preemptive Mixed Race Stress`):** Concurrent timer-IRQ preemptions racing
//!   against `yield_now`, `Notification` `park`/`wake` (`IF = 1`), `exit`, and 36-slot recycling.

use core::arch::global_asm;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::apic::{
    active_apic_mode, init_cpu_lapic, restore_calibrated_tick_init, set_active_tick_init_override,
    spurious_apic_irqs, QUANTUM_TICKS,
};
use crate::arch::{rdtsc, read_rflags, sti};
use crate::percpu::get_percpu_by_idx;
use crate::sched::state::{preempt_invariant_panic, ThreadId, WakeOutcome};
use crate::sched::tests::audit_quiescent_h1_invariants;
use crate::sched::{
    arm_epoch_watchdog, cancel_park_current, commit_park_current, current_cpu_idx,
    current_slot_on_cpu, disarm_epoch_watchdog, idle_dispatch_once, init_scheduler,
    note_current_thread_progress, notify_thread, preempt_count, preempt_disable, preempt_enable,
    prepare_park_current, set_ap_epoch_kind, set_nested_irq_fault_inject,
    set_preemptive_mode_active, start_cpu_timer_for_phase, stop_cpu_timer_for_phase, tcb,
    total_ctx_switches, try_spawn_kernel_thread_ex, wake_thread, yield_now, ApEpochKind,
    AP_PHASE_DONE_CPUS, AP_PHASE_EPOCH, AP_SCHED_RELEASE, GLOBAL_DEFERRED_RESCHEDS,
    GLOBAL_EXITED_THREADS, GLOBAL_MIGRATIONS, GLOBAL_PREEMPTIONS, GLOBAL_RECYCLED_SLOTS,
    GLOBAL_SPAWNED_THREADS, GLOBAL_TIMER_IRQS, PHASE_PENDING_THREADS,
};
use crate::serial_println;

const PREEMPT_TIMEOUT_TSC: u64 = 12_000_000_000;
const RACE_YIELD_ITERS: u64 = 24;
const RACE_PARK_ROUNDS: u64 = 24;
const RACE_RECYCLE_THREADS: usize = 48;
const RACE_RECYCLE_ITERS: u64 = 6;
const MAX_RACE_PAIRS: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PreemptFaultTest {
    None,
    IrqStorm,
    NestedIrq,
    LostWakeup,
    DoubleSched,
    StackReuse,
}

impl PreemptFaultTest {
    pub fn from_cmdline(cmdline: &str) -> Self {
        if cmdline.contains("preempt_fault=irq_storm") {
            Self::IrqStorm
        } else if cmdline.contains("preempt_fault=nested_irq") {
            Self::NestedIrq
        } else if cmdline.contains("preempt_fault=lost_wakeup") {
            Self::LostWakeup
        } else if cmdline.contains("preempt_fault=double_sched") {
            Self::DoubleSched
        } else if cmdline.contains("preempt_fault=stack_reuse") {
            Self::StackReuse
        } else {
            Self::None
        }
    }
}

// Assembly torture routine proving `TrapFrame` + `SwitchFrame` preserve ALL caller-saved GPRs
// (`rax,rcx,rdx,rsi,rdi,r8..r11`), callee-saved GPRs (`rbx,r12..r15`), arithmetic `RFLAGS`
// (`ZF`, `CF`), and 16-byte System V stack alignment (`RSP % 16 == 0`) across asynchronous
// Local APIC timer interrupts (`iretq`) AND involuntary thread preemptions + CPU migrations.
global_asm!(
    ".global dry_irq_frame_torture",
    ".balign 16",
    "dry_irq_frame_torture:",
    "    push rbp",
    "    push rbx",
    "    push r12",
    "    push r13",
    "    push r14",
    "    push r15",
    "    sub rsp, 24",
    "    mov [rsp + 0], rcx",
    "    mov rax, 500000000",
    "    mov [rsp + 8], rax",
    "    mov r13, rdi",
    "    mov r14, rsi",
    "    mov r15, rdx",
    "    mov rax, 0x1111222233334444",
    "    xor rax, r13",
    "    mov rbx, 0x2222333344445555",
    "    xor rbx, r13",
    "    mov rcx, 0x3333444455556666",
    "    xor rcx, r13",
    "    mov rdx, 0x4444555566667777",
    "    xor rdx, r13",
    "    mov rsi, 0x5555666677778888",
    "    xor rsi, r13",
    "    mov rdi, 0x6666777788889999",
    "    xor rdi, r13",
    "    mov r8,  0x777788889999aaaa",
    "    xor r8,  r13",
    "    mov r9,  0x88889999aaaabbbb",
    "    xor r9,  r13",
    "    mov r10, 0x9999aaaabbbbcccc",
    "    xor r10, r13",
    "    mov r11, 0xaaaabbbbccccdddd",
    "    xor r11, r13",
    "    mov r12, 0xbbbbccccddddeeee",
    "    xor r12, r13",
    "2:",
    "    mov rbp, [rsp + 0]",
    "    add qword ptr [rbp], 1",
    "    test rsp, 0xf",
    "    jnz 91f",
    "    jne 92f",
    "    jc  92f",
    "    stc",
    "    jnc 92f",
    "    clc",
    "    jc  92f",
    "    mov rbp, 0x1111222233334444",
    "    xor rbp, r13",
    "    cmp rax, rbp",
    "    jne 93f",
    "    mov rbp, 0x2222333344445555",
    "    xor rbp, r13",
    "    cmp rbx, rbp",
    "    jne 93f",
    "    mov rbp, 0x3333444455556666",
    "    xor rbp, r13",
    "    cmp rcx, rbp",
    "    jne 93f",
    "    mov rbp, 0x4444555566667777",
    "    xor rbp, r13",
    "    cmp rdx, rbp",
    "    jne 93f",
    "    mov rbp, 0x5555666677778888",
    "    xor rbp, r13",
    "    cmp rsi, rbp",
    "    jne 93f",
    "    mov rbp, 0x6666777788889999",
    "    xor rbp, r13",
    "    cmp rdi, rbp",
    "    jne 93f",
    "    mov rbp, 0x777788889999aaaa",
    "    xor rbp, r13",
    "    cmp r8, rbp",
    "    jne 93f",
    "    mov rbp, 0x88889999aaaabbbb",
    "    xor rbp, r13",
    "    cmp r9, rbp",
    "    jne 93f",
    "    mov rbp, 0x9999aaaabbbbcccc",
    "    xor rbp, r13",
    "    cmp r10, rbp",
    "    jne 93f",
    "    mov rbp, 0xaaaabbbbccccdddd",
    "    xor rbp, r13",
    "    cmp r11, rbp",
    "    jne 93f",
    "    mov rbp, 0xbbbbccccddddeeee",
    "    xor rbp, r13",
    "    cmp r12, rbp",
    "    jne 93f",
    "    cmp qword ptr [r15], r14",
    "    jae 90f",
    "    sub qword ptr [rsp + 8], 1",
    "    jz  94f",
    "    pause",
    "    jmp 2b",
    "90: xor eax, eax",
    "    jmp 99f",
    "91: mov eax, 1",
    "    jmp 99f",
    "92: mov eax, 2",
    "    jmp 99f",
    "93: mov eax, 3",
    "    jmp 99f",
    "94: mov eax, 4",
    "99:",
    "    add rsp, 24",
    "    pop r15",
    "    pop r14",
    "    pop r13",
    "    pop r12",
    "    pop rbx",
    "    pop rbp",
    "    ret",
);

extern "C" {
    fn dry_irq_frame_torture(
        seed: u64,
        target_val: u64,
        watch_ptr: *const u64,
        progress_ptr: *mut u64,
    ) -> u64;
}

static NESTING_DONE: AtomicBool = AtomicBool::new(false);
static SPINNER_COUNT_TARGET: AtomicU64 = AtomicU64::new(0);
static SPINNER_ARRIVED: AtomicU64 = AtomicU64::new(0);
static SPINNER_PREEMPT_GOAL: AtomicU64 = AtomicU64::new(0);

static RACE_ONLINE_CPUS: AtomicU64 = AtomicU64::new(1);
static RACE_ALL_SPAWNED: AtomicBool = AtomicBool::new(false);
static RACE_PARK_ROUNDS_DONE: AtomicU64 = AtomicU64::new(0);
static RACE_CONSUMER_TIDS: [AtomicU32; MAX_RACE_PAIRS] = [const { AtomicU32::new(0) }; MAX_RACE_PAIRS];
static RACE_CONSUMER_ACK: [AtomicU64; MAX_RACE_PAIRS] = [const { AtomicU64::new(0) }; MAX_RACE_PAIRS];
static RACE_RECYCLE_TIDS: [AtomicU32; RACE_RECYCLE_THREADS] =
    [const { AtomicU32::new(0) }; RACE_RECYCLE_THREADS];
static ACTIVE_REUSE_SLOT: AtomicU64 = AtomicU64::new(0);

fn run_cpu_irq_only_torture(cpu_idx: usize) {
    let pcpu = get_percpu_by_idx(cpu_idx);
    let start_irqs = pcpu.timer_irqs.load(Ordering::Acquire);
    let target_irqs = start_irqs + 10;
    let seed = 0x5a5a_0000_c0de_0000u64 ^ ((cpu_idx as u64 + 1).wrapping_mul(0x9e37_79b9));

    start_cpu_timer_for_phase(pcpu);
    sti();
    assert_ne!(read_rflags() & (1 << 9), 0, "RFLAGS.IF failed to enable on CPU {}", cpu_idx);

    // SAFETY: `pcpu.timer_irqs` and `tcb(cpu_idx).progress_seq` live in `'static` `.bss`.
    let rc = unsafe {
        dry_irq_frame_torture(
            seed,
            target_irqs,
            pcpu.timer_irqs.as_ptr(),
            tcb(cpu_idx).progress_seq.as_ptr(),
        )
    };
    stop_cpu_timer_for_phase(pcpu);

    assert_eq!(
        rc, 0,
        "IRQ-only TrapFrame/RFLAGS/alignment torture failed (code={}) on CPU {}",
        rc, cpu_idx
    );
    let end_irqs = pcpu.timer_irqs.load(Ordering::Acquire);
    assert!(
        end_irqs >= target_irqs,
        "Expected >= {} timer IRQs on CPU {}, got {}",
        target_irqs,
        cpu_idx,
        end_irqs
    );
}

pub(crate) fn run_ap_irq_only_stage(cpu_idx: usize) {
    init_cpu_lapic(cpu_idx);
    run_cpu_irq_only_torture(cpu_idx);
}

pub(crate) fn begin_preemptive_epoch(substage: u8, expected_threads: usize) {
    arm_epoch_watchdog(substage);
    set_ap_epoch_kind(ApEpochKind::PreemptiveSched);
    AP_PHASE_DONE_CPUS.store(0, Ordering::Release);
    PHASE_PENDING_THREADS.store(expected_threads, Ordering::Release);
    AP_PHASE_EPOCH.fetch_add(1, Ordering::AcqRel);
}

fn spawn_preemptible_with_assist(
    prio: usize,
    target_cpu: usize,
    entry: fn(u64),
    arg: u64,
) -> ThreadId {
    let start = rdtsc();
    let online = crate::sched::ONLINE_SCHED_CPUS.load(Ordering::Relaxed);
    let timeout_tsc = if online > 8 { PREEMPT_TIMEOUT_TSC * 3 } else { PREEMPT_TIMEOUT_TSC };
    let bsp_pcpu = get_percpu_by_idx(0);
    loop {
        if let Some(tid) = try_spawn_kernel_thread_ex(prio, target_cpu, entry, arg, true) {
            return tid;
        }
        // On 1-vCPU (no APs), BSP must dispatch runnable work to free a `Dead` slot.
        // On >=2 vCPUs, APs `1..online` are already draining slots in parallel; keeping BSP on
        // `slot 0` until spawning finishes prevents a non-yielding worker from hijacking BSP mid-spawn.
        if online == 1 {
            start_cpu_timer_for_phase(bsp_pcpu);
            sti();
            let _ = idle_dispatch_once(0);
            stop_cpu_timer_for_phase(bsp_pcpu);
        }
        if rdtsc().wrapping_sub(start) >= timeout_tsc {
            preempt_invariant_panic(format_args!(
                "Timed out waiting for Dead worker slot reclamation during preemptive spawn"
            ));
        }
        core::hint::spin_loop();
    }
}

pub(crate) fn drain_preemptive_epoch_and_audit(online_cpus: usize) {
    let start = rdtsc();
    let timeout_tsc = if online_cpus > 8 { PREEMPT_TIMEOUT_TSC * 3 } else { PREEMPT_TIMEOUT_TSC };
    let bsp_pcpu = get_percpu_by_idx(0);
    start_cpu_timer_for_phase(bsp_pcpu);
    sti();
    while PHASE_PENDING_THREADS.load(Ordering::Acquire) > 0 {
        if !idle_dispatch_once(0) {
            core::hint::spin_loop();
        }
        if rdtsc().wrapping_sub(start) >= timeout_tsc {
            stop_cpu_timer_for_phase(bsp_pcpu);
            preempt_invariant_panic(format_args!(
                "Preemptive phase timeout: pending_threads={} preemptions={} timer_irqs={}",
                PHASE_PENDING_THREADS.load(Ordering::Acquire),
                GLOBAL_PREEMPTIONS.load(Ordering::Acquire),
                GLOBAL_TIMER_IRQS.load(Ordering::Acquire)
            ));
        }
    }
    stop_cpu_timer_for_phase(bsp_pcpu);
    disarm_epoch_watchdog();

    let ap_target = online_cpus - 1;
    while AP_PHASE_DONE_CPUS.load(Ordering::Acquire) < ap_target {
        assert!(
            rdtsc().wrapping_sub(start) < timeout_tsc,
            "Timed out waiting for AP preemptive idle convergence"
        );
        core::hint::spin_loop();
    }

    audit_quiescent_h1_invariants(online_cpus);
}

// =============================================================================
// Sub-Stage G2: `preempt_count` Nesting (`0 -> 1 -> 2 -> 1 -> 0`) + `need_resched`
// =============================================================================

fn nesting_leader_worker(_arg: u64) {
    assert_eq!(preempt_count(), 0);
    assert_ne!(read_rflags() & (1 << 9), 0, "Preemptible worker started with IF=0");

    preempt_disable();
    assert_eq!(preempt_count(), 1);
    preempt_disable();
    assert_eq!(preempt_count(), 2);

    let cpu = current_cpu_idx();
    let pcpu = get_percpu_by_idx(cpu);
    let sw_before = pcpu.ctx_switches.load(Ordering::Acquire);
    let irqs_before = pcpu.timer_irqs.load(Ordering::Acquire);
    let target_irqs = irqs_before + (QUANTUM_TICKS as u64) + 2;

    while pcpu.timer_irqs.load(Ordering::Acquire) < target_irqs {
        note_current_thread_progress();
        core::hint::spin_loop();
    }

    assert!(
        pcpu.need_resched.load(Ordering::Acquire),
        "Timer quantum expiry did not latch need_resched while preempt_count=2"
    );
    assert_eq!(
        pcpu.ctx_switches.load(Ordering::Acquire),
        sw_before,
        "Thread was illegally preempted while preempt_count=2"
    );

    // Inner `preempt_enable` (`2 -> 1`): must NOT trigger preemption yet!
    preempt_enable();
    assert_eq!(preempt_count(), 1);
    assert!(pcpu.need_resched.load(Ordering::Acquire));
    assert_eq!(
        pcpu.ctx_switches.load(Ordering::Acquire),
        sw_before,
        "Inner preempt_enable (2->1) prematurely preempted critical section"
    );

    // Outer `preempt_enable` (`1 -> 0`): MUST immediately honor `need_resched`!
    preempt_enable();
    assert_eq!(preempt_count(), 0);
    assert!(
        GLOBAL_DEFERRED_RESCHEDS.load(Ordering::Acquire) >= 1,
        "Outer preempt_enable (1->0) failed to deliver deferred resched"
    );
    NESTING_DONE.store(true, Ordering::Release);
}

fn nesting_peer_worker(_arg: u64) {
    while !NESTING_DONE.load(Ordering::Acquire) {
        note_current_thread_progress();
        yield_now();
    }
}

// =============================================================================
// Sub-Stage G3: Pure Non-Yielding Spinner Preemption (`0` `yield_now()` calls!)
// =============================================================================

fn non_yielding_spinner_worker(idx: u64) {
    let slot = current_slot_on_cpu(current_cpu_idx());
    let seed = 0xa5a5_f00d_0000_0000u64 ^ ((idx + 1).wrapping_mul(0x517c_c1b7_2722_0a95));
    let n_total = SPINNER_COUNT_TARGET.load(Ordering::Acquire);

    // Zero voluntary yields: `SPINNER_ARRIVED == n_total` (`n_total > online_cpus`) is reachable
    // ONLY if the Local APIC timer interrupt involuntarily preempts spinning threads!
    SPINNER_ARRIVED.fetch_add(1, Ordering::AcqRel);
    // SAFETY: Static atomic watch counter + owned TCB progress counter.
    let rc1 = unsafe {
        dry_irq_frame_torture(
            seed,
            n_total,
            SPINNER_ARRIVED.as_ptr(),
            tcb(slot).progress_seq.as_ptr(),
        )
    };
    assert_eq!(rc1, 0, "Spinner stage-1 GPR/RFLAGS/align check failed (rc={})", rc1);

    let goal = SPINNER_PREEMPT_GOAL.load(Ordering::Acquire);
    // SAFETY: Static `GLOBAL_PREEMPTIONS` watch counter + owned TCB progress counter.
    let rc2 = unsafe {
        dry_irq_frame_torture(
            seed ^ 0xdead_beef_cafe_babe,
            goal,
            GLOBAL_PREEMPTIONS.as_ptr(),
            tcb(slot).progress_seq.as_ptr(),
        )
    };
    assert_eq!(rc2, 0, "Spinner stage-2 GPR/RFLAGS/align check failed (rc={})", rc2);
}

// =============================================================================
// Sub-Stage G4: Mixed Preemptive Races (`IRQ` vs `yield` + `park`/`wake` + `exit`)
// =============================================================================

fn mixed_bg_spinner_worker(_arg: u64) {
    let online = RACE_ONLINE_CPUS.load(Ordering::Relaxed);
    let slot = current_slot_on_cpu(current_cpu_idx());
    while tcb(slot).preempted_count.load(Ordering::Relaxed) < 3 {
        let live = GLOBAL_SPAWNED_THREADS
            .load(Ordering::Acquire)
            .saturating_sub(GLOBAL_EXITED_THREADS.load(Ordering::Acquire));
        if RACE_ALL_SPAWNED.load(Ordering::Acquire) && live <= online {
            break;
        }
        note_current_thread_progress();
        core::hint::spin_loop();
    }
}

fn mixed_yield_worker(seed: u64) {
    let slot = current_slot_on_cpu(current_cpu_idx());
    let mut guard = 0xcafe_babe_1234_5678u64 ^ seed.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    for step in 0..RACE_YIELD_ITERS {
        let next_guard = guard.rotate_left(11) ^ step ^ (slot as u64);
        // Spin briefly with `IF=1` before voluntary `yield_now` so timer IRQs hit both windows.
        for _ in 0..128 {
            note_current_thread_progress();
            core::hint::spin_loop();
        }
        yield_now();
        guard = guard.rotate_left(11) ^ step ^ (slot as u64);
        assert_eq!(guard, next_guard, "Mixed preemptive yield register corruption");
    }
}

fn mixed_park_consumer_worker(pair_u64: u64) {
    let pair = pair_u64 as usize;
    let slot = current_slot_on_cpu(current_cpu_idx());
    let my_tcb = tcb(slot);

    for round in 0..RACE_PARK_ROUNDS {
        let bit = 1u64 << round;
        loop {
            // `RFLAGS.IF == 1` remains enabled right across `prepare_park -> consume -> commit_park`!
            prepare_park_current();
            let fired = my_tcb.notif.consume();
            if (fired & bit) != 0 {
                cancel_park_current();
                break;
            }
            let _ = commit_park_current();
        }
        RACE_PARK_ROUNDS_DONE.fetch_add(1, Ordering::AcqRel);
        RACE_CONSUMER_ACK[pair].store(round + 1, Ordering::Release);
    }
}

fn mixed_park_producer_worker(pair_u64: u64) {
    let pair = pair_u64 as usize;
    let target = ThreadId::from_raw(RACE_CONSUMER_TIDS[pair].load(Ordering::Acquire));

    for round in 0..RACE_PARK_ROUNDS {
        while RACE_CONSUMER_ACK[pair].load(Ordering::Acquire) < round {
            note_current_thread_progress();
            core::hint::spin_loop();
        }
        for _ in 0..((round % 3) * 64) {
            note_current_thread_progress();
            core::hint::spin_loop();
        }
        let bit = 1u64 << round;
        let outcome = notify_thread(target, bit, current_cpu_idx() + pair + 1);
        assert_ne!(outcome, WakeOutcome::Terminated, "Preemptive consumer unexpectedly Terminated");
    }

    while RACE_CONSUMER_ACK[pair].load(Ordering::Acquire) < RACE_PARK_ROUNDS {
        note_current_thread_progress();
        yield_now();
    }
}

fn mixed_recycle_exit_worker(idx_u64: u64) {
    let idx = idx_u64 as usize;
    for step in 0..RACE_RECYCLE_ITERS {
        for _ in 0..96 {
            note_current_thread_progress();
            core::hint::spin_loop();
        }
        if idx > 0 && step == 2 {
            let prev_raw = RACE_RECYCLE_TIDS[idx - 1].load(Ordering::Acquire);
            if prev_raw != 0 {
                let _ = wake_thread(ThreadId::from_raw(prev_raw), current_cpu_idx() + 1);
            }
        }
        if (step & 1) == 0 {
            yield_now();
        }
    }
}

// =============================================================================
// Negative Preemptive Fault Scenarios (`preempt_fault=*`)
// =============================================================================

fn storm_victim_worker(_arg: u64) {
    // Deliberately frozen `progress_seq` under pathological 1-tick (`~16ns`) APIC timer storm.
    loop {
        core::hint::spin_loop();
    }
}

fn broken_preempt_park_worker(_arg: u64) {
    prepare_park_current();
    // Simulate a buggy caller clobbering/cancelling `ParkToken::Armed` before `commit_park_current()`.
    cancel_park_current();
    let _ = commit_park_current();
}

fn stack_reuse_holder_worker(_arg: u64) {
    let slot = current_slot_on_cpu(current_cpu_idx());
    ACTIVE_REUSE_SLOT.store(slot as u64, Ordering::Release);
    loop {
        note_current_thread_progress();
        core::hint::spin_loop();
    }
}

fn stack_reuse_attacker_worker(_arg: u64) {
    while ACTIVE_REUSE_SLOT.load(Ordering::Acquire) == 0 {
        note_current_thread_progress();
        core::hint::spin_loop();
    }
    let victim_slot = ACTIVE_REUSE_SLOT.load(Ordering::Acquire) as usize;
    // Illegal premature stack/TCB reuse while `victim_slot` still owns its active stack!
    let _ = tcb(victim_slot).sched.init_spawned_ready(victim_slot, 99);
}

fn run_negative_preempt_fault(online_cpus: usize, fault: PreemptFaultTest) -> ! {
    init_scheduler(online_cpus);
    set_preemptive_mode_active(true);
    AP_SCHED_RELEASE.store(true, Ordering::Release);

    match fault {
        PreemptFaultTest::IrqStorm => {
            set_active_tick_init_override(1);
            begin_preemptive_epoch(9, 1);
            let _ = spawn_preemptible_with_assist(1, 0, storm_victim_worker, 0);
            drain_preemptive_epoch_and_audit(online_cpus);
        }
        PreemptFaultTest::NestedIrq => {
            set_nested_irq_fault_inject(true);
            begin_preemptive_epoch(9, 1);
            let _ = spawn_preemptible_with_assist(1, 0, storm_victim_worker, 0);
            drain_preemptive_epoch_and_audit(online_cpus);
        }
        PreemptFaultTest::LostWakeup => {
            begin_preemptive_epoch(9, 1);
            let _ = spawn_preemptible_with_assist(1, 1 % online_cpus, broken_preempt_park_worker, 0);
            drain_preemptive_epoch_and_audit(online_cpus);
        }
        PreemptFaultTest::DoubleSched => {
            let tid = try_spawn_kernel_thread_ex(1, 0, mixed_yield_worker, 0, true)
                .expect("spawn for double_sched");
            // `tid` is already queued (`in_flight_refs == 1`); attempting a second enqueue must trap!
            tcb(tid.slot()).sched.claim_enqueue_ref(tid.slot());
        }
        PreemptFaultTest::StackReuse => {
            ACTIVE_REUSE_SLOT.store(0, Ordering::Release);
            begin_preemptive_epoch(9, 2);
            let _ = spawn_preemptible_with_assist(1, 0, stack_reuse_holder_worker, 0);
            let _ = spawn_preemptible_with_assist(1, 1 % online_cpus, stack_reuse_attacker_worker, 0);
            drain_preemptive_epoch_and_audit(online_cpus);
        }
        PreemptFaultTest::None => unreachable!(),
    }
    panic!("Negative preemptive scheduler test failed to trap as expected");
}

/// Run Milestone 1A-G incremental verification (`Sub-Stage G1` IRQ-only -> `G2..G4` Preemptive).
pub fn verify_preemptive_scheduler(
    online_cpus: usize,
    irq_only_stage: bool,
    fault: PreemptFaultTest,
) {
    restore_calibrated_tick_init();
    set_nested_irq_fault_inject(false);
    set_preemptive_mode_active(false);
    GLOBAL_TIMER_IRQS.store(0, Ordering::Release);

    // -------------------------------------------------------------------------
    // Sub-Stage G1: IRQ-only delivery, EOI (`ISR[0xEF]==0`), and `TrapFrame` check
    // -------------------------------------------------------------------------
    AP_SCHED_RELEASE.store(true, Ordering::Release);
    set_ap_epoch_kind(ApEpochKind::IrqOnlyVerify);
    AP_PHASE_DONE_CPUS.store(0, Ordering::Release);
    AP_PHASE_EPOCH.fetch_add(1, Ordering::AcqRel);

    run_cpu_irq_only_torture(0);

    let start = rdtsc();
    while AP_PHASE_DONE_CPUS.load(Ordering::Acquire) < (online_cpus - 1) {
        assert!(
            rdtsc().wrapping_sub(start) < PREEMPT_TIMEOUT_TSC,
            "Timed out waiting for AP IRQ-only verification"
        );
        core::hint::spin_loop();
    }

    let mut min_cpu_irqs = u64::MAX;
    let mut total_irq_only = 0u64;
    for cpu in 0..online_cpus {
        let count = get_percpu_by_idx(cpu).timer_irqs.load(Ordering::Acquire);
        min_cpu_irqs = min_cpu_irqs.min(count);
        total_irq_only += count;
    }
    serial_println!(
        "[DRY-IRQ-ONLY] PASS: cpus={} total_timer_irqs={} min_cpu_irqs={} eoi_verified=true trapframe_gpr_rflags_ok=true spurious_irqs={}",
        online_cpus,
        total_irq_only,
        min_cpu_irqs,
        spurious_apic_irqs()
    );

    if irq_only_stage {
        return;
    }

    if fault != PreemptFaultTest::None {
        run_negative_preempt_fault(online_cpus, fault);
    }

    // -------------------------------------------------------------------------
    // Sub-Stage G2: `preempt_count` nesting (`0 -> 1 -> 2 -> 1 -> 0`) + `need_resched`
    // -------------------------------------------------------------------------
    init_scheduler(online_cpus);
    set_preemptive_mode_active(true);
    NESTING_DONE.store(false, Ordering::Release);
    begin_preemptive_epoch(2, 2);
    let _ = spawn_preemptible_with_assist(1, 0, nesting_leader_worker, 0);
    let _ = spawn_preemptible_with_assist(1, 0, nesting_peer_worker, 0);
    drain_preemptive_epoch_and_audit(online_cpus);
    let deferred_after_g2 = GLOBAL_DEFERRED_RESCHEDS.load(Ordering::Acquire);
    assert!(deferred_after_g2 >= 1, "Expected >= 1 deferred resched in G2");

    // -------------------------------------------------------------------------
    // Sub-Stage G3: Non-yielding spinner preemption (`0` `yield_now()` calls)
    // -------------------------------------------------------------------------
    let n_spinners = (online_cpus * 2).clamp(4, 32);
    SPINNER_ARRIVED.store(0, Ordering::Release);
    SPINNER_COUNT_TARGET.store(n_spinners as u64, Ordering::Release);
    let p_before_g3 = GLOBAL_PREEMPTIONS.load(Ordering::Acquire);
    SPINNER_PREEMPT_GOAL.store(p_before_g3 + (n_spinners as u64), Ordering::Release);

    begin_preemptive_epoch(3, n_spinners);
    for i in 0..n_spinners {
        let _ = spawn_preemptible_with_assist(1, i % online_cpus, non_yielding_spinner_worker, i as u64);
    }
    drain_preemptive_epoch_and_audit(online_cpus);
    let spinner_preempts = GLOBAL_PREEMPTIONS
        .load(Ordering::Acquire)
        .saturating_sub(p_before_g3);
    assert!(
        spinner_preempts >= (n_spinners as u64),
        "Expected >= {} involuntary spinner preemptions, got {}",
        n_spinners,
        spinner_preempts
    );

    // -------------------------------------------------------------------------
    // Sub-Stage G4: Mixed Preemptive Race (`IRQ` vs `yield` + `park`/`wake` + `exit`/recycle)
    // -------------------------------------------------------------------------
    init_scheduler(online_cpus);
    set_preemptive_mode_active(true);
    RACE_ONLINE_CPUS.store(online_cpus as u64, Ordering::Release);
    RACE_ALL_SPAWNED.store(false, Ordering::Release);
    RACE_PARK_ROUNDS_DONE.store(0, Ordering::Release);

    let n_bg = online_cpus.clamp(2, 4);
    let n_yield = online_cpus.clamp(4, 6);
    let n_pairs = online_cpus.clamp(3, 5);
    let total_race_threads = n_bg + n_yield + (2 * n_pairs) + RACE_RECYCLE_THREADS;

    for p in 0..n_pairs {
        RACE_CONSUMER_ACK[p].store(0, Ordering::Release);
        RACE_CONSUMER_TIDS[p].store(0, Ordering::Release);
    }
    for slot in &RACE_RECYCLE_TIDS {
        slot.store(0, Ordering::Release);
    }

    begin_preemptive_epoch(4, total_race_threads);
    for i in 0..n_bg {
        let _ = spawn_preemptible_with_assist(1, i % online_cpus, mixed_bg_spinner_worker, i as u64);
    }
    for i in 0..n_yield {
        let _ = spawn_preemptible_with_assist(1, i % online_cpus, mixed_yield_worker, i as u64);
    }
    for (p, c_slot) in RACE_CONSUMER_TIDS.iter().enumerate().take(n_pairs) {
        let cid = spawn_preemptible_with_assist(1, p % online_cpus, mixed_park_consumer_worker, p as u64);
        c_slot.store(cid.raw(), Ordering::Release);
        let _ = spawn_preemptible_with_assist(1, (p + 1) % online_cpus, mixed_park_producer_worker, p as u64);
    }
    for (i, r_slot) in RACE_RECYCLE_TIDS.iter().enumerate() {
        let tid = spawn_preemptible_with_assist(1, i % online_cpus, mixed_recycle_exit_worker, i as u64);
        r_slot.store(tid.raw(), Ordering::Release);
    }
    RACE_ALL_SPAWNED.store(true, Ordering::Release);
    drain_preemptive_epoch_and_audit(online_cpus);

    let mut stale_rejected = 0u64;
    for r_slot in &RACE_RECYCLE_TIDS {
        let old_tid = ThreadId::from_raw(r_slot.load(Ordering::Acquire));
        assert_eq!(wake_thread(old_tid, 0), WakeOutcome::Terminated);
        stale_rejected += 1;
    }
    set_preemptive_mode_active(false);

    let race_spawned = GLOBAL_SPAWNED_THREADS.load(Ordering::Acquire);
    let race_exited = GLOBAL_EXITED_THREADS.load(Ordering::Acquire);
    let race_recycled = GLOBAL_RECYCLED_SLOTS.load(Ordering::Acquire);
    let race_switches = total_ctx_switches();
    let race_migrations = GLOBAL_MIGRATIONS.load(Ordering::Acquire);
    let race_park_rounds = RACE_PARK_ROUNDS_DONE.load(Ordering::Acquire);
    let total_preemptions = spinner_preempts + GLOBAL_PREEMPTIONS.load(Ordering::Acquire) + deferred_after_g2;
    let total_timer_irqs = GLOBAL_TIMER_IRQS.load(Ordering::Acquire);

    assert_eq!(race_spawned, race_exited);
    assert_eq!(race_park_rounds, (n_pairs as u64) * RACE_PARK_ROUNDS);
    assert!(race_recycled >= 12, "Expected >= 12 recycled slots in preemptive race");

    serial_println!(
        "[DRY-PREEMPT] PASS: cpus={} mode={} timer_irqs={} preemptions={} deferred_rescheds={} spinner_threads={} spinner_preempts={} race_spawned={} race_exited={} race_recycled={} race_switches={} race_migrations={} race_park_rounds={} stale_wakes_rejected={} orphan_ready=0",
        online_cpus,
        active_apic_mode().as_str(),
        total_timer_irqs,
        total_preemptions,
        deferred_after_g2 + GLOBAL_DEFERRED_RESCHEDS.load(Ordering::Acquire),
        n_spinners,
        spinner_preempts,
        race_spawned,
        race_exited,
        race_recycled,
        race_switches,
        race_migrations,
        race_park_rounds,
        stale_rejected,
    );
}
