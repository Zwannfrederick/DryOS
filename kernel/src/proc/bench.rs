// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `Milestone 1B-D` Stage G: Ring 3 Transition, Syscall, Context-Switch, `uaccess`, Process
//! Lifecycle, and SMP Contention Microbenchmarks (`p50 / p95 / p99` cycles across `1..20` vCPUs).

use core::arch::global_asm;
use core::ptr::{copy_nonoverlapping, read_volatile, write_volatile};
use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::rdtsc;
use crate::gdt_tss::USER_CS;
use crate::pmm::{
    alloc_pages, flush_all_magazines_quiescent, free_pages, measure_fragmentation, AllocFlags,
    PageOrder, PAGE_SIZE,
};
use crate::proc::{
    activate_raw_bench_pid_slot, reclaim_process, release_raw_bench_pid_slot, spawn_elf_process,
    ProcessHandle, ProcessOutcome, ReclaimStatus, SpawnOptions, MAX_PROCESSES,
};
use crate::sched::preempt_tests::{begin_preemptive_epoch, drain_preemptive_epoch_and_audit};
use crate::sched::{
    current_cpu_idx, current_slot_on_cpu, exit_current_thread, idle_dispatch_once, init_scheduler,
    set_preemptive_mode_active, tcb, try_spawn_thread_bound, try_spawn_thread_in_vspace, yield_now,
};
use crate::security::{encode_compartment, SecDomain, SecFlags, SecLabel};
use crate::serial_println;
use crate::syscall::entry::{enter_user_iretq_unchecked, DRY_BENCH_STAMP_ENTRY, USER_RFLAGS_FIXED_SET};
use crate::syscall::set_force_syscall_iretq;
use crate::uaccess::{copy_from_user, copy_to_user};
use crate::vspace::pte::VmProt;
use crate::vspace::{
    cr3_perf_counters, create_vspace, destroy_vspace, map_4k, reset_cr3_perf_counters,
    switch_cpu_vspace_if_needed, unmap_4k, VSpaceHandle, KERNEL_VSPACE_ID,
};

const SAMPLES_FAST: usize = 256;
const SAMPLES_PROC_LIFE: usize = 32;
const BENCH_CODE_VA: u64 = 0x0080_0000;
const BENCH_DATA_VA: u64 = 0x0080_1000;
const BENCH_USTACK_VA: u64 = 0x0080_2000;
const BENCH_USTACK_RSP: u64 = BENCH_USTACK_VA + 0xff0;

static K2K_HANDOFF_TSC: AtomicU64 = AtomicU64::new(0);
static mut K2K_SAMPLES: [u64; SAMPLES_FAST] = [0; SAMPLES_FAST];
static K2K_SAMPLE_IDX: AtomicU64 = AtomicU64::new(0);

