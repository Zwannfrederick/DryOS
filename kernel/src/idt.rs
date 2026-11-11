// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! 64-bit x86-64 Interrupt Descriptor Table (`IDT`) & Exception Dispatch (`#DE..#PF`).

use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::gdt_tss::{DF_IST_INDEX, KERNEL_CS};
use crate::serial;

static IDT_INITIALIZED: AtomicBool = AtomicBool::new(false);
static BP_TRAP_COUNT: AtomicU64 = AtomicU64::new(0);
static LAST_BP_RIP: AtomicU64 = AtomicU64::new(0);

/// Unified CPU Trap/Interrupt Frame pushed by hardware + `dry_isr_common` assembly stub.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

const _: () = assert!(size_of::<TrapFrame>() == 22 * 8);

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    zero: u32,
}

const _: () = assert!(size_of::<IdtEntry>() == 16);

impl IdtEntry {
    const fn missing() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            zero: 0,
        }
    }

    fn gate(handler: u64, ist: u8, dpl: u8) -> Self {
        Self {
            offset_low: handler as u16,
            selector: KERNEL_CS,
            ist: ist & 0x7,
            type_attr: 0x8e | ((dpl & 0x3) << 5), // Present=1, Type=0xE (64-bit Interrupt Gate)
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            zero: 0,
        }
    }
}

#[repr(C, align(16))]
struct IdtTable(UnsafeCell<[IdtEntry; 256]>);

// SAFETY: `IDT` is populated once on BSP before `IDT_INITIALIZED` becomes true, then read-only.
unsafe impl Sync for IdtTable {}

static IDT: IdtTable = IdtTable(UnsafeCell::new([IdtEntry::missing(); 256]));

#[repr(C, packed)]
struct IdtPtr {
    limit: u16,
    base: u64,
}

// Generate 32 architectural exception stubs (`0..=31`) + generic fallback stub (`32..=255`).
// Vectors `8, 10, 11, 12, 13, 14, 17, 21, 29, 30` push a hardware error code; others push `0`.
global_asm!(
    ".macro ISR_NOERR vec",
    ".global dry_isr_stub_\\vec",
    ".balign 16",
    "dry_isr_stub_\\vec:",
    "    push 0",
    "    push \\vec",
    "    jmp dry_isr_common",
    ".endm",
    ".macro ISR_ERR vec",
    ".global dry_isr_stub_\\vec",
    ".balign 16",
    "dry_isr_stub_\\vec:",
    "    push \\vec",
    "    jmp dry_isr_common",
    ".endm",
    "ISR_NOERR 0",
    "ISR_NOERR 1",
    "ISR_NOERR 2",
    "ISR_NOERR 3",
    "ISR_NOERR 4",
    "ISR_NOERR 5",
    "ISR_NOERR 6",
    "ISR_NOERR 7",
    "ISR_ERR   8",
    "ISR_NOERR 9",
    "ISR_ERR   10",
    "ISR_ERR   11",
    "ISR_ERR   12",
    "ISR_ERR   13",
    "ISR_ERR   14",
    "ISR_NOERR 15",
    "ISR_NOERR 16",
    "ISR_ERR   17",
    "ISR_NOERR 18",
    "ISR_NOERR 19",
    "ISR_NOERR 20",
    "ISR_ERR   21",
    "ISR_NOERR 22",
    "ISR_NOERR 23",
    "ISR_NOERR 24",
    "ISR_NOERR 25",
    "ISR_NOERR 26",
    "ISR_NOERR 27",
    "ISR_NOERR 28",
    "ISR_ERR   29",
    "ISR_ERR   30",
    "ISR_NOERR 31",
    "ISR_NOERR 255",
    ".balign 16",
    "dry_isr_common:",
    "    push rax",
    "    push rbx",
    "    push rcx",
    "    push rdx",
    "    push rsi",
    "    push rdi",
    "    push rbp",
    "    push r8",
    "    push r9",
    "    push r10",
    "    push r11",
    "    push r12",
    "    push r13",
    "    push r14",
    "    push r15",
    "    cld",
    "    mov rdi, rsp",
    "    call {dispatch}",
    "    pop r15",
    "    pop r14",
    "    pop r13",
    "    pop r12",
    "    pop r11",
    "    pop r10",
    "    pop r9",
    "    pop r8",
    "    pop rbp",
    "    pop rdi",
    "    pop rsi",
    "    pop rdx",
    "    pop rcx",
    "    pop rbx",
    "    pop rax",
    "    add rsp, 16",
    "    iretq",
    dispatch = sym rust_exception_dispatch,
);

extern "C" {
    fn dry_isr_stub_0();
    fn dry_isr_stub_1();
    fn dry_isr_stub_2();
    fn dry_isr_stub_3();
    fn dry_isr_stub_4();
    fn dry_isr_stub_5();
    fn dry_isr_stub_6();
    fn dry_isr_stub_7();
    fn dry_isr_stub_8();
    fn dry_isr_stub_9();
    fn dry_isr_stub_10();
    fn dry_isr_stub_11();
    fn dry_isr_stub_12();
    fn dry_isr_stub_13();
    fn dry_isr_stub_14();
    fn dry_isr_stub_15();
    fn dry_isr_stub_16();
    fn dry_isr_stub_17();
    fn dry_isr_stub_18();
    fn dry_isr_stub_19();
    fn dry_isr_stub_20();
    fn dry_isr_stub_21();
    fn dry_isr_stub_22();
    fn dry_isr_stub_23();
    fn dry_isr_stub_24();
    fn dry_isr_stub_25();
    fn dry_isr_stub_26();
    fn dry_isr_stub_27();
    fn dry_isr_stub_28();
    fn dry_isr_stub_29();
    fn dry_isr_stub_30();
    fn dry_isr_stub_31();
    fn dry_isr_stub_255();
}

