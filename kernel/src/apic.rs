// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Local APIC / `x2APIC` Capability Detection, Explicit Fallback Policy & PIT Calibration
//! (`Milestone 1A-G`).
//!
//! # Hardware Fallback Policy
//! 1. **`x2APIC` MSR Mode (`Preferred`):** Selected whenever `CPUID.01H:EDX[9]` (APIC) and
//!    `CPUID.01H:ECX[21]` (`x2APIC`) are present and `cmdline` does not request `apic_mode=xapic`.
//!    Uses architectural MSRs `0x800..0x83F` (`wrmsr(0x80B, 0)` EOI, zero MMIO page-walk overhead).
//! 2. **`xAPIC` MMIO Mode (`Fallback`):** Selected when `x2APIC` is absent or `apic_mode=xapic` is
//!    requested. Maps the 4 KiB Local APIC physical frame (`IA32_APIC_BASE & ~0xFFF`, default
//!    `0xFEE0_0000`) uncached (`PCD | PWT | RW | NX | P`) in HHDM before APs run.
//! 3. **`Deterministic Abort`:** Panics cleanly (`exit=35`) if no on-chip Local APIC exists;
//!    legacy 8259 PIC (`0x20/0xA0`) is remapped to `0x20..0x2F` and masked (`0xFF`) prior to `STI`.

use core::arch::asm;
use core::arch::x86_64::{__cpuid, __cpuid_count};
use core::cell::UnsafeCell;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};

use crate::arch::{inb, irq_restore, irq_save_cli, outb, rdmsr, rdtsc, read_cr3, wrmsr};
use crate::percpu::get_percpu_by_idx;

pub const VEC_PIC_SPURIOUS_MASTER: u8 = 0x27;
pub const VEC_PIC_SPURIOUS_SLAVE: u8 = 0x2f;
pub const VEC_EXT_IRQ_BASE: u8 = 0x30;
pub const EXT_IRQ_VECTOR_COUNT: usize = 16;
pub const VEC_EXT_IRQ_LAST: u8 = VEC_EXT_IRQ_BASE + (EXT_IRQ_VECTOR_COUNT as u8) - 1;
pub const VEC_PCI_MSI_BASE: u8 = 0x40;
pub const PCI_MSI_VECTOR_COUNT: usize = 32;
pub const VEC_PCI_MSI_LAST: u8 = VEC_PCI_MSI_BASE + (PCI_MSI_VECTOR_COUNT as u8) - 1;
pub const VEC_RESCHED_WAKE: u8 = 0xed;
pub const VEC_TLB_SHOOTDOWN: u8 = 0xee;
pub const VEC_LAPIC_TIMER: u8 = 0xef;
pub const VEC_APIC_SPURIOUS: u8 = 0xff;

pub const QUANTUM_TICKS: u32 = 2;

const MSR_IA32_APIC_BASE: u32 = 0x0000_001b;
const APIC_BASE_BSP: u64 = 1 << 8;
const APIC_BASE_EXTD: u64 = 1 << 10;
const APIC_BASE_EN: u64 = 1 << 11;
const PHYS_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

// x2APIC MSR addresses (`0x800 + (mmio_offset >> 4)`).
const MSR_X2APIC_ID: u32 = 0x802;
const MSR_X2APIC_EOI: u32 = 0x80b;
const MSR_X2APIC_SVR: u32 = 0x80f;
const MSR_X2APIC_ISR7: u32 = 0x817;
const MSR_X2APIC_ESR: u32 = 0x828;
const MSR_X2APIC_ICR: u32 = 0x830;
const MSR_X2APIC_LVT_TIMER: u32 = 0x832;
const MSR_X2APIC_TIMER_INIT: u32 = 0x838;
const MSR_X2APIC_TIMER_CUR: u32 = 0x839;
const MSR_X2APIC_TIMER_DIV: u32 = 0x83e;

// xAPIC MMIO register offsets (`0xFEE0_0000 + offset`).
const MMIO_APIC_ID: u64 = 0x020;
const MMIO_APIC_EOI: u64 = 0x0b0;
const MMIO_APIC_SVR: u64 = 0x0f0;
const MMIO_APIC_ISR7: u64 = 0x170;
const MMIO_APIC_ESR: u64 = 0x280;
const MMIO_ICR_LOW: u64 = 0x300;
const MMIO_ICR_HIGH: u64 = 0x310;
const MMIO_LVT_TIMER: u64 = 0x320;
const MMIO_TIMER_INIT: u64 = 0x380;
const MMIO_TIMER_CUR: u64 = 0x390;
const MMIO_TIMER_DIV: u64 = 0x3e0;

