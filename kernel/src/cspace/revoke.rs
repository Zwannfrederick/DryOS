// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Intrusive $O(k)$ Transitive Capability Revocation, Automatic `VSpace` Unmap + Synchronous SMP
//! TLB Shootdown Drain, and Active-Use Safe Object Destruction (`Milestone 1B-C` Stage B).

use core::sync::atomic::Ordering;

use crate::arch::read_rflags;
use crate::cspace::cap::{
    next_slot_gen, pack_state_gen, unpack_obj_meta, unpack_state_gen, with_cdt_lock, CSpaceId,
    CapError, CapMapRecord, CapObjectKind, CapPtr, SlotState, CDT_NONE, MAP_REC_NONE,
    MAX_CAP_MAPPINGS,
};
use crate::cspace::{
    cspace_invariant_panic, cspace_lookup_fast, global_slot_id, slot_by_global, split_global_slot,
    with_pinned_cap, CSPACE_TABLE, MAX_CSPACES,
};
use crate::percpu::read_gs_u64_at_8;
use crate::pmm::untyped::{release_untyped_to_pmm, untyped_revoke, UntypedCapToken};
use crate::pmm::{free_pages, inspect_frame_live, FrameHandle, PageOrder};
use crate::sched::inspect_tcb_live;
use crate::sched::state::ExecState;
use crate::security::{CapRights, CapabilityMeta, SecError, SecLabel};
use crate::vspace::huge::unmap_2m;
use crate::vspace::tlb::poll_and_ack_pending_shootdown;
use crate::vspace::{destroy_vspace, inspect_vspace_live, unmap_4k, VSpaceHandle};

/// Attach a `VSpace` mapping record to `owner_global_slot` iff the slot is still `SlotState::Live`.
pub(crate) fn attach_cap_mapping_record(
    owner_global_slot: u32,
    expected_slot_gen: u32,
    vspace_id: u16,
    vspace_gen: u32,
    vaddr: u64,
    is_huge_2m: bool,
) -> Result<(), CapError> {
    attach_cap_mapping_range_record(
        owner_global_slot,
        expected_slot_gen,
        vspace_id,
        vspace_gen,
        vaddr,
        is_huge_2m,
        1,
    )
}

/// Attach a contiguous `num_pages` `VSpace` mapping record (e.g., MMIO framebuffer BAR) to `owner_global_slot`.
pub(crate) fn attach_cap_mapping_range_record(
    owner_global_slot: u32,
    expected_slot_gen: u32,
    vspace_id: u16,
    vspace_gen: u32,
    vaddr: u64,
    is_huge_2m: bool,
    num_pages: u16,
) -> Result<(), CapError> {
    let slot = slot_by_global(owner_global_slot);
    with_cdt_lock(|maps| {
        let (st, _, _, sgen) = unpack_state_gen(slot.state_gen.load(Ordering::Acquire));
        if st != SlotState::Live || sgen != expected_slot_gen {
            return Err(CapError::Revoked);
        }
        let mut free_idx = None;
        for (i, rec) in maps.iter().enumerate() {
            if !rec.active {
                free_idx = Some(i as u16);
                break;
            }
        }
        let idx = free_idx.ok_or(CapError::OutOfSlots)?;
        let old_head = slot.first_map_idx.load(Ordering::Relaxed);
        maps[idx as usize] = CapMapRecord {
            active: true,
            is_huge_2m,
            vspace_id,
            vspace_gen,
            vaddr,
            next_idx: old_head,
            num_pages: num_pages.max(1),
        };
        slot.first_map_idx.store(idx, Ordering::Release);
        Ok(())
    })
}

/// Remove one matching `(vspace_id, vaddr)` mapping record from `owner_global_slot`.
pub(crate) fn detach_matching_cap_mapping_record(
    owner_global_slot: u32,
    vspace_id: u16,
    vaddr: u64,
) -> bool {
    let slot = slot_by_global(owner_global_slot);
    with_cdt_lock(|maps| {
        let mut prev = MAP_REC_NONE;
        let mut cur = slot.first_map_idx.load(Ordering::Relaxed);
        while cur != MAP_REC_NONE {
            let c = cur as usize;
            let next = maps[c].next_idx;
            if maps[c].active && maps[c].vspace_id == vspace_id && maps[c].vaddr == vaddr {
                maps[c].active = false;
                maps[c].next_idx = MAP_REC_NONE;
                if prev == MAP_REC_NONE {
                    slot.first_map_idx.store(next, Ordering::Release);
                } else {
                    maps[prev as usize].next_idx = next;
                }
                return true;
            }
            prev = cur;
            cur = next;
        }
        false
    })
}

