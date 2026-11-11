// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `Endpoint` Storage, Intrusive $O(1)$ Wait Queues, Blocking Resolution & Lifecycle Teardown.
//!
//! Separated from `endpoint.rs` (`send`/`recv`/`call`/`reply` data-plane operations) to uphold
//! strict module responsibility isolation and `< 800`-line file bounds (`ADR-002`, `INV-IPC-01..07`).

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::arch::{irq_restore, irq_save_cli, rdtsc};
use crate::cspace::cap::{next_slot_gen, CSpaceId, CapError, CapObjectKind, CapPtr};
use crate::cspace::{cspace_owner_label, insert_root_cap};
use crate::gdt_tss::MAX_CPUS;
use crate::ipc::MAX_IPC_EXTRA_BYTES;
use crate::ipc::MsgTag;
use crate::percpu::read_gs_u64_at_8;
use crate::sched::donate::revert_donation_for_caller;
use crate::sched::state::ThreadId;
use crate::sched::{
    cancel_park_current, ipc_park_current, prepare_park_current, tcb, wake_thread, TOTAL_TCB_SLOTS,
    WORKER_POOL_CAP,
};
use crate::security::{CapRights, SecDomain, SecFlags, SecLabel};
use crate::uaccess::SysError;
use crate::vspace::tlb::poll_and_ack_pending_shootdown;

pub const MAX_ENDPOINTS: usize = 32;
pub(super) const NO_SLOT: u16 = u16::MAX;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WaitPhase {
    Idle = 0,
    WaitingSend = 1,
    WaitingRecv = 2,
    WaitingReply = 3,
    Delivered = 4,
    Failed = 5,
}

#[repr(C, align(64))]
pub(super) struct TcbIpcSlot {
    pub(super) wait_phase: WaitPhase,
    pub(super) is_call: bool,
    pub(super) prev_waiter: u16,
    pub(super) next_waiter: u16,
    pub(super) wait_cpu: u16,
    pub(super) ep_id: u16,
    pub(super) ep_gen: u32,
    pub(super) cap_gslot: u32,
    pub(super) fail_err: SysError,
    pub(super) call_seq: u32,
    pub(super) replier_slot: u16,
    pub(super) replier_gen26: u32,
    pub(super) sender_cspace: CSpaceId,
    pub(super) receiver_cspace: CSpaceId,
    pub(super) sender_label: SecLabel,
    pub(super) sender_badge: u64,
    pub(super) tag: MsgTag,
    pub(super) mrs: [u64; 4],
    pub(super) grant_req: Option<(CapPtr, CapRights, u32, u64)>,
    pub(super) transferred_cap: CapPtr,
    pub(super) reply_token: u64,
    pub(super) extra_len: u16,
    pub(super) extra_buf: [u8; MAX_IPC_EXTRA_BYTES],
}

impl TcbIpcSlot {
    const fn new() -> Self {
        Self {
            wait_phase: WaitPhase::Idle,
            is_call: false,
            prev_waiter: NO_SLOT,
            next_waiter: NO_SLOT,
            wait_cpu: 0,
            ep_id: NO_SLOT,
            ep_gen: 0,
            cap_gslot: u32::MAX,
            fail_err: SysError::Inval,
            call_seq: 1,
            replier_slot: NO_SLOT,
            replier_gen26: 0,
            sender_cspace: 0,
            receiver_cspace: 0,
            sender_label: SecLabel::new(SecDomain::Kernel, SecFlags::NONE, 0, 0),
            sender_badge: 0,
            tag: MsgTag::EMPTY,
            mrs: [0; 4],
            grant_req: None,
            transferred_cap: CapPtr::NULL,
            reply_token: 0,
            extra_len: 0,
            extra_buf: [0; MAX_IPC_EXTRA_BYTES],
        }
    }
}

pub(super) struct IpcScratchPool(pub(super) UnsafeCell<[TcbIpcSlot; TOTAL_TCB_SLOTS]>);
// SAFETY: Slot `s` is mutated either exclusively by thread `s` while `wait_phase == Idle | Delivered | Failed`,
// or under the owning `Endpoint` spinlock (`ENDPOINTS[ep_id].lock` with `CLI`) while `s` is queued/waiting.
unsafe impl Sync for IpcScratchPool {}

