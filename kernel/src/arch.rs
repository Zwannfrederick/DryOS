// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Low-level x86-64 port I/O, MSRs, TSC, CPU control, and QEMU `isa-debug-exit` helpers.

use core::arch::asm;

/// QEMU `-device isa-debug-exit,iobase=0xf4,iosize=0x04` port.
const QEMU_DEBUG_EXIT_PORT: u16 = 0xf4;

/// Host exit status `(0x10 << 1) | 1 == 33`.
pub const QEMU_EXIT_SUCCESS: u32 = 0x10;
/// Host exit status `(0x11 << 1) | 1 == 35`.
pub const QEMU_EXIT_PANIC: u32 = 0x11;

pub const MSR_IA32_EFER: u32 = 0xc000_0080;
pub const MSR_IA32_STAR: u32 = 0xc000_0081;
pub const MSR_IA32_LSTAR: u32 = 0xc000_0082;
pub const MSR_IA32_FMASK: u32 = 0xc000_0084;
pub const MSR_IA32_FS_BASE: u32 = 0xc000_0100;
pub const MSR_IA32_GS_BASE: u32 = 0xc000_0101;
pub const MSR_IA32_KERNEL_GS_BASE: u32 = 0xc000_0102;

#[inline]
pub unsafe fn outb(port: u16, val: u8) {
    // SAFETY: Caller guarantees `port` is a valid architectural I/O port.
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
    }
}

#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    // SAFETY: Caller guarantees `port` is a valid architectural I/O port.
    unsafe {
        asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    val
}

#[inline]
pub unsafe fn outl(port: u16, val: u32) {
    // SAFETY: Caller guarantees `port` is a valid architectural I/O port.
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack, preserves_flags));
    }
}

#[inline]
pub unsafe fn wrmsr(msr: u32, val: u64) {
    let low = val as u32;
    let high = (val >> 32) as u32;
    // SAFETY: Caller guarantees `msr` and `val` are architecturally valid at CPL=0.
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") low,
            in("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
}

#[inline]
pub unsafe fn rdmsr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: Caller guarantees `msr` is readable at CPL=0.
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
    ((high as u64) << 32) | (low as u64)
}

#[inline]
pub fn rdtsc() -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: Serialized `lfence; rdtsc` is readable at CPL=0 with no side effects.
    unsafe {
        asm!(
            "lfence",
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
    ((high as u64) << 32) | (low as u64)
}

#[inline]
pub fn cli() {
    // SAFETY: Kernel runs at CPL=0; masking local interrupts is always valid.
    unsafe {
        asm!("cli", options(nomem, nostack, preserves_flags));
    }
}

#[inline]
pub fn sti() {
    // SAFETY: Caller guarantees IDT, per-CPU GS_BASE, masked 8259 PIC, and Local APIC are ready.
    unsafe {
        asm!("sti", options(nomem, nostack, preserves_flags));
    }
}

/// Save current `RFLAGS.IF` (`bit 9`) and mask local interrupts (`CLI`).
#[inline]
pub fn irq_save_cli() -> bool {
    let was_enabled = (read_rflags() & (1 << 9)) != 0;
    cli();
    was_enabled
}

/// Restore `RFLAGS.IF = 1` (`STI`) if interrupts were previously enabled before `irq_save_cli()`.
#[inline]
pub fn irq_restore(was_enabled: bool) {
    if was_enabled {
        sti();
    }
}

#[inline]
pub fn read_cr0() -> u64 {
    let cr0: u64;
    // SAFETY: Reading CR0 at CPL=0 has no side effects.
    unsafe {
        asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
    }
    cr0
}

#[inline]
pub unsafe fn write_cr0(val: u64) {
    // SAFETY: Caller guarantees `val` preserves `PE` and `PG` with valid architectural flags.
    unsafe {
        asm!("mov cr0, {}", in(reg) val, options(nomem, nostack, preserves_flags));
    }
}

#[inline]
pub fn read_cr3() -> u64 {
    let cr3: u64;
    // SAFETY: Reading CR3 at CPL=0 has no side effects.
    unsafe {
        asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
    }
    cr3
}

#[inline]
pub unsafe fn write_cr3(phys_pml4: u64) {
    // SAFETY: Caller guarantees `phys_pml4` is a 4 KiB-aligned valid PML4 frame mapping the active RIP/RSP.
    unsafe {
        asm!("mov cr3, {}", in(reg) phys_pml4, options(nostack, preserves_flags));
    }
}

#[inline]
pub fn read_cr4() -> u64 {
    let cr4: u64;
    // SAFETY: Reading CR4 at CPL=0 has no side effects.
    unsafe {
        asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
    }
    cr4
}

#[inline]
pub unsafe fn write_cr4(val: u64) {
    // SAFETY: Caller guarantees `val` preserves `PAE` and only sets CPUID-verified feature bits.
    unsafe {
        asm!("mov cr4, {}", in(reg) val, options(nomem, nostack, preserves_flags));
    }
}

#[inline]
pub fn invlpg(vaddr: u64) {
    // SAFETY: `invlpg` is valid for any virtual address at CPL=0 and invalidates matching TLB + paging caches.
    unsafe {
        asm!("invlpg [{}]", in(reg) vaddr, options(nostack, preserves_flags));
    }
}

#[inline]
pub fn read_rsp() -> u64 {
    let rsp: u64;
    // SAFETY: Reading RSP has no side effects.
    unsafe {
        asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags));
    }
    rsp
}

#[inline]
pub fn read_rflags() -> u64 {
    let rflags: u64;
    // SAFETY: Pushing RFLAGS onto kernel stack and popping into a register is safe.
    unsafe {
        asm!("pushfq; pop {}", out(reg) rflags, options(nomem, preserves_flags));
    }
    rflags
}

pub fn qemu_exit(code: u32) -> ! {
    cli();
    // SAFETY: `0xf4` is dedicated to QEMU `isa-debug-exit` in test VMs and harmless on bare metal.
    unsafe {
        outl(QEMU_DEBUG_EXIT_PORT, code);
    }
    halt_forever()
}

pub fn halt_forever() -> ! {
    cli();
    loop {
        // SAFETY: Halting the CPU with interrupts disabled freezes execution deterministically.
        unsafe {
            asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}
