// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Per-`TCB` Bound `Notification` Support (`Milestone 2C-2` — `seL4_TCB_BindNotification` semantics).
//!
//! Allows an isolated userspace server or driver thread to bind a single `Notification` object to
//! its `TCB` so synchronous `Endpoint::recv` / `reply_recv` returns both synchronous IPC calls AND
//! asynchronous IRQ/doorbell signals (`IPC_OP_BOUND_NOTIF`, badge = accumulated notification bits)
//! on one `0 Hz` blocking point without `SYS_NOTIFY_WAIT` polling timers.
//!
//! Lock ordering invariant: `ep_recv`/`ep_reply_recv` hold `ep.lock` and invoke
//! `poll_bound_notif_for_recv`, which MUST remain strictly lock-free (`0` `notif.lock` acquisition).

use core::sync::atomic::{fence, AtomicU16, AtomicU32, Ordering};

use crate::cspace::cap::{unpack_state_gen, CSpaceId, CapError, CapObjectKind, CapPtr, SlotState};
use crate::cspace::{slot_by_global, with_pinned_cap};
use crate::gdt_tss::MAX_CPUS;
use crate::ipc::notif::{
    with_notif_lock, MAX_NOTIFICATIONS, NOTIF_TABLE, NO_WAITER, WAIT_IDLE,
};
use crate::ipc::{
    map_cap_err_to_sys, MSG_FLAG_NON_BLOCKING, NOTIFY_FLAG_BIND_TCB, NOTIFY_FLAG_UNBIND_TCB,
};
use crate::sched::{current_cpu_idx, current_slot_on_cpu, tcb, wake_thread, TOTAL_TCB_SLOTS};
use crate::sched::state::ThreadId;
use crate::security::{current_lease_epoch, CapRights, SecLabel};
use crate::uaccess::SysError;

const NO_NOTIF: u16 = u16::MAX;

#[repr(C, align(16))]
struct BoundTcbEntry {
    notif_idx: AtomicU16,
    notif_gen: AtomicU32,
    cap_gslot: AtomicU32,
}

impl BoundTcbEntry {
    const fn empty() -> Self {
        Self {
            notif_idx: AtomicU16::new(NO_NOTIF),
            notif_gen: AtomicU32::new(0),
            cap_gslot: AtomicU32::new(u32::MAX),
        }
    }
}

static BOUND_TCB: [BoundTcbEntry; TOTAL_TCB_SLOTS] =
    [const { BoundTcbEntry::empty() }; TOTAL_TCB_SLOTS];

#[inline(always)]
pub(super) const fn pack_bound_target(slot: usize, gen26: u32, cpu_idx: usize) -> u64 {
    (1u64 << 63)
        | (((cpu_idx as u64) & 0xff) << 48)
        | (((gen26 as u64) & 0x03ff_ffff) << 16)
        | (slot as u64)
}

#[inline(always)]
pub(super) fn unpack_bound_target(raw: u64) -> Option<(usize, u32, usize)> {
    if raw == 0 {
        return None;
    }
    let slot = (raw & 0xffff) as usize;
    let gen26 = ((raw >> 16) & 0x03ff_ffff) as u32;
    let cpu_hint = ((raw >> 48) & 0xff) as usize;
    let pref_cpu = if (MAX_CPUS..TOTAL_TCB_SLOTS).contains(&slot) {
        let lc = tcb(slot).last_cpu.load(Ordering::Relaxed);
        if lc < MAX_CPUS { lc } else { cpu_hint }
    } else {
        cpu_hint
    };
    Some((slot, gen26, pref_cpu))
}

#[inline]
pub(super) fn clear_slot_matching(slot: usize, notif_idx: u16, notif_gen: u32) {
    if slot >= TOTAL_TCB_SLOTS {
        return;
    }
    let e = &BOUND_TCB[slot];
    if e.notif_idx.load(Ordering::Acquire) == notif_idx
        && e.notif_gen.load(Ordering::Acquire) == notif_gen
    {
        e.notif_idx.store(NO_NOTIF, Ordering::Release);
        e.notif_gen.store(0, Ordering::Release);
        e.cap_gslot.store(u32::MAX, Ordering::Release);
    }
}

