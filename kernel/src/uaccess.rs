// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Safe User-Memory Copy (`copy_from_user` / `copy_to_user`), Runtime `SMAP` (`STAC`/`CLAC`)
//! Enforcement, and `#PF` Exception-Table (`extable`) Fixup (`Milestone 1B-D` Stage C).

use core::arch::x86_64::{__cpuid, __cpuid_count};
use core::arch::{asm, global_asm};
use core::ptr::read_volatile;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};

use crate::arch::{read_cr4, read_rflags, write_cr4};
use crate::idt::TrapFrame;
use crate::pmm::PAGE_SHIFT;
use crate::sched::current_cpu_idx;
use crate::vspace::pte::{
    is_canonical_48, pt_indices, KERNEL_HALF_BASE, PAGE_SIZE_2M, PAGE_SIZE_4K, PTE_HUGE_PAGE,
    PTE_PHYS_MASK_4K, PTE_PRESENT, PTE_USER, USER_VADDR_LIMIT, USER_VADDR_MIN,
};
use crate::vspace::{cpu_active_vspace, pt_virt_ptr, KERNEL_VSPACE_ID, MAX_VSPACES, VSPACE_TABLE};

const CR4_SMAP: u64 = 1 << 21;
const RFLAGS_AC: u64 = 1 << 18;
pub const MAX_UACCESS_BYTES: usize = 64 * 1024;

/// Read directly from assembly (`dry_isr_common` & `__dry_uaccess_memcpy`) to gate `stac`/`clac`.
#[no_mangle]
pub static DRY_SMAP_ACTIVE: AtomicU8 = AtomicU8::new(0);
static SMAP_HW_SUPPORTED: AtomicBool = AtomicBool::new(false);

static UACCESS_COPY_CALLS: AtomicU64 = AtomicU64::new(0);
static UACCESS_COPY_BYTES: AtomicU64 = AtomicU64::new(0);
static UACCESS_FIXUP_HITS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(isize)]
pub enum SysError {
    Perm = -1,
    BadCap = -9,
    Fault = -14,
    Busy = -16,
    Inval = -22,
    NoSys = -38,
    Revoked = -110,
}

impl SysError {
    #[inline]
    pub const fn as_raw(self) -> isize {
        self as isize
    }
}

#[derive(Clone, Copy, Debug)]
pub struct UaccessTelemetry {
    pub smap_supported: bool,
    pub smap_active: bool,
    pub copy_calls: u64,
    pub copy_bytes: u64,
    pub fixup_hits: u64,
}

global_asm!(
    ".global __dry_uaccess_memcpy",
    ".global __dry_uaccess_fault_start",
    ".global __dry_uaccess_fault_end",
    ".global __dry_uaccess_fault_fixup",
    ".balign 16",
    "__dry_uaccess_memcpy:",
    "    xor eax, eax",
    "    test rdx, rdx",
    "    jz .Luaccess_done_nostac",
    "    cmp byte ptr [rip + DRY_SMAP_ACTIVE], 0",
    "    je .Luaccess_body",
    "    .byte 0x0f, 0x01, 0xcb", // stac
    ".Luaccess_body:",
    "    mov rcx, rdx",
    "    shr rcx, 3",
    "__dry_uaccess_fault_start:",
    "    rep movsq",
    "    mov rcx, rdx",
    "    and rcx, 7",
    "    rep movsb",
    "__dry_uaccess_fault_end:",
    "    cmp byte ptr [rip + DRY_SMAP_ACTIVE], 0",
    "    je .Luaccess_done_nostac",
    "    .byte 0x0f, 0x01, 0xca", // clac
    ".Luaccess_done_nostac:",
    "    ret",
    ".balign 16",
    "__dry_uaccess_fault_fixup:",
    "    cmp byte ptr [rip + DRY_SMAP_ACTIVE], 0",
    "    je .Luaccess_fixup_out",
    "    .byte 0x0f, 0x01, 0xca", // clac
    ".Luaccess_fixup_out:",
    "    mov rax, -14",
    "    ret",
);

extern "C" {
    fn __dry_uaccess_memcpy(dst: *mut u8, src: *const u8, len: usize) -> isize;
    fn __dry_uaccess_fault_start();
    fn __dry_uaccess_fault_end();
    fn __dry_uaccess_fault_fixup();
}

