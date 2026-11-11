// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `Milestone 1C` Stage G: 11-Metric Capability IPC, Notification, Priority Donation & Service
//! Startup Microbenchmarks (`p50 / p95 / p99` cycles across `1, 2, 4, 8, 20` vCPUs).

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::arch::rdtsc;
use crate::cspace::cap::CapPtr;
use crate::cspace::objects::cspace_alloc_frame;
use crate::cspace::revoke::{cspace_delete_cap, cspace_destroy_object};
use crate::cspace::{create_cspace, cspace_derive, cspace_lookup_fast, destroy_cspace};
use crate::ipc::endpoint::{
    ep_create, ep_recv, ep_reply, ep_reply_recv, ep_send_or_call, IpcSendInput,
};
use crate::ipc::notif::{notif_create, notif_signal, notif_wait};
use crate::ipc::{MsgTag, MSG_FLAG_GRANT_CAP, MSG_FLAG_NON_BLOCKING};
use crate::pmm::{flush_all_magazines_quiescent, measure_fragmentation, PageOrder};
use crate::proc::{
    reclaim_process, spawn_elf_process, ProcessOutcome, ReclaimStatus, SpawnOptions,
};
use crate::sched::donate::{donate_on_call, init_slot_priority, revert_donation_for_caller};
use crate::sched::preempt_tests::{begin_preemptive_epoch, drain_preemptive_epoch_and_audit};
use crate::sched::{
    exit_current_thread, init_scheduler, note_current_thread_progress, set_preemptive_mode_active,
    try_spawn_thread_bound,
};
use crate::security::{encode_compartment, CapRights, SecDomain, SecFlags, SecLabel};
use crate::serial_println;
use crate::vspace::KERNEL_VSPACE_ID;

const FAST_SAMPLES: usize = 128;
const IPC_RT_SAMPLES: usize = 64;
const SVC_SAMPLES: usize = 16;

static BENCH_EP_SRV_CAP: AtomicU64 = AtomicU64::new(0);
static BENCH_EP_CLI_CAP: AtomicU64 = AtomicU64::new(0);
static BENCH_GRANT_SRC_CAP: AtomicU64 = AtomicU64::new(0);
static BENCH_WAKE_STAMP: AtomicU64 = AtomicU64::new(0);
static BENCH_RT_SAMPLES: [AtomicU64; IPC_RT_SAMPLES] = [const { AtomicU64::new(0) }; IPC_RT_SAMPLES];
static BENCH_WAKE_SAMPLES: [AtomicU64; IPC_RT_SAMPLES] =
    [const { AtomicU64::new(0) }; IPC_RT_SAMPLES];
static BENCH_CONT_IDX: AtomicUsize = AtomicUsize::new(0);

fn sort_and_percentiles(buf: &mut [u64]) -> (u64, u64, u64) {
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
    let p50 = buf[n / 2];
    let p95 = buf[(n * 95) / 100];
    let p99 = buf[(n * 99) / 100];
    (p50, p95, p99)
}

fn bench_send_recv_server(packed: u64) {
    let cs = (packed & 0xffff) as u16;
    let count = (packed >> 16) as usize;
    let cap = CapPtr(BENCH_EP_SRV_CAP.load(Ordering::Acquire));
    let lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(14, 1),
        1,
    );
    for i in 0..count {
        let out = ep_recv(cs, lbl, cap, false, 0, None).unwrap();
        let t_rx = rdtsc();
        if let Some(slot) = BENCH_WAKE_SAMPLES.get(i) {
            slot.store(t_rx.saturating_sub(out.mrs[0]), Ordering::Relaxed);
        }
        note_current_thread_progress();
    }
    exit_current_thread()
}

