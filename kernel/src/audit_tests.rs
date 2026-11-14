// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Regression reproducers for the `Milestone 2A-4a` independent baseline audit (`docs/AUDIT_2A4A.md`).
//!
//! Every case failed (hang -> epoch watchdog panic, or assertion) on `9f9c575` before its fix:
//! - `AUD-K1` root `TCB` capability deletable by its own process -> un-reapable process slot.
//! - `AUD-K2` `UNMAP_4K` accepted non-`Frame4K` capabilities and detached their mapping record.
//! - `AUD-K3` destroying an object through a derived capability left the root capability dangling.
//! - `AUD-K4` `Notification` wait/signal Dekker inversion (lost wakeup).
//! - `AUD-K5` `recv_locked` silently dropped the 5th deferred sender wakeup.
//! - `AUD-K15` pre-park `kill_requested` lost wakeup in untimed `notif_wait` / `ep_recv`.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::cspace::cap::{CapError, CapPtr};
use crate::cspace::objects::{
    cspace_alloc_frame, cspace_vspace_create, cspace_vspace_map_2m, cspace_vspace_unmap_2m,
    cspace_vspace_unmap_4k,
};
use crate::cspace::revoke::{cspace_delete_cap, cspace_destroy_object};
use crate::cspace::{create_cspace, cspace_derive, destroy_cspace};
use crate::ipc::endpoint::{ep_create, ep_recv, ep_send_or_call, IpcSendInput};
use crate::ipc::notif::{notif_create, notif_signal, notif_wait};
use crate::ipc::{MsgTag, MSG_FLAG_GRANT_CAP};
use crate::pmm::{flush_all_magazines_quiescent, inspect_frame_live, measure_fragmentation, PageOrder};
use crate::proc::{
    activate_raw_bench_pid_slot, reclaim_process, release_raw_bench_pid_slot, slot_ref,
    spawn_elf_process, ProcessOutcome, ReclaimStatus, SpawnOptions, MAX_PROCESSES,
};
use crate::sched::preempt_tests::{begin_preemptive_epoch, drain_preemptive_epoch_and_audit};
use crate::sched::{
    current_cpu_idx, current_slot_on_cpu, exit_current_thread, init_scheduler,
    note_current_thread_progress, set_preemptive_mode_active, tcb, try_spawn_thread_bound,
    yield_now,
};
use crate::security::{encode_compartment, CapRights, SecDomain, SecFlags, SecLabel};
use crate::uaccess::SysError;
use crate::vspace::pte::VmProt;
use crate::vspace::KERNEL_VSPACE_ID;

const NOTIF_ROUNDS: u64 = 200_000;
const WAKE_SENDERS: u64 = 6;
const NOTIF_LOST_TIMEOUT_TSC: u64 = 300_000_000;

static CAP_A: AtomicU64 = AtomicU64::new(0);
static CAP_B: AtomicU64 = AtomicU64::new(0);
static COUNTER: AtomicU64 = AtomicU64::new(0);
static DONE: AtomicU64 = AtomicU64::new(0);
static LOST: AtomicU64 = AtomicU64::new(0);

fn test_label() -> SecLabel {
    SecLabel::new(SecDomain::Userspace, SecFlags::NONE, encode_compartment(21, 1), 1)
}

/// `AUD-K1`: a process must not be able to delete its own root `TCB` capability, and reaping must
/// succeed regardless.
fn case_root_tcb_cap_not_deletable(online_cpus: usize) {
    let hello = crate::limine::get_boot_module("user-hello.elf").expect("user-hello.elf");
    let lbl = test_label();
    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xa1, 1);
    let info = spawn_elf_process(
        hello,
        SpawnOptions {
            subj_label: lbl,
            arg0: 0,
            arg1: 2,
            target_cpu: 0,
            trace_lifecycle: false,
            user_fs_base: 0,
            user_gs_base: 0,
        },
    )
    .expect("spawn user-hello");
    // Exactly what `SYS_CAP_REVOKE(own_tcb_cap, op = 1)` executes on behalf of the child.
    let del = cspace_delete_cap(info.cspace_id, lbl, info.tcb_cap, 1);
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);
    assert_eq!(del, Err(CapError::BusyActiveObject), "AUD-K1: root TCB cap was deletable");
    assert_eq!(
        reclaim_process(info.handle),
        Ok(ReclaimStatus::ReclaimedNow(ProcessOutcome::Exited(0))),
        "AUD-K1: process became un-reapable"
    );
}

