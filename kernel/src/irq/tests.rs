// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `Milestone 2A-1` (`P1 First Interaction`) Security, Race & Interactive Session Suite:
//! - Narrow `IoPortCap` & `IrqCap` positive/negative authorization checks.
//! - Deterministic double-`SYS_IRQ_ACK` (`SysError::Already`), spurious/masked pin containment,
//!   and burst storm clamping (`STORM_BURST_LIMIT`).
//! - Preemptive SMP concurrent `revoke` vs live hardware APIC interrupt race + zero-lost-wakeup proof.
//! - Fail-closed `irq_fault=` negative QEMU test modes (`exit=35`).
//! - Live `interactive=1` Ring 3 `dry-init -> dry-input -> dry-shell -> dry-display` session runner.

use core::ptr::copy_nonoverlapping;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::apic::{send_fixed_ipi, VEC_EXT_IRQ_BASE};
use crate::arch::{cli, sti};
use crate::cspace::cap::{CapError, CapPtr};
use crate::cspace::objects::cspace_alloc_frame;
use crate::cspace::revoke::{attach_cap_mapping_record, cspace_delete_cap, cspace_destroy_object, cspace_revoke_children};
use crate::cspace::{create_cspace, cspace_derive, cspace_lookup_fast, cspace_transfer, destroy_cspace};
use crate::display::{is_framebuffer_available, mint_root_framebuffer_cap};
use crate::ipc::notif::{notif_create, notif_wait};
use crate::irq::ioport::{
    mint_root_ioport_cap, sys_ioport_rw, I8042_DATA_PORT, I8042_STATUS_CMD_PORT, IOPORT_OP_INB,
    IOPORT_OP_OUTB,
};
use crate::irq::{
    ioapic_present, irq_telemetry, mint_root_irq_cap, sys_irq_ack, sys_irq_bind, IRQF_ONESHOT,
    IRQ_ACK_MASK, IRQ_ACK_UNBIND, IRQ_ACK_UNMASK, IRQ_KEYBOARD_PS2, IRQ_TEST_SW_LINE,
    STORM_BURST_LIMIT,
};
use crate::limine::get_boot_module;
use crate::percpu::get_percpu_by_idx;
use crate::pmm::{flush_all_magazines_quiescent, measure_fragmentation, PageOrder};
use crate::proc::supervisor::mint_boot_module_cap;
use crate::proc::{reclaim_process, spawn_elf_process_with, ProcessOutcome, ReclaimStatus, SpawnOptions};
use crate::sched::preempt_tests::{begin_preemptive_epoch, drain_preemptive_epoch_and_audit};
use crate::sched::{
    disarm_epoch_watchdog, exit_current_thread, idle_dispatch_once, init_scheduler,
    note_current_thread_progress, set_ap_epoch_kind, set_preemptive_mode_active,
    start_cpu_timer_for_phase, stop_cpu_timer_for_phase, try_spawn_thread_bound, yield_now,
    ApEpochKind, AP_PHASE_EPOCH, PHASE_PENDING_THREADS,
};
use crate::security::{encode_compartment, CapRights, SecDomain, SecFlags, SecLabel};
use crate::serial_println;
use crate::uaccess::SysError;
use crate::vspace::pte::VmProt;
use crate::vspace::{map_4k, KERNEL_VSPACE_ID};

const BOOT_INFO_VADDR: u64 = 0x0070_0000;
const ROOT_INIT_BOOT_MAGIC: u64 = 0x4452_595f_494e_4954;

