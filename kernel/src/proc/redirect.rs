// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-Mediated Linux Syscall Redirection (`Milestone 3 Part B`, `0` new kernel syscalls).
//!
//! When `KernelTcb::syscall_redirect` is armed via `SYS_PROC_SPAWN | SPAWN_FLAG_SYSCALL_REDIRECT`,
//! Ring-3 `SYSCALL` transitions are cold-dispatched over a capability-bound `Endpoint` to a Ring-3
//! personality monitor (`dry-linux-abi`), reusing existing `ep_send_or_call`, `uaccess`, and
//! `CSpace`/`VSpace` primitives without inflating `rust_syscall_dispatch` or exposing raw Linux ABIs
//! inside microkernel hot paths.

use core::sync::atomic::Ordering;

use crate::arch::{cli, sti, wrmsr, MSR_IA32_FS_BASE};
use crate::cspace::cap::{CSpaceId, CapPtr};
use crate::cspace::objects::{cspace_alloc_frame, cspace_vspace_map_4k};
use crate::cspace::revoke::cspace_destroy_object;
use crate::ipc::endpoint::{ep_send_or_call, IpcSendInput};
use crate::ipc::{MsgTag, MAX_IPC_EXTRA_BYTES, MSG_FLAG_EXTRA_BUF};
use crate::percpu::current_percpu;
use crate::pmm::PageOrder;
use crate::proc::{
    enter_process_syscall_or_terminate, leave_process_syscall_or_terminate_if_killed, slot_ref,
    terminate_current_user_thread_bad_context, terminate_current_user_thread_exit, ProcessSlot,
    ProcessState,
};
use crate::sched::tcb;
use crate::security::{current_lease_epoch, pack_seclabel_u64, unpack_seclabel_u64, SecLabel};
use crate::syscall::entry::{
    validate_user_context, SyscallFrame, USER_RFLAGS_ALLOWED_MASK, USER_RFLAGS_FIXED_SET,
};
use crate::uaccess::{copy_from_user, copy_to_user};
use crate::vspace::pte::{
    is_canonical_48, validate_user_range, VmProt, PAGE_SIZE_4K, USER_VADDR_LIMIT,
};

pub const SPAWN_FLAG_SYSCALL_REDIRECT: u64 = 1 << 49;

pub const SYSCALL_REDIRECT_LABEL: u32 = 0x7fa8;
pub const SYSCALL_REPLY_RET: u32 = 0x7fa9;
pub const SYSCALL_REPLY_MEM_READ: u32 = 0x7faa;
pub const SYSCALL_REPLY_MEM_WRITE: u32 = 0x7fab;
pub const SYSCALL_REPLY_MAP_ANON: u32 = 0x7fac;
pub const SYSCALL_REPLY_EXIT: u32 = 0x7fad;

const MAX_REDIRECT_SUBOPS: usize = 64;
const MAX_ANON_MAP_PAGES: u64 = 32;

#[inline]
fn slot_alive(slot: &ProcessSlot) -> bool {
    !slot.kill_requested.load(Ordering::SeqCst)
        && slot.state.load(Ordering::SeqCst) == (ProcessState::Running as u8)
}

fn handle_mem_read_subop(
    guest_va: u64,
    req_len: usize,
    out_buf: &mut [u8; MAX_IPC_EXTRA_BYTES],
) -> (MsgTag, [u64; 4], usize) {
    if req_len > MAX_IPC_EXTRA_BYTES {
        return (
            MsgTag::new(SYSCALL_REDIRECT_LABEL, 0, 4, 0),
            [(-90i64) as u64, 0, 0, 1],
            0,
        );
    }
    match copy_from_user(&mut out_buf[..req_len], guest_va) {
        Ok(()) => (
            MsgTag::new(
                SYSCALL_REDIRECT_LABEL,
                if req_len > 0 { MSG_FLAG_EXTRA_BUF } else { 0 },
                4,
                req_len as u16,
            ),
            [0, req_len as u64, 0, 1],
            req_len,
        ),
        Err(e) => (
            MsgTag::new(SYSCALL_REDIRECT_LABEL, 0, 4, 0),
            [(e.as_raw() as i64) as u64, 0, 0, 1],
            0,
        ),
    }
}

