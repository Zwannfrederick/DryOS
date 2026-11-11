// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Minimal Static `ET_EXEC` `x86-64` ELF64 Loader & Guard-Paged User Stack Builder
//! (`Milestone 1B-D` Stage D).
//!
//! # Explicit Scope & Security Contract
//! - Supports static 64-bit little-endian `ET_EXEC` (`EM_X86_64`) executables only (dynamic linking,
//!   `PT_INTERP`, and `ET_DYN` relocations belong to Phase 2 userspace `ld.so` / process server).
//! - Strictly enforces `W^X` (`PF_W | PF_X` rejected), canonical lower-half bounds, non-overlapping
//!   page mappings, `.bss` (`p_memsz > p_filesz`) zeroing, and `CSpace`-owned `Frame4K` allocation.

use core::mem::size_of;
use core::ptr::copy_nonoverlapping;

use crate::cspace::cap::{CSpaceId, CapError, CapPtr};
use crate::cspace::objects::{cspace_alloc_frame, cspace_vspace_map_4k};
use crate::pmm::{PageOrder, PAGE_SIZE};
use crate::security::{current_lease_epoch, SecLabel};
use crate::vspace::pte::{validate_user_range, VmProt, PAGE_SIZE_4K};
use crate::vspace::VSpaceHandle;

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EV_CURRENT: u8 = 1;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;
const PT_LOAD: u32 = 1;

const PF_X: u32 = 1 << 0;
const PF_W: u32 = 1 << 1;
const PF_R: u32 = 1 << 2;

pub const MAX_PHDRS: usize = 16;

