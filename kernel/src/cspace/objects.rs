// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-Gated Kernel Object Bindings (`Frame4K`, `Frame2M`, `READ_ONLY_EXPORT`, `VSpace`,
//! `Untyped`, and `TCB` — `Milestone 1B-C` Stages A.7, B.4–B.8 & C.1–C.8).

use core::ptr::copy_nonoverlapping;
use core::sync::atomic::Ordering;

use crate::cspace::cap::{
    next_slot_gen, pack_obj_meta, pack_state_gen, unpack_state_gen, with_cdt_lock, CSpaceId,
    CapError, CapObjectKind, CapPtr, SlotState, CDT_NONE, MAP_REC_NONE,
};
use crate::cspace::revoke::{
    attach_cap_mapping_record, detach_matching_cap_mapping_record, revoke_subtree_by_global_slot,
};
use crate::cspace::{
    cspace_lookup_fast, cspace_owner_label, global_slot_id, insert_root_cap, slot_by_global,
    with_pinned_cap, CSPACE_TABLE,
};
use crate::pmm::meta::{frame_meta, pack_seclabel};
use crate::pmm::untyped::{
    carve_untyped, untyped_retype, untyped_revoke, untyped_split, RetypeObjKind, UntypedCapToken,
};
use crate::pmm::{alloc_pages, free_pages, AllocFlags, FrameHandle, PageOrder, PAGE_SIZE};
use crate::sched::context::KernelThreadFn;
use crate::sched::state::{ExecState, ThreadId};
use crate::sched::{
    inspect_tcb_live, notify_thread, try_spawn_thread_bound, ONLINE_SCHED_CPUS,
};
use crate::security::{
    pack_seclabel_u64, CapRights, CapabilityMeta, SecDomain, SecError, SecFlags, SecLabel,
};
use crate::vspace::huge::map_2m_cap_authorized;
use crate::vspace::pte::VmProt;
use crate::vspace::{
    create_vspace, destroy_vspace, map_4k_cap_authorized, protect_4k, unmap_4k, VSpaceHandle,
};

/// Allocate a physical `Frame4K` or `Frame2M` bound to `obj_label` and install its root capability.
pub fn cspace_alloc_frame(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    obj_label: SecLabel,
    order: PageOrder,
    executable_code_root: bool,
    lease_epoch: u32,
) -> Result<(CapPtr, FrameHandle), CapError> {
    let owner = cspace_owner_label(cspace_id)?;
    if subj_label.domain != owner.domain || subj_label.compartment_id != owner.compartment_id {
        return Err(CapError::DomainViolation);
    }
    if subj_label.domain != SecDomain::Kernel
        && (obj_label.domain != subj_label.domain
            || obj_label.compartment_id != subj_label.compartment_id)
    {
        return Err(CapError::DomainViolation);
    }
    if obj_label.flags.contains(SecFlags::READ_ONLY_EXPORT) && executable_code_root {
        // `READ_ONLY_EXPORT` frames must be sealed via `cspace_create_readonly_export_frame`.
        return Err(CapError::RightsAmplification);
    }

    let fh = alloc_pages(order, obj_label, AllocFlags::ZERO).map_err(|_| CapError::OutOfSlots)?;
    let kind = match order {
        PageOrder::Order4KiB => CapObjectKind::Frame4K,
        PageOrder::Order2MiB => CapObjectKind::Frame2M,
    };
    let rights = if executable_code_root || obj_label.flags.contains(SecFlags::READ_ONLY_EXPORT) {
        CapRights::FRAME_CODE_ROOT
    } else {
        CapRights::FRAME_DATA_ROOT
    };

    match insert_root_cap(
        cspace_id,
        kind,
        fh.pfn,
        fh.generation,
        rights,
        obj_label,
        lease_epoch,
        0,
    ) {
        Ok(cap) => Ok((cap, fh)),
        Err(e) => {
            free_pages(fh, order, obj_label);
            Err(e)
        }
    }
}

