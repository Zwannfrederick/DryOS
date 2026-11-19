// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! x86-64 4-Level Page-Table Entries (`PML4/PDPT/PD/PT`), Canonical/Alignment Validators, Default
//! `W^X` Protection Policy, and Hardware Control Register Capabilities (`CR0.WP`, `EFER.NXE`, `CR4`).

use core::arch::x86_64::{__cpuid, __cpuid_count};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::{
    rdmsr, read_cr0, read_cr4, wrmsr, write_cr0, write_cr4, MSR_IA32_EFER, MSR_IA32_PAT,
};

pub const PTE_PRESENT: u64 = 1 << 0;
pub const PTE_WRITABLE: u64 = 1 << 1;
pub const PTE_USER: u64 = 1 << 2;
pub const PTE_PWT: u64 = 1 << 3;
pub const PTE_PCD: u64 = 1 << 4;
pub const PTE_PAT_4K: u64 = 1 << 7;
pub const PTE_HUGE_PAGE: u64 = 1 << 7;
pub const PTE_PAT_2M: u64 = 1 << 12;
pub const PTE_GLOBAL: u64 = 1 << 8;
/// OS software AVL bit (`bit 9`, ignored by x86-64 MMU) marking capability-validated device MMIO leaf PTEs.
pub const PTE_SW_MMIO: u64 = 1 << 9;
pub const PTE_NX: u64 = 1 << 63;

/// Expected `IA32_PAT` MSR value: preserves `PA0=WB(06), PA1=WT(04), PA2=UC-(07), PA3=UC(00)` and
/// configures `PA4=WC(01)` so `PTE_PAT_4K=1, PCD=0, PWT=0` selects Write-Combining on 4 KiB MMIO.
const DRY_IA32_PAT_VALUE: u64 = 0x0007_0401_0007_0406;

pub const PTE_PHYS_MASK_4K: u64 = 0x000f_ffff_ffff_f000;
pub const PTE_PHYS_MASK_2M: u64 = 0x000f_ffff_ffe0_0000;

/// Intermediate user-half directory flags (`P | RW | US`, `NX = 0`) so leaf PTEs govern exact rights.
pub const PTE_USER_TABLE_INTERMEDIATE: u64 = PTE_PRESENT | PTE_WRITABLE | PTE_USER;
/// Intermediate kernel-half directory flags (`P | RW`, `US = 0`, `NX = 0`).
pub const PTE_KERNEL_TABLE_INTERMEDIATE: u64 = PTE_PRESENT | PTE_WRITABLE;

/// Unmapped 4 KiB NULL-dereference guard page (`[0x0 .. 0x1000)`).
pub const USER_VADDR_MIN: u64 = 0x0000_0000_0000_1000;
/// Exclusive upper bound of lower canonical user half (`PML4[0..256]`, `128 TiB`).
pub const USER_VADDR_LIMIT: u64 = 0x0000_8000_0000_0000;
/// Base of upper canonical kernel half (`PML4[256..512]`).
pub const KERNEL_HALF_BASE: u64 = 0xffff_8000_0000_0000;

pub const PAGE_SIZE_4K: u64 = 4_096;
pub const PAGE_SIZE_2M: u64 = 2 * 1024 * 1024;

const CR0_WP: u64 = 1 << 16;
const CR4_PGE: u64 = 1 << 7;
const CR4_PCIDE: u64 = 1 << 17;
const CR4_SMEP: u64 = 1 << 20;
const CR4_SMAP: u64 = 1 << 21;
const EFER_NXE: u64 = 1 << 11;

static PCID_ENABLED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VmProt(pub u8);

impl VmProt {
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const EXEC: Self = Self(1 << 2);
    pub const USER: Self = Self(1 << 3);
    pub const UNCACHED: Self = Self(1 << 4);
    pub const GLOBAL: Self = Self(1 << 5);
    pub const WRITE_COMBINING: Self = Self(1 << 6);