// Position-independent CPL=3 microbenchmark stubs copied into a 4 KiB `USER_RX` page (`0x0080_0000`).
global_asm!(
    ".balign 16",
    ".global __dry_cpl3_bench_stubs_start",
    ".global __dry_cpl3_bench_r0_to_r3",
    ".global __dry_cpl3_bench_syscall_loop",
    ".global __dry_cpl3_bench_u2u_pingpong",
    ".global __dry_cpl3_bench_stubs_end",
    "__dry_cpl3_bench_stubs_start:",
    // Stub 1 (`rdi = data_va`): capture post-`iretq` CPL=3 `rdtsc` immediately and `SYS_EXIT(0)`.
    "__dry_cpl3_bench_r0_to_r3:",
    "    lfence",
    "    rdtsc",
    "    shl rdx, 32",
    "    or rax, rdx",
    "    mov qword ptr [rdi], rax",
    "    mov eax, 3",
    "    xor edi, edi",
    "    syscall",
    "    ud2",
    ".balign 16",
    // Stub 2 (`rdi = data_va`, `rsi = (kind << 32) | count`):
    // `kind == 0`: `SYS_ABI_PING(0)` round-trip (`t2 - t0`)
    // `kind == 1`: `SYS_ABI_PING(1)` `r3 -> r0` entry (`rdx - t0`)
    // `kind == 2`: `SYS_YIELD` latency (`t2 - t0`)
    "__dry_cpl3_bench_syscall_loop:",
    "    mov r12, rdi",
    "    mov r13d, esi",
    "    mov r14, rsi",
    "    shr r14, 32",
    "    xor r15d, r15d",
    "1:",
    "    cmp r15, r13",
    "    jae 5f",
    "    lfence",
    "    rdtsc",
    "    shl rdx, 32",
    "    or rax, rdx",
    "    mov rbx, rax",
    "    cmp r14, 1",
    "    je 2f",
    "    cmp r14, 2",
    "    je 3f",
    "    xor eax, eax",
    "    xor edi, edi",
    "    syscall",
    "    lfence",
    "    rdtsc",
    "    shl rdx, 32",
    "    or rax, rdx",
    "    sub rax, rbx",
    "    jmp 4f",
    "2:",
    "    xor eax, eax",
    "    mov edi, 1",
    "    syscall",
    "    mov rax, rdx",
    "    sub rax, rbx",
    "    jmp 4f",
    "3:",
    "    mov eax, 2",
    "    syscall",
    "    lfence",
    "    rdtsc",
    "    shl rdx, 32",
    "    or rax, rdx",
    "    sub rax, rbx",
    "4:",
    "    mov qword ptr [r12 + r15 * 8], rax",
    "    add r15, 1",
    "    jmp 1b",
    "5:",
    "    mov eax, 3",
    "    xor edi, edi",
    "    syscall",
    "    ud2",
    ".balign 16",
    // Stub 3 (`rdi = shared_data_va`, `rsi = role (0 or 1)`):
    // `[r12 + 0]` = `handoff_tsc`, `[r12 + 8]` = `sample_idx`, `[r12 + 16..]` = `u64` samples.
    "__dry_cpl3_bench_u2u_pingpong:",
    "    mov r12, rdi",
    "    mov r13, rsi",
    "    mov r14, 128",
    "    test r13, r13",
    "    jnz 11f",
    "10:",
    "    lfence",
    "    rdtsc",
    "    shl rdx, 32",
    "    or rax, rdx",
    "    mov qword ptr [r12], rax",
    "    mov eax, 2",
    "    syscall",
    "    lfence",
    "    rdtsc",
    "    shl rdx, 32",
    "    or rax, rdx",
    "    mov rcx, qword ptr [r12]",
    "    test rcx, rcx",
    "    jz 12f",
    "    sub rax, rcx",
    "    mov rdx, qword ptr [r12 + 8]",
    "    cmp rdx, 256",
    "    jae 12f",
    "    mov qword ptr [r12 + 16 + rdx * 8], rax",
    "    add rdx, 1",
    "    mov qword ptr [r12 + 8], rdx",
    "12:",
    "    sub r14, 1",
    "    jnz 10b",
    "    jmp 15f",
    "11:",
    "    lfence",
    "    rdtsc",
    "    shl rdx, 32",
    "    or rax, rdx",
    "    mov rcx, qword ptr [r12]",
    "    test rcx, rcx",
    "    jz 13f",
    "    sub rax, rcx",
    "    mov rdx, qword ptr [r12 + 8]",
    "    cmp rdx, 256",
    "    jae 13f",
    "    mov qword ptr [r12 + 16 + rdx * 8], rax",
    "    add rdx, 1",
    "    mov qword ptr [r12 + 8], rdx",
    "13:",
    "    lfence",
    "    rdtsc",
    "    shl rdx, 32",
    "    or rax, rdx",
    "    mov qword ptr [r12], rax",
    "    mov eax, 2",
    "    syscall",
    "    sub r14, 1",
    "    jnz 11b",
    "15:",
    "    mov qword ptr [r12], 0",
    "    mov eax, 3",
    "    xor edi, edi",
    "    syscall",
    "    ud2",
    "__dry_cpl3_bench_stubs_end:",
);

extern "C" {
    fn __dry_cpl3_bench_stubs_start();
    fn __dry_cpl3_bench_r0_to_r3();
    fn __dry_cpl3_bench_syscall_loop();
    fn __dry_cpl3_bench_u2u_pingpong();
    fn __dry_cpl3_bench_stubs_end();
}

#[derive(Clone, Copy)]
struct Pct {
    p50: u64,
    p95: u64,
    p99: u64,
}

fn compute_percentiles(samples: &mut [u64]) -> Pct {
    let n = samples.len();
    assert!(n > 0);
    // Insertion sort on bounded array (`<= 256` elements, zero heap).
    for i in 1..n {
        let key = samples[i];
        let mut j = i;
        while j > 0 && samples[j - 1] > key {
            samples[j] = samples[j - 1];
            j -= 1;
        }
        samples[j] = key;
    }
    Pct {
        p50: samples[(n * 50) / 100],
        p95: samples[((n * 95) / 100).min(n - 1)],
        p99: samples[((n * 99) / 100).min(n - 1)],
    }
}