/// Create a controlled `RootSystem` `READ_ONLY_EXPORT` shared library page (`Stage C.8`),
/// initializing its read-only payload once in Ring 0 before sealing away all `WRITE` rights.
pub fn cspace_create_readonly_export_frame(
    root_cspace: CSpaceId,
    root_subj: SecLabel,
    init_payload: &[u8],
) -> Result<(CapPtr, FrameHandle), CapError> {
    if root_subj.domain != SecDomain::RootSystem
        || root_subj.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        return Err(CapError::DomainViolation);
    }
    if init_payload.len() > PAGE_SIZE {
        return Err(CapError::RightsAmplification);
    }

    let export_label = SecLabel::new(
        SecDomain::RootSystem,
        SecFlags::READ_ONLY_EXPORT,
        root_subj.compartment_id,
        root_subj.policy_tag,
    );
    let fh =
        alloc_pages(PageOrder::Order4KiB, export_label, AllocFlags::ZERO).map_err(|_| CapError::OutOfSlots)?;
    if !init_payload.is_empty() {
        // SAFETY: `fh` is freshly allocated, unmapped, and `init_payload.len() <= PAGE_SIZE`.
        unsafe {
            copy_nonoverlapping(init_payload.as_ptr(), fh.hhdm_ptr(), init_payload.len());
        }
    }
    let m = frame_meta(fh.pfn).unwrap();
    m.word1.store(pack_seclabel(export_label), Ordering::Release);

    match insert_root_cap(
        root_cspace,
        CapObjectKind::Frame4K,
        fh.pfn,
        fh.generation,
        CapRights::FRAME_CODE_ROOT,
        export_label,
        0,
        0,
    ) {
        Ok(cap) => Ok((cap, fh)),
        Err(e) => {
            free_pages(fh, PageOrder::Order4KiB, export_label);
            Err(e)
        }
    }
}

/// Seal a `RootSystem` `READ_ONLY_EXPORT` 4 KiB frame directly from a validated user slice (`0` large stack buffers).
pub fn cspace_create_readonly_export_from_user(
    root_cspace: CSpaceId,
    root_subj: SecLabel,
    user_ptr: u64,
    byte_len: usize,
) -> Result<CapPtr, crate::uaccess::SysError> {
    use crate::ipc::map_cap_err_to_sys;
    use crate::uaccess::{copy_from_user, SysError};

    if root_subj.domain != SecDomain::RootSystem
        || root_subj.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        return Err(SysError::Perm);
    }
    if byte_len > PAGE_SIZE {
        return Err(SysError::Inval);
    }
    let export_label = SecLabel::new(
        SecDomain::RootSystem,
        SecFlags::READ_ONLY_EXPORT,
        root_subj.compartment_id,
        root_subj.policy_tag,
    );
    let fh = alloc_pages(PageOrder::Order4KiB, export_label, AllocFlags::ZERO)
        .map_err(|_| SysError::NoMem)?;
    if byte_len > 0 {
        // SAFETY: `fh` is freshly allocated, zeroed, unmapped, and `byte_len <= PAGE_SIZE`.
        let dst = unsafe { core::slice::from_raw_parts_mut(fh.hhdm_ptr(), byte_len) };
        if let Err(e) = copy_from_user(dst, user_ptr) {
            free_pages(fh, PageOrder::Order4KiB, export_label);
            return Err(e);
        }
    }
    let m = frame_meta(fh.pfn).unwrap();
    m.word1.store(pack_seclabel(export_label), Ordering::Release);

    match insert_root_cap(
        root_cspace,
        CapObjectKind::Frame4K,
        fh.pfn,
        fh.generation,
        CapRights::FRAME_CODE_ROOT,
        export_label,
        0,
        0,
    ) {
        Ok(cap) => Ok(cap),
        Err(e) => {
            free_pages(fh, PageOrder::Order4KiB, export_label);
            Err(map_cap_err_to_sys(e))
        }
    }
}

