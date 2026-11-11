// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `Milestone 1C` Stages B, C, D, E & F: Capability IPC Security/Race Verification, Real CPL=3
//! Root Supervisor (`dry_init`) + Echo (`dry_echo`) + Framebuffer (`dry_display`) Proof, and
//! Fail-Closed Negative Fault Injection.

use core::ptr::{copy_nonoverlapping, read_volatile};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::cspace::cap::{CapError, CapPtr};
use crate::cspace::objects::cspace_alloc_frame;
use crate::cspace::revoke::{cspace_delete_cap, cspace_destroy_object, cspace_revoke_children};
use crate::cspace::{create_cspace, cspace_derive, cspace_transfer, destroy_cspace};
use crate::display::{
    framebuffer_summary, is_framebuffer_available, mint_root_framebuffer_cap, sys_fb_map,
};
use crate::ipc::endpoint::{
    ep_create, ep_recv, ep_reply, ep_reply_recv, ep_send_or_call, IpcSendInput,
};
use crate::ipc::notif::{notif_create, notif_signal, notif_wait};
use crate::ipc::{
    MsgTag, SharedPayloadDesc, MAX_IPC_EXTRA_BYTES, MSG_FLAG_EXTRA_BUF, MSG_FLAG_NON_BLOCKING,
    MSG_FLAG_SHARED_DESC,
};
use crate::pmm::{flush_all_magazines_quiescent, measure_fragmentation, PageOrder};
use crate::proc::supervisor::mint_boot_module_cap;
use crate::proc::{
    reclaim_process, spawn_elf_process_with, ProcessOutcome, ReclaimStatus, SpawnOptions,
};
use crate::sched::donate::{
    cleanup_donations_on_thread_exit, donate_on_call, init_slot_priority, inspect_slot_priorities,
    revert_donation_for_caller, DonateError, GLOBAL_DONATION_CYCLES,
};
use crate::sched::preempt_tests::{begin_preemptive_epoch, drain_preemptive_epoch_and_audit};
use crate::sched::{
    exit_current_thread, idle_dispatch_once, init_scheduler, note_current_thread_progress,
    preempt_disable, preempt_enable, set_preemptive_mode_active, try_spawn_thread_bound, yield_now,
};
use crate::security::{encode_compartment, CapRights, SecDomain, SecFlags, SecLabel};
use crate::serial_println;
use crate::uaccess::SysError;
use crate::vspace::pte::VmProt;
use crate::vspace::{
    create_vspace, destroy_vspace, map_4k, map_mmio_4k, switch_cpu_vspace_if_needed,
    walk_vspace_address, KERNEL_VSPACE_ID,
};

const BOOT_INFO_VADDR: u64 = 0x0070_0000;
const ROOT_INIT_BOOT_MAGIC: u64 = 0x4452_595f_494e_4954;

#[repr(C)]
#[derive(Clone, Copy)]
struct RootInitBootInfo {
    magic: u64,
    echo_mod_cap: u64,
    display_mod_cap: u64,
    fb_cap: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IpcFaultCmdline {
    None,
    IrqBlock,
    UnauthFb,
    ForgedEp,
    DonationCycle,
}

impl IpcFaultCmdline {
    pub fn from_cmdline(cmd: &str) -> Self {
        if cmd.contains("ipc_fault=irq_block") {
            Self::IrqBlock
        } else if cmd.contains("ipc_fault=unauth_fb") {
            Self::UnauthFb
        } else if cmd.contains("ipc_fault=forged_ep") {
            Self::ForgedEp
        } else if cmd.contains("ipc_fault=donation_cycle") {
            Self::DonationCycle
        } else {
            Self::None
        }
    }
}

fn ipc_invariant_panic(args: core::fmt::Arguments<'_>) -> ! {
    crate::enter_fatal_or_halt();
    crate::serial::emergency_print(format_args!("\r\n[DRY-IPC-FAIL] {}\r\n", args));
    panic!("IPC / service security invariant failure");
}

pub fn maybe_run_ipc_fault_test(cmdline: &str, _online_cpus: usize) {
    let mode = IpcFaultCmdline::from_cmdline(cmdline);
    if mode == IpcFaultCmdline::None {
        return;
    }
    let u_lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(9, 1),
        1,
    );
    let (cs, _) = create_cspace(u_lbl, 0x62).unwrap();
    let (ep_cap, _) = ep_create(cs, u_lbl, u_lbl, 0).unwrap();

