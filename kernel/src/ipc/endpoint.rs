// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Synchronous Capability-Based `Endpoint` IPC Data-Plane (`send`, `recv`, `call`, `reply`, `reply_recv`).
//!
//! Queue storage, `ReplyToken` packing, H1 park/wake resolution, and revocation/exit teardown live
//! in `endpoint_state.rs`.

use core::sync::atomic::Ordering;

use crate::arch::rdtsc;
use crate::cspace::cap::{
    unpack_state_gen, CSpaceId, CapError, CapObjectKind, CapPtr, ResolvedCap, SlotState,
};
use crate::cspace::{cspace_lookup_fast, cspace_transfer, slot_by_global, with_pinned_cap};
use crate::gdt_tss::MAX_CPUS;
use crate::pmm::{HUGE_2M_FRAMES, PAGE_SIZE};
use crate::sched::donate::{donate_on_call, revert_donation_for_caller, DonateError};
use crate::sched::state::ThreadId;
use crate::sched::{
    cancel_park_current, current_cpu_idx, current_slot_on_cpu, prepare_park_current, tcb,
    wake_thread, TOTAL_TCB_SLOTS,
};
use crate::security::{current_lease_epoch, CapRights, SecLabel};
use crate::uaccess::SysError;

use crate::ipc::endpoint_state::{
    next_seq16, pop_recv_q, pop_send_q, push_recv_q, push_send_q, wait_on_endpoint_slot,
    with_ep_lock, EndpointInner, TcbIpcSlot, WaitPhase, ENDPOINTS, IPC_SCRATCH, NO_SLOT,
};
pub use crate::ipc::endpoint_state::{
    ep_create, is_endpoint_live, pack_reply_token, unpack_reply_token, MAX_ENDPOINTS,
};
pub(crate) use crate::ipc::endpoint_state::{
    destroy_endpoint, on_delegated_server_exit, on_endpoint_cap_revoked, on_thread_exit_cleanup,
};
use crate::ipc::{
    check_blocking_ipc_allowed, map_cap_err_to_sys, MsgTag, SharedPayloadDesc, MSG_FLAG_GRANT_CAP,
    MSG_FLAG_NON_BLOCKING, MSG_FLAG_SHARED_DESC,
};

#[derive(Clone, Copy)]
pub struct IpcSendInput<'a> {
    pub tag: MsgTag,
    pub mrs: [u64; 4],
    pub extra: &'a [u8],
    pub grant: Option<(CapPtr, CapRights, u32, u64)>,
    pub shared_desc: Option<SharedPayloadDesc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IpcFastOut {
    pub tag: MsgTag,
    pub mrs: [u64; 4],
    pub sender_badge: u64,
    pub reply_token: u64,
    pub transferred_cap: CapPtr,
    pub extra_len: usize,
}

/// Validate optional `SharedPayloadDesc` capability & bounds (`Stage A.3` zero-copy extensibility).
fn validate_shared_desc(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    desc: SharedPayloadDesc,
    now_epoch: u32,
) -> Result<(), SysError> {
    let res =
        cspace_lookup_fast(cspace_id, subj_label, desc.region_cap, CapRights::READ, now_epoch)
            .map_err(map_cap_err_to_sys)?;
    let max_bytes = match res.obj_kind {
        CapObjectKind::Frame4K => PAGE_SIZE,
        CapObjectKind::Frame2M => PAGE_SIZE * HUGE_2M_FRAMES,
        _ => return Err(SysError::Perm),
    };
    let end = (desc.byte_offset as usize)
        .checked_add(desc.byte_len as usize)
        .ok_or(SysError::MsgSize)?;
    if end > max_bytes {
        return Err(SysError::MsgSize);
    }
    Ok(())
}

