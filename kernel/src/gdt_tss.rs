// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Per-CPU 64-bit GDT, TSS, Boot Stack, and `#DF` `IST1` Stack Ownership (`x86-64`).
//!
//! # Architectural Lifetime & `#DF` `IST1` Ownership Analysis
//! 1. **TSS Lifetime & Hardware `Busy` Bit (`0x89 -> 0x8B`):**
//!    Executing `ltr` mutates the GDT TSS descriptor in memory to set `Type = 0xB` (`Busy`) and
//!    caches `&CPU_DESCS[cpu_idx].tss` inside the CPU's hidden `TR.base` register. The hardware
//!    dereferences `TR.base` directly on every Ring 3 -> Ring 0 trap (`RSP0`) and every `IST`-
//!    gated fault (`IST1`) for the entire uptime of that CPU. Therefore:
//!    - Every CPU **must** own a private GDT and private TSS (sharing a TSS across CPUs triggers
//!      `#GP(0x28)` on the second CPU's `ltr` and corrupts `RSP0`/`IST` across cores).
//!    - `CPU_DESCS` **must** reside in permanently mapped, non-reclaimable `'static` kernel `.bss`
//!      (`SecDomain::Kernel`), never on a stack or bootloader-reclaimable frame.
//! 2. **`#DF` (`IST1`) Exclusive Ownership & Non-Reentrancy:**
//!    When IDT vector 8 (`#DF`) specifies `IST = 1`, hardware unconditionally sets `RSP = TSS.IST[0]`
//!    on entry. Sharing `IST1` with recoverable exceptions (`#PF`, `#GP`, `#BP`) or IRQs would cause
//!    a nested fault to clobber the active `TrapFrame` at `df_top`. Hence `IST1` is bound **solely**
//!    to terminal `#DF` (vector 8), guarded by a bottom canary (`DF_CANARY_MAGIC ^ cpu_idx`) and a
//!    per-CPU atomic `in_df_handler` re-entrancy latch.

use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::size_of;
use core::ptr::{read_volatile, write_volatile};

pub const KERNEL_CS: u16 = 0x08;
pub const KERNEL_DS: u16 = 0x10;
pub const USER_DS: u16 = 0x18 | 3; // 0x1b (`GDT[3]` User Data, `RPL = 3`)
pub const USER_CS: u16 = 0x20 | 3; // 0x23 (`GDT[4]` User 64-bit Code, `RPL = 3`)
pub const TSS_SEL: u16 = 0x28;
pub const DF_IST_INDEX: u8 = 1;
pub const NMI_IST_INDEX: u8 = 2;
pub const MC_IST_INDEX: u8 = 3;

pub const PER_CPU_STACK_BYTES: usize = 16 * 1024;
pub const PER_CPU_AUX_IST_BYTES: usize = 8 * 1024;
pub const PER_CPU_BOOT_STACK_BYTES: usize = 32 * 1024;
pub const MAX_CPUS: usize = 64;

const BOOT_CANARY_MAGIC: u64 = 0x4452_595f_4254_5354; // "DRY_BTST"
const DF_CANARY_MAGIC: u64 = 0x4452_595f_4446_4953;   // "DRY_DFIS"
const NMI_CANARY_MAGIC: u64 = 0x4452_595f_4e4d_4953;  // "DRY_NMIS"
const MC_CANARY_MAGIC: u64 = 0x4452_595f_4d43_4953;   // "DRY_MCIS"

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct AlignedStack([u8; PER_CPU_STACK_BYTES]);

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct AuxIstStack([u8; PER_CPU_AUX_IST_BYTES]);

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct BootAlignedStack([u8; PER_CPU_BOOT_STACK_BYTES]);

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

#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct CpuDescBlock {
    gdt: [u64; 7],
    tss: Tss64,
    df_stack: AlignedStack,
    nmi_stack: AuxIstStack,
    mc_stack: AuxIstStack,
    boot_stack: BootAlignedStack,
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
            df_stack: AlignedStack([0; PER_CPU_STACK_BYTES]),
            nmi_stack: AuxIstStack([0; PER_CPU_AUX_IST_BYTES]),
            mc_stack: AuxIstStack([0; PER_CPU_AUX_IST_BYTES]),
            boot_stack: BootAlignedStack([0; PER_CPU_BOOT_STACK_BYTES]),
        }
    }
}