pub(super) static IPC_SCRATCH: IpcScratchPool =
    IpcScratchPool(UnsafeCell::new([const { TcbIpcSlot::new() }; TOTAL_TCB_SLOTS]));

pub(super) struct EndpointInner {
    pub(super) closed: bool,
    pub(super) server_dead: bool,
    pub(super) send_head: u16,
    pub(super) send_tail: u16,
    pub(super) recv_head: u16,
    pub(super) recv_tail: u16,
    pub(super) last_server_slot: u16,
    pub(super) last_server_gen26: u32,
}

impl EndpointInner {
    pub(super) const fn new() -> Self {
        Self {
            closed: false,
            server_dead: false,
            send_head: NO_SLOT,
            send_tail: NO_SLOT,
            recv_head: NO_SLOT,
            recv_tail: NO_SLOT,
            last_server_slot: NO_SLOT,
            last_server_gen26: 0,
        }
    }
}

#[repr(C, align(64))]
pub(super) struct EndpointSlot {
    pub(super) allocated: AtomicBool,
    pub(super) lock: AtomicBool,
    pub(super) generation: AtomicU32,
    pub(super) inner: UnsafeCell<EndpointInner>,
}

// SAFETY: `inner` is mutated strictly under `EndpointSlot.lock` with `RFLAGS.IF == 0`.
unsafe impl Sync for EndpointSlot {}

pub(super) static ENDPOINTS: [EndpointSlot; MAX_ENDPOINTS] = [const {
    EndpointSlot {
        allocated: AtomicBool::new(false),
        lock: AtomicBool::new(false),
        generation: AtomicU32::new(1),
        inner: UnsafeCell::new(EndpointInner::new()),
    }
}; MAX_ENDPOINTS];

#[inline]
pub(super) fn with_ep_lock<R>(
    ep: &EndpointSlot,
    f: impl FnOnce(&mut EndpointInner, &mut [TcbIpcSlot; TOTAL_TCB_SLOTS]) -> R,
) -> R {
    let saved_if = irq_save_cli();
    let cpu_idx = read_gs_u64_at_8() as usize;
    while ep
        .lock
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        poll_and_ack_pending_shootdown(cpu_idx);
        core::hint::spin_loop();
    }
    // SAFETY: Exclusive per-endpoint lock + `CLI` held; `IPC_SCRATCH` slots belonging to this
    // endpoint's queues/waiters are serialized by `ep.lock`.
    let out = unsafe { f(&mut *ep.inner.get(), &mut *IPC_SCRATCH.0.get()) };
    ep.lock.store(false, Ordering::Release);
    irq_restore(saved_if);
    out
}

#[inline(always)]
pub const fn pack_reply_token(
    ep_id: u16,
    caller_slot: u16,
    caller_gen26: u32,
    call_seq: u32,
) -> u64 {
    (1u64 << 63)
        | (((ep_id as u64) & 0x1f) << 58)
        | (((call_seq as u64) & 0xffff) << 42)
        | (((caller_gen26 as u64) & 0x03ff_ffff) << 16)
        | (caller_slot as u64)
}

#[inline(always)]
pub const fn unpack_reply_token(raw: u64) -> Option<(usize, usize, u32, u32)> {
    if (raw >> 63) == 0 {
        return None;
    }
    let ep_id = ((raw >> 58) & 0x1f) as usize;
    let call_seq = ((raw >> 42) & 0xffff) as u32;
    let gen26 = ((raw >> 16) & 0x03ff_ffff) as u32;
    let slot = (raw & 0xffff) as usize;
    if call_seq == 0 {
        return None;
    }
    Some((ep_id, slot, gen26, call_seq))
}

#[inline(always)]
pub(super) fn next_seq16(cur: u32) -> u32 {
    if cur >= 0xfffe {
        1
    } else {
        cur + 1
    }
}

