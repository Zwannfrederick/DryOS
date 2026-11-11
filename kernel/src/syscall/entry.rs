// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Low-Level `x86-64` `SYSCALL` / `SYSRETQ` / `IRETQ` Assembly Entry & Initial CPL=3 Transition
//! (`Milestone 1B-D` Stages A.3–A.5 & B.1–B.5).

use core::arch::{asm, global_asm};
use core::mem::size_of;
use core::sync::atomic::{AtomicU64, AtomicU8};

use crate::arch::{
    rdmsr, wrmsr, MSR_IA32_EFER, MSR_IA32_FMASK, MSR_IA32_LSTAR, MSR_IA32_STAR,
};
use crate::gdt_tss::{KERNEL_CS, USER_CS, USER_DS};
use crate::percpu::{PCPU_OFF_KERNEL_RSP0, PCPU_OFF_USER_RSP_SCRATCH};
use crate::vspace::pte::{is_canonical_48, USER_VADDR_LIMIT, USER_VADDR_MIN};

const EFER_SCE: u64 = 1 << 0;

/// `IA32_STAR`:
/// - `[47:32] = 0x0008` -> `SYSCALL` loads `CS = 0x08` (`KERNEL_CS`), `SS = 0x10` (`KERNEL_DS`).
/// - `[63:48] = 0x0010` -> `SYSRETQ` loads `SS = (0x10 + 8) | 3 = 0x1b` (`USER_DS`),
///   `CS = (0x10 + 16) | 3 = 0x23` (`USER_CS`).
pub const STAR_SYSCALL_SYSRET: u64 = ((0x0010u64) << 48) | ((KERNEL_CS as u64) << 32);

/// `IA32_FMASK`: Atomically clears `TF(8) | IF(9) | DF(10) | IOPL(12..13) | NT(14) | AC(18)` on `SYSCALL`.
pub const FMASK_SYSCALL_CLEAR: u64 =
    (1 << 8) | (1 << 9) | (1 << 10) | (3 << 12) | (1 << 14) | (1 << 18);

/// Allowed user `RFLAGS` arithmetic/status mask; `IF(9)` + reserved `bit 1` are forced on.
pub const USER_RFLAGS_ALLOWED_MASK: u64 = 0x0020_0fd5;
pub const USER_RFLAGS_FIXED_SET: u64 = 0x0000_0202;

/// Optional benchmark timestamp latched immediately after `SYSCALL` `swapgs` + kernel stack switch.
#[no_mangle]
pub static DRY_BENCH_STAMP_ENTRY: AtomicU8 = AtomicU8::new(0);
#[no_mangle]
pub static DRY_BENCH_LAST_SYSCALL_ENTRY_TSC: AtomicU64 = AtomicU64::new(0);

/// 144-byte (`18 * 8`, 16-byte aligned) Ring 3 `SYSCALL` frame pushed onto the thread's kernel stack.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SyscallFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub r9: u64,
    pub r8: u64,
    pub r10: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rax: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

const _: () = assert!(size_of::<SyscallFrame>() == 18 * 8);
const _: () = assert!(PCPU_OFF_KERNEL_RSP0 == 192);
const _: () = assert!(PCPU_OFF_USER_RSP_SCRATCH == 200);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserContextError {
    BadSelector,
    NonCanonicalOrKernelRip,
    NonCanonicalOrKernelRsp,
    UnalignedRsp,
    ForbiddenRflags,
}

