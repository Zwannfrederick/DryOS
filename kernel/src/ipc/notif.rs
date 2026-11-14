// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-Gated Asynchronous `Notification` Objects (`Milestone 1C` Stage A.2).
//!
//! - `notif_signal`: Non-blocking atomic `fetch_or` + `SeqCst` fence + H1 wakeup (`0` sleep, safe
//!   from preempt-disabled or driver/IRQ producer contexts).
//! - `notif_wait`: Lock-free 1-atomic-swap fast path when bits are pending; H1 park/wake with
//!   local CPU cache locality, bounded TSC timeout, and deterministic `revoke` wakeup on slow path.

use core::sync::atomic::{fence, AtomicBool, AtomicI32, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

use crate::arch::{irq_restore, irq_save_cli, rdtsc};
use crate::cspace::cap::{
    next_slot_gen, unpack_state_gen, CSpaceId, CapError, CapObjectKind, CapPtr, SlotState,
};
use crate::cspace::{cspace_owner_label, insert_root_cap, slot_by_global, with_pinned_cap};
use crate::percpu::read_gs_u64_at_8;
use crate::sched::state::ThreadId;
use crate::sched::{
    cancel_park_current, current_cpu_idx, current_slot_on_cpu, ipc_park_current,
    prepare_park_current, tcb, wake_thread,
};
use crate::security::{current_lease_epoch, CapRights, SecDomain, SecLabel};
use crate::uaccess::SysError;
use crate::vspace::tlb::poll_and_ack_pending_shootdown;

use crate::ipc::{check_blocking_ipc_allowed, map_cap_err_to_sys};

pub const MAX_NOTIFICATIONS: usize = 32;
const NO_WAITER: u16 = u16::MAX;

const WAIT_IDLE: u8 = 0;
const WAIT_BLOCKED: u8 = 1;
const WAIT_DELIVERED: u8 = 2;
const WAIT_FAILED: u8 = 3;

#[repr(C, align(64))]
struct NotifSlot {
    allocated: AtomicBool,
    closed: AtomicBool,
    wait_state: AtomicU8,
    lock: AtomicBool,
    generation: AtomicU32,
    pending_bits: AtomicU64,
    delivered_bits: AtomicU64,
    waiter_slot: AtomicU16,
    waiter_cpu: AtomicU16,
    waiter_gen26: AtomicU32,
    waiter_cap_gslot: AtomicU32,
    fail_errno: AtomicI32,
}

impl NotifSlot {
    const fn new() -> Self {
        Self {
            allocated: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            wait_state: AtomicU8::new(WAIT_IDLE),
            lock: AtomicBool::new(false),
            generation: AtomicU32::new(1),
            pending_bits: AtomicU64::new(0),
            delivered_bits: AtomicU64::new(0),
            waiter_slot: AtomicU16::new(NO_WAITER),
            waiter_cpu: AtomicU16::new(0),
            waiter_gen26: AtomicU32::new(0),
            waiter_cap_gslot: AtomicU32::new(u32::MAX),
            fail_errno: AtomicI32::new(0),
        }
    }
}

static NOTIF_TABLE: [NotifSlot; MAX_NOTIFICATIONS] = [const { NotifSlot::new() }; MAX_NOTIFICATIONS];

#[inline]
fn with_notif_lock<R>(n: &NotifSlot, f: impl FnOnce() -> R) -> R {
    let saved_if = irq_save_cli();
    let cpu_idx = read_gs_u64_at_8() as usize;
    while n
        .lock
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        poll_and_ack_pending_shootdown(cpu_idx);
        core::hint::spin_loop();
    }
    let out = f();
    n.lock.store(false, Ordering::Release);
    irq_restore(saved_if);
    out
}

#[inline(always)]
pub fn is_notif_live(obj_id: u32, expected_gen: u32) -> bool {
    let idx = obj_id as usize;
    idx < MAX_NOTIFICATIONS
        && NOTIF_TABLE[idx].allocated.load(Ordering::Acquire)
        && NOTIF_TABLE[idx].generation.load(Ordering::Acquire) == expected_gen
}