pub(super) fn push_send_q(
    inner: &mut EndpointInner,
    slots: &mut [TcbIpcSlot; TOTAL_TCB_SLOTS],
    s: u16,
) {
    let idx = s as usize;
    slots[idx].prev_waiter = inner.send_tail;
    slots[idx].next_waiter = NO_SLOT;
    if inner.send_tail != NO_SLOT {
        slots[inner.send_tail as usize].next_waiter = s;
    } else {
        inner.send_head = s;
    }
    inner.send_tail = s;
}

pub(super) fn pop_send_q(
    inner: &mut EndpointInner,
    slots: &mut [TcbIpcSlot; TOTAL_TCB_SLOTS],
) -> Option<usize> {
    let h = inner.send_head;
    if h == NO_SLOT {
        return None;
    }
    let nxt = slots[h as usize].next_waiter;
    inner.send_head = nxt;
    if nxt != NO_SLOT {
        slots[nxt as usize].prev_waiter = NO_SLOT;
    } else {
        inner.send_tail = NO_SLOT;
    }
    slots[h as usize].prev_waiter = NO_SLOT;
    slots[h as usize].next_waiter = NO_SLOT;
    Some(h as usize)
}

pub(super) fn unlink_send_q(
    inner: &mut EndpointInner,
    slots: &mut [TcbIpcSlot; TOTAL_TCB_SLOTS],
    s: u16,
) {
    let idx = s as usize;
    let prv = slots[idx].prev_waiter;
    let nxt = slots[idx].next_waiter;
    if prv != NO_SLOT {
        slots[prv as usize].next_waiter = nxt;
    } else if inner.send_head == s {
        inner.send_head = nxt;
    }
    if nxt != NO_SLOT {
        slots[nxt as usize].prev_waiter = prv;
    } else if inner.send_tail == s {
        inner.send_tail = prv;
    }
    slots[idx].prev_waiter = NO_SLOT;
    slots[idx].next_waiter = NO_SLOT;
}

pub(super) fn push_recv_q(
    inner: &mut EndpointInner,
    slots: &mut [TcbIpcSlot; TOTAL_TCB_SLOTS],
    s: u16,
) {
    let idx = s as usize;
    slots[idx].prev_waiter = inner.recv_tail;
    slots[idx].next_waiter = NO_SLOT;
    if inner.recv_tail != NO_SLOT {
        slots[inner.recv_tail as usize].next_waiter = s;
    } else {
        inner.recv_head = s;
    }
    inner.recv_tail = s;
}

pub(super) fn pop_recv_q(
    inner: &mut EndpointInner,
    slots: &mut [TcbIpcSlot; TOTAL_TCB_SLOTS],
) -> Option<usize> {
    let h = inner.recv_head;
    if h == NO_SLOT {
        return None;
    }
    let nxt = slots[h as usize].next_waiter;
    inner.recv_head = nxt;
    if nxt != NO_SLOT {
        slots[nxt as usize].prev_waiter = NO_SLOT;
    } else {
        inner.recv_tail = NO_SLOT;
    }
    slots[h as usize].prev_waiter = NO_SLOT;
    slots[h as usize].next_waiter = NO_SLOT;
    Some(h as usize)
}

pub(super) fn unlink_recv_q(
    inner: &mut EndpointInner,
    slots: &mut [TcbIpcSlot; TOTAL_TCB_SLOTS],
    s: u16,
) {
    let idx = s as usize;
    let prv = slots[idx].prev_waiter;
    let nxt = slots[idx].next_waiter;
    if prv != NO_SLOT {
        slots[prv as usize].next_waiter = nxt;
    } else if inner.recv_head == s {
        inner.recv_head = nxt;
    }
    if nxt != NO_SLOT {
        slots[nxt as usize].prev_waiter = prv;
    } else if inner.recv_tail == s {
        inner.recv_tail = prv;
    }
    slots[idx].prev_waiter = NO_SLOT;
    slots[idx].next_waiter = NO_SLOT;
}

#[inline(always)]
pub fn is_endpoint_live(obj_id: u32, expected_gen: u32) -> bool {
    let idx = obj_id as usize;
    idx < MAX_ENDPOINTS
        && ENDPOINTS[idx].allocated.load(Ordering::Acquire)
        && ENDPOINTS[idx].generation.load(Ordering::Acquire) == expected_gen
}

