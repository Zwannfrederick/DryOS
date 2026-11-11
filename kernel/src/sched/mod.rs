// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `H1` Kernel-Thread Scheduler (`ADR-004`, Milestone 1A-F Cooperative & 1A-G Preemptive).
//!
//! Provides `BKL`-free per-CPU runqueues, independent 16 KiB kernel thread stacks with generation-
//! stamped bottom canaries, x86-64 assembly `dry_switch_to` / `dry_thread_entry_trampoline`,
//! strict `post_switch_*` stack-ownership handoff, `Notification` park/wake, `Dead` slot
//! reclamation, `preempt_count` nesting with `need_resched`, and calibrated Local APIC One-Shot
//! timer preemption (`IST = 0`, `EOI` prior to switch).
//!
//! **Explicit Milestone 1A-G Scope Boundary:**
//! - Shared kernel `VSpace` (`CR3` / TLB switching and Ring 3 userspace belong to Milestone 1B).

pub mod context;
pub mod preempt;
pub mod preempt_tests;
pub mod runqueue;
pub mod state;
pub mod tests;

pub use preempt::{
    arm_epoch_watchdog, disarm_epoch_watchdog, is_preemptive_mode_active,
    note_current_thread_progress, on_lapic_timer_irq, preempt_count, preempt_disable,
    preempt_enable, set_nested_irq_fault_inject, set_preemptive_mode_active,
    start_cpu_timer_for_phase, stop_cpu_timer_for_phase,
};

use core::cell::UnsafeCell;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicU8, AtomicUsize, Ordering};

use crate::apic::QUANTUM_TICKS;
use crate::arch::{cli, irq_restore, irq_save_cli, read_rsp, sti};
use crate::gdt_tss::{set_cpu_tss_rsp0, verify_cpu_tss_and_canaries, MAX_CPUS};
use crate::percpu::{current_percpu, get_percpu_by_idx, read_gs_u64_at_8};
use crate::sched::context::{
    dry_switch_to, init_new_thread_stack, KernelThreadFn, PostSwitchKind,
};
use crate::sched::runqueue::{cpu_rq, pop_local_or_steal};
use crate::sched::state::{
    preempt_invariant_panic, sched_invariant_panic, CommitParkOutcome, ExecState, Notification,
    TaskSchedState, ThreadId, WakeOutcome, GEN_MAX, NO_CPU,
};
use crate::vspace::{switch_cpu_vspace_if_needed, VSpaceId, KERNEL_VSPACE_ID};

pub const THREAD_STACK_BYTES: usize = 16 * 1024;
pub const WORKER_POOL_CAP: usize = 36;
pub const TOTAL_TCB_SLOTS: usize = MAX_CPUS + WORKER_POOL_CAP;

const WORKER_CANARY_MAGIC: u64 = 0x4452_595f_5448_5354; // "DRY_THST"

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ApEpochKind {
    Cooperative = 0,
    IrqOnlyVerify = 1,
    PreemptiveSched = 2,
    KernelVSpaceSync = 3,
}

impl ApEpochKind {
    #[inline]
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::IrqOnlyVerify,
            2 => Self::PreemptiveSched,
            3 => Self::KernelVSpaceSync,
            _ => Self::Cooperative,
        }
    }
}

#[repr(C, align(64))]
struct WorkerStack([u8; THREAD_STACK_BYTES]);

struct WorkerStackPool(UnsafeCell<[WorkerStack; WORKER_POOL_CAP]>);
// SAFETY: Stack `idx` is accessed solely by the CPU holding `TCB_TABLE[MAX_CPUS + idx].sched.stack_owner`,
// or during single-owner initialization while `slot_free == false` prior to `Ready` publication.
unsafe impl Sync for WorkerStackPool {}

static WORKER_STACKS: WorkerStackPool =
    WorkerStackPool(UnsafeCell::new([const { WorkerStack([0; THREAD_STACK_BYTES]) }; WORKER_POOL_CAP]));

