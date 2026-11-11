// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Milestone `1B-B` Verification Suite: 4 KiB `VSpace` Isolation, `W^X` / `U/S` / `R/W` / `NX`
//! Enforcement, Page-Table Reclamation, Separate `2 MiB` Sub-Stage, `rdtsc` Microbenchmarks,
//! `1/2/4/20`-vCPU Preemptive `map/unmap/protect/CR3` Stress, and 9 Negative Fault Scenarios.

use core::cell::UnsafeCell;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::arch::{cli, rdtsc};
use crate::gdt_tss::verify_cpu_tss_and_canaries;
use crate::idt::verify_breakpoint_trap_recovery;
use crate::percpu::get_percpu_by_idx;
use crate::pmm::{
    alloc_pages, flush_all_magazines_quiescent, free_pages, measure_fragmentation,
    resolve_frame_ptr, AllocFlags, PageOrder,
};
use crate::sched::preempt_tests::{begin_preemptive_epoch, drain_preemptive_epoch_and_audit};
use crate::sched::{
    current_cpu_idx, exit_current_thread, init_scheduler, note_current_thread_progress,
    set_preemptive_mode_active, try_spawn_thread_in_vspace, yield_now,
};
use crate::security::{SecDomain, SecFlags, SecLabel};
use crate::serial_println;
use crate::vspace::huge::verify_hugepage_2m_substage;
use crate::vspace::pte::{
    is_pcid_enabled, VSpaceError, VmProt, KERNEL_HALF_BASE, PAGE_SIZE_4K, USER_VADDR_LIMIT,
};
use crate::vspace::tlb::{
    reset_tlb_telemetry, set_shootdown_timeout_cycles_for_test, set_skip_ipi_for_stale_tlb_test,
    tlb_invariant_panic, tlb_telemetry,
};
use crate::vspace::{
    cpu_active_vspace, cr3_perf_counters, create_vspace, destroy_vspace,
    enforce_user_access_or_panic, map_4k, protect_4k, reset_cr3_perf_counters,
    switch_cpu_vspace_if_needed, unmap_4k, vspace_invariant_panic, vspace_owned_pt_frames,
    walk_vspace_address, KernelVSpaceSummary, VSpaceHandle, KERNEL_VSPACE_ID,
};

const BENCH_SAMPLES: usize = 512;
const SHOOTDOWN_SAMPLES: usize = 256;
const SMP_STRESS_VSPACES: usize = 4;
const SMP_STRESS_ROUNDS: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VSpaceFaultTest {
    None,
    WrongAspace,
    UsBypass,
    WriteRo,
    ExecNx,
    DoubleMap,
    DoubleUnmap,
    StaleTlb,
    ShootdownTimeout,
    ActiveFrameReuse,
}

impl VSpaceFaultTest {
    pub fn from_cmdline(cmdline: &str) -> Self {
        if cmdline.contains("vspace_fault=wrong_aspace") {
            Self::WrongAspace
        } else if cmdline.contains("vspace_fault=us_bypass") {
            Self::UsBypass
        } else if cmdline.contains("vspace_fault=write_ro") {
            Self::WriteRo
        } else if cmdline.contains("vspace_fault=exec_nx") {
            Self::ExecNx
        } else if cmdline.contains("vspace_fault=double_map") {
            Self::DoubleMap
        } else if cmdline.contains("vspace_fault=double_unmap") {
            Self::DoubleUnmap
        } else if cmdline.contains("vspace_fault=stale_tlb") {
            Self::StaleTlb
        } else if cmdline.contains("vspace_fault=shootdown_timeout") {
            Self::ShootdownTimeout
        } else if cmdline.contains("vspace_fault=active_frame_reuse") {
            Self::ActiveFrameReuse
        } else {
            Self::None
        }
    }
}

struct BenchBuf<const N: usize>(UnsafeCell<[u64; N]>);
// SAFETY: Mutated solely by a single designated benchmark initiator thread per phase.
unsafe impl<const N: usize> Sync for BenchBuf<N> {}

static BENCH_BUF_512: BenchBuf<BENCH_SAMPLES> = BenchBuf(UnsafeCell::new([0; BENCH_SAMPLES]));
static SHOOT_BUF_256: BenchBuf<SHOOTDOWN_SAMPLES> =
    BenchBuf(UnsafeCell::new([0; SHOOTDOWN_SAMPLES]));

static BENCH_TARGET_PEERS: AtomicUsize = AtomicUsize::new(0);
static BENCH_PEERS_READY: AtomicUsize = AtomicUsize::new(0);
static BENCH_STOP_PEERS: AtomicBool = AtomicBool::new(false);
static BENCH_SHARED_VS: AtomicU64 = AtomicU64::new(0);

static FAULT_PEER_WARMED: AtomicBool = AtomicBool::new(false);
static FAULT_UNMAP_DONE: AtomicBool = AtomicBool::new(false);