fn handle_mem_write_subop(
    guest_va: u64,
    req_len: usize,
    avail_extra: usize,
    in_buf: &[u8; MAX_IPC_EXTRA_BYTES],
) -> (MsgTag, [u64; 4], usize) {
    if req_len > avail_extra {
        return (
            MsgTag::new(SYSCALL_REDIRECT_LABEL, 0, 4, 0),
            [(-22i64) as u64, 0, 0, 1],
            0,
        );
    }
    match copy_to_user(guest_va, &in_buf[..req_len]) {
        Ok(()) => (
            MsgTag::new(SYSCALL_REDIRECT_LABEL, 0, 4, 0),
            [0, req_len as u64, 0, 1],
            0,
        ),
        Err(e) => (
            MsgTag::new(SYSCALL_REDIRECT_LABEL, 0, 4, 0),
            [(e.as_raw() as i64) as u64, 0, 0, 1],
            0,
        ),
    }
}

fn handle_map_anon_subop(slot: &ProcessSlot, va: u64, pages: u64, prot_bits: u64) -> (MsgTag, [u64; 4], usize) {
    let fail = |err: i64| {
        (
            MsgTag::new(SYSCALL_REDIRECT_LABEL, 0, 4, 0),
            [err as u64, 0, 0, 1],
            0,
        )
    };
    if (va & (PAGE_SIZE_4K - 1)) != 0 || !(1..=MAX_ANON_MAP_PAGES).contains(&pages) {
        return fail(-22);
    }
    let total_bytes = pages * PAGE_SIZE_4K;
    if validate_user_range(va, total_bytes, PAGE_SIZE_4K).is_err() {
        return fail(-22);
    }
    let write_bit = (prot_bits & 2) != 0;
    let exec_bit = (prot_bits & 4) != 0;
    if write_bit && exec_bit {
        return fail(-1);
    }
    let prot = if exec_bit {
        VmProt::USER_RX
    } else if write_bit {
        VmProt::USER_RW
    } else {
        VmProt::USER_RO
    };

    let cs_id = slot.cspace_id.load(Ordering::Acquire);
    let vs_cap = CapPtr(slot.vs_cap_raw.load(Ordering::Acquire));
    let subj = unpack_seclabel_u64(slot.subj_label_raw.load(Ordering::Acquire));
    let now_epoch = current_lease_epoch();

    let mut mapped_caps = [CapPtr::NULL; MAX_ANON_MAP_PAGES as usize];
    for idx in 0..(pages as usize) {
        let Ok((fcap, _)) = cspace_alloc_frame(
            cs_id,
            subj,
            subj,
            PageOrder::Order4KiB,
            exec_bit,
            now_epoch,
        ) else {
            for &cap in &mapped_caps[..idx] {
                let _ = cspace_destroy_object(cs_id, subj, cap, now_epoch);
            }
            return fail(-12);
        };
        let page_va = va + (idx as u64) * PAGE_SIZE_4K;
        if cspace_vspace_map_4k(cs_id, subj, vs_cap, fcap, page_va, prot, now_epoch).is_err() {
            let _ = cspace_destroy_object(cs_id, subj, fcap, now_epoch);
            for &cap in &mapped_caps[..idx] {
                let _ = cspace_destroy_object(cs_id, subj, cap, now_epoch);
            }
            return fail(-12);
        }
        mapped_caps[idx] = fcap;
    }

    (
        MsgTag::new(SYSCALL_REDIRECT_LABEL, 0, 4, 0),
        [va, pages, 0, 1],
        0,
    )
}

pub(crate) fn bind_redirect_endpoint(
    slot: &ProcessSlot,
    redirect_ep: Option<(CSpaceId, SecLabel, CapPtr)>,
) -> bool {
    if let Some((cs_id, subj, cap)) = redirect_ep {
        slot.redirect_ep_cspace.store(cs_id, Ordering::Release);
        slot.redirect_ep_label_raw
            .store(pack_seclabel_u64(subj), Ordering::Release);
        slot.redirect_ep_cap_raw.store(cap.0, Ordering::Release);
        true
    } else {
        slot.redirect_ep_cspace.store(0, Ordering::Release);
        slot.redirect_ep_label_raw.store(0, Ordering::Release);
        slot.redirect_ep_cap_raw.store(0, Ordering::Release);
        false
    }
}