// Hardware `SYSCALL` / `SYSRETQ` / `IRETQ` Entry Contract:
// - `IA32_FMASK` (`FMASK_SYSCALL_CLEAR`) atomically clears `TF(8) | IF(9) | DF(10) | IOPL(12..13)
//   | NT(14) | AC(18)` inside `SYSCALL` microcode (verified via readback in `init_syscall_msrs_on_current_cpu`),
//   so `RFLAGS.IF == 0`, `RFLAGS.DF == 0` (no software `cld` needed), and `RFLAGS.AC == 0` (`SMAP` armed).
// - `swapgs` runs with `IF = 0` and `#DF` bound exclusively to `IST1`; user `RSP` is staged in
//   `gs:[200]` before loading `RSP = gs:[192]` (`kernel_rsp0`, 16-byte aligned).
// - Pushing `18 * 8 = 144` bytes (`SyscallFrame`) preserves `(RSP0 - 144) % 16 == 0` prior to `call`.
// - On `SYSRETQ` (`eax == 0`): `rbx, rbp, r12..r15` restored, `rdi, rsi, r8, r9, r10` scrubbed to `0`.
// - On `IRETQ` (`eax != 0`): `rbx, rbp, r12..r15` restored, `rdi, rsi, rcx, r8, r9, r10, r11` scrubbed to `0`.
global_asm!(
    ".global dry_syscall_entry",
    ".balign 16",
    "dry_syscall_entry:",
    "    swapgs",
    "    mov gs:[200], rsp",
    "    mov rsp, gs:[192]",
    "    push 0x1b",
    "    push qword ptr gs:[200]",
    "    push r11",
    "    push 0x23",
    "    push rcx",
    "    push rax",
    "    push rdi",
    "    push rsi",
    "    push rdx",
    "    push r10",
    "    push r8",
    "    push r9",
    "    push rbx",
    "    push rbp",
    "    push r12",
    "    push r13",
    "    push r14",
    "    push r15",
    "    cmp byte ptr [rip + DRY_BENCH_STAMP_ENTRY], 0",
    "    je .Lsyscall_no_stamp",
    "    lfence",
    "    rdtsc",
    "    shl rdx, 32",
    "    or rax, rdx",
    "    mov qword ptr [rip + DRY_BENCH_LAST_SYSCALL_ENTRY_TSC], rax",
    ".Lsyscall_no_stamp:",
    "    mov rdi, rsp",
    "    call {dispatch}",
    "    mov edi, eax",
    "    pop r15",
    "    pop r14",
    "    pop r13",
    "    pop r12",
    "    pop rbp",
    "    pop rbx",
    "    add rsp, 24",
    "    pop rdx",
    "    add rsp, 16",
    "    pop rax",
    "    test edi, edi",
    "    jnz .Lsyscall_iretq_fallback",
    "    mov rcx, [rsp]",
    "    mov r11, [rsp + 16]",
    "    mov rsp, [rsp + 24]",
    "    xor edi, edi",
    "    xor esi, esi",
    "    xor r8d, r8d",
    "    xor r9d, r9d",
    "    xor r10d, r10d",
    "    swapgs",
    "    sysretq",
    ".Lsyscall_iretq_fallback:",
    "    xor edi, edi",
    "    xor esi, esi",
    "    xor ecx, ecx",
    "    xor r8d, r8d",
    "    xor r9d, r9d",
    "    xor r10d, r10d",
    "    xor r11d, r11d",
    "    swapgs",
    "    iretq",
    dispatch = sym super::rust_syscall_dispatch,
);

extern "C" {
    pub fn dry_syscall_entry();
}

/// Program `IA32_EFER.SCE`, `IA32_STAR`, `IA32_LSTAR`, and `IA32_FMASK` on the calling CPU.
pub fn init_syscall_msrs_on_current_cpu() {
    let lstar = dry_syscall_entry as *const () as usize as u64;
    // SAFETY: Valid Ring 0 MSR programming with GDT layout verified compatible with `STAR_SYSCALL_SYSRET`.
    unsafe {
        let efer = rdmsr(MSR_IA32_EFER);
        wrmsr(MSR_IA32_EFER, efer | EFER_SCE);
        wrmsr(MSR_IA32_STAR, STAR_SYSCALL_SYSRET);
        wrmsr(MSR_IA32_LSTAR, lstar);
        wrmsr(MSR_IA32_FMASK, FMASK_SYSCALL_CLEAR);

        assert_ne!(rdmsr(MSR_IA32_EFER) & EFER_SCE, 0, "EFER.SCE failed to latch");
        assert_eq!(rdmsr(MSR_IA32_STAR), STAR_SYSCALL_SYSRET, "STAR readback mismatch");
        assert_eq!(rdmsr(MSR_IA32_LSTAR), lstar, "LSTAR readback mismatch");
        assert_eq!(rdmsr(MSR_IA32_FMASK), FMASK_SYSCALL_CLEAR, "FMASK readback mismatch");
    }
}