static STRESS_VS_HANDLES: BenchBuf<SMP_STRESS_VSPACES> =
    BenchBuf(UnsafeCell::new([0; SMP_STRESS_VSPACES]));
static STRESS_ONLINE_CPUS: AtomicUsize = AtomicUsize::new(1);
static STRESS_VS_RESIDENT: [AtomicUsize; SMP_STRESS_VSPACES] =
    [const { AtomicUsize::new(0) }; SMP_STRESS_VSPACES];
static STRESS_MAP_UNMAP_OPS: AtomicU64 = AtomicU64::new(0);
static STRESS_PROTECT_OPS: AtomicU64 = AtomicU64::new(0);
static STRESS_STALE_READS: AtomicU64 = AtomicU64::new(0);

#[inline]
fn pack_vs(h: VSpaceHandle) -> u64 {
    (h.id as u64) | ((h.generation as u64) << 16)
}

#[inline]
fn unpack_vs(raw: u64) -> VSpaceHandle {
    VSpaceHandle {
        id: (raw & 0xffff) as u16,
        generation: (raw >> 16) as u32,
    }
}

fn shell_sort(slice: &mut [u64]) {
    let n = slice.len();
    let mut gap = n / 2;
    while gap > 0 {
        for i in gap..n {
            let key = slice[i];
            let mut j = i;
            while j >= gap && slice[j - gap] > key {
                slice[j] = slice[j - gap];
                j -= gap;
            }
            slice[j] = key;
        }
        gap /= 2;
    }
}

fn stale_tlb_peer_worker(_arg: u64) {
    const VADDR: u64 = 0x0060_0000;
    const OLD_MAGIC: u64 = 0x1111_2222_3333_4444;
    // Warm this CPU's hardware TLB with `OLD_MAGIC` before CPU 0 unmaps without shootdown.
    // SAFETY: `VADDR` is mapped `USER_RW` in this worker's active `VSpace`.
    let first = unsafe { read_volatile(VADDR as *const u64) };
    assert_eq!(first, OLD_MAGIC);
    FAULT_PEER_WARMED.store(true, Ordering::Release);

    while !FAULT_UNMAP_DONE.load(Ordering::Acquire) {
        note_current_thread_progress();
        core::hint::spin_loop();
    }
    // SAFETY: Intentional read across an un-invalidated TLB entry for negative test `vspace_stale_tlb`.
    let second = unsafe { read_volatile(VADDR as *const u64) };
    let hw_stale_hit = second == OLD_MAGIC;
    let handled = tlb_telemetry().ipis_handled;
    if hw_stale_hit || handled == 0 {
        tlb_invariant_panic(format_args!(
            "Stale TLB hazard detected on cpu={} vaddr={:#x} hw_stale_hit={} ipis_handled={}",
            current_cpu_idx(),
            VADDR,
            hw_stale_hit,
            handled
        ));
    }
    exit_current_thread()
}

fn stale_tlb_initiator_worker(_arg: u64) {
    const VADDR: u64 = 0x0060_0000;
    let vs = unpack_vs(BENCH_SHARED_VS.load(Ordering::Acquire));
    let lbl = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 901, 1);

    while !FAULT_PEER_WARMED.load(Ordering::Acquire) {
        note_current_thread_progress();
        core::hint::spin_loop();
    }
    set_skip_ipi_for_stale_tlb_test(true);
    let old_f = unmap_4k(vs, VADDR).unwrap();
    let new_f = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::ZERO).unwrap();
    // SAFETY: `new_f` is owned by `lbl`.
    unsafe { write_volatile(resolve_frame_ptr(new_f, lbl) as *mut u64, 0x9999_8888_7777_6666) };
    map_4k(vs, VADDR, new_f, VmProt::USER_RW).unwrap();
    let _ = old_f;
    FAULT_UNMAP_DONE.store(true, Ordering::Release);

    loop {
        note_current_thread_progress();
        core::hint::spin_loop();
    }
}

fn wedged_cli_peer_worker(_arg: u64) {
    // Deliberately mask `RFLAGS.IF = 0` inside active `VSpace` WITHOUT cooperative shootdown polling.
    cli();
    FAULT_PEER_WARMED.store(true, Ordering::Release);
    loop {
        core::hint::spin_loop();
    }
}

fn timeout_initiator_worker(_arg: u64) {
    let vs = unpack_vs(BENCH_SHARED_VS.load(Ordering::Acquire));
    while !FAULT_PEER_WARMED.load(Ordering::Acquire) {
        note_current_thread_progress();
        core::hint::spin_loop();
    }
    // Peer CPU holds `IF == 0` in `vs.active_cpus`; `unmap_4k` must hit bounded TSC timeout (`exit=35`).
    let _ = unmap_4k(vs, 0x0061_0000);
    exit_current_thread()
}