/// Create a standalone `Notification` object and install its root capability into `cspace_id`.
pub fn notif_create(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    obj_label: SecLabel,
    lease_epoch: u32,
) -> Result<(CapPtr, u32), CapError> {
    let owner = cspace_owner_label(cspace_id)?;
    if owner.domain != subj_label.domain || owner.compartment_id != subj_label.compartment_id {
        return Err(CapError::DomainViolation);
    }
    if subj_label.domain == SecDomain::Userspace && obj_label != subj_label {
        return Err(CapError::DomainViolation);
    }
    if obj_label.domain == SecDomain::Kernel && subj_label.domain != SecDomain::Kernel {
        return Err(CapError::DomainViolation);
    }

    for (idx, n) in NOTIF_TABLE.iter().enumerate() {
        if n.allocated
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let cur_gen = n.generation.load(Ordering::Acquire).max(1);
            n.closed.store(false, Ordering::Release);
            n.pending_bits.store(0, Ordering::Release);
            n.delivered_bits.store(0, Ordering::Release);
            n.wait_state.store(WAIT_IDLE, Ordering::Release);
            n.waiter_slot.store(NO_WAITER, Ordering::Release);
            n.waiter_cap_gslot.store(u32::MAX, Ordering::Release);
            n.fail_errno.store(0, Ordering::Release);

            match insert_root_cap(
                cspace_id,
                CapObjectKind::Notification,
                idx as u32,
                cur_gen,
                CapRights::NOTIF_ROOT,
                obj_label,
                lease_epoch,
                0,
            ) {
                Ok(cap) => return Ok((cap, idx as u32)),
                Err(e) => {
                    n.allocated.store(false, Ordering::Release);
                    return Err(e);
                }
            }
        }
    }
    Err(CapError::OutOfSlots)
}

/// Non-blocking `Notification::signal` (`CapRights::SIGNAL`).
/// Accumulates capability `badge` (if non-zero) or `signal_bits` via `fetch_or` and wakes any waiter.
pub fn notif_signal(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    notif_cap: CapPtr,
    signal_bits: u64,
) -> Result<u64, SysError> {
    let now_epoch = current_lease_epoch();
    with_pinned_cap(
        cspace_id,
        subj_label,
        notif_cap,
        CapRights::SIGNAL,
        now_epoch,
        |res| {
            if res.obj_kind != CapObjectKind::Notification {
                return Err(CapError::WrongObjectKind);
            }
            let n = &NOTIF_TABLE[res.obj_id as usize];
            if n.closed.load(Ordering::Acquire) {
                return Err(CapError::Revoked);
            }
            let bits = if res.badge != 0 {
                res.badge
            } else {
                signal_bits.max(1)
            };
            n.pending_bits.fetch_or(bits, Ordering::AcqRel);
            fence(Ordering::SeqCst);

            let mut wake_target: Option<(ThreadId, usize)> = None;
            if n.waiter_slot.load(Ordering::SeqCst) != NO_WAITER {
                with_notif_lock(n, || {
                    if n.wait_state.load(Ordering::Acquire) == WAIT_BLOCKED {
                        let w_slot = n.waiter_slot.load(Ordering::Relaxed);
                        if w_slot != NO_WAITER {
                            let word = n.pending_bits.swap(0, Ordering::AcqRel);
                            if word != 0 {
                                let w_gen = n.waiter_gen26.load(Ordering::Relaxed);
                                let w_cpu = n.waiter_cpu.load(Ordering::Relaxed) as usize;
                                n.delivered_bits.store(word, Ordering::Release);
                                n.waiter_slot.store(NO_WAITER, Ordering::Release);
                                n.wait_state.store(WAIT_DELIVERED, Ordering::Release);
                                wake_target = Some((ThreadId::new(w_slot as usize, w_gen), w_cpu));
                            }
                        }
                    }
                });
            }
            if let Some((tid, pref_cpu)) = wake_target {
                let _ = wake_thread(tid, pref_cpu);
            }
            Ok(bits)
        },
    )
    .map_err(map_cap_err_to_sys)
}

