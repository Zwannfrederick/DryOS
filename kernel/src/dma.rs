// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Pinned `DEVICE_DMA` Allocation (`SYS_DMA_ALLOC`) and Scatter-Gather IOMMU DMA Control
//! (`SYS_DMA_CTL`, syscall `32`) with per-device `IOVA != HPA` separation (`Milestone 2.1`).

use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::size_of;
use core::sync::atomic::{fence, AtomicBool, AtomicU64, Ordering};

use crate::arch::{irq_restore, irq_save_cli};
use crate::cspace::cap::{CSpaceId, CapError, CapObjectKind, CapPtr};
use crate::cspace::objects::{
    cspace_alloc_frame, cspace_vspace_map_2m, cspace_vspace_map_4k, cspace_vspace_unmap_2m,
    cspace_vspace_unmap_4k,
};
use crate::cspace::revoke::cspace_destroy_object;
use crate::cspace::with_pinned_cap;
use crate::iommu::ops::IommuPerms;
use crate::iommu::{
    detach_and_scrub_domain, is_hw_enforcing, map_dma_range, map_dma_range_iova,
    poll_and_clear_faults, unmap_dma_range,
};
use crate::ipc::map_cap_err_to_sys;
use crate::pci::{
    claim_pci_slot, is_pci_cap_live, record_pci_denied_first_or, release_claimed_pci_slot,
    reset_pci_slot_owner, validated_pci_slot, MAX_MANAGED_PCI_DEVS,
};
use crate::pmm::{
    adjust_frame_iommu_pin, FrameHandle, PageOrder, HUGE_2M_FRAMES, PAGE_SIZE,
};
use crate::proc::process_vspace_cap;
use crate::security::{current_lease_epoch, CapRights, SecDomain, SecFlags, SecLabel};
use crate::uaccess::{copy_from_user, copy_to_user, SysError};
use crate::vspace::pte::{VmProt, USER_VADDR_LIMIT};

const MAX_DMA_PINS_PER_DEV: usize = 32;
const MAX_SG_BUFS_PER_DEV: usize = 4;
const DMA_ALLOC_FLAG_2M: u64 = 1;

pub const DMA_MAX_SG_SEGS: usize = 64;
const DMA_IOVA_BASE: u64 = 0xc000_0000;

pub const DMA_OP_ALLOC_SG: u64 = 1;
pub const DMA_OP_FREE: u64 = 2;
pub const DMA_OP_SYNC: u64 = 3;
pub const DMA_OP_QUERY_STATS: u64 = 4;

pub const DMA_DIR_BIDIR: u8 = 0;
pub const DMA_DIR_TO_DEV: u8 = 1;
pub const DMA_DIR_FROM_DEV: u8 = 2;

pub const DMA_SG_FLAG_IOVA_DISTINCT: u8 = 1 << 0;
pub const DMA_SG_FLAG_HW_IOMMU: u8 = 1 << 1;

#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub struct UserDmaSgSeg {
    pub iova: u64,
    pub len_bytes: u32,
    pub flags: u32,
}

#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub struct UserDmaSgDesc {
    pub user_vaddr: u64,
    pub buf_id: u64,
    pub iova_base: u64,
    pub total_bytes: u64,
    pub page_count: u16,
    pub direction: u8,
    pub out_flags: u8,
    pub _pad: u32,
}

#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub struct UserDmaStats {
    pub active_sg_bufs: u32,
    pub active_single_pins: u32,
    pub pinned_pages: u32,
    pub hw_enforcing: u8,
    pub _pad: [u8; 3],
    pub mapped_iova_bytes: u64,
    pub iova_cursor: u64,
    pub iommu_faults: u64,
}

const _: () = assert!(size_of::<UserDmaSgSeg>() == 16);
const _: () = assert!(size_of::<UserDmaSgDesc>() == 40);
const _: () = assert!(size_of::<UserDmaStats>() == 40);

#[derive(Clone, Copy)]
struct SgBufEntry {
    active: bool,
    buf_id: u64,
    owner_cspace: CSpaceId,
    direction: u8,
    page_count: u16,
    iova_base: u64,
    user_vaddr: u64,
    frames: [Option<(u64, FrameHandle)>; DMA_MAX_SG_SEGS],
}

