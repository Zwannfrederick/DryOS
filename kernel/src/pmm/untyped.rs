// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! `Untyped` Memory Capability Lifecycle Contract (`split / retype / revoke` — `ADR-003` & `ADR-007`).
//!
//! Scope boundary (`Milestone 1B-A`):
//! - Implements physical `Untyped` region ownership, parent-to-child `SecLabel` inheritance,
//!   monotonic `watermark_bytes` object carving (`retype`), `BusyPinned` DMA/map revocation gates,
//!   descendant generation invalidation, and mandatory zero scrubbing on `retype`/`revoke`.
//! - Intentionally omits a full `CSpace` radix capability table (scheduled for `Milestone 1B-C`).

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::arch::{irq_restore, irq_save_cli};
use crate::pmm::meta::{
    frame_meta, next_generation, pack_seclabel, pack_word0, phys_to_hhdm, pmm_invariant_panic,
    unpack_word0, zero_frame_4k, FrameState, PmmError, ZeroState, PAGE_SHIFT, PAGE_SIZE,
};
use crate::pmm::{carve_contiguous_for_untyped, release_contiguous_from_untyped};
use crate::security::{
    authorize_object_access, CapRights, CapabilityMeta, KernelObjHeader, SecDomain, SecError,
    SecFlags, SecLabel,
};

pub const MAX_UNTYPED_REGIONS: usize = 64;
const NO_PARENT: u16 = u16::MAX;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetypeObjKind {
    Frame4KiB = 0,
    PageTable4KiB = 1,
    TcbObj = 2,
    EndpointObj = 3,
    NotificationObj = 4,
    CNodeSlotBlock = 5,
}

