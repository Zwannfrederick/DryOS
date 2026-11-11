// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `user-peer` — Isolated Peer & Hardware Fault/Syscall Verification Process (`Milestone 1B-D`).
//!
//! Runs in a separate `VSpace` + `CSpace` from `user-hello` (`PID=1`). Driven by `mode` (`RDI`)
//! and `aux` (`RSI`) to exercise genuine Ring 3 isolation, syscall error paths, preemption stack
//! red-zones, and hardware CPU exceptions (`#PF`, `#GP`, `#UD`) from a separately compiled ELF64.

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::ptr::{read_volatile, write_volatile};

use dry_userspace::{
    raw_syscall6, read_cs_ss, sys_abi_ping, sys_cap_query, sys_cap_query_raw, sys_exit,
    sys_get_tid, sys_write_debug, sys_write_debug_raw, sys_yield, UserCapInfo, EBADCAP, EFAULT,
    EINVAL, ENOSYS,
};

#[used]
static PEER_RODATA_WORD: u64 = 0x5045_4552_524f_4454; // "PEERRODT"
static mut PEER_BSS_SCRATCH: [u64; 16] = [0; 16];

global_asm!(
    ".section .text._start, \"ax\"",
    ".global _start",
    ".balign 16",
    "_start:",
    "    and rsp, -16",
    "    xor ebp, ebp",
    "    call {entry}",
    "    ud2",
    entry = sym user_peer_main,
);

/// Verify that `SYSCALL` return preserves all callee-saved GPRs (`rbx`, `rbp`, `r12..r15`), scrubs
/// caller-saved GPRs to `0`, and clears hostile `RFLAGS.DF = 1` (`std`) via hardware `IA32_FMASK`.
#[inline(never)]
fn verify_syscall_register_hygiene() -> bool {
    let (r_rdi, r_rsi, r_r8, r_r9, r_r10): (u64, u64, u64, u64, u64);
    let (c_rbx, c_rbp, c_r12, c_r13, c_r14, c_r15): (u64, u64, u64, u64, u64, u64);
    // SAFETY: Sets known sentinels across caller/callee-saved GPRs + `RFLAGS.DF=1` (`std`) across
    // `SYS_ABI_PING`, then restores `rbx`, `rbp`, and `cld`.
    unsafe {
        asm!(
            "push rbp",
            "push rbx",
            "mov rbx, 0x1111222233334444",
            "mov rbp, 0x6666777788889999",
            "mov r12, 0x2222333344445555",
            "mov r13, 0x3333444455556666",
            "mov r14, 0x4444555566667777",
            "mov r15, 0x5555666677778888",
            "xor eax, eax",
            "mov rdi, 0xaaaa000000000001",
            "mov rsi, 0xaaaa000000000002",
            "mov rdx, 0xaaaa000000000003",
            "mov r10, 0xaaaa000000000004",
            "mov r8,  0xaaaa000000000005",
            "mov r9,  0xaaaa000000000006",
            "std",
            "syscall",
            "cld",
            "mov rax, rbx",
            "mov rdx, rbp",
            "pop rbx",
            "pop rbp",
            out("rax") c_rbx,
            out("rdx") c_rbp,
            out("r12") c_r12,
            out("r13") c_r13,
            out("r14") c_r14,
            out("r15") c_r15,
            out("rdi") r_rdi,
            out("rsi") r_rsi,
            out("r8") r_r8,
            out("r9") r_r9,
            out("r10") r_r10,
            out("rcx") _,
            out("r11") _,
        );
    }
    c_rbx == 0x1111_2222_3333_4444
        && c_rbp == 0x6666_7777_8888_9999
        && c_r12 == 0x2222_3333_4444_5555
        && c_r13 == 0x3333_4444_5555_6666
        && c_r14 == 0x4444_5555_6666_7777
        && c_r15 == 0x5555_6666_7777_8888
        && r_rdi == 0
        && r_rsi == 0
        && r_r8 == 0
        && r_r9 == 0
        && r_r10 == 0
}