impl SgBufEntry {
    const EMPTY: Self = Self {
        active: false,
        buf_id: 0,
        owner_cspace: 0,
        direction: 0,
        page_count: 0,
        iova_base: 0,
        user_vaddr: 0,
        frames: [None; DMA_MAX_SG_SEGS],
    };
}

struct DmaSlotInner {
    iova_next: u64,
    mapped_iova_bytes: u64,
    pinned_frames: [Option<FrameHandle>; MAX_DMA_PINS_PER_DEV],
    sg_bufs: [SgBufEntry; MAX_SG_BUFS_PER_DEV],
}

impl DmaSlotInner {
    const fn new() -> Self {
        Self {
            iova_next: DMA_IOVA_BASE,
            mapped_iova_bytes: 0,
            pinned_frames: [None; MAX_DMA_PINS_PER_DEV],
            sg_bufs: [SgBufEntry::EMPTY; MAX_SG_BUFS_PER_DEV],
        }
    }
}

struct PciDmaSlot {
    lock: AtomicBool,
    inner: UnsafeCell<DmaSlotInner>,
}

impl PciDmaSlot {
    const fn new() -> Self {
        Self {
            lock: AtomicBool::new(false),
            inner: UnsafeCell::new(DmaSlotInner::new()),
        }
    }
}

// SAFETY: `inner` is mutated exclusively under per-slot `lock` with interrupts disabled (`CLI`).
unsafe impl Sync for PciDmaSlot {}

static DMA_SLOTS: [PciDmaSlot; MAX_MANAGED_PCI_DEVS] =
    [const { PciDmaSlot::new() }; MAX_MANAGED_PCI_DEVS];
static NEXT_SG_BUF_ID: AtomicU64 = AtomicU64::new(1);

#[inline]
fn with_dma_slot_lock<R>(dev_idx: usize, f: impl FnOnce(&mut DmaSlotInner) -> R) -> R {
    let slot = &DMA_SLOTS[dev_idx];
    let saved_if = irq_save_cli();
    while slot
        .lock
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    // SAFETY: Exclusive per-slot `lock` + `CLI` held.
    let out = f(unsafe { &mut *slot.inner.get() });
    slot.lock.store(false, Ordering::Release);
    irq_restore(saved_if);
    out
}

#[inline]
fn perms_for_dir(direction: u8) -> Result<IommuPerms, SysError> {
    match direction {
        DMA_DIR_BIDIR => Ok(IommuPerms::RW),
        DMA_DIR_TO_DEV => Ok(IommuPerms::READ),
        DMA_DIR_FROM_DEV => Ok(IommuPerms::WRITE),
        _ => Err(SysError::Inval),
    }
}

fn unmap_and_destroy_dma_frame(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    vs_cap: CapPtr,
    frame_cap: CapPtr,
    user_vaddr: u64,
    is_2m: bool,
) {
    if is_2m {
        let _ = cspace_vspace_unmap_2m(cspace_id, subj_label, vs_cap, frame_cap, user_vaddr, 0);
    } else {
        let _ = cspace_vspace_unmap_4k(cspace_id, subj_label, vs_cap, frame_cap, user_vaddr, 0);
    }
    let _ = cspace_destroy_object(cspace_id, subj_label, frame_cap, 0);
}

/// Release all IOMMU mappings, `iommu_pin_count` pins, and reset IOVA cursor for `dev_idx`.
pub(crate) fn quiesce_dma_for_slot(dev_idx: usize, cspace_id: CSpaceId) {
    with_dma_slot_lock(dev_idx, |st| {
        detach_and_scrub_domain(dev_idx);
        for pin in st.pinned_frames.iter_mut() {
            if let Some(fh) = pin.take() {
                let _ = adjust_frame_iommu_pin(fh, -1);
            }
        }
        for sg in st.sg_bufs.iter_mut() {
            if sg.active {
                for item in sg.frames.iter_mut() {
                    if let Some((_, fh)) = item.take() {
                        let _ = adjust_frame_iommu_pin(fh, -1);
                    }
                }
                sg.active = false;
                sg.buf_id = 0;
            }
        }
        st.mapped_iova_bytes = 0;
        st.iova_next = DMA_IOVA_BASE;
        reset_pci_slot_owner(dev_idx, cspace_id);
    });
}

