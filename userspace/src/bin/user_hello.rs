// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `user-hello` — First Native Ring 3 (`CPL=3`) Userspace Process for DRY OS (`Milestone 1B-D`).

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::ptr::{read_volatile, write_volatile};

use dry_userspace::{
    read_cs_ss, sys_abi_ping, sys_cap_query, sys_exit, sys_get_tid, sys_write_debug, sys_yield,
    UserCapInfo, DRY_SYSCALL_ABI_VERSION,
};

#[used]
static RODATA_BANNER_MAGIC: u64 = 0x4452_595f_5553_5233; // "DRY_USR3"
static mut BSS_COUNTER: [u64; 4] = [0; 4];

global_asm!(
    ".section .text._start, \"ax\"",
    ".global _start",
    ".balign 16",
    "_start:",
    "    and rsp, -16",
    "    xor ebp, ebp",
    "    call {entry}",
    "    ud2",
    entry = sym user_hello_main,
);

extern "C" fn user_hello_main(own_cap_raw: u64, rounds: u64) -> ! {
    // 1. Hardware privilege check directly from CPL=3 (`CS == 0x23`, `SS == 0x1b`, `RPL == 3`).
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || cs != 0x23 || (ss & 3) != 3 || ss != 0x1b {
        sys_exit(90);
    }

    // 2. Verify `.rodata` (`USER_RO`) and zero-filled `.bss` (`USER_RW`) ELF64 segment mappings.
    // SAFETY: Single-threaded process accessing its own static `.rodata` and `.bss`.
    unsafe {
        if read_volatile(&raw const RODATA_BANNER_MAGIC) != 0x4452_595f_5553_5233 {
            sys_exit(91);
        }
        let bss_ptr = (&raw mut BSS_COUNTER) as *mut u64;
        if read_volatile(bss_ptr) != 0 || read_volatile(bss_ptr.add(3)) != 0 {
            sys_exit(92);
        }
        write_volatile(bss_ptr, 0x1234_5678_9abc_def0);
        if read_volatile(bss_ptr) != 0x1234_5678_9abc_def0 {
            sys_exit(93);
        }
    }

    // 3. Verify DRY Native Syscall ABI v0.1 version ping, `get_tid`, and optional `cap_query`.
    let (ver, zero_aux) = sys_abi_ping();
    if (ver as u64) != DRY_SYSCALL_ABI_VERSION || zero_aux != 0 {
        sys_exit(94);
    }
    let (tid, pid) = sys_get_tid();
    if tid == 0 || pid == 0 {
        sys_exit(95);
    }
    if own_cap_raw != 0 {
        let mut info = UserCapInfo::default();
        if sys_cap_query(own_cap_raw, &mut info) != 0 || info.rights_bits == 0 {
            sys_exit(96);
        }
    }

    // 4. Emit target greeting via bounded `SYS_WRITE_DEBUG` (`copy_from_user`) and voluntary `SYS_YIELD`.
    let msg = b"Hello from DRY OS!\n";
    if sys_write_debug(msg) != (msg.len() as isize) {
        sys_exit(97);
    }

    let yield_iters = if rounds == 0 { 1 } else { rounds.min(16) };
    for _ in 0..yield_iters {
        if sys_yield() != 0 {
            sys_exit(98);
        }
    }

    sys_exit(0)
}
