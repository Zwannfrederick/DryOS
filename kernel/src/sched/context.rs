// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! x86-64 Kernel-Thread Assembly Context Switch (`Milestone 1A-F` Cooperative & `1A-G` Preemptive).
//!
//! # Architectural Context-Switch Contract (`System V AMD64 Kernel ABI`)
//! 1. **Stack Alignment (`16`-Byte Invariant & `-mno-red-zone`):**
//!    - Target `x86_64-unknown-none` disables the 128-byte red zone (`disable-redzone: true`) and
//!      kernel SSE/FPU state (`+soft-float,-mmx,-sse`).
//!    - Caller invokes `call dry_switch_to` with `RSP % 16 == 0`; `call` pushes 8-byte `return_rip`
//!      (`RSP % 16 == 8`). Pushing `RFLAGS` + 6 callee-saved GPRs (`56` bytes) yields
//!      `saved_rsp % 16 == 0`.
//!    - Immediately after `mov rsp, rsi` (`% 16 == 0`) — while `RFLAGS.IF == 0` (`CLI`) and while
//!      `next`'s saved GPRs/`RFLAGS` remain intact on `next`'s stack — `call {post_switch}` enters
//!      `finish_post_switch` with System V compliant `RSP % 16 == 8`.
//!    - After `finish_post_switch` returns (`RSP % 16 == 0`), 6 GPRs + `popfq` (`+56` bytes) leave
//!      `RSP % 16 == 8` at `ret_rip`, so `ret` enters `dry_thread_entry_trampoline` with
//!      `RSP == stack_top` (`% 16 == 0`) or resumes the caller with `RSP % 16 == 0`.
//! 2. **Callee-Saved vs. Caller-Saved Registers:**
//!    - Voluntary `dry_switch_to` preserves `RBX`, `RBP`, `R12..R15` + `RFLAGS` (`64`B `SwitchFrame`).
//!    - Involuntary IRQ preemption pushes a `176`B `TrapFrame` (`all 15 GPRs + RFLAGS + RIP + RSP`)
//!      at `IST = 0` on the interrupted thread's private 16 KiB stack *above* `SwitchFrame`.
//! 3. **`RFLAGS` & Interrupt-Masking Ordering:**
//!    - `pushfq; cli` captures the outgoing context's `IF` bit (`0` in cooperative mode or inside
//!      an Interrupt Gate; `1` if called with interrupts enabled), then holds `IF = 0` across the
//!      entire stack handoff and `finish_post_switch` (`stack_owner` release + RQ push + APIC One-Shot
//!      re-arm). `popfq; ret` restores `next`'s exact `IF` state only after handoff is complete.
//! 4. **Explicit Address-Space (`CR3` / TLB) Boundary:**
//!    - All Milestone 1A-F/G kernel threads share the single kernel `CR3` (`SecDomain::Kernel`).
//!    - Per-process `VSpace` (`CR3`) switching and `active_cpus` TLB shootdowns belong to Milestone 1B.

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
    "    cli",
    "    push rbp",
    "    push rbx",
    "    push r12",
    "    push r13",
    "    push r14",
    "    push r15",
    "    mov [rdi], rsp",
    "    mov rsp, rsi",
    "    cld",
    "    mov rdi, rdx",
    "    mov rsi, rcx",
    "    call {post_switch}",
    "    pop r15",
    "    pop r14",
    "    pop r13",
    "    pop r12",
    "    pop rbx",
    "    pop rbp",
    "    popfq",
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
    // Unconditional invariant across BOTH cooperative & preemptive modes: `dry_switch_to` masks
    // local interrupts (`cli`, `RFLAGS.IF == 0`) immediately prior to stack handoff.
    debug_assert_eq!(read_rflags() & (1 << 9), 0, "IF=1 observed inside finish_post_switch");
    crate::sched::complete_post_switch_from_asm(prev_slot, PostSwitchKind::unpack(packed_action));
}

extern "C" fn thread_bootstrap(entry_raw: usize, arg: u64, slot_idx: usize) -> ! {
    let if_enabled = (read_rflags() & (1 << 9)) != 0;
    assert_eq!(
        if_enabled,
        crate::sched::is_preemptive_mode_active(),
        "Bootstrap RFLAGS.IF mismatch on slot {}",
        slot_idx
    );
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
    preemptible: bool,
) -> u64 {
    assert_eq!(stack_top & 0xf, 0, "Thread stack_top {:#x} not 16-byte aligned", stack_top);
    let frame_addr = stack_top - (core::mem::size_of::<InitialSwitchFrame>() as u64);
    assert_eq!(frame_addr & 0xf, 0, "InitialSwitchFrame not 16-byte aligned");
    let rflags = if preemptible { 0x0202 } else { 0x0002 };
    let frame = InitialSwitchFrame {
        r15: 0,
        r14: slot_idx as u64,
        r13: arg,
        r12: entry as usize as u64,
        rbx: 0,
        rbp: 0,
        rflags,
        ret_rip: dry_thread_entry_trampoline as *const () as usize as u64,
    };
    // SAFETY: Caller exclusively owns the freshly allocated `Dead` stack `[stack_base, stack_top)`.
    unsafe {
        write_volatile(frame_addr as *mut InitialSwitchFrame, frame);
    }
    frame_addr
}