fn run_negative_fault_test(fault: VSpaceFaultTest, online_cpus: usize, tsc_mhz: u64) {
    let lbl = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 900, 1);
    match fault {
        VSpaceFaultTest::None => {}
        VSpaceFaultTest::WrongAspace => {
            let vs_a = create_vspace(lbl).unwrap();
            let vs_b = create_vspace(lbl).unwrap();
            let f = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::ZERO).unwrap();
            map_4k(vs_a, 0x0040_0000, f, VmProt::USER_RW).unwrap();
            switch_cpu_vspace_if_needed(0, vs_b.id);
            serial_println!("[DRY-VSPACE-INFO] Testing wrong address-space hardware #PF at 0x400000...");
            // SAFETY: Intentional unmapped read in `vs_b` to trigger hardware `#PF` (`cr2=0x400000`).
            let _ = unsafe { read_volatile(0x0040_0000 as *const u64) };
        }
        VSpaceFaultTest::UsBypass => {
            let vs_a = create_vspace(lbl).unwrap();
            let f = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::ZERO).unwrap();
            map_4k(vs_a, 0x0041_0000, f, VmProt::SUPERVISOR_LOWER_RO).unwrap();
            enforce_user_access_or_panic(vs_a.id, 0x0041_0000, false, false);
        }
        VSpaceFaultTest::WriteRo => {
            let vs_a = create_vspace(lbl).unwrap();
            let f = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::ZERO).unwrap();
            map_4k(vs_a, 0x0050_0000, f, VmProt::USER_RW).unwrap();
            switch_cpu_vspace_if_needed(0, vs_a.id);
            // SAFETY: Mapped `USER_RW` initially, then protected `USER_RO` before write fault.
            unsafe {
                write_volatile(0x0050_0000 as *mut u64, 0xabcd_1234);
                protect_4k(vs_a, 0x0050_0000, VmProt::USER_RO).unwrap();
                assert_eq!(read_volatile(0x0050_0000 as *const u64), 0xabcd_1234);
                serial_println!("[DRY-VSPACE-INFO] Testing write-to-readonly hardware #PF at 0x500000...");
                write_volatile(0x0050_0000 as *mut u64, 0xdead_beef);
            }
        }
        VSpaceFaultTest::ExecNx => {
            let vs_a = create_vspace(lbl).unwrap();
            let f = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::ZERO).unwrap();
            let code_ptr = resolve_frame_ptr(f, lbl);
            // `mov eax, 42; ret` (`b8 2a 00 00 00 c3`).
            let stub: [u8; 6] = [0xb8, 0x2a, 0x00, 0x00, 0x00, 0xc3];
            // SAFETY: Writing 6-byte machine code stub into owned frame via HHDM.
            unsafe { core::ptr::copy_nonoverlapping(stub.as_ptr(), code_ptr, stub.len()) };
            map_4k(vs_a, 0x0050_1000, f, VmProt::SUPERVISOR_LOWER_RX).unwrap();
            switch_cpu_vspace_if_needed(0, vs_a.id);
            // SAFETY: `0x501000` is mapped executable (`NX=0, U/S=0`) for initial sanity check, then `NX=1`.
            unsafe {
                let func: extern "C" fn() -> u32 = core::mem::transmute(0x0050_1000usize);
                assert_eq!(func(), 42);
                protect_4k(vs_a, 0x0050_1000, VmProt::SUPERVISOR_LOWER_RO).unwrap();
                serial_println!("[DRY-VSPACE-INFO] Testing NX instruction-fetch hardware #PF at 0x501000...");
                let _ = func();
            }
        }
        VSpaceFaultTest::DoubleMap => {
            let vs_a = create_vspace(lbl).unwrap();
            let f1 = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::ZERO).unwrap();
            let f2 = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::ZERO).unwrap();
            map_4k(vs_a, 0x0042_0000, f1, VmProt::USER_RW).unwrap();
            if map_4k(vs_a, 0x0042_0000, f2, VmProt::USER_RW) == Err(VSpaceError::AlreadyMapped) {
                vspace_invariant_panic(format_args!(
                    "Double map detected on vspace={} vaddr=0x420000",
                    vs_a.id
                ));
            }
        }
        VSpaceFaultTest::DoubleUnmap => {
            let vs_a = create_vspace(lbl).unwrap();
            let f1 = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::ZERO).unwrap();
            map_4k(vs_a, 0x0042_0000, f1, VmProt::USER_RW).unwrap();
            let _ = unmap_4k(vs_a, 0x0042_0000).unwrap();
            if unmap_4k(vs_a, 0x0042_0000) == Err(VSpaceError::NotMapped) {
                vspace_invariant_panic(format_args!(
                    "Double unmap detected on vspace={} vaddr=0x420000",
                    vs_a.id
                ));
            }
        }
        VSpaceFaultTest::StaleTlb => {
            assert!(online_cpus >= 2, "vspace_stale_tlb requires smp >= 2");
            let peer_lbl = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 901, 1);
            let vs_s = create_vspace(peer_lbl).unwrap();
            let f_old = alloc_pages(PageOrder::Order4KiB, peer_lbl, AllocFlags::ZERO).unwrap();
            // SAFETY: Writing test pattern to `f_old`.
            unsafe {
                write_volatile(
                    resolve_frame_ptr(f_old, peer_lbl) as *mut u64,
                    0x1111_2222_3333_4444,
                )
            };
            map_4k(vs_s, 0x0060_0000, f_old, VmProt::USER_RW).unwrap();
            BENCH_SHARED_VS.store(pack_vs(vs_s), Ordering::Release);
            reset_tlb_telemetry();
            FAULT_PEER_WARMED.store(false, Ordering::Release);
            FAULT_UNMAP_DONE.store(false, Ordering::Release);

            set_preemptive_mode_active(true);
            init_scheduler(online_cpus);
            begin_preemptive_epoch(0xe1, 2);
            assert!(try_spawn_thread_in_vspace(1, 0, KERNEL_VSPACE_ID, stale_tlb_initiator_worker, 0, true).is_some());
            assert!(try_spawn_thread_in_vspace(1, 1, vs_s.id, stale_tlb_peer_worker, 0, true).is_some());
            drain_preemptive_epoch_and_audit(online_cpus);
        }
        VSpaceFaultTest::ShootdownTimeout => {
            assert!(online_cpus >= 2, "vspace_shootdown_timeout requires smp >= 2");
            set_shootdown_timeout_cycles_for_test(tsc_mhz.max(500) * 60_000);
            let vs_t = create_vspace(lbl).unwrap();
            let f = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::ZERO).unwrap();
            map_4k(vs_t, 0x0061_0000, f, VmProt::USER_RW).unwrap();
            BENCH_SHARED_VS.store(pack_vs(vs_t), Ordering::Release);
            FAULT_PEER_WARMED.store(false, Ordering::Release);

            set_preemptive_mode_active(true);
            init_scheduler(online_cpus);
            begin_preemptive_epoch(0xe2, 2);
            assert!(try_spawn_thread_in_vspace(1, 0, KERNEL_VSPACE_ID, timeout_initiator_worker, 0, true).is_some());
            assert!(try_spawn_thread_in_vspace(1, 1, vs_t.id, wedged_cli_peer_worker, 0, true).is_some());
            drain_preemptive_epoch_and_audit(online_cpus);
        }
        VSpaceFaultTest::ActiveFrameReuse => {
            let vs_a = create_vspace(lbl).unwrap();
            let f = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::ZERO).unwrap();
            map_4k(vs_a, 0x0043_0000, f, VmProt::USER_RW).unwrap();
            // Attempting to return `f` to `PMM` while `map_ref_count == 1` must trap immediately.
            free_pages(f, PageOrder::Order4KiB, lbl);
        }
    }
}

