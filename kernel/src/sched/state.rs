// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Verified `H1` Scheduler Atomic Ownership & Sleep/Wakeup Protocol (`ADR-004`, Milestone 1A-F).
//!
//! Packs orthogonal execution ownership (`ExecState`, `bits 0..=3`), doorbell park permit
//! (`ParkToken`, `bits 4..=5`), and TCB generation (`gen26`, `bits 6..=31`) into a single
//! `AtomicU32`, enforcing all 4 `H1` invariants proved in TLA+ (`formal/h1/DrySched.tla`)
//! and Rust Loom (`experiments/h1-sched/`):
//! 1. **Single Active Stack Owner (`Bug1` guard):** `stack_owner` (`NO_CPU <-> cpu_idx`) is
//!    released strictly *after* hardware `RSP` switches off the outgoing stack (`post_switch_*`).
//! 2. **Strict Ready Publication (`Bug2` guard):** `ExecState::Ready` is stored via `AcqRel` CAS
//!    strictly *before* `PerCpuRunQueue::push(ticket)` and audited against orphaned `Ready` tasks.
//! 3. **Zero Lost Wakeups (`Bug3` guard):** Explicit `fence(Ordering::SeqCst)` in `prepare_park`
//!    and `Notification::signal`, plus `CAS` inspection of `ParkToken::Notified` in `post_switch_park`.
//! 4. **No Exiting/Dead Resurrection (`Bug4` guard):** `Exiting`/`Dead` tasks reject wakeups,
//!    never re-enter a runqueue, and increment `gen26` upon TCB/stack slot recycling.

use core::sync::atomic::{fence, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::sched::runqueue::PerCpuRunQueue;

pub const NO_CPU: usize = usize::MAX;

const STATE_MASK: u32 = 0x0f;
const TOKEN_SHIFT: u32 = 4;
const TOKEN_MASK: u32 = 0x03 << TOKEN_SHIFT;
const GEN_SHIFT: u32 = 6;
pub const GEN_MAX: u32 = 0x03ff_ffff;

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
            _ => Self::Dead,
        }
    }
}

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
            _ => Self::None,
        }
    }
}

#[inline]
pub const fn pack_sched(state: ExecState, token: ParkToken, gen26: u32) -> u32 {
    (state as u32) | ((token as u32) << TOKEN_SHIFT) | ((gen26 & GEN_MAX) << GEN_SHIFT)
}

#[inline]
pub const fn unpack_sched(word: u32) -> (ExecState, ParkToken, u32) {
    (
        ExecState::from_bits(word),
        ParkToken::from_bits(word),
        (word >> GEN_SHIFT) & GEN_MAX,
    )
}

/// Generation-tagged thread handle (`(gen26 << 8) | slot8`) carried across wakeups & runqueues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadId(u32);

impl ThreadId {
    #[inline]
    pub const fn new(slot: usize, gen26: u32) -> Self {
        Self(((gen26 & GEN_MAX) << 8) | ((slot as u32) & 0xff))
    }

    #[inline]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }

    #[inline]
    pub const fn slot(self) -> usize {
        (self.0 & 0xff) as usize
    }