/// Create an isolated user `VSpace` and bind its root capability into `cspace_id` (`Stage A.7`).
pub fn cspace_vspace_create(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    lease_epoch: u32,
) -> Result<(CapPtr, VSpaceHandle), CapError> {
    if subj_label.domain == SecDomain::Kernel {
        return Err(CapError::DomainViolation);
    }
    let owner = cspace_owner_label(cspace_id)?;
    if owner.domain != subj_label.domain || owner.compartment_id != subj_label.compartment_id {
        return Err(CapError::DomainViolation);
    }
    let vs = create_vspace(subj_label).map_err(|_| CapError::OutOfSlots)?;
    match insert_root_cap(
        cspace_id,
        CapObjectKind::VSpace,
        vs.id as u32,
        vs.generation,
        CapRights::VSPACE_ROOT,
        subj_label,
        lease_epoch,
        0,
    ) {
        Ok(cap) => Ok((cap, vs)),
        Err(e) => {
            let _ = destroy_vspace(vs);
            Err(e)
        }
    }
}

fn required_frame_rights_for_prot(prot: VmProt) -> Result<CapRights, CapError> {
    if !prot.contains(VmProt::USER) {
        return Err(CapError::RightsAmplification);
    }
    let writable = (prot.0 & VmProt::WRITE.0) != 0;
    let executable = (prot.0 & VmProt::EXEC.0) != 0;
    if writable && executable {
        return Err(CapError::RightsAmplification);
    }
    let mut r = CapRights::READ.0 | CapRights::MAP.0;
    if writable {
        r |= CapRights::WRITE.0;
    }
    if executable {
        r |= CapRights::EXEC.0;
    }
    Ok(CapRights(r))
}

/// Map a `Frame4K` capability into a `VSpace` capability at `vaddr`, recording a `CapMapRecord`
/// so revoking any ancestor capability deterministically unmaps + TLB-shoots-down `vaddr` (`INV-CAP-05`).
pub fn cspace_vspace_map_4k(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    vspace_cap: CapPtr,
    frame_cap: CapPtr,
    vaddr: u64,
    prot: VmProt,
    now_epoch: u32,
) -> Result<(), CapError> {
    let frame_req = required_frame_rights_for_prot(prot)?;
    let vs_req = CapRights(CapRights::MAP.0 | CapRights::WRITE.0);

    with_pinned_cap(cspace_id, subj_label, vspace_cap, vs_req, now_epoch, |vs_res| {
        if vs_res.obj_kind != CapObjectKind::VSpace {
            return Err(CapError::WrongObjectKind);
        }
        with_pinned_cap(cspace_id, subj_label, frame_cap, frame_req, now_epoch, |fr_res| {
            if fr_res.obj_kind != CapObjectKind::Frame4K {
                return Err(CapError::WrongObjectKind);
            }
            if fr_res.obj_label.domain == SecDomain::Kernel {
                return Err(CapError::DomainViolation);
            }
            let vs_h = VSpaceHandle {
                id: vs_res.obj_id as u16,
                generation: vs_res.expected_obj_gen,
            };
            let fr_h = FrameHandle {
                pfn: fr_res.obj_id,
                generation: fr_res.expected_obj_gen,
            };
            map_4k_cap_authorized(vs_h, vaddr, fr_h, prot).map_err(|_| CapError::PagingError)?;
            if let Err(e) = attach_cap_mapping_record(
                fr_res.global_slot,
                frame_cap.slot_gen(),
                vs_h.id,
                vs_h.generation,
                vaddr,
                false,
            ) {
                let _ = unmap_4k(vs_h, vaddr);
                return Err(e);
            }
            Ok(())
        })
    })
}

/// Map a `Frame2M` capability into a `VSpace` capability at 2 MiB-aligned `vaddr`.
pub fn cspace_vspace_map_2m(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    vspace_cap: CapPtr,
    frame2m_cap: CapPtr,
    vaddr: u64,
    prot: VmProt,
    now_epoch: u32,
) -> Result<(), CapError> {
    let frame_req = required_frame_rights_for_prot(prot)?;
    let vs_req = CapRights(CapRights::MAP.0 | CapRights::WRITE.0);

    with_pinned_cap(cspace_id, subj_label, vspace_cap, vs_req, now_epoch, |vs_res| {
        if vs_res.obj_kind != CapObjectKind::VSpace {
            return Err(CapError::WrongObjectKind);
        }
        with_pinned_cap(cspace_id, subj_label, frame2m_cap, frame_req, now_epoch, |fr_res| {
            if fr_res.obj_kind != CapObjectKind::Frame2M {
                return Err(CapError::WrongObjectKind);
            }
            if fr_res.obj_label.domain == SecDomain::Kernel {
                return Err(CapError::DomainViolation);
            }
            let vs_h = VSpaceHandle {
                id: vs_res.obj_id as u16,
                generation: vs_res.expected_obj_gen,
            };
            let fr_h = FrameHandle {
                pfn: fr_res.obj_id,
                generation: fr_res.expected_obj_gen,
            };
            map_2m_cap_authorized(vs_h, vaddr, fr_h, prot).map_err(|_| CapError::PagingError)?;
            if let Err(e) = attach_cap_mapping_record(
                fr_res.global_slot,
                frame2m_cap.slot_gen(),
                vs_h.id,
                vs_h.generation,
                vaddr,
                true,
            ) {
                let _ = crate::vspace::huge::unmap_2m(vs_h, vaddr);
                return Err(e);
            }
            Ok(())
        })
    })
}