/// Negative assembly transition test: invoke `SYS_CAP_QUERY` (`copy_to_user` `rep movsb`) with
/// hostile `RFLAGS.DF = 1` (`std`) to prove `IA32_FMASK` clears `DF` before Ring 0 string copies.
#[inline(never)]
fn verify_syscall_hostile_df_uaccess(own_cap: u64) -> bool {
    let mut info = UserCapInfo::default();
    let out_ptr = (&raw mut info) as u64;
    let rc: isize;
    // SAFETY: `info` is valid writable 32-byte user stack memory; `cld` restored immediately after.
    unsafe {
        asm!(
            "std",
            "syscall",
            "cld",
            inlateout("rax") 5u64 => rc,
            in("rdi") own_cap,
            in("rsi") out_ptr,
            lateout("rdi") _,
            lateout("rsi") _,
            lateout("rdx") _,
            lateout("rcx") _,
            lateout("r8") _,
            lateout("r9") _,
            lateout("r10") _,
            lateout("r11") _,
        );
    }
    rc == 0 && info.obj_kind != 0 && info.rights_bits != 0
}

fn run_syscall_negative_suite(own_cap: u64, packed_addrs: u64) -> ! {
    if !verify_syscall_register_hygiene() {
        sys_exit(10);
    }
    // 1. Invalid syscall number -> ENOSYS (-38).
    if raw_syscall6(0xdead_beef, 1, 2, 3, 4, 5, 6).0 != ENOSYS {
        sys_exit(11);
    }
    // 2. Invalid user pointers (`NULL`, unmapped canonical, kernel higher-half, non-canonical).
    if sys_write_debug_raw(0, 8) != EFAULT {
        sys_exit(12);
    }
    if sys_write_debug_raw(0x0000_0000_0070_0000, 8) != EFAULT {
        sys_exit(13);
    }
    if sys_write_debug_raw(0xffff_ffff_8000_0000, 8) != EFAULT {
        sys_exit(14);
    }
    if sys_write_debug_raw(0x0001_0000_0000_0000, 8) != EFAULT {
        sys_exit(15);
    }
    // Supervisor-only lower-half page (`U/S = 0`) if supplied in low 48 bits of `packed_addrs`.
    let sup_lower_vaddr = packed_addrs & 0x0000_ffff_ffff_ffff;
    if sup_lower_vaddr != 0 && sys_write_debug_raw(sup_lower_vaddr, 8) != EFAULT {
        sys_exit(16);
    }
    // `copy_to_user` targeting read-only `.rodata` page (`CR0.WP + extable` fixup -> EFAULT).
    let ro_ptr = (&raw const PEER_RODATA_WORD) as u64;
    if own_cap != 0 && sys_cap_query_raw(own_cap, ro_ptr) != EFAULT {
        sys_exit(17);
    }
    // 3. User buffer boundary overflow:
    //    (a) Length > MAX_WRITE_DEBUG_LEN (512) -> EINVAL (-22).
    if sys_write_debug_raw(ro_ptr, 4096) != EINVAL {
        sys_exit(18);
    }
    //    (b) `u64::MAX` wraparound overflow -> EFAULT (-14).
    if sys_write_debug_raw(0x0000_7fff_ffff_fff8, 32) != EFAULT {
        sys_exit(19);
    }
    //    (c) Buffer starting 8 bytes before top of user stack and crossing into unmapped guard page!
    let cur_rsp: u64;
    // SAFETY: Pure read of `RSP`.
    unsafe {
        asm!("mov {}, rsp", out(reg) cur_rsp, options(nomem, nostack, preserves_flags));
    }
    let stack_top_page_end = (cur_rsp + 0xfff) & !0xfff;
    if sys_write_debug_raw(stack_top_page_end - 8, 32) != EFAULT {
        sys_exit(20);
    }
    // 4. Forged / NULL / wrong-guard capability handles via `SYS_CAP_QUERY` -> EBADCAP (-9).
    let mut info = UserCapInfo::default();
    if sys_cap_query(0, &mut info) != EBADCAP {
        sys_exit(21);
    }
    if sys_cap_query(0xfe08_0000_0001_0005, &mut info) != EBADCAP {
        sys_exit(22);
    }
    if sys_cap_query(0x0008_dead_beef_00ff, &mut info) != EBADCAP {
        sys_exit(23);
    }
    if own_cap != 0 && (sys_cap_query(own_cap, &mut info) != 0 || !verify_syscall_hostile_df_uaccess(own_cap)) {
        sys_exit(24);
    }
    sys_exit(0x5afe)
}

