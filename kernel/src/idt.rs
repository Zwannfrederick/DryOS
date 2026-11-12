// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! 64-bit x86-64 Interrupt Descriptor Table (`IDT`) & Exception Dispatch (`#DE..#PF`).

use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::arch::{qemu_exit, read_rsp, QEMU_EXIT_PANIC};
use crate::gdt_tss::{verify_df_canary_only, DF_IST_INDEX, KERNEL_CS};
use crate::percpu::{current_percpu, try_current_percpu, PerCpuData};
use crate::serial;

static IDT_INITIALIZED: AtomicBool = AtomicBool::new(false);
static TOTAL_BP_TRAPS: AtomicU64 = AtomicU64::new(0);

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

// SAFETY: `IDT` is populated once on BSP before `IDT_INITIALIZED` is published (`Release`),
// after which all CPUs only perform read-only `lidt` and hardware interrupt vector lookups.
unsafe impl Sync for IdtTable {}

static IDT: IdtTable = IdtTable(UnsafeCell::new([IdtEntry::missing(); 256]));

#[repr(C, packed)]
struct IdtPtr {
    limit: u16,
    base: u64,
}

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
    "ISR_NOERR 39",
    "ISR_NOERR 47",
    "ISR_NOERR 48",
    "ISR_NOERR 49",
    "ISR_NOERR 50",
    "ISR_NOERR 51",
    "ISR_NOERR 52",
    "ISR_NOERR 53",
    "ISR_NOERR 54",
    "ISR_NOERR 55",
    "ISR_NOERR 56",
    "ISR_NOERR 57",
    "ISR_NOERR 58",
    "ISR_NOERR 59",
    "ISR_NOERR 60",
    "ISR_NOERR 61",
    "ISR_NOERR 62",
    "ISR_NOERR 63",
    "ISR_NOERR 237",
    "ISR_NOERR 238",
    "ISR_NOERR 239",
    "ISR_NOERR 254",
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
    "    cmp byte ptr [rip + DRY_SMAP_ACTIVE], 0",
    "    je .Lisr_no_clac",
    "    .byte 0x0f, 0x01, 0xca",
    ".Lisr_no_clac:",
    "    xor ebx, ebx",
    "    test byte ptr [rsp + 144], 3",
    "    jnz .Lisr_do_swapgs",
    "    cmp qword ptr [rsp + 120], 2",
    "    je .Lisr_check_gs_msr",
    "    cmp qword ptr [rsp + 120], 8",
    "    jne .Lisr_gs_ready",
    ".Lisr_check_gs_msr:",
    "    mov ecx, 0xc0000101",
    "    rdmsr",
    "    test edx, edx",
    "    js .Lisr_gs_ready",
    ".Lisr_do_swapgs:",
    "    swapgs",
    "    mov ebx, 1",
    ".Lisr_gs_ready:",
    "    mov rdi, rsp",
    "    call {dispatch}",
    "    test ebx, ebx",
    "    jz .Lisr_exit_pop",
    "    swapgs",
    ".Lisr_exit_pop:",
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
    fn dry_isr_stub_39();
    fn dry_isr_stub_47();
    fn dry_isr_stub_48();
    fn dry_isr_stub_49();
    fn dry_isr_stub_50();
    fn dry_isr_stub_51();
    fn dry_isr_stub_52();
    fn dry_isr_stub_53();
    fn dry_isr_stub_54();
    fn dry_isr_stub_55();
    fn dry_isr_stub_56();
    fn dry_isr_stub_57();
    fn dry_isr_stub_58();
    fn dry_isr_stub_59();
    fn dry_isr_stub_60();
    fn dry_isr_stub_61();
    fn dry_isr_stub_62();
    fn dry_isr_stub_63();
    fn dry_isr_stub_237();
    fn dry_isr_stub_238();
    fn dry_isr_stub_239();
    fn dry_isr_stub_254();
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