fn pop_one_mapping_from_slot(global_slot: u32) -> Option<CapMapRecord> {
    let slot = slot_by_global(global_slot);
    with_cdt_lock(|maps| {
        let head = slot.first_map_idx.load(Ordering::Relaxed);
        if head == MAP_REC_NONE {
            return None;
        }
        let rec = maps[head as usize];
        maps[head as usize].active = false;
        maps[head as usize].next_idx = MAP_REC_NONE;
        slot.first_map_idx.store(rec.next_idx, Ordering::Release);
        if rec.active {
            Some(rec)
        } else {
            None
        }
    })
}

pub(crate) fn unmap_all_records_for_slot(global_slot: u32) -> usize {
    let mut unmapped = 0usize;
    let mut guard = 0usize;
    while let Some(rec) = pop_one_mapping_from_slot(global_slot) {
        let vs = VSpaceHandle {
            id: rec.vspace_id,
            generation: rec.vspace_gen,
        };
        if rec.is_huge_2m {
            if unmap_2m(vs, rec.vaddr).is_ok() {
                unmapped += 1;
            }
        } else {
            let pages = rec.num_pages.max(1) as u64;
            for p in 0..pages {
                if unmap_4k(vs, rec.vaddr + p * 4096).is_ok() {
                    unmapped += 1;
                }
            }
        }
        guard += 1;
        assert!(guard <= MAX_CAP_MAPPINGS, "Corrupt CapMapRecord cycle");
    }
    unmapped
}

#[inline]
fn wait_slot_in_flight_zero(global_slot: u32) {
    let slot = slot_by_global(global_slot);
    let if_masked = (read_rflags() & (1 << 9)) == 0;
    let cpu_idx = read_gs_u64_at_8() as usize;
    while slot.in_flight_ops.load(Ordering::SeqCst) != 0 {
        if if_masked {
            poll_and_ack_pending_shootdown(cpu_idx);
        }
        core::hint::spin_loop();
    }
}

fn unlink_node_from_parent_locked(global_slot: u32) {
    let slot = slot_by_global(global_slot);
    let parent = slot.cdt_parent.load(Ordering::Relaxed);
    let prev = slot.cdt_prev_sib.load(Ordering::Relaxed);
    let next = slot.cdt_next_sib.load(Ordering::Relaxed);

    if prev != CDT_NONE {
        slot_by_global(prev)
            .cdt_next_sib
            .store(next, Ordering::Relaxed);
    } else if parent != CDT_NONE {
        let p_slot = slot_by_global(parent);
        if p_slot.cdt_first_child.load(Ordering::Relaxed) == global_slot {
            p_slot.cdt_first_child.store(next, Ordering::Relaxed);
        }
    }
    if next != CDT_NONE {
        slot_by_global(next)
            .cdt_prev_sib
            .store(prev, Ordering::Relaxed);
    }

    slot.cdt_parent.store(CDT_NONE, Ordering::Relaxed);
    slot.cdt_prev_sib.store(CDT_NONE, Ordering::Relaxed);
    slot.cdt_next_sib.store(CDT_NONE, Ordering::Relaxed);
}

/// Revoke 100% of transitive derived child capabilities under `parent_ptr` in $O(k)$ live children:
/// `Live -> Revoking` -> drain `in_flight_ops` -> `VSpace` unmap + SMP TLB shootdown -> `EmptyRevoked`.
pub fn cspace_revoke_children(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    parent_ptr: CapPtr,
    now_epoch: u32,
) -> Result<usize, CapError> {
    with_pinned_cap(
        cspace_id,
        subj_label,
        parent_ptr,
        CapRights::REVOKE,
        now_epoch,
        |parent| Ok(revoke_subtree_by_global_slot(parent.global_slot)),
    )
}