fn verify_4k_isolation_permissions_and_reclaim(summary: &KernelVSpaceSummary, online_cpus: usize) {
    flush_all_magazines_quiescent(online_cpus);
    let baseline = measure_fragmentation(online_cpus);

    let lbl_a = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 10, 1);
    let lbl_b = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 20, 1);
    let vs1 = create_vspace(lbl_a).expect("create_vspace(vs1)");
    let vs2 = create_vspace(lbl_b).expect("create_vspace(vs2)");
    let fa = alloc_pages(PageOrder::Order4KiB, lbl_a, AllocFlags::ZERO).unwrap();
    let fb = alloc_pages(PageOrder::Order4KiB, lbl_b, AllocFlags::ZERO).unwrap();

    // Canonical, NULL-guard, alignment, overflow, kernel-range, and default `W^X` rejections.
    assert_eq!(
        map_4k(vs1, 0x0001_0000_0000_0000, fa, VmProt::USER_RW),
        Err(VSpaceError::NonCanonicalAddress)
    );
    assert_eq!(map_4k(vs1, 0, fa, VmProt::USER_RW), Err(VSpaceError::NullGuardPage));
    assert_eq!(map_4k(vs1, 0x400123, fa, VmProt::USER_RW), Err(VSpaceError::UnalignedAddress));
    assert_eq!(
        map_4k(vs1, USER_VADDR_LIMIT, fa, VmProt::USER_RW),
        Err(VSpaceError::NonCanonicalAddress)
    );
    assert_eq!(
        map_4k(vs1, KERNEL_HALF_BASE, fa, VmProt::USER_RW),
        Err(VSpaceError::UserRangeViolation)
    );
    assert_eq!(
        map_4k(vs1, 0x0040_0000, fa, VmProt::USER_WX_FORBIDDEN),
        Err(VSpaceError::WxViolation)
    );

    // Map identical virtual address `0x0040_0000` in both `vs1` and `vs2` backed by distinct frames.
    assert_eq!(vspace_owned_pt_frames(vs1).unwrap(), 0);
    map_4k(vs1, 0x0040_0000, fa, VmProt::USER_RW).unwrap();
    assert_eq!(vspace_owned_pt_frames(vs1).unwrap(), 3);
    map_4k(vs2, 0x0040_0000, fb, VmProt::USER_RW).unwrap();

    switch_cpu_vspace_if_needed(0, vs1.id);
    verify_cpu_tss_and_canaries(0);
    let _ = verify_breakpoint_trap_recovery(get_percpu_by_idx(0));
    // Upper-half kernel mappings in `vs1` MUST remain supervisor-only (`effective_user == false`).
    let kwalk = walk_vspace_address(vs1.id, get_percpu_by_idx(0).boot_stack_base).unwrap();
    assert!(!kwalk.effective_user && kwalk.effective_writable && !kwalk.effective_exec);
    enforce_user_access_or_panic(vs1.id, 0x0040_0000, true, false);
    // SAFETY: `0x400000` is mapped `USER_RW` in `vs1` and `vs2` to private frames `fa` and `fb`.
    unsafe {
        write_volatile(0x0040_0000 as *mut u64, 0xaaaa_1111_2222_3333);
        switch_cpu_vspace_if_needed(0, vs2.id);
        assert_eq!(read_volatile(0x0040_0000 as *const u64), 0);
        write_volatile(0x0040_0000 as *mut u64, 0xbbbb_4444_5555_6666);
        switch_cpu_vspace_if_needed(0, vs1.id);
        assert_eq!(read_volatile(0x0040_0000 as *const u64), 0xaaaa_1111_2222_3333);
        protect_4k(vs1, 0x0040_0000, VmProt::USER_RO).unwrap();
        assert!(!walk_vspace_address(vs1.id, 0x0040_0000).unwrap().effective_writable);
        protect_4k(vs1, 0x0040_0000, VmProt::USER_RX).unwrap();
        let rx_walk = walk_vspace_address(vs1.id, 0x0040_0000).unwrap();
        assert!(rx_walk.effective_user && !rx_walk.effective_writable && rx_walk.effective_exec);
        enforce_user_access_or_panic(vs1.id, 0x0040_0000, false, true);
        assert_eq!(protect_4k(vs1, 0x0040_0000, VmProt::USER_WX_FORBIDDEN), Err(VSpaceError::WxViolation));
    }

    // Active `VSpace` (`active_cpus != 0`) cannot be destroyed while loaded in `CR3`.
    assert_eq!(destroy_vspace(vs1), Err(VSpaceError::VSpaceActiveBusy));
    switch_cpu_vspace_if_needed(0, KERNEL_VSPACE_ID);

    let out_a = unmap_4k(vs1, 0x0040_0000).unwrap();
    assert_eq!(out_a, fa);
    assert_eq!(vspace_owned_pt_frames(vs1).unwrap(), 0, "Empty PT/PD/PDPT must be reclaimed");
    let out_b = unmap_4k(vs2, 0x0040_0000).unwrap();
    assert_eq!(out_b, fb);

    free_pages(out_a, PageOrder::Order4KiB, lbl_a);
    free_pages(out_b, PageOrder::Order4KiB, lbl_b);
    destroy_vspace(vs1).unwrap();
    destroy_vspace(vs2).unwrap();
    flush_all_magazines_quiescent(online_cpus);

    let after = measure_fragmentation(online_cpus);
    let leaked = baseline.free_global_frames.saturating_sub(after.free_global_frames);
    assert_eq!(leaked, 0, "Frame leak after 4KiB VSpace lifecycle: {}", leaked);

    serial_println!(
        "[DRY-VSPACE] PASS: cpus={} boot_cr3={:#x} kernel_cr3={:#x} kernel_pt_frames={} reclaimed_boot_frames={} boot_reclaim_blocked_while_active={} kernel_wx_enforced={} wp_nxe_verified={} smep_enabled={} pcid_hw={} invpcid_hw={} pcid_enabled={} pcid_assumed=false isolated_vspaces_ok=true pt_reclaim_ok=true leaked_frames={}",
        online_cpus,
        summary.boot_cr3_phys,
        summary.kernel_cr3_phys,
        summary.kernel_pt_frames,
        summary.reclaimed_boot_frames,
        summary.boot_reclaim_blocked_while_active,
        summary.kernel_wx_enforced,
        summary.hw_caps.wp_enabled && summary.hw_caps.nxe_enabled,
        summary.hw_caps.smep_enabled,
        summary.hw_caps.has_pcid,
        summary.hw_caps.has_invpcid,
        is_pcid_enabled(),
        leaked
    );
}

