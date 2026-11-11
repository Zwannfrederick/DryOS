// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Symmetric Multiprocessing (`SMP`) Bring-Up via Limine MP Protocol (`ADR-001` & Milestone 1A-E).
//!
//! # AP Startup Concurrency & Memory Ordering Protocol
//! 1. **BSP Pre-Publication (`Single-Writer`):**
//!    BSP initializes `IDT`, its own `GDT[0]/TSS[0]/IST1[0]` and `PerCpuData[0]`, then populates
//!    each `AP_LAUNCH_SLOTS[idx]` (`boot_stack_top`, `assigned_cpu_idx`, `claimed_cpu_idx`,
//!    `expected_lapic_id`, `expected_cr3`, `fault_mode`) and writes `mp_info.extra_argument`.
//! 2. **`Release -> Acquire` Handoff on `LimineMpInfo::goto_address`:**
//!    BSP issues `fence(Ordering::Release)` followed by `mp_info.goto_address.store(entry, Release)`.
//!    Limine's AP spinloop observes the non-zero entry and jumps to `dry_ap_trampoline(mp_info)` in
//!    64-bit Long Mode with `RFLAGS.IF = 0` (`CLI`) and identical HHDM/kernel `CR3`.
//! 3. **Immediate Stack Handoff (`dry_ap_trampoline`):**
//!    Before creating any Rust stack frame on Limine's temporary `BOOTLOADER_RECLAIMABLE` stack,
//!    assembly loads `RSP = slot.boot_stack_top` (16-byte aligned `'static` kernel `.bss` stack),
//!    clears `RBP = 0`, and calls `ap_main_rust`, which opens with `fence(Ordering::Acquire)`.
//! 4. **Per-CPU Isolation & Pairwise Disjointness Check:**
//!    Each CPU loads its own private `GDT[i]/TSS[i]` (`ltr 0x28`), `IDT`, and `IA32_GS_BASE`,
//!    verifies `gs:[0]`/`gs:[8]`, registers its `(cpu_idx, lapic_id, boot_stack, df_ist1_stack)`
//!    in `register_cpu_identity_and_stacks` (rejecting duplicate IDs or overlapping stack ranges),
//!    fires a live hardware `int3` (`#BP`) trap, and publishes readiness via `ONLINE_COUNT` (`Release`).
//! 5. **Bounded Timeout & Symmetric Rendezvous (`No Dedicated Controller CPU`):**
//!    BSP waits for `ONLINE_COUNT == cpu_count` with a bounded TSC/iteration deadline (panicking
//!    deterministically on AP timeout), then opens `RENDEZVOUS_GATE` (`Release`) so **all** CPUs
//!    (`0..cpu_count`) execute symmetric atomic work concurrently before parking cleanly.

use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::mem::offset_of;
use core::ptr::write_volatile;
use core::sync::atomic::{fence, AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::arch::{cli, halt_forever, rdtsc, read_cr3, read_rsp};
use crate::gdt_tss::{
    init_cpu_gdt_tss, prepare_cpu_boot_stack, verify_cpu_tss_and_canaries, CpuDescLayout, MAX_CPUS,
};
use crate::idt::{load_idt, total_bp_traps, verify_breakpoint_trap_recovery};
use crate::limine::{LimineMpInfo, MP_REQ};
use crate::percpu::{
    get_percpu_by_idx, init_cpu_percpu_and_gs, read_gs_u64_at_8, CpuRegionInfo, PerCpuData,
};
use crate::serial_println;

const TICKS_PER_CPU: u64 = 4096;
/// Bounded timeout (~3.0B TSC cycles ~ 1.0s on modern x86-64, capped by spin count).
const AP_TIMEOUT_TSC_DELTA: u64 = 3_000_000_000;
const AP_TIMEOUT_MAX_SPINS: u64 = 200_000_000;

#[repr(u64)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SmpFaultTest {
    None = 0,
    ApTimeout = 1,
    DupCpuId = 2,
    StackOverlap = 3,
    DfIst1OnAp1 = 4,
}

