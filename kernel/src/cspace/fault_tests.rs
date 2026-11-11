// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `Milestone 1B-C` Stage D: 10 Real-Lifecycle Negative Capability & Security Fault Isolation Tests.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::cspace::objects::{
    cspace_alloc_frame, cspace_spawn_tcb, cspace_vspace_create, cspace_vspace_map_4k,
};
use crate::cspace::revoke::{
    cspace_delete_cap, cspace_destroy_object, cspace_revoke_children, unwrap_cap_or_panic,
};
use crate::cspace::{
    create_cspace, cspace_derive, cspace_invariant_panic, cspace_lookup_fast, cspace_transfer,
    CSpaceId, CapError, CapPtr, CNODE_RADIX_BITS,
};
use crate::pmm::PageOrder;
use crate::sched::preempt_tests::{begin_preemptive_epoch, drain_preemptive_epoch_and_audit};
use crate::sched::{
    exit_current_thread, init_scheduler, note_current_thread_progress, set_preemptive_mode_active,
    try_spawn_thread_in_vspace,
};
use crate::security::{encode_compartment, CapRights, SecDomain, SecFlags, SecLabel};
use crate::serial_println;
use crate::vspace::pte::VmProt;
use crate::vspace::{switch_cpu_vspace_if_needed, KERNEL_VSPACE_ID};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CSpaceFaultTest {
    None,
    ForgedHandle,
    StaleAba,
    RightsAmp,
    CrossDomain,
    ConcurrentRevoke,
    StaleMapping,
    ParentChildLifetime,
    TransferEscalation,
    DoubleDestroy,
    ActiveReclaim,
}

impl CSpaceFaultTest {
    pub fn from_cmdline(cmdline: &str) -> Self {
        if cmdline.contains("cspace_fault=forged_handle") {
            Self::ForgedHandle
        } else if cmdline.contains("cspace_fault=stale_aba") {
            Self::StaleAba
        } else if cmdline.contains("cspace_fault=rights_amp") {
            Self::RightsAmp
        } else if cmdline.contains("cspace_fault=cross_domain") {
            Self::CrossDomain
        } else if cmdline.contains("cspace_fault=concurrent_revoke") {
            Self::ConcurrentRevoke
        } else if cmdline.contains("cspace_fault=stale_mapping") {
            Self::StaleMapping
        } else if cmdline.contains("cspace_fault=parent_child_lifetime") {
            Self::ParentChildLifetime
        } else if cmdline.contains("cspace_fault=transfer_escalation") {
            Self::TransferEscalation
        } else if cmdline.contains("cspace_fault=double_destroy") {
            Self::DoubleDestroy
        } else if cmdline.contains("cspace_fault=active_reclaim") {
            Self::ActiveReclaim
        } else {
            Self::None
        }
    }
}

static RACE_CSPACE_ID: AtomicU64 = AtomicU64::new(0);
static RACE_CHILD_PTR: AtomicU64 = AtomicU64::new(0);
static RACE_READER_STARTED: AtomicBool = AtomicBool::new(false);
static RACE_REVOKE_DONE: AtomicBool = AtomicBool::new(false);
static ACTIVE_TCB_RUNNING: AtomicBool = AtomicBool::new(false);
static ACTIVE_TCB_STOP: AtomicBool = AtomicBool::new(false);
static STALE_TLB_WARMED: AtomicBool = AtomicBool::new(false);
static STALE_TLB_RELEASE: AtomicBool = AtomicBool::new(false);

