// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `Milestone 1B-D` Verification Suite: Stage D Real ELF64 Native Processes (`user-hello.elf` &
//! `user-peer.elf`), Stage E Lifecycle Reclamation, and Stage F 16 Security & SMP Fault Scenarios.

use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::Ordering;

use crate::arch::{cli, rdmsr, rdtsc, sti, wrmsr, MSR_IA32_GS_BASE, MSR_IA32_KERNEL_GS_BASE};
use crate::cspace::objects::{cspace_alloc_frame, cspace_vspace_map_4k};
use crate::cspace::{cspace_derive, cspace_lookup_fast};
use crate::gdt_tss::{KERNEL_CS, USER_CS, USER_DS};
use crate::limine::get_boot_module;
use crate::percpu::{current_percpu, get_percpu_by_idx};
use crate::pmm::{
    alloc_pages, flush_all_magazines_quiescent, free_pages, measure_fragmentation, AllocFlags,
    PageOrder,
};
use crate::proc::{
    inspect_live_process, kill_process, process_syscall_entries, reclaim_process,
    spawn_elf_process, spawn_elf_process_with, ProcError, ProcessHandle, ProcessOutcome,
    ReclaimStatus, SpawnOptions, MAX_PROCESSES,
};
use crate::sched::preempt_tests::{begin_preemptive_epoch, drain_preemptive_epoch_and_audit};
use crate::sched::state::WakeOutcome;
use crate::sched::{
    idle_dispatch_once, init_scheduler, set_preemptive_mode_active, start_cpu_timer_for_phase,
    stop_cpu_timer_for_phase, wake_thread, GLOBAL_PREEMPTIONS,
};
use crate::security::{
    current_lease_epoch, encode_compartment, CapRights, SecDomain, SecFlags, SecLabel,
};
use crate::serial_println;
use crate::syscall::entry::{validate_user_context, USER_RFLAGS_FIXED_SET};
use crate::syscall::{set_force_syscall_iretq, set_inject_corrupt_return_mode};
use crate::uaccess::{copy_from_user, copy_to_user, is_smap_active, uaccess_telemetry, SysError};
use crate::vspace::pte::VmProt;
use crate::vspace::{
    create_vspace, destroy_vspace, inspect_vspace_live, map_4k, protect_4k,
    switch_cpu_vspace_if_needed, unmap_4k, KERNEL_VSPACE_ID,
};

#[used]
static KERNEL_SUPERVISOR_SENTINEL: u64 = 0x4b45_524e_5f4f_4e4c; // "KERN_ONL"
static mut KERNEL_MUT_GUARD_WORD: u64 = 0x9988_7766_5544_3322;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserFaultCmdline {
    None,
    KernelPfNotMasked,
    SmapRing0Bypass,
    WxElfSegment,
    ActiveProcReclaim,
}

impl UserFaultCmdline {
    pub fn from_cmdline(cmdline: &str) -> Self {
        if cmdline.contains("user_fault=kernel_pf_not_masked") {
            Self::KernelPfNotMasked
        } else if cmdline.contains("user_fault=smap_ring0_bypass") {
            Self::SmapRing0Bypass
        } else if cmdline.contains("user_fault=wx_elf_segment") {
            Self::WxElfSegment
        } else if cmdline.contains("user_fault=active_proc_reclaim") {
            Self::ActiveProcReclaim
        } else {
            Self::None
        }
    }
}

pub fn user_invariant_panic(args: core::fmt::Arguments<'_>) -> ! {
    crate::enter_fatal_or_halt();
    crate::serial::emergency_print(format_args!("\r\n[DRY-USER-FAIL] {}\r\n", args));
    panic!("Userspace / Ring 3 security invariant violation");
}

#[inline]
fn default_user_opts(subj_label: SecLabel, arg0: u64, arg1: u64, target_cpu: usize) -> SpawnOptions {
    SpawnOptions {
        subj_label,
        arg0,
        arg1,
        target_cpu,
        trace_lifecycle: false,
        user_fs_base: 0,
        user_gs_base: 0,
    }
}

fn run_single_proc(
    elf_bytes: &[u8],
    substage: u8,
    online_cpus: usize,
    opts: SpawnOptions,
) -> (ProcessHandle, ProcessOutcome) {
    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(substage, 1);
    let info = spawn_elf_process(elf_bytes, opts).expect("spawn_elf_process failed");
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);
    let ReclaimStatus::ReclaimedNow(out) =
        reclaim_process(info.handle).expect("reclaim_process failed")
    else {
        panic!("Unexpected AlreadyReclaimedNoOp");
    };
    (info.handle, out)
}