fn handle_double_fault(frame: &TrapFrame, pcpu: Option<&'static PerCpuData>) -> ! {
    crate::enter_fatal_or_halt();
    let handler_rsp = read_rsp();
    if let Some(cpu) = pcpu {
        // Non-reentrancy guard: nested `#DF` on `IST1` would clobber the active frame; exit immediately.
        if cpu.in_df_handler.swap(true, Ordering::AcqRel) {
            qemu_exit(QEMU_EXIT_PANIC);
        }
        let on_private_ist1 = handler_rsp >= cpu.df_ist1_base && handler_rsp < cpu.df_ist1_top;
        let canary_ok = verify_df_canary_only(cpu.cpu_idx as usize);
        if on_private_ist1 && canary_ok {
            serial::emergency_print(format_args!(
                "\r\n[DRY-DF-IST1] PASS: hardware #DF caught on cpu={} lapic={} on private IST1 rsp={:#x} [base={:#x}, top={:#x}] fault_rsp={:#x}\r\n",
                cpu.cpu_idx,
                cpu.lapic_id,
                handler_rsp,
                cpu.df_ist1_base,
                cpu.df_ist1_top,
                frame.rsp
            ));
        } else {
            serial::emergency_print(format_args!(
                "\r\n[DRY-DF-IST1] FAIL: cpu={} rsp={:#x} expected=[{:#x}, {:#x}) canary_ok={}\r\n",
                cpu.cpu_idx, handler_rsp, cpu.df_ist1_base, cpu.df_ist1_top, canary_ok
            ));
        }
    }
    panic!(
        "terminal hardware Double Fault (#DF) at rip={:#x} fault_rsp={:#x}",
        frame.rip, frame.rsp
    );
}