const LVT_MASKED: u32 = 1 << 16;
const ICR_LEVEL_ASSERT: u32 = 1 << 14;
const ICR_DELIV_PENDING: u32 = 1 << 12;
const SVR_ENABLE: u32 = 1 << 8;
const TIMER_DIV_BY_16: u32 = 0x3;

// i8254 PIT Channel 2 (`1,193,182 Hz` architectural reference oscillator, zero IRQ generation).
const PIT_FREQ_HZ: u64 = 1_193_182;
const PIT_5MS_TICKS: u16 = 5_966; // 5.000075 ms window
const PIT_MAX_POLL_ITERS: u64 = 50_000_000;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApicMode {
    Uninitialized = 0,
    X2ApicMsr = 1,
    XApicMmio = 2,
}

impl ApicMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uninitialized => "uninit",
            Self::X2ApicMsr => "x2apic-msr",
            Self::XApicMmio => "xapic-mmio",
        }
    }

    #[inline]
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::X2ApicMsr,
            2 => Self::XApicMmio,
            _ => Self::Uninitialized,
        }
    }
}

pub struct ApicCalibrationSummary {
    pub mode: ApicMode,
    pub samples_ticks_ms: [u64; 3],
    pub ticks_per_ms: u64,
    pub quantum_ticks: u32,
    pub quantum_us: u64,
    pub tsc_mhz: u64,
    pub cpuid_crystal_hz: u32,
    pub cpuid_base_mhz: u32,
    pub has_arat: bool,
    pub has_inv_tsc: bool,
    pub has_tsc_deadline: bool,
}

#[repr(C, align(4096))]
struct PageTablePage([u64; 512]);
struct SyncPtPage(UnsafeCell<PageTablePage>);
// SAFETY: Mutated solely on BSP with `CLI` before APs switch to `xAPIC` MMIO.
unsafe impl Sync for SyncPtPage {}

static XAPIC_PDPT: SyncPtPage = SyncPtPage(UnsafeCell::new(PageTablePage([0; 512])));
static XAPIC_PD: SyncPtPage = SyncPtPage(UnsafeCell::new(PageTablePage([0; 512])));
static XAPIC_PT: SyncPtPage = SyncPtPage(UnsafeCell::new(PageTablePage([0; 512])));

static ACTIVE_APIC_MODE: AtomicU8 = AtomicU8::new(ApicMode::Uninitialized as u8);
static APIC_PHYS_BASE: AtomicU64 = AtomicU64::new(0);
static XAPIC_MMIO_VIRT: AtomicU64 = AtomicU64::new(0);
static CALIBRATED_TICK_INIT: AtomicU32 = AtomicU32::new(0);
static ACTIVE_TICK_INIT: AtomicU32 = AtomicU32::new(0);
static SPURIOUS_APIC_IRQS: AtomicU64 = AtomicU64::new(0);
static BOOT_TSC_ORIGIN: AtomicU64 = AtomicU64::new(0);
static CALIBRATED_TSC_KHZ: AtomicU64 = AtomicU64::new(0);

#[inline]
fn io_delay() {
    // SAFETY: Port `0x80` POST diagnostic write provides a ~1us ISA bus settle delay.
    unsafe {
        outb(0x80, 0);
    }
}

/// Remap legacy 8259 Master/Slave PIC (`0x08..0x0F` -> `0x20..0x2F`) and mask all 16 ISA IRQ lines.
pub fn mask_legacy_8259_pic() {
    // SAFETY: Standard PC-AT 8259A initialization + full mask (`0xFF`) sequence with `CLI` held.
    unsafe {
        outb(0x20, 0x11);
        io_delay();
        outb(0xa0, 0x11);
        io_delay();
        outb(0x21, 0x20); // Master offset 0x20 (32..39)
        io_delay();
        outb(0xa1, 0x28); // Slave offset 0x28 (40..47)
        io_delay();
        outb(0x21, 0x04);
        io_delay();
        outb(0xa1, 0x02);
        io_delay();
        outb(0x21, 0x01);
        io_delay();
        outb(0xa1, 0x01);
        io_delay();
        outb(0x21, 0xff); // Mask all Master IRQs
        io_delay();
        outb(0xa1, 0xff); // Mask all Slave IRQs
        io_delay();
    }
}