impl SmpFaultTest {
    pub fn from_cmdline(cmdline: &str) -> Self {
        if cmdline.contains("smp_fault=ap_timeout") {
            Self::ApTimeout
        } else if cmdline.contains("smp_fault=dup_cpu_id") {
            Self::DupCpuId
        } else if cmdline.contains("smp_fault=stack_overlap") {
            Self::StackOverlap
        } else if cmdline.contains("smp_fault=df_ist1_ap") {
            Self::DfIst1OnAp1
        } else {
            Self::None
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ApLaunchSlot {
    boot_stack_top: u64,
    assigned_cpu_idx: u64,
    claimed_cpu_idx: u64,
    expected_lapic_id: u32,
    _pad: u32,
    expected_cr3: u64,
    fault_mode: u64,
}

const _: () = assert!(offset_of!(ApLaunchSlot, boot_stack_top) == 0);

impl ApLaunchSlot {
    const fn zero() -> Self {
        Self {
            boot_stack_top: 0,
            assigned_cpu_idx: 0,
            claimed_cpu_idx: 0,
            expected_lapic_id: 0,
            _pad: 0,
            expected_cr3: 0,
            fault_mode: 0,
        }
    }
}

struct LaunchSlotTable(UnsafeCell<[ApLaunchSlot; MAX_CPUS]>);
// SAFETY: BSP writes `AP_LAUNCH_SLOTS[idx]` before `Release` store to `goto_address`; AP only reads.
unsafe impl Sync for LaunchSlotTable {}

static AP_LAUNCH_SLOTS: LaunchSlotTable = LaunchSlotTable(UnsafeCell::new([ApLaunchSlot::zero(); MAX_CPUS]));

#[derive(Clone, Copy)]
struct StackRecord {
    cpu_idx: usize,
    lapic_id: u32,
    boot_base: u64,
    boot_top: u64,
    df_base: u64,
    df_top: u64,
}

impl StackRecord {
    const fn empty() -> Self {
        Self { cpu_idx: 0, lapic_id: 0, boot_base: 0, boot_top: 0, df_base: 0, df_top: 0 }
    }
}

struct StackRegistry {
    records: [StackRecord; MAX_CPUS],
    count: usize,
}

struct SyncRegistry(UnsafeCell<StackRegistry>);
// SAFETY: Guarded by `REGISTRY_LOCK` (`Acquire`/`Release`) with local interrupts disabled (`CLI`).
unsafe impl Sync for SyncRegistry {}

static REGISTRY_LOCK: AtomicBool = AtomicBool::new(false);
static STACK_REGISTRY: SyncRegistry = SyncRegistry(UnsafeCell::new(StackRegistry {
    records: [StackRecord::empty(); MAX_CPUS],
    count: 0,
}));

static CLAIMED_CPU_MASK: AtomicU64 = AtomicU64::new(0);
static ONLINE_CPU_MASK: AtomicU64 = AtomicU64::new(0);
static ONLINE_COUNT: AtomicUsize = AtomicUsize::new(0);
static RENDEZVOUS_GATE: AtomicBool = AtomicBool::new(false);
static WORK_DONE_COUNT: AtomicUsize = AtomicUsize::new(0);
static GLOBAL_WORK_COUNTER: AtomicU64 = AtomicU64::new(0);
static AP1_TRIGGER_DF_GATE: AtomicBool = AtomicBool::new(false);

#[inline]
fn ranges_overlap(a_base: u64, a_top: u64, b_base: u64, b_top: u64) -> bool {
    a_base < b_top && b_base < a_top
}

/// Atomically verify unique `cpu_idx`, unique `lapic_id`, and pairwise-disjoint `boot_stack`
/// and `df_ist1` stack ranges across all online CPUs.
pub fn register_cpu_identity_and_stacks(
    claimed_cpu_idx: usize,
    lapic_id: u32,
    boot_base: u64,
    boot_top: u64,
    df_base: u64,
    df_top: u64,
) {
    assert!(claimed_cpu_idx < MAX_CPUS, "claimed_cpu_idx {} >= MAX_CPUS", claimed_cpu_idx);
    assert!(boot_base < boot_top && df_base < df_top, "Invalid stack interval bounds");

    let bit = 1u64 << claimed_cpu_idx;
    let prev_mask = CLAIMED_CPU_MASK.fetch_or(bit, Ordering::AcqRel);
    if (prev_mask & bit) != 0 {
        crate::enter_fatal_or_halt();
        crate::serial::emergency_print(format_args!(
            "\r\n[DRY-SMP-FAIL] Duplicate CPU ID detected: cpu_idx={} lapic_id={}\r\n",
            claimed_cpu_idx, lapic_id
        ));
        panic!("SMP duplicate CPU ID {}", claimed_cpu_idx);
    }

    if ranges_overlap(boot_base, boot_top, df_base, df_top) {
        crate::enter_fatal_or_halt();
        crate::serial::emergency_print(format_args!(
            "\r\n[DRY-SMP-FAIL] Stack overlap on same cpu={}: boot=[{:#x},{:#x}) df=[{:#x},{:#x})\r\n",
            claimed_cpu_idx, boot_base, boot_top, df_base, df_top
        ));
        panic!("Same-CPU boot/DF stack overlap on CPU {}", claimed_cpu_idx);
    }

    while REGISTRY_LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }

    // SAFETY: Exclusive access under `REGISTRY_LOCK` with interrupts disabled.
    let conflict = unsafe {
        let reg = &mut *STACK_REGISTRY.0.get();
        let mut err: Option<(usize, &'static str)> = None;
        for existing in reg.records.iter().take(reg.count) {
            if existing.lapic_id == lapic_id {
                err = Some((existing.cpu_idx, "duplicate_lapic_id"));
                break;
            }
            if ranges_overlap(boot_base, boot_top, existing.boot_base, existing.boot_top)
                || ranges_overlap(boot_base, boot_top, existing.df_base, existing.df_top)
                || ranges_overlap(df_base, df_top, existing.boot_base, existing.boot_top)
                || ranges_overlap(df_base, df_top, existing.df_base, existing.df_top)
            {
                err = Some((existing.cpu_idx, "stack_overlap"));
                break;
            }
        }
        if err.is_none() {
            reg.records[reg.count] = StackRecord {
                cpu_idx: claimed_cpu_idx,
                lapic_id,
                boot_base,
                boot_top,
                df_base,
                df_top,
            };
            reg.count += 1;
        }
        err
    };

    REGISTRY_LOCK.store(false, Ordering::Release);

    if let Some((other_cpu, reason)) = conflict {
        crate::enter_fatal_or_halt();
        crate::serial::emergency_print(format_args!(
            "\r\n[DRY-SMP-FAIL] Stack/LAPIC conflict ({}) between cpu={} and cpu={}\r\n",
            reason, claimed_cpu_idx, other_cpu
        ));
        panic!("SMP {} between CPU {} and CPU {}", reason, claimed_cpu_idx, other_cpu);
    }
}

global_asm!(
    ".global dry_ap_trampoline",
    ".balign 16",
    "dry_ap_trampoline:",
    "    cli",
    "    cld",
    "    mov rsi, [rdi + 24]",
    "    mov rsp, [rsi + 0]",
    "    xor ebp, ebp",
    "    call {ap_rust}",
    ap_rust = sym ap_main_rust,
);

extern "C" {
    fn dry_ap_trampoline(info: *const LimineMpInfo) -> !;
}

extern "C" fn ap_main_rust(mp_info: *const LimineMpInfo, slot_ptr: *const ApLaunchSlot) -> ! {
    cli();
    fence(Ordering::Acquire);

    // SAFETY: `mp_info` and `slot_ptr` were populated by BSP before `Release` write to `goto_address`.
    let (lapic_id, slot) = unsafe { ((*mp_info).lapic_id, *slot_ptr) };
    let cpu_idx = slot.assigned_cpu_idx as usize;

    // Negative test 1 (`smp_fault=ap_timeout`): simulate a wedged AP core before online check-in.
    if slot.fault_mode == (SmpFaultTest::ApTimeout as u64) && cpu_idx == 1 {
        halt_forever();
    }

    assert_eq!(lapic_id, slot.expected_lapic_id, "AP {} LAPIC ID mismatch", cpu_idx);
    assert_eq!(read_cr3(), slot.expected_cr3, "AP {} CR3 mismatch", cpu_idx);

    let rsp = read_rsp();
    let layout: CpuDescLayout = init_cpu_gdt_tss(cpu_idx);
    assert!(
        rsp > layout.boot_stack_base && rsp <= layout.boot_stack_top,
        "AP {} RSP {:#x} outside owned boot stack [{:#x}, {:#x}]",
        cpu_idx,
        rsp,
        layout.boot_stack_base,
        layout.boot_stack_top
    );
    load_idt();

    let pcpu: &'static PerCpuData = init_cpu_percpu_and_gs(CpuRegionInfo {
        cpu_idx,
        lapic_id,
        boot_stack_base: layout.boot_stack_base,
        boot_stack_top: layout.boot_stack_top,
        df_ist1_base: layout.df_ist1_base,
        df_ist1_top: layout.df_ist1_top,
        tss_base: layout.tss_base,
        gdt_base: layout.gdt_base,
    });

    // Negative test 3 (`smp_fault=stack_overlap`): pass aliased CPU 0 `df_ist1` bounds to verifier.
    let (reg_df_base, reg_df_top) = if slot.fault_mode == (SmpFaultTest::StackOverlap as u64) && cpu_idx == 1 {
        let cpu0 = get_percpu_by_idx(0);
        (cpu0.df_ist1_base, cpu0.df_ist1_top)
    } else {
        (layout.df_ist1_base, layout.df_ist1_top)
    };

    register_cpu_identity_and_stacks(
        slot.claimed_cpu_idx as usize,
        lapic_id,
        layout.boot_stack_base,
        layout.boot_stack_top,
        reg_df_base,
        reg_df_top,
    );

    // Prove IDT + per-CPU `GS_BASE` work under real hardware `#BP` trap on this AP.
    let _ = verify_breakpoint_trap_recovery(pcpu);

    ONLINE_CPU_MASK.fetch_or(1u64 << cpu_idx, Ordering::AcqRel);
    ONLINE_COUNT.fetch_add(1, Ordering::Release);

    // Wait for symmetric rendezvous gate from BSP.
    while !RENDEZVOUS_GATE.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }

