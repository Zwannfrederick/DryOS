// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Early 16550 UART serial console (`COM1 = 0x3F8`, 115200 8N1) with hardware presence probing
//! (so UART-less laptops never stall on missing COM1 LSR polling) and an in-memory 16 KiB
//! diagnostic ring buffer (`dmesg` / `/persist/boot.log`).

use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::{cli, inb, outb, read_rflags};

const COM1_PORT: u16 = 0x3f8;
const DIAG_RING_CAP: usize = 16 * 1024;
const UART_TX_SPIN_LIMIT: usize = 4096;

static SERIAL_READY: AtomicBool = AtomicBool::new(false);
static UART_HW_PRESENT: AtomicBool = AtomicBool::new(false);
static SERIAL_LOCK: AtomicBool = AtomicBool::new(false);

struct DiagRing {
    buf: [u8; DIAG_RING_CAP],
    total_written: usize,
}

struct SyncDiagRing(UnsafeCell<DiagRing>);
// SAFETY: Protected by `SERIAL_LOCK` (`CLI`); lock-free panic path skips lock only after `enter_fatal_or_halt`.
unsafe impl Sync for SyncDiagRing {}

static DIAG_RING: SyncDiagRing = SyncDiagRing(UnsafeCell::new(DiagRing {
    buf: [0; DIAG_RING_CAP],
    total_written: 0,
}));

pub struct SerialPort;

#[inline]
pub fn is_uart_hw_present() -> bool {
    UART_HW_PRESENT.load(Ordering::Acquire)
}

#[inline]
fn probe_com1_uart() -> bool {
    // SAFETY: Standard 16550 Scratch Register (`COM1+7`) read/write probe + LSR floating-bus check.
    unsafe {
        let orig_scr = inb(COM1_PORT + 7);
        outb(COM1_PORT + 7, 0x5a);
        let r1 = inb(COM1_PORT + 7);
        outb(COM1_PORT + 7, 0xa5);
        let r2 = inb(COM1_PORT + 7);
        outb(COM1_PORT + 7, orig_scr);
        let lsr = inb(COM1_PORT + 5);
        r1 == 0x5a && r2 == 0xa5 && lsr != 0xff
    }
}

pub fn init() {
    let hw_ok = probe_com1_uart();
    if hw_ok {
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
    }
    UART_HW_PRESENT.store(hw_ok, Ordering::Release);
    SERIAL_READY.store(true, Ordering::Release);
}

#[inline]
fn push_diag_byte(byte: u8) {
    // SAFETY: Called under `SERIAL_LOCK` (or exclusive panic context).
    unsafe {
        let ring = &mut *DIAG_RING.0.get();
        let idx = ring.total_written & (DIAG_RING_CAP - 1);
        ring.buf[idx] = byte;
        ring.total_written = ring.total_written.wrapping_add(1);
    }
}

#[inline]
fn write_byte_raw(byte: u8) {
    push_diag_byte(byte);
    if !UART_HW_PRESENT.load(Ordering::Relaxed) {
        return;
    }
    // SAFETY: Polls LSR bit 5 (THR empty) with bounded spin and writes data byte to THR (`0x3F8`).
    unsafe {
        for _ in 0..UART_TX_SPIN_LIMIT {
            if (inb(COM1_PORT + 5) & 0x20) != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        outb(COM1_PORT, byte);
    }
}

/// Read `[off .. off + dst.len())` from the last `tail_max` bytes of `DIAG_RING` (unlocked).
unsafe fn read_ring_window_unlocked(tail_max: usize, off: usize, dst: &mut [u8]) -> (usize, usize) {
    // SAFETY: Caller guarantees either `SERIAL_LOCK` is held with `CLI` or exclusive panic ownership.
    unsafe {
        let ring = &*DIAG_RING.0.get();
        let total = ring.total_written.min(DIAG_RING_CAP).min(tail_max);
        if off >= total || dst.is_empty() {
            return (total, 0);
        }
        let n = (total - off).min(dst.len());
        let base_total = ring.total_written.saturating_sub(total);
        for (i, slot) in dst.iter_mut().take(n).enumerate() {
            *slot = ring.buf[(base_total + off + i) & (DIAG_RING_CAP - 1)];
        }
        (total, n)
    }
}

/// Copy `[off .. off + dst.len())` within the tail window `tail_max` of `DIAG_RING`.
#[inline(never)]
pub fn copy_diag_ring_window(tail_max: usize, off: usize, dst: &mut [u8]) -> (usize, usize) {
    if dst.is_empty() || tail_max == 0 {
        return (0, 0);
    }
    let saved_if = (read_rflags() & (1 << 9)) != 0;
    cli();
    while SERIAL_LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        if crate::is_panicking() {
            break;
        }
        core::hint::spin_loop();
    }
    // SAFETY: `SERIAL_LOCK` held with interrupts masked.
    let res = unsafe { read_ring_window_unlocked(tail_max, off, dst) };
    SERIAL_LOCK.store(false, Ordering::Release);
    if saved_if {
        // SAFETY: Restores previous IF=1 state only if interrupts were enabled on entry.
        unsafe {
            core::arch::asm!("sti", options(nomem, nostack, preserves_flags));
        }
    }
    res
}

/// Lock-free tail copy exclusively for fatal panic/exception screen rendering after `enter_fatal_or_halt`.
#[inline(never)]
pub fn copy_diag_ring_tail_panic(dst: &mut [u8]) -> usize {
    // SAFETY: Caller has claimed exclusive panic ownership via `enter_fatal_or_halt`.
    unsafe { read_ring_window_unlocked(DIAG_RING_CAP, 0, dst).1 }
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

/// Atomically write bounded user debug text under `SERIAL_LOCK`, sanitizing non-printable bytes.
pub fn write_sanitized_user_bytes(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
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
    for &b in bytes {
        match b {
            b'\n' => {
                write_byte_raw(b'\r');
                write_byte_raw(b'\n');
            }
            b'\t' | 0x20..=0x7e => write_byte_raw(b),
            _ => write_byte_raw(b'?'),
        }
    }
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