fn check_pci_cap(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    pci_cap_raw: u64,
    req: CapRights,
) -> Result<usize, SysError> {
    with_pinned_cap(
        cspace_id,
        subj_label,
        CapPtr(pci_cap_raw),
        req,
        current_lease_epoch(),
        |res| {
            if res.obj_kind != CapObjectKind::PciDev || !is_pci_cap_live(res.obj_id, res.expected_obj_gen) {
                return Err(CapError::WrongObjectKind);
            }
            Ok(res.obj_id as usize)
        },
    )
    .map_err(|e| {
        record_pci_denied_first_or(None);
        map_cap_err_to_sys(e)
    })
}

/// `SYS_DMA_ALLOC` (`30`): Allocate a pinned `4 KiB` or `2 MiB` (`user_vaddr & DMA_ALLOC_FLAG_2M`)
/// `DEVICE_DMA | NO_DELEGATE` frame for an authorized `RootSystem` driver holding `pci_cap`,
/// map it (`USER_RW`), write physical DMA address to `paddr_out_ptr`, and return `(0, frame_cap.0)`.
#[inline(never)]
pub fn sys_dma_alloc(
    pid: u32,
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    pci_cap_raw: u64,
    user_vaddr: u64,
    paddr_out_ptr: u64,
) -> Result<(isize, u64), SysError> {
    if subj_label.domain != SecDomain::RootSystem
        || subj_label.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        record_pci_denied_first_or(None);
        return Err(SysError::Perm);
    }
    let is_2m = (user_vaddr & DMA_ALLOC_FLAG_2M) != 0;
    let map_vaddr = user_vaddr & !DMA_ALLOC_FLAG_2M;
    let align_mask = if is_2m {
        (HUGE_2M_FRAMES as u64 * PAGE_SIZE as u64) - 1
    } else {
        (PAGE_SIZE as u64) - 1
    };
    if (map_vaddr & align_mask) != 0
        || map_vaddr < (PAGE_SIZE as u64)
        || paddr_out_ptr == 0
        || (paddr_out_ptr & 7) != 0
    {
        return Err(SysError::Inval);
    }
    let vs_cap = process_vspace_cap(pid).ok_or(SysError::Perm)?;
    let req = CapRights(CapRights::READ.0 | CapRights::WRITE.0 | CapRights::MAP.0);
    let dev_idx = check_pci_cap(cspace_id, subj_label, pci_cap_raw, req)?;

    let dma_label = SecLabel::new(
        SecDomain::RootSystem,
        SecFlags(SecFlags::DEVICE_DMA.0 | SecFlags::NO_DELEGATE.0),
        subj_label.compartment_id,
        subj_label.policy_tag,
    );
    let order = if is_2m {
        PageOrder::Order2MiB
    } else {
        PageOrder::Order4KiB
    };
    let (frame_cap, fh) =
        cspace_alloc_frame(cspace_id, subj_label, dma_label, order, false, 0)
            .map_err(map_cap_err_to_sys)?;

    let map_res = if is_2m {
        cspace_vspace_map_2m(cspace_id, subj_label, vs_cap, frame_cap, map_vaddr, VmProt::USER_RW, 0)
    } else {
        cspace_vspace_map_4k(cspace_id, subj_label, vs_cap, frame_cap, map_vaddr, VmProt::USER_RW, 0)
    };
    if let Err(e) = map_res {
        let _ = cspace_destroy_object(cspace_id, subj_label, frame_cap, 0);
        return Err(map_cap_err_to_sys(e));
    }

    let len_bytes = if is_2m {
        (HUGE_2M_FRAMES as u64) * (PAGE_SIZE as u64)
    } else {
        PAGE_SIZE as u64
    };
    let dma_paddr: u64 = fh.phys_addr();
    let Some(vdev) = validated_pci_slot(dev_idx) else {
        unmap_and_destroy_dma_frame(cspace_id, subj_label, vs_cap, frame_cap, map_vaddr, is_2m);
        return Err(SysError::Inval);
    };

    let mut rollback_claimed = false;
    let pinned_ok = with_dma_slot_lock(dev_idx, |st| {
        let claimed_new = claim_pci_slot(dev_idx, cspace_id, &vdev).map_err(map_cap_err_to_sys)?;
        let Some(free_slot) = st.pinned_frames.iter_mut().find(|s| s.is_none()) else {
            release_claimed_pci_slot(dev_idx, claimed_new);
            return Err(SysError::NoMem);
        };
        if adjust_frame_iommu_pin(fh, 1).is_err() {
            release_claimed_pci_slot(dev_idx, claimed_new);
            return Err(SysError::NoMem);
        }
        if map_dma_range(dev_idx, dma_paddr, len_bytes).is_err() {
            let _ = adjust_frame_iommu_pin(fh, -1);
            release_claimed_pci_slot(dev_idx, claimed_new);
            return Err(SysError::NoMem);
        }
        *free_slot = Some(fh);
        rollback_claimed = claimed_new;
        Ok(())
    });
    if let Err(e) = pinned_ok {
        unmap_and_destroy_dma_frame(cspace_id, subj_label, vs_cap, frame_cap, map_vaddr, is_2m);
        return Err(e);
    }

    let pa_bytes = dma_paddr.to_le_bytes();
    if let Err(fault) = copy_to_user(paddr_out_ptr, &pa_bytes) {
        with_dma_slot_lock(dev_idx, |st| {
            unmap_dma_range(dev_idx, dma_paddr, len_bytes);
            for pin in st.pinned_frames.iter_mut() {
                if *pin == Some(fh) {
                    *pin = None;
                    let _ = adjust_frame_iommu_pin(fh, -1);
                    break;
                }
            }
            if rollback_claimed
                && st.pinned_frames.iter().all(|p| p.is_none())
                && st.sg_bufs.iter().all(|s| !s.active)
            {
                release_claimed_pci_slot(dev_idx, true);
            }
        });
        unmap_and_destroy_dma_frame(cspace_id, subj_label, vs_cap, frame_cap, map_vaddr, is_2m);
        return Err(fault);
    }

    Ok((0, frame_cap.0))
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn dma_op_alloc_sg(
    pid: u32,
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    dev_idx: usize,
    desc_ptr: u64,
    segs_ptr: u64,
) -> Result<(isize, u64), SysError> {
    if desc_ptr == 0 || (desc_ptr & 7) != 0 || (segs_ptr != 0 && (segs_ptr & 7) != 0) {
        return Err(SysError::Inval);
    }
    let mut desc = UserDmaSgDesc::default();
    let desc_bytes = unsafe {
        core::slice::from_raw_parts_mut(
            (&raw mut desc).cast::<u8>(),
            size_of::<UserDmaSgDesc>(),
        )
    };
    copy_from_user(desc_bytes, desc_ptr)?;

    let pages = desc.page_count as usize;
    if pages == 0
        || pages > DMA_MAX_SG_SEGS
        || desc.user_vaddr < (PAGE_SIZE as u64)
        || (desc.user_vaddr & ((PAGE_SIZE as u64) - 1)) != 0
    {
        return Err(SysError::Inval);
    }
    let total_bytes = (pages as u64) * (PAGE_SIZE as u64);
    let Some(end_va) = desc.user_vaddr.checked_add(total_bytes) else {
        return Err(SysError::Inval);
    };
    if end_va > USER_VADDR_LIMIT {
        return Err(SysError::Inval);
    }
    let perms = perms_for_dir(desc.direction)?;
    let vs_cap = process_vspace_cap(pid).ok_or(SysError::Perm)?;
    let Some(vdev) = validated_pci_slot(dev_idx) else {
        return Err(SysError::Inval);
    };

    let has_free_slot = with_dma_slot_lock(dev_idx, |st| st.sg_bufs.iter().any(|s| !s.active));
    if !has_free_slot {
        return Err(SysError::NoMem);
    }

    let dma_label = SecLabel::new(
        SecDomain::RootSystem,
        SecFlags(SecFlags::DEVICE_DMA.0 | SecFlags::NO_DELEGATE.0),
        subj_label.compartment_id,
        subj_label.policy_tag,
    );
    let mut frames: [Option<(u64, FrameHandle)>; DMA_MAX_SG_SEGS] = [None; DMA_MAX_SG_SEGS];
    for (idx, dst) in frames.iter_mut().take(pages).enumerate() {
        let va = desc.user_vaddr + (idx as u64) * (PAGE_SIZE as u64);
        let Ok((fr_cap, fh)) =
            cspace_alloc_frame(cspace_id, subj_label, dma_label, PageOrder::Order4KiB, false, 0)
        else {
            for (rb_idx, rb) in frames.iter_mut().take(idx).enumerate() {
                if let Some((rcap, _)) = rb.take() {
                    let rva = desc.user_vaddr + (rb_idx as u64) * (PAGE_SIZE as u64);
                    unmap_and_destroy_dma_frame(cspace_id, subj_label, vs_cap, CapPtr(rcap), rva, false);
                }
            }
            return Err(SysError::NoMem);
        };
        if let Err(e) =
            cspace_vspace_map_4k(cspace_id, subj_label, vs_cap, fr_cap, va, VmProt::USER_RW, 0)
        {
            let _ = cspace_destroy_object(cspace_id, subj_label, fr_cap, 0);
            for (rb_idx, rb) in frames.iter_mut().take(idx).enumerate() {
                if let Some((rcap, _)) = rb.take() {
                    let rva = desc.user_vaddr + (rb_idx as u64) * (PAGE_SIZE as u64);
                    unmap_and_destroy_dma_frame(cspace_id, subj_label, vs_cap, CapPtr(rcap), rva, false);
                }
            }
            return Err(map_cap_err_to_sys(e));
        }
        *dst = Some((fr_cap.0, fh));
    }

    let hw = is_hw_enforcing();
    let mut segs_out = [UserDmaSgSeg::default(); DMA_MAX_SG_SEGS];
    let buf_id = NEXT_SG_BUF_ID.fetch_add(1, Ordering::AcqRel).max(1);

    let map_res = with_dma_slot_lock(dev_idx, |st| {
        let claimed_new = claim_pci_slot(dev_idx, cspace_id, &vdev).map_err(map_cap_err_to_sys)?;
        let Some(sg_slot) = st.sg_bufs.iter_mut().find(|s| !s.active) else {
            release_claimed_pci_slot(dev_idx, claimed_new);
            return Err(SysError::NoMem);
        };

        let iova_base = if hw {
            let mut candidate = st.iova_next;
            // Ensure no segment in `[candidate .. candidate + total_bytes)` aliases its own HPA.
            for _ in 0..8 {
                let conflict = frames
                    .iter()
                    .take(pages)
                    .enumerate()
                    .any(|(i, item)| {
                        item.is_some_and(|(_, fh)| {
                            candidate + (i as u64) * (PAGE_SIZE as u64) == fh.phys_addr()
                        })
                    });
                if !conflict {
                    break;
                }
                candidate = candidate.saturating_add(total_bytes);
            }
            candidate
        } else {
            frames[0].map_or(0, |(_, fh)| fh.phys_addr())
        };

        let mut mapped_cnt = 0usize;
        for (i, item) in frames.iter().take(pages).enumerate() {
            let Some((_, fh)) = *item else {
                continue;
            };
            let hpa = fh.phys_addr();
            let seg_iova = if hw {
                iova_base + (i as u64) * (PAGE_SIZE as u64)
            } else {
                hpa
            };
            if adjust_frame_iommu_pin(fh, 1).is_err() {
                for (rb_i, rb_item) in frames.iter().take(mapped_cnt).enumerate() {
                    if let Some((_, rfh)) = *rb_item {
                        if hw {
                            unmap_dma_range(
                                dev_idx,
                                iova_base + (rb_i as u64) * (PAGE_SIZE as u64),
                                PAGE_SIZE as u64,
                            );
                        }
                        let _ = adjust_frame_iommu_pin(rfh, -1);
                    }
                }
                release_claimed_pci_slot(dev_idx, claimed_new);
                return Err(SysError::NoMem);
            }
            if hw && map_dma_range_iova(dev_idx, seg_iova, hpa, PAGE_SIZE as u64, perms).is_err() {
                let _ = adjust_frame_iommu_pin(fh, -1);
                for (rb_i, rb_item) in frames.iter().take(mapped_cnt).enumerate() {
                    if let Some((_, rfh)) = *rb_item {
                        unmap_dma_range(
                            dev_idx,
                            iova_base + (rb_i as u64) * (PAGE_SIZE as u64),
                            PAGE_SIZE as u64,
                        );
                        let _ = adjust_frame_iommu_pin(rfh, -1);
                    }
                }
                release_claimed_pci_slot(dev_idx, claimed_new);
                return Err(SysError::NoMem);
            }
            segs_out[i] = UserDmaSgSeg {
                iova: seg_iova,
                len_bytes: PAGE_SIZE as u32,
                flags: if hw && seg_iova != hpa {
                    DMA_SG_FLAG_IOVA_DISTINCT as u32
                } else {
                    0
                },
            };
            mapped_cnt += 1;
        }

        if hw {
            st.iova_next = iova_base.saturating_add(total_bytes);
            st.mapped_iova_bytes = st.mapped_iova_bytes.saturating_add(total_bytes);
        }
        *sg_slot = SgBufEntry {
            active: true,
            buf_id,
            owner_cspace: cspace_id,
            direction: desc.direction,
            page_count: desc.page_count,
            iova_base,
            user_vaddr: desc.user_vaddr,
            frames,
        };
        Ok((iova_base, claimed_new))
    });

    let (iova_base, claimed_new) = match map_res {
        Ok(pair) => pair,
        Err(e) => {
            for (rb_idx, rb) in frames.iter_mut().take(pages).enumerate() {
                if let Some((rcap, _)) = rb.take() {
                    let rva = desc.user_vaddr + (rb_idx as u64) * (PAGE_SIZE as u64);
                    unmap_and_destroy_dma_frame(cspace_id, subj_label, vs_cap, CapPtr(rcap), rva, false);
                }
            }
            return Err(e);
        }
    };

    desc.buf_id = buf_id;
    desc.iova_base = iova_base;
    desc.total_bytes = total_bytes;
    desc.out_flags = if hw {
        let distinct = frames
            .iter()
            .take(pages)
            .enumerate()
            .all(|(i, item)| {
                item.is_some_and(|(_, fh)| iova_base + (i as u64) * (PAGE_SIZE as u64) != fh.phys_addr())
            });
        DMA_SG_FLAG_HW_IOMMU | if distinct { DMA_SG_FLAG_IOVA_DISTINCT } else { 0 }
    } else {
        0
    };

    let out_desc_bytes = unsafe {
        core::slice::from_raw_parts((&raw const desc).cast::<u8>(), size_of::<UserDmaSgDesc>())
    };
    let copy_ok = copy_to_user(desc_ptr, out_desc_bytes).and_then(|()| {
        if segs_ptr != 0 {
            let seg_bytes = unsafe {
                core::slice::from_raw_parts(
                    segs_out.as_ptr().cast::<u8>(),
                    pages * size_of::<UserDmaSgSeg>(),
                )
            };
            copy_to_user(segs_ptr, seg_bytes)
        } else {
            Ok(())
        }
    });
    if let Err(fault) = copy_ok {
        let _ = dma_free_internal(cspace_id, subj_label, pid, dev_idx, buf_id, claimed_new);
        return Err(fault);
    }

    Ok((0, buf_id))
}

