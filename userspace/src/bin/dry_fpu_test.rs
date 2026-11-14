// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `dry-fpu-test` — Ring 3 FPU/SIMD context-isolation probe (`Milestone 2A-4a` Contract B).
//!
//! `_start(seed, rounds)`:
//! 1. First FPU use must observe a clean state (all `XMM`/`YMM` zero, `MXCSR = 0x1F80`,
//!    `FCW = 0x037F`): proves no register leak from any earlier/other process (`exit 10..12`).
//! 2. Each round loads a seed-unique pattern into `XMM0..15` (`YMM0..15` when the kernel enabled
//!    AVX), a private `MXCSR` rounding mode and one x87 stack value, yields and burns CPU so it is
//!    preempted/migrated, then verifies every byte (`exit 20..23`).
//!
//! The crate is built with `-sse`, so the compiler never touches vector registers behind our back.

#![no_std]
#![no_main]

use core::arch::x86_64::__cpuid;
use core::arch::{asm, global_asm};
use core::hint::black_box;

use dry_userspace::{sys_exit, sys_yield};

const SPIN_ITERS: u64 = 60_000;

global_asm!(
    ".section .text._start, \"ax\"",
    ".global _start",
    ".balign 16",
    "_start:",
    "    and rsp, -16",
    "    xor ebp, ebp",
    "    call {entry}",
    "    ud2",
    entry = sym fpu_test_main,
);

#[repr(C, align(64))]
struct VecState {
    regs: [[u8; 32]; 16],
    mxcsr: u32,
    fcw: u16,
}

impl VecState {
    const fn zeroed() -> Self {
        Self { regs: [[0; 32]; 16], mxcsr: 0, fcw: 0 }
    }
}

fn avx_enabled() -> bool {
    let ecx = __cpuid(1).ecx;
    if (ecx & (1 << 27)) == 0 || (ecx & (1 << 28)) == 0 {
        return false;
    }
    let (lo, _hi): (u32, u32);
    // SAFETY: `CPUID.1:ECX.OSXSAVE = 1` guarantees `xgetbv` is executable at CPL=3.
    unsafe {
        asm!("xgetbv", in("ecx") 0u32, out("eax") lo, out("edx") _hi, options(nomem, nostack));
    }
    (lo & 0x6) == 0x6
}

#[inline(never)]
fn read_state(st: &mut VecState, avx: bool) {
    let p = st.regs.as_mut_ptr().cast::<u8>();
    // SAFETY: `p` addresses 16 * 32 writable bytes followed by `mxcsr` (+512) and `fcw` (+516).
    unsafe {
        if avx {
            asm!(
            "vmovdqu ymmword ptr [{0} + 0], ymm0",
            "vmovdqu ymmword ptr [{0} + 32], ymm1",
            "vmovdqu ymmword ptr [{0} + 64], ymm2",
            "vmovdqu ymmword ptr [{0} + 96], ymm3",
            "vmovdqu ymmword ptr [{0} + 128], ymm4",
            "vmovdqu ymmword ptr [{0} + 160], ymm5",
            "vmovdqu ymmword ptr [{0} + 192], ymm6",
            "vmovdqu ymmword ptr [{0} + 224], ymm7",
            "vmovdqu ymmword ptr [{0} + 256], ymm8",
            "vmovdqu ymmword ptr [{0} + 288], ymm9",
            "vmovdqu ymmword ptr [{0} + 320], ymm10",
            "vmovdqu ymmword ptr [{0} + 352], ymm11",
            "vmovdqu ymmword ptr [{0} + 384], ymm12",
            "vmovdqu ymmword ptr [{0} + 416], ymm13",
            "vmovdqu ymmword ptr [{0} + 448], ymm14",
            "vmovdqu ymmword ptr [{0} + 480], ymm15",
                in(reg) p, options(nostack, preserves_flags)
            );
        } else {
            asm!(
            "movdqu xmmword ptr [{0} + 0], xmm0",
            "movdqu xmmword ptr [{0} + 32], xmm1",
            "movdqu xmmword ptr [{0} + 64], xmm2",
            "movdqu xmmword ptr [{0} + 96], xmm3",
            "movdqu xmmword ptr [{0} + 128], xmm4",
            "movdqu xmmword ptr [{0} + 160], xmm5",
            "movdqu xmmword ptr [{0} + 192], xmm6",
            "movdqu xmmword ptr [{0} + 224], xmm7",
            "movdqu xmmword ptr [{0} + 256], xmm8",
            "movdqu xmmword ptr [{0} + 288], xmm9",
            "movdqu xmmword ptr [{0} + 320], xmm10",
            "movdqu xmmword ptr [{0} + 352], xmm11",
            "movdqu xmmword ptr [{0} + 384], xmm12",
            "movdqu xmmword ptr [{0} + 416], xmm13",
            "movdqu xmmword ptr [{0} + 448], xmm14",
            "movdqu xmmword ptr [{0} + 480], xmm15",
                in(reg) p, options(nostack, preserves_flags)
            );
        }
        asm!("stmxcsr [{0} + 512]", "fnstcw [{0} + 516]", in(reg) p, options(nostack, preserves_flags));
    }
}