extern "C" fn rust_exception_dispatch(frame: &mut TrapFrame) {
    if frame.vector == (crate::apic::VEC_RESCHED_WAKE as u64) {
        crate::apic::lapic_eoi();
        return;
    }

    if frame.vector == (crate::apic::VEC_TLB_SHOOTDOWN as u64) {
        crate::vspace::tlb::on_tlb_shootdown_ipi();
        return;
    }

    if frame.vector == (crate::apic::VEC_LAPIC_TIMER as u64) {
        crate::sched::on_lapic_timer_irq(frame);
        return;
    }

    if frame.vector == (crate::apic::VEC_APIC_SPURIOUS as u64) {
        // Intel SDM Vol. 3A 10.9: Spurious APIC interrupt MUST NOT write EOI.
        crate::apic::record_spurious_apic_irq();
        return;
    }

    if frame.vector >= (crate::apic::VEC_EXT_IRQ_BASE as u64)
        && frame.vector <= (crate::apic::VEC_EXT_IRQ_LAST as u64)
    {
        crate::irq::on_external_irq(frame.vector as u8);
        return;
    }

    if frame.vector == (crate::apic::VEC_PIC_SPURIOUS_MASTER as u64)
        || frame.vector == (crate::apic::VEC_PIC_SPURIOUS_SLAVE as u64)
    {
        return;
    }

    if frame.vector == 2 {
        let pcpu = current_percpu();
        assert_eq!(
            pcpu.self_ptr,
            crate::percpu::read_gs_u64_at_0(),
            "NMI GS_BASE invariant broken on cpu={}",
            pcpu.cpu_idx
        );
        pcpu.nmi_count.fetch_add(1, Ordering::AcqRel);
        return;
    }

    let pcpu = try_current_percpu();

    if frame.vector == 3 && (frame.cs & 3) == 0 {
        // `#BP` (`int3`) is a Trap: `frame.rip` points to the instruction immediately after `0xcc`.
        TOTAL_BP_TRAPS.fetch_add(1, Ordering::AcqRel);
        if let Some(cpu) = pcpu {
            cpu.bp_trap_count.fetch_add(1, Ordering::AcqRel);
            cpu.last_bp_rip.store(frame.rip, Ordering::Release);
        }
        return;
    }

    if frame.vector == 8 {
        handle_double_fault(frame, pcpu);
    }

    if frame.vector == 14 && (frame.cs & 3) == 0 && crate::uaccess::try_fixup_uaccess_pf(frame) {
        return;
    }

    if (frame.cs & 3) == 3 {
        let cr2 = if frame.vector == 14 { read_cr2() } else { 0 };
        crate::proc::handle_user_exception(frame, cr2);
    }

    crate::enter_fatal_or_halt();
    let cr2 = if frame.vector == 14 { read_cr2() } else { 0 };
    let cpu_id = pcpu.map_or(u64::MAX, |c| c.cpu_idx);
    serial::emergency_print(format_args!(
        "\r\n[DRY-EXCEPTION] cpu={} vec={} ({}) rip={:#x} cs={:#x} rflags={:#x} rsp={:#x} err={:#x} cr2={:#x}\r\n",
        cpu_id,
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
        "unrecoverable kernel CPU exception vec={} ({}) on cpu={} at rip={:#x}",
        frame.vector,
        vector_mnemonic(frame.vector),
        cpu_id,
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
        let fallback = dry_isr_stub_254 as *const () as usize as u64;
        for slot in table.iter_mut().skip(32) {
            *slot = IdtEntry::gate(fallback, 0, 0);
        }
        table[crate::apic::VEC_PIC_SPURIOUS_MASTER as usize] =
            IdtEntry::gate(dry_isr_stub_39 as *const () as usize as u64, 0, 0);
        table[crate::apic::VEC_PIC_SPURIOUS_SLAVE as usize] =
            IdtEntry::gate(dry_isr_stub_47 as *const () as usize as u64, 0, 0);
        let ext_stubs: [unsafe extern "C" fn(); 16] = [
            dry_isr_stub_48, dry_isr_stub_49, dry_isr_stub_50, dry_isr_stub_51,
            dry_isr_stub_52, dry_isr_stub_53, dry_isr_stub_54, dry_isr_stub_55,
            dry_isr_stub_56, dry_isr_stub_57, dry_isr_stub_58, dry_isr_stub_59,
            dry_isr_stub_60, dry_isr_stub_61, dry_isr_stub_62, dry_isr_stub_63,
        ];
        for (idx, stub) in ext_stubs.iter().enumerate() {
            table[(crate::apic::VEC_EXT_IRQ_BASE as usize) + idx] =
                IdtEntry::gate(*stub as usize as u64, 0, 0);
        }
        // `IST = 0`: `TrapFrame` lands on the active thread's private 16 KiB stack (`#DF` owns `IST1`).
        table[crate::apic::VEC_RESCHED_WAKE as usize] =
            IdtEntry::gate(dry_isr_stub_237 as *const () as usize as u64, 0, 0);
        table[crate::apic::VEC_TLB_SHOOTDOWN as usize] =
            IdtEntry::gate(dry_isr_stub_238 as *const () as usize as u64, 0, 0);
        table[crate::apic::VEC_LAPIC_TIMER as usize] =
            IdtEntry::gate(dry_isr_stub_239 as *const () as usize as u64, 0, 0);
        table[crate::apic::VEC_APIC_SPURIOUS as usize] =
            IdtEntry::gate(dry_isr_stub_255 as *const () as usize as u64, 0, 0);
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
    // SAFETY: `IDT` contains 256 valid 64-bit interrupt gates in static kernel `.bss`.
    unsafe {
        asm!("lidt [{}]", in(reg) &idtr, options(nostack, preserves_flags));
    }
}

/// Trigger a live hardware `#BP` (`int3`) trap on the calling CPU and verify per-CPU `GS_BASE`
/// trap accounting + exact `RIP` capture + `iretq` resume.
pub fn verify_breakpoint_trap_recovery(pcpu: &'static PerCpuData) -> (u64, u64) {
    let before = pcpu.bp_trap_count.load(Ordering::Acquire);
    let after_int3_label: u64;
    // SAFETY: `int3` triggers vector 3 (`#BP`), updating `pcpu.bp_trap_count` and returning via `iretq`.
    unsafe {
        asm!(
            "int3",
            "2:",
            "lea {}, [rip + 2b]",
            out(reg) after_int3_label,
            options(nostack, preserves_flags),
        );
    }
    let after = pcpu.bp_trap_count.load(Ordering::Acquire);
    let recorded_rip = pcpu.last_bp_rip.load(Ordering::Acquire);
    assert_eq!(after, before + 1, "Per-CPU #BP trap handler did not fire once on CPU {}", pcpu.cpu_idx);
    assert_eq!(recorded_rip, after_int3_label, "Per-CPU #BP saved RIP mismatch on CPU {}", pcpu.cpu_idx);
    (after, recorded_rip)
}

pub fn total_bp_traps() -> u64 {
    TOTAL_BP_TRAPS.load(Ordering::Acquire)
}