/// Create a synchronous `Endpoint` object and bind its root capability into `cspace_id`.
pub fn ep_create(
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

    for (idx, ep) in ENDPOINTS.iter().enumerate() {
        if ep
            .allocated
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let cur_gen = ep.generation.load(Ordering::Acquire).max(1);
            with_ep_lock(ep, |inner, _| {
                *inner = EndpointInner::new();
            });
            match insert_root_cap(
                cspace_id,
                CapObjectKind::Endpoint,
                idx as u32,
                cur_gen,
                CapRights::ENDPOINT_ROOT,
                obj_label,
                lease_epoch,
                0,
            ) {
                Ok(cap) => return Ok((cap, idx as u32)),
                Err(e) => {
                    ep.allocated.store(false, Ordering::Release);
                    return Err(e);
                }
            }
        }
    }
    Err(CapError::OutOfSlots)
}

#[inline]
fn try_resolve_ipc_wait(
    inner: &mut EndpointInner,
    slots: &mut [TcbIpcSlot; TOTAL_TCB_SLOTS],
    cur_slot: usize,
    deadline: u64,
) -> Option<Result<(), SysError>> {
    match slots[cur_slot].wait_phase {
        WaitPhase::Delivered => {
            slots[cur_slot].wait_phase = WaitPhase::Idle;
            Some(Ok(()))
        }
        WaitPhase::Failed => {
            let e = slots[cur_slot].fail_err;
            slots[cur_slot].wait_phase = WaitPhase::Idle;
            Some(Err(e))
        }
        WaitPhase::WaitingSend => {
            if deadline != 0 && rdtsc() >= deadline {
                unlink_send_q(inner, slots, cur_slot as u16);
                slots[cur_slot].call_seq = next_seq16(slots[cur_slot].call_seq);
                slots[cur_slot].wait_phase = WaitPhase::Idle;
                let _ = revert_donation_for_caller(cur_slot);
                Some(Err(SysError::TimedOut))
            } else {
                None
            }
        }
        WaitPhase::WaitingRecv => {
            if deadline != 0 && rdtsc() >= deadline {
                unlink_recv_q(inner, slots, cur_slot as u16);
                slots[cur_slot].wait_phase = WaitPhase::Idle;
                Some(Err(SysError::TimedOut))
            } else {
                None
            }
        }
        WaitPhase::WaitingReply => {
            if deadline != 0 && rdtsc() >= deadline {
                slots[cur_slot].call_seq = next_seq16(slots[cur_slot].call_seq);
                slots[cur_slot].replier_slot = NO_SLOT;
                slots[cur_slot].wait_phase = WaitPhase::Idle;
                let _ = revert_donation_for_caller(cur_slot);
                Some(Err(SysError::TimedOut))
            } else {
                None
            }
        }
        WaitPhase::Idle => Some(Err(SysError::Canceled)),
    }
}

/// Wait on `cur_slot`'s `TcbIpcSlot` until `Delivered` or `Failed`, or expire on `deadline`.
pub(super) fn wait_on_endpoint_slot(
    ep_idx: usize,
    cur_slot: usize,
    deadline: u64,
) -> Result<(), SysError> {
    let ep = &ENDPOINTS[ep_idx];
    let mut first_iter = true;
    loop {
        if !first_iter {
            prepare_park_current();
            let done = with_ep_lock(ep, |inner, slots| {
                try_resolve_ipc_wait(inner, slots, cur_slot, deadline)
            });
            if let Some(res) = done {
                cancel_park_current();
                tcb(cur_slot).ipc_deadline_tsc.store(0, Ordering::Release);
                return res;
            }
        }
        first_iter = false;

        if deadline != 0 {
            tcb(cur_slot)
                .ipc_deadline_tsc
                .store(deadline, Ordering::Release);
        }
        let _ = ipc_park_current();
        if deadline != 0 {
            tcb(cur_slot).ipc_deadline_tsc.store(0, Ordering::Release);
        }

        let done = with_ep_lock(ep, |inner, slots| {
            try_resolve_ipc_wait(inner, slots, cur_slot, deadline)
        });
        if let Some(res) = done {
            return res;
        }
    }
}