    match mode {
        IpcFaultCmdline::None => unreachable!(),
        IpcFaultCmdline::IrqBlock => {
            preempt_disable();
            let err = ep_recv(cs, u_lbl, ep_cap, false, 0, None).unwrap_err();
            preempt_enable();
            ipc_invariant_panic(format_args!(
                "Blocking IPC in preempt-disabled/IRQ context rejected: err={:?}",
                err
            ));
        }
        IpcFaultCmdline::UnauthFb => {
            let vs = create_vspace(u_lbl).unwrap();
            let err = sys_fb_map(cs, u_lbl, vs.id, ep_cap.0, 0x1000_0000, 0x2000_0000).unwrap_err();
            let _ = destroy_vspace(vs);
            ipc_invariant_panic(format_args!(
                "Unauthorized framebuffer mapping rejected: err={:?}",
                err
            ));
        }
        IpcFaultCmdline::ForgedEp => {
            let forged = CapPtr::pack(0xfe, 8, 99, 7);
            let err = ep_recv(cs, u_lbl, forged, true, 0, None).unwrap_err();
            ipc_invariant_panic(format_args!(
                "Forged endpoint capability rejected: err={:?}",
                err
            ));
        }
        IpcFaultCmdline::DonationCycle => {
            let s_a = crate::gdt_tss::MAX_CPUS;
            let s_b = crate::gdt_tss::MAX_CPUS + 1;
            init_slot_priority(s_a, 3);
            init_slot_priority(s_b, 2);
            let _ = donate_on_call(s_a, s_b).unwrap();
            let cyc = donate_on_call(s_b, s_a);
            cleanup_donations_on_thread_exit(s_a);
            cleanup_donations_on_thread_exit(s_b);
            ipc_invariant_panic(format_args!(
                "Priority donation cycle rejected: err={:?}",
                cyc.err()
            ));
        }
    }
}

// --- Static synchronization state for kernel-thread SMP race & security scenarios ---

static REV_WAIT_READY: AtomicUsize = AtomicUsize::new(0);
static REV_SEND_ERR: AtomicU32 = AtomicU32::new(0);
static REV_RECV_ERR: AtomicU32 = AtomicU32::new(0);

fn revoke_blocked_sender_worker(packed: u64) {
    let cs = (packed & 0xffff) as u16;
    let cap = CapPtr(RES_SHARED_CAP0.load(Ordering::Acquire));
    let lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(11, 1),
        1,
    );
    REV_WAIT_READY.fetch_add(1, Ordering::AcqRel);
    let res = ep_send_or_call(
        cs,
        lbl,
        cap,
        IpcSendInput {
            tag: MsgTag::new(7, 0, 1, 0),
            mrs: [0xaa55, 0, 0, 0],
            extra: &[],
            grant: None,
            shared_desc: None,
        },
        false,
        0,
        None,
    );
    if res == Err(SysError::Revoked) {
        REV_SEND_ERR.store(1, Ordering::Release);
    }
    exit_current_thread()
}

fn revoke_blocked_receiver_worker(packed: u64) {
    let cs = (packed & 0xffff) as u16;
    let cap = CapPtr(RES_SHARED_CAP1.load(Ordering::Acquire));
    let lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(11, 1),
        1,
    );
    REV_WAIT_READY.fetch_add(1, Ordering::AcqRel);
    let res = ep_recv(cs, lbl, cap, false, 0, None);
    if res == Err(SysError::Revoked) {
        REV_RECV_ERR.store(1, Ordering::Release);
    }
    exit_current_thread()
}

static RES_SHARED_CAP0: AtomicU64 = AtomicU64::new(0);
static RES_SHARED_CAP1: AtomicU64 = AtomicU64::new(0);
static RES_SHARED_CAP2: AtomicU64 = AtomicU64::new(0);
static SRV_EXIT_CALLER_OK: AtomicBool = AtomicBool::new(false);
static TOKEN_REUSE_OK: AtomicBool = AtomicBool::new(false);
static DOUBLE_REPLY_OK: AtomicBool = AtomicBool::new(false);

fn server_exit_and_double_reply_server(packed: u64) {
    let cs = (packed & 0xffff) as u16;
    let mode = packed >> 16;
    let cap = CapPtr(RES_SHARED_CAP0.load(Ordering::Acquire));
    let lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(12, 1),
        1,
    );

    if mode == 0 {
        // Receive request (`caller` enters `WaitingReply`), then exit WITHOUT replying (`Stage F.4`)!
        let _ = ep_recv(cs, lbl, cap, false, 0, None).unwrap();
        exit_current_thread()
    } else {
        // Receive request, reply once (`Ok`), then attempt second reply + token reuse (`Stage F.5 & F.7`)!
        let req = ep_recv(cs, lbl, cap, false, 0, None).unwrap();
        let tok = req.reply_token;
        let rep_in = IpcSendInput {
            tag: MsgTag::new(9, 0, 1, 0),
            mrs: [0x900d, 0, 0, 0],
            extra: &[],
            grant: None,
            shared_desc: None,
        };
        assert_eq!(ep_reply(cs, lbl, cap, tok, rep_in), Ok(()));
        let second = ep_reply(cs, lbl, cap, tok, rep_in);
        if second == Err(SysError::BadCap) || second == Err(SysError::Pipe) {
            DOUBLE_REPLY_OK.store(true, Ordering::Release);
            TOKEN_REUSE_OK.store(true, Ordering::Release);
        }
        exit_current_thread()
    }
}