#[repr(C)]
#[derive(Clone, Copy)]
struct P1InitBootInfo {
    magic: u64,
    echo_mod_cap: u64,
    display_mod_cap: u64,
    fb_cap: u64,
    input_mod_cap: u64,
    shell_mod_cap: u64,
    irq1_cap: u64,
    i8042_port_cap: u64,
    session_mode: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IrqFaultCmdline {
    None,
    UnauthIoPort,
    UnauthIrqAck,
    DoubleIrqAck,
    DisplayCapLeak,
}

impl IrqFaultCmdline {
    pub fn from_cmdline(cmd: &str) -> Self {
        if cmd.contains("irq_fault=unauth_ioport") {
            Self::UnauthIoPort
        } else if cmd.contains("irq_fault=unauth_irq_ack") {
            Self::UnauthIrqAck
        } else if cmd.contains("irq_fault=double_irq_ack") {
            Self::DoubleIrqAck
        } else if cmd.contains("irq_fault=display_cap_leak") {
            Self::DisplayCapLeak
        } else {
            Self::None
        }
    }
}

fn irq_invariant_panic(args: core::fmt::Arguments<'_>) -> ! {
    crate::enter_fatal_or_halt();
    crate::serial::emergency_print(format_args!("\r\n[DRY-IRQ-FAIL] {}\r\n", args));
    panic!("IRQ / IoPort capability security invariant failure");
}

pub fn maybe_run_irq_fault_test(cmdline: &str, _online_cpus: usize) {
    let mode = IrqFaultCmdline::from_cmdline(cmdline);
    if mode == IrqFaultCmdline::None {
        return;
    }

    let root_lbl = SecLabel::new(SecDomain::RootSystem, SecFlags::NONE, 1, 1);
    let user_lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(9, 2),
        1,
    );
    let (rcs, _) = create_cspace(root_lbl, 0x73).unwrap();
    let (ucs, _) = create_cspace(user_lbl, 0x74).unwrap();

    match mode {
        IrqFaultCmdline::None => unreachable!(),
        IrqFaultCmdline::UnauthIoPort => {
            let io_cap =
                mint_root_ioport_cap(rcs, root_lbl, &[I8042_DATA_PORT, I8042_STATUS_CMD_PORT])
                    .unwrap();
            let e_user = sys_ioport_rw(ucs, user_lbl, io_cap, IOPORT_OP_INB, 0x0060, 0).unwrap_err();
            let e_port = sys_ioport_rw(rcs, root_lbl, io_cap, IOPORT_OP_INB, 0x0061, 0).unwrap_err();
            let e_exit = sys_ioport_rw(rcs, root_lbl, io_cap, IOPORT_OP_OUTB, 0x00f4, 0x10).unwrap_err();
            assert_eq!(e_user, SysError::Perm);
            assert_eq!(e_port, SysError::Perm);
            assert_eq!(e_exit, SysError::Perm);
            irq_invariant_panic(format_args!(
                "Unauthorized port I/O blocked cleanly (user={:?}, adj_0x61={:?}, exit_0xf4={:?})",
                e_user, e_port, e_exit
            ));
        }
        IrqFaultCmdline::UnauthIrqAck => {
            let irq_cap = mint_root_irq_cap(rcs, root_lbl, IRQ_TEST_SW_LINE, 1 << 15).unwrap();
            let xfer_err = cspace_transfer(
                rcs,
                root_lbl,
                irq_cap,
                ucs,
                CapRights::SIGNAL,
                0,
                0,
                0,
            )
            .unwrap_err();
            let ack_err = sys_irq_ack(ucs, user_lbl, irq_cap, IRQ_ACK_UNMASK).unwrap_err();
            assert_eq!(xfer_err, CapError::TransferEscalation);
            assert_eq!(ack_err, SysError::Perm);
            irq_invariant_panic(format_args!(
                "Unauthorized IRQ ack / Userspace delegation blocked (xfer={:?}, ack={:?})",
                xfer_err, ack_err
            ));
        }
        IrqFaultCmdline::DoubleIrqAck => {
            let irq_cap = mint_root_irq_cap(rcs, root_lbl, IRQ_TEST_SW_LINE, 1 << 15).unwrap();
            let (ntf_cap, _) = notif_create(rcs, root_lbl, root_lbl, 0).unwrap();
            let vec = sys_irq_bind(rcs, root_lbl, irq_cap, ntf_cap, IRQF_ONESHOT).unwrap();
            sti();
            send_fixed_ipi(get_percpu_by_idx(0).lapic_id, vec);
            cli();
            let _ = notif_wait(rcs, root_lbl, ntf_cap, true, 0).unwrap();
            let first = sys_irq_ack(rcs, root_lbl, irq_cap, IRQ_ACK_UNMASK);
            let second = sys_irq_ack(rcs, root_lbl, irq_cap, IRQ_ACK_UNMASK).unwrap_err();
            assert_eq!(first, Ok(1));
            assert_eq!(second, SysError::Already);
            irq_invariant_panic(format_args!(
                "Double SYS_IRQ_ACK deterministically rejected (first={:?}, second={:?})",
                first, second
            ));
        }
        IrqFaultCmdline::DisplayCapLeak => {
            let fb_or_io_err = if is_framebuffer_available() {
                let fb_cap = mint_root_framebuffer_cap(rcs, root_lbl).unwrap();
                cspace_transfer(
                    rcs,
                    root_lbl,
                    fb_cap,
                    ucs,
                    CapRights(CapRights::READ.0 | CapRights::MAP.0),
                    0,
                    0,
                    0,
                )
                .unwrap_err()
            } else {
                // Headless `-vga none` fallback: verify `RootSystem` data frame without `READ_ONLY_EXPORT`
                // cannot leak into `Userspace`.
                let (fr, _) =
                    cspace_alloc_frame(rcs, root_lbl, root_lbl, PageOrder::Order4KiB, false, 0)
                        .unwrap();
                cspace_transfer(rcs, root_lbl, fr, ucs, CapRights::READ, 0, 0, 0).unwrap_err()
            };
            assert_eq!(fb_or_io_err, CapError::TransferEscalation);
            irq_invariant_panic(format_args!(
                "Display capability leak into Userspace shell compartment rejected ({:?})",
                fb_or_io_err
            ));
        }
    }
}