fn bench_send_recv_client(packed: u64) {
    let cs = (packed & 0xffff) as u16;
    let count = (packed >> 16) as usize;
    let cap = CapPtr(BENCH_EP_CLI_CAP.load(Ordering::Acquire));
    let lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(14, 1),
        1,
    );
    for i in 0..count {
        let t0 = rdtsc();
        ep_send_or_call(
            cs,
            lbl,
            cap,
            IpcSendInput {
                tag: MsgTag::new(1, 0, 1, 0),
                mrs: [t0, 0, 0, 0],
                extra: &[],
                grant: None,
                shared_desc: None,
            },
            false,
            0,
            None,
        )
        .unwrap();
        let dt = rdtsc().saturating_sub(t0);
        if i >= 8 && (i - 8) < IPC_RT_SAMPLES {
            BENCH_RT_SAMPLES[i - 8].store(dt, Ordering::Relaxed);
        }
        note_current_thread_progress();
    }
    exit_current_thread()
}

fn bench_call_reply_server(packed: u64) {
    let cs = (packed & 0xffff) as u16;
    let count = ((packed >> 16) & 0xffff) as usize;
    let delete_xfer = ((packed >> 32) & 1) != 0;
    let cap = CapPtr(BENCH_EP_SRV_CAP.load(Ordering::Acquire));
    let lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(14, 1),
        1,
    );

    let mut reply_tok = 0u64;
    let mut reply_tag = MsgTag::EMPTY;
    let mut reply_mrs = [0u64; 4];

    for _ in 0..count {
        let req = ep_reply_recv(
            cs,
            lbl,
            cap,
            reply_tok,
            IpcSendInput {
                tag: reply_tag,
                mrs: reply_mrs,
                extra: &[],
                grant: None,
                shared_desc: None,
            },
            false,
            0,
            None,
        )
        .unwrap();
        if delete_xfer && req.transferred_cap != CapPtr::NULL {
            let _ = cspace_delete_cap(cs, lbl, req.transferred_cap, 1);
        }
        reply_tok = req.reply_token;
        reply_tag = MsgTag::new(2, 0, 1, 0);
        reply_mrs = [rdtsc(), 0, 0, 0];
        note_current_thread_progress();
    }
    if reply_tok != 0 {
        BENCH_WAKE_STAMP.store(rdtsc(), Ordering::Release);
        ep_reply(
            cs,
            lbl,
            cap,
            reply_tok,
            IpcSendInput {
                tag: reply_tag,
                mrs: [rdtsc(), 0, 0, 0],
                extra: &[],
                grant: None,
                shared_desc: None,
            },
        )
        .unwrap();
    }
    exit_current_thread()
}

fn bench_call_reply_client(packed: u64) {
    let cs = (packed & 0xffff) as u16;
    let count = ((packed >> 16) & 0xffff) as usize;
    let with_grant = ((packed >> 32) & 1) != 0;
    let is_contended = ((packed >> 33) & 1) != 0;
    let cap = CapPtr(BENCH_EP_CLI_CAP.load(Ordering::Acquire));
    let grant_src = CapPtr(BENCH_GRANT_SRC_CAP.load(Ordering::Acquire));
    let lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(14, 1),
        1,
    );

    for i in 0..count {
        let (tag, grant) = if with_grant {
            (
                MsgTag::new(1, MSG_FLAG_GRANT_CAP, 1, 0),
                Some((grant_src, CapRights::READ, 0, 0x33)),
            )
        } else {
            (MsgTag::new(1, 0, 1, 0), None)
        };
        let t0 = rdtsc();
        let out = ep_send_or_call(
            cs,
            lbl,
            cap,
            IpcSendInput {
                tag,
                mrs: [t0, 0, 0, 0],
                extra: &[],
                grant,
                shared_desc: None,
            },
            true,
            0,
            None,
        )
        .unwrap();
        let t1 = rdtsc();
        if is_contended {
            let slot = BENCH_CONT_IDX.fetch_add(1, Ordering::Relaxed);
            if slot < IPC_RT_SAMPLES {
                BENCH_RT_SAMPLES[slot].store(t1.saturating_sub(t0), Ordering::Relaxed);
            }
        } else if i >= 8 && (i - 8) < IPC_RT_SAMPLES {
            BENCH_RT_SAMPLES[i - 8].store(t1.saturating_sub(t0), Ordering::Relaxed);
            BENCH_WAKE_SAMPLES[i - 8].store(t1.saturating_sub(out.mrs[0]), Ordering::Relaxed);
        }
        note_current_thread_progress();
    }
    exit_current_thread()
}