fn shootdown_peer_holder_worker(_arg: u64) {
    let vs = unpack_vs(BENCH_SHARED_VS.load(Ordering::Acquire));
    assert_eq!(cpu_active_vspace(current_cpu_idx()), vs.id);
    // Touch mapped benchmark page once so this peer CPU holds a live hardware TLB entry.
    // SAFETY: `0x0080_0000` is mapped in `vs`.
    let _ = unsafe { read_volatile(0x0080_0000 as *const u64) };
    BENCH_PEERS_READY.fetch_add(1, Ordering::AcqRel);
    while !BENCH_STOP_PEERS.load(Ordering::Acquire) {
        note_current_thread_progress();
        core::hint::spin_loop();
    }
    exit_current_thread()
}

fn shootdown_initiator_worker(_arg: u64) {
    let target_peers = BENCH_TARGET_PEERS.load(Ordering::Acquire);
    let vs = unpack_vs(BENCH_SHARED_VS.load(Ordering::Acquire));
    while BENCH_PEERS_READY.load(Ordering::Acquire) < target_peers {
        note_current_thread_progress();
        core::hint::spin_loop();
    }
    // SAFETY: Single initiator owns `SHOOT_BUF_256`.
    let buf = unsafe { &mut *SHOOT_BUF_256.0.get() };
    for (i, slot) in buf.iter_mut().enumerate() {
        let prot = if (i & 1) == 0 { VmProt::USER_RO } else { VmProt::USER_RW };
        let t0 = rdtsc();
        protect_4k(vs, 0x0080_0000, prot).unwrap();
        *slot = rdtsc().wrapping_sub(t0);
        if (i & 15) == 0 {
            note_current_thread_progress();
        }
    }
    BENCH_STOP_PEERS.store(true, Ordering::Release);
    exit_current_thread()
}

