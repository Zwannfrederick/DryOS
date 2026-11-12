// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Bounded Guarded `CSpace` Infrastructure, Lock-Free $O(1)$ Fast-Path Lookup, Monotonic
//! `derive / copy / mint`, and IPC-Ready `transfer` (`Milestone 1B-C` Stages A, B, C).

pub mod bench;
pub mod cap;
pub mod fault_tests;
pub mod objects;
pub mod revoke;
pub mod tests;

pub use cap::{
    CSpaceId, CapError, CapObjectKind, CapPtr, IpcCapTransferReceipt, ResolvedCap,
    CNODE_RADIX_BITS, CNODE_SLOTS, KERNEL_RESERVED_CSPACE_ID, MAX_CSPACES,
};

use core::mem::size_of;
use core::sync::atomic::{fence, AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};

use crate::cspace::cap::{
    next_slot_gen, pack_obj_meta, pack_state_gen, unpack_obj_meta, unpack_state_gen, with_cdt_lock,
    CapSlot, SlotState, CDT_NONE, MAP_REC_NONE,
};
use crate::pmm::untyped::inspect_untyped_live_gen;
use crate::pmm::{inspect_frame_live, FrameState, HUGE_2M_FRAMES};
use crate::sched::inspect_tcb_live;
use crate::security::{
    authorize_cspace_fast, pack_seclabel_u64, unpack_seclabel_u64, CapRights, SecDomain, SecError,
    SecFlags, SecLabel,
};
use crate::vspace::inspect_vspace_live;

#[repr(C, align(64))]
pub struct CNode {
    pub allocated: AtomicBool,
    pub guard: AtomicU8,
    pub radix_bits: AtomicU8,
    pub generation: AtomicU32,
    pub owner_label_raw: AtomicU64,
    pub live_slots: AtomicU32,
    pub slots: [CapSlot; CNODE_SLOTS],
}

impl CNode {
    const fn new() -> Self {
        Self {
            allocated: AtomicBool::new(false),
            guard: AtomicU8::new(0),
            radix_bits: AtomicU8::new(CNODE_RADIX_BITS),
            generation: AtomicU32::new(1),
            owner_label_raw: AtomicU64::new(0),
            live_slots: AtomicU32::new(0),
            slots: [const { CapSlot::new_empty() }; CNODE_SLOTS],
        }
    }
}

pub(crate) static CSPACE_TABLE: [CNode; MAX_CSPACES] = [const { CNode::new() }; MAX_CSPACES];

#[derive(Clone, Copy, Debug)]
pub struct CSpaceMetadataStats {
    pub bytes_per_slot: usize,
    pub slots_per_cnode: usize,
    pub total_pool_bytes: usize,
}

pub fn cspace_metadata_stats() -> CSpaceMetadataStats {
    CSpaceMetadataStats {
        bytes_per_slot: size_of::<CapSlot>(),
        slots_per_cnode: CNODE_SLOTS,
        total_pool_bytes: size_of::<[CNode; MAX_CSPACES]>(),
    }
}

pub fn cspace_invariant_panic(args: core::fmt::Arguments<'_>) -> ! {
    crate::enter_fatal_or_halt();
    crate::serial::emergency_print(format_args!("\r\n[DRY-CSPACE-FAIL] {}\r\n", args));
    panic!("CSpace / capability security invariant violation");
}

#[inline(always)]
pub(crate) fn global_slot_id(cspace_id: CSpaceId, slot_idx: u16) -> u32 {
    ((cspace_id as u32) * (CNODE_SLOTS as u32)) + (slot_idx as u32)
}

#[inline(always)]
pub(crate) fn split_global_slot(global_slot: u32) -> (CSpaceId, u16) {
    (
        (global_slot / (CNODE_SLOTS as u32)) as CSpaceId,
        (global_slot % (CNODE_SLOTS as u32)) as u16,
    )
}

#[inline(always)]
pub(crate) fn slot_by_global(global_slot: u32) -> &'static CapSlot {
    let (cs, idx) = split_global_slot(global_slot);
    &CSPACE_TABLE[cs as usize].slots[idx as usize]
}