/// Non-blocking hardware-ISR `Notification` signal (`0` `CSpace` lookups, `0` sleeping operations).
///
/// Safe to invoke inside `on_external_irq` (`irq_depth > 0`, `RFLAGS.IF == 0`).
pub(crate) fn notif_signal_from_irq(obj_id: u32, expected_gen: u32, signal_bits: u64) -> bool {
    let idx = obj_id as usize;
    if idx >= MAX_NOTIFICATIONS {
        return false;
    }
    let n = &NOTIF_TABLE[idx];
    if !n.allocated.load(Ordering::Acquire)
        || n.closed.load(Ordering::Acquire)
        || n.generation.load(Ordering::Acquire) != expected_gen
    {
        return false;
    }
    let bits = signal_bits.max(1);
    n.pending_bits.fetch_or(bits, Ordering::AcqRel);
    fence(Ordering::SeqCst);

    let mut wake_target: Option<(ThreadId, usize)> = None;
    if n.waiter_slot.load(Ordering::SeqCst) != NO_WAITER {
        with_notif_lock(n, || {
            if n.allocated.load(Ordering::Acquire)
                && !n.closed.load(Ordering::Acquire)
                && n.generation.load(Ordering::Acquire) == expected_gen
                && n.wait_state.load(Ordering::Acquire) == WAIT_BLOCKED
            {
                let w_slot = n.waiter_slot.load(Ordering::Relaxed);
                if w_slot != NO_WAITER {
                    let word = n.pending_bits.swap(0, Ordering::AcqRel);
                    if word != 0 {
                        let w_gen = n.waiter_gen26.load(Ordering::Relaxed);
                        let w_cpu = n.waiter_cpu.load(Ordering::Relaxed) as usize;
                        n.delivered_bits.store(word, Ordering::Release);
                        n.waiter_slot.store(NO_WAITER, Ordering::Release);
                        n.wait_state.store(WAIT_DELIVERED, Ordering::Release);
                        wake_target = Some((ThreadId::new(w_slot as usize, w_gen), w_cpu));
                    }
                }
            }
        });
    }
    if let Some((tid, pref_cpu)) = wake_target {
        let _ = wake_thread(tid, pref_cpu);
    }
    true
}