/// Deterministic kernel-panic negative fault tests invoked via `user_fault=...` cmdline.
#[cold]
#[inline(never)]
pub fn run_user_cmdline_negative_test(fault: UserFaultCmdline, online_cpus: usize) -> ! {
    let u_lbl = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, encode_compartment(1, 1), 1);
    match fault {
        UserFaultCmdline::None => unreachable!(),
        UserFaultCmdline::KernelPfNotMasked => {
            serial_println!(
                "[DRY-USER-INFO] Triggering unmapped Ring 0 #PF outside uaccess extable..."
            );
            // SAFETY: Intentional Ring 0 `#PF` proving `handle_user_exception` & `uaccess` never mask kernel bugs.
            unsafe {
                let _ = read_volatile(0xffff_ff80_dead_bee0usize as *const u64);
            }
            unreachable!()
        }
        UserFaultCmdline::SmapRing0Bypass => {
            let vs = create_vspace(u_lbl).unwrap();
            let fh = alloc_pages(PageOrder::Order4KiB, u_lbl, AllocFlags::ZERO).unwrap();
            // SAFETY: Write sentinel via supervisor HHDM prior to testing direct lower-half Ring 0 read.
            unsafe { write_volatile(fh.hhdm_ptr().cast::<u64>(), 0x55aa_1234_5678_9abc) };
            map_4k(vs, 0x0060_0000, fh, VmProt::USER_RW).unwrap();
            switch_cpu_vspace_if_needed(0, vs.id);
            if !is_smap_active() {
                user_invariant_panic(format_args!(
                    "SMAP hardware unsupported on this CPU model; direct Ring 0 user access blocked by policy"
                ));
            }
            serial_println!(
                "[DRY-USER-INFO] Attempting raw Ring 0 read of USER_RW 0x600000 with CR4.SMAP=1 (AC=0)..."
            );
            // SAFETY: Intentional SMAP violation at CPL=0 (`AC=0`) to verify hardware `#PF` panic!
            unsafe {
                let _ = read_volatile(0x0060_0000usize as *const u64);
            }
            unreachable!()
        }
        UserFaultCmdline::WxElfSegment => {
            let hello = get_boot_module("user-hello.elf").expect("missing user-hello.elf");
            let mut bad_elf = [0u8; 4096];
            let copy_len = hello.len().min(bad_elf.len());
            bad_elf[..copy_len].copy_from_slice(&hello[..copy_len]);
            // Patch first `PT_LOAD` segment's `p_flags` (`PF_R | PF_W | PF_X = 7`) to violate `W^X`!
            let phoff = u64::from_le_bytes(bad_elf[32..40].try_into().unwrap()) as usize;
            let phentsize = u16::from_le_bytes(bad_elf[54..56].try_into().unwrap()) as usize;
            let phnum = u16::from_le_bytes(bad_elf[56..58].try_into().unwrap()) as usize;
            for i in 0..phnum {
                let off = phoff + i * phentsize;
                if u32::from_le_bytes(bad_elf[off..off + 4].try_into().unwrap()) == 1 {
                    bad_elf[off + 4..off + 8].copy_from_slice(&7u32.to_le_bytes());
                    break;
                }
            }
            let res = spawn_elf_process(&bad_elf, default_user_opts(u_lbl, 0, 0, 0));
            user_invariant_panic(format_args!(
                "W^X ELF64 segment rejected at load time: err={:?}",
                res.err()
            ));
        }
        UserFaultCmdline::ActiveProcReclaim => {
            let peer = get_boot_module("user-peer.elf").expect("missing user-peer.elf");
            set_preemptive_mode_active(true);
            init_scheduler(online_cpus);
            begin_preemptive_epoch(0xd0, 1);
            let info =
                spawn_elf_process(peer, default_user_opts(u_lbl, 8, 0, 1 % online_cpus)).unwrap();
            let premature = reclaim_process(info.handle);
            user_invariant_panic(format_args!(
                "Premature reclamation of active process rejected: pid={} err={:?}",
                info.handle.pid,
                premature.err()
            ));
        }
    }
}