#[inline(never)]
fn write_state(st: &VecState, avx: bool) {
    let p = st.regs.as_ptr().cast::<u8>();
    // SAFETY: `p` addresses 16 * 32 readable bytes; all 16 vector registers are deliberately
    // overwritten (the crate is compiled without SSE, so Rust code holds no live values there).
    unsafe {
        if avx {
            asm!(
            "vmovdqu ymm0, ymmword ptr [{0} + 0]",
            "vmovdqu ymm1, ymmword ptr [{0} + 32]",
            "vmovdqu ymm2, ymmword ptr [{0} + 64]",
            "vmovdqu ymm3, ymmword ptr [{0} + 96]",
            "vmovdqu ymm4, ymmword ptr [{0} + 128]",
            "vmovdqu ymm5, ymmword ptr [{0} + 160]",
            "vmovdqu ymm6, ymmword ptr [{0} + 192]",
            "vmovdqu ymm7, ymmword ptr [{0} + 224]",
            "vmovdqu ymm8, ymmword ptr [{0} + 256]",
            "vmovdqu ymm9, ymmword ptr [{0} + 288]",
            "vmovdqu ymm10, ymmword ptr [{0} + 320]",
            "vmovdqu ymm11, ymmword ptr [{0} + 352]",
            "vmovdqu ymm12, ymmword ptr [{0} + 384]",
            "vmovdqu ymm13, ymmword ptr [{0} + 416]",
            "vmovdqu ymm14, ymmword ptr [{0} + 448]",
            "vmovdqu ymm15, ymmword ptr [{0} + 480]",
                in(reg) p, options(nostack, preserves_flags, readonly)
            );
        } else {
            asm!(
            "movdqu xmm0, xmmword ptr [{0} + 0]",
            "movdqu xmm1, xmmword ptr [{0} + 32]",
            "movdqu xmm2, xmmword ptr [{0} + 64]",
            "movdqu xmm3, xmmword ptr [{0} + 96]",
            "movdqu xmm4, xmmword ptr [{0} + 128]",
            "movdqu xmm5, xmmword ptr [{0} + 160]",
            "movdqu xmm6, xmmword ptr [{0} + 192]",
            "movdqu xmm7, xmmword ptr [{0} + 224]",
            "movdqu xmm8, xmmword ptr [{0} + 256]",
            "movdqu xmm9, xmmword ptr [{0} + 288]",
            "movdqu xmm10, xmmword ptr [{0} + 320]",
            "movdqu xmm11, xmmword ptr [{0} + 352]",
            "movdqu xmm12, xmmword ptr [{0} + 384]",
            "movdqu xmm13, xmmword ptr [{0} + 416]",
            "movdqu xmm14, xmmword ptr [{0} + 448]",
            "movdqu xmm15, xmmword ptr [{0} + 480]",
                in(reg) p, options(nostack, preserves_flags, readonly)
            );
        }
    }
}

fn fill_pattern(st: &mut VecState, seed: u64, round: u64) {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ round.wrapping_mul(0xff51_afd7_ed55_8ccd);
    for reg in st.regs.iter_mut() {
        for chunk in reg.as_chunks_mut::<8>().0 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            chunk.copy_from_slice(&x.to_le_bytes());
        }
    }
}

extern "C" fn fpu_test_main(seed: u64, rounds: u64) -> ! {
    let avx = avx_enabled();
    let width = if avx { 32 } else { 16 };
    let mut want = VecState::zeroed();
    let mut got = VecState::zeroed();

    read_state(&mut got, avx);
    if got.regs.iter().any(|r| r[..width].iter().any(|&b| b != 0)) {
        sys_exit(10);
    }
    if got.mxcsr != 0x1f80 {
        sys_exit(11);
    }
    if got.fcw != 0x037f {
        sys_exit(12);
    }

    let my_mxcsr: u32 = 0x1f80 | (((seed & 3) as u32) << 13);
    // SAFETY: Loads a valid `MXCSR` (all exceptions masked, seed-selected rounding mode).
    unsafe {
        asm!("ldmxcsr [{0}]", in(reg) &raw const my_mxcsr, options(nostack, preserves_flags, readonly));
    }

    for round in 0..rounds {
        fill_pattern(&mut want, seed, round);
        write_state(&want, avx);
        let x87_in: i64 = (seed << 32 | round) as i64;
        // SAFETY: Pushes one value on the empty x87 stack; popped again below.
        unsafe {
            asm!("fild qword ptr [{0}]", in(reg) &raw const x87_in, options(nostack, preserves_flags, readonly));
        }

        let _ = sys_yield();
        let mut acc = round;
        for i in 0..SPIN_ITERS {
            acc = black_box(acc.wrapping_mul(6364136223846793005).wrapping_add(i));
        }
        let _ = black_box(acc);
        let _ = sys_yield();

        let mut x87_out: i64 = 0;
        // SAFETY: Pops the value pushed above into `x87_out`.
        unsafe {
            asm!("fistp qword ptr [{0}]", in(reg) &raw mut x87_out, options(nostack, preserves_flags));
        }
        read_state(&mut got, avx);
        if got.regs.iter().zip(want.regs.iter()).any(|(g, w)| g[..width] != w[..width]) {
            sys_exit(20);
        }
        if got.mxcsr != my_mxcsr {
            sys_exit(21);
        }
        if x87_out != x87_in {
            sys_exit(22);
        }
        if got.fcw != 0x037f {
            sys_exit(23);
        }
    }
    sys_exit(0)
}