#[repr(C, align(64))]
pub struct KernelTcb {
    pub sched: TaskSchedState,
    saved_rsp: UnsafeCell<u64>,
    pub stack_base: AtomicU64,
    pub stack_top: AtomicU64,
    pub prio: AtomicUsize,
    pub last_cpu: AtomicUsize,
    pub alloc_count: AtomicU64,
    pub progress_seq: AtomicU64,
    pub preempted_count: AtomicU64,
    pub vspace_id: AtomicU16,
    pub slot_free: AtomicBool,
    pub notif: Notification,
}

// SAFETY: `saved_rsp` is mutated exclusively while the calling CPU holds `sched.stack_owner`.
unsafe impl Sync for KernelTcb {}

impl KernelTcb {
    const fn uninit() -> Self {
        Self {
            sched: TaskSchedState::new_dead(),
            saved_rsp: UnsafeCell::new(0),
            stack_base: AtomicU64::new(0),
            stack_top: AtomicU64::new(0),
            prio: AtomicUsize::new(0),
            last_cpu: AtomicUsize::new(NO_CPU),
            alloc_count: AtomicU64::new(0),
            progress_seq: AtomicU64::new(0),
            preempted_count: AtomicU64::new(0),
            vspace_id: AtomicU16::new(KERNEL_VSPACE_ID),
            slot_free: AtomicBool::new(false),
            notif: Notification::new(),
        }
    }
}

#[allow(clippy::declare_interior_mutable_const)]
const UNINIT_TCB: KernelTcb = KernelTcb::uninit();
static TCB_TABLE: [KernelTcb; TOTAL_TCB_SLOTS] = [UNINIT_TCB; TOTAL_TCB_SLOTS];

pub(crate) static ONLINE_SCHED_CPUS: AtomicUsize = AtomicUsize::new(1);
pub static GLOBAL_SPAWNED_THREADS: AtomicU64 = AtomicU64::new(0);
pub static GLOBAL_EXITED_THREADS: AtomicU64 = AtomicU64::new(0);
pub static GLOBAL_RECYCLED_SLOTS: AtomicU64 = AtomicU64::new(0);
pub static GLOBAL_CTX_SWITCHES: AtomicU64 = AtomicU64::new(0);
pub static GLOBAL_MIGRATIONS: AtomicU64 = AtomicU64::new(0);
pub static GLOBAL_TIMER_IRQS: AtomicU64 = AtomicU64::new(0);
pub static GLOBAL_PREEMPTIONS: AtomicU64 = AtomicU64::new(0);
pub static GLOBAL_DEFERRED_RESCHEDS: AtomicU64 = AtomicU64::new(0);
pub static PHASE_PENDING_THREADS: AtomicUsize = AtomicUsize::new(0);

pub static AP_SCHED_RELEASE: AtomicBool = AtomicBool::new(false);
static AP_SCHED_SHUTDOWN: AtomicBool = AtomicBool::new(false);
static AP_EPOCH_KIND: AtomicU8 = AtomicU8::new(ApEpochKind::Cooperative as u8);
pub static AP_PHASE_EPOCH: AtomicUsize = AtomicUsize::new(0);
pub static AP_PHASE_DONE_CPUS: AtomicUsize = AtomicUsize::new(0);

#[inline]
pub fn tcb(slot: usize) -> &'static KernelTcb {
    &TCB_TABLE[slot]
}

#[inline]
pub fn current_cpu_idx() -> usize {
    read_gs_u64_at_8() as usize
}

#[inline]
pub fn current_slot_on_cpu(cpu_idx: usize) -> usize {
    get_percpu_by_idx(cpu_idx).current_slot.load(Ordering::Relaxed) as usize
}

#[inline]
pub fn current_thread_id() -> ThreadId {
    let cpu = current_cpu_idx();
    let slot = current_slot_on_cpu(cpu);
    let (_, _, gen26) = tcb(slot).sched.snapshot();
    ThreadId::new(slot, gen26)
}

#[inline]
pub fn set_ap_epoch_kind(kind: ApEpochKind) {
    AP_EPOCH_KIND.store(kind as u8, Ordering::Release);
}