/// `AUD-K2`: `UNMAP_4K` through a `Frame2M` capability must be rejected without detaching the
/// 2 MiB mapping record (otherwise the mapping survives revocation).
fn case_unmap_4k_rejects_foreign_kind() {
    let lbl = test_label();
    let (cs, _) = create_cspace(lbl, 0).unwrap();
    let (vs_cap, _) = cspace_vspace_create(cs, lbl, 0).unwrap();
    let (fr2m, _) = cspace_alloc_frame(cs, lbl, lbl, PageOrder::Order2MiB, false, 0).unwrap();
    let va = 0x0000_0000_4000_0000u64;
    cspace_vspace_map_2m(cs, lbl, vs_cap, fr2m, va, VmProt::USER_RW, 1).unwrap();
    assert_eq!(
        cspace_vspace_unmap_4k(cs, lbl, vs_cap, fr2m, va, 1),
        Err(CapError::WrongObjectKind),
        "AUD-K2: UNMAP_4K accepted a Frame2M capability"
    );
    assert_eq!(
        cspace_vspace_unmap_2m(cs, lbl, vs_cap, fr2m, va, 1),
        Ok(()),
        "AUD-K2: 2 MiB mapping record was detached by UNMAP_4K"
    );
    cspace_destroy_object(cs, lbl, fr2m, 1).unwrap();
    cspace_destroy_object(cs, lbl, vs_cap, 1).unwrap();
    destroy_cspace(cs).unwrap();
}

/// `AUD-K3`: destroying through a derived (non-root) capability only drops that capability; the
/// object stays alive for the root holder (no dangling root, no recycled-object confusion).
fn case_destroy_via_derived_cap_keeps_object() {
    let lbl = test_label();
    let (cs, _) = create_cspace(lbl, 0).unwrap();
    let (ep_root, _) = ep_create(cs, lbl, lbl, 0).unwrap();
    let ep_child = cspace_derive(cs, lbl, ep_root, CapRights::ENDPOINT_ROOT, 0, 0, 1).unwrap();
    cspace_destroy_object(cs, lbl, ep_child, 1).unwrap();
    assert_eq!(
        ep_recv(cs, lbl, ep_root, true, 0, None).map(|_| ()),
        Err(SysError::WouldBlock),
        "AUD-K3: endpoint died under its root capability"
    );
    assert!(
        cspace_delete_cap(cs, lbl, ep_child, 1).is_err(),
        "AUD-K3: derived capability survived its own destroy"
    );

    let (fr_root, fh) = cspace_alloc_frame(cs, lbl, lbl, PageOrder::Order4KiB, false, 0).unwrap();
    let fr_c1 = cspace_derive(cs, lbl, fr_root, CapRights::FRAME_DATA_ROOT, 0, 0, 1).unwrap();
    let fr_c2 = cspace_derive(cs, lbl, fr_root, CapRights::FRAME_DATA_ROOT, 0, 0, 1).unwrap();
    cspace_destroy_object(cs, lbl, fr_c1, 1).unwrap();
    cspace_destroy_object(cs, lbl, fr_c2, 1).unwrap();
    let (_, _, _, gen, _) = inspect_frame_live(fh.pfn).unwrap();
    assert_eq!(gen, fh.generation, "AUD-K3: frame freed under its root capability");
    cspace_destroy_object(cs, lbl, fr_root, 1).unwrap();
    cspace_destroy_object(cs, lbl, ep_root, 1).unwrap();
    destroy_cspace(cs).unwrap();
}

/// A healthy round completes in microseconds; a `~100 ms` timeout therefore means the signal was
/// posted but its wakeup was lost (the bits are still pending and are consumed by the next wait).
fn wait_or_count_lost(cs: u16, lbl: SecLabel, cap: CapPtr) {
    loop {
        match notif_wait(cs, lbl, cap, false, NOTIF_LOST_TIMEOUT_TSC) {
            Ok(_) => return,
            Err(SysError::TimedOut) => {
                LOST.fetch_add(1, Ordering::AcqRel);
            }
            Err(e) => panic!("AUD-K4: notif_wait failed: {:?}", e),
        }
    }
}

fn notif_pinger(cs_raw: u64) {
    let (cs, lbl) = (cs_raw as u16, test_label());
    let (a, b) = (CapPtr(CAP_A.load(Ordering::Acquire)), CapPtr(CAP_B.load(Ordering::Acquire)));
    for _ in 0..NOTIF_ROUNDS {
        notif_signal(cs, lbl, a, 1).unwrap();
        wait_or_count_lost(cs, lbl, b);
        note_current_thread_progress();
    }
    DONE.fetch_add(1, Ordering::AcqRel);
    exit_current_thread()
}