    run_symmetric_cpu_workload(cpu_idx, pcpu);
    WORK_DONE_COUNT.fetch_add(1, Ordering::AcqRel);

    // Negative test 4 (`smp_fault=df_ist1_ap`): trigger real hardware `#PF -> #DF` on AP 1 to prove
    // AP 1's private `TSS.IST[0]` catches a corrupted-RSP double fault without triple-faulting!
    if slot.fault_mode == (SmpFaultTest::DfIst1OnAp1 as u64) && cpu_idx == 1 {
        while !AP1_TRIGGER_DF_GATE.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
        // SAFETY: Intentional unmapped RSP push to verify hardware `#PF -> #DF` switch onto `IST1`.
        unsafe {
            asm!(
                "mov rsp, {bad_rsp}",
                "push rax",
                bad_rsp = in(reg) 0x0000_7fff_dead_0008u64,
                options(noreturn),
            );
        }
    }

    halt_forever()
}

fn run_symmetric_cpu_workload(expected_cpu_idx: usize, pcpu: &'static PerCpuData) {
    for _ in 0..TICKS_PER_CPU {
        assert_eq!(read_gs_u64_at_8(), expected_cpu_idx as u64, "GS cpu_idx drift during SMP load");
        pcpu.exec_ticks.fetch_add(1, Ordering::Relaxed);
        GLOBAL_WORK_COUNTER.fetch_add(1, Ordering::AcqRel);
    }
    verify_cpu_tss_and_canaries(expected_cpu_idx);
}