#[inline(never)]
pub fn sys_notify_bind_or_unbind(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    notif_cap: CapPtr,
    flags: u64,
) -> Result<u64, SysError> {
    let bind = (flags & NOTIFY_FLAG_BIND_TCB) != 0;
    let unbind = (flags & NOTIFY_FLAG_UNBIND_TCB) != 0;
    let allowed = NOTIFY_FLAG_BIND_TCB | NOTIFY_FLAG_UNBIND_TCB;
    if (flags & !allowed) != 0 || (flags & (MSG_FLAG_NON_BLOCKING as u64)) != 0 || bind == unbind {
        return Err(SysError::Inval);
    }

    let cpu_idx = current_cpu_idx();
    let cur_slot = current_slot_on_cpu(cpu_idx);
    if !(MAX_CPUS..TOTAL_TCB_SLOTS).contains(&cur_slot) {
        return Err(SysError::Inval);
    }
    let (_, _, cur_gen26) = tcb(cur_slot).sched.snapshot();
    let now_epoch = current_lease_epoch();

    if unbind {
        if notif_cap.0 == 0 {
            unbind_current_slot(cur_slot);
            return Ok(0);
        }
        let mut inner_err: Option<SysError> = None;
        let pin_res = with_pinned_cap(
            cspace_id,
            subj_label,
            notif_cap,
            CapRights::WAIT,
            now_epoch,
            |res| {
                if res.obj_kind != CapObjectKind::Notification {
                    return Err(CapError::WrongObjectKind);
                }
                let idx = res.obj_id as usize;
                let n = &NOTIF_TABLE[idx];
                with_notif_lock(n, || {
                    let (st, _, _, sgen) = unpack_state_gen(
                        slot_by_global(res.global_slot).state_gen.load(Ordering::Acquire),
                    );
                    if st != SlotState::Live
                        || sgen != notif_cap.slot_gen()
                        || !n.allocated.load(Ordering::Acquire)
                        || n.closed.load(Ordering::Acquire)
                    {
                        inner_err = Some(SysError::Revoked);
                        return;
                    }
                    let bt = n.bound_target.load(Ordering::Acquire);
                    if bt == 0 || (bt & 0xffff) as usize != cur_slot {
                        inner_err = Some(SysError::Inval);
                        return;
                    }
                    n.bound_target.store(0, Ordering::Release);
                    n.bound_cap_gslot.store(u32::MAX, Ordering::Release);
                    clear_slot_matching(cur_slot, idx as u16, res.expected_obj_gen);
                });
                Ok(())
            },
        )
        .map_err(map_cap_err_to_sys);
        pin_res?;
        if let Some(se) = inner_err {
            return Err(se);
        }
        return Ok(0);
    }

    // Bind path: verify current thread does not already own a live bound `Notification`.
    let existing = BOUND_TCB[cur_slot].notif_idx.load(Ordering::Acquire);
    if existing != NO_NOTIF {
        let ex_gen = BOUND_TCB[cur_slot].notif_gen.load(Ordering::Acquire);
        if crate::ipc::notif::is_notif_live(existing as u32, ex_gen) {
            return Err(SysError::Busy);
        }
        clear_slot_matching(cur_slot, existing, ex_gen);
    }

    let mut inner_err: Option<SysError> = None;
    let mut wake_self: Option<ThreadId> = None;
    let pin_res = with_pinned_cap(
        cspace_id,
        subj_label,
        notif_cap,
        CapRights::WAIT,
        now_epoch,
        |res| {
            if res.obj_kind != CapObjectKind::Notification {
                return Err(CapError::WrongObjectKind);
            }
            let idx = res.obj_id as usize;
            let n = &NOTIF_TABLE[idx];
            with_notif_lock(n, || {
                let (st, _, _, sgen) = unpack_state_gen(
                    slot_by_global(res.global_slot).state_gen.load(Ordering::Acquire),
                );
                if st != SlotState::Live
                    || sgen != notif_cap.slot_gen()
                    || !n.allocated.load(Ordering::Acquire)
                    || n.closed.load(Ordering::Acquire)
                {
                    inner_err = Some(SysError::Revoked);
                    return;
                }
                if n.bound_target.load(Ordering::Acquire) != 0
                    || n.waiter_slot.load(Ordering::Acquire) != NO_WAITER
                    || n.wait_state.load(Ordering::Acquire) != WAIT_IDLE
                {
                    inner_err = Some(SysError::Busy);
                    return;
                }
                let e = &BOUND_TCB[cur_slot];
                e.notif_gen.store(res.expected_obj_gen, Ordering::Release);
                e.cap_gslot.store(res.global_slot, Ordering::Release);
                e.notif_idx.store(idx as u16, Ordering::Release);
                n.bound_cap_gslot.store(res.global_slot, Ordering::Release);
                n.bound_target
                    .store(pack_bound_target(cur_slot, cur_gen26, cpu_idx), Ordering::SeqCst);
                fence(Ordering::SeqCst);
                if n.pending_bits.load(Ordering::SeqCst) != 0 {
                    wake_self = Some(ThreadId::new(cur_slot, cur_gen26));
                }
            });
            Ok(())
        },
    )
    .map_err(map_cap_err_to_sys);
    pin_res?;
    if let Some(se) = inner_err {
        return Err(se);
    }
    if let Some(tid) = wake_self {
        let _ = wake_thread(tid, cpu_idx);
    }
    Ok(0)
}