/// Unmap a capability-backed 4 KiB mapping at `vaddr` and synchronize remote TLBs.
pub fn cspace_vspace_unmap_4k(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    vspace_cap: CapPtr,
    frame_cap: CapPtr,
    vaddr: u64,
    now_epoch: u32,
) -> Result<(), CapError> {
    let vs_req = CapRights(CapRights::MAP.0 | CapRights::WRITE.0);
    with_pinned_cap(cspace_id, subj_label, vspace_cap, vs_req, now_epoch, |vs_res| {
        if vs_res.obj_kind != CapObjectKind::VSpace {
            return Err(CapError::WrongObjectKind);
        }
        with_pinned_cap(cspace_id, subj_label, frame_cap, CapRights::MAP, now_epoch, |fr_res| {
            let vs_h = VSpaceHandle {
                id: vs_res.obj_id as u16,
                generation: vs_res.expected_obj_gen,
            };
            if !detach_matching_cap_mapping_record(fr_res.global_slot, vs_h.id, vaddr) {
                return Err(CapError::PagingError);
            }
            let _ = unmap_4k(vs_h, vaddr).map_err(|_| CapError::PagingError)?;
            Ok(())
        })
    })
}

/// Unmap a capability-backed 2 MiB hugepage mapping at `vaddr` and synchronize remote TLBs.
pub fn cspace_vspace_unmap_2m(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    vspace_cap: CapPtr,
    frame2m_cap: CapPtr,
    vaddr: u64,
    now_epoch: u32,
) -> Result<(), CapError> {
    let vs_req = CapRights(CapRights::MAP.0 | CapRights::WRITE.0);
    with_pinned_cap(cspace_id, subj_label, vspace_cap, vs_req, now_epoch, |vs_res| {
        if vs_res.obj_kind != CapObjectKind::VSpace {
            return Err(CapError::WrongObjectKind);
        }
        with_pinned_cap(cspace_id, subj_label, frame2m_cap, CapRights::MAP, now_epoch, |fr_res| {
            if fr_res.obj_kind != CapObjectKind::Frame2M {
                return Err(CapError::WrongObjectKind);
            }
            let vs_h = VSpaceHandle {
                id: vs_res.obj_id as u16,
                generation: vs_res.expected_obj_gen,
            };
            if !detach_matching_cap_mapping_record(fr_res.global_slot, vs_h.id, vaddr) {
                return Err(CapError::PagingError);
            }
            let _ = crate::vspace::huge::unmap_2m(vs_h, vaddr).map_err(|_| CapError::PagingError)?;
            Ok(())
        })
    })
}

/// Update 4 KiB PTE permissions, re-validating both `vspace_cap` and `frame_cap` rights (`W^X` + no RO->RW escalation).
pub fn cspace_vspace_protect_4k(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    vspace_cap: CapPtr,
    frame_cap: CapPtr,
    vaddr: u64,
    new_prot: VmProt,
    now_epoch: u32,
) -> Result<(), CapError> {
    let frame_req = required_frame_rights_for_prot(new_prot)?;
    let vs_req = CapRights(CapRights::MAP.0 | CapRights::WRITE.0);
    with_pinned_cap(cspace_id, subj_label, vspace_cap, vs_req, now_epoch, |vs_res| {
        if vs_res.obj_kind != CapObjectKind::VSpace {
            return Err(CapError::WrongObjectKind);
        }
        with_pinned_cap(cspace_id, subj_label, frame_cap, frame_req, now_epoch, |_fr_res| {
            let vs_h = VSpaceHandle {
                id: vs_res.obj_id as u16,
                generation: vs_res.expected_obj_gen,
            };
            protect_4k(vs_h, vaddr, new_prot).map_err(|_| CapError::PagingError)
        })
    })
}