static RACE_CS: AtomicU64 = AtomicU64::new(0);
static RACE_ROOT_IRQ: AtomicU64 = AtomicU64::new(0);
static RACE_CHILD_IRQ: AtomicU64 = AtomicU64::new(0);
static RACE_NTF: AtomicU64 = AtomicU64::new(0);
static RACE_WAKES_OK: AtomicUsize = AtomicUsize::new(0);
static RACE_REVOKE_PHASE: AtomicBool = AtomicBool::new(false);
static RACE_REVOKED_OBSERVED: AtomicBool = AtomicBool::new(false);

fn smp_irq_driver_worker(rounds: u64) {
    let cs = RACE_CS.load(Ordering::Acquire) as u16;
    let child_irq = CapPtr(RACE_CHILD_IRQ.load(Ordering::Acquire));
    let ntf = CapPtr(RACE_NTF.load(Ordering::Acquire));
    let drv_lbl = SecLabel::new(
        SecDomain::RootSystem,
        SecFlags::NONE,
        encode_compartment(0, 78),
        1,
    );

    for _ in 0..(rounds as usize) {
        let bits = notif_wait(cs, drv_lbl, ntf, false, 0).expect("race notif_wait");
        assert_ne!(bits, 0);
        sys_irq_ack(cs, drv_lbl, child_irq, IRQ_ACK_UNMASK).expect("race sys_irq_ack");
        RACE_WAKES_OK.fetch_add(1, Ordering::AcqRel);
        note_current_thread_progress();
    }

    RACE_REVOKE_PHASE.store(true, Ordering::Release);
    loop {
        match notif_wait(cs, drv_lbl, ntf, true, 0) {
            Ok(_) | Err(SysError::WouldBlock) => {}
            Err(SysError::Revoked) => {
                RACE_REVOKED_OBSERVED.store(true, Ordering::Release);
                break;
            }
            Err(other) => panic!("Unexpected notif_wait error {:?}", other),
        }
        match sys_irq_ack(cs, drv_lbl, child_irq, IRQ_ACK_UNMASK) {
            Ok(_) | Err(SysError::Already) => {}
            Err(SysError::Revoked) => {
                RACE_REVOKED_OBSERVED.store(true, Ordering::Release);
                break;
            }
            Err(other) => panic!("Unexpected sys_irq_ack error {:?}", other),
        }
        note_current_thread_progress();
        yield_now();
    }
    exit_current_thread()
}

