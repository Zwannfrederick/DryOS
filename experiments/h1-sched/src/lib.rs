//! DRY Kernel — H1 Scheduler Ownership, Sleep/Wakeup, and Work-Stealing State Machine.
//!
//! Packs orthogonal execution ownership (`ExecState`, low 4 bits) and doorbell park permit
//! (`ParkToken`, bits `4..=5`) into a single `AtomicU32` per task (`Task::sched`), paired with:
//! - Explicit `fence(Ordering::SeqCst)` barriers preventing RC11 / AArch64 `StoreLoad` lost wakeups.
//! - Two-phase context-switch stack handoff (`SwPreempt` / `SwPark` -> `post_switch_*`) verified by
//!   both `stack_owner` atomic guards and `loom::cell::UnsafeCell` C11 data-race tracking.
//! - Strict publication ordering (`exec_state -> Ready` BEFORE `rq.push(task_id)`).
//! - Quiescent reference tracking (`in_flight_refs`) preventing dead-task resurrection or early reap.

mod runqueue;
mod sync;

pub use runqueue::{PerCpuRunQueue, RunQueue};
use sync::{fence, yield_now, AtomicU32, AtomicU64, AtomicUsize, Ordering, UnsafeCell};

pub const NO_CPU: usize = usize::MAX;

const STATE_MASK: u32 = 0x0F;
const TOKEN_SHIFT: u32 = 4;
const TOKEN_MASK: u32 = 0x03 << TOKEN_SHIFT;

/// Per-task execution ownership state (`bits 0..=3` of `Task::sched`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecState {
    Ready = 0,
    Running = 1,
    SwPreempt = 2,
    SwPark = 3,
    Blocked = 4,
    Waking = 5,
    Exiting = 6,
    Dead = 7,
}

impl ExecState {
    #[inline]
    const fn from_bits(bits: u32) -> Self {
        match bits & STATE_MASK {
            0 => Self::Ready,
            1 => Self::Running,
            2 => Self::SwPreempt,
            3 => Self::SwPark,
            4 => Self::Blocked,
            5 => Self::Waking,
            6 => Self::Exiting,
            7 => Self::Dead,
            _ => unreachable!(),
        }
    }
}

/// Orthogonal sleep/wakeup token (`bits 4..=5` of `Task::sched`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkToken {
    None = 0,
    Armed = 1,
    Notified = 2,
}

impl ParkToken {
    #[inline]
    const fn from_bits(bits: u32) -> Self {
        match (bits & TOKEN_MASK) >> TOKEN_SHIFT {
            0 => Self::None,
            1 => Self::Armed,
            2 => Self::Notified,
            _ => unreachable!(),
        }
    }
}

#[inline]
const fn pack_sched(state: ExecState, token: ParkToken) -> u32 {
    (state as u32) | ((token as u32) << TOKEN_SHIFT)
}

#[inline]
const fn unpack_sched(word: u32) -> (ExecState, ParkToken) {
    (ExecState::from_bits(word), ParkToken::from_bits(word))
}

/// Outcome of `Task::commit_park`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitParkOutcome {
    /// Concurrent waker deposited `ParkToken::Notified`; consumed in-place without switching stacks.
    ConsumedToken,
    /// Transitioned `(Running, Armed) -> (SwPark, None)`; caller must switch off stack & call `post_switch_park`.
    SwitchToPark,
    /// Task was concurrently terminated (`Exiting`); caller must switch off stack without parking.
    Exiting,
}

/// Outcome of `Task::wakeup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeOutcome {
    /// Caller won `Blocked -> Waking` and MUST call `finish_waking_and_push`.
    WokeBlocked,
    /// Task was in-flight (`Running`, `SwPreempt`, `Ready`, `SwPark`, or `Waking`); token updated if needed.
    NotifiedInFlight,
    /// Task is `Exiting` or `Dead`; wakeup ignored.
    Terminated,
}

/// Kernel TCB scheduling & stack-ownership state verified under H1.
pub struct Task {
    id: usize,
    sched: AtomicU32,
    stack_owner: AtomicUsize,
    in_flight_refs: AtomicUsize,
    /// Simulated non-atomic kernel stack / callee-saved register frame.
    /// Under `#[cfg(loom)]`, `loom::cell::UnsafeCell` panics on any C11 data race!
    kernel_stack_frame: UnsafeCell<u64>,
}

// SAFETY: Access to `kernel_stack_frame` is strictly serialized by the `Task::sched`
// Acquire/Release state machine and `stack_owner` handoff; `loom` verifies this dynamically.
unsafe impl Sync for Task {}
unsafe impl Send for Task {}