#[allow(clippy::too_many_arguments)]
fn insert_cdt_child_object_cap(
    cspace_id: CSpaceId,
    parent_global_slot: u32,
    parent_slot_gen: u32,
    kind: CapObjectKind,
    obj_id: u32,
    expected_obj_gen: u32,
    rights: CapRights,
    obj_label: SecLabel,
    lease_epoch: u32,
) -> Result<CapPtr, CapError> {
    let cnode = &CSPACE_TABLE[cspace_id as usize];
    let mut claimed = None;
    for idx in 1..crate::cspace::CNODE_SLOTS {
        let slot = &cnode.slots[idx];
        let old = slot.state_gen.load(Ordering::Acquire);
        let (st, _, _, sg) = unpack_state_gen(old);
        if (st == SlotState::Empty || st == SlotState::EmptyRevoked)
            && slot.in_flight_ops.load(Ordering::Acquire) == 0
        {
            let claim_gen = sg.max(1);
            let b = pack_state_gen(
                SlotState::Building,
                CapObjectKind::None,
                CapRights::NONE,
                claim_gen,
            );
            if slot
                .state_gen
                .compare_exchange(old, b, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                cnode.live_slots.fetch_add(1, Ordering::Relaxed);
                claimed = Some((idx as u16, claim_gen));
                break;
            }
        }
    }
    let (dst_idx, dst_sgen) = claimed.ok_or(CapError::OutOfSlots)?;
    let child_global = global_slot_id(cspace_id, dst_idx);
    let child_slot = &cnode.slots[dst_idx as usize];
    let parent_slot = slot_by_global(parent_global_slot);

    child_slot
        .obj_meta
        .store(pack_obj_meta(obj_id, expected_obj_gen), Ordering::Relaxed);
    child_slot
        .sec_label_raw
        .store(pack_seclabel_u64(obj_label), Ordering::Relaxed);
    child_slot.badge.store(0, Ordering::Relaxed);
    child_slot.lease_epoch.store(lease_epoch, Ordering::Relaxed);
    child_slot.first_map_idx.store(MAP_REC_NONE, Ordering::Relaxed);
    child_slot.origin_cspace.store(cspace_id, Ordering::Relaxed);
    child_slot.cdt_first_child.store(CDT_NONE, Ordering::Relaxed);

    let ok = with_cdt_lock(|_| {
        let (pst, _, _, pgen) = unpack_state_gen(parent_slot.state_gen.load(Ordering::Acquire));
        if pst != SlotState::Live || pgen != parent_slot_gen {
            return false;
        }
        let old_first = parent_slot.cdt_first_child.load(Ordering::Relaxed);
        child_slot
            .cdt_parent
            .store(parent_global_slot, Ordering::Relaxed);
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
            pack_state_gen(SlotState::Live, kind, rights, dst_sgen),
            Ordering::Release,
        );
        true
    });

    if !ok {
        cnode.live_slots.fetch_sub(1, Ordering::Relaxed);
        let bumped = next_slot_gen(dst_sgen);
        child_slot.state_gen.store(
            pack_state_gen(SlotState::Empty, CapObjectKind::None, CapRights::NONE, bumped),
            Ordering::Release,
        );
        return Err(CapError::Revoked);
    }

    Ok(CapPtr::pack(
        cnode.guard.load(Ordering::Relaxed),
        cnode.radix_bits.load(Ordering::Relaxed),
        dst_sgen,
        dst_idx,
    ))
}