/// Synchronous `Endpoint::send` (one-way) or `Endpoint::call` (`is_call = true`, request + reply).
pub fn ep_send_or_call(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    ep_cap: CapPtr,
    input: IpcSendInput<'_>,
    is_call: bool,
    timeout_ticks: u64,
    reply_extra_out: Option<&mut [u8]>,
) -> Result<IpcFastOut, SysError> {
    input.tag.validate()?;
    if input.extra.len() != input.tag.extra_len() {
        return Err(SysError::Inval);
    }
    let non_blocking = (input.tag.flags() & MSG_FLAG_NON_BLOCKING) != 0;
    if is_call && non_blocking {
        return Err(SysError::Inval);
    }
    if !non_blocking {
        check_blocking_ipc_allowed()?;
    }

    let now_epoch = current_lease_epoch();
    if (input.tag.flags() & MSG_FLAG_SHARED_DESC) != 0 {
        let desc = input.shared_desc.ok_or(SysError::Inval)?;
        validate_shared_desc(cspace_id, subj_label, desc, now_epoch)?;
    }
    let grant_req = if (input.tag.flags() & MSG_FLAG_GRANT_CAP) != 0 {
        Some(input.grant.ok_or(SysError::Inval)?)
    } else {
        None
    };

    let req_rights = if is_call {
        CapRights(CapRights::SEND.0 | CapRights::CALL.0)
    } else {
        CapRights::SEND
    };

    let cpu_idx = current_cpu_idx();
    let cur_slot = current_slot_on_cpu(cpu_idx);
    let (_, _, cur_gen26) = tcb(cur_slot).sched.snapshot();
    let deadline = if timeout_ticks > 0 {
        rdtsc().saturating_add(timeout_ticks).max(1)
    } else {
        0
    };

    if !non_blocking {
        prepare_park_current();
    }

    let mut wake_receiver: Option<(ThreadId, usize)> = None;
    let mut immediate_done = false;

    let pin_res = with_pinned_cap(cspace_id, subj_label, ep_cap, req_rights, now_epoch, |res| {
        if res.obj_kind != CapObjectKind::Endpoint {
            return Err(CapError::WrongObjectKind);
        }
        let ep_idx = res.obj_id as usize;
        let ep = &ENDPOINTS[ep_idx];
        let mut op_err: Option<SysError> = None;

        with_ep_lock(ep, |inner, slots| {
            let (st, _, _, sgen) =
                unpack_state_gen(slot_by_global(res.global_slot).state_gen.load(Ordering::Acquire));
            if st != SlotState::Live || sgen != ep_cap.slot_gen() || inner.closed {
                op_err = Some(SysError::Revoked);
                return;
            }
            if inner.server_dead && inner.recv_head == NO_SLOT {
                op_err = Some(SysError::Pipe);
                return;
            }

            if let Some(rcv_slot) = pop_recv_q(inner, slots) {
                let (_, _, rcv_gen26) = tcb(rcv_slot).sched.snapshot();
                let dst_cs = slots[rcv_slot].receiver_cspace;

                if is_call && donate_on_call(cur_slot, rcv_slot) == Err(DonateError::CycleDetected) {
                    let rs = rcv_slot as u16;
                    slots[rcv_slot].prev_waiter = NO_SLOT;
                    slots[rcv_slot].next_waiter = inner.recv_head;
                    if inner.recv_head != NO_SLOT {
                        slots[inner.recv_head as usize].prev_waiter = rs;
                    } else {
                        inner.recv_tail = rs;
                    }
                    inner.recv_head = rs;
                    op_err = Some(SysError::Deadlock);
                    return;
                }

                let xfer_cap = if let Some((g_cap, g_rights, g_lease, g_badge)) = grant_req {
                    match cspace_transfer(
                        cspace_id, subj_label, g_cap, dst_cs, g_rights, g_lease, g_badge, now_epoch,
                    ) {
                        Ok(rcpt) => rcpt.dst_cap,
                        Err(ce) => {
                            if is_call {
                                revert_donation_for_caller(cur_slot);
                            }
                            let rs = rcv_slot as u16;
                            slots[rcv_slot].prev_waiter = NO_SLOT;
                            slots[rcv_slot].next_waiter = inner.recv_head;
                            if inner.recv_head != NO_SLOT {
                                slots[inner.recv_head as usize].prev_waiter = rs;
                            } else {
                                inner.recv_tail = rs;
                            }
                            inner.recv_head = rs;
                            op_err = Some(map_cap_err_to_sys(ce));
                            return;
                        }
                    }
                } else {
                    CapPtr::NULL
                };

                let reply_tok = if is_call {
                    let seq = next_seq16(slots[cur_slot].call_seq);
                    slots[cur_slot].call_seq = seq;
                    slots[cur_slot].wait_phase = WaitPhase::WaitingReply;
                    slots[cur_slot].is_call = true;
                    slots[cur_slot].wait_cpu = cpu_idx as u16;
                    slots[cur_slot].ep_id = ep_idx as u16;
                    slots[cur_slot].ep_gen = res.expected_obj_gen;
                    slots[cur_slot].cap_gslot = res.global_slot;
                    slots[cur_slot].sender_cspace = cspace_id;
                    slots[cur_slot].replier_slot = rcv_slot as u16;
                    slots[cur_slot].replier_gen26 = rcv_gen26;
                    pack_reply_token(ep_idx as u16, cur_slot as u16, cur_gen26, seq)
                } else {
                    immediate_done = true;
                    0
                };

                let rcv = &mut slots[rcv_slot];
                rcv.tag = input.tag;
                rcv.mrs = input.mrs;
                rcv.sender_badge = res.badge;
                rcv.reply_token = reply_tok;
                rcv.transferred_cap = xfer_cap;
                rcv.extra_len = input.extra.len() as u16;
                if !input.extra.is_empty() {
                    rcv.extra_buf[..input.extra.len()].copy_from_slice(input.extra);
                }
                rcv.wait_phase = WaitPhase::Delivered;
                wake_receiver = Some((ThreadId::new(rcv_slot, rcv_gen26), cpu_idx));
                return;
            }

            if non_blocking {
                op_err = Some(SysError::WouldBlock);
                return;
            }

            if is_call && inner.last_server_slot != NO_SLOT {
                let srv = inner.last_server_slot as usize;
                let (srv_st, _, srv_gen) = tcb(srv).sched.snapshot();
                if srv_st != crate::sched::state::ExecState::Dead
                    && srv_gen == inner.last_server_gen26
                    && donate_on_call(cur_slot, srv) == Err(DonateError::CycleDetected)
                {
                    op_err = Some(SysError::Deadlock);
                    return;
                }
            }

            let me = &mut slots[cur_slot];
            me.wait_phase = WaitPhase::WaitingSend;
            me.is_call = is_call;
            me.wait_cpu = cpu_idx as u16;
            me.ep_id = ep_idx as u16;
            me.ep_gen = res.expected_obj_gen;
            me.cap_gslot = res.global_slot;
            me.sender_cspace = cspace_id;
            me.sender_label = subj_label;
            me.sender_badge = res.badge;
            me.tag = input.tag;
            me.mrs = input.mrs;
            me.grant_req = grant_req;
            me.transferred_cap = CapPtr::NULL;
            me.reply_token = 0;
            me.replier_slot = NO_SLOT;
            me.extra_len = input.extra.len() as u16;
            if !input.extra.is_empty() {
                me.extra_buf[..input.extra.len()].copy_from_slice(input.extra);
            }
            push_send_q(inner, slots, cur_slot as u16);
        });

        Ok((ep_idx, op_err))
    });

    let (ep_idx, maybe_err) = match pin_res {
        Ok(v) => v,
        Err(ce) => {
            if !non_blocking {
                cancel_park_current();
            }
            return Err(map_cap_err_to_sys(ce));
        }
    };
    if let Some(se) = maybe_err {
        if !non_blocking {
            cancel_park_current();
        }
        return Err(se);
    }

    if let Some((tid, pref_cpu)) = wake_receiver {
        let _ = wake_thread(tid, pref_cpu);
    }

    if immediate_done {
        if !non_blocking {
            cancel_park_current();
        }
        return Ok(IpcFastOut {
            tag: MsgTag::EMPTY,
            mrs: [0; 4],
            sender_badge: 0,
            reply_token: 0,
            transferred_cap: CapPtr::NULL,
            extra_len: 0,
        });
    }

    wait_on_endpoint_slot(ep_idx, cur_slot, deadline)?;

    if !is_call {
        return Ok(IpcFastOut {
            tag: MsgTag::EMPTY,
            mrs: [0; 4],
            sender_badge: 0,
            reply_token: 0,
            transferred_cap: CapPtr::NULL,
            extra_len: 0,
        });
    }

    // SAFETY: `cur_slot` is no longer queued (`wait_phase == Idle`), so calling thread has exclusive access.
    let out = unsafe {
        let st = &(*IPC_SCRATCH.0.get())[cur_slot];
        let elen = st.extra_len as usize;
        if elen > 0 {
            if let Some(dst) = reply_extra_out {
                let n = dst.len().min(elen);
                dst[..n].copy_from_slice(&st.extra_buf[..n]);
            }
        }
        IpcFastOut {
            tag: st.tag,
            mrs: st.mrs,
            sender_badge: st.sender_badge,
            reply_token: 0,
            transferred_cap: st.transferred_cap,
            extra_len: elen,
        }
    };
    Ok(out)
}