fn read_cr2() -> u64 {
    let cr2: u64;
    // SAFETY: Reading CR2 at CPL=0 is side-effect free.
    unsafe {
        asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags));
    }
    cr2
}

fn vector_mnemonic(vec: u64) -> &'static str {
    match vec {
        0 => "#DE Divide-by-Zero",
        1 => "#DB Debug",
        2 => "NMI",
        3 => "#BP Breakpoint",
        4 => "#OF Overflow",
        5 => "#BR Bound-Range",
        6 => "#UD Invalid-Opcode",
        7 => "#NM Device-Not-Available",
        8 => "#DF Double-Fault",
        10 => "#TS Invalid-TSS",
        11 => "#NP Segment-Not-Present",
        12 => "#SS Stack-Segment",
        13 => "#GP General-Protection",
        14 => "#PF Page-Fault",
        16 => "#MF x87-FP",
        17 => "#AC Alignment-Check",
        18 => "#MC Machine-Check",
        19 => "#XM SIMD-FP",
        21 => "#CP Control-Protection",
        _ => "Unhandled-Vector",
    }
}

extern "C" fn rust_exception_dispatch(frame: &mut TrapFrame) {
    if frame.vector == 3 {
        // `#BP` (`int3`) is a Trap: `frame.rip` points to the instruction immediately after `0xcc`.
        BP_TRAP_COUNT.fetch_add(1, Ordering::AcqRel);
        LAST_BP_RIP.store(frame.rip, Ordering::Release);
        return;
    }

    let cr2 = if frame.vector == 14 { read_cr2() } else { 0 };
    serial::emergency_print(format_args!(
        "\r\n[DRY-EXCEPTION] vec={} ({}) rip={:#x} cs={:#x} rflags={:#x} rsp={:#x} err={:#x} cr2={:#x}\r\n",
        frame.vector,
        vector_mnemonic(frame.vector),
        frame.rip,
        frame.cs,
        frame.rflags,
        frame.rsp,
        frame.error_code,
        cr2
    ));
    panic!(
        "unrecoverable kernel CPU exception vec={} ({}) at rip={:#x}",
        frame.vector,
        vector_mnemonic(frame.vector),
        frame.rip
    );
}

pub fn init_bsp_idt() {
    let stubs: [unsafe extern "C" fn(); 32] = [
        dry_isr_stub_0, dry_isr_stub_1, dry_isr_stub_2, dry_isr_stub_3,
        dry_isr_stub_4, dry_isr_stub_5, dry_isr_stub_6, dry_isr_stub_7,
        dry_isr_stub_8, dry_isr_stub_9, dry_isr_stub_10, dry_isr_stub_11,
        dry_isr_stub_12, dry_isr_stub_13, dry_isr_stub_14, dry_isr_stub_15,
        dry_isr_stub_16, dry_isr_stub_17, dry_isr_stub_18, dry_isr_stub_19,
        dry_isr_stub_20, dry_isr_stub_21, dry_isr_stub_22, dry_isr_stub_23,
        dry_isr_stub_24, dry_isr_stub_25, dry_isr_stub_26, dry_isr_stub_27,
        dry_isr_stub_28, dry_isr_stub_29, dry_isr_stub_30, dry_isr_stub_31,
    ];

    // SAFETY: Executed once on BSP with interrupts masked before `IDT_INITIALIZED` is published.
    unsafe {
        let table = &mut *IDT.0.get();
        for (vec, stub) in stubs.iter().enumerate() {
            let ist = if vec == 8 { DF_IST_INDEX } else { 0 };
            let dpl = if vec == 3 { 3 } else { 0 };
            table[vec] = IdtEntry::gate(*stub as usize as u64, ist, dpl);
        }
        let fallback = dry_isr_stub_255 as *const () as usize as u64;
        for slot in table.iter_mut().skip(32) {
            *slot = IdtEntry::gate(fallback, 0, 0);
        }
    }
    IDT_INITIALIZED.store(true, Ordering::Release);
    load_idt();
}

pub fn load_idt() {
    assert!(IDT_INITIALIZED.load(Ordering::Acquire), "IDT not initialized before lidt");
    let idtr = IdtPtr {
        limit: (size_of::<[IdtEntry; 256]>() - 1) as u16,
        base: IDT.0.get() as u64,
    };
    // SAFETY: `IDT` contains 256 valid 64-bit interrupt gates in static kernel `.bss`/`.data`.
    unsafe {
        asm!("lidt [{}]", in(reg) &idtr, options(nostack, preserves_flags));
    }
}

/// Trigger a live hardware `#BP` (`int3`) trap and verify exact `RIP` capture + `iretq` resume.
pub fn verify_breakpoint_trap_recovery() -> (u64, u64) {
    let before = BP_TRAP_COUNT.load(Ordering::Acquire);
    let after_int3_label: u64;
    // SAFETY: `int3` triggers vector 3 (`#BP`), which records `frame.rip` and returns via `iretq`.
    unsafe {
        asm!(
            "int3",
            "2:",
            "lea {}, [rip + 2b]",
            out(reg) after_int3_label,
            options(nostack, preserves_flags),
        );
    }
    let after = BP_TRAP_COUNT.load(Ordering::Acquire);
    let recorded_rip = LAST_BP_RIP.load(Ordering::Acquire);
    assert_eq!(after, before + 1, "#BP trap handler did not fire exactly once");
    assert_eq!(recorded_rip, after_int3_label, "#BP saved RIP mismatch");
    (after, recorded_rip)
}
