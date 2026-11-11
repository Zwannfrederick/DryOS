// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `Milestone 1B-C` Verification Suite: Positive Lifecycle & `ADR-007` Enforcement, Synthetic
//! `rdtsc` Microbenchmarks (`Stage E`), and Preemptive SMP Concurrent Revocation Stress.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::cspace::bench::measure_cspace_benchmarks;
use crate::cspace::fault_tests::{run_cspace_negative_fault_test, CSpaceFaultTest};
use crate::cspace::objects::{
    cspace_alloc_frame, cspace_create_readonly_export_frame, cspace_signal_tcb, cspace_spawn_tcb,
    cspace_untyped_carve, cspace_untyped_retype_frame4k, cspace_untyped_revoke_tree,
    cspace_untyped_split, cspace_vspace_create, cspace_vspace_map_2m, cspace_vspace_map_4k,
    cspace_vspace_protect_4k, cspace_vspace_unmap_4k,
};
use crate::cspace::revoke::{cspace_delete_cap, cspace_destroy_object, cspace_revoke_children};
use crate::cspace::{
    create_cspace, cspace_derive, cspace_lookup_fast, cspace_transfer, destroy_cspace, CSpaceId,
    CapError, CapObjectKind, CapPtr, CNODE_RADIX_BITS,
};
use crate::pmm::{
    adjust_frame_iommu_pin, flush_all_magazines_quiescent, measure_fragmentation, PageOrder,
};
use crate::sched::preempt_tests::{begin_preemptive_epoch, drain_preemptive_epoch_and_audit};
use crate::sched::{
    cancel_park_current, commit_park_current, current_cpu_idx, current_slot_on_cpu,
    exit_current_thread, init_scheduler, note_current_thread_progress, prepare_park_current,
    set_preemptive_mode_active, tcb, try_spawn_thread_in_vspace, yield_now,
};
use crate::security::{
    current_lease_epoch, decode_compartment, encode_compartment, set_lease_epoch, CapRights,
    SecDomain, SecFlags, SecLabel,
};
use crate::serial_println;
use crate::vspace::pte::{VmProt, PAGE_SIZE_4K};
use crate::vspace::{
    switch_cpu_vspace_if_needed, walk_vspace_address, VSpaceHandle, KERNEL_VSPACE_ID,
};

const STRESS_PAIRS: usize = 4;
const STRESS_ROUNDS: usize = 16;

static BOUND_TCB_READY: AtomicBool = AtomicBool::new(false);
static BOUND_TCB_VERIFIED: AtomicBool = AtomicBool::new(false);

static STRESS_SRC_CS: [AtomicU64; STRESS_PAIRS] = [const { AtomicU64::new(0) }; STRESS_PAIRS];
static STRESS_DST_CS: [AtomicU64; STRESS_PAIRS] = [const { AtomicU64::new(0) }; STRESS_PAIRS];
static STRESS_VS_CAP: [AtomicU64; STRESS_PAIRS] = [const { AtomicU64::new(0) }; STRESS_PAIRS];
static STRESS_VS_ID: [AtomicU64; STRESS_PAIRS] = [const { AtomicU64::new(0) }; STRESS_PAIRS];
static STRESS_SHARED_CHILD: [AtomicU64; STRESS_PAIRS] = [const { AtomicU64::new(0) }; STRESS_PAIRS];

static STRESS_LOOKUPS: AtomicU64 = AtomicU64::new(0);
static STRESS_DERIVES: AtomicU64 = AtomicU64::new(0);
static STRESS_TRANSFERS: AtomicU64 = AtomicU64::new(0);
static STRESS_MAPS: AtomicU64 = AtomicU64::new(0);
static STRESS_REVOKES: AtomicU64 = AtomicU64::new(0);
static STRESS_STALE_REJECTED: AtomicU64 = AtomicU64::new(0);
static STRESS_UAF_ERRORS: AtomicU64 = AtomicU64::new(0);