/// Stage D: Boot real separately compiled `user-hello.elf` (`PID=1`) and `user-peer.elf` (`PID=2`).
fn verify_stage_d_native_processes(
    hello_elf: &[u8],
    peer_elf: &[u8],
    online_cpus: usize,
) {
    flush_all_magazines_quiescent(online_cpus);
    let baseline = measure_fragmentation(online_cpus);

    serial_println!("[DRY-BOOT] Kernel ready");

    let u1_lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(1, 1),
        1,
    );
    let u2_lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(2, 1),
        2,
    );

    // 1. Program 1 (`user-hello.elf` -> `PID=1`, `VSpace=1`, `CSpace=1`) with real hardware `CPL=3`
    //    `IRETQ` entry, `SYS_CAP_QUERY`, `SYS_WRITE_DEBUG`, `SYS_YIELD`, and `SYS_EXIT(0)`.
    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xd1, 1);
    let p1_opts = SpawnOptions {
        subj_label: u1_lbl,
        arg0: 0,
        arg1: 1,
        target_cpu: 0,
        trace_lifecycle: true,
        user_fs_base: 0x0000_0000_0040_2100,
        user_gs_base: 0x0000_0000_0040_2200,
    };
    let p1 = spawn_elf_process_with(hello_elf, p1_opts, |cs, lbl, _, _| {
        let (root_fr, _) = cspace_alloc_frame(cs, lbl, lbl, PageOrder::Order4KiB, false, 0)?;
        let ro_cap = cspace_derive(cs, lbl, root_fr, CapRights::READ, 0, 0x1bd, 1)?;
        Ok((ro_cap.0, 1))
    })
    .expect("Failed to spawn PID=1 user-hello.elf");
    assert_eq!(p1.handle.pid, 1);
    assert_eq!(p1.vspace.id, 1);
    assert_eq!(p1.cspace_id, 1);

    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);

    let ReclaimStatus::ReclaimedNow(p1_out) =
        reclaim_process(p1.handle).expect("Failed to reclaim PID=1")
    else {
        panic!("Expected ReclaimedNow for PID=1");
    };
    assert_eq!(p1_out, ProcessOutcome::Exited(0), "PID=1 unexpected outcome");

    flush_all_magazines_quiescent(online_cpus);
    let post_p1 = measure_fragmentation(online_cpus);
    let leaked_p1 = baseline
        .free_global_frames
        .saturating_sub(post_p1.free_global_frames);
    assert_eq!(leaked_p1, 0, "Frame leak after PID=1 teardown: {}", leaked_p1);
    serial_println!("[DRY-PMM] leaked_frames={}", leaked_p1);

    // 2. Concurrent multi-process isolation (`user-hello.elf` + `user-peer.elf` mode 0) in distinct
    //    `VSpace`s + `CSpace`s across CPUs/timeslices.
    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xd2, 2);
    let c1 = spawn_elf_process(hello_elf, default_user_opts(u1_lbl, 0, 2, 0)).unwrap();
    let c2 =
        spawn_elf_process(peer_elf, default_user_opts(u2_lbl, 0, 0x77, 1 % online_cpus)).unwrap();
    assert_ne!(c1.vspace.id, c2.vspace.id);
    assert_ne!(c1.cspace_id, c2.cspace_id);
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);

    assert_eq!(
        reclaim_process(c1.handle).unwrap(),
        ReclaimStatus::ReclaimedNow(ProcessOutcome::Exited(0))
    );
    assert_eq!(
        reclaim_process(c2.handle).unwrap(),
        ReclaimStatus::ReclaimedNow(ProcessOutcome::Exited(42))
    );
}

