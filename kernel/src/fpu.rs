// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Per-Thread FPU/SIMD Context Isolation (`Milestone 2A-4a` Contract B, `0` new syscalls).
//!
//! The kernel is built soft-float and never touches x87/SSE/AVX registers, so FPU state is purely a
//! Ring 3 resource. Design (minimum overhead for the SIMD-free native services):
//! 1. **First-use trap:** every CPU runs with `CR0.TS = 1` unless the current thread already owns
//!    FPU state. The first x87/SSE/AVX instruction of a thread raises `#NM`; the handler loads a
//!    deterministic clean state (`FCW = 0x037F`, `MXCSR = 0x1F80`, all registers zero) and marks
//!    the thread `USED`. A thread can therefore never observe another thread's registers.
//! 2. **Eager switch for users of the FPU:** a `USED` thread is saved on every switch-out
//!    (`XSAVEOPT64` / `XSAVE64` / `FXSAVE64`) and restored on switch-in unless this CPU still holds
//!    exactly its registers (`OWNER[cpu] == slot && LAST_CPU[slot] == cpu`). No cross-CPU lazy
//!    state exists, so migration needs no IPI.
//! 3. **Feature detection per host:** `XCR0` is limited to `x87 | SSE | AVX` intersected with
//!    `CPUID.(EAX=0DH,ECX=0)`; if `XSAVE` is absent or the enabled area would exceed
//!    `FPU_AREA_BYTES`, the kernel falls back to `FXSAVE64` and leaves `CR4.OSXSAVE = 0` (AVX is
//!    then invisible to Ring 3). AVX-512 state is deliberately not enabled (`#UD` in Ring 3).

use core::arch::asm;
use core::arch::x86_64::{__cpuid, __cpuid_count};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

use crate::arch::{read_cr0, read_cr4, write_cr0, write_cr4};
use crate::gdt_tss::MAX_CPUS;
use crate::sched::WORKER_POOL_CAP;

pub const FPU_AREA_BYTES: usize = 1024;

const CR0_MP: u64 = 1 << 1;
const CR0_EM: u64 = 1 << 2;
const CR0_TS: u64 = 1 << 3;
const CR0_NE: u64 = 1 << 5;
const CR4_OSFXSR: u64 = 1 << 9;
const CR4_OSXMMEXCPT: u64 = 1 << 10;
const CR4_OSXSAVE: u64 = 1 << 18;

const XCR0_X87: u64 = 1 << 0;
const XCR0_SSE: u64 = 1 << 1;
const XCR0_AVX: u64 = 1 << 2;

const MODE_UNINIT: u8 = 0;
const MODE_FXSAVE: u8 = 1;
const MODE_XSAVE: u8 = 2;
const MODE_XSAVEOPT: u8 = 3;

const NO_SLOT: u32 = u32::MAX;
const NO_CPU16: u16 = u16::MAX;

const FXSAVE_OFF_FCW: usize = 0;
const FXSAVE_OFF_MXCSR: usize = 24;
const CLEAN_FCW: u16 = 0x037f;
const CLEAN_MXCSR: u32 = 0x1f80;

#[repr(C, align(64))]
struct FpuArea(UnsafeCell<[u8; FPU_AREA_BYTES]>);

// SAFETY: Area `i` is touched only by the CPU that currently owns worker slot `MAX_CPUS + i`'s
// stack (`CLI` held during save/restore), or by `on_device_not_available` on that same thread.
unsafe impl Sync for FpuArea {}

static AREAS: [FpuArea; WORKER_POOL_CAP] =
    [const { FpuArea(UnsafeCell::new([0; FPU_AREA_BYTES])) }; WORKER_POOL_CAP];
static USED: [AtomicBool; WORKER_POOL_CAP] = [const { AtomicBool::new(false) }; WORKER_POOL_CAP];
static LAST_CPU: [AtomicU16; WORKER_POOL_CAP] =
    [const { AtomicU16::new(NO_CPU16) }; WORKER_POOL_CAP];
static OWNER: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(NO_SLOT) }; MAX_CPUS];
static TS_SET: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

static MODE: AtomicU8 = AtomicU8::new(MODE_UNINIT);
static XCR0_MASK: AtomicU64 = AtomicU64::new(0);
static AREA_USED_BYTES: AtomicU32 = AtomicU32::new(0);
static FORCE_FXSAVE: AtomicBool = AtomicBool::new(false);
static INIT_CPU_MASK: AtomicU64 = AtomicU64::new(0);
static NM_TRAPS: AtomicU64 = AtomicU64::new(0);