/// Detect `CPUID.(EAX=7,ECX=0):EBX[20]` (`SMAP`) and enable `CR4.SMAP = 1` with `RFLAGS.AC = 0`
/// on the calling CPU. Safe on CPUs lacking `SMAP` (records `smap_active = false` without `#UD`).
pub fn enable_smap_on_current_cpu() -> bool {
    let has_smap = {
        let l0 = __cpuid(0);
        if l0.eax >= 7 {
            (__cpuid_count(7, 0).ebx & (1 << 20)) != 0
        } else {
            false
        }
    };
    if !has_smap {
        return false;
    }
    // Clear `RFLAGS.AC` prior to enabling `CR4.SMAP` so supervisor access to `U/S=1` is locked immediately.
    // SAFETY: Clearing `EFLAGS.AC` and setting CPUID-verified `CR4.SMAP` at CPL=0 is architecturally valid.
    unsafe {
        asm!(
            "pushfq",
            "and qword ptr [rsp], {mask}",
            "popfq",
            mask = const !(1i64 << 18),
            options(nomem),
        );
        write_cr4(read_cr4() | CR4_SMAP);
    }
    assert_ne!(read_cr4() & CR4_SMAP, 0, "CR4.SMAP failed to latch");
    assert_eq!(read_rflags() & RFLAGS_AC, 0, "RFLAGS.AC non-zero after SMAP init");
    SMAP_HW_SUPPORTED.store(true, Ordering::Release);
    DRY_SMAP_ACTIVE.store(1, Ordering::Release);
    true
}

#[inline]
pub fn is_smap_active() -> bool {
    DRY_SMAP_ACTIVE.load(Ordering::Acquire) != 0
}

pub fn uaccess_telemetry() -> UaccessTelemetry {
    UaccessTelemetry {
        smap_supported: SMAP_HW_SUPPORTED.load(Ordering::Acquire),
        smap_active: is_smap_active(),
        copy_calls: UACCESS_COPY_CALLS.load(Ordering::Acquire),
        copy_bytes: UACCESS_COPY_BYTES.load(Ordering::Acquire),
        fixup_hits: UACCESS_FIXUP_HITS.load(Ordering::Acquire),
    }
}

/// Invoked from the `#PF` handler (`vector == 14`) when `(frame.cs & 3) == 0`.
///
/// Redirects page faults occurring strictly inside `[__dry_uaccess_fault_start, __dry_uaccess_fault_end)`
/// to `__dry_uaccess_fault_fixup` with `RFLAGS.AC` cleared, returning `true` if handled.
#[inline]
pub fn try_fixup_uaccess_pf(frame: &mut TrapFrame) -> bool {
    if (frame.cs & 3) != 0 {
        return false;
    }
    let start = __dry_uaccess_fault_start as *const () as usize as u64;
    let end = __dry_uaccess_fault_end as *const () as usize as u64;
    if frame.rip >= start && frame.rip < end {
        frame.rip = __dry_uaccess_fault_fixup as *const () as usize as u64;
        frame.rflags &= !RFLAGS_AC;
        UACCESS_FIXUP_HITS.fetch_add(1, Ordering::Relaxed);
        return true;
    }
    false
}

/// Validate canonical lower-half user address range `[user_ptr .. user_ptr + len)`.
#[inline]
pub fn validate_user_range_bounds(user_ptr: u64, len: usize) -> Result<(), SysError> {
    if len > MAX_UACCESS_BYTES {
        return Err(SysError::Inval);
    }
    if !is_canonical_48(user_ptr) || !(USER_VADDR_MIN..USER_VADDR_LIMIT).contains(&user_ptr) {
        return Err(SysError::Fault);
    }
    if len == 0 {
        return Ok(());
    }
    let end = user_ptr
        .checked_add(len as u64)
        .ok_or(SysError::Fault)?;
    if end > USER_VADDR_LIMIT || !is_canonical_48(end - 1) {
        return Err(SysError::Fault);
    }
    Ok(())
}