    #[inline]
    pub const fn gen26(self) -> u32 {
        (self.0 >> 8) & GEN_MAX
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitParkOutcome {
    ConsumedToken,
    SwitchToPark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeOutcome {
    WokeBlocked,
    NotifiedInFlight,
    Terminated,
}

pub fn sched_invariant_panic(msg: core::fmt::Arguments<'_>) -> ! {
    crate::enter_fatal_or_halt();
    crate::serial::emergency_print(format_args!("\r\n[DRY-SCHED-FAIL] {}\r\n", msg));
    panic!("H1 scheduler invariant failure: {}", msg);
}

pub struct TaskSchedState {
    sched: AtomicU32,
    stack_owner: AtomicUsize,
    in_flight_refs: AtomicUsize,
}

impl TaskSchedState {
    pub const fn new_dead() -> Self {
        Self {
            sched: AtomicU32::new(pack_sched(ExecState::Dead, ParkToken::None, 1)),
            stack_owner: AtomicUsize::new(NO_CPU),
            in_flight_refs: AtomicUsize::new(0),
        }
    }

    #[inline]
    pub fn snapshot(&self) -> (ExecState, ParkToken, u32) {
        unpack_sched(self.sched.load(Ordering::Acquire))
    }

    #[inline]
    pub fn stack_owner(&self) -> usize {
        self.stack_owner.load(Ordering::Acquire)
    }

    pub fn init_idle_running(&self, cpu_idx: usize) {
        self.in_flight_refs.store(0, Ordering::Release);
        self.stack_owner.store(cpu_idx, Ordering::Release);
        self.sched
            .store(pack_sched(ExecState::Running, ParkToken::None, 1), Ordering::Release);
    }

    pub fn init_spawned_ready(&self, slot: usize, next_gen: u32) -> ThreadId {
        let cur_owner = self.stack_owner.load(Ordering::Acquire);
        if cur_owner != NO_CPU {
            sched_invariant_panic(format_args!(
                "H1 Bug1 stack_owner violation on spawn: slot={} still owned by cpu={}",
                slot, cur_owner
            ));
        }
        if self.in_flight_refs.load(Ordering::Acquire) != 0 {
            sched_invariant_panic(format_args!(
                "H1 Bug4 spawn on slot={} with non-zero in_flight_refs",
                slot
            ));
        }
        self.in_flight_refs.store(1, Ordering::Release);
        self.sched
            .store(pack_sched(ExecState::Ready, ParkToken::None, next_gen), Ordering::Release);
        ThreadId::new(slot, next_gen)
    }

    #[inline]
    pub fn claim_idle_stack(&self, slot: usize, cpu_idx: usize) {
        let prev = self.stack_owner.swap(cpu_idx, Ordering::Acquire);
        if prev != NO_CPU {
            sched_invariant_panic(format_args!(
                "H1 Bug1 stack_owner violation on idle slot={}: prev_owner={} cpu={}",
                slot, prev, cpu_idx
            ));
        }
    }

    #[inline]
    pub fn release_stack(&self, slot: usize, cpu_idx: usize) {
        let prev = self.stack_owner.swap(NO_CPU, Ordering::Release);
        if prev != cpu_idx {
            sched_invariant_panic(format_args!(
                "H1 Bug1 stack_owner violation on release: slot={} expected_cpu={} actual={}",
                slot, cpu_idx, prev
            ));
        }
    }

    /// Claim `Ready -> Running` and exclusive `stack_owner = cpu_idx` for a popped runqueue ticket.
    pub fn dispatch_popped_ticket(&self, ticket: ThreadId, cpu_idx: usize) {
        let mut raw = self.sched.load(Ordering::Acquire);
        loop {
            let (state, tok, cur_gen) = unpack_sched(raw);
            if cur_gen != ticket.gen26() || matches!(state, ExecState::Exiting | ExecState::Dead) {
                sched_invariant_panic(format_args!(
                    "H1 Bug4 exiting/dead thread resurrection rejected: slot={} ticket_gen={} cur_gen={} state={:?}",
                    ticket.slot(),
                    ticket.gen26(),
                    cur_gen,
                    state
                ));
            }
            if state != ExecState::Ready {
                sched_invariant_panic(format_args!(
                    "H1 Bug1/Bug2 non-Ready ticket in runqueue: slot={} state={:?}",
                    ticket.slot(),
                    state
                ));
            }
            let next = pack_sched(ExecState::Running, tok, cur_gen);
            match self
                .sched
                .compare_exchange_weak(raw, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    let prev = self.stack_owner.swap(cpu_idx, Ordering::Acquire);
                    if prev != NO_CPU {
                        sched_invariant_panic(format_args!(
                            "H1 Bug1 stack_owner violation on dispatch: slot={} claimed by cpu={} while owned by cpu={}",
                            ticket.slot(),
                            cpu_idx,
                            prev
                        ));
                    }
                    self.in_flight_refs.fetch_sub(1, Ordering::AcqRel);
                    return;
                }
                Err(actual) => raw = actual,
            }
        }
    }

    /// Cooperative yield / preemption entry: `(Running, tok, gen) -> (SwPreempt, tok, gen)`.
    pub fn begin_yield(&self, slot: usize, cpu_idx: usize) {
        if self.stack_owner.load(Ordering::Relaxed) != cpu_idx {
            sched_invariant_panic(format_args!(
                "H1 Bug1 stack_owner violation in begin_yield on slot={} cpu={}",
                slot, cpu_idx
            ));
        }
        let mut raw = self.sched.load(Ordering::Acquire);
        loop {
            let (state, tok, gen26) = unpack_sched(raw);
            assert_eq!(state, ExecState::Running, "begin_yield on non-Running task");
            let next = pack_sched(ExecState::SwPreempt, tok, gen26);
            match self
                .sched
                .compare_exchange_weak(raw, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(actual) => raw = actual,
            }
        }
    }

    /// Runs strictly AFTER hardware `RSP` vacated `slot`'s stack: releases `stack_owner`,
    /// transitions `(SwPreempt, tok, gen) -> (Ready, tok, gen)`, and publishes to `rq`.
    pub fn post_switch_yield(&self, slot: usize, cpu_idx: usize, prio: usize, rq: &PerCpuRunQueue) {
        self.release_stack(slot, cpu_idx);
        self.in_flight_refs.fetch_add(1, Ordering::AcqRel);
        let mut raw = self.sched.load(Ordering::Acquire);
        loop {
            let (state, tok, gen26) = unpack_sched(raw);
            assert_eq!(state, ExecState::SwPreempt, "post_switch_yield expected SwPreempt");
            let next = pack_sched(ExecState::Ready, tok, gen26);
            match self
                .sched
                .compare_exchange_weak(raw, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    let ticket = ThreadId::new(slot, gen26).raw();
                    assert!(rq.push(prio, ticket), "PerCpuRunQueue capacity exhausted");
                    return;
                }
                Err(actual) => raw = actual,
            }
        }
    }

    /// Step 1 of `H1` sleep protocol: arm `ParkToken::Armed` + full `SeqCst` barrier.
    pub fn prepare_park(&self) {
        let mut raw = self.sched.load(Ordering::Relaxed);
        loop {
            let (state, _, gen26) = unpack_sched(raw);
            assert_eq!(state, ExecState::Running, "prepare_park requires Running state");
            let next = pack_sched(ExecState::Running, ParkToken::Armed, gen26);
            match self
                .sched
                .compare_exchange_weak(raw, next, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => {
                    fence(Ordering::SeqCst);
                    return;
                }
                Err(actual) => raw = actual,
            }
        }
    }

    pub fn cancel_park(&self) {
        let mut raw = self.sched.load(Ordering::Relaxed);
        loop {
            let (state, tok, gen26) = unpack_sched(raw);
            if state != ExecState::Running || tok == ParkToken::None {
                return;
            }
            let next = pack_sched(ExecState::Running, ParkToken::None, gen26);
            match self
                .sched
                .compare_exchange_weak(raw, next, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => return,
                Err(actual) => raw = actual,
            }
        }
    }

    /// Step 3 of `H1` sleep protocol: consume `Notified` in-place or enter `SwPark`.
    pub fn commit_park(&self, slot: usize, cpu_idx: usize) -> CommitParkOutcome {
        if self.stack_owner.load(Ordering::Relaxed) != cpu_idx {
            sched_invariant_panic(format_args!(
                "H1 Bug1 stack_owner violation in commit_park on slot={} cpu={}",
                slot, cpu_idx
            ));
        }
        let mut raw = self.sched.load(Ordering::Acquire);
        loop {
            let (state, tok, gen26) = unpack_sched(raw);
            match (state, tok) {
                (ExecState::Running, ParkToken::Notified) => {
                    let next = pack_sched(ExecState::Running, ParkToken::None, gen26);
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
                    let next = pack_sched(ExecState::SwPark, ParkToken::None, gen26);
                    match self.sched.compare_exchange_weak(
                        raw,
                        next,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return CommitParkOutcome::SwitchToPark,
                        Err(actual) => raw = actual,
                    }
                }
                (other_state, other_tok) => {
                    sched_invariant_panic(format_args!(
                        "H1 Bug3 invalid park protocol state on slot={}: state={:?} token={:?}",
                        slot, other_state, other_tok
                    ));
                }
            }
        }
    }

    /// Runs strictly AFTER hardware `RSP` vacated `slot`'s stack on `SwitchToPark`.
    pub fn post_switch_park(&self, slot: usize, cpu_idx: usize, prio: usize, rq: &PerCpuRunQueue) {
        self.release_stack(slot, cpu_idx);
        let mut raw = self.sched.load(Ordering::Acquire);
        loop {
            let (state, tok, gen26) = unpack_sched(raw);
            assert_eq!(state, ExecState::SwPark, "post_switch_park expected SwPark");
            match tok {
                ParkToken::Notified => {
                    self.in_flight_refs.fetch_add(1, Ordering::AcqRel);
                    let next = pack_sched(ExecState::Ready, ParkToken::None, gen26);
                    match self.sched.compare_exchange_weak(
                        raw,
                        next,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => {
                            let ticket = ThreadId::new(slot, gen26).raw();
                            assert!(rq.push(prio, ticket), "PerCpuRunQueue capacity exhausted");
                            return;
                        }
                        Err(actual) => {
                            self.in_flight_refs.fetch_sub(1, Ordering::AcqRel);
                            raw = actual;
                        }
                    }
                }
                ParkToken::None => {
                    let next = pack_sched(ExecState::Blocked, ParkToken::None, gen26);
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
                ParkToken::Armed => unreachable!(),
            }
        }
    }

    /// Atomic generation-checked wakeup (`Blocked -> Waking` or `ParkToken::Notified`).
    pub fn wakeup(&self, expected_gen: u32) -> WakeOutcome {
        let mut raw = self.sched.load(Ordering::Acquire);
        loop {
            let (state, tok, cur_gen) = unpack_sched(raw);
            if cur_gen != expected_gen || matches!(state, ExecState::Exiting | ExecState::Dead) {
                return WakeOutcome::Terminated;
            }
            match state {
                ExecState::Blocked => {
                    self.in_flight_refs.fetch_add(1, Ordering::AcqRel);
                    let next = pack_sched(ExecState::Waking, ParkToken::None, cur_gen);
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
                    let next = pack_sched(state, ParkToken::Notified, cur_gen);
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
                    let next = pack_sched(state, ParkToken::Notified, cur_gen);
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
                ExecState::Exiting | ExecState::Dead => return WakeOutcome::Terminated,
            }
        }
    }

    pub fn finish_waking_and_push(&self, slot: usize, prio: usize, rq: &PerCpuRunQueue) {
        let mut raw = self.sched.load(Ordering::Acquire);
        loop {
            let (state, tok, gen26) = unpack_sched(raw);
            assert_eq!(state, ExecState::Waking, "finish_waking expected Waking");
            let next = pack_sched(ExecState::Ready, tok, gen26);
            match self
                .sched
                .compare_exchange_weak(raw, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    let ticket = ThreadId::new(slot, gen26).raw();
                    assert!(rq.push(prio, ticket), "PerCpuRunQueue capacity exhausted");
                    return;
                }
                Err(actual) => raw = actual,
            }
        }
    }

    /// Voluntary exit entry on calling CPU: `(Running, _, gen) -> (Exiting, None, gen)`.
    pub fn begin_exit(&self, slot: usize, cpu_idx: usize) {
        if self.stack_owner.load(Ordering::Relaxed) != cpu_idx {
            sched_invariant_panic(format_args!(
                "H1 Bug1 stack_owner violation in begin_exit on slot={} cpu={}",
                slot, cpu_idx
            ));
        }
        let mut raw = self.sched.load(Ordering::Acquire);
        loop {
            let (state, _, gen26) = unpack_sched(raw);
            assert_eq!(state, ExecState::Running, "begin_exit requires Running state");
            let next = pack_sched(ExecState::Exiting, ParkToken::None, gen26);
            match self
                .sched
                .compare_exchange_weak(raw, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(actual) => raw = actual,
            }
        }
    }

    /// Runs strictly AFTER hardware `RSP` vacated exiting thread's stack: releases `stack_owner`
    /// and transitions `Exiting -> Dead` once quiescent (`in_flight_refs == 0`).
    pub fn post_switch_exit_and_reap(&self, slot: usize, cpu_idx: usize) -> u32 {
        self.release_stack(slot, cpu_idx);
        assert_eq!(
            self.in_flight_refs.load(Ordering::Acquire),
            0,
            "Unexpected in_flight_refs on cooperative exit"
        );
        let raw = self.sched.load(Ordering::Acquire);
        let (state, tok, gen26) = unpack_sched(raw);
        assert_eq!((state, tok), (ExecState::Exiting, ParkToken::None));
        let dead = pack_sched(ExecState::Dead, ParkToken::None, gen26);
        self.sched
            .compare_exchange(raw, dead, Ordering::AcqRel, Ordering::Acquire)
            .expect("Failed Exiting -> Dead CAS in post_switch_exit");
        gen26
    }

    /// Fault-injection helper used ONLY by negative `sched_fault=*` QEMU tests.
    pub fn force_raw_for_fault_test(&self, state: ExecState, token: ParkToken, gen26: u32, owner: usize) {
        self.stack_owner.store(owner, Ordering::Release);
        self.sched
            .store(pack_sched(state, token, gen26), Ordering::Release);
    }
}

/// TCB-bound async Notification / Doorbell object (`ADR-002`).
pub struct Notification {
    signals: AtomicU64,
}

impl Notification {
    pub const fn new() -> Self {
        Self {
            signals: AtomicU64::new(0),
        }
    }

    pub fn reset(&self) {
        self.signals.store(0, Ordering::Release);
    }

    pub fn signal_raw(&self, bits: u64) {
        self.signals.fetch_or(bits, Ordering::Release);
        // Critical RC11 / AArch64 full barrier ordering `signals.fetch_or` BEFORE `wakeup()`.
        fence(Ordering::SeqCst);
    }

    #[inline]
    pub fn consume(&self) -> u64 {
        self.signals.swap(0, Ordering::AcqRel)
    }
}
