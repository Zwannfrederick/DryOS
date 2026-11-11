// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Early 16550 UART serial console (`COM1 = 0x3F8`, 115200 8N1) with IRQ-safe spinlock.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::{cli, inb, outb, read_rflags};

const COM1_PORT: u16 = 0x3f8;
static SERIAL_READY: AtomicBool = AtomicBool::new(false);
static SERIAL_LOCK: AtomicBool = AtomicBool::new(false);

pub struct SerialPort;

pub fn init() {
    // SAFETY: Standard PC 16550 UART initialization sequence on I/O ports `0x3F8..=0x3FD`.
    unsafe {
        outb(COM1_PORT + 1, 0x00); // Disable UART interrupts
        outb(COM1_PORT + 3, 0x80); // Enable DLAB
        outb(COM1_PORT, 0x01); // Divisor low byte (115200 baud)
        outb(COM1_PORT + 1, 0x00); // Divisor high byte
        outb(COM1_PORT + 3, 0x03); // 8 bits, no parity, 1 stop bit (8N1)
        outb(COM1_PORT + 2, 0xc7); // Enable & clear FIFO, 14-byte threshold
        outb(COM1_PORT + 4, 0x0b); // RTS/DSR set
    }
    SERIAL_READY.store(true, Ordering::Release);
}

#[inline]
fn write_byte_raw(byte: u8) {
    // SAFETY: Polls LSR bit 5 (THR empty) and writes data byte to THR (`0x3F8`).
    unsafe {
        for _ in 0..100_000 {
            if (inb(COM1_PORT + 5) & 0x20) != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        outb(COM1_PORT, byte);
    }
}

impl Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if b == b'\n' {
                write_byte_raw(b'\r');
            }
            write_byte_raw(b);
        }
        Ok(())
    }
}

pub fn _print(args: fmt::Arguments<'_>) {
    if !SERIAL_READY.load(Ordering::Acquire) {
        init();
    }
    let saved_if = (read_rflags() & (1 << 9)) != 0;
    cli();
    while SERIAL_LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    let _ = SerialPort.write_fmt(args);
    SERIAL_LOCK.store(false, Ordering::Release);
    if saved_if {
        // SAFETY: Restores previous IF=1 state only if interrupts were enabled on entry.
        unsafe {
            core::arch::asm!("sti", options(nomem, nostack, preserves_flags));
        }
    }
}

/// Lock-free emergency print used exclusively from `#[panic_handler]` / `#DF` when a CPU may
/// already hold `SERIAL_LOCK` at the time of fault.
pub fn emergency_print(args: fmt::Arguments<'_>) {
    cli();
    let _ = SerialPort.write_fmt(args);
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::serial::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($fmt:expr) => ($crate::serial_print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => ($crate::serial_print!(concat!($fmt, "\n"), $($arg)*));
}