#[inline]
pub fn shutdown_ap_scheduler_service() {
    AP_SCHED_SHUTDOWN.store(true, Ordering::Release);
}

#[inline]
fn worker_canary_value(slot: usize, gen26: u32) -> u64 {
    WORKER_CANARY_MAGIC ^ ((slot as u64) << 32) ^ (gen26 as u64)
}

pub fn verify_slot_canary(slot: usize) {
    if slot < MAX_CPUS {
        verify_cpu_tss_and_canaries(slot);
    } else {
        let base = tcb(slot).stack_base.load(Ordering::Acquire);
        let (_, _, gen26) = tcb(slot).sched.snapshot();
        // SAFETY: Bottom 8 bytes of `'static` worker stack hold the generation-tagged canary.
        let val = unsafe { read_volatile(base as *const u64) };
        assert_eq!(
            val,
            worker_canary_value(slot, gen26),
            "Worker stack canary corrupted on slot={} gen={}",
            slot,
            gen26
        );
    }
}

/// Initialize per-CPU Boot/Idle TCBs (`0..online_cpus`) and 36 independent worker TCB/stack slots.
pub fn init_scheduler(online_cpus: usize) {
    ONLINE_SCHED_CPUS.store(online_cpus, Ordering::Release);
    GLOBAL_SPAWNED_THREADS.store(0, Ordering::Release);
    GLOBAL_EXITED_THREADS.store(0, Ordering::Release);
    GLOBAL_RECYCLED_SLOTS.store(0, Ordering::Release);
    GLOBAL_CTX_SWITCHES.store(0, Ordering::Release);
    GLOBAL_MIGRATIONS.store(0, Ordering::Release);
    GLOBAL_PREEMPTIONS.store(0, Ordering::Release);
    GLOBAL_DEFERRED_RESCHEDS.store(0, Ordering::Release);
    PHASE_PENDING_THREADS.store(0, Ordering::Release);

    for cpu in 0..online_cpus {
        cpu_rq(cpu).reset();
        let pcpu = get_percpu_by_idx(cpu);
        pcpu.current_slot.store(cpu as u64, Ordering::Release);
        pcpu.ctx_switches.store(0, Ordering::Release);
        pcpu.preemptions.store(0, Ordering::Release);
        pcpu.deferred_rescheds.store(0, Ordering::Release);
        pcpu.preempt_count.store(0, Ordering::Release);
        pcpu.irq_depth.store(0, Ordering::Release);
        pcpu.need_resched.store(false, Ordering::Release);

        let idle = tcb(cpu);
        idle.stack_base.store(pcpu.boot_stack_base, Ordering::Release);
        idle.stack_top.store(pcpu.boot_stack_top, Ordering::Release);
        idle.prio.store(3, Ordering::Release);
        idle.last_cpu.store(cpu, Ordering::Release);
        idle.vspace_id.store(KERNEL_VSPACE_ID, Ordering::Release);
        idle.slot_free.store(false, Ordering::Release);
        idle.sched.init_idle_running(cpu);
    }

    for idx in 0..WORKER_POOL_CAP {
        let slot = MAX_CPUS + idx;
        let w = tcb(slot);
        let (_, _, cur_gen) = w.sched.snapshot();
        // SAFETY: Single-threaded BSP initialization at quiescent phase boundary (`CLI`).
        let (base, top) = unsafe {
            let stk_ptr = (*WORKER_STACKS.0.get())[idx].0.as_mut_ptr();
            let b = stk_ptr as u64;
            let t = b + (THREAD_STACK_BYTES as u64);
            write_volatile(b as *mut u64, worker_canary_value(slot, cur_gen));
            (b, t)
        };
        w.stack_base.store(base, Ordering::Release);
        w.stack_top.store(top, Ordering::Release);
        w.prio.store(1, Ordering::Release);
        w.last_cpu.store(NO_CPU, Ordering::Release);
        w.alloc_count.store(0, Ordering::Release);
        w.progress_seq.store(0, Ordering::Release);
        w.preempted_count.store(0, Ordering::Release);
        w.vspace_id.store(KERNEL_VSPACE_ID, Ordering::Release);
        w.notif.reset();
        w.slot_free.store(true, Ordering::Release);
    }
}

