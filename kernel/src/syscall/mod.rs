// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! DRY Native Syscall ABI (`v0.2`) Dispatcher & Capability/Domain Enforcement
//! (`Milestone 1B-D` Stage B & `Milestone 1C` Stages A, C, D, E).
//!
//! # Versioned ABI Contract (`DRY_SYSCALL_ABI_VERSION = 0x0001_0000`)
//! - **Entry:** `syscall` at `CPL = 3` (`CS = 0x23`, `SS = 0x1b`).
//! - **Arguments (`ADR-002`):** `rax = nr`, `rdi = a0`, `rsi = a1`, `rdx = a2`, `r10 = a3`, `r8 = a4`, `r9 = a5`.
//! - **Inline IPC Message Registers:** `r12 = mr0`, `r13 = mr1`, `r14 = mr2`, `r15 = mr3` (`32 B` zero-copy).
//!   Modified ONLY by `SYS_IPC_RECV`, `SYS_IPC_CALL`, and `SYS_IPC_REPLY_RECV`; preserved on all other syscalls.
//! - **Returns:** `rax = ret0` (`>= 0` on success, `-errno` in `[-4095..-1]` on error), `rdx = ret1`.
//! - **Preserved:** `rbx`, `rbp`, `rsp`, `r12`..`r15` (except explicit IPC receive outputs).
//! - **Clobbered / Scrubbed:** `rcx` (`RIP`), `r11` (`RFLAGS`), and `rdi`, `rsi`, `r8`, `r9`, `r10`.

pub mod entry;

use core::mem::size_of;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::arch::{cli, read_rflags, read_rsp, sti};
use crate::cspace::cap::{CapError, CapPtr};
use crate::cspace::revoke::{cspace_delete_cap, cspace_destroy_object, cspace_revoke_children};
use crate::cspace::{cspace_derive, with_pinned_cap};
use crate::display::sys_fb_map;
use crate::ipc::endpoint::{ep_create, ep_recv, ep_reply, ep_reply_recv, ep_send_or_call, IpcSendInput};
use crate::ipc::notif::{notif_create, notif_signal, notif_wait};
use crate::ipc::{
    map_cap_err_to_sys, MsgTag, SharedPayloadDesc, UserIpcRecvExt, UserIpcSendExt,
    MAX_IPC_EXTRA_BYTES, MSG_FLAG_EXTRA_BUF, MSG_FLAG_GRANT_CAP, MSG_FLAG_NON_BLOCKING,
    MSG_FLAG_SHARED_DESC,
};
use crate::percpu::current_percpu;
use crate::proc::supervisor::{sys_proc_spawn, sys_proc_wait};
use crate::sched::{
    current_slot_on_cpu, current_thread_id, set_ap_epoch_kind, tcb, yield_now, ApEpochKind,
    AP_PHASE_DONE_CPUS, AP_PHASE_EPOCH,
};
use crate::security::{
    current_lease_epoch, unpack_seclabel_u64, CapRights, SecDomain, SecFlags, SecLabel,
};
use crate::serial::write_sanitized_user_bytes;
use crate::serial_println;
use crate::syscall::entry::{
    init_syscall_msrs_on_current_cpu, validate_user_context, SyscallFrame,
    DRY_BENCH_LAST_SYSCALL_ENTRY_TSC, USER_RFLAGS_ALLOWED_MASK, USER_RFLAGS_FIXED_SET,
};
use crate::uaccess::{copy_from_user, copy_to_user, enable_smap_on_current_cpu, SysError};

pub const DRY_SYSCALL_ABI_VERSION: u64 = 0x0001_0000;