pub(crate) fn on_endpoint_cap_revoked(obj_id: u32, expected_gen: u32, global_slot: u32) {
    let idx = obj_id as usize;
    if idx >= MAX_ENDPOINTS {
        return;
    }
    let ep = &ENDPOINTS[idx];
    let mut wakes: [Option<(ThreadId, usize)>; WORKER_POOL_CAP] = [None; WORKER_POOL_CAP];
    let mut wcnt = 0usize;

    with_ep_lock(ep, |inner, slots| {
        if !ep.allocated.load(Ordering::Acquire)
            || ep.generation.load(Ordering::Acquire) != expected_gen
        {
            return;
        }
        for w in 0..WORKER_POOL_CAP {
            let s = MAX_CPUS + w;
            if slots[s].ep_id == (idx as u16)
                && slots[s].ep_gen == expected_gen
                && slots[s].cap_gslot == global_slot
            {
                match slots[s].wait_phase {
                    WaitPhase::WaitingSend => unlink_send_q(inner, slots, s as u16),
                    WaitPhase::WaitingRecv => unlink_recv_q(inner, slots, s as u16),
                    WaitPhase::WaitingReply => {
                        slots[s].call_seq = next_seq16(slots[s].call_seq);
                        slots[s].replier_slot = NO_SLOT;
                    }
                    _ => continue,
                }
                let _ = revert_donation_for_caller(s);
                slots[s].fail_err = SysError::Revoked;
                slots[s].wait_phase = WaitPhase::Failed;
                let (_, _, gen26) = tcb(s).sched.snapshot();
                wakes[wcnt] = Some((ThreadId::new(s, gen26), slots[s].wait_cpu as usize));
                wcnt += 1;
            }
        }
    });

    for (tid, pcpu) in wakes.into_iter().flatten() {
        let _ = wake_thread(tid, pcpu);
    }
}

pub(crate) fn destroy_endpoint(obj_id: u32, expected_gen: u32) {
    let idx = obj_id as usize;
    if idx >= MAX_ENDPOINTS {
        return;
    }
    let ep = &ENDPOINTS[idx];
    let mut wakes: [Option<(ThreadId, usize)>; WORKER_POOL_CAP] = [None; WORKER_POOL_CAP];
    let mut wcnt = 0usize;

    with_ep_lock(ep, |inner, slots| {
        if !ep.allocated.load(Ordering::Acquire)
            || ep.generation.load(Ordering::Acquire) != expected_gen
        {
            return;
        }
        inner.closed = true;
        for w in 0..WORKER_POOL_CAP {
            let s = MAX_CPUS + w;
            if slots[s].ep_id == (idx as u16) && slots[s].ep_gen == expected_gen {
                match slots[s].wait_phase {
                    WaitPhase::WaitingSend => unlink_send_q(inner, slots, s as u16),
                    WaitPhase::WaitingRecv => unlink_recv_q(inner, slots, s as u16),
                    WaitPhase::WaitingReply => {
                        slots[s].call_seq = next_seq16(slots[s].call_seq);
                        slots[s].replier_slot = NO_SLOT;
                    }
                    _ => continue,
                }
                let _ = revert_donation_for_caller(s);
                slots[s].fail_err = SysError::Revoked;
                slots[s].wait_phase = WaitPhase::Failed;
                let (_, _, gen26) = tcb(s).sched.snapshot();
                wakes[wcnt] = Some((ThreadId::new(s, gen26), slots[s].wait_cpu as usize));
                wcnt += 1;
            }
        }
        let next_gen = next_slot_gen(expected_gen);
        ep.generation.store(next_gen, Ordering::Release);
        ep.allocated.store(false, Ordering::Release);
    });

    for (tid, pcpu) in wakes.into_iter().flatten() {
        let _ = wake_thread(tid, pcpu);
    }
}