/// Test hook (`fpu_mode=fxsave`): must be called on the BSP before the first `init_on_current_cpu`.
pub fn force_fxsave_fallback() {
    FORCE_FXSAVE.store(true, Ordering::Release);
}

#[inline]
fn worker_idx(slot: usize) -> Option<usize> {
    slot.checked_sub(MAX_CPUS).filter(|&i| i < WORKER_POOL_CAP)
}

#[inline]
fn set_ts(cpu: usize, want: bool) {
    if TS_SET[cpu].load(Ordering::Relaxed) == want {
        return;
    }
    // SAFETY: Toggling only `CR0.TS` at CPL=0 under `CLI`; the kernel itself executes no FPU code.
    unsafe {
        if want {
            write_cr0(read_cr0() | CR0_TS);
        } else {
            asm!("clts", options(nomem, nostack, preserves_flags));
        }
    }
    TS_SET[cpu].store(want, Ordering::Relaxed);
}

/// Decide the save format from CPUID; returns `(mode, xcr0, area_bytes)`.
fn detect_mode() -> (u8, u64, u32) {
    let l1 = __cpuid(1);
    assert!(
        (l1.edx & (1 << 24)) != 0 && (l1.edx & (1 << 25)) != 0,
        "x86-64 CPU without FXSR/SSE"
    );
    let has_xsave = (l1.ecx & (1 << 26)) != 0 && __cpuid(0).eax >= 0xd;
    if !has_xsave || FORCE_FXSAVE.load(Ordering::Acquire) {
        return (MODE_FXSAVE, XCR0_X87 | XCR0_SSE, 512);
    }
    let d0 = __cpuid_count(0xd, 0);
    let supported = (d0.eax as u64) | ((d0.edx as u64) << 32);
    let xcr0 = supported & (XCR0_X87 | XCR0_SSE | XCR0_AVX);
    if (xcr0 & (XCR0_X87 | XCR0_SSE)) != (XCR0_X87 | XCR0_SSE) {
        return (MODE_FXSAVE, XCR0_X87 | XCR0_SSE, 512);
    }
    // Standard-format size: legacy 512 + header 64 + AVX component end (CPUID.0DH.2).
    let size = if (xcr0 & XCR0_AVX) != 0 {
        let avx = __cpuid_count(0xd, 2);
        (avx.ebx + avx.eax).max(576)
    } else {
        576
    };
    if (size as usize) > FPU_AREA_BYTES {
        return (MODE_FXSAVE, XCR0_X87 | XCR0_SSE, 512);
    }
    let opt = (__cpuid_count(0xd, 1).eax & 1) != 0;
    (if opt { MODE_XSAVEOPT } else { MODE_XSAVE }, xcr0, size)
}

/// Enable FPU/SSE (and `XSAVE`/AVX when available) on the calling CPU and arm the first-use trap.
pub fn init_on_current_cpu(cpu_idx: usize) {
    let (mode, xcr0, size) = detect_mode();
    match MODE.compare_exchange(MODE_UNINIT, mode, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {
            XCR0_MASK.store(xcr0, Ordering::Release);
            AREA_USED_BYTES.store(size, Ordering::Release);
        }
        Err(prev) => assert!(
            prev == mode && XCR0_MASK.load(Ordering::Acquire) == xcr0,
            "Asymmetric FPU features on cpu={}: mode {} vs {}",
            cpu_idx,
            mode,
            prev
        ),
    }
    // SAFETY: CPUID-verified control-register bits; `xsetbv` only with `CR4.OSXSAVE = 1`.
    unsafe {
        write_cr0((read_cr0() & !(CR0_EM | CR0_TS)) | CR0_MP | CR0_NE);
        let mut cr4 = read_cr4() | CR4_OSFXSR | CR4_OSXMMEXCPT;
        if mode == MODE_FXSAVE {
            cr4 &= !CR4_OSXSAVE;
        } else {
            cr4 |= CR4_OSXSAVE;
        }
        write_cr4(cr4);
        if mode != MODE_FXSAVE {
            asm!(
                "xsetbv",
                in("ecx") 0u32,
                in("eax") xcr0 as u32,
                in("edx") (xcr0 >> 32) as u32,
                options(nomem, nostack, preserves_flags)
            );
        }
        asm!("fninit", options(nomem, nostack, preserves_flags));
        write_cr0(read_cr0() | CR0_TS);
    }
    OWNER[cpu_idx].store(NO_SLOT, Ordering::Relaxed);
    TS_SET[cpu_idx].store(true, Ordering::Relaxed);
    INIT_CPU_MASK.fetch_or(1u64 << cpu_idx, Ordering::AcqRel);
}