#[inline(always)]
pub fn cspace_owner_label(cspace_id: CSpaceId) -> Result<SecLabel, CapError> {
    if (cspace_id as usize) >= MAX_CSPACES {
        return Err(CapError::InvalidHandle);
    }
    let cnode = &CSPACE_TABLE[cspace_id as usize];
    if !cnode.allocated.load(Ordering::Acquire) {
        return Err(CapError::InvalidHandle);
    }
    Ok(unpack_seclabel_u64(cnode.owner_label_raw.load(Ordering::Acquire)))
}

#[cold]
#[inline(never)]
fn verify_live_object_cold(kind: CapObjectKind, obj_id: u32, expected_gen: u32) -> bool {
    match kind {
        CapObjectKind::None => false,
        CapObjectKind::Untyped => {
            inspect_untyped_live_gen(obj_id as u16) == expected_gen && expected_gen != 0
        }
        CapObjectKind::Frame2M => {
            (obj_id as usize) & (HUGE_2M_FRAMES - 1) == 0
                && matches!(
                    inspect_frame_live(obj_id),
                    Some((FrameState::AllocatedFrame, _, _, gen, _)) if gen == expected_gen
                )
        }
        CapObjectKind::BootModule => {
            expected_gen == 1 && crate::limine::get_boot_module_by_index(obj_id as usize).is_some()
        }
        CapObjectKind::Framebuffer => crate::display::is_framebuffer_cap_live(obj_id, expected_gen),
        CapObjectKind::IrqLine => crate::irq::is_irq_line_live(obj_id, expected_gen),
        CapObjectKind::IoPort => crate::irq::ioport::is_ioport_cap_live(obj_id, expected_gen),
        CapObjectKind::PowerCtrl => crate::power::is_power_cap_live(obj_id, expected_gen),
        CapObjectKind::CNode => {
            let idx = obj_id as usize;
            idx < MAX_CSPACES
                && CSPACE_TABLE[idx].allocated.load(Ordering::Acquire)
                && CSPACE_TABLE[idx].generation.load(Ordering::Acquire) == expected_gen
        }
        _ => false,
    }
}