fn run_smp_shootdown_bench_phase(vs: VSpaceHandle, peer_count: usize, online_cpus: usize) -> (u64, u64, u64) {
    BENCH_SHARED_VS.store(pack_vs(vs), Ordering::Release);
    BENCH_TARGET_PEERS.store(peer_count, Ordering::Release);
    BENCH_PEERS_READY.store(0, Ordering::Release);
    BENCH_STOP_PEERS.store(false, Ordering::Release);

    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xd0 + (peer_count as u8), 1 + peer_count);
    assert!(try_spawn_thread_in_vspace(1, 0, KERNEL_VSPACE_ID, shootdown_initiator_worker, 0, true).is_some());
    for p in 0..peer_count {
        assert!(try_spawn_thread_in_vspace(1, 1 + p, vs.id, shootdown_peer_holder_worker, p as u64, true).is_some());
    }
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);

    // SAFETY: Quiescent after epoch drain.
    let buf = unsafe { &mut *SHOOT_BUF_256.0.get() };
    shell_sort(buf);
    (buf[128], buf[243], buf[253])
}

fn measure_vspace_and_shootdown_benchmarks(online_cpus: usize) {
    let lbl = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 60, 1);
    let vs_a = create_vspace(lbl).unwrap();
    let vs_b = create_vspace(lbl).unwrap();
    let pin_frame = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::ZERO).unwrap();
    let work_frame = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::ZERO).unwrap();
    // Keep one anchor page mapped at `0x0080_0000` so `map_4k`/`unmap_4k` at `0x0080_1000` measures
    // steady-state leaf PTE operations (separate from PT allocation), and peer shootdowns have a valid page.
    map_4k(vs_a, 0x0080_0000, pin_frame, VmProt::USER_RW).unwrap();

    // SAFETY: Single-threaded BSP quiescent benchmark prior to SMP shootdown phase.
    let buf = unsafe { &mut *BENCH_BUF_512.0.get() };

    switch_cpu_vspace_if_needed(0, vs_a.id);
    for (i, slot) in buf.iter_mut().enumerate() {
        let dst = if (i & 1) == 0 { vs_b.id } else { vs_a.id };
        let t0 = rdtsc();
        switch_cpu_vspace_if_needed(0, dst);
        *slot = rdtsc().wrapping_sub(t0);
    }
    shell_sort(buf);
    let (cr3_p50, cr3_p95, cr3_p99) = (buf[256], buf[486], buf[506]);

    switch_cpu_vspace_if_needed(0, vs_a.id);
    for slot in buf.iter_mut() {
        let t0 = rdtsc();
        switch_cpu_vspace_if_needed(0, vs_a.id);
        *slot = rdtsc().wrapping_sub(t0);
    }
    shell_sort(buf);
    let cr3_noop_p50 = buf[256];
    switch_cpu_vspace_if_needed(0, KERNEL_VSPACE_ID);

    let mut unmap_samples = [0u64; BENCH_SAMPLES];
    for i in 0..BENCH_SAMPLES {
        let t_m = rdtsc();
        map_4k(vs_a, 0x0080_1000, work_frame, VmProt::USER_RW).unwrap();
        buf[i] = rdtsc().wrapping_sub(t_m);

        let t_u = rdtsc();
        let _ = unmap_4k(vs_a, 0x0080_1000).unwrap();
        unmap_samples[i] = rdtsc().wrapping_sub(t_u);
    }
    shell_sort(buf);
    let (map_p50, map_p95, map_p99) = (buf[256], buf[486], buf[506]);
    shell_sort(&mut unmap_samples);
    let (unmap_p50, unmap_p95, unmap_p99) = (unmap_samples[256], unmap_samples[486], unmap_samples[506]);

    switch_cpu_vspace_if_needed(0, vs_a.id);
    for (i, slot) in buf.iter_mut().enumerate() {
        let p = if (i & 1) == 0 { VmProt::USER_RO } else { VmProt::USER_RW };
        let t0 = rdtsc();
        protect_4k(vs_a, 0x0080_0000, p).unwrap();
        *slot = rdtsc().wrapping_sub(t0);
    }
    switch_cpu_vspace_if_needed(0, KERNEL_VSPACE_ID);
    shell_sort(buf);
    let (prot_p50, prot_p95, prot_p99) = (buf[256], buf[486], buf[506]);

    let (sd1_p50, sd1_p95, sd1_p99, sda_p50, sda_p95, sda_p99) = if online_cpus >= 2 {
        let (p1_50, p1_95, p1_99) = run_smp_shootdown_bench_phase(vs_a, 1, online_cpus);
        let (pa_50, pa_95, pa_99) =
            run_smp_shootdown_bench_phase(vs_a, online_cpus - 1, online_cpus);
        (p1_50, p1_95, p1_99, pa_50, pa_95, pa_99)
    } else {
        (0, 0, 0, 0, 0, 0)
    };

    let ret_pin = unmap_4k(vs_a, 0x0080_0000).unwrap();
    free_pages(ret_pin, PageOrder::Order4KiB, lbl);
    free_pages(work_frame, PageOrder::Order4KiB, lbl);
    destroy_vspace(vs_a).unwrap();
    destroy_vspace(vs_b).unwrap();
    flush_all_magazines_quiescent(online_cpus);

    serial_println!(
        "[DRY-VSPACE-BENCH] PASS: cpus={} samples={} cr3_switch_p50_cyc={} cr3_switch_p95_cyc={} cr3_switch_p99_cyc={} cr3_noop_p50_cyc={} map_4k_p50_cyc={} map_4k_p95_cyc={} map_4k_p99_cyc={} protect_4k_p50_cyc={} protect_4k_p95_cyc={} protect_4k_p99_cyc={} unmap_local_p50_cyc={} unmap_local_p95_cyc={} unmap_local_p99_cyc={} shootdown_1peer_p50_cyc={} shootdown_1peer_p95_cyc={} shootdown_1peer_p99_cyc={} shootdown_all_p50_cyc={} shootdown_all_p95_cyc={} shootdown_all_p99_cyc={}",
        online_cpus,
        BENCH_SAMPLES,
        cr3_p50,
        cr3_p95,
        cr3_p99,
        cr3_noop_p50,
        map_p50,
        map_p95,
        map_p99,
        prot_p50,
        prot_p95,
        prot_p99,
        unmap_p50,
        unmap_p95,
        unmap_p99,
        sd1_p50,
        sd1_p95,
        sd1_p99,
        sda_p50,
        sda_p95,
        sda_p99
    );
}