pub fn try_spawn_kernel_thread(
    prio: usize,
    target_cpu: usize,
    entry: KernelThreadFn,
    arg: u64,
) -> Option<ThreadId> {
    try_spawn_kernel_thread_ex(prio, target_cpu, entry, arg, false)
}

/// Allocate a `Dead` worker TCB + 16 KiB stack, advance its `gen26`, initialize its `SwitchFrame`,
/// and publish it as `ExecState::Ready` onto `target_cpu`'s `PerCpuRunQueue`.
pub fn try_spawn_kernel_thread_ex(
    prio: usize,
    target_cpu: usize,
    entry: KernelThreadFn,
    arg: u64,
    preemptible: bool,
) -> Option<ThreadId> {
    try_spawn_thread_in_vspace(prio, target_cpu, KERNEL_VSPACE_ID, entry, arg, preemptible)
}

/// Allocate a `Dead` worker TCB + 16 KiB stack bound to `vspace_id`, advance its `gen26`,
/// initialize its `SwitchFrame`, and publish it as `ExecState::Ready` onto `target_cpu`'s runqueue.
pub fn try_spawn_thread_in_vspace(
    prio: usize,
    target_cpu: usize,
    vspace_id: VSpaceId,
    entry: KernelThreadFn,
    arg: u64,
    preemptible: bool,
) -> Option<ThreadId> {
    let saved_if = irq_save_cli();
    let online = ONLINE_SCHED_CPUS.load(Ordering::Acquire);
    let dst_cpu = target_cpu % online;
    let mut spawned_tid = None;

    for idx in 0..WORKER_POOL_CAP {
        let slot = MAX_CPUS + idx;
        let w = tcb(slot);
        if w.slot_free
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            continue;
        }

        let (prev_state, _, prev_gen) = w.sched.snapshot();
        assert_eq!(prev_state, ExecState::Dead, "Allocated non-Dead slot {}", slot);
        let next_gen = if prev_gen >= GEN_MAX { 1 } else { prev_gen + 1 };

        let prev_allocs = w.alloc_count.fetch_add(1, Ordering::AcqRel);
        if prev_allocs >= 1 {
            GLOBAL_RECYCLED_SLOTS.fetch_add(1, Ordering::AcqRel);
        }

        let base = w.stack_base.load(Ordering::Acquire);
        let top = w.stack_top.load(Ordering::Acquire);
        w.prio.store(prio, Ordering::Release);
        w.last_cpu.store(NO_CPU, Ordering::Release);
        w.progress_seq.store(0, Ordering::Release);
        w.preempted_count.store(0, Ordering::Release);
        w.vspace_id.store(vspace_id, Ordering::Release);
        w.notif.reset();

        // SAFETY: `slot_free` CAS + `Dead` state + `stack_owner == NO_CPU` guarantee exclusive stack access.
        unsafe {
            write_volatile(base as *mut u64, worker_canary_value(slot, next_gen));
            let init_rsp = init_new_thread_stack(top, slot, entry, arg, preemptible);
            *w.saved_rsp.get() = init_rsp;
        }

        let tid = w.sched.init_spawned_ready(slot, next_gen);
        GLOBAL_SPAWNED_THREADS.fetch_add(1, Ordering::AcqRel);
        assert!(
            cpu_rq(dst_cpu).push(prio, tid.raw()),
            "PerCpuRunQueue push failed on spawn"
        );
        spawned_tid = Some(tid);
        break;
    }

    irq_restore(saved_if);
    spawned_tid
}