fn notif_ponger(cs_raw: u64) {
    let (cs, lbl) = (cs_raw as u16, test_label());
    let (a, b) = (CapPtr(CAP_A.load(Ordering::Acquire)), CapPtr(CAP_B.load(Ordering::Acquire)));
    for _ in 0..NOTIF_ROUNDS {
        wait_or_count_lost(cs, lbl, a);
        notif_signal(cs, lbl, b, 1).unwrap();
        note_current_thread_progress();
    }
    DONE.fetch_add(1, Ordering::AcqRel);
    exit_current_thread()
}

/// `AUD-K4`: untimed `Notification` ping-pong across CPUs; a single lost wakeup hangs forever.
fn case_notif_no_lost_wakeup(online_cpus: usize) {
    let lbl = test_label();
    let (cs, _) = create_cspace(lbl, 0).unwrap();
    let (na, _) = notif_create(cs, lbl, lbl, 0).unwrap();
    let (nb, _) = notif_create(cs, lbl, lbl, 0).unwrap();
    CAP_A.store(na.0, Ordering::Release);
    CAP_B.store(nb.0, Ordering::Release);
    DONE.store(0, Ordering::Release);
    LOST.store(0, Ordering::Release);
    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xa4, 2);
    try_spawn_thread_bound(2, 0, KERNEL_VSPACE_ID, cs, lbl, notif_pinger, cs as u64, true).unwrap();
    try_spawn_thread_bound(2, 1 % online_cpus, KERNEL_VSPACE_ID, cs, lbl, notif_ponger, cs as u64, true)
        .unwrap();
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);
    assert_eq!(DONE.load(Ordering::Acquire), 2, "AUD-K4: notification ping-pong incomplete");
    assert_eq!(LOST.load(Ordering::Acquire), 0, "AUD-K4: notification wakeups were lost");
    cspace_destroy_object(cs, lbl, na, 1).unwrap();
    cspace_destroy_object(cs, lbl, nb, 1).unwrap();
    destroy_cspace(cs).unwrap();
}

fn wake_sender(packed: u64) {
    let (cs, idx, lbl) = (packed as u16, packed >> 16, test_label());
    let ep = CapPtr(CAP_A.load(Ordering::Acquire));
    while COUNTER.load(Ordering::Acquire) != idx {
        yield_now();
    }
    let last = idx == WAKE_SENDERS - 1;
    let input = IpcSendInput {
        tag: MsgTag::new(0x77, if last { 0 } else { MSG_FLAG_GRANT_CAP }, 1, 0),
        mrs: [idx, 0, 0, 0],
        extra: &[],
        // Bogus grant: fails at delivery time inside the receiver's `recv_locked`.
        grant: (!last).then_some((CapPtr(0x00de_ad00), CapRights::READ, 0, 0)),
        shared_desc: None,
    };
    COUNTER.fetch_add(1, Ordering::AcqRel);
    let res = ep_send_or_call(cs, lbl, ep, input, false, 0, None);
    assert_eq!(res.is_ok(), last, "AUD-K5: unexpected send outcome for sender {}", idx);
    DONE.fetch_add(1, Ordering::AcqRel);
    exit_current_thread()
}

fn wake_receiver(cs_raw: u64) {
    let (cs, lbl) = (cs_raw as u16, test_label());
    let ep = CapPtr(CAP_B.load(Ordering::Acquire));
    while COUNTER.load(Ordering::Acquire) != WAKE_SENDERS {
        yield_now();
    }
    for _ in 0..256 {
        yield_now();
    }
    let out = ep_recv(cs, lbl, ep, false, 0, None).expect("AUD-K5: recv failed");
    assert_eq!(out.mrs[0], WAKE_SENDERS - 1, "AUD-K5: wrong sender delivered");
    DONE.fetch_add(1, Ordering::AcqRel);
    exit_current_thread()
}