#[inline]
fn stub_user_rip(fn_addr: usize) -> u64 {
    let base = __dry_cpl3_bench_stubs_start as *const () as usize;
    BENCH_CODE_VA + ((fn_addr - base) as u64)
}

#[repr(C)]
struct RawCpl3TaskCfg {
    user_rip: u64,
    user_rsp: u64,
    arg0: u64,
    arg1: u64,
    r0_tsc_out: *mut u64,
}

fn raw_cpl3_bench_thread_entry(cfg_ptr_raw: u64) {
    // SAFETY: `cfg_ptr_raw` points to stack-pinned `RawCpl3TaskCfg` valid until CPU 0 drains task.
    let cfg = unsafe { &*(cfg_ptr_raw as *const RawCpl3TaskCfg) };
    crate::arch::cli();
    let cpu = current_cpu_idx();
    let cur_slot = current_slot_on_cpu(cpu);
    let ktop = tcb(cur_slot).stack_top.load(Ordering::Relaxed);
    debug_assert_eq!(USER_CS, 0x23);
    // SAFETY: Validated lower-half benchmark code/stack in active `VSpace` on CPU 0.
    unsafe {
        enter_user_iretq_unchecked(
            ktop,
            cfg.user_rip,
            cfg.user_rsp,
            USER_RFLAGS_FIXED_SET,
            cfg.arg0,
            cfg.arg1,
            cfg.r0_tsc_out,
        )
    }
}

fn k2k_pingpong_worker(role: u64) {
    for _ in 0..128 {
        if role == 0 {
            K2K_HANDOFF_TSC.store(rdtsc(), Ordering::Release);
            yield_now();
            let now = rdtsc();
            let prev = K2K_HANDOFF_TSC.load(Ordering::Acquire);
            if prev != 0 {
                let idx = K2K_SAMPLE_IDX.fetch_add(1, Ordering::Relaxed) as usize;
                if idx < SAMPLES_FAST {
                    // SAFETY: Single-CPU (`CPU 0`) serialized ping-pong benchmark.
                    unsafe { K2K_SAMPLES[idx] = now.saturating_sub(prev) };
                }
            }
        } else {
            let now = rdtsc();
            let prev = K2K_HANDOFF_TSC.load(Ordering::Acquire);
            if prev != 0 {
                let idx = K2K_SAMPLE_IDX.fetch_add(1, Ordering::Relaxed) as usize;
                if idx < SAMPLES_FAST {
                    // SAFETY: Single-CPU (`CPU 0`) serialized ping-pong benchmark.
                    unsafe { K2K_SAMPLES[idx] = now.saturating_sub(prev) };
                }
            }
            K2K_HANDOFF_TSC.store(rdtsc(), Ordering::Release);
            yield_now();
        }
    }
    K2K_HANDOFF_TSC.store(0, Ordering::Release);
    exit_current_thread()
}

fn spawn_raw_bench_user_thread(
    vs: VSpaceHandle,
    lbl: SecLabel,
    cfg: &RawCpl3TaskCfg,
    pid_slot: u32,
) {
    activate_raw_bench_pid_slot(pid_slot);
    let tid = try_spawn_thread_bound(
        1,
        0,
        vs.id,
        u16::MAX,
        lbl,
        raw_cpl3_bench_thread_entry,
        (cfg as *const RawCpl3TaskCfg) as u64,
        false,
    )
    .expect("spawn_raw_bench_user_thread");
    tcb(tid.slot()).process_id.store(pid_slot, Ordering::Release);
}