struct CpuDescArray(UnsafeCell<[CpuDescBlock; MAX_CPUS]>);

// SAFETY: Each logical CPU (`cpu_idx`) exclusively mutates `CPU_DESCS[cpu_idx]`.
unsafe impl Sync for CpuDescArray {}

static CPU_DESCS: CpuDescArray = CpuDescArray(UnsafeCell::new([CpuDescBlock::new(); MAX_CPUS]));

#[repr(C, packed)]
struct DescriptorTablePtr {
    limit: u16,
    base: u64,
}

pub struct CpuDescLayout {
    pub boot_stack_base: u64,
    pub boot_stack_top: u64,
    pub df_ist1_base: u64,
    pub df_ist1_top: u64,
    pub tss_base: u64,
    pub gdt_base: u64,
}

fn encode_tss_descriptor(tss_addr: u64) -> (u64, u64) {
    let limit = (size_of::<Tss64>() - 1) as u64;
    let low = (limit & 0xffff)
        | ((tss_addr & 0xff_ffff) << 16)
        | (0x89u64 << 40) // P=1, DPL=0, Type=0x9 (64-bit Available TSS; `ltr` flips to 0x8B Busy)
        | (((limit >> 16) & 0x0f) << 48)
        | (((tss_addr >> 24) & 0xff) << 56);
    let high = tss_addr >> 32;
    (low, high)
}

/// Stamp bottom canaries and return permanent `'static` boot stack `[base, top)` for `cpu_idx`
/// BEFORE the CPU switches its `RSP` onto `boot_stack_top`.
pub fn prepare_cpu_boot_stack(cpu_idx: usize) -> (u64, u64) {
    assert!(cpu_idx < MAX_CPUS, "cpu_idx {} >= MAX_CPUS", cpu_idx);
    // SAFETY: Called once per `cpu_idx` before `cpu_idx` begins executing on `boot_stack`.
    unsafe {
        let block = &mut (*CPU_DESCS.0.get())[cpu_idx];
        let boot_base = block.boot_stack.0.as_mut_ptr() as u64;
        let boot_top = boot_base + (PER_CPU_BOOT_STACK_BYTES as u64);
        let df_base = block.df_stack.0.as_mut_ptr() as u64;
        let nmi_base = block.nmi_stack.0.as_mut_ptr() as u64;
        let mc_base = block.mc_stack.0.as_mut_ptr() as u64;
        let mask = cpu_idx as u64;
        write_volatile(boot_base as *mut u64, BOOT_CANARY_MAGIC ^ mask);
        write_volatile(df_base as *mut u64, DF_CANARY_MAGIC ^ mask);
        write_volatile(nmi_base as *mut u64, NMI_CANARY_MAGIC ^ mask);
        write_volatile(mc_base as *mut u64, MC_CANARY_MAGIC ^ mask);
        assert_eq!(boot_top & 0xf, 0, "Unaligned boot_stack_top");
        (boot_base, boot_top)
    }
}