#[inline]
pub(crate) fn record_worker_dispatch(slot: usize, cpu_idx: usize) {
    let prev_cpu = tcb(slot).last_cpu.swap(cpu_idx, Ordering::AcqRel);
    if prev_cpu != NO_CPU && prev_cpu != cpu_idx {
        GLOBAL_MIGRATIONS.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn switch_to_slot(
    cpu_idx: usize,
    prev_slot: usize,
    next_slot: usize,
    post_kind: PostSwitchKind,
) {
    debug_assert_ne!(prev_slot, next_slot, "Attempted self-switch on slot {}", prev_slot);
    let prev_tcb = tcb(prev_slot);
    let next_tcb = tcb(next_slot);

    let next_top = next_tcb.stack_top.load(Ordering::Relaxed);
    set_cpu_tss_rsp0(cpu_idx, next_top);
    get_percpu_by_idx(cpu_idx)
        .current_slot
        .store(next_slot as u64, Ordering::Release);

    // SAFETY: `prev_slot` owns `prev_tcb.saved_rsp` on `cpu_idx`, and `next_slot` was already
    // claimed (`stack_owner == cpu_idx`) prior to loading `next_tcb.saved_rsp`.
    unsafe {
        let prev_rsp_ptr = prev_tcb.saved_rsp.get();
        let next_rsp = *next_tcb.saved_rsp.get();
        dry_switch_to(prev_rsp_ptr, next_rsp, prev_slot, post_kind.pack());
    }
}

/// Invoked from `dry_switch_to` assembly immediately AFTER `mov rsp, rsi` switched onto `next`'s
/// stack while `RFLAGS.IF == 0` (`CLI`) is unconditionally held.
pub(crate) fn complete_post_switch_from_asm(prev_slot: usize, post_kind: PostSwitchKind) {
    let cpu_idx = current_cpu_idx();
    let pcpu = get_percpu_by_idx(cpu_idx);
    let cur_slot = pcpu.current_slot.load(Ordering::Relaxed) as usize;
    let rsp = read_rsp();

    let cur_base = tcb(cur_slot).stack_base.load(Ordering::Relaxed);
    let cur_top = tcb(cur_slot).stack_top.load(Ordering::Relaxed);
    if rsp <= cur_base || rsp > cur_top {
        sched_invariant_panic(format_args!(
            "H1 Bug1 RSP {:#x} outside active slot={} stack [{:#x}, {:#x}] on cpu={}",
            rsp, cur_slot, cur_base, cur_top, cpu_idx
        ));
    }

    let prev_base = tcb(prev_slot).stack_base.load(Ordering::Relaxed);
    let prev_top = tcb(prev_slot).stack_top.load(Ordering::Relaxed);
    if rsp > prev_base && rsp <= prev_top {
        sched_invariant_panic(format_args!(
            "H1 Bug1 RSP {:#x} still inside outgoing slot={} stack [{:#x}, {:#x}]",
            rsp, prev_slot, prev_base, prev_top
        ));
    }

    verify_slot_canary(prev_slot);
    pcpu.ctx_switches.fetch_add(1, Ordering::Relaxed);
    GLOBAL_CTX_SWITCHES.fetch_add(1, Ordering::Relaxed);

    // Switch hardware `CR3` (if `next`'s `vspace_id` differs from CPU's active `VSpace`) *before*
    // reaping `prev_slot` or decrementing `PHASE_PENDING_THREADS`, so exiting user-VSpace workers
    // always vacate `VSpace.active_cpus` prior to epoch completion.
    let next_vs = tcb(cur_slot).vspace_id.load(Ordering::Acquire);
    switch_cpu_vspace_if_needed(cpu_idx, next_vs);

    let prev = tcb(prev_slot);
    match post_kind {
        PostSwitchKind::FromIdle => {
            prev.sched.release_stack(prev_slot, cpu_idx);
        }
        PostSwitchKind::Yield { target_cpu } => {
            let prio = prev.prio.load(Ordering::Relaxed);
            prev.sched
                .post_switch_yield(prev_slot, cpu_idx, prio, cpu_rq(target_cpu));
        }
        PostSwitchKind::Park { target_cpu } => {
            let prio = prev.prio.load(Ordering::Relaxed);
            prev.sched
                .post_switch_park(prev_slot, cpu_idx, prio, cpu_rq(target_cpu));
        }
        PostSwitchKind::Exit => {
            let _ = prev.sched.post_switch_exit_and_reap(prev_slot, cpu_idx);
            prev.slot_free.store(true, Ordering::Release);
            GLOBAL_EXITED_THREADS.fetch_add(1, Ordering::AcqRel);
            PHASE_PENDING_THREADS.fetch_sub(1, Ordering::AcqRel);
        }
    }

    // Zero-instruction IRQ livelock guard (`HANDOVER.md` Rule 3): reset quantum and re-arm the
    // One-Shot Local APIC timer right before `popfq`/`iretq` grants `next` the CPU.
    if pcpu.timer_armed.load(Ordering::Relaxed) {
        pcpu.quantum_rem.store(QUANTUM_TICKS, Ordering::Relaxed);
        pcpu.storm_slot.store(cur_slot as u64, Ordering::Relaxed);
        pcpu.last_progress_seq
            .store(tcb(cur_slot).progress_seq.load(Ordering::Relaxed), Ordering::Relaxed);
        pcpu.storm_streak.store(0, Ordering::Relaxed);
        crate::apic::arm_default_oneshot_tick();
    }
}

pub(crate) fn verify_current_thread_entry_bounds(expected_slot: usize) {
    let cpu_idx = current_cpu_idx();
    let cur_slot = current_slot_on_cpu(cpu_idx);
    assert_eq!(cur_slot, expected_slot, "Bootstrap slot mismatch");
    assert_eq!(tcb(cur_slot).sched.stack_owner(), cpu_idx, "Bootstrap stack_owner mismatch");
    let rsp = read_rsp();
    let base = tcb(cur_slot).stack_base.load(Ordering::Relaxed);
    let top = tcb(cur_slot).stack_top.load(Ordering::Relaxed);
    assert!(rsp > base && rsp <= top, "Bootstrap RSP out of owned stack bounds");
    verify_slot_canary(cur_slot);
}

fn select_next_slot_for_worker(cpu_idx: usize, online_cpus: usize) -> usize {
    if let Some(raw_ticket) = pop_local_or_steal(cpu_idx, online_cpus) {
        let ticket = ThreadId::from_raw(raw_ticket);
        let next_slot = ticket.slot();
        tcb(next_slot).sched.dispatch_popped_ticket(ticket, cpu_idx);
        record_worker_dispatch(next_slot, cpu_idx);
        next_slot
    } else {
        let idle_slot = cpu_idx;
        tcb(idle_slot).sched.claim_idle_stack(idle_slot, cpu_idx);
        idle_slot
    }
}

#[inline]
fn assert_thread_context_schedulable(op: &'static str) {
    let pcpu = current_percpu();
    if pcpu.irq_depth.load(Ordering::Relaxed) != 0 {
        preempt_invariant_panic(format_args!(
            "Illegal {} inside IRQ context on cpu={}",
            op, pcpu.cpu_idx
        ));
    }
    if pcpu.preempt_count.load(Ordering::Relaxed) != 0 {
        preempt_invariant_panic(format_args!(
            "Double scheduling / illegal {} while preempt_count={} on cpu={}",
            op,
            pcpu.preempt_count.load(Ordering::Relaxed),
            pcpu.cpu_idx
        ));
    }
}

/// Voluntary `yield_now()` from an active worker thread (`slot >= MAX_CPUS`).
pub fn yield_now() {
    assert_thread_context_schedulable("yield_now");
    let saved_if = irq_save_cli();
    let cpu_idx = current_cpu_idx();
    let cur_slot = current_slot_on_cpu(cpu_idx);
    assert!(cur_slot >= MAX_CPUS, "yield_now called from idle context");
    tcb(cur_slot).progress_seq.fetch_add(1, Ordering::Relaxed);
    let online = ONLINE_SCHED_CPUS.load(Ordering::Relaxed);
    let target_cpu = (cpu_idx + 1) % online;

    tcb(cur_slot).sched.begin_yield(cur_slot, cpu_idx);
    let next_slot = select_next_slot_for_worker(cpu_idx, online);
    switch_to_slot(cpu_idx, cur_slot, next_slot, PostSwitchKind::Yield { target_cpu });

    let resumed_cpu = current_cpu_idx();
    assert_eq!(current_slot_on_cpu(resumed_cpu), cur_slot);
    assert_eq!(tcb(cur_slot).sched.stack_owner(), resumed_cpu);
    irq_restore(saved_if);
}

/// Step 1 of `H1` sleep protocol on current worker thread.
pub fn prepare_park_current() {
    assert_thread_context_schedulable("prepare_park");
    let cpu_idx = current_cpu_idx();
    let cur_slot = current_slot_on_cpu(cpu_idx);
    tcb(cur_slot).progress_seq.fetch_add(1, Ordering::Relaxed);
    tcb(cur_slot).sched.prepare_park();
}

pub fn cancel_park_current() {
    let cpu_idx = current_cpu_idx();
    let cur_slot = current_slot_on_cpu(cpu_idx);
    tcb(cur_slot).sched.cancel_park();
}

/// Step 3 of `H1` sleep protocol: either consumes `Notified` token in-place or switches off stack.
pub fn commit_park_current() -> CommitParkOutcome {
    assert_thread_context_schedulable("commit_park");
    let saved_if = irq_save_cli();
    let cpu_idx = current_cpu_idx();
    let cur_slot = current_slot_on_cpu(cpu_idx);
    tcb(cur_slot).progress_seq.fetch_add(1, Ordering::Relaxed);
    let outcome = tcb(cur_slot).sched.commit_park(cur_slot, cpu_idx);
    if outcome == CommitParkOutcome::SwitchToPark {
        let online = ONLINE_SCHED_CPUS.load(Ordering::Relaxed);
        let target_cpu = (cpu_idx + 1) % online;
        let next_slot = select_next_slot_for_worker(cpu_idx, online);
        switch_to_slot(cpu_idx, cur_slot, next_slot, PostSwitchKind::Park { target_cpu });
        let resumed_cpu = current_cpu_idx();
        assert_eq!(current_slot_on_cpu(resumed_cpu), cur_slot);
        assert_eq!(tcb(cur_slot).sched.stack_owner(), resumed_cpu);
    }
    irq_restore(saved_if);
    outcome
}

/// Signal `bits` on `target`'s TCB-bound `Notification` and wake `target` (`H1` doorbell).
pub fn notify_thread(target: ThreadId, bits: u64, preferred_cpu: usize) -> WakeOutcome {
    let slot = target.slot();
    assert!((MAX_CPUS..TOTAL_TCB_SLOTS).contains(&slot), "Invalid notify slot {}", slot);
    let w = tcb(slot);
    let (_, _, cur_gen) = w.sched.snapshot();
    if cur_gen == target.gen26() {
        w.notif.signal_raw(bits);
    }
    wake_thread(target, preferred_cpu)
}

/// Wake `target` (`Blocked -> Waking -> Ready + rq.push`, or deposit `ParkToken::Notified`).
///
/// Masks local interrupts (`irq_save_cli`) so an IRQ preemption on `preferred_cpu`'s peer can
/// never suspend a producer between `StaticRunQueue::enqueue_pos` CAS and `cell.seq` commit
/// and enqueue the preempted producer behind its own uncommitted ring slot.
pub fn wake_thread(target: ThreadId, preferred_cpu: usize) -> WakeOutcome {
    let slot = target.slot();
    if !(MAX_CPUS..TOTAL_TCB_SLOTS).contains(&slot) {
        return WakeOutcome::Terminated;
    }
    let saved_if = irq_save_cli();
    let w = tcb(slot);
    let outcome = w.sched.wakeup(target.gen26());
    if outcome == WakeOutcome::WokeBlocked {
        let online = ONLINE_SCHED_CPUS.load(Ordering::Relaxed);
        let dst_cpu = preferred_cpu % online;
        let prio = w.prio.load(Ordering::Relaxed);
        w.sched.finish_waking_and_push(slot, prio, cpu_rq(dst_cpu));
    }
    irq_restore(saved_if);
    outcome
}

/// Terminate the currently executing worker thread (`Running -> Exiting -> post_switch_exit -> Dead`).
pub fn exit_current_thread() -> ! {
    assert_thread_context_schedulable("exit_current_thread");
    cli();
    let cpu_idx = current_cpu_idx();
    let cur_slot = current_slot_on_cpu(cpu_idx);
    assert!(cur_slot >= MAX_CPUS, "Idle thread cannot exit");
    let online = ONLINE_SCHED_CPUS.load(Ordering::Relaxed);

    tcb(cur_slot).sched.begin_exit(cur_slot, cpu_idx);
    let next_slot = select_next_slot_for_worker(cpu_idx, online);
    switch_to_slot(cpu_idx, cur_slot, next_slot, PostSwitchKind::Exit);
    unreachable!("Dead thread resumed after exit_current_thread")
}

/// Execute one dispatch cycle from CPU `cpu_idx`'s Boot/Idle context if a runnable worker exists.
pub fn idle_dispatch_once(cpu_idx: usize) -> bool {
    let saved_if = irq_save_cli();
    tcb(cpu_idx).progress_seq.fetch_add(1, Ordering::Relaxed);
    let online = ONLINE_SCHED_CPUS.load(Ordering::Relaxed);
    let dispatched = if let Some(raw_ticket) = pop_local_or_steal(cpu_idx, online) {
        let ticket = ThreadId::from_raw(raw_ticket);
        let next_slot = ticket.slot();
        tcb(next_slot).sched.dispatch_popped_ticket(ticket, cpu_idx);
        record_worker_dispatch(next_slot, cpu_idx);
        switch_to_slot(cpu_idx, cpu_idx, next_slot, PostSwitchKind::FromIdle);
        assert_eq!(current_cpu_idx(), cpu_idx);
        assert_eq!(current_slot_on_cpu(cpu_idx), cpu_idx);
        true
    } else {
        false
    };
    irq_restore(saved_if);
    dispatched
}

/// Secondary CPU (`AP`) scheduler service loop supporting Cooperative, IRQ-Only & Preemptive epochs.
pub fn ap_scheduler_service_loop(cpu_idx: usize) {
    while !AP_SCHED_RELEASE.load(Ordering::Acquire) {
        if crate::is_panicking() {
            crate::arch::halt_forever();
        }
        core::hint::spin_loop();
    }

    let mut seen_epoch = 0usize;
    loop {
        if crate::is_panicking() {
            crate::arch::halt_forever();
        }
        if AP_SCHED_SHUTDOWN.load(Ordering::Acquire) {
            return;
        }
        let cur_epoch = AP_PHASE_EPOCH.load(Ordering::Acquire);
        if cur_epoch > seen_epoch {
            match ApEpochKind::from_u8(AP_EPOCH_KIND.load(Ordering::Acquire)) {
                ApEpochKind::Cooperative => {
                    while PHASE_PENDING_THREADS.load(Ordering::Acquire) > 0 {
                        if crate::is_panicking() {
                            crate::arch::halt_forever();
                        }
                        if !idle_dispatch_once(cpu_idx) {
                            core::hint::spin_loop();
                        }
                    }
                }
                ApEpochKind::IrqOnlyVerify => {
                    preempt_tests::run_ap_irq_only_stage(cpu_idx);
                }
                ApEpochKind::PreemptiveSched => {
                    let pcpu = get_percpu_by_idx(cpu_idx);
                    start_cpu_timer_for_phase(pcpu);
                    sti();
                    while PHASE_PENDING_THREADS.load(Ordering::Acquire) > 0 {
                        if crate::is_panicking() {
                            crate::arch::halt_forever();
                        }
                        if !idle_dispatch_once(cpu_idx) {
                            core::hint::spin_loop();
                        }
                    }
                    stop_cpu_timer_for_phase(pcpu);
                }
                ApEpochKind::KernelVSpaceSync => {
                    crate::vspace::activate_kernel_vspace_on_ap(cpu_idx);
                }
            }
            seen_epoch = cur_epoch;
            AP_PHASE_DONE_CPUS.fetch_add(1, Ordering::AcqRel);
        } else {
            core::hint::spin_loop();
        }
    }
}
