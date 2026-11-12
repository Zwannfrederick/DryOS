// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `Milestone 2A-1` (`P1 First Interaction`) IRQ, Port I/O & Wakeup Microbenchmarks (`p50/p95/p99`).

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::apic::{send_fixed_ipi, VEC_EXT_IRQ_BASE};
use crate::arch::{cli, rdtsc, sti};
use crate::cspace::cap::CapPtr;
use crate::cspace::revoke::{cspace_delete_cap, cspace_destroy_object};
use crate::cspace::{create_cspace, cspace_derive, destroy_cspace};
use crate::ipc::notif::{notif_create, notif_wait};
use crate::irq::ioport::{mint_root_ioport_cap, sys_ioport_rw, I8042_STATUS_CMD_PORT, IOPORT_OP_INB};
use crate::irq::{
    mint_root_irq_cap, sys_irq_ack, sys_irq_bind, IRQF_ONESHOT, IRQ_ACK_UNMASK, IRQ_TEST_SW_LINE,
};
use crate::percpu::get_percpu_by_idx;
use crate::pmm::{flush_all_magazines_quiescent, measure_fragmentation};
use crate::sched::preempt_tests::{begin_preemptive_epoch, drain_preemptive_epoch_and_audit};
use crate::sched::{
    exit_current_thread, init_scheduler, note_current_thread_progress, set_preemptive_mode_active,
    try_spawn_thread_bound, yield_now,
};
use crate::security::{encode_compartment, CapRights, SecDomain, SecFlags, SecLabel};
use crate::serial_println;
use crate::vspace::KERNEL_VSPACE_ID;

const SAMPLES: usize = 64;

static BENCH_DRV_CS: AtomicU64 = AtomicU64::new(0);
static BENCH_IRQ_CAP: AtomicU64 = AtomicU64::new(0);
static BENCH_NTF_CAP: AtomicU64 = AtomicU64::new(0);
static BENCH_SEND_TSC: AtomicU64 = AtomicU64::new(0);
static BENCH_WAITER_ARMED: AtomicBool = AtomicBool::new(false);
static BENCH_STEP_DONE: AtomicBool = AtomicBool::new(false);
static BENCH_E2E_SAMPLES: [AtomicU64; SAMPLES] = [const { AtomicU64::new(0) }; SAMPLES];

fn sort_percentiles(buf: &mut [u64]) -> (u64, u64, u64) {
    let n = buf.len();
    for i in 1..n {
        let key = buf[i];
        let mut j = i;
        while j > 0 && buf[j - 1] > key {
            buf[j] = buf[j - 1];
            j -= 1;
        }
        buf[j] = key;
    }
    (buf[n / 2], buf[(n * 95) / 100], buf[(n * 99) / 100])
}

fn irq_bench_waiter_worker(rounds: u64) {
    let cs = BENCH_DRV_CS.load(Ordering::Acquire) as u16;
    let irq_cap = CapPtr(BENCH_IRQ_CAP.load(Ordering::Acquire));
    let ntf_cap = CapPtr(BENCH_NTF_CAP.load(Ordering::Acquire));
    let drv_lbl = SecLabel::new(
        SecDomain::RootSystem,
        SecFlags::NONE,
        encode_compartment(0, 77),
        1,
    );

    for i in 0..(rounds as usize) {
        BENCH_WAITER_ARMED.store(true, Ordering::Release);
        let bits = notif_wait(cs, drv_lbl, ntf_cap, false, 0).expect("bench notif_wait");
        let t_woke = rdtsc();
        assert_ne!(bits, 0);
        sys_irq_ack(cs, drv_lbl, irq_cap, IRQ_ACK_UNMASK).expect("bench sys_irq_ack");
        let t0 = BENCH_SEND_TSC.load(Ordering::Acquire);
        if i >= 4 && (i - 4) < SAMPLES {
            BENCH_E2E_SAMPLES[i - 4].store(t_woke.saturating_sub(t0), Ordering::Relaxed);
        }
        BENCH_STEP_DONE.store(true, Ordering::Release);
        note_current_thread_progress();
    }
    exit_current_thread()
}

fn irq_bench_sender_worker(packed: u64) {
    let rounds = (packed & 0xffff) as usize;
    let dest_lapic = (packed >> 16) as u32;
    let vec = VEC_EXT_IRQ_BASE + IRQ_TEST_SW_LINE;

    for _ in 0..rounds {
        while !BENCH_WAITER_ARMED.swap(false, Ordering::AcqRel) {
            note_current_thread_progress();
            yield_now();
        }
        // Give waiter thread a short window to finish committing park before firing the APIC vector.
        for _ in 0..64 {
            core::hint::spin_loop();
        }
        BENCH_STEP_DONE.store(false, Ordering::Release);
        BENCH_SEND_TSC.store(rdtsc(), Ordering::Release);
        send_fixed_ipi(dest_lapic, vec);
        while !BENCH_STEP_DONE.load(Ordering::Acquire) {
            note_current_thread_progress();
            yield_now();
        }
    }
    exit_current_thread()
}