/// Spin in CPL=3 across timer IRQ preemptions while guarding 128 bytes below `RSP` (red-zone
/// sentinel) + callee-saved GPRs to prove interrupts NEVER touch the user stack (`TSS.RSP0` used).
fn run_preemption_stack_isolation_loop(spin_iters: u64) -> ! {
    let ok: u64;
    let iters = spin_iters.max(200_000);
    // SAFETY: Writes deterministic sentinels into `[rsp - 128 .. rsp)` without moving `rsp`,
    // spins for `iters` iterations while timer IRQs preempt CPL=3, then verifies every qword.
    unsafe {
        asm!(
            "push rbx",
            "mov rbx, 0xa5a5a5a55a5a5a5a",
            "mov r12, 0x123456789abcdef0",
            "mov r13, 0x0fedcba987654321",
            "mov rdx, 0xfeedfacecafebe01",
            "mov qword ptr [rsp - 8],   rdx",
            "mov rdx, 0xfeedfacecafebe02",
            "mov qword ptr [rsp - 16],  rdx",
            "mov rdx, 0xfeedfacecafebe03",
            "mov qword ptr [rsp - 32],  rdx",
            "mov rdx, 0xfeedfacecafebe04",
            "mov qword ptr [rsp - 64],  rdx",
            "mov rdx, 0xfeedfacecafebe05",
            "mov qword ptr [rsp - 96],  rdx",
            "mov rdx, 0xfeedfacecafebe06",
            "mov qword ptr [rsp - 128], rdx",
            "mov rcx, {iters}",
            "2:",
            "pause",
            "sub rcx, 1",
            "jnz 2b",
            "mov eax, 1",
            "mov rdx, 0xa5a5a5a55a5a5a5a",
            "cmp rbx, rdx",
            "jne 3f",
            "mov rdx, 0x123456789abcdef0",
            "cmp r12, rdx",
            "jne 3f",
            "mov rdx, 0x0fedcba987654321",
            "cmp r13, rdx",
            "jne 3f",
            "mov rdx, 0xfeedfacecafebe01",
            "cmp qword ptr [rsp - 8], rdx",
            "jne 3f",
            "mov rdx, 0xfeedfacecafebe02",
            "cmp qword ptr [rsp - 16], rdx",
            "jne 3f",
            "mov rdx, 0xfeedfacecafebe03",
            "cmp qword ptr [rsp - 32], rdx",
            "jne 3f",
            "mov rdx, 0xfeedfacecafebe04",
            "cmp qword ptr [rsp - 64], rdx",
            "jne 3f",
            "mov rdx, 0xfeedfacecafebe05",
            "cmp qword ptr [rsp - 96], rdx",
            "jne 3f",
            "mov rdx, 0xfeedfacecafebe06",
            "cmp qword ptr [rsp - 128], rdx",
            "jne 3f",
            "jmp 4f",
            "3:",
            "xor eax, eax",
            "4:",
            "pop rbx",
            iters = in(reg) iters,
            out("rax") ok,
            out("rcx") _,
            out("rdx") _,
            out("r12") _,
            out("r13") _,
        );
    }
    if ok == 1 {
        sys_exit(0x7777)
    } else {
        sys_exit(77)
    }
}