pub(crate) fn revoke_subtree_by_global_slot(root_global_slot: u32) -> usize {
    // Phase 1 (`with_cdt_lock`): Collect all transitive descendants into an intrusive worklist via
    // `cdt_next_sib`, unlink them from the CDT, and transition each `Live@g -> Revoking@(g+1)` (`SeqCst`).
    let q_head = with_cdt_lock(|_| {
        let root = slot_by_global(root_global_slot);
        let first = root.cdt_first_child.load(Ordering::Relaxed);
        if first == CDT_NONE {
            return CDT_NONE;
        }
        root.cdt_first_child.store(CDT_NONE, Ordering::Relaxed);

        let mut tail = first;
        loop {
            let t_slot = slot_by_global(tail);
            t_slot.cdt_parent.store(CDT_NONE, Ordering::Relaxed);
            t_slot.cdt_prev_sib.store(CDT_NONE, Ordering::Relaxed);
            let nxt = t_slot.cdt_next_sib.load(Ordering::Relaxed);
            if nxt == CDT_NONE {
                break;
            }
            tail = nxt;
        }

        let mut cur = first;
        while cur != CDT_NONE {
            let c_slot = slot_by_global(cur);
            let sub_first = c_slot.cdt_first_child.load(Ordering::Relaxed);
            if sub_first != CDT_NONE {
                c_slot.cdt_first_child.store(CDT_NONE, Ordering::Relaxed);
                slot_by_global(tail)
                    .cdt_next_sib
                    .store(sub_first, Ordering::Relaxed);
                let mut sub_tail = sub_first;
                loop {
                    let st_slot = slot_by_global(sub_tail);
                    st_slot.cdt_parent.store(CDT_NONE, Ordering::Relaxed);
                    st_slot.cdt_prev_sib.store(CDT_NONE, Ordering::Relaxed);
                    let snxt = st_slot.cdt_next_sib.load(Ordering::Relaxed);
                    if snxt == CDT_NONE {
                        break;
                    }
                    sub_tail = snxt;
                }
                tail = sub_tail;
            }

            let (_, kind, _, old_gen) = unpack_state_gen(c_slot.state_gen.load(Ordering::Acquire));
            let bumped = next_slot_gen(old_gen);
            c_slot.state_gen.store(
                pack_state_gen(SlotState::Revoking, kind, CapRights::NONE, bumped),
                Ordering::SeqCst,
            );
            cur = c_slot.cdt_next_sib.load(Ordering::Relaxed);
        }
        first
    });

    if q_head == CDT_NONE {
        return 0;
    }

    // Phase 2 & 3 (outside `CDT_LOCK`): Wait for `in_flight_ops == 0`, wake any blocked IPC
    // threads using the revoked capability, then unmap + SMP TLB shootdown.
    let mut cur = q_head;
    while cur != CDT_NONE {
        wait_slot_in_flight_zero(cur);
        let c_slot = slot_by_global(cur);
        let (_, kind, _, _) = unpack_state_gen(c_slot.state_gen.load(Ordering::Acquire));
        let (obj_id, obj_gen) = unpack_obj_meta(c_slot.obj_meta.load(Ordering::Acquire));
        crate::ipc::on_cap_slot_revoked(kind, obj_id, obj_gen, cur);
        let _ = unmap_all_records_for_slot(cur);
        cur = c_slot.cdt_next_sib.load(Ordering::Relaxed);
    }

    // Phase 4: Finalize all revoked slots to `SlotState::EmptyRevoked` (preserving bumped `slot_gen`).
    let mut revoked_count = 0usize;
    let mut fin = q_head;
    while fin != CDT_NONE {
        let slot = slot_by_global(fin);
        let nxt = slot.cdt_next_sib.load(Ordering::Relaxed);
        slot.cdt_next_sib.store(CDT_NONE, Ordering::Relaxed);
        let (_, _, _, bumped_gen) = unpack_state_gen(slot.state_gen.load(Ordering::Acquire));
        let (cs, _) = split_global_slot(fin);
        CSPACE_TABLE[cs as usize]
            .live_slots
            .fetch_sub(1, Ordering::Relaxed);
        slot.state_gen.store(
            pack_state_gen(
                SlotState::EmptyRevoked,
                CapObjectKind::None,
                CapRights::NONE,
                bumped_gen,
            ),
            Ordering::Release,
        );
        revoked_count += 1;
        fin = nxt;
    }

    revoked_count
}