fn dma_free_internal(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    pid: u32,
    dev_idx: usize,
    buf_id: u64,
    release_on_empty: bool,
) -> Result<usize, SysError> {
    if buf_id == 0 {
        return Err(SysError::Inval);
    }
    let hw = is_hw_enforcing();
    let mut removed: Option<SgBufEntry> = None;
    with_dma_slot_lock(dev_idx, |st| {
        let Some(sg) = st
            .sg_bufs
            .iter_mut()
            .find(|s| s.active && s.buf_id == buf_id && s.owner_cspace == cspace_id)
        else {
            return;
        };
        let pages = sg.page_count as usize;
        let len_bytes = (pages as u64) * (PAGE_SIZE as u64);
        if hw {
            unmap_dma_range(dev_idx, sg.iova_base, len_bytes);
            st.mapped_iova_bytes = st.mapped_iova_bytes.saturating_sub(len_bytes);
        }
        for item in sg.frames.iter().take(pages) {
            if let Some((_, fh)) = *item {
                let _ = adjust_frame_iommu_pin(fh, -1);
            }
        }
        let copy = *sg;
        *sg = SgBufEntry::EMPTY;
        removed = Some(copy);
        if release_on_empty
            && st.pinned_frames.iter().all(|p| p.is_none())
            && st.sg_bufs.iter().all(|s| !s.active)
        {
            release_claimed_pci_slot(dev_idx, true);
        }
    });

    let Some(entry) = removed else {
        return Err(SysError::Inval);
    };
    let pages = entry.page_count as usize;
    if let Some(vs_cap) = process_vspace_cap(pid) {
        for (i, item) in entry.frames.iter().take(pages).enumerate() {
            if let Some((fr_cap, _)) = *item {
                let va = entry.user_vaddr + (i as u64) * (PAGE_SIZE as u64);
                unmap_and_destroy_dma_frame(cspace_id, subj_label, vs_cap, CapPtr(fr_cap), va, false);
            }
        }
    }
    Ok(pages)
}

