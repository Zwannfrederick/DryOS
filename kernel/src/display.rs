// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Capability-Gated Linear Framebuffer MMIO Subsystem (`Milestone 1C` Stage E, `INV-IPC-09`).
//!
//! Validates Limine's UEFI GOP linear framebuffer against the physical memory inventory
//! (`FrameState::ReservedOrMmio`, disjoint from usable RAM/kernel frames, overflow-checked
//! `pitch * height`), exposes it exclusively via `CapObjectKind::Framebuffer` (`SecDomain::RootSystem`),
//! and maps it into authorized display services as `VmProt::USER_MMIO_UC` (`U/S | RW | PWT | PCD | NX`).

use core::cell::UnsafeCell;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::cspace::cap::{CSpaceId, CapError, CapObjectKind, CapPtr};
use crate::cspace::revoke::attach_cap_mapping_range_record;
use crate::cspace::{insert_root_cap, with_pinned_cap};
use crate::limine::{get_latched_framebuffer, LIMINE_FRAMEBUFFER_RGB};
use crate::mem_verify::MemorySummary;
use crate::pmm::{query_frame_state, FrameState, PAGE_SIZE};
use crate::security::{current_lease_epoch, CapRights, SecDomain, SecFlags, SecLabel};
use crate::serial_println;
use crate::uaccess::{copy_to_user, SysError};
use crate::vspace::pte::{VmProt, USER_VADDR_LIMIT};
use crate::vspace::{inspect_vspace_live, map_mmio_4k, unmap_4k, VSpaceHandle, VSpaceId};

pub const MAX_FB_PAGES: usize = 4096; // Up to 16 MiB linear GOP BAR (e.g. 1920x1080x32 = 2025 pages)

/// Sanitized 32-byte framebuffer geometry descriptor copied to CPL=3 by `SYS_FB_MAP` (0 phys addrs).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UserFbInfo {
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bpp: u16,
    pub memory_model: u8,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
    pub red_size: u8,
    pub green_size: u8,
    pub blue_size: u8,
    pub _pad: [u8; 3],
    pub mapped_bytes: u64,
}

const _: () = assert!(size_of::<UserFbInfo>() == 32);

#[derive(Clone, Copy, Debug)]
pub struct ValidatedFramebuffer {
    pub phys_base: u64,
    pub num_pages: u16,
    pub info: UserFbInfo,
}

struct FbGlobal {
    available: AtomicBool,
    generation: AtomicU32,
    inner: UnsafeCell<ValidatedFramebuffer>,
}

// SAFETY: `inner` is populated once on the BSP during `init_framebuffer` prior to `available = true`.
unsafe impl Sync for FbGlobal {}

static FB_GLOBAL: FbGlobal = FbGlobal {
    available: AtomicBool::new(false),
    generation: AtomicU32::new(1),
    inner: UnsafeCell::new(ValidatedFramebuffer {
        phys_base: 0,
        num_pages: 0,
        info: UserFbInfo {
            width: 0,
            height: 0,
            pitch: 0,
            bpp: 0,
            memory_model: 0,
            red_shift: 0,
            green_shift: 0,
            blue_shift: 0,
            red_size: 0,
            green_size: 0,
            blue_size: 0,
            _pad: [0; 3],
            mapped_bytes: 0,
        },
    }),
};

#[inline]
pub fn is_framebuffer_available() -> bool {
    FB_GLOBAL.available.load(Ordering::Acquire)
}

#[inline]
pub fn framebuffer_summary() -> Option<ValidatedFramebuffer> {
    if !is_framebuffer_available() {
        return None;
    }
    // SAFETY: Immutable after `FB_GLOBAL.available` becomes `true`.
    Some(unsafe { *FB_GLOBAL.inner.get() })
}

#[inline]
pub fn is_framebuffer_cap_live(obj_id: u32, expected_gen: u32) -> bool {
    obj_id == 0
        && FB_GLOBAL.available.load(Ordering::Acquire)
        && FB_GLOBAL.generation.load(Ordering::Acquire) == expected_gen
}