fn unbind_current_slot(slot: usize) {
    let e = &BOUND_TCB[slot];
    let idx = e.notif_idx.load(Ordering::Acquire);
    if idx == NO_NOTIF || (idx as usize) >= MAX_NOTIFICATIONS {
        return;
    }
    let exp_gen = e.notif_gen.load(Ordering::Acquire);
    let n = &NOTIF_TABLE[idx as usize];
    with_notif_lock(n, || {
        if n.allocated.load(Ordering::Acquire) && n.generation.load(Ordering::Acquire) == exp_gen {
            let bt = n.bound_target.load(Ordering::Acquire);
            if bt != 0 && (bt & 0xffff) as usize == slot {
                n.bound_target.store(0, Ordering::Release);
                n.bound_cap_gslot.store(u32::MAX, Ordering::Release);
            }
        }
        clear_slot_matching(slot, idx, exp_gen);
    });
}

/// Lock-free bound-notification poll invoked inside `ep_recv` / `ep_reply_recv` (`ep.lock` held).
#[inline]
pub(crate) fn poll_bound_notif_for_recv(cur_slot: usize) -> Option<u64> {
    if !(MAX_CPUS..TOTAL_TCB_SLOTS).contains(&cur_slot) {
        return None;
    }
    let e = &BOUND_TCB[cur_slot];
    let idx = e.notif_idx.load(Ordering::Acquire);
    if idx == NO_NOTIF || (idx as usize) >= MAX_NOTIFICATIONS {
        return None;
    }
    let exp_gen = e.notif_gen.load(Ordering::Acquire);
    let n = &NOTIF_TABLE[idx as usize];
    if !n.allocated.load(Ordering::Acquire)
        || n.closed.load(Ordering::Acquire)
        || n.generation.load(Ordering::Acquire) != exp_gen
    {
        return None;
    }
    let bt = n.bound_target.load(Ordering::Acquire);
    if bt == 0 || (bt & 0xffff) as usize != cur_slot {
        return None;
    }
    let bits = n.pending_bits.swap(0, Ordering::SeqCst);
    if bits != 0 {
        Some(bits)
    } else {
        None
    }
}

pub(crate) fn on_thread_exit_cleanup(slot: usize) {
    if !(MAX_CPUS..TOTAL_TCB_SLOTS).contains(&slot) {
        return;
    }
    unbind_current_slot(slot);
}
