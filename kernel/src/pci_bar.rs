// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Safe 32-Bit & 64-Bit PCI Base Address Register (`BAR0..=BAR5`) Discovery, Bounds Validation,
//! `> 4 GiB` Physical Address Reconstruction, Cache Attribute Enforcement (`UC`/`WC`), and
//! Bounded Userspace Mapping Descriptors (`Milestone 2C-2`).

use core::mem::size_of;

use crate::pmm::{query_frame_state, FrameState, PAGE_SIZE};
use crate::serial_println;
use crate::vspace::pte::cpu_max_phys_addr_limit;

pub const MAX_WINDOW_PAGES: usize = 64; // Max 256 KiB per bounded `SYS_DEV_MAP` window call.

pub const DEV_MAP_OP_MASK: u64 = 0xfff;
pub const DEV_MAP_OP_LEGACY_VIRTIO: u64 = 0;
pub const DEV_MAP_OP_QUERY_BARS: u64 = 1;
pub const DEV_MAP_OP_MAP_WINDOW: u64 = 2;
pub const DEV_MAP_OP_CFG_RW: u64 = 3;
pub const DEV_MAP_OP_MAP_WINDOW_2M: u64 = 4;
pub const DEV_MAP_OP_ENUM_MINT: u64 = 5;

pub const PCI_BAR_FLAG_MMIO: u32 = 1 << 0;
pub const PCI_BAR_FLAG_64BIT: u32 = 1 << 1;
pub const PCI_BAR_FLAG_PREFETCHABLE: u32 = 1 << 2;
pub const PCI_BAR_FLAG_ABOVE_4G: u32 = 1 << 3;
pub const PCI_BAR_FLAG_IO: u32 = 1 << 4;

pub const PCI_DEV_FLAG_IOMMU_ACTIVE: u16 = 1 << 0;
pub const PCI_DEV_FLAG_MSI: u16 = 1 << 1;
pub const PCI_DEV_FLAG_MSIX: u16 = 1 << 2;
pub const PCI_DEV_FLAG_VIRTIO: u16 = 1 << 3;
pub const PCI_DEV_FLAG_PCIE: u16 = 1 << 4;
pub const PCI_DEV_FLAG_ECAM: u16 = 1 << 5;
pub const PCI_DEV_FLAG_FLR: u16 = 1 << 6;
pub const PCI_DEV_FLAG_REBAR: u16 = 1 << 7;
pub const PCI_DEV_FLAG_ACS: u16 = 1 << 8;
pub const PCI_DEV_FLAG_AER: u16 = 1 << 9;
pub const PCI_DEV_FLAG_SRIOV: u16 = 1 << 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PciBarKind {
    Unused,
    IoPort,
    Mem32,
    Mem64,
    UpperHalfOf64,
    Malformed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PciBarError {
    Malformed64AtSlot5,
    ReservedMemType,
    InvalidSize,
    UnassignedBase,
    UnalignedBase,
    Overflow,
    ExceedsMaxPhysAddr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciBarDesc {
    pub kind: PciBarKind,
    pub base_phys: u64,
    pub size_bytes: u64,
    pub prefetchable: bool,
    pub above_4g: bool,
}

impl PciBarDesc {
    pub const UNUSED: Self = Self {
        kind: PciBarKind::Unused,
        base_phys: 0,
        size_bytes: 0,
        prefetchable: false,
        above_4g: false,
    };

    #[inline]
    pub const fn is_mmio(&self) -> bool {
        matches!(self.kind, PciBarKind::Mem32 | PciBarKind::Mem64)
    }
}

/// Sanitized per-BAR descriptor returned to Ring 3 by `SYS_DEV_MAP(QUERY_BARS)` (`16` bytes, zero phys addrs).
#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub struct UserPciBarInfo {
    pub size_bytes: u64,
    pub flags: u32,
    pub page_suboff: u16,
    pub _pad: u16,
}

const _: () = assert!(size_of::<UserPciBarInfo>() == 16);

/// Sanitized generic PCI capability & BAR summary copied to Ring 3 (`112` bytes, zero phys addrs).
#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub struct UserPciDeviceInfo {
    pub vendor_id: u16,
    pub device_id: u16,
    pub bus_dev_fn: u16,
    pub dev_flags: u16,
    pub msix_table_size: u16,
    pub msi_cap_present: u8,
    pub iommu_protected: u8,
    pub iommu_faults: u32,
    pub bars: [UserPciBarInfo; 6],
}

const _: () = assert!(size_of::<UserPciDeviceInfo>() == 112);