fn concurrent_revoke_reader_worker(_arg: u64) {
    let cs = RACE_CSPACE_ID.load(Ordering::Acquire) as CSpaceId;
    let child = CapPtr(RACE_CHILD_PTR.load(Ordering::Acquire));
    let subj = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, encode_compartment(1, 1), 1);

    // Confirm valid lookup works before revocation races in, then hammer `cspace_lookup_fast`
    // concurrently while CPU 0 executes `cspace_revoke_children`.
    assert!(cspace_lookup_fast(cs, subj, child, CapRights::READ, 1).is_ok());
    RACE_READER_STARTED.store(true, Ordering::Release);

    while !RACE_REVOKE_DONE.load(Ordering::Acquire) {
        let _ = cspace_lookup_fast(cs, subj, child, CapRights::READ, 1);
        note_current_thread_progress();
    }

    // Once revocation completes, enforced lookup MUST deterministically fail closed.
    let post = cspace_lookup_fast(cs, subj, child, CapRights::READ, 1);
    if matches!(post, Err(CapError::Revoked | CapError::StaleGenerationAba)) {
        cspace_invariant_panic(format_args!(
            "Concurrent lookup on revoked capability rejected: cap={:#x} err={:?}",
            child.0, post
        ));
    }
    exit_current_thread()
}

fn stale_mapping_peer_warmer(_arg: u64) {
    // Warm remote CPU TLB at `0x0070_0000` before BSP revokes the backing capability!
    // SAFETY: Mapped `USER_RW` in shared `VSpace` before `STALE_TLB_WARMED` is set.
    let val = unsafe { read_volatile(0x0070_0000usize as *const u64) };
    assert_eq!(val, 0xcafe_babe_7000_0001);
    STALE_TLB_WARMED.store(true, Ordering::Release);
    while !STALE_TLB_RELEASE.load(Ordering::Acquire) {
        note_current_thread_progress();
        core::hint::spin_loop();
    }
    exit_current_thread()
}

fn active_tcb_spinner_worker(_arg: u64) {
    ACTIVE_TCB_RUNNING.store(true, Ordering::Release);
    while !ACTIVE_TCB_STOP.load(Ordering::Acquire) {
        note_current_thread_progress();
        core::hint::spin_loop();
    }
    exit_current_thread()
}