fn server_exit_and_double_reply_client(packed: u64) {
    let cs = (packed & 0xffff) as u16;
    let mode = packed >> 16;
    let cap = CapPtr(RES_SHARED_CAP1.load(Ordering::Acquire));
    let lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(12, 1),
        1,
    );

    let res = ep_send_or_call(
        cs,
        lbl,
        cap,
        IpcSendInput {
            tag: MsgTag::new(8, 0, 1, 0),
            mrs: [0x1111, 0, 0, 0],
            extra: &[],
            grant: None,
            shared_desc: None,
        },
        true,
        0,
        None,
    );
    if mode == 0 {
        if res == Err(SysError::Pipe) {
            SRV_EXIT_CALLER_OK.store(true, Ordering::Release);
        }
    } else if let Ok(out) = res {
        assert_eq!(out.mrs[0], 0x900d);
    }
    exit_current_thread()
}

// --- Multi-client SMP contention + timeout/wakeup race workers (`Stage B.4, F.8, F.12`) ---

const SMP_CLIENTS: usize = 8;
const SMP_ROUNDS_PER_CLIENT: usize = 32;
static SMP_DONE_CALLS: AtomicUsize = AtomicUsize::new(0);
static SMP_TIMEOUT_RACE_OK: AtomicUsize = AtomicUsize::new(0);

fn smp_contended_server_worker(cs_raw: u64) {
    let cs = cs_raw as u16;
    let ep_cap = CapPtr(RES_SHARED_CAP0.load(Ordering::Acquire));
    let notif_cap = CapPtr(RES_SHARED_CAP2.load(Ordering::Acquire));
    let lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(13, 1),
        1,
    );

    let total_reqs = SMP_CLIENTS * SMP_ROUNDS_PER_CLIENT;
    let mut prev_tok = 0u64;
    let mut prev_tag = MsgTag::EMPTY;
    let mut prev_mrs = [0u64; 4];

    for i in 0..total_reqs {
        let req = ep_reply_recv(
            cs,
            lbl,
            ep_cap,
            prev_tok,
            IpcSendInput {
                tag: prev_tag,
                mrs: prev_mrs,
                extra: &[],
                grant: None,
                shared_desc: None,
            },
            false,
            0,
            None,
        )
        .expect("Contended server ep_reply_recv failed");
        prev_tok = req.reply_token;
        prev_tag = MsgTag::new(req.tag.label() + 100, 0, 2, 0);
        prev_mrs = [req.mrs[0] ^ 0xa5a5_5a5a, req.sender_badge, 0, 0];
        if (i & 7) == 0 {
            let _ = notif_signal(cs, lbl, notif_cap, 1 << (i & 3));
        }
        note_current_thread_progress();
    }
    if prev_tok != 0 {
        ep_reply(
            cs,
            lbl,
            ep_cap,
            prev_tok,
            IpcSendInput {
                tag: prev_tag,
                mrs: prev_mrs,
                extra: &[],
                grant: None,
                shared_desc: None,
            },
        )
        .unwrap();
    }
    exit_current_thread()
}

fn smp_contended_client_worker(packed: u64) {
    let cs = (packed & 0xffff) as u16;
    let cid = ((packed >> 16) & 0xff) as usize;
    let ep_cap = CapPtr(RES_SHARED_CAP1.load(Ordering::Acquire));
    let lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(13, 1),
        1,
    );

    for r in 0..SMP_ROUNDS_PER_CLIENT {
        let val = ((cid as u64) << 32) | (r as u64);
        let out = ep_send_or_call(
            cs,
            lbl,
            ep_cap,
            IpcSendInput {
                tag: MsgTag::new(10, 0, 1, 0),
                mrs: [val, 0, 0, 0],
                extra: &[],
                grant: None,
                shared_desc: None,
            },
            true,
            0,
            None,
        )
        .expect("SMP client ep_call failed");
        assert_eq!(out.tag.label(), 110);
        assert_eq!(out.mrs[0], val ^ 0xa5a5_5a5a);
        assert_eq!(out.mrs[1], 0xbeef);
        SMP_DONE_CALLS.fetch_add(1, Ordering::AcqRel);
        if (r & 3) == 0 {
            yield_now();
        }
        note_current_thread_progress();
    }
    exit_current_thread()
}

fn timeout_waiter_worker(cs_raw: u64) {
    let cs = cs_raw as u16;
    let ep_cap = CapPtr(RES_SHARED_CAP0.load(Ordering::Acquire));
    let notif_cap = CapPtr(RES_SHARED_CAP2.load(Ordering::Acquire));
    let lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(13, 1),
        1,
    );
    // Short bounded TSC deadline (`50_000` cycles) on idle endpoint & notification must time out cleanly!
    let r1 = ep_recv(cs, lbl, ep_cap, false, 50_000, None);
    let r2 = notif_wait(cs, lbl, notif_cap, false, 50_000);
    if r1 == Err(SysError::TimedOut) && r2 == Err(SysError::TimedOut) {
        SMP_TIMEOUT_RACE_OK.fetch_add(1, Ordering::AcqRel);
    }
    exit_current_thread()
}