/// Decode one PCI BAR slot (`raw_lo`, `size_mask_lo`) paired with optional upper 32-bit slot
/// (`raw_hi`, `size_mask_hi`) against `phys_limit`, enforcing 32-bit/64-bit pairing, power-of-two
/// size, alignment, overflow safety, and CPU `MAXPHYADDR` bounds.
/// Returns `(decoded_bar, consumed_upper_slot)`.
pub fn decode_bar_pair(
    bar_idx: usize,
    raw_lo: u32,
    size_mask_lo: u32,
    raw_hi: u32,
    size_mask_hi: u32,
    phys_limit: u64,
) -> Result<(PciBarDesc, bool), PciBarError> {
    if (raw_lo & 1) != 0 {
        let port_base = (raw_lo & !0x3) as u64;
        let port_mask = (size_mask_lo & !0x3) | 0xffff_0000;
        let size = if (size_mask_lo & !0x3) == 0 {
            0
        } else {
            ((!port_mask).wrapping_add(1)) as u64
        };
        if port_base == 0 && size == 0 {
            return Ok((PciBarDesc::UNUSED, false));
        }
        return Ok((
            PciBarDesc {
                kind: PciBarKind::IoPort,
                base_phys: port_base,
                size_bytes: size,
                prefetchable: false,
                above_4g: false,
            },
            false,
        ));
    }

    let mem_type = (raw_lo >> 1) & 0x3;
    let prefetchable = (raw_lo & (1 << 3)) != 0;

    match mem_type {
        0 => {
            // 32-bit Memory BAR
            if raw_lo == 0 && (size_mask_lo & !0xf) == 0 {
                return Ok((PciBarDesc::UNUSED, false));
            }
            let base = (raw_lo & !0xf) as u64;
            let mask_lo_bits = size_mask_lo & !0xf;
            if mask_lo_bits == 0 {
                return Err(PciBarError::InvalidSize);
            }
            let mask64 = 0xffff_ffff_0000_0000u64 | (mask_lo_bits as u64);
            let size = (!mask64).wrapping_add(1);
            validate_decoded_mmio_bar(PciBarKind::Mem32, base, size, prefetchable, phys_limit)
                .map(|d| (d, false))
        }
        2 => {
            // 64-bit Memory BAR consuming `bar_idx` and `bar_idx + 1`
            if bar_idx >= 5 {
                return Err(PciBarError::Malformed64AtSlot5);
            }
            if raw_lo == 0 && raw_hi == 0 && (size_mask_lo & !0xf) == 0 && size_mask_hi == 0 {
                return Ok((PciBarDesc::UNUSED, true));
            }
            let base = ((raw_hi as u64) << 32) | ((raw_lo & !0xf) as u64);
            let mask64 = ((size_mask_hi as u64) << 32) | ((size_mask_lo & !0xf) as u64);
            if mask64 == 0 {
                return Err(PciBarError::InvalidSize);
            }
            let size = (!mask64).wrapping_add(1);
            validate_decoded_mmio_bar(PciBarKind::Mem64, base, size, prefetchable, phys_limit)
                .map(|d| (d, true))
        }
        _ => Err(PciBarError::ReservedMemType),
    }
}

fn validate_decoded_mmio_bar(
    kind: PciBarKind,
    base_phys: u64,
    size_bytes: u64,
    prefetchable: bool,
    phys_limit: u64,
) -> Result<PciBarDesc, PciBarError> {
    if size_bytes < 16 || !size_bytes.is_power_of_two() {
        return Err(PciBarError::InvalidSize);
    }
    if base_phys == 0 {
        return Err(PciBarError::UnassignedBase);
    }
    if (base_phys & ((PAGE_SIZE as u64) - 1)) != 0 && size_bytes >= (PAGE_SIZE as u64) {
        return Err(PciBarError::UnalignedBase);
    }
    let end = base_phys
        .checked_add(size_bytes)
        .ok_or(PciBarError::Overflow)?;
    if end - 1 > phys_limit {
        return Err(PciBarError::ExceedsMaxPhysAddr);
    }
    Ok(PciBarDesc {
        kind,
        base_phys,
        size_bytes,
        prefetchable,
        above_4g: end > 0x0000_0001_0000_0000,
    })
}

/// Verify a candidate MMIO BAR's endpoints belong to `ReservedOrMmio` (never usable/kernel RAM).
pub fn verify_bar_non_ram(desc: PciBarDesc) -> bool {
    if !desc.is_mmio() {
        return true;
    }
    let page_sz = PAGE_SIZE as u64;
    if desc.size_bytes < page_sz {
        return query_frame_state(desc.base_phys & !(page_sz - 1)) == FrameState::ReservedOrMmio;
    }
    let first = desc.base_phys;
    let mid = desc.base_phys + ((desc.size_bytes / 2) & !(page_sz - 1));
    let last = desc.base_phys + desc.size_bytes - page_sz;
    query_frame_state(first) == FrameState::ReservedOrMmio
        && query_frame_state(mid) == FrameState::ReservedOrMmio
        && query_frame_state(last) == FrameState::ReservedOrMmio
}