unsafe fn ensure_xapic_mmio_mapped(
    hhdm_offset: u64,
    kernel_phys_base: u64,
    kernel_virt_base: u64,
    apic_phys: u64,
) -> u64 {
    let page_phys = apic_phys & PHYS_ADDR_MASK;
    let virt = hhdm_offset + page_phys;
    let cr3_phys = read_cr3() & PHYS_ADDR_MASK;

    let i4 = ((virt >> 39) & 0x1ff) as usize;
    let i3 = ((virt >> 30) & 0x1ff) as usize;
    let i2 = ((virt >> 21) & 0x1ff) as usize;
    let i1 = ((virt >> 12) & 0x1ff) as usize;

    let kphys_of = |ptr: *const PageTablePage| -> u64 {
        let kvirt = ptr as u64;
        assert!(kvirt >= kernel_virt_base, "Static PT page below kernel_virt_base");
        kernel_phys_base + (kvirt - kernel_virt_base)
    };

    // SAFETY: Temporarily clear `CR0.WP` on BSP (`CLI`) in case Limine marked upper page-table
    // frames read-only in HHDM; restored immediately after installing the UC MMIO leaf PTE.
    unsafe {
        let saved_cr0: u64;
        asm!("mov {}, cr0", out(reg) saved_cr0, options(nomem, nostack, preserves_flags));
        asm!("mov cr0, {}", in(reg) (saved_cr0 & !(1u64 << 16)), options(nomem, nostack, preserves_flags));

        let pml4 = (hhdm_offset + cr3_phys) as *mut u64;
        let mut e4 = read_volatile(pml4.add(i4));
        if (e4 & 1) == 0 {
            e4 = kphys_of(XAPIC_PDPT.0.get()) | 0x03;
            write_volatile(pml4.add(i4), e4);
        }

        let pdpt = (hhdm_offset + (e4 & PHYS_ADDR_MASK)) as *mut u64;
        let mut e3 = read_volatile(pdpt.add(i3));
        if (e3 & 0x81) == 0x81 {
            asm!("mov cr0, {}", in(reg) saved_cr0, options(nomem, nostack, preserves_flags));
            return virt;
        }
        if (e3 & 1) == 0 {
            e3 = kphys_of(XAPIC_PD.0.get()) | 0x03;
            write_volatile(pdpt.add(i3), e3);
        }

        let pd = (hhdm_offset + (e3 & PHYS_ADDR_MASK)) as *mut u64;
        let mut e2 = read_volatile(pd.add(i2));
        if (e2 & 0x81) == 0x81 {
            asm!("mov cr0, {}", in(reg) saved_cr0, options(nomem, nostack, preserves_flags));
            return virt;
        }
        if (e2 & 1) == 0 {
            e2 = kphys_of(XAPIC_PT.0.get()) | 0x03;
            write_volatile(pd.add(i2), e2);
        }

        let pt = (hhdm_offset + (e2 & PHYS_ADDR_MASK)) as *mut u64;
        let uc_leaf = page_phys | (1u64 << 63) | (1u64 << 4) | (1u64 << 3) | 0x03;
        write_volatile(pt.add(i1), uc_leaf);
        asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));
        asm!("mov cr0, {}", in(reg) saved_cr0, options(nomem, nostack, preserves_flags));
    }
    virt
}

#[inline]
pub fn active_apic_mode() -> ApicMode {
    ApicMode::from_u8(ACTIVE_APIC_MODE.load(Ordering::Relaxed))
}

#[inline]
fn apic_write(msr: u32, mmio_off: u64, val: u32) {
    match active_apic_mode() {
        ApicMode::X2ApicMsr => {
            // SAFETY: Valid x2APIC MSR write at CPL=0 after enabling `IA32_APIC_BASE.EXTD`.
            unsafe { wrmsr(msr, val as u64) };
        }
        ApicMode::XApicMmio => {
            let base = XAPIC_MMIO_VIRT.load(Ordering::Relaxed);
            // SAFETY: `base` is an uncached 4 KiB HHDM mapping of Local APIC MMIO space.
            unsafe { write_volatile((base + mmio_off) as *mut u32, val) };
        }
        ApicMode::Uninitialized => panic!("Local APIC accessed before initialization"),
    }
}