impl Task {
    /// Creates a task initially in `ExecState::Ready` (call `prepare_enqueue` before first `rq.push`).
    pub fn new_ready(id: usize) -> Self {
        Self {
            id,
            sched: AtomicU32::new(pack_sched(ExecState::Ready, ParkToken::None)),
            stack_owner: AtomicUsize::new(NO_CPU),
            in_flight_refs: AtomicUsize::new(0),
            kernel_stack_frame: UnsafeCell::new(0),
        }
    }

    /// Creates a task already running on `cpu_id` (used for focused single-task Loom models).
    pub fn new_running_for_test(cpu_id: usize) -> Self {
        Self {
            id: 0,
            sched: AtomicU32::new(pack_sched(ExecState::Running, ParkToken::None)),
            stack_owner: AtomicUsize::new(cpu_id),
            in_flight_refs: AtomicUsize::new(0),
            kernel_stack_frame: UnsafeCell::new(0),
        }
    }

    #[inline]
    pub fn id(&self) -> usize {
        self.id
    }

    #[inline]
    pub fn state(&self) -> ExecState {
        unpack_sched(self.sched.load(Ordering::Acquire)).0
    }

    #[inline]
    pub fn token(&self) -> ParkToken {
        unpack_sched(self.sched.load(Ordering::Acquire)).1
    }

    #[inline]
    pub fn in_flight_refs(&self) -> usize {
        self.in_flight_refs.load(Ordering::Acquire)
    }

    /// Increments `in_flight_refs` prior to initial `RunQueue::push` at task creation.
    #[inline]
    pub fn prepare_enqueue(&self) {
        self.in_flight_refs.fetch_add(1, Ordering::AcqRel);
    }

    /// Mutates the non-atomic kernel stack frame on the owning CPU.
    #[inline]
    pub fn touch_stack_frame(&self) {
        assert_ne!(
            self.stack_owner.load(Ordering::Relaxed),
            NO_CPU,
            "H1 VIOLATION: touched kernel stack without active CPU ownership"
        );
        self.kernel_stack_frame.with_mut(|ptr| {
            // SAFETY: Protected by `Running` / pre-`post_switch` exclusive ownership.
            unsafe {
                *ptr = (*ptr).wrapping_add(1);
            }
        });
    }

