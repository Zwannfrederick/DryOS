// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-Gated Userspace Service Supervision (`SYS_PROC_SPAWN` & `SYS_PROC_WAIT`,
//! `Milestone 1C` Stages C & D, `INV-IPC-08`).
//!
//! Allows the root userspace supervisor (`dry_init` in `SecDomain::RootSystem`) to spawn isolated
//! CPL=3 child services (`dry_echo` in `SecDomain::Userspace`, `dry_display` in display compartment)
//! purely from explicit `CapObjectKind::BootModule` capabilities, delegate initial capabilities into
//! the child's `CSpace`, and reap exited/faulted children without Ring 0 service-manager bloat.

use core::sync::atomic::Ordering;

use crate::arch::rdtsc;
use crate::cspace::cap::{CSpaceId, CapError, CapObjectKind, CapPtr};
use crate::cspace::{cspace_transfer, insert_root_cap, with_pinned_cap};
use crate::ipc::{check_blocking_ipc_allowed, map_cap_err_to_sys};
use crate::limine::{find_boot_module_index, get_boot_module_by_index};
use crate::proc::{
    reclaim_process, slot_ref, spawn_elf_process_with, ProcError, ProcessHandle, ProcessOutcome,
    ReclaimStatus, SpawnOptions,
};
use crate::sched::{
    cancel_park_current, current_cpu_idx, current_slot_on_cpu, current_thread_id, ipc_park_current,
    prepare_park_current, tcb, ONLINE_SCHED_CPUS, PHASE_PENDING_THREADS,
};
use crate::security::{current_lease_epoch, CapRights, SecDomain, SecFlags, SecLabel};
use crate::uaccess::SysError;

#[inline]
pub const fn pack_proc_handle(pid: u32, generation: u32) -> u64 {
    (((generation & 0x7fff_ffff) as u64) << 32) | (pid as u64)
}

#[inline]
pub const fn unpack_proc_handle(raw: u64) -> (u32, u32) {
    ((raw & 0xffff_ffff) as u32, ((raw >> 32) & 0x7fff_ffff) as u32)
}

/// Mint a `CapObjectKind::BootModule` capability into a `SecDomain::RootSystem` supervisor `CSpace`.
pub fn mint_boot_module_cap(
    cspace_id: CSpaceId,
    root_subj: SecLabel,
    module_suffix: &str,
) -> Result<CapPtr, CapError> {
    if root_subj.domain != SecDomain::RootSystem
        || root_subj.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        return Err(CapError::DomainViolation);
    }
    let mod_idx = find_boot_module_index(module_suffix).ok_or(CapError::InvalidHandle)?;
    insert_root_cap(
        cspace_id,
        CapObjectKind::BootModule,
        mod_idx as u32,
        1,
        CapRights::BOOTMOD_ROOT,
        root_subj,
        0,
        0,
    )
}

/// `SYS_PROC_SPAWN`: Spawn an isolated CPL=3 child process from `boot_mod_cap`, optionally
/// transferring up to two initial capabilities (`grant_cap0`, `grant_cap1`) into the child's
/// `CSpace` and passing their child `CapPtr` values in `_start(rdi, rsi)`.
#[allow(clippy::too_many_arguments)]
pub fn sys_proc_spawn(
    parent_pid: u32,
    parent_cspace: CSpaceId,
    parent_subj: SecLabel,
    boot_mod_cap_raw: u64,
    domain_and_comp: u64,
    grant_cap0_raw: u64,
    grant0_rights_badge: u64,
    grant_cap1_raw: u64,
    grant1_rights: u64,
) -> Result<(isize, u64), SysError> {
    if parent_pid == 0 || parent_subj.domain != SecDomain::RootSystem {
        return Err(SysError::Perm);
    }
    if parent_subj.flags.contains(SecFlags::DEV_WORKSPACE) {
        return Err(SysError::Perm);
    }

    let child_dom_code = (domain_and_comp & 0xff) as u8;
    let child_comp = ((domain_and_comp >> 16) & 0xffff) as u16;
    let cpu_hint = ((domain_and_comp >> 32) & 0xffff) as usize;

    let child_domain = match child_dom_code {
        0 => SecDomain::Userspace,
        1 => SecDomain::RootSystem,
        _ => return Err(SysError::Perm),
    };
    let child_label = SecLabel::new(child_domain, SecFlags::NONE, child_comp, 1);

    let now_epoch = current_lease_epoch();
    let req_mod = CapRights(CapRights::READ.0 | CapRights::SPAWN.0);
    let mod_idx = with_pinned_cap(
        parent_cspace,
        parent_subj,
        CapPtr(boot_mod_cap_raw),
        req_mod,
        now_epoch,
        |res| {
            if res.obj_kind != CapObjectKind::BootModule {
                return Err(CapError::WrongObjectKind);
            }
            Ok(res.obj_id as usize)
        },
    )
    .map_err(map_cap_err_to_sys)?;

    let elf_bytes = get_boot_module_by_index(mod_idx).ok_or(SysError::BadCap)?;
    let online = ONLINE_SCHED_CPUS.load(Ordering::Acquire).max(1);
    let target_cpu = if cpu_hint == 0xffff {
        (current_cpu_idx() + 1) % online
    } else {
        cpu_hint % online
    };

    // Reserve an active scheduler epoch thread ticket BEFORE publishing the child's TCB so AP
    // service loops never observe `PHASE_PENDING_THREADS == 0` prematurely on child exit.
    let tracked_in_epoch = PHASE_PENDING_THREADS
        .try_update(Ordering::AcqRel, Ordering::Acquire, |v| {
            (v > 0).then_some(v + 1)
        })
        .is_ok();

    let spawn_res = spawn_elf_process_with(
        elf_bytes,
        SpawnOptions {
            subj_label: child_label,
            arg0: 0,
            arg1: 0,
            target_cpu,
            trace_lifecycle: false,
            user_fs_base: 0,
            user_gs_base: 0,
        },
        |child_cs, _, _, _| {
            let arg0 = if grant_cap0_raw != 0 {
                let rights0 = CapRights((grant0_rights_badge & 0xffff) as u16);
                let badge0 = grant0_rights_badge >> 16;
                let rcpt0 = cspace_transfer(
                    parent_cspace,
                    parent_subj,
                    CapPtr(grant_cap0_raw),
                    child_cs,
                    rights0,
                    0,
                    badge0,
                    now_epoch,
                )?;
                rcpt0.dst_cap.0
            } else {
                0
            };
            let arg1 = if grant_cap1_raw != 0 {
                let rights1 = CapRights((grant1_rights & 0xffff) as u16);
                let rcpt1 = cspace_transfer(
                    parent_cspace,
                    parent_subj,
                    CapPtr(grant_cap1_raw),
                    child_cs,
                    rights1,
                    0,
                    0,
                    now_epoch,
                )?;
                rcpt1.dst_cap.0
            } else {
                0
            };
            Ok((arg0, arg1))
        },
    );

    match spawn_res {
        Ok(info) => {
            if let Some(slot) = slot_ref(info.handle.pid) {
                slot.parent_pid.store(parent_pid, Ordering::Release);
            }
            let packed = pack_proc_handle(info.handle.pid, info.handle.generation);
            Ok((packed as isize, info.handle.pid as u64))
        }
        Err(pe) => {
            if tracked_in_epoch {
                let _ = PHASE_PENDING_THREADS.try_update(
                    Ordering::AcqRel,
                    Ordering::Acquire,
                    |v| (v > 0).then_some(v - 1),
                );
            }
            let se = match pe {
                ProcError::Cap(ce) => map_cap_err_to_sys(ce),
                ProcError::OutOfProcesses => SysError::NoMem,
                _ => SysError::Inval,
            };
            Err(se)
        }
    }
}