/// Run all 16 Stage F security, race, boundary, donation, and leak-audit scenarios.
fn run_stage_f_ipc_security_and_races(online_cpus: usize) {
    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);

    let root_lbl = SecLabel::new(SecDomain::RootSystem, SecFlags::NONE, 0, 1);
    let u_a = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, encode_compartment(11, 1), 1);
    let u_a_nd =
        SecLabel::new(SecDomain::Userspace, SecFlags::NO_DELEGATE, encode_compartment(11, 1), 1);
    let u_b = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, encode_compartment(11, 2), 1);

    let (cs_root, _) = create_cspace(root_lbl, 0x71).unwrap();
    let (cs_a, _) = create_cspace(u_a, 0x72).unwrap();
    let (cs_nd, _) = create_cspace(u_a_nd, 0x76).unwrap();
    let (cs_b, _) = create_cspace(u_b, 0x73).unwrap();

    let (ep_a, _) = ep_create(cs_a, u_a, u_a, 0).unwrap();
    let (ep_nd, _) = ep_create(cs_nd, u_a_nd, u_a_nd, 0).unwrap();
    let send_only = cspace_derive(cs_a, u_a, ep_a, CapRights::SEND, 0, 0x11, 1).unwrap();

    // 1. Unauthorized endpoint access (`NO_DELEGATE` cross-compartment, ungranted handle, & `SEND` -> `RECV`/`CALL`).
    assert_eq!(ep_recv(cs_nd, u_b, ep_nd, true, 0, None), Err(SysError::Perm));
    assert_eq!(ep_recv(cs_b, u_b, ep_a, true, 0, None), Err(SysError::BadCap));
    cspace_destroy_object(cs_nd, u_a_nd, ep_nd, 1).unwrap();
    destroy_cspace(cs_nd).unwrap();
    assert_eq!(ep_recv(cs_a, u_a, send_only, true, 0, None), Err(SysError::Perm));
    let dummy_in = IpcSendInput {
        tag: MsgTag::new(1, 0, 1, 0),
        mrs: [1, 0, 0, 0],
        extra: &[],
        grant: None,
        shared_desc: None,
    };
    assert_eq!(
        ep_send_or_call(cs_a, u_a, send_only, dummy_in, true, 0, None),
        Err(SysError::Perm)
    );

    // 2. Forged capability & wrong object kind (`Frame4K` passed to `ep_recv`).
    let forged = CapPtr::pack(0xaa, 8, 55, 9);
    assert_eq!(ep_recv(cs_a, u_a, forged, true, 0, None), Err(SysError::BadCap));
    let (fr_cap, _) = cspace_alloc_frame(cs_a, u_a, u_a, PageOrder::Order4KiB, false, 0).unwrap();
    assert_eq!(ep_recv(cs_a, u_a, fr_cap, true, 0, None), Err(SysError::Perm));

    // 6 & 13. Cross-domain capability transfer restriction (`RootSystem` private -> `Userspace` denied)
    // + authorized cross-compartment transfer (`cs_a -> cs_b`) + transitive CDT `revoke` (`Stage F.13`).
    let (root_priv_ep, _) = ep_create(cs_root, root_lbl, root_lbl, 0).unwrap();
    assert!(cspace_transfer(cs_root, root_lbl, root_priv_ep, cs_a, CapRights::SEND, 0, 0, 1).is_err());
    cspace_destroy_object(cs_root, root_lbl, root_priv_ep, 1).unwrap();

    let xfer_rcpt = cspace_transfer(cs_a, u_a, ep_a, cs_b, CapRights::RECV, 0, 0x22, 1).unwrap();
    assert_eq!(ep_recv(cs_b, u_b, xfer_rcpt.dst_cap, true, 0, None), Err(SysError::WouldBlock));
    cspace_revoke_children(cs_a, u_a, ep_a, 1).unwrap();
    assert_eq!(ep_recv(cs_b, u_b, xfer_rcpt.dst_cap, true, 0, None), Err(SysError::Revoked));
    let send_only = cspace_derive(cs_a, u_a, ep_a, CapRights::SEND, 0, 0x11, 1).unwrap();

    // 9. Bounded 4-hop Priority Donation (`5 -> 3 -> 1` boost + exact revert `1 -> 3 -> 5` + cycle `-EDEADLK`).
    let s0 = crate::gdt_tss::MAX_CPUS;
    let s1 = crate::gdt_tss::MAX_CPUS + 1;
    let s2 = crate::gdt_tss::MAX_CPUS + 2;
    init_slot_priority(s0, 5);
    init_slot_priority(s1, 3);
    init_slot_priority(s2, 1);
    assert_eq!(donate_on_call(s1, s0), Ok(3));
    assert_eq!(inspect_slot_priorities(s0), (5, 3));
    assert_eq!(donate_on_call(s2, s1), Ok(1));
    assert_eq!(inspect_slot_priorities(s0), (5, 1));
    // Cycle `s0 -> s2` (`s2 -> s1 -> s0 -> s2`) MUST return `CycleDetected`!
    assert_eq!(donate_on_call(s0, s2), Err(DonateError::CycleDetected));
    assert!(GLOBAL_DONATION_CYCLES.load(Ordering::Relaxed) >= 1);
    assert!(revert_donation_for_caller(s2));
    assert_eq!(inspect_slot_priorities(s0), (5, 3));
    assert!(revert_donation_for_caller(s1));
    assert_eq!(inspect_slot_priorities(s0), (5, 5));

    // 10. Payload bound overflow (`words > 4`, `extra_len > 256`, `SharedPayloadDesc > 4096`).
    assert_eq!(MsgTag::new(1, 0, 5, 0).validate(), Err(SysError::MsgSize));
    assert_eq!(
        MsgTag::new(1, MSG_FLAG_EXTRA_BUF, 1, (MAX_IPC_EXTRA_BYTES + 1) as u16).validate(),
        Err(SysError::MsgSize)
    );
    assert_eq!(
        ep_send_or_call(
            cs_a,
            u_a,
            ep_a,
            IpcSendInput {
                tag: MsgTag::new(1, MSG_FLAG_SHARED_DESC | MSG_FLAG_NON_BLOCKING, 0, 0),
                mrs: [0; 4],
                extra: &[],
                grant: None,
                shared_desc: Some(SharedPayloadDesc {
                    region_cap: fr_cap,
                    byte_offset: 2048,
                    byte_len: 3000,
                }),
            },
            false,
            0,
            None,
        ),
        Err(SysError::MsgSize)
    );
    cspace_destroy_object(cs_a, u_a, fr_cap, 1).unwrap();

    // 11. Invalid userspace pointer (`copy_from_user` / `sys_fb_map` kernel pointer -> `-EFAULT`).
    let mut dummy_k = [0u8; 8];
    assert_eq!(
        crate::uaccess::copy_from_user(&mut dummy_k, 0xffff_8000_0000_1000),
        Err(SysError::Fault)
    );

    // 15. Unauthorized framebuffer access (`Userspace` subject denied `mint_root_framebuffer_cap` & `sys_fb_map`;
    //     `RootSystem` `fb_cap` maps cleanly and `revoke` unmaps 100% of MMIO PTEs!).
    assert_eq!(
        mint_root_framebuffer_cap(cs_a, u_a),
        Err(CapError::DomainViolation)
    );
    if is_framebuffer_available() {
        let fb_root = mint_root_framebuffer_cap(cs_root, root_lbl).unwrap();
        // Cross-domain transfer of `Framebuffer` to `Userspace` is strictly forbidden!
        assert!(
            cspace_transfer(cs_root, root_lbl, fb_root, cs_a, CapRights::MAP, 0, 0, 1).is_err()
        );
        let fb_child = cspace_derive(
            cs_root,
            root_lbl,
            fb_root,
            CapRights(CapRights::READ.0 | CapRights::WRITE.0 | CapRights::MAP.0),
            0,
            0,
            1,
        )
        .unwrap();
        let vs_root = create_vspace(root_lbl).unwrap();
        // Kernel higher-half info pointer must return `-EFAULT` and roll back MMIO PTEs!
        assert_eq!(
            sys_fb_map(
                cs_root,
                root_lbl,
                vs_root.id,
                fb_child.0,
                0x1200_0000,
                0xffff_8000_0000_0000
            ),
            Err(SysError::Fault)
        );
        assert!(walk_vspace_address(vs_root.id, 0x1200_0000).is_none());
        destroy_vspace(vs_root).unwrap();
        cspace_delete_cap(cs_root, root_lbl, fb_child, 1).unwrap();
        cspace_destroy_object(cs_root, root_lbl, fb_root, 1).unwrap();
    }

    // 16. Blocking IPC inside `preempt_disable` (`preempt_count > 0`) rejected with `-EIRQCTX`.
    preempt_disable();
    let irq_guard_res = ep_recv(cs_a, u_a, ep_a, false, 0, None);
    preempt_enable();
    assert_eq!(irq_guard_res, Err(SysError::IrqForbidden));

    // 14. Stale endpoint handles rejected after `cspace_destroy_object` + slot reuse (`ABA` guard).
    cspace_destroy_object(cs_a, u_a, ep_a, 1).unwrap();
    assert_eq!(ep_recv(cs_a, u_a, ep_a, true, 0, None), Err(SysError::Revoked));
    assert_eq!(ep_recv(cs_a, u_a, send_only, true, 0, None), Err(SysError::Revoked));
    let (ep_reborn, _) = ep_create(cs_a, u_a, u_a, 0).unwrap();
    assert_eq!(ep_recv(cs_a, u_a, ep_a, true, 0, None), Err(SysError::BadCap));
    cspace_destroy_object(cs_a, u_a, ep_reborn, 1).unwrap();

    // 3. Pending sender & pending receiver woken with `-EREVOKED` upon `cspace_revoke_children`!
    let (ep_s_root, _) = ep_create(cs_a, u_a, u_a, 0).unwrap();
    let (ep_r_root, _) = ep_create(cs_a, u_a, u_a, 0).unwrap();
    let s_child = cspace_derive(cs_a, u_a, ep_s_root, CapRights::SEND, 0, 0, 1).unwrap();
    let r_child = cspace_derive(cs_a, u_a, ep_r_root, CapRights::RECV, 0, 0, 1).unwrap();
    RES_SHARED_CAP0.store(s_child.0, Ordering::Release);
    RES_SHARED_CAP1.store(r_child.0, Ordering::Release);
    REV_WAIT_READY.store(0, Ordering::Release);
    REV_SEND_ERR.store(0, Ordering::Release);
    REV_RECV_ERR.store(0, Ordering::Release);

    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xe1, 2);
    try_spawn_thread_bound(
        2,
        0,
        KERNEL_VSPACE_ID,
        cs_a,
        u_a,
        revoke_blocked_sender_worker,
        cs_a as u64,
        true,
    )
    .unwrap();
    try_spawn_thread_bound(
        2,
        1 % online_cpus,
        KERNEL_VSPACE_ID,
        cs_a,
        u_a,
        revoke_blocked_receiver_worker,
        cs_a as u64,
        true,
    )
    .unwrap();
    while REV_WAIT_READY.load(Ordering::Acquire) < 2 {
        let _ = idle_dispatch_once(0);
        core::hint::spin_loop();
    }
    for _ in 0..8 {
        let _ = idle_dispatch_once(0);
    }
    cspace_revoke_children(cs_a, u_a, ep_s_root, 1).unwrap();
    cspace_revoke_children(cs_a, u_a, ep_r_root, 1).unwrap();
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);
    assert_eq!(REV_SEND_ERR.load(Ordering::Acquire), 1);
    assert_eq!(REV_RECV_ERR.load(Ordering::Acquire), 1);
    cspace_destroy_object(cs_a, u_a, ep_s_root, 1).unwrap();
    cspace_destroy_object(cs_a, u_a, ep_r_root, 1).unwrap();

    // 4, 5 & 7. Server exit during `WaitingReply` (`-EPIPE`), `ReplyToken` reuse, & Double Reply!
    let u_c = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(12, 1),
        1,
    );
    let (cs_c, _) = create_cspace(u_c, 0x74).unwrap();
    for mode in [0u64, 1u64] {
        let (ep_m, _) = ep_create(cs_c, u_c, u_c, 0).unwrap();
        let srv_cap = cspace_derive(
            cs_c,
            u_c,
            ep_m,
            CapRights(CapRights::RECV.0 | CapRights::REPLY.0),
            0,
            0,
            1,
        )
        .unwrap();
        let cli_cap = cspace_derive(
            cs_c,
            u_c,
            ep_m,
            CapRights(CapRights::SEND.0 | CapRights::CALL.0),
            0,
            0x55,
            1,
        )
        .unwrap();
        RES_SHARED_CAP0.store(srv_cap.0, Ordering::Release);
        RES_SHARED_CAP1.store(cli_cap.0, Ordering::Release);

        set_preemptive_mode_active(true);
        init_scheduler(online_cpus);
        begin_preemptive_epoch(0xe2 + (mode as u8), 2);
        let arg = (cs_c as u64) | (mode << 16);
        try_spawn_thread_bound(
            2,
            0,
            KERNEL_VSPACE_ID,
            cs_c,
            u_c,
            server_exit_and_double_reply_server,
            arg,
            true,
        )
        .unwrap();
        try_spawn_thread_bound(
            2,
            1 % online_cpus,
            KERNEL_VSPACE_ID,
            cs_c,
            u_c,
            server_exit_and_double_reply_client,
            arg,
            true,
        )
        .unwrap();
        drain_preemptive_epoch_and_audit(online_cpus);
        set_preemptive_mode_active(false);
        cspace_destroy_object(cs_c, u_c, ep_m, 1).unwrap();
    }
    assert!(SRV_EXIT_CALLER_OK.load(Ordering::Acquire));
    assert!(TOKEN_REUSE_OK.load(Ordering::Acquire));
    assert!(DOUBLE_REPLY_OK.load(Ordering::Acquire));
    destroy_cspace(cs_c).unwrap();

    // 8 & 12. SMP Multi-Client Contention + Bounded Timeout / Notification Race (`Stage B.4, F.8, F.12`).
    let u_d = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(13, 1),
        1,
    );
    let (cs_d, _) = create_cspace(u_d, 0x75).unwrap();
    let (ep_cont, _) = ep_create(cs_d, u_d, u_d, 0).unwrap();
    let (ep_idle, _) = ep_create(cs_d, u_d, u_d, 0).unwrap();
    let (ntf_cont, _) = notif_create(cs_d, u_d, u_d, 0).unwrap();
    let (ntf_idle, _) = notif_create(cs_d, u_d, u_d, 0).unwrap();
    let cli_cont = cspace_derive(
        cs_d,
        u_d,
        ep_cont,
        CapRights(CapRights::SEND.0 | CapRights::CALL.0),
        0,
        0xbeef,
        1,
    )
    .unwrap();

    SMP_DONE_CALLS.store(0, Ordering::Release);
    SMP_TIMEOUT_RACE_OK.store(0, Ordering::Release);
    RES_SHARED_CAP0.store(ep_cont.0, Ordering::Release);
    RES_SHARED_CAP1.store(cli_cont.0, Ordering::Release);
    RES_SHARED_CAP2.store(ntf_cont.0, Ordering::Release);

    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xe4, 1 + SMP_CLIENTS);
    try_spawn_thread_bound(
        2,
        0,
        KERNEL_VSPACE_ID,
        cs_d,
        u_d,
        smp_contended_server_worker,
        cs_d as u64,
        true,
    )
    .unwrap();
    for cid in 0..SMP_CLIENTS {
        let arg = (cs_d as u64) | ((cid as u64) << 16);
        try_spawn_thread_bound(
            2,
            (cid + 1) % online_cpus,
            KERNEL_VSPACE_ID,
            cs_d,
            u_d,
            smp_contended_client_worker,
            arg,
            true,
        )
        .unwrap();
    }
    drain_preemptive_epoch_and_audit(online_cpus);
    assert_eq!(
        SMP_DONE_CALLS.load(Ordering::Acquire),
        SMP_CLIENTS * SMP_ROUNDS_PER_CLIENT
    );
    // Verify accumulated non-blocking `Notification` bits from server!
    let accum_bits = notif_wait(cs_d, u_d, ntf_cont, true, 0).unwrap();
    assert_ne!(accum_bits, 0);

    // Timeout race test on idle endpoint + notification.
    RES_SHARED_CAP0.store(ep_idle.0, Ordering::Release);
    RES_SHARED_CAP2.store(ntf_idle.0, Ordering::Release);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xe5, 1);
    try_spawn_thread_bound(
        2,
        0,
        KERNEL_VSPACE_ID,
        cs_d,
        u_d,
        timeout_waiter_worker,
        cs_d as u64,
        true,
    )
    .unwrap();
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);
    assert_eq!(SMP_TIMEOUT_RACE_OK.load(Ordering::Acquire), 1);

    cspace_destroy_object(cs_d, u_d, ep_cont, 1).unwrap();
    cspace_destroy_object(cs_d, u_d, ep_idle, 1).unwrap();
    cspace_destroy_object(cs_d, u_d, ntf_cont, 1).unwrap();
    cspace_destroy_object(cs_d, u_d, ntf_idle, 1).unwrap();
    destroy_cspace(cs_d).unwrap();
    destroy_cspace(cs_b).unwrap();
    destroy_cspace(cs_a).unwrap();
    destroy_cspace(cs_root).unwrap();

    flush_all_magazines_quiescent(online_cpus);
    let after = measure_fragmentation(online_cpus);
    let leaked = before
        .free_global_frames
        .saturating_sub(after.free_global_frames);
    assert_eq!(leaked, 0, "Frame leak in Stage F IPC security suite: {}", leaked);

    serial_println!(
        "[DRY-IPC-SEC] PASS: cpus={} scenarios=16/16 smp_calls={} leaked_frames={}",
        online_cpus,
        SMP_CLIENTS * SMP_ROUNDS_PER_CLIENT,
        leaked
    );
}