#[inline]
fn apic_read(msr: u32, mmio_off: u64) -> u32 {
    match active_apic_mode() {
        ApicMode::X2ApicMsr => {
            // SAFETY: Valid x2APIC MSR read at CPL=0.
            unsafe { rdmsr(msr) as u32 }
        }
        ApicMode::XApicMmio => {
            let base = XAPIC_MMIO_VIRT.load(Ordering::Relaxed);
            // SAFETY: `base` is an uncached 4 KiB HHDM mapping of Local APIC MMIO space.
            unsafe { read_volatile((base + mmio_off) as *const u32) }
        }
        ApicMode::Uninitialized => panic!("Local APIC accessed before initialization"),
    }
}

fn enable_mode_on_current_cpu(mode: ApicMode) {
    // SAFETY: Reading/updating `IA32_APIC_BASE` (`0x1B`) at CPL=0 per Intel SDM Vol. 3A 10.12.
    unsafe {
        let cur = rdmsr(MSR_IA32_APIC_BASE);
        let base_and_bsp = (cur & PHYS_ADDR_MASK) | (cur & APIC_BASE_BSP);
        match mode {
            ApicMode::X2ApicMsr => {
                let target = base_and_bsp | APIC_BASE_EN | APIC_BASE_EXTD;
                if (cur & (APIC_BASE_EN | APIC_BASE_EXTD)) != (APIC_BASE_EN | APIC_BASE_EXTD) {
                    wrmsr(MSR_IA32_APIC_BASE, target);
                }
                assert_eq!(
                    rdmsr(MSR_IA32_APIC_BASE) & (APIC_BASE_EN | APIC_BASE_EXTD),
                    APIC_BASE_EN | APIC_BASE_EXTD,
                    "Failed to enable x2APIC MSR mode"
                );
            }
            ApicMode::XApicMmio => {
                if (cur & APIC_BASE_EXTD) != 0 {
                    // Architectural x2APIC -> xAPIC transition requires clearing EN|EXTD first.
                    wrmsr(MSR_IA32_APIC_BASE, base_and_bsp);
                }
                wrmsr(MSR_IA32_APIC_BASE, base_and_bsp | APIC_BASE_EN);
                let rb = rdmsr(MSR_IA32_APIC_BASE);
                assert_eq!(
                    rb & (APIC_BASE_EN | APIC_BASE_EXTD),
                    APIC_BASE_EN,
                    "Failed to configure xAPIC MMIO mode"
                );
            }
            ApicMode::Uninitialized => unreachable!(),
        }
    }
}

fn read_hw_lapic_id() -> u32 {
    match active_apic_mode() {
        ApicMode::X2ApicMsr => apic_read(MSR_X2APIC_ID, MMIO_APIC_ID),
        ApicMode::XApicMmio => apic_read(MSR_X2APIC_ID, MMIO_APIC_ID) >> 24,
        ApicMode::Uninitialized => u32::MAX,
    }
}

/// Initialize the calling CPU's Local APIC (`SVR`, masked One-Shot `LVT_TIMER`, `DIV=16`, `EOI`).
pub fn init_cpu_lapic(cpu_idx: usize) {
    let mode = active_apic_mode();
    assert_ne!(mode, ApicMode::Uninitialized, "BSP APIC mode not selected before init_cpu_lapic");
    enable_mode_on_current_cpu(mode);

    apic_write(MSR_X2APIC_SVR, MMIO_APIC_SVR, SVR_ENABLE | (VEC_APIC_SPURIOUS as u32));
    apic_write(MSR_X2APIC_ESR, MMIO_APIC_ESR, 0);
    apic_write(MSR_X2APIC_TIMER_DIV, MMIO_TIMER_DIV, TIMER_DIV_BY_16);
    apic_write(
        MSR_X2APIC_LVT_TIMER,
        MMIO_LVT_TIMER,
        LVT_MASKED | (VEC_LAPIC_TIMER as u32),
    );
    apic_write(MSR_X2APIC_TIMER_INIT, MMIO_TIMER_INIT, 0);
    lapic_eoi();

    let hw_id = read_hw_lapic_id();
    let expected_id = get_percpu_by_idx(cpu_idx).lapic_id;
    assert_eq!(
        hw_id, expected_id,
        "Hardware Local APIC ID {} != Limine MP lapic_id {} on CPU {}",
        hw_id, expected_id, cpu_idx
    );
}