pub fn run_cspace_negative_fault_test(fault: CSpaceFaultTest, online_cpus: usize) -> ! {
    let u_lbl = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, encode_compartment(1, 1), 10);
    let u2_lbl = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, encode_compartment(2, 1), 20);
    let root_priv_lbl = SecLabel::new(
        SecDomain::RootSystem,
        SecFlags::NO_DELEGATE,
        encode_compartment(0, 1),
        99,
    );

    match fault {
        CSpaceFaultTest::None => unreachable!(),

        CSpaceFaultTest::ForgedHandle => {
            let (cs, _) = create_cspace(u_lbl, 0x5a).unwrap();
            let (vs_cap, _) = cspace_vspace_create(cs, u_lbl, 0).unwrap();
            let (real_frame, _) =
                cspace_alloc_frame(cs, u_lbl, u_lbl, PageOrder::Order4KiB, false, 0).unwrap();
            // Craft forged handle targeting real slot with wrong guard (`0x33 != 0x5a`).
            let forged = CapPtr::pack(
                0x33,
                CNODE_RADIX_BITS,
                real_frame.slot_gen(),
                real_frame.slot_idx(),
            );
            let res =
                cspace_vspace_map_4k(cs, u_lbl, vs_cap, forged, 0x0070_0000, VmProt::USER_RO, 1);
            unwrap_cap_or_panic(res, "forged_guard_capptr");
            unreachable!()
        }

        CSpaceFaultTest::StaleAba => {
            let (cs, _) = create_cspace(u_lbl, 0x61).unwrap();
            let (vs_cap, _) = cspace_vspace_create(cs, u_lbl, 0).unwrap();
            let (root_a, _) =
                cspace_alloc_frame(cs, u_lbl, u_lbl, PageOrder::Order4KiB, false, 0).unwrap();
            let ro_map = CapRights(CapRights::READ.0 | CapRights::MAP.0);
            let stale_leaf = cspace_derive(cs, u_lbl, root_a, ro_map, 0, 0, 1).unwrap();
            cspace_delete_cap(cs, u_lbl, stale_leaf, 1).unwrap();

            // Reallocate the exact same slot (`stale_leaf.slot_idx()`) to a brand-new frame!
            let (reused_cap, _) =
                cspace_alloc_frame(cs, u_lbl, u_lbl, PageOrder::Order4KiB, false, 0).unwrap();
            assert_eq!(reused_cap.slot_idx(), stale_leaf.slot_idx());
            assert_ne!(reused_cap.slot_gen(), stale_leaf.slot_gen());

            // Attempt mapping via cached `stale_leaf` -> ABA caught by `slot_gen` mismatch!
            let res = cspace_vspace_map_4k(
                cs,
                u_lbl,
                vs_cap,
                stale_leaf,
                0x0070_0000,
                VmProt::USER_RO,
                1,
            );
            unwrap_cap_or_panic(res, "aba_reused_slot");
            unreachable!()
        }

        CSpaceFaultTest::RightsAmp => {
            let (cs, _) = create_cspace(u_lbl, 0x62).unwrap();
            let (vs_cap, _) = cspace_vspace_create(cs, u_lbl, 0).unwrap();
            let (root_rw, _) =
                cspace_alloc_frame(cs, u_lbl, u_lbl, PageOrder::Order4KiB, false, 0).unwrap();
            let ro_only = CapRights(CapRights::READ.0 | CapRights::MAP.0);
            let child_ro = cspace_derive(cs, u_lbl, root_rw, ro_only, 0, 0, 1).unwrap();

            // 1. Verify derive amplification (`RO -> RW`) fails closed; 2. Verify `USER_RW` map with `RO` cap panics.
            assert_eq!(
                cspace_derive(
                    cs,
                    u_lbl,
                    child_ro,
                    CapRights(CapRights::READ.0 | CapRights::WRITE.0 | CapRights::MAP.0),
                    0,
                    0,
                    1
                ),
                Err(CapError::RightsAmplification)
            );
            let map_amp =
                cspace_vspace_map_4k(cs, u_lbl, vs_cap, child_ro, 0x0070_0000, VmProt::USER_RW, 1);
            unwrap_cap_or_panic(map_amp, "map_rw_with_ro_cap");
            unreachable!()
        }

        CSpaceFaultTest::CrossDomain => {
            let (cs_root, _) = create_cspace(root_priv_lbl, 0x63).unwrap();
            let (priv_frame, _) = cspace_alloc_frame(
                cs_root,
                root_priv_lbl,
                root_priv_lbl,
                PageOrder::Order4KiB,
                false,
                0,
            )
            .unwrap();
            // `Userspace` subject attempting direct lookup/access against `RootSystem` private object:
            let res = cspace_lookup_fast(cs_root, u_lbl, priv_frame, CapRights::READ, 1);
            unwrap_cap_or_panic(res, "userspace_to_rootsystem_private");
            unreachable!()
        }

        CSpaceFaultTest::ConcurrentRevoke => {
            let (cs, _) = create_cspace(u_lbl, 0x64).unwrap();
            let (root_fr, _) =
                cspace_alloc_frame(cs, u_lbl, u_lbl, PageOrder::Order4KiB, false, 0).unwrap();
            let ro_map = CapRights(CapRights::READ.0 | CapRights::MAP.0);
            let child = cspace_derive(cs, u_lbl, root_fr, ro_map, 0, 0, 1).unwrap();

            RACE_CSPACE_ID.store(cs as u64, Ordering::Release);
            RACE_CHILD_PTR.store(child.0, Ordering::Release);
            RACE_READER_STARTED.store(false, Ordering::Release);
            RACE_REVOKE_DONE.store(false, Ordering::Release);

            set_preemptive_mode_active(true);
            init_scheduler(online_cpus);
            begin_preemptive_epoch(0xf1, 1);
            assert!(
                try_spawn_thread_in_vspace(
                    1,
                    1 % online_cpus,
                    KERNEL_VSPACE_ID,
                    concurrent_revoke_reader_worker,
                    0,
                    true
                )
                .is_some()
            );
            while !RACE_READER_STARTED.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            let revoked = cspace_revoke_children(cs, u_lbl, root_fr, 1).unwrap();
            assert_eq!(revoked, 1);
            RACE_REVOKE_DONE.store(true, Ordering::Release);
            drain_preemptive_epoch_and_audit(online_cpus);
            unreachable!()
        }

        CSpaceFaultTest::StaleMapping => {
            let (cs, _) = create_cspace(u_lbl, 0x65).unwrap();
            let (vs_cap, vs_h) = cspace_vspace_create(cs, u_lbl, 0).unwrap();
            let (root_fr, _) =
                cspace_alloc_frame(cs, u_lbl, u_lbl, PageOrder::Order4KiB, false, 0).unwrap();
            let child_rw = cspace_derive(
                cs,
                u_lbl,
                root_fr,
                CapRights(CapRights::READ.0 | CapRights::WRITE.0 | CapRights::MAP.0),
                0,
                0,
                1,
            )
            .unwrap();

            cspace_vspace_map_4k(cs, u_lbl, vs_cap, child_rw, 0x0070_0000, VmProt::USER_RW, 1)
                .unwrap();
            switch_cpu_vspace_if_needed(0, vs_h.id);
            // SAFETY: `0x0070_0000` is mapped `USER_RW` in `vs_h` prior to capability revocation.
            unsafe {
                write_volatile(0x0070_0000usize as *mut u64, 0xcafe_babe_7000_0001);
                assert_eq!(
                    read_volatile(0x0070_0000usize as *const u64),
                    0xcafe_babe_7000_0001
                );
            }
            switch_cpu_vspace_if_needed(0, KERNEL_VSPACE_ID);

            if online_cpus >= 2 {
                STALE_TLB_WARMED.store(false, Ordering::Release);
                STALE_TLB_RELEASE.store(false, Ordering::Release);
                set_preemptive_mode_active(true);
                init_scheduler(online_cpus);
                begin_preemptive_epoch(0xf2, 1);
                assert!(
                    try_spawn_thread_in_vspace(1, 1, vs_h.id, stale_mapping_peer_warmer, 0, true)
                        .is_some()
                );
                while !STALE_TLB_WARMED.load(Ordering::Acquire) {
                    core::hint::spin_loop();
                }
                // Revoke `root_fr`'s children WHILE AP 1 actively holds `vs_h` in `CR3`!
                let rev = cspace_revoke_children(cs, u_lbl, root_fr, 1).unwrap();
                assert_eq!(rev, 1);
                STALE_TLB_RELEASE.store(true, Ordering::Release);
                drain_preemptive_epoch_and_audit(online_cpus);
                set_preemptive_mode_active(false);
            } else {
                let rev = cspace_revoke_children(cs, u_lbl, root_fr, 1).unwrap();
                assert_eq!(rev, 1);
            }

            switch_cpu_vspace_if_needed(0, vs_h.id);
            serial_println!(
                "[DRY-CSPACE-INFO] Testing post-revoke stale VSpace mapping hardware #PF at vaddr=0x700000..."
            );
            // SAFETY: Intentionally dereferences the revoked & unmapped virtual address to prove hardware `#PF`!
            unsafe {
                let _ = read_volatile(0x0070_0000usize as *const u64);
            }
            unreachable!()
        }

        CSpaceFaultTest::ParentChildLifetime => {
            let (cs_a, _) = create_cspace(u_lbl, 0x66).unwrap();
            let (cs_b, _) = create_cspace(u2_lbl, 0x67).unwrap();
            let (vs_b, _) = cspace_vspace_create(cs_b, u2_lbl, 0).unwrap();
            let (root_fr, _) =
                cspace_alloc_frame(cs_a, u_lbl, u_lbl, PageOrder::Order4KiB, false, 0).unwrap();
            let child = cspace_derive(
                cs_a,
                u_lbl,
                root_fr,
                CapRights(CapRights::READ.0 | CapRights::MAP.0 | CapRights::GRANT.0),
                0,
                0,
                1,
            )
            .unwrap();
            let xfer = cspace_transfer(
                cs_a,
                u_lbl,
                child,
                cs_b,
                CapRights(CapRights::READ.0 | CapRights::MAP.0),
                0,
                0x99,
                1,
            )
            .unwrap();
            let grandchild = cspace_derive(
                cs_b,
                u2_lbl,
                xfer.dst_cap,
                CapRights(CapRights::READ.0 | CapRights::MAP.0),
                0,
                0,
                1,
            )
            .unwrap();

            // Revoke ancestor `root_fr` -> must invalidate `child`, `xfer.dst_cap`, AND `grandchild`!
            let rev_cnt = cspace_revoke_children(cs_a, u_lbl, root_fr, 1).unwrap();
            assert_eq!(rev_cnt, 3);
            let res = cspace_vspace_map_4k(
                cs_b,
                u2_lbl,
                vs_b,
                grandchild,
                0x0070_0000,
                VmProt::USER_RO,
                1,
            );
            if res.is_err() {
                cspace_invariant_panic(format_args!(
                    "Parent-child capability lifetime violation rejected: grandchild={:#x} err={:?}",
                    grandchild.0, res
                ));
            }
            unreachable!()
        }

        CSpaceFaultTest::TransferEscalation => {
            let (cs_a, _) = create_cspace(u_lbl, 0x68).unwrap();
            let (cs_b, _) = create_cspace(u2_lbl, 0x69).unwrap();
            let (root_fr, _) =
                cspace_alloc_frame(cs_a, u_lbl, u_lbl, PageOrder::Order4KiB, false, 0).unwrap();
            // Derive a capability WITHOUT `CapRights::GRANT`, then attempt cross-compartment transfer!
            let no_grant = cspace_derive(
                cs_a,
                u_lbl,
                root_fr,
                CapRights(CapRights::READ.0 | CapRights::MAP.0),
                0,
                0,
                1,
            )
            .unwrap();
            let xfer = cspace_transfer(
                cs_a,
                u_lbl,
                no_grant,
                cs_b,
                CapRights(CapRights::READ.0 | CapRights::MAP.0),
                0,
                0x11,
                1,
            );
            unwrap_cap_or_panic(xfer, "cross_compartment_without_grant");
            unreachable!()
        }

        CSpaceFaultTest::DoubleDestroy => {
            let (cs, _) = create_cspace(u_lbl, 0x6a).unwrap();
            let (vs_cap, _) = cspace_vspace_create(cs, u_lbl, 0).unwrap();
            cspace_destroy_object(cs, u_lbl, vs_cap, 1).unwrap();
            let second = cspace_destroy_object(cs, u_lbl, vs_cap, 1);
            if second.is_err() {
                cspace_invariant_panic(format_args!(
                    "Double destroy / double revoke detected on cap={:#x} err={:?}",
                    vs_cap.0, second
                ));
            }
            unreachable!()
        }

        CSpaceFaultTest::ActiveReclaim => {
            let (cs, _) = create_cspace(u_lbl, 0x6b).unwrap();
            let (vs_cap, _) = cspace_vspace_create(cs, u_lbl, 0).unwrap();
            ACTIVE_TCB_RUNNING.store(false, Ordering::Release);
            ACTIVE_TCB_STOP.store(false, Ordering::Release);

            set_preemptive_mode_active(true);
            init_scheduler(online_cpus);
            begin_preemptive_epoch(0xf3, 1);
            let (tcb_cap, _) = cspace_spawn_tcb(
                cs,
                u_lbl,
                vs_cap,
                1,
                1 % online_cpus,
                active_tcb_spinner_worker,
                0,
                true,
                1,
            )
            .unwrap();

            while !ACTIVE_TCB_RUNNING.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            // Attempt to destroy/reclaim the actively executing `TCB` while AP 1 owns its stack!
            let res = cspace_destroy_object(cs, u_lbl, tcb_cap, 1);
            unwrap_cap_or_panic(res, "destroy_running_tcb");
            unreachable!()
        }
    }
}