pub const SYS_ABI_PING: u64 = 0;
pub const SYS_WRITE_DEBUG: u64 = 1;
pub const SYS_YIELD: u64 = 2;
pub const SYS_EXIT: u64 = 3;
pub const SYS_GET_TID: u64 = 4;
pub const SYS_CAP_QUERY: u64 = 5;
pub const SYS_EP_CREATE: u64 = 6;
pub const SYS_IPC_SEND: u64 = 7;
pub const SYS_IPC_RECV: u64 = 8;
pub const SYS_IPC_CALL: u64 = 9;
pub const SYS_IPC_REPLY: u64 = 10;
pub const SYS_IPC_REPLY_RECV: u64 = 11;
pub const SYS_NOTIFY_CREATE: u64 = 12;
pub const SYS_NOTIFY_SIGNAL: u64 = 13;
pub const SYS_NOTIFY_WAIT: u64 = 14;
pub const SYS_CAP_DERIVE: u64 = 15;
pub const SYS_CAP_REVOKE: u64 = 16;
pub const SYS_PROC_SPAWN: u64 = 17;
pub const SYS_PROC_WAIT: u64 = 18;
pub const SYS_FB_MAP: u64 = 19;

pub const MAX_WRITE_DEBUG_LEN: usize = 512;

static FORCE_SYSCALL_IRETQ: AtomicBool = AtomicBool::new(false);
/// Test hook (`Stage F.16`): `1` = non-canonical `RIP`, `2` = kernel `RSP`, `3` = forbidden `IOPL=3`.
static INJECT_CORRUPT_RETURN_MODE: AtomicU8 = AtomicU8::new(0);

/// Sanitized 32-byte capability metadata returned to CPL=3 by `SYS_CAP_QUERY` (zero PFNs or ptrs).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UserCapInfo {
    pub obj_kind: u8,
    pub domain: u8,
    pub flags: u16,
    pub rights_bits: u32,
    pub compartment_id: u16,
    pub _pad: u16,
    pub lease_epoch: u32,
    pub badge: u64,
    pub abi_reserved: u64,
}

const _: () = assert!(size_of::<UserCapInfo>() == 32);

#[inline]
pub fn set_force_syscall_iretq(force: bool) {
    FORCE_SYSCALL_IRETQ.store(force, Ordering::Release);
}

#[inline]
pub fn set_inject_corrupt_return_mode(mode: u8) {
    INJECT_CORRUPT_RETURN_MODE.store(mode, Ordering::Release);
}

/// Enable `SYSCALL` MSRs (`SCE`, `STAR`, `LSTAR`, `FMASK`) and `CR4.SMAP` on `cpu_idx`.
pub fn init_cpu_syscall_and_smap(_cpu_idx: usize) {
    init_syscall_msrs_on_current_cpu();
    let _ = enable_smap_on_current_cpu();
}

/// Synchronize `SYSCALL` MSRs and `CR4.SMAP` across BSP + all online APs (`Stage B.1 & C`).
pub fn init_all_cpus_syscall_and_smap(online_cpus: usize) {
    init_cpu_syscall_and_smap(0);
    if online_cpus > 1 {
        set_ap_epoch_kind(ApEpochKind::UserspaceSyscallInit);
        AP_PHASE_DONE_CPUS.store(0, Ordering::Release);
        AP_PHASE_EPOCH.fetch_add(1, Ordering::AcqRel);
        while AP_PHASE_DONE_CPUS.load(Ordering::Acquire) < (online_cpus - 1) {
            if crate::is_panicking() {
                crate::arch::halt_forever();
            }
            core::hint::spin_loop();
        }
    }
}

fn decode_target_obj_label(subj_label: SecLabel, spec: u64) -> Result<SecLabel, SysError> {
    if spec == 0 {
        return Ok(subj_label);
    }
    let dom = match (spec & 0xff) as u8 {
        0 => SecDomain::Userspace,
        1 => SecDomain::RootSystem,
        _ => return Err(SysError::Perm),
    };
    let comp = ((spec >> 16) & 0xffff) as u16;
    Ok(SecLabel::new(dom, SecFlags::NONE, comp, 1))
}