fn measure_rpc_pair(
    online_cpus: usize,
    cs: u16,
    lbl: SecLabel,
    srv_cpu: usize,
    cli_cpu: usize,
    with_grant: bool,
) -> ((u64, u64, u64), (u64, u64, u64)) {
    let total = IPC_RT_SAMPLES + 8;
    let flags = u64::from(with_grant);
    let srv_arg = (cs as u64) | ((total as u64) << 16) | (flags << 32);
    let cli_arg = (cs as u64) | ((total as u64) << 16) | (flags << 32);

    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xe8, 2);
    try_spawn_thread_bound(
        2,
        srv_cpu % online_cpus,
        KERNEL_VSPACE_ID,
        cs,
        lbl,
        bench_call_reply_server,
        srv_arg,
        true,
    )
    .unwrap();
    try_spawn_thread_bound(
        2,
        cli_cpu % online_cpus,
        KERNEL_VSPACE_ID,
        cs,
        lbl,
        bench_call_reply_client,
        cli_arg,
        true,
    )
    .unwrap();
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);

    let mut rt = [0u64; IPC_RT_SAMPLES];
    let mut wk = [0u64; IPC_RT_SAMPLES];
    for i in 0..IPC_RT_SAMPLES {
        rt[i] = BENCH_RT_SAMPLES[i].load(Ordering::Acquire);
        wk[i] = BENCH_WAKE_SAMPLES[i].load(Ordering::Acquire);
    }
    (sort_and_percentiles(&mut rt), sort_and_percentiles(&mut wk))
}