    /// Attempts to claim execution of a popped/stolen task on `cpu_id` (`Ready -> Running`).
    pub fn try_dispatch(&self, cpu_id: usize) -> bool {
        let mut raw = self.sched.load(Ordering::Acquire);
        loop {
            let (state, tok) = unpack_sched(raw);
            if state != ExecState::Ready {
                return false;
            }
            let next = pack_sched(ExecState::Running, tok);
            match self
                .sched
                .compare_exchange_weak(raw, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    let prev_cpu = self.stack_owner.swap(cpu_id, Ordering::Acquire);
                    assert_eq!(
                        prev_cpu, NO_CPU,
                        "H1 VIOLATION: dual-CPU stack collision on dispatch!"
                    );
                    self.in_flight_refs.fetch_sub(1, Ordering::AcqRel);
                    return true;
                }
                Err(actual) => raw = actual,
            }
        }
    }

    /// Step 1 of sleep protocol: arms `ParkToken::Armed` on `Running` task and emits `SeqCst` fence.
    pub fn prepare_park(&self) {
        let mut raw = self.sched.load(Ordering::Relaxed);
        loop {
            let (state, _) = unpack_sched(raw);
            if state == ExecState::Exiting {
                return;
            }
            debug_assert_eq!(state, ExecState::Running);
            let next = pack_sched(ExecState::Running, ParkToken::Armed);
            match self
                .sched
                .compare_exchange_weak(raw, next, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => {
                    // Critical RC11 / AArch64 full barrier ordering `sched = Armed` BEFORE `cond.load()`.
                    fence(Ordering::SeqCst);
                    return;
                }
                Err(actual) => raw = actual,
            }
        }
    }

    /// Voluntarily cancels an armed park before `commit_park` (e.g., condition already satisfied or local abort).
    pub fn cancel_park(&self) {
        let mut raw = self.sched.load(Ordering::Relaxed);
        loop {
            let (state, tok) = unpack_sched(raw);
            if state != ExecState::Running || tok == ParkToken::None {
                return;
            }
            let next = pack_sched(ExecState::Running, ParkToken::None);
            match self
                .sched
                .compare_exchange_weak(raw, next, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => return,
                Err(actual) => raw = actual,
            }
        }
    }

    /// Step 3 of sleep protocol: either consumes `ParkToken::Notified` in-place (fast path)
    /// or transitions `(Running, Armed) -> (SwPark, None)` to enter context switch.
    pub fn commit_park(&self, cpu_id: usize) -> CommitParkOutcome {
        assert_eq!(self.stack_owner.load(Ordering::Relaxed), cpu_id);
        let mut raw = self.sched.load(Ordering::Acquire);
        loop {
            let (state, tok) = unpack_sched(raw);
            match (state, tok) {
                (ExecState::Exiting, _) => return CommitParkOutcome::Exiting,
                (ExecState::Running, ParkToken::Notified) => {
                    let next = pack_sched(ExecState::Running, ParkToken::None);
                    match self.sched.compare_exchange_weak(
                        raw,
                        next,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return CommitParkOutcome::ConsumedToken,
                        Err(actual) => raw = actual,
                    }
                }
                (ExecState::Running, ParkToken::Armed) => {
                    let next = pack_sched(ExecState::SwPark, ParkToken::None);
                    match self.sched.compare_exchange_weak(
                        raw,
                        next,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => {
                            // Simulate saving callee-saved registers onto kernel stack BEFORE switching RSP.
                            self.touch_stack_frame();
                            return CommitParkOutcome::SwitchToPark;
                        }
                        Err(actual) => raw = actual,
                    }
                }
                other => panic!("Invalid state in commit_park: {other:?}"),
            }
        }
    }

    /// Initiates timer/IRQ preemption on `cpu_id`: transitions `(Running, tok) -> (SwPreempt, tok)`
    /// while strictly preserving `ParkToken` (`Armed` or `Notified`) and retaining `stack_owner`.
    pub fn begin_preempt(&self, cpu_id: usize) -> bool {
        assert_eq!(self.stack_owner.load(Ordering::Relaxed), cpu_id);
        let mut raw = self.sched.load(Ordering::Acquire);
        loop {
            let (state, tok) = unpack_sched(raw);
            if state == ExecState::Exiting {
                return false;
            }
            assert_eq!(state, ExecState::Running);
            let next = pack_sched(ExecState::SwPreempt, tok);
            match self
                .sched
                .compare_exchange_weak(raw, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    self.touch_stack_frame();
                    return true;
                }
                Err(actual) => raw = actual,
            }
        }
    }

    /// Completes context switch after `begin_preempt`:
    /// 1. Releases kernel stack ownership (`Release`).
    /// 2. Atomically transitions `(SwPreempt, tok) -> (Ready, tok)` preserving `tok`.
    /// 3. Publishes `task_id` to `rq` strictly AFTER `Ready`.
    pub fn post_switch_preempt(&self, cpu_id: usize, rq: &RunQueue, task_id: usize) {
        let prev = self.stack_owner.swap(NO_CPU, Ordering::Release);
        assert_eq!(prev, cpu_id, "H1 VIOLATION: wrong CPU in post_switch_preempt");

        self.in_flight_refs.fetch_add(1, Ordering::AcqRel);
        let mut raw = self.sched.load(Ordering::Acquire);
        loop {
            let (state, tok) = unpack_sched(raw);
            if state != ExecState::SwPreempt {
                // Concurrent `terminate()` moved task to `Exiting`; abort enqueue!
                self.in_flight_refs.fetch_sub(1, Ordering::AcqRel);
                return;
            }
            let next = pack_sched(ExecState::Ready, tok);
            match self
                .sched
                .compare_exchange_weak(raw, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    assert!(rq.push(task_id), "RunQueue capacity exhausted");
                    return;
                }
                Err(actual) => raw = actual,
            }
        }
    }

    /// Convenience wrapper when `task_id == self.id`.
    #[inline]
    pub fn post_switch_park(&self, cpu_id: usize, rq: &RunQueue) {
        self.post_switch_park_id(cpu_id, rq, self.id);
    }

    /// Completes context switch after `commit_park -> SwitchToPark`:
    /// - Releases stack ownership first.
    /// - If `ParkToken::Notified` arrived during `SwPark`, transitions directly to `(Ready, None)` and pushes!
    /// - Otherwise transitions `(SwPark, None) -> (Blocked, None)`.
    pub fn post_switch_park_id(&self, cpu_id: usize, rq: &RunQueue, task_id: usize) {
        let prev = self.stack_owner.swap(NO_CPU, Ordering::Release);
        assert_eq!(prev, cpu_id, "H1 VIOLATION: wrong CPU in post_switch_park");

        let mut raw = self.sched.load(Ordering::Acquire);
        loop {
            let (state, tok) = unpack_sched(raw);
            if state != ExecState::SwPark {
                return;
            }
            match tok {
                ParkToken::Notified => {
                    self.in_flight_refs.fetch_add(1, Ordering::AcqRel);
                    let next = pack_sched(ExecState::Ready, ParkToken::None);
                    match self.sched.compare_exchange_weak(
                        raw,
                        next,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => {
                            assert!(rq.push(task_id), "RunQueue capacity exhausted");
                            return;
                        }
                        Err(actual) => {
                            self.in_flight_refs.fetch_sub(1, Ordering::AcqRel);
                            raw = actual;
                        }
                    }
                }
                ParkToken::None => {
                    let next = pack_sched(ExecState::Blocked, ParkToken::None);
                    match self.sched.compare_exchange_weak(
                        raw,
                        next,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return,
                        Err(actual) => raw = actual,
                    }
                }
                ParkToken::Armed => unreachable!("SwPark never carries Armed"),
            }
        }
    }

    /// Releases stack ownership when a `Running` task observed `Exiting`.
    pub fn post_switch_exit(&self, cpu_id: usize) {
        let prev = self.stack_owner.swap(NO_CPU, Ordering::Release);
        assert_eq!(prev, cpu_id);
    }

    /// Atomic doorbell wakeup transition on `Task::sched`.
    /// If `WakeOutcome::WokeBlocked` is returned, caller exclusively owns `Waking`
    /// and MUST invoke `finish_waking_and_push`.
    pub fn wakeup(&self) -> WakeOutcome {
        let mut raw = self.sched.load(Ordering::Acquire);
        loop {
            let (state, tok) = unpack_sched(raw);
            match state {
                ExecState::Exiting | ExecState::Dead => return WakeOutcome::Terminated,
                ExecState::Blocked => {
                    self.in_flight_refs.fetch_add(1, Ordering::AcqRel);
                    let next = pack_sched(ExecState::Waking, ParkToken::None);
                    match self.sched.compare_exchange_weak(
                        raw,
                        next,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return WakeOutcome::WokeBlocked,
                        Err(actual) => {
                            self.in_flight_refs.fetch_sub(1, Ordering::AcqRel);
                            raw = actual;
                        }
                    }
                }
                ExecState::SwPark | ExecState::Waking => {
                    if tok == ParkToken::Notified {
                        return WakeOutcome::NotifiedInFlight;
                    }
                    let next = pack_sched(state, ParkToken::Notified);
                    match self.sched.compare_exchange_weak(
                        raw,
                        next,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return WakeOutcome::NotifiedInFlight,
                        Err(actual) => raw = actual,
                    }
                }
                ExecState::Running | ExecState::SwPreempt | ExecState::Ready => {
                    if tok != ParkToken::Armed {
                        return WakeOutcome::NotifiedInFlight;
                    }
                    let next = pack_sched(state, ParkToken::Notified);
                    match self.sched.compare_exchange_weak(
                        raw,
                        next,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return WakeOutcome::NotifiedInFlight,
                        Err(actual) => raw = actual,
                    }
                }
            }
        }
    }

    /// Completes `Waking -> Ready` via CAS (preserving concurrent `ParkToken::Notified`
    /// and aborting if `Exiting`), then publishes `task_id` into `rq`.
    pub fn finish_waking_and_push(&self, rq: &RunQueue, task_id: usize) {
        let mut raw = self.sched.load(Ordering::Acquire);
        loop {
            let (state, tok) = unpack_sched(raw);
            if state != ExecState::Waking {
                // Concurrent `terminate()` moved task to `Exiting`; release ref without pushing!
                self.in_flight_refs.fetch_sub(1, Ordering::AcqRel);
                return;
            }
            let next = pack_sched(ExecState::Ready, tok);
            match self
                .sched
                .compare_exchange_weak(raw, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    assert!(rq.push(task_id), "RunQueue capacity exhausted");
                    return;
                }
                Err(actual) => raw = actual,
            }
        }
    }

    /// Marks the task `ExecState::Exiting` (used by task exit or `CSpace::Revoke`).
    pub fn terminate(&self) {
        let mut raw = self.sched.load(Ordering::Acquire);
        loop {
            let (state, _) = unpack_sched(raw);
            if matches!(state, ExecState::Exiting | ExecState::Dead) {
                return;
            }
            let next = pack_sched(ExecState::Exiting, ParkToken::None);
            match self
                .sched
                .compare_exchange_weak(raw, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(actual) => raw = actual,
            }
        }
    }

    /// Releases the runqueue reference when a CPU pops a stale `Exiting` task entry and drops it.
    #[inline]
    pub fn acknowledge_dropped_rq_entry(&self) {
        self.in_flight_refs.fetch_sub(1, Ordering::AcqRel);
    }

    /// Transitions `Exiting -> Dead` ONLY when zero stack users and zero in-flight runqueue/waker refs remain.
    pub fn try_reap(&self) -> bool {
        if self.stack_owner.load(Ordering::Acquire) != NO_CPU {
            return false;
        }
        if self.in_flight_refs.load(Ordering::Acquire) != 0 {
            return false;
        }
        let expected = pack_sched(ExecState::Exiting, ParkToken::None);
        let dead = pack_sched(ExecState::Dead, ParkToken::None);
        self.sched
            .compare_exchange(expected, dead, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    // =========================================================================
    // Explicit Defect-Injection Helpers (for negative `#[should_panic]` Loom proofs)
    // =========================================================================

    #[doc(hidden)]
    pub fn prepare_park_relaxed_no_fence_for_bug_demo(&self) {
        let next = pack_sched(ExecState::Running, ParkToken::Armed);
        self.sched.store(next, Ordering::Release);
    }

    #[doc(hidden)]
    pub fn buggy_post_switch_park_blind_store(&self, cpu_id: usize) {
        let prev = self.stack_owner.swap(NO_CPU, Ordering::Release);
        assert_eq!(prev, cpu_id);
        self.sched.store(
            pack_sched(ExecState::Blocked, ParkToken::None),
            Ordering::Release,
        );
    }

    #[doc(hidden)]
    pub fn buggy_post_switch_push_before_ready(
        &self,
        cpu_id: usize,
        rq: &RunQueue,
        task_id: usize,
    ) {
        let prev = self.stack_owner.swap(NO_CPU, Ordering::Release);
        assert_eq!(prev, cpu_id);
        self.in_flight_refs.fetch_add(1, Ordering::AcqRel);
        assert!(rq.push(task_id));
        yield_now();
        self.sched.store(
            pack_sched(ExecState::Ready, ParkToken::None),
            Ordering::Release,
        );
    }

    #[doc(hidden)]
    pub fn buggy_preempt_early_ready_before_stack_release(
        &self,
        cpu_id: usize,
        rq: &RunQueue,
        task_id: usize,
    ) {
        assert_eq!(self.stack_owner.load(Ordering::Relaxed), cpu_id);
        self.in_flight_refs.fetch_add(1, Ordering::AcqRel);
        self.sched.store(
            pack_sched(ExecState::Ready, ParkToken::None),
            Ordering::Release,
        );
        assert!(rq.push(task_id));
        yield_now();
        self.kernel_stack_frame.with_mut(|ptr| unsafe {
            *ptr = (*ptr).wrapping_add(1);
        });
        let _ = self.stack_owner.compare_exchange(
            cpu_id,
            NO_CPU,
            Ordering::Release,
            Ordering::Relaxed,
        );
    }

    #[doc(hidden)]
    pub fn buggy_finish_waking_blind_store(&self, rq: &RunQueue, task_id: usize) {
        self.sched.store(
            pack_sched(ExecState::Ready, ParkToken::None),
            Ordering::Release,
        );
        assert!(rq.push(task_id));
    }
}

/// TCB-bound async Notification / Doorbell object (`ADR-002` / `ARCHITECTURE.md`).
pub struct Notification {
    signals: AtomicU64,
}

impl Default for Notification {
    fn default() -> Self {
        Self::new()
    }
}

impl Notification {
    pub fn new() -> Self {
        Self {
            signals: AtomicU64::new(0),
        }
    }

    /// Deposits signal badge bits, executes `SeqCst` barrier, and wakes bound `task`.
    pub fn signal(&self, bits: u64, task: &Task, rq: &RunQueue, task_id: usize) {
        self.signals.fetch_or(bits, Ordering::Release);
        // Critical RC11 / AArch64 full barrier ordering `signals.fetch_or` BEFORE `task.wakeup()`.
        fence(Ordering::SeqCst);
        if task.wakeup() == WakeOutcome::WokeBlocked {
            task.finish_waking_and_push(rq, task_id);
        }
    }

    /// Reads pending signal bits without clearing.
    #[inline]
    pub fn poll(&self) -> u64 {
        self.signals.load(Ordering::Acquire)
    }

    /// Atomically reads and clears all pending signal bits.
    #[inline]
    pub fn consume(&self) -> u64 {
        self.signals.swap(0, Ordering::AcqRel)
    }
}