fn sys_write_debug_impl(user_ptr: u64, byte_len: u64, trace: bool) -> Result<isize, SysError> {
    if byte_len > (MAX_WRITE_DEBUG_LEN as u64) {
        return Err(SysError::Inval);
    }
    let len = byte_len as usize;
    let mut buf = [0u8; MAX_WRITE_DEBUG_LEN];
    copy_from_user(&mut buf[..len], user_ptr)?;
    write_sanitized_user_bytes(&buf[..len]);
    if trace {
        serial_println!("[DRY-SYSCALL] write_debug: OK");
    }
    Ok(len as isize)
}

fn sys_cap_query_impl(
    cspace_id: u16,
    subj_raw: u64,
    cap_raw: u64,
    user_out_ptr: u64,
) -> Result<isize, SysError> {
    if (user_out_ptr & 7) != 0 {
        return Err(SysError::Inval);
    }
    let subj_label = unpack_seclabel_u64(subj_raw);
    let now_epoch = current_lease_epoch();
    let ptr = CapPtr(cap_raw);

    let info = with_pinned_cap(cspace_id, subj_label, ptr, CapRights::READ, now_epoch, |res| {
        Ok(UserCapInfo {
            obj_kind: res.obj_kind as u8,
            domain: res.obj_label.domain as u8,
            flags: res.obj_label.flags.0 as u16,
            rights_bits: res.rights.0 as u32,
            compartment_id: res.obj_label.compartment_id,
            _pad: 0,
            lease_epoch: res.lease_epoch,
            badge: res.badge,
            abi_reserved: 0,
        })
    })
    .map_err(map_cap_err_to_sys)?;

    // SAFETY: `UserCapInfo` is `#[repr(C)]` POD (`32` bytes) with zero uninitialized padding (`_pad = 0`).
    let bytes = unsafe {
        core::slice::from_raw_parts((&raw const info).cast::<u8>(), size_of::<UserCapInfo>())
    };
    copy_to_user(user_out_ptr, bytes)?;
    Ok(0)
}

type ParsedSendExt = (
    usize,
    Option<(CapPtr, CapRights, u32, u64)>,
    Option<SharedPayloadDesc>,
);

fn load_user_send_ext(
    tag: MsgTag,
    ext_ptr: u64,
    extra_buf: &mut [u8; MAX_IPC_EXTRA_BYTES],
) -> Result<ParsedSendExt, SysError> {
    tag.validate()?;
    let need_ext = (tag.flags() & (MSG_FLAG_EXTRA_BUF | MSG_FLAG_GRANT_CAP | MSG_FLAG_SHARED_DESC)) != 0;
    if !need_ext {
        return Ok((0, None, None));
    }
    if ext_ptr == 0 || (ext_ptr & 7) != 0 {
        return Err(SysError::Fault);
    }
    let mut raw = [0u8; size_of::<UserIpcSendExt>()];
    copy_from_user(&mut raw, ext_ptr)?;
    // SAFETY: `UserIpcSendExt` is `#[repr(C)]` POD (`48` bytes) of integer fields.
    let ext: UserIpcSendExt = unsafe { core::ptr::read_unaligned(raw.as_ptr().cast()) };

    let elen = tag.extra_len();
    if elen > 0 {
        copy_from_user(&mut extra_buf[..elen], ext.extra_buf_ptr)?;
    }
    let grant = if (tag.flags() & MSG_FLAG_GRANT_CAP) != 0 {
        Some((
            CapPtr(ext.grant_cap),
            CapRights(ext.grant_rights),
            ext.grant_lease,
            ext.grant_badge,
        ))
    } else {
        None
    };
    let shared = if (tag.flags() & MSG_FLAG_SHARED_DESC) != 0 {
        Some(SharedPayloadDesc {
            region_cap: CapPtr(ext.shared_region_cap),
            byte_offset: (ext.shared_offset_and_len & 0xffff_ffff) as u32,
            byte_len: (ext.shared_offset_and_len >> 32) as u32,
        })
    } else {
        None
    };
    Ok((elen, grant, shared))
}