/// Reject any `PTE_PRESENT` supervisor-only (`U/S == 0`) mapping in `[user_ptr .. user_ptr + len)`
/// so `CPL=0` `rep movsq` can never act as a confused deputy on a `SUPERVISOR_LOWER_*` page, while
/// leaving unmapped (`P == 0`) and `USER_RO` (`R/W == 0`) faults to hardware MMU + `extable` fixup.
fn reject_supervisor_only_lower_pages(user_ptr: u64, len: usize) -> Result<(), SysError> {
    let vs_id = cpu_active_vspace(current_cpu_idx());
    if vs_id == KERNEL_VSPACE_ID || (vs_id as usize) >= MAX_VSPACES {
        return Err(SysError::Fault);
    }
    if len == 0 {
        return Ok(());
    }
    let slot = &VSPACE_TABLE[vs_id as usize];
    if !slot.allocated.load(Ordering::Acquire) {
        return Err(SysError::Fault);
    }
    let pml4_pfn = slot.pml4_pfn.load(Ordering::Acquire);
    let end = user_ptr + (len as u64);
    let mut cur = user_ptr & !(PAGE_SIZE_4K - 1);

    // SAFETY: Current CPU holds bit `(1 << cpu_idx)` in `slot.active_cpus`, preventing `destroy_vspace`
    // from freeing any page-table frame of `vs_id` during this lock-free read-only walk.
    unsafe {
        while cur < end {
            let (i4, i3, i2, i1) = pt_indices(cur);
            let e4 = read_volatile(pt_virt_ptr(pml4_pfn).add(i4));
            if (e4 & PTE_PRESENT) == 0 {
                cur += PAGE_SIZE_4K;
                continue;
            }
            if (e4 & PTE_USER) == 0 {
                return Err(SysError::Fault);
            }
            let e3 = read_volatile(pt_virt_ptr(((e4 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32).add(i3));
            if (e3 & PTE_PRESENT) == 0 {
                cur += PAGE_SIZE_4K;
                continue;
            }
            if (e3 & PTE_USER) == 0 {
                return Err(SysError::Fault);
            }
            let e2 = read_volatile(pt_virt_ptr(((e3 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32).add(i2));
            if (e2 & PTE_PRESENT) == 0 {
                cur += PAGE_SIZE_4K;
                continue;
            }
            if (e2 & PTE_USER) == 0 {
                return Err(SysError::Fault);
            }
            if (e2 & PTE_HUGE_PAGE) != 0 {
                cur = (cur & !(PAGE_SIZE_2M - 1)) + PAGE_SIZE_2M;
                continue;
            }
            let leaf = read_volatile(pt_virt_ptr(((e2 & PTE_PHYS_MASK_4K) >> PAGE_SHIFT) as u32).add(i1));
            if (leaf & PTE_PRESENT) != 0 && (leaf & PTE_USER) == 0 {
                return Err(SysError::Fault);
            }
            cur += PAGE_SIZE_4K;
        }
    }
    Ok(())
}

/// Safely copy `dst.len()` bytes from untrusted user virtual address `user_src` into kernel `dst`.
pub fn copy_from_user(dst: &mut [u8], user_src: u64) -> Result<(), SysError> {
    validate_user_range_bounds(user_src, dst.len())?;
    let kptr = dst.as_mut_ptr() as u64;
    if kptr < KERNEL_HALF_BASE {
        return Err(SysError::Inval);
    }
    reject_supervisor_only_lower_pages(user_src, dst.len())?;
    if dst.is_empty() {
        return Ok(());
    }

    // SAFETY: `dst` is valid higher-half kernel slice; `user_src` is canonical user address guarded
    // by `STAC`/`CLAC` and `#PF` exception-table fixup (`__dry_uaccess_fault_fixup`).
    let rc = unsafe { __dry_uaccess_memcpy(dst.as_mut_ptr(), user_src as *const u8, dst.len()) };
    debug_assert_eq!(read_rflags() & RFLAGS_AC, 0, "RFLAGS.AC leaked after copy_from_user");
    if rc == 0 {
        UACCESS_COPY_CALLS.fetch_add(1, Ordering::Relaxed);
        UACCESS_COPY_BYTES.fetch_add(dst.len() as u64, Ordering::Relaxed);
        Ok(())
    } else {
        Err(SysError::Fault)
    }
}

/// Safely copy `src.len()` bytes from kernel `src` into untrusted user virtual address `user_dst`.
pub fn copy_to_user(user_dst: u64, src: &[u8]) -> Result<(), SysError> {
    validate_user_range_bounds(user_dst, src.len())?;
    let kptr = src.as_ptr() as u64;
    if kptr < KERNEL_HALF_BASE {
        return Err(SysError::Inval);
    }
    reject_supervisor_only_lower_pages(user_dst, src.len())?;
    if src.is_empty() {
        return Ok(());
    }

    // SAFETY: `src` is valid higher-half kernel slice; `user_dst` is canonical user address guarded
    // by `CR0.WP=1`, `STAC`/`CLAC`, and `#PF` exception-table fixup (`__dry_uaccess_fault_fixup`).
    let rc = unsafe { __dry_uaccess_memcpy(user_dst as *mut u8, src.as_ptr(), src.len()) };
    debug_assert_eq!(read_rflags() & RFLAGS_AC, 0, "RFLAGS.AC leaked after copy_to_user");
    if rc == 0 {
        UACCESS_COPY_CALLS.fetch_add(1, Ordering::Relaxed);
        UACCESS_COPY_BYTES.fetch_add(src.len() as u64, Ordering::Relaxed);
        Ok(())
    } else {
        Err(SysError::Fault)
    }
}
