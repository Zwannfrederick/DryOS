// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Minimal Ring 3 (`CPL=3`) Process Model, Exception Isolation, and Ordered Idempotent
//! Reclamation (`Milestone 1B-D` Stages A, D, E).
//!
//! # Process Lifecycle State Machine (`Stage E.3`)
//! `Free / Reclaimed --(CAS)--> Creating --> Running --(CAS)--> Exiting | Faulting | Killing`
//! `--(CAS)--> Reaping --> Reclaimed`
//!
//! # Ordered Resource Reclamation Contract (`Stage E.4 & E.5`)
//! 1. `in_flight_syscalls == 0` (`SeqCst` Dekker barrier with `enter_process_syscall_or_terminate`).
//! 2. Bound `TCB` quiescent (`ExecState::Dead && stack_owner == NO_CPU`) -> `cspace_destroy_object(tcb_cap)`.
//! 3. `VSpace.active_cpus == 0` -> `cspace_destroy_object(vs_cap)` (unmaps user PTEs, decrements
//!    `map_ref_count`, and frees owned `PT/PD/PDPT/PML4` frames).
//! 4. Unmap any residual cross-`VSpace` `CapMapRecord`s and destroy all root `Frame4K`/`Frame2M`/
//!    `Untyped` capabilities in `cspace_id` (`free_pages` back to PMM).
//! 5. `destroy_cspace(cspace_id)` (bumps `CNode.generation`) and mark `ProcessState::Reclaimed`.
//!    Repeated `reclaim_process` calls with the same `ProcessHandle` return `AlreadyReclaimedNoOp`.

pub mod bench;
pub mod elf;
pub mod supervisor;
pub mod tests;

use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

use crate::arch::{cli, wrmsr, MSR_IA32_FS_BASE, MSR_IA32_KERNEL_GS_BASE};
use crate::cspace::cap::{unpack_state_gen, CSpaceId, CapError, CapObjectKind, CapPtr, SlotState, CDT_NONE};
use crate::cspace::objects::{cspace_spawn_tcb, cspace_vspace_create};
use crate::cspace::revoke::{cspace_destroy_object, unmap_all_records_for_slot};
use crate::cspace::{create_cspace, destroy_cspace, global_slot_id, CNODE_SLOTS, CSPACE_TABLE};
use crate::gdt_tss::{USER_CS, USER_DS};
use crate::idt::TrapFrame;
use crate::percpu::current_percpu;
use crate::proc::elf::{load_static_elf64_into_vspace, ElfLoadError};
use crate::sched::state::{ExecState, ThreadId};
use crate::sched::{
    current_slot_on_cpu, exit_current_thread, inspect_tcb_live, tcb, wake_thread,
};
use crate::security::{current_lease_epoch, pack_seclabel_u64, unpack_seclabel_u64, SecLabel};
use crate::serial_println;
use crate::syscall::entry::{
    enter_user_iretq_unchecked, validate_user_context, SyscallFrame, UserContextError,
    USER_RFLAGS_FIXED_SET,
};
use crate::vspace::pte::{is_canonical_48, USER_VADDR_LIMIT};
use crate::vspace::{inspect_vspace_live, VSpaceHandle, VSpaceId};