/// Validate Limine's UEFI GOP framebuffer on the BSP right after `init_pmm` (`Stage E.1`).
pub fn init_framebuffer(mem: &MemorySummary, cmdline: &str) -> bool {
    if cmdline.contains("no_fb=1") {
        serial_println!("[DRY-FB] Disabled by cmdline (no_fb=1); continuing headless");
        return false;
    }
    let Some(fb) = get_latched_framebuffer() else {
        serial_println!("[DRY-FB] Limine GOP framebuffer unavailable; continuing headless");
        return false;
    };

    if fb.hhdm_virt < mem.hhdm_offset {
        serial_println!("[DRY-FB] Rejecting framebuffer outside HHDM window");
        return false;
    }
    let phys_base = fb.hhdm_virt - mem.hhdm_offset;
    if (phys_base & ((PAGE_SIZE as u64) - 1)) != 0 {
        serial_println!("[DRY-FB] Rejecting unaligned framebuffer BAR {:#x}", phys_base);
        return false;
    }
    if fb.width == 0
        || fb.height == 0
        || fb.width > 8192
        || fb.height > 8192
        || fb.bpp != 32
        || fb.memory_model != LIMINE_FRAMEBUFFER_RGB
    {
        serial_println!(
            "[DRY-FB] Unsupported GOP mode {}x{}x{} model={}",
            fb.width,
            fb.height,
            fb.bpp,
            fb.memory_model
        );
        return false;
    }

    let Some(min_pitch) = fb.width.checked_mul(4) else {
        return false;
    };
    if fb.pitch < min_pitch {
        return false;
    }
    let Some(raw_bytes) = fb.pitch.checked_mul(fb.height) else {
        return false;
    };
    let Some(aligned_bytes) = raw_bytes.checked_add((PAGE_SIZE as u64) - 1) else {
        return false;
    };
    let aligned_bytes = aligned_bytes & !((PAGE_SIZE as u64) - 1);
    let pages = (aligned_bytes / (PAGE_SIZE as u64)) as usize;
    if pages == 0 || pages > MAX_FB_PAGES {
        serial_println!("[DRY-FB] Framebuffer page count {} out of bounds", pages);
        return false;
    }

    // Verify every 4 KiB page is strictly `FrameState::ReservedOrMmio` (never usable RAM/kernel).
    for p in 0..pages {
        let pa = phys_base + (p as u64) * (PAGE_SIZE as u64);
        if query_frame_state(pa) != FrameState::ReservedOrMmio {
            serial_println!("[DRY-FB] Rejecting non-MMIO page at {:#x}", pa);
            return false;
        }
    }

    let vfb = ValidatedFramebuffer {
        phys_base,
        num_pages: pages as u16,
        info: UserFbInfo {
            width: fb.width as u32,
            height: fb.height as u32,
            pitch: fb.pitch as u32,
            bpp: fb.bpp,
            memory_model: fb.memory_model,
            red_shift: fb.red_mask_shift,
            green_shift: fb.green_mask_shift,
            blue_shift: fb.blue_mask_shift,
            red_size: fb.red_mask_size,
            green_size: fb.green_mask_size,
            blue_size: fb.blue_mask_size,
            _pad: [0; 3],
            mapped_bytes: aligned_bytes,
        },
    };

    // SAFETY: Single-threaded BSP initialization prior to `available.store(true, Release)`.
    unsafe {
        *FB_GLOBAL.inner.get() = vfb;
    }
    FB_GLOBAL.available.store(true, Ordering::Release);
    serial_println!(
        "[DRY-FB] GOP validated: {}x{}x{} pitch={} pages={} phys={:#x}",
        vfb.info.width,
        vfb.info.height,
        vfb.info.bpp,
        vfb.info.pitch,
        vfb.num_pages,
        vfb.phys_base
    );
    true
}

/// Mint a root `CapObjectKind::Framebuffer` capability exclusively into a `SecDomain::RootSystem` `CSpace`.
pub fn mint_root_framebuffer_cap(
    cspace_id: CSpaceId,
    root_subj: SecLabel,
) -> Result<CapPtr, CapError> {
    if root_subj.domain != SecDomain::RootSystem
        || root_subj.flags.contains(SecFlags::DEV_WORKSPACE)
    {
        return Err(CapError::DomainViolation);
    }
    if !is_framebuffer_available() {
        return Err(CapError::InvalidHandle);
    }
    let gen = FB_GLOBAL.generation.load(Ordering::Acquire);
    insert_root_cap(
        cspace_id,
        CapObjectKind::Framebuffer,
        0,
        gen,
        CapRights::FB_ROOT,
        root_subj,
        0,
        0,
    )
}

