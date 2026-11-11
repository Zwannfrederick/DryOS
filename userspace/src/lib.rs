// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Minimal `no_std` DRY Native Syscall ABI (`v0.1`) bindings for Ring 3 userspace binaries.

#![no_std]

use core::arch::asm;
use core::panic::PanicInfo;

pub const DRY_SYSCALL_ABI_VERSION: u64 = 0x0001_0000;

pub const SYS_ABI_PING: u64 = 0;
pub const SYS_WRITE_DEBUG: u64 = 1;
pub const SYS_YIELD: u64 = 2;
pub const SYS_EXIT: u64 = 3;
pub const SYS_GET_TID: u64 = 4;
pub const SYS_CAP_QUERY: u64 = 5;

pub const EPERM: isize = -1;
pub const EBADCAP: isize = -9;
pub const EFAULT: isize = -14;
pub const EBUSY: isize = -16;
pub const EINVAL: isize = -22;
pub const ENOSYS: isize = -38;
pub const EREVOKED: isize = -110;

/// Sanitized capability metadata returned by `SYS_CAP_QUERY` (zero kernel pointers or PFNs).
#[repr(C)]
#[derive(Clone, Copy, Default)]
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

#[inline(always)]
pub fn raw_syscall6(
    nr: u64,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
) -> (isize, u64) {
    let ret0: isize;
    let ret1: u64;
    // SAFETY: Executes Ring 3 `syscall` according to DRY Native Syscall ABI v0.1 (`ADR-002`).
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr as isize => ret0,
            inlateout("rdi") a0 => _,
            inlateout("rsi") a1 => _,
            inlateout("rdx") a2 => ret1,
            inlateout("r10") a3 => _,
            inlateout("r8") a4 => _,
            inlateout("r9") a5 => _,
            out("rcx") _,
            out("r11") _,
            options(nostack),
        );
    }
    (ret0, ret1)
}

#[inline]
pub fn sys_abi_ping() -> (isize, u64) {
    raw_syscall6(SYS_ABI_PING, 0, 0, 0, 0, 0, 0)
}

#[inline]
pub fn sys_write_debug(bytes: &[u8]) -> isize {
    raw_syscall6(SYS_WRITE_DEBUG, bytes.as_ptr() as u64, bytes.len() as u64, 0, 0, 0, 0).0
}

#[inline]
pub fn sys_write_debug_raw(ptr: u64, len: u64) -> isize {
    raw_syscall6(SYS_WRITE_DEBUG, ptr, len, 0, 0, 0, 0).0
}

#[inline]
pub fn sys_yield() -> isize {
    raw_syscall6(SYS_YIELD, 0, 0, 0, 0, 0, 0).0
}

#[inline]
pub fn sys_exit(code: i32) -> ! {
    let _ = raw_syscall6(SYS_EXIT, code as u64, 0, 0, 0, 0, 0);
    loop {
        // SAFETY: Unreachable fallback if exit ever failed; `ud2` traps into Ring 0 fault reaper.
        unsafe {
            asm!("ud2", options(nomem, nostack));
        }
    }
}

#[inline]
pub fn sys_get_tid() -> (u64, u64) {
    let (r0, r1) = raw_syscall6(SYS_GET_TID, 0, 0, 0, 0, 0, 0);
    (r0 as u64, r1)
}

#[inline]
pub fn sys_cap_query(cap_raw: u64, out: &mut UserCapInfo) -> isize {
    raw_syscall6(
        SYS_CAP_QUERY,
        cap_raw,
        (out as *mut UserCapInfo) as u64,
        0,
        0,
        0,
        0,
    )
    .0
}

#[inline]
pub fn sys_cap_query_raw(cap_raw: u64, out_ptr: u64) -> isize {
    raw_syscall6(SYS_CAP_QUERY, cap_raw, out_ptr, 0, 0, 0, 0).0
}

/// Read live architectural `CS` and `SS` segment selectors at CPL=3 to prove hardware Ring 3.
#[inline]
pub fn read_cs_ss() -> (u16, u16) {
    let cs: u16;
    let ss: u16;
    // SAFETY: Reading segment selectors into GPRs is unprivileged in 64-bit mode.
    unsafe {
        asm!(
            "mov {cs:x}, cs",
            "mov {ss:x}, ss",
            cs = out(reg) cs,
            ss = out(reg) ss,
            options(nomem, nostack, preserves_flags),
        );
    }
    (cs, ss)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys_exit(101)
}