/// Initialize and load private GDT + private TSS (`IST1` `#DF` stack + `RSP0`) on calling CPU.
pub fn init_cpu_gdt_tss(cpu_idx: usize) -> CpuDescLayout {
    assert!(cpu_idx < MAX_CPUS, "cpu_idx {} exceeds MAX_CPUS {}", cpu_idx, MAX_CPUS);

    // SAFETY: `cpu_idx` is exclusively owned by the calling hardware thread during bring-up.
    let (gdtr, layout) = unsafe {
        let block = &mut (*CPU_DESCS.0.get())[cpu_idx];
        let boot_base = block.boot_stack.0.as_ptr() as u64;
        let boot_top = boot_base + (PER_CPU_BOOT_STACK_BYTES as u64);
        let df_base = block.df_stack.0.as_ptr() as u64;
        let df_top = df_base + (PER_CPU_STACK_BYTES as u64);
        let nmi_top = (block.nmi_stack.0.as_ptr() as u64) + (PER_CPU_AUX_IST_BYTES as u64);
        let mc_top = (block.mc_stack.0.as_ptr() as u64) + (PER_CPU_AUX_IST_BYTES as u64);
        assert_eq!(df_top & 0xf, 0, "Unaligned IST1 stack top");
        assert_eq!(nmi_top & 0xf, 0, "Unaligned IST2 (NMI) stack top");
        assert_eq!(mc_top & 0xf, 0, "Unaligned IST3 (#MC) stack top");
        assert_eq!(boot_top & 0xf, 0, "Unaligned RSP0 stack top");

        block.tss.ist[(DF_IST_INDEX - 1) as usize] = df_top;
        block.tss.ist[(NMI_IST_INDEX - 1) as usize] = nmi_top;
        block.tss.ist[(MC_IST_INDEX - 1) as usize] = mc_top;
        block.tss.rsp[0] = boot_top;
        block.tss.iomap_base = size_of::<Tss64>() as u16;

        let tss_addr = (&raw const block.tss) as u64;
        let gdt_addr = block.gdt.as_ptr() as u64;
        let (tss_lo, tss_hi) = encode_tss_descriptor(tss_addr);
        block.gdt[5] = tss_lo;
        block.gdt[6] = tss_hi;

        let gdtr = DescriptorTablePtr {
            limit: (size_of::<[u64; 7]>() - 1) as u16,
            base: gdt_addr,
        };
        (
            gdtr,
            CpuDescLayout {
                boot_stack_base: boot_base,
                boot_stack_top: boot_top,
                df_ist1_base: df_base,
                df_ist1_top: df_top,
                tss_base: tss_addr,
                gdt_base: gdt_addr,
            },
        )
    };

    // SAFETY: `gdtr` points to static 64-byte-aligned per-CPU GDT with valid 64-bit CS/DS/TSS.
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

    verify_cpu_tss_and_canaries(cpu_idx);
    layout
}

/// Verify hardware `TR == 0x28`, private GDT TSS descriptor `Busy` bit (`0x8B`), and stack canaries.
pub fn verify_cpu_tss_and_canaries(cpu_idx: usize) {
    assert!(cpu_idx < MAX_CPUS);
    let loaded_cs: u16;
    let loaded_tr: u16;
    // SAFETY: Reading `CS` and `STR` at CPL=0 is side-effect free.
    unsafe {
        asm!("mov {0:x}, cs", out(reg) loaded_cs, options(nomem, nostack, preserves_flags));
        asm!("str {0:x}", out(reg) loaded_tr, options(nomem, nostack, preserves_flags));
    }
    assert_eq!(loaded_cs, KERNEL_CS, "CPU {} CS reload mismatch", cpu_idx);
    assert_eq!(loaded_tr, TSS_SEL, "CPU {} TR load mismatch", cpu_idx);

    assert!(verify_df_canary_only(cpu_idx), "CPU {} DF IST1 stack bottom canary corrupted", cpu_idx);

    // SAFETY: Reads immutable/CPU-local GDT[5] and boot stack bottom canary in `'static` `.bss`.
    unsafe {
        let block = &(*CPU_DESCS.0.get())[cpu_idx];
        let gdt_tss_low = read_volatile(&raw const block.gdt[5]);
        let access_byte = ((gdt_tss_low >> 40) & 0xff) as u8;
        // Hardware `ltr` MUST transition TSS type from 0x89 (Available) to 0x8B (Busy).
        assert_eq!(
            access_byte, 0x8b,
            "CPU {} TSS GDT access byte {:#x} != 0x8b (Busy)",
            cpu_idx, access_byte
        );
        let boot_canary = read_volatile(block.boot_stack.0.as_ptr().cast::<u64>());
        let mask = cpu_idx as u64;
        assert_eq!(
            boot_canary,
            BOOT_CANARY_MAGIC ^ mask,
            "CPU {} boot stack bottom canary corrupted",
            cpu_idx
        );
        let nmi_canary = read_volatile(block.nmi_stack.0.as_ptr().cast::<u64>());
        assert_eq!(
            nmi_canary,
            NMI_CANARY_MAGIC ^ mask,
            "CPU {} NMI IST2 stack bottom canary corrupted",
            cpu_idx
        );
        let mc_canary = read_volatile(block.mc_stack.0.as_ptr().cast::<u64>());
        assert_eq!(
            mc_canary,
            MC_CANARY_MAGIC ^ mask,
            "CPU {} #MC IST3 stack bottom canary corrupted",
            cpu_idx
        );
        let ist = block.tss.ist;
        assert!(
            ist[0] != 0 && ist[1] != 0 && ist[2] != 0
                && ist[0] != ist[1] && ist[0] != ist[2] && ist[1] != ist[2],
            "CPU {} IST1..3 stacks must be non-zero and pairwise disjoint",
            cpu_idx
        );
    }
}