/// Probe all 6 PCI BAR slots for `(bus, dev, func)` with Memory/I/O decode safely masked while
/// writing `0xffff_ffff` size probes, reconstructing 64-bit >4GiB BARs and bounds-checking all ranges.
pub fn probe_function_bars(bus: u8, dev: u8, func: u8) -> [PciBarDesc; 6] {
    use crate::pci::pci_cfg_probe_all_bars_raw;
    let phys_limit = cpu_max_phys_addr_limit();
    let (raw, masks) = pci_cfg_probe_all_bars_raw(bus, dev, func);

    let mut out = [PciBarDesc::UNUSED; 6];
    let mut idx = 0usize;
    while idx < 6 {
        let raw_hi = if idx + 1 < 6 { raw[idx + 1] } else { 0 };
        let mask_hi = if idx + 1 < 6 { masks[idx + 1] } else { 0 };
        match decode_bar_pair(idx, raw[idx], masks[idx], raw_hi, mask_hi, phys_limit) {
            Ok((mut desc, consumed_next)) => {
                if desc.is_mmio() && !verify_bar_non_ram(desc) {
                    desc.kind = PciBarKind::Malformed;
                }
                out[idx] = desc;
                if consumed_next && idx + 1 < 6 {
                    out[idx + 1] = PciBarDesc {
                        kind: if desc.kind == PciBarKind::Unused {
                            PciBarKind::Unused
                        } else {
                            PciBarKind::UpperHalfOf64
                        },
                        ..PciBarDesc::UNUSED
                    };
                    idx += 2;
                    continue;
                }
            }
            Err(_) => {
                out[idx] = PciBarDesc {
                    kind: PciBarKind::Malformed,
                    ..PciBarDesc::UNUSED
                };
                // If BAR0..4 was a malformed 64-bit BAR, skip its paired upper slot cleanly.
                if (raw[idx] & 1) == 0 && ((raw[idx] >> 1) & 0x3) == 2 && idx + 1 < 6 {
                    out[idx + 1] = PciBarDesc {
                        kind: PciBarKind::Malformed,
                        ..PciBarDesc::UNUSED
                    };
                    idx += 2;
                    continue;
                }
            }
        }
        idx += 1;
    }
    out
}

pub fn build_user_pci_info(
    vendor_id: u16,
    device_id: u16,
    bus_dev_fn: u16,
    dev_flags: u16,
    msix_table_size: u16,
    bars: &[PciBarDesc; 6],
) -> UserPciDeviceInfo {
    let mut u_bars = [UserPciBarInfo::default(); 6];
    for (dst, src) in u_bars.iter_mut().zip(bars.iter()) {
        let mut flags = 0u32;
        if src.is_mmio() {
            flags |= PCI_BAR_FLAG_MMIO;
        }
        if src.kind == PciBarKind::Mem64 {
            flags |= PCI_BAR_FLAG_64BIT;
        }
        if src.kind == PciBarKind::IoPort {
            flags |= PCI_BAR_FLAG_IO;
        }
        if src.prefetchable {
            flags |= PCI_BAR_FLAG_PREFETCHABLE;
        }
        if src.above_4g {
            flags |= PCI_BAR_FLAG_ABOVE_4G;
        }
        dst.size_bytes = src.size_bytes;
        dst.flags = flags;
        dst.page_suboff = if src.is_mmio() {
            (src.base_phys & 0xfff) as u16
        } else {
            0
        };
    }
    UserPciDeviceInfo {
        vendor_id,
        device_id,
        bus_dev_fn,
        dev_flags,
        msix_table_size,
        msi_cap_present: ((dev_flags & PCI_DEV_FLAG_MSI) != 0) as u8,
        iommu_protected: ((dev_flags & PCI_DEV_FLAG_IOMMU_ACTIVE) != 0) as u8,
        iommu_faults: 0,
        bars: u_bars,
    }
}