/// Validate initial `IRETQ` Ring 3 context (`Stage A.4`) and `SYSRETQ` return safety (`Stage B.5` & `F.16`).
#[inline]
pub fn validate_user_context(
    rip: u64,
    rsp: u64,
    rflags: u64,
    cs: u64,
    ss: u64,
    require_aligned_rsp: bool,
) -> Result<(), UserContextError> {
    if cs != (USER_CS as u64) || ss != (USER_DS as u64) || (cs & 3) != 3 || (ss & 3) != 3 {
        return Err(UserContextError::BadSelector);
    }
    if !is_canonical_48(rip) || !(USER_VADDR_MIN..USER_VADDR_LIMIT).contains(&rip) {
        return Err(UserContextError::NonCanonicalOrKernelRip);
    }
    if !is_canonical_48(rsp) || !(USER_VADDR_MIN..=USER_VADDR_LIMIT).contains(&rsp) {
        return Err(UserContextError::NonCanonicalOrKernelRsp);
    }
    if require_aligned_rsp && (rsp & 0xf) != 0 {
        return Err(UserContextError::UnalignedRsp);
    }
    // Enforce `IF=1`, `bit1=1`, and zero `IOPL/NT/VM/AC/TF`.
    let forbidden = (3 << 12) | (1 << 14) | (1 << 17) | (1 << 18) | (1 << 8);
    if (rflags & USER_RFLAGS_FIXED_SET) != USER_RFLAGS_FIXED_SET || (rflags & forbidden) != 0 {
        return Err(UserContextError::ForbiddenRflags);
    }
    Ok(())
}

/// Reset calling thread's kernel stack to `kstack_top`, scrub all Ring 0 GPRs (except `rdi = arg0`,
/// `rsi = arg1`), execute `swapgs`, and transition to `CPL=3` via hardware `iretq`.
///
/// If `bench_r0_tsc_out` is non-null, writes `rdtsc` immediately prior to `swapgs; iretq`.
#[allow(clippy::too_many_arguments)]
pub unsafe fn enter_user_iretq_unchecked(
    kstack_top: u64,
    user_rip: u64,
    user_rsp: u64,
    user_rflags: u64,
    arg0: u64,
    arg1: u64,
    bench_r0_tsc_ptr: *mut u64,
) -> ! {
    // SAFETY: Caller verified `validate_user_context`, owns `[stack_base .. kstack_top]` with `CLI`
    // active, and loaded the process's `VSpace` into `CR3`.
    unsafe {
        asm!(
            "cli",
            "mov rsp, r11",
            "push {uss}",
            "push r8",
            "push r9",
            "push {ucs}",
            "push rcx",
            "test r10, r10",
            "jz 2f",
            "lfence",
            "rdtsc",
            "shl rdx, 32",
            "or rax, rdx",
            "mov qword ptr [r10], rax",
            "2:",
            "xor eax, eax",
            "xor ebx, ebx",
            "xor ecx, ecx",
            "xor edx, edx",
            "xor ebp, ebp",
            "xor r8d, r8d",
            "xor r9d, r9d",
            "xor r10d, r10d",
            "xor r11d, r11d",
            "xor r12d, r12d",
            "xor r13d, r13d",
            "xor r14d, r14d",
            "xor r15d, r15d",
            "swapgs",
            "iretq",
            uss = const USER_DS as u64,
            ucs = const USER_CS as u64,
            in("r11") kstack_top,
            in("rcx") user_rip,
            in("r8") user_rsp,
            in("r9") user_rflags,
            in("rdi") arg0,
            in("rsi") arg1,
            in("r10") bench_r0_tsc_ptr,
            options(noreturn),
        );
    }
}