#[inline(always)]
pub(crate) fn verify_live_object_generation(
    kind: CapObjectKind,
    obj_id: u32,
    expected_gen: u32,
) -> bool {
    match kind {
        CapObjectKind::Endpoint => crate::ipc::endpoint::is_endpoint_live(obj_id, expected_gen),
        CapObjectKind::Notification => crate::ipc::notif::is_notif_live(obj_id, expected_gen),
        CapObjectKind::Frame4K => matches!(
            inspect_frame_live(obj_id),
            Some((FrameState::AllocatedFrame | FrameState::UntypedRetypedObj, _, _, gen, _)) if gen == expected_gen
        ),
        CapObjectKind::VSpace => matches!(
            inspect_vspace_live(obj_id as u16),
            Some((gen, _, _)) if gen == expected_gen
        ),
        CapObjectKind::Tcb => matches!(
            inspect_tcb_live(obj_id as usize),
            Some((_, gen26, _)) if gen26 == expected_gen && expected_gen != 0
        ),
        _ => verify_live_object_cold(kind, obj_id, expected_gen),
    }
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn lookup_slot_unchecked_bounds(
    cspace_id: CSpaceId,
    cnode: &CNode,
    slot_idx: usize,
    slot: &CapSlot,
    subj_label: SecLabel,
    ptr: CapPtr,
    req_rights: CapRights,
    now_epoch: u32,
) -> Result<ResolvedCap, CapError> {
    if !cnode.allocated.load(Ordering::Acquire) {
        return Err(CapError::InvalidHandle);
    }
    if ptr.guard() != cnode.guard.load(Ordering::Relaxed)
        || ptr.radix_bits() != cnode.radix_bits.load(Ordering::Relaxed)
    {
        return Err(CapError::InvalidHandle);
    }

    // `SeqCst` pairs with `with_pinned_cap`'s `in_flight_ops.fetch_add(1, SeqCst)` and `revoke`'s
    // `state_gen.store(Revoking, SeqCst)` + `in_flight_ops.load(SeqCst)` Dekker barrier (0 extra
    // instructions on x86-64).
    let w1 = slot.state_gen.load(Ordering::SeqCst);
    let (state, kind, rights, sgen) = unpack_state_gen(w1);
    match state {
        SlotState::Revoking | SlotState::EmptyRevoked => return Err(CapError::Revoked),
        SlotState::Empty | SlotState::Building => {
            if ptr.slot_gen() == 0 || sgen == 1 {
                return Err(CapError::InvalidHandle);
            }
            return Err(CapError::StaleGenerationAba);
        }
        SlotState::Live => {
            if sgen != ptr.slot_gen() {
                return Err(CapError::StaleGenerationAba);
            }
        }
    }

    if req_rights.0 == 0 || !rights.contains(req_rights) {
        return Err(CapError::InsufficientRights);
    }

    let (obj_id, expected_obj_gen) = unpack_obj_meta(slot.obj_meta.load(Ordering::Acquire));
    let obj_label = unpack_seclabel_u64(slot.sec_label_raw.load(Ordering::Acquire));
    let lease_epoch = slot.lease_epoch.load(Ordering::Acquire);
    let badge = slot.badge.load(Ordering::Acquire);

    match authorize_cspace_fast(subj_label, obj_label, rights, req_rights, lease_epoch, now_epoch) {
        Ok(()) => {}
        Err(SecError::Revoked) => return Err(CapError::LeaseExpired),
        Err(SecError::Inval) => return Err(CapError::RightsAmplification),
        Err(SecError::PermCap | SecError::BusyPinned) => return Err(CapError::DomainViolation),
    }

    if !verify_live_object_generation(kind, obj_id, expected_obj_gen) {
        return Err(CapError::StaleGenerationAba);
    }

    // Seqlock validation: ensure slot did not transition (`Live -> Revoking`) mid-read.
    fence(Ordering::Acquire);
    if slot.state_gen.load(Ordering::Acquire) != w1 {
        return Err(CapError::Revoked);
    }

    Ok(ResolvedCap {
        global_slot: global_slot_id(cspace_id, slot_idx as u16),
        obj_kind: kind,
        obj_id,
        expected_obj_gen,
        rights,
        obj_label,
        lease_epoch,
        badge,
    })
}

/// Lock-free $O(1)$ fast-path capability lookup + joint `ADR-007` security validation (`INV-CAP-02`).
/// Zero locks, zero heap allocations, zero table scans, zero atomic RMWs.
#[inline]
pub fn cspace_lookup_fast(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    ptr: CapPtr,
    req_rights: CapRights,
    now_epoch: u32,
) -> Result<ResolvedCap, CapError> {
    if (cspace_id as usize) >= MAX_CSPACES {
        return Err(CapError::InvalidHandle);
    }
    let slot_idx = ptr.slot_idx() as usize;
    if slot_idx == 0 || slot_idx >= CNODE_SLOTS {
        return Err(CapError::InvalidHandle);
    }
    let cnode = &CSPACE_TABLE[cspace_id as usize];
    let slot = &cnode.slots[slot_idx];
    lookup_slot_unchecked_bounds(
        cspace_id, cnode, slot_idx, slot, subj_label, ptr, req_rights, now_epoch,
    )
}

/// Pin `slot.in_flight_ops` across a kernel object operation (`map`, `retype`, `tcb`) so `revoke`
/// and slot reallocation cannot recycle the capability or underlying object while in use (`INV-CAP-08`).
#[inline]
pub fn with_pinned_cap<R>(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    ptr: CapPtr,
    req_rights: CapRights,
    now_epoch: u32,
    f: impl FnOnce(ResolvedCap) -> Result<R, CapError>,
) -> Result<R, CapError> {
    if (cspace_id as usize) >= MAX_CSPACES {
        return Err(CapError::InvalidHandle);
    }
    let slot_idx = ptr.slot_idx() as usize;
    if slot_idx == 0 || slot_idx >= CNODE_SLOTS {
        return Err(CapError::InvalidHandle);
    }
    let cnode = &CSPACE_TABLE[cspace_id as usize];
    let slot = &cnode.slots[slot_idx];
    slot.in_flight_ops.fetch_add(1, Ordering::SeqCst);
    let res = match lookup_slot_unchecked_bounds(
        cspace_id, cnode, slot_idx, slot, subj_label, ptr, req_rights, now_epoch,
    ) {
        Ok(resolved) => f(resolved),
        Err(e) => Err(e),
    };
    slot.in_flight_ops.fetch_sub(1, Ordering::Release);
    res
}

fn claim_empty_slot(cnode: &CNode) -> Result<(u16, u32), CapError> {
    // Slot `0` is permanently `CAP_NULL`.
    for idx in 1..CNODE_SLOTS {
        let slot = &cnode.slots[idx];
        let old = slot.state_gen.load(Ordering::Acquire);
        let (st, _, _, cur_gen) = unpack_state_gen(old);
        if (st == SlotState::Empty || st == SlotState::EmptyRevoked)
            && slot.in_flight_ops.load(Ordering::Acquire) == 0
        {
            let claim_gen = if cur_gen == 0 { 1 } else { cur_gen };
            let building = pack_state_gen(
                SlotState::Building,
                CapObjectKind::None,
                CapRights::NONE,
                claim_gen,
            );
            if slot
                .state_gen
                .compare_exchange(old, building, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                cnode.live_slots.fetch_add(1, Ordering::Relaxed);
                return Ok((idx as u16, claim_gen));
            }
        }
    }
    Err(CapError::OutOfSlots)
}

fn abort_building_slot(cnode: &CNode, slot_idx: u16, sgen: u32) {
    cnode.live_slots.fetch_sub(1, Ordering::Relaxed);
    cnode.slots[slot_idx as usize].state_gen.store(
        pack_state_gen(SlotState::Empty, CapObjectKind::None, CapRights::NONE, sgen),
        Ordering::Release,
    );
}

/// Allocate a bounded guarded `CNode` (`256` slots; slot `0` of `CSPACE_TABLE` reserved as kernel
/// sentinel so valid user `CSpaceId`s are `1..MAX_CSPACES`) and install a self `CNode` root cap.
pub fn create_cspace(owner_label: SecLabel, guard_hint: u8) -> Result<(CSpaceId, CapPtr), CapError> {
    for (idx, cnode) in CSPACE_TABLE.iter().enumerate().skip(1) {
        if cnode
            .allocated
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let guard = if guard_hint == 0 {
                0x50 | (idx as u8)
            } else {
                guard_hint
            };
            let cgen = cnode.generation.load(Ordering::Acquire);
            cnode.guard.store(guard, Ordering::Release);
            cnode.radix_bits.store(CNODE_RADIX_BITS, Ordering::Release);
            cnode
                .owner_label_raw
                .store(pack_seclabel_u64(owner_label), Ordering::Release);
            cnode.live_slots.store(0, Ordering::Release);

            let cspace_id = idx as CSpaceId;
            let self_cap = insert_root_cap(
                cspace_id,
                CapObjectKind::CNode,
                cspace_id as u32,
                cgen,
                CapRights::CNODE_ROOT,
                owner_label,
                0,
                0,
            )?;
            return Ok((cspace_id, self_cap));
        }
    }
    Err(CapError::OutOfSlots)
}