#[inline(never)]
pub fn run_irq_and_ioport_benchmarks(online_cpus: usize) {
    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);

    let drv_lbl = SecLabel::new(
        SecDomain::RootSystem,
        SecFlags::NONE,
        encode_compartment(0, 77),
        1,
    );
    let (cs, _) = create_cspace(drv_lbl, 0x71).expect("bench create_cspace");
    let root_ioport =
        mint_root_ioport_cap(cs, drv_lbl, &[0x0060, I8042_STATUS_CMD_PORT]).expect("mint ioport");
    let drv_ioport = cspace_derive(
        cs,
        drv_lbl,
        root_ioport,
        CapRights(CapRights::READ.0 | CapRights::WRITE.0),
        0,
        0,
        0,
    )
    .expect("derive ioport");

    let root_irq =
        mint_root_irq_cap(cs, drv_lbl, IRQ_TEST_SW_LINE, 1u64 << IRQ_TEST_SW_LINE).expect("mint irq");
    let drv_irq = cspace_derive(
        cs,
        drv_lbl,
        root_irq,
        CapRights(CapRights::READ.0 | CapRights::SIGNAL.0 | CapRights::WAIT.0),
        0,
        0,
        0,
    )
    .expect("derive irq");
    let (ntf_cap, _) = notif_create(cs, drv_lbl, drv_lbl, 0).expect("notif_create");

    let mut ioport_samples = [0u64; SAMPLES];
    for s in &mut ioport_samples {
        let t0 = rdtsc();
        let _ = sys_ioport_rw(
            cs,
            drv_lbl,
            drv_ioport,
            IOPORT_OP_INB,
            I8042_STATUS_CMD_PORT as u64,
            0,
        )
        .expect("ioport inb");
        *s = rdtsc().saturating_sub(t0);
    }

    let vec = sys_irq_bind(cs, drv_lbl, drv_irq, ntf_cap, IRQF_ONESHOT).expect("sys_irq_bind");
    assert_eq!(vec, VEC_EXT_IRQ_BASE + IRQ_TEST_SW_LINE);
    let bsp_lapic = get_percpu_by_idx(0).lapic_id;

    let mut top_half_samples = [0u64; SAMPLES];
    let mut ack_samples = [0u64; SAMPLES];
    for i in 0..SAMPLES {
        sti();
        let t0 = rdtsc();
        send_fixed_ipi(bsp_lapic, vec);
        let t1 = rdtsc();
        cli();
        let bits = notif_wait(cs, drv_lbl, ntf_cap, true, 0).expect("nonblock notif_wait");
        assert_eq!(bits, 1u64 << IRQ_TEST_SW_LINE);
        top_half_samples[i] = t1.saturating_sub(t0);

        let ta0 = rdtsc();
        sys_irq_ack(cs, drv_lbl, drv_irq, IRQ_ACK_UNMASK).expect("sys_irq_ack");
        ack_samples[i] = rdtsc().saturating_sub(ta0);
    }

    // Measure full preemptive interrupt -> Notification wakeup -> Ring 0/3 driver resume cycle.
    BENCH_DRV_CS.store(cs as u64, Ordering::Release);
    BENCH_IRQ_CAP.store(drv_irq.0, Ordering::Release);
    BENCH_NTF_CAP.store(ntf_cap.0, Ordering::Release);
    BENCH_WAITER_ARMED.store(false, Ordering::Release);
    BENCH_STEP_DONE.store(false, Ordering::Release);
    let total_rounds = (SAMPLES + 4) as u64;
    let waiter_cpu = 0usize;
    let sender_cpu = if online_cpus >= 2 { 1 } else { 0 };
    let waiter_lapic = get_percpu_by_idx(waiter_cpu).lapic_id;

    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xf1, 2);
    let _ = try_spawn_thread_bound(
        2,
        waiter_cpu,
        KERNEL_VSPACE_ID,
        cs,
        drv_lbl,
        irq_bench_waiter_worker,
        total_rounds,
        true,
    )
    .expect("spawn irq bench waiter");
    let _ = try_spawn_thread_bound(
        1,
        sender_cpu,
        KERNEL_VSPACE_ID,
        cs,
        drv_lbl,
        irq_bench_sender_worker,
        total_rounds | ((waiter_lapic as u64) << 16),
        true,
    )
    .expect("spawn irq bench sender");
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);

    let mut wake_samples = [0u64; SAMPLES];
    for (i, dst) in wake_samples.iter_mut().enumerate() {
        *dst = BENCH_E2E_SAMPLES[i].load(Ordering::Acquire);
    }

    cspace_delete_cap(cs, drv_lbl, drv_ioport, 0).unwrap();
    cspace_delete_cap(cs, drv_lbl, drv_irq, 0).unwrap();
    cspace_destroy_object(cs, drv_lbl, root_irq, 0).unwrap();
    cspace_destroy_object(cs, drv_lbl, root_ioport, 0).unwrap();
    cspace_destroy_object(cs, drv_lbl, ntf_cap, 0).unwrap();
    destroy_cspace(cs).unwrap();

    flush_all_magazines_quiescent(online_cpus);
    let after = measure_fragmentation(online_cpus);
    assert_eq!(before.free_global_frames, after.free_global_frames);

    let (io_50, io_95, io_99) = sort_percentiles(&mut ioport_samples);
    let (th_50, th_95, th_99) = sort_percentiles(&mut top_half_samples);
    let (ak_50, ak_95, ak_99) = sort_percentiles(&mut ack_samples);
    let (wk_50, wk_95, wk_99) = sort_percentiles(&mut wake_samples);

    serial_println!(
        "[DRY-IRQ-BENCH] PASS: cpus={} samples={} ioport_inb_p50_cyc={} ioport_inb_p95_cyc={} ioport_inb_p99_cyc={} irq_top_p50_cyc={} irq_top_p95_cyc={} irq_top_p99_cyc={} irq_ack_p50_cyc={} irq_ack_p95_cyc={} irq_ack_p99_cyc={} irq_wake_p50_cyc={} irq_wake_p95_cyc={} irq_wake_p99_cyc={}",
        online_cpus,
        SAMPLES,
        io_50,
        io_95,
        io_99,
        th_50,
        th_95,
        th_99,
        ak_50,
        ak_95,
        ak_99,
        wk_50,
        wk_95,
        wk_99
    );
}