fn precheck_user_recv_ext(recv_ext_ptr: u64) -> Result<Option<UserIpcRecvExt>, SysError> {
    if recv_ext_ptr == 0 {
        return Ok(None);
    }
    if (recv_ext_ptr & 7) != 0 {
        return Err(SysError::Fault);
    }
    let mut raw = [0u8; size_of::<UserIpcRecvExt>()];
    copy_from_user(&mut raw, recv_ext_ptr)?;
    // SAFETY: `UserIpcRecvExt` is `#[repr(C)]` POD (`40` bytes).
    let ext: UserIpcRecvExt = unsafe { core::ptr::read_unaligned(raw.as_ptr().cast()) };
    // Pre-verify write access to `recv_ext_ptr` (and optional `extra_buf_ptr` if non-empty) so a
    // bogus pointer returns `-EFAULT` BEFORE consuming any queued IPC message (`Stage F.11`).
    copy_to_user(recv_ext_ptr, &raw)?;
    if ext.extra_buf_cap > 0 {
        let probe_len = (ext.extra_buf_cap as usize).min(MAX_IPC_EXTRA_BYTES);
        if probe_len > 0 {
            let mut first_byte = [0u8; 1];
            copy_from_user(&mut first_byte, ext.extra_buf_ptr)?;
            copy_to_user(ext.extra_buf_ptr, &first_byte)?;
            if probe_len > 1 {
                let last_va = ext
                    .extra_buf_ptr
                    .checked_add((probe_len - 1) as u64)
                    .ok_or(SysError::Fault)?;
                copy_from_user(&mut first_byte, last_va)?;
                copy_to_user(last_va, &first_byte)?;
            }
        }
    }
    Ok(Some(ext))
}

fn commit_user_recv_ext(
    recv_ext_ptr: u64,
    tmpl: Option<UserIpcRecvExt>,
    out: &crate::ipc::endpoint::IpcFastOut,
    extra_buf: &[u8; MAX_IPC_EXTRA_BYTES],
) -> Result<(), SysError> {
    let Some(mut ext) = tmpl else {
        return Ok(());
    };
    if out.extra_len > 0 && ext.extra_buf_cap > 0 {
        let copy_n = out
            .extra_len
            .min(ext.extra_buf_cap as usize)
            .min(MAX_IPC_EXTRA_BYTES);
        copy_to_user(ext.extra_buf_ptr, &extra_buf[..copy_n])?;
    }
    ext.sender_badge = out.sender_badge;
    ext.reply_token = out.reply_token;
    ext.transferred_cap = out.transferred_cap.0;
    // SAFETY: `UserIpcRecvExt` is `#[repr(C)]` POD (`40` bytes) with zero padding.
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (&raw const ext).cast::<u8>(),
            size_of::<UserIpcRecvExt>(),
        )
    };
    copy_to_user(recv_ext_ptr, bytes)
}