/// `AUD-K5`: five queued senders fail at delivery and the sixth is delivered by one `recv`; every
/// one of them must be woken (the old 4-entry deferred-wake array dropped the rest).
fn case_recv_wakes_every_dequeued_sender(online_cpus: usize) {
    let lbl = test_label();
    let (cs, _) = create_cspace(lbl, 0).unwrap();
    let (ep_root, _) = ep_create(cs, lbl, lbl, 0).unwrap();
    let ep_send = cspace_derive(cs, lbl, ep_root, CapRights::SEND, 0, 0x51, 1).unwrap();
    CAP_A.store(ep_send.0, Ordering::Release);
    CAP_B.store(ep_root.0, Ordering::Release);
    COUNTER.store(0, Ordering::Release);
    DONE.store(0, Ordering::Release);
    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xa5, WAKE_SENDERS as usize + 1);
    for i in 0..WAKE_SENDERS {
        let arg = (cs as u64) | (i << 16);
        try_spawn_thread_bound(2, 0, KERNEL_VSPACE_ID, cs, lbl, wake_sender, arg, true).unwrap();
    }
    try_spawn_thread_bound(2, 0, KERNEL_VSPACE_ID, cs, lbl, wake_receiver, cs as u64, true).unwrap();
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);
    assert_eq!(DONE.load(Ordering::Acquire), WAKE_SENDERS + 1, "AUD-K5: a sender was never woken");
    cspace_destroy_object(cs, lbl, ep_root, 1).unwrap();
    destroy_cspace(cs).unwrap();
}

fn kill_wait_worker(cs_raw: u64) {
    let (cs, lbl) = (cs_raw as u16, test_label());
    let na = CapPtr(CAP_A.load(Ordering::Acquire));
    let ep = CapPtr(CAP_B.load(Ordering::Acquire));
    let test_pid = MAX_PROCESSES as u32;
    let cur_slot = current_slot_on_cpu(current_cpu_idx());

    tcb(cur_slot).process_id.store(test_pid, Ordering::Release);
    slot_ref(test_pid)
        .expect("AUD-K15 pid slot")
        .kill_requested
        .store(true, Ordering::SeqCst);

    let r_notif = notif_wait(cs, lbl, na, false, 0);
    assert_eq!(
        r_notif,
        Err(SysError::Canceled),
        "AUD-K15: pre-park kill did not cancel untimed notif_wait"
    );
    let r_ep = ep_recv(cs, lbl, ep, false, 0, None);
    assert_eq!(
        r_ep,
        Err(SysError::Canceled),
        "AUD-K15: pre-park kill did not cancel untimed ep_recv"
    );

    tcb(cur_slot).process_id.store(0, Ordering::Release);
    DONE.fetch_add(1, Ordering::AcqRel);
    exit_current_thread()
}

/// `AUD-K15`: if `kill_requested` arrives *before* `prepare_park_current()`, both untimed
/// `notif_wait` and untimed `ep_recv` must return `Err(SysError::Canceled)` on the first lock
/// check without parking forever.
fn case_pre_park_kill_cancels_untimed_waits(online_cpus: usize) {
    let lbl = test_label();
    let (cs, _) = create_cspace(lbl, 0).unwrap();
    let (na, _) = notif_create(cs, lbl, lbl, 0).unwrap();
    let (ep, _) = ep_create(cs, lbl, lbl, 0).unwrap();
    let test_pid = MAX_PROCESSES as u32;

    CAP_A.store(na.0, Ordering::Release);
    CAP_B.store(ep.0, Ordering::Release);
    DONE.store(0, Ordering::Release);
    activate_raw_bench_pid_slot(test_pid);

    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xaf, 1);
    try_spawn_thread_bound(2, 0, KERNEL_VSPACE_ID, cs, lbl, kill_wait_worker, cs as u64, true).unwrap();
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);

    if let Some(slot) = slot_ref(test_pid) {
        slot.kill_requested.store(false, Ordering::Release);
    }
    release_raw_bench_pid_slot(test_pid);

    assert_eq!(DONE.load(Ordering::Acquire), 1, "AUD-K15: worker hung or missed assertion");
    cspace_destroy_object(cs, lbl, ep, 1).unwrap();
    cspace_destroy_object(cs, lbl, na, 1).unwrap();
    destroy_cspace(cs).unwrap();
}

pub fn verify_audit_regressions(online_cpus: usize) {
    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);
    case_root_tcb_cap_not_deletable(online_cpus);
    case_unmap_4k_rejects_foreign_kind();
    case_destroy_via_derived_cap_keeps_object();
    case_notif_no_lost_wakeup(online_cpus);
    case_recv_wakes_every_dequeued_sender(online_cpus);
    case_pre_park_kill_cancels_untimed_waits(online_cpus);
    flush_all_magazines_quiescent(online_cpus);
    let after = measure_fragmentation(online_cpus);
    let leaked = before.free_global_frames.saturating_sub(after.free_global_frames);
    assert_eq!(leaked, 0, "Frame leak in audit regression suite: {}", leaked);
    crate::serial_println!(
        "[DRY-AUDIT-REG] PASS: cpus={} cases=6/6 notif_rounds={} leaked_frames={}",
        online_cpus,
        NOTIF_ROUNDS,
        leaked
    );
}