fn smp_irq_injector_and_revoker(packed: u64) {
    let rounds = (packed & 0xffff) as usize;
    let dest_lapic = (packed >> 16) as u32;
    let cs = RACE_CS.load(Ordering::Acquire) as u16;
    let root_irq = CapPtr(RACE_ROOT_IRQ.load(Ordering::Acquire));
    let drv_lbl = SecLabel::new(
        SecDomain::RootSystem,
        SecFlags::NONE,
        encode_compartment(0, 78),
        1,
    );
    let vec = VEC_EXT_IRQ_BASE + IRQ_TEST_SW_LINE;

    for target in 1..=rounds {
        while RACE_WAKES_OK.load(Ordering::Acquire) < target {
            send_fixed_ipi(dest_lapic, vec);
            note_current_thread_progress();
            yield_now();
        }
    }

    while !RACE_REVOKE_PHASE.load(Ordering::Acquire) {
        note_current_thread_progress();
        yield_now();
    }
    // Fire concurrent APIC interrupts right around live capability revocation!
    send_fixed_ipi(dest_lapic, vec);
    let revoked_n = cspace_revoke_children(cs, drv_lbl, root_irq, 0).expect("revoke child irq");
    assert_eq!(revoked_n, 1);
    send_fixed_ipi(dest_lapic, vec);
    exit_current_thread()
}