/// Unmapped 4 KiB lower guard page (`0x0000_007f_fff0_0000`).
pub const USER_STACK_LOWER_GUARD: u64 = 0x0000_007f_fff0_0000;
/// 16 KiB (`4 * 4 KiB`) `USER_RW | NX` user stack (`0x0000_007f_fff0_1000 .. 0x0000_007f_fff0_5000`).
pub const USER_STACK_BASE: u64 = USER_STACK_LOWER_GUARD + PAGE_SIZE_4K;
pub const USER_STACK_PAGES: u64 = 4;
pub const USER_STACK_TOP_PAGE_END: u64 = USER_STACK_BASE + USER_STACK_PAGES * PAGE_SIZE_4K;
/// Initial 16-byte-aligned CPL=3 `RSP` (`0x0000_007f_fff0_4ff0`), bounded above by unmapped guard page `0x0000_007f_fff0_5000`.
pub const USER_STACK_INITIAL_RSP: u64 = USER_STACK_TOP_PAGE_END - 16;

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Ehdr {
    e_ident: [u8; 16],
    e_type: u16,
    e_machine: u16,
    e_version: u32,
    e_entry: u64,
    e_phoff: u64,
    e_shoff: u64,
    e_flags: u32,
    e_ehsize: u16,
    e_phentsize: u16,
    e_phnum: u16,
    e_shentsize: u16,
    e_shnum: u16,
    e_shstrndx: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Phdr {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

const _: () = assert!(size_of::<Elf64Ehdr>() == 64);
const _: () = assert!(size_of::<Elf64Phdr>() == 56);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElfLoadError {
    BadHeader,
    UnsupportedType,
    WxViolation,
    MalformedSegment,
    EntryOutsideExecSegment,
    StackCollision,
    CapOrMapFailed(CapError),
}

#[derive(Clone, Copy, Debug)]
pub struct LoadedUserImage {
    pub entry_rip: u64,
    pub initial_rsp: u64,
    pub mapped_pages: u64,
}

fn read_pod_unaligned<T: Copy>(bytes: &[u8], offset: usize) -> Option<T> {
    let end = offset.checked_add(size_of::<T>())?;
    if end > bytes.len() {
        return None;
    }
    // SAFETY: Bounds verified above; `read_unaligned` handles arbitrary byte alignment safely.
    Some(unsafe { core::ptr::read_unaligned(bytes.as_ptr().add(offset).cast::<T>()) })
}

/// Parse `elf_bytes`, allocate capability-bound `Frame4K`s in `cspace_id`, map `PT_LOAD` segments
/// (`W^X` enforced) + a guard-paged 16 KiB `USER_RW` stack into `vs_cap` (`vs_h`).
pub fn load_static_elf64_into_vspace(
    elf_bytes: &[u8],
    cspace_id: CSpaceId,
    subj_label: SecLabel,
    vs_cap: CapPtr,
    _vs_h: VSpaceHandle,
    lease_epoch: u32,
) -> Result<LoadedUserImage, ElfLoadError> {
    let ehdr: Elf64Ehdr = read_pod_unaligned(elf_bytes, 0).ok_or(ElfLoadError::BadHeader)?;
    if ehdr.e_ident[0..4] != ELF_MAGIC
        || ehdr.e_ident[4] != ELFCLASS64
        || ehdr.e_ident[5] != ELFDATA2LSB
        || ehdr.e_ident[6] != EV_CURRENT
        || ehdr.e_version != 1
    {
        return Err(ElfLoadError::BadHeader);
    }
    if ehdr.e_type != ET_EXEC || ehdr.e_machine != EM_X86_64 {
        return Err(ElfLoadError::UnsupportedType);
    }
    if (ehdr.e_phentsize as usize) != size_of::<Elf64Phdr>()
        || ehdr.e_phnum == 0
        || (ehdr.e_phnum as usize) > MAX_PHDRS
    {
        return Err(ElfLoadError::BadHeader);
    }

    let phoff = ehdr.e_phoff as usize;
    let phnum = ehdr.e_phnum as usize;
    phoff
        .checked_add(phnum * size_of::<Elf64Phdr>())
        .filter(|&end| end <= elf_bytes.len())
        .ok_or(ElfLoadError::BadHeader)?;

    let now_epoch = current_lease_epoch();
    let mut entry_in_exec = false;
    let mut mapped_pages = 0u64;

    for i in 0..phnum {
        let ph: Elf64Phdr =
            read_pod_unaligned(elf_bytes, phoff + i * size_of::<Elf64Phdr>())
                .ok_or(ElfLoadError::BadHeader)?;
        if ph.p_type != PT_LOAD || ph.p_memsz == 0 {
            continue;
        }
        if ph.p_filesz > ph.p_memsz || (ph.p_flags & PF_R) == 0 {
            return Err(ElfLoadError::MalformedSegment);
        }
        if (ph.p_flags & PF_W) != 0 && (ph.p_flags & PF_X) != 0 {
            return Err(ElfLoadError::WxViolation);
        }

        let file_end = ph
            .p_offset
            .checked_add(ph.p_filesz)
            .ok_or(ElfLoadError::MalformedSegment)?;
        if file_end > (elf_bytes.len() as u64) {
            return Err(ElfLoadError::MalformedSegment);
        }

        let seg_end = ph
            .p_vaddr
            .checked_add(ph.p_memsz)
            .ok_or(ElfLoadError::MalformedSegment)?;
        let page_start = ph.p_vaddr & !(PAGE_SIZE_4K - 1);
        let page_end = (seg_end + (PAGE_SIZE_4K - 1)) & !(PAGE_SIZE_4K - 1);
        validate_user_range(page_start, page_end - page_start, PAGE_SIZE_4K)
            .map_err(|_| ElfLoadError::MalformedSegment)?;

        // Forbid collision with `[USER_STACK_LOWER_GUARD .. USER_STACK_TOP_PAGE_END + 4K)`.
        if page_end > USER_STACK_LOWER_GUARD && page_start < (USER_STACK_TOP_PAGE_END + PAGE_SIZE_4K)
        {
            return Err(ElfLoadError::StackCollision);
        }

        let is_exec = (ph.p_flags & PF_X) != 0;
        let is_write = (ph.p_flags & PF_W) != 0;
        let prot = if is_exec {
            if ehdr.e_entry >= ph.p_vaddr && ehdr.e_entry < seg_end {
                entry_in_exec = true;
            }
            VmProt::USER_RX
        } else if is_write {
            VmProt::USER_RW
        } else {
            VmProt::USER_RO
        };

        let file_vaddr_end = ph.p_vaddr + ph.p_filesz;
        let mut page_va = page_start;
        while page_va < page_end {
            let (fr_cap, fh) = cspace_alloc_frame(
                cspace_id,
                subj_label,
                subj_label,
                PageOrder::Order4KiB,
                is_exec,
                lease_epoch,
            )
            .map_err(ElfLoadError::CapOrMapFailed)?;

            // Copy overlapping file payload bytes; `.bss` tail remains zeroed (`AllocFlags::ZERO`).
            let copy_va_start = page_va.max(ph.p_vaddr);
            let copy_va_end = (page_va + PAGE_SIZE_4K).min(file_vaddr_end);
            if copy_va_end > copy_va_start {
                let dst_off = (copy_va_start - page_va) as usize;
                let src_off = (ph.p_offset + (copy_va_start - ph.p_vaddr)) as usize;
                let nbytes = (copy_va_end - copy_va_start) as usize;
                debug_assert!(dst_off + nbytes <= PAGE_SIZE);
                // SAFETY: `fh` is freshly zeroed and exclusively owned; `src_off + nbytes <= elf_bytes.len()`.
                unsafe {
                    copy_nonoverlapping(
                        elf_bytes.as_ptr().add(src_off),
                        fh.hhdm_ptr().add(dst_off),
                        nbytes,
                    );
                }
            }

            cspace_vspace_map_4k(
                cspace_id,
                subj_label,
                vs_cap,
                fr_cap,
                page_va,
                prot,
                now_epoch,
            )
            .map_err(ElfLoadError::CapOrMapFailed)?;
            mapped_pages += 1;
            page_va += PAGE_SIZE_4K;
        }
    }

    if !entry_in_exec {
        return Err(ElfLoadError::EntryOutsideExecSegment);
    }

    // Allocate & map 16 KiB `USER_RW | NX` user stack flanked by unmapped 4 KiB lower & upper guard pages.
    for p in 0..USER_STACK_PAGES {
        let va = USER_STACK_BASE + p * PAGE_SIZE_4K;
        let (stk_cap, _) = cspace_alloc_frame(
            cspace_id,
            subj_label,
            subj_label,
            PageOrder::Order4KiB,
            false,
            lease_epoch,
        )
        .map_err(ElfLoadError::CapOrMapFailed)?;
        cspace_vspace_map_4k(
            cspace_id,
            subj_label,
            vs_cap,
            stk_cap,
            va,
            VmProt::USER_RW,
            now_epoch,
        )
        .map_err(ElfLoadError::CapOrMapFailed)?;
        mapped_pages += 1;
    }

    Ok(LoadedUserImage {
        entry_rip: ehdr.e_entry,
        initial_rsp: USER_STACK_INITIAL_RSP,
        mapped_pages,
    })
}
