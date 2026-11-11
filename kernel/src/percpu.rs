// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Cache-line-isolated Per-CPU State (`PerCpuData`) & `IA32_GS_BASE` Hardware Binding.
//!
//! Each logical CPU owns a dedicated 192-byte (3 cache lines, `align(64)`) slot in `'static`
//! kernel `.bss`. Offset `0` stores `self_ptr` and offset `8` stores `cpu_idx`, allowing single-
//! instruction `mov reg, gs:[0]` / `mov reg, gs:[8]` lookup without locks or atomics.

use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::{offset_of, size_of};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::arch::{rdmsr, wrmsr, MSR_IA32_FS_BASE, MSR_IA32_GS_BASE, MSR_IA32_KERNEL_GS_BASE};
use crate::gdt_tss::MAX_CPUS;

const PERCPU_MAGIC: u64 = 0x4452_595f_5043_5055; // "DRY_PCPU"

pub const PCPU_OFF_KERNEL_RSP0: usize = 192;
pub const PCPU_OFF_USER_RSP_SCRATCH: usize = 200;

#[repr(C, align(64))]
pub struct PerCpuData {
    /// Offset 0: Canonical virtual address of this `PerCpuData` slot (`gs:[0]`).
    pub self_ptr: u64,
    /// Offset 8: Logical DRY CPU index `0..cpu_count` (`gs:[8]`).
    pub cpu_idx: u64,
    /// Offset 16: Hardware Local APIC ID (`gs:[16]`).
    pub lapic_id: u32,
    /// Offset 20: Reserved alignment pad.
    pub _pad0: u32,
    /// Offset 24: Validation magic (`PERCPU_MAGIC`) proving `GS_BASE` is initialized.
    pub magic: u64,
    /// Offset 32..64: Owned stack & descriptor bounds for hardware exception / `#DF` checks.
    pub boot_stack_base: u64,
    pub boot_stack_top: u64,
    pub df_ist1_base: u64,
    pub df_ist1_top: u64,
    /// Second 64-byte cache line (`64..128`): per-CPU counters, active thread slot & latches.
    pub tss_base: u64,
    pub gdt_base: u64,
    pub bp_trap_count: AtomicU64,
    pub last_bp_rip: AtomicU64,
    pub exec_ticks: AtomicU64,
    pub current_slot: AtomicU64,
    pub ctx_switches: AtomicU64,
    pub in_df_handler: AtomicBool,
    pub need_resched: AtomicBool,
    pub timer_armed: AtomicBool,
    pub timer_needs_rearm: AtomicBool,
    pub _pad1: [u8; 4],
    /// Third 64-byte cache line (`128..192`): Local APIC timer IRQ, quantum & preemption state.
    pub timer_irqs: AtomicU64,
    pub preemptions: AtomicU64,
    pub deferred_rescheds: AtomicU64,
    pub last_progress_seq: AtomicU64,
    pub preempt_count: AtomicU32,
    pub irq_depth: AtomicU32,
    pub quantum_rem: AtomicU32,
    pub storm_streak: AtomicU32,
    pub storm_slot: AtomicU64,
    pub nmi_count: AtomicU64,
    /// Fourth 64-byte cache line (`192..256`): Ring 3 `SYSCALL` stack scratch, `FS`/`GS` & local counters.
    pub kernel_rsp0: AtomicU64,
    pub user_rsp_scratch: AtomicU64,
    pub cur_fs_base: AtomicU64,
    pub cur_user_gs_base: AtomicU64,
    pub cr3_noops_local: AtomicU64,
    pub cr3_switches_local: AtomicU64,
    pub syscall_count: AtomicU64,
    pub user_fault_count: AtomicU64,
}

const _: () = assert!(offset_of!(PerCpuData, self_ptr) == 0);
const _: () = assert!(offset_of!(PerCpuData, cpu_idx) == 8);
const _: () = assert!(offset_of!(PerCpuData, lapic_id) == 16);
const _: () = assert!(offset_of!(PerCpuData, magic) == 24);
const _: () = assert!(offset_of!(PerCpuData, timer_irqs) == 128);
const _: () = assert!(offset_of!(PerCpuData, kernel_rsp0) == PCPU_OFF_KERNEL_RSP0);
const _: () = assert!(offset_of!(PerCpuData, user_rsp_scratch) == PCPU_OFF_USER_RSP_SCRATCH);
const _: () = assert!(size_of::<PerCpuData>() == 256);