/// Deterministic kernel unit tests for 32-bit, 64-bit >4GiB, malformed, overflow, and MAXPHYADDR BAR checks.
#[inline(never)]
pub fn run_bar64_unit_tests() {
    let limit_40 = (1u64 << 40) - 1;
    let limit_48 = (1u64 << 48) - 1;

    // 1. Valid 32-bit MMIO BAR (1 MiB)
    let (d1, c1) = decode_bar_pair(0, 0xFE00_0000, 0xFFF0_0000, 0, 0, limit_40).expect("valid bar32");
    assert_eq!(d1.kind, PciBarKind::Mem32);
    assert_eq!(d1.base_phys, 0xFE00_0000);
    assert_eq!(d1.size_bytes, 1024 * 1024);
    assert!(!d1.above_4g && !c1);

    // 2. Valid 64-bit prefetchable BAR above 4 GiB (256 MiB at 0x8_4000_0000)
    let (d2, c2) = decode_bar_pair(
        2,
        0x4000_000C,
        0xF000_000C,
        0x0000_0008,
        0xFFFF_FFFF,
        limit_40,
    )
    .expect("valid bar64 >4G");
    assert_eq!(d2.kind, PciBarKind::Mem64);
    assert_eq!(d2.base_phys, 0x0000_0008_4000_0000);
    assert_eq!(d2.size_bytes, 256 * 1024 * 1024);
    assert!(d2.prefetchable && d2.above_4g && c2);

    // 3. Valid huge 64-bit BAR >= 4 GiB aperture size (8 GiB at 0x10_0000_0000) on 48-bit host
    let (d3, c3) = decode_bar_pair(
        2,
        0x0000_000C,
        0x0000_000C,
        0x0000_0010,
        0xFFFF_FFFE,
        limit_48,
    )
    .expect("valid 8GiB bar64");
    assert_eq!(d3.size_bytes, 8u64 << 30);
    assert!(c3);

    // 4. Reject malformed 64-bit BAR starting at BAR5
    assert_eq!(
        decode_bar_pair(5, 0x0000_0004, 0xFFFF_F004, 0, 0, limit_48),
        Err(PciBarError::Malformed64AtSlot5)
    );

    // 5. Reject reserved MMIO type (1 and 3)
    assert_eq!(
        decode_bar_pair(0, 0xFE00_0002, 0xFFFF_F002, 0, 0, limit_48),
        Err(PciBarError::ReservedMemType)
    );
    assert_eq!(
        decode_bar_pair(0, 0xFE00_0006, 0xFFFF_F006, 0, 0, limit_48),
        Err(PciBarError::ReservedMemType)
    );

    // 6. Reject u64 arithmetic overflow near u64::MAX
    assert_eq!(
        decode_bar_pair(
            0,
            0xFFF0_0004,
            0xFFF0_0004,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
            u64::MAX
        ),
        Err(PciBarError::Overflow)
    );

    // 7. Reject > MAXPHYADDR BAR on 40-bit host
    assert_eq!(
        decode_bar_pair(
            2,
            0x0000_0004,
            0xFFFF_F004,
            0x0000_0200,
            0xFFFF_FFFF,
            limit_40
        ),
        Err(PciBarError::ExceedsMaxPhysAddr)
    );

    // 8. Reject non-power-of-two corrupt size mask
    assert_eq!(
        decode_bar_pair(0, 0xFE00_0000, 0xFFFA_0000, 0, 0, limit_40),
        Err(PciBarError::InvalidSize)
    );

    // 9. Sub-page (< 4 KiB) MMIO BAR (e.g., 256-byte rtl8139 BAR1) preserves `page_suboff` while IoPort zeros it
    let (d_io, c_io) = decode_bar_pair(0, 0xC001, 0xFFFF_FF01, 0, 0, limit_40).expect("valid ioport bar");
    let (d9, c9) = decode_bar_pair(1, 0xFEBC_1200, 0xFFFF_FF00, 0, 0, limit_40).expect("valid subpage bar32");
    assert_eq!(d_io.kind, PciBarKind::IoPort);
    assert_eq!(d9.kind, PciBarKind::Mem32);
    assert_eq!(d9.base_phys, 0xFEBC_1200);
    assert_eq!(d9.size_bytes, 256);
    assert!(!c_io && !c9);
    let mut bars = [PciBarDesc::UNUSED; 6];
    bars[0] = d_io;
    bars[1] = d9;
    let uinfo = build_user_pci_info(0x10ec, 0x8139, 0x0018, 0, 0, &bars);
    assert_eq!(uinfo.bars[0].page_suboff, 0);
    assert_eq!(uinfo.bars[1].size_bytes, 256);
    assert_eq!(uinfo.bars[1].page_suboff, 0x200);
    assert_ne!(uinfo.bars[1].flags & PCI_BAR_FLAG_MMIO, 0);

    serial_println!("[DRY-BAR64] PASS: unit_cases=9 max_phys_limit={:#x}", phys_limit_str(limit_40));
}

#[inline]
fn phys_limit_str(limit_40: u64) -> u64 {
    let _ = limit_40;
    cpu_max_phys_addr_limit()
}
