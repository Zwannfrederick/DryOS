// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! x86-64 Cooperative Kernel-Thread Assembly Context Switch (`Milestone 1A-F`).
//!
//! # Architectural Context-Switch Contract (`System V AMD64 Kernel ABI`)
//! 1. **Stack Alignment (`16`-Byte Invariant & `-mno-red-zone`):**
//!    - Target `x86_64-unknown-none` disables the 128-byte red zone (`disable-redzone: true`) and
//!      kernel SSE/FPU state (`+soft-float,-mmx,-sse`).
//!    - Caller invokes `call dry_switch_to` with `RSP % 16 == 0`; `call` pushes 8-byte `return_rip`
//!      (`RSP % 16 == 8`). Pushing 7 qwords (`56` bytes) yields `saved_rsp % 16 == 0`.
//!    - After `mov rsp, rsi` (`% 16 == 0`) and popping 7 qwords (`+56` bytes), `RSP % 16 == 8`.
//!      `sub rsp, 8` aligns `RSP % 16 == 0` prior to `call {post_switch}` so `finish_post_switch`
//!      is entered with `RSP % 16 == 8` on **both** resumed threads and newly spawned threads.
//!    - For a brand-new thread, `ret` pops `dry_thread_entry_trampoline` at `stack_top - 8`,
//!      entering the trampoline with `RSP == stack_top` (`% 16 == 0`) and `RBP == 0`, so
//!      `call {bootstrap}` enters Rust with `RSP == stack_top - 8` (`% 16 == 8`).
//! 2. **Callee-Saved Registers:**
//!    - Preserves `RBX`, `RBP`, `R12`, `R13`, `R14`, `R15` on the outgoing kernel stack.
//!    - New threads seed `R12 = entry_fn`, `R13 = entry_arg`, `R14 = slot_idx`, `RBP = 0`.
//! 3. **`RFLAGS` & Interrupt State (`Stage 1 Cooperative Contract`):**
//!    - `pushfq` / `popfq` + `cld` preserves architectural flags with `DF = 0`.
//!    - In Milestone 1A-F (cooperative kernel-thread stage), `RFLAGS.IF == 0` (`CLI`) is invariant
//!      across all threads (seeded as `0x0002`, verified on switch); Local APIC timer and
//!      interrupt-driven preemption are intentionally excluded until Stage 2.
//! 4. **Explicit Address-Space (`CR3` / TLB) Boundary:**
//!    - All Milestone 1A-F cooperative kernel threads share the single kernel `CR3` (`SecDomain::Kernel`).
//!    - `dry_switch_to` does **not** switch `CR3` or perform `invlpg` / IPI TLB shootdowns; per-process
//!      `VSpace` switching and `active_cpus` TLB shootdowns are deferred to Milestone 1B.

use core::arch::global_asm;
use core::ptr::write_volatile;

use crate::arch::read_rflags;

pub type KernelThreadFn = fn(u64);

/// Initial 64-byte (`8 * u64`) frame written at `stack_top - 64` for a newly spawned kernel thread.
#[repr(C)]
struct InitialSwitchFrame {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    rbx: u64,
    rbp: u64,
    rflags: u64,
    ret_rip: u64,
}

const _: () = assert!(core::mem::size_of::<InitialSwitchFrame>() == 64);

#[derive(Clone, Copy)]
pub enum PostSwitchKind {
    FromIdle,
    Yield { target_cpu: usize },
    Park { target_cpu: usize },
    Exit,
}

impl PostSwitchKind {
    #[inline]
    pub fn pack(self) -> u64 {
        match self {
            Self::FromIdle => 1,
            Self::Yield { target_cpu } => 2 | ((target_cpu as u64) << 8),
            Self::Park { target_cpu } => 3 | ((target_cpu as u64) << 8),
            Self::Exit => 4,
        }
    }