/// Queue a wakeup to run after the endpoint lock is dropped. `AUD-K5`: when the small deferred
/// array is full the thread is woken immediately instead of being silently dropped (`wake_thread`
/// takes no endpoint lock, so this cannot deadlock; the woken thread just spins on `ep.lock`).
#[inline]
fn defer_wake(
    wakes: &mut [Option<(ThreadId, usize)>; 4],
    wake_cnt: &mut usize,
    tid: ThreadId,
    cpu: usize,
) {
    if *wake_cnt < wakes.len() {
        wakes[*wake_cnt] = Some((tid, cpu));
        *wake_cnt += 1;
    } else {
        let _ = wake_thread(tid, cpu);
    }
}

#[allow(clippy::too_many_arguments)]
fn recv_locked(
    inner: &mut EndpointInner,
    slots: &mut [TcbIpcSlot; TOTAL_TCB_SLOTS],
    res: &ResolvedCap,
    ep_idx: usize,
    cspace_id: CSpaceId,
    now_epoch: u32,
    cpu_idx: usize,
    cur_slot: usize,
    cur_gen26: u32,
    non_blocking: bool,
    extra_out: &mut Option<&mut [u8]>,
    wakes: &mut [Option<(ThreadId, usize)>; 4],
    wake_cnt: &mut usize,
) -> Result<Option<IpcFastOut>, SysError> {
    inner.server_dead = false;
    inner.last_server_slot = cur_slot as u16;
    inner.last_server_gen26 = cur_gen26;

    while let Some(snd_slot) = pop_send_q(inner, slots) {
        let (_, _, snd_gen26) = tcb(snd_slot).sched.snapshot();
        let snd_cpu = slots[snd_slot].wait_cpu as usize;
        let snd_is_call = slots[snd_slot].is_call;

        if snd_is_call && donate_on_call(snd_slot, cur_slot) == Err(DonateError::CycleDetected) {
            slots[snd_slot].fail_err = SysError::Deadlock;
            slots[snd_slot].wait_phase = WaitPhase::Failed;
            defer_wake(wakes, wake_cnt, ThreadId::new(snd_slot, snd_gen26), snd_cpu);
            continue;
        }

        let xfer_cap =
            if let Some((g_cap, g_rights, g_lease, g_badge)) = slots[snd_slot].grant_req.take() {
                let s_cs = slots[snd_slot].sender_cspace;
                let s_lbl = slots[snd_slot].sender_label;
                match cspace_transfer(
                    s_cs, s_lbl, g_cap, cspace_id, g_rights, g_lease, g_badge, now_epoch,
                ) {
                    Ok(rcpt) => rcpt.dst_cap,
                    Err(ce) => {
                        let _ = revert_donation_for_caller(snd_slot);
                        slots[snd_slot].fail_err = map_cap_err_to_sys(ce);
                        slots[snd_slot].wait_phase = WaitPhase::Failed;
                        defer_wake(wakes, wake_cnt, ThreadId::new(snd_slot, snd_gen26), snd_cpu);
                        continue;
                    }
                }
            } else {
                CapPtr::NULL
            };

        let elen = slots[snd_slot].extra_len as usize;
        if elen > 0 {
            if let Some(dst) = extra_out.as_deref_mut() {
                let n = dst.len().min(elen);
                dst[..n].copy_from_slice(&slots[snd_slot].extra_buf[..n]);
            }
        }

        let reply_tok = if snd_is_call {
            let seq = next_seq16(slots[snd_slot].call_seq);
            slots[snd_slot].call_seq = seq;
            slots[snd_slot].replier_slot = cur_slot as u16;
            slots[snd_slot].replier_gen26 = cur_gen26;
            slots[snd_slot].wait_phase = WaitPhase::WaitingReply;
            pack_reply_token(ep_idx as u16, snd_slot as u16, snd_gen26, seq)
        } else {
            slots[snd_slot].wait_phase = WaitPhase::Delivered;
            defer_wake(wakes, wake_cnt, ThreadId::new(snd_slot, snd_gen26), snd_cpu);
            0
        };

        return Ok(Some(IpcFastOut {
            tag: slots[snd_slot].tag,
            mrs: slots[snd_slot].mrs,
            sender_badge: slots[snd_slot].sender_badge,
            reply_token: reply_tok,
            transferred_cap: xfer_cap,
            extra_len: elen,
        }));
    }

    if let Some(bits) = crate::ipc::bound_notif::poll_bound_notif_for_recv(cur_slot) {
        return Ok(Some(IpcFastOut {
            tag: crate::ipc::MsgTag::new(crate::ipc::IPC_OP_BOUND_NOTIF, 0, 0, 0),
            mrs: [0; 4],
            sender_badge: bits,
            reply_token: 0,
            transferred_cap: CapPtr::NULL,
            extra_len: 0,
        }));
    }

    if non_blocking {
        return Err(SysError::WouldBlock);
    }

    let me = &mut slots[cur_slot];
    me.wait_phase = WaitPhase::WaitingRecv;
    me.is_call = false;
    me.wait_cpu = cpu_idx as u16;
    me.ep_id = ep_idx as u16;
    me.ep_gen = res.expected_obj_gen;
    me.cap_gslot = res.global_slot;
    me.receiver_cspace = cspace_id;
    me.transferred_cap = CapPtr::NULL;
    me.reply_token = 0;
    me.extra_len = 0;
    push_recv_q(inner, slots, cur_slot as u16);
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn reply_locked(
    inner: &EndpointInner,
    slots: &mut [TcbIpcSlot; TOTAL_TCB_SLOTS],
    res: &ResolvedCap,
    tok_ep_idx: usize,
    caller_slot: usize,
    caller_gen26: u32,
    tok_seq: u32,
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    now_epoch: u32,
    cpu_idx: usize,
    cur_slot: usize,
    cur_gen26: u32,
    input: &IpcSendInput<'_>,
    grant_req: Option<(CapPtr, CapRights, u32, u64)>,
) -> Result<(ThreadId, usize), SysError> {
    if inner.closed {
        return Err(SysError::Revoked);
    }
    let (c_exec, _, live_cgen) = tcb(caller_slot).sched.snapshot();
    if c_exec == crate::sched::state::ExecState::Dead || live_cgen != caller_gen26 {
        return Err(SysError::Pipe);
    }
    let c_ipc = &mut slots[caller_slot];
    if c_ipc.wait_phase != WaitPhase::WaitingReply
        || (c_ipc.ep_id as usize) != tok_ep_idx
        || c_ipc.ep_gen != res.expected_obj_gen
        || c_ipc.call_seq != tok_seq
        || c_ipc.replier_slot != (cur_slot as u16)
        || c_ipc.replier_gen26 != cur_gen26
    {
        return Err(SysError::BadCap);
    }

    let xfer_cap = if let Some((g_cap, g_rights, g_lease, g_badge)) = grant_req {
        let dst_cs = c_ipc.sender_cspace;
        match cspace_transfer(
            cspace_id, subj_label, g_cap, dst_cs, g_rights, g_lease, g_badge, now_epoch,
        ) {
            Ok(rcpt) => rcpt.dst_cap,
            Err(ce) => return Err(map_cap_err_to_sys(ce)),
        }
    } else {
        CapPtr::NULL
    };

    c_ipc.call_seq = next_seq16(c_ipc.call_seq);
    c_ipc.replier_slot = NO_SLOT;
    c_ipc.tag = input.tag;
    c_ipc.mrs = input.mrs;
    c_ipc.sender_badge = res.badge;
    c_ipc.transferred_cap = xfer_cap;
    c_ipc.extra_len = input.extra.len() as u16;
    if !input.extra.is_empty() {
        c_ipc.extra_buf[..input.extra.len()].copy_from_slice(input.extra);
    }
    c_ipc.wait_phase = WaitPhase::Delivered;
    let _ = revert_donation_for_caller(caller_slot);
    Ok((ThreadId::new(caller_slot, caller_gen26), cpu_idx))
}

/// Synchronous `Endpoint::recv` (`CapRights::RECV`).
pub fn ep_recv(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    ep_cap: CapPtr,
    non_blocking: bool,
    timeout_ticks: u64,
    mut extra_out: Option<&mut [u8]>,
) -> Result<IpcFastOut, SysError> {
    if !non_blocking {
        check_blocking_ipc_allowed()?;
    }

    let now_epoch = current_lease_epoch();
    let cpu_idx = current_cpu_idx();
    let cur_slot = current_slot_on_cpu(cpu_idx);
    let (_, _, cur_gen26) = tcb(cur_slot).sched.snapshot();
    let deadline = if timeout_ticks > 0 {
        rdtsc().saturating_add(timeout_ticks).max(1)
    } else {
        0
    };

    if !non_blocking {
        prepare_park_current();
    }

    let mut wakes: [Option<(ThreadId, usize)>; 4] = [None; 4];
    let mut wake_cnt = 0usize;
    let mut fast_delivered: Option<IpcFastOut> = None;

    let pin_res = with_pinned_cap(cspace_id, subj_label, ep_cap, CapRights::RECV, now_epoch, |res| {
        if res.obj_kind != CapObjectKind::Endpoint {
            return Err(CapError::WrongObjectKind);
        }
        let ep_idx = res.obj_id as usize;
        let ep = &ENDPOINTS[ep_idx];
        let mut op_err: Option<SysError> = None;

        with_ep_lock(ep, |inner, slots| {
            let (st, _, _, sgen) =
                unpack_state_gen(slot_by_global(res.global_slot).state_gen.load(Ordering::Acquire));
            if st != SlotState::Live || sgen != ep_cap.slot_gen() || inner.closed {
                op_err = Some(SysError::Revoked);
                return;
            }
            match recv_locked(
                inner,
                slots,
                &res,
                ep_idx,
                cspace_id,
                now_epoch,
                cpu_idx,
                cur_slot,
                cur_gen26,
                non_blocking,
                &mut extra_out,
                &mut wakes,
                &mut wake_cnt,
            ) {
                Ok(done) => fast_delivered = done,
                Err(se) => op_err = Some(se),
            }
        });

        Ok((ep_idx, op_err))
    });

    for (tid, pcpu) in wakes.into_iter().flatten() {
        let _ = wake_thread(tid, pcpu);
    }

    let (ep_idx, maybe_err) = match pin_res {
        Ok(v) => v,
        Err(ce) => {
            if !non_blocking {
                cancel_park_current();
            }
            return Err(map_cap_err_to_sys(ce));
        }
    };
    if let Some(se) = maybe_err {
        if !non_blocking {
            cancel_park_current();
        }
        return Err(se);
    }
    if let Some(done) = fast_delivered {
        if !non_blocking {
            cancel_park_current();
        }
        return Ok(done);
    }

    wait_on_endpoint_slot(ep_idx, cur_slot, deadline)?;

    // SAFETY: `cur_slot` transitioned to `WaitPhase::Idle`, giving exclusive local access.
    let out = unsafe {
        let st = &(*IPC_SCRATCH.0.get())[cur_slot];
        let elen = st.extra_len as usize;
        if elen > 0 {
            if let Some(dst) = extra_out {
                let n = dst.len().min(elen);
                dst[..n].copy_from_slice(&st.extra_buf[..n]);
            }
        }
        IpcFastOut {
            tag: st.tag,
            mrs: st.mrs,
            sender_badge: st.sender_badge,
            reply_token: st.reply_token,
            transferred_cap: st.transferred_cap,
            extra_len: elen,
        }
    };
    Ok(out)
}

/// Synchronous single-shot `Endpoint::reply` (`CapRights::REPLY`, `INV-IPC-03`).
pub fn ep_reply(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    ep_cap: CapPtr,
    reply_token: u64,
    input: IpcSendInput<'_>,
) -> Result<(), SysError> {
    input.tag.validate()?;
    if input.extra.len() != input.tag.extra_len() {
        return Err(SysError::Inval);
    }
    let Some((tok_ep_idx, caller_slot, caller_gen26, tok_seq)) = unpack_reply_token(reply_token)
    else {
        return Err(SysError::BadCap);
    };
    if !(MAX_CPUS..TOTAL_TCB_SLOTS).contains(&caller_slot) || tok_ep_idx >= MAX_ENDPOINTS {
        return Err(SysError::BadCap);
    }

    let now_epoch = current_lease_epoch();
    let grant_req = if (input.tag.flags() & MSG_FLAG_GRANT_CAP) != 0 {
        Some(input.grant.ok_or(SysError::Inval)?)
    } else {
        None
    };

    let cpu_idx = current_cpu_idx();
    let cur_slot = current_slot_on_cpu(cpu_idx);
    let (_, _, cur_gen26) = tcb(cur_slot).sched.snapshot();

    let mut wake_caller: Option<(ThreadId, usize)> = None;

    let pin_res =
        with_pinned_cap(cspace_id, subj_label, ep_cap, CapRights::REPLY, now_epoch, |res| {
            if res.obj_kind != CapObjectKind::Endpoint {
                return Err(CapError::WrongObjectKind);
            }
            if (res.obj_id as usize) != tok_ep_idx {
                return Err(CapError::InvalidHandle);
            }
            let ep = &ENDPOINTS[tok_ep_idx];
            let mut op_err: Option<SysError> = None;

            with_ep_lock(ep, |inner, slots| match reply_locked(
                inner,
                slots,
                &res,
                tok_ep_idx,
                caller_slot,
                caller_gen26,
                tok_seq,
                cspace_id,
                subj_label,
                now_epoch,
                cpu_idx,
                cur_slot,
                cur_gen26,
                &input,
                grant_req,
            ) {
                Ok(wc) => wake_caller = Some(wc),
                Err(se) => op_err = Some(se),
            });

            Ok(op_err)
        });

    let maybe_err = pin_res.map_err(map_cap_err_to_sys)?;
    if let Some(se) = maybe_err {
        return Err(se);
    }
    if let Some((tid, pref_cpu)) = wake_caller {
        let _ = wake_thread(tid, pref_cpu);
    }
    Ok(())
}

/// Combined single-syscall server fast path: when `reply_token != 0`, fuses `reply` + `recv` under
/// a single capability pin (`2` fewer `LOCK XADD`s, `1` fewer `cspace_lookup_fast`) and a single
/// `Endpoint` lock acquisition (`1` fewer spinlock round-trip).
#[allow(clippy::too_many_arguments)]
pub fn ep_reply_recv(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    ep_cap: CapPtr,
    reply_token: u64,
    reply_input: IpcSendInput<'_>,
    recv_non_blocking: bool,
    recv_timeout_ticks: u64,
    mut recv_extra_out: Option<&mut [u8]>,
) -> Result<IpcFastOut, SysError> {
    if reply_token == 0 {
        return ep_recv(
            cspace_id,
            subj_label,
            ep_cap,
            recv_non_blocking,
            recv_timeout_ticks,
            recv_extra_out,
        );
    }

    reply_input.tag.validate()?;
    if reply_input.extra.len() != reply_input.tag.extra_len() {
        return Err(SysError::Inval);
    }
    let Some((tok_ep_idx, caller_slot, caller_gen26, tok_seq)) = unpack_reply_token(reply_token)
    else {
        return Err(SysError::BadCap);
    };
    if !(MAX_CPUS..TOTAL_TCB_SLOTS).contains(&caller_slot) || tok_ep_idx >= MAX_ENDPOINTS {
        return Err(SysError::BadCap);
    }
    let grant_req = if (reply_input.tag.flags() & MSG_FLAG_GRANT_CAP) != 0 {
        Some(reply_input.grant.ok_or(SysError::Inval)?)
    } else {
        None
    };
    if !recv_non_blocking {
        check_blocking_ipc_allowed()?;
    }

    let now_epoch = current_lease_epoch();
    let cpu_idx = current_cpu_idx();
    let cur_slot = current_slot_on_cpu(cpu_idx);
    let (_, _, cur_gen26) = tcb(cur_slot).sched.snapshot();
    let deadline = if recv_timeout_ticks > 0 {
        rdtsc().saturating_add(recv_timeout_ticks).max(1)
    } else {
        0
    };

    if !recv_non_blocking {
        prepare_park_current();
    }

    let req_rights = CapRights(CapRights::RECV.0 | CapRights::REPLY.0);
    let mut wake_caller: Option<(ThreadId, usize)> = None;
    let mut wakes: [Option<(ThreadId, usize)>; 4] = [None; 4];
    let mut wake_cnt = 0usize;
    let mut fast_delivered: Option<IpcFastOut> = None;

    let pin_res = with_pinned_cap(cspace_id, subj_label, ep_cap, req_rights, now_epoch, |res| {
        if res.obj_kind != CapObjectKind::Endpoint {
            return Err(CapError::WrongObjectKind);
        }
        if (res.obj_id as usize) != tok_ep_idx {
            return Err(CapError::InvalidHandle);
        }
        let ep = &ENDPOINTS[tok_ep_idx];
        let mut op_err: Option<SysError> = None;

        with_ep_lock(ep, |inner, slots| {
            let (st, _, _, sgen) =
                unpack_state_gen(slot_by_global(res.global_slot).state_gen.load(Ordering::Acquire));
            if st != SlotState::Live || sgen != ep_cap.slot_gen() || inner.closed {
                op_err = Some(SysError::Revoked);
                return;
            }

            match reply_locked(
                inner,
                slots,
                &res,
                tok_ep_idx,
                caller_slot,
                caller_gen26,
                tok_seq,
                cspace_id,
                subj_label,
                now_epoch,
                cpu_idx,
                cur_slot,
                cur_gen26,
                &reply_input,
                grant_req,
            ) {
                Ok(wc) => wake_caller = Some(wc),
                Err(se) => {
                    op_err = Some(se);
                    return;
                }
            }

            match recv_locked(
                inner,
                slots,
                &res,
                tok_ep_idx,
                cspace_id,
                now_epoch,
                cpu_idx,
                cur_slot,
                cur_gen26,
                recv_non_blocking,
                &mut recv_extra_out,
                &mut wakes,
                &mut wake_cnt,
            ) {
                Ok(done) => fast_delivered = done,
                Err(se) => op_err = Some(se),
            }
        });

        Ok(op_err)
    });

    if let Some((tid, pref_cpu)) = wake_caller {
        let _ = wake_thread(tid, pref_cpu);
    }
    for (tid, pcpu) in wakes.into_iter().flatten() {
        let _ = wake_thread(tid, pcpu);
    }

    let maybe_err = match pin_res {
        Ok(v) => v,
        Err(ce) => {
            if !recv_non_blocking {
                cancel_park_current();
            }
            return Err(map_cap_err_to_sys(ce));
        }
    };
    if let Some(se) = maybe_err {
        if !recv_non_blocking {
            cancel_park_current();
        }
        return Err(se);
    }
    if let Some(done) = fast_delivered {
        if !recv_non_blocking {
            cancel_park_current();
        }
        return Ok(done);
    }

    wait_on_endpoint_slot(tok_ep_idx, cur_slot, deadline)?;

    // SAFETY: `cur_slot` transitioned to `WaitPhase::Idle`, giving exclusive local access.
    let out = unsafe {
        let st = &(*IPC_SCRATCH.0.get())[cur_slot];
        let elen = st.extra_len as usize;
        if elen > 0 {
            if let Some(dst) = recv_extra_out {
                let n = dst.len().min(elen);
                dst[..n].copy_from_slice(&st.extra_buf[..n]);
            }
        }
        IpcFastOut {
            tag: st.tag,
            mrs: st.mrs,
            sender_badge: st.sender_badge,
            reply_token: st.reply_token,
            transferred_cap: st.transferred_cap,
            extra_len: elen,
        }
    };
    Ok(out)
}