/// Stages C, D & E: Launch real CPL=3 `dry-init.elf` (`SecDomain::RootSystem`), which spawns
/// `dry-echo.elf` (`SecDomain::Userspace`) + `dry-display.elf` (`RootSystem` display compartment).
fn run_stage_cde_root_services_and_display(online_cpus: usize) {
    let init_elf =
        crate::limine::get_boot_module("dry-init.elf").expect("Missing dry-init.elf boot module");
    let _ =
        crate::limine::get_boot_module("dry-echo.elf").expect("Missing dry-echo.elf boot module");

    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);

    serial_println!("[DRY-BOOT] Kernel ready");

    let root_lbl = SecLabel::new(SecDomain::RootSystem, SecFlags::NONE, 0, 1);

    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xe6, 1);

    let init_proc = spawn_elf_process_with(
        init_elf,
        SpawnOptions {
            subj_label: root_lbl,
            arg0: BOOT_INFO_VADDR,
            arg1: 0,
            target_cpu: 0,
            trace_lifecycle: false,
            user_fs_base: 0,
            user_gs_base: 0,
        },
        |cs, lbl, vs_cap, vs_h| {
            let echo_cap = mint_boot_module_cap(cs, lbl, "dry-echo.elf")?;
            let disp_cap = mint_boot_module_cap(cs, lbl, "dry-display.elf").unwrap_or(CapPtr::NULL);
            let fb_cap = if is_framebuffer_available() {
                mint_root_framebuffer_cap(cs, lbl)?
            } else {
                CapPtr::NULL
            };

            let (info_fr_cap, info_fh) =
                cspace_alloc_frame(cs, lbl, lbl, PageOrder::Order4KiB, false, 0)?;
            let boot_info = RootInitBootInfo {
                magic: ROOT_INIT_BOOT_MAGIC,
                echo_mod_cap: echo_cap.0,
                display_mod_cap: disp_cap.0,
                fb_cap: fb_cap.0,
            };
            // SAFETY: `info_fh` is freshly allocated and zeroed 4 KiB frame.
            unsafe {
                copy_nonoverlapping(
                    (&raw const boot_info).cast::<u8>(),
                    info_fh.hhdm_ptr(),
                    core::mem::size_of::<RootInitBootInfo>(),
                );
            }
            map_4k(vs_h, BOOT_INFO_VADDR, info_fh, VmProt::USER_RO)
                .map_err(|_| CapError::PagingError)?;
            crate::cspace::revoke::attach_cap_mapping_record(
                crate::cspace::cspace_lookup_fast(cs, lbl, info_fr_cap, CapRights::READ, 0)?
                    .global_slot,
                info_fr_cap.slot_gen(),
                vs_h.id,
                vs_h.generation,
                BOOT_INFO_VADDR,
                false,
            )?;
            let _ = vs_cap;
            Ok((BOOT_INFO_VADDR, 0))
        },
    )
    .expect("Failed to spawn CPL=3 dry-init.elf");

    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);

    let ReclaimStatus::ReclaimedNow(outcome) =
        reclaim_process(init_proc.handle).expect("Failed to reclaim dry-init")
    else {
        panic!("Expected ReclaimedNow for dry-init");
    };
    assert_eq!(
        outcome,
        ProcessOutcome::Exited(0),
        "dry-init failed with outcome {:?}",
        outcome
    );

    // Verify rendered pixels directly in GOP VRAM via an ephemeral supervisor-only RO+UC `VSpace`
    // (`KERNEL_VSPACE` intentionally leaves PCI MMIO BARs unmapped in HHDM for least privilege).
    if let Some(vfb) = framebuffer_summary() {
        let probe_vs = create_vspace(root_lbl).expect("probe vspace");
        let ro_uc = VmProt(VmProt::READ.0 | VmProt::UNCACHED.0);
        let probe_base = 0x2000_0000u64;
        for p in 0..(vfb.num_pages as u64) {
            map_mmio_4k(probe_vs, probe_base + p * 4096, vfb.phys_base + p * 4096, ro_uc)
                .expect("probe map_mmio_4k");
        }
        let saved_if = crate::arch::irq_save_cli();
        switch_cpu_vspace_if_needed(0, probe_vs.id);
        let px_ptr = probe_base as *const u32;
        let total_px = ((vfb.info.pitch as usize) / 4) * (vfb.info.height as usize);
        // SAFETY: `probe_base` maps `vfb.num_pages * 4 KiB` of GOP VRAM as supervisor `RO + UC`.
        let corner_bg = unsafe { read_volatile(px_ptr) };
        let mut distinct_non_bg = 0usize;
        let step = (total_px / 16384).max(1);
        let mut idx = 0usize;
        while idx < total_px {
            let val = unsafe { read_volatile(px_ptr.add(idx)) };
            if val != corner_bg {
                distinct_non_bg += 1;
            }
            idx += step;
        }
        switch_cpu_vspace_if_needed(0, KERNEL_VSPACE_ID);
        crate::arch::irq_restore(saved_if);
        destroy_vspace(probe_vs).expect("destroy probe vspace");

        assert!(
            distinct_non_bg >= 32,
            "Framebuffer pixels appear blank (distinct_non_bg={})",
            distinct_non_bg
        );
        serial_println!(
            "[DRY-FB] Verified Ring 3 pixels: res={}x{} bg_px={:#010x} sampled_fg={}",
            vfb.info.width,
            vfb.info.height,
            corner_bg,
            distinct_non_bg
        );
    }

    flush_all_magazines_quiescent(online_cpus);
    let after = measure_fragmentation(online_cpus);
    let leaked = before
        .free_global_frames
        .saturating_sub(after.free_global_frames);
    assert_eq!(
        leaked, 0,
        "Frame leak after dry_init/dry_echo/dry_display lifecycle: {}",
        leaked
    );

    serial_println!(
        "[DRY-SERVICES] PASS: cpus={} init_echo_ok=true crash_restart_ok=true display_ok={} leaked_frames={}",
        online_cpus,
        is_framebuffer_available(),
        leaked
    );
}

/// Entry point invoked from `kernel_main` (`main.rs`) for `Milestone 1C`.
pub fn verify_ipc_services_and_display(cmdline: &str, online_cpus: usize) {
    maybe_run_ipc_fault_test(cmdline, online_cpus);
    run_stage_cde_root_services_and_display(online_cpus);
    run_stage_f_ipc_security_and_races(online_cpus);
    crate::ipc::bench::run_ipc_benchmarks(online_cpus);
}