impl RetypeObjKind {
    #[inline]
    pub const fn layout_bytes(self) -> (usize, usize) {
        match self {
            Self::Frame4KiB | Self::PageTable4KiB | Self::CNodeSlotBlock => (PAGE_SIZE, PAGE_SIZE),
            Self::TcbObj => (1024, 1024),
            Self::EndpointObj | Self::NotificationObj => (64, 64),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UntypedCapToken {
    pub slot_idx: u16,
    pub meta: CapabilityMeta,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetypedObjectSlice {
    pub kind: RetypeObjKind,
    pub base_phys: u64,
    pub hhdm_ptr: *mut u8,
    pub byte_len: usize,
    pub backing_pfn: u32,
    pub backing_pfn_gen: u32,
}

pub struct UntypedRegion {
    pub active: bool,
    pub has_children: bool,
    pub parent_idx: u16,
    pub base_pfn: u32,
    pub frame_count: u32,
    pub watermark_bytes: usize,
    pub header: KernelObjHeader,
}

impl UntypedRegion {
    const fn empty() -> Self {
        Self {
            active: false,
            has_children: false,
            parent_idx: NO_PARENT,
            base_pfn: 0,
            frame_count: 0,
            watermark_bytes: 0,
            header: KernelObjHeader::new(SecLabel::new(
                SecDomain::RootSystem,
                SecFlags::NONE,
                0,
                0,
            )),
        }
    }
}

struct UntypedTable {
    locked: AtomicBool,
    slots: UnsafeCell<[UntypedRegion; MAX_UNTYPED_REGIONS]>,
}

// SAFETY: `slots` is mutated exclusively under `locked` with local interrupts masked (`CLI`).
unsafe impl Sync for UntypedTable {}

#[allow(clippy::declare_interior_mutable_const)]
const EMPTY_UNTYPED: UntypedRegion = UntypedRegion::empty();
static UNTYPED_TABLE: UntypedTable = UntypedTable {
    locked: AtomicBool::new(false),
    slots: UnsafeCell::new([EMPTY_UNTYPED; MAX_UNTYPED_REGIONS]),
};
static UNTYPED_LIVE_GEN: [AtomicU32; MAX_UNTYPED_REGIONS] =
    [const { AtomicU32::new(0) }; MAX_UNTYPED_REGIONS];

/// Lock-free active generation query (`0` = inactive, `>= 1` = active generation) for `CSpace` fast-path.
#[inline(always)]
pub fn inspect_untyped_live_gen(idx: u16) -> u32 {
    if (idx as usize) >= MAX_UNTYPED_REGIONS {
        return 0;
    }
    UNTYPED_LIVE_GEN[idx as usize].load(Ordering::Acquire)
}

#[inline]
fn with_untyped_table<R>(f: impl FnOnce(&mut [UntypedRegion; MAX_UNTYPED_REGIONS]) -> R) -> R {
    let saved_if = irq_save_cli();
    while UNTYPED_TABLE
        .locked
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    // SAFETY: `locked` + `CLI` held exclusively on current CPU.
    let res = f(unsafe { &mut *UNTYPED_TABLE.slots.get() });
    UNTYPED_TABLE.locked.store(false, Ordering::Release);
    irq_restore(saved_if);
    res
}

fn alloc_free_region_slot(
    slots: &mut [UntypedRegion; MAX_UNTYPED_REGIONS],
    label: SecLabel,
    base_pfn: u32,
    frame_count: u32,
    parent_idx: u16,
) -> Option<(u16, u32)> {
    for (idx, slot) in slots.iter_mut().enumerate() {
        if !slot.active {
            let cur_gen = slot.header.generation.load(Ordering::Acquire);
            let next_gen = next_generation(cur_gen);
            slot.active = true;
            slot.has_children = false;
            slot.parent_idx = parent_idx;
            slot.base_pfn = base_pfn;
            slot.frame_count = frame_count;
            slot.watermark_bytes = 0;
            slot.header.label = label;
            slot.header.generation.store(next_gen, Ordering::Release);
            slot.header.iommu_pin_count.store(0, Ordering::Release);
            slot.header.map_ref_count.store(0, Ordering::Release);
            UNTYPED_LIVE_GEN[idx].store(next_gen, Ordering::Release);
            return Some((idx as u16, next_gen));
        }
    }
    None
}

/// Carve a power-of-two physically contiguous `Untyped` region from `PMM` and return a root capability.
pub fn carve_untyped(
    frame_count: usize,
    label: SecLabel,
    lease_epoch: u32,
) -> Result<UntypedCapToken, PmmError> {
    assert!(
        frame_count > 0 && frame_count.is_power_of_two() && label.domain != SecDomain::Kernel,
        "Untyped regions must be power-of-two and owned by RootSystem or Userspace"
    );
    let base_pfn = carve_contiguous_for_untyped(frame_count, frame_count, label)?;

    let allocated = with_untyped_table(|slots| {
        alloc_free_region_slot(slots, label, base_pfn, frame_count as u32, NO_PARENT)
    });
    let Some((slot_idx, gen32)) = allocated else {
        release_contiguous_from_untyped(base_pfn, frame_count, label);
        return Err(PmmError::OutOfMemory);
    };

    Ok(UntypedCapToken {
        slot_idx,
        meta: CapabilityMeta {
            rights: CapRights(
                CapRights::READ.0 | CapRights::WRITE.0 | CapRights::GRANT.0 | CapRights::RETYPE.0,
            ),
            expected_gen: gen32,
            lease_epoch,
            badge: 0,
        },
    })
}

/// Split an unallocated power-of-two `Untyped` capability into two equal child `Untyped` capabilities,
/// inheriting the parent's `SecLabel` (`ADR-007` Rule 4).
pub fn untyped_split(
    subject_label: SecLabel,
    parent_cap: UntypedCapToken,
    current_lease_epoch: u32,
) -> Result<(UntypedCapToken, UntypedCapToken), SecError> {
    let idx = parent_cap.slot_idx as usize;
    if idx >= MAX_UNTYPED_REGIONS {
        return Err(SecError::Inval);
    }

    with_untyped_table(|slots| {
        if !slots[idx].active {
            return Err(SecError::Revoked);
        }
        authorize_object_access(
            subject_label,
            parent_cap.meta,
            &slots[idx].header,
            CapRights::RETYPE,
            current_lease_epoch,
        )?;
        if slots[idx].watermark_bytes != 0
            || slots[idx].has_children
            || slots[idx].frame_count < 2
            || !slots[idx].frame_count.is_power_of_two()
        {
            return Err(SecError::Inval);
        }

        let child_label = slots[idx].header.label.inherit_child();
        let half = slots[idx].frame_count / 2;
        let base0 = slots[idx].base_pfn;
        let base1 = base0 + half;

        let Some((c0_idx, c0_gen)) =
            alloc_free_region_slot(slots, child_label, base0, half, parent_cap.slot_idx)
        else {
            return Err(SecError::Inval);
        };
        let Some((c1_idx, c1_gen)) =
            alloc_free_region_slot(slots, child_label, base1, half, parent_cap.slot_idx)
        else {
            slots[c0_idx as usize].active = false;
            UNTYPED_LIVE_GEN[c0_idx as usize].store(0, Ordering::Release);
            return Err(SecError::Inval);
        };
        slots[idx].has_children = true;

        let mk_cap = |slot_idx: u16, expected_gen: u32| UntypedCapToken {
            slot_idx,
            meta: CapabilityMeta {
                rights: parent_cap.meta.rights,
                expected_gen,
                lease_epoch: parent_cap.meta.lease_epoch,
                badge: parent_cap.meta.badge,
            },
        };
        Ok((mk_cap(c0_idx, c0_gen), mk_cap(c1_idx, c1_gen)))
    })
}

/// Retype `count` kernel objects of `kind` from `cap` via monotonic `watermark_bytes` bump,
/// enforcing mandatory zero-initialization before returning the object slice.
pub fn untyped_retype(
    subject_label: SecLabel,
    cap: UntypedCapToken,
    kind: RetypeObjKind,
    count: usize,
    current_lease_epoch: u32,
) -> Result<RetypedObjectSlice, SecError> {
    let idx = cap.slot_idx as usize;
    if idx >= MAX_UNTYPED_REGIONS || count == 0 {
        return Err(SecError::Inval);
    }

    with_untyped_table(|slots| {
        let reg = &mut slots[idx];
        if !reg.active {
            return Err(SecError::Revoked);
        }
        authorize_object_access(
            subject_label,
            cap.meta,
            &reg.header,
            CapRights::RETYPE,
            current_lease_epoch,
        )?;
        if reg.has_children {
            return Err(SecError::Inval);
        }

        let (obj_size, obj_align) = kind.layout_bytes();
        let total_req = obj_size.checked_mul(count).ok_or(SecError::Inval)?;
        let cap_bytes = (reg.frame_count as usize) * PAGE_SIZE;
        let aligned_off = (reg.watermark_bytes + obj_align - 1) & !(obj_align - 1);
        let new_watermark = aligned_off.checked_add(total_req).ok_or(SecError::Inval)?;
        if new_watermark > cap_bytes {
            return Err(SecError::Inval);
        }

        let first_pfn_off = aligned_off / PAGE_SIZE;
        let end_pfn_off = new_watermark.div_ceil(PAGE_SIZE);
        let label = reg.header.label;
        let mut backing_pfn_gen = 0u32;

        for pfn_off in first_pfn_off..end_pfn_off {
            let pfn = reg.base_pfn + (pfn_off as u32);
            let m = frame_meta(pfn).unwrap();
            let (st, _, _, _, pins, maps, cur_gen) = unpack_word0(m.word0.load(Ordering::Acquire));
            if st == FrameState::UntypedFree {
                zero_frame_4k(phys_to_hhdm((pfn as u64) << PAGE_SHIFT));
                m.word1.store(pack_seclabel(label), Ordering::Relaxed);
                m.word0.store(
                    pack_word0(
                        FrameState::UntypedRetypedObj,
                        ZeroState::Dirty,
                        0,
                        label.domain as u8,
                        pins,
                        maps,
                        cur_gen,
                    ),
                    Ordering::Release,
                );
            } else if st != FrameState::UntypedRetypedObj {
                pmm_invariant_panic(format_args!(
                    "Corrupt Untyped backing frame state {:?} on pfn={:#x}",
                    st, pfn
                ));
            }
            if pfn_off == first_pfn_off {
                backing_pfn_gen = cur_gen;
            }
        }

        let base_phys = ((reg.base_pfn as u64) << PAGE_SHIFT) + (aligned_off as u64);
        let hhdm_ptr = phys_to_hhdm(base_phys);
        // Sub-page allocations inside an already-touched `UntypedRetypedObj` frame are also zeroed.
        if obj_size < PAGE_SIZE {
            // SAFETY: `[hhdm_ptr .. hhdm_ptr + total_req)` lies inside `[base_pfn .. base_pfn + frame_count)`.
            unsafe {
                core::ptr::write_bytes(hhdm_ptr, 0, total_req);
            }
        }
        reg.watermark_bytes = new_watermark;

        Ok(RetypedObjectSlice {
            kind,
            base_phys,
            hhdm_ptr,
            byte_len: total_req,
            backing_pfn: reg.base_pfn + (first_pfn_off as u32),
            backing_pfn_gen,
        })
    })
}

fn is_descendant_of(
    slots: &[UntypedRegion; MAX_UNTYPED_REGIONS],
    cand_idx: usize,
    root_idx: u16,
) -> bool {
    let mut cur_parent = slots[cand_idx].parent_idx;
    let mut hops = 0usize;
    while cur_parent != NO_PARENT && hops < MAX_UNTYPED_REGIONS {
        if cur_parent == root_idx {
            return true;
        }
        cur_parent = slots[cur_parent as usize].parent_idx;
        hops += 1;
    }
    false
}

/// Revoke all child `Untyped` capabilities and retyped objects derived from `root_cap`.
///
/// Fails atomically with `Err(SecError::BusyPinned)` if ANY backing frame or child header holds an
/// active IOMMU DMA pin (`iommu_pin_count > 0`) or active `VSpace` mapping (`map_ref_count > 0`).
pub fn untyped_revoke(
    subject_label: SecLabel,
    root_cap: UntypedCapToken,
    current_lease_epoch: u32,
) -> Result<usize, SecError> {
    let root_idx = root_cap.slot_idx as usize;
    if root_idx >= MAX_UNTYPED_REGIONS {
        return Err(SecError::Inval);
    }

    with_untyped_table(|slots| {
        if !slots[root_idx].active {
            return Err(SecError::Revoked);
        }
        authorize_object_access(
            subject_label,
            root_cap.meta,
            &slots[root_idx].header,
            CapRights::RETYPE,
            current_lease_epoch,
        )?;

        let base_pfn = slots[root_idx].base_pfn;
        let frame_count = slots[root_idx].frame_count;
        let root_label = slots[root_idx].header.label;

        // Pass 1: Fail-closed `BusyPinned` scan across all descendants and physical backing frames.
        for idx in 0..MAX_UNTYPED_REGIONS {
            if slots[idx].active
                && (idx == root_idx || is_descendant_of(slots, idx, root_cap.slot_idx))
                && (slots[idx].header.iommu_pin_count.load(Ordering::Acquire) > 0
                    || slots[idx].header.map_ref_count.load(Ordering::Acquire) > 0)
            {
                return Err(SecError::BusyPinned);
            }
        }
        for off in 0..frame_count {
            let m = frame_meta(base_pfn + off).unwrap();
            let (_, _, _, _, pins, maps, _) = unpack_word0(m.word0.load(Ordering::Acquire));
            if pins > 0 || maps > 0 {
                return Err(SecError::BusyPinned);
            }
        }

        // Pass 2: Invalidate all descendant `Untyped` slots (`generation + 1`, `active = false`).
        let mut reclaimed_items = 0usize;
        for idx in 0..MAX_UNTYPED_REGIONS {
            if idx != root_idx && slots[idx].active && is_descendant_of(slots, idx, root_cap.slot_idx) {
                let g = slots[idx].header.generation.load(Ordering::Acquire);
                slots[idx]
                    .header
                    .generation
                    .store(next_generation(g), Ordering::Release);
                slots[idx].active = false;
                slots[idx].has_children = false;
                slots[idx].watermark_bytes = 0;
                UNTYPED_LIVE_GEN[idx].store(0, Ordering::Release);
                reclaimed_items += 1;
            }
        }

        // Scrub every retyped frame via `rep stosq`, bump frame generation, and restore `UntypedFree`.
        for off in 0..frame_count {
            let pfn = base_pfn + off;
            let m = frame_meta(pfn).unwrap();
            let (st, _, _, _, _, _, cur_gen) = unpack_word0(m.word0.load(Ordering::Acquire));
            if st == FrameState::UntypedRetypedObj {
                zero_frame_4k(phys_to_hhdm((pfn as u64) << PAGE_SHIFT));
                let next_gen = next_generation(cur_gen);
                m.word1.store(pack_seclabel(root_label), Ordering::Relaxed);
                m.word0.store(
                    pack_word0(
                        FrameState::UntypedFree,
                        ZeroState::VerifiedZero,
                        0,
                        root_label.domain as u8,
                        0,
                        0,
                        next_gen,
                    ),
                    Ordering::Release,
                );
                reclaimed_items += 1;
            }
        }

        slots[root_idx].has_children = false;
        slots[root_idx].watermark_bytes = 0;
        Ok(reclaimed_items)
    })
}

/// Return a clean/revoked root `Untyped` region back to `PMM` `GlobalFreePool`.
pub fn release_untyped_to_pmm(
    subject_label: SecLabel,
    root_cap: UntypedCapToken,
    current_lease_epoch: u32,
) -> Result<(), SecError> {
    let idx = root_cap.slot_idx as usize;
    if idx >= MAX_UNTYPED_REGIONS {
        return Err(SecError::Inval);
    }

    let (base_pfn, frame_count, label) = with_untyped_table(|slots| {
        if !slots[idx].active {
            return Err(SecError::Revoked);
        }
        authorize_object_access(
            subject_label,
            root_cap.meta,
            &slots[idx].header,
            CapRights::RETYPE,
            current_lease_epoch,
        )?;
        if slots[idx].parent_idx != NO_PARENT
            || slots[idx].has_children
            || slots[idx].watermark_bytes != 0
        {
            return Err(SecError::Inval);
        }
        let g = slots[idx].header.generation.load(Ordering::Acquire);
        slots[idx]
            .header
            .generation
            .store(next_generation(g), Ordering::Release);
        slots[idx].active = false;
        UNTYPED_LIVE_GEN[idx].store(0, Ordering::Release);
        Ok((slots[idx].base_pfn, slots[idx].frame_count as usize, slots[idx].header.label))
    })?;

    release_contiguous_from_untyped(base_pfn, frame_count, label);
    Ok(())
}

/// Strict enforcement wrapper that panics deterministically when a revoked `Untyped` capability is used.
pub fn untyped_retype_checked(
    subject_label: SecLabel,
    cap: UntypedCapToken,
    kind: RetypeObjKind,
    count: usize,
    current_lease_epoch: u32,
) -> RetypedObjectSlice {
    match untyped_retype(subject_label, cap, kind, count, current_lease_epoch) {
        Ok(slice) => slice,
        Err(SecError::Revoked) => pmm_invariant_panic(format_args!(
            "Revoked Untyped / stale capability access rejected: slot={} expected_gen={}",
            cap.slot_idx, cap.meta.expected_gen
        )),
        Err(err) => pmm_invariant_panic(format_args!(
            "Untyped retype rejected with {:?} on slot={}",
            err, cap.slot_idx
        )),
    }
}