/// Wait for and atomically consume accumulated `Notification` bits (`CapRights::WAIT`).
pub fn notif_wait(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    notif_cap: CapPtr,
    non_blocking: bool,
    timeout_ticks: u64,
) -> Result<u64, SysError> {
    let now_epoch = current_lease_epoch();

    // 1. Lock-free 1-atomic-swap fast path (`0` locks, `0` context switches when bits pending!).
    let fast_res = with_pinned_cap(
        cspace_id,
        subj_label,
        notif_cap,
        CapRights::WAIT,
        now_epoch,
        |res| {
            if res.obj_kind != CapObjectKind::Notification {
                return Err(CapError::WrongObjectKind);
            }
            let n = &NOTIF_TABLE[res.obj_id as usize];
            if n.closed.load(Ordering::Acquire) {
                return Err(CapError::Revoked);
            }
            let word = n.pending_bits.swap(0, Ordering::AcqRel);
            Ok((res.obj_id as usize, res.expected_obj_gen, res.global_slot, word))
        },
    )
    .map_err(map_cap_err_to_sys)?;

    if fast_res.3 != 0 {
        return Ok(fast_res.3);
    }
    if non_blocking {
        return Err(SysError::WouldBlock);
    }
    check_blocking_ipc_allowed()?;

    let cpu_idx = current_cpu_idx();
    let cur_slot = current_slot_on_cpu(cpu_idx);
    let (_, _, cur_gen26) = tcb(cur_slot).sched.snapshot();
    let deadline = if timeout_ticks > 0 {
        rdtsc().saturating_add(timeout_ticks).max(1)
    } else {
        0
    };

    prepare_park_current();

    // 2. Register waiter while `notif_cap` is pinned; drop `in_flight_ops` BEFORE sleeping!
    let reg = with_pinned_cap(
        cspace_id,
        subj_label,
        notif_cap,
        CapRights::WAIT,
        now_epoch,
        |res| {
            if res.obj_kind != CapObjectKind::Notification {
                return Err(CapError::WrongObjectKind);
            }
            let n = &NOTIF_TABLE[res.obj_id as usize];
            let mut inner_res: Result<Option<u64>, SysError> = Ok(None);
            with_notif_lock(n, || {
                let (st, _, _, sgen) =
                    unpack_state_gen(slot_by_global(res.global_slot).state_gen.load(Ordering::Acquire));
                if st != SlotState::Live || sgen != notif_cap.slot_gen() || n.closed.load(Ordering::Acquire) {
                    inner_res = Err(SysError::Revoked);
                    return;
                }
                if current_thread_killed(cur_slot) {
                    inner_res = Err(SysError::Canceled);
                    return;
                }
                let word = n.pending_bits.swap(0, Ordering::AcqRel);
                if word != 0 {
                    inner_res = Ok(Some(word));
                    return;
                }
                // `AUD-K4b`: a delivered-but-not-yet-resumed waiter still owns `wait_state`.
                if n.waiter_slot.load(Ordering::Acquire) != NO_WAITER
                    || n.wait_state.load(Ordering::Acquire) != WAIT_IDLE
                {
                    inner_res = Err(SysError::Busy);
                    return;
                }
                n.delivered_bits.store(0, Ordering::Release);
                n.fail_errno.store(0, Ordering::Release);
                n.waiter_gen26.store(cur_gen26, Ordering::Release);
                n.waiter_cpu.store(cpu_idx as u16, Ordering::Release);
                n.waiter_cap_gslot.store(res.global_slot, Ordering::Release);
                n.wait_state.store(WAIT_BLOCKED, Ordering::Release);
                n.waiter_slot.store(cur_slot as u16, Ordering::SeqCst);
                // `AUD-K4`: Dekker re-check. A signaller that set `pending_bits` after the swap
                // above but read `waiter_slot` before this store saw no waiter and skipped the
                // wake; publish-then-recheck guarantees one side observes the other.
                fence(Ordering::SeqCst);
                let late = n.pending_bits.swap(0, Ordering::AcqRel);
                if late != 0 {
                    n.waiter_slot.store(NO_WAITER, Ordering::Release);
                    n.wait_state.store(WAIT_IDLE, Ordering::Release);
                    inner_res = Ok(Some(late));
                }
            });
            Ok((res.obj_id as usize, inner_res))
        },
    );

    let (notif_idx, reg_outcome) = match reg {
        Ok(pair) => pair,
        Err(ce) => {
            cancel_park_current();
            return Err(map_cap_err_to_sys(ce));
        }
    };
    match reg_outcome {
        Ok(Some(word)) => {
            cancel_park_current();
            return Ok(word);
        }
        Err(se) => {
            cancel_park_current();
            return Err(se);
        }
        Ok(None) => {}
    }

    let n = &NOTIF_TABLE[notif_idx];

    // 3. H1 condition wait loop (`INV-IPC-02` single-winner arbitration under `n.lock`).
    let mut first_iter = true;
    loop {
        if !first_iter {
            prepare_park_current();
            let mut terminal: Option<Result<u64, SysError>> = None;
            with_notif_lock(n, || match n.wait_state.load(Ordering::Acquire) {
                WAIT_DELIVERED => {
                    let word = n.delivered_bits.load(Ordering::Acquire);
                    n.wait_state.store(WAIT_IDLE, Ordering::Release);
                    terminal = Some(Ok(word));
                }
                WAIT_FAILED => {
                    let code = n.fail_errno.load(Ordering::Acquire);
                    n.wait_state.store(WAIT_IDLE, Ordering::Release);
                    terminal = Some(Err(errno_to_syserr(code)));
                }
                _ => {
                    let word = n.pending_bits.swap(0, Ordering::AcqRel);
                    if word != 0 {
                        n.waiter_slot.store(NO_WAITER, Ordering::Release);
                        n.wait_state.store(WAIT_IDLE, Ordering::Release);
                        terminal = Some(Ok(word));
                    } else if deadline != 0 && rdtsc() >= deadline {
                        n.waiter_slot.store(NO_WAITER, Ordering::Release);
                        n.wait_state.store(WAIT_IDLE, Ordering::Release);
                        terminal = Some(Err(SysError::TimedOut));
                    } else if current_thread_killed(cur_slot) {
                        // `AUD-K15`: a killed process must leave an untimed wait.
                        n.waiter_slot.store(NO_WAITER, Ordering::Release);
                        n.wait_state.store(WAIT_IDLE, Ordering::Release);
                        terminal = Some(Err(SysError::Canceled));
                    }
                }
            });
            if let Some(done) = terminal {
                cancel_park_current();
                tcb(cur_slot).ipc_deadline_tsc.store(0, Ordering::Release);
                return done;
            }
        }
        first_iter = false;

        if deadline != 0 {
            tcb(cur_slot)
                .ipc_deadline_tsc
                .store(deadline, Ordering::Release);
        }
        let _ = ipc_park_current();
        tcb(cur_slot).ipc_deadline_tsc.store(0, Ordering::Release);

        let mut terminal: Option<Result<u64, SysError>> = None;
        with_notif_lock(n, || match n.wait_state.load(Ordering::Acquire) {
            WAIT_DELIVERED => {
                let word = n.delivered_bits.load(Ordering::Acquire);
                n.wait_state.store(WAIT_IDLE, Ordering::Release);
                terminal = Some(Ok(word));
            }
            WAIT_FAILED => {
                let code = n.fail_errno.load(Ordering::Acquire);
                n.wait_state.store(WAIT_IDLE, Ordering::Release);
                terminal = Some(Err(errno_to_syserr(code)));
            }
            _ => {
                let word = n.pending_bits.swap(0, Ordering::AcqRel);
                if word != 0 {
                    n.waiter_slot.store(NO_WAITER, Ordering::Release);
                    n.wait_state.store(WAIT_IDLE, Ordering::Release);
                    terminal = Some(Ok(word));
                } else if deadline != 0 && rdtsc() >= deadline {
                    n.waiter_slot.store(NO_WAITER, Ordering::Release);
                    n.wait_state.store(WAIT_IDLE, Ordering::Release);
                    terminal = Some(Err(SysError::TimedOut));
                } else if current_thread_killed(cur_slot) {
                    n.waiter_slot.store(NO_WAITER, Ordering::Release);
                    n.wait_state.store(WAIT_IDLE, Ordering::Release);
                    terminal = Some(Err(SysError::Canceled));
                }
            }
        });
        if let Some(done) = terminal {
            return done;
        }
    }
}