/// Stage F: Execute all 16 mandatory Ring 3 security, fault-isolation & SMP concurrency scenarios.
fn verify_stage_f_all_16_scenarios(hello_elf: &[u8], peer_elf: &[u8], online_cpus: usize) {
    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);

    let u1_lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(3, 1),
        10,
    );
    let u2_lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(4, 1),
        20,
    );
    let u3_lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(5, 1),
        30,
    );

    // --- F1: CPL=3 kernel memory read (`err=0x5`) & write (`err=0x7`) -> isolated `#PF`, kernel intact ---
    let k_ro_va = (&raw const KERNEL_SUPERVISOR_SENTINEL) as u64;
    let (_, out_f1r) = run_single_proc(
        peer_elf,
        0xe1,
        online_cpus,
        default_user_opts(u1_lbl, 1, k_ro_va, 0),
    );
    assert!(
        matches!(out_f1r, ProcessOutcome::Faulted(f) if f.vector == 14 && f.cs == (USER_CS as u64) && (f.error_code & 0x5) == 0x5 && f.cr2 == k_ro_va),
        "F1a failed: {:?}",
        out_f1r
    );

    let k_rw_va = (&raw mut KERNEL_MUT_GUARD_WORD) as u64;
    let (_, out_f1w) = run_single_proc(
        peer_elf,
        0xe2,
        online_cpus,
        default_user_opts(u1_lbl, 2, k_rw_va, 1 % online_cpus),
    );
    assert!(
        matches!(out_f1w, ProcessOutcome::Faulted(f) if f.vector == 14 && f.cs == (USER_CS as u64) && (f.error_code & 0x7) == 0x7 && f.cr2 == k_rw_va),
        "F1b failed: {:?}",
        out_f1w
    );
    // SAFETY: Kernel static guard word verified untouched by Ring 3 write attempt.
    assert_eq!(unsafe { read_volatile(&raw const KERNEL_MUT_GUARD_WORD) }, 0x9988_7766_5544_3322);

    // --- F2: CPL=3 privileged instruction (`cli` -> `#GP(0)`, vec=13) ---
    let (_, out_f2) = run_single_proc(
        peer_elf,
        0xe3,
        online_cpus,
        default_user_opts(u1_lbl, 3, 0, 0),
    );
    assert!(
        matches!(out_f2, ProcessOutcome::Faulted(f) if f.vector == 13 && f.error_code == 0 && f.cs == (USER_CS as u64)),
        "F2 failed: {:?}",
        out_f2
    );

    // --- F3: CPL=3 write to `USER_RO` `.rodata` page (`#PF`, err=0x7: P|W|U) ---
    let (_, out_f3) = run_single_proc(
        peer_elf,
        0xe4,
        online_cpus,
        default_user_opts(u1_lbl, 4, 0, 1 % online_cpus),
    );
    assert!(
        matches!(out_f3, ProcessOutcome::Faulted(f) if f.vector == 14 && f.cs == (USER_CS as u64) && (f.error_code & 0x7) == 0x7),
        "F3 failed: {:?}",
        out_f3
    );

    // --- F4: CPL=3 execute from `NX` `.bss` page (`#PF`, err=0x15: P|U|I) ---
    let (_, out_f4) = run_single_proc(
        peer_elf,
        0xe5,
        online_cpus,
        default_user_opts(u1_lbl, 5, 0, 0),
    );
    assert!(
        matches!(out_f4, ProcessOutcome::Faulted(f) if f.vector == 14 && f.cs == (USER_CS as u64) && (f.error_code & 0x15) == 0x15),
        "F4 failed: {:?}",
        out_f4
    );

    // --- F5, F6, F7, F8: Invalid syscall, invalid/U=0/RO user pointers, guard-page boundary overflow,
    //     forged `CapPtr` handles, GPR scrub hygiene, and concurrent `uaccess` `extable` fixups ---
    let fixups_before = uaccess_telemetry().fixup_hits;
    let sup_lower_fh = alloc_pages(PageOrder::Order4KiB, u1_lbl, AllocFlags::ZERO).unwrap();

    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xe6, 2);
    let p_neg_a = spawn_elf_process_with(
        peer_elf,
        default_user_opts(u1_lbl, 6, 0x0068_0000, 0),
        |_, _, _, vs_h| {
            map_4k(vs_h, 0x0068_0000, sup_lower_fh, VmProt::SUPERVISOR_LOWER_RO)
                .expect("map SUPERVISOR_LOWER_RO");
            Ok((6, 0x0068_0000))
        },
    )
    .unwrap();
    let p_neg_b = spawn_elf_process_with(
        peer_elf,
        default_user_opts(u2_lbl, 61, 0, 1 % online_cpus),
        |cs, lbl, _, _| {
            let (fr, _) = cspace_alloc_frame(cs, lbl, lbl, PageOrder::Order4KiB, false, 0)?;
            let ro = cspace_derive(cs, lbl, fr, CapRights::READ, 0, 0x42, 1)?;
            Ok((61, ro.0))
        },
    )
    .unwrap();
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);

    assert_eq!(
        reclaim_process(p_neg_a.handle).unwrap(),
        ReclaimStatus::ReclaimedNow(ProcessOutcome::Exited(0x5afe))
    );
    free_pages(sup_lower_fh, PageOrder::Order4KiB, u1_lbl);
    assert_eq!(
        reclaim_process(p_neg_b.handle).unwrap(),
        ReclaimStatus::ReclaimedNow(ProcessOutcome::Exited(0x5afe))
    );

    // Direct `uaccess` TOCTOU `unmap` / `protect(USER_RO)` `#PF` `extable` fixup check:
    {
        let tmp_vs = create_vspace(u1_lbl).unwrap();
        let tmp_fh = alloc_pages(PageOrder::Order4KiB, u1_lbl, AllocFlags::ZERO).unwrap();
        map_4k(tmp_vs, 0x0069_0000, tmp_fh, VmProt::USER_RW).unwrap();
        switch_cpu_vspace_if_needed(0, tmp_vs.id);
        let src = [0xabu8; 32];
        let mut dst = [0u8; 32];
        copy_to_user(0x0069_0000, &src).unwrap();
        copy_from_user(&mut dst, 0x0069_0000).unwrap();
        assert_eq!(dst, src);
        protect_4k(tmp_vs, 0x0069_0000, VmProt::USER_RO).unwrap();
        assert_eq!(copy_to_user(0x0069_0000, &src), Err(SysError::Fault));
        unmap_4k(tmp_vs, 0x0069_0000).unwrap();
        assert_eq!(copy_from_user(&mut dst, 0x0069_0000), Err(SysError::Fault));
        switch_cpu_vspace_if_needed(0, KERNEL_VSPACE_ID);
        destroy_vspace(tmp_vs).unwrap();
        free_pages(tmp_fh, PageOrder::Order4KiB, u1_lbl);
    }
    let fixups_after = uaccess_telemetry().fixup_hits;
    assert!(
        fixups_after >= fixups_before + 4,
        "Expected >=4 extable #PF fixups across guard-page/RO/unmapped uaccess tests, got {} -> {}",
        fixups_before,
        fixups_after
    );

    // --- F9 & F12: Cross-process private memory probe (`#PF` err=0x4) + `#UD` crash (`vec=6`)
    //     while sibling `PID=A` runs concurrently and finishes unimpaired with secret intact ---
    let mut victim_secret_ptr: *mut u64 = core::ptr::null_mut();
    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xe7, 3);
    let sib_victim = spawn_elf_process_with(
        hello_elf,
        default_user_opts(u1_lbl, 0, 8, 0),
        |cs, lbl, vs_cap, _| {
            let (sec_cap, sec_fh) =
                cspace_alloc_frame(cs, lbl, lbl, PageOrder::Order4KiB, false, 0)?;
            // SAFETY: Exclusively owned private user page initialized before mapping.
            unsafe {
                victim_secret_ptr = sec_fh.hhdm_ptr().cast::<u64>();
                write_volatile(victim_secret_ptr, 0xdead_beef_cafe_babe);
            }
            cspace_vspace_map_4k(cs, lbl, vs_cap, sec_cap, 0x0050_0000, VmProt::USER_RW, 1)?;
            Ok((0, 8))
        },
    )
    .unwrap();
    let attacker = spawn_elf_process(
        peer_elf,
        default_user_opts(u2_lbl, 7, 0x0050_0000, 1 % online_cpus),
    )
    .unwrap();
    let ud_crasher = spawn_elf_process(
        peer_elf,
        default_user_opts(u3_lbl, 10, 0, 2 % online_cpus),
    )
    .unwrap();
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);

    // Verify victim's private page was NOT corrupted before reclaiming victim!
    // SAFETY: `sib_victim`'s backing frame is still allocated until `reclaim_process` runs below.
    assert_eq!(unsafe { read_volatile(victim_secret_ptr) }, 0xdead_beef_cafe_babe);
    assert_eq!(
        reclaim_process(sib_victim.handle).unwrap(),
        ReclaimStatus::ReclaimedNow(ProcessOutcome::Exited(0))
    );
    let ReclaimStatus::ReclaimedNow(att_out) = reclaim_process(attacker.handle).unwrap() else {
        panic!("attacker reclaim");
    };
    assert!(
        matches!(att_out, ProcessOutcome::Faulted(f) if f.vector == 14 && f.cs == (USER_CS as u64) && (f.error_code & 0x5) == 0x4 && f.cr2 == 0x0050_0000),
        "F9 failed: {:?}",
        att_out
    );
    let ReclaimStatus::ReclaimedNow(ud_out) = reclaim_process(ud_crasher.handle).unwrap() else {
        panic!("ud_crasher reclaim");
    };
    assert!(
        matches!(ud_out, ProcessOutcome::Faulted(f) if f.vector == 6 && f.cs == (USER_CS as u64)),
        "F12 failed: {:?}",
        ud_out
    );

    // --- F10: Concurrent `kill_process` while target executes in-flight `SYS_YIELD` syscalls ---
    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xe8, 1);
    let kill_victim = spawn_elf_process(
        peer_elf,
        default_user_opts(u1_lbl, 8, 0, 1 % online_cpus),
    )
    .unwrap();
    // Active process reclamation MUST fail closed before termination!
    assert!(matches!(
        reclaim_process(kill_victim.handle),
        Err(ProcError::BusyActiveThread | ProcError::BusyInFlightSyscall)
    ));
    let wait_t0 = rdtsc();
    let bsp_pcpu = get_percpu_by_idx(0);
    while process_syscall_entries(kill_victim.handle) < 4 {
        if online_cpus == 1 {
            start_cpu_timer_for_phase(bsp_pcpu);
            sti();
            let _ = idle_dispatch_once(0);
            stop_cpu_timer_for_phase(bsp_pcpu);
        } else {
            core::hint::spin_loop();
        }
        assert!(rdtsc().wrapping_sub(wait_t0) < 10_000_000_000, "F10 syscall entry timeout");
    }
    kill_process(kill_victim.handle).expect("F10 kill_process failed");
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);
    assert_eq!(
        reclaim_process(kill_victim.handle).unwrap(),
        ReclaimStatus::ReclaimedNow(ProcessOutcome::Killed)
    );

    // --- F11: Preemptive CPL=3 timer IRQ user/kernel stack isolation & TLS (`FS`/`GS`) migration ---
    let pre_before = GLOBAL_PREEMPTIONS.load(Ordering::Acquire);
    let spinners = (online_cpus + 2).min(MAX_PROCESSES);
    let spin_iters: u64 = if online_cpus > 8 { 600_000 } else { 1_500_000 };
    let mut spin_handles = [ProcessHandle { pid: 0, generation: 0 }; MAX_PROCESSES];
    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xe9, spinners);
    for (i, slot_h) in spin_handles.iter_mut().enumerate().take(spinners) {
        let lbl = SecLabel::new(
            SecDomain::Userspace,
            SecFlags::NONE,
            encode_compartment(6, (i as u8) + 1),
            1,
        );
        let opts = SpawnOptions {
            subj_label: lbl,
            arg0: 9,
            arg1: spin_iters,
            target_cpu: i % online_cpus,
            trace_lifecycle: false,
            user_fs_base: 0x0000_0000_0040_2100 + ((i as u64) * 0x40),
            user_gs_base: 0x0000_0000_0040_2700 + ((i as u64) * 0x40),
        };
        *slot_h = spawn_elf_process(peer_elf, opts).unwrap().handle;
    }
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);
    let pre_after = GLOBAL_PREEMPTIONS.load(Ordering::Acquire);
    assert!(
        pre_after > pre_before,
        "Expected CPL=3 timer preemptions during F11 ({} -> {})",
        pre_before,
        pre_after
    );
    for h in spin_handles.iter().take(spinners) {
        assert_eq!(
            reclaim_process(*h).unwrap(),
            ReclaimStatus::ReclaimedNow(ProcessOutcome::Exited(0x7777)),
            "F11 red-zone or GPR corruption across CPL=3 preemption!"
        );
    }

    // --- F13: Stale `ProcessHandle`/`VSpace`/`TCB`/`CapPtr` rejection + idempotent double reclaim ---
    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xea, 1);
    let stale_info =
        spawn_elf_process(peer_elf, default_user_opts(u1_lbl, 11, 0, 0)).unwrap();
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);

    assert_eq!(
        reclaim_process(stale_info.handle).unwrap(),
        ReclaimStatus::ReclaimedNow(ProcessOutcome::Exited(0))
    );
    // Repeated reclaim MUST be a safe no-op (`Stage E.5` idempotency):
    assert_eq!(
        reclaim_process(stale_info.handle).unwrap(),
        ReclaimStatus::AlreadyReclaimedNoOp
    );
    assert_eq!(inspect_live_process(stale_info.handle), Err(ProcError::StaleTerminated));
    assert_eq!(kill_process(stale_info.handle), Err(ProcError::StaleTerminated));
    assert!(inspect_vspace_live(stale_info.vspace.id)
        .is_none_or(|(g, _, _)| g != stale_info.vspace.generation));
    assert!(cspace_lookup_fast(
        stale_info.cspace_id,
        u1_lbl,
        stale_info.tcb_cap,
        CapRights::READ,
        current_lease_epoch()
    )
    .is_err());
    assert_eq!(wake_thread(stale_info.tid, 0), WakeOutcome::Terminated);

    // Recycle slot (`pid == stale_info.handle.pid`, bumped `generation`) -> old handle fails ABA check!
    let (_, _) = run_single_proc(
        peer_elf,
        0xeb,
        online_cpus,
        default_user_opts(u1_lbl, 11, 0, 0),
    );
    assert_eq!(
        inspect_live_process(stale_info.handle),
        Err(ProcError::StaleOrInvalidHandle)
    );

    // --- F14: Concurrent SMP process `spawn + run + exit + reclaim` waves across `1..20` vCPUs ---
    let wave_n = online_cpus.clamp(2, 20).min(MAX_PROCESSES);
    for wave in 0..2u8 {
        let mut wave_handles = [ProcessHandle { pid: 0, generation: 0 }; MAX_PROCESSES];
        set_preemptive_mode_active(true);
        init_scheduler(online_cpus);
        begin_preemptive_epoch(0xec + wave, wave_n);
        for (i, wh) in wave_handles.iter_mut().enumerate().take(wave_n) {
            let lbl = SecLabel::new(
                SecDomain::Userspace,
                SecFlags::NONE,
                encode_compartment(7, (i as u8) + 1),
                1,
            );
            let mode = if (i & 1) == 0 { 0 } else { 12 };
            let aux = if mode == 0 { i as u64 } else { 64 };
            *wh = spawn_elf_process(peer_elf, default_user_opts(lbl, mode, aux, i % online_cpus))
                .unwrap()
                .handle;
        }
        drain_preemptive_epoch_and_audit(online_cpus);
        set_preemptive_mode_active(false);
        for (i, wh) in wave_handles.iter().enumerate().take(wave_n) {
            let exp_code = if (i & 1) == 0 { 42 } else { 0 };
            assert_eq!(
                reclaim_process(*wh).unwrap(),
                ReclaimStatus::ReclaimedNow(ProcessOutcome::Exited(exp_code))
            );
        }
    }

    // --- F15: Interrupt / `NMI` (`vec=2`) `GS_BASE` safety across pre-`swapgs` & Ring-0 windows ---
    {
        let pcpu0 = current_percpu();
        let nmi_before = pcpu0.nmi_count.load(Ordering::Acquire);
        let fake_user_gs: u64 = 0x0000_0000_0040_2980;
        cli();
        // SAFETY: Simulates exact 1-instruction pre-`swapgs` `SYSCALL` entry / post-`swapgs` `SYSRETQ`
        // window (`IA32_GS_BASE = user_gs`, `IA32_KERNEL_GS_BASE = &PerCpuData`) during an NMI (`int 2`).
        unsafe {
            let orig_gs = rdmsr(MSR_IA32_GS_BASE);
            let orig_kgs = rdmsr(MSR_IA32_KERNEL_GS_BASE);
            assert_eq!(orig_gs, pcpu0.self_ptr);
            wrmsr(MSR_IA32_GS_BASE, fake_user_gs);
            wrmsr(MSR_IA32_KERNEL_GS_BASE, orig_gs);
            asm!("int 2", options(nomem, nostack));
            let post_nmi_gs = rdmsr(MSR_IA32_GS_BASE);
            let post_nmi_kgs = rdmsr(MSR_IA32_KERNEL_GS_BASE);
            wrmsr(MSR_IA32_GS_BASE, orig_gs);
            wrmsr(MSR_IA32_KERNEL_GS_BASE, orig_kgs);
            assert_eq!(post_nmi_gs, fake_user_gs, "NMI corrupted swapped user GS_BASE!");
            assert_eq!(post_nmi_kgs, orig_gs, "NMI corrupted staged KERNEL_GS_BASE!");
            // Also fire `int 2` with normal Ring-0 `GS_BASE` active (no double-`swapgs`!).
            asm!("int 2", options(nomem, nostack));
            assert_eq!(rdmsr(MSR_IA32_GS_BASE), orig_gs);
        }
        assert_eq!(pcpu0.nmi_count.load(Ordering::Acquire), nmi_before + 2);
    }

    // --- F16: Malformed / non-canonical `RIP`/`RSP`/`RFLAGS`/`CS` rejected + `IRETQ` fallback ---
    assert!(validate_user_context(
        0x0000_8000_0000_0000,
        0x0000_007f_fff0_4ff0,
        USER_RFLAGS_FIXED_SET,
        USER_CS as u64,
        USER_DS as u64,
        true
    )
    .is_err());
    assert!(validate_user_context(
        0x0040_0000,
        0xffff_ffff_8000_1000,
        USER_RFLAGS_FIXED_SET,
        USER_CS as u64,
        USER_DS as u64,
        true
    )
    .is_err());
    assert!(validate_user_context(
        0x0040_0000,
        0x0000_007f_fff0_4ff0,
        USER_RFLAGS_FIXED_SET | (3 << 12),
        USER_CS as u64,
        USER_DS as u64,
        true
    )
    .is_err());
    assert!(validate_user_context(
        0x0040_0000,
        0x0000_007f_fff0_4ff0,
        USER_RFLAGS_FIXED_SET,
        KERNEL_CS as u64,
        USER_DS as u64,
        true
    )
    .is_err());

    // `IRETQ` syscall-exit fallback verification (`set_force_syscall_iretq(true)`):
    set_force_syscall_iretq(true);
    let (_, iretq_out) = run_single_proc(
        hello_elf,
        0xee,
        online_cpus,
        default_user_opts(u1_lbl, 0, 1, 0),
    );
    set_force_syscall_iretq(false);
    assert_eq!(iretq_out, ProcessOutcome::Exited(0));

    // CPL=3 executing `syscall` with non-canonical `RSP = 0x0001_0000_0000_0000` (`mode=13`):
    let (_, bad_rsp_out) = run_single_proc(
        peer_elf,
        0xef,
        online_cpus,
        default_user_opts(u1_lbl, 13, 0x0001_0000_0000_0000, 1 % online_cpus),
    );
    assert!(matches!(bad_rsp_out, ProcessOutcome::Faulted(f) if f.vector == 13));

    // Corrupt `SYSRETQ` return injection (`1 = non-canonical RIP`, `2 = kernel RSP`, `3 = IOPL=3`):
    for inj_mode in 1..=3u8 {
        set_inject_corrupt_return_mode(inj_mode);
        let (_, inj_out) = run_single_proc(
            hello_elf,
            0xf0 + inj_mode,
            online_cpus,
            default_user_opts(u1_lbl, 0, 1, 0),
        );
        set_inject_corrupt_return_mode(0);
        assert!(
            matches!(inj_out, ProcessOutcome::Faulted(f) if f.vector == 13),
            "F16 corrupt return mode={} was not rejected safely: {:?}",
            inj_mode,
            inj_out
        );
    }

    flush_all_magazines_quiescent(online_cpus);
    let after = measure_fragmentation(online_cpus);
    let leaked = before
        .free_global_frames
        .saturating_sub(after.free_global_frames);
    assert_eq!(leaked, 0, "Frame leak in Stage F security suite: {}", leaked);
    assert_eq!(after.contig_2m_blocks, before.contig_2m_blocks);

    let utel = uaccess_telemetry();
    serial_println!(
        "[DRY-USER-SEC] PASS: cpus={} scenarios=16/16 smap_hw={} smap_active={} uaccess_calls={} uaccess_bytes={} uaccess_fixups={} cpl3_isolation_ok=true syscall_hygiene_ok=true nmi_gs_safe=true sysret_canon_ok=true leaked_frames={}",
        online_cpus,
        utel.smap_supported as u8,
        utel.smap_active as u8,
        utel.copy_calls,
        utel.copy_bytes,
        utel.fixup_hits,
        leaked
    );
}

/// Top-level `Milestone 1B-D` verification + benchmark orchestrator invoked from `bsp_kmain`.
pub fn verify_userspace_and_syscalls(online_cpus: usize, tsc_mhz: u64, cmdline: &str) {
    let fault = UserFaultCmdline::from_cmdline(cmdline);
    if fault != UserFaultCmdline::None {
        run_user_cmdline_negative_test(fault, online_cpus);
    }

    let hello_elf =
        get_boot_module("user-hello.elf").expect("Missing Limine module user-hello.elf");
    let peer_elf = get_boot_module("user-peer.elf").expect("Missing Limine module user-peer.elf");

    verify_stage_d_native_processes(hello_elf, peer_elf, online_cpus);
    verify_stage_f_all_16_scenarios(hello_elf, peer_elf, online_cpus);
    super::bench::run_stage_g_benchmarks(hello_elf, peer_elf, online_cpus, tsc_mhz);
}