/// Carve a contiguous `Untyped` memory capability and bind it into `cspace_id` (`Stage A.7`).
pub fn cspace_untyped_carve(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    frame_count: usize,
    lease_epoch: u32,
) -> Result<CapPtr, CapError> {
    let owner = cspace_owner_label(cspace_id)?;
    if owner.domain != subj_label.domain || owner.compartment_id != subj_label.compartment_id {
        return Err(CapError::DomainViolation);
    }
    let tok = carve_untyped(frame_count, subj_label, lease_epoch).map_err(|_| CapError::OutOfSlots)?;
    insert_root_cap(
        cspace_id,
        CapObjectKind::Untyped,
        tok.slot_idx as u32,
        tok.meta.expected_gen,
        CapRights::UNTYPED_ROOT,
        subj_label,
        lease_epoch,
        0,
    )
}

/// Split an `Untyped` capability into two CDT-linked child `Untyped` capabilities inheriting `SecLabel`.
pub fn cspace_untyped_split(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    parent_ut_cap: CapPtr,
    now_epoch: u32,
) -> Result<(CapPtr, CapPtr), CapError> {
    with_pinned_cap(
        cspace_id,
        subj_label,
        parent_ut_cap,
        CapRights::RETYPE,
        now_epoch,
        |ut_res| {
            if ut_res.obj_kind != CapObjectKind::Untyped {
                return Err(CapError::WrongObjectKind);
            }
            let tok = UntypedCapToken {
                slot_idx: ut_res.obj_id as u16,
                meta: CapabilityMeta {
                    rights: ut_res.rights,
                    expected_gen: ut_res.expected_obj_gen,
                    lease_epoch: ut_res.lease_epoch,
                    badge: ut_res.badge,
                },
            };
            let (c0, c1) = untyped_split(subj_label, tok, now_epoch)
                .map_err(|_| CapError::RightsAmplification)?;
            let child_lbl = ut_res.obj_label.inherit_child();
            let p0 = insert_cdt_child_object_cap(
                cspace_id,
                ut_res.global_slot,
                parent_ut_cap.slot_gen(),
                CapObjectKind::Untyped,
                c0.slot_idx as u32,
                c0.meta.expected_gen,
                CapRights::UNTYPED_ROOT,
                child_lbl,
                ut_res.lease_epoch,
            )?;
            let p1 = insert_cdt_child_object_cap(
                cspace_id,
                ut_res.global_slot,
                parent_ut_cap.slot_gen(),
                CapObjectKind::Untyped,
                c1.slot_idx as u32,
                c1.meta.expected_gen,
                CapRights::UNTYPED_ROOT,
                child_lbl,
                ut_res.lease_epoch,
            )?;
            Ok((p0, p1))
        },
    )
}

/// Retype one `4 KiB` physical frame from `ut_cap`, inheriting `ut_cap`'s `SecLabel` (`Stage C.5` —
/// zero label-rewrite escalation!) and linking the resulting `Frame4K` cap under `ut_cap` in the CDT.
pub fn cspace_untyped_retype_frame4k(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    ut_cap: CapPtr,
    now_epoch: u32,
) -> Result<(CapPtr, FrameHandle), CapError> {
    with_pinned_cap(cspace_id, subj_label, ut_cap, CapRights::RETYPE, now_epoch, |ut_res| {
        if ut_res.obj_kind != CapObjectKind::Untyped {
            return Err(CapError::WrongObjectKind);
        }
        let tok = UntypedCapToken {
            slot_idx: ut_res.obj_id as u16,
            meta: CapabilityMeta {
                rights: ut_res.rights,
                expected_gen: ut_res.expected_obj_gen,
                lease_epoch: ut_res.lease_epoch,
                badge: ut_res.badge,
            },
        };
        let slice = untyped_retype(subj_label, tok, RetypeObjKind::Frame4KiB, 1, now_epoch)
            .map_err(|e| match e {
                SecError::BusyPinned => CapError::BusyActiveObject,
                SecError::Revoked => CapError::Revoked,
                _ => CapError::InsufficientRights,
            })?;
        let fh = FrameHandle {
            pfn: slice.backing_pfn,
            generation: slice.backing_pfn_gen,
        };
        let inherited = ut_res.obj_label.inherit_child();
        let cap = insert_cdt_child_object_cap(
            cspace_id,
            ut_res.global_slot,
            ut_cap.slot_gen(),
            CapObjectKind::Frame4K,
            fh.pfn,
            fh.generation,
            CapRights::FRAME_DATA_ROOT,
            inherited,
            ut_res.lease_epoch,
        )?;
        Ok((cap, fh))
    })
}