#[inline]
pub fn lapic_eoi() {
    apic_write(MSR_X2APIC_EOI, MMIO_APIC_EOI, 0);
}

#[inline]
pub fn bsp_apic_msi_dest_id() -> u32 {
    get_percpu_by_idx(0).lapic_id
}

#[inline]
pub fn apic_phys_base() -> u64 {
    APIC_PHYS_BASE.load(Ordering::Acquire)
}

/// Return `(tsc_mhz, uptime_ms, elapsed_tsc)` measured monotonically from `BOOT_TSC_ORIGIN`.
#[inline]
pub fn uptime_telemetry() -> (u64, u64, u64) {
    let origin = BOOT_TSC_ORIGIN.load(Ordering::Acquire);
    let khz = CALIBRATED_TSC_KHZ.load(Ordering::Acquire).max(1_000);
    let elapsed = if origin == 0 {
        0
    } else {
        rdtsc().saturating_sub(origin)
    };
    (khz / 1_000, elapsed / khz, elapsed)
}

/// Send an edge-triggered Fixed Inter-Processor Interrupt (`vector >= 32`) to `dest_lapic_id`
/// using the active `x2APIC` MSR (`0x830`) or `xAPIC` MMIO (`0x310`/`0x300`) transport.
pub fn send_fixed_ipi(dest_lapic_id: u32, vector: u8) {
    debug_assert!(vector >= 32, "Fixed IPI vector must be >= 32");
    let low_cmd = ICR_LEVEL_ASSERT | (vector as u32);
    match active_apic_mode() {
        ApicMode::X2ApicMsr => {
            let icr = ((dest_lapic_id as u64) << 32) | (low_cmd as u64);
            // SAFETY: Valid 64-bit `IA32_X2APIC_ICR` (`0x830`) write at CPL=0.
            unsafe { wrmsr(MSR_X2APIC_ICR, icr) };
        }
        ApicMode::XApicMmio => {
            let base = XAPIC_MMIO_VIRT.load(Ordering::Relaxed);
            // Mask local IRQs across the two-part `ICR_HIGH` + `ICR_LOW` MMIO write.
            let saved_if = irq_save_cli();
            // SAFETY: `base` maps the 4 KiB Local APIC MMIO page uncached (`PCD | PWT`).
            unsafe {
                while (read_volatile((base + MMIO_ICR_LOW) as *const u32) & ICR_DELIV_PENDING) != 0 {
                    core::hint::spin_loop();
                }
                write_volatile((base + MMIO_ICR_HIGH) as *mut u32, dest_lapic_id << 24);
                write_volatile((base + MMIO_ICR_LOW) as *mut u32, low_cmd);
                while (read_volatile((base + MMIO_ICR_LOW) as *const u32) & ICR_DELIV_PENDING) != 0 {
                    core::hint::spin_loop();
                }
            }
            irq_restore(saved_if);
        }
        ApicMode::Uninitialized => panic!("send_fixed_ipi before Local APIC init"),
    }
}

#[inline]
pub fn arm_cpu_oneshot_tick(init_count: u32) {
    apic_write(MSR_X2APIC_TIMER_DIV, MMIO_TIMER_DIV, TIMER_DIV_BY_16);
    apic_write(MSR_X2APIC_LVT_TIMER, MMIO_LVT_TIMER, VEC_LAPIC_TIMER as u32);
    apic_write(MSR_X2APIC_TIMER_INIT, MMIO_TIMER_INIT, init_count.max(1));
}

#[inline]
pub fn arm_default_oneshot_tick() {
    arm_cpu_oneshot_tick(ACTIVE_TICK_INIT.load(Ordering::Relaxed));
}

/// Fast single-register One-Shot re-arm (`TIMER_INIT` only) when `TIMER_DIV` (`16`) and unmasked
/// `LVT_TIMER` (`VEC_LAPIC_TIMER`) are already configured on the calling CPU (`Intel SDM Vol. 3A §10.5.4`).
#[inline]
pub fn rearm_oneshot_tick_count_only() {
    let init_count = ACTIVE_TICK_INIT.load(Ordering::Relaxed).max(1);
    apic_write(MSR_X2APIC_TIMER_INIT, MMIO_TIMER_INIT, init_count);
}

#[inline]
pub fn set_active_tick_init_override(init_count: u32) {
    ACTIVE_TICK_INIT.store(init_count.max(1), Ordering::Release);
}