pub(crate) fn on_delegated_server_exit(obj_id: u32, expected_gen: u32, _pid: u32) {
    let idx = obj_id as usize;
    if idx >= MAX_ENDPOINTS {
        return;
    }
    let ep = &ENDPOINTS[idx];
    let mut wakes: [Option<(ThreadId, usize)>; WORKER_POOL_CAP] = [None; WORKER_POOL_CAP];
    let mut wcnt = 0usize;

    with_ep_lock(ep, |inner, slots| {
        if !ep.allocated.load(Ordering::Acquire)
            || ep.generation.load(Ordering::Acquire) != expected_gen
        {
            return;
        }
        if inner.recv_head == NO_SLOT {
            inner.server_dead = true;
            while let Some(s) = pop_send_q(inner, slots) {
                let _ = revert_donation_for_caller(s);
                slots[s].call_seq = next_seq16(slots[s].call_seq);
                slots[s].fail_err = SysError::Pipe;
                slots[s].wait_phase = WaitPhase::Failed;
                let (_, _, gen26) = tcb(s).sched.snapshot();
                wakes[wcnt] = Some((ThreadId::new(s, gen26), slots[s].wait_cpu as usize));
                wcnt += 1;
            }
        }
    });

    for (tid, pcpu) in wakes.into_iter().flatten() {
        let _ = wake_thread(tid, pcpu);
    }
}

pub(crate) fn on_thread_exit_cleanup(dying_slot: usize, pid: u32) {
    if !(MAX_CPUS..TOTAL_TCB_SLOTS).contains(&dying_slot) {
        return;
    }
    let (_, _, dying_gen26) = tcb(dying_slot).sched.snapshot();
    for (idx, ep) in ENDPOINTS.iter().enumerate() {
        if !ep.allocated.load(Ordering::Relaxed) {
            continue;
        }
        let mut wakes: [Option<(ThreadId, usize)>; WORKER_POOL_CAP] = [None; WORKER_POOL_CAP];
        let mut wcnt = 0usize;

        with_ep_lock(ep, |inner, slots| {
            if !ep.allocated.load(Ordering::Acquire) {
                return;
            }
            if slots[dying_slot].ep_id == (idx as u16) {
                match slots[dying_slot].wait_phase {
                    WaitPhase::WaitingSend => unlink_send_q(inner, slots, dying_slot as u16),
                    WaitPhase::WaitingRecv => unlink_recv_q(inner, slots, dying_slot as u16),
                    _ => {}
                }
                slots[dying_slot].wait_phase = WaitPhase::Idle;
                slots[dying_slot].call_seq = next_seq16(slots[dying_slot].call_seq);
            }

            if inner.last_server_slot == (dying_slot as u16)
                && inner.last_server_gen26 == dying_gen26
            {
                inner.last_server_slot = NO_SLOT;
                if inner.recv_head == NO_SLOT {
                    if pid != 0 {
                        inner.server_dead = true;
                    }
                    while let Some(s) = pop_send_q(inner, slots) {
                        let _ = revert_donation_for_caller(s);
                        slots[s].call_seq = next_seq16(slots[s].call_seq);
                        slots[s].fail_err = SysError::Pipe;
                        slots[s].wait_phase = WaitPhase::Failed;
                        let (_, _, gen26) = tcb(s).sched.snapshot();
                        wakes[wcnt] = Some((ThreadId::new(s, gen26), slots[s].wait_cpu as usize));
                        wcnt += 1;
                    }
                }
            }

            for w in 0..WORKER_POOL_CAP {
                let s = MAX_CPUS + w;
                if slots[s].ep_id == (idx as u16)
                    && slots[s].wait_phase == WaitPhase::WaitingReply
                    && slots[s].replier_slot == (dying_slot as u16)
                    && slots[s].replier_gen26 == dying_gen26
                {
                    let _ = revert_donation_for_caller(s);
                    slots[s].call_seq = next_seq16(slots[s].call_seq);
                    slots[s].replier_slot = NO_SLOT;
                    slots[s].fail_err = SysError::Pipe;
                    slots[s].wait_phase = WaitPhase::Failed;
                    let (_, _, gen26) = tcb(s).sched.snapshot();
                    wakes[wcnt] = Some((ThreadId::new(s, gen26), slots[s].wait_cpu as usize));
                    wcnt += 1;
                }
            }
        });

        for (tid, pcpu) in wakes.into_iter().flatten() {
            let _ = wake_thread(tid, pcpu);
        }
    }
}