/// Destroy a `CSpace`, revoking/unmapping any residual capabilities and bumping `CNode` generation.
pub fn destroy_cspace(cspace_id: CSpaceId) -> Result<(), CapError> {
    if cspace_id == KERNEL_RESERVED_CSPACE_ID || (cspace_id as usize) >= MAX_CSPACES {
        return Err(CapError::InvalidHandle);
    }
    let cnode = &CSPACE_TABLE[cspace_id as usize];
    if !cnode.allocated.load(Ordering::Acquire) {
        return Err(CapError::Revoked);
    }

    for idx in 1..CNODE_SLOTS {
        let _ = revoke::force_cleanup_slot_on_cspace_teardown(cspace_id, idx as u16);
    }

    let next_gen = next_slot_gen(cnode.generation.load(Ordering::Acquire));
    cnode.generation.store(next_gen, Ordering::Release);
    cnode.live_slots.store(0, Ordering::Release);
    cnode.allocated.store(false, Ordering::Release);
    Ok(())
}

/// Insert a root capability (not derived from an existing `CapSlot`) into `cspace_id`.
#[allow(clippy::too_many_arguments)]
pub fn insert_root_cap(
    cspace_id: CSpaceId,
    kind: CapObjectKind,
    obj_id: u32,
    expected_obj_gen: u32,
    rights: CapRights,
    obj_label: SecLabel,
    lease_epoch: u32,
    badge: u64,
) -> Result<CapPtr, CapError> {
    if (cspace_id as usize) >= MAX_CSPACES {
        return Err(CapError::InvalidHandle);
    }
    let cnode = &CSPACE_TABLE[cspace_id as usize];
    if !cnode.allocated.load(Ordering::Acquire) {
        return Err(CapError::InvalidHandle);
    }
    if rights.contains(CapRights::WRITE) && rights.contains(CapRights::EXEC) {
        return Err(CapError::RightsAmplification);
    }

    let (slot_idx, sgen) = claim_empty_slot(cnode)?;
    let slot = &cnode.slots[slot_idx as usize];
    slot.obj_meta
        .store(pack_obj_meta(obj_id, expected_obj_gen), Ordering::Relaxed);
    slot.sec_label_raw
        .store(pack_seclabel_u64(obj_label), Ordering::Relaxed);
    slot.badge.store(badge, Ordering::Relaxed);
    slot.lease_epoch.store(lease_epoch, Ordering::Relaxed);
    slot.cdt_parent.store(CDT_NONE, Ordering::Relaxed);
    slot.cdt_first_child.store(CDT_NONE, Ordering::Relaxed);
    slot.cdt_prev_sib.store(CDT_NONE, Ordering::Relaxed);
    slot.cdt_next_sib.store(CDT_NONE, Ordering::Relaxed);
    slot.first_map_idx.store(MAP_REC_NONE, Ordering::Relaxed);
    slot.origin_cspace.store(cspace_id, Ordering::Relaxed);

    slot.state_gen.store(
        pack_state_gen(SlotState::Live, kind, rights, sgen),
        Ordering::Release,
    );

    Ok(CapPtr::pack(
        cnode.guard.load(Ordering::Relaxed),
        cnode.radix_bits.load(Ordering::Relaxed),
        sgen,
        slot_idx,
    ))
}