impl PerCpuData {
    const fn uninit() -> Self {
        Self {
            self_ptr: 0,
            cpu_idx: u64::MAX,
            lapic_id: u32::MAX,
            _pad0: 0,
            magic: 0,
            boot_stack_base: 0,
            boot_stack_top: 0,
            df_ist1_base: 0,
            df_ist1_top: 0,
            tss_base: 0,
            gdt_base: 0,
            bp_trap_count: AtomicU64::new(0),
            last_bp_rip: AtomicU64::new(0),
            exec_ticks: AtomicU64::new(0),
            current_slot: AtomicU64::new(u64::MAX),
            ctx_switches: AtomicU64::new(0),
            in_df_handler: AtomicBool::new(false),
            need_resched: AtomicBool::new(false),
            timer_armed: AtomicBool::new(false),
            timer_needs_rearm: AtomicBool::new(false),
            _pad1: [0; 4],
            timer_irqs: AtomicU64::new(0),
            preemptions: AtomicU64::new(0),
            deferred_rescheds: AtomicU64::new(0),
            last_progress_seq: AtomicU64::new(0),
            preempt_count: AtomicU32::new(0),
            irq_depth: AtomicU32::new(0),
            quantum_rem: AtomicU32::new(0),
            storm_streak: AtomicU32::new(0),
            storm_slot: AtomicU64::new(u64::MAX),
            nmi_count: AtomicU64::new(0),
            kernel_rsp0: AtomicU64::new(0),
            user_rsp_scratch: AtomicU64::new(0),
            cur_fs_base: AtomicU64::new(0),
            cur_user_gs_base: AtomicU64::new(0),
            cr3_noops_local: AtomicU64::new(0),
            cr3_switches_local: AtomicU64::new(0),
            syscall_count: AtomicU64::new(0),
            user_fault_count: AtomicU64::new(0),
        }
    }
}

struct PerCpuTable(UnsafeCell<[PerCpuData; MAX_CPUS]>);

// SAFETY: Slot `cpu_idx` is written only by the owning logical CPU during its own bring-up;
// cross-CPU inspection after bring-up reads immutable fields or `Atomic*` fields.
unsafe impl Sync for PerCpuTable {}

#[allow(clippy::declare_interior_mutable_const)]
const UNINIT_PERCPU: PerCpuData = PerCpuData::uninit();
static PER_CPU_TABLE: PerCpuTable = PerCpuTable(UnsafeCell::new([UNINIT_PERCPU; MAX_CPUS]));

pub struct CpuRegionInfo {
    pub cpu_idx: usize,
    pub lapic_id: u32,
    pub boot_stack_base: u64,
    pub boot_stack_top: u64,
    pub df_ist1_base: u64,
    pub df_ist1_top: u64,
    pub tss_base: u64,
    pub gdt_base: u64,
}

/// Populate `PER_CPU_TABLE[cpu_idx]` and bind `IA32_GS_BASE` + `IA32_KERNEL_GS_BASE`.
///
/// MUST be called **after** `init_cpu_gdt_tss(cpu_idx)` because `mov gs, ax` inside GDT reload
/// clears the CPU's hidden `GS.base` register on x86-64.
pub fn init_cpu_percpu_and_gs(info: CpuRegionInfo) -> &'static PerCpuData {
    assert!(info.cpu_idx < MAX_CPUS, "cpu_idx {} >= MAX_CPUS", info.cpu_idx);

    // SAFETY: Calling CPU exclusively owns `PER_CPU_TABLE[info.cpu_idx]` during initialization.
    let slot_ref: &'static PerCpuData = unsafe {
        let slot = &mut (*PER_CPU_TABLE.0.get())[info.cpu_idx];
        let ptr_val = (slot as *const PerCpuData) as u64;
        slot.self_ptr = ptr_val;
        slot.cpu_idx = info.cpu_idx as u64;
        slot.lapic_id = info.lapic_id;
        slot.boot_stack_base = info.boot_stack_base;
        slot.boot_stack_top = info.boot_stack_top;
        slot.df_ist1_base = info.df_ist1_base;
        slot.df_ist1_top = info.df_ist1_top;
        slot.tss_base = info.tss_base;
        slot.gdt_base = info.gdt_base;
        slot.bp_trap_count.store(0, Ordering::Release);
        slot.last_bp_rip.store(0, Ordering::Release);
        slot.exec_ticks.store(0, Ordering::Release);
        slot.current_slot.store(info.cpu_idx as u64, Ordering::Release);
        slot.ctx_switches.store(0, Ordering::Release);
        slot.in_df_handler.store(false, Ordering::Release);
        slot.need_resched.store(false, Ordering::Release);
        slot.timer_armed.store(false, Ordering::Release);
        slot.timer_needs_rearm.store(false, Ordering::Release);
        slot.timer_irqs.store(0, Ordering::Release);
        slot.preemptions.store(0, Ordering::Release);
        slot.deferred_rescheds.store(0, Ordering::Release);
        slot.last_progress_seq.store(0, Ordering::Release);
        slot.preempt_count.store(0, Ordering::Release);
        slot.irq_depth.store(0, Ordering::Release);
        slot.quantum_rem.store(0, Ordering::Release);
        slot.storm_streak.store(0, Ordering::Release);
        slot.storm_slot.store(u64::MAX, Ordering::Release);
        slot.nmi_count.store(0, Ordering::Release);
        slot.kernel_rsp0.store(info.boot_stack_top, Ordering::Release);
        slot.user_rsp_scratch.store(0, Ordering::Release);
        slot.cur_fs_base.store(0, Ordering::Release);
        slot.cur_user_gs_base.store(0, Ordering::Release);
        slot.cr3_noops_local.store(0, Ordering::Release);
        slot.cr3_switches_local.store(0, Ordering::Release);
        slot.syscall_count.store(0, Ordering::Release);
        slot.user_fault_count.store(0, Ordering::Release);
        slot.magic = PERCPU_MAGIC;

        // Ring 0 GS invariant: `IA32_GS_BASE` points to `&PerCpuData`; `IA32_KERNEL_GS_BASE`
        // holds the active thread's user `GS` shadow (`0` initially), swapped on CPL=0 <-> CPL=3.
        wrmsr(MSR_IA32_FS_BASE, 0);
        wrmsr(MSR_IA32_GS_BASE, ptr_val);
        wrmsr(MSR_IA32_KERNEL_GS_BASE, 0);
        assert_eq!(rdmsr(MSR_IA32_GS_BASE), ptr_val, "IA32_GS_BASE readback mismatch");
        assert_eq!(rdmsr(MSR_IA32_KERNEL_GS_BASE), 0, "IA32_KERNEL_GS_BASE readback mismatch");
        &*(slot as *const PerCpuData)
    };

    // Hardware GS segment-override verification (`gs:[0]`, `gs:[8]`, `gs:[24]`).
    let gs_self = read_gs_u64_at_0();
    let gs_cpu = read_gs_u64_at_8();
    let gs_magic = read_gs_u64_at_24();
    assert_eq!(gs_self, slot_ref.self_ptr, "gs:[0] self_ptr mismatch");
    assert_eq!(gs_cpu, info.cpu_idx as u64, "gs:[8] cpu_idx mismatch");
    assert_eq!(gs_magic, PERCPU_MAGIC, "gs:[24] magic mismatch");

    slot_ref
}