    #[inline]
    pub fn unpack(raw: u64) -> Self {
        let target_cpu = ((raw >> 8) & 0xffff) as usize;
        match raw & 0xff {
            1 => Self::FromIdle,
            2 => Self::Yield { target_cpu },
            3 => Self::Park { target_cpu },
            4 => Self::Exit,
            other => panic!("Invalid PostSwitchKind code {}", other),
        }
    }
}

global_asm!(
    ".global dry_switch_to",
    ".balign 16",
    "dry_switch_to:",
    "    pushfq",
    "    push rbp",
    "    push rbx",
    "    push r12",
    "    push r13",
    "    push r14",
    "    push r15",
    "    mov [rdi], rsp",
    "    mov rsp, rsi",
    "    pop r15",
    "    pop r14",
    "    pop r13",
    "    pop r12",
    "    pop rbx",
    "    pop rbp",
    "    popfq",
    "    cld",
    "    sub rsp, 8",
    "    mov rdi, rdx",
    "    mov rsi, rcx",
    "    call {post_switch}",
    "    add rsp, 8",
    "    ret",
    ".global dry_thread_entry_trampoline",
    ".balign 16",
    "dry_thread_entry_trampoline:",
    "    xor ebp, ebp",
    "    mov rdi, r12",
    "    mov rsi, r13",
    "    mov rdx, r14",
    "    call {bootstrap}",
    "    ud2",
    post_switch = sym finish_post_switch,
    bootstrap = sym thread_bootstrap,
);

extern "C" {
    pub fn dry_switch_to(
        prev_rsp_out: *mut u64,
        next_rsp: u64,
        prev_slot: usize,
        packed_post_action: u64,
    );
    fn dry_thread_entry_trampoline();
}

extern "C" fn finish_post_switch(prev_slot: usize, packed_action: u64) {
    // Enforce Stage 1 cooperative interrupt-masked invariant (`RFLAGS.IF == 0`).
    debug_assert_eq!(read_rflags() & (1 << 9), 0, "IF=1 observed during cooperative switch");
    crate::sched::complete_post_switch_from_asm(prev_slot, PostSwitchKind::unpack(packed_action));
}

extern "C" fn thread_bootstrap(entry_raw: usize, arg: u64, slot_idx: usize) -> ! {
    debug_assert_eq!(read_rflags() & (1 << 9), 0, "IF=1 observed in thread_bootstrap");
    crate::sched::verify_current_thread_entry_bounds(slot_idx);
    // SAFETY: `entry_raw` was transmuted from a valid `fn(u64)` in `init_new_thread_stack`.
    let entry: KernelThreadFn = unsafe { core::mem::transmute::<usize, KernelThreadFn>(entry_raw) };
    entry(arg);
    crate::sched::exit_current_thread()
}

/// Build the initial 16-byte-aligned `InitialSwitchFrame` at `stack_top - 64` for `slot_idx`.
pub unsafe fn init_new_thread_stack(
    stack_top: u64,
    slot_idx: usize,
    entry: KernelThreadFn,
    arg: u64,
) -> u64 {
    assert_eq!(stack_top & 0xf, 0, "Thread stack_top {:#x} not 16-byte aligned", stack_top);
    let frame_addr = stack_top - (core::mem::size_of::<InitialSwitchFrame>() as u64);
    assert_eq!(frame_addr & 0xf, 0, "InitialSwitchFrame not 16-byte aligned");
    let frame = InitialSwitchFrame {
        r15: 0,
        r14: slot_idx as u64,
        r13: arg,
        r12: entry as usize as u64,
        rbx: 0,
        rbp: 0,
        rflags: 0x0002, // Architectural reserved bit 1 set, IF=0, DF=0
        ret_rip: dry_thread_entry_trampoline as *const () as usize as u64,
    };
    // SAFETY: Caller exclusively owns the freshly allocated `Dead` stack `[stack_base, stack_top)`.
    unsafe {
        write_volatile(frame_addr as *mut InitialSwitchFrame, frame);
    }
    frame_addr
}