fn bound_tcb_worker(expected_pack: u64) {
    let exp_cs = (expected_pack & 0xffff) as u16;
    let exp_vs = ((expected_pack >> 16) & 0xffff) as u16;
    let cpu = current_cpu_idx();
    let my_slot = current_slot_on_cpu(cpu);
    assert_eq!(tcb(my_slot).cspace_id.load(Ordering::Acquire), exp_cs);
    assert_eq!(tcb(my_slot).vspace_id.load(Ordering::Acquire), exp_vs);

    loop {
        prepare_park_current();
        BOUND_TCB_READY.store(true, Ordering::Release);
        if tcb(my_slot).notif.consume() != 0 {
            cancel_park_current();
            break;
        }
        let _ = commit_park_current();
        if tcb(my_slot).notif.consume() != 0 {
            break;
        }
    }
    BOUND_TCB_VERIFIED.store(true, Ordering::Release);
    exit_current_thread()
}

fn verify_positive_cspace_objects_and_adr007(online_cpus: usize) {
    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);
    set_lease_epoch(1);

    let comp_u1_a1 = encode_compartment(1, 1);
    let comp_u1_a2 = encode_compartment(1, 2);
    let comp_u2_a1 = encode_compartment(2, 1);
    assert_eq!(decode_compartment(comp_u1_a2), (1, 2));

    let u1_a1 = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, comp_u1_a1, 10);
    let u1_a2 = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, comp_u1_a2, 11);
    let u2_a1 = SecLabel::new(SecDomain::Userspace, SecFlags::NONE, comp_u2_a1, 20);
    let root_sys = SecLabel::new(SecDomain::RootSystem, SecFlags::NONE, encode_compartment(0, 1), 1);
    let dev_ws = SecLabel::new(
        SecDomain::RootSystem,
        SecFlags::DEV_WORKSPACE,
        encode_compartment(0, 2),
        2,
    );
    let kern_subj = SecLabel::new(SecDomain::Kernel, SecFlags::NONE, 0, 0);

    let (cs_u1a1, self_cnode_cap) = create_cspace(u1_a1, 0x51).unwrap();
    let (cs_u1a2, _) = create_cspace(u1_a2, 0x52).unwrap();
    let (cs_u2a1, _) = create_cspace(u2_a1, 0x53).unwrap();
    let (cs_root, _) = create_cspace(root_sys, 0x54).unwrap();
    let (cs_dev, _) = create_cspace(dev_ws, 0x55).unwrap();
    let (cs_kern, _) = create_cspace(kern_subj, 0x56).unwrap();

    // 1. Radix-ready `CapPtr`, guard & `CAP_NULL` checks (`Stage A.1 & A.2`).
    let resolved_self = cspace_lookup_fast(cs_u1a1, u1_a1, self_cnode_cap, CapRights::READ, 1).unwrap();
    assert_eq!(resolved_self.obj_kind, CapObjectKind::CNode);
    assert_eq!(
        cspace_lookup_fast(cs_u1a1, u1_a1, CapPtr::NULL, CapRights::READ, 1),
        Err(CapError::InvalidHandle)
    );
    let bad_radix = CapPtr::pack(0x51, CNODE_RADIX_BITS + 4, self_cnode_cap.slot_gen(), 1);
    assert_eq!(
        cspace_lookup_fast(cs_u1a1, u1_a1, bad_radix, CapRights::READ, 1),
        Err(CapError::InvalidHandle)
    );

    // 2. `VSpace`, `Untyped` (`carve -> split -> retype -> map -> revoke_tree`), and zero-scrub check.
    let (vs_cap, vs_h) = cspace_vspace_create(cs_u1a1, u1_a1, 0).unwrap();
    let ut_root = cspace_untyped_carve(cs_u1a1, u1_a1, 4, 0).unwrap();
    let (ut_c0, ut_c1) = cspace_untyped_split(cs_u1a1, u1_a1, ut_root, 1).unwrap();
    let (retyped_fr_cap, retyped_fh) =
        cspace_untyped_retype_frame4k(cs_u1a1, u1_a1, ut_c0, 1).unwrap();

    // Inherited `SecLabel` must match `u1_a1` (`Stage C.5` — no label-rewrite escalation).
    let rf_meta = cspace_lookup_fast(cs_u1a1, u1_a1, retyped_fr_cap, CapRights::READ, 1).unwrap();
    assert_eq!(rf_meta.obj_label, u1_a1);

    cspace_vspace_map_4k(cs_u1a1, u1_a1, vs_cap, retyped_fr_cap, 0x0060_0000, VmProt::USER_RW, 1)
        .unwrap();
    switch_cpu_vspace_if_needed(0, vs_h.id);
    // SAFETY: `0x0060_0000` is mapped `USER_RW` in `vs_h`.
    unsafe {
        write_volatile(0x0060_0000usize as *mut u64, 0x55aa_cc33_1234_9876);
        assert_eq!(
            read_volatile(0x0060_0000usize as *const u64),
            0x55aa_cc33_1234_9876
        );
    }
    switch_cpu_vspace_if_needed(0, KERNEL_VSPACE_ID);

    // Revoking `ut_root` automatically unmaps `0x0060_0000` (+ SMP TLB shootdown), invalidates
    // `ut_c0`, `ut_c1`, and `retyped_fr_cap`, and scrubs the physical frame (`INV-CAP-05`).
    let rev_items = cspace_untyped_revoke_tree(cs_u1a1, u1_a1, ut_root, 1).unwrap();
    assert!(rev_items >= 3);
    assert!(walk_vspace_address(vs_h.id, 0x0060_0000).is_none());
    assert_eq!(
        cspace_lookup_fast(cs_u1a1, u1_a1, ut_c0, CapRights::READ, 1),
        Err(CapError::Revoked)
    );
    assert_eq!(
        cspace_lookup_fast(cs_u1a1, u1_a1, ut_c1, CapRights::READ, 1),
        Err(CapError::Revoked)
    );
    assert_eq!(
        cspace_lookup_fast(cs_u1a1, u1_a1, retyped_fr_cap, CapRights::READ, 1),
        Err(CapError::Revoked)
    );
    // SAFETY: Physical frame is unmapped and scrubbed back into `ut_root`.
    let scrubbed = unsafe { read_volatile(retyped_fh.hhdm_ptr() as *const u64) };
    assert_eq!(scrubbed, 0);
    cspace_destroy_object(cs_u1a1, u1_a1, ut_root, 1).unwrap();

    // 3. `Frame2M` (`2 MiB` huge page) capability binding + IOMMU `pin_count` reclamation gate (`Stage B.4`).
    let (fr2m_cap, fr2m_h) =
        cspace_alloc_frame(cs_u1a1, u1_a1, u1_a1, PageOrder::Order2MiB, false, 0).unwrap();
    cspace_vspace_map_2m(cs_u1a1, u1_a1, vs_cap, fr2m_cap, 0x4000_0000, VmProt::USER_RW, 1)
        .unwrap();
    assert!(walk_vspace_address(vs_h.id, 0x4000_0000).unwrap().is_huge_2m);
    // While mapped (`map_ref_count > 0`), `cspace_destroy_object` MUST refuse reclamation!
    assert_eq!(
        cspace_destroy_object(cs_u1a1, u1_a1, fr2m_cap, 1),
        Err(CapError::BusyActiveObject)
    );
    // Unmap via child/explicit unmap_2m, then verify IOMMU `pin_count > 0` ALSO blocks reclamation!
    let _ = crate::vspace::huge::unmap_2m(vs_h, 0x4000_0000).unwrap();
    let _ = crate::cspace::revoke::detach_matching_cap_mapping_record(
        cspace_lookup_fast(cs_u1a1, u1_a1, fr2m_cap, CapRights::READ, 1)
            .unwrap()
            .global_slot,
        vs_h.id,
        0x4000_0000,
    );
    adjust_frame_iommu_pin(fr2m_h, 1).unwrap();
    assert_eq!(
        cspace_destroy_object(cs_u1a1, u1_a1, fr2m_cap, 1),
        Err(CapError::BusyActiveObject)
    );
    adjust_frame_iommu_pin(fr2m_h, -1).unwrap();
    cspace_destroy_object(cs_u1a1, u1_a1, fr2m_cap, 1).unwrap();

    // 4. `ADR-007` 3-Domain Wall (`Kernel`, `RootSystem`, `Userspace`) + `READ_ONLY_EXPORT` + `DRY Vault` Lease.
    let (kern_fr, _) =
        cspace_alloc_frame(cs_kern, kern_subj, kern_subj, PageOrder::Order4KiB, false, 0).unwrap();
    // `RootSystem` and `Userspace` can NEVER directly access or transfer `Kernel` objects (`Stage C.2 & C.3`).
    assert_eq!(
        cspace_lookup_fast(cs_kern, root_sys, kern_fr, CapRights::READ, 1),
        Err(CapError::DomainViolation)
    );
    assert_eq!(
        cspace_transfer(cs_kern, kern_subj, kern_fr, cs_root, CapRights::READ, 0, 0, 1),
        Err(CapError::TransferEscalation)
    );
    cspace_destroy_object(cs_kern, kern_subj, kern_fr, 1).unwrap();

    // Controlled `READ_ONLY_EXPORT` shared system library (`Stage C.8`):
    let (ro_lib_cap, _) =
        cspace_create_readonly_export_frame(cs_root, root_sys, b"DRY_SHARED_LIB_RO_V1").unwrap();
    // Cannot export with `WRITE`:
    assert!(
        cspace_transfer(
            cs_root,
            root_sys,
            ro_lib_cap,
            cs_u1a1,
            CapRights(CapRights::READ.0 | CapRights::WRITE.0 | CapRights::MAP.0),
            0,
            0,
            1
        )
        .is_err()
    );
    let ro_xfer = cspace_transfer(
        cs_root,
        root_sys,
        ro_lib_cap,
        cs_u1a1,
        CapRights::FRAME_SHARED_RO,
        0,
        0x77,
        1,
    )
    .unwrap();
    assert_eq!(ro_xfer.sender_label, root_sys);
    assert_eq!(ro_xfer.badge, 0x77);

    cspace_vspace_map_4k(
        cs_u1a1,
        u1_a1,
        vs_cap,
        ro_xfer.dst_cap,
        0x0061_0000,
        VmProt::USER_RX,
        1,
    )
    .unwrap();
    // Escalating PTE from `USER_RX` to `USER_RW` via `protect_4k` must fail closed!
    assert!(
        cspace_vspace_protect_4k(
            cs_u1a1,
            u1_a1,
            vs_cap,
            ro_xfer.dst_cap,
            0x0061_0000,
            VmProt::USER_RW,
            1
        )
        .is_err()
    );
    switch_cpu_vspace_if_needed(0, vs_h.id);
    // SAFETY: Mapped `USER_RX` at `0x0061_0000`.
    let sig_u64 = unsafe { read_volatile(0x0061_0000usize as *const u64) };
    assert_eq!(sig_u64, u64::from_le_bytes(*b"DRY_SHAR"));
    switch_cpu_vspace_if_needed(0, KERNEL_VSPACE_ID);
    cspace_vspace_unmap_4k(cs_u1a1, u1_a1, vs_cap, ro_xfer.dst_cap, 0x0061_0000, 1).unwrap();
    cspace_destroy_object(cs_root, root_sys, ro_lib_cap, 1).unwrap();

    // `DRY Vault` & `DEV_WORKSPACE` scoped lease (`Stage C.7`):
    let (vault_res, _) =
        cspace_alloc_frame(cs_root, root_sys, root_sys, PageOrder::Order4KiB, false, 0).unwrap();
    assert_eq!(
        cspace_lookup_fast(cs_root, dev_ws, vault_res, CapRights::READ, 1),
        Err(CapError::DomainViolation)
    );
    // Permanent (`lease_epoch = 0`) grant to `DEV_WORKSPACE` is forbidden; bounded lease (`epoch = 5`) works!
    assert_eq!(
        cspace_transfer(cs_root, root_sys, vault_res, cs_dev, CapRights::READ, 0, 0x10, 1),
        Err(CapError::TransferEscalation)
    );
    let dev_lease =
        cspace_transfer(cs_root, root_sys, vault_res, cs_dev, CapRights::READ, 5, 0x10, 1).unwrap();
    assert!(cspace_lookup_fast(cs_dev, dev_ws, dev_lease.dst_cap, CapRights::READ, 5).is_ok());
    set_lease_epoch(6);
    assert_eq!(
        cspace_lookup_fast(cs_dev, dev_ws, dev_lease.dst_cap, CapRights::READ, current_lease_epoch()),
        Err(CapError::LeaseExpired)
    );
    set_lease_epoch(1);
    cspace_destroy_object(cs_root, root_sys, vault_res, 1).unwrap();

    // 5. Cross-app (`u1_a1` vs `u1_a2`) & cross-user (`u1_a1` vs `u2_a1`) IPC transfer + badge monotonicity.
    let no_del_lbl = SecLabel::new(SecDomain::Userspace, SecFlags::NO_DELEGATE, comp_u1_a1, 10);
    let (nd_cap, _) =
        cspace_alloc_frame(cs_u1a1, u1_a1, no_del_lbl, PageOrder::Order4KiB, false, 0).unwrap();
    assert_eq!(
        cspace_transfer(cs_u1a1, u1_a1, nd_cap, cs_u1a2, CapRights::READ, 0, 1, 1),
        Err(CapError::TransferEscalation)
    );
    cspace_destroy_object(cs_u1a1, u1_a1, nd_cap, 1).unwrap();

    let (ipc_fr, _) =
        cspace_alloc_frame(cs_u1a1, u1_a1, u1_a1, PageOrder::Order4KiB, false, 0).unwrap();
    let ipc_rcpt = cspace_transfer(
        cs_u1a1,
        u1_a1,
        ipc_fr,
        cs_u2a1,
        CapRights(CapRights::READ.0 | CapRights::MAP.0),
        0,
        0xcafe_0042,
        1,
    )
    .unwrap();
    assert_eq!(ipc_rcpt.badge, 0xcafe_0042);
    assert_eq!(ipc_rcpt.sender_label, u1_a1);
    // Attempting to overwrite an already-badged capability (`0xcafe_0042 -> 0xdead`) fails closed!
    assert_eq!(
        cspace_derive(cs_u2a1, u2_a1, ipc_rcpt.dst_cap, CapRights::READ, 0, 0xdead, 1),
        Err(CapError::RightsAmplification)
    );
    cspace_destroy_object(cs_u1a1, u1_a1, ipc_fr, 1).unwrap();

    // 6. `TCB` capability binding, signal wake, and quiescent post-exit destruction (`Stage A.7`).
    BOUND_TCB_READY.store(false, Ordering::Release);
    BOUND_TCB_VERIFIED.store(false, Ordering::Release);
    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xf4, 1);
    let pack_arg = (cs_u1a1 as u64) | ((vs_h.id as u64) << 16);
    let (tcb_cap, _) = cspace_spawn_tcb(
        cs_u1a1,
        u1_a1,
        vs_cap,
        1,
        1 % online_cpus,
        bound_tcb_worker,
        pack_arg,
        true,
        1,
    )
    .unwrap();
    while !BOUND_TCB_READY.load(Ordering::Acquire) {
        if online_cpus == 1 {
            let _ = crate::sched::idle_dispatch_once(0);
        } else {
            core::hint::spin_loop();
        }
    }
    cspace_signal_tcb(cs_u1a1, u1_a1, tcb_cap, 1).unwrap();
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);
    assert!(BOUND_TCB_VERIFIED.load(Ordering::Acquire));
    cspace_destroy_object(cs_u1a1, u1_a1, tcb_cap, 1).unwrap();

    cspace_destroy_object(cs_u1a1, u1_a1, vs_cap, 1).unwrap();
    for cs in [cs_u1a1, cs_u1a2, cs_u2a1, cs_root, cs_dev, cs_kern] {
        destroy_cspace(cs).unwrap();
    }

    flush_all_magazines_quiescent(online_cpus);
    let after = measure_fragmentation(online_cpus);
    let leaked = before
        .free_global_frames
        .saturating_sub(after.free_global_frames);
    assert_eq!(leaked, 0, "Frame leak in positive CSpace suite: {}", leaked);
    assert_eq!(after.contig_2m_blocks, before.contig_2m_blocks);

    serial_println!(
        "[DRY-CSPACE] PASS: cpus={} cspaces_ok=true radix_ready_capptr=true objects_bound_ok=true transfer_ipc_ok=true revoke_unmap_ok=true sec_domains_ok=true ro_export_ok=true vault_lease_ok=true leaked_frames={}",
        online_cpus,
        leaked
    );
}

