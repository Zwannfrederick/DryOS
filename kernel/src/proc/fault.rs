// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `FaultEndpoint` (`Milestone 2A-4a` Contract C): capability-mediated delivery of Ring 3 CPU
//! exceptions to the supervisor that spawned the faulting process (`0` new syscalls).
//!
//! - **Binding:** `SYS_PROC_SPAWN` with `SPAWN_FLAG_FAULT_EP` interprets `grant_cap1` as an
//!   `Endpoint` capability (`SEND | CALL`) in the *parent's* `CSpace`. It is not transferred to
//!   the child; the kernel records `(parent CSpace, CapPtr, parent label)`.
//! - **Delivery:** on a `CPL = 3` exception the faulting thread performs a kernel-originated
//!   `Endpoint::call` through that capability. The lookup happens at fault time, so a revoked,
//!   deleted, or expired capability (or a dead supervisor) silently falls back to termination.
//! - **Authenticity:** the supervisor receives the kernel-stamped badge of the capability it
//!   bound (`INV-SEC-06`); an ordinary client of the same endpoint can copy the message label but
//!   never the badge.
//! - **Policy:** the reply label selects `FAULT_REPLY_RESUME` (optionally at a new, validated
//!   `RIP` in `mr0`) or termination (anything else).
//! - **Bounded:** each process may deliver at most `FAULT_BUDGET` faults; a fault storm ends in
//!   termination, and a supervisor that never replies only blocks the faulting process, which
//!   stays killable (`SYS_PROC_KILL`).

use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::{cli, sti};
use crate::cspace::cap::{CSpaceId, CapError, CapObjectKind, CapPtr};
use crate::cspace::with_pinned_cap;
use crate::idt::TrapFrame;
use crate::ipc::endpoint::{ep_send_or_call, IpcSendInput};
use crate::ipc::MsgTag;
use crate::proc::{slot_ref, ProcessState};
use crate::security::{current_lease_epoch, pack_seclabel_u64, unpack_seclabel_u64, CapRights, SecLabel};
use crate::syscall::entry::{validate_user_context, USER_RFLAGS_ALLOWED_MASK, USER_RFLAGS_FIXED_SET};

/// `SYS_PROC_SPAWN` `domain_and_comp` bit: `grant_cap1` is the child's fault endpoint.
pub const SPAWN_FLAG_FAULT_EP: u64 = 1 << 48;
/// `MsgTag` label of a kernel-originated fault message (`mr0 = vector | error_code << 16`,
/// `mr1 = rip`, `mr2 = cr2`, `mr3 = rsp`).
pub const FAULT_MSG_LABEL: u32 = 0x7fa1;
/// Reply label: resume the faulting thread (`mr0 != 0` selects a new `RIP`).
pub const FAULT_REPLY_RESUME: u32 = 0x7fa2;
/// Maximum faults delivered per process generation (fault-storm bound).
pub const FAULT_BUDGET: u32 = 16;

static DELIVERED: AtomicU64 = AtomicU64::new(0);
static RESUMED: AtomicU64 = AtomicU64::new(0);

/// `(delivered, resumed)` fault messages since boot.
pub fn fault_ep_telemetry() -> (u64, u64) {
    (DELIVERED.load(Ordering::Relaxed), RESUMED.load(Ordering::Relaxed))
}

/// Validate `cap` as a `SEND | CALL` `Endpoint` capability of the parent (`Err` -> spawn fails).
pub fn check_fault_ep_cap(
    parent_cspace: CSpaceId,
    parent_subj: SecLabel,
    cap: CapPtr,
) -> Result<(), CapError> {
    let req = CapRights(CapRights::SEND.0 | CapRights::CALL.0);
    with_pinned_cap(parent_cspace, parent_subj, cap, req, current_lease_epoch(), |res| {
        if res.obj_kind == CapObjectKind::Endpoint {
            Ok(())
        } else {
            Err(CapError::WrongObjectKind)
        }
    })
}

/// Record the fault endpoint of a freshly spawned child (`pid`).
pub fn bind_fault_endpoint(pid: u32, parent_cspace: CSpaceId, parent_subj: SecLabel, cap: CapPtr) {
    if let Some(slot) = slot_ref(pid) {
        slot.fault_ep_cspace.store(parent_cspace, Ordering::Release);
        slot.fault_ep_label_raw
            .store(pack_seclabel_u64(parent_subj), Ordering::Release);
        slot.fault_budget.store(FAULT_BUDGET, Ordering::Release);
        slot.fault_ep_cap_raw.store(cap.0, Ordering::Release);
    }
}

