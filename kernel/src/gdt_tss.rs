// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Per-CPU 64-bit GDT, TSS, and IST1 Double-Fault (`#DF`) Stack Setup (`x86-64`).
//!
//! Segment selector layout is already ordered for `ADR-002` `SYSCALL`/`SYSRETQ` (`IA32_STAR`):
//! `0x08 = Kernel CS`, `0x10 = Kernel DS`, `0x18 = User DS`, `0x20 = User CS64`, `0x28 = TSS`.

use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::size_of;

pub const KERNEL_CS: u16 = 0x08;
pub const KERNEL_DS: u16 = 0x10;
pub const TSS_SEL: u16 = 0x28;
pub const DF_IST_INDEX: u8 = 1;

const DF_STACK_BYTES: usize = 16 * 1024;
pub const MAX_CPUS: usize = 64;

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct AlignedStack([u8; DF_STACK_BYTES]);

/// Architectural 64-bit Task State Segment (`104` bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct Tss64 {
    reserved0: u32,
    pub rsp: [u64; 3],
    reserved1: u64,
    pub ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    iomap_base: u16,
}

const _: () = assert!(size_of::<Tss64>() == 104);

impl Tss64 {
    const fn empty() -> Self {
        Self {
            reserved0: 0,
            rsp: [0; 3],
            reserved1: 0,
            ist: [0; 7],
            reserved2: 0,
            reserved3: 0,
            iomap_base: size_of::<Self>() as u16,
        }
    }
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct CpuDescBlock {
    gdt: [u64; 7],
    tss: Tss64,
    df_stack: AlignedStack,
    kern_stack: AlignedStack,
}

impl CpuDescBlock {
    const fn new() -> Self {
        Self {
            gdt: [
                0x0000_0000_0000_0000, // 0x00: Null
                0x00af_9a00_0000_ffff, // 0x08: Kernel 64-bit Code (DPL=0, L=1)
                0x00cf_9200_0000_ffff, // 0x10: Kernel Data (DPL=0)
                0x00cf_f200_0000_ffff, // 0x18: User Data (DPL=3)
                0x00af_fa00_0000_ffff, // 0x20: User 64-bit Code (DPL=3, L=1)
                0,                     // 0x28: TSS low 64 bits (filled at init)
                0,                     // 0x30: TSS high 64 bits (filled at init)
            ],
            tss: Tss64::empty(),
            df_stack: AlignedStack([0; DF_STACK_BYTES]),
            kern_stack: AlignedStack([0; DF_STACK_BYTES]),
        }
    }
}

struct CpuDescArray(UnsafeCell<[CpuDescBlock; MAX_CPUS]>);

// SAFETY: Each logical CPU (`cpu_idx`) exclusively initializes and owns `CPU_DESCS[cpu_idx]`.
unsafe impl Sync for CpuDescArray {}

static CPU_DESCS: CpuDescArray = CpuDescArray(UnsafeCell::new([CpuDescBlock::new(); MAX_CPUS]));

#[repr(C, packed)]
struct DescriptorTablePtr {
    limit: u16,
    base: u64,
}

fn encode_tss_descriptor(tss_addr: u64) -> (u64, u64) {
    let limit = (size_of::<Tss64>() - 1) as u64;
    let low = (limit & 0xffff)
        | ((tss_addr & 0xff_ffff) << 16)
        | (0x89u64 << 40) // P=1, DPL=0, Type=0x9 (64-bit Available TSS)
        | (((limit >> 16) & 0x0f) << 48)
        | (((tss_addr >> 24) & 0xff) << 56);
    let high = tss_addr >> 32;
    (low, high)
}

/// Initialize and load GDT + TSS (`IST1` `#DF` stack + `RSP0`) on the calling CPU (`cpu_idx`).
pub fn init_cpu_gdt_tss(cpu_idx: usize) -> (u64, u64) {
    assert!(cpu_idx < MAX_CPUS, "cpu_idx {} exceeds MAX_CPUS {}", cpu_idx, MAX_CPUS);

    // SAFETY: `cpu_idx` is exclusively owned by the calling hardware thread during bring-up.
    let (gdtr, df_top, rsp0_top) = unsafe {
        let block = &mut (*CPU_DESCS.0.get())[cpu_idx];
        let df_top = (block.df_stack.0.as_ptr() as u64) + (DF_STACK_BYTES as u64);
        let rsp0_top = (block.kern_stack.0.as_ptr() as u64) + (DF_STACK_BYTES as u64);
        assert_eq!(df_top & 0xf, 0, "Unaligned IST1 stack top");
        assert_eq!(rsp0_top & 0xf, 0, "Unaligned RSP0 stack top");

        block.tss.ist[(DF_IST_INDEX - 1) as usize] = df_top;
        block.tss.rsp[0] = rsp0_top;
        block.tss.iomap_base = size_of::<Tss64>() as u16;

        let tss_addr = (&raw const block.tss) as u64;
        let (tss_lo, tss_hi) = encode_tss_descriptor(tss_addr);
        block.gdt[5] = tss_lo;
        block.gdt[6] = tss_hi;

        let gdtr = DescriptorTablePtr {
            limit: (size_of::<[u64; 7]>() - 1) as u16,
            base: block.gdt.as_ptr() as u64,
        };
        (gdtr, df_top, rsp0_top)
    };

    // SAFETY: `gdtr` points to static 16-byte-aligned per-CPU GDT with valid 64-bit CS/DS/TSS.
    unsafe {
        asm!(
            "lgdt [{gdtr}]",
            "push {kcs}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            "mov ax, {kds:x}",
            "mov ds, ax",
            "mov es, ax",
            "mov ss, ax",
            "xor eax, eax",
            "mov fs, ax",
            "mov gs, ax",
            "mov ax, {tss:x}",
            "ltr ax",
            gdtr = in(reg) &gdtr,
            tmp = lateout(reg) _,
            kcs = const KERNEL_CS as u64,
            kds = in(reg) KERNEL_DS,
            tss = in(reg) TSS_SEL,
            out("rax") _,
            options(preserves_flags),
        );
    }

    // Read back CS and TR to prove hardware accepted the descriptors.
    let loaded_cs: u16;
    let loaded_tr: u16;
    // SAFETY: Reading CS and STR at CPL=0 is side-effect free.
    unsafe {
        asm!("mov {0:x}, cs", out(reg) loaded_cs, options(nomem, nostack, preserves_flags));
        asm!("str {0:x}", out(reg) loaded_tr, options(nomem, nostack, preserves_flags));
    }
    assert_eq!(loaded_cs, KERNEL_CS, "CS reload mismatch");
    assert_eq!(loaded_tr, TSS_SEL, "TR load mismatch");

    (df_top, rsp0_top)
}