    pub const USER_RO: Self = Self(Self::READ.0 | Self::USER.0);
    pub const USER_RW: Self = Self(Self::READ.0 | Self::WRITE.0 | Self::USER.0);
    pub const USER_RX: Self = Self(Self::READ.0 | Self::EXEC.0 | Self::USER.0);
    pub const USER_MMIO_UC: Self =
        Self(Self::READ.0 | Self::WRITE.0 | Self::USER.0 | Self::UNCACHED.0);
    pub const USER_MMIO_WC: Self =
        Self(Self::READ.0 | Self::WRITE.0 | Self::USER.0 | Self::WRITE_COMBINING.0);
    pub const USER_WX_FORBIDDEN: Self =
        Self(Self::READ.0 | Self::WRITE.0 | Self::EXEC.0 | Self::USER.0);

    pub const KERNEL_RO: Self = Self(Self::READ.0 | Self::GLOBAL.0);
    pub const KERNEL_RW: Self = Self(Self::READ.0 | Self::WRITE.0 | Self::GLOBAL.0);
    pub const KERNEL_RX: Self = Self(Self::READ.0 | Self::EXEC.0 | Self::GLOBAL.0);
    pub const KERNEL_MMIO_UC: Self =
        Self(Self::READ.0 | Self::WRITE.0 | Self::UNCACHED.0 | Self::GLOBAL.0);