#[allow(clippy::too_many_arguments)]
fn validate_narrowing_and_delegation(
    sender_label: SecLabel,
    dst_owner_label: SecLabel,
    parent: &ResolvedCap,
    new_rights: CapRights,
    req_lease: u32,
    req_badge: u64,
    now_epoch: u32,
    is_transfer_across_cspace: bool,
) -> Result<(u32, u64), CapError> {
    if new_rights.0 == 0 || !parent.rights.contains(new_rights) {
        return Err(CapError::RightsAmplification);
    }
    if new_rights.contains(CapRights::WRITE) && new_rights.contains(CapRights::EXEC) {
        return Err(CapError::RightsAmplification);
    }
    if parent.obj_label.flags.contains(SecFlags::READ_ONLY_EXPORT)
        && (new_rights.0 & (CapRights::WRITE.0 | CapRights::RETYPE.0 | CapRights::MANAGE.0)) != 0
    {
        return Err(CapError::RightsAmplification);
    }

    let eff_lease = match (parent.lease_epoch, req_lease) {
        (0, r) => r,
        (p, 0) => p,
        (p, r) if r <= p => r,
        _ => return Err(CapError::RightsAmplification),
    };

    let eff_badge = if parent.badge != 0 {
        if req_badge != 0 && req_badge != parent.badge {
            return Err(CapError::RightsAmplification);
        }
        parent.badge
    } else {
        req_badge
    };

    let cross_boundary = sender_label.domain != dst_owner_label.domain
        || sender_label.compartment_id != dst_owner_label.compartment_id;

    if (is_transfer_across_cspace || cross_boundary) && cross_boundary {
        if !parent.rights.contains(CapRights::GRANT)
            || parent.obj_label.flags.contains(SecFlags::NO_DELEGATE)
        {
            return Err(CapError::TransferEscalation);
        }
        if parent.obj_label.domain == SecDomain::Kernel {
            return Err(CapError::TransferEscalation);
        }
        if dst_owner_label.domain == SecDomain::Userspace {
            match parent.obj_label.domain {
                SecDomain::Kernel => return Err(CapError::TransferEscalation),
                SecDomain::RootSystem => {
                    let allowed_ro = CapRights::READ.0 | CapRights::EXEC.0 | CapRights::MAP.0;
                    if !parent.obj_label.flags.contains(SecFlags::READ_ONLY_EXPORT)
                        || (new_rights.0 & !allowed_ro) != 0
                    {
                        return Err(CapError::TransferEscalation);
                    }
                }
                SecDomain::Userspace => {}
            }
        }
        // `DRY Vault` / `DEV_WORKSPACE` cross-compartment leasing requires bounded `lease_epoch > 0`.
        if dst_owner_label.flags.contains(SecFlags::DEV_WORKSPACE) && eff_lease == 0 {
            return Err(CapError::TransferEscalation);
        }
    }

    authorize_cspace_fast(
        dst_owner_label,
        parent.obj_label,
        new_rights,
        new_rights,
        eff_lease,
        now_epoch,
    )
    .map_err(|e| match e {
        SecError::Revoked => CapError::LeaseExpired,
        _ => CapError::DomainViolation,
    })?;
    Ok((eff_lease, eff_badge))
}