#[inline]
fn current_thread_killed(cur_slot: usize) -> bool {
    let pid = tcb(cur_slot).process_id.load(Ordering::Relaxed);
    pid != 0 && crate::proc::is_process_kill_requested(pid)
}

#[inline]
fn errno_to_syserr(code: i32) -> SysError {
    match code as isize {
        -110 => SysError::Revoked,
        -116 => SysError::TimedOut,
        -125 => SysError::Canceled,
        _ => SysError::Pipe,
    }
}

pub(crate) fn on_notif_cap_revoked(obj_id: u32, expected_gen: u32, global_slot: u32) {
    let idx = obj_id as usize;
    if idx >= MAX_NOTIFICATIONS {
        return;
    }
    let n = &NOTIF_TABLE[idx];
    let mut wake_target: Option<(ThreadId, usize)> = None;
    with_notif_lock(n, || {
        if !n.allocated.load(Ordering::Acquire) || n.generation.load(Ordering::Acquire) != expected_gen {
            return;
        }
        if n.wait_state.load(Ordering::Acquire) == WAIT_BLOCKED
            && n.waiter_cap_gslot.load(Ordering::Acquire) == global_slot
        {
            let w_slot = n.waiter_slot.swap(NO_WAITER, Ordering::AcqRel);
            if w_slot != NO_WAITER {
                let w_gen = n.waiter_gen26.load(Ordering::Relaxed);
                let w_cpu = n.waiter_cpu.load(Ordering::Relaxed) as usize;
                n.fail_errno
                    .store(SysError::Revoked.as_raw() as i32, Ordering::Release);
                n.wait_state.store(WAIT_FAILED, Ordering::Release);
                wake_target = Some((ThreadId::new(w_slot as usize, w_gen), w_cpu));
            }
        }
    });
    if let Some((tid, pref_cpu)) = wake_target {
        let _ = wake_thread(tid, pref_cpu);
    }
}

pub(crate) fn destroy_notif(obj_id: u32, expected_gen: u32) {
    let idx = obj_id as usize;
    if idx >= MAX_NOTIFICATIONS {
        return;
    }
    let n = &NOTIF_TABLE[idx];
    let mut wake_target: Option<(ThreadId, usize)> = None;
    with_notif_lock(n, || {
        if !n.allocated.load(Ordering::Acquire) || n.generation.load(Ordering::Acquire) != expected_gen {
            return;
        }
        n.closed.store(true, Ordering::Release);
        if n.wait_state.load(Ordering::Acquire) == WAIT_BLOCKED {
            let w_slot = n.waiter_slot.swap(NO_WAITER, Ordering::AcqRel);
            if w_slot != NO_WAITER {
                let w_gen = n.waiter_gen26.load(Ordering::Relaxed);
                let w_cpu = n.waiter_cpu.load(Ordering::Relaxed) as usize;
                n.fail_errno
                    .store(SysError::Revoked.as_raw() as i32, Ordering::Release);
                n.wait_state.store(WAIT_FAILED, Ordering::Release);
                wake_target = Some((ThreadId::new(w_slot as usize, w_gen), w_cpu));
            }
        }
        let next_gen = next_slot_gen(expected_gen);
        n.generation.store(next_gen, Ordering::Release);
        n.pending_bits.store(0, Ordering::Release);
        n.allocated.store(false, Ordering::Release);
    });
    if let Some((tid, pref_cpu)) = wake_target {
        let _ = wake_thread(tid, pref_cpu);
    }
}

pub(crate) fn on_thread_exit_cleanup(slot: usize) {
    for n in &NOTIF_TABLE {
        if !n.allocated.load(Ordering::Relaxed) {
            continue;
        }
        if n.waiter_slot.load(Ordering::Relaxed) == (slot as u16) {
            with_notif_lock(n, || {
                if n.waiter_slot.load(Ordering::Acquire) == (slot as u16) {
                    n.waiter_slot.store(NO_WAITER, Ordering::Release);
                    n.wait_state.store(WAIT_IDLE, Ordering::Release);
                }
            });
        }
    }
}