pub const MAX_PROCESSES: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessHandle {
    pub pid: u32,
    pub generation: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ProcessState {
    Free = 0,
    Creating = 1,
    Running = 2,
    Exiting = 3,
    Faulting = 4,
    Killing = 5,
    Reaping = 6,
    Reclaimed = 7,
}

impl ProcessState {
    #[inline]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Free,
            1 => Self::Creating,
            2 => Self::Running,
            3 => Self::Exiting,
            4 => Self::Faulting,
            5 => Self::Killing,
            6 => Self::Reaping,
            _ => Self::Reclaimed,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessFaultInfo {
    pub vector: u64,
    pub error_code: u64,
    pub cr2: u64,
    pub rip: u64,
    pub cs: u64,
    pub rsp: u64,
    pub rflags: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessOutcome {
    Exited(i32),
    Faulted(ProcessFaultInfo),
    Killed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReclaimStatus {
    ReclaimedNow(ProcessOutcome),
    AlreadyReclaimedNoOp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcError {
    OutOfProcesses,
    Elf(ElfLoadError),
    Cap(CapError),
    BadInitialContext(UserContextError),
    StaleOrInvalidHandle,
    StaleTerminated,
    BusyInFlightSyscall,
    BusyActiveThread,
}

#[derive(Clone, Copy)]
pub struct SpawnOptions {
    pub subj_label: SecLabel,
    pub arg0: u64,
    pub arg1: u64,
    pub target_cpu: usize,
    pub trace_lifecycle: bool,
    pub user_fs_base: u64,
    pub user_gs_base: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct SpawnedProcessInfo {
    pub handle: ProcessHandle,
    pub vspace: VSpaceHandle,
    pub cspace_id: CSpaceId,
    pub tid: ThreadId,
    pub tcb_cap: CapPtr,
}

#[repr(C, align(64))]
pub(crate) struct ProcessSlot {
    pub(crate) state: AtomicU8,
    trace_lifecycle: AtomicBool,
    kill_requested: AtomicBool,
    launch_ready: AtomicBool,
    pub(crate) tcb_quiescent: AtomicBool,
    cspace_id: AtomicU16,
    vspace_id: AtomicU16,
    pub(crate) generation: AtomicU32,
    last_reclaimed_gen: AtomicU32,
    vspace_gen: AtomicU32,
    in_flight_syscalls: AtomicU32,
    syscall_entries: AtomicU64,
    exit_code: AtomicI32,
    subj_label_raw: AtomicU64,
    vs_cap_raw: AtomicU64,
    tcb_cap_raw: AtomicU64,
    pub(crate) tcb_tid_raw: AtomicU32,
    pub(crate) parent_pid: AtomicU32,
    pub(crate) waiter_tid_raw: AtomicU32,
    entry_rip: AtomicU64,
    initial_rsp: AtomicU64,
    initial_rflags: AtomicU64,
    arg0: AtomicU64,
    arg1: AtomicU64,
    user_fs_base: AtomicU64,
    user_gs_base: AtomicU64,
    fault_vec: AtomicU64,
    fault_err: AtomicU64,
    fault_cr2: AtomicU64,
    fault_rip: AtomicU64,
    fault_cs: AtomicU64,
    fault_rsp: AtomicU64,
    fault_rflags: AtomicU64,
}

impl ProcessSlot {
    const fn new() -> Self {
        Self {
            state: AtomicU8::new(ProcessState::Free as u8),
            trace_lifecycle: AtomicBool::new(false),
            kill_requested: AtomicBool::new(false),
            launch_ready: AtomicBool::new(false),
            tcb_quiescent: AtomicBool::new(false),
            cspace_id: AtomicU16::new(0),
            vspace_id: AtomicU16::new(0),
            generation: AtomicU32::new(1),
            last_reclaimed_gen: AtomicU32::new(0),
            vspace_gen: AtomicU32::new(0),
            in_flight_syscalls: AtomicU32::new(0),
            syscall_entries: AtomicU64::new(0),
            exit_code: AtomicI32::new(0),
            subj_label_raw: AtomicU64::new(0),
            vs_cap_raw: AtomicU64::new(0),
            tcb_cap_raw: AtomicU64::new(0),
            tcb_tid_raw: AtomicU32::new(0),
            parent_pid: AtomicU32::new(0),
            waiter_tid_raw: AtomicU32::new(0),
            entry_rip: AtomicU64::new(0),
            initial_rsp: AtomicU64::new(0),
            initial_rflags: AtomicU64::new(USER_RFLAGS_FIXED_SET),
            arg0: AtomicU64::new(0),
            arg1: AtomicU64::new(0),
            user_fs_base: AtomicU64::new(0),
            user_gs_base: AtomicU64::new(0),
            fault_vec: AtomicU64::new(0),
            fault_err: AtomicU64::new(0),
            fault_cr2: AtomicU64::new(0),
            fault_rip: AtomicU64::new(0),
            fault_cs: AtomicU64::new(0),
            fault_rsp: AtomicU64::new(0),
            fault_rflags: AtomicU64::new(0),
        }
    }
}

static PROCESS_TABLE: [ProcessSlot; MAX_PROCESSES + 1] =
    [const { ProcessSlot::new() }; MAX_PROCESSES + 1];

#[inline]
pub(crate) fn slot_ref(pid: u32) -> Option<&'static ProcessSlot> {
    let idx = pid as usize;
    if idx == 0 || idx > MAX_PROCESSES {
        None
    } else {
        Some(&PROCESS_TABLE[idx])
    }
}

fn destroy_cspace_owned_resources(cspace_id: CSpaceId, subj_label: SecLabel, vs_cap: CapPtr) {
    let now_epoch = current_lease_epoch();
    if vs_cap != CapPtr::NULL {
        let _ = cspace_destroy_object(cspace_id, subj_label, vs_cap, now_epoch);
    }
    let cnode = &CSPACE_TABLE[cspace_id as usize];
    let guard = cnode.guard.load(Ordering::Relaxed);
    let radix = cnode.radix_bits.load(Ordering::Relaxed);
    for idx in 1..CNODE_SLOTS {
        let slot = &cnode.slots[idx];
        let (st, kind, _, sgen) = unpack_state_gen(slot.state_gen.load(Ordering::Acquire));
        if st == SlotState::Live
            && slot.cdt_parent.load(Ordering::Acquire) == CDT_NONE
            && matches!(
                kind,
                CapObjectKind::Frame4K
                    | CapObjectKind::Frame2M
                    | CapObjectKind::Untyped
                    | CapObjectKind::Endpoint
                    | CapObjectKind::Notification
            )
        {
            let _ = unmap_all_records_for_slot(global_slot_id(cspace_id, idx as u16));
            let ptr = CapPtr::pack(guard, radix, sgen, idx as u16);
            let _ = cspace_destroy_object(cspace_id, subj_label, ptr, now_epoch);
        }
    }
    let _ = destroy_cspace(cspace_id);
}

/// Spawn an isolated `CPL=3` process from `elf_bytes` with private `CSpace` + `VSpace`, optional
/// pre-launch capability/mapping setup closure, and validated `IRETQ` context.
pub fn spawn_elf_process_with<F>(
    elf_bytes: &[u8],
    opts: SpawnOptions,
    setup: F,
) -> Result<SpawnedProcessInfo, ProcError>
where
    F: FnOnce(CSpaceId, SecLabel, CapPtr, VSpaceHandle) -> Result<(u64, u64), CapError>,
{
    if (opts.user_fs_base != 0
        && (!is_canonical_48(opts.user_fs_base) || opts.user_fs_base >= USER_VADDR_LIMIT))
        || (opts.user_gs_base != 0
            && (!is_canonical_48(opts.user_gs_base) || opts.user_gs_base >= USER_VADDR_LIMIT))
    {
        return Err(ProcError::BadInitialContext(
            UserContextError::NonCanonicalOrKernelRsp,
        ));
    }

    let mut claimed: Option<(u32, u8, u32)> = None;
    for pid in 1..=(MAX_PROCESSES as u32) {
        let slot = &PROCESS_TABLE[pid as usize];
        let cur = slot.state.load(Ordering::Acquire);
        if (cur == (ProcessState::Free as u8) || cur == (ProcessState::Reclaimed as u8))
            && slot
                .state
                .compare_exchange(
                    cur,
                    ProcessState::Creating as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        {
            let prev_gen = slot.generation.load(Ordering::Acquire);
            let next_gen = if cur == (ProcessState::Reclaimed as u8) {
                prev_gen.wrapping_add(1).max(1)
            } else {
                prev_gen.max(1)
            };
            slot.generation.store(next_gen, Ordering::Release);
            claimed = Some((pid, cur, next_gen));
            break;
        }
    }
    let (pid, prev_state_u8, pgen) = claimed.ok_or(ProcError::OutOfProcesses)?;
    let slot = &PROCESS_TABLE[pid as usize];

    let abort_slot = |prev_st: u8| {
        slot.state.store(prev_st, Ordering::Release);
    };

    let (cspace_id, _) = match create_cspace(opts.subj_label, 0) {
        Ok(v) => v,
        Err(e) => {
            abort_slot(prev_state_u8);
            return Err(ProcError::Cap(e));
        }
    };

    let (vs_cap, vs_h) = match cspace_vspace_create(cspace_id, opts.subj_label, 0) {
        Ok(v) => v,
        Err(e) => {
            let _ = destroy_cspace(cspace_id);
            abort_slot(prev_state_u8);
            return Err(ProcError::Cap(e));
        }
    };

    let image = match load_static_elf64_into_vspace(
        elf_bytes,
        cspace_id,
        opts.subj_label,
        vs_cap,
        vs_h,
        0,
    ) {
        Ok(img) => img,
        Err(e) => {
            destroy_cspace_owned_resources(cspace_id, opts.subj_label, vs_cap);
            abort_slot(prev_state_u8);
            return Err(ProcError::Elf(e));
        }
    };

    debug_assert!(image.mapped_pages >= 5);
    if let Err(ctx_err) = validate_user_context(
        image.entry_rip,
        image.initial_rsp,
        USER_RFLAGS_FIXED_SET,
        USER_CS as u64,
        USER_DS as u64,
        true,
    ) {
        destroy_cspace_owned_resources(cspace_id, opts.subj_label, vs_cap);
        abort_slot(prev_state_u8);
        return Err(ProcError::BadInitialContext(ctx_err));
    }

    let (final_arg0, final_arg1) = match setup(cspace_id, opts.subj_label, vs_cap, vs_h) {
        Ok(pair) => pair,
        Err(e) => {
            destroy_cspace_owned_resources(cspace_id, opts.subj_label, vs_cap);
            abort_slot(prev_state_u8);
            return Err(ProcError::Cap(e));
        }
    };

    slot.launch_ready.store(false, Ordering::Release);
    slot.kill_requested.store(false, Ordering::Release);
    slot.tcb_quiescent.store(false, Ordering::Release);
    slot.parent_pid.store(0, Ordering::Release);
    slot.waiter_tid_raw.store(0, Ordering::Release);
    slot.trace_lifecycle.store(opts.trace_lifecycle, Ordering::Release);
    slot.cspace_id.store(cspace_id, Ordering::Release);
    slot.vspace_id.store(vs_h.id, Ordering::Release);
    slot.vspace_gen.store(vs_h.generation, Ordering::Release);
    slot.in_flight_syscalls.store(0, Ordering::Release);
    slot.syscall_entries.store(0, Ordering::Release);
    slot.exit_code.store(0, Ordering::Release);
    slot.subj_label_raw
        .store(pack_seclabel_u64(opts.subj_label), Ordering::Release);
    slot.vs_cap_raw.store(vs_cap.0, Ordering::Release);
    slot.entry_rip.store(image.entry_rip, Ordering::Release);
    slot.initial_rsp.store(image.initial_rsp, Ordering::Release);
    slot.initial_rflags
        .store(USER_RFLAGS_FIXED_SET, Ordering::Release);
    slot.arg0.store(final_arg0, Ordering::Release);
    slot.arg1.store(final_arg1, Ordering::Release);
    slot.user_fs_base.store(opts.user_fs_base, Ordering::Release);
    slot.user_gs_base.store(opts.user_gs_base, Ordering::Release);
    slot.fault_vec.store(0, Ordering::Release);
    slot.fault_err.store(0, Ordering::Release);
    slot.fault_cr2.store(0, Ordering::Release);
    slot.fault_rip.store(0, Ordering::Release);
    slot.fault_cs.store(0, Ordering::Release);
    slot.fault_rsp.store(0, Ordering::Release);
    slot.fault_rflags.store(0, Ordering::Release);

    let now_epoch = current_lease_epoch();
    let (tcb_cap, tid) = match cspace_spawn_tcb(
        cspace_id,
        opts.subj_label,
        vs_cap,
        1,
        opts.target_cpu,
        user_thread_bootstrap_entry,
        pid as u64,
        crate::sched::is_preemptive_mode_active(),
        now_epoch,
    ) {
        Ok(v) => v,
        Err(e) => {
            destroy_cspace_owned_resources(cspace_id, opts.subj_label, vs_cap);
            abort_slot(prev_state_u8);
            return Err(ProcError::Cap(e));
        }
    };

    let w_tcb = tcb(tid.slot());
    w_tcb.process_id.store(pid, Ordering::Release);
    w_tcb.user_fs_base.store(opts.user_fs_base, Ordering::Release);
    w_tcb.user_gs_base.store(opts.user_gs_base, Ordering::Release);

    slot.tcb_tid_raw.store(tid.raw(), Ordering::Release);
    slot.tcb_cap_raw.store(tcb_cap.0, Ordering::Release);
    slot.state
        .store(ProcessState::Running as u8, Ordering::Release);

    if opts.trace_lifecycle {
        serial_println!(
            "[DRY-PROC] PID={} VSpace={} CSpace={}",
            pid,
            vs_h.id,
            cspace_id
        );
    }
    slot.launch_ready.store(true, Ordering::Release);

    Ok(SpawnedProcessInfo {
        handle: ProcessHandle {
            pid,
            generation: pgen,
        },
        vspace: vs_h,
        cspace_id,
        tid,
        tcb_cap,
    })
}

pub fn spawn_elf_process(elf_bytes: &[u8], opts: SpawnOptions) -> Result<SpawnedProcessInfo, ProcError> {
    spawn_elf_process_with(elf_bytes, opts, |_, _, _, _| Ok((opts.arg0, opts.arg1)))
}

fn user_thread_bootstrap_entry(pid_raw: u64) {
    let pid = pid_raw as u32;
    let slot = slot_ref(pid).expect("Invalid PID in user_thread_bootstrap_entry");
    while !slot.launch_ready.load(Ordering::Acquire) {
        if crate::is_panicking() {
            crate::arch::halt_forever();
        }
        core::hint::spin_loop();
    }

    cli();
    let pcpu = current_percpu();
    let cpu_idx = pcpu.cpu_idx as usize;
    let cur_slot = current_slot_on_cpu(cpu_idx);
    let cur_tcb = tcb(cur_slot);
    assert_eq!(cur_tcb.process_id.load(Ordering::Acquire), pid);

    if slot.kill_requested.load(Ordering::SeqCst) {
        exit_current_thread();
    }

    let u_fs = cur_tcb.user_fs_base.load(Ordering::Acquire);
    let u_gs = cur_tcb.user_gs_base.load(Ordering::Acquire);
    // SAFETY: Validated canonical lower-half `FS_BASE` / `KERNEL_GS_BASE` synchronized under `CLI`.
    unsafe {
        if pcpu.cur_fs_base.load(Ordering::Relaxed) != u_fs {
            wrmsr(MSR_IA32_FS_BASE, u_fs);
            pcpu.cur_fs_base.store(u_fs, Ordering::Relaxed);
        }
        if pcpu.cur_user_gs_base.load(Ordering::Relaxed) != u_gs {
            wrmsr(MSR_IA32_KERNEL_GS_BASE, u_gs);
            pcpu.cur_user_gs_base.store(u_gs, Ordering::Relaxed);
        }
    }

    let kstack_top = cur_tcb.stack_top.load(Ordering::Relaxed);
    let rip = slot.entry_rip.load(Ordering::Acquire);
    let rsp = slot.initial_rsp.load(Ordering::Acquire);
    let rflags = slot.initial_rflags.load(Ordering::Acquire);
    let a0 = slot.arg0.load(Ordering::Acquire);
    let a1 = slot.arg1.load(Ordering::Acquire);

    if slot.trace_lifecycle.load(Ordering::Acquire) {
        serial_println!("[DRY-USER] Entering CPL3");
    }

    // SAFETY: `validate_user_context` verified `(rip, rsp, rflags, USER_CS, USER_DS)` at spawn;
    // `complete_post_switch_from_asm` loaded `slot.vspace_id` into `CR3` and set `TSS.RSP0 = kstack_top`.
    unsafe { enter_user_iretq_unchecked(kstack_top, rip, rsp, rflags, a0, a1, null_mut()) }
}

/// Request asynchronous termination of `handle` (`Running -> Killing`, `Stage F.10`).
pub fn kill_process(handle: ProcessHandle) -> Result<(), ProcError> {
    let slot = slot_ref(handle.pid).ok_or(ProcError::StaleOrInvalidHandle)?;
    if slot.generation.load(Ordering::Acquire) != handle.generation {
        return Err(ProcError::StaleOrInvalidHandle);
    }
    match slot.state.compare_exchange(
        ProcessState::Running as u8,
        ProcessState::Killing as u8,
        Ordering::SeqCst,
        Ordering::SeqCst,
    ) {
        Ok(_) => {
            slot.kill_requested.store(true, Ordering::SeqCst);
            crate::ipc::on_process_terminating(handle.pid, slot.cspace_id.load(Ordering::Acquire));
            let tid = ThreadId::from_raw(slot.tcb_tid_raw.load(Ordering::Acquire));
            let _ = wake_thread(tid, 0);
            Ok(())
        }
        Err(cur) => {
            if cur == (ProcessState::Free as u8) {
                Err(ProcError::StaleOrInvalidHandle)
            } else {
                Err(ProcError::StaleTerminated)
            }
        }
    }
}

/// Inspect live state of `handle`; rejects stale or post-termination handles (`Stage F.13`).
pub fn inspect_live_process(handle: ProcessHandle) -> Result<(ProcessState, VSpaceId, CSpaceId), ProcError> {
    let slot = slot_ref(handle.pid).ok_or(ProcError::StaleOrInvalidHandle)?;
    if slot.generation.load(Ordering::Acquire) != handle.generation {
        return Err(ProcError::StaleOrInvalidHandle);
    }
    let st = ProcessState::from_u8(slot.state.load(Ordering::Acquire));
    match st {
        ProcessState::Free => Err(ProcError::StaleOrInvalidHandle),
        ProcessState::Reclaimed => Err(ProcError::StaleTerminated),
        _ => Ok((
            st,
            slot.vspace_id.load(Ordering::Acquire),
            slot.cspace_id.load(Ordering::Acquire),
        )),
    }
}

#[inline]
pub fn process_syscall_entries(handle: ProcessHandle) -> u64 {
    slot_ref(handle.pid).map_or(0, |s| s.syscall_entries.load(Ordering::Acquire))
}

#[inline]
pub(crate) fn is_process_kill_requested(pid: u32) -> bool {
    slot_ref(pid).is_some_and(|s| s.kill_requested.load(Ordering::Acquire))
}

pub(crate) fn activate_raw_bench_pid_slot(pid: u32) {
    let slot = slot_ref(pid).expect("bench pid slot");
    slot.kill_requested.store(false, Ordering::Release);
    slot.trace_lifecycle.store(false, Ordering::Release);
    slot.in_flight_syscalls.store(0, Ordering::Release);
    slot.state.store(ProcessState::Running as u8, Ordering::Release);
}

pub(crate) fn release_raw_bench_pid_slot(pid: u32) {
    let slot = slot_ref(pid).expect("bench pid slot");
    let g = slot.generation.load(Ordering::Acquire);
    slot.in_flight_syscalls.store(0, Ordering::Release);
    slot.last_reclaimed_gen.store(g, Ordering::Release);
    slot.state.store(ProcessState::Reclaimed as u8, Ordering::Release);
}

pub(crate) fn terminate_killed_from_cpl3_irq() -> ! {
    exit_current_thread()
}

/// Invoked from `sched::complete_post_switch_from_asm` after `PostSwitchKind::Exit` reaps the TCB.
pub fn on_user_tcb_quiescent_dead(pid: u32) {
    if let Some(slot) = slot_ref(pid) {
        slot.tcb_quiescent.store(true, Ordering::SeqCst);
        let waiter = slot.waiter_tid_raw.swap(0, Ordering::AcqRel);
        if waiter != 0 {
            let _ = wake_thread(ThreadId::from_raw(waiter), 0);
        }
    }
}

/// Fast `CLI`-held liveness & kill gate for stateless register-only syscalls (`SYS_ABI_PING`,
/// `SYS_GET_TID`) that touch zero `CSpace`/`VSpace`/`Endpoint` state (`0` `LOCK` RMWs).
#[inline]
pub fn check_process_alive_fast_under_cli(pid: u32) {
    let Some(slot) = slot_ref(pid) else {
        exit_current_thread();
    };
    if slot.kill_requested.load(Ordering::Acquire)
        || slot.state.load(Ordering::Acquire) != (ProcessState::Running as u8)
    {
        exit_current_thread();
    }
    let next = slot.syscall_entries.load(Ordering::Relaxed).wrapping_add(1);
    slot.syscall_entries.store(next, Ordering::Release);
}

/// Pin `in_flight_syscalls` upon entering `rust_syscall_dispatch` (`CLI` held); if `pid` was killed,
/// drop the pin and exit immediately without executing the syscall.
pub fn enter_process_syscall_or_terminate(pid: u32) -> bool {
    let Some(slot) = slot_ref(pid) else {
        exit_current_thread();
    };
    slot.in_flight_syscalls.fetch_add(1, Ordering::SeqCst);
    if slot.kill_requested.load(Ordering::SeqCst)
        || slot.state.load(Ordering::SeqCst) != (ProcessState::Running as u8)
    {
        slot.in_flight_syscalls.fetch_sub(1, Ordering::SeqCst);
        exit_current_thread();
    }
    let next = slot.syscall_entries.load(Ordering::Relaxed).wrapping_add(1);
    slot.syscall_entries.store(next, Ordering::Release);
    slot.trace_lifecycle.load(Ordering::Relaxed)
}

/// Drop `in_flight_syscalls` prior to leaving `rust_syscall_dispatch` (`CLI` held); if `kill_process`
/// arrived mid-syscall, terminate the calling user thread cleanly before `SYSRETQ`/`IRETQ`.
pub fn leave_process_syscall_or_terminate_if_killed(pid: u32) {
    let Some(slot) = slot_ref(pid) else {
        exit_current_thread();
    };
    let prev = slot.in_flight_syscalls.fetch_sub(1, Ordering::SeqCst);
    debug_assert!(prev >= 1, "in_flight_syscalls underflow");
    if slot.kill_requested.load(Ordering::SeqCst)
        || slot.state.load(Ordering::Acquire) != (ProcessState::Running as u8)
    {
        exit_current_thread();
    }
}

/// Terminate calling user thread from `SYS_EXIT` (`CLI` held).
pub fn terminate_current_user_thread_exit(pid: u32, exit_code: i32) -> ! {
    if let Some(slot) = slot_ref(pid) {
        slot.exit_code.store(exit_code, Ordering::Release);
        let _ = slot.state.compare_exchange(
            ProcessState::Running as u8,
            ProcessState::Exiting as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        crate::ipc::on_process_terminating(pid, slot.cspace_id.load(Ordering::Acquire));
        let prev = slot.in_flight_syscalls.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(prev >= 1, "in_flight_syscalls underflow on exit");
    }
    exit_current_thread()
}

/// Isolate malformed `SYSCALL` return state (`Stage B.5 & F.16`) without executing a Ring 0 `#GP` `SYSRETQ`!
pub fn terminate_current_user_thread_bad_context(
    pid: u32,
    frame: &SyscallFrame,
    err: UserContextError,
) -> ! {
    current_percpu()
        .user_fault_count
        .fetch_add(1, Ordering::Relaxed);
    if let Some(slot) = slot_ref(pid) {
        slot.fault_vec.store(13, Ordering::Release);
        slot.fault_err.store(err as u64, Ordering::Release);
        slot.fault_cr2.store(0, Ordering::Release);
        slot.fault_rip.store(frame.rip, Ordering::Release);
        slot.fault_cs.store(frame.cs, Ordering::Release);
        slot.fault_rsp.store(frame.rsp, Ordering::Release);
        slot.fault_rflags.store(frame.rflags, Ordering::Release);
        let _ = slot.state.compare_exchange(
            ProcessState::Running as u8,
            ProcessState::Faulting as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        crate::ipc::on_process_terminating(pid, slot.cspace_id.load(Ordering::Acquire));
    }
    exit_current_thread()
}

/// Isolate hardware CPU exceptions originating in `CPL = 3` (`(frame.cs & 3) == 3`, `Stage E.1`).
/// Never panics the kernel; records exact hardware trap registers and terminates only the faulting process.
pub fn handle_user_exception(frame: &mut TrapFrame, cr2: u64) -> ! {
    cli();
    let pcpu = current_percpu();
    pcpu.user_fault_count.fetch_add(1, Ordering::Relaxed);
    let cur_slot = current_slot_on_cpu(pcpu.cpu_idx as usize);
    let pid = tcb(cur_slot).process_id.load(Ordering::Acquire);

    if let Some(slot) = slot_ref(pid) {
        slot.fault_vec.store(frame.vector, Ordering::Release);
        slot.fault_err.store(frame.error_code, Ordering::Release);
        slot.fault_cr2.store(cr2, Ordering::Release);
        slot.fault_rip.store(frame.rip, Ordering::Release);
        slot.fault_cs.store(frame.cs, Ordering::Release);
        slot.fault_rsp.store(frame.rsp, Ordering::Release);
        slot.fault_rflags.store(frame.rflags, Ordering::Release);
        let _ = slot.state.compare_exchange(
            ProcessState::Running as u8,
            ProcessState::Faulting as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        crate::ipc::on_process_terminating(pid, slot.cspace_id.load(Ordering::Acquire));
    }
    exit_current_thread()
}

/// Ordered, idempotent process reclamation (`Stage E.4 & E.5`).
///
/// Refuses premature teardown with `BusyInFlightSyscall` / `BusyActiveThread` if any CPU is still
/// inside a syscall, still owns the `TCB` kernel stack, or still holds `VSpace` in `CR3`.
pub fn reclaim_process(handle: ProcessHandle) -> Result<ReclaimStatus, ProcError> {
    let slot = slot_ref(handle.pid).ok_or(ProcError::StaleOrInvalidHandle)?;
    let cur_gen = slot.generation.load(Ordering::Acquire);
    if cur_gen != handle.generation {
        return Err(ProcError::StaleOrInvalidHandle);
    }
    if slot.last_reclaimed_gen.load(Ordering::Acquire) == handle.generation {
        return Ok(ReclaimStatus::AlreadyReclaimedNoOp);
    }

    let cur_st = slot.state.load(Ordering::Acquire);
    let term_st = match ProcessState::from_u8(cur_st) {
        ProcessState::Free => return Err(ProcError::StaleOrInvalidHandle),
        ProcessState::Reclaimed => return Ok(ReclaimStatus::AlreadyReclaimedNoOp),
        ProcessState::Creating | ProcessState::Running | ProcessState::Reaping => {
            if slot.in_flight_syscalls.load(Ordering::SeqCst) != 0 {
                return Err(ProcError::BusyInFlightSyscall);
            }
            return Err(ProcError::BusyActiveThread);
        }
        st @ (ProcessState::Exiting | ProcessState::Faulting | ProcessState::Killing) => st,
    };

    if slot.in_flight_syscalls.load(Ordering::SeqCst) != 0 {
        return Err(ProcError::BusyInFlightSyscall);
    }
    if !slot.tcb_quiescent.load(Ordering::Acquire) {
        return Err(ProcError::BusyActiveThread);
    }

    let tid = ThreadId::from_raw(slot.tcb_tid_raw.load(Ordering::Acquire));
    if let Some((t_state, t_gen, stack_active)) = inspect_tcb_live(tid.slot()) {
        if t_gen == tid.gen26() && (t_state != ExecState::Dead || stack_active) {
            return Err(ProcError::BusyActiveThread);
        }
    }
    let vs_id = slot.vspace_id.load(Ordering::Acquire);
    let vs_gen = slot.vspace_gen.load(Ordering::Acquire);
    if let Some((live_vgen, active_cpus, _)) = inspect_vspace_live(vs_id) {
        if live_vgen == vs_gen && active_cpus != 0 {
            return Err(ProcError::BusyActiveThread);
        }
    }

    slot.state
        .compare_exchange(
            cur_st,
            ProcessState::Reaping as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map_err(|_| ProcError::BusyActiveThread)?;

    let cspace_id = slot.cspace_id.load(Ordering::Acquire);
    let subj_label = unpack_seclabel_u64(slot.subj_label_raw.load(Ordering::Acquire));
    let tcb_cap = CapPtr(slot.tcb_cap_raw.load(Ordering::Acquire));
    let vs_cap = CapPtr(slot.vs_cap_raw.load(Ordering::Acquire));
    let now_epoch = current_lease_epoch();

    // 1. Destroy quiescent `TCB` capability.
    cspace_destroy_object(cspace_id, subj_label, tcb_cap, now_epoch).map_err(ProcError::Cap)?;
    // 2 & 3. Destroy `VSpace` + all owned `Frame4K`/`Frame2M`/`Untyped` caps + `CSpace`.
    destroy_cspace_owned_resources(cspace_id, subj_label, vs_cap);

    let outcome = match term_st {
        ProcessState::Exiting => ProcessOutcome::Exited(slot.exit_code.load(Ordering::Acquire)),
        ProcessState::Faulting => ProcessOutcome::Faulted(ProcessFaultInfo {
            vector: slot.fault_vec.load(Ordering::Acquire),
            error_code: slot.fault_err.load(Ordering::Acquire),
            cr2: slot.fault_cr2.load(Ordering::Acquire),
            rip: slot.fault_rip.load(Ordering::Acquire),
            cs: slot.fault_cs.load(Ordering::Acquire),
            rsp: slot.fault_rsp.load(Ordering::Acquire),
            rflags: slot.fault_rflags.load(Ordering::Acquire),
        }),
        ProcessState::Killing => ProcessOutcome::Killed,
        _ => unreachable!(),
    };

    if slot.trace_lifecycle.load(Ordering::Acquire) {
        serial_println!("[DRY-PROC] PID={} terminated", handle.pid);
    }
    slot.last_reclaimed_gen
        .store(handle.generation, Ordering::Release);
    slot.state
        .store(ProcessState::Reclaimed as u8, Ordering::Release);

    Ok(ReclaimStatus::ReclaimedNow(outcome))
}