/// Deliver a `CPL = 3` exception of `pid` to its fault endpoint and apply the supervisor's reply.
/// Returns `true` if the thread must resume (`frame` updated), `false` to terminate as before.
/// Entered and left with `RFLAGS.IF == 0` on the faulting thread's kernel stack.
pub(crate) fn deliver_user_fault(frame: &mut TrapFrame, cr2: u64, pid: u32) -> bool {
    let Some(slot) = slot_ref(pid) else {
        return false;
    };
    let cap_raw = slot.fault_ep_cap_raw.load(Ordering::Acquire);
    if cap_raw == 0
        || slot
            .fault_budget
            .try_update(Ordering::AcqRel, Ordering::Acquire, |b| b.checked_sub(1))
            .is_err()
    {
        return false;
    }
    // Pin like a syscall so `reclaim_process` waits for us; never start a call once killed.
    slot.in_flight_syscalls.fetch_add(1, Ordering::SeqCst);
    let alive = |s: &super::ProcessSlot| {
        !s.kill_requested.load(Ordering::SeqCst)
            && s.state.load(Ordering::SeqCst) == (ProcessState::Running as u8)
    };
    if !alive(slot) {
        slot.in_flight_syscalls.fetch_sub(1, Ordering::SeqCst);
        return false;
    }

    let cs = slot.fault_ep_cspace.load(Ordering::Acquire);
    let lbl = unpack_seclabel_u64(slot.fault_ep_label_raw.load(Ordering::Acquire));
    let input = IpcSendInput {
        tag: MsgTag::new(FAULT_MSG_LABEL, 0, 4, 0),
        mrs: [
            (frame.vector & 0xff) | ((frame.error_code & 0xffff_ffff) << 16),
            frame.rip,
            cr2,
            frame.rsp,
        ],
        extra: &[],
        grant: None,
        shared_desc: None,
    };
    DELIVERED.fetch_add(1, Ordering::Relaxed);
    sti();
    let res = ep_send_or_call(cs, lbl, CapPtr(cap_raw), input, true, 0, None);
    cli();
    slot.in_flight_syscalls.fetch_sub(1, Ordering::SeqCst);
    if !alive(slot) {
        return false;
    }
    let Ok(out) = res else {
        return false;
    };
    if out.tag.label() != FAULT_REPLY_RESUME {
        return false;
    }
    let new_rip = if out.mrs[0] != 0 { out.mrs[0] } else { frame.rip };
    let rflags = (frame.rflags & USER_RFLAGS_ALLOWED_MASK) | USER_RFLAGS_FIXED_SET;
    if validate_user_context(new_rip, frame.rsp, rflags, frame.cs, frame.ss, false).is_err() {
        return false;
    }
    frame.rip = new_rip;
    frame.rflags = rflags;
    RESUMED.fetch_add(1, Ordering::Relaxed);
    true
}

/// Boot self-test: run the Ring 3 `dry-fault-test.elf` supervisor (resume, kill policy, storm
/// bound, revoked capability, forged message, kill-while-blocked) and audit frame accounting.
pub fn verify_fault_endpoint(online_cpus: usize) {
    use crate::pmm::{flush_all_magazines_quiescent, measure_fragmentation};
    use crate::proc::supervisor::mint_boot_module_cap;
    use crate::proc::{reclaim_process, spawn_elf_process_with, ProcessOutcome, ReclaimStatus, SpawnOptions};
    use crate::sched::preempt_tests::{begin_preemptive_epoch, drain_preemptive_epoch_and_audit};
    use crate::security::{SecDomain, SecFlags};

    const SUPERVISOR_MAGIC: u64 = 0x4641_554c_5445_5031;
    let elf = crate::limine::get_boot_module("dry-fault-test.elf")
        .expect("Missing Limine module dry-fault-test.elf");
    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);
    let (d0, r0) = fault_ep_telemetry();

    crate::sched::set_preemptive_mode_active(true);
    crate::sched::init_scheduler(online_cpus);
    begin_preemptive_epoch(0xfe, 1);
    let lbl = SecLabel::new(SecDomain::RootSystem, SecFlags::NONE, 0x0030, 1);
    let info = spawn_elf_process_with(
        elf,
        SpawnOptions {
            subj_label: lbl,
            arg0: 0,
            arg1: SUPERVISOR_MAGIC,
            target_cpu: 0,
            trace_lifecycle: false,
            user_fs_base: 0,
            user_gs_base: 0,
        },
        |cs, l, _, _| Ok((mint_boot_module_cap(cs, l, "dry-fault-test.elf")?.0, SUPERVISOR_MAGIC)),
    )
    .expect("spawn dry-fault-test failed");
    drain_preemptive_epoch_and_audit(online_cpus);
    crate::sched::set_preemptive_mode_active(false);
    assert_eq!(
        reclaim_process(info.handle),
        Ok(ReclaimStatus::ReclaimedNow(ProcessOutcome::Exited(0))),
        "dry-fault-test supervisor reported a FaultEndpoint failure"
    );
    flush_all_magazines_quiescent(online_cpus);
    let after = measure_fragmentation(online_cpus);
    let leaked = before.free_global_frames.saturating_sub(after.free_global_frames);
    assert_eq!(leaked, 0, "Frame leak after FaultEndpoint test: {}", leaked);
    let (d1, r1) = fault_ep_telemetry();
    crate::serial_println!(
        "[DRY-FAULT-EP] PASS: cpus={} delivered={} resumed={} storm_budget={} leaked_frames={}",
        online_cpus, d1 - d0, r1 - r0, FAULT_BUDGET, leaked
    );
}