#[inline]
pub fn read_gs_u64_at_0() -> u64 {
    let val: u64;
    // SAFETY: Caller guarantees `IA32_GS_BASE` points to a valid `PerCpuData` (checked via MSR).
    unsafe {
        asm!("mov {}, gs:[0]", out(reg) val, options(nomem, nostack, preserves_flags));
    }
    val
}

#[inline]
pub fn read_gs_u64_at_8() -> u64 {
    let val: u64;
    // SAFETY: Offset 8 in `PerCpuData` is `cpu_idx: u64`.
    unsafe {
        asm!("mov {}, gs:[8]", out(reg) val, options(nomem, nostack, preserves_flags));
    }
    val
}

#[inline]
fn read_gs_u64_at_24() -> u64 {
    let val: u64;
    // SAFETY: Offset 24 in `PerCpuData` is `magic: u64`.
    unsafe {
        asm!("mov {}, gs:[24]", out(reg) val, options(nomem, nostack, preserves_flags));
    }
    val
}

/// Safe `GS_BASE` lookup safe to invoke from early exception handlers even before `GS` is set.
pub fn try_current_percpu() -> Option<&'static PerCpuData> {
    // SAFETY: Reading `IA32_GS_BASE` at CPL=0 is always valid; if 0, `GS` is not yet initialized.
    let base = unsafe { rdmsr(MSR_IA32_GS_BASE) };
    if base < 0xffff_ffff_8000_0000 || (base & 0x3f) != 0 {
        return None;
    }
    // SAFETY: `base` is canonical higher-half 64-byte-aligned address written by `init_cpu_percpu_and_gs`.
    let pcpu = unsafe { &*(base as *const PerCpuData) };
    if pcpu.magic != PERCPU_MAGIC || pcpu.self_ptr != base {
        return None;
    }
    Some(pcpu)
}

/// Fast 1-instruction `gs:[0]` lookup for runtime paths after `init_cpu_percpu_and_gs`.
#[inline]
pub fn current_percpu() -> &'static PerCpuData {
    let ptr = read_gs_u64_at_0() as *const PerCpuData;
    // SAFETY: `gs:[0]` holds the `'static` address of this CPU's `PerCpuData` slot.
    unsafe { &*ptr }
}

pub fn get_percpu_by_idx(cpu_idx: usize) -> &'static PerCpuData {
    assert!(cpu_idx < MAX_CPUS);
    // SAFETY: Static table lives for the entire kernel lifetime.
    unsafe { &(*PER_CPU_TABLE.0.get())[cpu_idx] }
}