pub struct SmpSummary {
    pub cpu_count: usize,
    pub online_mask: u64,
    pub total_work_ops: u64,
    pub total_bp_traps: u64,
}

/// Bring up all secondary CPUs via Limine MP, verify per-CPU isolation, and execute symmetric work.
pub fn init_and_verify_smp(bsp_pcpu: &'static PerCpuData, fault_test: SmpFaultTest) -> SmpSummary {
    let mp_resp = MP_REQ.read().response;
    assert!(!mp_resp.is_null(), "Limine MP response is null");

    // SAFETY: Limine populates `MP_REQ` response and `cpus` pointer array prior to `_start`.
    let (bsp_lapic_id, cpu_count, cpus_ptr) = unsafe {
        (
            (*mp_resp).bsp_lapic_id,
            (*mp_resp).cpu_count as usize,
            (*mp_resp).cpus,
        )
    };
    assert!((1..=MAX_CPUS).contains(&cpu_count), "Unsupported cpu_count={}", cpu_count);
    assert!(!cpus_ptr.is_null(), "Limine MP cpus array is null");

    // Register BSP (`cpu_idx = 0`) with its true hardware `bsp_lapic_id`.
    ONLINE_CPU_MASK.store(1, Ordering::Release);
    ONLINE_COUNT.store(1, Ordering::Release);
    register_cpu_identity_and_stacks(
        0,
        bsp_lapic_id,
        bsp_pcpu.boot_stack_base,
        bsp_pcpu.boot_stack_top,
        bsp_pcpu.df_ist1_base,
        bsp_pcpu.df_ist1_top,
    );

    let bsp_cr3 = read_cr3();
    let mut next_ap_idx = 1usize;

    for raw_i in 0..cpu_count {
        // SAFETY: `raw_i < cpu_count` in Limine `cpus` array.
        let mp_info_ptr = unsafe { *cpus_ptr.add(raw_i) };
        assert!(!mp_info_ptr.is_null(), "Null LimineMpInfo at index {}", raw_i);
        // SAFETY: Valid `LimineMpInfo` entry allocated by Limine.
        let lapic_id = unsafe { (*mp_info_ptr).lapic_id };
        if lapic_id == bsp_lapic_id {
            continue;
        }

        let cpu_idx = next_ap_idx;
        next_ap_idx += 1;
        let (_, boot_stack_top) = prepare_cpu_boot_stack(cpu_idx);
        let claimed_idx = if fault_test == SmpFaultTest::DupCpuId && cpu_idx == 1 {
            0 // Intentional collision with BSP (`cpu_idx = 0`) for negative test
        } else {
            cpu_idx
        };

        // SAFETY: BSP is sole writer of `AP_LAUNCH_SLOTS[cpu_idx]` and `extra_argument` before `goto_address`.
        unsafe {
            let slot_ptr = &raw mut (*AP_LAUNCH_SLOTS.0.get())[cpu_idx];
            *slot_ptr = ApLaunchSlot {
                boot_stack_top,
                assigned_cpu_idx: cpu_idx as u64,
                claimed_cpu_idx: claimed_idx as u64,
                expected_lapic_id: lapic_id,
                _pad: 0,
                expected_cr3: bsp_cr3,
                fault_mode: fault_test as u64,
            };
            write_volatile(&raw mut (*mp_info_ptr).extra_argument, slot_ptr as u64);
            fence(Ordering::Release);
            (*mp_info_ptr)
                .goto_address
                .store(dry_ap_trampoline as *const () as usize as u64, Ordering::Release);
        }
    }
    assert_eq!(next_ap_idx, cpu_count, "BSP entry missing or duplicated in Limine MP table");

    // Bounded wait for all APs to complete GDT/TSS/IST1/IDT/GS_BASE + `#BP` verification.
    let start_tsc = rdtsc();
    let mut spins = 0u64;
    while ONLINE_COUNT.load(Ordering::Acquire) < cpu_count {
        if crate::is_panicking() {
            halt_forever();
        }
        spins += 1;
        if spins >= AP_TIMEOUT_MAX_SPINS || rdtsc().wrapping_sub(start_tsc) >= AP_TIMEOUT_TSC_DELTA {
            crate::enter_fatal_or_halt();
            let expected_mask = if cpu_count == 64 { u64::MAX } else { (1u64 << cpu_count) - 1 };
            let online_mask = ONLINE_CPU_MASK.load(Ordering::Acquire);
            let missing_mask = expected_mask & !online_mask;
            crate::serial::emergency_print(format_args!(
                "\r\n[DRY-SMP-FAIL] AP timeout: online={}/{} online_mask={:#x} missing_mask={:#x}\r\n",
                ONLINE_COUNT.load(Ordering::Acquire),
                cpu_count,
                online_mask,
                missing_mask
            ));
            panic!("SMP AP bring-up timeout (missing_mask={:#x})", missing_mask);
        }
        core::hint::spin_loop();
    }

    // Release symmetric rendezvous workload across BSP + all APs simultaneously.
    RENDEZVOUS_GATE.store(true, Ordering::Release);
    run_symmetric_cpu_workload(0, bsp_pcpu);
    WORK_DONE_COUNT.fetch_add(1, Ordering::AcqRel);

    let wait_start = rdtsc();
    while WORK_DONE_COUNT.load(Ordering::Acquire) < cpu_count {
        assert!(
            rdtsc().wrapping_sub(wait_start) < AP_TIMEOUT_TSC_DELTA,
            "SMP workload rendezvous timeout"
        );
        core::hint::spin_loop();
    }

    // Verify every individual CPU's post-workload state, counters, TSS Busy bit, and stack canaries.
    let expected_mask = if cpu_count == 64 { u64::MAX } else { (1u64 << cpu_count) - 1 };
    let online_mask = ONLINE_CPU_MASK.load(Ordering::Acquire);
    assert_eq!(online_mask, expected_mask, "Online CPU bitmask mismatch");

    for idx in 0..cpu_count {
        let pcpu = get_percpu_by_idx(idx);
        assert_eq!(pcpu.cpu_idx, idx as u64);
        assert_eq!(pcpu.bp_trap_count.load(Ordering::Acquire), 1, "CPU {} #BP count != 1", idx);
        assert_eq!(
            pcpu.exec_ticks.load(Ordering::Acquire),
            TICKS_PER_CPU,
            "CPU {} exec_ticks mismatch",
            idx
        );
        verify_cpu_tss_and_canaries(idx);
    }

    let total_work_ops = GLOBAL_WORK_COUNTER.load(Ordering::Acquire);
    assert_eq!(total_work_ops, (cpu_count as u64) * TICKS_PER_CPU);
    let bp_total = total_bp_traps();
    assert_eq!(bp_total, cpu_count as u64);

    if fault_test == SmpFaultTest::DfIst1OnAp1 && cpu_count >= 2 {
        serial_println!("[DRY-SMP-INFO] Triggering intentional hardware #PF->#DF on AP 1 IST1...");
        AP1_TRIGGER_DF_GATE.store(true, Ordering::Release);
        let df_wait = rdtsc();
        while rdtsc().wrapping_sub(df_wait) < AP_TIMEOUT_TSC_DELTA {
            if crate::is_panicking() {
                halt_forever();
            }
            core::hint::spin_loop();
        }
        panic!("AP 1 #DF IST1 self-test timed out without firing");
    }

    SmpSummary {
        cpu_count,
        online_mask,
        total_work_ops,
        total_bp_traps: bp_total,
    }
}