/// `SYS_PROC_WAIT`: Wait for a direct child process to exit/fault/terminate and deterministically
/// reclaim 100% of its `TCB`, `VSpace`, frames, IPC objects, and `CSpace`.
pub fn sys_proc_wait(
    caller_pid: u32,
    child_handle_raw: u64,
    flags: u64,
    timeout_ticks: u64,
) -> Result<(isize, u64), SysError> {
    let (pid, gen) = unpack_proc_handle(child_handle_raw);
    let slot = slot_ref(pid).ok_or(SysError::NoChild)?;
    if slot.generation.load(Ordering::Acquire) != gen
        || slot.parent_pid.load(Ordering::Acquire) != caller_pid
    {
        return Err(SysError::NoChild);
    }

    let non_blocking = (flags & 1) != 0;
    let cur_slot = current_slot_on_cpu(current_cpu_idx());
    let cur_tid = current_thread_id();
    let deadline = if timeout_ticks > 0 {
        rdtsc().saturating_add(timeout_ticks).max(1)
    } else {
        0
    };

    loop {
        match reclaim_process(ProcessHandle { pid, generation: gen }) {
            Ok(ReclaimStatus::ReclaimedNow(outcome)) => {
                slot.waiter_tid_raw.store(0, Ordering::Release);
                return Ok(match outcome {
                    ProcessOutcome::Exited(code) => (0, (code as u32) as u64),
                    ProcessOutcome::Faulted(f) => (1, (1u64 << 32) | (f.vector & 0xff)),
                    ProcessOutcome::Killed => (2, 2u64 << 32),
                });
            }
            Ok(ReclaimStatus::AlreadyReclaimedNoOp)
            | Err(ProcError::StaleOrInvalidHandle | ProcError::StaleTerminated) => {
                return Err(SysError::NoChild);
            }
            Err(ProcError::BusyInFlightSyscall | ProcError::BusyActiveThread) => {
                if non_blocking {
                    return Err(SysError::WouldBlock);
                }
                check_blocking_ipc_allowed()?;
                if deadline != 0 && rdtsc() >= deadline {
                    slot.waiter_tid_raw.store(0, Ordering::Release);
                    return Err(SysError::TimedOut);
                }

                prepare_park_current();
                slot.waiter_tid_raw.store(cur_tid.raw(), Ordering::SeqCst);
                if slot.tcb_quiescent.load(Ordering::SeqCst) {
                    slot.waiter_tid_raw.store(0, Ordering::Release);
                    cancel_park_current();
                    continue;
                }

                if deadline != 0 {
                    tcb(cur_slot)
                        .ipc_deadline_tsc
                        .store(deadline, Ordering::Release);
                }
                let _ = ipc_park_current();
                tcb(cur_slot).ipc_deadline_tsc.store(0, Ordering::Release);
                slot.waiter_tid_raw.store(0, Ordering::Release);
            }
            Err(_) => return Err(SysError::Inval),
        }
    }
}