/// Execute all 11 Stage G `p50/p95/p99` IPC & service benchmarks.
pub fn run_ipc_benchmarks(online_cpus: usize) {
    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);

    let lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(14, 1),
        1,
    );
    let (cs, _) = create_cspace(lbl, 0x82).unwrap();
    let (ep_root, _) = ep_create(cs, lbl, lbl, 0).unwrap();
    let srv_cap = cspace_derive(
        cs,
        lbl,
        ep_root,
        CapRights(CapRights::RECV.0 | CapRights::REPLY.0),
        0,
        0,
        1,
    )
    .unwrap();
    let cli_cap = cspace_derive(
        cs,
        lbl,
        ep_root,
        CapRights(CapRights::SEND.0 | CapRights::CALL.0),
        0,
        0x77,
        1,
    )
    .unwrap();
    let (fr_cap, _) = cspace_alloc_frame(cs, lbl, lbl, PageOrder::Order4KiB, false, 0).unwrap();
    let (ntf_cap, _) = notif_create(cs, lbl, lbl, 0).unwrap();

    BENCH_EP_SRV_CAP.store(srv_cap.0, Ordering::Release);
    BENCH_EP_CLI_CAP.store(cli_cap.0, Ordering::Release);
    BENCH_GRANT_SRC_CAP.store(fr_cap.0, Ordering::Release);

    // 1. Endpoint capability lookup (`cspace_lookup_fast`).
    let mut s_lookup = [0u64; FAST_SAMPLES];
    for _ in 0..16 {
        let _ = cspace_lookup_fast(cs, lbl, cli_cap, CapRights::SEND, 1).unwrap();
    }
    for slot in &mut s_lookup {
        let t0 = rdtsc();
        let _ = core::hint::black_box(cspace_lookup_fast(cs, lbl, cli_cap, CapRights::SEND, 1).unwrap());
        *slot = rdtsc().saturating_sub(t0);
    }
    let p_lookup = sort_and_percentiles(&mut s_lookup);

    // 2. Synchronous 1-way `send/recv` latency.
    let total_sr = IPC_RT_SAMPLES + 8;
    let sr_arg = (cs as u64) | ((total_sr as u64) << 16);
    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xe7, 2);
    try_spawn_thread_bound(
        2,
        0,
        KERNEL_VSPACE_ID,
        cs,
        lbl,
        bench_send_recv_server,
        sr_arg,
        true,
    )
    .unwrap();
    try_spawn_thread_bound(
        2,
        0,
        KERNEL_VSPACE_ID,
        cs,
        lbl,
        bench_send_recv_client,
        sr_arg,
        true,
    )
    .unwrap();
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);
    let mut s_sr = [0u64; IPC_RT_SAMPLES];
    for (i, item) in s_sr.iter_mut().enumerate() {
        *item = BENCH_RT_SAMPLES[i].load(Ordering::Acquire);
    }
    let p_send_recv = sort_and_percentiles(&mut s_sr);

    // 3, 4 & 9. `call/reply` round-trip, Same-CPU IPC (`0 <-> 0`), & IPC scheduler wakeup latency.
    let (p_same_cpu, p_wakeup) = measure_rpc_pair(online_cpus, cs, lbl, 0, 0, false);
    let p_call_reply = p_same_cpu;

    // 5. Cross-CPU IPC (`0 <-> 1` when `online_cpus >= 2`, else `0 <-> 0` tagged).
    let (p_cross_cpu, _) = if online_cpus >= 2 {
        measure_rpc_pair(online_cpus, cs, lbl, 0, 1, false)
    } else {
        (p_same_cpu, p_wakeup)
    };

    // 6. Contended Endpoint (`4` concurrent clients -> `1` server).
    let cont_clients = 4usize;
    let per_cli = IPC_RT_SAMPLES / cont_clients;
    BENCH_CONT_IDX.store(0, Ordering::Release);
    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xe9, 1 + cont_clients);
    try_spawn_thread_bound(
        2,
        0,
        KERNEL_VSPACE_ID,
        cs,
        lbl,
        bench_call_reply_server,
        (cs as u64) | ((IPC_RT_SAMPLES as u64) << 16),
        true,
    )
    .unwrap();
    for c in 0..cont_clients {
        let arg = (cs as u64) | ((per_cli as u64) << 16) | (1u64 << 33);
        try_spawn_thread_bound(
            2,
            (c + 1) % online_cpus,
            KERNEL_VSPACE_ID,
            cs,
            lbl,
            bench_call_reply_client,
            arg,
            true,
        )
        .unwrap();
    }
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);
    let mut s_cont = [0u64; IPC_RT_SAMPLES];
    for (i, item) in s_cont.iter_mut().enumerate() {
        *item = BENCH_RT_SAMPLES[i].load(Ordering::Acquire);
    }
    let p_contended = sort_and_percentiles(&mut s_cont);

    // 7. `Notification` `signal + wait` fast path.
    let mut s_notif = [0u64; FAST_SAMPLES];
    for slot in &mut s_notif {
        let t0 = rdtsc();
        notif_signal(cs, lbl, ntf_cap, 0x10).unwrap();
        let w = notif_wait(cs, lbl, ntf_cap, MSG_FLAG_NON_BLOCKING != 0, 0).unwrap();
        core::hint::black_box(w);
        *slot = rdtsc().saturating_sub(t0);
    }
    let p_notif = sort_and_percentiles(&mut s_notif);

    // 8. Capability transfer (`MSG_FLAG_GRANT_CAP` over IPC `call`).
    let (p_cap_xfer, _) = measure_rpc_pair(online_cpus, cs, lbl, 0, 1 % online_cpus, true);

    // 10. Priority donation apply + revert overhead (`donate_on_call` + `revert_donation_for_caller`).
    let d_srv = crate::gdt_tss::MAX_CPUS;
    let d_cli = crate::gdt_tss::MAX_CPUS + 1;
    init_slot_priority(d_srv, 4);
    init_slot_priority(d_cli, 1);
    let mut s_donate = [0u64; FAST_SAMPLES];
    for slot in &mut s_donate {
        let t0 = rdtsc();
        let _ = donate_on_call(d_cli, d_srv).unwrap();
        let _ = revert_donation_for_caller(d_cli);
        *slot = rdtsc().saturating_sub(t0);
    }
    let p_donate = sort_and_percentiles(&mut s_donate);

    // 11. Service process startup + exit + reap (`dry-echo.elf`).
    let echo_elf = crate::limine::get_boot_module("dry-echo.elf").unwrap();
    let mut s_svc = [0u64; SVC_SAMPLES];
    for item in &mut s_svc {
        let t0 = rdtsc();
        set_preemptive_mode_active(true);
        init_scheduler(online_cpus);
        begin_preemptive_epoch(0xea, 1);
        // `ep_cap = 0` causes `dry_echo` to validate CPL=3 selectors and exit immediately (`90`),
        // measuring pure ELF64 service creation + CPL=3 entry + exit + full reclamation!
        let p = spawn_elf_process(
            echo_elf,
            SpawnOptions {
                subj_label: lbl,
                arg0: 0,
                arg1: 0,
                target_cpu: 0,
                trace_lifecycle: false,
                user_fs_base: 0,
                user_gs_base: 0,
            },
        )
        .unwrap();
        drain_preemptive_epoch_and_audit(online_cpus);
        set_preemptive_mode_active(false);
        let ReclaimStatus::ReclaimedNow(out) = reclaim_process(p.handle).unwrap() else {
            panic!("svc bench reclaim failed");
        };
        assert_eq!(out, ProcessOutcome::Exited(90));
        *item = rdtsc().saturating_sub(t0);
    }
    let p_svc = sort_and_percentiles(&mut s_svc);

    cspace_destroy_object(cs, lbl, ntf_cap, 1).unwrap();
    cspace_destroy_object(cs, lbl, fr_cap, 1).unwrap();
    cspace_destroy_object(cs, lbl, ep_root, 1).unwrap();
    destroy_cspace(cs).unwrap();

    flush_all_magazines_quiescent(online_cpus);
    let after = measure_fragmentation(online_cpus);
    let leaked = before
        .free_global_frames
        .saturating_sub(after.free_global_frames);
    assert_eq!(leaked, 0, "Frame leak in Stage G IPC benchmarks: {}", leaked);

    serial_println!(
        "[DRY-IPC-BENCH] cpus={} ep_lookup={}..{}..{} send_recv={}..{}..{} call_reply={}..{}..{} same_cpu={}..{}..{} cross_cpu={}..{}..{} contended={}..{}..{}",
        online_cpus,
        p_lookup.0, p_lookup.1, p_lookup.2,
        p_send_recv.0, p_send_recv.1, p_send_recv.2,
        p_call_reply.0, p_call_reply.1, p_call_reply.2,
        p_same_cpu.0, p_same_cpu.1, p_same_cpu.2,
        p_cross_cpu.0, p_cross_cpu.1, p_cross_cpu.2,
        p_contended.0, p_contended.1, p_contended.2
    );
    serial_println!(
        "[DRY-IPC-BENCH2] cpus={} notif_sig_wait={}..{}..{} cap_transfer={}..{}..{} sched_wakeup={}..{}..{} prio_donate={}..{}..{} svc_startup={}..{}..{}",
        online_cpus,
        p_notif.0, p_notif.1, p_notif.2,
        p_cap_xfer.0, p_cap_xfer.1, p_cap_xfer.2,
        p_wakeup.0, p_wakeup.1, p_wakeup.2,
        p_donate.0, p_donate.1, p_donate.2,
        p_svc.0, p_svc.1, p_svc.2
    );
    serial_println!(
        "[DRY-IPC-BENCH] PASS: cpus={} metrics=11/11 leaked_frames={}",
        online_cpus,
        leaked
    );
}