fn smp_vspace_stress_worker(worker_idx: u64) {
    let vs_slot = ((worker_idx as usize) / 2) % SMP_STRESS_VSPACES;
    // SAFETY: `STRESS_VS_HANDLES` is immutable across the stress epoch.
    let vs = unpack_vs(unsafe { (*STRESS_VS_HANDLES.0.get())[vs_slot] });
    let lbl = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 500 + (vs_slot as u16), 1);
    let my_vaddr = 0x00c0_0000 + (worker_idx * PAGE_SIZE_4K);
    let online = STRESS_ONLINE_CPUS.load(Ordering::Relaxed);

    for r in 0..SMP_STRESS_ROUNDS {
        assert_eq!(cpu_active_vspace(current_cpu_idx()), vs.id);
        let f = alloc_pages(PageOrder::Order4KiB, lbl, AllocFlags::ZERO).unwrap();
        let expected = 0x55aa_0000_0000_0000u64 ^ ((worker_idx + 1) << 32) ^ (r as u64 + 1);
        map_4k(vs, my_vaddr, f, VmProt::USER_RW).unwrap();
        if online >= 2 {
            STRESS_VS_RESIDENT[vs_slot].fetch_add(1, Ordering::AcqRel);
            let t0 = rdtsc();
            while STRESS_VS_RESIDENT[vs_slot].load(Ordering::Acquire) < 2
                && rdtsc().wrapping_sub(t0) < 25_000
            {
                core::hint::spin_loop();
            }
        }
        // SAFETY: `my_vaddr` is private to `worker_idx` inside `vs` and mapped `USER_RW`.
        unsafe {
            write_volatile(my_vaddr as *mut u64, expected);
            protect_4k(vs, my_vaddr, VmProt::USER_RO).unwrap();
            let got = read_volatile(my_vaddr as *const u64);
            if got != expected {
                STRESS_STALE_READS.fetch_add(1, Ordering::Relaxed);
            }
        }
        if online >= 2 {
            STRESS_VS_RESIDENT[vs_slot].fetch_sub(1, Ordering::AcqRel);
        }
        yield_now();
        assert_eq!(cpu_active_vspace(current_cpu_idx()), vs.id);
        // SAFETY: Still mapped `USER_RO` across `CR3` switches & cross-CPU migrations!
        let got_after_mig = unsafe { read_volatile(my_vaddr as *const u64) };
        if got_after_mig != expected {
            STRESS_STALE_READS.fetch_add(1, Ordering::Relaxed);
        }

        let reclaimed = unmap_4k(vs, my_vaddr).unwrap();
        free_pages(reclaimed, PageOrder::Order4KiB, lbl);
        STRESS_MAP_UNMAP_OPS.fetch_add(2, Ordering::Relaxed);
        STRESS_PROTECT_OPS.fetch_add(1, Ordering::Relaxed);
        note_current_thread_progress();
    }
    exit_current_thread()
}