fn smp_cspace_stress_worker(worker_idx: u64) {
    let pair = ((worker_idx as usize) / 2) % STRESS_PAIRS;
    let is_revoker = (worker_idx & 1) == 0;
    let src_cs = STRESS_SRC_CS[pair].load(Ordering::Acquire) as CSpaceId;
    let dst_cs = STRESS_DST_CS[pair].load(Ordering::Acquire) as CSpaceId;
    let vs_cap = CapPtr(STRESS_VS_CAP[pair].load(Ordering::Acquire));
    let src_lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(5, (pair as u8) + 1),
        1,
    );
    let dst_lbl = SecLabel::new(
        SecDomain::Userspace,
        SecFlags::NONE,
        encode_compartment(6, (pair as u8) + 1),
        1,
    );
    let my_vaddr = 0x00a0_0000 + (worker_idx * PAGE_SIZE_4K);

    if is_revoker {
        let (root_fr, _) =
            cspace_alloc_frame(src_cs, src_lbl, src_lbl, PageOrder::Order4KiB, false, 0).unwrap();
        for r in 0..STRESS_ROUNDS {
            let child = cspace_derive(
                src_cs,
                src_lbl,
                root_fr,
                CapRights(CapRights::READ.0 | CapRights::WRITE.0 | CapRights::MAP.0 | CapRights::GRANT.0),
                0,
                0,
                1,
            )
            .unwrap();
            STRESS_DERIVES.fetch_add(1, Ordering::Relaxed);
            STRESS_SHARED_CHILD[pair].store(child.0, Ordering::Release);

            let rcpt = cspace_transfer(
                src_cs,
                src_lbl,
                child,
                dst_cs,
                CapRights(CapRights::READ.0 | CapRights::MAP.0),
                0,
                0x100 + (r as u64),
                1,
            )
            .unwrap();
            STRESS_TRANSFERS.fetch_add(1, Ordering::Relaxed);

            cspace_vspace_map_4k(src_cs, src_lbl, vs_cap, child, my_vaddr, VmProt::USER_RW, 1)
                .unwrap();
            STRESS_MAPS.fetch_add(1, Ordering::Relaxed);

            let exp = 0x77cc_0000_0000_0000u64 ^ ((worker_idx + 1) << 32) ^ (r as u64);
            // SAFETY: `my_vaddr` is private to `worker_idx` in active `vs_id` (`USER_RW`).
            unsafe {
                write_volatile(my_vaddr as *mut u64, exp);
                if read_volatile(my_vaddr as *const u64) != exp {
                    STRESS_UAF_ERRORS.fetch_add(1, Ordering::Relaxed);
                }
            }
            yield_now();

            let rev_cnt = cspace_revoke_children(src_cs, src_lbl, root_fr, 1).unwrap();
            assert!(rev_cnt >= 2);
            STRESS_REVOKES.fetch_add(1, Ordering::Relaxed);

            // Both `child` and cross-`CSpace` `rcpt.dst_cap` must now fail closed!
            if cspace_lookup_fast(src_cs, src_lbl, child, CapRights::READ, 1).is_err() {
                STRESS_STALE_REJECTED.fetch_add(1, Ordering::Relaxed);
            } else {
                STRESS_UAF_ERRORS.fetch_add(1, Ordering::Relaxed);
            }
            if cspace_lookup_fast(dst_cs, dst_lbl, rcpt.dst_cap, CapRights::READ, 1).is_err() {
                STRESS_STALE_REJECTED.fetch_add(1, Ordering::Relaxed);
            } else {
                STRESS_UAF_ERRORS.fetch_add(1, Ordering::Relaxed);
            }
            note_current_thread_progress();
        }
        cspace_destroy_object(src_cs, src_lbl, root_fr, 1).unwrap();
    } else {
        for _ in 0..(STRESS_ROUNDS * 8) {
            let cand = CapPtr(STRESS_SHARED_CHILD[pair].load(Ordering::Acquire));
            STRESS_LOOKUPS.fetch_add(1, Ordering::Relaxed);
            if cand != CapPtr::NULL {
                match cspace_lookup_fast(src_cs, src_lbl, cand, CapRights::READ, 1) {
                    Ok(res) => {
                        if res.obj_kind != CapObjectKind::Frame4K || res.obj_label != src_lbl {
                            STRESS_UAF_ERRORS.fetch_add(1, Ordering::Relaxed);
                        }
                        if let Ok(grand) =
                            cspace_derive(src_cs, src_lbl, cand, CapRights::READ, 0, 0, 1)
                        {
                            STRESS_DERIVES.fetch_add(1, Ordering::Relaxed);
                            let _ = cspace_delete_cap(src_cs, src_lbl, grand, 1);
                        }
                    }
                    Err(CapError::Revoked | CapError::StaleGenerationAba | CapError::InvalidHandle) => {
                        STRESS_STALE_REJECTED.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => {
                        STRESS_UAF_ERRORS.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            if (STRESS_LOOKUPS.load(Ordering::Relaxed) & 7) == 0 {
                yield_now();
            }
            note_current_thread_progress();
        }
    }
    exit_current_thread()
}

fn run_smp_cspace_revoke_stress(online_cpus: usize) {
    flush_all_magazines_quiescent(online_cpus);
    let before = measure_fragmentation(online_cpus);

    let mut src_cs = [0u16; STRESS_PAIRS];
    let mut dst_cs = [0u16; STRESS_PAIRS];
    let mut vs_caps = [CapPtr::NULL; STRESS_PAIRS];
    let mut vs_handles = [VSpaceHandle { id: 0, generation: 0 }; STRESS_PAIRS];

    for p in 0..STRESS_PAIRS {
        let s_lbl = SecLabel::new(
            SecDomain::Userspace,
            SecFlags::NONE,
            encode_compartment(5, (p as u8) + 1),
            1,
        );
        let d_lbl = SecLabel::new(
            SecDomain::Userspace,
            SecFlags::NONE,
            encode_compartment(6, (p as u8) + 1),
            1,
        );
        let (scs, _) = create_cspace(s_lbl, 0x90 + (p as u8)).unwrap();
        let (dcs, _) = create_cspace(d_lbl, 0xa0 + (p as u8)).unwrap();
        let (vcap, vh) = cspace_vspace_create(scs, s_lbl, 0).unwrap();
        src_cs[p] = scs;
        dst_cs[p] = dcs;
        vs_caps[p] = vcap;
        vs_handles[p] = vh;
        STRESS_SRC_CS[p].store(scs as u64, Ordering::Release);
        STRESS_DST_CS[p].store(dcs as u64, Ordering::Release);
        STRESS_VS_CAP[p].store(vcap.0, Ordering::Release);
        STRESS_VS_ID[p].store(vh.id as u64, Ordering::Release);
        STRESS_SHARED_CHILD[p].store(0, Ordering::Release);
    }

    STRESS_LOOKUPS.store(0, Ordering::Release);
    STRESS_DERIVES.store(0, Ordering::Release);
    STRESS_TRANSFERS.store(0, Ordering::Release);
    STRESS_MAPS.store(0, Ordering::Release);
    STRESS_REVOKES.store(0, Ordering::Release);
    STRESS_STALE_REJECTED.store(0, Ordering::Release);
    STRESS_UAF_ERRORS.store(0, Ordering::Release);

    let workers = (online_cpus * 2).clamp(8, 24);
    set_preemptive_mode_active(true);
    init_scheduler(online_cpus);
    begin_preemptive_epoch(0xf6, workers);
    for w in 0..workers {
        let pair = (w / 2) % STRESS_PAIRS;
        let vs_id = vs_handles[pair].id;
        assert!(
            try_spawn_thread_in_vspace(
                1,
                w % online_cpus,
                vs_id,
                smp_cspace_stress_worker,
                w as u64,
                true
            )
            .is_some()
        );
    }
    drain_preemptive_epoch_and_audit(online_cpus);
    set_preemptive_mode_active(false);

    for p in 0..STRESS_PAIRS {
        let s_lbl = SecLabel::new(
            SecDomain::Userspace,
            SecFlags::NONE,
            encode_compartment(5, (p as u8) + 1),
            1,
        );
        cspace_destroy_object(src_cs[p], s_lbl, vs_caps[p], 1).unwrap();
        destroy_cspace(src_cs[p]).unwrap();
        destroy_cspace(dst_cs[p]).unwrap();
    }

    flush_all_magazines_quiescent(online_cpus);
    let after = measure_fragmentation(online_cpus);
    let leaked = before
        .free_global_frames
        .saturating_sub(after.free_global_frames);
    let uaf = STRESS_UAF_ERRORS.load(Ordering::Acquire);
    assert_eq!(uaf, 0, "CSpace SMP UAF/corruption detected");
    assert_eq!(leaked, 0, "CSpace SMP frame leak detected: {}", leaked);

    serial_println!(
        "[DRY-CSPACE-SMP] PASS: cpus={} workers={} lookups={} derives={} transfers={} maps={} revokes={} stale_rejected={} uaf_errors={} leaked_frames={}",
        online_cpus,
        workers,
        STRESS_LOOKUPS.load(Ordering::Acquire),
        STRESS_DERIVES.load(Ordering::Acquire),
        STRESS_TRANSFERS.load(Ordering::Acquire),
        STRESS_MAPS.load(Ordering::Acquire),
        STRESS_REVOKES.load(Ordering::Acquire),
        STRESS_STALE_REJECTED.load(Ordering::Acquire),
        uaf,
        leaked
    );
}

pub fn verify_cspace_and_security(online_cpus: usize, tsc_mhz: u64, cmdline: &str) {
    let fault = CSpaceFaultTest::from_cmdline(cmdline);
    if fault != CSpaceFaultTest::None {
        run_cspace_negative_fault_test(fault, online_cpus);
    }

    verify_positive_cspace_objects_and_adr007(online_cpus);
    measure_cspace_benchmarks(online_cpus, tsc_mhz);
    run_smp_cspace_revoke_stress(online_cpus);
}