extern "C" fn user_peer_main(mode: u64, aux: u64) -> ! {
    let (cs, ss) = read_cs_ss();
    if (cs & 3) != 3 || (ss & 3) != 3 {
        sys_exit(80);
    }

    match mode {
        // Mode 0: Normal isolated peer in separate VSpace/CSpace.
        0 => {
            // SAFETY: Private `.bss` of this process (`0x403000` in its own VSpace).
            unsafe {
                let p = (&raw mut PEER_BSS_SCRATCH) as *mut u64;
                if read_volatile(p) != 0 {
                    sys_exit(81);
                }
                write_volatile(p, 0xcafe_d00d_0000_0002 ^ aux);
                for _ in 0..4 {
                    let _ = sys_yield();
                }
                if read_volatile(p) != (0xcafe_d00d_0000_0002 ^ aux) {
                    sys_exit(82);
                }
            }
            let _ = sys_get_tid();
            sys_exit(42)
        }
        // Mode 1: CPL=3 read from kernel memory (`#PF`, err=0x5: P|U).
        1 => unsafe {
            let _ = read_volatile(aux as *const u64);
            sys_exit(1)
        },
        // Mode 2: CPL=3 write to kernel memory (`#PF`, err=0x7: P|W|U).
        2 => unsafe {
            write_volatile(aux as *mut u64, 0xbad0_c0de);
            sys_exit(2)
        },
        // Mode 3: CPL=3 privileged instruction `cli` (`#GP(0)`, vec=13).
        3 => unsafe {
            asm!("cli", options(nomem, nostack));
            sys_exit(3)
        },
        // Mode 4: CPL=3 write to `USER_RO` `.rodata` page (`#PF`, err=0x7: P|W|U).
        4 => unsafe {
            let p = (&raw const PEER_RODATA_WORD) as *mut u64;
            write_volatile(p, 0xdead_beef);
            sys_exit(4)
        },
        // Mode 5: CPL=3 instruction fetch from `NX` `.bss` page (`#PF`, err=0x15: P|U|I).
        5 => unsafe {
            let p = (&raw mut PEER_BSS_SCRATCH) as *mut u8;
            write_volatile(p, 0xc3); // `ret`
            let f: extern "C" fn() = core::mem::transmute(p);
            f();
            sys_exit(5)
        },
        // Mode 6: Syscall negative validation suite (`aux` = sup_lower_vaddr, `own_cap` = 0 or passed via high bits).
        6 => run_syscall_negative_suite(0, aux),
        // Mode 61: Syscall negative suite with a valid read-only cap in `aux`.
        61 => run_syscall_negative_suite(aux, 0),
        // Mode 7: Cross-process private memory probe (`aux` = victim's private VAddr `0x0050_0000`).
        7 => unsafe {
            let _ = read_volatile(aux as *const u64);
            sys_exit(7)
        },
        // Mode 8: Continuous in-flight `SYS_YIELD` loop for concurrent cross-CPU `kill_process` race.
        8 => loop {
            let _ = sys_yield();
        },
        // Mode 9: Preemption user/kernel stack isolation & GPR/red-zone sentinel check.
        9 => run_preemption_stack_isolation_loop(aux),
        // Mode 10: CPL=3 `#UD` (`ud2`) fault while sibling process continues unimpaired.
        10 => unsafe {
            asm!("ud2", options(nomem, nostack));
            sys_exit(10)
        },
        // Mode 11: Immediate clean exit for `spawn + run + exit + reclaim` cycle benchmark.
        11 => sys_exit(0),
        // Mode 12: SMP concurrent syscall contention worker (`aux` = iterations).
        12 => {
            let n = aux.max(64);
            for _ in 0..n {
                let (v, _) = sys_abi_ping();
                if v < 0 {
                    sys_exit(88);
                }
                let _ = sys_get_tid();
            }
            let _ = sys_write_debug(b"");
            sys_exit(0)
        }
        // Mode 13: Corrupt own RSP to non-canonical/kernel value right before `syscall` to test return/entry safety.
        13 => unsafe {
            asm!(
                "mov rsp, {bad_rsp}",
                "mov eax, 0",
                "syscall",
                "ud2",
                bad_rsp = in(reg) aux,
                options(noreturn),
            );
        },
        _ => sys_exit(127),
    }
}