/// Delete a single leaf capability slot (`Empty`, bumping `slot_gen` to prevent ABA reuse).
pub fn cspace_delete_cap(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    ptr: CapPtr,
    now_epoch: u32,
) -> Result<(), CapError> {
    let resolved = cspace_lookup_fast(cspace_id, subj_label, ptr, CapRights::READ, now_epoch)?;
    let slot = slot_by_global(resolved.global_slot);

    let bumped = with_cdt_lock(|_| {
        let (st, kind, _, sgen) = unpack_state_gen(slot.state_gen.load(Ordering::Acquire));
        if st != SlotState::Live || sgen != ptr.slot_gen() {
            return Err(CapError::Revoked);
        }
        if slot.cdt_first_child.load(Ordering::Relaxed) != CDT_NONE {
            return Err(CapError::BusyActiveObject);
        }
        unlink_node_from_parent_locked(resolved.global_slot);
        let b = next_slot_gen(sgen);
        slot.state_gen.store(
            pack_state_gen(SlotState::Revoking, kind, CapRights::NONE, b),
            Ordering::SeqCst,
        );
        Ok(b)
    })?;

    wait_slot_in_flight_zero(resolved.global_slot);
    crate::ipc::on_cap_slot_revoked(
        resolved.obj_kind,
        resolved.obj_id,
        resolved.expected_obj_gen,
        resolved.global_slot,
    );
    let _ = unmap_all_records_for_slot(resolved.global_slot);
    CSPACE_TABLE[cspace_id as usize]
        .live_slots
        .fetch_sub(1, Ordering::Relaxed);
    slot.state_gen.store(
        pack_state_gen(SlotState::Empty, CapObjectKind::None, CapRights::NONE, bumped),
        Ordering::Release,
    );
    Ok(())
}