/// `(mode_name, xcr0, area_bytes, initialized_cpus, nm_traps)` for boot telemetry.
pub fn telemetry() -> (&'static str, u64, u32, u32, u64) {
    let name = match MODE.load(Ordering::Acquire) {
        MODE_FXSAVE => "fxsave",
        MODE_XSAVE => "xsave",
        MODE_XSAVEOPT => "xsaveopt",
        _ => "uninit",
    };
    (
        name,
        XCR0_MASK.load(Ordering::Acquire),
        AREA_USED_BYTES.load(Ordering::Acquire),
        INIT_CPU_MASK.load(Ordering::Acquire).count_ones(),
        NM_TRAPS.load(Ordering::Relaxed),
    )
}

/// Forget any FPU state of a freshly allocated worker slot (called while the slot is `Dead`).
#[inline]
pub fn reset_slot(slot: usize) {
    if let Some(i) = worker_idx(slot) {
        USED[i].store(false, Ordering::Release);
        LAST_CPU[i].store(NO_CPU16, Ordering::Release);
        let s = slot as u32;
        for owner in &OWNER {
            let _ = owner.compare_exchange(s, NO_SLOT, Ordering::AcqRel, Ordering::Acquire);
        }
    }
}

#[inline]
unsafe fn save_area(ptr: *mut u8) {
    // SAFETY: Caller guarantees `CR0.TS == 0`, `CLI`, and exclusive 64-byte-aligned `ptr`.
    unsafe {
        match MODE.load(Ordering::Acquire) {
            MODE_XSAVEOPT => asm!("xsaveopt64 [{p}]", p = in(reg) ptr,
                in("eax") u32::MAX, in("edx") u32::MAX, options(nostack, preserves_flags)),
            MODE_XSAVE => asm!("xsave64 [{p}]", p = in(reg) ptr,
                in("eax") u32::MAX, in("edx") u32::MAX, options(nostack, preserves_flags)),
            _ => asm!("fxsave64 [{p}]", p = in(reg) ptr, options(nostack, preserves_flags)),
        }
    }
}

#[inline]
unsafe fn restore_area(ptr: *const u8) {
    // SAFETY: Caller guarantees `CR0.TS == 0`, `CLI`, and a kernel-written valid image at `ptr`.
    unsafe {
        if MODE.load(Ordering::Acquire) == MODE_FXSAVE {
            asm!("fxrstor64 [{p}]", p = in(reg) ptr, options(nostack, preserves_flags));
        } else {
            asm!("xrstor64 [{p}]", p = in(reg) ptr,
                in("eax") u32::MAX, in("edx") u32::MAX, options(nostack, preserves_flags));
        }
    }
}

/// Context-switch hook: runs on `next`'s stack with `CLI`, before `prev`'s stack is released.
#[inline]
pub fn on_context_switch(cpu: usize, prev_slot: usize, next_slot: usize) {
    if let Some(p) = worker_idx(prev_slot) {
        if USED[p].load(Ordering::Acquire) {
            // SAFETY: `prev` ran with `CR0.TS == 0` on this CPU and still owns its area.
            unsafe { save_area(AREAS[p].0.get().cast()) };
        }
    }
    match worker_idx(next_slot) {
        Some(n) if USED[n].load(Ordering::Acquire) => {
            set_ts(cpu, false);
            if OWNER[cpu].load(Ordering::Acquire) != next_slot as u32
                || LAST_CPU[n].load(Ordering::Acquire) != cpu as u16
            {
                // SAFETY: `next` is owned by this CPU; its area holds a complete saved image.
                unsafe { restore_area(AREAS[n].0.get().cast()) };
                OWNER[cpu].store(next_slot as u32, Ordering::Release);
                LAST_CPU[n].store(cpu as u16, Ordering::Release);
            }
        }
        _ => set_ts(cpu, true),
    }
}