#[inline(never)]
fn dma_op_sync(cspace_id: CSpaceId, dev_idx: usize, buf_id: u64, direction: u8) -> Result<(isize, u64), SysError> {
    if buf_id == 0 || direction > DMA_DIR_FROM_DEV {
        return Err(SysError::Inval);
    }
    let found = with_dma_slot_lock(dev_idx, |st| {
        st.sg_bufs.iter().any(|s| {
            s.active
                && s.buf_id == buf_id
                && s.owner_cspace == cspace_id
                && (s.direction == DMA_DIR_BIDIR || s.direction == direction)
        })
    });
    if !found {
        return Err(SysError::Inval);
    }
    fence(Ordering::SeqCst);
    // SAFETY: Serializing fence on x86-64 ensuring store visibility prior to device doorbell / CPU read.
    unsafe {
        asm!("mfence", options(nostack, preserves_flags));
    }
    Ok((0, 0))
}

#[inline(never)]
fn dma_op_query_stats(dev_idx: usize, out_ptr: u64) -> Result<(isize, u64), SysError> {
    if out_ptr == 0 || (out_ptr & 7) != 0 {
        return Err(SysError::Inval);
    }
    let hw = is_hw_enforcing();
    let mut stats = with_dma_slot_lock(dev_idx, |st| {
        let single_cnt = st.pinned_frames.iter().filter(|p| p.is_some()).count() as u32;
        let sg_cnt = st.sg_bufs.iter().filter(|s| s.active).count() as u32;
        let sg_pages: u32 = st
            .sg_bufs
            .iter()
            .filter(|s| s.active)
            .map(|s| s.page_count as u32)
            .sum();
        UserDmaStats {
            active_sg_bufs: sg_cnt,
            active_single_pins: single_cnt,
            pinned_pages: single_cnt.saturating_add(sg_pages),
            hw_enforcing: hw as u8,
            _pad: [0; 3],
            mapped_iova_bytes: st.mapped_iova_bytes,
            iova_cursor: st.iova_next,
            iommu_faults: 0,
        }
    });
    stats.iommu_faults = poll_and_clear_faults();
    let bytes = unsafe {
        core::slice::from_raw_parts((&raw const stats).cast::<u8>(), size_of::<UserDmaStats>())
    };
    copy_to_user(out_ptr, bytes)?;
    Ok((0, 0))
}