fn run_smp_concurrent_vspace_stress(online_cpus: usize) {
    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);

    let mut handles = [VSpaceHandle { id: 0, generation: 0 }; SMP_STRESS_VSPACES];
    for (i, slot) in handles.iter_mut().enumerate() {
        let lbl = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, 500 + (i as u16), 1);
        *slot = create_vspace(lbl).unwrap();
        // SAFETY: Populated before spawning stress workers.
        unsafe { (*STRESS_VS_HANDLES.0.get())[i] = pack_vs(*slot) };
    }

    STRESS_ONLINE_CPUS.store(online_cpus, Ordering::Release);
    for slot in &STRESS_VS_RESIDENT {
        slot.store(0, Ordering::Release);
    }
    STRESS_MAP_UNMAP_OPS.store(0, Ordering::Release);
    STRESS_PROTECT_OPS.store(0, Ordering::Release);
    STRESS_STALE_READS.store(0, Ordering::Release);
    reset_cr3_perf_counters();
    reset_tlb_telemetry();

    let workers = (online_cpus * 2).clamp(8, 24);
    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    for w in 0..workers {
        let vs = handles[(w / 2) % SMP_STRESS_VSPACES];
        assert!(
            try_spawn_thread_in_vspace(1, w % online_cpus, vs.id, smp_vspace_stress_worker, w as u64, true)
                .is_some()
        );
    }
    begin_preemptive_epoch(0xe0, workers);
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);

    let (cr3_switches, cr3_noops) = cr3_perf_counters();
    let tlb_stats = tlb_telemetry();
    for h in handles {
        destroy_vspace(h).unwrap();
    }
    flush_all_magazines_quiescent(online_cpus);

    let after = measure_fragmentation(online_cpus);
    let leaked = before.free_global_frames.saturating_sub(after.free_global_frames);
    let stale = STRESS_STALE_READS.load(Ordering::Acquire);
    assert_eq!(stale, 0, "Stale TLB reads detected under SMP stress");
    assert_eq!(leaked, 0, "Frame leak detected after SMP VSpace stress: {}", leaked);

    serial_println!(
        "[DRY-VSPACE-SMP] PASS: cpus={} workers={} vspaces={} cr3_switches={} cr3_noops={} map_unmap_ops={} protect_ops={} shootdowns={} ipis_sent={} ipis_handled={} coop_acks={} stale_tlb_reads={} leaked_frames={}",
        online_cpus,
        workers,
        SMP_STRESS_VSPACES,
        cr3_switches,
        cr3_noops,
        STRESS_MAP_UNMAP_OPS.load(Ordering::Acquire),
        STRESS_PROTECT_OPS.load(Ordering::Acquire),
        tlb_stats.shootdowns,
        tlb_stats.ipis_sent,
        tlb_stats.ipis_handled,
        tlb_stats.coop_acks,
        stale,
        leaked
    );
}

pub fn verify_vspace_and_tlb(
    summary: &KernelVSpaceSummary,
    online_cpus: usize,
    tsc_mhz: u64,
    cmdline: &str,
) {
    let fault = VSpaceFaultTest::from_cmdline(cmdline);
    if fault != VSpaceFaultTest::None {
        run_negative_fault_test(fault, online_cpus, tsc_mhz);
        return;
    }

    verify_4k_isolation_permissions_and_reclaim(summary, online_cpus);
    verify_hugepage_2m_substage(online_cpus);
    measure_vspace_and_shootdown_benchmarks(online_cpus);
    run_smp_concurrent_vspace_stress(online_cpus);
    crate::vspace::tlb_debt::verify_tlb_debt_and_oversubscription(online_cpus);
}