#[allow(clippy::too_many_arguments)]
fn derive_or_transfer_internal(
    src_cspace: CSpaceId,
    sender_label: SecLabel,
    src_ptr: CapPtr,
    dst_cspace: CSpaceId,
    new_rights: CapRights,
    req_lease: u32,
    req_badge: u64,
    now_epoch: u32,
    is_transfer: bool,
) -> Result<(CapPtr, u64), CapError> {
    let dst_owner = cspace_owner_label(dst_cspace)?;
    let dst_cnode = &CSPACE_TABLE[dst_cspace as usize];

    with_pinned_cap(src_cspace, sender_label, src_ptr, CapRights::READ, now_epoch, |parent| {
            let (eff_lease, eff_badge) = validate_narrowing_and_delegation(
                sender_label,
                dst_owner,
                &parent,
                new_rights,
                req_lease,
                req_badge,
                now_epoch,
                is_transfer,
            )?;

            let (dst_idx, dst_sgen) = claim_empty_slot(dst_cnode)?;
            let child_global = global_slot_id(dst_cspace, dst_idx);
            let child_slot = &dst_cnode.slots[dst_idx as usize];
            let parent_slot = slot_by_global(parent.global_slot);

            child_slot.obj_meta.store(
                pack_obj_meta(parent.obj_id, parent.expected_obj_gen),
                Ordering::Relaxed,
            );
            child_slot
                .sec_label_raw
                .store(pack_seclabel_u64(parent.obj_label), Ordering::Relaxed);
            child_slot.badge.store(eff_badge, Ordering::Relaxed);
            child_slot.lease_epoch.store(eff_lease, Ordering::Relaxed);
            child_slot.first_map_idx.store(MAP_REC_NONE, Ordering::Relaxed);
            child_slot.origin_cspace.store(dst_cspace, Ordering::Relaxed);
            child_slot.cdt_first_child.store(CDT_NONE, Ordering::Relaxed);

            // Serialize CDT child insertion against concurrent `cspace_revoke_children`!
            let linked_ok = with_cdt_lock(|_| {
                let (p_st, _, _, p_gen) = unpack_state_gen(parent_slot.state_gen.load(Ordering::Acquire));
                if p_st != SlotState::Live || p_gen != src_ptr.slot_gen() {
                    return false;
                }
                let old_first = parent_slot.cdt_first_child.load(Ordering::Relaxed);
                child_slot
                    .cdt_parent
                    .store(parent.global_slot, Ordering::Relaxed);
                child_slot.cdt_prev_sib.store(CDT_NONE, Ordering::Relaxed);
                child_slot.cdt_next_sib.store(old_first, Ordering::Relaxed);
                if old_first != CDT_NONE {
                    slot_by_global(old_first)
                        .cdt_prev_sib
                        .store(child_global, Ordering::Relaxed);
                }
                parent_slot
                    .cdt_first_child
                    .store(child_global, Ordering::Relaxed);
                child_slot.state_gen.store(
                    pack_state_gen(SlotState::Live, parent.obj_kind, new_rights, dst_sgen),
                    Ordering::Release,
                );
                true
            });

            if !linked_ok {
                abort_building_slot(dst_cnode, dst_idx, dst_sgen);
                return Err(CapError::Revoked);
            }

            let ptr = CapPtr::pack(
                dst_cnode.guard.load(Ordering::Relaxed),
                dst_cnode.radix_bits.load(Ordering::Relaxed),
                dst_sgen,
                dst_idx,
            );
            Ok((ptr, eff_badge))
        },
    )
}