/// `SYS_DMA_CTL` (`32`): Capability-gated Scatter-Gather IOMMU DMA management.
#[inline(never)]
pub fn sys_dma_ctl(
    pid: u32,
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    pci_cap_raw: u64,
    op: u64,
    arg2: u64,
    arg3: u64,
) -> Result<(isize, u64), SysError> {
    if subj_label.domain != SecDomain::RootSystem
        || subj_label.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        record_pci_denied_first_or(None);
        return Err(SysError::Perm);
    }
    match op {
        DMA_OP_ALLOC_SG => {
            let req = CapRights(CapRights::READ.0 | CapRights::WRITE.0 | CapRights::MAP.0);
            let dev_idx = check_pci_cap(cspace_id, subj_label, pci_cap_raw, req)?;
            dma_op_alloc_sg(pid, cspace_id, subj_label, dev_idx, arg2, arg3)
        }
        DMA_OP_FREE => {
            let req = CapRights(CapRights::READ.0 | CapRights::WRITE.0);
            let dev_idx = check_pci_cap(cspace_id, subj_label, pci_cap_raw, req)?;
            let pages = dma_free_internal(cspace_id, subj_label, pid, dev_idx, arg2, false)?;
            Ok((pages as isize, 0))
        }
        DMA_OP_SYNC => {
            let dev_idx = check_pci_cap(cspace_id, subj_label, pci_cap_raw, CapRights::READ)?;
            dma_op_sync(cspace_id, dev_idx, arg2, arg3 as u8)
        }
        DMA_OP_QUERY_STATS => {
            let dev_idx = check_pci_cap(cspace_id, subj_label, pci_cap_raw, CapRights::READ)?;
            dma_op_query_stats(dev_idx, arg2)
        }
        _ => Err(SysError::Inval),
    }
}