/// Destroy a kernel object managed by `manage_ptr`, first revoking all derived capabilities and
/// strictly refusing premature reclamation while any CPU, `VSpace`, or IOMMU pin is active (`INV-CAP-05, 08`).
pub fn cspace_destroy_object(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    manage_ptr: CapPtr,
    now_epoch: u32,
) -> Result<(), CapError> {
    let req = CapRights(CapRights::MANAGE.0 | CapRights::REVOKE.0);
    let resolved = cspace_lookup_fast(cspace_id, subj_label, manage_ptr, req, now_epoch)?;

    // Revoke all derived child capabilities (and auto-unmap any mappings made via children).
    let _ = revoke_subtree_by_global_slot(resolved.global_slot);

    // Enforce active-use / pin / map quiescence BEFORE invalidating `manage_ptr` or freeing memory!
    match resolved.obj_kind {
        CapObjectKind::Frame4K | CapObjectKind::Frame2M => {
            let Some((_, pins, maps, gen, _)) = inspect_frame_live(resolved.obj_id) else {
                return Err(CapError::StaleGenerationAba);
            };
            if gen != resolved.expected_obj_gen {
                return Err(CapError::StaleGenerationAba);
            }
            if pins > 0 || maps > 0 {
                return Err(CapError::BusyActiveObject);
            }
        }
        CapObjectKind::VSpace => {
            let Some((gen, active_cpus, _)) = inspect_vspace_live(resolved.obj_id as u16) else {
                return Err(CapError::StaleGenerationAba);
            };
            if gen != resolved.expected_obj_gen {
                return Err(CapError::StaleGenerationAba);
            }
            if active_cpus != 0 {
                return Err(CapError::BusyActiveObject);
            }
        }
        CapObjectKind::Tcb => {
            let Some((state, gen26, stack_active)) = inspect_tcb_live(resolved.obj_id as usize) else {
                return Err(CapError::InvalidHandle);
            };
            if gen26 != resolved.expected_obj_gen {
                return Err(CapError::StaleGenerationAba);
            }
            if state != ExecState::Dead || stack_active {
                return Err(CapError::BusyActiveObject);
            }
        }
        CapObjectKind::Notification => {
            if !crate::ipc::notif::is_notif_live(resolved.obj_id, resolved.expected_obj_gen) {
                return Err(CapError::StaleGenerationAba);
            }
        }
        CapObjectKind::Endpoint => {
            if !crate::ipc::endpoint::is_endpoint_live(resolved.obj_id, resolved.expected_obj_gen) {
                return Err(CapError::StaleGenerationAba);
            }
        }
        CapObjectKind::BootModule => {
            if resolved.expected_obj_gen != 1
                || crate::limine::get_boot_module_by_index(resolved.obj_id as usize).is_none()
            {
                return Err(CapError::StaleGenerationAba);
            }
        }
        CapObjectKind::Framebuffer => {
            if !crate::display::is_framebuffer_cap_live(resolved.obj_id, resolved.expected_obj_gen) {
                return Err(CapError::StaleGenerationAba);
            }
        }
        CapObjectKind::IrqLine => {
            if !crate::irq::is_irq_line_live(resolved.obj_id, resolved.expected_obj_gen) {
                return Err(CapError::StaleGenerationAba);
            }
        }
        CapObjectKind::IoPort => {
            if !crate::irq::ioport::is_ioport_cap_live(resolved.obj_id, resolved.expected_obj_gen) {
                return Err(CapError::StaleGenerationAba);
            }
        }
        CapObjectKind::Untyped => {
            let tok = UntypedCapToken {
                slot_idx: resolved.obj_id as u16,
                meta: CapabilityMeta {
                    rights: resolved.rights,
                    expected_gen: resolved.expected_obj_gen,
                    lease_epoch: resolved.lease_epoch,
                    badge: resolved.badge,
                },
            };
            match untyped_revoke(subj_label, tok, now_epoch) {
                Ok(_) => {}
                Err(SecError::BusyPinned) => return Err(CapError::BusyActiveObject),
                Err(SecError::Revoked) => return Err(CapError::Revoked),
                Err(_) => return Err(CapError::InsufficientRights),
            }
        }
        CapObjectKind::CNode => {
            if (resolved.obj_id as u16) == cspace_id {
                return Err(CapError::BusyActiveObject);
            }
        }
        CapObjectKind::None => return Err(CapError::InvalidHandle),
    }

    // Atomically transition `manage_ptr` itself `Live -> Revoking -> EmptyRevoked` so concurrent
    // double-destroy attempts deterministically observe `Err(CapError::Revoked)`!
    let slot = slot_by_global(resolved.global_slot);
    let bumped = with_cdt_lock(|_| {
        let (st, kind, _, sgen) = unpack_state_gen(slot.state_gen.load(Ordering::Acquire));
        if st != SlotState::Live || sgen != manage_ptr.slot_gen() {
            return Err(CapError::Revoked);
        }
        unlink_node_from_parent_locked(resolved.global_slot);
        let b = next_slot_gen(sgen);
        slot.state_gen.store(
            pack_state_gen(SlotState::Revoking, kind, CapRights::NONE, b),
            Ordering::SeqCst,
        );
        Ok(b)
    })?;

    wait_slot_in_flight_zero(resolved.global_slot);
    crate::ipc::on_cap_slot_revoked(
        resolved.obj_kind,
        resolved.obj_id,
        resolved.expected_obj_gen,
        resolved.global_slot,
    );
    let _ = unmap_all_records_for_slot(resolved.global_slot);

    // Reclaim the underlying physical / kernel resource now that all capability references are dead.
    match resolved.obj_kind {
        CapObjectKind::Frame4K => {
            free_pages(
                FrameHandle {
                    pfn: resolved.obj_id,
                    generation: resolved.expected_obj_gen,
                },
                PageOrder::Order4KiB,
                resolved.obj_label,
            );
        }
        CapObjectKind::Frame2M => {
            free_pages(
                FrameHandle {
                    pfn: resolved.obj_id,
                    generation: resolved.expected_obj_gen,
                },
                PageOrder::Order2MiB,
                resolved.obj_label,
            );
        }
        CapObjectKind::VSpace => {
            destroy_vspace(VSpaceHandle {
                id: resolved.obj_id as u16,
                generation: resolved.expected_obj_gen,
            })
            .map_err(|_| CapError::BusyActiveObject)?;
        }
        CapObjectKind::Untyped => {
            let tok = UntypedCapToken {
                slot_idx: resolved.obj_id as u16,
                meta: CapabilityMeta {
                    rights: resolved.rights,
                    expected_gen: resolved.expected_obj_gen,
                    lease_epoch: resolved.lease_epoch,
                    badge: resolved.badge,
                },
            };
            release_untyped_to_pmm(subj_label, tok, now_epoch)
                .map_err(|_| CapError::BusyActiveObject)?;
        }
        CapObjectKind::CNode => {
            if (resolved.obj_id as usize) < MAX_CSPACES {
                let _ = crate::cspace::destroy_cspace(resolved.obj_id as u16);
            }
        }
        CapObjectKind::Tcb => {
            if !crate::sched::release_dead_tcb_slot(
                resolved.obj_id as usize,
                resolved.expected_obj_gen,
            ) {
                return Err(CapError::BusyActiveObject);
            }
        }
        CapObjectKind::Endpoint => {
            crate::ipc::endpoint::destroy_endpoint(resolved.obj_id, resolved.expected_obj_gen);
        }
        CapObjectKind::Notification => {
            crate::ipc::notif::destroy_notif(resolved.obj_id, resolved.expected_obj_gen);
        }
        CapObjectKind::IrqLine => {
            crate::irq::destroy_irq_line(resolved.obj_id, resolved.expected_obj_gen);
        }
        CapObjectKind::IoPort => {
            crate::irq::ioport::destroy_ioport_cap(resolved.obj_id, resolved.expected_obj_gen);
        }
        CapObjectKind::BootModule | CapObjectKind::Framebuffer | CapObjectKind::None => {}
    }

    CSPACE_TABLE[cspace_id as usize]
        .live_slots
        .fetch_sub(1, Ordering::Relaxed);
    slot.state_gen.store(
        pack_state_gen(
            SlotState::EmptyRevoked,
            CapObjectKind::None,
            CapRights::NONE,
            bumped,
        ),
        Ordering::Release,
    );
    Ok(())
}

