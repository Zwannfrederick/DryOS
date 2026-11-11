// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! DRY Native Syscall ABI (`v0.1`) Dispatcher & Capability/Domain Enforcement (`Milestone 1B-D` Stage B).
//!
//! # Versioned ABI Contract (`DRY_SYSCALL_ABI_VERSION = 0x0001_0000`)
//! - **Entry:** `syscall` at `CPL = 3` (`CS = 0x23`, `SS = 0x1b`).
//! - **Arguments (`ADR-002`):** `rax = nr`, `rdi = a0`, `rsi = a1`, `rdx = a2`, `r10 = a3`, `r8 = a4`, `r9 = a5`.
//! - **Returns:** `rax = ret0` (`>= 0` on success, `-errno` in `[-4095..-1]` on error), `rdx = ret1`.
//! - **Preserved:** `rbx`, `rbp`, `rsp`, `r12`, `r13`, `r14`, `r15`.
//! - **Clobbered / Scrubbed:** `rcx` (`RIP`), `r11` (`RFLAGS`), and `rdi`, `rsi`, `r8`, `r9`, `r10`
//!   (explicitly zeroed before leaving Ring 0 so zero kernel pointers leak to userspace).

pub mod entry;

use core::mem::size_of;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::arch::{cli, read_rflags, read_rsp, sti};
use crate::cspace::cap::{CapError, CapPtr};
use crate::cspace::with_pinned_cap;
use crate::percpu::current_percpu;
use crate::sched::{
    current_slot_on_cpu, current_thread_id, set_ap_epoch_kind, tcb, yield_now, ApEpochKind,
    AP_PHASE_DONE_CPUS, AP_PHASE_EPOCH,
};
use crate::security::{current_lease_epoch, unpack_seclabel_u64, CapRights};
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

#[inline]
fn map_cap_error(err: CapError) -> SysError {
    match err {
        CapError::InvalidHandle | CapError::StaleGenerationAba => SysError::BadCap,
        CapError::Revoked | CapError::LeaseExpired => SysError::Revoked,
        CapError::BusyActiveObject => SysError::Busy,
        CapError::InsufficientRights
        | CapError::RightsAmplification
        | CapError::DomainViolation
        | CapError::TransferEscalation
        | CapError::WrongObjectKind => SysError::Perm,
        CapError::OutOfSlots | CapError::PagingError => SysError::Inval,
    }
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
    .map_err(map_cap_error)?;

    // SAFETY: `UserCapInfo` is `#[repr(C)]` POD (`32` bytes) with zero uninitialized padding (`_pad = 0`).
    let bytes = unsafe {
        core::slice::from_raw_parts((&raw const info).cast::<u8>(), size_of::<UserCapInfo>())
    };
    copy_to_user(user_out_ptr, bytes)?;
    Ok(0)
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