/// `#NM` from `CPL = 3`: give the current thread a clean private FPU state. Returns `false` if the
/// trap cannot be a legitimate first use (kernel bug or non-worker context).
pub fn on_device_not_available(cpu: usize, cur_slot: usize) -> bool {
    let Some(i) = worker_idx(cur_slot) else {
        return false;
    };
    if USED[i].load(Ordering::Acquire) || MODE.load(Ordering::Acquire) == MODE_UNINIT {
        return false;
    }
    set_ts(cpu, false);
    // SAFETY: Interrupt gate (`IF = 0`) on the current thread; the area is exclusively ours.
    unsafe {
        let area = &mut *AREAS[i].0.get();
        area.fill(0);
        area[FXSAVE_OFF_FCW..FXSAVE_OFF_FCW + 2].copy_from_slice(&CLEAN_FCW.to_le_bytes());
        area[FXSAVE_OFF_MXCSR..FXSAVE_OFF_MXCSR + 4].copy_from_slice(&CLEAN_MXCSR.to_le_bytes());
        restore_area(area.as_ptr());
    }
    USED[i].store(true, Ordering::Release);
    OWNER[cpu].store(cur_slot as u32, Ordering::Release);
    LAST_CPU[i].store(cpu as u16, Ordering::Release);
    NM_TRAPS.fetch_add(1, Ordering::Relaxed);
    true
}

/// Boot self-test: `procs` concurrent `dry-fpu-test.elf` instances with distinct register patterns
/// must each start from a clean state and never observe a foreign or lost value under preemptive
/// SMP scheduling (`exit 0`), with zero leaked frames.
pub fn verify_fpu_isolation(online_cpus: usize) {
    use crate::pmm::{flush_all_magazines_quiescent, measure_fragmentation};
    use crate::proc::{reclaim_process, spawn_elf_process, ProcessOutcome, ReclaimStatus, SpawnOptions};
    use crate::sched::preempt_tests::{begin_preemptive_epoch, drain_preemptive_epoch_and_audit};
    use crate::security::{encode_compartment, SecDomain, SecFlags, SecLabel};

    const PROCS: usize = 6;
    const ROUNDS: u64 = 48;
    let elf = crate::limine::get_boot_module("dry-fpu-test.elf")
        .expect("Missing Limine module dry-fpu-test.elf");
    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);
    let traps_before = NM_TRAPS.load(Ordering::Relaxed);

    crate::sched::set_preemptive_mode_active(true);
    crate::sched::init_scheduler(online_cpus);
    begin_preemptive_epoch(0xf1, PROCS);
    let mut handles = [None; PROCS];
    for (i, h) in handles.iter_mut().enumerate() {
        let lbl = SecLabel::new(
            SecDomain::Userspace,
            SecFlags::NONE,
            encode_compartment(1 + i as u8, 1),
            1,
        );
        let info = spawn_elf_process(
            elf,
            SpawnOptions {
                subj_label: lbl,
                arg0: 0x5eed_0001 + i as u64,
                arg1: ROUNDS,
                target_cpu: i % online_cpus,
                trace_lifecycle: false,
                user_fs_base: 0,
                user_gs_base: 0,
            },
        )
        .expect("spawn dry-fpu-test failed");
        *h = Some(info.handle);
    }
    drain_preemptive_epoch_and_audit(online_cpus);
    crate::sched::set_preemptive_mode_active(false);

    for (i, h) in handles.iter().enumerate() {
        let out = reclaim_process(h.expect("handle")).expect("reclaim dry-fpu-test");
        assert_eq!(
            out,
            ReclaimStatus::ReclaimedNow(ProcessOutcome::Exited(0)),
            "dry-fpu-test instance {} detected FPU/SIMD state corruption or leak",
            i
        );
    }
    flush_all_magazines_quiescent(online_cpus);
    let after = measure_fragmentation(online_cpus);
    let leaked = before.free_global_frames.saturating_sub(after.free_global_frames);
    assert_eq!(leaked, 0, "Frame leak after FPU isolation test: {}", leaked);

    let (mode, xcr0, area, cpus, traps) = telemetry();
    assert_eq!(cpus as usize, online_cpus, "FPU not initialized on every CPU");
    assert_eq!(traps - traps_before, PROCS as u64, "Unexpected #NM first-use trap count");
    crate::serial_println!(
        "[DRY-FPU] PASS: cpus={} mode={} xcr0={:#x} area_bytes={} procs={} rounds={} nm_first_use={} corrupt=0 leaked_frames={}",
        online_cpus, mode, xcr0, area, PROCS, ROUNDS, traps - traps_before, leaked
    );
}