/// Rust `SYSCALL` entry invoked by `dry_syscall_entry` on the calling thread's private kernel stack
/// with `RFLAGS.IF == 0` and `RFLAGS.AC == 0` (`IA32_FMASK`).
///
/// Returns `0` for fast `sysretq` or `1` for `iretq` fallback.
extern "C" fn rust_syscall_dispatch(frame: &mut SyscallFrame) -> u64 {
    debug_assert_eq!(read_rflags() & ((1 << 9) | (1 << 18)), 0);
    let pcpu = current_percpu();
    let cpu_idx = pcpu.cpu_idx as usize;
    let cur_slot = current_slot_on_cpu(cpu_idx);
    let cur_tcb = tcb(cur_slot);

    let rsp_now = read_rsp();
    let s_base = cur_tcb.stack_base.load(Ordering::Relaxed);
    let s_top = cur_tcb.stack_top.load(Ordering::Relaxed);
    assert!(
        rsp_now > s_base && rsp_now <= s_top,
        "SYSCALL kernel stack violation: rsp={:#x} outside [{:#x}, {:#x}]",
        rsp_now,
        s_base,
        s_top
    );
    assert_eq!(frame.cs & 3, 3, "SyscallFrame CS RPL != 3");

    pcpu.syscall_count.fetch_add(1, Ordering::Relaxed);
    let pid = cur_tcb.process_id.load(Ordering::Acquire);
    let trace = crate::proc::enter_process_syscall_or_terminate(pid);

    let nr = frame.rax;
    let a0 = frame.rdi;
    let a1 = frame.rsi;
    let a2 = frame.rdx;
    let a3 = frame.r10;
    let a4 = frame.r8;
    let a5 = frame.r9;

    // Enable maskable interrupts (`STI`) now that we are safely on the owned kernel stack (`Stage B.4`).
    sti();

    let (ret0, ret1): (isize, u64) = match nr {
        SYS_ABI_PING => {
            let aux = if a0 == 1 {
                DRY_BENCH_LAST_SYSCALL_ENTRY_TSC.load(Ordering::Relaxed)
            } else {
                0
            };
            (DRY_SYSCALL_ABI_VERSION as isize, aux)
        }
        SYS_WRITE_DEBUG => match sys_write_debug_impl(a0, a1, trace) {
            Ok(n) => (n, 0),
            Err(e) => (e.as_raw(), 0),
        },
        SYS_YIELD => {
            yield_now();
            if trace {
                serial_println!("[DRY-SYSCALL] yield: OK");
            }
            (0, 0)
        }
        SYS_EXIT => {
            let exit_code = (a0 as i64) as i32;
            if trace {
                serial_println!("[DRY-USER] exit({})", exit_code);
            }
            cli();
            crate::proc::terminate_current_user_thread_exit(pid, exit_code)
        }
        SYS_GET_TID => (current_thread_id().raw() as isize, pid as u64),
        SYS_CAP_QUERY => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj_raw = cur_tcb.subj_label_raw.load(Ordering::Acquire);
            match sys_cap_query_impl(cs_id, subj_raw, a0, a1) {
                Ok(v) => (v, 0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_EP_CREATE => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            match decode_target_obj_label(subj, a0)
                .and_then(|obj_lbl| ep_create(cs_id, subj, obj_lbl, 0).map_err(map_cap_err_to_sys))
            {
                Ok((cap, _)) => (0, cap.0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_IPC_SEND => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            let tag = MsgTag(a1);
            let mrs = [frame.r12, frame.r13, frame.r14, frame.r15];
            let mut extra_buf = [0u8; MAX_IPC_EXTRA_BYTES];
            let res = load_user_send_ext(tag, a3, &mut extra_buf).and_then(|(elen, grant, sh)| {
                ep_send_or_call(
                    cs_id,
                    subj,
                    CapPtr(a0),
                    IpcSendInput {
                        tag,
                        mrs,
                        extra: &extra_buf[..elen],
                        grant,
                        shared_desc: sh,
                    },
                    false,
                    a2,
                    None,
                )
            });
            match res {
                Ok(_) => (0, 0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_IPC_RECV => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            let non_block = (a1 as u8 & MSG_FLAG_NON_BLOCKING) != 0;
            let mut extra_buf = [0u8; MAX_IPC_EXTRA_BYTES];
            let res = precheck_user_recv_ext(a3).and_then(|tmpl| {
                let extra_opt = if tmpl.is_some() {
                    Some(&mut extra_buf[..])
                } else {
                    None
                };
                let out = ep_recv(cs_id, subj, CapPtr(a0), non_block, a2, extra_opt)?;
                commit_user_recv_ext(a3, tmpl, &out, &extra_buf)?;
                Ok(out)
            });
            match res {
                Ok(out) => {
                    frame.r12 = out.mrs[0];
                    frame.r13 = out.mrs[1];
                    frame.r14 = out.mrs[2];
                    frame.r15 = out.mrs[3];
                    let aux = if out.reply_token != 0 {
                        out.reply_token
                    } else {
                        out.sender_badge
                    };
                    (out.tag.0 as isize, aux)
                }
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_IPC_CALL => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            let tag = MsgTag(a1);
            let mrs = [frame.r12, frame.r13, frame.r14, frame.r15];
            let mut s_extra = [0u8; MAX_IPC_EXTRA_BYTES];
            let mut r_extra = [0u8; MAX_IPC_EXTRA_BYTES];
            let res = precheck_user_recv_ext(a4).and_then(|tmpl| {
                let (elen, grant, sh) = load_user_send_ext(tag, a3, &mut s_extra)?;
                let r_slice = if tmpl.is_some() {
                    Some(&mut r_extra[..])
                } else {
                    None
                };
                let out = ep_send_or_call(
                    cs_id,
                    subj,
                    CapPtr(a0),
                    IpcSendInput {
                        tag,
                        mrs,
                        extra: &s_extra[..elen],
                        grant,
                        shared_desc: sh,
                    },
                    true,
                    a2,
                    r_slice,
                )?;
                commit_user_recv_ext(a4, tmpl, &out, &r_extra)?;
                Ok(out)
            });
            match res {
                Ok(out) => {
                    frame.r12 = out.mrs[0];
                    frame.r13 = out.mrs[1];
                    frame.r14 = out.mrs[2];
                    frame.r15 = out.mrs[3];
                    let aux = if out.transferred_cap.0 != 0 {
                        out.transferred_cap.0
                    } else {
                        out.sender_badge
                    };
                    (out.tag.0 as isize, aux)
                }
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_IPC_REPLY => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            let tag = MsgTag(a2);
            let mrs = [frame.r12, frame.r13, frame.r14, frame.r15];
            let mut extra_buf = [0u8; MAX_IPC_EXTRA_BYTES];
            let res = load_user_send_ext(tag, a3, &mut extra_buf).and_then(|(elen, grant, sh)| {
                ep_reply(
                    cs_id,
                    subj,
                    CapPtr(a0),
                    a1,
                    IpcSendInput {
                        tag,
                        mrs,
                        extra: &extra_buf[..elen],
                        grant,
                        shared_desc: sh,
                    },
                )
            });
            match res {
                Ok(()) => (0, 0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_IPC_REPLY_RECV => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            let reply_tag = MsgTag(a2);
            let reply_mrs = [frame.r12, frame.r13, frame.r14, frame.r15];
            let non_block = (a3 as u8 & MSG_FLAG_NON_BLOCKING) != 0;
            let mut r_extra = [0u8; MAX_IPC_EXTRA_BYTES];
            let res = reply_tag
                .validate()
                .and_then(|()| {
                    if reply_tag.extra_len() != 0 || reply_tag.flags() != 0 {
                        return Err(SysError::Inval);
                    }
                    let tmpl = precheck_user_recv_ext(a5)?;
                    let r_slice = if tmpl.is_some() {
                        Some(&mut r_extra[..])
                    } else {
                        None
                    };
                    let out = ep_reply_recv(
                        cs_id,
                        subj,
                        CapPtr(a0),
                        a1,
                        IpcSendInput {
                            tag: reply_tag,
                            mrs: reply_mrs,
                            extra: &[],
                            grant: None,
                            shared_desc: None,
                        },
                        non_block,
                        a4,
                        r_slice,
                    )?;
                    commit_user_recv_ext(a5, tmpl, &out, &r_extra)?;
                    Ok(out)
                });
            match res {
                Ok(out) => {
                    frame.r12 = out.mrs[0];
                    frame.r13 = out.mrs[1];
                    frame.r14 = out.mrs[2];
                    frame.r15 = out.mrs[3];
                    let aux = if out.reply_token != 0 {
                        out.reply_token
                    } else {
                        out.sender_badge
                    };
                    (out.tag.0 as isize, aux)
                }
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_NOTIFY_CREATE => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            match decode_target_obj_label(subj, a0)
                .and_then(|obj_lbl| notif_create(cs_id, subj, obj_lbl, 0).map_err(map_cap_err_to_sys))
            {
                Ok((cap, _)) => (0, cap.0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_NOTIFY_SIGNAL => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            match notif_signal(cs_id, subj, CapPtr(a0), a1) {
                Ok(bits) => (0, bits),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_NOTIFY_WAIT => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            let non_block = (a1 as u8 & MSG_FLAG_NON_BLOCKING) != 0;
            match notif_wait(cs_id, subj, CapPtr(a0), non_block, a2) {
                Ok(bits) => (0, bits),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_CAP_DERIVE => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            let now = current_lease_epoch();
            match cspace_derive(
                cs_id,
                subj,
                CapPtr(a0),
                CapRights((a1 & 0xffff) as u16),
                a2 as u32,
                a3,
                now,
            )
            .map_err(map_cap_err_to_sys)
            {
                Ok(cap) => (0, cap.0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_CAP_REVOKE => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            let now = current_lease_epoch();
            let res: Result<usize, CapError> = match a1 {
                0 => cspace_revoke_children(cs_id, subj, CapPtr(a0), now),
                1 => cspace_delete_cap(cs_id, subj, CapPtr(a0), now).map(|()| 1),
                2 => cspace_destroy_object(cs_id, subj, CapPtr(a0), now).map(|()| 1),
                _ => Err(CapError::InvalidHandle),
            };
            match res.map_err(map_cap_err_to_sys) {
                Ok(n) => (n as isize, 0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_PROC_SPAWN => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            match sys_proc_spawn(pid, cs_id, subj, a0, a1, a2, a3, a4, a5) {
                Ok(pair) => pair,
                Err(e) => (e.as_raw(), 0),
            }
        }
        SYS_PROC_WAIT => match sys_proc_wait(pid, a0, a1, a2) {
            Ok(pair) => pair,
            Err(e) => (e.as_raw(), 0),
        },
        SYS_FB_MAP => {
            let cs_id = cur_tcb.cspace_id.load(Ordering::Acquire);
            let subj = unpack_seclabel_u64(cur_tcb.subj_label_raw.load(Ordering::Acquire));
            let vs_id = cur_tcb.vspace_id.load(Ordering::Acquire);
            match sys_fb_map(cs_id, subj, vs_id, a0, a1, a2) {
                Ok(pages) => (pages, 0),
                Err(e) => (e.as_raw(), 0),
            }
        }
        _ => (SysError::NoSys.as_raw(), 0),
    };

    // Mask interrupts (`CLI`) for atomic `in_flight_syscalls` release, kill check, and return validation.
    cli();

    // Sanitize user `RFLAGS` before return-context check unless testing corrupt RFLAGS injection.
    frame.rflags = (frame.rflags & USER_RFLAGS_ALLOWED_MASK) | USER_RFLAGS_FIXED_SET;
    let inj = INJECT_CORRUPT_RETURN_MODE.swap(0, Ordering::AcqRel);
    match inj {
        1 => frame.rip = 0x0000_8000_0000_0000, // Non-canonical Intel SYSRETQ hazard address!
        2 => frame.rsp = 0xffff_ffff_8000_1000, // Kernel higher-half RSP!
        3 => frame.rflags |= 3 << 12,           // Forbidden IOPL=3 escalation attempt!
        _ => {}
    }

    crate::proc::leave_process_syscall_or_terminate_if_killed(pid);

    if let Err(ctx_err) =
        validate_user_context(frame.rip, frame.rsp, frame.rflags, frame.cs, frame.ss, false)
    {
        crate::proc::terminate_current_user_thread_bad_context(pid, frame, ctx_err);
    }

    frame.rax = ret0 as u64;
    frame.rdx = ret1;

    u64::from(FORCE_SYSCALL_IRETQ.load(Ordering::Relaxed))
}