#[inline]
pub fn restore_calibrated_tick_init() {
    ACTIVE_TICK_INIT.store(CALIBRATED_TICK_INIT.load(Ordering::Acquire), Ordering::Release);
}

#[inline]
pub fn stop_cpu_lapic_timer() {
    apic_write(
        MSR_X2APIC_LVT_TIMER,
        MMIO_LVT_TIMER,
        LVT_MASKED | (VEC_LAPIC_TIMER as u32),
    );
    apic_write(MSR_X2APIC_TIMER_INIT, MMIO_TIMER_INIT, 0);
}

#[inline]
pub fn is_timer_isr_bit_set() -> bool {
    let isr7 = apic_read(MSR_X2APIC_ISR7, MMIO_APIC_ISR7);
    (isr7 & (1u32 << (VEC_LAPIC_TIMER & 31))) != 0
}

#[inline]
pub fn record_spurious_apic_irq() {
    SPURIOUS_APIC_IRQS.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn spurious_apic_irqs() -> u64 {
    SPURIOUS_APIC_IRQS.load(Ordering::Acquire)
}

fn measure_one_5ms_pit_ch2_sample() -> (u64, u64) {
    apic_write(MSR_X2APIC_TIMER_DIV, MMIO_TIMER_DIV, TIMER_DIV_BY_16);
    apic_write(
        MSR_X2APIC_LVT_TIMER,
        MMIO_LVT_TIMER,
        LVT_MASKED | (VEC_LAPIC_TIMER as u32),
    );

    // SAFETY: Polls standard i8254 PIT Channel 2 (`0x61`, `0x43`, `0x42`) with speaker muted.
    let (apic_cur, tsc_delta) = unsafe {
        let orig_61 = inb(0x61);
        outb(0x61, orig_61 & !0x03); // GATE2=0, SPKR=0
        outb(0x43, 0xb0); // Ch2, lobyte/hibyte, Mode 0 (terminal count), binary
        outb(0x42, (PIT_5MS_TICKS & 0xff) as u8);
        outb(0x42, (PIT_5MS_TICKS >> 8) as u8);

        apic_write(MSR_X2APIC_TIMER_INIT, MMIO_TIMER_INIT, u32::MAX);
        let t0 = rdtsc();
        // Arm `TIMER_INIT` and `t0` BEFORE raising `GATE2=1` (`0x61` PIO VM-exit): host vCPU
        // preemption on `outb(0x61)` then cannot elapse PIT wall-clock time prior to `t0`.
        outb(0x61, (orig_61 & !0x02) | 0x01); // Raise GATE2=1 (starts Ch2 countdown)

        let mut polls = 0u64;
        while (inb(0x61) & 0x20) == 0 {
            polls += 1;
            assert!(polls < PIT_MAX_POLL_ITERS, "PIT Channel 2 terminal-count calibration timeout");
            core::hint::spin_loop();
        }

        let cur = apic_read(MSR_X2APIC_TIMER_CUR, MMIO_TIMER_CUR);
        let t1 = rdtsc();
        apic_write(MSR_X2APIC_TIMER_INIT, MMIO_TIMER_INIT, 0);
        outb(0x61, orig_61 & !0x03);
        (cur, t1.wrapping_sub(t0))
    };

    let apic_elapsed = u32::MAX.wrapping_sub(apic_cur) as u64;
    let ticks_per_ms = (apic_elapsed * PIT_FREQ_HZ) / ((PIT_5MS_TICKS as u64) * 1_000);
    let tsc_khz = (tsc_delta * PIT_FREQ_HZ) / ((PIT_5MS_TICKS as u64) * 1_000);
    (ticks_per_ms, tsc_khz)
}

/// Detect APIC/`x2APIC` capabilities, enforce fallback policy, initialize BSP Local APIC,
/// and calibrate the Local APIC timer over 3 PIT Channel 2 hardware reference windows.
pub fn detect_and_init_bsp_apic(
    hhdm_offset: u64,
    kernel_phys_base: u64,
    kernel_virt_base: u64,
    force_xapic: bool,
) -> ApicCalibrationSummary {
    mask_legacy_8259_pic();

    let (has_apic, has_x2apic, has_tsc_deadline, has_arat, has_inv_tsc, crystal_hz, base_mhz) = {
        let leaf0 = __cpuid(0);
        let leaf1 = __cpuid(1);
        let apic = (leaf1.edx & (1 << 9)) != 0;
        let x2apic = (leaf1.ecx & (1 << 21)) != 0;
        let tsc_dl = (leaf1.ecx & (1 << 24)) != 0;
        let arat = if leaf0.eax >= 6 {
            (__cpuid(6).eax & (1 << 2)) != 0
        } else {
            false
        };
        let ext0 = __cpuid(0x8000_0000);
        let inv_tsc = if ext0.eax >= 0x8000_0007 {
            (__cpuid(0x8000_0007).edx & (1 << 8)) != 0
        } else {
            false
        };
        let crystal = if leaf0.eax >= 0x15 {
            __cpuid_count(0x15, 0).ecx
        } else {
            0
        };
        let b_mhz = if leaf0.eax >= 0x16 {
            __cpuid_count(0x16, 0).eax & 0xffff
        } else {
            0
        };
        (apic, x2apic, tsc_dl, arat, inv_tsc, crystal, b_mhz)
    };

    if !has_apic {
        crate::enter_fatal_or_halt();
        crate::serial::emergency_print(format_args!(
            "\r\n[DRY-APIC-FAIL] CPUID.01H:EDX[9] == 0: Local APIC absent\r\n"
        ));
        panic!("Local APIC hardware absent");
    }

    // SAFETY: `IA32_APIC_BASE` is architecturally present when `CPUID.01H:EDX[9] == 1`.
    let apic_base_msr = unsafe { rdmsr(MSR_IA32_APIC_BASE) };
    let apic_phys = apic_base_msr & PHYS_ADDR_MASK;
    assert_ne!(apic_phys, 0, "IA32_APIC_BASE physical base is zero");
    APIC_PHYS_BASE.store(apic_phys, Ordering::Release);

    let chosen_mode = if !force_xapic && has_x2apic {
        ApicMode::X2ApicMsr
    } else {
        // SAFETY: Installs uncached HHDM leaf PTE for `apic_phys` on BSP before APs use xAPIC MMIO.
        let mmio_virt =
            unsafe { ensure_xapic_mmio_mapped(hhdm_offset, kernel_phys_base, kernel_virt_base, apic_phys) };
        XAPIC_MMIO_VIRT.store(mmio_virt, Ordering::Release);
        ApicMode::XApicMmio
    };

    ACTIVE_APIC_MODE.store(chosen_mode as u8, Ordering::Release);
    init_cpu_lapic(0);

    let (s0, k0) = measure_one_5ms_pit_ch2_sample();
    let (s1, k1) = measure_one_5ms_pit_ch2_sample();
    let (s2, k2) = measure_one_5ms_pit_ch2_sample();

    let mut sorted_ticks = [s0, s1, s2];
    sorted_ticks.sort_unstable();
    let mut sorted_tsc = [k0, k1, k2];
    sorted_tsc.sort_unstable();

    // PIT Channel 2 Mode 0 `OUT2` cannot assert early (< 5.000 ms); host vCPU preemption only
    // adds positive post-terminal-count read latency, so the minimum of 3 windows is tightest.
    let best_ticks_ms = sorted_ticks[0];
    let best_tsc_khz = sorted_tsc[0];
    assert!(
        (100..=50_000_000).contains(&best_ticks_ms),
        "Calibrated Local APIC ticks_per_ms={} outside sane hardware bounds",
        best_ticks_ms
    );

    let tick_init = (best_ticks_ms.clamp(1_000, 50_000_000)) as u32;
    CALIBRATED_TICK_INIT.store(tick_init, Ordering::Release);
    ACTIVE_TICK_INIT.store(tick_init, Ordering::Release);
    BOOT_TSC_ORIGIN.store(rdtsc(), Ordering::Release);
    CALIBRATED_TSC_KHZ.store(best_tsc_khz.max(1_000), Ordering::Release);

    ApicCalibrationSummary {
        mode: chosen_mode,
        samples_ticks_ms: [s0, s1, s2],
        ticks_per_ms: best_ticks_ms,
        quantum_ticks: QUANTUM_TICKS,
        quantum_us: (QUANTUM_TICKS as u64) * 1_000,
        tsc_mhz: best_tsc_khz / 1_000,
        cpuid_crystal_hz: crystal_hz,
        cpuid_base_mhz: base_mhz,
        has_arat,
        has_inv_tsc,
        has_tsc_deadline,
    }
}