#[inline(never)]
pub fn verify_irq_and_ioport_security(cmdline: &str, online_cpus: usize) {
    maybe_run_irq_fault_test(cmdline, online_cpus);

    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);

    let root_lbl = SecLabel::new(
        SecDomain::RootSystem,
        SecFlags::NONE,
        encode_compartment(0, 78),
        1,
    );
    let user_lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(9, 78),
        1,
    );
    let (rcs, _) = create_cspace(root_lbl, 0x75).unwrap();
    let (ucs, _) = create_cspace(user_lbl, 0x76).unwrap();

    // 1. Narrow `IoPortCap` security & denylist checks.
    assert_eq!(
        mint_root_ioport_cap(ucs, user_lbl, &[I8042_DATA_PORT]),
        Err(CapError::DomainViolation)
    );
    for forbidden in [0x0020u16, 0x0040, 0x0061, 0x0062, 0x0070, 0x00a0, 0x00f4, 0x03f8, 0x0cf8] {
        assert_eq!(
            mint_root_ioport_cap(rcs, root_lbl, &[forbidden]),
            Err(CapError::RightsAmplification)
        );
    }

    let io_root =
        mint_root_ioport_cap(rcs, root_lbl, &[I8042_DATA_PORT, I8042_STATUS_CMD_PORT]).unwrap();
    let io_ro = cspace_derive(rcs, root_lbl, io_root, CapRights::READ, 0, 0, 0).unwrap();
    let _status = sys_ioport_rw(rcs, root_lbl, io_ro, IOPORT_OP_INB, 0x0064, 0).unwrap();
    assert_eq!(
        sys_ioport_rw(rcs, root_lbl, io_ro, IOPORT_OP_OUTB, 0x0064, 0xaa),
        Err(SysError::Perm)
    );
    assert_eq!(
        sys_ioport_rw(rcs, root_lbl, io_root, IOPORT_OP_INB, 0x0062, 0),
        Err(SysError::Perm)
    );
    assert_eq!(
        sys_ioport_rw(ucs, user_lbl, io_root, IOPORT_OP_INB, 0x0060, 0),
        Err(SysError::Perm)
    );
    assert_eq!(
        cspace_transfer(rcs, root_lbl, io_root, ucs, CapRights::READ, 0, 0, 0),
        Err(CapError::TransferEscalation)
    );

    // 2. One-shot `IrqCap` lifecycle, double-ack protection & storm burst clamping.
    let irq_root = mint_root_irq_cap(rcs, root_lbl, IRQ_TEST_SW_LINE, 1u64 << 15).unwrap();
    let (ntf_cap, _) = notif_create(rcs, root_lbl, root_lbl, 0).unwrap();
    assert_eq!(
        sys_irq_ack(rcs, root_lbl, irq_root, IRQ_ACK_UNMASK),
        Err(SysError::Inval)
    );
    let vec = sys_irq_bind(rcs, root_lbl, irq_root, ntf_cap, IRQF_ONESHOT).unwrap();
    assert_eq!(
        sys_irq_ack(rcs, root_lbl, irq_root, IRQ_ACK_UNMASK),
        Err(SysError::Already)
    );

    let (_, _, spur_before, storm_before) = irq_telemetry();
    let bsp_lapic = get_percpu_by_idx(0).lapic_id;
    sti();
    send_fixed_ipi(bsp_lapic, vec);
    // Second vector while `oneshot_pending_ack == true` must be masked/absorbed cleanly!
    send_fixed_ipi(bsp_lapic, vec);
    cli();
    let bits = notif_wait(rcs, root_lbl, ntf_cap, true, 0).unwrap();
    assert_eq!(bits, 1u64 << 15);
    let (_, _, spur_after, _) = irq_telemetry();
    assert!(spur_after > spur_before, "Expected pending one-shot pin guard to count duplicate vector");

    assert_eq!(sys_irq_ack(rcs, root_lbl, irq_root, IRQ_ACK_UNMASK), Ok(1));
    assert_eq!(
        sys_irq_ack(rcs, root_lbl, irq_root, IRQ_ACK_UNMASK),
        Err(SysError::Already)
    );

    // Burst > `STORM_BURST_LIMIT` inside `STORM_WINDOW_TSC` must trigger deterministic storm clamp.
    for _ in 0..=(STORM_BURST_LIMIT + 8) {
        sti();
        send_fixed_ipi(bsp_lapic, vec);
        cli();
        let _ = notif_wait(rcs, root_lbl, ntf_cap, true, 0);
        let _ = sys_irq_ack(rcs, root_lbl, irq_root, IRQ_ACK_UNMASK);
    }
    let (_, _, _, storm_after) = irq_telemetry();
    assert!(
        storm_after > storm_before,
        "Expected IRQ storm burst limiter to clamp >{} rapid interrupts",
        STORM_BURST_LIMIT
    );
    sys_irq_ack(rcs, root_lbl, irq_root, IRQ_ACK_MASK).unwrap();
    sys_irq_ack(rcs, root_lbl, irq_root, IRQ_ACK_UNBIND).unwrap();

    // 3. SMP lost-wakeup & concurrent `revoke` vs hardware interrupt stress.
    let child_irq = cspace_derive(
        rcs,
        root_lbl,
        irq_root,
        CapRights(CapRights::READ.0 | CapRights::SIGNAL.0 | CapRights::WAIT.0),
        0,
        0,
        0,
    )
    .unwrap();
    let _ = sys_irq_bind(rcs, root_lbl, child_irq, ntf_cap, IRQF_ONESHOT).unwrap();

    let rounds = 48usize;
    RACE_CS.store(rcs as u64, Ordering::Release);
    RACE_ROOT_IRQ.store(irq_root.0, Ordering::Release);
    RACE_CHILD_IRQ.store(child_irq.0, Ordering::Release);
    RACE_NTF.store(ntf_cap.0, Ordering::Release);
    RACE_WAKES_OK.store(0, Ordering::Release);
    RACE_REVOKE_PHASE.store(false, Ordering::Release);
    RACE_REVOKED_OBSERVED.store(false, Ordering::Release);

    let drv_cpu = 0usize;
    let inj_cpu = if online_cpus >= 2 { 1 } else { 0 };
    let drv_lapic = get_percpu_by_idx(drv_cpu).lapic_id;

    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xf0, 2);
    let _ = try_spawn_thread_bound(
        2,
        drv_cpu,
        KERNEL_VSPACE_ID,
        rcs,
        root_lbl,
        smp_irq_driver_worker,
        rounds as u64,
        true,
    )
    .unwrap();
    let _ = try_spawn_thread_bound(
        1,
        inj_cpu,
        KERNEL_VSPACE_ID,
        rcs,
        root_lbl,
        smp_irq_injector_and_revoker,
        (rounds as u64) | ((drv_lapic as u64) << 16),
        true,
    )
    .unwrap();
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);

    assert_eq!(RACE_WAKES_OK.load(Ordering::Acquire), rounds);
    assert!(RACE_REVOKED_OBSERVED.load(Ordering::Acquire));

    cspace_delete_cap(rcs, root_lbl, io_ro, 0).unwrap();
    cspace_destroy_object(rcs, root_lbl, irq_root, 0).unwrap();
    cspace_destroy_object(rcs, root_lbl, io_root, 0).unwrap();
    cspace_destroy_object(rcs, root_lbl, ntf_cap, 0).unwrap();
    destroy_cspace(rcs).unwrap();
    destroy_cspace(ucs).unwrap();

    flush_all_magazines_quiescent(online_cpus);
    let after = measure_fragmentation(online_cpus);
    let leaked = before
        .free_global_frames
        .saturating_sub(after.free_global_frames);
    assert_eq!(leaked, 0, "Frame leak in verify_irq_and_ioport_security: {}", leaked);

    serial_println!(
        "[DRY-IRQ] PASS: cpus={} ioapic={} ioport_guards=ok double_ack_rejected=true storm_clamped={} revoke_race_ok=true lost_wakeups=0 leaked_frames={}",
        online_cpus,
        ioapic_present(),
        storm_after - storm_before,
        leaked
    );

    crate::irq::bench::run_irq_and_ioport_benchmarks(online_cpus);
}