/// Forward an intercepted Ring-3 `SYSCALL` over the process's bound personality `Endpoint`.
/// Entered with `RFLAGS.IF == 0`; returns `2` for `sysretq` preserving guest GPRs (`rdi..r9`).
#[cold]
#[inline(never)]
pub(crate) fn dispatch_redirected_syscall(
    pid: u32,
    cur_slot: usize,
    frame: &mut SyscallFrame,
) -> u64 {
    let _trace = enter_process_syscall_or_terminate(pid);
    let Some(slot) = slot_ref(pid) else {
        terminate_current_user_thread_exit(pid, -1);
    };
    let ep_cap = slot.redirect_ep_cap_raw.load(Ordering::Acquire);
    let ep_cs = slot.redirect_ep_cspace.load(Ordering::Acquire);
    let ep_lbl = unpack_seclabel_u64(slot.redirect_ep_label_raw.load(Ordering::Acquire));
    if ep_cap == 0 {
        terminate_current_user_thread_exit(pid, -1);
    }

    let mut io_buf = [0u8; MAX_IPC_EXTRA_BYTES];
    io_buf[0..8].copy_from_slice(&frame.r10.to_le_bytes());
    io_buf[8..16].copy_from_slice(&frame.r8.to_le_bytes());
    io_buf[16..24].copy_from_slice(&frame.r9.to_le_bytes());

    let mut next_tag = MsgTag::new(SYSCALL_REDIRECT_LABEL, MSG_FLAG_EXTRA_BUF, 4, 24);
    let mut next_mrs = [frame.rax, frame.rdi, frame.rsi, frame.rdx];
    let mut next_extra_len = 24usize;
    let mut reply_extra = [0u8; MAX_IPC_EXTRA_BYTES];

    sti();
    for _ in 0..MAX_REDIRECT_SUBOPS {
        let res = ep_send_or_call(
            ep_cs,
            ep_lbl,
            CapPtr(ep_cap),
            IpcSendInput {
                tag: next_tag,
                mrs: next_mrs,
                extra: &io_buf[..next_extra_len],
                grant: None,
                shared_desc: None,
            },
            true,
            0,
            Some(&mut reply_extra[..]),
        );
        if !slot_alive(slot) {
            cli();
            terminate_current_user_thread_exit(pid, -1);
        }
        let Ok(out) = res else {
            cli();
            terminate_current_user_thread_exit(pid, -1);
        };

        match out.tag.label() {
            SYSCALL_REPLY_RET => {
                if (out.mrs[3] & 1) != 0 {
                    let new_fs = out.mrs[1];
                    if new_fs != 0 && (!is_canonical_48(new_fs) || new_fs >= USER_VADDR_LIMIT) {
                        cli();
                        terminate_current_user_thread_exit(pid, -1);
                    }
                    cli();
                    tcb(cur_slot).user_fs_base.store(new_fs, Ordering::Release);
                    slot.user_fs_base.store(new_fs, Ordering::Release);
                    let pcpu = current_percpu();
                    if pcpu.cur_fs_base.load(Ordering::Relaxed) != new_fs {
                        // SAFETY: Validated canonical lower-half `new_fs` written to `IA32_FS_BASE` under `CLI`.
                        unsafe { wrmsr(MSR_IA32_FS_BASE, new_fs) };
                        pcpu.cur_fs_base.store(new_fs, Ordering::Relaxed);
                    }
                } else {
                    cli();
                }
                leave_process_syscall_or_terminate_if_killed(pid);
                let rflags = (frame.rflags & USER_RFLAGS_ALLOWED_MASK) | USER_RFLAGS_FIXED_SET;
                if let Err(ctx_err) = validate_user_context(
                    frame.rip, frame.rsp, rflags, frame.cs, frame.ss, false,
                ) {
                    terminate_current_user_thread_bad_context(pid, frame, ctx_err);
                }
                frame.rflags = rflags;
                frame.rax = out.mrs[0];
                return 2;
            }
            SYSCALL_REPLY_EXIT => {
                cli();
                terminate_current_user_thread_exit(pid, out.mrs[0] as i32);
            }
            SYSCALL_REPLY_MEM_READ => {
                (next_tag, next_mrs, next_extra_len) =
                    handle_mem_read_subop(out.mrs[0], out.mrs[1] as usize, &mut io_buf);
            }
            SYSCALL_REPLY_MEM_WRITE => {
                let avail = out.tag.extra_len().min(MAX_IPC_EXTRA_BYTES);
                (next_tag, next_mrs, next_extra_len) = handle_mem_write_subop(
                    out.mrs[0],
                    out.mrs[1] as usize,
                    avail,
                    &reply_extra,
                );
            }
            SYSCALL_REPLY_MAP_ANON => {
                (next_tag, next_mrs, next_extra_len) =
                    handle_map_anon_subop(slot, out.mrs[0], out.mrs[1], out.mrs[2]);
            }
            _ => {
                cli();
                terminate_current_user_thread_exit(pid, -1);
            }
        }
    }

    cli();
    terminate_current_user_thread_exit(pid, -1);
}