/// Revoke an `Untyped` capability's entire CDT subtree (auto-unmapping + TLB shooting down any
/// `VSpace` mappings of retyped frames) and scrub/reset the `UntypedRegion` (`INV-CAP-05`).
pub fn cspace_untyped_revoke_tree(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    ut_cap: CapPtr,
    now_epoch: u32,
) -> Result<usize, CapError> {
    let req = CapRights(CapRights::RETYPE.0 | CapRights::REVOKE.0);
    with_pinned_cap(cspace_id, subj_label, ut_cap, req, now_epoch, |ut_res| {
        if ut_res.obj_kind != CapObjectKind::Untyped {
            return Err(CapError::WrongObjectKind);
        }
        let cdt_revoked = revoke_subtree_by_global_slot(ut_res.global_slot);
        let tok = UntypedCapToken {
            slot_idx: ut_res.obj_id as u16,
            meta: CapabilityMeta {
                rights: ut_res.rights,
                expected_gen: ut_res.expected_obj_gen,
                lease_epoch: ut_res.lease_epoch,
                badge: ut_res.badge,
            },
        };
        match untyped_revoke(subj_label, tok, now_epoch) {
            Ok(ut_items) => Ok(cdt_revoked + ut_items),
            Err(SecError::BusyPinned) => Err(CapError::BusyActiveObject),
            Err(SecError::Revoked) => Err(CapError::Revoked),
            Err(_) => Err(CapError::InsufficientRights),
        }
    })
}

/// Spawn a worker `TCB` bound to `(vspace_cap, cspace_id, subj_label)` and install a `Tcb` capability (`Stage A.7`).
#[allow(clippy::too_many_arguments)]
pub fn cspace_spawn_tcb(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    vspace_cap: CapPtr,
    prio: usize,
    target_cpu: usize,
    entry: KernelThreadFn,
    arg: u64,
    preemptible: bool,
    now_epoch: u32,
) -> Result<(CapPtr, ThreadId), CapError> {
    let vs_res = cspace_lookup_fast(cspace_id, subj_label, vspace_cap, CapRights::READ, now_epoch)?;
    if vs_res.obj_kind != CapObjectKind::VSpace {
        return Err(CapError::WrongObjectKind);
    }

    // Reserve a `CSpace` slot *before* publishing the runnable `TCB` so we never leak a live thread on `OutOfSlots`.
    let tid = try_spawn_thread_bound(
        prio,
        target_cpu,
        vs_res.obj_id as u16,
        cspace_id,
        subj_label,
        entry,
        arg,
        preemptible,
    )
    .ok_or(CapError::OutOfSlots)?;

    let cap = insert_root_cap(
        cspace_id,
        CapObjectKind::Tcb,
        tid.slot() as u32,
        tid.gen26(),
        CapRights::TCB_ROOT,
        subj_label,
        vs_res.lease_epoch,
        0,
    )?;
    Ok((cap, tid))
}

/// Deliver an IPC-ready notification signal to a `Tcb` / `Notification` capability (`CapRights::SIGNAL`).
pub fn cspace_signal_tcb(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    tcb_cap: CapPtr,
    now_epoch: u32,
) -> Result<u64, CapError> {
    with_pinned_cap(cspace_id, subj_label, tcb_cap, CapRights::SIGNAL, now_epoch, |res| {
        if res.obj_kind != CapObjectKind::Tcb && res.obj_kind != CapObjectKind::Notification {
            return Err(CapError::WrongObjectKind);
        }
        let slot = res.obj_id as usize;
        let Some((state, gen26, _)) = inspect_tcb_live(slot) else {
            return Err(CapError::InvalidHandle);
        };
        if state == ExecState::Dead || gen26 != res.expected_obj_gen {
            return Err(CapError::StaleGenerationAba);
        }
        let tid = ThreadId::new(slot, gen26);
        let online = ONLINE_SCHED_CPUS.load(Ordering::Acquire).max(1);
        let bits = if res.badge != 0 { res.badge } else { 1 };
        let _ = notify_thread(tid, bits, slot % online);
        Ok(res.badge)
    })
}