/// Launch the live interactive Ring 3 `P1` session (`dry-init` + `dry-input` + `dry-shell` + `dry-display`)
/// when `interactive=1` is present on the kernel command line.
#[inline(never)]
pub fn maybe_run_p1_interactive_session(cmdline: &str, online_cpus: usize) {
    if !cmdline.contains("interactive=1") {
        return;
    }

    let init_elf = get_boot_module("dry-init.elf").expect("missing dry-init.elf");
    let _ = get_boot_module("dry-input.elf").expect("missing dry-input.elf");
    let _ = get_boot_module("dry-shell.elf").expect("missing dry-shell.elf");

    let session_mode = if cmdline.contains("p1_test_crash=1") {
        2u64
    } else {
        1u64
    };

    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);

    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    disarm_epoch_watchdog();
    PHASE_PENDING_THREADS.store(1, Ordering::Release);

    let root_lbl = SecLabel::new(SecDomain::RootSystem, SecFlags::NONE, 0, 1);
    let init_proc = spawn_elf_process_with(
        init_elf,
        SpawnOptions {
            subj_label: root_lbl,
            target_cpu: 0,
            arg0: BOOT_INFO_VADDR,
            arg1: session_mode,
            user_fs_base: 0,
            user_gs_base: 0,
            trace_lifecycle: false,
        },
        |cs, lbl, vs_cap, vs_h| {
            let echo_cap = mint_boot_module_cap(cs, lbl, "dry-echo.elf")?;
            let disp_cap = mint_boot_module_cap(cs, lbl, "dry-display.elf").unwrap_or(CapPtr::NULL);
            let input_cap = mint_boot_module_cap(cs, lbl, "dry-input.elf")?;
            let shell_cap = mint_boot_module_cap(cs, lbl, "dry-shell.elf")?;
            let fb_cap = if is_framebuffer_available() {
                mint_root_framebuffer_cap(cs, lbl)?
            } else {
                CapPtr::NULL
            };
            let irq1_cap = mint_root_irq_cap(cs, lbl, IRQ_KEYBOARD_PS2, 1u64 << IRQ_KEYBOARD_PS2)?;
            let i8042_cap =
                mint_root_ioport_cap(cs, lbl, &[I8042_DATA_PORT, I8042_STATUS_CMD_PORT])?;

            let (info_fr_cap, info_fh) =
                cspace_alloc_frame(cs, lbl, lbl, PageOrder::Order4KiB, false, 0)?;
            let boot_info = P1InitBootInfo {
                magic: ROOT_INIT_BOOT_MAGIC,
                echo_mod_cap: echo_cap.0,
                display_mod_cap: disp_cap.0,
                fb_cap: fb_cap.0,
                input_mod_cap: input_cap.0,
                shell_mod_cap: shell_cap.0,
                irq1_cap: irq1_cap.0,
                i8042_port_cap: i8042_cap.0,
                session_mode,
            };
            // SAFETY: `info_fh` is a freshly allocated, zeroed 4 KiB frame.
            unsafe {
                copy_nonoverlapping(
                    (&raw const boot_info).cast::<u8>(),
                    info_fh.hhdm_ptr(),
                    core::mem::size_of::<P1InitBootInfo>(),
                );
            }
            map_4k(vs_h, BOOT_INFO_VADDR, info_fh, VmProt::USER_RO)
                .map_err(|_| CapError::PagingError)?;
            attach_cap_mapping_record(
                cspace_lookup_fast(cs, lbl, info_fr_cap, CapRights::READ, 0)?.global_slot,
                info_fr_cap.slot_gen(),
                vs_h.id,
                vs_h.generation,
                BOOT_INFO_VADDR,
                false,
            )?;
            let _ = vs_cap;
            Ok((BOOT_INFO_VADDR, session_mode))
        },
    )
    .expect("Failed to spawn P1 dry-init.elf");

    serial_println!(
        "[DRY-P1-READY] cpus={} mode={} fb={} ioapic={}",
        online_cpus,
        session_mode,
        is_framebuffer_available(),
        ioapic_present()
    );

    // Release all APs into preemptive scheduling without a fixed test timeout and dispatch on BSP
    // until `dry-shell` receives `shutdown` and `dry-init` reaps all services cleanly.
    if online_cpus > 1 {
        set_ap_epoch_kind(ApEpochKind::PreemptiveSched);
        AP_PHASE_EPOCH.fetch_add(1, Ordering::AcqRel);
    }
    let bsp_pcpu = get_percpu_by_idx(0);
    start_cpu_timer_for_phase(bsp_pcpu);
    sti();
    while PHASE_PENDING_THREADS.load(Ordering::Acquire) > 0 {
        if crate::is_panicking() {
            crate::arch::halt_forever();
        }
        if !idle_dispatch_once(0) {
            core::hint::spin_loop();
        }
    }
    stop_cpu_timer_for_phase(bsp_pcpu);
    cli();
    set_preemptive_mode_active(false);

    let ReclaimStatus::ReclaimedNow(outcome) =
        reclaim_process(init_proc.handle).expect("Failed to reclaim P1 dry-init")
    else {
        panic!("Expected ReclaimedNow for P1 dry-init");
    };
    assert_eq!(
        outcome,
        ProcessOutcome::Exited(0),
        "P1 dry-init exited with unexpected outcome {:?}",
        outcome
    );

    flush_all_magazines_quiescent(online_cpus);
    let after = measure_fragmentation(online_cpus);
    let leaked = before
        .free_global_frames
        .saturating_sub(after.free_global_frames);
    assert_eq!(leaked, 0, "Frame leak after P1 interactive session: {}", leaked);

    let (ext_irqs, irq_acks, _, _) = irq_telemetry();
    let (io_ops, io_denied) = crate::irq::ioport::ioport_telemetry();
    serial_println!(
        "[DRY-P1-SESSION] PASS: cpus={} ext_irqs={} irq_acks={} ioport_ops={} ioport_denied={} leaked_frames={}",
        online_cpus,
        ext_irqs,
        irq_acks,
        io_ops,
        io_denied,
        leaked
    );
}
