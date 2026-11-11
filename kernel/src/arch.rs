// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Low-level x86-64 port I/O, CPU control, and QEMU `isa-debug-exit` helpers.

use core::arch::asm;

/// QEMU `-device isa-debug-exit,iobase=0xf4,iosize=0x04` port.
/// Writing `code` causes QEMU to exit with host status `(code << 1) | 1`.
const QEMU_DEBUG_EXIT_PORT: u16 = 0xf4;

/// Host exit status `(0x10 << 1) | 1 == 33`.
pub const QEMU_EXIT_SUCCESS: u32 = 0x10;
/// Host exit status `(0x11 << 1) | 1 == 35`.
pub const QEMU_EXIT_PANIC: u32 = 0x11;

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
pub fn cli() {
    // SAFETY: Kernel runs at CPL=0; masking local interrupts is always valid.
    unsafe {
        asm!("cli", options(nomem, nostack, preserves_flags));
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