/// Map `fb_cap` into the calling process's user `VSpace` at `user_vaddr` (`VmProt::USER_MMIO_UC`)
/// and write sanitized `UserFbInfo` to `user_info_out_ptr` (`SYS_FB_MAP`).
pub fn sys_fb_map(
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    vspace_id: VSpaceId,
    fb_cap_raw: u64,
    user_vaddr: u64,
    user_info_out_ptr: u64,
) -> Result<isize, SysError> {
    if (user_vaddr & ((PAGE_SIZE as u64) - 1)) != 0
        || user_vaddr < (PAGE_SIZE as u64)
        || (user_info_out_ptr & 7) != 0
    {
        return Err(SysError::Inval);
    }
    let Some(vfb) = framebuffer_summary() else {
        return Err(SysError::NoSys);
    };
    let Some(end_vaddr) = user_vaddr.checked_add(vfb.info.mapped_bytes) else {
        return Err(SysError::Inval);
    };
    if end_vaddr > USER_VADDR_LIMIT {
        return Err(SysError::Inval);
    }
    let Some((vs_gen, _, _)) = inspect_vspace_live(vspace_id) else {
        return Err(SysError::Inval);
    };
    let vs_h = VSpaceHandle {
        id: vspace_id,
        generation: vs_gen,
    };

    let req = CapRights(CapRights::READ.0 | CapRights::WRITE.0 | CapRights::MAP.0);
    let now_epoch = current_lease_epoch();
    let ptr = CapPtr(fb_cap_raw);
    let mut fb_global_slot = 0u32;

    with_pinned_cap(cspace_id, subj_label, ptr, req, now_epoch, |res| {
        if res.obj_kind != CapObjectKind::Framebuffer {
            return Err(CapError::WrongObjectKind);
        }
        let pages = vfb.num_pages as usize;
        for p in 0..pages {
            let va = user_vaddr + (p as u64) * (PAGE_SIZE as u64);
            let pa = vfb.phys_base + (p as u64) * (PAGE_SIZE as u64);
            if map_mmio_4k(vs_h, va, pa, VmProt::USER_MMIO_UC).is_err() {
                for rb in 0..p {
                    let _ = unmap_4k(vs_h, user_vaddr + (rb as u64) * (PAGE_SIZE as u64));
                }
                return Err(CapError::PagingError);
            }
        }
        if let Err(e) = attach_cap_mapping_range_record(
            res.global_slot,
            ptr.slot_gen(),
            vs_h.id,
            vs_h.generation,
            user_vaddr,
            false,
            vfb.num_pages,
        ) {
            for rb in 0..pages {
                let _ = unmap_4k(vs_h, user_vaddr + (rb as u64) * (PAGE_SIZE as u64));
            }
            return Err(e);
        }
        fb_global_slot = res.global_slot;
        Ok(())
    })
    .map_err(|e| match e {
        CapError::InvalidHandle | CapError::StaleGenerationAba => SysError::BadCap,
        CapError::Revoked | CapError::LeaseExpired => SysError::Revoked,
        CapError::PagingError | CapError::OutOfSlots => SysError::Inval,
        _ => SysError::Perm,
    })?;

    // SAFETY: `UserFbInfo` is `#[repr(C)]` POD (`32` bytes) with initialized `_pad`.
    let info_bytes = unsafe {
        core::slice::from_raw_parts(
            (&raw const vfb.info).cast::<u8>(),
            size_of::<UserFbInfo>(),
        )
    };
    if let Err(fault) = copy_to_user(user_info_out_ptr, info_bytes) {
        let _ = crate::cspace::revoke::detach_matching_cap_mapping_record(
            fb_global_slot,
            vs_h.id,
            user_vaddr,
        );
        for rb in 0..(vfb.num_pages as usize) {
            let _ = unmap_4k(vs_h, user_vaddr + (rb as u64) * (PAGE_SIZE as u64));
        }
        return Err(fault);
    }

    Ok(vfb.num_pages as isize)
}