/// Return hardware `TSS.IST[0..3]` tops `(df_ist1_top, nmi_ist2_top, mc_ist3_top)` for `cpu_idx`.
pub fn cpu_ist_tops(cpu_idx: usize) -> (u64, u64, u64) {
    assert!(cpu_idx < MAX_CPUS);
    // SAFETY: `CPU_DESCS[cpu_idx].tss.ist[0..3]` are initialized once at bring-up in `'static` `.bss`.
    let ist = unsafe { (*CPU_DESCS.0.get())[cpu_idx].tss.ist };
    (ist[0], ist[1], ist[2])
}

/// Update `TSS.RSP0` and `PerCpuData.kernel_rsp0` (`gs:[192]`) for `cpu_idx` when switching to a
/// thread with kernel stack top `rsp0`.
#[inline]
pub fn set_cpu_tss_rsp0(cpu_idx: usize, rsp0: u64) {
    debug_assert!(cpu_idx < MAX_CPUS && (rsp0 & 0xf) == 0);
    // SAFETY: `cpu_idx` is the calling CPU (`IF = 0`); `TSS.RSP0` is CPU-private in `'static` `.bss`.
    unsafe {
        let block = &mut (*CPU_DESCS.0.get())[cpu_idx];
        write_volatile(&raw mut block.tss.rsp[0], rsp0);
    }
    crate::percpu::get_percpu_by_idx(cpu_idx)
        .kernel_rsp0
        .store(rsp0, core::sync::atomic::Ordering::Relaxed);
}

#[cold]
#[inline(never)]
fn cpu_stack_canary_panic(cpu_idx: usize, kind: &'static str) -> ! {
    panic!("CPU {} {} stack bottom canary corrupted", cpu_idx, kind);
}

/// Fast hot-path boot + `#DF` (`IST1`) stack bottom canary verification without `STR` VM-exit.
#[inline]
pub fn verify_cpu_stack_canaries_fast(cpu_idx: usize) {
    let pcpu = crate::percpu::get_percpu_by_idx(cpu_idx);
    let mask = cpu_idx as u64;
    // SAFETY: `boot_stack_base` and `df_ist1_base` point to 8-byte bottom canaries in `.bss`.
    unsafe {
        if read_volatile(pcpu.boot_stack_base as *const u64) != (BOOT_CANARY_MAGIC ^ mask) {
            cpu_stack_canary_panic(cpu_idx, "boot");
        }
        if read_volatile(pcpu.df_ist1_base as *const u64) != (DF_CANARY_MAGIC ^ mask) {
            cpu_stack_canary_panic(cpu_idx, "DF IST1");
        }
    }
}

/// Lock-free `#DF` stack bottom canary check safe to invoke from inside `#DF` (`IST1`) handler.
pub fn verify_df_canary_only(cpu_idx: usize) -> bool {
    if cpu_idx >= MAX_CPUS {
        return false;
    }
    // SAFETY: Reads the 8-byte canary at the bottom of `CPU_DESCS[cpu_idx].df_stack`.
    unsafe {
        let block = &(*CPU_DESCS.0.get())[cpu_idx];
        let df_canary = read_volatile(block.df_stack.0.as_ptr().cast::<u64>());
        df_canary == (DF_CANARY_MAGIC ^ (cpu_idx as u64))
    }
}