/// Copy a capability within `cspace_id` preserving identical rights, lease, and badge (`Stage A.4`).
pub fn cspace_copy(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    src_ptr: CapPtr,
    now_epoch: u32,
) -> Result<CapPtr, CapError> {
    let snap = cspace_lookup_fast(cspace_id, subj_label, src_ptr, CapRights::READ, now_epoch)?;
    let (ptr, _) = derive_or_transfer_internal(
        cspace_id,
        subj_label,
        src_ptr,
        cspace_id,
        snap.rights,
        snap.lease_epoch,
        snap.badge,
        now_epoch,
        false,
    )?;
    Ok(ptr)
}

/// Derive / Mint a narrowed capability within `cspace_id` (`Stage A.4`, `INV-CAP-03`).
pub fn cspace_derive(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    src_ptr: CapPtr,
    new_rights: CapRights,
    new_lease_epoch: u32,
    new_badge: u64,
    now_epoch: u32,
) -> Result<CapPtr, CapError> {
    let (ptr, _) = derive_or_transfer_internal(
        cspace_id,
        subj_label,
        src_ptr,
        cspace_id,
        new_rights,
        new_lease_epoch,
        new_badge,
        now_epoch,
        false,
    )?;
    Ok(ptr)
}

/// IPC-Compatible Cross-`CSpace` Capability Transfer (`Stage A.5`, `ADR-002` & `INV-CAP-07`).
#[allow(clippy::too_many_arguments)]
pub fn cspace_transfer(
    src_cspace: CSpaceId,
    sender_label: SecLabel,
    src_ptr: CapPtr,
    dst_cspace: CSpaceId,
    attenuated_rights: CapRights,
    lease_epoch: u32,
    badge: u64,
    now_epoch: u32,
) -> Result<IpcCapTransferReceipt, CapError> {
    let base_req = CapRights::READ;
    let snap = cspace_lookup_fast(src_cspace, sender_label, src_ptr, base_req, now_epoch)?;
    let dst_owner = cspace_owner_label(dst_cspace)?;
    let cross = sender_label.domain != dst_owner.domain
        || sender_label.compartment_id != dst_owner.compartment_id;
    if cross
        && (!snap.rights.contains(CapRights::GRANT)
            || snap.obj_label.flags.contains(SecFlags::NO_DELEGATE))
    {
        return Err(CapError::TransferEscalation);
    }

    let (dst_cap, eff_badge) = derive_or_transfer_internal(
        src_cspace,
        sender_label,
        src_ptr,
        dst_cspace,
        attenuated_rights,
        lease_epoch,
        badge,
        now_epoch,
        true,
    )?;
    Ok(IpcCapTransferReceipt {
        dst_cspace,
        dst_cap,
        badge: eff_badge,
        sender_label,
    })
}