pub(crate) fn force_cleanup_slot_on_cspace_teardown(cspace_id: CSpaceId, slot_idx: u16) -> bool {
    let gslot = global_slot_id(cspace_id, slot_idx);
    let slot = slot_by_global(gslot);
    let (st, kind, _, sgen) = unpack_state_gen(slot.state_gen.load(Ordering::Acquire));
    if st != SlotState::Live {
        return false;
    }
    let _ = revoke_subtree_by_global_slot(gslot);
    let bumped = with_cdt_lock(|_| {
        let (st2, _, _, sg2) = unpack_state_gen(slot.state_gen.load(Ordering::Acquire));
        if st2 != SlotState::Live || sg2 != sgen {
            return None;
        }
        unlink_node_from_parent_locked(gslot);
        let b = next_slot_gen(sg2);
        slot.state_gen.store(
            pack_state_gen(SlotState::Revoking, kind, CapRights::NONE, b),
            Ordering::SeqCst,
        );
        Some(b)
    });
    let Some(b) = bumped else {
        return false;
    };
    wait_slot_in_flight_zero(gslot);
    let (obj_id, obj_gen) = unpack_obj_meta(slot.obj_meta.load(Ordering::Acquire));
    crate::ipc::on_cap_slot_revoked(kind, obj_id, obj_gen, gslot);
    let _ = unmap_all_records_for_slot(gslot);
    slot.state_gen.store(
        pack_state_gen(SlotState::Empty, CapObjectKind::None, CapRights::NONE, b),
        Ordering::Release,
    );
    true
}

/// Checked enforcement helper translating `CapError` into deterministic `[DRY-CSPACE-FAIL]` panics.
pub fn unwrap_cap_or_panic<T>(res: Result<T, CapError>, context: &str) -> T {
    match res {
        Ok(val) => val,
        Err(CapError::InvalidHandle) => {
            cspace_invariant_panic(format_args!("Forged capability handle rejected ({})", context))
        }
        Err(CapError::StaleGenerationAba) => cspace_invariant_panic(format_args!(
            "Stale generation / ABA capability access rejected ({})",
            context
        )),
        Err(CapError::RightsAmplification | CapError::InsufficientRights) => {
            cspace_invariant_panic(format_args!("Rights amplification rejected ({})", context))
        }
        Err(CapError::DomainViolation) => cspace_invariant_panic(format_args!(
            "Cross-domain unauthorized access rejected ({})",
            context
        )),
        Err(CapError::TransferEscalation) => cspace_invariant_panic(format_args!(
            "Capability transfer privilege escalation rejected ({})",
            context
        )),
        Err(CapError::BusyActiveObject) => cspace_invariant_panic(format_args!(
            "Premature reclamation of active TCB/Frame blocked ({})",
            context
        )),
        Err(CapError::Revoked | CapError::LeaseExpired) => {
            cspace_invariant_panic(format_args!("{} (err={:?})", context, res.err()))
        }
        Err(other) => {
            cspace_invariant_panic(format_args!("CSpace invariant failure {:?} ({})", other, context))
        }
    }
}