/// Execute all 11 `Stage G` benchmarks and emit `[DRY-USER-BENCH] PASS: ...`.
pub fn run_stage_g_benchmarks(
    hello_elf: &[u8],
    peer_elf: &[u8],
    online_cpus: usize,
    tsc_mhz: u64,
) {
    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);

    let u_lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(9, 1),
        1,
    );
    let _ = hello_elf;

    let code_fh = alloc_pages(PageOrder::Order4KiB, u_lbl, AllocFlags::ZERO).unwrap();
    let data_fh = alloc_pages(PageOrder::Order4KiB, u_lbl, AllocFlags::ZERO).unwrap();
    let stk1_fh = alloc_pages(PageOrder::Order4KiB, u_lbl, AllocFlags::ZERO).unwrap();
    let stk2_fh = alloc_pages(PageOrder::Order4KiB, u_lbl, AllocFlags::ZERO).unwrap();

    let stub_len =
        (__dry_cpl3_bench_stubs_end as *const () as usize) - (__dry_cpl3_bench_stubs_start as *const () as usize);
    assert!(stub_len <= PAGE_SIZE);
    // SAFETY: Copy position-independent CPL=3 benchmark stubs into `code_fh`.
    unsafe {
        copy_nonoverlapping(
            __dry_cpl3_bench_stubs_start as *const u8,
            code_fh.hhdm_ptr(),
            stub_len,
        );
    }

    let vs1 = create_vspace(u_lbl).unwrap();
    let vs2 = create_vspace(u_lbl).unwrap();
    assert_ne!(vs1.id, vs2.id);
    map_4k(vs1, BENCH_CODE_VA, code_fh, VmProt::USER_RX).unwrap();
    map_4k(vs1, BENCH_DATA_VA, data_fh, VmProt::USER_RW).unwrap();
    map_4k(vs1, BENCH_USTACK_VA, stk1_fh, VmProt::USER_RW).unwrap();
    map_4k(vs1, BENCH_USTACK_VA + 0x2000, stk2_fh, VmProt::USER_RW).unwrap();

    map_4k(vs2, BENCH_CODE_VA, code_fh, VmProt::USER_RX).unwrap();
    map_4k(vs2, BENCH_DATA_VA, data_fh, VmProt::USER_RW).unwrap();
    map_4k(vs2, BENCH_USTACK_VA, stk2_fh, VmProt::USER_RW).unwrap();

    let data_kptr = data_fh.hhdm_ptr().cast::<u64>();
    let mut samp = [0u64; SAMPLES_FAST];

    // Activate single-core deterministic bench harness on CPU 0 (`APs` quiescent at epoch barrier).
    set_preemptive_mode_active(false);
    init_scheduler(1);

    // 1. Ring 0 -> Ring 3 (`iretq`) hardware transition (`64` samples).
    let mut r0_r3_samples = [0u64; 64];
    for slot_s in &mut r0_r3_samples {
        let mut t0_r0 = 0u64;
        // SAFETY: Zero sample slot before run.
        unsafe { write_volatile(data_kptr, 0) };
        let cfg = RawCpl3TaskCfg {
            user_rip: stub_user_rip(__dry_cpl3_bench_r0_to_r3 as *const () as usize),
            user_rsp: BENCH_USTACK_RSP,
            arg0: BENCH_DATA_VA,
            arg1: 0,
            r0_tsc_out: &raw mut t0_r0,
        };
        spawn_raw_bench_user_thread(vs1, u_lbl, &cfg, 1);
        while idle_dispatch_once(0) {}
        // SAFETY: Read post-`iretq` CPL=3 timestamp written by Stub 1.
        let t1_r3 = unsafe { read_volatile(data_kptr) };
        *slot_s = t1_r3.saturating_sub(t0_r0);
    }
    let r0_to_r3 = compute_percentiles(&mut r0_r3_samples);

    // 2. Ring 3 -> Ring 0 (`syscall` entry) (`256` samples).
    DRY_BENCH_STAMP_ENTRY.store(1, Ordering::Release);
    {
        let cfg = RawCpl3TaskCfg {
            user_rip: stub_user_rip(__dry_cpl3_bench_syscall_loop as *const () as usize),
            user_rsp: BENCH_USTACK_RSP,
            arg0: BENCH_DATA_VA,
            arg1: (1u64 << 32) | (SAMPLES_FAST as u64),
            r0_tsc_out: core::ptr::null_mut(),
        };
        spawn_raw_bench_user_thread(vs1, u_lbl, &cfg, 1);
        while idle_dispatch_once(0) {}
    }
    DRY_BENCH_STAMP_ENTRY.store(0, Ordering::Release);
    for (i, dst) in samp.iter_mut().enumerate() {
        // SAFETY: `SAMPLES_FAST * 8 == 2048 <= 4096`.
        *dst = unsafe { read_volatile(data_kptr.add(i)) };
    }
    let r3_to_r0 = compute_percentiles(&mut samp);

    // 3a. Null syscall round-trip (`SYSRETQ` fast path, `256` samples).
    {
        let cfg = RawCpl3TaskCfg {
            user_rip: stub_user_rip(__dry_cpl3_bench_syscall_loop as *const () as usize),
            user_rsp: BENCH_USTACK_RSP,
            arg0: BENCH_DATA_VA,
            arg1: SAMPLES_FAST as u64,
            r0_tsc_out: core::ptr::null_mut(),
        };
        spawn_raw_bench_user_thread(vs1, u_lbl, &cfg, 1);
        while idle_dispatch_once(0) {}
    }
    for (i, dst) in samp.iter_mut().enumerate() {
        // SAFETY: Valid `data_fh` buffer.
        *dst = unsafe { read_volatile(data_kptr.add(i)) };
    }
    let null_sysret = compute_percentiles(&mut samp);

    // 3b. Null syscall round-trip (`IRETQ` fallback comparison, `256` samples).
    set_force_syscall_iretq(true);
    {
        let cfg = RawCpl3TaskCfg {
            user_rip: stub_user_rip(__dry_cpl3_bench_syscall_loop as *const () as usize),
            user_rsp: BENCH_USTACK_RSP,
            arg0: BENCH_DATA_VA,
            arg1: SAMPLES_FAST as u64,
            r0_tsc_out: core::ptr::null_mut(),
        };
        spawn_raw_bench_user_thread(vs1, u_lbl, &cfg, 1);
        while idle_dispatch_once(0) {}
    }
    set_force_syscall_iretq(false);
    for (i, dst) in samp.iter_mut().enumerate() {
        // SAFETY: Valid `data_fh` buffer.
        *dst = unsafe { read_volatile(data_kptr.add(i)) };
    }
    let null_iretq = compute_percentiles(&mut samp);

    // 4. `SYS_YIELD` syscall latency (`256` samples).
    {
        let cfg = RawCpl3TaskCfg {
            user_rip: stub_user_rip(__dry_cpl3_bench_syscall_loop as *const () as usize),
            user_rsp: BENCH_USTACK_RSP,
            arg0: BENCH_DATA_VA,
            arg1: (2u64 << 32) | (SAMPLES_FAST as u64),
            r0_tsc_out: core::ptr::null_mut(),
        };
        spawn_raw_bench_user_thread(vs1, u_lbl, &cfg, 1);
        while idle_dispatch_once(0) {}
    }
    for (i, dst) in samp.iter_mut().enumerate() {
        // SAFETY: Valid `data_fh` buffer.
        *dst = unsafe { read_volatile(data_kptr.add(i)) };
    }
    let yield_sys = compute_percentiles(&mut samp);

    // 5. Kernel -> Kernel (`k2k`) context switch (`0` `CR3` switches!).
    K2K_HANDOFF_TSC.store(0, Ordering::Release);
    K2K_SAMPLE_IDX.store(0, Ordering::Release);
    reset_cr3_perf_counters();
    let _ = try_spawn_thread_in_vspace(1, 0, KERNEL_VSPACE_ID, k2k_pingpong_worker, 0, false).unwrap();
    let _ = try_spawn_thread_in_vspace(1, 0, KERNEL_VSPACE_ID, k2k_pingpong_worker, 1, false).unwrap();
    while idle_dispatch_once(0) {}
    let (k2k_cr3_sw, _) = cr3_perf_counters();
    assert_eq!(k2k_cr3_sw, 0, "Unexpected CR3 switch during Kernel->Kernel benchmark!");
    let k2k_n = (K2K_SAMPLE_IDX.load(Ordering::Acquire) as usize).min(SAMPLES_FAST);
    // SAFETY: Single-CPU completed array.
    let k2k_sw = unsafe { compute_percentiles(&mut K2K_SAMPLES[..k2k_n]) };

    // 6. User -> User SAME-`VSpace` context switch (`vs1 <-> vs1`: ZERO `CR3` switches between user threads!).
    // SAFETY: Reset shared header `[handoff_tsc = 0, sample_idx = 0]`.
    unsafe {
        write_volatile(data_kptr, 0);
        write_volatile(data_kptr.add(1), 0);
    }
    reset_cr3_perf_counters();
    {
        let cfg_a = RawCpl3TaskCfg {
            user_rip: stub_user_rip(__dry_cpl3_bench_u2u_pingpong as *const () as usize),
            user_rsp: BENCH_USTACK_RSP,
            arg0: BENCH_DATA_VA,
            arg1: 0,
            r0_tsc_out: core::ptr::null_mut(),
        };
        let cfg_b = RawCpl3TaskCfg {
            user_rip: stub_user_rip(__dry_cpl3_bench_u2u_pingpong as *const () as usize),
            user_rsp: BENCH_USTACK_RSP + 0x2000,
            arg0: BENCH_DATA_VA,
            arg1: 1,
            r0_tsc_out: core::ptr::null_mut(),
        };
        spawn_raw_bench_user_thread(vs1, u_lbl, &cfg_a, 1);
        spawn_raw_bench_user_thread(vs1, u_lbl, &cfg_b, 2);
        while idle_dispatch_once(0) {}
    }
    let (same_vs_cr3_sw, same_vs_cr3_noops) = cr3_perf_counters();
    // Exactly `2` CR3 transitions (`idle(0) -> vs1` on entry and `vs1 -> idle(0)` on final exit);
    // all `255` User->User switches executed ZERO `mov cr3` reloads!
    assert_eq!(
        same_vs_cr3_sw, 2,
        "Expected 0 CR3 reloads across User->User same-VSpace switches (got total={})",
        same_vs_cr3_sw
    );
    assert!(same_vs_cr3_noops >= 250);
    // SAFETY: Read sample count + samples from `data_fh`.
    let same_n = unsafe { (read_volatile(data_kptr.add(1)) as usize).clamp(1, SAMPLES_FAST - 4) };
    for (i, dst) in samp.iter_mut().take(same_n).enumerate() {
        // SAFETY: `2 + i < 512` qwords in 4 KiB page.
        *dst = unsafe { read_volatile(data_kptr.add(2 + i)) };
    }
    let u2u_same_vs = compute_percentiles(&mut samp[..same_n]);

    // 7. User -> User CROSS-`VSpace` context switch (`vs1 <-> vs2`: hardware `mov cr3` every handoff).
    // SAFETY: Reset shared header `[handoff_tsc = 0, sample_idx = 0]`.
    unsafe {
        write_volatile(data_kptr, 0);
        write_volatile(data_kptr.add(1), 0);
    }
    reset_cr3_perf_counters();
    {
        let cfg_a = RawCpl3TaskCfg {
            user_rip: stub_user_rip(__dry_cpl3_bench_u2u_pingpong as *const () as usize),
            user_rsp: BENCH_USTACK_RSP,
            arg0: BENCH_DATA_VA,
            arg1: 0,
            r0_tsc_out: core::ptr::null_mut(),
        };
        let cfg_b = RawCpl3TaskCfg {
            user_rip: stub_user_rip(__dry_cpl3_bench_u2u_pingpong as *const () as usize),
            user_rsp: BENCH_USTACK_RSP,
            arg0: BENCH_DATA_VA,
            arg1: 1,
            r0_tsc_out: core::ptr::null_mut(),
        };
        spawn_raw_bench_user_thread(vs1, u_lbl, &cfg_a, 1);
        spawn_raw_bench_user_thread(vs2, u_lbl, &cfg_b, 2);
        while idle_dispatch_once(0) {}
    }
    let (cross_vs_cr3_sw, _) = cr3_perf_counters();
    assert!(cross_vs_cr3_sw >= 250);
    // SAFETY: Read sample count + samples from `data_fh`.
    let cross_n = unsafe { (read_volatile(data_kptr.add(1)) as usize).clamp(1, SAMPLES_FAST - 4) };
    for (i, dst) in samp.iter_mut().take(cross_n).enumerate() {
        // SAFETY: `2 + i < 512` qwords in 4 KiB page.
        *dst = unsafe { read_volatile(data_kptr.add(2 + i)) };
    }
    let u2u_cross_vs = compute_percentiles(&mut samp[..cross_n]);

    release_raw_bench_pid_slot(1);
    release_raw_bench_pid_slot(2);

    // 8. Isolated `CR3` switch cost (`vs1 <-> vs2`) & 9. `copy_from_user` / `copy_to_user` throughput.
    for dst in &mut samp {
        let t0 = rdtsc();
        switch_cpu_vspace_if_needed(0, vs1.id);
        switch_cpu_vspace_if_needed(0, vs2.id);
        *dst = rdtsc().saturating_sub(t0) / 2;
    }
    let cr3_sw = compute_percentiles(&mut samp);

    switch_cpu_vspace_if_needed(0, vs1.id);
    let mut kbuf_4k = [0x5au8; PAGE_SIZE];
    for dst in &mut samp {
        let t0 = rdtsc();
        copy_from_user(&mut kbuf_4k[..64], BENCH_DATA_VA).unwrap();
        *dst = rdtsc().saturating_sub(t0);
    }
    let uaccess_64b = compute_percentiles(&mut samp);

    for dst in &mut samp {
        let t0 = rdtsc();
        copy_from_user(&mut kbuf_4k, BENCH_DATA_VA).unwrap();
        *dst = rdtsc().saturating_sub(t0);
    }
    let uaccess_4k_from = compute_percentiles(&mut samp);

    for dst in &mut samp {
        let t0 = rdtsc();
        copy_to_user(BENCH_DATA_VA, &kbuf_4k).unwrap();
        *dst = rdtsc().saturating_sub(t0);
    }
    let uaccess_4k_to = compute_percentiles(&mut samp);
    switch_cpu_vspace_if_needed(0, KERNEL_VSPACE_ID);

    unmap_4k(vs2, BENCH_USTACK_VA).unwrap();
    unmap_4k(vs2, BENCH_DATA_VA).unwrap();
    unmap_4k(vs2, BENCH_CODE_VA).unwrap();
    unmap_4k(vs1, BENCH_USTACK_VA + 0x2000).unwrap();
    unmap_4k(vs1, BENCH_USTACK_VA).unwrap();
    unmap_4k(vs1, BENCH_DATA_VA).unwrap();
    unmap_4k(vs1, BENCH_CODE_VA).unwrap();
    destroy_vspace(vs2).unwrap();
    destroy_vspace(vs1).unwrap();
    free_pages(stk2_fh, PageOrder::Order4KiB, u_lbl);
    free_pages(stk1_fh, PageOrder::Order4KiB, u_lbl);
    free_pages(data_fh, PageOrder::Order4KiB, u_lbl);
    free_pages(code_fh, PageOrder::Order4KiB, u_lbl);

    // 10. Full process `spawn + ELF64 load + CPL=3 run + SYS_EXIT(0) + reclaim` (`32` samples).
    let mut life_samples = [0u64; SAMPLES_PROC_LIFE];
    init_scheduler(1);
    for slot_l in &mut life_samples {
        let t0 = rdtsc();
        let info = spawn_elf_process(
            peer_elf,
            SpawnOptions {
                subj_label: u_lbl,
                arg0: 11,
                arg1: 0,
                target_cpu: 0,
                trace_lifecycle: false,
                user_fs_base: 0,
                user_gs_base: 0,
            },
        )
        .unwrap();
        while idle_dispatch_once(0) {}
        let ReclaimStatus::ReclaimedNow(out) = reclaim_process(info.handle).unwrap() else {
            panic!("bench reclaim");
        };
        assert_eq!(out, ProcessOutcome::Exited(0));
        *slot_l = rdtsc().saturating_sub(t0);
    }
    let proc_life = compute_percentiles(&mut life_samples);

    // 11. SMP concurrency contention across `online_cpus` (`1, 2, 4, 8, 20` vCPUs).
    let workers = online_cpus.clamp(1, MAX_PROCESSES);
    let loops_per_worker: u64 = 128; // `256` syscalls (`abi_ping` + `get_tid`) + `write_debug` + `exit` per worker
    let syscalls_per_worker: u64 = loops_per_worker * 2 + 2;
    let mut smp_handles = [ProcessHandle { pid: 0, generation: 0 }; MAX_PROCESSES];

    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xf8, workers);
    let smp_t0 = rdtsc();
    for (i, sh) in smp_handles.iter_mut().enumerate().take(workers) {
        let lbl = SecLabel::new(
            SecDomain::Userspace,
            SecFlags::NONE,
            encode_compartment(10, (i as u8) + 1),
            1,
        );
        *sh = spawn_elf_process(
            peer_elf,
            SpawnOptions {
                subj_label: lbl,
                arg0: 12,
                arg1: loops_per_worker,
                target_cpu: i % online_cpus,
                trace_lifecycle: false,
                user_fs_base: 0,
                user_gs_base: 0,
            },
        )
        .unwrap()
        .handle;
    }
    drain_preemptive_epoch_and_audit(online_cpus);
    let smp_wall_cyc = rdtsc().saturating_sub(smp_t0).max(1);
    set_preemptive_mode_active(false);

    for sh in smp_handles.iter().take(workers) {
        assert_eq!(
            reclaim_process(*sh).unwrap(),
            ReclaimStatus::ReclaimedNow(ProcessOutcome::Exited(0))
        );
    }

    flush_all_magazines_quiescent(online_cpus);
    let after = measure_fragmentation(online_cpus);
    let leaked = before
        .free_global_frames
        .saturating_sub(after.free_global_frames);
    assert_eq!(leaked, 0, "Frame leak in Stage G benchmarks: {}", leaked);

    let avg_4k_cyc = ((uaccess_4k_from.p50 + uaccess_4k_to.p50) / 2).max(1);
    // `MiB/s = (4096 bytes * tsc_mhz * 10^6) / (avg_4k_cyc * 1048576) = (4096 * tsc_mhz * 1000000) >> 20 / avg_4k_cyc`.
    let uaccess_mib_s = ((PAGE_SIZE as u64) * tsc_mhz.max(1) * 1_000_000)
        .checked_div(avg_4k_cyc * 1_048_576)
        .unwrap_or(0);
    let total_smp_syscalls = (workers as u64) * syscalls_per_worker;
    let smp_mops_x100 = (total_smp_syscalls * tsc_mhz.max(1) * 100) / smp_wall_cyc;
    let smp_per_sys_cyc = smp_wall_cyc / total_smp_syscalls.max(1);

    serial_println!(
        "[DRY-USER-BENCH] PASS: cpus={} tsc_mhz={} r0_to_r3_p50_cyc={} r0_to_r3_p95_cyc={} r0_to_r3_p99_cyc={} r3_to_r0_p50_cyc={} r3_to_r0_p95_cyc={} r3_to_r0_p99_cyc={} null_sys_p50_cyc={} null_sys_p95_cyc={} null_sys_p99_cyc={} null_iretq_p50_cyc={} yield_sys_p50_cyc={} yield_sys_p95_cyc={} yield_sys_p99_cyc={} k2k_sw_p50_cyc={} k2k_sw_p95_cyc={} k2k_sw_p99_cyc={} u2u_same_vs_p50_cyc={} u2u_same_vs_p95_cyc={} u2u_same_vs_p99_cyc={} same_vs_cr3_reloads=0 u2u_cross_vs_p50_cyc={} u2u_cross_vs_p95_cyc={} u2u_cross_vs_p99_cyc={} cr3_sw_p50_cyc={} cr3_sw_p95_cyc={} cr3_sw_p99_cyc={} uaccess_64b_p50_cyc={} uaccess_64b_p95_cyc={} uaccess_64b_p99_cyc={} uaccess_4k_from_p50_cyc={} uaccess_4k_to_p50_cyc={} uaccess_mib_s={} proc_life_p50_cyc={} proc_life_p95_cyc={} proc_life_p99_cyc={} smp_workers={} smp_syscalls={} smp_wall_cyc={} smp_per_sys_cyc={} smp_mops_x100={} leaked_frames={}",
        online_cpus,
        tsc_mhz,
        r0_to_r3.p50,
        r0_to_r3.p95,
        r0_to_r3.p99,
        r3_to_r0.p50,
        r3_to_r0.p95,
        r3_to_r0.p99,
        null_sysret.p50,
        null_sysret.p95,
        null_sysret.p99,
        null_iretq.p50,
        yield_sys.p50,
        yield_sys.p95,
        yield_sys.p99,
        k2k_sw.p50,
        k2k_sw.p95,
        k2k_sw.p99,
        u2u_same_vs.p50,
        u2u_same_vs.p95,
        u2u_same_vs.p99,
        u2u_cross_vs.p50,
        u2u_cross_vs.p95,
        u2u_cross_vs.p99,
        cr3_sw.p50,
        cr3_sw.p95,
        cr3_sw.p99,
        uaccess_64b.p50,
        uaccess_64b.p95,
        uaccess_64b.p99,
        uaccess_4k_from.p50,
        uaccess_4k_to.p50,
        uaccess_mib_s,
        proc_life.p50,
        proc_life.p95,
        proc_life.p99,
        workers,
        total_smp_syscalls,
        smp_wall_cyc,
        smp_per_sys_cyc,
        smp_mops_x100,
        leaked
    );
}