    /// Supervisor-only (`U/S = 0`) lower-half test mappings used to verify `U/S` & Ring-0 `NX` without `SMEP` masking.
    pub const SUPERVISOR_LOWER_RO: Self = Self::READ;
    pub const SUPERVISOR_LOWER_RX: Self = Self(Self::READ.0 | Self::EXEC.0);

    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VSpaceError {
    NonCanonicalAddress,
    UnalignedAddress,
    AddressOverflow,
    NullGuardPage,
    UserRangeViolation,
    WxViolation,
    PermissionViolation,
    AlreadyMapped,
    NotMapped,
    PageSizeMismatch,
    OutOfMemory,
    InvalidVSpace,
    VSpaceActiveBusy,
    BootTableStillActive,
}

#[derive(Clone, Copy, Debug)]
pub struct PagingHwCaps {
    pub has_pcid: bool,
    pub has_invpcid: bool,
    pub wp_enabled: bool,
    pub nxe_enabled: bool,
    pub smep_enabled: bool,
}

/// Check 48-bit canonical sign extension (`bits[63:48]` must replicate `bit[47]`).
#[inline]
pub const fn is_canonical_48(vaddr: u64) -> bool {
    (((vaddr as i64) << 16) >> 16) as u64 == vaddr
}

/// Extract 9-bit indices `(i4, i3, i2, i1)` for `PML4`, `PDPT`, `PD`, and `PT`.
#[inline]
pub const fn pt_indices(vaddr: u64) -> (usize, usize, usize, usize) {
    (
        ((vaddr >> 39) & 0x1ff) as usize,
        ((vaddr >> 30) & 0x1ff) as usize,
        ((vaddr >> 21) & 0x1ff) as usize,
        ((vaddr >> 12) & 0x1ff) as usize,
    )
}

/// Validate that `[vaddr .. vaddr + size_bytes)` is canonical, aligned to `align_bytes` (power of 2),
/// overflow-free, above the 4 KiB NULL guard page, and strictly within `PML4[0..256]`.
pub fn validate_user_range(
    vaddr: u64,
    size_bytes: u64,
    align_bytes: u64,
) -> Result<(), VSpaceError> {
    debug_assert!(align_bytes.is_power_of_two());
    if !is_canonical_48(vaddr) {
        return Err(VSpaceError::NonCanonicalAddress);
    }
    if (vaddr & (align_bytes - 1)) != 0 {
        return Err(VSpaceError::UnalignedAddress);
    }
    if size_bytes == 0 {
        return Err(VSpaceError::AddressOverflow);
    }
    let end = vaddr
        .checked_add(size_bytes)
        .ok_or(VSpaceError::AddressOverflow)?;
    if !is_canonical_48(end.saturating_sub(1)) {
        return Err(VSpaceError::NonCanonicalAddress);
    }
    if vaddr < USER_VADDR_MIN {
        return Err(VSpaceError::NullGuardPage);
    }
    if end > USER_VADDR_LIMIT {
        return Err(VSpaceError::UserRangeViolation);
    }
    Ok(())
}

/// Translate `VmProt` into x86-64 leaf PTE/PDE bits while enforcing default **`W^X`**.
pub fn prot_to_pte_flags(prot: VmProt, is_huge_2m: bool) -> Result<u64, VSpaceError> {
    if !prot.contains(VmProt::READ) {
        return Err(VSpaceError::PermissionViolation);
    }
    // Default W^X invariant (ADR-007 / Milestone 1B-B): simultaneous WRITE + EXEC is rejected.
    if prot.contains(VmProt::WRITE) && prot.contains(VmProt::EXEC) {
        return Err(VSpaceError::WxViolation);
    }
    // User mappings must NEVER set `PTE_GLOBAL` (`G=1` would survive non-PCID `CR3` switches).
    if prot.contains(VmProt::USER) && prot.contains(VmProt::GLOBAL) {
        return Err(VSpaceError::PermissionViolation);
    }
    if prot.contains(VmProt::UNCACHED) && prot.contains(VmProt::WRITE_COMBINING) {
        return Err(VSpaceError::PermissionViolation);
    }

    let mut flags = PTE_PRESENT;
    if prot.contains(VmProt::WRITE) {
        flags |= PTE_WRITABLE;
    }
    if prot.contains(VmProt::USER) {
        flags |= PTE_USER;
    }
    if !prot.contains(VmProt::EXEC) {
        flags |= PTE_NX;
    }
    if prot.contains(VmProt::UNCACHED) {
        flags |= PTE_PCD | PTE_PWT;
    } else if prot.contains(VmProt::WRITE_COMBINING) {
        // Selects PA4 (`WC = 01`) programmed into `IA32_PAT`: bit 7 on 4 KiB PTE, bit 12 on 2 MiB PDE.PS.
        flags |= if is_huge_2m { PTE_PAT_2M } else { PTE_PAT_4K };
    }
    if prot.contains(VmProt::GLOBAL) {
        flags |= PTE_GLOBAL;
    }
    if is_huge_2m {
        flags |= PTE_HUGE_PAGE;
    }
    Ok(flags)
}

#[inline]
pub fn cpu_max_phys_bits() -> u8 {
    let ext0 = __cpuid(0x8000_0000);
    if ext0.eax >= 0x8000_0008 {
        let bits = (__cpuid(0x8000_0008).eax & 0xff) as u8;
        bits.clamp(32, 52)
    } else {
        36
    }
}

#[inline]
pub fn cpu_max_phys_addr_limit() -> u64 {
    let bits = cpu_max_phys_bits();
    if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

#[inline]
pub fn is_pcid_enabled() -> bool {
    PCID_ENABLED.load(Ordering::Relaxed)
}

/// Detect CPUID paging capabilities on the calling CPU and enforce:
/// - `IA32_EFER.NXE = 1` (`bit 11`, required before loading any `PTE_NX` entry),
/// - `CR0.WP = 1` (`bit 16`, enforces read-only protection even in Ring 0),
/// - `CR4.PGE = 1` (when supported) + `CR4.SMEP = 1` (when supported),
/// - `CR4.PCIDE = 0` (`PCID` explicitly kept off; zero unverified `PCID` assumptions).
pub fn detect_and_enable_cpu_paging_caps(cpu_idx: usize) -> PagingHwCaps {
    let (has_nx, has_pge, has_pat, has_smep, has_pcid, has_invpcid) = {
        let l0 = __cpuid(0);
        let l1 = __cpuid(1);
        let pge = (l1.edx & (1 << 13)) != 0;
        let pat = (l1.edx & (1 << 16)) != 0;
        let pcid = (l1.ecx & (1 << 17)) != 0;
        let (smep, invpcid) = if l0.eax >= 7 {
            let l7 = __cpuid_count(7, 0);
            ((l7.ebx & (1 << 7)) != 0, (l7.ebx & (1 << 10)) != 0)
        } else {
            (false, false)
        };
        let ext0 = __cpuid(0x8000_0000);
        let nx = if ext0.eax >= 0x8000_0001 {
            (__cpuid(0x8000_0001).edx & (1 << 20)) != 0
        } else {
            false
        };
        (nx, pge, pat, smep, pcid, invpcid)
    };

    assert!(
        has_nx,
        "CPU {} lacks mandatory NX capability (CPUID.80000001H:EDX[20])",
        cpu_idx
    );

    // SAFETY: Enabling `EFER.NXE`, `CR0.WP`, `CR4.PGE`, `CR4.SMEP`, and `IA32_PAT[4]=WC`
    // (while keeping `PCIDE=0` and `SMAP=0`) at CPL=0 after verifying CPUID support bits.
    let (wp_ok, nxe_ok, smep_ok, pcid_on) = unsafe {
        if has_pat {
            wrmsr(MSR_IA32_PAT, DRY_IA32_PAT_VALUE);
            let pat_rb = rdmsr(MSR_IA32_PAT);
            assert_eq!(
                (pat_rb >> 32) & 0xff,
                0x01,
                "IA32_PAT PA4=WC failed to latch on CPU {}",
                cpu_idx
            );
        }
        let efer = rdmsr(MSR_IA32_EFER);
        if (efer & EFER_NXE) == 0 {
            wrmsr(MSR_IA32_EFER, efer | EFER_NXE);
        }
        let efer_rb = rdmsr(MSR_IA32_EFER);

        let cr0 = read_cr0();
        if (cr0 & CR0_WP) == 0 {
            write_cr0(cr0 | CR0_WP);
        }
        let cr0_rb = read_cr0();

        let mut cr4 = read_cr4();
        cr4 &= !(CR4_PCIDE | CR4_SMAP);
        if has_pge {
            cr4 |= CR4_PGE;
        }
        if has_smep {
            cr4 |= CR4_SMEP;
        }
        write_cr4(cr4);
        let cr4_rb = read_cr4();

        (
            (cr0_rb & CR0_WP) != 0,
            (efer_rb & EFER_NXE) != 0,
            (cr4_rb & CR4_SMEP) != 0,
            (cr4_rb & CR4_PCIDE) != 0,
        )
    };

    assert!(
        wp_ok && nxe_ok && !pcid_on,
        "Failed to enforce CR0.WP/EFER.NXE/!PCIDE on CPU {}",
        cpu_idx
    );
    PCID_ENABLED.store(pcid_on, Ordering::Release);

    PagingHwCaps {
        has_pcid,
        has_invpcid,
        wp_enabled: wp_ok,
        nxe_enabled: nxe_ok,
        smep_enabled: smep_ok,
    }
}

/// Disjoint upper-half Supervisor MMIO slot (`PML4[385]`) shared across `KERNEL_VSPACE` and all
/// cloned user `VSpace`s for Ring-0 VT-d DRHD registers, PCI MSI-X tables, and VirtIO resets.
pub const KERNEL_MMIO_DYN_BASE: u64 = 0xffff_c080_0000_0000;
pub const KERNEL_MMIO_SLOT_STRIDE: u64 = 0x1_0000;
pub const KERNEL_VTD_MAX_PAGES: usize = 4;
pub const KERNEL_MSIX_MMIO_OFFSET: u64 = 0x0010_0000;
pub const KERNEL_MSIX_MAX_PAGES: usize = 4;
pub const KERNEL_VIRTIO_COMMON_MMIO_OFFSET: u64 = 0x0080_0000;
pub const KERNEL_ECAM_MMIO_OFFSET: u64 = 0x0100_0000;
pub const KERNEL_GOP_DIAG_BASE: u64 = KERNEL_MMIO_DYN_BASE + 0x0200_0000;

#[inline]
pub const fn vtd_unit_vaddr(unit_idx: usize) -> u64 {
    KERNEL_MMIO_DYN_BASE + (unit_idx as u64) * KERNEL_MMIO_SLOT_STRIDE
}

#[inline]
pub const fn pci_msix_table_vaddr(slot_idx: usize) -> u64 {
    KERNEL_MMIO_DYN_BASE + KERNEL_MSIX_MMIO_OFFSET + (slot_idx as u64) * KERNEL_MMIO_SLOT_STRIDE
}

#[inline]
pub const fn pci_virtio_common_vaddr(slot_idx: usize) -> u64 {
    KERNEL_MMIO_DYN_BASE
        + KERNEL_VIRTIO_COMMON_MMIO_OFFSET
        + (slot_idx as u64) * KERNEL_MMIO_SLOT_STRIDE
}

#[inline]
pub const fn pci_ecam_vaddr(slot_idx: usize) -> u64 {
    KERNEL_MMIO_DYN_BASE + KERNEL_ECAM_MMIO_OFFSET + (slot_idx as u64) * 4096
}
